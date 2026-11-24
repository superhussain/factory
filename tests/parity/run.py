"""Parity runner: run each parity case on a factory binary and compare it with the case's golden files.

python3 tests/parity/run.py --bin PATH [--ids FILE|DIR] [--case ID]... [--jobs N] [--shard K/N] [--record]

Each case runs in its own temp root with its own home, stub folder and HTTP port, so cases run in a process pool.
Results print in case order. --shard K/N keeps every Nth case starting at the Kth, for a CI matrix. --record writes
each selected case's normalized output to tests/parity/golden/<case-id>/ instead of comparing it.
"""
import argparse
import calendar
import concurrent.futures
import contextlib
import difflib
import fnmatch
import gzip
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import stub

ROOT = HERE.parents[1]
FACTORY_BIN = os.environ.get("FACTORY_BIN", "")
TOOLS = ("gh", "paseo", "git", "crontab")
CASES = HERE / "cases"
GOLDEN = HERE / "golden"
REQUEST_HEADERS = ("content-type", "if-match", "if-none-match", "cf-access-client-id", "cf-access-client-secret",
                   "user-agent", "authorization")
FIELDS = ("exit code", "stdout", "stderr", "home tree") + tuple(f"{tool} calls" for tool in TOOLS) + ("requests",)
# A golden's dates relative to the run's UTC date. --record writes these placeholders, and a compare expands them.
DAYS = {"{today}": 0, "{today-6}": 6}
ELAPSED = re.compile(rb'"elapsed": \d+')
STAMP = re.compile(rb"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ")


def isolated_env(home):
    env = dict(os.environ)
    for key in list(env):
        if (key in ("FACTORY_HOME", "FACTORY_SESSION", "PASEO_HOME", "PASEO_AGENT_ID", "CLAUDE_CONFIG_DIR",
                    "GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE")
                or key.startswith("GIT_CONFIG_") or key.lower().endswith("_proxy")):
            env.pop(key, None)
    env.update(HOME=str(home), USERPROFILE=str(home), XDG_CONFIG_HOME=str(home / "config"),
               NO_PROXY="localhost,127.0.0.1",
               GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=str(home / "gitconfig"),
               GIT_AUTHOR_NAME="Test", GIT_AUTHOR_EMAIL="test@example.invalid",
               GIT_COMMITTER_NAME="Test", GIT_COMMITTER_EMAIL="test@example.invalid")
    return env


def snapshot(root):
    return {str(path.relative_to(root)): (stat.S_IMODE(path.stat().st_mode),
            path.read_bytes() if path.is_file() else None)
            for path in [root] + sorted(root.rglob("*"))}


@contextlib.contextmanager
def http_fixture(routes):
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            data = self.rfile.read(int(self.headers.get("Content-Length") or 0))
            requests.append((self.command, self.path, dict(self.headers), data))
            route = routes.get(f"{self.command} {self.path}", routes.get(self.path, (404, b"")))
            # A list serves its replies in order, and the last one repeats.
            if isinstance(route, list):
                route = route.pop(0) if len(route) > 1 else route[0]
            status, body, *extra = route
            if not isinstance(body, bytes):
                body = json.dumps(body).encode("utf-8")
            self.send_response(status)
            if status == 302:
                self.send_header("Location", "https://example.invalid/login")
            self.send_header("ETag", '"' + hashlib.sha256(body).hexdigest() + '"')
            for name, value in (extra[0] if extra else {}).items():
                self.send_header(name, value)
            self.end_headers()
            self.wfile.write(body)
        do_POST = do_PUT = do_PATCH = do_GET
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.requests = requests
    server.origin = "http://127.0.0.1:" + str(server.server_port)
    thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.01}, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


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


