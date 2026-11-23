"""Differential runner: run each parity case on Python and on a factory binary, then compare.

python3 tests/parity/run.py [--bin PATH] [--ids FILE|DIR] [--case ID]... [--jobs N] [--shard K/N]

Each case side runs in its own temp root with its own home, stub folder and HTTP port, so cases run in a process
pool. Results print in case order. --shard K/N keeps every Nth case starting at the Kth, for a CI matrix.
"""
import argparse
import calendar
import concurrent.futures
import contextlib
import difflib
import fnmatch
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[1] / "tests" / "cli"))
from support import FACTORY_BIN, ROOT, SKILL, http_fixture, isolated_env, snapshot
import stub

PYTHON = [sys.executable, "-B", str(SKILL / "runtime/factory.py")]
TOOLS = ("gh", "paseo", "git", "crontab")
CASES = HERE / "cases"
REQUEST_HEADERS = ("content-type", "if-match", "if-none-match", "cf-access-client-id", "cf-access-client-secret",
                   "user-agent", "authorization")
STAMP = re.compile(rb"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ")


def load_cases(folder=CASES):
    cases = []
    for path in sorted(Path(folder).glob("*.json")):
        for case in json.loads(path.read_text(encoding="utf-8"))["cases"]:
            prefix = case["id"].split("/", 1)[0]
            if prefix != path.stem:
                raise ValueError(f"parity: {path.name}: case {case['id']} belongs in {prefix}.json")
            cases.append(case)
    for row in json.loads((ROOT / "tests/fixtures/cli-v1.json").read_text(encoding="utf-8"))["commands"]:
        if row["name"] == "ideas":
            continue
        for count in (row["min_args"] - 1, row["max_args"] + 1):
            if count >= 0:
                cases.append({"id": f"arity/{row['name']}/{count}", "argv": [row["name"]] + ["operand"] * count,
                              "home": None, "stubs": {}})
    seen = set()
    for case in cases:
        if case["id"] in seen:
            raise ValueError(f"parity: duplicate case id {case['id']}")
        seen.add(case["id"])
    return cases


def read_ids(path):
    path = Path(path)
    files = sorted(path.glob("*.txt")) if path.is_dir() else [path]
    patterns = []
    for file in files:
        for line in file.read_text(encoding="utf-8").splitlines():
            pattern = line.split("#", 1)[0].strip()
            if pattern and path.is_dir() and not pattern.startswith(file.stem + "/"):
                raise ValueError(f"parity: {file.name}: pattern {pattern} must start with {file.stem}/")
            if pattern:
                patterns.append(pattern)
    return patterns


def expand(text, values):
    """Text with each {name} placeholder in values replaced by its value."""
    for name, value in values.items():
        text = text.replace("{" + name + "}", value)
    return text


def routes_for(case, values):
    """The case's routes for http_fixture, as fresh lists because list replies pop. A string body is sent as text.
    Keys and string bodies get their placeholders expanded."""
    def reply(route):
        status, body, *extra = route
        return (status, expand(body, values).encode("utf-8") if isinstance(body, str) else body, *extra)
    return {expand(key, values): [reply(r) for r in value] if isinstance(value[0], list) else reply(value)
            for key, value in case["routes"].items()}


def request_lines(server, normalize):
    lines = []
    for method, path, headers, body in server.requests:
        kept = {name.lower(): value for name, value in headers.items() if name.lower() in REQUEST_HEADERS}
        line = json.dumps({"method": method, "path": path, "headers": kept,
                           "body": body.decode("utf-8", "surrogateescape")}, sort_keys=True)
        lines.append(normalize(line.encode("ascii")).decode("ascii"))
    return lines


def now_stamps(data, start, end):
    def stamp(match):
        try:
            seconds = calendar.timegm(time.strptime(match.group().decode(), "%Y-%m-%dT%H:%M:%SZ"))
        except ValueError:
            return match.group()
        return b"{now}" if start <= seconds <= end else match.group()
    return STAMP.sub(stamp, data)


