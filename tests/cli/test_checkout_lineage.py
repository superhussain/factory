import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

factory = load_runtime()
JOB = "812-dark-mode"


class CheckoutLineageTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.home, self.origin = self.root / "home", self.root / "origin.git"
        self.seed, self.repo = self.root / "seed", self.root / "repo"
        (self.home / "projects").mkdir(parents=True)
        (self.home / "paths.md").write_text(f"---\nfactory: {self.repo}\n---\n", encoding="utf-8")
        self.git(self.root, "init", "-q", "--bare", str(self.origin))
        self.git(self.root, "init", "-q", str(self.seed))
        for repo in (self.origin, self.seed):
            self.git(repo, "symbolic-ref", "HEAD", "refs/heads/main")
        self.commit(self.seed, "public root")
        self.git(self.seed, "remote", "add", "origin", str(self.origin))
        self.git(self.seed, "push", "-q", "origin", "main")
        self.git(self.root, "clone", "-q", str(self.origin), str(self.repo))
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()

    def git(self, cwd, *args):
        return subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=True,
                              check=True).stdout.strip()

    def commit(self, cwd, message):
        self.git(cwd, "commit", "-q", "--allow-empty", "-m", message)
        return self.git(cwd, "rev-parse", "HEAD")

    def archive_root(self):
        """An unrelated root commit on branch archive in the clone, as a stale host holds."""
        self.git(self.repo, "checkout", "-q", "--orphan", "archive")
        root = self.commit(self.repo, "archive root")
        self.git(self.repo, "checkout", "-q", "main")
        return root

    def foreign(self, ref, root):
        return (f"unsafe checkout: {self.repo} {ref} has root {root[:12]}, which origin/main lacks, "
                'so it holds archive history; repoint it as docs/public-cutover.md "Other hosts" says')

    def problem(self, branch=None):
        return factory.checkout_problem(self.home, "factory", str(self.repo), branch)

    def test_clean_clone_is_safe(self):
        self.assertIsNone(self.problem())
        self.assertIsNone(self.problem(f"factory/{JOB}"))

    def test_foreign_root_on_local_main(self):
        root = self.archive_root()
        self.git(self.repo, "checkout", "-q", "archive")
        self.git(self.repo, "branch", "-f", "main", "archive")
        self.assertEqual(self.problem(), self.foreign("main", root))

    def test_merge_of_both_histories(self):
        root = self.archive_root()
        self.git(self.repo, "merge", "-q", "--allow-unrelated-histories", "-m", "merge", "archive")
        self.assertEqual(self.problem(), self.foreign("main", root))

    def test_rewritten_origin_is_caught_through_the_fetch(self):
        old = self.git(self.repo, "rev-parse", "main")
        self.git(self.seed, "checkout", "-q", "--orphan", "public")
        self.commit(self.seed, "rewritten root")
        self.git(self.seed, "push", "-q", "-f", "origin", "public:main")
        self.assertEqual(self.git(self.repo, "rev-parse", "refs/remotes/origin/main"), old)
        self.assertEqual(self.problem(), self.foreign("main", old))

    def test_missing_local_base(self):
        self.git(self.repo, "checkout", "-q", "-b", "other")
        self.git(self.repo, "branch", "-q", "-D", "main")
        self.assertEqual(self.problem(), f"unsafe checkout: {self.repo} has no local main branch; "
                                         f"run git -C {self.repo} branch main origin/main")

    def test_failed_fetch_leaves_the_url_out(self):
        self.git(self.repo, "remote", "set-url", "origin", str(self.root / "secret-token" / "gone.git"))
        line = self.problem()
        self.assertRegex(line, rf"^unsafe checkout: git fetch origin main failed in {self.repo} \(exit \d+\)$")
        self.assertNotIn("secret-token", line)

    def test_unsafe_base(self):
        for base in ("-x", "../x", "a b"):
            with self.subTest(base=base):
                (self.home / "projects" / "factory.md").write_text(f"---\nbase: {base}\n---\n", encoding="utf-8")
                self.assertEqual(self.problem(), f"unsafe checkout: factory base {base} is not a safe branch name")

    def test_project_base_is_used(self):
        (self.home / "projects" / "factory.md").write_text("---\nbase: trunk\n---\n", encoding="utf-8")
        self.assertEqual(self.problem(), "unsafe checkout: git fetch origin trunk failed in "
                                         f"{self.repo} (exit 128)")

    def worktree(self):
        branches = self.git(self.repo, "branch", "--list")
        result = invoke(factory, ["worktree", f"factory/{JOB}"])
        self.assertFalse((self.home / "worktrees").exists())
        self.assertEqual(self.git(self.repo, "branch", "--list"), branches)
        return result

    def test_worktree_stops_on_a_bad_base(self):
        (self.home / "projects" / "factory.md").write_text("---\nbase: -x\n---\n", encoding="utf-8")
        self.assertEqual(self.worktree(),
                         (6, "", "factory.py: unsafe checkout: factory base -x is not a safe branch name\n"))

    def test_worktree_stops_on_a_foreign_job_branch(self):
        root = self.archive_root()
        self.git(self.repo, "branch", f"factory/{JOB}", "archive")
        self.assertEqual(self.worktree(), (6, "", f"factory.py: {self.foreign(f'factory/{JOB}', root)}\n"))

    def test_worktree_passes_a_clean_clone_to_the_branch_lookup(self):
        self.assertEqual(self.worktree(), (4, "", f"factory.py: no factory/{JOB} branch here or on origin\n"))


if __name__ == "__main__":
    unittest.main()
