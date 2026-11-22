import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
JOB = "042-sample-export"
ROUND_1 = {"security": "PASS", "ux": "PASS", "ui": "SKIPPED", "code": "CHANGES"}
ROUND_2 = {"security": "pending", "ux": "SKIPPED", "ui": "SKIPPED", "code": "pending"}


class RoundWaitTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        self.folder = self.home / "jobs" / "sample" / JOB
        (self.folder / "round-2").mkdir(parents=True)
        self.now = 0.0
        self.sleeps = {}
        self.holds = []
        self.write("ux", "VERDICT: SKIPPED\n")
        self.write("ui", "VERDICT: SKIPPED\n")

    def write(self, reviewer, text):
        (self.folder / "round-2" / f"review-{reviewer}.md").write_text(text, encoding="utf-8")

    def sleep(self, seconds):
        self.now += seconds
        if self.now in self.sleeps:
            self.sleeps.pop(self.now)()

    def hold(self, target):
        self.holds.append(target)

    def round_wait(self, *args, rounds=(ROUND_1, ROUND_2), jobs=None):
        """Run round-wait against a fixed board, keeping its PUT bodies in self.puts."""
        job = {"id": JOB, "project": "sample", "title": "Sample export", "stage": "review", "round": 2,
               "rounds": list(rounds)}
        with http_fixture({"/api/board": (200, {"updated": "", "jobs": [job] if jobs is None else jobs})}) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\ndevice: sample-device\n---\n")
            with patch.object(factory, "hold", self.hold), \
                    patch.object(factory.time, "sleep", self.sleep), \
                    patch.object(factory.time, "monotonic", lambda: self.now):
                result = invoke(factory, ["--home", str(self.home), "round-wait", f"sample/{JOB}", *args])
        self.puts = [json.loads(r[3]) for r in server.requests if r[0] == "PUT"]
        return result

    def result(self, *args):
        code, out, err = self.round_wait(*args)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out.count("\n"), 1)
        return json.loads(out)

    def test_round_event(self):
        self.sleeps[212] = lambda: self.write("security", "VERDICT: CHANGES\n")
        self.sleeps[305] = lambda: self.write("code", "VERDICT: PASS\n")
        self.assertEqual(self.result("2"), {
            "event": "round", "round": 2,
            "verdicts": {"security": "CHANGES", "ux": "SKIPPED", "ui": "SKIPPED", "code": "PASS"},
            "files": {"round-2/review-security.md": "VERDICT: CHANGES", "round-2/review-ux.md": "VERDICT: SKIPPED",
                      "round-2/review-ui.md": "VERDICT: SKIPPED", "round-2/review-code.md": "VERDICT: PASS"},
            "elapsed": 305})
        self.assertEqual(self.holds, [f"sample/{JOB}"])

    def test_writes_as_verdicts_land(self):
        self.sleeps[212] = lambda: self.write("security", "VERDICT: CHANGES\n")
        self.sleeps[305] = lambda: self.write("code", "VERDICT: PASS\n")
        self.result("2")
        self.assertEqual([body["jobs"][0]["rounds"] for body in self.puts], [
            [ROUND_1, {**ROUND_2, "security": "CHANGES"}],
            [ROUND_1, {**ROUND_2, "security": "CHANGES", "code": "PASS"}],
        ])

    def test_timeout(self):
        self.assertEqual(self.result("2", "--timeout", "5"), {
            "event": "timeout", "round": 2, "verdicts": ROUND_2,
            "files": {"round-2/review-security.md": None, "round-2/review-ux.md": "VERDICT: SKIPPED",
                      "round-2/review-ui.md": "VERDICT: SKIPPED", "round-2/review-code.md": None},
            "elapsed": 5})
        self.assertEqual(self.puts, [])
        self.now = 0.0
        self.assertEqual(self.result("2")["elapsed"], 540)

    def test_malformed(self):
        self.write("code", "Looks good\n")
        reading = self.result("2")
        self.assertEqual((reading["event"], reading["verdicts"]["code"], reading["files"]["round-2/review-code.md"],
                          reading["elapsed"]), ("malformed", "pending", "Looks good", 1))
        self.assertEqual(self.puts, [])

    def test_refusals(self):
        for kwargs in ({"jobs": []}, {"rounds": (ROUND_1,)}):
            with self.subTest(kwargs):
                code, out, err = self.round_wait("2", **kwargs)
                self.assertEqual((code, out, self.puts), (4, "", []))
        self.holds.clear()
        for args in (["0"], ["x"], ["2", "--timeout", "541"], ["2", "--timeout", "0"], ["2", "--timeout"],
                     ["2", "3"]):
            with self.subTest(args):
                code, out, err = self.round_wait(*args)
                self.assertEqual((code, out, self.puts), (2, "", []))
                self.assertIn(factory.ROUND_WAIT_USAGE, err)
        self.assertEqual(self.holds, [])

    def test_keeps_unverified(self):
        flags = ["review-code.md export.py:412 bad-line"]
        self.sleeps[212] = lambda: self.write("security", "VERDICT: CHANGES\n")
        code, out, err = self.round_wait("2", "--timeout", "300", rounds=(ROUND_1, {**ROUND_2, "unverified": flags}))
        self.assertEqual((code, err), (0, ""))
        self.assertEqual([body["jobs"][0]["rounds"][1] for body in self.puts],
                         [{**ROUND_2, "security": "CHANGES", "unverified": flags}])
