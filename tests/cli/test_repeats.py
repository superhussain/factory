import json
import shutil
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

CODE = ("VERDICT: CHANGES\n\nThe export route skips the person check.\n"
        "Signature: worker/export.js#missing-person-check\n\nNit: rename `rowsToCsv`.\n")
PASS = "VERDICT: PASS\n\nNothing to change.\n"
REPEAT = {"decision": "repeat", "round": 2, "previous": 1,
          "repeats": [{"reviewer": "code", "signature": "worker/export.js#missing-person-check"}]}
CONTINUE = {"decision": "continue", "round": 2, "previous": 1, "repeats": []}


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
        self.assertEqual(self.check("1"), (0, {"decision": "continue", "round": 1, "previous": None, "repeats": []}, ""))

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
