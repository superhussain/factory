//! Board reads and writes. Each tries the host daemon's mirror first and falls back to the Worker.
//! Writes go as row PATCHes when the board read advertised `Accept-Patch`, else as a full PUT.
//!
//! The daemon's endpoint speaks JSON lines. A request is one line, `{"v":1,"op":...}`. A board
//! reply is a header line `{"v":1,"ok":true,"etag":E,"patch":B}` and then the board's JSON text on
//! one line; every other reply is one line. `ok: false` means the mirror can't answer, so the
//! caller goes to the Worker. On the loopback endpoint each side first proves it holds the token
//! from `daemon.addr`, so a stale file pointing at another process's port is never trusted.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::exit::{fail, Exit, Res};
use super::http::{api, jitter, Response};
use super::pyjson::{dumps, loads, COMPACT};

pub const PROTOCOL: u64 = 1;
/// How long a board read waits for the daemon.
const READ_LIMIT: Duration = Duration::from_secs(5);
/// How long a write waits: the daemon's Worker request, plus the wait for its own frame.
const WRITE_LIMIT: Duration = Duration::from_secs(45);
/// The Worker's PATCH limits.
const MAX_PATCH_ROWS: usize = 100;
const MAX_PATCH_BYTES: usize = 1024 * 1024;
/// How many times a board write retries a 503, and the first wait, which doubles each retry.
const BUSY_RETRIES: u32 = 3;
#[cfg(not(test))]
const BUSY_BASE: Duration = Duration::from_secs(1);
#[cfg(test)]
const BUSY_BASE: Duration = Duration::from_millis(1);

/// Whether the Worker answered 503, as when it ran out of CPU time.
fn busy(e: &Exit) -> bool {
    matches!(e.status, Some((503, _)))
}

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

/// The loopback handshake's proof that `role` holds token, for this nonce.
pub fn proof(token: &str, role: &str, nonce: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{token}:{role}:{nonce}")))
}

/// Compares two strings in time that depends only on their lengths.
pub fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

/// A connection to the loopback endpoint, once the peer has proved it holds the token. The client
/// sends a fresh nonce with its own proof, never the token, and the daemon answers with its proof.
pub fn connect_tcp(home: &Path) -> Option<BufReader<Box<dyn Stream>>> {
    let (addr, token) = tcp_endpoint(home)?;
    let addr = addr.parse().ok()?;
    let stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).ok()?;
    stream.limit(READ_LIMIT).ok()?;
    let mut reader = BufReader::new(Box::new(stream) as Box<dyn Stream>);
    let nonce = crate::commands::catch_up::random_hex(16);
    let hello = json!({"v": PROTOCOL, "op": "hello", "nonce": nonce, "auth": proof(&token, "client", &nonce)});
    reader
        .get_mut()
        .write_all(format!("{hello}\n").as_bytes())
        .ok()?;
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let reply: Value = serde_json::from_str(&line).ok()?;
    let want = proof(&token, "daemon", &nonce);
    same_secret(reply["proof"].as_str()?, &want).then_some(reader)
}

#[cfg(unix)]
fn connect(home: &Path) -> Option<BufReader<Box<dyn Stream>>> {
    let stream = std::os::unix::net::UnixStream::connect(socket_path(home)).ok()?;
    Some(BufReader::new(Box::new(stream)))
}

#[cfg(not(unix))]
fn connect(home: &Path) -> Option<BufReader<Box<dyn Stream>>> {
    connect_tcp(home)
}

/// Why a request got no usable reply.
#[derive(Debug, PartialEq)]
enum Unanswered {
    /// No daemon took the request: nothing connected or proved itself, or it refused the request.
    Unreached,
    /// The request went out and no reply came, so the daemon may have acted on it.
    Lost,
}

