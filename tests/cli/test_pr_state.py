import io
import json
import tempfile
from contextlib import nullcontext
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
JOB = "042-sample-export"
PR = "https://github.com/example/sample/pull/44"
HEAD = "2026-01-02T10:00:00Z"
PARENT = "factory/041-sample-api"
HEAD_SHA = "a" * 40


def review(state, at="2026-01-02T11:00:00Z", login="reviewer-a", association="MEMBER"):
    return {"author": {"login": login}, "authorAssociation": association, "state": state, "submittedAt": at}


def view(state="OPEN", decision="APPROVED", reviews=(), merge="CLEAN", auto=None, rollup=(), base="main", sha=HEAD_SHA):
    return 0, json.dumps({"state": state, "baseRefName": base, "headRefOid": sha, "reviewDecision": decision,
                          "latestReviews": list(reviews),
                          "mergeStateStatus": merge, "autoMergeRequest": auto, "author": {"login": "pr-owner"},
                          "statusCheckRollup": list(rollup),
                          "commits": [{"committedDate": "2026-01-01T09:00:00Z"}, {"committedDate": HEAD}]})


def graph(queue=False, threads=(), reviews=(), entry=None, merge_queue=False):
    return 0, json.dumps({"isInMergeQueue": queue, "isMergeQueueEnabled": merge_queue, "mergeQueueEntry": entry, "reviewThreads": {"nodes": list(threads)},
                          "latestReviews": {"nodes": [author(login) for login in reviews]}})


# GraphQL names a GitHub App by its bare slug and marks it with __typename Bot; only REST adds "[bot]".
BOTS = ("ci-helper", "lint-helper")


def author(login):
    return {"authorAssociation": "NONE" if login == "outsider" or login.endswith("[bot]") else "MEMBER",
            "author": {"__typename": "Bot" if login in BOTS else "User", "login": login}}


def thread(*logins, resolved=False, path="web/app.js", body="Drop the unused import."):
    return {"isResolved": resolved, "path": path,
            "comments": {"nodes": [dict(author(login), body=body) for login in logins]}}


def checks(*buckets):
    return 0, json.dumps([{"bucket": b, "name": f"check-{i}"} for i, b in enumerate(buckets)])


LINT = "check:da966368ea663ea5"
LINT_FAILS = (0, json.dumps([{"bucket": "fail", "name": "lint"}]))
AT = "2026-01-02T10:00:00Z"

TOKEN = "bk-synthetic-token-0001"
BUILD = "/v2/organizations/example-org/pipelines/sample-web/builds/812"
JOB_ID = "0190a1b2-0000-4000-8000-000000000001"
LINK = "https://buildkite.com/example-org/sample-web/builds/812"


def buildkite(link=LINK + "#" + JOB_ID, bucket="fail"):
    return 0, json.dumps([{"bucket": "pass", "name": "lint", "link": "https://github.com/example/sample/actions/runs/1"},
                          {"bucket": bucket, "name": "buildkite/sample-web", "link": link}])


def build_jobs(*jobs):
    return 200, {"jobs": [{"id": f"0190a1b2-0000-4000-8000-00000000000{i}", "type": kind, "state": state, "name": name}
                          for i, (kind, state, name) in enumerate(jobs, 1)]}


class PrStateTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "projects").mkdir()
        (self.home / "projects" / "sample.md").write_text(
            "---\nrepo: example/sample\nbase: main\nbuildkite_org: example-org\n---\n# Sample\n")
        self.now = 0.0

    def sleep(self, seconds):
        self.now += seconds

    def state(self, readings, *args, pr=PR, routes=None, upstream_base="main", handled=None, behind=0):
        """Run pr-state with gh answering each (view, checks[, queue]) reading in turn, repeating the last."""
        job = {"id": JOB, "project": "sample", "title": "Sample export", "stage": "pr-open"}
        if pr is not None:
            job["pr"] = pr
        if handled is not None:
            job["handled"] = handled
        calls = []

        def gh(*gh_args, stderr=False):
            calls.append(gh_args)
            if "/compare/" in gh_args[0]:
                return 0, f"{behind}\n"
            if gh_args[0].startswith("repos/"):
                return 0, upstream_base + "\n"
            reading = readings[min(sum(c[0] == "view" for c in calls) - 1, len(readings) - 1)]
            answer = {"view": reading[0], "checks": reading[1]}.get(gh_args[0], (reading + (graph(),))[2])
            return answer + ("",) if stderr and answer and len(answer) == 2 else answer

        with http_fixture({"/api/board": (200, {"updated": "", "jobs": [job]}), **(routes or {})}) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")
            self.server = server
            with patch.object(factory, "gh_pr", gh), patch.object(factory, "gh_cli", lambda *a: gh(*a[1:])), \
                    patch.object(factory, "BUILDKITE_API", server.origin + "/v2"), \
                    patch.object(factory.time, "sleep", self.sleep), \
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
        self.assertEqual(json.loads(out), {"pr": 44, "state": "open", "review": "approved", "bots": 0, "checks": "pass",
                                           "queued": False, "conflict": False, "stacked": False, "base": "main",
                                           "next": "merge", "ci_log": None, "threads": 0, "merge_queue": False,
                                           "items": [], "capped": [], "rearm": [], "behind": 0})
        self.assertEqual(calls[0][:3], ("view", PR, "--json"))
        self.assertIn("baseRefName", calls[0][3])
        self.assertIn("headRefOid", calls[0][3])
        self.assertNotIn("isInMergeQueue", calls[0][3])
        self.assertEqual(calls[1], ("checks", PR, "--required", "--json", "name,bucket,link"))
        self.assertEqual(calls[2][0], "graphql")
        self.assertIn("pullRequest(number:$n){isInMergeQueue isMergeQueueEnabled mergeQueueEntry{state} "
                      "reviewThreads(last:100){nodes{isResolved path "
                      "comments(first:100){nodes{authorAssociation body author{__typename login}}}}} "
                      "latestReviews(first:100){nodes{author{__typename login}}}}}}", calls[2][2])
        self.assertEqual(calls[2][3:], ("-f", "o=example", "-f", "r=sample", "-F", "n=44",
                                        "--jq", ".data.repository.pullRequest"))
        compares = [c for c in calls if "/compare/" in c[0]]
        self.assertEqual(compares, [(f"repos/example/sample/compare/main...{HEAD_SHA}", "--jq", ".behind_by")])

    def test_behind_pr_reads_update(self):
        code, out, err, _ = self.state([(view(), checks("pass"))], behind=2)
        self.assertEqual((code, err), (0, ""))
        reading = json.loads(out)
        self.assertEqual((reading["behind"], reading["next"]), (2, "update"))

    def test_merge_queue_skips_the_behind_read(self):
        code, out, err, calls = self.state([(view(), checks("pass"), graph(merge_queue=True))], behind=2)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual((json.loads(out)["behind"], json.loads(out)["next"]), (None, "merge"))
        self.assertFalse([c for c in calls if "/compare/" in c[0]])

    def test_behind_is_null_before_the_merge_decision(self):
        code, out, err, calls = self.state([(view(decision="REVIEW_REQUIRED"), checks("pass"))], behind=2)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual((json.loads(out)["behind"], json.loads(out)["next"]), (None, "needs-review"))
        self.assertFalse([c for c in calls if "/compare/" in c[0]])

    def test_bad_head_sha_exits_5(self):
        for sha in (None, "a" * 39, "A" * 40, "main"):
            with self.subTest(sha=sha):
                code, out, err, calls = self.state([(view(sha=sha), checks("pass"))])
                self.assertEqual((code, out), (5, ""))
                self.assertIn("gh pr view printed an unusable headRefOid", err)
                self.assertFalse([c for c in calls if "/compare/" in c[0]])

    def test_unanswered_bot_threads_ask_for_fixes(self):
        code, out, err, _ = self.state([(view(), checks("pass"), graph(threads=[thread("ci-helper")]))])
        self.assertEqual((code, err), (0, ""))
        reading = json.loads(out)
        self.assertEqual((reading["bots"], reading["next"], reading["review"]), (1, "fix-reviews", "approved"))
        two = graph(threads=[thread("ci-helper"), thread("lint-helper", "reviewer-a")])
        code, out, _, _ = self.state([(view(), checks("pass"), two)])
        self.assertEqual((json.loads(out)["bots"], json.loads(out)["next"]), (2, "fix-reviews"))

    def test_answered_resolved_and_outsider_threads_do_not_count(self):
        threads = [thread("ci-helper", resolved=True), thread("ci-helper", "pr-owner"),
                   thread("outsider"), thread("reviewer-a", "pr-owner"), thread("helper[bot]"), thread()]
        code, out, err, _ = self.state([(view(), checks("pass"), graph(threads=threads))])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual((json.loads(out)["bots"], json.loads(out)["next"]), (0, "merge"))
        bot = review("COMMENTED", login="ci-helper")
        self.assertEqual(self.next_of((view(reviews=[bot]), checks("pass"), graph(reviews=["ci-helper"]))), "merge")

    def test_reviewer_threads_ask_for_fixes_under_an_approval(self):
        reading = (view(), checks("pass"), graph(threads=[thread("reviewer-a"), thread("reviewer-b", "pr-owner"),
                                                          thread("ci-helper")]))
        code, out, err, _ = self.state([reading])
        self.assertEqual((code, err), (0, ""))
        got = json.loads(out)
        self.assertEqual((got["review"], got["bots"], got["threads"], got["next"]), ("approved", 1, 1, "fix-reviews"))
        only = (view(), checks("pass"), graph(threads=[thread("reviewer-a")]))
        self.assertEqual(self.next_of(only), "fix-reviews")

    def test_merge_queue_is_read_from_graphql(self):
        for enabled in (True, False):
            with self.subTest(enabled):
                code, out, _, _ = self.state([(view(), checks("pass"), graph(merge_queue=enabled))])
                self.assertEqual((code, json.loads(out)["merge_queue"]), (0, enabled))

    def test_no_required_checks_reads_as_none(self):
        code, out, err, calls = self.state([(view(rollup=[{"name": "nightly"}]),
                                             (1, "", "no required checks reported on the 'x' branch\n"))])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual((json.loads(out)["checks"], json.loads(out)["next"]), ("none", "merge"))

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
        bot = review("COMMENTED", login="ci-helper")
        self.assertEqual(self.next_of((view(reviews=[old, bot]), checks("pass"), graph(reviews=["ci-helper"]))),
                         "merge")

    def test_needs_review_without_a_human_approval(self):
        for reading in ((view(decision="REVIEW_REQUIRED"), checks("pass")),
                        (view(decision=""), checks("pass")),
                        (view(decision="", reviews=[review("APPROVED", login="ci-helper")]), checks("pass"),
                         graph(reviews=["ci-helper"]))):
            with self.subTest(reading):
                self.assertEqual(self.next_of(reading), "needs-review")
        code, out, _, _ = self.state([(view(decision=""), checks("pass"))])
        self.assertEqual(json.loads(out)["review"], "none")

    def test_unapproved_pr_is_worked_before_needs_review(self):
        required = {"decision": "REVIEW_REQUIRED"}
        for reading, step in (((view(merge="DIRTY", **required), checks("pass")), "rebase"),
                              ((view(**required), checks("fail")), "fix-ci"),
                              ((view(**required), checks("pending")), "wait"),
                              ((view(auto={"enabledAt": HEAD}, **required), checks("pass")), "needs-review"),
                              ((view(**required), checks("pass"), graph(True)), "needs-review"),
                              ((view(base=PARENT, **required), checks("pass")), "needs-review")):
            with self.subTest(reading, step=step):
                self.assertEqual(self.next_of(reading), step)

    def test_graphql_bot_reviews_never_count(self):
        bots = graph(reviews=["ci-helper", "reviewer-a"])
        approve = review("APPROVED", login="ci-helper")
        self.assertEqual(self.next_of((view(decision="", reviews=[approve]), checks("pass"), bots)), "needs-review")
        comment = review("COMMENTED", login="ci-helper")
        self.assertEqual(self.next_of((view(reviews=[comment]), checks("pass"), bots)), "merge")
        human = review("APPROVED")
        self.assertEqual(self.next_of((view(decision="", reviews=[approve, human]), checks("pass"), bots)), "merge")
        self.assertEqual(self.next_of((view(reviews=[comment, review("COMMENTED")]), checks("pass"), bots)),
                         "fix-reviews")

    def test_an_approving_latest_review_counts_when_no_decision(self):
        reading = (view(decision="", reviews=[review("APPROVED")]), checks("pass"))
        self.assertEqual(self.next_of(reading), "merge")

    def test_rebase_then_fix_ci_then_wait(self):
        self.assertEqual(self.next_of((view(merge="DIRTY"), checks("fail"))), "rebase")
        for bucket in ("fail", "cancel"):
            with self.subTest(bucket):
                self.assertEqual(self.next_of((view(), checks("pass", bucket, "pending"))), "fix-ci")
        self.assertEqual(self.next_of((view(), checks("pass", "pending"))), "wait")
        self.assertEqual(self.next_of((view(), checks("pass"), graph(True))), "wait")
        self.assertEqual(self.next_of((view(auto={"enabledAt": HEAD}), checks("pass"))), "wait")

    def test_unreadable_checks_read_as_none_when_the_pr_lists_none(self):
        for listing in ((1, ""), ("timeout", ""), (0, "not json"), (0, '[{"name": "x"}]')):
            with self.subTest(listing):
                code, out, _, _ = self.state([(view(), listing)])
                self.assertEqual((code, json.loads(out)["checks"], json.loads(out)["next"]), (0, "none", "merge"))

    def test_unreadable_checks_exit_5_when_the_pr_lists_some(self):
        for listing in ((1, ""), ("timeout", ""), (0, "not json")):
            with self.subTest(listing):
                code, out, err, _ = self.state([(view(rollup=[{"name": "ci"}]), listing)])
                self.assertEqual((code, out), (5, ""))
                self.assertIn("gh pr checks", err)

    def test_no_checks_waits_while_the_head_commit_is_new(self):
        head = factory.datetime(2026, 1, 2, 10, tzinfo=factory.timezone.utc).timestamp()
        for age, step in ((60, "wait"), (120, "merge")):
            with self.subTest(age), patch.object(factory.time, "time", lambda: head + age):
                self.assertEqual(self.next_of((view(), checks())), step)

    def test_only_owner_member_and_collaborator_reviews_count(self):
        for association in ("CONTRIBUTOR", "NONE", "FIRST_TIME_CONTRIBUTOR"):
            with self.subTest(association):
                approve = review("APPROVED", association=association)
                comment = review("COMMENTED", login="reviewer-b", association=association)
                self.assertEqual(self.next_of((view(decision="", reviews=[approve]), checks("pass"))),
                                 "needs-review")
                self.assertEqual(self.next_of((view(reviews=[comment]), checks("pass"))), "merge")
        for association in ("OWNER", "MEMBER", "COLLABORATOR"):
            with self.subTest(association):
                approve = review("APPROVED", association=association)
                self.assertEqual(self.next_of((view(decision="", reviews=[approve]), checks("pass"))), "merge")

    def test_a_reply_from_the_pr_author_answers_earlier_reviews(self):
        comment = review("COMMENTED")
        reply = review("COMMENTED", at="2026-01-02T12:00:00Z", login="pr-owner", association="OWNER")
        self.assertEqual(self.next_of((view(reviews=[comment, reply]), checks("pass"))), "merge")
        self.assertEqual(self.next_of((view(reviews=[reply]), checks("pass"))), "merge")
        later = review("COMMENTED", at="2026-01-02T13:00:00Z", login="reviewer-b")
        self.assertEqual(self.next_of((view(reviews=[comment, reply, later]), checks("pass"))), "fix-reviews")

    def test_a_merge_queue_entry_reads_as_queued(self):
        code, out, err, _ = self.state([(view(), checks("pass"), graph(entry={"state": "QUEUED"}))])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual((json.loads(out)["queued"], json.loads(out)["next"]), (True, "wait"))

    def test_wait_rereads_every_30_seconds_until_next_changes(self):
        pending = (view(), checks("pending"))
        code, out, err, calls = self.state([pending, pending, (view(), checks("pass"))], "--wait", "540")
        self.assertEqual((code, err, json.loads(out)["next"]), (0, "", "merge"))
        self.assertEqual(out.count("\n"), 1)
        self.assertEqual(self.now, 60)
        self.assertEqual(sum(c[0] == "view" for c in calls), 3)

    def test_a_stacked_pr_waits_for_its_parent(self):
        code, out, err, _ = self.state([(view(base=PARENT), checks("pass"))])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual((json.loads(out)["stacked"], json.loads(out)["base"], json.loads(out)["next"]),
                         (True, PARENT, "wait-base"))
        self.assertEqual(self.next_of((view(base=PARENT), checks("fail"))), "fix-ci")
        self.assertEqual(self.next_of((view(base=PARENT, merge="DIRTY"), checks("pass"))), "rebase")

    def test_wait_rereads_a_stacked_pr_until_it_is_retargeted(self):
        stacked = (view(base=PARENT), checks("pass"))
        code, out, err, calls = self.state([stacked, stacked, (view(), checks("pass"))], "--wait", "540")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual((json.loads(out)["stacked"], json.loads(out)["next"]), (False, "merge"))
        self.assertEqual(self.now, 60)
        self.assertEqual(sum(c[0] == "view" for c in calls), 3)

    def test_wait_stops_at_its_deadline(self):
        code, out, _, _ = self.state([(view(), checks("pending"))], "--wait", "45")
        self.assertEqual((code, json.loads(out)["next"]), (0, "wait"))
        self.assertEqual(self.now, 45)

    def test_without_wait_a_wait_reading_prints_at_once(self):
        code, out, _, calls = self.state([(view(), checks("pending"))])
        self.assertEqual((code, json.loads(out)["next"], self.now, len(calls)), (0, "wait", 0, 3))

    def test_bad_pr_exits_4(self):
        for pr, why in ((None, "no pr"), ("https://github.com/example/other/pull/44", "example/sample"),
                        ("not a url", "example/sample")):
            with self.subTest(pr=pr):
                code, out, err, calls = self.state([(view(), checks("pass"))], pr=pr)
                self.assertEqual((code, out, calls), (4, "", []))
                self.assertIn(why, err)

    def test_archive_pr_is_read(self):
        (self.home / "projects" / "sample.md").write_text(
            "---\nrepo: example/sample\narchive: example-upstream/sample\nbase: main\n---\n# Sample\n")
        upstream = "https://github.com/example-upstream/sample/pull/47"
        code, out, err, calls = self.state([(view(), checks("pass"))], pr=upstream)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual((json.loads(out)["pr"], json.loads(out)["next"]), (47, "merge"))
        self.assertEqual(calls[0], ("repos/example-upstream/sample", "--jq", ".default_branch"))
        self.assertEqual(calls[1][:2], ("view", upstream))
        job = {"id": JOB, "project": "sample", "title": "Sample export", "stage": "pr-open", "pr": upstream}
        with http_fixture({"/api/board": (200, {"updated": "", "jobs": [job]})}) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")
            code, out, err = invoke(factory, ["--home", str(self.home), "pr-queue", f"sample/{JOB}"])
        self.assertEqual((code, out), (4, ""))
        self.assertIn("is not a pull request in example/sample", err)

    def test_archive_pr_compares_the_archive_default_branch(self):
        (self.home / "projects" / "sample.md").write_text(
            "---\nrepo: example/sample\narchive: example-upstream/sample\nbase: main\n---\n# Sample\n")
        upstream = "https://github.com/example-upstream/sample/pull/47"
        code, out, err, _ = self.state([(view(base="trunk"), checks("pass"))], pr=upstream, upstream_base="trunk")
        self.assertEqual((code, err), (0, ""))
        reading = json.loads(out)
        self.assertEqual((reading["base"], reading["stacked"], reading["next"]), ("trunk", False, "merge"))

    def test_archive_pr_unusable_default_branch_exits_5(self):
        (self.home / "projects" / "sample.md").write_text(
            "---\nrepo: example/sample\narchive: example-upstream/sample\nbase: main\n---\n# Sample\n")
        upstream = "https://github.com/example-upstream/sample/pull/47"
        code, out, err, _ = self.state([(view(), checks("pass"))], pr=upstream, upstream_base="-bad")
        self.assertEqual((code, out), (5, ""))
        self.assertIn("gh printed an unusable default_branch", err)

    def test_gh_problem_exits_5(self):
        for reading, why in ((None, "not installed"), (("timeout", ""), "timed out"), ((1, ""), "gh auth status"),
                             ((0, "not json"), "no JSON")):
            with self.subTest(reading=reading):
                code, out, err, _ = self.state([(reading, checks("pass"))])
                self.assertEqual((code, out), (5, ""))
                self.assertIn(why, err)
        for reading, why in ((None, "not installed"), (("timeout", ""), "gh api graphql timed out"),
                             ((1, ""), "gh api graphql exited 1"),
                             ((0, "not json"), "gh api graphql printed no JSON object")):
            with self.subTest(queue=reading):
                code, out, err, _ = self.state([(view(), checks("pass"), reading)])
                self.assertEqual((code, out), (5, ""))
                self.assertIn(why, err)

    def test_a_missing_base_reads_as_the_project_base(self):
        code, out, err, _ = self.state([(view(base=None), checks("pass"))])
        self.assertEqual((code, err), (0, ""))
        self.assertEqual((json.loads(out)["base"], json.loads(out)["stacked"]), ("main", False))

    def test_an_unusable_base_exits_5(self):
        for base in ("main;id", "feat branch", "$(id)", "-x", "a" * 256, 7):
            with self.subTest(base=base):
                code, out, err, _ = self.state([(view(base=base), checks("pass"))])
                self.assertEqual((code, out), (5, ""))
                self.assertIn("gh printed an unusable baseRefName", err)

    def buildkite_requests(self):
        return [r for r in self.server.requests if r[1].startswith("/v2/")]

    def test_fix_ci_reads_the_failed_buildkite_job_log_tail(self):
        (self.home / ".env").write_text(f"BUILDKITE_API_TOKEN={TOKEN}\nSAMPLE_SECRET=sample-secret-value-0002\n")
        lines = [f"\x1b_bk;t=17000000{i:02}000\x07\x1b[32mstep {i}\x1b[0m" for i in range(100)]
        log = "\r\n".join(lines + ["progress 10%\rprogress 100%", f"using {TOKEN} and sample-secret-value-0002",
                                    "error  'unused' is defined but never used"]) + "\r\n"
        routes = {BUILD: build_jobs(("script", "failed", "Lint frontend")),
                  f"{BUILD}/jobs/{JOB_ID}/log": (200, {"url": "", "content": log, "size": len(log)})}
        code, out, err, _ = self.state([(view(), buildkite())], routes=routes)
        self.assertEqual((code, err), (0, ""))
        reading = json.loads(out)
        tail = "\n".join([f"step {i}" for i in range(23, 100)] + [
            "progress 100%", "using [redacted] and [redacted]", "error  'unused' is defined but never used"])
        self.assertEqual((reading["next"], reading["ci_log"]), ("fix-ci", {
            "check": "buildkite/sample-web", "job": "Lint frontend", "tail": tail, "problem": ""}))
        sent = self.buildkite_requests()
        self.assertEqual([r[1] for r in sent], [BUILD, f"{BUILD}/jobs/{JOB_ID}/log"])
        for request in sent:
            self.assertEqual((request[2]["Authorization"], request[2]["Accept"], request[2]["User-Agent"]),
                             (f"Bearer {TOKEN}", "application/json", "factory"))
        self.assertNotIn(TOKEN, out)

    def test_fix_ci_picks_the_first_failed_buildkite_job(self):
        (self.home / ".env").write_text(f"BUILDKITE_API_TOKEN={TOKEN}\n")
        second = "0190a1b2-0000-4000-8000-000000000003"
        routes = {BUILD: build_jobs(("waiter", "failed", ""), ("script", "passed", "Unit tests"),
                                    ("script", "timed_out", "Lint frontend"), ("script", "failed", "Type check")),
                  f"{BUILD}/jobs/{second}/log": (200, {"content": "x" * 9000 + "\n"})}
        code, out, err, _ = self.state([(view(), buildkite(LINK))], routes=routes)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out)["ci_log"], {"check": "buildkite/sample-web", "job": "Lint frontend",
                                                     "tail": "x" * 8000, "problem": ""})
        self.assertEqual([r[1] for r in self.buildkite_requests()], [BUILD, f"{BUILD}/jobs/{second}/log"])
        routes = {BUILD: build_jobs(("script", "passed", "Unit tests"))}
        code, out, _, _ = self.state([(view(), buildkite(LINK))], routes=routes)
        self.assertEqual((code, json.loads(out)["ci_log"]["problem"]), (0, "no failed job in build 812"))

    def test_buildkite_problems_still_print_the_reading(self):
        real_open = factory.urllib.request.OpenerDirector.open

        def refuse(opener, request, *args, **kwargs):
            if "/v2/" in request.full_url:
                raise error
            return real_open(opener, request, *args, **kwargs)

        def problem(env, routes, down=False):
            if env:
                (self.home / ".env").write_text(env)
            with patch.object(factory.urllib.request.OpenerDirector, "open", refuse) if down else nullcontext():
                code, out, err, _ = self.state([(view(), buildkite())], routes=routes)
            self.assertEqual((code, err), (0, ""))
            self.assertNotIn(TOKEN, out)
            reading = json.loads(out)
            self.assertEqual((reading["next"], reading["ci_log"]["job"], reading["ci_log"]["tail"]), ("fix-ci", "", ""))
            return reading["ci_log"]["problem"]

        self.assertEqual(problem("", {BUILD: build_jobs()}),
                         "set BUILDKITE_API_TOKEN in the factory .env to read Buildkite logs")
        self.assertEqual(self.buildkite_requests(), [])
        env = f"BUILDKITE_API_TOKEN={TOKEN}\n"
        self.assertEqual(problem(env, {BUILD: (401, {})}),
                         "Buildkite answered 401 (the token needs read_builds and read_build_logs)")
        self.assertEqual(problem(env, {BUILD: (302, {})}), "Buildkite answered 302")
        self.assertEqual(problem(env, {BUILD: (200, b"not json")}), "Buildkite sent no usable log")
        self.assertEqual(problem(env, {BUILD: build_jobs(("script", "failed", "Lint frontend")),
                                       f"{BUILD}/jobs/{JOB_ID}/log": (200, {"size": 0})}),
                         "Buildkite sent no usable log")
        error = OSError(f"refused for {TOKEN}")
        self.assertEqual(problem(env, {}, down=True), "can't reach Buildkite: refused for [token]")
        error = factory.http.client.IncompleteRead(b"")
        self.assertEqual(problem(env, {}, down=True), "can't reach Buildkite: IncompleteRead(0 bytes read)")

    def test_a_link_to_another_org_reads_no_log(self):
        (self.home / ".env").write_text(f"BUILDKITE_API_TOKEN={TOKEN}\n")
        code, out, err, _ = self.state([(view(), buildkite("https://buildkite.com/other-org/payroll/builds/77"))])
        self.assertEqual((code, err), (0, ""))
        reading = json.loads(out)
        self.assertEqual((reading["next"], reading["ci_log"]["problem"]), ("fix-ci", "the check links to Buildkite "
                         "org other-org, not the project's buildkite_org example-org"))
        self.assertEqual(self.buildkite_requests(), [])

    def test_without_buildkite_org_no_log_is_read(self):
        (self.home / ".env").write_text(f"BUILDKITE_API_TOKEN={TOKEN}\n")
        (self.home / "projects" / "sample.md").write_text("---\nrepo: example/sample\nbase: main\n---\n# Sample\n")
        code, out, err, _ = self.state([(view(), buildkite())], routes={BUILD: build_jobs()})
        self.assertEqual((code, err), (0, ""))
        reading = json.loads(out)
        self.assertEqual((reading["next"], reading["ci_log"]["problem"]),
                         ("fix-ci", "set buildkite_org in the project frontmatter to read Buildkite logs"))
        self.assertEqual(self.buildkite_requests(), [])

    def test_a_buildkite_log_over_the_read_cap_is_no_usable_log(self):
        (self.home / ".env").write_text(f"BUILDKITE_API_TOKEN={TOKEN}\n")
        routes = {BUILD: (200, {"jobs": [{"id": JOB_ID}]}),
                  f"{BUILD}/jobs/{JOB_ID}/log": (200, {"content": "x" * 100})}
        with patch.object(factory, "BUILDKITE_MAX_BYTES", 64):
            code, out, err, _ = self.state([(view(), buildkite())], routes=routes)
        self.assertEqual((code, err), (0, ""))
        ci_log = json.loads(out)["ci_log"]
        self.assertEqual((ci_log["problem"], ci_log["tail"]), ("Buildkite sent no usable log", ""))
        self.assertEqual([r[1] for r in self.buildkite_requests()], [BUILD, f"{BUILD}/jobs/{JOB_ID}/log"])

    def test_ci_log_is_null_without_a_buildkite_failure(self):
        (self.home / ".env").write_text(f"BUILDKITE_API_TOKEN={TOKEN}\n")
        actions = (0, json.dumps([{"bucket": "fail", "name": "test",
                                   "link": "https://github.com/example/sample/actions/runs/1/job/2"}]))
        for reading, step in (((view(), buildkite(bucket="pass")), "merge"), ((view(), actions), "fix-ci"),
                              ((view(merge="DIRTY"), buildkite()), "rebase"),
                              ((view(), buildkite("https://buildkite.com/example-org/sample-web/builds/812/x")),
                               "fix-ci"),
                              ((view(), buildkite("https://buildkite.com/example-org/../builds/812")), "fix-ci")):
            with self.subTest(reading=reading):
                code, out, err, _ = self.state([reading], routes={BUILD: build_jobs()})
                self.assertEqual((code, err, json.loads(out)["next"], json.loads(out)["ci_log"]), (0, "", step, None))
                self.assertEqual(self.buildkite_requests(), [])

    def test_failing_check_is_an_item(self):
        code, out, err, _ = self.state([(view(), LINT_FAILS)])
        self.assertEqual((code, err), (0, ""))
        reading = json.loads(out)
        self.assertEqual((reading["items"], reading["capped"], reading["rearm"], reading["next"]),
                         ([{"sig": LINT, "label": "lint", "count": 0}], [], [], "fix-ci"))

    def test_handled_thread_is_skipped(self):
        sig = factory.handled_sig("thread", "web/app.js", "Drop the unused import.")
        code, out, err, _ = self.state([(view(), checks("pass"), graph(threads=[thread("ci-helper")]))],
                                       handled={sig: {"count": 1, "at": AT}})
        self.assertEqual((code, err), (0, ""))
        reading = json.loads(out)
        self.assertEqual((reading["bots"], reading["items"], reading["next"]), (0, [], "merge"))
        code, out, _, _ = self.state([(view(), checks("pass"), graph(threads=[thread("ci-helper", body="Rename x.")]))],
                                     handled={sig: {"count": 1, "at": AT}})
        reading = json.loads(out)
        self.assertEqual((reading["bots"], reading["next"]), (1, "fix-reviews"))
        self.assertEqual(reading["items"][0]["label"], "web/app.js")

    def test_check_at_the_cap_reads_capped(self):
        code, out, err, _ = self.state([(view(), LINT_FAILS)], handled={LINT: {"count": 3, "at": AT}})
        self.assertEqual((code, err), (0, ""))
        reading = json.loads(out)
        self.assertEqual((reading["capped"], reading["items"], reading["next"], reading["ci_log"]),
                         ([{"sig": LINT, "label": "lint", "count": 3}], [], "capped", None))

    def test_check_under_the_cap_is_worked_again(self):
        code, out, err, _ = self.state([(view(), LINT_FAILS)], handled={LINT: {"count": 2, "at": AT}})
        self.assertEqual((code, err), (0, ""))
        reading = json.loads(out)
        self.assertEqual((reading["items"], reading["capped"], reading["next"]),
                         ([{"sig": LINT, "label": "lint", "count": 2}], [], "fix-ci"))

    def test_passing_check_and_mergeable_pr_rearm(self):
        conflict = factory.handled_sig("conflict", "main")
        handled = {LINT: {"count": 2, "at": AT}, conflict: {"count": 1, "at": AT}}
        passing = (0, json.dumps([{"bucket": "pass", "name": "lint"}]))
        code, out, err, _ = self.state([(view(), passing)], handled=handled)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual((json.loads(out)["rearm"], json.loads(out)["next"]), ([LINT, conflict], "merge"))
        for merge in ("DIRTY", "UNKNOWN"):
            with self.subTest(merge):
                code, out, _, _ = self.state([(view(merge=merge), passing)], handled=handled)
                self.assertEqual((code, json.loads(out)["rearm"]), (0, [LINT]))
        code, out, _, _ = self.state([(view(merge="DIRTY"), passing)], handled=handled)
        self.assertEqual((json.loads(out)["items"], json.loads(out)["next"]),
                         ([{"sig": conflict, "label": "a merge conflict with main", "count": 1}], "rebase"))

    def test_a_listed_item_is_not_rearmed(self):
        twice = (0, json.dumps([{"bucket": "fail", "name": "lint"}, {"bucket": "pass", "name": "lint"}]))
        code, out, err, _ = self.state([(view(), twice)], handled={LINT: {"count": 2, "at": AT}})
        self.assertEqual((code, err), (0, ""))
        reading = json.loads(out)
        self.assertEqual((reading["items"], reading["rearm"]), ([{"sig": LINT, "label": "lint", "count": 2}], []))

    def test_bad_wait_exits_2(self):
        for args in (["--wait", "0"], ["--wait", "601"], ["--wait"], ["--wait", "x"], ["--timeout", "5"]):
            with self.subTest(args):
                code, out, _, calls = self.state([(view(), checks("pass"))], *args)
                self.assertEqual((code, out, calls), (2, "", []))


