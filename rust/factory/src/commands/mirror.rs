//! The daemon's board mirror and the local endpoint commands read and write it through.
//!
//! The daemon fetches the whole board once per connect, then applies each rows frame whose `prev`
//! is the mirror's ETag. A frame that doesn't follow, a `changed` frame for the board, or a
//! reconnect refetches it. A private snapshot at `<home>/cache/board.json` lets a restarted daemon
//! answer from the last board while its first fetch runs. While the socket is down the mirror
//! answers nothing, so commands read the Worker. A mirror that has neither fetched nor applied a
//! frame for REFRESH is refetched, and one that hasn't for MAX_AGE answers nothing, so a lost
//! announcement or a snapshot that can't be refreshed never serves an old board for long.

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::commands::catch_up::random_hex;
use crate::shared::board_client::{
    addr_path, proof, same_secret, socket_path, view, Stream, MAX_PATCH_BYTES, MAX_PATCH_ROWS,
    PROTOCOL,
};
use crate::shared::fs::write_atomic;
use crate::shared::http::api;
use crate::shared::pyjson::{dumps, loads, COMPACT};

/// How many recent ETags the mirror remembers, to tell an old frame or fetch from a new one.
const SEEN: usize = 32;
/// How long a write waits for its own frame before the daemon refetches.
const OWN_FRAME: Duration = Duration::from_secs(2);
/// The longest request line: a 1 MiB PATCH body, escaped.
const MAX_REQUEST: u64 = 4 * 1024 * 1024;
/// How long the mirror goes without a fetch or applied frame before the reader refetches it.
pub const REFRESH: Duration = Duration::from_secs(300);
/// How long after its last fetch or applied frame the mirror still answers.
const MAX_AGE: Duration = Duration::from_secs(360);

#[derive(Default)]
struct State {
    doc: Option<Value>,
    text: Arc<String>,
    etag: String,
    patch: bool,
    /// True while the mirror may answer: the socket is up and the mirror holds a board.
    live: bool,
    /// ETags the mirror has held, newest last.
    seen: VecDeque<String>,
    /// When the mirror last matched the Worker: a fetch, an applied frame, or the snapshot's save.
    synced: Option<Instant>,
    /// How many times the socket has closed, so a fetch that began before a close can't make the
    /// mirror live again.
    closes: u64,
}

pub struct Mirror {
    home: PathBuf,
    state: Mutex<State>,
    moved: Condvar,
    /// Held for each fetch, so fetches finish in the order they start.
    fetching: Mutex<()>,
}

pub fn snapshot_path(home: &Path) -> PathBuf {
    home.join("cache").join("board.json")
}

