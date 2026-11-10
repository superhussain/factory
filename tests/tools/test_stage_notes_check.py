"""Tests for tools/stage-notes-check.py with synthetic pipeline views built from the committed notes."""
import json
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
TOOL = ROOT / "tools" / "stage-notes-check.py"
DOC = (ROOT / "docs" / "repo-security.md").read_text(encoding="utf8")


def doc_note(heading):
    return re.search(rf"^### {heading}\n\n```text\n(.*?)\n```$", DOC, re.M | re.S).group(1)


def envelope(source, stage, text):
    return f'<operator-preferences source="{source}" stage="{stage}" trust="data">\n{text}\n</operator-preferences>'


def view(review, build):
    prompts = {"review": envelope("pipeline:global@3", "review", "Global review text")}
    prompts["review"] += "\n\n" + envelope("pipeline:factory@3", "review", review)
    if build is not None:
        prompts["build"] = envelope("pipeline:factory@3", "build", build)
    return {"prompts": prompts}


def run(stdin, tool=TOOL):
    if isinstance(stdin, str):
        stdin = stdin.encode("utf8")
    result = subprocess.run([sys.executable, "-B", str(tool)], input=stdin, capture_output=True, check=False)
    result.stdout = result.stdout.decode("utf8")
    result.stderr = result.stderr.decode("utf8")
    return result


class StageNotesCheckTest(unittest.TestCase):
    review = doc_note("Review stage note")
    build = doc_note("Build stage note")

    def test_both_match_with_a_global_envelope_first(self):
        result = run(json.dumps(view(self.review, self.build)))
        self.assertEqual(result.stdout, "review: matches\nbuild: matches\n")
        self.assertEqual(result.returncode, 0)

    def test_edited_build_text_differs(self):
        result = run(json.dumps(view(self.review, self.build.replace("synthetic", "fake"))))
        self.assertEqual(
            result.stdout, "review: matches\nbuild: differs from docs/repo-security.md\n"
        )
        self.assertEqual(result.returncode, 1)

    def test_missing_build_prompt(self):
        result = run(json.dumps(view(self.review, None)))
        self.assertEqual(result.stdout, "review: matches\nbuild: missing from the live pipeline\n")
        self.assertEqual(result.returncode, 1)

    def test_non_json_input(self):
        result = run("not json")
        self.assertEqual(result.returncode, 2)
        self.assertIn("stdin is not a factory.py pipeline view", result.stderr)

    def test_json_list(self):
        result = run("[]")
        self.assertEqual(result.returncode, 2)
        self.assertIn("stdin is not a factory.py pipeline view", result.stderr)

    def test_prompts_not_object(self):
        result = run(json.dumps({"prompts": []}))
        self.assertEqual(result.returncode, 2)
        self.assertIn("stdin is not a factory.py pipeline view", result.stderr)

    def test_prompt_not_string(self):
        result = run(json.dumps({"prompts": {"review": 1}}))
        self.assertEqual(result.returncode, 2)
        self.assertIn("stdin is not a factory.py pipeline view", result.stderr)

    def test_invalid_utf8(self):
        result = run(b"\xff")
        self.assertEqual(result.returncode, 2)
        self.assertIn("stdin is not a factory.py pipeline view", result.stderr)

    def test_missing_doc_block(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "tools").mkdir()
            (root / "docs").mkdir()
            tool = root / "tools" / "stage-notes-check.py"
            shutil.copy(TOOL, tool)
            (root / "docs" / "repo-security.md").write_text(
                f"### Review stage note\n\n```text\n{self.review}\n```\n", encoding="utf8"
            )
            result = run(json.dumps(view(self.review, self.build)), tool)
        self.assertEqual(result.stdout, "review: matches\nbuild: no note in docs/repo-security.md\n")
        self.assertEqual(result.returncode, 1)


if __name__ == "__main__":
    unittest.main()