def utc_date(days_back):
    return time.strftime("%Y-%m-%d", time.gmtime(time.time() - days_back * 86400))


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
            values = {"home": str(home), "tmp": str(root), "today": utc_date(0), "root": str(ROOT)}
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
            # The binary's local dates are UTC dates, so a golden holds the same dates in every zone.
            env["TZ"] = "UTC"
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
            # The goldens came from Python, which printed factory.py's path where the binary prints its own;
            # both read {runtime}. The skill root is two folders up.
            runtime = os.path.realpath(command[-1])
            data = data.replace(os.path.join(os.path.dirname(runtime), "factory.py").encode(), b"{runtime}")
            if "skill" in case:
                # In a skill folder the binary sits in runtime/ and reads {bin}.
                data = data.replace(os.path.join(os.path.dirname(runtime), "factory").encode(), b"{bin}")
            data = data.replace(runtime.encode(), b"{runtime}").replace(os.path.abspath(command[-1]).encode(), b"{runtime}")
            data = data.replace(os.path.dirname(os.path.dirname(runtime)).encode(), b"{skill}")
            data = data.replace(str(home).encode(), b"{home}").replace(str(root).encode(), b"{tmp}")
            data = data.replace(str(ROOT).encode(), b"{root}")
            return ELAPSED.sub(rb'"elapsed": {n}', now_stamps(data, start, end))

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


def golden_file(folder, field):
    """A field's golden file. The home tree repeats each home's fixtures, so it is gzipped."""
    name = field.replace(" ", "-")
    return folder / (name + ".gz" if field == "home tree" else name)


def read_golden(case):
    """The case's golden fields, or None when it has no golden folder. A missing file is an empty field."""
    folder = GOLDEN / case["id"]
    if not folder.is_dir():
        return None
    fields = []
    for field in FIELDS:
        path = golden_file(folder, field)
        data = path.read_bytes() if path.is_file() else b""
        if data and path.suffix == ".gz":
            data = gzip.decompress(data)
        fields.append((field, data.decode("utf-8", "surrogateescape")))
    return fields


def write_golden(case, fields):
    folder = GOLDEN / case["id"]
    if folder.is_dir():
        shutil.rmtree(folder)
    folder.mkdir(parents=True)
    for field, value in fields:
        if value or field == "exit code":
            path = golden_file(folder, field)
            data = value.encode("utf-8", "surrogateescape")
            path.write_bytes(gzip.compress(data, 9, mtime=0) if path.suffix == ".gz" else data)


def run_case(case, command, record):
    fields = run_side(case, command)
    dates = {name: utc_date(back) for name, back in DAYS.items()}
    if record:
        # A date the case itself spells out is fixture data, not today's date, so it stays literal.
        written = json.dumps(case)
        for name, date in dates.items():
            if date not in written:
                fields = [(field, value.replace(date, name)) for field, value in fields]
        if ("routes" in case or "exit" in case) and fields[0][1] != str(case.get("exit")):
            return [f"FAIL {case['id']}: exit {fields[0][1]}, case expects {case.get('exit')}"]
        write_golden(case, fields)
        return []
    golden = read_golden(case)
    if golden is None:
        return [f"FAIL {case['id']}: no golden folder {GOLDEN / case['id']}; run with --record"]
    for (field, want), (_, got) in zip(golden, fields):
        want = expand(want, {name[1:-1]: date for name, date in dates.items()})
        if want != got:
            diff = difflib.unified_diff(want.splitlines(), got.splitlines(), "golden", "bin", lineterm="")
            return [f"FAIL {case['id']}: {field} differs"] + list(diff)[:40]
    return []


def main(argv):
    parser = argparse.ArgumentParser(prog="run.py")
    parser.add_argument("--bin")
    parser.add_argument("--ids")
    parser.add_argument("--case", action="append", default=[])
    parser.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    parser.add_argument("--shard")
    parser.add_argument("--record", action="store_true")
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
    if not binary:
        print("parity: --bin or FACTORY_BIN required", file=sys.stderr)
        return 2
    command = [os.path.abspath(binary)]
    failed = 0
    # Processes, not threads: each case's harness work holds the GIL, so threads barely overlap.
    with concurrent.futures.ProcessPoolExecutor(max_workers=args.jobs) as pool:
        runs = pool.map(run_case, cases, [command] * len(cases), [args.record] * len(cases), chunksize=4)
        for case, lines in zip(cases, runs):
            print("\n".join(lines) if lines else f"PASS {case['id']}", flush=True)
            failed += bool(lines)
    print(f"parity: {len(cases) - failed} {'recorded' if args.record else 'passed'}, {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