def recent():
    return (datetime.now(timezone.utc) - timedelta(minutes=5)).strftime("%Y-%m-%dT%H:%M:%SZ")


class PrHandledTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "projects").mkdir()
        (self.home / "projects" / "sample.md").write_text("---\nrepo: example/sample\nbase: main\n---\n# Sample\n")
        self.claim = {"device": "host-a", "session": "s1", "since": recent(), "seen": recent()}

    def run_cmd(self, *args, handled=None, claim=None):
        """Run pr-handled as host-a session s1 and return (code, out, err, the written row or None)."""
        job = {"id": JOB, "project": "sample", "stage": "pr-open", "pr": PR, "claim": claim or self.claim}
        if handled is not None:
            job["handled"] = handled
        with http_fixture({"/api/board": (200, {"updated": "", "jobs": [job]})}) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\ndevice: host-a\n---\n")
            code, out, err = invoke(factory, ["--home", str(self.home), "--session", "s1", "pr-handled",
                                              f"sample/{JOB}", *args])
        puts = [r for r in server.requests if r[0] == "PUT"]
        return code, out, err, json.loads(puts[-1][3])["jobs"][0] if puts else None

    def test_counts_each_sig(self):
        thread_sig = factory.handled_sig("thread", "web/app.js", "Drop the unused import.")
        code, out, err, row = self.run_cmd(LINT, thread_sig, handled={LINT: {"count": 2, "at": AT}})
        self.assertEqual((code, err), (0, ""))
        self.assertEqual((row["handled"][LINT]["count"], row["handled"][thread_sig]["count"]), (3, 1))
        self.assertRegex(row["handled"][LINT]["at"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
        self.assertEqual(row["claim"], self.claim)
        self.assertEqual(json.loads(out), {"id": JOB, "handled": row["handled"]})

    def test_rearm_drops_entries(self):
        conflict = factory.handled_sig("conflict", "main")
        handled = {LINT: {"count": 2, "at": AT}, conflict: {"count": 1, "at": AT}}
        code, _, err, row = self.run_cmd("--rearm", LINT, handled=handled)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(row["handled"], {conflict: {"count": 1, "at": AT}})

    def test_keeps_the_newest_32(self):
        old = {f"check:{i:016x}": {"count": 1, "at": f"2026-01-01T00:00:{i:02}Z"} for i in range(32)}
        code, _, err, row = self.run_cmd(LINT, handled=old)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(len(row["handled"]), 32)
        self.assertIn(LINT, row["handled"])
        self.assertNotIn("check:0000000000000000", row["handled"])
        self.assertIn("check:0000000000000001", row["handled"])

    def test_bad_sigs_exit_2(self):
        for args in (["lint"], ["check:DA966368EA663EA5"], ["merge:da966368ea663ea5"], ["check:da96636"],
                     ["--rearm"], [f"check:{i:016x}" for i in range(33)], ["--rearm", "--rearm"]):
            with self.subTest(args=args[:2]):
                code, out, err, row = self.run_cmd(*args)
                self.assertEqual((code, out, row), (2, "", None))
                self.assertIn("pr-handled", err)

    def test_other_live_claim_exits_6(self):
        other = {"device": "host-b", "session": "s9", "since": recent(), "seen": recent()}
        code, out, err, row = self.run_cmd(LINT, claim=other)
        self.assertEqual((code, out, row), (6, "", None))
        self.assertIn("is held by host-b session s9", err)
        self.assertIn("Nothing was written", err)

    def test_board_refuses_handled(self):
        with patch.object(factory.sys, "stdin", io.StringIO(json.dumps({"handled": {LINT: {"count": 1, "at": AT}}}))):
            result = invoke(factory, ["board", str(self.home), JOB])
        self.assertEqual(result, (2, "", "factory.py: only factory.py pr-handled sets handled. Nothing was written.\n"))