impl Mirror {
    /// A mirror seeded from the snapshot, if one is readable. It answers nothing until connected.
    pub fn new(home: &Path) -> Arc<Mirror> {
        let mut state = State::default();
        if let Some(snap) = std::fs::read_to_string(snapshot_path(home))
            .ok()
            .and_then(|t| loads(&t).ok())
        {
            if let (Some(etag), Some(doc)) = (snap["etag"].as_str(), snap.get("board")) {
                state.text = Arc::new(dumps(doc, COMPACT));
                state.doc = Some(doc.clone());
                state.etag = etag.to_string();
                state.patch = snap["patch"] == true;
                state.seen.push_back(etag.to_string());
                state.synced = std::fs::metadata(snapshot_path(home))
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|at| at.elapsed().ok())
                    .and_then(|age| Instant::now().checked_sub(age));
            }
        }
        Arc::new(Mirror {
            home: home.to_path_buf(),
            state: Mutex::new(state),
            moved: Condvar::new(),
            fetching: Mutex::new(()),
        })
    }

    /// The socket opened: answer from what the mirror holds while the caller refetches.
    pub fn connected(&self) {
        let mut s = self.state.lock().unwrap();
        s.live = s.doc.is_some();
    }

    /// The socket closed: answer nothing until the next connect.
    pub fn closed(&self) {
        let mut s = self.state.lock().unwrap();
        s.live = false;
        s.closes += 1;
    }

    /// True when the mirror has gone REFRESH without a fetch or applied frame.
    pub fn stale(&self) -> bool {
        self.state
            .lock()
            .unwrap()
            .synced
            .is_none_or(|at| at.elapsed() >= REFRESH)
    }

    /// The board's version and job rows, when the mirror holds a version other than `seen`.
    pub fn jobs_since(&self, seen: &str) -> Option<(String, Vec<Value>)> {
        let s = self.state.lock().unwrap();
        if s.etag == seen {
            return None;
        }
        let jobs = s.doc.as_ref()?["jobs"].as_array()?.clone();
        Some((s.etag.clone(), jobs))
    }

    /// Fetches the whole board. Returns how many jobs it holds, or the failure.
    pub fn fetch(&self) -> Result<usize, String> {
        let closes = self.state.lock().unwrap().closes;
        self.fetch_since(closes)
    }

    /// `fetch` for a caller that read `closes` before it decided to fetch. A close since then
    /// leaves the mirror answering nothing, though the fetched board is kept.
    fn fetch_since(&self, closes: u64) -> Result<usize, String> {
        let _turn = self.fetching.lock().unwrap();
        let r = api(&self.home, "GET", "/api/board", None, &[200, 404], &[])
            .map_err(|e| e.stderr.trim_end().to_string())?;
        if r.status == 404 {
            self.state.lock().unwrap().live = false;
            return Err("no board yet".to_string());
        }
        let etag = r.header("ETag").ok_or("the Worker sent no ETag")?;
        let etag = etag.strip_prefix("W/").unwrap_or(etag).to_string();
        let text = r.text().map_err(|e| e.stderr.trim_end().to_string())?;
        let doc = loads(&text).map_err(|e| format!("the board is not valid JSON: {e}"))?;
        let jobs = doc["jobs"].as_array().map_or(0, Vec::len);
        let mut s = self.state.lock().unwrap();
        // After a close since this fetch began, whatever the socket's own close and connect set
        // stands: not live, or live again on a new socket.
        let was_live = s.live;
        // A frame applied while this fetch ran already moved past the version it read.
        if s.etag != etag && s.seen.contains(&etag) {
            s.synced = Some(Instant::now());
        } else {
            s.patch = r.header("Accept-Patch").is_some();
            s.text = Arc::new(dumps(&doc, COMPACT));
            s.doc = Some(doc);
            self.moved_to(&mut s, etag);
        }
        s.live = s.closes == closes || was_live;
        Ok(jobs)
    }

    /// Applies a rows frame. False when it doesn't follow the mirror's version, so the caller
    /// refetches. A new row goes at the end, where the Worker puts it: the Worker sends `changed`
    /// instead of rows for a write that inserts or reorders rows anywhere else.
    pub fn apply(&self, frame: &Value) -> bool {
        let (Some(etag), Some(prev)) = (frame["etag"].as_str(), frame["prev"].as_str()) else {
            return false;
        };
        let mut s = self.state.lock().unwrap();
        self.apply_locked(&mut s, frame, etag, prev)
    }

    /// `apply` with the state already locked.
    fn apply_locked(&self, s: &mut State, frame: &Value, etag: &str, prev: &str) -> bool {
        if s.seen.iter().any(|e| e == etag) {
            return true;
        }
        if s.etag != prev {
            return false;
        }
        let Some(doc) = s.doc.as_mut() else {
            return false;
        };
        let Some(jobs) = doc["jobs"].as_array_mut() else {
            return false;
        };
        let mut at: HashMap<String, usize> = jobs
            .iter()
            .enumerate()
            .filter_map(|(i, j)| Some((j.get("id")?.as_str()?.to_string(), i)))
            .collect();
        for row in frame["rows"].as_array().into_iter().flatten() {
            let Some(id) = row.get("id").and_then(Value::as_str) else {
                continue;
            };
            match at.get(id) {
                Some(&i) => jobs[i] = row.clone(),
                None => {
                    at.insert(id.to_string(), jobs.len());
                    jobs.push(row.clone());
                }
            }
        }
        if let Some(removed) = frame["removed"].as_array() {
            jobs.retain(|j| !removed.contains(j.get("id").unwrap_or(&Value::Null)));
        }
        if let Some(updated) = frame.get("updated") {
            doc["updated"] = updated.clone();
        }
        s.text = Arc::new(dumps(s.doc.as_ref().unwrap(), COMPACT));
        self.moved_to(s, etag.to_string());
        true
    }

    /// Records the new version, saves the snapshot and wakes anyone waiting for it.
    fn moved_to(&self, s: &mut State, etag: String) {
        s.etag = etag.clone();
        s.live = true;
        s.synced = Some(Instant::now());
        s.seen.push_back(etag);
        if s.seen.len() > SEEN {
            s.seen.pop_front();
        }
        let snap = format!(
            "{{\"etag\":{},\"patch\":{},\"board\":{}}}\n",
            dumps(&Value::from(s.etag.as_str()), COMPACT),
            s.patch,
            s.text
        );
        let path = snapshot_path(&self.home);
        let _ = std::fs::create_dir_all(path.parent().unwrap())
            .and_then(|_| write_atomic(&path, snap.as_bytes()));
        self.moved.notify_all();
    }

    /// Waits until the mirror has held etag, or refetches after OWN_FRAME. A mirror that is not
    /// live answers nothing, so it doesn't wait.
    fn wait_for(&self, etag: &str) {
        let end = Instant::now() + OWN_FRAME;
        let mut s = self.state.lock().unwrap();
        while s.live && !s.seen.iter().any(|e| e == etag) {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                let closes = s.closes;
                drop(s);
                let _ = self.fetch_since(closes);
                return;
            }
            s = self.moved.wait_timeout(s, left).unwrap().0;
        }
    }

    /// The board reply: its header and text, filtered when the request names a job or project.
    fn board(&self, request: &Value) -> Option<(Value, Arc<String>)> {
        let s = self.state.lock().unwrap();
        if !s.live || s.synced.is_none_or(|at| at.elapsed() >= MAX_AGE) {
            return None;
        }
        let head = json!({"v": PROTOCOL, "ok": true, "etag": s.etag, "patch": s.patch});
        let filter: Map<String, Value> = ["job", "project"]
            .iter()
            .filter_map(|k| Some((k.to_string(), request.get(*k)?.clone())))
            .collect();
        if filter.is_empty() {
            return Some((head, s.text.clone()));
        }
        let text = dumps(&view(s.doc.as_ref()?, &filter), COMPACT);
        Some((head, Arc::new(text)))
    }

    /// Forwards a row PATCH to the Worker and returns its answer unchanged. A committed one goes
    /// into the mirror at once when the Worker names the version it applied to and the mirror
    /// holds that version, else the daemon waits for its frame. A refused one never touches it.
    fn patch(&self, body: &Value, text: &str) -> Value {
        let sent = api(
            &self.home,
            "PATCH",
            "/api/board",
            Some(text.as_bytes()),
            &[200, 409],
            &[("Content-Type", "application/json")],
        );
        match sent {
            Ok(r) if r.status == 200 => {
                if let Some(etag) = r.header("ETag") {
                    let etag = etag.strip_prefix("W/").unwrap_or(etag);
                    let applied = r
                        .header("X-Board-Prev")
                        .is_some_and(|prev| self.apply_own(body, etag, prev));
                    if !applied {
                        self.wait_for(etag);
                    }
                }
                json!({"v": PROTOCOL, "ok": true, "status": 200})
            }
            Ok(r) => json!({"v": PROTOCOL, "ok": true, "status": r.status}),
            Err(e) => json!({"v": PROTOCOL, "ok": true, "code": e.code, "stderr": e.stderr,
                             "status": e.status.map(|(code, detail)| json!([code, detail]))}),
        }
    }

    /// Applies a committed row PATCH as the rows frame the Worker announces for it: its written
    /// rows, its removed ids, and `updated` moved forward only when the PATCH's is later. False
    /// when the mirror is not live or does not hold `prev`, the version the Worker applied it to.
    fn apply_own(&self, body: &Value, etag: &str, prev: &str) -> bool {
        let mut s = self.state.lock().unwrap();
        // A mirror that is not live has stopped hearing frames, so a write must not revive it.
        if !s.live || s.etag != prev {
            return false;
        }
        let Some(doc) = s.doc.as_ref() else {
            return false;
        };
        let was = doc["updated"].as_str().unwrap_or("");
        let updated = match body["updated"].as_str() {
            Some(at) if at > was => Value::from(at),
            _ => doc.get("updated").cloned().unwrap_or(Value::Null),
        };
        let entries = body["rows"].as_array().into_iter().flatten();
        let (rows, removed): (Vec<&Value>, Vec<&Value>) =
            entries.partition(|e| !e["row"].is_null());
        let frame = json!({
            "etag": etag,
            "prev": prev,
            "updated": updated,
            "rows": rows.iter().map(|e| e["row"].clone()).collect::<Vec<_>>(),
            "removed": removed.iter().map(|e| e["id"].clone()).collect::<Vec<_>>(),
        });
        self.apply_locked(&mut s, &frame, etag, prev)
    }

    /// Answers one request on stream. With a token, as on the loopback endpoint, the first line
    /// must be a `hello` whose `auth` proves the client holds it; the reply proves the daemon does.
    pub fn handle(&self, stream: Box<dyn Stream>, token: Option<&str>) {
        let _ = stream.limit(Duration::from_secs(60));
        let mut reader = BufReader::new(stream);
        let refused = |error: &str| json!({"v": PROTOCOL, "ok": false, "error": error});
        let mut line = String::new();
        if let Some(token) = token {
            if (&mut reader).take(4096).read_line(&mut line).is_err() {
                return;
            }
            let hello: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            let nonce = hello["nonce"].as_str().unwrap_or_default();
            let auth = hello["auth"].as_str().unwrap_or_default();
            if hello["op"] != "hello"
                || nonce.is_empty()
                || !same_secret(auth, &proof(token, "client", nonce))
            {
                let _ = reader
                    .get_mut()
                    .write_all(format!("{}\n", refused("bad token")).as_bytes());
                return;
            }
            let reply = json!({"v": PROTOCOL, "ok": true, "proof": proof(token, "daemon", nonce)});
            if reader
                .get_mut()
                .write_all(format!("{reply}\n").as_bytes())
                .is_err()
            {
                return;
            }
            line.clear();
        }
        if (&mut reader)
            .take(MAX_REQUEST)
            .read_line(&mut line)
            .is_err()
        {
            return;
        }
        let request: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let reply = if request["v"] != PROTOCOL {
            refused("unknown protocol version")
        } else {
            match request["op"].as_str() {
                Some("board") => match self.board(&request) {
                    Some((head, text)) => {
                        let stream = reader.get_mut();
                        let _ = stream
                            .write_all(format!("{head}\n").as_bytes())
                            .and_then(|_| stream.write_all(text.as_bytes()))
                            .and_then(|_| stream.write_all(b"\n"));
                        return;
                    }
                    None => refused("the mirror is not live"),
                },
                Some("patch") => match request["body"].as_str().and_then(row_patch_body) {
                    Some((body, text)) => self.patch(&body, text),
                    None => refused("patch needs a board row PATCH body"),
                },
                Some("await") => {
                    if let Some(etag) = request["etag"].as_str() {
                        self.wait_for(etag);
                    }
                    json!({"v": PROTOCOL, "ok": true})
                }
                Some("gh" | "gh-wait") => crate::commands::ghpoll::answer(&request),
                _ => refused("unknown op"),
            }
        };
        let _ = reader.get_mut().write_all(format!("{reply}\n").as_bytes());
    }
}

