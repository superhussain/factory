//! The Paseo server and session this process runs under.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{json, Value};

use super::consts::PASEO_ID;
use super::proc::{paseo_bin, paseo_capture};
use super::py::{strip, universal_newlines};

/// This host's Paseo server id, else None.
pub fn paseo_server() -> Option<String> {
    let base = match std::env::var_os("PASEO_HOME").filter(|v| !v.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => std::env::home_dir()?.join(".paseo"),
    };
    let text = String::from_utf8(fs::read(base.join("server-id")).ok()?).ok()?;
    let server = strip(&universal_newlines(text)).to_string();
    PASEO_ID.is_full(&server).then_some(server)
}

/// {"server", "agent"} of the Paseo session running this process, else None.
pub fn paseo_session() -> Option<Value> {
    let agent = std::env::var("PASEO_AGENT_ID").unwrap_or_default();
    let agent = strip(&agent);
    let server = paseo_server()?;
    (PASEO_ID.is_full(agent) && !agent.starts_with('-'))
        .then(|| json!({"server": server, "agent": agent}))
}

/// False once this host's Paseo has rejected `--background`, as one older than the flag does.
static BACKGROUND: AtomicBool = AtomicBool::new(true);

/// This host's `paseo ls -g --background --json` list, with any further arguments, under
/// PASEO_HOME when set; None when paseo is missing, fails, or prints no JSON. `--background` keeps
/// agents in background workspaces, which the plain list leaves out. A Paseo that rejects the
/// flag is asked once more without it, and the flag stays off until the daemon restarts.
pub fn paseo_ls(extra: &[&str]) -> Option<Value> {
    paseo_ls_with(&paseo_bin()?, extra)
}

fn paseo_ls_with(exe: &str, extra: &[&str]) -> Option<Value> {
    let home = std::env::var("PASEO_HOME").unwrap_or_default();
    let run = |background: bool| {
        let mut argv = vec![exe, "ls", "-g"];
        if background {
            argv.push("--background");
        }
        argv.push("--json");
        argv.extend(extra);
        if !home.is_empty() {
            argv.extend(["--home", home.as_str()]);
        }
        paseo_capture(&argv, Duration::from_secs(20)).ok()
    };
    let mut r = run(BACKGROUND.load(Ordering::Relaxed))?;
    if r.code != 0
        && BACKGROUND.load(Ordering::Relaxed)
        && r.stderr.contains("unknown option '--background'")
    {
        BACKGROUND.store(false, Ordering::Relaxed);
        r = run(false)?;
    }
    serde_json::from_str(&r.stdout).ok().filter(|_| r.code == 0)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A Paseo without `--background` rejects it as Paseo does an unknown option; the list is
    /// read again without it, and later reads leave it off.
    #[test]
    fn a_paseo_without_background_is_asked_without_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::shared::fs::temp_dir("paseo-no-background");
        let (fake, log) = (dir.join("paseo"), dir.join("calls"));
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$*\" in *--background*) echo \"error: unknown option '--background'\" >&2; exit 1;; esac\necho '[{{\"id\":\"a1\"}}]'\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let fake = fake.to_string_lossy().into_owned();
        assert_eq!(paseo_ls_with(&fake, &[]), Some(json!([{"id": "a1"}])));
        assert_eq!(paseo_ls_with(&fake, &[]), Some(json!([{"id": "a1"}])));
        let calls = std::fs::read_to_string(&log).unwrap();
        let calls: Vec<&str> = calls
            .lines()
            .map(|l| l.split(" --home").next().unwrap())
            .collect();
        assert_eq!(
            calls,
            ["ls -g --background --json", "ls -g --json", "ls -g --json"]
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
