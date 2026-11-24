"""Ported from tests/cli/test_documents.py: prompt and doc text that describes project documents."""
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
SKILL = ROOT / "skills/factory"
ROLES = ("approver", "builder", "review-code", "review-security", "review-ui", "review-ux", "spec-writer")


class DocumentsTextTest(unittest.TestCase):
    def test_dispatched_roles_read_project_documents(self):
        for role in ROLES:
            text = (SKILL / f"agents/factory-{role}.md").read_text(encoding="utf-8")
            self.assertIn("### Project documents\n\nYour prompt may also give the absolute path of a project "
                          "documents file.", text, role)
            self.assertLess(text.index("### Project documents"), text.index("### Operator preferences"), role)
        self.assertNotIn("Project documents", (SKILL / "agents/factory-explorer.md").read_text(encoding="utf-8"))

    def test_skill_and_trust_describe_documents(self):
        skill = (SKILL / "SKILL.md").read_text(encoding="utf-8")
        self.assertIn("the absolute path in `$DOCUMENTS`", skill)
        self.assertIn('runtime/factory documents "$SLUG"', skill)
        self.assertNotIn("automatic agent consumption", skill)
        self.assertIn("document:<slug>/<name>", (ROOT / "docs/trust.md").read_text(encoding="utf-8"))
        settings = (SKILL / "references/project-settings.md").read_text(encoding="utf-8")
        flat = " ".join((skill + " " + settings).split())
        self.assertNotIn("cache/sync", flat)
        self.assertIn("`factory documents <slug>` writes the non-empty documents to "
                      "`projects/<slug>.documents.md`", flat)
        self.assertIn("That copy is read-only: edit documents in the dashboard; the CLI never uploads them.", flat)


if __name__ == "__main__":
    unittest.main()
