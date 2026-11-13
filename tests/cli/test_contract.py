import json
from unittest.mock import Mock, patch

from support import ROOT, IsolatedTestCase, invoke, load_runtime


class LegacyCommandContractTest(IsolatedTestCase):
    def setUp(self):
        self.runtime = load_runtime()
        self.fixture = json.loads((ROOT / "tests/fixtures/cli-v1.json").read_text())

    def json_keys(self, name):
        record = next(row for row in self.fixture["commands"] if row["name"] == name)
        return set(record["stdout"].split(":", 1)[1].split(","))

    def test_registry_and_every_legacy_dispatch(self):
        self.assertEqual(self.fixture["version"], 1)
        self.assertEqual(len(self.fixture["commands"]), 18)
        for row in self.fixture["commands"]:
            name = row["name"]
            self.assertEqual(set(row), {"name", "min_args", "max_args", "stdout", "exit_codes"})
            self.assertTrue({0, 2} <= set(row["exit_codes"]))
            command, low, high = self.runtime.COMMANDS[name]
            self.assertEqual((low, high), (row["min_args"], row["max_args"]))
            for count in range(low, high + 1):
                args = ["operand"] * count
                called = Mock()
                with patch.dict(self.runtime.COMMANDS, {name: (called, low, high)}):
                    self.assertEqual(invoke(self.runtime, [name] + args), (0, "", ""))
                called.assert_called_once_with(*args)
            for count in (low - 1, high + 1):
                if count < 0 or name == "ideas":
                    # ideas bypasses the COMMANDS arity gate in main(): every argument
                    # count routes to its own parser, which rejects invalid ones with
                    # its usage line (covered by IdeaTest through main()).
                    continue
                called = Mock()
                with patch.dict(self.runtime.COMMANDS, {name: (called, low, high)}):
                    code, out, err = invoke(self.runtime, [name] + ["operand"] * count)
                self.assertEqual((code, out), (2, ""))
                self.assertIn("usage", err)
                called.assert_not_called()

    def test_dispatch_preserves_exit_and_streams(self):
        for code in (1, 2, 3, 4, 5, 6):
            def command():
                self.runtime.fail("synthetic failure", code)
            with patch.dict(self.runtime.COMMANDS, {"next": (command, 0, 0)}):
                actual, out, err = invoke(self.runtime, ["next"])
            self.assertEqual((actual, out), (code, ""))
            self.assertEqual(err, "factory.py: synthetic failure\n")

    def test_job_json_retains_unknown_fields(self):
        job = {"id": "001-test", "unknown": {"retained": True}}
        with patch.object(self.runtime, "read_board", return_value={"jobs": [job]}):
            code, out, err = invoke(self.runtime, ["job", "001-test"])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), job)

    def test_real_builds_ideas_stuck_and_next_json_shapes(self):
        import os
        from pathlib import Path
        r = self.runtime
        root = Path(os.environ["HOME"]).resolve()
        with patch.object(r, "read_board", return_value={"jobs": [{"id": "001-test", "project": "sample", "stage": "merged", "build": {"state": "queued"}}]}):
            code, out, err = invoke(r, ["builds", "sample"])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), {"queued": ["001-test"], "in_flight": [], "stale": [], "running": [], "due": True})
        self.assertEqual(set(json.loads(out)), self.json_keys("builds"))
        for name, stage, expected in (("ideas", "idea", {"id", "project", "repo_path", "session"}),
                                      ("stuck", "build", {"id", "project", "repo_path", "session", "stage"})):
            data = {"jobs": [{"id": "001-test", "project": "sample", "stage": stage,
                              "updated": "2000-01-01T00:00:00Z", "idea": "not printed"}]}
            def cas(home, path, change):
                return change(json.dumps(data))[1]
            with patch.object(r, "cas", side_effect=cas), patch.object(r, "read_paths", return_value={"sample": str(root)}), patch.object(r, "holder", return_value=("device", "tag", 120)):
                code, out, err = invoke(r, [name])
            self.assertEqual((code, err), (0, ""))
            self.assertEqual(set(json.loads(out)[0]), expected)
            self.assertEqual(set(json.loads(out)[0]), self.json_keys(name))
            self.assertNotIn("not printed", out)
        def api(home, method, path, **kwargs):
            return 200, {}, '[{"slug":"sample"}]' if path == "/projects.json" else "- [ ] sample: Test\n"
        with patch.object(r, "api", side_effect=api), patch.object(r, "read_paths", return_value={"sample": str(root)}), patch.object(r, "claim_id", return_value="001-test"), patch.object(r, "cas", return_value=True):
            code, out, err = invoke(r, ["next"])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), {"id": "001-test", "project": "sample", "feature": "Test"})
        self.assertEqual(set(json.loads(out)), self.json_keys("next"))

    def test_actual_missing_empty_and_conflicting_commands_keep_exit_codes(self):
        r = self.runtime
        with patch.object(r, "read_board", return_value={"jobs": []}), \
                patch.object(r, "api", return_value=(404, {}, "not found")):
            self.assertEqual(invoke(r, ["job", "001-missing"])[0], 4)
        with patch.object(r, "api", return_value=(200, {}, "[]")), patch.object(r, "read_paths", return_value={}), patch.object(r, "cas", side_effect=lambda home, route, change: change(None)[1]):
            self.assertEqual(invoke(r, ["next"])[0], 3)
            self.assertEqual(invoke(r, ["ideas"])[0], 3)
        with patch.object(r, "api", side_effect=SystemExit(5)):
            self.assertEqual(invoke(r, ["job", "001-missing"])[0], 5)
        with patch.object(r, "r2_target", return_value=(ROOT, "sample", "001-test")), patch.object(r, "safe_job_folder", side_effect=SystemExit(6)):
            self.assertEqual(invoke(r, ["pull", "sample/001-test"])[0], 6)

    def test_session_is_scoped_and_new_tag_uses_stderr(self):
        def command():
            self.runtime.new_session()
            print(json.dumps({"id": "001-test", "project": "sample", "feature": "Test"}))
        with patch.dict(self.runtime.COMMANDS, {"next": (command, 0, 0)}):
            code, out, err = invoke(self.runtime, ["next"])
            self.assertEqual(code, 0)
            self.assertEqual(set(json.loads(out)), {"id", "project", "feature"})
            self.assertIn("new session", err)
            self.assertEqual(invoke(self.runtime, ["next", "--session", "test"])[2], "")


