use std::fs;
use std::process::Command;

fn run(home: &std::path::Path, session: Option<&str>) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_factory"));
    cmd.env_remove("FACTORY_SESSION")
        .env_remove("PASEO_AGENT_ID")
        .arg("--home")
        .arg(home);
    if let Some(tag) = session {
        cmd.args(["--session", tag]);
    }
    cmd.args(["claim"])
        .arg(home)
        .args(["feat", "proj"])
        .stdin(std::process::Stdio::null());
    String::from_utf8_lossy(&cmd.output().unwrap().stderr).into_owned()
}

#[test]
fn session_is_scoped_and_new_tag_uses_stderr() {
    let home = std::env::temp_dir().join(format!("factory-contract-{}", std::process::id()));
    fs::create_dir_all(home.join("jobs")).unwrap();
    fs::write(
        home.join("config.md"),
        "---\napi_url: http://127.0.0.1:1\n---\n",
    )
    .unwrap();
    let fresh = run(&home, None);
    let first = fresh.lines().next().unwrap();
    assert!(first.starts_with("new session "), "{fresh}");
    assert!(
        first.ends_with(": pass --session <tag> to every later claim, next, hold, and board call")
            || first.contains(": pass --session "),
        "{fresh}"
    );
    let kept = run(&home, Some("test"));
    assert!(!kept.contains("new session"), "{kept}");
    let _ = fs::remove_file(home.join("config.md"));
    let _ = fs::remove_dir(home.join("jobs"));
    let _ = fs::remove_dir(&home);
}
