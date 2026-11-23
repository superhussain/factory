import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from support import ROOT, SKILL, IsolatedTestCase, invoke, invoke_process, load_runtime

sys.path.insert(0, str(ROOT / "tests" / "parity"))
import run
import stub


def run_main(argv):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = run.main(argv)
    return code, out.getvalue(), err.getvalue()


class ProcessInvokeTest(IsolatedTestCase):
    def test_process_matches_in_process(self):
        runtime = load_runtime()
        for args in (["nope"], ["claim", "a"]):
            with self.subTest(args=args):
                self.assertEqual(invoke_process(args, dict(os.environ)), invoke(runtime, args))
        home = os.environ["HOME"]
        args = ["--home", home, "board", home, "412-fix-login"]
        with patch.object(runtime.sys, "stdin", io.StringIO("[]")):
            in_process = invoke(runtime, args)
        self.assertEqual(in_process, (2, "", "factory.py: stdin must be a JSON object\n"))
        self.assertEqual(invoke_process(args, dict(os.environ), input="[]"), in_process)


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


@unittest.skipIf(os.name == "nt", "the parity runner refuses Windows in phase 0")
class RunnerTest(unittest.TestCase):
    def test_python_against_python_passes_every_case(self):
        with patch.object(run, "FACTORY_BIN", ""):
            code, out, err = run_main([])
        self.assertEqual((code, err), (0, ""), out)
        self.assertEqual(out.splitlines()[-1], f"parity: {len(run.load_cases())} passed, 0 failed")

    def test_a_wrong_binary_fails(self):
        with tempfile.TemporaryDirectory() as temp:
            wrong = Path(temp, "wrong")
            wrong.write_text("#!/bin/sh\necho wrong\n")
            wrong.chmod(0o755)
            code, out, err = run_main(["--bin", str(wrong), "--case", "main/no-args"])
        self.assertEqual((code, err), (1, ""))
        lines = out.splitlines()
        self.assertEqual(lines[0], "FAIL main/no-args: exit code differs")
        self.assertEqual(lines[1:3], ["--- python", "+++ bin"])
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

    def case(self, case_id):
        return next(case for case in run.load_cases() if case["id"] == case_id)

    def test_http_case_records_requests(self):
        fields = dict(run.run_side(self.case("pipeline/view"), run.PYTHON))
        self.assertEqual(fields["exit code"], "0")
        self.assertEqual([json.loads(line) for line in fields["requests"].splitlines()], [{
            "method": "GET", "path": "/api/pipeline/effective/sample", "body": "",
            "headers": {"cf-access-client-id": "parity-client-id", "cf-access-client-secret": "parity-client-secret",
                        "user-agent": "factory"}}])
        self.assertIn("b'---\\napi_url: {api}\\ndevice: parity\\n---\\n'", fields["home tree"])

    def test_host_access_token_never_reaches_a_side(self):
        show = [sys.executable, "-c", "import os; print(os.environ.get('FACTORY_ACCESS_CLIENT_ID'), "
                                      "os.environ.get('FACTORY_ACCESS_CLIENT_SECRET'))"]
        with patch.dict(os.environ, {"FACTORY_ACCESS_CLIENT_ID": "host-id", "FACTORY_ACCESS_CLIENT_SECRET": "host-secret"}):
            fields = dict(run.run_side(self.case("main/no-args"), show))
        self.assertEqual(fields["stdout"], "None None\n")

    def test_request_drift_fails(self):
        with tempfile.TemporaryDirectory() as temp:
            drift = Path(temp, "drift")
            drift.write_text(f"#!{sys.executable}\n" + (
                "import re, runpy, sys, urllib.error, urllib.request\n"
                "home = sys.argv[sys.argv.index('--home') + 1]\n"
                "origin = re.search(r'api_url: (\\S+)', open(home + '/config.md').read()).group(1)\n"
                "try:\n"
                "    urllib.request.urlopen(origin + '/extra')\n"
                "except urllib.error.HTTPError:\n"
                "    pass\n"
                f"sys.argv[0] = {str(SKILL / 'runtime/factory.py')!r}\n"
                "runpy.run_path(sys.argv[0], run_name='__main__')\n"))
            drift.chmod(0o755)
            code, out, err = run_main(["--bin", str(drift), "--case", "pipeline/view"])
        self.assertEqual((code, err), (1, ""))
        lines = out.splitlines()
        self.assertEqual(lines[0], "FAIL pipeline/view: requests differs")
        self.assertIn('+{"body": "", "headers": {"user-agent": "Python-urllib/%d.%d"}, "method": "GET", '
                      '"path": "/extra"}' % sys.version_info[:2], lines)

    def test_python_exit_must_match_case(self):
        case = dict(self.case("pipeline/view"), exit=3)
        self.assertEqual(run.run_case(case, ["false"]), ["FAIL pipeline/view: python exit 0, case expects 3"])

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
            with patch.object(run, "FACTORY_BIN", ""):
                code, out, err = run_main(["--ids", temp])
            self.assertEqual((code, out.splitlines()[-1], err), (0, "parity: 2 passed, 0 failed", ""))
            Path(temp, "board.txt").write_text("main/no-args\n")
            self.assertEqual(run_main(["--ids", temp]),
                             (2, "", "parity: board.txt: pattern main/no-args must start with board/\n"))


class UsageTextTest(unittest.TestCase):
    def test_usage_txt_matches_python_no_argument_stderr(self):
        result = subprocess.run([sys.executable, "-B", str(SKILL / "runtime/factory.py")], capture_output=True,
                                stdin=subprocess.DEVNULL, timeout=60)
        self.assertEqual(result.returncode, 2)
        self.assertEqual((ROOT / "rust/factory/src/usage.txt").read_bytes(), result.stderr.replace(b"\r\n", b"\n"),
                         "regenerate with: python3 -B skills/factory/runtime/factory.py 2> rust/factory/src/usage.txt")
