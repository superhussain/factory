import json
import os
import signal
import subprocess
import sys
import tempfile
from pathlib import Path
from unittest import mock

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
JOB = "001-sample-job"


def board(round=None):
    job = {"id": JOB, "project": "sample", "title": "Sample job", "stage": "review"}
    if round is not None:
        job["round"] = round
    return {"updated": "", "jobs": [job]}


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
        self.origin = self.home / "origin.git"
        subprocess.run(["git", "init", "-q", "--bare", str(self.origin)], check=True)
        self.git("remote", "add", "origin", str(self.origin))
        url = mock.patch.object(factory, "PROJECT_FETCH_URL", str(self.origin))
        url.start()
        self.addCleanup(url.stop)
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
        self.git("push", "-q", "-f", "origin", "HEAD:refs/heads/main")

    def run_gate(self, round=None):
        routes = {"/api/board": (200, board(round))}
        with http_fixture(routes) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")
            return invoke(factory, ["--home", str(self.home), "pre-review", f"sample/{JOB}"])

    def test_no_checks_file_is_none(self):
        self.git("push", "-q", "origin", "HEAD:refs/heads/main")
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        result = json.loads(out)
        self.assertEqual((result["result"], result["checks"], result["round"], result["repeats"]), ("none", [], 1, []))
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
        self.assertEqual((result["round"], result["repeats"]), (1, []))
        self.assertEqual([(c["name"], c["exit"]) for c in result["checks"]], [("lint", 0), ("cli", 0)])
        self.assertTrue(all(type(c["seconds"]) is int for c in result["checks"]))
        self.assertEqual(self.logs.stat().st_mode & 0o777, 0o700)
        text = self.out.read_text()
        self.assertTrue(text.startswith("PRE-REVIEW: PASS\n\n"))
        self.assertIn(f"Head: {head}\n", text)
        self.assertIn(f"Checks: origin/main:.factory/checks.json at {head}\n", text)
        self.assertIn("| lint | 0 |", text)
        self.assertIn("| cli | 0 |", text)
        self.assertNotIn("## ", text)

    def test_failure_writes_tail_and_log(self):
        noisy = "import sys\nfor i in range(60): print(f'line {i} ~x~')\nsys.exit(1)"
        self.base([check("format", "pass"), check("lint", noisy)])
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (1, ""))
        self.assertEqual(json.loads(out)["result"], "fail")
        text = self.out.read_text()
        self.assertTrue(text.startswith("PRE-REVIEW: FAIL\n\n"))
        self.assertIn("| lint | 1 |", text)
        self.assertNotIn("## format", text)
        block = text.split("## lint\n\n~~~text\n")[1].split("~~~\n")[0].splitlines()
        self.assertEqual(len(block), 40)
        self.assertEqual((block[0], block[-1]), ("line 20 -x-", "line 59 -x-"))
        self.assertEqual(len((self.logs / "lint.log").read_text().splitlines()), 60)

    def test_a_check_failing_in_two_consecutive_rounds_is_a_repeat(self):
        self.base([check("lint", "pass"), check("unit", "raise SystemExit(1)")])
        code, out, _ = self.run_gate()
        self.assertEqual(code, 1)
        self.assertEqual((json.loads(out)["round"], json.loads(out)["repeats"]), (1, []))
        code, out, _ = self.run_gate(round=1)
        self.assertEqual(code, 1)
        self.assertEqual((json.loads(out)["round"], json.loads(out)["repeats"]), (2, ["unit"]))
        record = self.logs.parent / "pre-review-failures.json"
        self.assertEqual(json.loads(record.read_text()), {"1": ["unit"], "2": ["unit"]})

    def test_failure_classes(self):
        record = self.logs.parent / "pre-review-failures.json"
        rows = (("budget", check("unit", "import time; time.sleep(30)"), "timeout", "budget"),
                ("missing", {"name": "unit", "run": ["factory-test-no-such-program"]}, "missing", "structural"),
                ("127", check("unit", "raise SystemExit(127)"), 127, "structural"),
                ("signal", check("unit", "import os, signal; os.kill(os.getpid(), signal.SIGTERM)"),
                 -signal.SIGTERM, "canceled"),
                ("transient", check("unit", "print('ECONNRESET'); raise SystemExit(1)"), 1, "transient, retried"),
                ("deterministic", check("unit", "raise SystemExit(1)"), 1, "deterministic"))
        for row, unit, exit, cell in rows:
            with self.subTest(row=row):
                if record.exists():
                    record.unlink()
                self.base([check("lint", "pass"), unit])
                with mock.patch.object(factory, "PRE_REVIEW_TIMEOUT", 1):
                    code, out, err = self.run_gate()
                self.assertEqual((code, err), (1, ""))
                lint, result = json.loads(out)["checks"]
                self.assertEqual((lint["class"], lint["retried"]), (None, False))
                self.assertEqual((result["exit"], result["class"], result["retried"]),
                                 (exit, cell.split(",")[0], cell.endswith("retried")))
                text = self.out.read_text()
                self.assertIn("| Check | Exit | Seconds | Class |\n| --- | --- | --- | --- |\n", text)
                self.assertRegex(text, r"\| lint \| 0 \| \d+ \| - \|\n")
                self.assertRegex(text, rf"\| unit \| {exit} \| \d+ \| {cell} \|\n")
                self.assertEqual(record.exists(), row == "deterministic")

    def test_a_transient_failure_is_retried_once(self):
        marker = self.tmp / "deps-ran"
        flaky = (f"import pathlib, sys\nmarker = pathlib.Path({str(marker)!r})\n"
                 "if not marker.exists():\n    marker.touch()\n    print('ECONNRESET')\n    sys.exit(1)\n"
                 "print('installed')")
        self.base([check("deps", flaky), check("unit", "raise SystemExit(1)")])
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (1, ""))
        deps, unit = json.loads(out)["checks"]
        self.assertEqual((deps["exit"], deps["class"], deps["retried"]), (0, None, True))
        self.assertEqual((unit["class"], unit["retried"]), ("deterministic", False))
        self.assertEqual((self.logs / "deps.attempt-1.log").read_text(), "ECONNRESET\n")
        self.assertEqual((self.logs / "deps.log").read_text(), "installed\n")
        self.assertFalse((self.logs / "unit.attempt-1.log").exists())
        self.assertRegex(self.out.read_text(), r"\| deps \| 0 \| \d+ \| -, retried \|\n")

    def test_a_pass_between_does_not_reset_the_repeat(self):
        record = self.logs.parent / "pre-review-failures.json"
        self.base([check("unit", "raise SystemExit(1)")])
        code, out, _ = self.run_gate()
        self.assertEqual((code, json.loads(out)["repeats"]), (1, []))
        self.base([check("unit", "pass"), check("deps", "print('ECONNRESET'); raise SystemExit(1)")])
        code, out, _ = self.run_gate(round=1)
        self.assertEqual((code, json.loads(out)["round"], json.loads(out)["repeats"]), (1, 2, []))
        self.assertEqual(json.loads(record.read_text()), {"1": ["unit"]})
        self.base([check("unit", "raise SystemExit(1)")])
        code, out, _ = self.run_gate(round=2)
        self.assertEqual((code, json.loads(out)["round"], json.loads(out)["repeats"]), (1, 3, ["unit"]))
        self.assertEqual(json.loads(record.read_text()), {"1": ["unit"], "3": ["unit"]})

    def test_two_failures_in_one_round_are_not_a_repeat(self):
        self.base([check("unit", "raise SystemExit(1)")])
        self.run_gate(round=2)
        self.base([check("lint", "raise SystemExit(1)"), check("unit", "pass")])
        code, out, _ = self.run_gate(round=2)
        self.assertEqual(code, 1)
        self.assertEqual((json.loads(out)["round"], json.loads(out)["repeats"]), (3, []))
        record = self.logs.parent / "pre-review-failures.json"
        self.assertEqual(json.loads(record.read_text()), {"3": ["lint", "unit"]})
        record.write_text("[not an object]")
        self.base([check("lint", "raise SystemExit(1)")])
        code, out, _ = self.run_gate(round=3)
        self.assertEqual(json.loads(out)["repeats"], [])
        self.assertEqual(json.loads(record.read_text()), {"4": ["lint"]})
        for bad in ({"4": 5}, {"4": "lint"}, {"4": [5]}):
            with self.subTest(record=bad):
                record.write_text(json.dumps(bad))
                code, out, _ = self.run_gate(round=4)
                self.assertEqual((code, json.loads(out)["repeats"]), (1, []))
                self.assertEqual(json.loads(record.read_text()), {"5": ["lint"]})

    def test_failure_tail_is_redacted(self):
        (self.home / ".env").write_text("FACTORY_TEST_SECRET=synthetic-secret-0123456789\n")  # gitleaks:allow synthetic secret
        leak = ("import sys\nprint('token=synthetic-secret-0123456789')\n"  # gitleaks:allow synthetic secret
                "print('Authorization: Bearer abcdefghijklmnop123456')\nsys.exit(1)")
        self.base([check("leak", leak)])
        code, out, err = self.run_gate()
        self.assertEqual(code, 1)
        text = self.out.read_text()
        self.assertNotIn("synthetic-secret-0123456789", text)
        self.assertNotIn("abcdefghijklmnop123456", text)
        self.assertIn("token=[redacted]", text)
        self.assertIn("Authorization: [redacted]", text)
        self.assertIn("synthetic-secret-0123456789", (self.logs / "leak.log").read_text())

    def test_nested_env_values_are_redacted_longest_first(self):
        (self.home / ".env").write_text("FACTORY_TEST_SHORT=abcdefghijkl\nFACTORY_TEST_LONG=abcdefghijklMNOP\n")
        self.base([check("nested", "import sys\nprint('value abcdefghijklMNOP')\nsys.exit(1)")])
        code, _, _ = self.run_gate()
        self.assertEqual(code, 1)
        text = self.out.read_text()
        self.assertNotIn("MNOP", text)
        self.assertIn("value [redacted]", text)

    def test_end_overlapping_env_values_are_fully_redacted(self):
        (self.home / ".env").write_text("FACTORY_TEST_LEFT=alpha-secret-overlap1234\n"  # gitleaks:allow synthetic
                                        "FACTORY_TEST_RIGHT=overlap1234-beta-secret\n")  # gitleaks:allow synthetic
        printed = "import sys\nprint('value alpha-secret-overlap1234-beta-secret')\nsys.exit(1)"  # gitleaks:allow
        self.base([check("overlap", printed)])
        code, _, _ = self.run_gate()
        self.assertEqual(code, 1)
        text = self.out.read_text()
        self.assertNotIn("secret", text)
        self.assertIn("value [redacted]\n", text)

    def test_timeout_and_missing(self):
        self.base([check("slow", "import time; time.sleep(30)"),
                   {"name": "gone", "run": ["factory-test-no-such-program"]},
                   {"name": "noexec", "run": ["./README.md"]}])
        with mock.patch.object(factory, "PRE_REVIEW_TIMEOUT", 1):
            code, out, err = self.run_gate()
        self.assertEqual((code, err), (1, ""))
        self.assertEqual([(c["name"], c["exit"], c["class"], c["retried"]) for c in json.loads(out)["checks"]],
                         [("slow", "timeout", "budget", False), ("gone", "missing", "structural", False),
                          ("noexec", "missing", "structural", False)])
        text = self.out.read_text()
        self.assertRegex(text, r"\| slow \| timeout \| \d+ \| budget \|\n")
        self.assertRegex(text, r"\| gone \| missing \| \d+ \| structural \|\n")
        self.assertRegex(text, r"\| noexec \| missing \| \d+ \| structural \|\n")
        self.assertFalse((self.logs.parent / "pre-review-failures.json").exists())
        self.assertEqual((self.logs / "gone.log").read_text(), "missing program: factory-test-no-such-program\n")
        self.assertEqual((self.logs / "noexec.log").read_text(), "missing program: ./README.md\n")

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

    def assertNone(self, reason):
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        result = json.loads(out)
        self.assertEqual((result["result"], result["checks"], result["reason"]), ("none", [], reason))
        self.assertIn(f"Reason: {reason}\n", self.out.read_text())
        self.assertFalse(self.logs.exists())

    def test_forged_local_base_is_refetched(self):
        self.base([check("base", "print('base ran')")])
        self.commit([check("forged", "raise SystemExit(3)")])
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual([c["name"] for c in json.loads(out)["checks"]], ["base"])
        self.assertFalse((self.logs / "forged.log").exists())

    def test_factory_base_is_none(self):
        self.base([check("lint", "pass")])
        (self.home / "projects" / "sample.md").write_text("---\nrepo: example/sample\nbase: factory/x\n---\n")
        self.assertNone("base factory/x is a factory branch")

    def test_fetch_failure_is_none(self):
        self.commit([check("lint", "pass")])
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.assertNone("could not fetch origin main")

    def forged(self):
        forged = self.home / "forged.git"
        subprocess.run(["git", "init", "-q", "--bare", str(forged)], check=True)
        self.commit([check("forged", "raise SystemExit(3)")])
        self.git("push", "-q", str(forged), "HEAD:refs/heads/main")
        return forged

    def linked(self):
        checkout = self.home / "checkout"
        self.wt.rename(checkout)
        subprocess.run(["git", "-C", str(checkout), "worktree", "add", "-q", "-b", "job", str(self.wt)], check=True,
                       capture_output=True)

    def test_repointed_origin_is_ignored(self):
        self.base([check("base", "print('base ran')")])
        self.git("remote", "set-url", "origin", str(self.forged()))
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual([c["name"] for c in json.loads(out)["checks"]], ["base"])
        self.assertFalse((self.logs / "forged.log").exists())

    def test_url_rewrite_is_none(self):
        self.base([check("base", "pass")])
        self.git("config", f"url.{self.forged()}.insteadOf", str(self.origin))
        self.assertNone("the worktree's git config sets url or http keys")

    def test_unreadable_git_config_is_none(self):
        self.base([check("base", "pass")])
        (self.wt / ".git" / "config").write_text("[core\n")
        self.assertNone("could not read the worktree's git config")

    def test_bad_repo_is_none(self):
        self.base([check("base", "pass")])
        (self.home / "projects" / "sample.md").write_text("---\nrepo: ../x\nbase: main\n---\n")
        self.assertNone("project file repo '../x' is not owner/name")

    def test_linked_worktree_fetches(self):
        self.base([check("base", "print('base ran')")])
        self.linked()
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual([c["name"] for c in json.loads(out)["checks"]], ["base"])

    def test_worktree_config_rewrite_is_none(self):
        self.base([check("base", "pass")])
        self.linked()
        forged = self.forged()
        self.git("config", "extensions.worktreeConfig", "true")
        self.git("config", "--worktree", f"url.{forged}.insteadOf", str(self.origin))
        self.assertNone("the worktree's git config sets url or http keys")

    def spec(self):
        (self.folder / "spec.md").write_text(SPEC)

    def write(self, sizes):
        for path, n in sizes.items():
            (self.wt / path).parent.mkdir(parents=True, exist_ok=True)
            (self.wt / path).write_text("x\n" * n)
        self.commit()

    def test_budget_over_warns(self):
        self.spec()
        self.base([check("lint", "pass")])
        self.write({"tools/a.py": 300, "tests/b.py": 300, "docs/c.md": 120, "docs/d.md": 120, "src/e.py": 120,
                    "src/f.py": 120, "src/g.py": 120})
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        result = json.loads(out)
        self.assertEqual(result["result"], "pass")
        unlisted = ["docs/c.md", "docs/d.md", "src/e.py", "src/f.py", "src/g.py"]
        self.assertEqual(result["budget"], {"result": "over", "files": 7, "lines": 1200, "expected_files": 2,
                                            "expected_lines": 300, "file_limit": 4, "line_limit": 900,
                                            "unlisted": unlisted, "reason": None})
        text = self.out.read_text()
        head = self.git("rev-parse", "HEAD")
        self.assertIn(f"Head: {head}\nBudget: over (files 7, limit 4; lines 1200, limit 900)\nChecks: ", text)
        section = text.split("## Diff budget\n")[1]
        self.assertIn("| Measure | Spec estimate | Limit | Branch |\n", section)
        self.assertIn("| Files | 2 | 4 | 7 |\n", section)
        self.assertIn("| Lines | 300 | 900 | 1200 |\n", section)
        self.assertIn("Files outside the spec's task list:\n", section)
        self.assertEqual([line for line in section.splitlines() if line.startswith("- ")],
                         [f"- `{p}`" for p in unlisted])

    def test_budget_cuts_unlisted_and_fences_backticks(self):
        self.spec()
        self.base([check("lint", "pass")])
        self.write({**{f"src/m{i:02}.py": 1 for i in range(21)}, "src/a`odd.py": 1})
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(len(json.loads(out)["budget"]["unlisted"]), 22)
        bullets = [line for line in self.out.read_text().splitlines() if line.startswith("- ")]
        self.assertEqual(len(bullets), 21)
        self.assertEqual(bullets[:2], ["- `` src/a`odd.py ``", "- `src/m00.py`"])
        self.assertEqual(bullets[19:], ["- `src/m18.py`", "- and 2 more"])

    def test_budget_fences_backtick_runs(self):
        self.spec()
        self.base([check("lint", "pass")])
        self.write({**{f"src/m{i}.py": 1 for i in range(4)}, "src/b``two.py": 1})
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        bullets = [line for line in self.out.read_text().splitlines() if line.startswith("- ")]
        self.assertEqual(bullets[0], "- ``` src/b``two.py ```")

    def test_budget_survives_non_utf8(self):
        (self.folder / "spec.md").write_bytes(SPEC.encode() + b"\xff\xfe bad\n")
        self.base([check("lint", "pass")])
        non_utf8 = sys.platform != "darwin"  # APFS rejects non-UTF-8 file names
        if non_utf8:
            (self.wt / os.fsdecode(b"bad\xff.py")).write_text("x\n")
        self.write({"tools/a.py": 10})
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        budget = json.loads(out)["budget"]
        files, lines = (2, 11) if non_utf8 else (1, 10)
        self.assertEqual((budget["result"], budget["files"], budget["expected_files"]), ("within", files, 2))
        self.assertIn(f"Budget: within (files {files}, limit 4; lines {lines}, limit 900)\n", self.out.read_text())

    def test_budget_within(self):
        self.spec()
        self.base([check("lint", "pass")])
        self.write({"tools/a.py": 10})
        code, out, err = self.run_gate()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out)["budget"]["result"], "within")
        text = self.out.read_text()
        self.assertIn("Budget: within (files 1, limit 4; lines 10, limit 900)\n", text)
        self.assertNotIn("## ", text)

    def assertBudgetNone(self, reason):
        code, out, err = self.run_gate()
        self.assertEqual(err, "")
        self.assertEqual(json.loads(out)["budget"], {
            "result": "none", "files": 0, "lines": 0, "expected_files": 0, "expected_lines": 0, "file_limit": 0,
            "line_limit": 0, "unlisted": [], "reason": reason})
        self.assertIn(f"\nBudget: none ({reason})\n", self.out.read_text())

    def test_budget_none_reasons(self):
        self.base([check("lint", "pass")])
        self.write({"tools/a.py": 10})
        with self.subTest("no spec"):
            self.assertBudgetNone("the job folder has no spec.md")
        with self.subTest("link"):
            (self.home / "elsewhere.md").write_text(SPEC)
            os.symlink(self.home / "elsewhere.md", self.folder / "spec.md")
            self.assertBudgetNone("spec.md is a symlink or junction")
            os.unlink(self.folder / "spec.md")
        with self.subTest("no paths"):
            (self.folder / "spec.md").write_text("## Tasks\n\n1. Step\n   - Files: `/etc/x` and `../y`\n")
            self.assertBudgetNone("spec.md has no task Files paths")
        self.spec()
        with self.subTest("unrelated base"):
            branch = self.git("rev-parse", "--abbrev-ref", "HEAD")
            self.git("checkout", "-q", "--orphan", "unrelated")
            self.commit()
            self.git("push", "-q", "-f", "origin", "HEAD:refs/heads/main")
            self.git("checkout", "-q", "-f", branch)
            self.assertBudgetNone("the branch diff could not be read")
        with self.subTest("fetch"):
            (self.home / "projects" / "sample.md").write_text("---\nrepo: example/sample\nbase: factory/x\n---\n")
            self.assertBudgetNone("the base could not be fetched")


