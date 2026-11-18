import json
import unittest
from pathlib import Path
from unittest.mock import patch

from support import ROOT, IsolatedTestCase, invoke, load_runtime

factory = load_runtime()
FIXTURE = json.loads(Path(ROOT, "tests/fixtures/inbox-count.json").read_text(encoding="utf-8"))
WAITING = [{"id": i, "project": "sample", "title": f"Synthetic {i}"}
           for i in ("042-sample-links", "043-sample-asked", "044-sample-export", "045-sample-legacy")]


class InboxTest(IsolatedTestCase):
    def run_inbox(self, api):
        with patch.object(factory, "read_board", return_value={"jobs": FIXTURE["jobs"]}), \
                patch.object(factory, "api", api):
            return invoke(factory, ["inbox"])

    def test_counts_jobs_and_backlog_lines(self):
        code, out, _ = self.run_inbox(lambda *a, **k: (200, {}, FIXTURE["backlog"]))
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(out), {"count": FIXTURE["count"], "jobs": WAITING})

    def test_missing_backlog_counts_as_empty(self):
        code, out, _ = self.run_inbox(lambda *a, **k: (404, {}, ""))
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(out), {"count": 4, "jobs": WAITING})

    def test_api_failure_exits_5(self):
        def fail(*a, **k):
            raise SystemExit(5)
        code, out, _ = self.run_inbox(fail)
        self.assertEqual((code, out), (5, ""))

    def test_jobs_need_a_string_id_and_default_to_empty_text(self):
        jobs = [{"stage": "needs-human"}, {"id": "043-asked", "stage": "spec", "needs": "pick A"}]
        with patch.object(factory, "read_board", return_value={"jobs": jobs}), \
                patch.object(factory, "api", lambda *a, **k: (404, {}, "")):
            code, out, _ = invoke(factory, ["inbox"])
        self.assertEqual((code, json.loads(out)),
                         (0, {"count": 2, "jobs": [{"id": "043-asked", "project": "", "title": ""}]}))

    def test_rows_are_cut_to_the_plugin_limits(self):
        jobs = [{"id": "043-asked", "stage": "needs-human", "title": "x" * 300, "project": "p" * 200},
                {"id": "bad id!", "stage": "needs-human"}]
        with patch.object(factory, "read_board", return_value={"jobs": jobs}), \
                patch.object(factory, "api", lambda *a, **k: (404, {}, "")):
            _, out, _ = invoke(factory, ["inbox"])
        self.assertEqual(json.loads(out)["jobs"],
                         [{"id": "043-asked", "project": "p" * 128, "title": "x" * 200}])

    def test_rows_are_cut_by_utf16_length_without_splitting_a_pair(self):
        jobs = [{"id": "043-asked", "stage": "needs-human", "title": "x" * 199 + "\U0001F600" * 3}]
        with patch.object(factory, "read_board", return_value={"jobs": jobs}), \
                patch.object(factory, "api", lambda *a, **k: (404, {}, "")):
            _, out, _ = invoke(factory, ["inbox"])
        self.assertEqual(json.loads(out)["jobs"][0]["title"], "x" * 199)

    def test_a_lone_surrogate_in_a_title_is_dropped(self):
        jobs = [{"id": "043-asked", "stage": "needs-human", "title": "x\ud800y"}]
        with patch.object(factory, "read_board", return_value={"jobs": jobs}), \
                patch.object(factory, "api", lambda *a, **k: (404, {}, "")):
            code, out, _ = invoke(factory, ["inbox"])
        self.assertEqual((code, json.loads(out)["jobs"][0]["title"]), (0, "xy"))

    def test_empty_board(self):
        self.assertEqual(factory.inbox_count([], ""), 0)


if __name__ == "__main__":
    unittest.main()
