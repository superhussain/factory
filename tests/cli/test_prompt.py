import os
import stat
import tempfile
from pathlib import Path

from support import SKILL, IsolatedTestCase, http_fixture, invoke, load_runtime, snapshot

factory = load_runtime()
JOB = "001-sample-job"
RIDER = "002-other-idea"
VIEW = {
    "project": "sample",
    "merge": "human",
    "review_rounds": 2,
    "reviewers": ["security", "ux", "ui", "code"],
    "gates": [],
    "stage_notes": {stage: [{"source": "pipeline:global@1", "text": f"Note for {stage}."}]
                    for stage in ("spec", "build", "review", "approve")},
    "versions": {"global": 1, "project": 0},
}


def envelope(stage):
    return (factory.PREFERENCES_SENTENCE + "\n\n"
            f'<operator-preferences source="pipeline:global@1" stage="{stage}" trust="data">\n'
            f"Note for {stage}.\n</operator-preferences>\n")


def board(*riders, idea="Add a sample feature."):
    jobs = [{"id": JOB, "project": "sample", "title": "Sample job", "idea": idea, "stage": "spec"}]
    for rider in riders:
        jobs.append(dict({"id": RIDER, "project": "sample", "title": "Other idea", "idea": "Do the other thing.",
                          "stage": "spec", "refs": [{"id": JOB, "kind": "batched-into"}]}, **rider))
    return {"updated": "", "jobs": jobs}


class PromptTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "projects").mkdir()
        self.project = self.home / "projects" / "sample.md"
        self.project.write_text("---\nrepo: example/sample\nbase: main\n---\n# Sample\n")
        self.documents = self.home / "projects" / "sample.documents.md"
        self.documents.write_text("This project has no notes.md, decisions.md or runbook.md.\n")
        self.folder = self.home / "jobs" / "sample" / JOB
        self.worktrees = self.home / "worktrees" / "sample"

    def prompt(self, *args, data=None, view=VIEW):
        routes = {"/api/board": (200, data or board()), "/api/pipeline/effective/sample": (200, view)}
        with http_fixture(routes) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\n---\n")
            return invoke(factory, ["--home", str(self.home), "prompt", f"sample/{JOB}", *args])

    def head(self, tree=JOB, base="main"):
        skill = SKILL.resolve()
        return (f"Job id: {JOB}\n"
                f"Job folder: {self.folder}\n"
                f"Worktree: {self.worktrees / tree} (branch factory/{tree}, base branch {base})\n"
                f"Project file: {self.project}\n"
                f"Project documents: {self.documents}\n"
                f"Visuals reference: {skill / 'references' / 'visuals.md'}\n"
                f"Walkthrough template: {skill / 'references' / 'walkthrough.html'}\n")

    def assertPrompt(self, args, expected, **kw):
        code, out, err = self.prompt(*args, **kw)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, expected)

    def test_spec(self):
        self.assertPrompt(["spec"], self.head() + "\nFeature text:\nAdd a sample feature.\n\n" + envelope("spec"))

    def test_spec_idea_images_and_title_fallback(self):
        self.folder.mkdir(parents=True)
        (self.folder / "idea-images.md").write_text("![idea](idea-image-0.png)\n")
        self.assertPrompt(["spec"], self.head() + f"Idea images: {self.folder / 'idea-images.md'}\n"
                          "\nFeature text:\nSample job\n\n" + envelope("spec"), data=board(idea=None))

    def test_build(self):
        self.assertPrompt(["build"], self.head() + f"Builder name: builder-{JOB}\n\n" + envelope("build"))
        round_folder = self.folder / "round-2"
        self.assertPrompt(["build", "--port", "8235", "--round", "2"],
                          self.head() + f"Builder name: builder-{JOB}\nDev-server port: 8235\n"
                          f"Round folder: {round_folder}\n\n" + envelope("build"))

    def test_review_roles(self):
        round_folder = self.folder / "round-1"
        for role in ("review-security", "review-ux", "review-ui", "review-code"):
            with self.subTest(role):
                self.assertPrompt([role, "--round", "1"], self.head() + f"Round folder: {round_folder}\n"
                                  f"Your file: {round_folder / (role + '.md')}\n\n" + envelope("review"))

    def test_approve(self):
        self.assertPrompt(["approve"], self.head() + f"Your file: {self.folder / 'decision.md'}\n\n"
                          + envelope("approve"))

    def test_no_stage_note_ends_after_role_lines(self):
        view = dict(VIEW, stage_notes={})
        self.assertPrompt(["approve"], self.head() + f"Your file: {self.folder / 'decision.md'}\n", view=view)

    def test_non_main_base(self):
        self.project.write_text("---\nrepo: example/sample\nbase: develop\n---\n")
        code, out, _ = self.prompt("approve")
        self.assertEqual(code, 0)
        self.assertIn(f"(branch factory/{JOB}, base branch develop)\n", out)

    def test_stacked_rider(self):
        data = board({"stacked": True})
        code, out, _ = self.prompt("build", data=data)
        self.assertEqual(code, 0)
        self.assertTrue(out.startswith(self.head()))
        self.assertIn(f"Rider worktree: {self.worktrees / RIDER} (branch factory/{RIDER})\n", out)
        for args in (["review-code", "--round", "1"], ["approve"]):
            with self.subTest(args[0]):
                code, out, _ = self.prompt(*args, data=data)
                self.assertEqual(code, 0)
                self.assertTrue(out.startswith(self.head(tree=RIDER)))

    def test_batched_riders(self):
        self.assertPrompt(["spec"], self.head() + "\nFeature text:\nAdd a sample feature.\n\n"
                          f"Batched ideas (spec each one):\n- {RIDER}: Do the other thing.\n\n" + envelope("spec"),
                          data=board({}))

    def test_bad_usage_exits_2_and_writes_nothing(self):
        out_path = str(self.home / "prompts" / "x.txt")
        cases = (["writer"], ["review-ux"], ["review-ux", "--round", "0"], ["spec", "--round", "1"],
                 ["approve", "--port", "8235"], ["build", "--port", "70000"], ["build", "--round"],
                 ["build", "--round", "1", "--round", "2"], ["build", "--color", "red"],
                 ["spec", "--out", "prompts/x.txt"], ["review-ux", "--port", "8235", "--round", "1"],
                 ["spec", "--out", out_path, "--port", "8235"])
        before = snapshot(self.home)
        for args in cases:
            with self.subTest(args):
                code, out, _ = self.prompt(*args)
                self.assertEqual((code, out), (2, ""))
        (self.home / "config.md").unlink()
        self.assertEqual(snapshot(self.home), before)

    def test_missing_job_documents_and_feature_exit_4(self):
        code, out, err = self.prompt("spec", data={"updated": "", "jobs": []})
        self.assertEqual((code, out), (4, ""))
        self.assertIn(f"no job {JOB} on the board", err)
        code, out, err = self.prompt("spec", data={"updated": "", "jobs": [{"id": JOB, "stage": "spec"}]})
        self.assertEqual((code, out), (4, ""))
        self.documents.unlink()
        code, out, err = self.prompt("spec")
        self.assertEqual((code, out), (4, ""))
        self.assertIn("run factory.py documents sample first", err)

    def test_out_writes_file(self):
        _, expected, _ = self.prompt("review-ux", "--round", "1")
        path = self.home / "prompts" / "sample" / JOB / "review-ux-round-1.txt"
        code, out, err = self.prompt("review-ux", "--round", "1", "--out", str(path))
        self.assertEqual((code, err, out), (0, "", f"{path}\n"))
        self.assertEqual(path.read_text(encoding="utf-8"), expected)
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertEqual(stat.S_IMODE(path.parent.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE((self.home / "prompts").stat().st_mode), 0o700)

    def test_out_refuses_job_folder(self):
        for path in (self.folder / "prompt.txt", self.home / "jobs", self.home / "prompts" / ".." / "jobs" / "x.txt"):
            with self.subTest(str(path)):
                code, out, err = self.prompt("spec", "--out", str(path))
                self.assertEqual((code, out), (2, ""))
                self.assertIn("--out must be an absolute path outside", err)
        self.assertFalse((self.home / "jobs").exists())

    def test_out_refuses_links(self):
        real = self.home / "elsewhere"
        real.mkdir()
        (self.home / "prompts").symlink_to(real, target_is_directory=True)
        code, out, err = self.prompt("spec", "--out", str(self.home / "prompts" / "spec.txt"))
        self.assertEqual((code, out), (6, ""))
        self.assertIn("symlink or junction", err)
        self.assertEqual(list(real.iterdir()), [])
