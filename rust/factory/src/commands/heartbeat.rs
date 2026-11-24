//! The daemon's claim heartbeat: before each pickup pass it refreshes `claim.seen` on this
//! device's claims whose session still has a live local Paseo agent, in one board write.
//!
//! A session maps to its agent through the job's `paseo` link, `{"server", "agent"}`, which `hold`
//! and `board` already write from `PASEO_AGENT_ID` for the session that holds the claim. The agent
//! is live when `paseo ls` shows it running, or open with a running agent labelled as its child
//! (`paseo.parent-agent-id`), as an orchestrator waiting on a subagent is. A claim whose agent is
//! gone, closed, or idle with no running child is left to age out, so takeover and `stuck` work
//! as before.

use std::cell::OnceCell;
use std::collections::HashSet;
use std::path::Path;

use serde_json::Value;

use crate::commands::claims::CLAIM_REFRESH_MINUTES;
use crate::shared::board::{dump_board, load_board};
use crate::shared::board_client::{cas_via, Api};
use crate::shared::consts::TERMINAL;
use crate::shared::exit::Res;
use crate::shared::home::device_name;
use crate::shared::paseo::paseo_server;
use crate::shared::time::{now, parse_stamp, unix_now};

const PARENT_AGENT: &str = "paseo.parent-agent-id";
const PARENT_SERVER: &str = "paseo.parent-server-id";

/// The daemon's heartbeat before a pickup pass, as this host's device on its Paseo server. It
/// logs the jobs it refreshed, or why the write failed, and never stops the pass.
pub(crate) fn before_pass(home: &Path, log: &dyn Fn(&str)) {
    // Tests that run the whole daemon never read this host's real agents.
    if cfg!(test) {
        return;
    }
    let Some(server) = paseo_server() else {
        return;
    };
    report(beat(home, &device_name(home), &server), log);
}

/// One log line for a heartbeat's result, when it wrote or failed.
pub(crate) fn report(result: Res<Vec<String>>, log: &dyn Fn(&str)) {
    match result {
        Ok(ids) if ids.is_empty() => {}
        Ok(ids) => log(&format!("heartbeat: refreshed seen on {}", ids.join(" "))),
        Err(e) => log(&format!("heartbeat failed: {}", e.stderr.trim())),
    }
}

/// One heartbeat for home, as device on Paseo server: the ids whose seen it refreshed, in board
/// order. Nothing is written, and paseo is not asked, when no claim is due.
pub(crate) fn beat(home: &Path, device: &str, server: &str) -> Res<Vec<String>> {
    beat_with(
        home,
        device,
        server,
        &|| crate::shared::paseo::paseo_ls(&[]),
        &crate::shared::http::api,
    )
}

