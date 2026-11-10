import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime


factory = load_runtime()


class CleanupTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name)
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory, "worktree_pids", return_value=[]).start()
        self.repo = self.home / "checkout"
        self.repo.mkdir()
        (self.home / "paths.md").write_text(f"---\nsample: {self.repo}\n---\n")
        self.git(self.repo, "init", "-q")
        self.git(self.repo, "symbolic-ref", "HEAD", "refs/heads/main")
        self.git(self.repo, "commit", "-q", "--allow-empty", "-m", "base")
        self.wt = self.home / "worktrees" / "sample" / "001-test"
        self.git(self.repo, "worktree", "add", "-q", str(self.wt), "-b", "factory/001-test")
        (self.wt / "work.txt").write_text("work\n")
        self.git(self.wt, "add", "work.txt")
        self.git(self.wt, "commit", "-q", "-m", "work")

    def git(self, cwd, *args):
        return subprocess.run(["git", "-C", str(cwd), *args], check=True,
                              capture_output=True, text=True).stdout.strip()

    def merge(self):
        self.git(self.repo, "merge", "-q", "--no-ff", "-m", "merge", "factory/001-test")

    def push(self):
        origin = self.home / "origin.git"
        self.git(self.home, "init", "-q", "--bare", str(origin))
        self.git(self.wt, "remote", "add", "origin", str(origin))
        self.git(self.wt, "push", "-q", "-u", "origin", "factory/001-test")

    def cleanup(self, target="sample/001-test"):
        code, out, err = invoke(factory, ["cleanup", target])
        return code, (json.loads(out) if out else None), err

    def branch_exists(self):
        return bool(self.git(self.repo, "branch", "--list", "factory/001-test"))

    def git_raises(self, verb, error):
        real_run, seen = subprocess.run, []

        def run(argv, *rest, **kwargs):
            if argv[0] == "git":
                seen.append(kwargs.get("timeout"))
                if verb is None or argv[3] == verb:
                    raise error
            return real_run(argv, *rest, **kwargs)

        with patch.object(factory.subprocess, "run", side_effect=run):
            code, out, _ = self.cleanup()
        return code, out, seen

    def test_merged_job_is_removed(self):
        self.merge()
        code, out, _ = self.cleanup()
        self.assertEqual(code, 0)
        self.assertEqual(out, {"removed": [f"worktree {self.wt}", "branch factory/001-test"], "left": []})
        self.assertFalse(self.wt.exists())
        self.assertFalse(self.branch_exists())

    def test_unmerged_branch_is_kept(self):
        code, out, _ = self.cleanup()
        self.assertEqual(code, 1)
        self.assertEqual(out["removed"], [f"worktree {self.wt}"])
        self.assertEqual(out["left"], ["branch factory/001-test: not merged into main"])
        self.assertTrue(self.branch_exists())

    def test_pushed_unmerged_branch_is_kept(self):
        self.push()
        code, out, _ = self.cleanup()
        self.assertEqual(code, 1)
        self.assertEqual(out["left"], ["branch factory/001-test: not merged into main"])
        self.assertTrue(self.branch_exists())
        self.assertEqual(self.git(self.repo, "config", "--get", "branch.factory/001-test.merge"),
                         "refs/heads/factory/001-test")

    def test_pushed_merged_branch_is_removed(self):
        self.merge()
        self.push()
        code, out, _ = self.cleanup()
        self.assertEqual(code, 0)
        self.assertEqual(out, {"removed": [f"worktree {self.wt}", "branch factory/001-test"], "left": []})
        self.assertFalse(self.branch_exists())

    def test_empty_stderr_still_prints_json(self):
        real_run = subprocess.run

        def run(args, *rest, **kwargs):
            if "remove" in args:
                return subprocess.CompletedProcess(args, 1, "", "")
            return real_run(args, *rest, **kwargs)

        with patch.object(factory.subprocess, "run", side_effect=run):
            code, out, _ = self.cleanup()
        self.assertEqual(code, 1)
        self.assertEqual(out["left"][0], f"worktree {self.wt}: git exited 1")

    def test_dirty_worktree_is_kept(self):
        self.merge()
        (self.wt / "untracked.txt").write_text("keep me\n")
        code, out, _ = self.cleanup()
        self.assertEqual(code, 1)
        self.assertEqual(out["removed"], [])
        self.assertEqual(len(out["left"]), 2)
        self.assertTrue(out["left"][0].startswith(f"worktree {self.wt}: fatal: "))
        self.assertNotIn("--force", out["left"][0])
        self.assertTrue(out["left"][0].endswith("contains modified or untracked files"))
        self.assertEqual(out["left"][1], "branch factory/001-test: kept because its worktree remains")
        self.assertEqual((self.wt / "untracked.txt").read_text(), "keep me\n")
        self.assertTrue(self.branch_exists())

    def test_running_process_keeps_everything(self):
        self.merge()
        with patch.object(factory, "stop_pids", return_value=[4242]):
            code, out, _ = self.cleanup()
        self.assertEqual(code, 1)
        self.assertEqual(out, {"removed": [], "left": [
            f"worktree {self.wt}: processes still running: 4242",
            "branch factory/001-test: kept because its worktree remains"]})
        self.assertTrue(self.wt.exists())
        self.assertTrue(self.branch_exists())

    def test_missing_worktree_is_pruned(self):
        self.merge()
        shutil.rmtree(self.wt)
        code, out, _ = self.cleanup()
        self.assertEqual(code, 0)
        self.assertEqual(out, {"removed": ["branch factory/001-test"], "left": []})
        self.assertNotIn(str(self.wt), self.git(self.repo, "worktree", "list"))
        self.assertFalse(self.branch_exists())

    def test_every_git_call_has_timeout(self):
        self.merge()
        self.push()
        code, _, seen = self.git_raises("no-such-verb", None)
        self.assertEqual(code, 0)
        self.assertTrue(seen)
        self.assertEqual(set(seen), {20})

    def test_git_times_out(self):
        code, out, _ = self.git_raises("worktree", subprocess.TimeoutExpired(["git"], 20))
        self.assertEqual(code, 1)
        self.assertEqual(out, {"removed": [], "left": [
            f"worktree {self.wt}: git worktree timed out after 20 s",
            "branch factory/001-test: kept because its worktree remains"]})
        self.assertTrue(self.wt.exists())

    def test_git_not_runnable(self):
        code, out, _ = self.git_raises(None, FileNotFoundError("git"))
        self.assertEqual(code, 1)
        self.assertEqual(out, {"removed": [], "left": [
            f"worktree {self.wt}: could not run git worktree",
            "branch factory/001-test: kept because its worktree remains"]})
        self.assertTrue(self.wt.exists())

    def test_branch_check_times_out(self):
        self.merge()
        code, out, _ = self.git_raises("merge-base", subprocess.TimeoutExpired(["git"], 20))
        self.assertEqual(code, 1)
        self.assertEqual(out, {"removed": [f"worktree {self.wt}"],
                               "left": ["branch factory/001-test: git merge-base timed out after 20 s"]})
        self.assertTrue(self.branch_exists())

    def test_branch_lookup_not_runnable(self):
        self.merge()
        code, out, _ = self.git_raises("rev-parse", OSError())
        self.assertEqual(code, 1)
        self.assertEqual(out["left"], ["branch factory/001-test: could not run git rev-parse"])
        self.assertTrue(self.branch_exists())

    def test_bad_target_and_unknown_project(self):
        code, out, err = self.cleanup("bad")
        self.assertEqual(code, 2)
        self.assertIn("bad target", err)
        code, out, err = self.cleanup("other/001-test")
        self.assertEqual(code, 4)
        self.assertIn("has no entry for other", err)


if __name__ == "__main__":
    unittest.main()
