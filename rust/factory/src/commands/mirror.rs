//! The daemon's board mirror and the local endpoint commands read and write it through.
//!
//! The daemon fetches the whole board once per connect, then applies each rows frame whose `prev`
//! is the mirror's ETag. A frame that doesn't follow, a `changed` frame for the board, or a
//! reconnect refetches it. A private snapshot at `<home>/cache/board.json` lets a restarted daemon
//! answer from the last board while its first fetch runs. While the socket is down the mirror
//! answers nothing, so commands read the Worker.

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::shared::board_client::{addr_path, socket_path, view, Stream, PROTOCOL};
use crate::shared::fs::write_atomic;
use crate::shared::http::api;
use crate::shared::pyjson::{dumps, loads, COMPACT};

/// How many recent ETags the mirror remembers, to tell an old frame or fetch from a new one.
const SEEN: usize = 32;
/// How long a write waits for its own frame before the daemon refetches.
const OWN_FRAME: Duration = Duration::from_secs(2);
/// The longest request line: a 1 MiB PATCH body, escaped.
const MAX_REQUEST: u64 = 4 * 1024 * 1024;

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
        self.state.lock().unwrap().live = false;
    }

    /// Fetches the whole board. Returns how many jobs it holds, or the failure.
    pub fn fetch(&self) -> Result<usize, String> {
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
        // A frame applied while this fetch ran already moved past the version it read.
        if s.etag != etag && s.seen.contains(&etag) {
            s.live = true;
            return Ok(jobs);
        }
        s.patch = r.header("Accept-Patch").is_some();
        s.text = Arc::new(dumps(&doc, COMPACT));
        s.doc = Some(doc);
        self.moved_to(&mut s, etag);
        Ok(jobs)
    }

    /// Applies a rows frame. False when it doesn't follow the mirror's version, so the caller
    /// refetches.
    pub fn apply(&self, frame: &Value) -> bool {
        let (Some(etag), Some(prev)) = (frame["etag"].as_str(), frame["prev"].as_str()) else {
            return false;
        };
        let mut s = self.state.lock().unwrap();
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
        self.moved_to(&mut s, etag.to_string());
        true
    }

    /// Records the new version, saves the snapshot and wakes anyone waiting for it.
    fn moved_to(&self, s: &mut State, etag: String) {
        s.etag = etag.clone();
        s.live = true;
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
                drop(s);
                let _ = self.fetch();
                return;
            }
            s = self.moved.wait_timeout(s, left).unwrap().0;
        }
    }

    /// The board reply: its header and text, filtered when the request names a job or project.
    fn board(&self, request: &Value) -> Option<(Value, Arc<String>)> {
        let s = self.state.lock().unwrap();
        if !s.live {
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

    /// Forwards a row PATCH to the Worker. A committed one waits until the mirror has it.
    fn patch(&self, body: &str) -> Value {
        let sent = api(
            &self.home,
            "PATCH",
            "/api/board",
            Some(body.as_bytes()),
            &[200, 409],
            &[("Content-Type", "application/json")],
        );
        match sent {
            Ok(r) if r.status == 200 => {
                if let Some(etag) = r.header("ETag") {
                    self.wait_for(etag.strip_prefix("W/").unwrap_or(etag));
                }
                json!({"v": PROTOCOL, "ok": true, "status": 200})
            }
            Ok(r) => json!({"v": PROTOCOL, "ok": true, "status": r.status}),
            Err(e) => json!({"v": PROTOCOL, "ok": true, "code": e.code, "stderr": e.stderr,
                             "status": e.status.map(|(code, detail)| json!([code, detail]))}),
        }
    }

    /// Answers one request on stream. token is the secret a loopback request must carry.
    pub fn handle(&self, stream: Box<dyn Stream>, token: Option<&str>) {
        let _ = stream.limit(Duration::from_secs(60));
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if (&mut reader)
            .take(MAX_REQUEST)
            .read_line(&mut line)
            .is_err()
        {
            return;
        }
        let request: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        let refused = |error: &str| json!({"v": PROTOCOL, "ok": false, "error": error});
        let reply = if token.is_some_and(|t| request["token"].as_str() != Some(t)) {
            refused("bad token")
        } else if request["v"] != PROTOCOL {
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
                Some("patch") => match request["body"].as_str() {
                    Some(body) => self.patch(body),
                    None => refused("patch needs a body"),
                },
                Some("await") => {
                    if let Some(etag) = request["etag"].as_str() {
                        self.wait_for(etag);
                    }
                    json!({"v": PROTOCOL, "ok": true})
                }
                _ => refused("unknown op"),
            }
        };
        let _ = reader.get_mut().write_all(format!("{reply}\n").as_bytes());
    }
}

/// A random 128-bit token in hex.
#[cfg_attr(unix, allow(dead_code))]
fn token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let half = || {
        std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish()
    };
    format!("{:016x}{:016x}", half(), half())
}

/// Serves the loopback endpoint and writes `<address> <token>` to `daemon.addr`, 0600.
#[cfg_attr(unix, allow(dead_code))]
pub fn listen_tcp(mirror: Arc<Mirror>) -> io::Result<()> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let secret = token();
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
#[cfg(unix)]
pub fn listen(mirror: Arc<Mirror>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = socket_path(&mirror.home);
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let listener = std::os::unix::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
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

    /// The Windows endpoint: loopback TCP with a token from a 0600 file. A request without the
    /// token gets nothing.
    #[test]
    fn loopback_endpoint_needs_the_token() {
        let (m, seen) = fetched("tcp", vec![(200, "ok", vec![("ETag", "\"e2\"")])]);
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
        stream
            .write_all(b"{\"v\":1,\"op\":\"board\",\"token\":\"wrong\"}\n")
            .unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&reply).unwrap()["error"],
            "bad token"
        );
        let (mut stream, token) = board_client::connect_tcp(&m.home).unwrap();
        stream
            .write_all(
                format!(
                    "{{\"v\":1,\"op\":\"board\",\"token\":\"{}\"}}\n",
                    token.unwrap()
                )
                .as_bytes(),
            )
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut head = String::new();
        reader.read_line(&mut head).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&head).unwrap()["ok"], true);
        drop(seen);
    }

    #[test]
    fn unknown_version_and_op_are_refused() {
        let (m, _) = fetched("refused", vec![]);
        listen_tcp(m.clone()).unwrap();
        let (addr, secret) = board_client::tcp_endpoint(&m.home).unwrap();
        for (request, error) in [
            (
                json!({"v": 2, "op": "board", "token": secret}),
                "unknown protocol version",
            ),
            (json!({"v": 1, "op": "drop", "token": secret}), "unknown op"),
        ] {
            let mut stream = std::net::TcpStream::connect(&addr).unwrap();
            stream.write_all(format!("{request}\n").as_bytes()).unwrap();
            let mut reply = String::new();
            BufReader::new(stream).read_line(&mut reply).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&reply).unwrap()["error"],
                error
            );
        }
    }
}
