use std::fs;

fn skill() -> String {
    let text = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../skills/factory/SKILL.md"
    ))
    .unwrap();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn skill_uses_wait() {
    let skill = skill();
    let ban = "Never poll for job files with `until` or `while` loops, `sleep`, file mtimes, quiet windows, Monitor or a task's output file.";
    assert!(skill.contains(ban));
    assert!(skill.contains(
        "`\"$RUNTIME\" wait \"$SLUG/<id>\" <artifact>...` once, with the Bash tool timeout at 600000 ms."
    ));
    assert!(skill.contains("`watch` and `wait` print those first lines."));
    let rest = skill.replace(ban, "");
    for polling in ["until [", "stat -c", "quiet"] {
        assert!(!rest.contains(polling), "{polling}");
    }
}