def place_skill(case, command, values):
    """The command run from a copy of its runtime in the case's skill folder, beside the case's bundle.json."""
    if "skill" not in case:
        return command
    skill = Path(expand(case["skill"], values))
    (skill / "runtime").mkdir(parents=True)
    if command == PYTHON:
        for path in (SKILL / "runtime").glob("*.py"):
            shutil.copy2(path, skill / "runtime")
        command = PYTHON[:2] + [str(skill / "runtime" / "factory.py")]
    else:
        shutil.copy2(command[0], skill / "runtime" / "factory")
        command = [str(skill / "runtime" / "factory")]
    if "bundle" in case:
        (skill / "bundle.json").write_text(expand(case["bundle"], values), encoding="utf-8")
    return command


def run_side(case, command):
    root = Path(tempfile.mkdtemp(prefix="parity-")).resolve()
    try:
        home, bin_dir, stub_dir = root / "home", root / "bin", root / "stub"
        routes = {}
        with http_fixture(routes) if "routes" in case else contextlib.nullcontext() as server:
            values = {"home": str(home), "tmp": str(root), "today": time.strftime("%Y-%m-%d"), "root": str(ROOT)}
            if server:
                values["api"] = server.origin
                routes.update(routes_for(case, values))
            if case["home"]:
                shutil.copytree(HERE / "homes" / case["home"], home)
                for path in home.rglob("*"):
                    if path.is_file() and not path.is_symlink():
                        text = path.read_bytes().decode("utf-8", "surrogateescape")
                        path.write_bytes(expand(text, values).encode("utf-8", "surrogateescape"))
            else:
                home.mkdir()
            bin_dir.mkdir()
            stub_dir.mkdir()
            for tool in TOOLS:
                stub.launcher(bin_dir, tool)
            # Stub files are JSON text, so each value goes in JSON-escaped.
            json_values = {name: json.dumps(value)[1:-1] for name, value in values.items()}
            for tool, entries in case["stubs"].items():
                (stub_dir / f"{tool}.json").write_text(expand(json.dumps(entries), json_values), encoding="utf-8")
            command = place_skill(case, command, values)
            env = isolated_env(home)
            # A side gets only the fake token from home/.env, so a real one never reaches the fixture or a diff.
            env.pop("FACTORY_ACCESS_CLIENT_ID", None)
            env.pop("FACTORY_ACCESS_CLIENT_SECRET", None)
            env.update(case.get("env", {}))
            env.update(PATH=str(bin_dir) + os.pathsep + os.environ.get("PATH", ""), FACTORY_STUB_DIR=str(stub_dir),
                       PYTHONUTF8="1", PYTHONDONTWRITEBYTECODE="1")
            argv = [arg.replace("{home}", str(home)) for arg in case["argv"]]
            stdin = {"input": case["stdin"].encode("utf-8")} if "stdin" in case else {"stdin": subprocess.DEVNULL}
            if server:
                (home / "config.md").write_text(f"---\napi_url: {server.origin}\ndevice: parity\n---\n",
                                                encoding="utf-8")
                (home / ".env").write_text("FACTORY_ACCESS_CLIENT_ID=parity-client-id\n"
                                           "FACTORY_ACCESS_CLIENT_SECRET=parity-client-secret\n", encoding="utf-8")
            start = int(time.time())
            result = subprocess.run(command + argv, env=env, cwd=root, capture_output=True, timeout=60, **stdin)
            end = time.time()

        def normalize(data):
            if server:
                data = data.replace(server.origin.encode(), b"{api}")
            # Python prints factory.py's path and the binary its own or the factory.py beside it; the
            # skill root is two folders up.
            runtime = os.path.realpath(command[-1])
            data = data.replace(os.path.join(os.path.dirname(runtime), "factory.py").encode(), b"{runtime}")
            if "skill" in case:
                # In a skill folder both sides sit in runtime/, so the binary beside factory.py reads {bin}.
                data = data.replace(os.path.join(os.path.dirname(runtime), "factory").encode(), b"{bin}")
            data = data.replace(runtime.encode(), b"{runtime}").replace(os.path.abspath(command[-1]).encode(), b"{runtime}")
            data = data.replace(os.path.dirname(os.path.dirname(runtime)).encode(), b"{skill}")
            data = data.replace(str(home).encode(), b"{home}").replace(str(root).encode(), b"{tmp}")
            return now_stamps(data, start, end)

        tree = [f"{path} {mode:o} {normalize(data)!r}" if data is not None else f"{path} {mode:o}"
                for path, (mode, data) in snapshot(home).items()]
        fields = [("exit code", str(result.returncode)), ("stdout", normalize(result.stdout)),
                  ("stderr", normalize(result.stderr)), ("home tree", "\n".join(tree))]
        for tool in TOOLS:
            log = stub_dir / f"{tool}.calls.jsonl"
            fields.append((f"{tool} calls", normalize(log.read_bytes()) if log.exists() else b""))
        fields.append(("requests", "\n".join(request_lines(server, normalize)) if server else ""))
        return [(name, value.decode("utf-8", "surrogateescape") if isinstance(value, bytes) else value)
                for name, value in fields]
    finally:
        shutil.rmtree(root)


