import contextlib
import io
import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import SKILL, IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
JOB = "042-sample-export"
PR = "https://github.com/example/sample/pull/42"
OPEN = (0, "OPEN\n")
WARN = "factory.py: warning: gh api graphql gave no usable check list ({})\n"
NEVER = "factory.py: gh api graphql never gave a usable check list\n"
OLD = "a1" * 20
NEW = "b2" * 20
UPDATED = ("factory.py: PR #42 was 1 commit behind main; gh pr update-branch merged main into it, so checks run "
           "again on the new head.\n")


VISIBLE = {"security": "PASS", "ux": "PASS", "ui": "PASS", "code": "PASS"}


def listed(*checks):
    return [{"bucket": bucket, "name": name} for bucket, name in checks], None


def unusable(problem):
    return [], problem


class ChecksTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "projects").mkdir()
        (self.home / "projects" / "sample.md").write_text("---\nrepo: example/sample\nbase: main\n---\n# Sample\n")
        self.now = 0.0

    def sleep(self, seconds):
        self.now += seconds

    def checks(self, view, polls, *args, pr=PR, extra=None, behind=None, update=(0, "", ""), others=()):
        """Run checks with gh answering view first. pr_behind gives each behind reading in turn, then head_checks
        gives each (checks, problem) poll in turn, both repeating the last, and update-branch answers update."""
        job = {"id": JOB, "project": "sample", "title": "Sample export", "stage": "pr-open"}
        job.update(extra or {})
        if pr is not None:
            job["pr"] = pr
        behind = behind or [(0, "a" * 40, "main")]
        calls = []

        def gh(*gh_args, stderr=False):
            calls.append(gh_args)
            if gh_args[0] == "view":
                return view
            return update

        def pr_behind(behind_pr):
            calls.append(("pr_behind", behind_pr))
            return behind[min(sum(c[0] == "pr_behind" for c in calls) - 1, len(behind) - 1)]

        def head_checks(head_pr, sha):
            calls.append(("head_checks", head_pr, sha))
            return polls[min(sum(c[0] == "head_checks" for c in calls) - 1, len(polls) - 1)]

        with http_fixture({"/api/board": (200, {"updated": "", "jobs": [job, *others]})}) as server:
            self.requests = server.requests
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")
            with patch.object(factory, "gh_pr", gh), patch.object(factory, "pr_behind", pr_behind), \
                    patch.object(factory, "head_checks", head_checks), \
                    patch.object(factory.time, "sleep", self.sleep), \
                    patch.object(factory.time, "monotonic", lambda: self.now):
                result = invoke(factory, ["--home", str(self.home), "checks", f"sample/{JOB}", *args])
        return result + (calls,)

    def test_behind_pr_updates_and_waits(self):
        code, out, err, calls = self.checks(
            OPEN, [listed(("pass", "ci")), listed(), listed(("pending", "ci")), listed(("pass", "ci"))],
            "--timeout", "540", behind=[(1, OLD, "main"), (0, NEW, "main")])
        self.assertEqual((code, out, err), (0, "pass ci\n", UPDATED))
        self.assertEqual([c for c in calls if c[0] == "update-branch"], [("update-branch", PR)])
        self.assertEqual(len([c for c in calls if c[0] == "pr_behind"]), 4)
        self.assertEqual(self.now, 30)

    def test_up_to_date_pr_makes_no_update(self):
        code, out, err, calls = self.checks(OPEN, [listed(("pass", "ci"))])
        self.assertEqual((code, out, err), (0, "pass ci\n", ""))
        self.assertNotIn("update-branch", [c[0] for c in calls])

    def test_stale_head_after_update_is_pending(self):
        code, out, err, calls = self.checks(OPEN, [listed(("pass", "ci"))], "--timeout", "540",
                                            behind=[(1, OLD, "main"), (1, OLD, "main"), (0, NEW, "main")])
        self.assertEqual((code, out, err), (0, "pass ci\n", UPDATED))
        self.assertEqual([c[0] for c in calls].count("update-branch"), 1)
        self.assertEqual(self.now, 20)

    def test_new_head_passing_exits_0_on_first_reading(self):
        code, out, err, calls = self.checks(OPEN, [listed(("pass", "ci"))], "--timeout", "540",
                                            behind=[(1, OLD, "main"), (0, NEW, "main")])
        self.assertEqual((code, out, err), (0, "pass ci\n", UPDATED))
        self.assertEqual([c[0] for c in calls].count("pr_behind"), 2)
        self.assertEqual(self.now, 10)

    def test_checks_are_read_for_the_head_sha(self):
        _, _, _, calls = self.checks(OPEN, [listed(("pass", "ci")), listed(), listed(("pass", "ci"))],
                                     "--timeout", "540", behind=[(1, OLD, "main"), (1, OLD, "main"), (0, NEW, "main")])
        polls = [c for c in calls if c[0] in ("pr_behind", "head_checks")]
        self.assertEqual(polls, [("pr_behind", PR), ("head_checks", PR, OLD)] * 2
                         + [("pr_behind", PR), ("head_checks", PR, NEW)])

    def test_stale_head_at_deadline_exits_3(self):
        code, out, err, _ = self.checks(OPEN, [listed(("pass", "ci"))], "--timeout", "30", behind=[(1, OLD, "main")])
        self.assertEqual((code, out, err),
                         (3, "", UPDATED + "factory.py: still waiting for checks on the updated head\n"))
        self.assertEqual(self.now, 30)

    def test_register_window_restarts_after_update(self):
        polls = [listed(("pending", "ci"))] * 10 + [listed(("pass", "ci"))] + [listed()] * 11 + [listed(("pass", "ci"))]
        code, out, err, _ = self.checks(OPEN, polls, "--timeout", "540",
                                        behind=[(1, OLD, "main")] * 11 + [(0, NEW, "main")])
        self.assertEqual((code, out, err), (0, "pass ci\n", UPDATED))
        self.assertEqual(self.now, 220)

    def test_update_failure_exits_6(self):
        code, out, err, _ = self.checks(OPEN, [listed(("pass", "ci"))], behind=[(2, OLD, "main")],
                                        update=(1, "", "merge conflict between base and head\nmore detail\n"))
        self.assertEqual((code, out), (6, ""))
        self.assertEqual(err, "factory.py: gh pr update-branch could not bring PR #42 up to date with main: merge "
                              "conflict between base and head more detail. Merge main into the branch and push.\n")
        for update, why in ((("timeout", "", ""), "gh pr update-branch timed out after 60 seconds"),
                            (None, "gh is not installed")):
            with self.subTest(update):
                code, out, err, _ = self.checks(OPEN, [listed(("pass", "ci"))], behind=[(2, OLD, "main")],
                                                update=update)
                self.assertEqual((code, out, err), (5, "", f"factory.py: {why}\n"))

    def test_failing_checks_skip_update(self):
        code, out, err, calls = self.checks(OPEN, [listed(("pass", "test"), ("fail", "ci"))],
                                            behind=[(1, OLD, "main")])
        self.assertEqual((code, out, err), (1, "pass test\nfail ci\n", "factory.py: failing checks: ci\n"))
        self.assertEqual([c[0] for c in calls], ["view", "pr_behind", "head_checks"])

    def test_all_pass_prints_each_check_and_exits_0(self):
        code, out, err, calls = self.checks(OPEN, [unusable("exited 1 with no check list"),
                                                   listed(("pending", "test"), ("pass", "lint")),
                                                   listed(("pass", "test"), ("pass", "lint"))])
        self.assertEqual((code, out, err), (0, "pass test\npass lint\n", WARN.format("exited 1 with no check list")))
        self.assertEqual(calls[0][:2], ("view", PR))
        self.assertEqual(calls[1:3], [("pr_behind", PR), ("head_checks", PR, "a" * 40)])
        self.assertEqual(self.now, 20)

    def test_skipping_counts_as_passing(self):
        code, out, err, _ = self.checks(OPEN, [listed(("pass", "test"), ("skipping", "lint"))])
        self.assertEqual((code, out, err), (0, "pass test\nskipping lint\n", ""))

    def test_no_checks_after_120_seconds_exits_0(self):
        code, out, err, _ = self.checks(OPEN, [listed()], "--timeout", "540")
        self.assertEqual((code, out, err), (0, "no checks reported\n", ""))
        self.assertEqual(self.now, 120)

    def test_no_usable_reply_by_120_seconds_exits_5(self):
        for problem in ("exited 1 with no check list", "timed out after 60 seconds", "printed no check list",
                        "listed more than 100 checks", "gh is not installed"):
            with self.subTest(problem):
                self.now = 0.0
                code, out, err, _ = self.checks(OPEN, [unusable(problem)], "--timeout", "540")
                self.assertEqual((code, out), (5, ""))
                self.assertEqual(err, WARN.format(problem) * 13 + NEVER)
                self.assertEqual(self.now, 120)

    def test_one_usable_reply_keeps_exit_0(self):
        miss = unusable("exited 1 with no check list")
        code, out, err, _ = self.checks(OPEN, [miss, listed(), miss], "--timeout", "540")
        self.assertEqual((code, out), (0, "no checks reported\n"))
        self.assertEqual(err, WARN.format("exited 1 with no check list") * 12)

    def test_failing_or_other_bucket_exits_1(self):
        for bucket in ("fail", "cancel", "neutral"):
            with self.subTest(bucket):
                code, out, err, _ = self.checks(OPEN, [listed(("pass", "test"), (bucket, "lint"))])
                self.assertEqual((code, out), (1, f"pass test\n{bucket} lint\n"))
                self.assertEqual(err, "factory.py: failing checks: lint\n")

    def test_pending_at_deadline_exits_3(self):
        code, out, err, _ = self.checks(OPEN, [listed(("pending", "test"), ("pass", "lint"))], "--timeout", "540")
        self.assertEqual((code, out, err), (3, "", "factory.py: still pending: test\n"))
        self.assertEqual(self.now, 540)

    def test_last_sleep_stops_at_the_deadline(self):
        code, out, err, _ = self.checks(OPEN, [listed(("pending", "test"))], "--timeout", "25")
        self.assertEqual((code, out, err), (3, "", "factory.py: still pending: test\n"))
        self.assertEqual(self.now, 25)

    def test_none_listed_at_deadline_before_120_seconds_exits_3(self):
        code, out, err, _ = self.checks(OPEN, [listed()], "--timeout", "30")
        self.assertEqual((code, out, err), (3, "", "factory.py: no checks listed yet\n"))
        self.assertEqual(self.now, 30)
        code, out, err, _ = self.checks(OPEN, [unusable("exited 1 with no check list")], "--timeout", "30")
        self.assertEqual((code, out, err), (5, "", WARN.format("exited 1 with no check list") * 4 + NEVER))

    def test_bad_pr_exits_4(self):
        for pr, view, why in ((None, OPEN, "no pr"), ("https://github.com/example/other/pull/42", OPEN, "example/sample"),
                              ("not a url", OPEN, "example/sample"), (PR, (0, "MERGED\n"), "MERGED")):
            with self.subTest(pr=pr, view=view):
                code, out, err, _ = self.checks(view, [listed(("pass", "test"))], pr=pr)
                self.assertEqual((code, out), (4, ""))
                self.assertIn(why, err)

    def test_gh_problem_exits_5(self):
        for view, why in ((None, "not installed"), (("timeout", ""), "timed out"), ((1, ""), "gh auth status")):
            with self.subTest(view=view):
                code, out, err, calls = self.checks(view, [listed(("pass", "test"))])
                self.assertEqual((code, out, len(calls)), (5, "", 1))
                self.assertIn(why, err)

    def decision(self, body):
        folder = self.home / "jobs" / "sample" / JOB
        folder.mkdir(parents=True)
        (folder / "decision.md").write_bytes(body)

    def test_escalate_decision_exits_7_before_gh(self):
        self.decision(b"\n  ESCALATE: auth change needs a person\n")
        code, out, err, calls = self.checks(OPEN, [listed(("pass", "test"))], extra={"automerge": True})
        self.assertEqual((code, out, calls), (7, "", []))
        self.assertEqual(err, f"factory.py: {JOB} has a decision.md that says ESCALATE, so a person reviews and "
                              f"merges {PR}. Nothing merges.\n")

    def test_escalated_board_row_exits_7(self):
        for extra in ({"escalated": True}, {"note": "Escalated: approver held it"}):
            with self.subTest(extra):
                code, out, err, calls = self.checks(OPEN, [listed(("pass", "test"))], extra=extra)
                self.assertEqual((code, out, calls), (7, "", []))
                self.assertEqual(err, f"factory.py: {JOB} is escalated on the board, so a person reviews and "
                                      f"merges {PR}. Nothing merges.\n")

    def test_earned_autonomy_hold_exits_8_before_gh(self):
        reverted = {"id": "040-sample-import", "project": "sample", "stage": "merged", "done": factory.now(),
                    "rounds": [VISIBLE], "outcome": {"result": "reverted", "at": "2026-01-09T12:00:00Z"}}
        code, out, err, calls = self.checks(OPEN, [listed(("pass", "test"))], others=[reverted],
                                            extra={"automerge": True, "rounds": [VISIBLE]})
        self.assertEqual((code, out, calls), (8, "", []))
        self.assertEqual(err, f"factory.py: {JOB} is held: earned autonomy holds sample visible PRs after reverted "
                              "040-sample-import; a person lifts it with factory.py autonomy restore sample visible. "
                              f"A person reviews and merges {PR}. Nothing merges.\n")
        puts = [json.loads(body) for method, _, _, body in self.requests if method == "PUT"]
        self.assertEqual(len(puts), 1)
        job = next(j for j in puts[0]["jobs"] if j["id"] == JOB)
        self.assertEqual((job["automerge"], job["note"]), (False, "earned autonomy hold: a person reviews and merges"))
        self.assertTrue(job["needs"].startswith("review and merge PR #42: earned autonomy holds sample visible"))

    def test_unreadable_decision_exits_7(self):
        self.decision(b"\xff\n")
        code, out, err, calls = self.checks(OPEN, [listed(("pass", "test"))])
        self.assertEqual((code, out, calls), (7, "", []))
        self.assertEqual(err, f"factory.py: {JOB} has a decision.md that cannot be read, so a person reviews and "
                              f"merges {PR}. Nothing merges.\n")

    def test_approve_decision_reads_checks(self):
        self.decision(b"APPROVE\n")
        code, out, err, calls = self.checks(OPEN, [listed(("pass", "test"))])
        self.assertEqual((code, out, err), (0, "pass test\n", ""))
        self.assertEqual(calls[0][:2], ("view", PR))

    def test_bad_timeout_exits_2(self):
        for args in (["--timeout", "601"], ["--timeout", "-1"], ["--timeout"], ["--timeout", "1", "--timeout", "1"],
                     ["--wait", "1"]):
            with self.subTest(args):
                code, out, _, calls = self.checks(OPEN, [listed(("pass", "test"))], *args)
                self.assertEqual((code, out, calls), (2, "", []))

    def test_skill_uses_checks(self):
        skill = " ".join((SKILL / "SKILL.md").read_text(encoding="utf-8").split())
        self.assertIn('Wait for checks with `python3 "$RUNTIME" checks "$SLUG/<id>" --timeout 540`. Exit 0 means '
                      "they passed, or the repo has none. When checks pass on a PR behind `$BASE`, checks runs "
                      "`gh pr update-branch` and waits for checks on the new head. Exit 3 means they are still "
                      "pending, so run it again. Exit 1 names the failing checks. Exit 4 means no open PR; 5 means gh "
                      "never gave a usable reply. Exit 6 means `gh pr update-branch` failed, usually on a conflict: "
                      "treat it as a merge conflict. Exit 7 means the approver escalated the job: the board marks it escalated or its decision.md "
                      'says ESCALATE. Nothing merges. Patch {"automerge": false, "escalated": true, "needs": "review '
                      'and merge PR #<n>: the approver escalated it", "note": "escalated: checks held the merge"} '
                      "and stop. Never poll `gh pr checks` in a loop or with `--watch`.", skill)
        self.assertIn("Pass `--hold` to refresh this session's claim first, so a re-run during a long build needs "
                      "no separate `hold`.", skill)