/// The PATCH body a client may send through the daemon, parsed: a board row PATCH of at most
/// MAX_PATCH_BYTES, `{"rows": [{id, expect, row}], "reads"?: [...], "updated"?: "..."}`, where
/// each expect and row is null or an object with the entry's id. The Worker checks the rest.
fn row_patch_body(text: &str) -> Option<(Value, &str)> {
    if text.len() > MAX_PATCH_BYTES {
        return None;
    }
    let body: Value = serde_json::from_str(text).ok()?;
    let doc = body.as_object()?;
    if doc
        .keys()
        .any(|k| !["rows", "reads", "updated"].contains(&k.as_str()))
        || !doc.get("reads").is_none_or(Value::is_array)
        || !doc.get("updated").is_none_or(Value::is_string)
    {
        return None;
    }
    let rows = doc.get("rows")?.as_array()?;
    let row_of = |v: &Value, id: &Value| v.is_null() || (v.is_object() && v.get("id") == Some(id));
    let entry = |e: &Value| {
        let e = e.as_object()?;
        let id = e.get("id").filter(|id| id.is_string())?;
        let (expect, row) = (e.get("expect")?, e.get("row")?);
        (e.len() == 3
            && row_of(expect, id)
            && row_of(row, id)
            && !(expect.is_null() && row.is_null()))
        .then_some(())
    };
    if rows.is_empty() || rows.len() > MAX_PATCH_ROWS || !rows.iter().all(|e| entry(e).is_some()) {
        return None;
    }
    Some((body, text))
}

