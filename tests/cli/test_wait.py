import tempfile
import threading
from pathlib import Path

from support import SKILL, IsolatedTestCase, invoke, load_runtime

factory = load_runtime()
JOB = "001-sample-job"


class WaitTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        self.folder = self.home / "jobs" / "sample" / JOB
        (self.folder / "round-1").mkdir(parents=True)

    def write(self, rel, text):
        (self.folder / rel).write_text(text, encoding="utf-8")

    def wait(self, *args):
        return invoke(factory, ["--home", str(self.home), "wait", f"sample/{JOB}", *args])

    def test_present_prints_first_lines_in_argument_order(self):
        self.write("spec.md", "# Spec: sample\n\nbody\n")
        self.write("build.md", "Routing: synthetic\n")
        self.write("round-1/review-ux.md", "VERDICT: CHANGES\n\n- fix it\n")
        self.write("round-1/review-code.md", "VERDICT: PASS\r\n")
        self.write("decision.md", "CLOSED: superseded by a later job\n")
        args = ["round-1/review-ux.md", "spec.md", "round-1/review-code.md", "build.md", "decision.md"]
        code, out, err = self.wait(*args)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out, "round-1/review-ux.md: VERDICT: CHANGES\nspec.md: # Spec: sample\n"
                              "round-1/review-code.md: VERDICT: PASS\nbuild.md: Routing: synthetic\n"
                              "decision.md: CLOSED: superseded by a later job\n")

    def test_missing_at_timeout_exits_3(self):
        self.write("round-1/review-ux.md", "not a verdict\n")
        code, out, err = self.wait("round-1/review-ux.md", "round-1/review-ui.md", "decision.md", "--timeout", "0")
        self.assertEqual((code, out), (3, ""))
        self.assertIn("round-1/review-ui.md", err)
        self.assertIn("decision.md", err)
        self.assertNotIn("review-ux", err)

    def test_malformed_at_timeout_exits_5(self):
        cases = {"round-1/review-security.md": "VERDICT: pass\n", "decision.md": "MERGE\n",
                 "spec.md": "\n# Spec\n", "build.md": ""}
        for rel, text in cases.items():
            with self.subTest(rel):
                self.write(rel, text)
                code, out, err = self.wait(rel, "--timeout", "0")
                self.assertEqual((code, out), (5, ""))
                self.assertIn(rel, err)

    def test_waits_for_a_file_that_lands_before_timeout(self):
        timer = threading.Timer(0.3, self.write, ("decision.md", "APPROVE\n"))
        timer.start()
        self.addCleanup(timer.cancel)
        code, out, _ = self.wait("decision.md", "--timeout", "5")
        self.assertEqual((code, out), (0, "decision.md: APPROVE\n"))

    def test_bad_usage_exits_2(self):
        for args in (["notes.md"], ["round-0/review-ux.md"], ["round-1/review-pm.md"], ["../spec.md"],
                     ["spec.md", "--timeout", "601"], ["spec.md", "--timeout", "-1"], ["spec.md", "--timeout"],
                     ["spec.md", "--timeout", "1", "--timeout", "1"], ["--timeout", "1"]):
            with self.subTest(args):
                code, out, _ = self.wait(*args)
                self.assertEqual((code, out), (2, ""))

    def test_linked_file_exits_6(self):
        real = self.home / "elsewhere.md"
        real.write_text("APPROVE\n")
        (self.folder / "decision.md").symlink_to(real)
        code, out, err = self.wait("decision.md", "--timeout", "0")
        self.assertEqual((code, out), (6, ""))
        self.assertIn("symlink or junction", err)

    def test_skill_uses_wait(self):
        skill = " ".join((SKILL / "SKILL.md").read_text(encoding="utf-8").split())
        ban = ("Never poll for job files with `until` or `while` loops, `sleep`, file mtimes, quiet windows, "
               "Monitor or a task's output file.")
        self.assertIn(ban, skill)
        self.assertIn('`python3 "$RUNTIME" wait "$SLUG/<id>" <artifact>...`', skill)
        self.assertIn("`wait` prints those first lines.", skill)
        rest = skill.replace(ban, "")
        for loop in ("until [", "stat -c", "quiet"):
            self.assertNotIn(loop, rest)