class PrBehindTest(IsolatedTestCase):
    def behind(self, view, compare):
        calls = []

        def gh_cli(*args, stderr=False):
            calls.append(args)
            return compare

        with patch.object(factory, "gh_pr", lambda *args, stderr=False: calls.append(args) or view), \
                patch.object(factory, "gh_cli", gh_cli):
            return factory.pr_behind(PR), calls

    def test_returns_behind_by_head_and_base(self):
        result, calls = self.behind((0, json.dumps({"headRefOid": OLD, "baseRefName": "main"})), (0, "1\n"))
        self.assertEqual(result, (1, OLD, "main"))
        self.assertEqual(calls, [("view", PR, "--json", "headRefOid,baseRefName"),
                                 ("api", f"repos/example/sample/compare/main...{OLD}", "--jq", ".behind_by")])

    def test_bad_output_exits_5(self):
        good = (0, json.dumps({"headRefOid": OLD, "baseRefName": "main"}))
        for view, compare in (((0, json.dumps({"headRefOid": "A1" * 20, "baseRefName": "main"})), (0, "1\n")),
                              ((0, json.dumps({"headRefOid": OLD, "baseRefName": "-main"})), (0, "1\n")),
                              ((0, "not json"), (0, "1\n")), (good, (0, "null\n")), (good, (0, "one\n")),
                              (good, (1, "")), (None, (0, "1\n"))):
            with self.subTest(view=view, compare=compare):
                with self.assertRaises(SystemExit) as raised, contextlib.redirect_stderr(io.StringIO()):
                    self.behind(view, compare)
                self.assertEqual(raised.exception.code, 5)


