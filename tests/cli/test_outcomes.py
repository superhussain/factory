import json
import os
import re
import subprocess
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
RUNNING_EXAMPLE = """\
outcomes of jobs merged in the last 90 days
project  held  reworked  reverted  unknown
alpha       1         1         1        0
alpha held: 1017-add-x
alpha reworked: 1018-add-y
alpha reverted: 1019-add-z
"""
DAY = 86400
T0 = 1767600000  # 2026-01-05T08:00:00Z


def stamp(days_ago):
    return (datetime.now(timezone.utc) - timedelta(days=days_ago)).strftime("%Y-%m-%dT%H:%M:%SZ")


def merged(job_id, pr, repo="example/alpha", days_ago=1, **extra):
    return {"id": job_id, "project": "alpha", "stage": "merged", "done": stamp(days_ago),
            "updated": "2026-01-05T10:00:00Z", "pr": f"https://github.com/{repo}/pull/{pr}", **extra}


class OutcomesTestCase(IsolatedTestCase):
    """A bare origin and a clone holding the running example: PR 7 adds x (held), PR 8 adds y and
    gets a fix commit a day later (reworked), PR 9 adds z and is reverted (reverted)."""

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.home = self.root / "home"
        (self.home / "projects").mkdir(parents=True)
        (self.home / "projects/alpha.md").write_text("---\nrepo: example/alpha\nbase: main\n---\n", encoding="utf-8")
        self.clone = self.root / "alpha"
        (self.home / "paths.md").write_text(f"---\nalpha: {self.clone}\n---\n", encoding="utf-8")
        os.environ["FACTORY_HOME"] = str(self.home)
        url = patch.object(factory, "PROJECT_FETCH_URL", str(self.root / "origin.git"))
        url.start()
        self.addCleanup(url.stop)
        self.config("https://factory.example.test")
        subprocess.run(["git", "init", "-q", "--bare", str(self.root / "origin.git")], check=True)
        subprocess.run(["git", "-C", str(self.root / "origin.git"), "symbolic-ref", "HEAD", "refs/heads/main"],
                       check=True)
        subprocess.run(["git", "clone", "-q", str(self.root / "origin.git"), str(self.clone)], check=True,
                       capture_output=True)
        self.git("checkout", "-q", "-b", "main")
        self.commit("start", {"README": "alpha\n"}, T0 - DAY)
        self.oids = {7: self.merge(7, "x.txt", T0), 8: self.merge(8, "y.txt", T0 + 600),
                     9: self.merge(9, "z.txt", T0 + 1200)}
        self.fix = self.commit("fix: handle empty y", {"y.txt": "y fixed\n"}, T0 + DAY)
        self.git("revert", "--no-edit", "-m", "1", self.oids[9], date=T0 + 2 * DAY)
        self.revert = self.git("rev-parse", "HEAD")
        self.git("push", "-q", "origin", "main")
        self.board = {"updated": "2026-01-09T12:00:00Z", "jobs": [
            merged("1017-add-x", 7), merged("1018-add-y", 8), merged("1019-add-z", 9),
            {"id": "1021-fix-y", "project": "alpha", "stage": "pr-open", "updated": "2026-01-09T12:00:00Z",
             "refs": [{"kind": "rework-of", "id": "1018-add-y"}]},
            merged("1016-old", 6, days_ago=91)]}

    def config(self, origin):
        (self.home / "config.md").write_text(f"---\napi_url: {origin}\ndevice: host-a\n---\n", encoding="utf-8")

    def git(self, *args, date=None):
        env = dict(os.environ, GIT_AUTHOR_DATE=f"@{date} +0000", GIT_COMMITTER_DATE=f"@{date} +0000") \
            if date else None
        return subprocess.run(["git", "-C", str(self.clone), *args], check=True, capture_output=True, text=True,
                              env=env).stdout.strip()

    def commit(self, message, files, date):
        for name, text in files.items():
            (self.clone / name).write_text(text, encoding="utf-8")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message, date=date)
        return self.git("rev-parse", "HEAD")

    def merge(self, pr, name, date, push_base="main"):
        self.git("checkout", "-q", "-b", f"pr-{pr}", push_base)
        self.commit(f"add {name}", {name: name + "\n"}, date - 60)
        self.git("checkout", "-q", push_base)
        self.git("merge", "-q", "--no-ff", "-m", f"Merge pull request #{pr}", f"pr-{pr}", date=date)
        return self.git("rev-parse", "HEAD")

    def run_cmd(self, *args, board=None, oids=None):
        """Invoke outcomes against the board fixture, with GitHub answering the clone's merge commits."""
        found = (dict(self.oids) if oids is None else oids, None)
        with http_fixture({"GET /api/board": (200, board or self.board), "PUT /api/board": (200, b"ok")}) as server, \
                patch.object(factory, "pr_merge_commits", lambda *_: found):
            self.config(server.origin)
            result = invoke(factory, ["outcomes", *args])
        puts = [json.loads(body) for method, _, _, body in server.requests if method == "PUT"]
        return result, puts


