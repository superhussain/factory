//! The daemon's claim heartbeat: before each pickup pass it refreshes `claim.seen` on this
//! device's claims whose session still has a live local Paseo agent, in one board write.
//!
//! A session maps to its agent through the job's `paseo` link, `{"server", "agent"}`, which `hold`
//! and `board` already write from `PASEO_AGENT_ID` for the session that holds the claim. The agent
//! is live when `paseo ls` shows it running, or open while `paseo ls --label
//! paseo.parent-agent-id=<agent>` finds a running child, as an orchestrator waiting on a subagent
//! is. The child query runs only for a due agent that is open but not running, since Paseo 0.16
//! leaves labels out of the plain list. A claim whose agent is
//! gone, closed, or idle with no running child is left to age out, so takeover and `stuck` work
//! as before.
//!
//! The agent must have claimed the job on this host, as the `paseo-claims/<job>/<agent>` marker
//! that `claim`, `board` and `hold` write shows, so a board row pointed at some other local agent
//! never keeps a claim alive. The agent must also carry the title pickup launches the job's
//! sessions with, `factory <id>` or `babysit <id>`. A long-lived agent that changed its session tag, left a job without releasing
//! it, or kept a stale link from idea pickup is titled for another job or for none, so its claims
//! age out.
//!
//! A claim that stays due, such as a dead session's, does not make every pass run `paseo ls`: the
//! heartbeat skips the read while the due claims match its last read, for up to 10 minutes.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::commands::claims::{paseo_claim_path, CLAIM_REFRESH_MINUTES};
use crate::shared::board::{dump_board, load_board};
use crate::shared::board_client::{cas_via, Api};
use crate::shared::consts::TERMINAL;
use crate::shared::exit::Res;
use crate::shared::home::device_name;
use crate::shared::paseo::paseo_server;
use crate::shared::time::{now, parse_stamp, unix_now};

const PARENT_AGENT: &str = "paseo.parent-agent-id";

/// How long a job may go with no real board change, by its own `updated`, before the heartbeat
/// stops refreshing its claim. A wedged agent that stays `running` then ages out, and `stuck`
/// can resume the job.
const QUIET_CAP_HOURS: i64 = 6;

/// What the heartbeat asks Paseo: this host's whole agent list for None, or for Some(agent) only
/// the agents labelled as that agent's children.
pub(crate) type List<'a> = &'a dyn Fn(Option<&str>) -> Option<Value>;

/// This host's agent list, or with parent the agents labelled as its children.
fn paseo_list(parent: Option<&str>) -> Option<Value> {
    match parent {
        None => crate::shared::paseo::paseo_ls(&[]),
        Some(agent) => {
            crate::shared::paseo::paseo_ls(&["--label", &format!("{PARENT_AGENT}={agent}")])
        }
    }
}

/// The due claims a heartbeat last asked Paseo about, and when. While the same claims stay due,
/// none of them was live at that read, so the next pass skips Paseo until `reprobe` has passed.
pub(crate) struct Heart {
    last: Option<(Vec<(String, String)>, Instant)>,
    reprobe: Duration,
}

impl Default for Heart {
    fn default() -> Self {
        Heart {
            last: None,
            reprobe: Duration::from_secs((CLAIM_REFRESH_MINUTES * 60.0) as u64),
        }
    }
}

/// One log line for a heartbeat's result, when it wrote or failed.
pub(crate) fn report(result: Res<Vec<String>>, log: &dyn Fn(&str)) {
    match result {
        Ok(ids) if ids.is_empty() => {}
        Ok(ids) => log(&format!("heartbeat: refreshed seen on {}", ids.join(" "))),
        Err(e) => log(&format!("heartbeat failed: {}", e.stderr.trim())),
    }
}

impl Heart {
    /// A heart that asks Paseo again about an unchanged due set after reprobe.
    #[cfg(test)]
    pub(crate) fn every(reprobe: Duration) -> Self {
        Heart {
            last: None,
            reprobe,
        }
    }

    /// The daemon's heartbeat before a pickup pass, as this host's device on its Paseo server.
    /// It logs the jobs it refreshed, or why the write failed, and never stops the pass.
    pub(crate) fn before_pass(&mut self, home: &Path, log: &dyn Fn(&str)) {
        // Tests that run the whole daemon never read this host's real agents.
        if cfg!(test) {
            return;
        }
        let Some(server) = paseo_server() else {
            return;
        };
        let result = self.beat_with(
            home,
            &device_name(home),
            &server,
            &paseo_list,
            &crate::shared::http::api,
        );
        report(result, log);
    }