def rollup(*nodes, more=False):
    return 0, json.dumps({"statusCheckRollup": {"contexts": {"pageInfo": {"hasNextPage": more}, "nodes": list(nodes)}}})


def run(name, status="COMPLETED", conclusion="SUCCESS", started="2026-01-01T00:00:00Z"):
    return {"__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion, "startedAt": started}


def status(context, state, created="2026-01-01T00:00:00Z"):
    return {"__typename": "StatusContext", "context": context, "state": state, "createdAt": created}


class HeadChecksTest(IsolatedTestCase):
    def head(self, reply):
        calls = []

        def gh_cli(*args, stderr=False):
            calls.append(args)
            return reply

        with patch.object(factory, "gh_cli", gh_cli):
            return factory.head_checks(PR, NEW), calls

    def test_query_names_the_sha(self):
        result, calls = self.head(rollup(run("ci")))
        self.assertEqual(result, ([{"name": "ci", "bucket": "pass"}], None))
        self.assertEqual(calls, [("api", "graphql", "-f", factory.HEAD_CHECKS_QUERY, "-f", "o=example", "-f", "r=sample",
                                  "-f", f"sha={NEW}", "--jq", ".data.repository.object")])
        self.assertIn("object(oid:$sha)", factory.HEAD_CHECKS_QUERY)

    def test_buckets(self):
        result, _ = self.head(rollup(
            run("a", status="IN_PROGRESS", conclusion=None), run("b", status="QUEUED", conclusion=None),
            run("c"), run("d", conclusion="SKIPPED"), run("e", conclusion="NEUTRAL"), run("f", conclusion="CANCELLED"),
            run("g", conclusion="FAILURE"), run("h", conclusion="TIMED_OUT"), run("i", conclusion=None),
            run("j", conclusion="STALE"), status("k", "SUCCESS"), status("l", "PENDING"), status("m", "EXPECTED"),
            status("n", "FAILURE"), status("o", "ERROR"), run("p", status="QUEUED", conclusion=None, started=None)))
        self.assertEqual(result, ([{"name": n, "bucket": b} for n, b in (
            ("a", "pending"), ("b", "pending"), ("c", "pass"), ("d", "skipping"), ("e", "skipping"), ("f", "cancel"),
            ("g", "fail"), ("h", "fail"), ("i", "fail"), ("j", "fail"), ("k", "pass"), ("l", "pending"),
            ("m", "pending"), ("n", "fail"), ("o", "fail"), ("p", "pending"))], None))

    def test_repeated_name_keeps_latest(self):
        result, _ = self.head(rollup(
            run("ci", conclusion="FAILURE", started="2026-01-01T00:00:10Z"), run("ci", started="2026-01-01T00:00:05Z"),
            status("lint", "PENDING", "2026-01-01T00:00:01Z"), status("lint", "SUCCESS", "2026-01-01T00:00:02Z"),
            run("build", status="QUEUED", conclusion=None, started="2026-01-01T00:00:09Z"),
            run("build", started="2026-01-01T00:00:03Z")))
        self.assertEqual(result, ([{"name": "build", "bucket": "pending"}, {"name": "ci", "bucket": "fail"},
                                   {"name": "lint", "bucket": "pass"}], None))

    def test_unusable_replies(self):
        for reply, problem in ((None, "gh is not installed"), (("timeout", ""), "timed out after 60 seconds"),
                               ((1, ""), "exited 1 with no check list"), ((0, "not json"), "printed no check list"),
                               ((0, "null"), "printed no check list"), ((0, "[]"), "printed no check list"),
                               ((0, '{"statusCheckRollup": {"contexts": {"pageInfo": {"hasNextPage": false}, '
                                    '"nodes": {}}}}'), "printed no check list"),
                               (rollup(run("ci"), more=True), "listed more than 100 checks")):
            with self.subTest(reply):
                self.assertEqual(self.head(reply)[0], ([], problem))
        self.assertEqual(self.head((0, '{"statusCheckRollup": null}'))[0], ([], None))

    def test_malformed_list_counts_as_none_listed(self):
        for node in ("pass test", None, {"__typename": "CheckRun", "status": "COMPLETED", "startedAt": "x"},
                     {**run("ci"), "name": 7},
                     {"__typename": "StatusContext", "context": "ci", "state": "SUCCESS"},
                     {"__typename": "Other", "name": "ci"}):
            with self.subTest(node):
                self.assertEqual(self.head(rollup(run("lint"), node))[0], ([], "printed no check list"))