class OutcomesOutputTest(OutcomesTestCase):
    def test_running_example(self):
        (code, out, err), puts = self.run_cmd()
        self.assertEqual((code, out, err, puts), (0, RUNNING_EXAMPLE, "", []))
        self.assertEqual(self.run_cmd("alpha")[0], (0, RUNNING_EXAMPLE, ""))

    def test_no_jobs(self):
        self.assertEqual(self.run_cmd("beta")[0], (0, "no jobs merged in the last 90 days\n", ""))


class OutcomesClassifyTest(OutcomesTestCase):
    def outcome(self, pr, rework=()):
        return factory.outcome_of(self.clone, "main", self.oids[pr], list(rework))

    def test_revert(self):
        self.assertEqual(self.outcome(9), ("reverted", [f"commit {self.revert[:12]}"]))

    def test_rework_ref(self):
        self.assertEqual(self.outcome(7, ["1022-redo-x"]), ("reworked", ["rework-of 1022-redo-x"]))

    def test_fix_within_seven_days(self):
        self.assertEqual(self.outcome(8, ["1021-fix-y"]),
                         ("reworked", [f"commit {self.fix[:12]}", "rework-of 1021-fix-y"]))

    def test_fix_on_other_files_holds(self):
        self.assertEqual(self.outcome(7), ("held", []))

    def test_fix_after_seven_days_holds(self):
        self.oids[10] = self.merge(10, "w.txt", T0 + 3 * DAY)
        self.commit("Fix w", {"w.txt": "w fixed\n"}, T0 + 10 * DAY + 1)
        self.git("push", "-q", "origin", "main")
        self.assertEqual(self.outcome(10), ("held", []))

    def test_fix_before_the_merge_holds(self):
        self.commit("fix x early", {"x.txt": "x fixed\n"}, T0 - 3600)
        self.git("push", "-q", "origin", "main")
        self.assertEqual(self.outcome(7), ("held", []))

    def test_riders_share_the_lead_result(self):
        self.board["jobs"].append(merged("1020-add-v", 8, refs=[{"kind": "batched-into", "id": "1018-add-y"}]))
        (code, out, _), _ = self.run_cmd()
        self.assertEqual(code, 0)
        self.assertIn("alpha reworked: 1018-add-y, 1020-add-v\n", out)