    /// One heartbeat for home, as device on Paseo server, with the Paseo list and Worker client
    /// injected: the ids whose seen it refreshed, in board order. Nothing is written, and Paseo
    /// is not asked, when no claim is due or the same claims were due at a read under reprobe ago.
    pub(crate) fn beat_with(
        &mut self,
        home: &Path,
        device: &str,
        server: &str,
        list: List,
        api: Api,
    ) -> Res<Vec<String>> {
        let agents: OnceCell<Option<Value>> = OnceCell::new();
        let mut children: HashMap<String, bool> = HashMap::new();
        let last = &mut self.last;
        let reprobe = self.reprobe;
        cas_via(home, None, api, |text| {
            let mut board = load_board(text)?;
            let cutoff = unix_now() - (CLAIM_REFRESH_MINUTES * 60.0) as i64;
            let mut due = due_claims(&board, device, server, cutoff);
            let quiet_since = unix_now() - QUIET_CAP_HOURS * 3600;
            due.retain(|(i, agent)| {
                let job = &board["jobs"][*i];
                job["updated"]
                    .as_str()
                    .and_then(parse_stamp)
                    .is_some_and(|u| u >= quiet_since)
                    && job["id"]
                        .as_str()
                        .and_then(|id| paseo_claim_path(home, id, agent))
                        .is_some_and(|marker| marker.is_file())
            });
            if due.is_empty() {
                return Ok((None, Vec::new()));
            }
            let key: Vec<(String, String)> = due
                .iter()
                .map(|(i, a)| {
                    (
                        board["jobs"][*i]["id"].as_str().unwrap_or_default().into(),
                        a.clone(),
                    )
                })
                .collect();
            if agents.get().is_none()
                && last
                    .as_ref()
                    .is_some_and(|(k, at)| *k == key && at.elapsed() < reprobe)
            {
                return Ok((None, Vec::new()));
            }
            let Some(all) = agents.get_or_init(|| list(None)) else {
                return Ok((None, Vec::new()));
            };
            let stamp = now();
            let mut ids = Vec::new();
            for (i, agent) in due {
                let id = board["jobs"][i]["id"].as_str().unwrap_or_default();
                let Some(entry) = entry_of(all, &agent).filter(|a| titled_for(a, id)) else {
                    continue;
                };
                let live = match entry["status"].as_str() {
                    Some("running") => true,
                    Some("closed") | None => false,
                    Some(_) => *children
                        .entry(agent.clone())
                        .or_insert_with(|| list(Some(&agent)).is_some_and(|c| any_running(&c))),
                };
                if live {
                    board["jobs"][i]["claim"]["seen"] = stamp.clone().into();
                    ids.push(
                        board["jobs"][i]["id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    );
                }
            }
            // Only a read that found nothing live is remembered: a write that fails leaves the
            // same claims due, and the next pass must read and try again.
            if ids.is_empty() {
                *last = Some((key, Instant::now()));
                return Ok((None, ids));
            }
            board["updated"] = stamp.into();
            Ok((Some(dump_board(&board)), ids))
        })
    }
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

/// Agent's entry in a `paseo ls --json` list; the last one wins, as with a dict.
fn entry_of<'a>(list: &'a Value, agent: &str) -> Option<&'a Value> {
    list.as_array()?
        .iter()
        .rev()
        .find(|a| a["id"].as_str() == Some(agent))
}

/// Whether an agent carries a title pickup launches job id's sessions with: `factory <id>` or
/// `babysit <id>`. An agent under any other title may have moved on from the job, or never been
/// its session, so its liveness says nothing about the claim.
fn titled_for(agent: &Value, id: &str) -> bool {
    agent["name"]
        .as_str()
        .is_some_and(|n| n == format!("factory {id}") || n == format!("babysit {id}"))
}

/// Whether a `paseo ls --json` list holds a running agent.
fn any_running(list: &Value) -> bool {
    list.as_array()
        .is_some_and(|l| l.iter().any(|a| a["status"] == "running"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::shared::http::tests::{home_for, serve};
    use crate::shared::time::format_stamp;
    use serde_json::json;
    use std::cell::RefCell;

    pub(crate) const SERVER: &str = "srv_here";

    pub(crate) fn minutes_ago(minutes: i64) -> String {
        format_stamp(unix_now() - minutes * 60)
    }

    pub(crate) fn claim(device: &str, session: &str, seen: &str) -> Value {
        json!({"device": device, "session": session, "since": seen, "seen": seen})
    }

    pub(crate) fn job(id: &str, stage: &str, claim: Value, agent: Option<(&str, &str)>) -> Value {
        let mut j = json!({"id": id, "project": "p", "stage": stage, "updated": minutes_ago(60), "claim": claim});
        if let Some((server, agent)) = agent {
            j["paseo"] = json!({"server": server, "agent": agent});
        }
        j
    }

    /// One agent as Paseo 0.16's `paseo ls -g --json` prints it: no labels key.
    pub(crate) fn agent(id: &str, status: &str, name: &str) -> Value {
        json!({"id": id, "shortId": &id[..id.len().min(7)], "name": name, "provider": "claude/claude-opus-5-5", "thinking": "medium", "status": status, "cwd": "~/w", "created": "1 minutes ago", "background": false, "workspaceId": "wks_1"})
    }

    /// A fake Paseo: `all` for the whole list, and for a child query the agents `children` lists
    /// under that parent. Every query is recorded.
    pub(crate) struct Fake {
        pub all: Option<Value>,
        pub children: Vec<(&'static str, Value)>,
        pub asked: RefCell<Vec<Option<String>>>,
    }

    impl Fake {
        pub(crate) fn new(all: Value, children: Vec<(&'static str, Value)>) -> Self {
            Fake {
                all: Some(all),
                children,
                asked: RefCell::new(Vec::new()),
            }
        }

        pub(crate) fn list(&self, parent: Option<&str>) -> Option<Value> {
            self.asked.borrow_mut().push(parent.map(String::from));
            match parent {
                None => self.all.clone(),
                Some(p) => Some(
                    self.children
                        .iter()
                        .find(|(id, _)| *id == p)
                        .map_or(json!([]), |(_, c)| c.clone()),
                ),
            }
        }
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

    /// Writes the paseo-claims marker for each job's agent on board, as `hold` does when this
    /// host's session claims it.
    pub(crate) fn mark(home: &Path, board: &Value) {
        for j in board["jobs"].as_array().unwrap() {
            if let (Some(id), Some(agent)) = (j["id"].as_str(), j["paseo"]["agent"].as_str()) {
                let path = paseo_claim_path(home, id, agent).unwrap();
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "").unwrap();
            }
        }
    }

    /// The ids one heartbeat over board refreshes, and the board requests it sent.
    fn beat_on(name: &str, board: &Value, fake: &Fake) -> (Vec<String>, Vec<String>) {
        let (origin, seen) = serve(vec![get(board), (200, "{}", vec![])]);
        let home = home_for(name, &origin);
        mark(&home, board);
        let ids = Heart::default()
            .beat_with(
                &home,
                "mac",
                SERVER,
                &|p| fake.list(p),
                &crate::shared::http::api,
            )
            .unwrap();
        let methods = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.method.clone())
            .collect();
        (ids, methods)
    }

    /// A running agent is live. An idle one is live only while the label query finds a running
    /// child, and that query runs only for an open agent that is not running.
    #[test]
    fn running_agents_and_idle_parents_of_running_children_are_live() {
        let old = minutes_ago(30);
        let jobs = ["run", "waiting", "idle", "gone", "missing", "idle-kid"]
            .iter()
            .enumerate()
            .map(|(n, a)| {
                let id = format!("00{}-x", n + 1);
                job(
                    leak(id),
                    "build",
                    claim("mac", "s", &old),
                    Some((SERVER, a)),
                )
            })
            .collect();
        let t = |n: usize| format!("factory 00{n}-x");
        let fake = Fake::new(
            json!([
                agent("run", "running", &t(1)),
                agent("waiting", "idle", &t(2)),
                agent("idle", "idle", &t(3)),
                agent("gone", "closed", &t(4)),
                agent("idle-kid", "idle", &t(6)),
            ]),
            vec![
                ("waiting", json!([agent("sub", "running", "s")])),
                ("idle-kid", json!([agent("sub2", "idle", "s")])),
                ("gone", json!([agent("sub3", "running", "s")])),
            ],
        );
        let (ids, methods) = beat_on("heartbeat-live", &board_of(jobs), &fake);
        assert_eq!(ids, ["001-x", "002-x"]);
        assert_eq!(methods, ["GET", "PATCH"]);
        let asked = fake.asked.borrow().clone();
        let want: Vec<Option<String>> = vec![
            None,
            Some("waiting".into()),
            Some("idle".into()),
            Some("idle-kid".into()),
        ];
        assert_eq!(asked, want);
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
        mark(&home, &board);
        let fake = Fake::new(
            json!([
                agent("live1", "running", "factory 001-a"),
                agent("dead", "closed", "factory 002-b"),
                agent("waiter", "idle", "babysit 003-c"),
                agent("sub", "running", "x"),
                agent("idle", "idle", "factory 004-d"),
            ]),
            vec![("waiter", json!([agent("sub", "running", "x")]))],
        );
        let ids = Heart::default()
            .beat_with(
                &home,
                "mac",
                SERVER,
                &|p| fake.list(p),
                &crate::shared::http::api,
            )
            .unwrap();
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
        let fake = Fake::new(json!([]), vec![]);
        let (ids, methods) = beat_on("heartbeat-fresh", &board, &fake);
        assert!(ids.is_empty());
        assert_eq!(methods, ["GET"]);
        assert!(fake.asked.borrow().is_empty());
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
        let mut fake = Fake::new(json!([]), vec![]);
        fake.all = None;
        let (ids, methods) = beat_on("heartbeat-silent", &board, &fake);
        assert!(ids.is_empty());
        assert_eq!(methods, ["GET"]);
    }

    /// Only a claim whose agent this host recorded claiming the job is refreshed. A row whose
    /// link names a running local agent with no marker, as a session on another host or a
    /// hand-edited row leaves, ages out, and with no marked claim due Paseo is not asked.
    #[test]
    fn a_claim_without_this_hosts_marker_ages_out() {
        let old = minutes_ago(30);
        let marked = job(
            "001-a",
            "build",
            claim("mac", "s1", &old),
            Some((SERVER, "mine")),
        );
        let unmarked = job(
            "002-b",
            "build",
            claim("mac", "s2", &old),
            Some((SERVER, "other")),
        );
        let fake = Fake::new(
            json!([
                agent("mine", "running", "factory 001-a"),
                agent("other", "running", "factory 002-b"),
            ]),
            vec![],
        );
        let board = board_of(vec![marked.clone(), unmarked.clone()]);
        let (origin, seen) = serve(vec![get(&board), (200, "{}", vec![]), get(&board)]);
        let home = home_for("heartbeat-marker", &origin);
        mark(&home, &board_of(vec![marked]));
        let mut heart = Heart::default();
        let list = |p: Option<&str>| fake.list(p);
        let ids = heart
            .beat_with(&home, "mac", SERVER, &list, &crate::shared::http::api)
            .unwrap();
        assert_eq!(ids, ["001-a"]);
        std::fs::remove_dir_all(home.join("paseo-claims")).unwrap();
        let ids = heart
            .beat_with(&home, "mac", SERVER, &list, &crate::shared::http::api)
            .unwrap();
        assert!(ids.is_empty());
        assert_eq!(fake.asked.borrow().len(), 1, "no marked claim: no read");
        let methods: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.method.clone())
            .collect();
        assert_eq!(methods, ["GET", "PATCH", "GET"]);
    }

    /// A running agent refreshes only the job its title names. One titled for another job, as a
    /// stale link from idea pickup leaves, or titled by hand, leaves the claim to age out, and an
    /// idle one is not asked about children.
    #[test]
    fn only_an_agent_titled_for_the_job_refreshes_it() {
        let old = minutes_ago(30);
        let board = board_of(vec![
            job(
                "001-a",
                "build",
                claim("mac", "s1", &old),
                Some((SERVER, "mine")),
            ),
            job(
                "002-b",
                "build",
                claim("mac", "s2", &old),
                Some((SERVER, "stale")),
            ),
            job(
                "003-c",
                "build",
                claim("mac", "s3", &old),
                Some((SERVER, "by-hand")),
            ),
            job(
                "004-d",
                "build",
                claim("mac", "s4", &old),
                Some((SERVER, "prefix")),
            ),
            job(
                "005-e",
                "build",
                claim("mac", "s5", &old),
                Some((SERVER, "idle-other")),
            ),
        ]);
        let fake = Fake::new(
            json!([
                agent("mine", "running", "babysit 001-a"),
                agent("stale", "running", "factory 009-later-idea"),
                agent("by-hand", "running", "Design review framework"),
                agent("prefix", "running", "factory 004-d daemon claim heartbeats"),
                agent("idle-other", "idle", "factory 010-other"),
            ]),
            vec![("idle-other", json!([agent("kid", "running", "x")]))],
        );
        let (ids, methods) = beat_on("heartbeat-title", &board, &fake);
        assert_eq!(ids, ["001-a"]);
        assert_eq!(methods, ["GET", "PATCH"]);
        assert_eq!(fake.asked.borrow().len(), 1);
    }

    /// A job with no real board change for QUIET_CAP_HOURS, or with no readable updated stamp,
    /// gets no refresh even while its agent runs.
    #[test]
    fn a_job_quiet_past_the_cap_ages_out() {
        let old = minutes_ago(30);
        let mut quiet = job(
            "002-b",
            "build",
            claim("mac", "s2", &old),
            Some((SERVER, "b")),
        );
        quiet["updated"] = minutes_ago(QUIET_CAP_HOURS * 60 + 1).into();
        let mut odd = job(
            "003-c",
            "build",
            claim("mac", "s3", &old),
            Some((SERVER, "c")),
        );
        odd["updated"] = "garbage".into();
        let mut near = job(
            "001-a",
            "build",
            claim("mac", "s1", &old),
            Some((SERVER, "a")),
        );
        near["updated"] = minutes_ago(QUIET_CAP_HOURS * 60 - 1).into();
        let fake = Fake::new(
            json!([
                agent("a", "running", "factory 001-a"),
                agent("b", "running", "factory 002-b"),
                agent("c", "running", "factory 003-c"),
            ]),
            vec![],
        );
        let (ids, methods) = beat_on("heartbeat-cap", &board_of(vec![near, quiet, odd]), &fake);
        assert_eq!(ids, ["001-a"]);
        assert_eq!(methods, ["GET", "PATCH"]);
    }

    /// A dead claim that stays due reads Paseo once: the next pass with the same due claims skips
    /// the read until reprobe has passed, and a change in the due claims reads again at once.
    #[test]
    fn an_unchanged_due_set_skips_paseo_until_reprobe() {
        let old = minutes_ago(30);
        let dead = job(
            "001-a",
            "build",
            claim("mac", "s1", &old),
            Some((SERVER, "a")),
        );
        let other = job(
            "002-b",
            "build",
            claim("mac", "s2", &old),
            Some((SERVER, "b")),
        );
        let one = board_of(vec![dead.clone()]);
        let two = board_of(vec![dead, other]);
        let (origin, seen) = serve(vec![get(&one), get(&one), get(&two), get(&two), get(&two)]);
        let home = home_for("heartbeat-reprobe", &origin);
        mark(&home, &two);
        let fake = Fake::new(json!([agent("a", "closed", "factory 001-a")]), vec![]);
        // A reprobe that never comes for the skips, then none at all for the reread, so no
        // assertion waits on the clock.
        let mut heart = Heart::every(Duration::MAX);
        let beat = |heart: &mut Heart| {
            let ids = heart
                .beat_with(
                    &home,
                    "mac",
                    SERVER,
                    &|p| fake.list(p),
                    &crate::shared::http::api,
                )
                .unwrap();
            assert!(ids.is_empty());
            fake.asked.borrow().len()
        };
        assert_eq!(beat(&mut heart), 1);
        assert_eq!(beat(&mut heart), 1, "same due claims: no read");
        assert_eq!(beat(&mut heart), 2, "a new due claim reads again");
        assert_eq!(beat(&mut heart), 2);
        heart.reprobe = Duration::ZERO;
        assert_eq!(
            beat(&mut heart),
            3,
            "past reprobe the same claims read again"
        );
        assert_eq!(seen.lock().unwrap().len(), 5);
    }

    /// A write that fails is not remembered: the next pass reads Paseo again and retries it.
    #[test]
    fn a_failed_write_is_retried_next_pass() {
        let old = minutes_ago(30);
        let board = board_of(vec![job(
            "001-a",
            "build",
            claim("mac", "s1", &old),
            Some((SERVER, "a")),
        )]);
        let (origin, seen) = serve(vec![
            get(&board),
            (500, "{}", vec![]),
            get(&board),
            (200, "{}", vec![]),
        ]);
        let home = home_for("heartbeat-retry", &origin);
        mark(&home, &board);
        let fake = Fake::new(json!([agent("a", "running", "factory 001-a")]), vec![]);
        let mut heart = Heart::every(Duration::MAX);
        let list = |p: Option<&str>| fake.list(p);
        assert!(heart
            .beat_with(&home, "mac", SERVER, &list, &crate::shared::http::api)
            .is_err());
        let ids = heart
            .beat_with(&home, "mac", SERVER, &list, &crate::shared::http::api)
            .unwrap();
        assert_eq!(ids, ["001-a"]);
        assert_eq!(fake.asked.borrow().len(), 2);
        let methods: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.method.clone())
            .collect();
        assert_eq!(methods, ["GET", "PATCH", "GET", "PATCH"]);
    }
}