/// Sends one request and returns the reply's first line and the reader for any more.
fn ask(
    home: &Path,
    mut request: Map<String, Value>,
    limit: Duration,
) -> Result<(Value, BufReader<Box<dyn Stream>>), Unanswered> {
    let mut reader = connect(home).ok_or(Unanswered::Unreached)?;
    reader
        .get_ref()
        .limit(limit)
        .map_err(|_| Unanswered::Unreached)?;
    request.insert("v".into(), PROTOCOL.into());
    let line = serde_json::to_string(&request).map_err(|_| Unanswered::Unreached)? + "\n";
    reader
        .get_mut()
        .write_all(line.as_bytes())
        .map_err(|_| Unanswered::Lost)?;
    let mut head = String::new();
    reader.read_line(&mut head).map_err(|_| Unanswered::Lost)?;
    let head: Value = serde_json::from_str(&head).map_err(|_| Unanswered::Lost)?;
    if head["v"] != PROTOCOL || head["ok"] != true {
        return Err(Unanswered::Unreached);
    }
    Ok((head, reader))
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
    let (head, mut reader) = ask(home, request, READ_LIMIT).ok()?;
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

/// The Worker client a board write goes through: `http::api`, or a test's stand-in for it.
pub type Api<'a> =
    &'a dyn Fn(&Path, &str, &str, Option<&[u8]>, &[u16], &[(&str, &str)]) -> Res<Response>;

/// The board straight from the Worker.
pub fn http_read(home: &Path) -> Res<BoardRead> {
    http_read_via(home, &api)
}

fn http_read_via(home: &Path, api: Api) -> Res<BoardRead> {
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

/// JavaScript's `Number.prototype.toString` for a finite number, as `JSON.stringify` writes it.
fn js_number(x: f64) -> String {
    if x == 0.0 {
        return "0".to_string();
    }
    let sign = if x < 0.0 { "-" } else { "" };
    // Rust's shortest round-trip digits, as JavaScript picks them.
    let sci = format!("{:e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap();
    let digits = mantissa.replace('.', "");
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().unwrap() + 1;
    let body = if k <= n && n <= 21 {
        digits + &"0".repeat((n - k) as usize)
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat(-n as usize))
    } else {
        let e = n - 1;
        let e = if e < 0 {
            format!("-{}", -e)
        } else {
            format!("+{e}")
        };
        match k {
            1 => format!("{digits}e{e}"),
            _ => format!("{}.{}e{e}", &digits[..1], &digits[1..]),
        }
    };
    format!("{sign}{body}")
}

/// The Worker's `canonicalJson`: `JSON.stringify` with object keys sorted at every depth.
fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&js_number(n.as_f64().unwrap_or(f64::NAN))),
        Value::String(s) => {
            out.push('"');
            for c in s.chars() {
                match c {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\u{8}' => out.push_str("\\b"),
                    '\u{c}' => out.push_str("\\f"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            // canonicalJson sorts keys by UTF-16 code units, but the object it builds still lists
            // array-index keys ("0" to "4294967294", no leading zero) first, in numeric order.
            let index = |k: &str| {
                (k == "0" || !k.starts_with('0'))
                    .then(|| k.parse::<u32>().ok())
                    .flatten()
                    .filter(|n| *n < u32::MAX && n.to_string() == k)
            };
            keys.sort_by(|a, b| match (index(a), index(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.encode_utf16().cmp(b.encode_utf16()),
            });
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(&Value::String((*key).clone()), out);
                out.push(':');
                canonical(&map[*key], out);
            }
            out.push('}');
        }
    }
}

/// The Worker's `rowSha256`: lowercase hex SHA-256 of the row's canonical JSON.
pub fn row_sha256(row: &Value) -> String {
    let mut text = String::new();
    canonical(row, &mut text);
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Which unchanged rows a write read to decide, so the Worker refuses it if any of them changed.
/// An open row it picks goes as a whole-row digest, since a claim reads many of its fields. A
/// merged or closed row it picks, and any depends-on row of a row it writes, goes as a stage
/// read: those decisions read only whether the row is still finished, and the Worker checks a
/// stage with a string compare instead of hashing the row.
pub type Reads<'a> = Option<&'a dyn Fn(&Value) -> bool>;

/// A row that is neither merged nor closed: the scope a claim under an in-flight cap reads.
pub fn open_row(j: &Value) -> bool {
    !j.get("stage")
        .and_then(Value::as_str)
        .is_some_and(|s| super::consts::TERMINAL.contains(&s))
}

/// The row PATCH body that turns the board `old` into `new`, or None when a PATCH can't: a change
/// outside `jobs` and `updated`, ids that are missing or repeat, no row changed, more rows or
/// bytes than the Worker takes, an `updated` that would not move forward, or a row order the
/// Worker would not produce (it keeps rows in place and appends new ones). Each unchanged row of
/// `old` that `reads` picks goes in as a `reads` digest.
pub fn row_patch(old: &str, new: &str, reads: Reads) -> Option<String> {
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
    if let Some(picks) = reads {
        // The depends-on rows of every row written, open or not, since a reopened or merged
        // dependency changes whether that row may start.
        let deps: HashSet<&str> = after
            .iter()
            .filter(|(id, row)| was.get(id.as_str()) != Some(&row))
            .flat_map(|(_, row)| {
                row.get("refs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter(|r| r.get("kind").and_then(Value::as_str) == Some("depends-on"))
            .filter_map(|r| r.get("id")?.as_str())
            .collect();
        let digests: Vec<Value> = before
            .iter()
            .filter(|(id, row)| now.get(id.as_str()) == Some(&row))
            .filter_map(|(id, row)| match (picks(row), open_row(row)) {
                (true, true) => Some(json!({"id": id, "sha256": row_sha256(row)})),
                (true, false) => {
                    Some(json!({"id": id, "stage": row.get("stage").unwrap_or(&Value::Null)}))
                }
                (false, _) if deps.contains(id.as_str()) => {
                    Some(json!({"id": id, "stage": row.get("stage").unwrap_or(&Value::Null)}))
                }
                _ => None,
            })
            .collect();
        if !digests.is_empty() {
            body["reads"] = Value::Array(digests);
        }
    }
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

/// Whether the board `now` already holds the write that turned `old` into `new`: every changed
/// top-level key but `jobs` and `updated` has its new value, every changed or added row is `new`'s
/// row, and every removed row is gone. Rows match by the first row with each id, and an absent
/// `old` counts as an empty board. False when `now` is absent, nothing changed, or JSON fails.
fn landed(old: Option<&str>, new: &str, now: Option<&str>) -> bool {
    let parse = |text: &str| match loads(text) {
        Ok(Value::Object(doc)) => Some(doc),
        _ => None,
    };
    let old = match old {
        Some(text) => parse(text),
        None => Some(Map::from_iter([("jobs".to_string(), json!([]))])),
    };
    let (Some(old), Some(new), Some(now)) = (old, parse(new), now.and_then(parse)) else {
        return false;
    };
    let mut changed = false;
    for key in old.keys().chain(new.keys()) {
        if key != "jobs" && key != "updated" && old.get(key) != new.get(key) {
            if now.get(key) != new.get(key) {
                return false;
            }
            changed = true;
        }
    }
    let rows = |doc: &Map<String, Value>| {
        let mut rows = HashMap::new();
        for j in doc
            .get("jobs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(id) = j.get("id").and_then(Value::as_str) {
                rows.entry(id.to_string()).or_insert_with(|| j.clone());
            }
        }
        rows
    };
    let (was, will, is) = (rows(&old), rows(&new), rows(&now));
    for (id, row) in &will {
        if was.get(id) != Some(row) {
            if is.get(id) != Some(row) {
                return false;
            }
            changed = true;
        }
    }
    for id in was.keys().filter(|id| !will.contains_key(*id)) {
        if is.contains_key(id) {
            return false;
        }
        changed = true;
    }
    changed
}

/// Sends a row PATCH, through the daemon when the read came from its mirror, else, or when no
/// daemon takes it, straight to the Worker. True once the Worker committed it, false on a 409
/// conflict, so the caller reads again. A request the daemon took but never answered fails,
/// since the write may have landed and running the change again could repeat it.
fn send_patch(home: &Path, body: &str, mirror: bool, api: Api) -> Res<bool> {
    if mirror {
        let mut request = Map::new();
        request.insert("op".into(), "patch".into());
        request.insert("body".into(), body.into());
        match ask(home, request, WRITE_LIMIT) {
            Err(Unanswered::Unreached) => {}
            Err(Unanswered::Lost) => {
                return Err(fail(
                    "the daemon took a board write and sent no reply, so it may have landed. Nothing else was written; read the board before you retry.",
                    5,
                ))
            }
            Ok((head, _)) => return match head["status"].as_u64() {
            Some(200) => Ok(true),
            Some(409) => Ok(false),
            _ => Err(Exit {
                code: head["code"].as_i64().unwrap_or(5) as i32,
                stderr: head["stderr"].as_str().unwrap_or_default().to_string(),
                status: head["status"].as_array().and_then(|s| {
                    Some((s.first()?.as_u64()? as u16, s.get(1)?.as_str()?.to_string()))
                }),
            }),
            },
        }
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
pub fn cas<T>(home: &Path, change: impl FnMut(Option<&str>) -> Res<(Option<String>, T)>) -> Res<T> {
    cas_reading(home, None, change)
}

/// `cas` for a change that decides from rows it doesn't write, such as a claim under an in-flight
/// cap: each unchanged row `reads` picks, and each depends-on row of a row it writes, goes in the
/// PATCH as a digest, so the Worker refuses the write if any of them changed since the read.
pub fn cas_reading<T>(
    home: &Path,
    reads: Reads,
    change: impl FnMut(Option<&str>) -> Res<(Option<String>, T)>,
) -> Res<T> {
    cas_via(home, reads, &api, change)
}

/// `cas_reading` through the Worker client `api`, so a caller's test seam reaches it too. A 503 on
/// the write or its read retries up to `BUSY_RETRIES` times after a backoff, reading the Worker
/// directly first, and a write the re-read shows `landed` returns without running its change again.
pub fn cas_via<T>(
    home: &Path,
    reads: Reads,
    api: Api,
    mut change: impl FnMut(Option<&str>) -> Res<(Option<String>, T)>,
) -> Res<T> {
    let mut direct = false;
    let mut busy_tries = 0;
    let mut wait_busy = |e: &Exit| {
        if !busy(e) || busy_tries == BUSY_RETRIES {
            return false;
        }
        busy_tries += 1;
        std::thread::sleep(BUSY_BASE * (1 << (busy_tries - 1)) + jitter());
        true
    };
    let mut sent: Option<(Option<String>, String, T)> = None;
    for _ in 0..20 {
        let read = match (direct, mirror_read(home, Map::new())) {
            (false, Some(read)) => read,
            _ => match http_read_via(home, api) {
                Ok(read) => read,
                Err(e) if wait_busy(&e) => {
                    direct = true;
                    continue;
                }
                Err(e) => return Err(e),
            },
        };
        if let Some((old, new, result)) = sent.take() {
            if landed(old.as_deref(), &new, read.text.as_deref()) {
                return Ok(result);
            }
        }
        if read.text.is_some() && read.etag.is_none() {
            return Err(fail("/api/board: the Worker sent no ETag", 1));
        }
        let (new, result) = change(read.text.as_deref())?;
        let Some(new) = new else { return Ok(result) };
        let body = match (&read.text, read.patch) {
            (Some(text), true) => row_patch(text, &new, reads),
            _ => None,
        };
        let wrote = match body {
            Some(body) => send_patch(home, &body, read.mirror, api),
            None => {
                let cond = match &read.etag {
                    Some(etag) => ("If-Match", etag.as_str()),
                    None => ("If-None-Match", "*"),
                };
                api(
                    home,
                    "PUT",
                    "/api/board",
                    Some(new.as_bytes()),
                    &[200, 412],
                    &[cond],
                )
                .map(|r| {
                    if let Some(etag) = r.header("ETag").filter(|_| r.status == 200) {
                        await_etag(home, etag.strip_prefix("W/").unwrap_or(etag));
                    }
                    r.status == 200
                })
            }
        };
        match wrote {
            Ok(true) => return Ok(result),
            Ok(false) => std::thread::sleep(jitter()),
            Err(e) if wait_busy(&e) => sent = Some((read.text, new, result)),
            Err(e) => return Err(e),
        }
        direct = true;
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
            parsed(row_patch(&old, &new, None)),
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
        assert!(row_patch(&old, &same_updated, None).is_some());
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
            assert_eq!(row_patch(&old, &new, None), None, "{new}");
        }
        let many: Vec<Value> = (0..101)
            .map(|n| json!({"id": format!("{n:04}-x")}))
            .collect();
        assert_eq!(
            row_patch(&board(json!([])), &board(Value::Array(many)), None),
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

    /// The digest matches the Worker's `rowSha256`, computed by Node for this row: sorted keys at
    /// every depth, JavaScript number forms and string escapes.
    #[test]
    fn row_sha256_matches_the_worker() {
        let row: Value = serde_json::from_str(r#"{"id":"2001-add-csv-export","title":"Add CSV éxport \"q\" \\ \n\t\u0001   😀","stage":"idea","n":[1,1.5,1e21,1.25e-7,-0.000001,123456789012,0.1,100],"z":{"b":true,"a":null,"B":[{"y":1,"x":2}]}}"#).unwrap();
        assert_eq!(
            row_sha256(&row),
            "2d406ceeddb2bd9f5727782ba827e461e2bcb83e54dee79b720b5bc5ec93829c"
        );
        // Array-index keys come first in numeric order, as JavaScript objects list them.
        let mixed: Value = serde_json::from_str(r#"{"id":"2001-x","b":1,"10":"ten","2":"two","a":{"01":1,"1":2,"4294967295":3,"4294967294":4,"-1":5,"Z":6}}"#).unwrap();
        assert_eq!(
            row_sha256(&mixed),
            "3eb214f8f550773261ce4bfca39a504f606119572fee6e6d56d9937452a5d51d"
        );
        for (x, js) in [
            (100.0, "100"),
            (1e21, "1e+21"),
            (1.5e-7, "1.5e-7"),
            (-0.5, "-0.5"),
            (123.456, "123.456"),
            (1e-6, "0.000001"),
        ] {
            assert_eq!(js_number(x), js);
        }
    }

    /// The Worker's conflict check for a PATCH body against board: a row whose stored value is not
    /// its expect, or a read whose digest no longer matches. Applies the body when there is none.
    fn worker_patch(board: &mut Value, body: &str) -> bool {
        let body: Value = serde_json::from_str(body).unwrap();
        let jobs = board["jobs"].as_array_mut().unwrap();
        let find = |jobs: &Vec<Value>, id: &Value| jobs.iter().position(|j| &j["id"] == id);
        for e in body["rows"].as_array().unwrap() {
            let stored = find(jobs, &e["id"])
                .map(|i| jobs[i].clone())
                .unwrap_or(Value::Null);
            if stored != e["expect"] {
                return false;
            }
        }
        for r in body["reads"].as_array().into_iter().flatten() {
            let Some(i) = find(jobs, &r["id"]) else {
                return false;
            };
            let held = match r.get("stage") {
                Some(stage) => jobs[i].get("stage").unwrap_or(&Value::Null) == stage,
                None => row_sha256(&jobs[i]) == r["sha256"],
            };
            if !held {
                return false;
            }
        }
        for e in body["rows"].as_array().unwrap() {
            match find(jobs, &e["id"]) {
                Some(i) => jobs[i] = e["row"].clone(),
                None => jobs.push(e["row"].clone()),
            }
        }
        true
    }

    /// Two hosts read one board with one job in flight under a cap of 2, and each claims a
    /// different idea. With the claim's reads only the first commits; without them both would,
    /// and three jobs would be in flight.
    #[test]
    fn two_claims_under_one_cap_cannot_both_commit() {
        let start = board(json!([
            {"id": "1999-a", "project": "factory", "stage": "build"},
            {"id": "2001-a", "project": "factory", "stage": "idea"},
            {"id": "2002-b", "project": "factory", "stage": "idea"},
            {"id": "1000-old", "project": "factory", "stage": "merged"},
        ]));
        let claim = |text: &str, pick: &str| {
            let mut b = loads(text).unwrap();
            let inflight = b["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|j| j["stage"] == "build")
                .count();
            assert!(inflight < 2);
            for j in b["jobs"].as_array_mut().unwrap() {
                if j["id"] == pick {
                    j["stage"] = "build".into();
                }
            }
            dumps(&b, COMPACT)
        };
        let open = |j: &Value| {
            !j["stage"]
                .as_str()
                .is_some_and(|s| ["merged", "closed"].contains(&s))
        };
        for (reads, both) in [
            (Some(&open as &dyn Fn(&Value) -> bool), false),
            (None, true),
        ] {
            let mac = row_patch(&start, &claim(&start, "2001-a"), reads).unwrap();
            let wsl = row_patch(&start, &claim(&start, "2002-b"), reads).unwrap();
            let mut worker = loads(&start).unwrap();
            assert!(worker_patch(&mut worker, &mac));
            assert_eq!(worker_patch(&mut worker, &wsl), both);
        }
        // Open rows go as digests. A merged row in scope, or a dependency, goes as a stage read,
        // and a row outside both is not sent.
        let everything = |_: &Value| true;
        let mac = parsed(row_patch(
            &start,
            &claim(&start, "2001-a"),
            Some(&everything),
        ));
        assert_eq!(mac["reads"][0]["id"], "1999-a");
        assert!(mac["reads"][0]["sha256"].is_string());
        assert_eq!(
            mac["reads"][2],
            json!({"id": "1000-old", "stage": "merged"})
        );
        let mac = parsed(row_patch(&start, &claim(&start, "2001-a"), Some(&open)));
        assert_eq!(mac["reads"].as_array().unwrap().len(), 2);
    }

    /// A merged dependency of a written row goes as a stage read even outside the scope, and its
    /// reopening refuses the write.
    #[test]
    fn a_reopened_dependency_refuses_the_write() {
        let start = board(json!([
            {"id": "1000-base", "stage": "merged", "note": "old"},
            {"id": "2001-a", "stage": "idea", "refs": [{"id": "1000-base", "kind": "depends-on"}]},
        ]));
        let mut b = loads(&start).unwrap();
        b["jobs"][1]["stage"] = "build".into();
        let none = |_: &Value| false;
        let body = row_patch(&start, &dumps(&b, COMPACT), Some(&none)).unwrap();
        assert_eq!(
            parsed(Some(body.clone()))["reads"],
            json!([{"id": "1000-base", "stage": "merged"}])
        );
        let mut worker = loads(&start).unwrap();
        worker["jobs"][0]["note"] = "edited, still merged".into();
        assert!(worker_patch(&mut worker.clone(), &body));
        worker["jobs"][0]["stage"] = "build".into();
        assert!(!worker_patch(&mut worker, &body));
    }

    #[test]
    fn a_body_over_1_mib_is_no_patch() {
        let old = board(json!([{"id": "1999-a"}]));
        let big = board(json!([{"id": "1999-a", "note": "x".repeat(MAX_PATCH_BYTES)}]));
        assert_eq!(row_patch(&old, &big, None), None);
    }

    #[test]
    fn landed_sees_rows_and_top_level_keys() {
        let old = board(json!([{"id": "1999-a", "stage": "build"}, {"id": "2000-b"}]));
        let changed = board(json!([{"id": "1999-a", "stage": "review"}, {"id": "2000-b"}]));
        let added =
            board(json!([{"id": "1999-a", "stage": "build"}, {"id": "2000-b"}, {"id": "2001-c"}]));
        let removed = board(json!([{"id": "1999-a", "stage": "build"}]));
        let autonomy = dumps(
            &json!({"updated": "2026-11-23T10:00:00Z", "jobs": [{"id": "1999-a", "stage": "build"}, {"id": "2000-b"}], "autonomy": {"paused": true}}),
            COMPACT,
        );
        for new in [&changed, &added, &removed, &autonomy] {
            assert!(landed(Some(&old), new, Some(new)), "{new}");
            assert!(!landed(Some(&old), new, Some(&old)), "{new}");
            assert!(!landed(Some(&old), new, None), "{new}");
        }
        // Another writer's later edit to the same row is not this write.
        let other = board(json!([{"id": "1999-a", "stage": "merged"}, {"id": "2000-b"}]));
        assert!(!landed(Some(&old), &changed, Some(&other)));
        // An unrelated row changing too does not hide a landed write.
        let both = board(json!([{"id": "1999-a", "stage": "review"}, {"id": "2000-b", "x": 1}]));
        assert!(landed(Some(&old), &changed, Some(&both)));
        // No board yet, then the first row.
        let first = board(json!([{"id": "1999-a"}]));
        assert!(landed(None, &first, Some(&first)));
        assert!(!landed(Some(&old), &old, Some(&old)));
        assert!(!landed(Some(&old), "not json", Some(&old)));
    }

    use crate::shared::http::tests::{home_for, serve};

    /// A change a PATCH can't say goes as a PUT with If-Match even when the read advertised
    /// Accept-Patch, and so does one over 1 MiB.
    #[test]
    fn cas_puts_what_a_patch_cannot_say() {
        let start = board(json!([{"id": "1999-a"}]));
        let start: &'static str = Box::leak(start.into_boxed_str());
        let patchable = vec![("ETag", "\"e1\""), ("Accept-Patch", "application/json")];
        let (origin, seen) = serve(vec![
            (200, start, patchable.clone()),
            (200, "ok", vec![("ETag", "\"e2\"")]),
            (200, start, patchable),
            (200, "ok", vec![("ETag", "\"e3\"")]),
        ]);
        let home = home_for("board-client-put", &origin);
        cas(&home, |text| {
            let mut b = loads(text.unwrap()).unwrap();
            b["autonomy"] = json!({"paused": true});
            Ok((Some(dumps(&b, COMPACT)), ()))
        })
        .unwrap();
        cas(&home, |text| {
            let mut b = loads(text.unwrap()).unwrap();
            b["jobs"][0]["note"] = "x".repeat(MAX_PATCH_BYTES).into();
            Ok((Some(dumps(&b, COMPACT)), ()))
        })
        .unwrap();
        let seen = seen.lock().unwrap();
        let methods: Vec<&str> = seen.iter().map(|s| s.method.as_str()).collect();
        assert_eq!(methods, ["GET", "PUT", "GET", "PUT"]);
        assert_eq!(seen[1].header("If-Match"), Some("\"e1\""));
    }

    /// A PATCH that gets 409 reads again and runs the change on the new board.
    #[test]
    fn cas_rereads_after_a_409() {
        let first = board(json!([{"id": "1999-a", "stage": "build"}]));
        let second = board(json!([{"id": "1999-a", "stage": "review"}]));
        let (first, second): (&'static str, &'static str) = (
            Box::leak(first.into_boxed_str()),
            Box::leak(second.into_boxed_str()),
        );
        let patchable = |etag| vec![("ETag", etag), ("Accept-Patch", "application/json")];
        let (origin, seen) = serve(vec![
            (200, first, patchable("\"e1\"")),
            (409, r#"{"error":"conflict"}"#, vec![]),
            (200, second, patchable("\"e2\"")),
            (200, "ok", vec![("ETag", "\"e3\"")]),
        ]);
        let home = home_for("board-client-409", &origin);
        let mut stages = Vec::new();
        cas(&home, |text| {
            let mut b = loads(text.unwrap()).unwrap();
            stages.push(b["jobs"][0]["stage"].clone());
            b["jobs"][0]["note"] = "seen".into();
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
    }

    const BUSY: &str = "error code: 1102";

    /// Runs `cas` against `replies`, moving row `1999-a` from build to review. Returns the result,
    /// how many times the change ran, and the request methods.
    fn cas_review(
        name: &str,
        replies: Vec<(u16, &'static str, Vec<(&'static str, &'static str)>)>,
    ) -> (Res<()>, usize, Vec<String>) {
        let (origin, seen) = serve(replies);
        let home = home_for(name, &origin);
        let mut runs = 0;
        let result = cas(&home, |text| {
            runs += 1;
            let mut b = loads(text.unwrap()).unwrap();
            b["jobs"][0]["stage"] = "review".into();
            Ok((Some(dumps(&b, COMPACT)), ()))
        });
        let methods = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.method.clone())
            .collect();
        (result, runs, methods)
    }

    fn leak(jobs: Value) -> &'static str {
        Box::leak(board(jobs).into_boxed_str())
    }

    fn patchable() -> Vec<(&'static str, &'static str)> {
        vec![("ETag", "\"e1\""), ("Accept-Patch", "application/json")]
    }

    /// A PATCH that gets 503 reads again and, when the write had not landed, runs it again.
    #[test]
    fn a_503_patch_rereads_and_retries() {
        let build = leak(json!([{"id": "1999-a", "stage": "build"}]));
        let (result, runs, methods) = cas_review(
            "board-client-503-retry",
            vec![
                (200, build, patchable()),
                (503, BUSY, vec![]),
                (200, build, patchable()),
                (200, "ok", vec![("ETag", "\"e2\"")]),
            ],
        );
        result.unwrap();
        assert_eq!(runs, 2);
        assert_eq!(methods, ["GET", "PATCH", "GET", "PATCH"]);
    }

    /// A PATCH that got 503 after it landed returns once the re-read shows it, without a second run.
    #[test]
    fn a_503_write_that_landed_runs_once() {
        let build = leak(json!([{"id": "1999-a", "stage": "build"}]));
        let review = leak(json!([{"id": "1999-a", "stage": "review"}]));
        let (result, runs, methods) = cas_review(
            "board-client-503-landed",
            vec![
                (200, build, patchable()),
                (503, BUSY, vec![]),
                (200, review, patchable()),
            ],
        );
        result.unwrap();
        assert_eq!(runs, 1);
        assert_eq!(methods, ["GET", "PATCH", "GET"]);
    }

    /// Four 503s in a row fail as a single 503 did before the retry.
    #[test]
    fn four_503s_fail_with_exit_5() {
        let build = leak(json!([{"id": "1999-a", "stage": "build"}]));
        let mut replies = Vec::new();
        for _ in 0..4 {
            replies.push((200, build, patchable()));
            replies.push((503, BUSY, vec![]));
        }
        let (result, _, methods) = cas_review("board-client-503-fail", replies);
        let e = result.unwrap_err();
        assert_eq!(e.code, 5);
        assert!(e.stderr.contains("returned 503"), "{}", e.stderr);
        assert_eq!(methods.len(), 8);
    }

    /// A daemon that takes a board write and closes without a reply fails the write, and the
    /// change never runs twice, since the write may have landed.
    #[cfg(unix)]
    #[test]
    fn a_lost_daemon_reply_is_an_error_not_a_retry() {
        use std::os::unix::fs::DirBuilderExt;
        use std::os::unix::net::UnixListener;
        use std::time::{SystemTime, UNIX_EPOCH};
        let (origin, seen) = serve(vec![]);
        let long = home_for("board-client-lost", &origin);
        let home = (0..100)
            .find_map(|_| {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path =
                    PathBuf::from(format!("/tmp/factory-lost-{}-{nanos}", std::process::id()));
                match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => Some(path),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => None,
                    Err(e) => panic!("{}: {e}", path.display()),
                }
            })
            .expect("no free /tmp/factory-lost home after 100 tries");
        std::fs::copy(long.join("config.md"), home.join("config.md")).unwrap();
        std::fs::copy(long.join(".env"), home.join(".env")).unwrap();
        let home = std::fs::canonicalize(&home).unwrap();
        let listener = UnixListener::bind(socket_path(&home)).unwrap();
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let mut stream = stream.unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                if n == 0 {
                    stream
                        .write_all(b"{\"v\":1,\"ok\":true,\"etag\":\"\\\"e1\\\"\",\"patch\":true}\n{\"updated\":\"\",\"jobs\":[{\"id\":\"1999-a\"}]}\n")
                        .unwrap();
                }
            }
        });
        let mut runs = 0;
        let e = cas(&home, |text| {
            runs += 1;
            let mut b = loads(text.unwrap()).unwrap();
            b["jobs"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id": "2001-new"}));
            Ok((Some(dumps(&b, COMPACT)), ()))
        })
        .unwrap_err();
        assert_eq!((e.code, runs), (5, 1));
        assert!(e.stderr.contains("may have landed"), "{}", e.stderr);
        assert!(seen.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }
}
