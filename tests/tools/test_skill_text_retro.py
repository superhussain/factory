"""The retro role's instructions name the aggregate fields and card shapes the runtime reads."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parent.parent.parent
ROLE = ROOT / "skills" / "factory" / "agents" / "factory-retro.md"


class RetroRoleTextTest(unittest.TestCase):
    def role(self):
        return ROLE.read_text(encoding="utf-8")

    def assert_phrases(self, *phrases):
        role = self.role()
        for phrase in phrases:
            with self.subTest(phrase=phrase):
                self.assertIn(phrase, role)

    def test_role_reads_red_flags(self):
        self.assert_phrases("`red_flags`", "`Red flag: <flag> via <tool>`", "never copy `target` into any string",
                            "denials, classifier friction and recurring red flags")

    def test_role_reads_repeated_findings(self):
        self.assert_phrases("`repeated-finding`", "review keeps flagging")

    def test_role_reads_lessons(self):
        self.assert_phrases("`lessons`", "Most rows teach nothing.", "Notes: lesson from <job>", "`lesson_cards`",
                            "A run holds at most 1 lesson")

    def test_role_reads_reviewers(self):
        self.assert_phrases("`reviewers`", "Noisy reviewer: <name> accepts <accept_pct>% of findings",
                            "`reviewer-rules`", "never an edit to the review stage notes")


if __name__ == "__main__":
    unittest.main()
