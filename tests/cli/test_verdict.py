import json
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

factory = load_runtime()
DEVICE = "host-a"
JOB = "042-sample-export"
USAGE = ("factory.py: usage: verdict <project-slug>/<job-id> <round> <reviewer> "
         "<PASS|CHANGES|SKIPPED|pending>\n")
# The spec's made-up running example: round 1 done, round 2 waiting on security and code.
ROUND_1 = {"security": "PASS", "ux": "PASS", "ui": "SKIPPED", "code": "CHANGES"}
ROUND_2 = {"security": "pending", "ux": "SKIPPED", "ui": "SKIPPED", "code": "pending"}


def stamp(**delta):
    return (datetime.now(timezone.utc) + timedelta(**delta)).strftime("%Y-%m-%dT%H:%M:%SZ")


class BoardFixture(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "config.md").write_text(f"---\ndevice: {DEVICE}\n---\n", encoding="utf-8")
        self.board = {"updated": "2026-01-01T00:00:00Z", "jobs": [
            {"id": JOB, "stage": "review", "round": 2, "project": "factory", "updated": "2026-01-01T00:00:00Z",
             "rounds": [dict(ROUND_1), dict(ROUND_2)],
             "claim": {"device": DEVICE, "session": "d4e5f6", "since": stamp(), "seen": stamp()}}]}
        self.puts = 0
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory, "api", side_effect=self.request).start()

    def request(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        self.assertEqual(path, "/api/board")
        if method == "GET":
            return 200, {"ETag": '"board"'}, factory.dump_board(self.board)
        self.puts += 1
        self.board = json.loads(body)
        return 200, {}, ""

    def job(self):
        return next(j for j in self.board["jobs"] if j["id"] == JOB)

    def verdict(self, *args, target=f"factory/{JOB}", session="d4e5f6"):
        return invoke(factory, ["verdict", target, *args, "--session", session])


class VerdictTest(BoardFixture):
    def test_sets_one_verdict(self):
        code, out, err = self.verdict("2", "code", "PASS")
        self.assertEqual((code, err), (0, ""))
        after = dict(ROUND_2, code="PASS")
        self.assertEqual(json.loads(out), {"round": 2, "verdicts": after})
        self.assertEqual(self.job()["rounds"], [ROUND_1, after])
        self.assertEqual(self.puts, 1)

    def test_skipped_for_ux(self):
        self.assertEqual(self.verdict("1", "ux", "SKIPPED")[0], 0)
        self.assertEqual(self.job()["rounds"], [dict(ROUND_1, ux="SKIPPED"), ROUND_2])

    def test_refuses(self):
        for args in (("0", "code", "PASS"), ("2", "qa", "PASS"), ("2", "code", "pass"),
                     ("2", "security", "SKIPPED")):
            self.assertEqual(self.verdict(*args), (2, "", USAGE), args)
        self.assertEqual(self.verdict("2", "code", "PASS", target=f"beta/{JOB}"),
                         (2, "", f"factory.py: {JOB} belongs to factory, not beta\n"))
        self.assertEqual(self.verdict("3", "code", "PASS"),
                         (4, "", f"factory.py: {JOB} has no rounds entry 3 on the board. Nothing was written.\n"))
        self.assertEqual(self.verdict("1", "code", "PASS", target="factory/043-missing"),
                         (4, "", "factory.py: 043-missing has no rounds entry 1 on the board. "
                                 "Nothing was written.\n"))
        code, out, err = self.verdict("2", "code", "PASS", session="other1")
        self.assertEqual((code, out), (6, ""))
        self.assertTrue(err.startswith(f"factory.py: {JOB} is held by {DEVICE} session d4e5f6 since "), err)
        self.assertEqual(self.puts, 0)
        self.assertEqual(self.job()["rounds"], [ROUND_1, ROUND_2])
