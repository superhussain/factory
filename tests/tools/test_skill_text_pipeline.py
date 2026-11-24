"""Text checks for how skills/factory rechecks and patches the pipeline."""
import unittest
from pathlib import Path

SKILL = Path(__file__).resolve().parent.parent.parent / "skills" / "factory"


def squashed(*parts):
    return " ".join(SKILL.joinpath(*parts).read_text(encoding="utf-8").split())


class SkillUsesPipelineTest(unittest.TestCase):
    def test_skill_rechecks_the_pipeline_before_every_merge(self):
        text = squashed("SKILL.md")
        merge = text.split("- Merging an APPROVE PR:")[1].split(" - Jobs at `pr-open`")[0]
        self.assertIn('Immediately before every `pr-merge`, run `factory pipeline "$SLUG"` again', merge)
        self.assertIn("When `$PIPELINE.merge` is `human`, patch `automerge` false", merge)
        self.assertLess(merge.index("factory pipeline"), merge.index('pr-merge "$SLUG/<id>"'))
        recover = squashed("references", "commands", "recover.md")
        row = recover.split("| pr-open, `automerge` true |")[1].split(" |")[0]
        self.assertIn('re-running `factory pipeline "$SLUG"` right before `pr-merge`', row)
        self.assertIn("`merge` `human` holds the PR for a person with `automerge` false", row)

    def test_skill_pipes_the_board_patch(self):
        text = squashed("SKILL.md")
        self.assertIn("Patch the versions onto the job with `set -o pipefail; <this skill's dir>/runtime/factory "
                      'pipeline "$SLUG" --board | "$RUNTIME" board "$FACTORY" <id>`.', text)
        self.assertIn("Read `$PIPELINE` fields from the printed JSON, and never echo it through the shell "
                      "into another parser.", text)


if __name__ == "__main__":
    unittest.main()
