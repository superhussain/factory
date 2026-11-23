//! Board reads and writes. Each tries the host daemon's mirror first and falls back to the Worker.
//! Writes go as row PATCHes when the board read advertised `Accept-Patch`, else as a full PUT.
//!
//! The daemon's endpoint speaks JSON lines. A request is one line, `{"v":1,"op":...}`. A board
//! reply is a header line `{"v":1,"ok":true,"etag":E,"patch":B}` and then the board's JSON text on
//! one line; every other reply is one line. `ok: false` means the mirror can't answer, so the
//! caller goes to the Worker.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};

use super::exit::{fail, Exit, Res};
use super::http::{api, jitter};
use super::pyjson::{dumps, loads, COMPACT};

pub const PROTOCOL: u64 = 1;
/// How long a board read waits for the daemon.
const READ_LIMIT: Duration = Duration::from_secs(5);
/// How long a write waits: the daemon's Worker request, plus the wait for its own frame.
const WRITE_LIMIT: Duration = Duration::from_secs(45);
/// The Worker's PATCH limits.
const MAX_PATCH_ROWS: usize = 100;
const MAX_PATCH_BYTES: usize = 1024 * 1024;

/// The daemon's Unix socket.
pub fn socket_path(home: &Path) -> PathBuf {
    home.join("daemon.sock")
}

/// Where a daemon without Unix sockets writes `<loopback address> <token>`.
pub fn addr_path(home: &Path) -> PathBuf {
    home.join("daemon.addr")
}

pub trait Stream: Read + Write + Send {
    fn limit(&self, d: Duration) -> io::Result<()>;
}

#[cfg(unix)]
impl Stream for std::os::unix::net::UnixStream {
    fn limit(&self, d: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(d))?;
        self.set_write_timeout(Some(d))
    }
}

impl Stream for std::net::TcpStream {
    fn limit(&self, d: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(d))?;
        self.set_write_timeout(Some(d))
    }
}

/// The loopback endpoint and token a daemon wrote to `daemon.addr`.
pub fn tcp_endpoint(home: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(addr_path(home)).ok()?;
    let (addr, token) = text.trim().split_once(' ')?;
    Some((addr.to_string(), token.to_string()))
}

/// A connection to the loopback endpoint, and the token its requests carry.
pub fn connect_tcp(home: &Path) -> Option<(Box<dyn Stream>, Option<String>)> {
    let (addr, token) = tcp_endpoint(home)?;
    let addr = addr.parse().ok()?;
    let stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).ok()?;
    Some((Box::new(stream), Some(token)))
}

#[cfg(unix)]
fn connect(home: &Path) -> Option<(Box<dyn Stream>, Option<String>)> {
    let stream = std::os::unix::net::UnixStream::connect(socket_path(home)).ok()?;
    Some((Box::new(stream), None))
}

#[cfg(not(unix))]
fn connect(home: &Path) -> Option<(Box<dyn Stream>, Option<String>)> {
    connect_tcp(home)
}

/// Sends one request and returns the reply's first line and the reader for any more. None when no
/// daemon answers, it answers with another protocol version, or it says ok: false.
fn ask(
    home: &Path,
    mut request: Map<String, Value>,
    limit: Duration,
) -> Option<(Value, BufReader<Box<dyn Stream>>)> {
    let (mut stream, token) = connect(home)?;
    stream.limit(limit).ok()?;
    request.insert("v".into(), PROTOCOL.into());
    if let Some(token) = token {
        request.insert("token".into(), token.into());
    }
    let line = serde_json::to_string(&request).ok()? + "\n";
    stream.write_all(line.as_bytes()).ok()?;
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    reader.read_line(&mut head).ok()?;
    let head: Value = serde_json::from_str(&head).ok()?;
    (head["v"] == PROTOCOL && head["ok"] == true).then_some((head, reader))
}

/// One board read: its text (None when the board is absent), ETag, whether the Worker takes row
/// PATCHes, and whether it came from the daemon's mirror.
pub struct BoardRead {
    pub text: Option<String>,
    pub etag: Option<String>,
    pub patch: bool,
    pub mirror: bool,
}

