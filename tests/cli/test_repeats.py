import json
import shutil
import subprocess
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

CODE = ("VERDICT: CHANGES\n\nThe export route skips the person check.\n"
        "Signature: worker/export.js#missing-person-check\n\nNit: rename `rowsToCsv`.\n")
PASS = "VERDICT: PASS\n\nNothing to change.\n"
REPEAT = {"decision": "repeat", "round": 2, "previous": 1,
          "repeats": [{"reviewer": "code", "signature": "worker/export.js#missing-person-check", "first": 1}],
          "churn": []}
CONTINUE = {"decision": "continue", "round": 2, "previous": 1, "repeats": [], "churn": []}


class RepeatsTest(IsolatedTestCase):
    def setUp(self):
        self.module = load_runtime()
        self.dir = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: shutil.rmtree(self.dir, ignore_errors=True))
        self.job = self.dir / "jobs" / "sample" / "042-csv-export"
        for n in (1, 2):
            self.write(n, code=CODE)
        patcher = patch.object(self.module, "factory_home", lambda: self.dir)
        patcher.start()
        self.addCleanup(patcher.stop)

    def write(self, n, **texts):
        folder = self.job / f"round-{n}"
        folder.mkdir(parents=True, exist_ok=True)
        for name in ("security", "ux", "ui", "code"):
            (folder / f"review-{name}.md").write_text(texts.get(name, PASS), encoding="utf-8")

    def check(self, round_text="2"):
        code, out, err = invoke(self.module, ["repeats", "sample/042-csv-export", round_text])
        return code, (json.loads(out) if out else None), err

    def test_the_same_reviewer_and_signature_in_two_rounds_is_a_repeat(self):
        self.assertEqual(self.check(), (0, REPEAT, ""))

    def test_a_bulleted_signature_line_counts(self):
        self.write(2, code=CODE.replace("Signature:", "- Signature:"))
        self.assertEqual(self.check(), (0, REPEAT, ""))

    def test_round_1_continues_with_no_previous_round(self):
        self.assertEqual(self.check("1"), (0, {"decision": "continue", "round": 1, "previous": None, "repeats": [],
                                                       "churn": []}, ""))

    def test_a_missing_previous_round_continues(self):
        shutil.rmtree(self.job / "round-1")
        self.assertEqual(self.check(), (0, CONTINUE, ""))

    def test_changes_reviews_without_signatures_continue(self):
        bare = "VERDICT: CHANGES\n\nThe export route skips the person check.\n"
        for n in (1, 2):
            self.write(n, code=bare)
        self.assertEqual(self.check(), (0, CONTINUE, ""))

    def test_the_same_signature_from_a_different_reviewer_continues(self):
        self.write(1, security=CODE)
        self.assertEqual(self.check(), (0, CONTINUE, ""))

    def test_a_pass_previous_review_is_not_a_repeat(self):
        self.write(1, code=CODE.replace("VERDICT: CHANGES", "VERDICT: PASS"))
        self.assertEqual(self.check(), (0, CONTINUE, ""))

    def test_a_pass_between_does_not_reset_a_signature(self):
        self.write(2)
        self.write(3, code=CODE)
        self.assertEqual(self.check("3"), (0, {**REPEAT, "round": 3, "previous": 2}, ""))

    def test_a_bad_round_exits_2(self):
        for bad in ("0", "-1", "x", "01", "1.5", ""):
            code, out, err = self.check(bad)
            self.assertEqual((code, out, err), (2, None, "factory.py: round must be a positive integer\n"), bad)

    def test_a_missing_review_exits_4(self):
        path = self.job / "round-2" / "review-ui.md"
        path.unlink()
        code, out, err = self.check()
        self.assertEqual((code, out), (4, None))
        self.assertEqual(err, f"factory.py: {path} is missing: the round is not complete\n")


def changes(slug):
    return f"VERDICT: CHANGES\n\nThe export still fails.\nSignature: worker/export.js#{slug}\n"


