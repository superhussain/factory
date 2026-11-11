import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

SECURITY = ("VERDICT: CHANGES\n\nThe export route skips the person check. "
            "The spec does not say if exports are person-only.\n")
CODE = "VERDICT: CHANGES\n\nRename `rowsToCsv`.\n"
PASS = "VERDICT: PASS\n\nNothing to change.\n"
ESCALATE = {"decision": "escalate", "confidence": 91, "threshold": 80,
            "reason": "clef-flash rated the findings as needing a person.", "model": "@cf/cloudflare/clef-flash"}
NO_CHANGES = {"decision": "continue", "confidence": None, "threshold": None,
              "reason": "no reviewer asked for changes", "model": None}
FAILED = {"decision": "continue", "confidence": None, "threshold": None,
          "reason": "the escalation check failed", "model": None}
WARN = "factory.py: review escalation check failed ({}); the review loop continues\n"


class ReviewEscalationTests(IsolatedTestCase):
    def setUp(self):
        self.module = load_runtime()
        self.dir = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(self.dir, ignore_errors=True))
        self.round = self.dir / "jobs" / "sample" / "042-csv-export" / "round-1"
        self.round.mkdir(parents=True)
        for name, text in (("security", SECURITY), ("ux", PASS), ("ui", PASS), ("code", CODE)):
            (self.round / f"review-{name}.md").write_text(text, encoding="utf-8")
        self.reply = (200, json.dumps(ESCALATE))
        self.posts = []

        def api(home, method, path, body=None, ok=(200,), headers=None, raw=False):
            self.assertEqual((method, path), ("POST", "/api/review/escalation"))
            self.assertEqual(ok, (200, 400, 404, 502, 503))
            self.assertEqual(headers, {"Content-Type": "application/json"})
            self.posts.append(json.loads(body))
            if isinstance(self.reply, OSError):
                self.module.fail("can't reach https://factory.example.test/api/review/escalation: timed out\x1b[2J", 5)
            if isinstance(self.reply, Exception):
                self.module.fail("can't reach https://factory.example.test/api/review/escalation: timed out", 5)
            status, text = self.reply
            return status, {}, text

        for target, fake in (("factory_home", lambda: self.dir), ("api", api)):
            patcher = patch.object(self.module, target, fake)
            patcher.start()
            self.addCleanup(patcher.stop)

    def check(self, *args):
        return invoke(self.module, ["escalation", *(args or ("sample/042-csv-export", "1"))])

    def test_sends_only_changes_reviews_and_prints_the_worker_verdict(self):
        code, out, err = self.check()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), ESCALATE)
        self.assertEqual(out.count("\n"), 1)
        self.assertEqual(self.posts, [{"reviews": {"security": SECURITY, "code": CODE}}])

    def test_cuts_each_review_to_8000_characters(self):
        long = "VERDICT: CHANGES\n" + "📊" * 9000
        (self.round / "review-ux.md").write_text(long, encoding="utf-8")
        self.check()
        self.assertEqual(self.posts[0]["reviews"]["ux"], long[:8000])
        self.assertEqual(len(self.posts[0]["reviews"]["ux"]), 8000)

    def test_no_changes_prints_the_fixed_continue_without_a_request(self):
        for name in ("security", "code"):
            (self.round / f"review-{name}.md").write_text(PASS, encoding="utf-8")
        code, out, err = self.check()
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), NO_CHANGES)
        self.assertEqual(self.posts, [])

    def test_a_missing_review_exits_4_and_a_bad_round_exits_2(self):
        (self.round / "review-ui.md").unlink()
        code, out, err = self.check()
        self.assertEqual((code, out), (4, ""))
        self.assertEqual(err, f"factory.py: {self.round / 'review-ui.md'} is missing: the round is not complete\n")
        for bad in ("0", "-1", "x", "01", "1.5", ""):
            code, out, err = self.check("sample/042-csv-export", bad)
            self.assertEqual((code, out, err), (2, "", "factory.py: round must be a positive integer\n"), bad)
        code, _, _ = self.check("sample", "1")
        self.assertEqual(code, 2)
        self.assertEqual(self.posts, [])

    def test_every_worker_or_network_failure_warns_and_continues(self):
        for reply, why in (
            ((400, "reviews.code: contains the reserved review-findings tag"),
             "400: reviews.code: contains the reserved review-findings tag"),
            ((404, "not found"), "404: not found"),
            ((502, "the escalation model failed"), "502: the escalation model failed"),
            ((502, "the escalation model failed\x1b[2J"), "502: the escalation model failed [2J"),
            ((502, "a\u202eb c\x07"), "502: a b c"),
            ((503, "review escalation needs the Workers AI binding; deploy the current Worker"),
             "503: review escalation needs the Workers AI binding; deploy the current Worker"),
            ((502, "  \n"), "502"),
            ((502, "line one\n\n" + "x" * 300), "502: " + ("line one " + "x" * 300)[:200]),
            (RuntimeError(), "can't reach https://factory.example.test/api/review/escalation: timed out"),
            (OSError(), "can't reach https://factory.example.test/api/review/escalation: timed out [2J"),
            ((200, "not json"), "malformed reply"),
            ((200, json.dumps({**ESCALATE, "decision": "hold"})), "malformed reply"),
            ((200, json.dumps({**ESCALATE, "confidence": "91"})), "malformed reply"),
        ):
            self.reply = reply
            code, out, err = self.check()
            self.assertEqual((code, err), (0, WARN.format(why)), why)
            self.assertEqual(json.loads(out), FAILED)