/// A board view from the mirror: the whole board, or `{"job": id}` or `{"project": slug}`.
fn mirror_read(home: &Path, filter: Map<String, Value>) -> Option<BoardRead> {
    let mut request = filter;
    request.insert("op".into(), "board".into());
    let (head, mut reader) = ask(home, request, READ_LIMIT)?;
    let mut text = String::new();
    reader.read_line(&mut text).ok()?;
    text.ends_with('\n').then_some(())?;
    text.pop();
    Some(BoardRead {
        text: Some(text),
        etag: head["etag"].as_str().map(String::from),
        patch: head["patch"] == true,
        mirror: true,
    })
}

/// The board straight from the Worker.
pub fn http_read(home: &Path) -> Res<BoardRead> {
    let r = api(home, "GET", "/api/board", None, &[200, 404], &[])?;
    if r.status == 404 {
        return Ok(BoardRead {
            text: None,
            etag: None,
            patch: false,
            mirror: false,
        });
    }
    Ok(BoardRead {
        etag: r
            .header("ETag")
            .map(|etag| etag.strip_prefix("W/").unwrap_or(etag).to_string()),
        patch: r.header("Accept-Patch").is_some(),
        text: Some(r.text()?),
        mirror: false,
    })
}

/// The whole board, from the mirror when the daemon has one, else from the Worker.
pub fn read(home: &Path) -> Res<BoardRead> {
    match mirror_read(home, Map::new()) {
        Some(read) => Ok(read),
        None => http_read(home),
    }
}

/// The board's text from the mirror, else from the Worker, where an absent board fails as any
/// status but 200 does.
pub fn read_text(home: &Path) -> Res<String> {
    match mirror_read(home, Map::new()).and_then(|read| read.text) {
        Some(text) => Ok(text),
        None => api(home, "GET", "/api/board", None, &[200], &[])?.text(),
    }
}

/// A board holding only the jobs `filter` names, `{"job": id}` or `{"project": slug}`, as text.
/// It comes from the mirror when the daemon has one, else from the whole board on the Worker.
pub fn read_view(home: &Path, filter: Map<String, Value>) -> Res<Option<String>> {
    if let Some(read) = mirror_read(home, filter.clone()) {
        return Ok(read.text);
    }
    let read = http_read(home)?;
    let Some(text) = read.text else {
        return Ok(None);
    };
    let board = loads(&text).map_err(|e| fail(format!("the board is not valid JSON: {e}"), 1))?;
    Ok(Some(dumps(&view(&board, &filter), COMPACT)))
}

