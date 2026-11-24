"""Text checks for how skills/factory names the checks command."""
import unittest
from pathlib import Path

SKILL = Path(__file__).resolve().parent.parent.parent / "skills" / "factory"


class SkillUsesChecksTest(unittest.TestCase):
    def test_skill_uses_checks(self):
        skill = " ".join((SKILL / "SKILL.md").read_text(encoding="utf-8").split())
        self.assertIn('Wait for checks with `"$RUNTIME" checks "$SLUG/<id>" --timeout 540`. Exit 0: '
                      "they passed, or the repo has none. It records that head as `checked_head`, and "
                      "`pr-merge` merges only that head. On a PR behind `$BASE`, it runs "
                      "`gh pr update-branch` and rechecks the new head. Exit 3: still pending; run it again. "
                      "Exit 1 names failing checks. Exit 4: no open PR. Exit 5: no usable gh reply. "
                      "Exit 6: `gh pr update-branch` failed; treat it as a merge conflict. "
                      "Exit 7 means the approver escalated the job: the board marks it escalated or its decision.md "
                      'says ESCALATE. Nothing merges. Patch {"automerge": false, "escalated": true, "needs": "review '
                      'and merge PR #<n>: the approver escalated it", "note": "escalated: checks held the merge"} '
                      "and stop. Exit 8: earned autonomy holds the class after a revert; checks already set "
                      "automerge false and needs. Stop. Never poll `gh pr checks` in a loop or with `--watch`.", skill)
        self.assertIn("Pass `--hold` to refresh this session's claim first, so a re-run during a long build needs "
                      "no separate `hold`.", skill)


if __name__ == "__main__":
    unittest.main()