SPEC = """# Sample

## Design

- Files: `outside/design.py`

## Tasks

1. Tool
   - Files: `tools/a.py`
2. Test
   - Files: `tests/b.py`

## Risks

- None.
"""


class BudgetPathsTest(IsolatedTestCase):
    def test_paths(self):
        text = ("## Design\n\n- Files: `outside.py`\n\n## Tasks\n\n1. One\n   - Files: `tools/*.py`, `docs/`\n"
                "   - Change: `not/a/files/line.py`\n2. Two\n   - Files: `tools/*.py`, `/etc/passwd`, `../up.py`, "
                "`a/../b.py`, `tests/t.py`\n\n## Risks\n\n- Files: `after.py`\n")
        self.assertEqual(factory.spec_budget_paths(text), ["tools/*.py", "docs/", "tests/t.py"])
        self.assertEqual(factory.spec_budget_paths("## Design\n\n- Files: `a.py`\n"), [])

    def test_covers(self):
        self.assertTrue(factory.budget_covers("tools/*.py", "tools/a.py"))
        self.assertFalse(factory.budget_covers("tools/*.py", "tools/a.md"))
        self.assertTrue(factory.budget_covers("docs/", "docs/deep/c.md"))
        self.assertFalse(factory.budget_covers("docs/", "docsx/c.md"))
        self.assertTrue(factory.budget_covers("tests/t.py", "tests/t.py"))
        self.assertFalse(factory.budget_covers("tests/t.py", "tests/t.pyc"))