def run_case(case, bin_command):
    python = run_side(case, PYTHON)
    if ("routes" in case or "exit" in case) and python[0][1] != str(case.get("exit")):
        return [f"FAIL {case['id']}: python exit {python[0][1]}, case expects {case.get('exit')}"]
    for (field, python), (_, binary) in zip(python, run_side(case, bin_command)):
        if python != binary:
            diff = difflib.unified_diff(python.splitlines(), binary.splitlines(), "python", "bin", lineterm="")
            return [f"FAIL {case['id']}: {field} differs"] + list(diff)[:40]
    return []


def main(argv):
    parser = argparse.ArgumentParser(prog="run.py")
    parser.add_argument("--bin")
    parser.add_argument("--ids")
    parser.add_argument("--case", action="append", default=[])
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    parser.add_argument("--shard")
    args = parser.parse_args(argv)
    shard = re.fullmatch(r"([1-9]\d*)/([1-9]\d*)", args.shard or "1/1")
    if args.jobs < 1 or not shard or int(shard[1]) > int(shard[2]):
        print("parity: --jobs takes a positive count and --shard takes K/N with 1 <= K <= N", file=sys.stderr)
        return 2
    if os.name == "nt":
        print("parity: POSIX hosts only in phase 0", file=sys.stderr)
        return 2
    if args.ids and not Path(args.ids).exists():
        print(f"parity: no such file or folder {args.ids}", file=sys.stderr)
        return 2
    binary = args.bin or FACTORY_BIN
    if binary and not (os.path.isfile(binary) and os.access(binary, os.X_OK)):
        print(f"parity: not executable {binary}", file=sys.stderr)
        return 2
    try:
        cases = load_cases()
        patterns = list(args.case) + (read_ids(args.ids) if args.ids else [])
    except ValueError as e:
        print(e, file=sys.stderr)
        return 2
    for pattern in patterns:
        if not any(fnmatch.fnmatchcase(case["id"], pattern) for case in cases):
            print(f"parity: no case matches {pattern}", file=sys.stderr)
            return 2
    if patterns:
        cases = [case for case in cases if any(fnmatch.fnmatchcase(case["id"], p) for p in patterns)]
    cases = cases[int(shard[1]) - 1::int(shard[2])]
    bin_command = [os.path.abspath(binary)] if binary else PYTHON
    failed = 0
    # Processes, not threads: each case's Python-side work holds the GIL, so threads barely overlap.
    with concurrent.futures.ProcessPoolExecutor(max_workers=args.jobs) as pool:
        for case, lines in zip(cases, pool.map(run_case, cases, [bin_command] * len(cases), chunksize=4)):
            print("\n".join(lines) if lines else f"PASS {case['id']}", flush=True)
            failed += bool(lines)
    print(f"parity: {len(cases) - failed} passed, {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
