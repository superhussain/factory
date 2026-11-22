import contextlib
import io
import json
import os
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

factory = load_runtime()
DEVICE = "host-a"
JOB = "042-sample-export"
# The spec's made-up running example: round 2 has code CHANGES.
ROUND_1 = {"security": "PASS", "ux": "PASS", "ui": "SKIPPED", "code": "CHANGES"}
ROUND_2 = {"security": "PASS", "ux": "SKIPPED", "ui": "SKIPPED", "code": "CHANGES"}
PENDING_ROUND = {"security": "pending", "ux": "pending", "ui": "pending", "code": "pending"}
ALL_PASS = {"security": "PASS", "ux": "PASS", "ui": "PASS", "code": "PASS"}
ROUND_2_MESSAGE = (f"factory.py: {JOB} cannot move to approve: round 2 has code CHANGES. Every verdict in the "
                   "latest round must be PASS or SKIPPED. Nothing was written.\n")


class GuardFixture(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "config.md").write_text(f"---\ndevice: {DEVICE}\n---\n", encoding="utf-8")
        now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        self.board = {"updated": "2026-01-01T00:00:00Z", "jobs": [
            {"id": JOB, "stage": "review", "round": 2, "project": "factory", "updated": "2026-01-01T00:00:00Z",
             "rounds": [dict(ROUND_1), dict(ROUND_2)],
             "claim": {"device": DEVICE, "session": "d4e5f6", "since": now, "seen": now}}]}
        self.puts = 0
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory, "api", side_effect=self.request).start()
        self.folder = self.home / "jobs" / "factory" / JOB

    def request(self, home, method, path, body=None, ok=(200,), headers=None, raw=False):
        self.assertEqual(path, "/api/board")
        if method == "GET":
            return 200, {"ETag": '"board"'}, factory.dump_board(self.board)
        self.puts += 1
        self.board = json.loads(body)
        return 200, {}, ""

    def job(self):
        return next(j for j in self.board["jobs"] if j["id"] == JOB)

    def verdict(self, *args):
        return invoke(factory, ["verdict", f"factory/{JOB}", *args, "--session", "d4e5f6"])

    def write(self, body):
        with patch.object(factory.sys, "stdin", io.StringIO(json.dumps(body))), \
                patch.dict(os.environ, {"FACTORY_SESSION": "d4e5f6", "PASEO_AGENT_ID": ""}):
            return invoke(factory, ["board", str(self.home), JOB])

    def decision(self, text):
        self.folder.mkdir(parents=True, exist_ok=True)
        (self.folder / "decision.md").write_text(text, encoding="utf-8")


class ApproveGuardTest(GuardFixture):
    def test_refuses_a_round_with_changes(self):
        self.assertEqual(self.write({"stage": "approve"}), (2, "", ROUND_2_MESSAGE))
        self.assertEqual((self.puts, self.job()["stage"]), (0, "review"))

    def test_lists_every_failing_reviewer_in_order(self):
        self.board["jobs"][0]["rounds"] = [{"code": "pending", "security": "CHANGES", "ux": "PASS"}]
        self.assertEqual(self.write({"stage": "approve"}), (2, "", (
            f"factory.py: {JOB} cannot move to approve: round 1 has security CHANGES, ui missing, code pending. "
            "Every verdict in the latest round must be PASS or SKIPPED. Nothing was written.\n")))
        self.assertEqual(self.puts, 0)

    def test_refuses_without_rounds(self):
        message = (f"factory.py: {JOB} cannot move to approve: it has no review round on the board. "
                   "Nothing was written.\n")
        for rounds in (None, [], [dict(ROUND_1), "pending"]):
            with self.subTest(rounds=rounds):
                self.board["jobs"][0]["rounds"] = rounds
                self.assertEqual(self.write({"stage": "approve"}), (2, "", message))
        self.assertEqual(self.puts, 0)

    def test_reads_rounds_from_the_patch(self):
        self.assertEqual(self.write({"stage": "approve", "rounds": [ROUND_1, ROUND_2, PENDING_ROUND]}), (2, "", (
            f"factory.py: {JOB} cannot move to approve: round 3 has security pending, ux pending, ui pending, "
            "code pending. Every verdict in the latest round must be PASS or SKIPPED. Nothing was written.\n")))
        self.assertEqual(self.puts, 0)

    def test_allows_pass_or_skipped(self):
        self.board["jobs"][0]["rounds"][1]["code"] = "PASS"
        self.assertEqual(self.write({"stage": "approve"})[0], 0)
        self.assertEqual((self.puts, self.job()["stage"]), (1, "approve"))

    def test_checks_a_job_already_at_approve(self):
        self.board["jobs"][0]["stage"] = "approve"
        self.assertEqual(self.write({"stage": "approve"}), (2, "", ROUND_2_MESSAGE))
        self.assertEqual(self.puts, 0)

    def test_verdict_still_writes_changes_at_approve(self):
        self.board["jobs"][0]["stage"] = "approve"
        self.board["jobs"][0]["rounds"][1]["code"] = "PASS"
        (self.folder / "round-2").mkdir(parents=True)
        (self.folder / "round-2" / "review-code.md").write_text("VERDICT: CHANGES\n", encoding="utf-8")
        self.assertEqual(self.verdict("2", "code", "CHANGES")[0], 0)
        self.assertEqual((self.puts, self.job()["stage"], self.job()["rounds"][1]["code"]),
                         (1, "approve", "CHANGES"))


