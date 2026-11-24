"""Text checks for how skills/factory names the watch command."""
import unittest
from pathlib import Path

SKILL = Path(__file__).resolve().parent.parent.parent / "skills" / "factory"


def squashed(*parts):
    return " ".join(SKILL.joinpath(*parts).read_text(encoding="utf-8").split())


class SkillUsesWatchTest(unittest.TestCase):
    def test_skill_uses_watch(self):
        skill = squashed("SKILL.md")
        self.assertIn('`"$RUNTIME" watch "$SLUG/<id>" <artifact>...` with the Bash tool timeout at 600000 ms', skill)
        self.assertIn('`"event": "timeout"`', skill)
        self.assertIn('"event": "malformed"', skill)
        self.assertIn("`watch` and `wait` print those first lines.", skill)
        babysit = squashed("references", "commands", "babysit.md")
        self.assertNotIn("--wait 540", babysit)
        self.assertIn('watch "$SLUG/<id>" --pr', babysit)
        self.assertIn("Run every watch with the Bash tool timeout at 600000 ms.", babysit)


if __name__ == "__main__":
    unittest.main()
