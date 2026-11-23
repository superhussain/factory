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


@unittest.skipIf(os.name == "nt", "the parity runner refuses Windows in phase 0")
class RunnerTest(unittest.TestCase):
    def test_python_against_python_passes_every_case(self):
        with patch.object(run, "factory_command", return_value=run.PYTHON):
            code, out, err = run_main([])
        self.assertEqual((code, err), (0, ""), out)
        self.assertEqual(out.splitlines()[-1], "parity: 49 passed, 0 failed")

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

    def test_cases_cover_cli_v1(self):
        ids = [case["id"] for case in run.load_cases()]
        self.assertEqual(len(ids), 49)
        self.assertEqual(len(set(ids)), 49)
        self.assertIn("arity/claim/2", ids)
        self.assertIn("arity/claim/4", ids)
        self.assertFalse([i for i in ids if i.startswith("arity/ideas/")])


class UsageTextTest(unittest.TestCase):
    def test_usage_txt_matches_python_no_argument_stderr(self):
        result = subprocess.run([sys.executable, "-B", str(SKILL / "runtime/factory.py")], capture_output=True,
                                stdin=subprocess.DEVNULL, timeout=60)
        self.assertEqual(result.returncode, 2)
        self.assertEqual((ROOT / "rust/factory/src/usage.txt").read_bytes(), result.stderr.replace(b"\r\n", b"\n"),
                         "regenerate with: python3 -B skills/factory/runtime/factory.py 2> rust/factory/src/usage.txt")