class OutcomesUnknownTest(OutcomesTestCase):
    def unknown(self, line, **kwargs):
        (code, out, err), puts = self.run_cmd("record", **kwargs)
        self.assertEqual((code, err), (0, ""))
        self.assertIn(line, out)
        self.assertIn("alpha       1         1         0        1\n", out)
        self.assertEqual(puts[0]["jobs"][2].get("outcome"), None)

    def test_no_checkout(self):
        (self.home / "paths.md").write_text("---\nbeta: /nowhere\n---\n", encoding="utf-8")
        (code, out, _), puts = self.run_cmd("record")
        self.assertEqual((code, puts), (0, []))
        self.assertIn("alpha unknown: 1017-add-x (no checkout of alpha in paths.md on host-a), 1018-add-y (no checkout",
                      out)
        self.assertIn("outcomes unchanged; nothing written\n", out)

    def test_foreign_pr(self):
        self.board["jobs"][2] = merged("1019-add-z", 9, repo="example/beta")
        self.unknown("alpha unknown: 1019-add-z (PR is not in example/alpha)\n")

    def test_gh_failure(self):
        self.board["jobs"] = self.board["jobs"][2:]
        with patch.object(factory, "gh_cli", lambda *_: None):
            found = factory.pr_merge_commits("example/alpha", [9], "host-a")
        self.assertEqual(found, (None, "gh is not installed on host-a"))
        with http_fixture({"GET /api/board": (200, self.board)}) as server, \
                patch.object(factory, "gh_cli", lambda *_: None):
            self.config(server.origin)
            code, out, err = invoke(factory, ["outcomes"])
        self.assertEqual((code, err), (0, ""))
        self.assertIn("alpha unknown: 1019-add-z (gh is not installed on host-a)\n", out)

    def test_no_merge_commit(self):
        self.unknown("alpha unknown: 1019-add-z (no merge commit from GitHub)\n", oids={**self.oids, 9: None})

    def test_bad_oid(self):
        self.board["jobs"] = self.board["jobs"][2:]
        answer = {"data": {"repository": {"p9": {"state": "MERGED", "mergeCommit": {"oid": "--output=x"}}}}}
        with http_fixture({"GET /api/board": (200, self.board)}) as server, \
                patch.object(factory, "gh_cli", lambda *_: (0, json.dumps(answer))):
            self.config(server.origin)
            code, out, err = invoke(factory, ["outcomes"])
        self.assertEqual((code, err), (0, ""))
        self.assertIn("alpha unknown: 1019-add-z (no merge commit from GitHub)\n", out)

    def test_oid_not_on_base(self):
        self.git("checkout", "-q", "-b", "local", "main")
        stray = self.merge(11, "s.txt", T0 + 5 * DAY, push_base="local")
        self.unknown(f"alpha unknown: 1019-add-z (merge commit {stray[:12]} is not on origin/main)\n",
                     oids={**self.oids, 9: stray})

    def test_merge_commits_query(self):
        calls = []

        def gh(*args):
            calls.append(args[-1])
            numbers = [int(n) for n in re.findall(r"pullRequest\(number: (\d+)\)", args[-1])]
            return 0, json.dumps({"data": {"repository": {
                f"p{n}": {"state": "MERGED" if n % 2 else "OPEN", "mergeCommit": {"oid": f"{n:040x}"}}
                for n in numbers if n != 51}}})
        with patch.object(factory, "gh_cli", gh):
            found, why = factory.pr_merge_commits("example/alpha", list(range(1, 52)), "host-a")
        self.assertEqual((len(calls), why), (2, None))
        self.assertEqual((found[1], found[2], found[51]), (f"{1:040x}", None, None))

    def checkout_unknown(self, *lines):
        (code, out, err), _ = self.run_cmd()
        self.assertEqual((code, err), (0, ""))
        for line in lines:
            self.assertIn(line, out)

    def test_missing_folder(self):
        gone = self.root / "gone"
        (self.home / "paths.md").write_text(f"---\nalpha: {gone}\n---\n", encoding="utf-8")
        self.checkout_unknown(f"alpha unknown: 1017-add-x (no folder at {gone} on host-a)")

    def test_not_a_checkout(self):
        plain = self.root / "plain"
        plain.mkdir()
        (self.home / "paths.md").write_text(f"---\nalpha: {plain}\n---\n", encoding="utf-8")
        self.checkout_unknown(f"({plain} is not a git checkout on host-a)")

    def test_fetch_failure(self):
        why = f"(could not fetch origin main in {self.clone} on host-a)"
        with patch.object(factory, "PROJECT_FETCH_URL", str(self.root / "gone.git")):
            self.checkout_unknown(*(f"{i} {why}" for i in ("1017-add-x", "1018-add-y", "1019-add-z")))

    def test_other_project_still_judged(self):
        (self.home / "projects/beta.md").write_text("---\nrepo: example/beta\nbase: main\n---\n", encoding="utf-8")
        gone = self.root / "gone"
        (self.home / "paths.md").write_text(f"---\nalpha: {self.clone}\nbeta: {gone}\n---\n", encoding="utf-8")
        self.board["jobs"].append(merged("1030-beta-job", 3, repo="example/beta", project="beta"))
        self.checkout_unknown("alpha held: 1017-add-x",
                              f"beta unknown: 1030-beta-job (no folder at {gone} on host-a)")

    def test_job_without_project(self):
        self.board["jobs"].append({"id": "1031-no-project", "stage": "merged", "done": stamp(1),
                                   "updated": "2026-01-05T10:00:00Z", "pr": "https://github.com/example/alpha/pull/7"})
        (code, out, err), _ = self.run_cmd()
        self.assertEqual((code, out, err), (0, RUNNING_EXAMPLE + "merged jobs with no project, skipped: 1031-no-project\n", ""))
        (code, _, _), puts = self.run_cmd("record")
        self.assertEqual(code, 0)
        saved = {j["id"]: j for j in puts[0]["jobs"]}
        self.assertNotIn("outcome", saved["1031-no-project"])


