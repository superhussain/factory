import os
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
RUNNING_EXAMPLE = """\
earned autonomy for jobs merged in the last 90 days
thresholds: 20 scanned jobs, no reverts, rework at most 10%, mean rounds at most 1.75, CHANGES at most 10%
project  class     jobs  held  reworked  reverted  unscanned  rounds  changes  recommend
alpha    internal    20    19         1         0          0     1.2       8%  automerge
alpha    visible      6     4         1         1          0     2.0      25%  hold
alpha visible hold: reverted 1019-add-z; rework 17% over 10%; rounds 2.0 over 1.75; changes 25% over 10%
alpha: recommend merge human until every class shows automerge
report only: a person changes Merge under Settings, Pipeline; nothing was written
"""
INTERNAL = {"security": "PASS", "ux": "SKIPPED", "ui": "SKIPPED", "code": "PASS"}
INTERNAL_CHANGES = {**INTERNAL, "code": "CHANGES"}
VISIBLE = {"security": "PASS", "ux": "PASS", "ui": "PASS", "code": "PASS"}
VISIBLE_CHANGES = {**VISIBLE, "ux": "CHANGES", "code": "CHANGES"}


def stamp(days_ago):
    return (datetime.now(timezone.utc) - timedelta(days=days_ago)).strftime("%Y-%m-%dT%H:%M:%SZ")


def merged(job_id, rounds, result="held", project="alpha", days_ago=1):
    job = {"id": job_id, "project": project, "stage": "merged", "done": stamp(days_ago),
           "updated": "2026-01-05T10:00:00Z", "rounds": rounds}
    if result:
        job["outcome"] = {"result": result, "at": "2026-01-09T12:00:00Z", "evidence": []}
    return job


def running_example():
    """alpha: 20 internal jobs (one reworked, four took two rounds after CHANGES) and 6 visible jobs
    (one reworked, 1019-add-z reverted, each took two rounds after two CHANGES)."""
    internal = [merged(f"20{n:02}-internal", [INTERNAL_CHANGES, INTERNAL] if n < 4 else [INTERNAL],
                       "reworked" if n == 19 else "held") for n in range(20)]
    results = ["held", "held", "held", "held", "reworked", "reverted"]
    names = ["1020-a", "1021-b", "1022-c", "1023-d", "1018-add-y", "1019-add-z"]
    visible = [merged(name, [VISIBLE_CHANGES, VISIBLE], result) for name, result in zip(names, results)]
    return {"updated": "2026-01-09T12:00:00Z", "jobs": internal + visible}


class AutonomyReportTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        os.environ["FACTORY_HOME"] = str(self.home)

    def run_cmd(self, *args, board=None):
        with http_fixture({"GET /api/board": (200, board or running_example()),
                           "PUT /api/board": (200, b"ok")}) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n", encoding="utf-8")
            result = invoke(factory, ["autonomy", "report", *args])
        self.methods = [method for method, *_ in server.requests]
        return result

    def test_running_example(self):
        self.assertEqual(self.run_cmd(), (0, RUNNING_EXAMPLE, ""))

    def test_slug_filter(self):
        board = running_example()
        board["jobs"].append(merged("1030-beta", [INTERNAL], project="beta"))
        self.assertEqual(self.run_cmd("alpha", board=board), (0, RUNNING_EXAMPLE, ""))

    def test_collect(self):
        board = {"jobs": [merged(f"110{n}-x", [INTERNAL]) for n in range(5)]}
        code, out, err = self.run_cmd(board=board)
        self.assertEqual((code, err), (0, ""))
        self.assertIn("alpha    internal     5     5         0         0          0     1.0       0%  collect\n", out)
        self.assertIn("alpha internal collect: needs 15 more scanned jobs\n", out)
        self.assertIn("alpha: recommend merge human until every class shows automerge\n", out)

    def test_automerge(self):
        board = {"jobs": [merged(f"12{n:02}-x", [INTERNAL]) for n in range(20)]}
        code, out, err = self.run_cmd(board=board)
        self.assertEqual((code, err), (0, ""))
        self.assertIn("alpha    internal    20    20         0         0          0     1.0       0%  automerge\n", out)
        self.assertIn("alpha: recommend merge auto\n", out)
        self.assertNotIn("alpha internal", out.split("automerge\n", 1)[1])

    def test_tuned_thresholds(self):
        board = {"jobs": [merged(f"13{n:02}-x", [INTERNAL_CHANGES, INTERNAL] if n < 6
                                 else [INTERNAL, INTERNAL] if n < 14 else [INTERNAL]) for n in range(20)]}
        code, out, err = self.run_cmd(board=board)
        self.assertEqual((code, err), (0, ""))
        self.assertIn("alpha    internal    20    20         0         0          0     1.7       9%  automerge\n", out)
        self.assertIn("alpha: recommend merge auto\n", out)

    def test_skipped(self):
        board = running_example()
        board["jobs"] += [merged("1040-rider", []), merged("1041-orphan", [INTERNAL], project=""),
                          merged("1042-unscanned", [VISIBLE], result=None)]
        code, out, err = self.run_cmd(board=board)
        self.assertEqual((code, err), (0, ""))
        self.assertIn("alpha    visible      7     4         1         1          1     1.9      23%  hold\n", out)
        self.assertTrue(out.endswith("merged jobs with no review rounds, skipped: 1040-rider\n"
                                     "merged jobs with no project, skipped: 1041-orphan\n"
                                     "report only: a person changes Merge under Settings, Pipeline; "
                                     "nothing was written\n"), out)
        self.assertNotIn("no project", self.run_cmd("alpha", board=board)[1])

    def test_window(self):
        board = running_example()
        board["jobs"] += [merged("1050-old", [INTERNAL], result="reverted", days_ago=91),
                          {**merged("1051-open", [INTERNAL], result="reverted"), "stage": "pr-open"}]
        self.assertEqual(self.run_cmd(board=board), (0, RUNNING_EXAMPLE, ""))
        self.assertEqual(self.run_cmd(board={"jobs": [merged("1050-old", [INTERNAL], days_ago=91)]}),
                         (0, "no reviewed jobs merged in the last 90 days\n", ""))

    def test_writes_nothing(self):
        self.run_cmd()
        self.assertEqual(self.methods, ["GET"])

    def test_usage(self):
        usage = ("factory.py: usage: autonomy status|pause|resume|report [<slug>]|allow <slug>|deny <slug>|self <slug>"
                 "|restore <slug> internal|visible|promote <slug> internal|visible\n")
        self.assertEqual(invoke(factory, ["autonomy", "report", "Bad!"]), (2, "", usage))
        self.assertEqual(invoke(factory, ["autonomy", "report", "a", "b"]), (2, "", usage))
        code, out, err = invoke(factory, ["autonomy", "report", "a", "b", "c"])
        self.assertEqual((code, out), (2, ""))
        self.assertIn("factory.py autonomy report [<project-slug>]", err)
