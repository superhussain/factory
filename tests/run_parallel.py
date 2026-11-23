"""Run unittest modules in parallel processes, one child per module, or per test class in the slow modules.

python3 -B tests/run_parallel.py START PATTERN [PATTERN...]

Each child runs unittest with START first on sys.path, the way `unittest discover -s START` does, so sibling
imports such as `from support import ...` keep working. Prints the output of every failing child and exits 1 on
any failure, error, import error or child crash.
"""
import concurrent.futures
import importlib
import json
import os
import subprocess
import sys
import time
import traceback
import unittest
from pathlib import Path

# The slowest modules split per test class so one file does not hold the run's tail.
SPLIT = {"test_factory", "test_pickup", "test_outcomes", "test_pre_review"}
SUMMARY = "run_parallel-summary:"


def units(start, patterns):
    """Unit names to run in children, slow ones first, and the modules that failed to import."""
    loader = unittest.TestLoader()
    sizes, broken = {}, []
    files = sorted({path for pattern in patterns for path in Path(start).glob(pattern)})
    for path in files:
        name = path.stem
        try:
            module = importlib.import_module(name)
        except BaseException:
            broken.append((name, traceback.format_exc()))
            continue
        if name in SPLIT and not hasattr(module, "load_tests"):
            for attr in sorted(dir(module)):
                value = getattr(module, attr)
                if isinstance(value, type) and issubclass(value, unittest.TestCase):
                    count = loader.loadTestsFromTestCase(value).countTestCases()
                    if count:
                        sizes[f"{name}.{attr}"] = count
        else:
            sizes[name] = loader.loadTestsFromModule(module).countTestCases()
    ordered = sorted(sizes, key=lambda unit: (unit.split(".")[0] not in SPLIT, -sizes[unit], unit))
    return ordered, broken


def child(start, name):
    sys.path.insert(0, start)
    suite = unittest.defaultTestLoader.loadTestsFromName(name)
    result = unittest.TextTestRunner(stream=sys.stderr, verbosity=1).run(suite)
    print(SUMMARY + json.dumps({"run": result.testsRun, "failures": len(result.failures),
                                "errors": len(result.errors), "skipped": len(result.skipped)}), flush=True)
    return 0 if result.wasSuccessful() else 1


def run_unit(start, name):
    began = time.monotonic()
    proc = subprocess.run([sys.executable, "-B", __file__, "--child", start, name], stdin=subprocess.DEVNULL,
                          capture_output=True, text=True, errors="replace")
    summary = None
    for line in proc.stdout.splitlines():
        if line.startswith(SUMMARY):
            summary = json.loads(line[len(SUMMARY):])
    return name, proc, summary, time.monotonic() - began


def main(argv):
    if argv[:1] == ["--child"]:
        return child(argv[1], argv[2])
    if len(argv) < 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    start, patterns = str(Path(argv[0]).resolve()), argv[1:]
    sys.path.insert(0, start)
    names, broken = units(start, patterns)
    began = time.monotonic()
    totals = {"run": 0, "failures": 0, "errors": 0, "skipped": 0}
    failed = []
    for name, error in broken:
        print(f"===== FAIL {name} (import error)")
        sys.stdout.write(error)
        totals["errors"] += 1
        failed.append(name)
    workers = os.cpu_count() or 1
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        for name, proc, summary, seconds in pool.map(lambda name: run_unit(start, name), names):
            ok = proc.returncode == 0 and summary is not None
            if summary:
                for key in totals:
                    totals[key] += summary[key]
            if not ok:
                failed.append(name)
                print(f"===== FAIL {name} (exit {proc.returncode}, {seconds:.1f} s)")
                sys.stdout.write(proc.stdout)
                sys.stdout.write(proc.stderr)
                if summary is None:
                    print(f"{name}: the child exited before reporting a result")
    print(f"Ran {totals['run']} tests in {time.monotonic() - began:.1f} s across {len(names)} processes "
          f"({workers} at a time): {totals['failures']} failures, {totals['errors']} errors, "
          f"{totals['skipped']} skipped")
    if not totals["run"] and not failed:
        failed.append("no tests found")
    if failed:
        print("FAILED: " + ", ".join(failed))
        return 1
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
