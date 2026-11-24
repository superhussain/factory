//! Text checks ported from tests/cli/test_factory.py. They read the skill's resume instructions.

fn execution() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../skills/factory/references/execution.md"
    ))
    .unwrap()
}

#[test]
fn resume_approve_requires_a_fresh_approver() {
    let text = execution();
    let row = text
        .lines()
        .find(|line| line.starts_with("| approve |"))
        .unwrap();
    assert!(row.contains("fresh approver"));
    assert!(row.contains("current HEAD and round"));
    assert!(!row.contains("Honor an existing decision"));
}

#[test]
fn resume_released_documents_strict_artifact_verification() {
    let text = execution();
    assert!(text.contains("may already be held"));
    assert!(text.contains("needs-human"));
    assert!(text.contains("Do not dispatch"));
}
