import json
import os
import subprocess
import tempfile
from pathlib import Path
from unittest.mock import patch

from support import IsolatedTestCase, http_fixture, invoke, load_runtime

factory = load_runtime()
JOB = "042-sample-export"
ROUND_1 = {"security": "PASS", "ux": "SKIPPED", "ui": "SKIPPED", "code": "CHANGES"}
CODE_REVIEW = """VERDICT: CHANGES

1. The export loop at `src/export.py:52` skips the last row.
2. `export.py:412` never closes the file.
3. Move the view into src/export_view.py:12.
4. The query in src/db.py:300 is unbounded.
5. Same loop again at src/export.py:52.
"""
SECURITY_REVIEW = "VERDICT: PASS\n\nThe escaping at src/export.py:60-64 is fine.\n"
SKIPPED_REVIEW = "VERDICT: SKIPPED\n\nfactory.py review-plan skipped this review: src/nope.py:1.\n"
UNVERIFIED = ["review-code.md export.py:412 bad-line", "review-code.md src/export_view.py:12 no-file",
              "review-code.md src/db.py:300 off-diff"]


def numbered(count, tag=""):
    return "".join(f"line {i}{tag}\n" for i in range(1, count + 1))


class GroundingTest(IsolatedTestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name).resolve()
        (self.home / "projects").mkdir()
        (self.home / "projects" / "sample.md").write_text("---\nrepo: example/sample\nbase: main\n---\n# Sample\n")
        self.folder = self.home / "jobs" / "sample" / JOB
        self.round = self.folder / "round-1"
        self.round.mkdir(parents=True)
        (self.folder / "spec.md").write_text("# Spec\n\nSample.\n\nMore.\n")
        self.out = self.round / "grounding.md"
        self.wt = self.home / "worktrees" / "sample" / JOB
        (self.wt / "src").mkdir(parents=True)
        self.git("init", "-q")
        self.origin = self.home / "origin.git"
        subprocess.run(["git", "init", "-q", "--bare", str(self.origin)], check=True)
        self.git("remote", "add", "origin", str(self.origin))
        (self.wt / "src" / "export.py").write_text(numbered(180))
        (self.wt / "src" / "db.py").write_text(numbered(400))
        self.commit()
        self.git("push", "-q", "origin", "HEAD:refs/heads/main")
        lines = numbered(180).splitlines(keepends=True)
        lines[39:75] = numbered(75, " changed").splitlines(keepends=True)[39:75]
        (self.wt / "src" / "export.py").write_text("".join(lines))
        self.commit()
        self.review("code", CODE_REVIEW)
        self.review("security", SECURITY_REVIEW)
        self.review("ux", SKIPPED_REVIEW)
        self.review("ui", SKIPPED_REVIEW)
        self.holds = []

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.wt), *args], check=True, capture_output=True,
                              text=True).stdout.strip()

    def commit(self):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "change")

    def review(self, reviewer, text):
        (self.round / f"review-{reviewer}.md").write_text(text, encoding="utf-8")

    def hold(self, target):
        self.holds.append(target)

    def grounding(self, round_text="1", rounds=(ROUND_1,), jobs=None):
        """Run grounding against a fixed board, keeping its PUT bodies in self.puts."""
        job = {"id": JOB, "project": "sample", "title": "Sample export", "stage": "review", "round": 1,
               "rounds": list(rounds)}
        with http_fixture({"/api/board": (200, {"updated": "", "jobs": [job] if jobs is None else jobs})}) as server:
            (self.home / "config.md").write_text(f"---\napi_url: {server.origin}\ndevice: sample-device\n---\n")
            with patch.object(factory, "hold", self.hold):
                result = invoke(factory, ["--home", str(self.home), "grounding", f"sample/{JOB}", round_text])
        self.puts = [json.loads(r[3]) for r in server.requests if r[0] == "PUT"]
        return result

    def result(self, **kwargs):
        code, out, err = self.grounding(**kwargs)
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(out.count("\n"), 1)
        return json.loads(out)

    def test_running_example(self):
        result = self.result()
        self.assertEqual(result, {"round": 1, "result": "unverified", "checked": 5, "diff": None, "unverified": [
            {"review": "review-code.md", "citation": "export.py:412", "status": "bad-line",
             "reason": "src/export.py has 180 lines"},
            {"review": "review-code.md", "citation": "src/export_view.py:12", "status": "no-file",
             "reason": "no file at src/export_view.py in the worktree or job folder"},
            {"review": "review-code.md", "citation": "src/db.py:300", "status": "off-diff",
             "reason": "src/db.py is not changed on this branch"}]})
        self.assertEqual(self.holds, [f"sample/{JOB}"])
        head = self.git("rev-parse", "HEAD")
        self.assertEqual(self.out.read_text(), f"""GROUNDING: UNVERIFIED 3

Head: {head}
Diff: origin/main
Checked: 5 citations in 2 review files

| Review | Citation | Status | Reason |
| --- | --- | --- | --- |
| review-code.md | `export.py:412` | bad-line | src/export.py has 180 lines |
| review-code.md | `src/export_view.py:12` | no-file | no file at src/export_view.py in the worktree or job folder |
| review-code.md | `src/db.py:300` | off-diff | src/db.py is not changed on this branch |

Unverified means the cited path or line did not check out. The finding may still be right. No verdict changed.
""")

    def test_board_entry(self):
        self.result()
        self.assertEqual([body["jobs"][0]["rounds"] for body in self.puts],
                         [[{**ROUND_1, "unverified": UNVERIFIED}]])

    def test_clean_rerun(self):
        self.review("code", "VERDICT: PASS\n\nThe loop at src/export.py:52 is fine now.\n")
        result = self.result(rounds=({**ROUND_1, "unverified": UNVERIFIED},))
        self.assertEqual((result["result"], result["checked"], result["unverified"]), ("clean", 2, []))
        self.assertEqual([body["jobs"][0]["rounds"] for body in self.puts], [[ROUND_1]])
        text = self.out.read_text()
        self.assertTrue(text.startswith("GROUNDING: CLEAN\n\nHead: "))
        self.assertIn("Checked: 2 citations in 2 review files\n\nUnverified means", text)
        self.assertNotIn("|", text)
        self.result()
        self.assertEqual(self.puts, [])

    def test_citation_forms(self):
        text = ("See ./src/a.py:3, src/b.py:4-9 and src/c.md:1,5,7 and ./src/a.py:3 again. "
                "Not https://example.com:443/a.py:3, localhost:8220, /abs/path.py:3, ../up.py:2 or a.py:x.")
        self.assertEqual(factory.find_citations(text), [
            ("./src/a.py:3", "src/a.py", [3], None), ("src/b.py:4-9", "src/b.py", [4, 9], (4, 9)),
            ("src/c.md:1,5,7", "src/c.md", [1, 5, 7], None)])

    def test_skipped_ignored(self):
        self.review("code", "VERDICT: SKIPPED\n\nsrc/nope.py:1\n")
        self.review("security", SKIPPED_REVIEW)
        result = self.result()
        self.assertEqual((result["result"], result["checked"]), ("clean", 0))
        self.assertIn("Checked: 0 citations in 0 review files\n", self.out.read_text())

    def test_resolution(self):
        (self.wt / "lib").mkdir()
        (self.wt / "lib" / "export.py").write_text(numbered(500))
        self.git("add", "-A")
        tracked = self.git("ls-files").splitlines()
        ground = factory.ground_citation
        self.assertEqual(ground("export.py", [412], self.wt, self.folder, tracked, None, False)[0], "ok")
        self.assertEqual(ground("export.py", [600], self.wt, self.folder, tracked, None, False),
                         ("bad-line", "lib/export.py, src/export.py", "2 files named export.py, none has line 600"))
        self.assertEqual(ground("spec.md", [3], self.wt, self.folder, tracked, {}, True),
                         ("ok", "job:spec.md", ""))
        self.assertEqual(ground("src/export.py", [80], self.wt, self.folder, tracked, {"src/export.py": [(40, 75)]},
                                True), ("ok", "src/export.py", ""))
        self.assertEqual(ground("src/export.py", [90], self.wt, self.folder, tracked, {"src/export.py": [(40, 75)]},
                                True)[2], "more than 10 lines from every change to src/export.py")
        self.assertEqual(ground("src/export.py", [20, 100], self.wt, self.folder, tracked,
                                {"src/export.py": [(40, 75)]}, True, (20, 100)), ("ok", "src/export.py", ""))
        self.assertEqual(ground("src/export.py", [20, 100], self.wt, self.folder, tracked,
                                {"src/export.py": [(40, 75)]}, True)[0], "off-diff")

    def test_unreadable_diff(self):
        self.git("remote", "remove", "origin")
        result = self.result()
        self.assertEqual(result["diff"], "could not fetch origin main")
        self.assertEqual([r["status"] for r in result["unverified"]], ["bad-line", "no-file"])
        text = self.out.read_text()
        self.assertIn("\nDiff: unreadable: could not fetch origin main\n", text)
        self.assertNotIn("off-diff", text)

    def test_refusals(self):
        for args in ("0", "x", "01"):
            with self.subTest(args):
                code, out, err = self.grounding(args)
                self.assertEqual((code, out, self.puts), (2, "", []))
                self.assertIn(factory.GROUNDING_USAGE, err)
        self.assertEqual(self.holds, [])
        for kwargs in ({"jobs": []}, {"rounds": ()}, {"round_text": "2"}):
            with self.subTest(kwargs):
                code, out, err = self.grounding(**kwargs)
                self.assertEqual((code, out, self.puts), (4, "", []))
        os.rename(self.wt, self.home / "moved")
        code, out, err = self.grounding()
        self.assertEqual((code, out, self.puts), (4, "", []))
        os.rename(self.home / "moved", self.wt)
        (self.home / "elsewhere.md").write_text("x\n")
        self.out.symlink_to(self.home / "elsewhere.md")
        code, out, err = self.grounding()
        self.assertEqual((code, out, self.puts), (6, "", []))
        self.assertEqual((self.home / "elsewhere.md").read_text(), "x\n")

    def test_arm_metrics_accepts_unverified(self):
        jobs = {JOB: {"project": "sample", "stage": "review", "rounds": [{**ROUND_1, "unverified": UNVERIFIED}]}}
        for name in ("code", "security", "ux", "ui"):
            (self.round / f"review-{name}.md").touch()
        self.assertEqual(factory.arm_metrics(jobs, [], self.home / "jobs")["coverage_unknown"], 0)
