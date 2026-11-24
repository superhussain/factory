"""Text checks for how skills/factory names the prompt command."""
import unittest
from pathlib import Path

SKILL = Path(__file__).resolve().parent.parent.parent / "skills" / "factory"


def squashed(*parts):
    return " ".join(SKILL.joinpath(*parts).read_text(encoding="utf-8").split())


class SkillUsesPromptTest(unittest.TestCase):
    def test_skill_uses_prompt(self):
        skill = squashed("SKILL.md")
        self.assertNotIn("end that stage's", skill)
        self.assertNotIn("Prompts carry only", skill)
        self.assertIn('`"$RUNTIME" prompt "$SLUG/<id>" <role> [--round N] [--port P] '
                      '--out "$FACTORY/prompts/$SLUG/<id>/<role>.txt"`', skill)
        self.assertIn("`Your full prompt is in <printed path>. Read it first and follow it exactly.`", skill)
        self.assertIn("Roles are `spec`, `build`, `review-security`, `review-ux`, `review-ui`, `review-code` and "
                      "`approve`. Only `build` and the four reviews take `--round`, and the reviews need it. "
                      "Only `build` takes `--port`.", skill)
        self.assertIn("`prompt` ends each prompt with `$PIPELINE.prompts.<stage>` verbatim.", skill)
        self.assertIn("the absolute path in `$DOCUMENTS`", skill)
        self.assertNotIn("same files, with the job folder", skill)
        feature = squashed("references", "commands", "feature.md")
        self.assertIn("`project`, and `idea` (the feature text)", feature)
        self.assertNotIn("with the job folder, round folder", feature)
        self.assertNotIn("Spawn `factory-spec-writer` with the feature text", feature)


if __name__ == "__main__":
    unittest.main()