/// `beat` with the Paseo list and Worker client injected.
pub(crate) fn beat_with(
    home: &Path,
    device: &str,
    server: &str,
    list: &dyn Fn() -> Option<Value>,
    api: Api,
) -> Res<Vec<String>> {
    let agents: OnceCell<Option<HashSet<String>>> = OnceCell::new();
    cas_via(home, None, api, |text| {
        let mut board = load_board(text)?;
        let cutoff = unix_now() - (CLAIM_REFRESH_MINUTES * 60.0) as i64;
        let due = due_claims(&board, device, server, cutoff);
        if due.is_empty() {
            return Ok((None, Vec::new()));
        }
        let Some(live) = agents.get_or_init(|| list().map(|l| live_agents(&l, server))) else {
            return Ok((None, Vec::new()));
        };
        let stamp = now();
        let mut ids = Vec::new();
        for (i, agent) in due {
            if live.contains(&agent) {
                board["jobs"][i]["claim"]["seen"] = stamp.clone().into();
                ids.push(
                    board["jobs"][i]["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                );
            }
        }
        if ids.is_empty() {
            return Ok((None, ids));
        }
        board["updated"] = stamp.into();
        Ok((Some(dump_board(&board)), ids))
    })
}

/// (row index, agent) of each open job device claims whose seen is older than cutoff and whose
/// `paseo` link names an agent on server.
fn due_claims(board: &Value, device: &str, server: &str, cutoff: i64) -> Vec<(usize, String)> {
    let jobs = board["jobs"].as_array().map_or(&[][..], Vec::as_slice);
    jobs.iter()
        .enumerate()
        .filter_map(|(i, j)| {
            j["id"].as_str()?;
            if j["stage"].as_str().is_some_and(|s| TERMINAL.contains(&s)) {
                return None;
            }
            let claim = j.get("claim").filter(|c| c.is_object())?;
            if claim["device"].as_str() != Some(device) {
                return None;
            }
            let seen = claim["seen"].as_str().and_then(parse_stamp)?;
            if seen >= cutoff {
                return None;
            }
            let link = j.get("paseo").filter(|p| p.is_object())?;
            if link["server"].as_str() != Some(server) {
                return None;
            }
            Some((i, link["agent"].as_str()?.to_string()))
        })
        .collect()
}

/// The agents a `paseo ls --json` list shows alive: each running agent, and each open agent that
/// a running agent of this server labels as its parent.
fn live_agents(list: &Value, server: &str) -> HashSet<String> {
    let agents = list.as_array().map_or(&[][..], Vec::as_slice);
    let open: HashSet<&str> = agents
        .iter()
        .filter(|a| a["status"].as_str().is_some_and(|s| s != "closed"))
        .filter_map(|a| a["id"].as_str())
        .collect();
    let mut live = HashSet::new();
    for a in agents.iter().filter(|a| a["status"] == "running") {
        if let Some(id) = a["id"].as_str() {
            live.insert(id.to_string());
        }
        let labels = &a["labels"];
        let here = labels[PARENT_SERVER].as_str().is_none_or(|s| s == server);
        if let Some(parent) = labels[PARENT_AGENT]
            .as_str()
            .filter(|p| here && open.contains(p))
        {
            live.insert(parent.to_string());
        }
    }
    live
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::shared::http::tests::{home_for, serve};
    use crate::shared::time::format_stamp;
    use serde_json::json;

    pub(crate) const SERVER: &str = "srv_here";

    pub(crate) fn claim(device: &str, session: &str, seen: &str) -> Value {
        json!({"device": device, "session": session, "since": seen, "seen": seen})
    }

    pub(crate) fn job(id: &str, stage: &str, claim: Value, agent: Option<(&str, &str)>) -> Value {
        let mut j = json!({"id": id, "project": "p", "stage": stage, "updated": "2026-11-24T00:00:00Z", "claim": claim});
        if let Some((server, agent)) = agent {
            j["paseo"] = json!({"server": server, "agent": agent});
        }
        j
    }

    pub(crate) fn agent(id: &str, status: &str, parent: Option<&str>) -> Value {
        let mut a = json!({"id": id, "status": status, "name": "x", "labels": {}});
        if let Some(p) = parent {
            a["labels"] = json!({PARENT_AGENT: p, PARENT_SERVER: SERVER});
        }
        a
    }

    #[test]
    fn running_agents_and_parents_of_running_children_are_live() {
        let list = json!([
            agent("run", "running", None),
            agent("idle", "idle", None),
            agent("waiting", "idle", None),
            agent("child", "running", Some("waiting")),
            agent("gone", "closed", None),
            agent("orphan", "running", Some("gone")),
            agent("idle-child", "idle", Some("idle")),
        ]);
        let mut live: Vec<String> = live_agents(&list, SERVER).into_iter().collect();
        live.sort();
        assert_eq!(live, ["child", "orphan", "run", "waiting"]);
        // A child whose parent lives on another server names no agent here.
        let mut far = agent("c2", "running", Some("idle"));
        far["labels"][PARENT_SERVER] = "srv_far".into();
        assert!(!live_agents(&json!([agent("idle", "idle", None), far]), SERVER).contains("idle"));
    }

    #[test]
    fn only_old_claims_of_this_device_on_this_server_are_due() {
        let old = minutes_ago(30);
        let fresh = minutes_ago(2);
        let board = json!({"jobs": [
            job("001-a", "build", claim("mac", "s1", &old), Some((SERVER, "a1"))),
            job("002-b", "build", claim("mac", "s2", &fresh), Some((SERVER, "a2"))),
            job("003-c", "build", claim("wsl", "s3", &old), Some((SERVER, "a3"))),
            job("004-d", "build", claim("mac", "s4", &old), Some(("srv_far", "a4"))),
            job("005-e", "build", claim("mac", "s5", &old), None),
            job("006-f", "merged", claim("mac", "s6", &old), Some((SERVER, "a6"))),
            job("007-g", "review", claim("mac", "s7", "garbage"), Some((SERVER, "a7"))),
            job("008-h", "review", claim("mac", "s8", &old), Some((SERVER, "a8"))),
        ]});
        let cutoff = unix_now() - 600;
        let due = due_claims(&board, "mac", SERVER, cutoff);
        assert_eq!(due, [(0, "a1".to_string()), (7, "a8".to_string())]);
    }

    pub(crate) fn minutes_ago(minutes: i64) -> String {
        format_stamp(unix_now() - minutes * 60)
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    /// A GET reply for board, which takes row PATCHes.
    pub(crate) fn get(board: &Value) -> (u16, &'static str, Vec<(&'static str, &'static str)>) {
        (
            200,
            leak(dump_board(board)),
            vec![("ETag", "\"e1\""), ("Accept-Patch", "application/json")],
        )
    }

    pub(crate) fn board_of(jobs: Vec<Value>) -> Value {
        json!({"updated": "2026-11-24T00:00:00Z", "jobs": jobs})
    }

    #[test]
    fn one_patch_refreshes_every_live_claim_and_leaves_dead_ones() {
        let old = minutes_ago(30);
        let board = board_of(vec![
            job(
                "001-a",
                "build",
                claim("mac", "s1", &old),
                Some((SERVER, "live1")),
            ),
            job(
                "002-b",
                "review",
                claim("mac", "s2", &old),
                Some((SERVER, "dead")),
            ),
            job(
                "003-c",
                "build",
                claim("mac", "s3", &old),
                Some((SERVER, "waiter")),
            ),
            job(
                "004-d",
                "build",
                claim("mac", "s4", &old),
                Some((SERVER, "idle")),
            ),
        ]);
        let (origin, seen) = serve(vec![get(&board), (200, "{}", vec![])]);
        let home = home_for("heartbeat-patch", &origin);
        let list = || {
            Some(json!([
                agent("live1", "running", None),
                agent("dead", "closed", None),
                agent("waiter", "idle", None),
                agent("sub", "running", Some("waiter")),
                agent("idle", "idle", None),
            ]))
        };
        let ids = beat_with(&home, "mac", SERVER, &list, &crate::shared::http::api).unwrap();
        assert_eq!(ids, ["001-a", "003-c"]);
        let seen = seen.lock().unwrap();
        let methods: Vec<&str> = seen.iter().map(|s| s.method.as_str()).collect();
        assert_eq!(methods, ["GET", "PATCH"]);
        let body: Value = serde_json::from_slice(&seen[1].body).unwrap();
        let rows = body["rows"].as_array().unwrap();
        let written: Vec<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(written, ["001-a", "003-c"]);
        for r in rows {
            let new = &r["row"]["claim"]["seen"];
            assert_ne!(new, &r["expect"]["claim"]["seen"]);
            // Only seen moves: the job's own updated stamp still marks its last real change.
            assert_eq!(r["row"]["updated"], r["expect"]["updated"]);
            assert_eq!(
                r["row"]["claim"]["session"],
                r["expect"]["claim"]["session"]
            );
        }
        assert!(body["updated"].as_str().unwrap() > "2026-11-24T00:00:00Z");
    }

    #[test]
    fn nothing_due_writes_nothing_and_skips_paseo() {
        let fresh = minutes_ago(1);
        let board = board_of(vec![job(
            "001-a",
            "build",
            claim("mac", "s1", &fresh),
            Some((SERVER, "live1")),
        )]);
        let (origin, seen) = serve(vec![get(&board)]);
        let home = home_for("heartbeat-fresh", &origin);
        let list = || -> Option<Value> { panic!("paseo is not asked when no claim is due") };
        let ids = beat_with(&home, "mac", SERVER, &list, &crate::shared::http::api).unwrap();
        assert!(ids.is_empty());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn paseo_silent_writes_nothing() {
        let old = minutes_ago(30);
        let board = board_of(vec![job(
            "001-a",
            "build",
            claim("mac", "s1", &old),
            Some((SERVER, "live1")),
        )]);
        let (origin, seen) = serve(vec![get(&board)]);
        let home = home_for("heartbeat-silent", &origin);
        let ids = beat_with(&home, "mac", SERVER, &|| None, &crate::shared::http::api).unwrap();
        assert!(ids.is_empty());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