/// Serves the loopback endpoint and writes `<address> <token>` to `daemon.addr`, 0600.
#[cfg_attr(unix, allow(dead_code))]
pub fn listen_tcp(mirror: Arc<Mirror>) -> io::Result<()> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let secret = random_hex(16);
    write_atomic(
        &addr_path(&mirror.home),
        format!("{} {secret}\n", listener.local_addr()?).as_bytes(),
    )?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (mirror, secret) = (mirror.clone(), secret.clone());
            std::thread::spawn(move || mirror.handle(Box::new(stream), Some(&secret)));
        }
    });
    Ok(())
}

/// Serves `<home>/daemon.sock`, 0600. The daemon holds its lock, so any socket file there is stale.
/// The socket is bound inside a new 0700 folder and made 0600 before it moves into place, so no
/// other user can connect to it at any moment.
#[cfg(unix)]
pub fn listen(mirror: Arc<Mirror>) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let path = socket_path(&mirror.home);
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let private = mirror.home.join(format!(".sock-{}", random_hex(3)));
    std::fs::DirBuilder::new().mode(0o700).create(&private)?;
    let bound = private.join("s");
    let listener = std::os::unix::net::UnixListener::bind(&bound).and_then(|listener| {
        std::fs::set_permissions(&bound, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&bound, &path)?;
        Ok(listener)
    });
    let _ = std::fs::remove_file(&bound);
    let _ = std::fs::remove_dir(&private);
    let listener = listener?;
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mirror = mirror.clone();
            std::thread::spawn(move || mirror.handle(Box::new(stream), None));
        }
    });
    Ok(())
}

#[cfg(not(unix))]
pub fn listen(mirror: Arc<Mirror>) -> io::Result<()> {
    listen_tcp(mirror)
}

