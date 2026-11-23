//! The Paseo server and session this process runs under.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

use super::consts::PASEO_ID;
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
