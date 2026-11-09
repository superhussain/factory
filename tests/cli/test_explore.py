import contextlib
import io
import json
import subprocess
from unittest.mock import patch

import test_suggest
from support import IsolatedTestCase

factory = test_suggest.factory
RUN = subprocess.run


def git(cwd, *args):
    return RUN(["git", "-C", str(cwd), *args], check=True, capture_output=True, text=True).stdout.strip()


class ExploreTest(IsolatedTestCase):
    request = test_suggest.SuggestTest.request

    def setUp(self):
        test_suggest.SuggestTest.setUp(self)
        patch.object(factory.subprocess, "run", RUN).start()
        seed, origin, self.repo = self.home / "seed", self.home / "origin.git", self.home / "repo"
        git(self.home, "init", "-q", "--bare", str(origin))
        git(self.home, "init", "-q", str(seed))
        for path in (origin, seed):
            git(path, "symbolic-ref", "HEAD", "refs/heads/main")
        (seed / "a.txt").write_text("a\n")
        (seed / ".env.example").write_text("KEY=\n")
        (seed / "link").symlink_to("/etc/hosts")
        git(seed, "add", "-A")
        git(seed, "commit", "-q", "-m", "one")
        git(seed, "push", "-q", str(origin), "main")
        git(self.home, "clone", "-q", str(origin), str(self.repo))
        # origin moves after the clone, so the run has to fetch to see it.
        (seed / "b.txt").write_text("b\n")
        git(seed, "add", "-A")
        git(seed, "commit", "-q", "-m", "two")
        git(seed, "push", "-q", str(origin), "main")
        self.head = git(origin, "rev-parse", "main")
        patch.object(factory, "read_paths", return_value={"sample": str(self.repo)}).start()
        patch.object(factory, "pull_project", return_value=False).start()
        self.base("main")

    def base(self, value):
        (self.home / "projects").mkdir(exist_ok=True)
        (self.home / "projects" / "sample.md").write_text(f"---\nrepo: x/sample\nbase: {value}\n---\n# sample\n")

    def explore(self, *args, code=0):
        self.output.seek(0)
        self.output.truncate()
        with contextlib.redirect_stderr(io.StringIO()) as err:
            try:
                factory.explore(*args)
                got = 0
            except SystemExit as e:
                got = e.code
        self.assertEqual(got, code, err.getvalue())
        return err.getvalue()

    def test_explore_refuses(self):
        cases = [({"paused": True, "projects": ["sample"], "self": []}, "sample",
                  "autonomy is paused; no explorer run"),
                 (test_suggest.ALLOWED, "other", "other is not on the autonomy allowlist; no explorer run")]
        for autonomy, slug, message in cases:
            with self.subTest(slug=slug):
                self.board["autonomy"] = dict(autonomy)
                self.assertIn(message, self.explore(slug, code=3))
        self.assertFalse((self.home / "explore").exists())

    def test_explore_snapshot(self):
        self.explore("sample")
        out = json.loads(self.output.getvalue())
        self.assertEqual(list(out), ["project", "run", "commit", "checkout", "out"])
        self.assertEqual((out["project"], out["commit"]), ("sample", self.head))
        self.assertTrue(factory.RUN_TAG.fullmatch(out["run"]))
        checkout = self.home / "explore" / "sample" / out["run"]
        self.assertEqual((out["checkout"], out["out"]), (str(checkout), str(checkout) + ".json"))
        self.assertEqual(sorted(p.name for p in checkout.iterdir()), [".env.example", "a.txt", "b.txt"])
        self.assertEqual((checkout / "a.txt").read_text(), "a\n")
        self.assertFalse(any(p.is_symlink() or p.name == ".git" for p in checkout.rglob("*")))
        self.assertFalse((checkout.parent / (out["run"] + ".json")).exists())

    def test_explore_done(self):
        self.explore("sample")
        out = json.loads(self.output.getvalue())
        record = self.home / "explore" / "sample" / (out["run"] + ".json")
        record.write_text("{}")
        self.explore("sample", "done", out["run"])
        self.assertEqual(self.output.getvalue(), f"removed {out['checkout']}\n")
        self.assertFalse((self.home / "explore" / "sample" / out["run"]).exists())
        self.assertTrue(record.exists())
        self.assertIn(f"no explorer checkout sample/{out['run']}", self.explore("sample", "done", out["run"], code=4))
        for args in (("done", "a b"), ("done", "../x"), ("done",), ("gone", "run"), ("done", "r", "x")):
            with self.subTest(args=args):
                self.assertIn("usage: explore <slug> | explore <slug> done <run>", self.explore("sample", *args, code=2))
        self.explore("bad slug", code=2)

    def test_explore_done_refuses_linked_parents(self):
        elsewhere = self.home / "elsewhere"
        (elsewhere / "sample" / "run-1").mkdir(parents=True)
        for link, target in ((self.home / "explore", elsewhere), (self.home / "explore" / "sample", elsewhere / "sample")):
            with self.subTest(link=link.name):
                if link.name == "sample":
                    (self.home / "explore").mkdir()
                link.symlink_to(target)
                self.assertIn("no explorer checkout sample/run-1", self.explore("sample", "done", "run-1", code=4))
                self.assertTrue((elsewhere / "sample" / "run-1").is_dir())
                link.unlink()

    def test_explore_bad_base(self):
        for value in ("-x", "../main", "a b"):
            with self.subTest(base=value), patch.object(factory, "origin_git") as fetch:
                self.base(value)
                self.assertIn(f"sample's base {value} is not a safe branch name", self.explore("sample", code=2))
                fetch.assert_not_called()
        self.assertFalse((self.home / "explore").exists())
