use std::fs;

fn read(rel: &str) -> String {
    fs::read_to_string(format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn dispatched_roles_read_project_documents() {
    for role in [
        "approver",
        "builder",
        "review-code",
        "review-security",
        "review-ui",
        "review-ux",
        "spec-writer",
    ] {
        let text = read(&format!("skills/factory/agents/factory-{role}.md"));
        assert!(
            text.contains(
                "### Project documents\n\nYour prompt may also give the absolute path of a project documents file."
            ),
            "{role}"
        );
        assert!(
            text.find("### Project documents").unwrap()
                < text.find("### Operator preferences").unwrap(),
            "{role}"
        );
    }
    assert!(!read("skills/factory/agents/factory-explorer.md").contains("Project documents"));
}

#[test]
fn skill_and_trust_describe_documents() {
    let skill = read("skills/factory/SKILL.md");
    assert!(skill.contains("the absolute path in `$DOCUMENTS`"));
    assert!(skill.contains("runtime/factory documents \"$SLUG\""));
    assert!(!skill.contains("automatic agent consumption"));
    assert!(read("docs/trust.md").contains("document:<slug>/<name>"));
    let settings = read("skills/factory/references/project-settings.md");
    let flat = format!("{skill} {settings}")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!flat.contains("cache/sync"));
    assert!(flat.contains(
        "`factory documents <slug>` writes the non-empty documents to `projects/<slug>.documents.md`"
    ));
    assert!(flat.contains(
        "That copy is read-only: edit documents in the dashboard; the CLI never uploads them."
    ));
}
