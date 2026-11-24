//! Home selection checks ported from tests/cli/test_location.py.

use std::fs;
use std::process::Command;

#[test]
fn home_flag_beats_factory_home_and_creates_no_default() {
    let root = std::env::temp_dir().join(format!("factory-location-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let root = fs::canonicalize(&root).unwrap();
    let (explicit, env_home) = (root.join("explicit"), root.join("environment"));
    fs::create_dir_all(&explicit).unwrap();
    fs::create_dir_all(&env_home).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_factory"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &root)
        .env("FACTORY_HOME", &env_home)
        .arg("--home")
        .arg(&explicit)
        .arg("where")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(2), "{text}");
    let want = format!(
        "Set api_url in {}, then run /factory init again.",
        explicit.canonicalize().unwrap().join("config.md").display()
    );
    assert!(text.contains(&want), "{text}");
    assert!(!text.contains(&env_home.display().to_string()), "{text}");
    assert!(!root.join(".config").exists());
    fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn a_non_utf8_session_is_a_bad_tag() {
    use std::os::unix::ffi::OsStrExt;
    let root = std::env::temp_dir().join(format!("factory-session-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_factory"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &root)
        .env("FACTORY_SESSION", std::ffi::OsStr::from_bytes(b"tag\xff"))
        .arg("where")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(
        (out.status.code(), String::from_utf8_lossy(&out.stderr)),
        (
            Some(2),
            "factory.py: bad session tag in --session or FACTORY_SESSION: use letters, digits, - and _\n".into()
        )
    );
    fs::remove_dir_all(&root).unwrap();
}
