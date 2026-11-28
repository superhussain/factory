//! The daemon's own catch-up: after a pass, start a detached `daemon catch-up` once per origin commit when the installed bundle has lacked a merge on origin's base for BEHIND_FOR and no factory agent runs.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

use crate::commands::{catch_up, daemon, pickup};
use crate::shared::exit::Res;
use crate::shared::fs::{is_link, write_atomic};
use crate::shared::py::os_error;
use crate::shared::time::{now, parse_stamp, unix_now};

/// How long the bundle must lack a merge on origin's base before the daemon starts catch-up.
pub(crate) const BEHIND_FOR: i64 = 1800;
/// The schedulers whose daemon restart a detached child survives.
const SCHEDULERS: [&str; 2] = ["launchd", "cron"];
/// Beside the daemon lock: {"origin": "<40 hex>", "started": "<UTC stamp>"}.
const STATE_FILE: &str = "catch-up.json";

fn state_path(home: &Path) -> std::path::PathBuf {
    home.join("logs").join("daemon").join(STATE_FILE)
}

pub(crate) fn due(since: &str, origin: &str, tried: Option<&str>, now: i64) -> bool {
    tried != Some(origin) && parse_stamp(since).is_some_and(|t| now - t >= BEHIND_FOR)
}

fn tried(home: &Path) -> Option<String> {
    pickup::read_json(&state_path(home))?["origin"]
        .as_str()
        .map(String::from)
}

fn record(home: &Path, origin: &str) -> io::Result<()> {
    let body = json!({"origin": origin, "started": now()});
    write_atomic(&state_path(home), body.to_string().as_bytes())
}

fn last_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|l| !l.is_empty())
}

pub(crate) fn maybe_start(
    exe: &Path,
    home: &Path,
    agents: &dyn Fn() -> Option<Vec<String>>,
    log: &dyn Fn(&str),
) {
    let Some(commit) = pickup::bundle_stamp()
        .as_ref()
        .and_then(|b| b.get("commit"))
        .and_then(Value::as_str)
        .map(String::from)
    else {
        return;
    };
    let Some((since, origin)) = pickup::bundle_behind(home, &commit) else {
        return;
    };
    if !due(&since, &origin, tried(home).as_deref(), unix_now()) {
        return;
    }
    if !SCHEDULERS.contains(&pickup::daemon_scheduler()) {
        return;
    }
    if !agents().is_some_and(|v| v.is_empty()) {
        return;
    }
    if let Err(e) = record(home, &origin) {
        log(&format!(
            "catch-up: could not record {}: {}",
            state_path(home).display(),
            os_error(&e, None)
        ));
        return;
    }
    let child = daemon::day_log(home).and_then(|(file, _)| {
        let mut cmd = Command::new(exe);
        cmd.arg("--home")
            .arg(home)
            .args(["daemon", "catch-up"])
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file);
        pickup::detach(&mut cmd);
        cmd.spawn()
    });
    let short = |s: &str| s.chars().take(7).collect::<String>();
    log(&match child {
        Ok(child) => format!(
            "catch-up: bundle {} is behind origin {} since {since}; started pid {}",
            short(&commit),
            short(&origin),
            child.id()
        ),
        Err(e) => format!("catch-up: could not start: {}", os_error(&e, None)),
    });
}

pub(crate) fn run_child(home: &Path) -> Res<()> {
    let mut out = Vec::new();
    let result = catch_up::catch_up_to(&mut out);
    let _ = io::stdout().write_all(&out);
    let code = result.as_ref().err().map_or(0, |e| e.code);
    let out = String::from_utf8_lossy(&out);
    let last = result
        .as_ref()
        .err()
        .and_then(|e| last_line(&e.stderr))
        .or_else(|| last_line(&out))
        .unwrap_or("no output");
    let line = format!("{} catch-up exited {code}: {last}", now());
    println!("{line}");
    append_service_log(home, &line);
    result
}

fn append_service_log(home: &Path, line: &str) {
    let path = home.join("logs").join("daemon.log");
    if is_link(&path).unwrap_or(true) {
        return;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, crate::shared::fs::O_NOFOLLOW);
    if let Ok(mut file) = options.open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;

    const SINCE: &str = "2026-11-27T09:00:00Z";
    const ORIGIN: &str = "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432";

    fn at(stamp: &str) -> i64 {
        parse_stamp(stamp).unwrap()
    }

    #[test]
    fn due_waits_thirty_minutes() {
        assert!(!due(SINCE, ORIGIN, None, at("2026-11-27T09:29:59Z")));
        assert!(due(SINCE, ORIGIN, None, at("2026-11-27T09:30:00Z")));
    }

    #[test]
    fn due_skips_a_tried_origin() {
        let later = at("2026-11-27T09:31:00Z");
        assert!(!due(SINCE, ORIGIN, Some(ORIGIN), later));
        assert!(due(SINCE, ORIGIN, Some("a1b2c3d"), later));
        assert!(due(SINCE, ORIGIN, None, later));
    }

    #[test]
    fn due_rejects_a_bad_stamp() {
        assert!(!due("junk", ORIGIN, None, at("2026-11-27T09:31:00Z")));
    }

    #[test]
    fn state_round_trips() {
        let home = temp_dir("daemon-catch-up-state");
        std::fs::create_dir_all(home.join("logs").join("daemon")).unwrap();
        assert_eq!(tried(&home), None);
        record(&home, ORIGIN).unwrap();
        assert_eq!(tried(&home).as_deref(), Some(ORIGIN));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn last_line_takes_last_non_blank() {
        assert_eq!(last_line("a\nb\n\n"), Some("b"));
        assert_eq!(last_line("  \n"), None);
    }
}