class OutcomesRecordTest(OutcomesTestCase):
    def test_record(self):
        (code, out, err), puts = self.run_cmd("record")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, RUNNING_EXAMPLE + "recorded 3 outcomes on the board\n")
        saved = {j["id"]: j for j in puts[0]["jobs"]}
        at = puts[0]["updated"]
        self.assertNotEqual(at, "2026-01-09T12:00:00Z")
        self.assertEqual(saved["1017-add-x"]["outcome"], {"result": "held", "at": at, "evidence": []})
        self.assertEqual(saved["1018-add-y"]["outcome"], {"result": "reworked", "at": at, "evidence": [
            f"commit {self.fix[:12]}", "rework-of 1021-fix-y"]})
        self.assertEqual(saved["1019-add-z"]["outcome"],
                         {"result": "reverted", "at": at, "evidence": [f"commit {self.revert[:12]}"]})
        self.assertNotIn("outcome", saved["1021-fix-y"])
        self.assertNotIn("outcome", saved["1016-old"])
        self.assertEqual({j["updated"] for j in puts[0]["jobs"]}, {"2026-01-05T10:00:00Z", "2026-01-09T12:00:00Z"})

        (code, out, _), again = self.run_cmd("record", board=puts[0])
        self.assertEqual((code, again), (0, []))
        self.assertEqual(out, RUNNING_EXAMPLE + "outcomes unchanged; nothing written\n")

        puts[0]["jobs"][0]["outcome"]["result"] = "reverted"
        (code, out, _), again = self.run_cmd("record", board=puts[0])
        self.assertEqual(out, RUNNING_EXAMPLE + "recorded 1 outcomes on the board\n")
        self.assertEqual(again[0]["jobs"][0]["outcome"]["result"], "held")
        self.assertEqual(again[0]["jobs"][1]["outcome"]["at"], at)


class OutcomesUsageTest(OutcomesTestCase):
    def test_bad_usage(self):
        for args in (["alpha", "beta"], ["record", "bad slug"], ["bad/slug"]):
            with self.subTest(args=args):
                code, out, err = invoke(factory, ["outcomes", *args])
                self.assertEqual((code, out, err), (2, "", "factory.py: usage: factory.py outcomes [record] "
                                                           "[<project-slug>]\n"))
