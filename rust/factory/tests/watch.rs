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
        "start `\"$RUNTIME\" watch \"$SLUG/<id>\" <artifact>... --timeout 7080` once, with the Bash tool's `run_in_background` and its timeout at 7200000 ms"
    ));
    assert!(skill.contains("Run one watch per stage, never a second while one runs."));
    assert!(skill.contains(
        "run the watch in the foreground with no `--timeout` and the Bash tool timeout at 600000 ms"
    ));
    assert!(skill.contains("`\"event\": \"timeout\"`"));
    assert!(skill.contains("\"event\": \"malformed\""));
    assert!(skill.contains("`watch` and `wait` print those first lines."));
    let babysit = read("references/commands/babysit.md");
    assert!(!babysit.contains("--wait 540"));
    assert!(babysit.contains("watch \"$SLUG/<id>\" --pr"));
    assert!(!babysit.contains("Run every watch with the Bash tool timeout at 600000 ms."));
    assert!(babysit.contains(
        "Start every watch as `\"$RUNTIME\" watch \"$SLUG/<id>\" --pr --timeout 7080` once, with the Bash tool's `run_in_background` and its timeout at 7200000 ms"
    ));
    assert!(babysit.contains("On `\"event\": \"timeout\"`, start it again."));
    assert!(babysit.contains("Time inside a watch does not count toward the 10 minutes."));
}
