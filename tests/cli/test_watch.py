import json
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import SKILL, IsolatedTestCase, http_fixture, invoke, load_runtime
from test_pr_state import checks, graph, view

factory = load_runtime()
JOB = "042-sample-export"
PR = "https://github.com/example/sample/pull/44"


class WatchTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        self.folder = self.home / "jobs" / "sample" / JOB
        self.folder.mkdir(parents=True)
        (self.home / "projects").mkdir()
        (self.home / "projects" / "sample.md").write_text("---\nrepo: example/sample\nbase: main\n---\n# Sample\n")
        self.now = 0.0
        self.sleeps = {}
        self.holds = []

    def write(self, rel, text):
        (self.folder / rel).write_text(text, encoding="utf-8")

    def sleep(self, seconds):
        self.now += seconds
        if self.now in self.sleeps:
            self.sleeps.pop(self.now)()

    def hold(self, target):
        self.holds.append((target, sorted(p.name for p in self.folder.iterdir()), self.now))

    def watch(self, *args, readings=((view(), checks("pass")),), pr=PR, hold=None, session=None, target=f"sample/{JOB}"):
        """Run watch with gh answering each (view, checks) reading in turn, repeating the last."""
        job = {"id": JOB, "project": "sample", "title": "Sample export", "stage": "pr-open"}
        if pr is not None:
            job["pr"] = pr
        self.gh_calls = []

        def gh(*gh_args, stderr=False):
            self.gh_calls.append(gh_args)
            if "/compare/" in gh_args[0]:
                return 0, "0\n"
            if gh_args[0].startswith("repos/"):
                return 0, "main\n"
            reading = readings[min(sum(c[0] == "view" for c in self.gh_calls) - 1, len(readings) - 1)]
            answer = {"view": reading[0], "checks": reading[1]}.get(gh_args[0], graph())
            return answer + ("",) if stderr else answer

        with http_fixture({"/api/board": (200, {"updated": "", "jobs": [job]})}) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")
            with patch.object(factory, "hold", hold or self.hold), patch.object(factory, "gh_pr", gh), \
                    patch.object(factory, "gh_cli", lambda *a: gh(*a[1:])), \
                    patch.object(factory.time, "sleep", self.sleep), \
                    patch.object(factory.time, "monotonic", lambda: self.now):
                tag = ["--session", session] if session else []
                return invoke(factory, ["--home", str(self.home), *tag, "watch", target, *args])

    def result(self, *args, **kwargs):
        code, out, err = self.watch(*args, **kwargs)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out.count("\n"), 1)
        return json.loads(out)

    def test_files_event(self):
        self.write("spec.md", "# Spec: sample export\n\nbody\n")
        self.write("build.md", "Routing: synthetic\n")
        self.assertEqual(self.result("spec.md", "build.md"),
                         {"event": "files", "files": {"spec.md": "# Spec: sample export", "build.md": "Routing: synthetic"},
                          "pr": None, "elapsed": 0})

    def test_spec_evidence_artifact(self):
        self.write("spec.md", "# Spec: sample export\n")
        self.write("spec-evidence.md", "# Spec evidence: sample export\n")
        self.assertEqual(self.result("spec.md", "spec-evidence.md"),
                         {"event": "files", "files": {"spec.md": "# Spec: sample export",
                                                      "spec-evidence.md": "# Spec evidence: sample export"},
                          "pr": None, "elapsed": 0})

    def test_file_lands_before_timeout(self):
        self.sleeps[212] = lambda: self.write("spec.md", "# Spec: sample export\n")
        self.assertEqual(self.result("spec.md"),
                         {"event": "files", "files": {"spec.md": "# Spec: sample export"}, "pr": None, "elapsed": 212})

    def test_timeout_exits_0(self):
        self.write("build.md", "")
        self.assertEqual(self.result("spec.md", "build.md", "--timeout", "5"),
                         {"event": "timeout", "files": {"spec.md": None, "build.md": ""}, "pr": None, "elapsed": 5})
        self.now = 0.0
        self.assertEqual(self.result("decision.md")["elapsed"], 540)

    def test_malformed_returns_early(self):
        self.write("decision.md", "Looks good\n")
        self.assertEqual(self.result("decision.md"),
                         {"event": "malformed", "files": {"decision.md": "Looks good"}, "pr": None, "elapsed": 1})

    def test_malformed_fixed_in_time(self):
        self.write("decision.md", "Looks good\n")
        self.sleeps[1] = lambda: self.write("decision.md", "APPROVE\n")
        self.assertEqual(self.result("decision.md"),
                         {"event": "files", "files": {"decision.md": "APPROVE"}, "pr": None, "elapsed": 1})

    def test_pr_event(self):
        readings = [(view(), checks("pending"))] * 2 + [(view(), checks("pass"))]
        reading = self.result("--pr", readings=readings)
        self.assertEqual((reading["event"], reading["files"], reading["elapsed"]), ("pr", {}, 60))
        self.assertEqual((reading["pr"]["pr"], reading["pr"]["next"]), (44, "merge"))
        self.assertEqual(sum(c[0] == "view" for c in self.gh_calls), 3)
        self.now = 0.0
        reading = self.result("--pr", "--timeout", "45", readings=[(view(), checks("pending"))])
        self.assertEqual((reading["event"], reading["pr"]["next"], reading["elapsed"]), ("timeout", "wait", 45))
        self.assertEqual(sum(c[0] == "view" for c in self.gh_calls), 2)

    def test_archive_pr_event(self):
        (self.home / "projects" / "sample.md").write_text(
            "---\nrepo: example/sample\narchive: example-upstream/sample\nbase: main\n---\n# Sample\n")
        reading = self.result("--pr", pr="https://github.com/example-upstream/sample/pull/47")
        self.assertEqual((reading["event"], reading["pr"]["pr"], reading["pr"]["next"]), ("pr", 47, "merge"))

    def test_files_win_over_pr(self):
        self.write("build.md", "Routing: synthetic\n")
        reading = self.result("build.md", "--pr")
        self.assertEqual((reading["event"], reading["pr"]["next"]), ("files", "merge"))

    def test_hold_once_first(self):
        self.sleeps[3] = lambda: self.write("spec.md", "# Spec: sample export\n")
        self.result("spec.md", session="a1b2c3")
        self.assertEqual(self.holds, [(f"sample/{JOB}", [], 0.0)])
        self.holds.clear()
        self.result("--pr", session="a1b2c3")
        self.assertEqual(len(self.holds), 1)

    def test_hold_exit_stops_watch(self):
        self.write("spec.md", "# Spec: sample export\n")

        def hold(target):
            raise SystemExit(6)

        code, out, _ = self.watch("spec.md", "--pr", hold=hold, session="a1b2c3")
        self.assertEqual((code, out, self.gh_calls), (6, "", []))

    def test_no_session_reads_only(self):
        self.write("spec.md", "# Spec: sample export\n")
        self.assertEqual(self.result("spec.md")["event"], "files")
        self.assertEqual(self.holds, [])

    def test_no_session_missing_job_exits_4(self):
        code, out, err = self.watch("spec.md", target="sample/043-missing")
        self.assertEqual((code, out), (4, ""))
        self.assertIn("no job 043-missing on the board. Nothing was written.", err)
        self.assertEqual(self.holds, [])

    def test_no_session_other_project_exits_2(self):
        (self.home / "projects" / "other.md").write_text("---\nrepo: example/other\nbase: main\n---\n# Other\n")
        code, out, err = self.watch("spec.md", target=f"other/{JOB}")
        self.assertEqual((code, out), (2, ""))
        self.assertIn(f"{JOB} belongs to sample, not other", err)
        self.assertEqual(self.holds, [])

    def test_bad_usage_exits_2(self):
        for args in (["--timeout", "0"], ["--timeout", "541"], ["--pr", "--pr"], ["notes.md"], ["--hold"],
                     ["spec.md", "--timeout"], ["spec.md", "--timeout", "1", "--timeout", "1"]):
            with self.subTest(args):
                code, out, err = self.watch(*args)
                self.assertEqual((code, out), (2, ""))
                self.assertIn(factory.WATCH_USAGE, err)
        self.assertEqual(self.holds, [])

    def test_bad_pr_exits_4(self):
        code, out, err = self.watch("--pr", pr=None)
        self.assertEqual((code, out), (4, ""))
        self.assertIn("has no pr", err)
        code, out, _ = self.watch("--pr", pr="https://github.com/other/repo/pull/1")
        self.assertEqual((code, out), (4, ""))

    def test_linked_file_exits_6(self):
        real = self.home / "elsewhere.md"
        real.write_text("# Spec: elsewhere\n")
        (self.folder / "spec.md").symlink_to(real)
        code, out, err = self.watch("spec.md")
        self.assertEqual((code, out), (6, ""))
        self.assertIn("symlink or junction", err)

    def test_skill_uses_watch(self):
        skill = " ".join((SKILL / "SKILL.md").read_text(encoding="utf-8").split())
        self.assertIn('`python3 "$RUNTIME" watch "$SLUG/<id>" <artifact>...` with the Bash tool timeout at 600000 ms',
                      skill)
        self.assertIn('`"event": "timeout"`', skill)
        self.assertIn('"event": "malformed"', skill)
        self.assertIn("`watch` and `wait` print those first lines.", skill)
        babysit = (SKILL / "references" / "commands" / "babysit.md").read_text(encoding="utf-8")
        self.assertNotIn("--wait 540", babysit)
        self.assertIn('watch "$SLUG/<id>" --pr', babysit)
        self.assertIn("Run every watch with the Bash tool timeout at 600000 ms.", babysit)