class RoundsGuardTest(GuardFixture):
    def refused(self, body, why):
        self.assertEqual(self.write(body), (2, "", f"factory.py: {JOB} {why} Nothing was written.\n"))
        self.assertEqual((self.puts, self.job()["rounds"]), (0, [ROUND_1, ROUND_2]))

    def test_refuses_dropping_rounds(self):
        self.refused({"stage": "approve", "rounds": [ALL_PASS]},
                     "cannot drop review rounds: the board has 2 and the patch sends 1.")

    def test_refuses_rewriting_a_round(self):
        self.refused({"stage": "approve", "rounds": [ROUND_1, dict(ROUND_2, code="PASS")]},
                     "cannot rewrite round 2 in a board patch. Only factory.py verdict and round-wait change "
                     "a recorded round.")

    def test_refuses_a_new_round_with_verdicts(self):
        self.refused({"stage": "approve", "rounds": [ROUND_1, ROUND_2, ALL_PASS]},
                     'cannot add round 3 with verdicts. A new round starts as {"security": "pending", '
                     '"ux": "pending", "ui": "pending", "code": "pending"}. Record verdicts with factory.py verdict.')

    def test_refuses_rounds_that_are_not_a_list(self):
        self.refused({"rounds": {"1": ALL_PASS}}, "rounds must be a list of review rounds.")

    def test_allows_pending_appends_resends_and_new_rows(self):
        reordered = {"code": "pending", "ui": "pending", "ux": "pending", "security": "pending"}
        self.assertEqual(self.write({"stage": "review", "round": 3, "rounds": [ROUND_1, ROUND_2, reordered]})[0], 0)
        self.assertEqual(self.job()["rounds"], [ROUND_1, ROUND_2, PENDING_ROUND])
        self.assertEqual(self.write({"note": "round 3 started", "rounds": [ROUND_1, ROUND_2, PENDING_ROUND]})[0], 0)
        with patch.object(factory.sys, "stdin", io.StringIO(json.dumps({"stage": "spec", "rounds": []}))), \
                patch.dict(os.environ, {"FACTORY_SESSION": "d4e5f6", "PASEO_AGENT_ID": ""}):
            self.assertEqual(invoke(factory, ["board", str(self.home), "044-sample-import"])[0], 0)
        self.assertEqual((self.puts, self.board["jobs"][-1]["rounds"]), (3, []))

    def test_rewrite_round_opens_only_that_round(self):
        def board_write(rounds):
            stderr = io.StringIO()
            with patch.dict(os.environ, {"FACTORY_SESSION": "d4e5f6", "PASEO_AGENT_ID": ""}), \
                    contextlib.redirect_stderr(stderr):
                try:
                    factory.board_write(str(self.home), JOB, {"rounds": rounds}, show=False, rewrite_round=2)
                except SystemExit as exc:
                    return exc.code, stderr.getvalue()
            return 0, stderr.getvalue()
        self.assertEqual(board_write([ROUND_1, dict(ROUND_2, code="PASS")]), (0, ""))
        self.assertEqual(self.job()["rounds"][1]["code"], "PASS")
        self.assertEqual(board_write([dict(ROUND_1, code="PASS"), dict(ROUND_2, code="PASS")]), (2, (
            f"factory.py: {JOB} cannot rewrite round 1 in a board patch. Only factory.py verdict and round-wait "
            "change a recorded round. Nothing was written.\n")))
        self.assertEqual(self.puts, 1)


class AutomergeGuardTest(GuardFixture):
    def refused(self, why, body=None):
        self.assertEqual(self.write(body or {"stage": "pr-open", "automerge": True}),
                         (2, "", f"factory.py: {JOB} cannot set automerge true: {why} Nothing was written.\n"))
        self.assertEqual(self.puts, 0)
        self.assertNotIn("automerge", self.job())

    def test_refuses_a_missing_decision(self):
        self.refused(f"decision.md is not on this host. Run factory.py pull factory/{JOB}, then retry.")

    def test_refuses_an_escalate_decision(self):
        self.decision("\n  ESCALATE: code reviewer asked for a person\nmore\n")
        self.refused("decision.md starts with 'ESCALATE: code reviewer asked for a pers', not APPROVE.")

    def test_refuses_a_word_that_only_starts_with_approve(self):
        self.decision("APPROVED\n")
        self.refused("decision.md starts with 'APPROVED', not APPROVE.")

    def test_refuses_a_symlinked_decision(self):
        self.folder.mkdir(parents=True)
        target = self.home / "elsewhere.md"
        target.write_text("APPROVE\n", encoding="utf-8")
        (self.folder / "decision.md").symlink_to(target)
        self.refused("its decision.md cannot be read.")

    def test_refuses_an_unreadable_decision(self):
        self.folder.mkdir(parents=True)
        (self.folder / "decision.md").write_bytes(b"\xff\xfe")
        self.refused("its decision.md cannot be read.")

    def test_refuses_a_job_without_a_project(self):
        self.board["jobs"][0].pop("project")
        self.refused("it has no project, so there is no decision.md to check.")
        self.refused("it has no project, so there is no decision.md to check.",
                     {"automerge": True, "project": "../beta"})

    def test_approve_check_runs_first(self):
        self.assertEqual(self.write({"stage": "approve", "automerge": True}), (2, "", ROUND_2_MESSAGE))
        self.assertEqual(self.puts, 0)

    def test_allows_an_approve_decision(self):
        self.decision("APPROVE\nA person approved this job with /factory approve.\n")
        self.assertEqual(self.write({"stage": "pr-open", "automerge": True})[0], 0)
        self.assertEqual((self.puts, self.job()["automerge"]), (1, True))

    def test_false_or_null_never_reads_the_file(self):
        with patch.object(factory, "is_link", side_effect=AssertionError("read decision.md")):
            for value in (False, None):
                with self.subTest(value=value):
                    self.assertEqual(self.write({"stage": "pr-open", "automerge": value})[0], 0)
        self.assertEqual(self.puts, 2)
