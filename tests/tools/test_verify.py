"""Targeted tests for tools/verify.py: selection, arguments, propagation and JSON.

Every command is mocked with a patched subprocess.run; no heavy command runs
inside a test.
"""
import importlib.util
import io
import json
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent.parent
HOSTED_SCRIPTS = ("test:assets", "test:auth", "test:protocol", "test:local", "test:browser", "test:deploy")

spec = importlib.util.spec_from_file_location("factory_verify", ROOT / "tools" / "verify.py")
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)


def ids(calls):
    result = []
    for call in calls:
        command = call.args[0]
        if command[1:2] == ["run"]:
            result.append(command[2])
        elif "tests/tools" in command:
            result.append("install-unittest")
        else:
            result.append("cli-unittest")
    return result


class SelectionTest(unittest.TestCase):
    def run_main(self, *argv):
        with mock.patch("subprocess.run") as run, redirect_stdout(io.StringIO()) as out:
            run.return_value = mock.Mock(returncode=0)
            code = verify.main(list(argv))
        return code, run, out.getvalue()

    def test_device_group_runs_fixed_unittest_commands(self):
        code, run, _ = self.run_main("device")
        self.assertEqual(code, 0)
        commands = [call.args[0] for call in run.call_args_list]
        self.assertEqual(commands, [
            [sys.executable, "-B", "-m", "unittest", "discover", "-s", "tests/cli", "-p", "test_*.py"],
            [sys.executable, "-B", "-m", "unittest", "discover", "-s", "tests/tools", "-p", "test_install.py"],
        ])
        for call in run.call_args_list:
            self.assertEqual(call.kwargs["cwd"], verify.ROOT)
            self.assertEqual(call.kwargs["env"]["PYTHONDONTWRITEBYTECODE"], "1")
            self.assertEqual(call.kwargs["env"]["WRANGLER_SEND_METRICS"], "false")

    def test_hosted_group_runs_existing_pnpm_scripts_in_order(self):
        code, run, _ = self.run_main("hosted")
        self.assertEqual(code, 0)
        commands = [call.args[0] for call in run.call_args_list]
        self.assertEqual([command[1:] for command in commands],
                         [["run", script] for script in HOSTED_SCRIPTS])
        for command in commands:
            self.assertEqual(Path(command[0]).name, "pnpm")

    def test_all_group_runs_each_leaf_once_in_order(self):
        code, run, _ = self.run_main("all")
        self.assertEqual(code, 0)
        self.assertEqual(ids(run.call_args_list),
                         ["cli-unittest", "install-unittest", *HOSTED_SCRIPTS])
        self.assertEqual(len(run.call_args_list), 8)

    def test_missing_pnpm_fails_closed_for_hosted(self):
        for group in ("hosted", "all"):
            with self.subTest(group=group), mock.patch("shutil.which", return_value=None) as which, \
                    mock.patch("subprocess.run") as run, redirect_stderr(io.StringIO()) as err:
                self.assertEqual(verify.main([group]), 2)
            which.assert_called_once_with("pnpm")
            run.assert_not_called()
            self.assertEqual(err.getvalue(),
                             "pnpm is required for the hosted checks but was not found on PATH\n")

    def test_device_group_does_not_resolve_pnpm(self):
        with mock.patch("shutil.which", side_effect=AssertionError("pnpm resolved")):
            code, run, _ = self.run_main("device")
        self.assertEqual(code, 0)
        self.assertEqual(len(run.call_args_list), 2)


class ArgumentTest(unittest.TestCase):
    def test_invalid_arguments_exit_2_without_running(self):
        with mock.patch("subprocess.run") as run:
            for argv in ([], ["nosuch"], ["device", "--wat"], ["device", "hosted"],
                         ["--json"], ["device", "--json", "extra"]):
                with self.subTest(argv=argv):
                    self.assertEqual(verify.main(argv), 2)
            run.assert_not_called()


class PropagationTest(unittest.TestCase):
    def run_with_failure(self, group, failing_index):
        codes = [0] * 8
        codes[failing_index] = 3
        with mock.patch("subprocess.run") as run, redirect_stdout(io.StringIO()):
            run.side_effect = [mock.Mock(returncode=code) for code in codes]
            code = verify.main([group])
        return code, run

    def test_first_failure_exits_1_and_stops(self):
        code, run = self.run_with_failure("device", 0)
        self.assertEqual(code, 1)
        self.assertEqual(len(run.call_args_list), 1)

    def test_later_hosted_failure_skips_remaining(self):
        code, run = self.run_with_failure("all", 4)
        self.assertEqual(code, 1)
        self.assertEqual(len(run.call_args_list), 5)


class JsonTest(unittest.TestCase):
    def summary(self, returncode):
        with mock.patch("subprocess.run") as run, redirect_stdout(io.StringIO()) as out:
            run.return_value = mock.Mock(returncode=returncode)
            code = verify.main(["device", "--json"])
        return code, json.loads(out.getvalue().strip().splitlines()[-1])

    def test_pass_summary_shape(self):
        code, summary = self.summary(0)
        self.assertEqual(code, 0)
        self.assertEqual(summary, {
            "version": 1,
            "group": "device",
            "status": "pass",
            "results": [
                {"id": "cli-unittest", "exit_code": 0},
                {"id": "install-unittest", "exit_code": 0},
            ],
        })

    def test_fail_summary_stops_at_first_failure(self):
        with mock.patch("subprocess.run") as run, redirect_stdout(io.StringIO()) as out:
            run.return_value = mock.Mock(returncode=1)
            code = verify.main(["device", "--json"])
        self.assertEqual(code, 1)
        self.assertEqual(json.loads(out.getvalue().strip().splitlines()[-1]), {
            "version": 1,
            "group": "device",
            "status": "fail",
            "results": [{"id": "cli-unittest", "exit_code": 1}],
        })


class ImportIsolationTest(unittest.TestCase):
    def test_import_performs_no_file_io(self):
        module = importlib.util.module_from_spec(spec)
        with mock.patch("builtins.open", side_effect=AssertionError("I/O at import")):
            spec.loader.exec_module(module)


if __name__ == "__main__":
    unittest.main()
