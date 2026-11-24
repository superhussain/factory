use std::fs;

fn raw(path: &str) -> String {
    fs::read_to_string(format!(
        "{}/../../skills/factory/{path}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn read(path: &str) -> String {
    raw(path).split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn skill_uses_watch() {
    let skill = read("SKILL.md");
    assert!(skill.contains(
        "`\"$RUNTIME\" watch \"$SLUG/<id>\" <artifact>...` with the Bash tool timeout at 600000 ms"
    ));
    assert!(skill.contains("`\"event\": \"timeout\"`"));
    assert!(skill.contains("\"event\": \"malformed\""));
    assert!(skill.contains("`watch` and `wait` print those first lines."));
    let babysit = raw("references/commands/babysit.md");
    assert!(!babysit.contains("--wait 540"));
    assert!(babysit.contains("watch \"$SLUG/<id>\" --pr"));
    assert!(babysit.contains("Run every watch with the Bash tool timeout at 600000 ms."));
}