class AddedCommandContractTest(IsolatedTestCase):
    def test_doctor_usage_error_stays_on_stderr(self):
        code, out, err = invoke(load_runtime(), ["doctor", "extra"])
        self.assertEqual((code, out), (2, ""))
        self.assertIn("usage", err)

    def test_init_flags_dispatch_without_changing_legacy_arity(self):
        runtime = load_runtime()
        called = Mock()
        with patch.dict(runtime.COMMANDS, {"init": (called, 0, 1)}):
            self.assertEqual(invoke(runtime, ["init", "--agents", "none", "checkout", "--connection", "descriptor"]), (0, "", ""))
        called.assert_called_once_with("checkout", agents="none", connection="descriptor")

    def test_adoption_prints_only_canonical_path(self):
        import os
        from pathlib import Path
        home = Path(os.environ["HOME"]) / "state"
        home.mkdir()
        (home / "config.md").write_text("---\napi_url:\n---\n")
        runtime = load_runtime()
        self.assertEqual(invoke(runtime, ["adopt-home", str(home)]), (0, str(home.resolve()) + "\n", ""))
        self.assertEqual(invoke(runtime, ["adopt-home"])[0], 2)


class LegacyInventoryTest(IsolatedTestCase):
    def test_all_original_methods_are_mapped_and_present(self):
        import unittest
        import test_factory
        fixture = json.loads((ROOT / "tests/fixtures/cli-v1.json").read_text())
        counts = {"ProjectSyncTest": 11, "FactoryTest": 61, "IdeaTest": 19,
                  "AgentInstallTest": 6, "ResumeInstructionTest": 2, "IdeaImagesTest": 8}
        self.assertEqual({row["id"]: row["count"] for row in fixture["legacy_groups"]}, counts)
        for row in fixture["legacy_groups"]:
            actual = unittest.TestLoader().getTestCaseNames(getattr(test_factory, row["id"]))
            self.assertEqual(len(actual), row["count"])
            self.assertEqual(set(actual), set(row["methods"]))
        self.assertEqual(sum(counts.values()), 107)
