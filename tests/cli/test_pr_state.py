import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
JOB = "042-sample-export"
PR = "https://github.com/example/sample/pull/44"
HEAD = "2026-01-02T10:00:00Z"


def review(state, at="2026-01-02T11:00:00Z", login="reviewer-a"):
    return {"author": {"login": login}, "state": state, "submittedAt": at}


def view(state="OPEN", decision="APPROVED", reviews=(), merge="CLEAN", queue=False, auto=None):
    return 0, json.dumps({"state": state, "reviewDecision": decision, "latestReviews": list(reviews),
                          "mergeStateStatus": merge, "isInMergeQueue": queue, "autoMergeRequest": auto,
                          "commits": [{"committedDate": "2026-01-01T09:00:00Z"}, {"committedDate": HEAD}]})


def checks(*buckets):
    return 0, json.dumps([{"bucket": b, "name": f"check-{i}"} for i, b in enumerate(buckets)])


class PrStateTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "projects").mkdir()
        (self.home / "projects" / "sample.md").write_text("---\nrepo: example/sample\nbase: main\n---\n# Sample\n")
        self.now = 0.0

    def sleep(self, seconds):
        self.now += seconds

    def state(self, readings, *args, pr=PR):
        """Run pr-state with gh answering each (view, checks) reading in turn, repeating the last."""
        job = {"id": JOB, "project": "sample", "title": "Sample export", "stage": "pr-open"}
        if pr is not None:
            job["pr"] = pr
        calls = []

        def gh(*gh_args):
            calls.append(gh_args)
            reading = readings[min(sum(c[0] == "view" for c in calls) - 1, len(readings) - 1)]
            return reading[0] if gh_args[0] == "view" else reading[1]

        with http_fixture({"/api/board": (200, {"updated": "", "jobs": [job]})}) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")
            with patch.object(factory, "gh_pr", gh), patch.object(factory.time, "sleep", self.sleep), \
                    patch.object(factory.time, "monotonic", lambda: self.now):
                result = invoke(factory, ["--home", str(self.home), "pr-state", f"sample/{JOB}", *args])
        return result + (calls,)

    def next_of(self, reading):
        code, out, err, _ = self.state([reading])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out.count("\n"), 1)
        return json.loads(out)["next"]

    def test_merge_prints_one_reading(self):
        code, out, err, calls = self.state([(view(), checks("pass", "skipping"))])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), {"pr": 44, "state": "open", "review": "approved", "checks": "pass",
                                           "queued": False, "conflict": False, "next": "merge"})
        self.assertEqual(calls[0][:3], ("view", PR, "--json"))
        self.assertIn("isInMergeQueue", calls[0][3])
        self.assertEqual(calls[1], ("checks", PR, "--json", "name,bucket"))

    def test_merged_and_closed(self):
        self.assertEqual(self.next_of((view("MERGED"), checks("pass"))), "merged")
        self.assertEqual(self.next_of((view("CLOSED", decision=""), checks())), "closed")

    def test_fix_reviews_after_the_head_commit_comes_first(self):
        for state in ("CHANGES_REQUESTED", "COMMENTED"):
            with self.subTest(state):
                reading = (view(decision="", reviews=[review(state)], merge="DIRTY"), checks("fail"))
                self.assertEqual(self.next_of(reading), "fix-reviews")
        code, out, _, _ = self.state([(view(decision="CHANGES_REQUESTED", reviews=[review("CHANGES_REQUESTED")]),
                                       checks("pass"))])
        self.assertEqual(json.loads(out)["review"], "changes")

    def test_older_and_bot_reviews_do_not_ask_for_fixes(self):
        old = review("CHANGES_REQUESTED", at="2026-01-02T09:00:00Z")
        bot = review("COMMENTED", login="ci-helper[bot]")
        self.assertEqual(self.next_of((view(reviews=[old, bot]), checks("pass"))), "merge")

    def test_needs_review_without_a_human_approval(self):
        for reading in ((view(decision="REVIEW_REQUIRED"), checks("pass")),
                        (view(decision=""), checks("pass")),
                        (view(decision="", reviews=[review("APPROVED", login="ci-helper[bot]")]), checks("pass"))):
            with self.subTest(reading):
                self.assertEqual(self.next_of(reading), "needs-review")
        code, out, _, _ = self.state([(view(decision=""), checks("pass"))])
        self.assertEqual(json.loads(out)["review"], "none")

    def test_an_approving_latest_review_counts_when_no_decision(self):
        reading = (view(decision="", reviews=[review("APPROVED")]), checks("pass"))
        self.assertEqual(self.next_of(reading), "merge")

    def test_rebase_then_fix_ci_then_wait(self):
        self.assertEqual(self.next_of((view(merge="DIRTY"), checks("fail"))), "rebase")
        for bucket in ("fail", "cancel"):
            with self.subTest(bucket):
                self.assertEqual(self.next_of((view(), checks("pass", bucket, "pending"))), "fix-ci")
        self.assertEqual(self.next_of((view(), checks("pass", "pending"))), "wait")
        self.assertEqual(self.next_of((view(queue=True), checks("pass"))), "wait")
        self.assertEqual(self.next_of((view(auto={"enabledAt": HEAD}), checks("pass"))), "wait")

    def test_unreadable_checks_read_as_none(self):
        for listing in ((1, ""), ("timeout", ""), (0, "not json"), (0, '[{"name": "x"}]')):
            with self.subTest(listing):
                code, out, _, _ = self.state([(view(), listing)])
                self.assertEqual((code, json.loads(out)["checks"], json.loads(out)["next"]), (0, "none", "merge"))

    def test_wait_rereads_every_30_seconds_until_next_changes(self):
        pending = (view(), checks("pending"))
        code, out, err, calls = self.state([pending, pending, (view(), checks("pass"))], "--wait", "540")
        self.assertEqual((code, err, json.loads(out)["next"]), (0, "", "merge"))
        self.assertEqual(out.count("\n"), 1)
        self.assertEqual(self.now, 60)
        self.assertEqual(sum(c[0] == "view" for c in calls), 3)

    def test_wait_stops_at_its_deadline(self):
        code, out, _, _ = self.state([(view(), checks("pending"))], "--wait", "45")
        self.assertEqual((code, json.loads(out)["next"]), (0, "wait"))
        self.assertEqual(self.now, 45)

    def test_without_wait_a_wait_reading_prints_at_once(self):
        code, out, _, calls = self.state([(view(), checks("pending"))])
        self.assertEqual((code, json.loads(out)["next"], self.now, len(calls)), (0, "wait", 0, 2))

    def test_bad_pr_exits_4(self):
        for pr, why in ((None, "no pr"), ("https://github.com/example/other/pull/44", "example/sample"),
                        ("not a url", "example/sample")):
            with self.subTest(pr=pr):
                code, out, err, calls = self.state([(view(), checks("pass"))], pr=pr)
                self.assertEqual((code, out, calls), (4, "", []))
                self.assertIn(why, err)

    def test_gh_problem_exits_5(self):
        for reading, why in ((None, "not installed"), (("timeout", ""), "timed out"), ((1, ""), "gh auth status"),
                             ((0, "not json"), "no JSON")):
            with self.subTest(reading=reading):
                code, out, err, _ = self.state([(reading, checks("pass"))])
                self.assertEqual((code, out), (5, ""))
                self.assertIn(why, err)

    def test_bad_wait_exits_2(self):
        for args in (["--wait", "0"], ["--wait", "601"], ["--wait"], ["--wait", "x"], ["--timeout", "5"]):
            with self.subTest(args):
                code, out, _, calls = self.state([(view(), checks("pass"))], *args)
                self.assertEqual((code, out, calls), (2, "", []))