/// Removes the endpoint file the daemon wrote.
pub fn unlisten(home: &Path) {
    let _ = std::fs::remove_file(if cfg!(unix) {
        socket_path(home)
    } else {
        addr_path(home)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::board_client;
    use crate::shared::http::tests::{home_for, serve};

    const E1: &str = "\"e1\"";

    fn board_json() -> &'static str {
        r#"{"updated":"2026-11-23T10:00:00Z","jobs":[{"id":"1999-synthetic-build","stage":"build"},{"id":"2000-synthetic-spec","stage":"spec"}]}"#
    }

    /// A mirror whose home points at a fake Worker that answers the board GET, then each extra reply.
    fn fetched(
        name: &str,
        extra: Vec<(u16, &'static str, Vec<(&'static str, &'static str)>)>,
    ) -> (
        Arc<Mirror>,
        Arc<std::sync::Mutex<Vec<crate::shared::http::tests::Seen>>>,
    ) {
        let mut replies = vec![(
            200,
            board_json(),
            vec![("ETag", E1), ("Accept-Patch", "application/json")],
        )];
        replies.extend(extra);
        let (origin, seen) = serve(replies);
        let home = home_for(&format!("mirror-{name}"), &origin);
        let mirror = Mirror::new(&home);
        mirror.connected();
        assert_eq!(mirror.fetch(), Ok(2));
        (mirror, seen)
    }

    fn frame(etag: &str, prev: &str, rows: Value, removed: Value) -> Value {
        json!({"type": "rows", "etag": etag, "prev": prev, "updated": "2026-11-23T10:00:05Z", "rows": rows, "removed": removed})
    }

    fn jobs(m: &Mirror) -> Value {
        m.state.lock().unwrap().doc.as_ref().unwrap()["jobs"].clone()
    }

    #[test]
    fn frames_apply_in_order_and_a_gap_asks_for_a_refetch() {
        let (m, _) = fetched("frames", vec![]);
        let changed = json!({"id": "1999-synthetic-build", "stage": "review"});
        let added = json!({"id": "2001-add-csv-export", "stage": "idea"});
        assert!(m.apply(&frame(
            "\"e2\"",
            E1,
            json!([changed, added]),
            json!(["2000-synthetic-spec"])
        )));
        assert_eq!(jobs(&m), json!([changed, added]));
        assert_eq!(
            m.state.lock().unwrap().doc.as_ref().unwrap()["updated"],
            "2026-11-23T10:00:05Z"
        );
        // A frame the mirror already has is skipped; one built on another version is a gap.
        assert!(m.apply(&frame("\"e2\"", E1, json!([]), json!([]))));
        assert!(!m.apply(&frame("\"e4\"", "\"e3\"", json!([]), json!([]))));
        assert!(!m.apply(&json!({"type": "rows"})));
        assert_eq!(m.state.lock().unwrap().etag, "\"e2\"");
    }

    #[test]
    fn snapshot_is_private_and_seeds_a_restarted_mirror() {
        let (m, _) = fetched("snapshot", vec![]);
        let path = snapshot_path(&m.home);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let again = Mirror::new(&m.home);
        // Seeded, but silent until the socket opens.
        assert!(again.board(&json!({})).is_none());
        again.connected();
        let (head, text) = again.board(&json!({})).unwrap();
        assert_eq!(head["etag"], E1);
        assert_eq!(head["patch"], true);
        assert_eq!(loads(&text).unwrap(), loads(board_json()).unwrap());
        again.closed();
        assert!(again.board(&json!({})).is_none());
    }

    #[test]
    fn a_fetch_older_than_an_applied_frame_is_dropped() {
        let (m, _) = fetched("old-fetch", vec![]);
        let home = m.home.clone();
        assert!(m.apply(&frame("\"e2\"", E1, json!([{"id": "2001-x"}]), json!([]))));
        // The Worker still serves e1, say from a read that began before the frame's write.
        let (origin, _) = serve(vec![(200, board_json(), vec![("ETag", E1)])]);
        std::fs::write(
            home.join("config.md"),
            format!("---\napi_url: {origin}\n---\n"),
        )
        .unwrap();
        assert_eq!(m.fetch(), Ok(2));
        assert_eq!(m.state.lock().unwrap().etag, "\"e2\"");
    }

    /// A command's read, view and PATCH through the endpoint, and its fallback once the
    /// daemon stops answering.
    fn round_trip(
        m: Arc<Mirror>,
        seen: Arc<std::sync::Mutex<Vec<crate::shared::http::tests::Seen>>>,
    ) {
        let home = m.home.clone();
        let read = board_client::read(&home).unwrap();
        assert!(read.mirror && read.patch);
        assert_eq!(read.etag.as_deref(), Some(E1));
        assert_eq!(
            loads(read.text.as_deref().unwrap()).unwrap(),
            loads(board_json()).unwrap()
        );
        let one = Map::from_iter([("job".to_string(), json!("2000-synthetic-spec"))]);
        let text = board_client::read_view(&home, one).unwrap().unwrap();
        assert_eq!(
            loads(&text).unwrap()["jobs"],
            json!([{"id": "2000-synthetic-spec", "stage": "spec"}])
        );
        // A write goes as a PATCH through the daemon, which then waits for its own frame.
        let waiter = m.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            waiter.apply(&frame(
                "\"e2\"",
                E1,
                json!([{"id": "2000-synthetic-spec", "stage": "build"}]),
                json!([]),
            ));
        });
        let got = board_client::cas(&home, |text| {
            let mut b = loads(text.unwrap()).unwrap();
            b["jobs"][1]["stage"] = "build".into();
            b["updated"] = "2026-11-23T10:00:05Z".into();
            Ok((Some(dumps(&b, COMPACT)), 7))
        })
        .unwrap();
        assert_eq!(got, 7);
        assert_eq!(m.state.lock().unwrap().etag, "\"e2\"");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "one GET at connect and one PATCH");
        assert_eq!(seen[1].method, "PATCH");
        assert_eq!(seen[1].header("Content-Type"), Some("application/json"));
        let body: Value = serde_json::from_slice(&seen[1].body).unwrap();
        assert_eq!(
            body,
            json!({"rows": [{"id": "2000-synthetic-spec", "expect": {"id": "2000-synthetic-spec", "stage": "spec"},
                             "row": {"id": "2000-synthetic-spec", "stage": "build"}}],
                   "updated": "2026-11-23T10:00:05Z"})
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_serves_reads_and_writes_and_falls_back_when_gone() {
        use std::os::unix::fs::PermissionsExt;
        let (m, seen) = fetched("unix", vec![(200, "ok", vec![("ETag", "\"e2\"")])]);
        let home = m.home.clone();
        listen(m.clone()).unwrap();
        let mode = std::fs::metadata(socket_path(&home))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // The private folder the socket was bound in is gone.
        let names: Vec<String> = std::fs::read_dir(&home)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(!names.iter().any(|n| n.starts_with(".sock-")), "{names:?}");
        round_trip(m.clone(), seen);
        // Down socket: the mirror answers nothing and the command reads the Worker.
        m.closed();
        let (origin, _) = serve(vec![(200, board_json(), vec![("ETag", "\"e9\"")])]);
        std::fs::write(
            home.join("config.md"),
            format!("---\napi_url: {origin}\n---\n"),
        )
        .unwrap();
        let read = board_client::read(&home).unwrap();
        assert!(!read.mirror && !read.patch);
        assert_eq!(read.etag.as_deref(), Some("\"e9\""));
        unlisten(&home);
        assert!(!socket_path(&home).exists());
    }

    /// The Windows endpoint: loopback TCP with a token from a 0600 file. Each side proves it
    /// holds the token without sending it, so a wrong proof gets nothing, and a stale file that
    /// points at another process's port is not trusted.
    #[test]
    fn loopback_endpoint_needs_the_token() {
        let (m, _) = fetched("tcp", vec![]);
        listen_tcp(m.clone()).unwrap();
        let (addr, secret) = board_client::tcp_endpoint(&m.home).unwrap();
        assert_eq!(secret.len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(addr_path(&m.home))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let mut stream = std::net::TcpStream::connect(&addr).unwrap();
        let hello =
            json!({"v": 1, "op": "hello", "nonce": "n1", "auth": proof("wrong", "client", "n1")});
        stream.write_all(format!("{hello}\n").as_bytes()).unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&reply).unwrap()["error"],
            "bad token"
        );
        let mut reader = board_client::connect_tcp(&m.home).unwrap();
        reader
            .get_mut()
            .write_all(b"{\"v\":1,\"op\":\"board\"}\n")
            .unwrap();
        let mut head = String::new();
        reader.read_line(&mut head).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&head).unwrap()["ok"], true);

        // Another process now holds the port the file names: it never sees the token, and its
        // made-up proof is refused.
        let impostor = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = impostor.local_addr().unwrap();
        let heard = std::thread::spawn(move || {
            let (stream, _) = impostor.accept().unwrap();
            let mut line = String::new();
            let mut reader = BufReader::new(stream);
            reader.read_line(&mut line).unwrap();
            let _ = reader
                .get_mut()
                .write_all(b"{\"v\":1,\"ok\":true,\"proof\":\"made-up\"}\n");
            line
        });
        std::fs::write(addr_path(&m.home), format!("{port} {secret}\n")).unwrap();
        assert!(board_client::connect_tcp(&m.home).is_none());
        assert!(!heard.join().unwrap().contains(&secret));
    }

    #[test]
    fn unknown_version_and_op_are_refused() {
        let (m, _) = fetched("refused", vec![]);
        listen_tcp(m.clone()).unwrap();
        for (request, error) in [
            (json!({"v": 2, "op": "board"}), "unknown protocol version"),
            (json!({"v": 1, "op": "drop"}), "unknown op"),
        ] {
            let mut reader = board_client::connect_tcp(&m.home).unwrap();
            reader
                .get_mut()
                .write_all(format!("{request}\n").as_bytes())
                .unwrap();
            let mut reply = String::new();
            reader.read_line(&mut reply).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&reply).unwrap()["error"],
                error
            );
        }
    }

    /// A mirror with no fetch or frame for REFRESH asks for a refetch, and one past MAX_AGE, such
    /// as an old snapshot whose first fetch keeps failing, answers nothing.
    #[test]
    fn an_old_mirror_refetches_and_then_goes_quiet() {
        let (m, _) = fetched("age", vec![]);
        assert!(!m.stale());
        assert!(m.board(&json!({})).is_some());
        m.state.lock().unwrap().synced = Instant::now().checked_sub(REFRESH);
        assert!(m.stale());
        assert!(m.board(&json!({})).is_some());
        m.state.lock().unwrap().synced = Instant::now().checked_sub(MAX_AGE);
        assert!(m.board(&json!({})).is_none());
        // A restart seeds from the snapshot's save time.
        let again = Mirror::new(&m.home);
        again.connected();
        assert!(!again.stale() && again.board(&json!({})).is_some());
    }

    /// A PATCH through the daemon that gets 409 makes the client read the Worker directly and
    /// write there, since the mirror may not have the other write yet.
    #[cfg(unix)]
    #[test]
    fn a_conflict_through_the_daemon_retries_on_the_worker() {
        let later = r#"{"updated":"2026-11-23T10:00:01Z","jobs":[{"id":"1999-synthetic-build","stage":"review"},{"id":"2000-synthetic-spec","stage":"spec"}]}"#;
        let (m, seen) = fetched(
            "conflict",
            vec![
                (409, r#"{"error":"conflict"}"#, vec![]),
                (
                    200,
                    later,
                    vec![("ETag", "\"e2\""), ("Accept-Patch", "application/json")],
                ),
                (200, "ok", vec![("ETag", "\"e3\"")]),
            ],
        );
        listen(m.clone()).unwrap();
        let mut stages = Vec::new();
        board_client::cas(&m.home, |text| {
            let mut b = loads(text.unwrap()).unwrap();
            stages.push(b["jobs"][0]["stage"].clone());
            b["jobs"][1]["stage"] = "build".into();
            Ok((Some(dumps(&b, COMPACT)), ()))
        })
        .unwrap();
        assert_eq!(stages, ["build", "review"]);
        let methods: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.method.clone())
            .collect();
        assert_eq!(methods, ["GET", "PATCH", "GET", "PATCH"]);
        unlisten(&m.home);
    }

    /// Read-your-writes: a write through the daemon goes into its mirror as soon as the Worker
    /// commits it and names the version it applied to, so an immediate `factory job` read on this
    /// host sees it, with no wait for the frame and no refetch.
    #[cfg(unix)]
    #[test]
    fn a_write_through_the_daemon_is_read_back_at_once() {
        let (m, seen) = fetched(
            "ryw",
            vec![(200, "ok", vec![("ETag", "\"e2\""), ("X-Board-Prev", E1)])],
        );
        let home = m.home.clone();
        listen(m.clone()).unwrap();
        let started = Instant::now();
        board_client::cas(&home, |text| {
            let mut b = loads(text.unwrap()).unwrap();
            b["jobs"][1]["stage"] = "build".into();
            b["jobs"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id": "2001-add-csv-export", "stage": "idea"}));
            b["updated"] = "2026-11-23T10:00:05Z".into();
            Ok((Some(dumps(&b, COMPACT)), ()))
        })
        .unwrap();
        assert!(started.elapsed() < OWN_FRAME, "no wait for the frame");
        let row = crate::commands::lookup::job_row(&home, "2000-synthetic-spec").unwrap();
        assert_eq!(row["stage"], "build");
        let added = crate::commands::lookup::job_row(&home, "2001-add-csv-export").unwrap();
        assert_eq!(added["stage"], "idea");
        {
            let s = m.state.lock().unwrap();
            assert_eq!(s.etag, "\"e2\"");
            assert_eq!(s.doc.as_ref().unwrap()["updated"], "2026-11-23T10:00:05Z");
        }
        let methods: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.method.clone())
            .collect();
        assert_eq!(methods, ["GET", "PATCH"]);
        // The Worker's own frame for the write, arriving later, changes nothing.
        assert!(m.apply(&frame(
            "\"e2\"",
            E1,
            json!([{"id": "2000-synthetic-spec", "stage": "build"}]),
            json!([])
        )));
        assert_eq!(jobs(&m).as_array().unwrap().len(), 3);
        unlisten(&home);
    }

    /// The Worker's refusal or failure comes back unchanged and leaves the mirror alone, and a
    /// commit built on a version the mirror doesn't hold isn't applied over it.
    #[test]
    fn a_refused_patch_never_touches_the_mirror() {
        let (m, _) = fetched(
            "refused-patch",
            vec![
                (409, r#"{"error":"conflict"}"#, vec![]),
                (503, "error code: 1102", vec![]),
            ],
        );
        let text = r#"{"rows":[{"id":"2000-synthetic-spec","expect":{"id":"2000-synthetic-spec","stage":"spec"},"row":{"id":"2000-synthetic-spec","stage":"build"}}]}"#;
        let (body, text) = row_patch_body(text).unwrap();
        let before = jobs(&m);
        assert_eq!(m.patch(&body, text)["status"], 409);
        let busy = m.patch(&body, text);
        assert_eq!(busy["status"][0], 503);
        assert_eq!(busy["code"], 5);
        assert_eq!(jobs(&m), before);
        assert!(!m.apply_own(&body, "\"e3\"", "\"e2\""));
        assert_eq!(jobs(&m), before);
        assert_eq!(m.state.lock().unwrap().etag, E1);
    }

    /// Only a board row PATCH body goes through the daemon. Anything else is refused before it
    /// reaches the Worker, and the client then writes straight to the Worker.
    #[test]
    fn the_daemon_forwards_only_row_patch_bodies() {
        let (m, seen) = fetched("patch-shape", vec![]);
        listen_tcp(m.clone()).unwrap();
        let row = r#"{"id":"2001-x"}"#;
        let big = format!(
            r#"{{"rows":[{{"id":"2001-x","expect":null,"row":{{"id":"2001-x","note":"{}"}}}}]}}"#,
            "x".repeat(MAX_PATCH_BYTES)
        );
        let many = format!(
            r#"{{"rows":[{}]}}"#,
            (0..=MAX_PATCH_ROWS)
                .map(|n| format!(r#"{{"id":"{n}","expect":null,"row":{{"id":"{n}"}}}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        for body in [
            "not json".to_string(),
            "[]".to_string(),
            r#"{"rows":[]}"#.to_string(),
            r#"{"jobs":[],"updated":"x"}"#.to_string(),
            format!(r#"{{"rows":[{{"id":"2001-x","expect":null,"row":{row}}}],"autonomy":{{}}}}"#),
            format!(r#"{{"rows":[{{"id":"2001-x","expect":null,"row":{row},"url":"/x"}}]}}"#),
            r#"{"rows":[{"id":"2001-x","expect":null,"row":{"id":"2001-y"}}]}"#.to_string(),
            r#"{"rows":[{"id":"2001-x","expect":null,"row":null}]}"#.to_string(),
            r#"{"rows":[{"id":7,"expect":null,"row":{"id":7}}]}"#.to_string(),
            format!(r#"{{"rows":[{{"id":"2001-x","expect":null,"row":{row}}}],"updated":1}}"#),
            format!(r#"{{"rows":[{{"id":"2001-x","expect":null,"row":{row}}}],"reads":{{}}}}"#),
            big,
            many,
        ] {
            let mut reader = board_client::connect_tcp(&m.home).unwrap();
            let request = json!({"v": 1, "op": "patch", "body": body});
            reader
                .get_mut()
                .write_all(format!("{request}\n").as_bytes())
                .unwrap();
            let mut reply = String::new();
            reader.read_line(&mut reply).unwrap();
            let reply: Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(reply["ok"], false, "{}", &body[..body.len().min(80)]);
        }
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "only the connect GET reached the Worker"
        );
        let good = format!(
            r#"{{"rows":[{{"id":"2001-x","expect":null,"row":{row}}}],"reads":[{{"id":"1999-synthetic-build","stage":"build"}}],"updated":"2026-11-23T10:00:05Z"}}"#
        );
        assert!(row_patch_body(&good).is_some());
    }

    /// A close that lands while a fetch is running, as when a write's refetch races the socket
    /// going down, leaves the mirror answering nothing: the fetch that began before it can't make
    /// the mirror live again.
    #[test]
    fn a_close_during_a_fetch_keeps_the_mirror_closed() {
        use crate::shared::http::tests::serve_hooked;
        let (m, _) = fetched("close-mid-fetch", vec![]);
        let closer = m.clone();
        let (origin, _) = serve_hooked(
            vec![(200, board_json(), vec![("ETag", "\"e2\"")])],
            move |_| closer.closed(),
        );
        std::fs::write(
            m.home.join("config.md"),
            format!("---\napi_url: {origin}\n---\n"),
        )
        .unwrap();
        assert_eq!(m.fetch(), Ok(2));
        assert!(m.board(&json!({})).is_none());
        assert_eq!(m.state.lock().unwrap().etag, "\"e2\"");
    }

    /// A fetch that spans a close and a reconnect leaves the reconnected mirror live: the new
    /// socket's connect, not the old fetch, decides.
    #[test]
    fn a_fetch_across_a_reconnect_keeps_the_mirror_live() {
        use crate::shared::http::tests::serve_hooked;
        let (m, _) = fetched("reconnect-mid-fetch", vec![]);
        let socket = m.clone();
        let (origin, _) = serve_hooked(
            vec![(200, board_json(), vec![("ETag", "\"e2\"")])],
            move |_| {
                socket.closed();
                socket.connected();
            },
        );
        std::fs::write(
            m.home.join("config.md"),
            format!("---\napi_url: {origin}\n---\n"),
        )
        .unwrap();
        assert_eq!(m.fetch(), Ok(2));
        assert!(m.board(&json!({})).is_some());
    }

    /// A write sent while the Worker socket is down doesn't revive the mirror: a mirror that is
    /// not live hears no frames, so it must keep answering nothing until the next connect.
    #[test]
    fn a_write_does_not_revive_a_closed_mirror() {
        let (m, _) = fetched(
            "closed-write",
            vec![(200, "ok", vec![("ETag", "\"e2\""), ("X-Board-Prev", E1)])],
        );
        m.closed();
        let text = r#"{"rows":[{"id":"2000-synthetic-spec","expect":{"id":"2000-synthetic-spec","stage":"spec"},"row":{"id":"2000-synthetic-spec","stage":"build"}}]}"#;
        let (body, text) = row_patch_body(text).unwrap();
        let started = Instant::now();
        assert_eq!(m.patch(&body, text)["status"], 200);
        assert!(
            started.elapsed() < OWN_FRAME,
            "no wait on a mirror that is not live"
        );
        assert!(m.board(&json!({})).is_none());
        assert_eq!(m.state.lock().unwrap().etag, E1);
    }
}
