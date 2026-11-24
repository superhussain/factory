"""Text checks on the factory skill's resume instructions, ported from tests/cli/test_factory.py.

A structural check on the instructions, not proof of model compliance.
"""
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
EXECUTION = ROOT / "skills" / "factory" / "references" / "execution.md"


class ResumeInstructionTest(unittest.TestCase):
    def setUp(self):
        self.text = EXECUTION.read_text(encoding="utf-8")

    def test_resume_approve_requires_a_fresh_approver(self):
        row = next(line for line in self.text.splitlines() if line.startswith("| approve |"))
        self.assertIn("fresh approver", row)
        self.assertIn("current HEAD and round", row)
        self.assertNotIn("Honor an existing decision", row)

    def test_resume_released_documents_strict_artifact_verification(self):
        # The paragraph wraps across lines, so compare with whitespace flattened.
        flat = " ".join(self.text.split())
        self.assertIn("may already be held", flat)
        self.assertIn("needs-human", flat)
        self.assertIn("Do not dispatch", flat)


if __name__ == "__main__":
    unittest.main()
