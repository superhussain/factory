//! Text checks on the skill's prompt command wording.
//! Ported from the deleted Python `tests/cli/test_prompt.py`.

use std::fs;

fn flat(rel: &str) -> String {
    let path = format!("{}/../../skills/factory/{rel}", env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn skill_uses_prompt() {
    let skill = flat("SKILL.md");
    for gone in [
        "end that stage's",
        "Prompts carry only",
        "same files, with the job folder",
    ] {
        assert!(!skill.contains(gone), "unexpected {gone:?}");
    }
    for phrase in [
        "`\"$RUNTIME\" prompt \"$SLUG/<id>\" <role> [--round N] [--port P] --out \"$FACTORY/prompts/$SLUG/<id>/<role>.txt\"`",
        "`Your full prompt is in <printed path>. Read it first and follow it exactly.`",
        "Roles are `spec`, `build`, `review-security`, `review-ux`, `review-ui`, `review-code` and `approve`. Only `build` and the four reviews take `--round`, and the reviews need it. Only `build` takes `--port`.",
        "`prompt` ends each prompt with `$PIPELINE.prompts.<stage>` verbatim.",
        "the absolute path in `$DOCUMENTS`",
    ] {
        assert!(skill.contains(phrase), "missing {phrase:?}");
    }
    let feature = flat("references/commands/feature.md");
    assert!(feature.contains("`project`, and `idea` (the feature text)"));
    for gone in [
        "with the job folder, round folder",
        "Spawn `factory-spec-writer` with the feature text",
    ] {
        assert!(!feature.contains(gone), "unexpected {gone:?}");
    }
}
