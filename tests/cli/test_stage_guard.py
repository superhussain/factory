import io
import json
import os
from unittest.mock import patch

from support import invoke
from test_verdict import JOB, ROUND_1, BoardFixture, factory

# The spec's made-up running example: round 2 has code CHANGES.
ROUND_2 = {"security": "PASS", "ux": "SKIPPED", "ui": "SKIPPED", "code": "CHANGES"}
ROUND_2_MESSAGE = (f"factory.py: {JOB} cannot move to approve: round 2 has code CHANGES. Every verdict in the "
                   "latest round must be PASS or SKIPPED. Nothing was written.\n")


class GuardFixture(BoardFixture):
    def setUp(self):
        super().setUp()
        self.board["jobs"][0]["rounds"] = [dict(ROUND_1), dict(ROUND_2)]
        self.folder = self.home / "jobs" / "factory" / JOB

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
        self.assertEqual(self.write({"stage": "approve", "rounds": [dict(ROUND_2, code="PASS")]})[0], 0)
        self.assertEqual(self.write({"stage": "approve", "rounds": [ROUND_2]}), (2, "", (
            f"factory.py: {JOB} cannot move to approve: round 1 has code CHANGES. Every verdict in the "
            "latest round must be PASS or SKIPPED. Nothing was written.\n")))
        self.assertEqual(self.puts, 1)

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
        self.assertEqual(self.verdict("2", "code", "CHANGES")[0], 0)
        self.assertEqual((self.puts, self.job()["stage"], self.job()["rounds"][1]["code"]),
                         (1, "approve", "CHANGES"))


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
