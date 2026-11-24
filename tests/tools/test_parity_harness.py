"""Tests for the parity harness itself: tests/parity/run.py and stub.py."""
import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tests" / "parity"))
import run
import stub


def run_main(argv):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = run.main(argv)
    return code, out.getvalue(), err.getvalue()


@unittest.skipIf(os.name == "nt", "stub launchers are POSIX shell scripts")
class StubTest(unittest.TestCase):
    def test_replays_logs_and_rejects_unrecorded_calls(self):
        with tempfile.TemporaryDirectory() as temp:
            bin_dir, stub_dir = Path(temp, "bin"), Path(temp, "stub")
            bin_dir.mkdir()
            stub_dir.mkdir()
            stub.launcher(bin_dir, "gh")
            entry = {"argv": ["pr", "view"], "stdout": "x\n", "stderr": "", "code": 0}
            (stub_dir / "gh.json").write_text(json.dumps([entry]))
            env = dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ["PATH"], FACTORY_STUB_DIR=str(stub_dir))

            def gh(*args):
                result = subprocess.run(["gh", *args], env=env, capture_output=True, text=True, timeout=60)
                return result.returncode, result.stdout, result.stderr

            self.assertEqual(gh("pr", "view"), (0, "x\n", ""))
            self.assertEqual(gh("pr", "view"), (0, "x\n", ""))
            self.assertEqual(gh("api", "y"), (127, "", "stub: no recorded response for gh api y\n"))
            calls = [json.loads(line) for line in (stub_dir / "gh.calls.jsonl").read_text().splitlines()]
            self.assertEqual(calls, [{"argv": ["pr", "view"]}, {"argv": ["pr", "view"]}, {"argv": ["api", "y"]}])

    def test_launcher_quotes_a_tool_name_with_shell_characters(self):
        with tempfile.TemporaryDirectory() as temp:
            stub_dir = Path(temp)
            stub.launcher(stub_dir, "it's $x")
            result = subprocess.run([str(stub_dir / "it's $x"), "y"], env=dict(os.environ, FACTORY_STUB_DIR=temp),
                                    capture_output=True, text=True, timeout=60)
            self.assertEqual((result.returncode, result.stderr), (127, "stub: no recorded response for it's $x y\n"))


@unittest.skipIf(os.name == "nt", "the parity runner refuses Windows")
class RunnerTest(unittest.TestCase):
    def test_a_wrong_binary_fails(self):
        with tempfile.TemporaryDirectory() as temp:
            wrong = Path(temp, "wrong")
            wrong.write_text("#!/bin/sh\necho wrong\n")
            wrong.chmod(0o755)
            code, out, err = run_main(["--bin", str(wrong), "--case", "main/no-args"])
        self.assertEqual((code, err), (1, ""))
        lines = out.splitlines()
        self.assertEqual(lines[0], "FAIL main/no-args: exit code differs")
        self.assertEqual(lines[1:3], ["--- golden", "+++ bin"])
        self.assertEqual(lines[-1], "parity: 0 passed, 1 failed")

    def test_ids_file_rejects_unknown_pattern(self):
        with tempfile.TemporaryDirectory() as temp:
            ids = Path(temp, "ids.txt")
            ids.write_text("# comment\nmain/no-args\nnope/*\n")
            self.assertEqual(run_main(["--ids", str(ids)]), (2, "", "parity: no case matches nope/*\n"))
        self.assertEqual(run_main(["--case", "nope"]), (2, "", "parity: no case matches nope\n"))

    def test_missing_ids_file_or_bin_exits_2(self):
        with tempfile.TemporaryDirectory() as temp:
            missing = str(Path(temp, "missing"))
            self.assertEqual(run_main(["--ids", missing]), (2, "", f"parity: no such file or folder {missing}\n"))
            self.assertEqual(run_main(["--bin", missing]), (2, "", f"parity: not executable {missing}\n"))
            self.assertEqual(run_main(["--bin", temp]), (2, "", f"parity: not executable {temp}\n"))
            with patch.object(run, "FACTORY_BIN", missing):
                self.assertEqual(run_main([]), (2, "", f"parity: not executable {missing}\n"))

    def test_cases_cover_cli_v1(self):
        ids = [case["id"] for case in run.load_cases()]
        self.assertEqual(len(set(ids)), len(ids))
        self.assertIn("arity/claim/2", ids)
        self.assertIn("arity/claim/4", ids)
        self.assertFalse([i for i in ids if i.startswith("arity/ideas/")])

    def test_host_access_token_never_reaches_a_side(self):
        show = [sys.executable, "-c", "import os; print(os.environ.get('FACTORY_ACCESS_CLIENT_ID'), "
                                      "os.environ.get('FACTORY_ACCESS_CLIENT_SECRET'))"]
        case = {"id": "main/token", "argv": [], "home": None, "stubs": {}}
        with patch.dict(os.environ, {"FACTORY_ACCESS_CLIENT_ID": "host-id", "FACTORY_ACCESS_CLIENT_SECRET": "host-secret"}):
            fields = dict(run.run_side(case, show))
        self.assertEqual(fields["stdout"], "None None\n")

    def test_request_drift_fails(self):
        case = {"id": "drift/get", "argv": ["--home", "{home}"], "home": None, "stubs": {}, "exit": 0,
                "routes": {"GET /a": [200, "ok"]}}

        def fake(path, paths):
            script = Path(path)
            script.write_text(f"#!{sys.executable}\n" + (
                "import re, sys, urllib.error, urllib.request\n"
                "home = sys.argv[sys.argv.index('--home') + 1]\n"
                "origin = re.search(r'api_url: (\\S+)', open(home + '/config.md').read()).group(1)\n"
                f"for path in {paths!r}:\n"
                "    try:\n"
                "        urllib.request.urlopen(origin + path)\n"
                "    except urllib.error.HTTPError:\n"
                "        pass\n"))
            script.chmod(0o755)
            return [str(script)]

        with tempfile.TemporaryDirectory() as temp:
            good, drift = fake(Path(temp, "good"), ["/a"]), fake(Path(temp, "drift"), ["/a", "/extra"])
            with patch.object(run, "GOLDEN", Path(temp, "golden")):
                self.assertEqual(run.run_case(case, good, True), [])
                self.assertEqual(run.run_case(case, good, False), [])
                lines = run.run_case(case, drift, False)
        self.assertEqual(lines[0], "FAIL drift/get: requests differs")
        self.assertTrue(any(line.startswith("+") and '"path": "/extra"' in line for line in lines), lines)

    def test_exit_must_match_case(self):
        case = {"id": "x/exit", "argv": [], "home": None, "stubs": {}, "exit": 3}
        with tempfile.TemporaryDirectory() as temp, patch.object(run, "GOLDEN", Path(temp)):
            self.assertEqual(run.run_case(case, ["false"], True), ["FAIL x/exit: exit 1, case expects 3"])
            self.assertEqual(run.run_case(case, ["true"], True), ["FAIL x/exit: exit 0, case expects 3"])
            self.assertEqual(run.run_case(dict(case, exit=1), ["false"], True), [])

    def test_cases_live_in_their_subsystem_file(self):
        row = {"id": "pipeline/x", "argv": [], "home": None, "stubs": {}}
        with tempfile.TemporaryDirectory() as temp:
            Path(temp, "board.json").write_text(json.dumps({"version": 1, "cases": [row]}))
            with patch.object(run.load_cases, "__defaults__", (Path(temp),)):
                self.assertEqual(run_main([]), (2, "", "parity: board.json: case pipeline/x belongs in pipeline.json\n"))
            Path(temp, "board.json").unlink()
            Path(temp, "pipeline.json").write_text(json.dumps({"version": 1, "cases": [row, row]}))
            with patch.object(run.load_cases, "__defaults__", (Path(temp),)):
                self.assertEqual(run_main([]), (2, "", "parity: duplicate case id pipeline/x\n"))

    def test_ids_directory_reads_every_file(self):
        with tempfile.TemporaryDirectory() as temp:
            Path(temp, "main.txt").write_text("# comment\nmain/no-args\n")
            Path(temp, "doctor.txt").write_text("doctor/extra\n")
            self.assertEqual(run.read_ids(temp), ["doctor/extra", "main/no-args"])
            Path(temp, "board.txt").write_text("main/no-args\n")
            self.assertEqual(run_main(["--ids", temp]),
                             (2, "", "parity: board.txt: pattern main/no-args must start with board/\n"))


