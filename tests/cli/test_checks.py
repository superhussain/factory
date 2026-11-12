import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import SKILL, IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
JOB = "042-sample-export"
PR = "https://github.com/example/sample/pull/42"
OPEN = (0, "OPEN\n")
WARN = "factory.py: warning: gh pr checks gave no usable reply ({})\n"


def listed(*checks):
    return 8, json.dumps([{"bucket": bucket, "name": name} for bucket, name in checks])


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

    def checks(self, view, polls, *args, pr=PR):
        """Run checks with gh answering view first, then each poll in turn, repeating the last."""
        job = {"id": JOB, "project": "sample", "title": "Sample export", "stage": "pr-open"}
        if pr is not None:
            job["pr"] = pr
        calls = []

        def gh(*gh_args, stderr=False):
            calls.append(gh_args)
            if gh_args[0] == "view":
                return view
            poll = polls[min(len(calls) - 2, len(polls) - 1)]
            return poll + ("",) if stderr and poll and len(poll) == 2 else poll

        with http_fixture({"/api/board": (200, {"updated": "", "jobs": [job]})}) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")
            with patch.object(factory, "gh_pr", gh), patch.object(factory.time, "sleep", self.sleep), \
                    patch.object(factory.time, "monotonic", lambda: self.now):
                result = invoke(factory, ["--home", str(self.home), "checks", f"sample/{JOB}", *args])
        return result + (calls,)

    def test_all_pass_prints_each_check_and_exits_0(self):
        code, out, err, calls = self.checks(OPEN, [(1, ""), listed(("pending", "test"), ("pass", "lint")),
                                                   (0, json.dumps([{"bucket": "pass", "name": "test"},
                                                                   {"bucket": "pass", "name": "lint"}]))])
        self.assertEqual((code, out, err), (0, "pass test\npass lint\n", WARN.format("exited 1 with no check list")))
        self.assertEqual(calls[0][:2], ("view", PR))
        self.assertEqual(calls[1], ("checks", PR, "--json", "name,bucket"))
        self.assertEqual(self.now, 20)

    def test_skipping_counts_as_passing(self):
        code, out, err, _ = self.checks(OPEN, [listed(("pass", "test"), ("skipping", "lint"))])
        self.assertEqual((code, out, err), (0, "pass test\nskipping lint\n", ""))

    def test_no_checks_after_120_seconds_exits_0(self):
        for poll in ((0, "[]"), (1, "", "no checks reported on the 'factory/042-sample-export' branch\n")):
            with self.subTest(poll):
                self.now = 0.0
                code, out, err, _ = self.checks(OPEN, [poll], "--timeout", "540")
                self.assertEqual((code, out, err), (0, "no checks reported\n", ""))
                self.assertEqual(self.now, 120)

    def test_no_usable_reply_by_120_seconds_exits_5(self):
        for poll, problem in (((1, ""), "exited 1 with no check list"), (("timeout", ""), "timed out after 60 seconds"),
                              ((0, "not json"), "printed no check list"), ((0, "{}"), "printed no check list"),
                              ((1, "", "HTTP 502\n"), "exited 1 with no check list"),
                              (None, "gh is not installed")):
            with self.subTest(poll):
                self.now = 0.0
                code, out, err, _ = self.checks(OPEN, [poll], "--timeout", "540")
                self.assertEqual((code, out), (5, ""))
                self.assertEqual(err, WARN.format(problem) * 13 + "factory.py: gh pr checks never gave a usable reply\n")
                self.assertEqual(self.now, 120)

    def test_one_usable_reply_keeps_exit_0(self):
        code, out, err, _ = self.checks(OPEN, [(1, ""), (0, "[]"), (1, "")], "--timeout", "540")
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

    def test_malformed_list_counts_as_none_listed(self):
        for body in ('["pass test"]', '[{"name": "test"}]', '[{"bucket": "pass", "name": 7}]',
                     '[{"bucket": "pass", "name": "lint"}, null]'):
            with self.subTest(body):
                self.now = 0.0
                code, out, err, _ = self.checks(OPEN, [(0, body)], "--timeout", "30")
                self.assertEqual((code, out, err), (5, "", WARN.format("printed no check list") * 4
                                                    + "factory.py: gh pr checks never gave a usable reply\n"))

    def test_none_listed_at_deadline_before_120_seconds_exits_3(self):
        code, out, err, _ = self.checks(OPEN, [(0, "[]")], "--timeout", "30")
        self.assertEqual((code, out, err), (3, "", "factory.py: no checks listed yet\n"))
        self.assertEqual(self.now, 30)
        code, out, err, _ = self.checks(OPEN, [(1, "")], "--timeout", "30")
        self.assertEqual((code, out, err), (5, "", WARN.format("exited 1 with no check list") * 4
                                            + "factory.py: gh pr checks never gave a usable reply\n"))

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

    def test_bad_timeout_exits_2(self):
        for args in (["--timeout", "601"], ["--timeout", "-1"], ["--timeout"], ["--timeout", "1", "--timeout", "1"],
                     ["--wait", "1"]):
            with self.subTest(args):
                code, out, _, calls = self.checks(OPEN, [listed(("pass", "test"))], *args)
                self.assertEqual((code, out, calls), (2, "", []))

    def test_skill_uses_checks(self):
        skill = " ".join((SKILL / "SKILL.md").read_text(encoding="utf-8").split())
        self.assertIn('Wait for checks with `python3 "$RUNTIME" checks "$SLUG/<id>" --timeout 540`. Exit 0 means '
                      "they passed, or the repo has none. Exit 3 means they are still pending, so run it again. "
                      "Exit 1 names the failing checks. Exit 4 means no open PR; 5 means gh never answered. Never poll `gh pr checks` in a loop or with `--watch`.", skill)
        self.assertIn("Pass `--hold` to refresh this session's claim first, so a re-run during a long build needs "
                      "no separate `hold`.", skill)