SIGNATURES = {1: "missing-csv-route", 2: "missing-person-check", 3: "csv-quote-escape", 4: "missing-header-row"}
CHURN = [{"path": "worker/export.js", "rounds": [2, 3, 4]}]


class ChurnTest(IsolatedTestCase):
    setUp = RepeatsTest.setUp
    write = RepeatsTest.write
    check = RepeatsTest.check

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.wt), *args], check=True, capture_output=True,
                              text=True).stdout.strip()

    def build(self, skip=None, merge_base=False):
        """Commit once per round, changing worker/export.js in every round but skip, and record each head.
        With merge_base, rounds 2 to 4 first merge a base branch commit that changes docs/base.md."""
        self.wt = self.dir / "worktrees" / "sample" / "042-csv-export"
        (self.wt / "worker").mkdir(parents=True)
        (self.wt / "docs").mkdir()
        self.git("init", "-q")
        for n in range(1, 5):
            self.write(n, code=changes(SIGNATURES[n]))
            if merge_base and n > 1:
                self.git("checkout", "-q", "base")
                (self.wt / "docs" / "base.md").write_text(f"base {n}\n")
                self.git("add", "-A")
                self.git("commit", "-q", "-m", f"base {n}")
                self.git("checkout", "-q", "-")
                self.git("merge", "-q", "--no-edit", "base")
            if n != skip:
                (self.wt / "worker" / "export.js").write_text(f"// round {n}\n")
            (self.wt / f"round-{n}.md").write_text(f"round {n}\n")
            self.git("add", "-A")
            self.git("commit", "-q", "-m", f"round {n}")
            self.plan(n, self.git("rev-parse", "HEAD"))
            if merge_base and n == 1:
                self.git("branch", "base")

    def plan(self, n, head):
        (self.job / f"round-{n}" / "review-plan.json").write_text(json.dumps({"round": n, "head": head}))

    def churned(self, decision="continue", churn=(), repeats=()):
        return (0, {"decision": decision, "round": 4, "previous": 3, "repeats": list(repeats), "churn": list(churn)},
                "")

    def test_a_file_changed_in_three_consecutive_rounds_is_churn(self):
        self.build()
        self.assertEqual(self.check("4"), self.churned("churn", CHURN))

    def test_round_3_never_churns(self):
        self.build()
        self.assertEqual(self.check("3"), (0, {"decision": "continue", "round": 3, "previous": 2, "repeats": [],
                                               "churn": []}, ""))

    def test_a_file_skipped_in_one_round_is_not_churn(self):
        self.build(skip=3)
        self.assertEqual(self.check("4"), self.churned())

    def test_a_missing_or_malformed_round_head_is_not_churn(self):
        self.build()
        path = self.job / "round-3" / "review-plan.json"
        head = json.loads(path.read_text())["head"]
        for text in (None, "{", json.dumps({"head": head[:39]}), json.dumps(["x"])):
            with self.subTest(text=text):
                path.unlink(missing_ok=True)
                if text is not None:
                    path.write_text(text)
                self.assertEqual(self.check("4"), self.churned())

    def test_a_non_ancestor_head_is_not_churn(self):
        self.build()
        tree = self.git("rev-parse", "HEAD^{tree}")
        self.plan(2, self.git("commit-tree", tree, "-m", "rewritten"))
        self.assertEqual(self.check("4"), self.churned())

    def test_no_worktree_is_not_churn(self):
        self.build()
        shutil.rmtree(self.wt)
        self.assertEqual(self.check("4"), self.churned())

    def test_a_repeat_wins_over_churn(self):
        self.build()
        self.write(4, code=changes(SIGNATURES[3]))
        repeats = [{"reviewer": "code", "signature": "worker/export.js#csv-quote-escape", "first": 3}]
        self.assertEqual(self.check("4"), self.churned("repeat", CHURN, repeats))

    def test_a_base_merge_is_not_churn(self):
        self.build(skip=3, merge_base=True)
        self.assertEqual(self.check("4"), self.churned())

    def test_own_churn_survives_base_merges(self):
        self.build(merge_base=True)
        self.assertEqual(self.check("4"), self.churned("churn", CHURN))
