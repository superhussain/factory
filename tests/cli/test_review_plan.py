import json
import shutil
import subprocess
import tempfile
from pathlib import Path

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
JOB = "001-sample-job"
ALL = ["security", "ux", "ui", "code"]
SPEC = "# Spec: sample\n\n## Acceptance criteria\n\n| # | Criterion |\n| 1 | The docs read well |\n"
LOW = SPEC + "\n## Review tier\n\nTier: low\n\n- Docs only.\n"
ROUTES = ("---\nprimary: provider-a model-big medium\nreview-low: provider-a model-small low\n"
          "review-medium: provider-a model-mid medium\n---\n")
SMALL = {"name": "review-low", "provider": "provider-a", "model": "model-small", "effort": "low"}
MID = {"name": "review-medium", "provider": "provider-a", "model": "model-mid", "effort": "medium"}
NULL = {"security": None, "code": None}


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
        origin = self.home / "origin.git"
        subprocess.run(["git", "init", "-q", "--bare", str(origin)], check=True)
        self.git("remote", "add", "origin", str(origin))
        self.git("push", "-q", "origin", "HEAD:refs/heads/main")
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
        plan = self.assertRunsAll()
        self.assertIn("could not be read", plan["reason"])
        self.assertIsNone(plan["protected"])

    def test_protected_paths_reported(self):
        self.worktree("skills/factory/runtime/factory.py", ".github/workflows/ci.yml", "docs/trust.md")
        self.assertEqual(self.assertSkipped()["protected"], [".github/workflows/ci.yml"])
        self.assertEqual(json.loads((self.folder / "round-1" / "review-plan.json").read_text()),
                         {"round": 1, "head": self.head(), "protected": [".github/workflows/ci.yml"],
                          "tier": "high", "routes": NULL})

    def test_repo_check_config_is_protected(self):
        self.assertEqual(factory.touched_protected([
            "tools/assets.mjs", "package.json", "paseo-plugin/package.json", ".gitleaks.toml", "mise.toml",
            ".oxfmtrc.json", "nx.json", "tools/verify.py", "tests/cli/support.py",
            "tests/support/browser-server.mjs", "docs/trust.md", "pnpm-lock.yaml"]),
            [".gitleaks.toml", ".oxfmtrc.json", "mise.toml", "nx.json", "package.json", "paseo-plugin/package.json",
             "tests/cli/support.py", "tests/support/browser-server.mjs", "tools/verify.py"])

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

    def head(self):
        return subprocess.run(["git", "-C", str(self.wt), "rev-parse", "HEAD"], check=True, capture_output=True,
                              text=True).stdout.strip()

    def fix_round(self, ux="VERDICT: PASS\n"):
        """Round 1 of a dashboard change with ux and ui verdicts, then a test-only fix commit."""
        self.worktree("web/dashboard.html")
        self.assertRunsAll("1")
        recorded = json.loads((self.folder / "round-1" / "review-plan.json").read_text())
        self.assertEqual(recorded, {"round": 1, "head": self.head(), "protected": [], "tier": "high",
                                    "routes": dict.fromkeys(ALL)})
        (self.folder / "round-1" / "review-ux.md").write_text(ux)
        (self.folder / "round-1" / "review-ui.md").write_text("VERDICT: PASS\n")

    def test_fix_round_reads_its_own_diff(self):
        self.fix_round()
        self.commit("tests/cli/test_export.py")
        self.assertEqual(self.assertSkipped("2")["reason"],
                         "none of the 1 files changed since round 1 matches a visible-file glob, "
                         "and round 1 ux and ui did not ask for changes")
        self.assertEqual(json.loads((self.folder / "round-2" / "review-plan.json").read_text()),
                         {"round": 2, "head": self.head(), "protected": [], "tier": "high", "routes": NULL})

    def test_fix_round_after_ux_changes_runs_all(self):
        self.fix_round(ux="VERDICT: CHANGES\n")
        self.commit("tests/cli/test_export.py")
        self.assertEqual(self.assertRunsAll("2")["reason"], "web/dashboard.html matches *.html")

    def test_fix_round_without_plan_file_runs_all(self):
        self.fix_round()
        (self.folder / "round-1" / "review-plan.json").unlink()
        self.commit("tests/cli/test_export.py")
        self.assertEqual(self.assertRunsAll("2")["reason"], "web/dashboard.html matches *.html")

    def test_fix_round_after_rewrite_runs_all(self):
        self.fix_round()
        self.git("commit", "-q", "--amend", "-m", "rewritten")
        self.commit("tests/cli/test_export.py")
        self.assertEqual(self.assertRunsAll("2")["reason"], "web/dashboard.html matches *.html")

    def test_fix_round_visible_change_decision_runs_all(self):
        self.fix_round()
        (self.folder / "decision.md").write_text("CHANGES\n\n## Visible change\n\nThe CLI prints new copy.\n")
        self.commit("tests/cli/test_export.py")
        self.assertEqual(self.assertRunsAll("2")["reason"], "decision.md names a visible change")

    def test_low_tier_routes_reviewers(self):
        (self.folder / "spec.md").write_text(LOW)
        (self.home / "routes.md").write_text(ROUTES)
        self.worktree("docs/trust.md")
        plan = self.assertSkipped()
        self.assertEqual((plan["tier"], plan["tier_reason"]), ("low", "spec.md rates the job low"))
        self.assertEqual(plan["routes"], {"security": MID, "code": SMALL})
        self.assertEqual(json.loads((self.folder / "round-1" / "review-plan.json").read_text()),
                         {"round": 1, "head": self.head(), "protected": [], "tier": "low",
                          "routes": {"security": MID, "code": SMALL}})
        (self.folder / "spec.md").write_text(LOW.replace("Tier: low", "tier: MEDIUM"))
        plan = self.assertSkipped("2")
        self.assertEqual((plan["tier"], plan["routes"]), ("medium", {"security": MID, "code": MID}))

    def test_floors_force_high(self):
        (self.home / "routes.md").write_text(ROUTES)
        cases = [("decision.md asks for a full review", LOW, "CHANGES\n\n## Full review\n\nAuth changed.\n",
                  ("docs/trust.md",)),
                 ("the protected paths could not be read", LOW, None, ("docs/trust.md",)),
                 ("the branch touches protected paths", LOW, None, (".github/workflows/ci.yml",)),
                 ("spec.md has no Review tier line", SPEC + "\nTier: low\n", None, ("docs/trust.md",))]
        for reason, spec, decision, files in cases:
            with self.subTest(reason):
                shutil.rmtree(self.wt, ignore_errors=True)
                shutil.rmtree(self.home / "origin.git", ignore_errors=True)
                shutil.rmtree(self.folder / "round-1", ignore_errors=True)
                (self.folder / "spec.md").write_text(spec)
                if decision:
                    (self.folder / "decision.md").write_text(decision)
                elif (self.folder / "decision.md").exists():
                    (self.folder / "decision.md").unlink()
                self.worktree(*files)
                if reason == "the protected paths could not be read":
                    self.git("remote", "remove", "origin")
                code, out, err = self.plan()
                self.assertEqual((code, err), (0, ""))
                plan = json.loads(out)
                self.assertEqual((plan["tier"], plan["tier_reason"]), ("high", reason))
                self.assertEqual(plan["routes"], dict.fromkeys(plan["run"]))
                recorded = json.loads((self.folder / "round-1" / "review-plan.json").read_text())
                self.assertEqual((recorded["tier"], recorded["routes"]), ("high", dict.fromkeys(plan["run"])))

    def test_no_routes_file_gives_null_routes(self):
        (self.folder / "spec.md").write_text(LOW)
        self.worktree("docs/trust.md")
        plan = self.assertSkipped()
        self.assertEqual((plan["tier"], plan["routes"]), ("low", NULL))

    def test_bad_review_route_exits_2(self):
        (self.folder / "spec.md").write_text(LOW)
        (self.home / "routes.md").write_text(ROUTES.replace("model-small low", "model-small fast"))
        self.worktree("docs/trust.md")
        code, out, err = self.plan()
        self.assertIn("bad review route review-low", err)
        self.assertNothingWritten("1", code, out, 2)
        self.assertFalse((self.folder / "round-1").exists())
