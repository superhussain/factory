import json
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, invoke, load_runtime

BOARD = {"jobs": [
    {"id": "041-old", "stage": "build", "project": "sample", "title": "Old"},
    {"id": "042-sample-links", "stage": "needs-human", "project": "sample", "title": "Sample links"},
    {"id": "043-asked", "stage": "spec", "needs": "pick A"},
    {"id": "044-done", "stage": "merged", "needs": "x"},
    {"stage": "needs-human", "title": "No id"},
]}


class WaitingTest(IsolatedTestCase):
    def setUp(self):
        self.runtime = load_runtime()
        self.addCleanup(patch.stopall)
        patch.object(self.runtime, "factory_home", return_value=Path("/nonexistent")).start()

    def run_waiting(self, board):
        with patch.object(self.runtime, "api", return_value=(200, {}, json.dumps(board))):
            return invoke(self.runtime, ["waiting"])

    def test_prints_waiting_jobs_in_board_order(self):
        code, out, err = self.run_waiting(BOARD)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), {"jobs": [
            {"id": "042-sample-links", "project": "sample", "title": "Sample links"},
            {"id": "043-asked", "project": "", "title": ""},
        ]})

    def test_empty_board(self):
        code, out, err = self.run_waiting({"jobs": []})
        self.assertEqual((code, json.loads(out), err), (0, {"jobs": []}, ""))

    def test_api_failure_keeps_exit_code(self):
        with patch.object(self.runtime, "api", side_effect=SystemExit(5)):
            code, out, _ = invoke(self.runtime, ["waiting"])
        self.assertEqual((code, out), (5, ""))
