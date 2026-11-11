import json
import subprocess
import tempfile
from pathlib import Path

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
JOB = "001-sample-job"
ALL = ["security", "ux", "ui", "code"]
SPEC = "# Spec: sample\n\n## Acceptance criteria\n\n| # | Criterion |\n| 1 | The docs read well |\n"


def board():
    return {"updated": "", "jobs": [{"id": JOB, "project": "sample", "title": "Sample job", "stage": "review"}]}


class ReviewPlanTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "projects").mkdir()
        self.project = self.home / "projects" / "sample.md"
        self.project.write_text("---\nrepo: example/sample\nbase: main\n---\n# Sample\n")
        self.folder = self.home / "jobs" / "sample" / JOB
        self.folder.mkdir(parents=True)
        (self.folder / "spec.md").write_text(SPEC)
        self.wt = self.home / "worktrees" / "sample" / JOB

    def git(self, *args):
        subprocess.run(["git", "-C", str(self.wt), *args], check=True, capture_output=True)

    def worktree(self, *files):
        self.wt.mkdir(parents=True)
        self.git("init", "-q")
        self.git("config", "user.name", "Test")
        self.git("config", "user.email", "test@example.invalid")
        (self.wt / "README.md").write_text("base\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "base")
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.commit(*files)

    def commit(self, *files):
        for rel in files:
            (self.wt / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.wt / rel).write_text("change\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "change")

    def plan(self, round_text="1"):
        routes = {"/api/board": (200, board())}
        with http_fixture(routes) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")
            return invoke(factory, ["--home", str(self.home), "review-plan", f"sample/{JOB}", round_text])

    def assertSkipped(self, round_text="1"):
        code, out, err = self.plan(round_text)
        self.assertEqual((code, err), (0, ""))
        plan = json.loads(out)
        self.assertEqual((plan["round"], plan["run"], plan["skipped"]),
                         (int(round_text), ["security", "code"], ["ux", "ui"]))
        for name in ("ux", "ui"):
            text = (self.folder / f"round-{round_text}" / f"review-{name}.md").read_text()
            self.assertTrue(text.startswith("VERDICT: SKIPPED\n\nfactory.py review-plan skipped this review: "))
        return plan

    def assertRunsAll(self, round_text="1"):
        code, out, err = self.plan(round_text)
        self.assertEqual((code, err), (0, ""))
        plan = json.loads(out)
        self.assertEqual((plan["round"], plan["run"], plan["skipped"]), (int(round_text), ALL, []))
        for name in ("ux", "ui"):
            self.assertFalse((self.folder / f"round-{round_text}" / f"review-{name}.md").exists())
        return plan

    def test_docs_only_skips(self):
        self.worktree("docs/trust.md")
        self.assertIn("none of the 1 changed files", self.assertSkipped()["reason"])

    def test_runtime_only_skips(self):
        self.worktree("skills/factory/runtime/x.py")
        self.assertSkipped()

    def test_dashboard_runs_all(self):
        self.worktree("web/dashboard.html")
        self.assertEqual(self.assertRunsAll()["reason"], "web/dashboard.html matches *.html")

    def test_visual_criterion_runs_all(self):
        (self.folder / "spec.md").write_text(SPEC + "| 2 | Capture before and after screenshots |\n")
        self.worktree("docs/trust.md")
        self.assertEqual(self.assertRunsAll()["reason"], "spec.md has a visual-evidence acceptance criterion")

    def test_project_ui_glob_runs_all(self):
        self.project.write_text(self.project.read_text() + "\n## UI\n\nScreens under `docs/screens/*` count.\n")
        self.worktree("docs/screens/a.md")
        self.assertEqual(self.assertRunsAll()["reason"], "docs/screens/a.md matches docs/screens/*")

    def test_visible_change_decision_runs_all(self):
        (self.folder / "decision.md").write_text("CHANGES\n\n## Visible change\n\nThe CLI prints new copy.\n")
        self.worktree("docs/trust.md")
        self.assertEqual(self.assertRunsAll()["reason"], "decision.md names a visible change")

    def test_missing_worktree_runs_all(self):
        self.assertIn("could not be read", self.assertRunsAll()["reason"])

    def test_later_round_adds_visible_file(self):
        self.worktree("docs/trust.md")
        self.assertSkipped("1")
        self.commit("web/a.css")
        self.assertEqual(self.assertRunsAll("2")["reason"], "web/a.css matches *.css")

    def test_existing_file_exits_6(self):
        self.worktree("docs/trust.md")
        (self.folder / "round-1").mkdir()
        (self.folder / "round-1" / "review-ui.md").write_text("VERDICT: PASS\n")
        code, out, err = self.plan()
        self.assertEqual((code, out), (6, ""))
        self.assertIn("already exists; nothing was written", err)
        self.assertFalse((self.folder / "round-1" / "review-ux.md").exists())

    def assertNothingWritten(self, round_text, code, out, expected):
        self.assertEqual((code, out), (expected, ""))
        for name in ("ux", "ui"):
            self.assertFalse((self.folder / f"round-{round_text}" / f"review-{name}.md").exists())

    def test_bad_round_exits_2(self):
        self.worktree("docs/trust.md")
        for round_text in ("0", "x", "-1"):
            with self.subTest(round_text):
                code, out, _ = self.plan(round_text)
                self.assertNothingWritten(round_text, code, out, 2)

    def test_linked_round_folder_exits_6(self):
        self.worktree("docs/trust.md")
        real = self.home / "elsewhere"
        real.mkdir()
        (self.folder / "round-1").symlink_to(real, target_is_directory=True)
        code, out, err = self.plan()
        self.assertIn("symlink or junction", err)
        self.assertNothingWritten("1", code, out, 6)
        self.assertEqual(list(real.iterdir()), [])

    def test_missing_spec_exits_4(self):
        self.worktree("docs/trust.md")
        (self.folder / "spec.md").unlink()
        code, out, err = self.plan()
        self.assertIn("spec.md is missing", err)
        self.assertNothingWritten("1", code, out, 4)
