//! Source checks ported from tests/cli/test_hosts.py.

use std::path::Path;

/// Every non-comment line outside a test module or tests.rs file that names /api/hosts, across the crate's source.
fn host_lines() -> Vec<String> {
    let mut files = vec![Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).to_path_buf()];
    let mut lines = Vec::new();
    while let Some(path) = files.pop() {
        if path.is_dir() {
            for entry in std::fs::read_dir(&path).unwrap() {
                files.push(entry.unwrap().path());
            }
            continue;
        }
        // A tests.rs file is a whole test module, as pickup/tests.rs is.
        if path.extension().is_none_or(|e| e != "rs") || path.ends_with("tests.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let code = text.split("#[cfg(test)]\nmod tests").next().unwrap();
        for line in code.lines() {
            let trimmed = line.trim();
            if trimmed.contains("/api/hosts") && !trimmed.starts_with("//") {
                lines.push(trimmed.to_string());
            }
        }
    }
    lines
}

#[test]
fn device_calls_only_pair_and_status() {
    let lines = host_lines();
    assert!(!lines.is_empty());
    let reads = [
        r#"let r = api(home, "GET", "/api/hosts", None, &[200, 404], &[])?;"#,
        r#"let hosts = parse(&api(home, "GET", "/api/hosts", None, &[200], &[])?.text()?)?;"#,
    ];
    for read in reads {
        assert_eq!(lines.iter().filter(|l| *l == read).count(), 1, "{read}");
    }
    for line in lines.iter().filter(|l| !reads.contains(&l.as_str())) {
        let ok = ["pair", "status"].iter().any(|end| {
            line.split("\"/api/hosts/{")
                .nth(1)
                .and_then(|rest| rest.split_once("}/"))
                .is_some_and(|(name, tail)| {
                    name.chars().all(|c| c.is_alphanumeric() || c == '_')
                        && tail.starts_with(&format!("{end}\""))
                })
        });
        assert!(ok, "a device call outside pair and status: {line}");
    }
}
