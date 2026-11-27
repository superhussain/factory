use std::fs;

fn read(path: &str) -> String {
    fs::read_to_string(format!(
        "{}/../../skills/factory/{path}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
}

#[test]
fn skill_uses_checks() {
    let execution = read("references/execution.md");
    let phrase = "Wait for checks with `\"$RUNTIME\" checks \"$SLUG/<id>\" --timeout 540`. Exit 0: \
         they passed, or the repo has none. It records that head as `checked_head`, and \
         `pr-merge` merges only that head. On a PR behind `$BASE`, it runs \
         `gh pr update-branch` and rechecks the new head. Exit 3: still pending; run it again once. \
         A second exit 3 in a row from `checks` or `pr-merge` moves the job to `needs-human` with \
         the note `merge stalled: exit 3 twice` and `needs` `merge or close PR #<n>: checks or \
         mergeability stayed pending`. Nothing merges. \
         Exit 1 names failing checks. Exit 4: no open PR. Exit 5: no usable gh reply. \
         Exit 6: `gh pr update-branch` failed; treat it as a merge conflict. Exit 7 means the approver escalated the job: the board marks it escalated or its decision.md \
         says ESCALATE. Nothing merges. Patch {\"automerge\": false, \"escalated\": true, \"needs\": \"review \
         and merge PR #<n>: the approver escalated it\", \"note\": \"escalated: checks held the merge\"} \
         and stop. Exit 8: earned autonomy holds the class after a revert; checks already set \
         automerge false and needs. Stop. Never poll `gh pr checks` in a loop or with `--watch`.";
    assert!(execution.contains(phrase), "{phrase}");
    let phrase = "Pass `--hold` to refresh this session's claim first, so a re-run during a long build needs \
         no separate `hold`.";
    assert!(read("SKILL.md").contains(phrase), "{phrase}");
}