@unittest.skipIf(os.name == "nt", "the parity runner refuses Windows")
class PlaceholderTest(unittest.TestCase):
    def test_expand_replaces_known_names_only(self):
        values = {"home": "/h", "tmp": "/t", "api": "http://a", "today": "2026-01-02"}
        self.assertEqual(run.expand("{home} {tmp} {api} {today} {other}", values), "/h /t http://a 2026-01-02 {other}")

    def test_stub_values_are_json_escaped(self):
        values = {"home": 'a"b\\c'}
        json_values = {name: json.dumps(value)[1:-1] for name, value in values.items()}
        self.assertEqual(json.loads(run.expand(json.dumps(["{home}/x"]), json_values)), ['a"b\\c/x'])

    def test_home_files_stubs_and_routes_are_expanded(self):
        script = (
            "import json, os, sys, time, urllib.request\n"
            "home = sys.argv[1]\n"
            "tmp = os.path.dirname(home)\n"
            "today = time.strftime('%Y-%m-%d', time.gmtime())\n"
            "h, api, day = open(home + '/f.txt').read().split('|')\n"
            "stub = json.load(open(os.environ['FACTORY_STUB_DIR'] + '/gh.json'))\n"
            "body = urllib.request.urlopen(api + '/day/' + today).read().decode()\n"
            "print(h == home, api.startswith('http://127.0.0.1:'), day == today, stub == [tmp], body == tmp)\n")
        with tempfile.TemporaryDirectory() as temp:
            Path(temp, "homes/placeholders").mkdir(parents=True)
            Path(temp, "homes/placeholders/f.txt").write_text("{home}|{api}|{today}")
            Path(temp, "side.py").write_text(script)
            case = {"id": "main/placeholders", "argv": ["{home}"], "home": "placeholders", "stubs": {"gh": ["{tmp}"]},
                    "routes": {"GET /day/{today}": [200, "{tmp}"]}}
            with patch.object(run, "HERE", Path(temp)):
                fields = dict(run.run_side(case, [sys.executable, str(Path(temp, "side.py"))]))
        self.assertEqual((fields["exit code"], fields["stdout"]), ("0", "True True True True True\n"), fields["stderr"])
        self.assertEqual(fields["requests"].count(time.strftime("%Y-%m-%d", time.gmtime())), 1)


if __name__ == "__main__":
    unittest.main()