/// The board with only the jobs filter names: `{"job": id}` or `{"project": slug}`.
pub fn view(board: &Value, filter: &Map<String, Value>) -> Value {
    let keep = |j: &Value| {
        filter.iter().all(|(key, want)| match key.as_str() {
            "job" => j.get("id") == Some(want),
            "project" => j.get("project") == Some(want),
            _ => true,
        })
    };
    let jobs: Vec<Value> = board["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|j| keep(j))
        .cloned()
        .collect();
    json!({"updated": board.get("updated").cloned().unwrap_or(Value::Null), "jobs": jobs})
}

/// The row PATCH body that turns the board `old` into `new`, or None when a PATCH can't: a change
/// outside `jobs` and `updated`, ids that are missing or repeat, no row changed, more rows or
/// bytes than the Worker takes, an `updated` that would not move forward, or a row order the
/// Worker would not produce (it keeps rows in place and appends new ones).
pub fn row_patch(old: &str, new: &str) -> Option<String> {
    let (Value::Object(old), Value::Object(new)) = (loads(old).ok()?, loads(new).ok()?) else {
        return None;
    };
    for key in old.keys().chain(new.keys()) {
        if key != "jobs" && key != "updated" && old.get(key) != new.get(key) {
            return None;
        }
    }
    let ids = |doc: &Map<String, Value>| -> Option<Vec<(String, Value)>> {
        let mut seen = HashSet::new();
        let mut rows = Vec::new();
        for j in doc.get("jobs")?.as_array()? {
            let id = j.get("id")?.as_str()?.to_string();
            if !seen.insert(id.clone()) {
                return None;
            }
            rows.push((id, j.clone()));
        }
        Some(rows)
    };
    let (before, after) = (ids(&old)?, ids(&new)?);
    let was: HashMap<&str, &Value> = before.iter().map(|(id, j)| (id.as_str(), j)).collect();
    let now: HashMap<&str, &Value> = after.iter().map(|(id, j)| (id.as_str(), j)).collect();
    let mut rows = Vec::new();
    for (id, row) in &after {
        match was.get(id.as_str()) {
            Some(prev) if *prev == row => {}
            prev => rows.push(json!({"id": id, "expect": prev.copied().cloned().unwrap_or(Value::Null), "row": row})),
        }
    }
    for (id, prev) in &before {
        if !now.contains_key(id.as_str()) {
            rows.push(json!({"id": id, "expect": prev, "row": null}));
        }
    }
    if rows.is_empty() || rows.len() > MAX_PATCH_ROWS {
        return None;
    }
    let kept = before
        .iter()
        .filter(|(id, _)| now.contains_key(id.as_str()))
        .map(|(id, _)| id);
    let added = after
        .iter()
        .filter(|(id, _)| !was.contains_key(id.as_str()))
        .map(|(id, _)| id);
    if !kept.chain(added).eq(after.iter().map(|(id, _)| id)) {
        return None;
    }
    let mut body = json!({"rows": rows});
    let (was_at, now_at) = (old.get("updated"), new.get("updated"));
    if now_at != was_at {
        let now_at = now_at?.as_str()?;
        let was_at = was_at.and_then(Value::as_str).unwrap_or("");
        if now_at <= was_at {
            return None;
        }
        body["updated"] = now_at.into();
    }
    let body = dumps(&body, COMPACT);
    (body.len() <= MAX_PATCH_BYTES).then_some(body)
}

/// Sends a row PATCH, through the daemon when the read came from its mirror. True once the Worker
/// committed it, false on a 409 conflict or when the daemon dropped the request, so the caller
/// reads again.
fn send_patch(home: &Path, body: &str, mirror: bool) -> Res<bool> {
    if mirror {
        let mut request = Map::new();
        request.insert("op".into(), "patch".into());
        request.insert("body".into(), body.into());
        let Some((head, _)) = ask(home, request, WRITE_LIMIT) else {
            return Ok(false);
        };
        return match head["status"].as_u64() {
            Some(200) => Ok(true),
            Some(409) => Ok(false),
            _ => Err(Exit {
                code: head["code"].as_i64().unwrap_or(5) as i32,
                stderr: head["stderr"].as_str().unwrap_or_default().to_string(),
                status: head["status"].as_array().and_then(|s| {
                    Some((s.first()?.as_u64()? as u16, s.get(1)?.as_str()?.to_string()))
                }),
            }),
        };
    }
    let r = api(
        home,
        "PATCH",
        "/api/board",
        Some(body.as_bytes()),
        &[200, 409],
        &[("Content-Type", "application/json")],
    )?;
    if let Some(etag) = r.header("ETag").filter(|_| r.status == 200) {
        await_etag(home, etag.strip_prefix("W/").unwrap_or(etag));
    }
    Ok(r.status == 200)
}

/// After a write straight to the Worker, waits until a running daemon's mirror has the board at
/// etag, so this host's next read sees its own write. Without a daemon it returns at once.
fn await_etag(home: &Path, etag: &str) {
    let mut request = Map::new();
    request.insert("op".into(), "await".into());
    request.insert("etag".into(), etag.into());
    let _ = ask(home, request, WRITE_LIMIT);
}

/// Python's `cas` for the board: read, change, and write the changed rows, or the whole board
/// when a PATCH can't carry the change, retrying when another writer got there first. After a
/// conflict it reads the Worker directly, since the mirror may not have the other write yet.
pub fn cas<T>(
    home: &Path,
    mut change: impl FnMut(Option<&str>) -> Res<(Option<String>, T)>,
) -> Res<T> {
    let mut direct = false;
    for _ in 0..20 {
        let read = if direct {
            http_read(home)?
        } else {
            read(home)?
        };
        if read.text.is_some() && read.etag.is_none() {
            return Err(fail("/api/board: the Worker sent no ETag", 1));
        }
        let (new, result) = change(read.text.as_deref())?;
        let Some(new) = new else { return Ok(result) };
        let body = match (&read.text, read.patch) {
            (Some(text), true) => row_patch(text, &new),
            _ => None,
        };
        let wrote = match body {
            Some(body) => send_patch(home, &body, read.mirror)?,
            None => {
                let cond = match &read.etag {
                    Some(etag) => ("If-Match", etag.as_str()),
                    None => ("If-None-Match", "*"),
                };
                let r = api(
                    home,
                    "PUT",
                    "/api/board",
                    Some(new.as_bytes()),
                    &[200, 412],
                    &[cond],
                )?;
                if let Some(etag) = r.header("ETag").filter(|_| r.status == 200) {
                    await_etag(home, etag.strip_prefix("W/").unwrap_or(etag));
                }
                r.status == 200
            }
        };
        if wrote {
            return Ok(result);
        }
        direct = true;
        std::thread::sleep(jitter());
    }
    Err(fail(
        "/api/board kept changing under us; gave up after 20 tries",
        5,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(jobs: Value) -> String {
        dumps(
            &json!({"updated": "2026-11-23T10:00:00Z", "jobs": jobs}),
            COMPACT,
        )
    }

    fn parsed(body: Option<String>) -> Value {
        serde_json::from_str(&body.expect("a patch")).unwrap()
    }

    const A: &str = r#"{"id":"2001-add-csv-export","stage":"idea"}"#;

    #[test]
    fn row_patch_carries_changed_added_and_removed_rows() {
        let old =
            board(json!([{"id": "1999-a", "stage": "build"}, {"id": "2000-b", "stage": "spec"}]));
        let new = dumps(
            &json!({"updated": "2026-11-23T10:00:01Z", "jobs": [
                {"id": "1999-a", "stage": "review"},
                serde_json::from_str::<Value>(A).unwrap(),
            ]}),
            COMPACT,
        );
        assert_eq!(
            parsed(row_patch(&old, &new)),
            json!({"rows": [
                {"id": "1999-a", "expect": {"id": "1999-a", "stage": "build"}, "row": {"id": "1999-a", "stage": "review"}},
                {"id": "2001-add-csv-export", "expect": null, "row": {"id": "2001-add-csv-export", "stage": "idea"}},
                {"id": "2000-b", "expect": {"id": "2000-b", "stage": "spec"}, "row": null},
            ], "updated": "2026-11-23T10:00:01Z"})
        );
    }

    #[test]
    fn row_patch_refuses_what_a_patch_cannot_say() {
        let old = board(json!([{"id": "1999-a"}, {"id": "2000-b"}]));
        let same_updated = board(json!([{"id": "1999-a", "x": 1}, {"id": "2000-b"}]));
        assert!(row_patch(&old, &same_updated).is_some());
        for new in [
            // No row changed.
            old.clone(),
            // A reorder.
            board(json!([{"id": "2000-b"}, {"id": "1999-a", "x": 1}])),
            // A new row anywhere but the end.
            board(json!([{"id": "2001-c"}, {"id": "1999-a"}, {"id": "2000-b"}])),
            // A repeated or missing id.
            board(json!([{"id": "1999-a"}, {"id": "1999-a"}, {"id": "2000-b"}])),
            board(json!([{"id": "1999-a"}, {"id": "2000-b"}, {"stage": "idea"}])),
            // A top-level change.
            dumps(
                &json!({"updated": "2026-11-23T10:00:00Z", "jobs": [{"id": "1999-a", "x": 1}, {"id": "2000-b"}], "autonomy": {}}),
                COMPACT,
            ),
            // An updated stamp that goes back.
            dumps(
                &json!({"updated": "2026-11-23T09:00:00Z", "jobs": [{"id": "1999-a", "x": 1}, {"id": "2000-b"}]}),
                COMPACT,
            ),
        ] {
            assert_eq!(row_patch(&old, &new), None, "{new}");
        }
        let many: Vec<Value> = (0..101)
            .map(|n| json!({"id": format!("{n:04}-x")}))
            .collect();
        assert_eq!(
            row_patch(&board(json!([])), &board(Value::Array(many))),
            None
        );
    }

    #[test]
    fn view_keeps_one_job_or_one_project() {
        let b = json!({"updated": "u", "jobs": [
            {"id": "1-a", "project": "sample"}, {"id": "2-b", "project": "other"}, {"id": "3-c", "project": "sample"}]});
        let job = Map::from_iter([("job".to_string(), json!("2-b"))]);
        assert_eq!(
            view(&b, &job)["jobs"],
            json!([{"id": "2-b", "project": "other"}])
        );
        let project = Map::from_iter([("project".to_string(), json!("sample"))]);
        assert_eq!(view(&b, &project)["jobs"].as_array().unwrap().len(), 2);
        assert_eq!(view(&b, &project)["updated"], "u");
    }
}
