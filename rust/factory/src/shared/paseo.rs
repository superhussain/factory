//! The Paseo server and session this process runs under.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use super::consts::PASEO_ID;
use super::proc::{capture, which};
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

/// This host's `paseo ls -g --json` list, with any further arguments, under PASEO_HOME when set;
/// None when paseo is missing, fails, or prints no JSON.
pub fn paseo_ls(extra: &[&str]) -> Option<Value> {
    let exe = which("paseo")?.to_string_lossy().into_owned();
    let home = std::env::var("PASEO_HOME").unwrap_or_default();
    let mut argv = vec![exe.as_str(), "ls", "-g", "--json"];
    argv.extend(extra);
    if !home.is_empty() {
        argv.extend(["--home", home.as_str()]);
    }
    let r = capture(&argv, None, &[], Some(Duration::from_secs(20))).ok()?;
    serde_json::from_str(&r.stdout).ok().filter(|_| r.code == 0)
}
