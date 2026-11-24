"""Text checks for how skills/factory names the wait command."""
import unittest
from pathlib import Path

SKILL = Path(__file__).resolve().parent.parent.parent / "skills" / "factory"


class SkillUsesWaitTest(unittest.TestCase):
    def test_skill_uses_wait(self):
        skill = " ".join((SKILL / "SKILL.md").read_text(encoding="utf-8").split())
        ban = ("Never poll for job files with `until` or `while` loops, `sleep`, file mtimes, quiet windows, "
               "Monitor or a task's output file.")
        self.assertIn(ban, skill)
        self.assertIn('`"$RUNTIME" wait "$SLUG/<id>" <artifact>...` once, with the Bash tool timeout at '
                      '600000 ms.', skill)
        self.assertIn("`watch` and `wait` print those first lines.", skill)
        rest = skill.replace(ban, "")
        for loop in ("until [", "stat -c", "quiet"):
            self.assertNotIn(loop, rest)


if __name__ == "__main__":
    unittest.main()
