"""Differential runner: run each parity case on Python and on a factory binary, then compare.

python3 tests/parity/run.py [--bin PATH] [--ids FILE] [--case ID]...
"""
import argparse
import difflib
import fnmatch
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1] / "tests" / "cli"))
from support import ROOT, SKILL, factory_command, isolated_env, snapshot
import stub

PYTHON = [sys.executable, "-B", str(SKILL / "runtime/factory.py")]
TOOLS = ("gh", "paseo", "git")


def load_cases():
    cases = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))["cases"]
    for row in json.loads((ROOT / "tests/fixtures/cli-v1.json").read_text(encoding="utf-8"))["commands"]:
        if row["name"] == "ideas":
            continue
        for count in (row["min_args"] - 1, row["max_args"] + 1):
            if count >= 0:
                cases.append({"id": f"arity/{row['name']}/{count}", "argv": [row["name"]] + ["operand"] * count,
                              "home": None, "stubs": {}})
    return cases


def run_side(case, command):
    root = Path(tempfile.mkdtemp(prefix="parity-")).resolve()
    try:
        home, bin_dir, stub_dir = root / "home", root / "bin", root / "stub"
        if case["home"]:
            shutil.copytree(HERE / "homes" / case["home"], home)
        else:
            home.mkdir()
        bin_dir.mkdir()
        stub_dir.mkdir()
        for tool in TOOLS:
            stub.launcher(bin_dir, tool)
        for tool, entries in case["stubs"].items():
            (stub_dir / f"{tool}.json").write_text(json.dumps(entries), encoding="utf-8")
        env = isolated_env(home)
        env.update(PATH=str(bin_dir) + os.pathsep + os.environ.get("PATH", ""), FACTORY_STUB_DIR=str(stub_dir),
                   PYTHONUTF8="1", PYTHONDONTWRITEBYTECODE="1")
        argv = [arg.replace("{home}", str(home)) for arg in case["argv"]]
        result = subprocess.run(command + argv, env=env, cwd=root, stdin=subprocess.DEVNULL, capture_output=True,
                                timeout=60)

        def normalize(data):
            return data.replace(str(home).encode(), b"{home}").replace(str(root).encode(), b"{tmp}")

        tree = [f"{path} {mode:o} {normalize(data)!r}" if data is not None else f"{path} {mode:o}"
                for path, (mode, data) in snapshot(home).items()]
        fields = [("exit code", str(result.returncode)), ("stdout", normalize(result.stdout)),
                  ("stderr", normalize(result.stderr)), ("home tree", "\n".join(tree))]
        for tool in TOOLS:
            log = stub_dir / f"{tool}.calls.jsonl"
            fields.append((f"{tool} calls", normalize(log.read_bytes()) if log.exists() else b""))
        return [(name, value.decode("utf-8", "surrogateescape") if isinstance(value, bytes) else value)
                for name, value in fields]
    finally:
        shutil.rmtree(root)


def run_case(case, bin_command):
    for (field, python), (_, binary) in zip(run_side(case, PYTHON), run_side(case, bin_command)):
        if python != binary:
            diff = difflib.unified_diff(python.splitlines(), binary.splitlines(), "python", "bin", lineterm="")
            return [f"FAIL {case['id']}: {field} differs"] + list(diff)[:40]
    return []


def main(argv):
    parser = argparse.ArgumentParser(prog="run.py")
    parser.add_argument("--bin")
    parser.add_argument("--ids")
    parser.add_argument("--case", action="append", default=[])
    args = parser.parse_args(argv)
    if os.name == "nt":
        print("parity: POSIX hosts only in phase 0", file=sys.stderr)
        return 2
    if args.ids and not Path(args.ids).is_file():
        print(f"parity: no such file {args.ids}", file=sys.stderr)
        return 2
    if args.bin and not os.access(args.bin, os.X_OK):
        print(f"parity: not executable {args.bin}", file=sys.stderr)
        return 2
    cases = load_cases()
    patterns = list(args.case)
    if args.ids:
        lines = Path(args.ids).read_text(encoding="utf-8").splitlines()
        patterns += [line.split("#", 1)[0].strip() for line in lines if line.split("#", 1)[0].strip()]
    for pattern in patterns:
        if not any(fnmatch.fnmatchcase(case["id"], pattern) for case in cases):
            print(f"parity: no case matches {pattern}", file=sys.stderr)
            return 2
    if patterns:
        cases = [case for case in cases if any(fnmatch.fnmatchcase(case["id"], p) for p in patterns)]
    bin_command = [os.path.abspath(args.bin)] if args.bin else factory_command()
    failed = 0
    for case in cases:
        lines = run_case(case, bin_command)
        print("\n".join(lines) if lines else f"PASS {case['id']}")
        failed += bool(lines)
    print(f"parity: {len(cases) - failed} passed, {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
