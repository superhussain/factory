import json
import unittest
from pathlib import Path
from unittest.mock import patch

from support import ROOT, IsolatedTestCase, invoke, load_runtime

factory = load_runtime()
FIXTURE = json.loads(Path(ROOT, "tests/fixtures/inbox-count.json").read_text(encoding="utf-8"))


class InboxTest(IsolatedTestCase):
    def run_inbox(self, api):
        with patch.object(factory, "read_board", return_value={"jobs": FIXTURE["jobs"]}), \
                patch.object(factory, "api", api):
            return invoke(factory, ["inbox"])

    def test_counts_jobs_and_backlog_lines(self):
        code, out, _ = self.run_inbox(lambda *a, **k: (200, {}, FIXTURE["backlog"]))
        self.assertEqual((code, out), (0, json.dumps({"count": FIXTURE["count"]}) + "\n"))

    def test_missing_backlog_counts_as_empty(self):
        code, out, _ = self.run_inbox(lambda *a, **k: (404, {}, ""))
        self.assertEqual((code, out), (0, '{"count": 4}\n'))

    def test_api_failure_exits_5(self):
        def fail(*a, **k):
            raise SystemExit(5)
        code, out, _ = self.run_inbox(fail)
        self.assertEqual((code, out), (5, ""))

    def test_empty_board(self):
        self.assertEqual(factory.inbox_count([], ""), 0)


if __name__ == "__main__":
    unittest.main()
