import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from unittest import mock

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
JOB = "001-sample-job"


def board():
    return {"updated": "", "jobs": [{"id": JOB, "project": "sample", "title": "Sample job", "stage": "review"}]}


def check(name, code):
    return {"name": name, "run": [sys.executable, "-c", code]}


class PreReviewTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "projects").mkdir()
        (self.home / "projects" / "sample.md").write_text("---\nrepo: example/sample\nbase: main\n---\n# Sample\n")
        self.folder = self.home / "jobs" / "sample" / JOB
        self.folder.mkdir(parents=True)
        self.out = self.folder / "pre-review.md"
        self.tmp = self.home / "tmp"
        self.tmp.mkdir()
        tempdir = mock.patch.object(tempfile, "tempdir", str(self.tmp))
        tempdir.start()
        self.addCleanup(tempdir.stop)
        self.logs = self.tmp / f"factory-sample-{JOB}" / "pre-review"
        self.wt = self.home / "worktrees" / "sample" / JOB
        self.wt.mkdir(parents=True)
        self.git("init", "-q")
        (self.wt / "README.md").write_text("base\n")
        self.commit()

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.wt), *args], check=True, capture_output=True,
                              text=True).stdout.strip()

    def commit(self, checks=None, text=None):
        if checks is not None or text is not None:
            (self.wt / ".factory").mkdir(exist_ok=True)
            (self.wt / ".factory" / "checks.json").write_text(text or json.dumps({"checks": checks}))
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "change")

    def base(self, checks=None, text=None):
        self.commit(checks, text)
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")

    def run_gate(self):
        routes = {"/api/board": (200, board())}
        with http_fixture(routes) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")
            return invoke(factory, ["--home", str(self.home), "pre-review", f"sample/{JOB}"])

    def test_no_checks_file_is_none(self):
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        result = json.loads(out)
        self.assertEqual((result["result"], result["checks"]), ("none", []))
        text = self.out.read_text()
        self.assertTrue(text.startswith("PRE-REVIEW: NONE\n\n"))
        self.assertIn("Reason: origin/main has no .factory/checks.json", text)

    def test_branch_copy_is_never_read(self):
        self.base([check("base", "print('base ran')")])
        self.commit([check("branch", "raise SystemExit(3)")])
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual([c["name"] for c in json.loads(out)["checks"]], ["base"])
        self.assertIn("base ran", (self.logs / "base.log").read_text())
        self.assertFalse((self.logs / "branch.log").exists())

    def test_all_pass(self):
        self.base([check("lint", "pass"), check("cli", "print('ok')")])
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        result = json.loads(out)
        head = self.git("rev-parse", "HEAD")
        self.assertEqual((result["result"], result["head"], result["reason"]), ("pass", head, None))
        self.assertEqual([(c["name"], c["exit"]) for c in result["checks"]], [("lint", 0), ("cli", 0)])
        text = self.out.read_text()
        self.assertTrue(text.startswith("PRE-REVIEW: PASS\n\n"))
        self.assertIn(f"Head: {head}\n", text)
        self.assertIn(f"Checks: origin/main:.factory/checks.json at {head}\n", text)
        self.assertIn("| lint | 0 |", text)
        self.assertIn("| cli | 0 |", text)
        self.assertNotIn("## ", text)

    def test_failure_writes_tail_and_log(self):
        noisy = "import sys\nfor i in range(60): print(f'line {i} `x`')\nsys.exit(1)"
        self.base([check("format", "pass"), check("lint", noisy)])
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (1, ""))
        self.assertEqual(json.loads(out)["result"], "fail")
        text = self.out.read_text()
        self.assertTrue(text.startswith("PRE-REVIEW: FAIL\n\n"))
        self.assertIn("| lint | 1 |", text)
        self.assertNotIn("## format", text)
        block = text.split("## lint\n\n```text\n")[1].split("```\n")[0].splitlines()
        self.assertEqual(len(block), 40)
        self.assertEqual((block[0], block[-1]), ("line 20 'x'", "line 59 'x'"))
        self.assertEqual(len((self.logs / "lint.log").read_text().splitlines()), 60)

    def test_timeout_and_missing(self):
        self.base([check("slow", "import time; time.sleep(30)"),
                   {"name": "gone", "run": ["factory-test-no-such-program"]}])
        with mock.patch.object(factory, "PRE_REVIEW_TIMEOUT", 1):
            code, out, err = self.run_gate()
        self.assertEqual((code, err), (1, ""))
        self.assertEqual([(c["name"], c["exit"]) for c in json.loads(out)["checks"]],
                         [("slow", "timeout"), ("gone", "missing")])
        text = self.out.read_text()
        self.assertIn("| slow | timeout |", text)
        self.assertIn("| gone | missing |", text)

    def test_invalid_checks_file_is_none(self):
        for text, reason in (("{not json", "is not valid JSON"),
                             (json.dumps({"checks": [{"name": "-bad", "run": ["x"]}]}), "unique name"),
                             (json.dumps({"checks": [{"name": "a", "run": []}]}), "run list")):
            with self.subTest(reason=reason):
                self.base(text=text)
                code, out, err = self.run_gate()
                self.assertEqual((code, err), (0, ""))
                result = json.loads(out)
                self.assertEqual(result["result"], "none")
                self.assertIn(reason, result["reason"])
                self.assertIn("Reason: ", self.out.read_text())

    def test_link_refused(self):
        self.base([check("lint", "pass")])
        elsewhere = self.home / "elsewhere.md"
        os.symlink(elsewhere, self.out)
        code, out, err = self.run_gate()
        self.assertEqual((code, out), (6, ""))
        self.assertIn("symlink or junction", err)
        self.assertFalse(elsewhere.exists())
        self.assertFalse(self.logs.exists())
        os.unlink(self.out)
        real = self.home / "jobs" / "sample" / "real"
        self.folder.rename(real)
        os.symlink(real, self.folder)
        code, out, err = self.run_gate()
        self.assertEqual((code, out), (6, ""))
        self.assertEqual(list(real.iterdir()), [])
