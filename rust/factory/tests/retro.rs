//! Text checks on the retro role card, `skills/factory/agents/factory-retro.md`.

fn role() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../skills/factory/agents/factory-retro.md"
    ))
    .unwrap()
}

fn assert_has(role: &str, phrases: &[&str]) {
    for phrase in phrases {
        assert!(role.contains(phrase), "role card lacks {phrase:?}");
    }
}

#[test]
fn role_reads_red_flags() {
    assert_has(
        &role(),
        &[
            "`red_flags`",
            "`Red flag: <flag> via <tool>`",
            "never copy `target` into any string",
            "denials, classifier friction and recurring red flags",
        ],
    );
}

#[test]
fn role_reads_repeated_findings() {
    assert_has(&role(), &["`repeated-finding`", "review keeps flagging"]);
}

#[test]
fn role_reads_lessons() {
    assert_has(
        &role(),
        &[
            "`lessons`",
            "Most rows teach nothing.",
            "Notes: lesson from <job>",
            "`lesson_cards`",
            "A run holds at most 1 lesson",
        ],
    );
}

#[test]
fn role_reads_reviewers() {
    assert_has(
        &role(),
        &[
            "`reviewers`",
            "Noisy reviewer: <name> accepts <accept_pct>% of findings",
            "`reviewer-rules`",
            "never an edit to the review stage notes",
        ],
    );
}
