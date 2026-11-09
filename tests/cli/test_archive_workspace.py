import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime


factory = load_runtime()

AGENT = "agent-1"
CWD = "/work/worktrees/x/factory-042-dark-mode"

FAKE_PASEO = """#!/usr/bin/env python3
import json, os, sys
root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
with open(os.path.join(root, "paseo-fixture.json"), encoding="utf-8") as f:
    conf = json.load(f)
path = os.path.join(root, "paseo-calls.json")
calls = []
if os.path.exists(path):
    with open(path, encoding="utf-8") as f:
        calls = json.load(f)
calls.append(sys.argv[1:])
with open(path, "w", encoding="utf-8") as f:
    json.dump(calls, f)
args = sys.argv[1:]
if args[:1] == ["--home"]:
    args = args[2:]
if args[:1] == ["inspect"]:
    print(json.dumps(conf.get("inspect", {})))
    sys.exit(conf.get("inspect_code", 0))
if args[:2] == ["workspace", "ls"]:
    print(json.dumps(conf.get("workspaces", [])))
    sys.exit(conf.get("ls_code", 0))
if args[:2] == ["workspace", "archive"]:
    sys.exit(conf.get("archive_code", 0))
sys.exit(9)
"""


@unittest.skipIf(os.name == "nt", "the fake paseo script needs a POSIX bin directory on PATH")
class ArchiveWorkspaceTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.root / "home").start()
        self.board = patch.object(factory, "read_board", return_value={"jobs": [
            {"id": "042-dark-mode", "stage": "merged"}, {"id": "043-other", "stage": "building"}]}).start()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        fake = self.bin / "paseo"
        fake.write_text(FAKE_PASEO, encoding="utf-8")
        fake.chmod(0o755)
        self.paseo_home = self.root / "paseo-home"
        self.paseo_home.mkdir()
        (self.paseo_home / "server-id").write_text("srv_1\n")
        os.environ["PATH"] = f"{self.bin}{os.pathsep}{os.environ.get('PATH', '')}"
        os.environ["PASEO_HOME"] = str(self.paseo_home)
        os.environ["PASEO_AGENT_ID"] = AGENT
        self.fixture(workspaces=[{"workspaceId": "wks_0a1b", "cwd": CWD, "isolation": "worktree"}])

    def fixture(self, **conf):
        conf.setdefault("inspect", {"Cwd": CWD})
        (self.root / "paseo-fixture.json").write_text(json.dumps(conf), encoding="utf-8")

    def calls(self):
        path = self.root / "paseo-calls.json"
        return json.loads(path.read_text(encoding="utf-8")) if path.exists() else []

    def run_verb(self, target="sample/042-dark-mode"):
        code, out, err = invoke(factory, ["archive-workspace", target])
        return code, (json.loads(out) if out else None), err

    def test_bad_target(self):
        code, out, err = self.run_verb("sample")
        self.assertEqual((code, out), (2, None))
        self.assertIn("bad target", err)
        self.assertEqual(self.calls(), [])

    def test_not_a_paseo_session(self):
        del os.environ["PASEO_AGENT_ID"]
        self.assertEqual(self.run_verb(), (0, {"archived": None, "reason": "not a Paseo session"}, ""))
        (self.paseo_home / "server-id").unlink()
        os.environ["PASEO_AGENT_ID"] = AGENT
        self.assertEqual(self.run_verb(), (0, {"archived": None, "reason": "not a Paseo session"}, ""))
        self.assertEqual(self.calls(), [])
        self.board.assert_not_called()

    def test_paseo_not_on_path(self):
        os.environ["PATH"] = str(self.root / "empty")
        self.assertEqual(self.run_verb(), (0, {"archived": None, "reason": "paseo is not on PATH"}, ""))
        self.board.assert_not_called()

    def test_job_not_on_board(self):
        code, out, err = self.run_verb("sample/044-missing")
        self.assertEqual((code, out), (4, None))
        self.assertIn("no job 044-missing on the board", err)
        self.assertEqual(self.calls(), [])

    def test_job_not_merged(self):
        code, out, err = self.run_verb("sample/043-other")
        self.assertEqual((code, out), (2, None))
        self.assertIn("043-other is at building, not merged", err)
        self.assertEqual(self.calls(), [])

    def test_inspect_fails(self):
        self.fixture(inspect_code=1)
        code, out, err = self.run_verb()
        self.assertEqual((code, out), (1, None))
        self.assertIn("paseo inspect failed", err)

    def test_workspace_ls_fails(self):
        self.fixture(ls_code=1)
        code, out, err = self.run_verb()
        self.assertEqual((code, out), (1, None))
        self.assertIn("paseo workspace ls failed", err)

    def test_no_workspace_at_cwd(self):
        self.fixture(workspaces=[{"workspaceId": "wks_9", "cwd": "/elsewhere", "isolation": "worktree"}])
        self.assertEqual(self.run_verb(), (0, {"archived": None, "reason": f"no Paseo workspace at {CWD}"}, ""))
        self.assertEqual([c[2] for c in self.calls()], ["inspect", "workspace"])

    def test_workspace_not_this_jobs_worktree(self):
        reason = {"archived": None, "reason": "workspace wks_0a1b is not this job's worktree workspace"}
        for cwd, isolation in ((CWD, "local"), ("/work/factory-042-dark-mode-x", "worktree"),
                               ("/work/factory-042-dark", "worktree")):
            self.fixture(inspect={"Cwd": cwd},
                         workspaces=[{"workspaceId": "wks_0a1b", "cwd": cwd, "isolation": isolation}])
            self.assertEqual(self.run_verb(), (0, reason, ""))
        self.assertNotIn("archive", [c[3] for c in self.calls() if len(c) > 3])

    def test_archive_fails(self):
        self.fixture(archive_code=3,
                     workspaces=[{"workspaceId": "wks_0a1b", "cwd": CWD, "isolation": "worktree"}])
        code, out, err = self.run_verb()
        self.assertEqual((code, out), (1, {"archived": "wks_0a1b"}))
        self.assertIn("paseo workspace archive wks_0a1b exit 3", err)

    def test_archived(self):
        cwd = CWD + "-2/"
        self.fixture(inspect={"Cwd": cwd}, workspaces=[
            {"workspaceId": "wks_9", "cwd": "/elsewhere", "isolation": "worktree"},
            {"workspaceId": "wks_0a1b", "cwd": CWD + "-2", "isolation": "worktree"}])
        self.assertEqual(self.run_verb(), (0, {"archived": "wks_0a1b"}, ""))
        home = ["--home", str(self.paseo_home)]
        self.assertEqual(self.calls(), [home + ["inspect", AGENT, "--json"],
                                        home + ["workspace", "ls", "--json"],
                                        home + ["workspace", "archive", "wks_0a1b"]])


if __name__ == "__main__":
    unittest.main()
