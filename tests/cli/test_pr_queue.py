import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
JOB = "042-sample-export"
PR = "https://github.com/example/sample/pull/44"
SHA = "0123456789abcdef0123456789abcdef01234567"
UUID = "0190a1b2-0000-4000-8000-0000000000aa"
PATH = "repos/example/sample/pulls/44/merge-async"
PUT = object()
REQUIRED = 'Required workflow "sample-validate" is not satisfied'


def view(base="main", sha=SHA):
    return 0, json.dumps({"headRefOid": sha, "baseRefName": base})


def put(body=None):
    return 0, json.dumps({"id": UUID, "status": "pending"} if body is None else body), ""


def status(value, **extra):
    return 0, json.dumps({"status": value, **extra})


class PrQueueTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "projects").mkdir()
        (self.home / "projects" / "sample.md").write_text("---\nrepo: example/sample\nbase: main\n---\n# Sample\n")
        self.now = 0.0

    def sleep(self, seconds):
        self.now += seconds

    def queue(self, polls, pr_view=None, put_answer=PUT):
        """Run pr-queue with gh answering the view, the PUT, then each poll in turn, repeating the last."""
        job = {"id": JOB, "project": "sample", "title": "Sample export", "stage": "pr-open", "pr": PR}
        calls = []

        def gh_pr(*args, stderr=False):
            calls.append(("pr",) + args)
            return view() if pr_view is None else pr_view

        def gh_cli(*args, stderr=False):
            calls.append(args)
            if "PUT" in args:
                return put() if put_answer is PUT else put_answer
            return polls[min(sum("PUT" not in c and c[0] == "api" for c in calls) - 1, len(polls) - 1)]

        with http_fixture({"/api/board": (200, {"updated": "", "jobs": [job]})}) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")
            with patch.object(factory, "gh_pr", gh_pr), patch.object(factory, "gh_cli", gh_cli), \
                    patch.object(factory.time, "sleep", self.sleep), \
                    patch.object(factory.time, "monotonic", lambda: self.now):
                result = invoke(factory, ["--home", str(self.home), "pr-queue", f"sample/{JOB}"])
        return result + (calls,)

    def test_enqueued_after_one_pending_poll(self):
        code, out, err, calls = self.queue([status("pending"), status("ENQUEUED")])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, '{"pr": 44, "status": "enqueued", "detail": ""}\n')
        self.assertEqual(calls[0], ("pr", "view", PR, "--json", "headRefOid,baseRefName"))
        self.assertEqual(calls[1], ("api", "-X", "PUT", PATH, "-f", f"sha={SHA}", "-f", "merge_action=merge_queue"))
        self.assertEqual(calls[2:], [("api", f"{PATH}/{UUID}")] * 2)
        self.assertEqual(self.now, 10)

    def test_failed_exits_1_with_detail(self):
        code, out, err, _ = self.queue([status("failed", message="queue rejected the head")])
        self.assertEqual((code, err), (1, ""))
        reading = json.loads(out)
        self.assertEqual((reading["pr"], reading["status"]), (44, "failed"))
        self.assertIn("queue rejected the head", reading["detail"])

    def test_refused_exits_1_with_detail(self):
        code, out, err, calls = self.queue([status("enqueued")], put_answer=(1, "", f"gh: {REQUIRED} (HTTP 422)\n"))
        self.assertEqual((code, err), (1, ""))
        self.assertEqual(json.loads(out), {"pr": 44, "status": "refused", "detail": f"gh: {REQUIRED} (HTTP 422)"})
        self.assertEqual(len(calls), 2)

    def test_pending_at_the_deadline_exits_3(self):
        code, out, _, calls = self.queue([status("pending")])
        self.assertEqual(code, 3)
        reading = json.loads(out)
        self.assertEqual((reading["status"], self.now), ("pending", 120))
        self.assertIn('"status": "pending"', reading["detail"])
        self.assertEqual(sum(c == ("api", f"{PATH}/{UUID}") for c in calls), 13)

    def test_a_stacked_base_exits_4_without_a_put(self):
        code, out, err, calls = self.queue([status("enqueued")], pr_view=view(base="factory/041-sample-api"))
        self.assertEqual((code, out), (4, ""))
        self.assertIn("PR #44 targets factory/041-sample-api, not main: it waits for its parent", err)
        self.assertEqual(len(calls), 1)

    def test_a_bad_head_exits_5(self):
        for answer in (view(sha="not-a-sha"), (0, "[]")):
            with self.subTest(answer=answer):
                code, out, _, calls = self.queue([status("enqueued")], pr_view=answer)
                self.assertEqual((code, out, len(calls)), (5, "", 1))

    def test_no_request_id_exits_5(self):
        for body in ({"status": "pending"}, {"id": "not-a-uuid"}, {"nested": {"id": UUID}}, []):
            with self.subTest(body=body):
                code, out, err, calls = self.queue([status("enqueued")], put_answer=put(body))
                self.assertEqual((code, out, len(calls)), (5, "", 2))
                self.assertIn("gh api merge-async printed no request id", err)

    def test_gh_problem_on_a_poll_exits_5(self):
        for answer, why in (((0, "[]"), "printed no JSON object"), ((1, ""), "gh api merge-async exited 1")):
            with self.subTest(answer=answer):
                code, out, err, _ = self.queue([answer])
                self.assertEqual((code, out), (5, ""))
                self.assertIn(why, err)

    def test_gh_problem_on_the_put_exits_5(self):
        for answer in (None, ("timeout", "", "")):
            with self.subTest(answer=answer):
                code, out, _, _ = self.queue([status("enqueued")], put_answer=answer)
                self.assertEqual((code, out), (5, ""))
