import json
import os
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

factory = load_runtime()

PERSON = "A person approved this job with /factory approve."


class DecisionVerdictTest(IsolatedTestCase):
    def test_table(self):
        cases = [
            (f"APPROVE\n\n{PERSON}\n", {"verdict": "APPROVE", "decider": "person"}),
            ("APPROVE\n\n**Summary.** The approver checked every review.\n", {"verdict": "APPROVE", "decider": "model"}),
            ("ESCALATE\n\n**Summary.** A person must merge this.\n", {"verdict": "ESCALATE", "decider": "model"}),
            ("CHANGES\n", {"verdict": "CHANGES", "decider": "model"}),
            ("APPROVE: ship it\n", {"verdict": "APPROVE", "decider": "model"}),
            ("CLOSED: not wanted\n\n**Summary.** Closed from the dashboard at 2026-11-23T15:00:00Z while at stage `build`.\n",
             {"verdict": "CLOSED", "decider": "person"}),
            ("CLOSED: pr closed\n\n**Summary.** GitHub reported pull request https://example.test/pr/1 closed.\n",
             {"verdict": "CLOSED", "decider": "person"}),
            ("CLOSED: superseded\n\n**Summary.** The orchestrator closed it.\n", {"verdict": "CLOSED", "decider": "model"}),
            ("APPROVED\n", None),
            ("# Decision\n\nAPPROVE\n", None),
            ("", None),
        ]
        for text, expected in cases:
            with self.subTest(text=text):
                self.assertEqual(factory.decision_verdict(text), expected)


class JobInferTest(IsolatedTestCase):
    def setUp(self):
        self.home = Path(os.environ["HOME"], "factory")
        self.folder = self.home / "jobs" / "sample" / "042-sample-export"
        self.folder.mkdir(parents=True)
        self.row = {"id": "042-sample-export", "project": "sample", "stage": "needs-human"}
        self.addCleanup(patch.stopall)
        patch.object(factory, "factory_home", return_value=self.home).start()
        patch.object(factory, "read_board", side_effect=lambda home: {"jobs": [dict(self.row)]}).start()

    def job(self):
        code, out, err = invoke(factory, ["job", "042-sample-export"])
        self.assertEqual((code, err), (0, ""))
        return json.loads(out)

    def test_infers_from_local_decision(self):
        (self.folder / "decision.md").write_text(f"APPROVE\n\n{PERSON}\n", encoding="utf-8")
        self.assertEqual(self.job()["decision"], {"verdict": "APPROVE", "decider": "person", "inferred": True})

    def test_keeps_a_stored_decision(self):
        (self.folder / "decision.md").write_text("ESCALATE\n", encoding="utf-8")
        stored = {"verdict": "APPROVE", "decider": "model", "at": "2026-11-23T15:00:00Z"}
        self.row["decision"] = stored
        self.assertEqual(self.job()["decision"], stored)

    def test_no_file_leaves_the_row(self):
        self.assertEqual(self.job(), self.row)

    def test_unparsed_or_linked_file_leaves_the_row(self):
        (self.folder / "decision.md").write_text("# notes\n", encoding="utf-8")
        self.assertEqual(self.job(), self.row)
        (self.folder / "decision.md").unlink()
        target = self.home / "elsewhere.md"
        target.write_text("APPROVE\n", encoding="utf-8")
        (self.folder / "decision.md").symlink_to(target)
        self.assertEqual(self.job(), self.row)

    def test_bad_project_or_id_leaves_the_row(self):
        (self.folder / "decision.md").write_text("APPROVE\n", encoding="utf-8")
        self.assertIsNone(factory.inferred_decision(self.home, {**self.row, "project": "../sample"}))
        self.assertIsNone(factory.inferred_decision(self.home, {**self.row, "id": "042..x"}))
