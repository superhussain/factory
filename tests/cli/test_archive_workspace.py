import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime


factory = load_runtime()

AGENT = "agent-1"

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
        self.git("init", "-q", "--bare", str(self.root / "remote.git"))
        self.cwd = self.clone("factory-042-dark-mode")
        self.fixture(workspaces=[{"workspaceId": "wks_0a1b", "cwd": self.cwd, "isolation": "worktree"}])

    def git(self, *args):
        return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout

    def clone(self, name):
        path = self.root / "worktrees" / "x" / name
        self.git("clone", "-q", str(self.root / "remote.git"), str(path))
        (path / "README").write_text(f"{name}\n")
        self.git("-C", str(path), "add", "README")
        self.git("-C", str(path), "commit", "-qm", "init")
        self.git("-C", str(path), "push", "-q", "origin", "HEAD")
        return str(path)

    def fixture(self, **conf):
        conf.setdefault("inspect", {"Cwd": self.cwd})
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
        self.assertIn("paseo inspect failed: exit 1", err)

    def test_workspace_ls_fails(self):
        self.fixture(ls_code=1)
        code, out, err = self.run_verb()
        self.assertEqual((code, out), (1, None))
        self.assertIn("paseo workspace ls failed: exit 1", err)

    def test_no_workspace_at_cwd(self):
        self.fixture(workspaces=[{"workspaceId": "wks_9", "cwd": "/elsewhere", "isolation": "worktree"}])
        self.assertEqual(self.run_verb(), (0, {"archived": None, "reason": f"no Paseo workspace at {self.cwd}"}, ""))
        self.assertEqual([c[2] for c in self.calls()], ["inspect", "workspace"])

    def test_workspace_not_this_jobs_worktree(self):
        reason = {"archived": None, "reason": "workspace wks_0a1b is not this job's worktree workspace"}
        for cwd, isolation in ((self.cwd, "local"), ("/work/factory-042-dark-mode-x", "worktree"),
                               ("/work/factory-042-dark", "worktree")):
            self.fixture(inspect={"Cwd": cwd},
                         workspaces=[{"workspaceId": "wks_0a1b", "cwd": cwd, "isolation": isolation}])
            self.assertEqual(self.run_verb(), (0, reason, ""))
        self.assertNotIn("archive", [c[3] for c in self.calls() if len(c) > 3])

    def test_bad_workspace_id(self):
        for wks in (None, 7, "-rf", "a b"):
            self.fixture(workspaces=[{"workspaceId": wks, "cwd": self.cwd, "isolation": "worktree"}])
            self.assertEqual(self.run_verb(), (0, {
                "archived": None, "reason": f"workspace {wks} is not this job's worktree workspace"}, ""))
        self.assertNotIn("archive", [c[3] for c in self.calls() if len(c) > 3])

    def test_agent_id_starting_with_dash(self):
        os.environ["PASEO_AGENT_ID"] = "-x"
        self.assertEqual(self.run_verb(), (0, {"archived": None, "reason": "not a Paseo session"}, ""))
        self.assertEqual(self.calls(), [])

    def archived(self, leftovers):
        self.assertEqual(self.run_verb(), (0, {"archived": "wks_0a1b", "leftovers": leftovers}, ""))
        self.assertIn("archive", [c[3] for c in self.calls() if len(c) > 3])

    def test_uncommitted_files(self):
        Path(self.cwd, "notes.txt").write_text("draft\n")
        self.archived(["uncommitted: ?? notes.txt"])

    def test_unpushed_commits(self):
        self.git("-C", self.cwd, "commit", "-q", "--allow-empty", "-m", "local")
        self.archived([f"commit: {self.git('-C', self.cwd, 'log', '-1', '--format=%h local').strip()}"])

    def test_branch_without_upstream(self):
        self.git("-C", self.cwd, "switch", "-q", "-c", "factory/042-dark-mode")
        self.git("-C", self.cwd, "commit", "-q", "--allow-empty", "-m", "local")
        self.archived([f"commit: {self.git('-C', self.cwd, 'log', '-1', '--format=%h local').strip()}"])

    def test_stale_local_main(self):
        self.git("-C", self.cwd, "commit", "-q", "--allow-empty", "-m", "stale")
        self.git("-C", self.cwd, "switch", "-q", "-c", "factory/042-dark-mode")
        self.archived([])

    def test_git_fails(self):
        cwd = str(self.root / "factory-042-dark-mode")
        os.mkdir(cwd)
        self.fixture(inspect={"Cwd": cwd},
                     workspaces=[{"workspaceId": "wks_0a1b", "cwd": cwd, "isolation": "worktree"}])
        self.archived(["git status failed: exit 128", "git branch failed: exit 128"])

    def test_check_only(self):
        Path(self.cwd, "notes.txt").write_text("draft\n")
        code, out, err = invoke(factory, ["archive-workspace", "sample/042-dark-mode", "--check"])
        self.assertEqual((code, json.loads(out), err), (0, {
            "archived": None, "would_archive": "wks_0a1b", "leftovers": ["uncommitted: ?? notes.txt"]}, ""))
        self.assertNotIn("archive", [c[3] for c in self.calls() if len(c) > 3])

    def test_bad_flag(self):
        code, out, err = invoke(factory, ["archive-workspace", "sample/042-dark-mode", "--bogus"])
        self.assertEqual((code, out), (2, ""))
        self.assertIn("usage: archive-workspace <project-slug>/<job-id> [--check]", err)
        self.assertEqual(self.calls(), [])

    def test_leftover_cap(self):
        for n in range(25):
            Path(self.cwd, f"f{n:02}.txt").write_text("x\n")
        code, out, err = self.run_verb()
        self.assertEqual(code, 0)
        self.assertEqual(out["leftovers"][19], "uncommitted: ?? f19.txt")
        self.assertEqual(out["leftovers"][20:], ["and 5 more"])
        self.assertEqual(len(out["leftovers"]), 21)

    def git_raises(self, error):
        cwd = str(self.root / "factory-042-dark-mode")
        os.mkdir(cwd)
        self.fixture(inspect={"Cwd": cwd},
                     workspaces=[{"workspaceId": "wks_0a1b", "cwd": cwd, "isolation": "worktree"}])
        real, seen = subprocess.run, {}

        def fake(argv, *a, **k):
            if argv[:2] == ["git", "-C"]:
                seen.update(k)
                raise error(argv)
            return real(argv, *a, **k)

        patch.object(factory.subprocess, "run", side_effect=fake).start()
        return cwd, seen

    def test_git_times_out(self):
        _, seen = self.git_raises(lambda argv: subprocess.TimeoutExpired(argv, 20))
        self.assertEqual(self.run_verb(), (0, {
            "archived": "wks_0a1b", "leftovers": ["git status failed: timed out after 20 s",
                                                  "git branch failed: timed out after 20 s"]}, ""))
        self.assertEqual(seen["timeout"], 20)
        self.assertIn("archive", [c[3] for c in self.calls() if len(c) > 3])

    def test_git_not_runnable(self):
        self.git_raises(lambda argv: FileNotFoundError(2, "No such file"))
        self.assertEqual(self.run_verb(), (0, {
            "archived": "wks_0a1b", "leftovers": ["git status failed: could not run git",
                                                  "git branch failed: could not run git"]}, ""))
        self.assertIn("archive", [c[3] for c in self.calls() if len(c) > 3])

    def test_archive_fails(self):
        self.fixture(archive_code=3,
                     workspaces=[{"workspaceId": "wks_0a1b", "cwd": self.cwd, "isolation": "worktree"}])
        code, out, err = self.run_verb()
        self.assertEqual((code, out), (1, {"archived": "wks_0a1b", "leftovers": []}))
        self.assertIn("paseo workspace archive wks_0a1b exit 3", err)

    def test_archived(self):
        cwd = self.clone("factory-042-dark-mode-2")
        self.fixture(inspect={"Cwd": cwd + "/"}, workspaces=[
            {"workspaceId": "wks_9", "cwd": "/elsewhere", "isolation": "worktree"},
            {"workspaceId": "wks_0a1b", "cwd": cwd, "isolation": "worktree"}])
        self.assertEqual(self.run_verb(), (0, {"archived": "wks_0a1b", "leftovers": []}, ""))
        home = ["--home", str(self.paseo_home)]
        self.assertEqual(self.calls(), [home + ["inspect", AGENT, "--json"],
                                        home + ["workspace", "ls", "--json"],
                                        home + ["workspace", "archive", "wks_0a1b"]])


if __name__ == "__main__":
    unittest.main()
