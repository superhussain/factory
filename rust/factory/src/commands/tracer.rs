//! The Tracer mirror: `tracer sync`, the board hook and the pickup passes.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use ureq::Agent;

use crate::cli::{not_ported, Call};
use crate::shared::consts::{NOTE_INVISIBLE, SLUG};
use crate::shared::exit::{fail, Exit, Res};
use crate::shared::fs::{read_text, regular_metadata};
use crate::shared::http::{api, urlopen_error};
use crate::shared::py::{is_space, repr, splitlines, strip};
use crate::shared::pyjson::{dumps, loads, DEFAULT};
use crate::shared::pyre::{Flags, Pattern};

const TRACER_TIMEOUT: Duration = Duration::from_secs(5);
/// Stages the Tracer mirror covers. idea and closed jobs are never mirrored.
const TRACER_STAGES: [&str; 7] = [
    "spec",
    "build",
    "review",
    "approve",
    "needs-human",
    "pr-open",
    "merged",
];
/// Stages where Follow in Tracer asks Tracer to gather scope first.
const TRACER_GATHER_STAGES: [&str; 2] = ["idea", "spec"];
pub(crate) const TRACER_GATE_NOTE: &str = "gate: tracer-scope";
pub(crate) const TRACER_APPROVED_NOTE: &str = "spec approved in Tracer";
pub(crate) const SPEC_APPROVED_NOTES: [&str; 2] =
    [TRACER_APPROVED_NOTE, "spec approved on the dashboard"];
const TRACER_STARTING: &str = "The companion is starting this build";
static TRACER_SIGNATURE: Pattern = Pattern::new(r"[A-Za-z0-9+/]{86}==", Flags::NONE);
const TRACER_BUILDER: &str = "factory-builder";
const TRACER_REVIEWERS: [&str; 4] = ["security", "ux", "ui", "code"];
static TRACER_SERVICE: Pattern = Pattern::new(r"[a-z0-9][a-z0-9._-]{0,39}", Flags::NONE);
static TRACER_TOKEN: Pattern = Pattern::new(r"[\x21-\x7e]{1,512}", Flags::NONE);
static TRACER_JIRA: Pattern = Pattern::new(
    r"(?<![A-Za-z0-9])[A-Za-z][A-Za-z0-9]*-[1-9][0-9]*",
    Flags::NONE,
);
static TRACER_ROUND_RUN: Pattern = Pattern::new(r"factory review round (\d+)\b", Flags::NONE);
const TRACER_TASKS_MAX: usize = 30;
static TRACER_TASK_ID: Pattern = Pattern::new(r"[A-Za-z0-9._-]{1,40}", Flags::NONE);
static TRACER_TASK_STATUS: Pattern = Pattern::new(r"[a-z_]{1,24}", Flags::NONE);
static TRACER_BIDI: Pattern = Pattern::new(
    "[\u{61c}\u{200e}\u{200f}\u{202a}-\u{202e}\u{2066}-\u{2069}]",
    Flags::NONE,
);
static SPEC_TASKS: Pattern = Pattern::new(
    r"^## (?:\d+\.\s*)?Tasks\b[^\n]*\n(.*?)(?=^## |\Z)",
    Flags {
        multiline: true,
        dotall: true,
        ..Flags::NONE
    },
);
static REPO_PATH: Pattern = Pattern::new(r"[A-Za-z0-9_./-]{1,200}", Flags::ASCII);
static TASK_ITEM: Pattern = Pattern::new(r"^\d+\.\s+", Flags::M);
static BACKTICKED: Pattern = Pattern::new(r"`([^`]*)`", Flags::NONE);
static TITLE_MARKS: Pattern = Pattern::new(r"[*`]", Flags::NONE);
static JOB_DIGITS: Pattern = Pattern::new(r"(\d{1,7})-", Flags::ASCII);

/// The Python exception names a malformed Tracer or board body raises, as `py_error` reports them.
const PY_KINDS: [&str; 6] = [
    "AttributeError",
    "TypeError",
    "KeyError",
    "UnicodeDecodeError",
    "PermissionError",
    "OSError",
];

/// Python truthiness.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
    }
}

/// Python `str()` of a JSON value; arrays and objects come out as JSON.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::String(s) => s.clone(),
        other => dumps(other, DEFAULT),
    }
}

/// `" ".join(s.split())`.
fn squash(s: &str) -> String {
    s.split(is_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `urllib.parse.quote(s, safe="")`.
fn quote_all(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Where Python raises an exception the caller reports by its type name.
fn py_error(kind: &str) -> Exit {
    fail(kind, 1)
}

/// The exception name a `py_error` carries.
fn py_kind(e: &Exit) -> Option<&str> {
    let kind = e.stderr.strip_prefix("factory.py: ")?.strip_suffix('\n')?;
    (e.code == 1 && PY_KINDS.contains(&kind)).then_some(kind)
}

fn chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `str(v or "").strip().upper()` for a ticket.
fn ticket_of(v: Option<&Value>) -> String {
    let v = v.filter(|v| truthy(v)).map(py_str).unwrap_or_default();
    strip(&v).to_uppercase()
}

fn stage_of(j: &Value) -> Option<&str> {
    j.get("stage").and_then(Value::as_str)
}

fn is_true(v: Option<&Value>) -> bool {
    v == Some(&Value::Bool(true))
}

/// `int(s, 10)` for a port: optional sign, ASCII digits, single underscores between digits.
fn parse_port(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let n: i64 = digits.replace('_', "").parse().ok()?;
    Some(if s.starts_with('-') { -n } else { n })
}

/// The Tracer base URL without a trailing slash: https, or http for localhost and 127.0.0.1,
/// with no credentials, query or fragment.
fn tracer_base(url: &str) -> Result<String, ()> {
    if url.is_empty() || url.chars().any(|c| c as u32 <= 32 || c as u32 == 127) {
        return Err(());
    }
    if url.contains(['\\', '?', '#']) {
        return Err(());
    }
    let (scheme, rest) = url.split_once(':').ok_or(())?;
    let mut scheme_chars = scheme.chars();
    if !scheme_chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !scheme_chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        return Err(());
    }
    let scheme = scheme.to_ascii_lowercase();
    let after = rest.strip_prefix("//").ok_or(())?;
    let netloc = &after[..after.find('/').unwrap_or(after.len())];
    if netloc.contains('@') || netloc.ends_with(':') || netloc.contains('[') != netloc.contains(']')
    {
        return Err(());
    }
    let (host, port) = match netloc.split_once('[') {
        Some((_, bracketed)) => {
            let (host, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            (host, after.split_once(':').map_or("", |(_, p)| p))
        }
        None => netloc.split_once(':').unwrap_or((netloc, "")),
    };
    let host = host.to_lowercase();
    if host.is_empty() {
        return Err(());
    }
    if !port.is_empty() && !matches!(parse_port(port), Some(1..=65535)) {
        return Err(());
    }
    if scheme != "https" && !(scheme == "http" && (host == "localhost" || host == "127.0.0.1")) {
        return Err(());
    }
    Ok(url.strip_suffix('/').unwrap_or(url).to_string())
}

struct SpecTask {
    title: String,
    what: String,
    path: Option<String>,
}

/// The numbered items under the spec's Tasks section.
fn spec_tasks(text: &str) -> Vec<SpecTask> {
    let text = format!("{text}\n");
    let Some(section) = SPEC_TASKS.search(&text) else {
        return Vec::new();
    };
    let section = section.get(1).map_or("", |m| m.as_str());
    let starts: Vec<_> = TASK_ITEM.find_iter(section).collect();
    let mut tasks = Vec::new();
    for (i, m) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(section.len(), |next| next.start());
        let item = &section[m.end()..end];
        let (first, rest) = strip(item).split_once('\n').unwrap_or((strip(item), ""));
        let mut path = None;
        for line in splitlines(item, false) {
            if strip(line).starts_with("- Files:") {
                path = BACKTICKED
                    .find_iter(line)
                    .map(|t| &t.as_str()[1..t.as_str().len() - 1])
                    .find(|t| REPO_PATH.is_full(t) && !t.starts_with('/') && !t.contains(".."))
                    .map(str::to_string);
                if path.is_some() {
                    break;
                }
            }
        }
        let title = TITLE_MARKS.replace_all(first, |_| String::new());
        let title = chars(strip(&title).trim_end_matches(['.', ':']), 120);
        let rest = strip(rest);
        tasks.push(SpecTask {
            title: if title.is_empty() {
                "Step".to_string()
            } else {
                title
            },
            what: chars(if rest.is_empty() { first } else { rest }, 1500),
            path,
        });
    }
    tasks
}

/// Each round whose four review files all carry a verdict, as (n, [(reviewer, verdict)]).
fn review_rounds(folder: &Path) -> Vec<(u64, Vec<(&'static str, String)>)> {
    let mut rounds = Vec::new();
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(digits) = name.strip_prefix("round-") else {
            continue;
        };
        let mut verdicts = Vec::new();
        for kind in TRACER_REVIEWERS {
            let Ok(text) = read_text(&entry.path().join(format!("review-{kind}.md"))) else {
                break;
            };
            let Some(first) = splitlines(&text, false).first().map(|l| strip(l)) else {
                break;
            };
            match first.strip_prefix("VERDICT: ") {
                Some(v @ ("PASS" | "CHANGES" | "SKIPPED")) => verdicts.push((kind, v.to_string())),
                _ => break,
            }
        }
        let n = digits
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| digits.parse::<u64>().ok())
            .flatten();
        if let Some(n) = n.filter(|_| verdicts.len() == TRACER_REVIEWERS.len()) {
            rounds.push((n, verdicts));
        }
    }
    rounds.sort_by_key(|r| r.0);
    rounds
}

/// (ticket, feature id): the Jira key when the board has one, else FACTORY-<n> from the job id's
/// leading digits, else the job id.
pub(crate) fn tracer_ids(job: &Value) -> Res<(String, String)> {
    if let Some(jira) = job.get("jira").and_then(Value::as_str) {
        if TRACER_JIRA.is_full(jira) {
            return Ok((jira.to_uppercase(), jira.to_lowercase()));
        }
    }
    let id = match job.get("id") {
        None => return Err(py_error("KeyError")),
        Some(Value::String(id)) => id,
        Some(_) => return Err(py_error("TypeError")),
    };
    if let Some(digits) = JOB_DIGITS.match_start(id) {
        let n: u64 = digits[1].parse().unwrap();
        return Ok((format!("FACTORY-{n}"), format!("factory-{n}")));
    }
    Ok((id.to_uppercase(), id.clone()))
}

/// Tracer's ticket text for a message: control, invisible and bidirectional characters removed,
/// or 'no ticket'.
fn tracer_ticket_label(v: &Value) -> String {
    let text = if truthy(v) { py_str(v) } else { String::new() };
    let text = NOTE_INVISIBLE.replace_all(&text.replace('\n', ""), |_| String::new());
    match strip(&text) {
        "" => "no ticket".to_string(),
        label => label.to_string(),
    }
}

/// Tracer task statuses for a job's stage, or None when the stage is not mirrored. existing is
/// each task's current Tracer status, or null where it has none.
fn task_statuses(
    job: &Value,
    folder: &Path,
    count: usize,
    existing: &[Value],
) -> Option<Vec<&'static str>> {
    let stage = stage_of(job).filter(|s| TRACER_STAGES.contains(s))?;
    let has_round = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("round-"));
    if stage == "build" && !has_round {
        let mut current = false;
        let statuses = (0..count)
            .map(|i| {
                if existing.get(i).and_then(Value::as_str) == Some("done") {
                    "done"
                } else if !current {
                    current = true;
                    "in_progress"
                } else {
                    "not_started"
                }
            })
            .collect();
        return Some(statuses);
    }
    let status = match stage {
        "spec" => "not_started",
        "build" => "in_progress",
        "review" | "approve" => "verifying",
        "needs-human" => "blocked",
        "merged" => "done",
        _ if is_true(job.get("escalated")) => "blocked",
        _ => "verifying",
    };
    Some(vec![status; count])
}

/// True when the job's Tracer scope was approved on a gathered job whose queued Tracer build
/// is not stopped yet.
fn tracer_stop_due(j: &Value) -> bool {
    let Some(tracer) = j.get("tracer").filter(|t| t.is_object()) else {
        return false;
    };
    is_true(tracer.get("gather"))
        && tracer.get("scope").and_then(Value::as_str) == Some("approved")
        && !is_true(tracer.get("stopped"))
        && !matches!(stage_of(j), Some("merged" | "closed"))
}

/// True when pickup marked this job as held by a Tracer build agent.
pub(crate) fn tracer_building(j: &Value) -> bool {
    j.get("tracer")
        .filter(|t| t.is_object())
        .is_some_and(|t| is_true(t.get("building")))
}

/// True when the feature is at build, open, on this ticket, and an agent other than the
/// factory's own builds it in a session.
fn tracer_feature_building(feature: &Value, ticket: &str) -> bool {
    feature.is_object()
        && stage_of(feature) == Some("build")
        && !feature.get("closed").is_some_and(truthy)
        && ticket_of(feature.get("ticket")) == ticket
        && feature
            .get("agents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|a| {
                a.is_object()
                    && stage_of(a) == Some("build")
                    && a.get("kind").and_then(Value::as_str) == Some("session")
                    && a.get("id").and_then(Value::as_str) != Some(TRACER_BUILDER)
            })
}

/// Up to TRACER_TASKS_MAX {id, title, status} from the feature's tasks, in Tracer's order.
fn tracer_task_summary(feature: &Value) -> Vec<Value> {
    let Some(tasks) = feature.get("tasks").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut summary = Vec::new();
    for t in tasks {
        let id = t.get("id").and_then(Value::as_str);
        let status = t.get("status").and_then(Value::as_str);
        let (Some(id), Some(status)) = (id, status) else {
            continue;
        };
        if !TRACER_TASK_ID.is_full(id) || !TRACER_TASK_STATUS.is_full(status) {
            continue;
        }
        let title = match t.get("title") {
            Some(Value::String(title)) => chars(
                strip(&TRACER_BIDI.replace_all(title, |_| String::new())),
                80,
            ),
            _ => String::new(),
        };
        let title = if title.is_empty() { id } else { &title };
        summary.push(json!({"id": id, "title": title, "status": status}));
        if summary.len() == TRACER_TASKS_MAX {
            break;
        }
    }
    summary
}

/// True only when the all-projects pipeline document sets tracer.enabled to true.
fn tracer_enabled(home: &Path) -> Res<bool> {
    let r = api(home, "GET", "/api/pipeline/global", None, &[200, 404], &[])?;
    if r.status == 404 {
        return Ok(false);
    }
    let doc = loads(&r.text()?).map_err(|_| fail("malformed all-projects pipeline document", 5))?;
    if !doc.is_object() {
        return Err(py_error("AttributeError"));
    }
    match doc.get("tracer").filter(|t| truthy(t)) {
        None => Ok(false),
        Some(Value::Object(t)) => Ok(is_true(t.get("enabled"))),
        Some(_) => Err(py_error("AttributeError")),
    }
}

#[derive(Clone, Debug, PartialEq)]
struct TracerCfg {
    url: String,
    token_file: String,
    service: String,
    language: String,
}

/// Each project's entry in <home>/tracer.json, none when the file is absent. With only, other
/// projects' entries are not validated.
fn read_tracer(home: &Path, only: Option<&str>) -> Res<BTreeMap<String, TracerCfg>> {
    let path = home.join("tracer.json");
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok(BTreeMap::new());
    }
    let bad = |what: &str| fail(format!("{}: {what}", path.display()), 2);
    let data = regular_metadata(&path)
        .ok()
        .and_then(|_| read_text(&path).ok())
        .and_then(|text| loads(&text).ok())
        .ok_or_else(|| bad("not a readable regular JSON file"))?;
    let projects = match &data {
        Value::Object(map)
            if map.len() == 2 && map.get("version").and_then(Value::as_i64) == Some(1) =>
        {
            map.get("projects").and_then(Value::as_object)
        }
        _ => None,
    }
    .ok_or_else(|| bad(r#"must be {"version": 1, "projects": {...}}"#))?;
    let mut out = BTreeMap::new();
    for (slug, entry) in projects {
        if only.is_some_and(|only| slug != only) {
            continue;
        }
        let Some(entry) = entry.as_object().filter(|_| SLUG.is_full(slug)) else {
            return Err(bad(&format!(
                "project {} must be a slug naming an object",
                repr(slug)
            )));
        };
        let known = ["url", "token_file", "service", "language"];
        if let Some(extra) = entry.keys().filter(|k| !known.contains(&k.as_str())).min() {
            return Err(bad(&format!("{slug}: unknown key {extra}")));
        }
        let text = |key: &str| entry.get(key).and_then(Value::as_str);
        for key in ["url", "token_file", "service"] {
            if text(key).is_none() {
                return Err(bad(&format!(
                    "{slug}.{key} is required and must be a string"
                )));
            }
        }
        let (url, token_file, service) = (
            text("url").unwrap(),
            text("token_file").unwrap(),
            text("service").unwrap(),
        );
        let url = tracer_base(url).map_err(|_| {
            bad(&format!("{slug}.url must be https, or http only for localhost or 127.0.0.1, with no credentials, query or fragment"))
        })?;
        if !Path::new(token_file).is_absolute() {
            return Err(bad(&format!("{slug}.token_file must be an absolute path")));
        }
        tracer_token(slug, Path::new(token_file))?;
        if !TRACER_SERVICE.is_full(service) {
            return Err(bad(&format!(
                "{slug}.service must match [a-z0-9][a-z0-9._-]{{0,39}}"
            )));
        }
        let language = match entry.get("language") {
            None => "",
            Some(Value::String(s)) if s.chars().count() <= 40 => s,
            Some(_) => {
                return Err(bad(&format!(
                    "{slug}.language must be a string of at most 40 characters"
                )))
            }
        };
        out.insert(
            slug.clone(),
            TracerCfg {
                url,
                token_file: token_file.to_string(),
                service: service.to_string(),
                language: language.to_string(),
            },
        );
    }
    Ok(out)
}

/// The one token in a project's token file.
fn tracer_token(slug: &str, path: &Path) -> Res<String> {
    if regular_metadata(path).is_ok() {
        if let Ok(text) = read_text(path) {
            let token = strip(&text);
            if TRACER_TOKEN.is_full(token) {
                return Ok(token.to_string());
            }
        }
    }
    Err(fail(
        format!("tracer token_file for {slug} must be a readable regular file holding one token"),
        2,
    ))
}

/// The Tracer client: no proxy, no redirects, a 5 second limit on each step.
fn tracer_agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let tls = TlsConfig::builder()
            .provider(TlsProvider::NativeTls)
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        let limit = Some(TRACER_TIMEOUT);
        Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .user_agent("factory")
            .accept("")
            .timeout_connect(limit)
            .timeout_send_request(limit)
            .timeout_send_body(limit)
            .timeout_recv_response(limit)
            .timeout_recv_body(limit)
            .tls_config(tls)
            .build()
            .new_agent()
    })
}

fn tracer_send(
    method: &str,
    url: &str,
    token: &str,
    data: Option<Vec<u8>>,
) -> Result<(u16, Vec<u8>), ureq::Error> {
    let request = ureq::http::Request::builder()
        .method(method)
        .uri(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");
    let response = match data {
        Some(data) => tracer_agent().run(request.body(data).map_err(ureq::Error::Http)?)?,
        None => tracer_agent().run(request.body(()).map_err(ureq::Error::Http)?)?,
    };
    let status = response.status().as_u16();
    let body = response
        .into_body()
        .with_config()
        .limit(u64::MAX)
        .read_to_vec()?;
    Ok((status, body))
}

/// (status, parsed JSON or null) from Tracer. Fails 5 on a network failure or a status outside
/// ok. The token goes only in the Authorization header and never in a message.
fn tracer_call(
    base: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
    ok: &[u16],
) -> Res<(u16, Value)> {
    let data = body.map(|b| dumps(b, DEFAULT).into_bytes());
    let (status, raw) =
        tracer_send(method, &format!("{base}/{path}"), token, data).map_err(|e| {
            fail(
                format!("can't reach Tracer at {base}: {}", urlopen_error(&e))
                    .replace(token, "[token]"),
                5,
            )
        })?;
    let text = String::from_utf8_lossy(&raw);
    if !ok.contains(&status) {
        let detail = match loads(&text) {
            Ok(Value::Object(map)) => map.get("detail").cloned(),
            _ => None,
        };
        let detail = match detail {
            None => text.to_string(),
            Some(Value::String(s)) => s,
            Some(other) => dumps(&other, DEFAULT),
        };
        let detail = chars(&squash(&detail.replace(token, "[token]")), 160);
        let hint = if status == 401 {
            " (check token_file)"
        } else {
            ""
        };
        return Err(fail(
            format!("Tracer {method} {path} returned {status}: {detail}{hint}"),
            5,
        ));
    }
    if strip(&text).is_empty() {
        return Ok((status, Value::Null));
    }
    let parsed = loads(&text).map_err(|_| {
        fail(
            format!("Tracer {method} {path} returned {status} with a body that is not JSON"),
            5,
        )
    })?;
    Ok((status, parsed))
}

pub fn tracer_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("tracer"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::{resolve, temp_dir};
    use serde_json::Map;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};

    const TOKEN: &str = "synthetic-token-0123456789";
    const RECEIPT_REFUSAL: &str = "a board write from a device cannot set the note \"spec approved in Tracer\" on \"001-sample\" without a Tracer-signed scope receipt for FACTORY-1. Nothing was written.";

    fn sample_job() -> Value {
        json!({"id": "001-sample", "project": "sample", "stage": "spec", "jira": "PLAN-1234",
               "title": "Sample feature"})
    }

    struct FakeState {
        board: Value,
        board_writes: u32,
        pipeline: Option<Value>,
        refuse_receipt: bool,
        features: Map<String, Value>,
        requests: Vec<(String, String, Value)>,
        token: String,
        scope_locked: bool,
        delay: Duration,
        stop_answer: (u16, Value),
        answer: Option<(u16, Value)>,
    }

    /// The factory Worker and, under /tracer, a Tracer stand-in on one loopback port. The
    /// listener lives for the whole test process, so its port and pipeline cache stay its own.
    struct Fake {
        origin: String,
        state: Arc<Mutex<FakeState>>,
    }

    impl Fake {
        fn new(pipeline: Option<Value>) -> Fake {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
            let state = Arc::new(Mutex::new(FakeState {
                board: json!({"jobs": [sample_job()]}),
                board_writes: 0,
                pipeline,
                refuse_receipt: false,
                features: Map::new(),
                requests: Vec::new(),
                token: TOKEN.to_string(),
                scope_locked: false,
                delay: Duration::ZERO,
                stop_answer: (409, json!({"detail": "No build is running."})),
                answer: None,
            }));
            let shared = state.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let state = shared.clone();
                    std::thread::spawn(move || serve(stream, &state));
                }
            });
            Fake { origin, state }
        }

        fn on() -> Fake {
            Fake::new(Some(json!({"schema": 1, "tracer": {"enabled": true}})))
        }

        fn state(&self) -> MutexGuard<'_, FakeState> {
            self.state.lock().unwrap()
        }

        fn url(&self) -> String {
            format!("{}/tracer", self.origin)
        }

        fn job(&self) -> Value {
            self.state().board["jobs"][0].clone()
        }

        fn feature(&self, id: &str) -> Value {
            self.state().features[id].clone()
        }

        fn requests(&self) -> Vec<(String, String, Value)> {
            self.state().requests.clone()
        }
    }

    fn serve(mut stream: TcpStream, state: &Mutex<FakeState>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.is_empty() {
            return;
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (
            parts.next().unwrap().to_string(),
            parts.next().unwrap().to_string(),
        );
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            reader.read_line(&mut h).unwrap();
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            let (k, v) = h.split_once(':').unwrap();
            headers.push((k.to_ascii_lowercase(), v.trim().to_string()));
        }
        let header = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        let length = header("content-length").map_or(0, |v| v.parse().unwrap());
        let mut raw = vec![0; length];
        reader.read_exact(&mut raw).unwrap();
        let (status, text, etag) = match path.strip_prefix("/tracer") {
            Some(path) => {
                let body = if raw.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&raw).unwrap()
                };
                let delay = {
                    let mut s = state.lock().unwrap();
                    s.requests
                        .push((method.clone(), path.to_string(), body.clone()));
                    s.delay
                };
                std::thread::sleep(delay);
                let auth = header("authorization");
                let (status, out) = answer(
                    &mut state.lock().unwrap(),
                    &method,
                    path,
                    auth.as_deref(),
                    &body,
                );
                (status, out.to_string(), false)
            }
            None => worker(&mut state.lock().unwrap(), &method, &path, &raw),
        };
        let etag = if etag { "ETag: \"x\"\r\n" } else { "" };
        let head = format!(
            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n{etag}\r\n",
            text.len()
        );
        let _ = stream.write_all(format!("{head}{text}").as_bytes());
    }

    fn worker(s: &mut FakeState, method: &str, path: &str, raw: &[u8]) -> (u16, String, bool) {
        match (method, path) {
            ("GET", "/api/pipeline/global") => match &s.pipeline {
                None => (404, "not found".to_string(), false),
                Some(p) => (200, p.to_string(), false),
            },
            ("GET", "/api/board") => (200, s.board.to_string(), true),
            ("PUT", "/api/board") => {
                let board: Value = serde_json::from_slice(raw).unwrap();
                let receipt = board["jobs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|j| j.get("tracer").and_then(|t| t.get("receipt")).is_some());
                if s.refuse_receipt && receipt {
                    return (400, RECEIPT_REFUSAL.to_string(), false);
                }
                s.board = board;
                s.board_writes += 1;
                (200, String::new(), true)
            }
            _ => (404, String::new(), false),
        }
    }

    /// FakeTracer.answer from test_tracer.py.
    fn answer(
        s: &mut FakeState,
        method: &str,
        path: &str,
        auth: Option<&str>,
        body: &Value,
    ) -> (u16, Value) {
        if let Some(fixed) = &s.answer {
            return fixed.clone();
        }
        if auth != Some(format!("Bearer {}", s.token).as_str()) {
            return (401, json!({"detail": "bad token"}));
        }
        let Some(rest) = path.strip_prefix("/api/") else {
            return (404, json!({"detail": "no route"}));
        };
        let parts: Vec<&str> = rest.split('/').collect();
        if parts == ["features"] {
            if method == "GET" {
                return (200, Value::Array(s.features.values().cloned().collect()));
            }
            let id = body["id"].as_str().unwrap().to_string();
            let held = s.features.get(&id).cloned();
            if held.as_ref().is_some_and(|h| truthy(&h["ticket"]))
                || s.features.values().any(|f| f["ticket"] == body["ticket"])
            {
                return (409, json!({"detail": "exists"}));
            }
            let mut feature = held.unwrap_or(json!({"tasks": [], "agents": []}));
            for (k, v) in body.as_object().unwrap() {
                feature[k] = v.clone();
            }
            s.features.insert(id, feature.clone());
            return (201, feature);
        }
        let (scope_locked, stop_answer) = (s.scope_locked, s.stop_answer.clone());
        let Some(feature) = s.features.get_mut(parts[1]) else {
            return (404, json!({"detail": "no feature"}));
        };
        if parts.len() == 2 {
            return (200, feature.clone());
        }
        if parts[2] == "agents" {
            let agents = feature["agents"].as_array_mut().unwrap();
            agents.retain(|a| a["id"] != parts[3]);
            let mut agent = body.clone();
            agent["id"] = json!(parts[3]);
            agents.push(agent);
            return (200, body.clone());
        }
        if parts[2..] == ["build-runs", "stop"] {
            return stop_answer;
        }
        let tasks = feature["tasks"].as_array_mut().unwrap();
        let pos = tasks.iter().position(|t| t["id"] == parts[3]);
        if parts[2] == "tasks" && parts.len() == 4 {
            if method == "PUT" {
                let task = pos
                    .map(|i| tasks.remove(i))
                    .unwrap_or(json!({"id": parts[3], "test_runs": [], "agent": null}));
                let mut put = task.clone();
                for (k, v) in body.as_object().unwrap() {
                    put[k] = v.clone();
                }
                tasks.push(put);
                return (200, task);
            }
            if scope_locked {
                return (409, json!({"detail": "scope is not approved"}));
            }
            let task = &mut tasks[pos.unwrap()];
            for (k, v) in body.as_object().unwrap() {
                task[k] = v.clone();
            }
            return (200, task.clone());
        }
        if parts.get(4) == Some(&"test-results") {
            let runs = tasks[pos.unwrap()]["test_runs"].as_array_mut().unwrap();
            let mut run = body.clone();
            run["id"] = json!(format!("r{}", runs.len() + 1));
            runs.push(run);
            return (201, body.clone());
        }
        (404, json!({"detail": "no route"}))
    }

    struct TempHome {
        path: PathBuf,
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    impl TempHome {
        fn folder(&self) -> PathBuf {
            self.path.join("jobs/sample/001-sample")
        }

        fn token_file(&self) -> PathBuf {
            self.path.join("token")
        }

        fn configure(&self, url: &str) {
            let entry = json!({"url": url, "token_file": self.token_file().to_str().unwrap(),
                               "service": "sample", "language": "Python"});
            let doc = json!({"version": 1, "projects": {"sample": entry}});
            std::fs::write(self.path.join("tracer.json"), doc.to_string()).unwrap();
        }

        fn review(&self, n: u32, verdicts: &[(&str, &str)]) {
            let d = self.folder().join(format!("round-{n}"));
            std::fs::create_dir_all(&d).unwrap();
            for (kind, verdict) in verdicts {
                std::fs::write(
                    d.join(format!("review-{kind}.md")),
                    format!("VERDICT: {verdict}\n\nBody.\n"),
                )
                .unwrap();
            }
        }
    }

    fn home(fake: &Fake) -> TempHome {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("factory-tracer-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("jobs/sample/001-sample")).unwrap();
        let t = TempHome {
            path: resolve(&path).unwrap(),
        };
        std::fs::write(
            t.path.join("config.md"),
            format!("---\ndevice: test\napi_url: {}\n---\n", fake.origin),
        )
        .unwrap();
        std::fs::write(t.token_file(), format!("{TOKEN}\n")).unwrap();
        std::fs::write(t.folder().join("spec.md"), SPEC).unwrap();
        t.configure(&fake.url());
        t
    }

    /// An origin nothing listens on.
    fn dead_origin() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port())
    }

    #[test]
    fn config_validation() {
        let fake = Fake::on();
        let t = home(&fake);
        let path = t.path.join("tracer.json");
        std::fs::remove_file(&path).unwrap();
        assert!(read_tracer(&t.path, None).unwrap().is_empty());
        let token = t.token_file().to_str().unwrap().to_string();
        let write = |change: Value| {
            let mut entry = json!({"url": "https://tracer.example.invalid/base/", "token_file": token, "service": "sample"});
            for (k, v) in change.as_object().unwrap() {
                entry[k] = v.clone();
            }
            std::fs::write(
                &path,
                json!({"version": 1, "projects": {"sample": entry}}).to_string(),
            )
            .unwrap();
        };
        write(json!({"language": "Python"}));
        assert_eq!(
            read_tracer(&t.path, None).unwrap()["sample"],
            TracerCfg {
                url: "https://tracer.example.invalid/base".into(),
                token_file: token.clone(),
                service: "sample".into(),
                language: "Python".into(),
            }
        );
        let refused = |change: Value, needle: &str| {
            write(change);
            let e = read_tracer(&t.path, None).unwrap_err();
            assert_eq!(e.code, 2, "{}", e.stderr);
            assert!(e.stderr.contains(needle), "{}", e.stderr);
            if !needle.starts_with("tracer token_file") {
                assert!(e.stderr.contains(path.to_str().unwrap()));
            }
            assert!(!e.stderr.contains(TOKEN));
        };
        refused(json!({"service": "Bad Name"}), "sample.service");
        refused(json!({"language": "x".repeat(41)}), "sample.language");
        refused(
            json!({"token_file": t.path.join("missing").to_str().unwrap()}),
            "tracer token_file for sample",
        );
        std::fs::write(t.path.join("empty"), " \n").unwrap();
        refused(
            json!({"token_file": t.path.join("empty").to_str().unwrap()}),
            "tracer token_file for sample",
        );
        std::fs::write(t.path.join("spaced"), "two words\n").unwrap();
        refused(
            json!({"token_file": t.path.join("spaced").to_str().unwrap()}),
            "tracer token_file for sample",
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(t.token_file(), t.path.join("link")).unwrap();
            refused(
                json!({"token_file": t.path.join("link").to_str().unwrap()}),
                "tracer token_file for sample",
            );
        }
        std::fs::write(
            &path,
            json!({"version": 1, "projects": {"other": {"url": "ftp://x"}}}).to_string(),
        )
        .unwrap();
        assert!(read_tracer(&t.path, Some("sample")).unwrap().is_empty());
    }

    #[test]
    fn unset_setting_is_off() {
        for pipeline in [None, Some(json!({"schema": 1}))] {
            let fake = Fake::new(pipeline);
            assert!(!tracer_enabled(&home(&fake).path).unwrap());
        }
        let fake = Fake::on();
        assert!(tracer_enabled(&home(&fake).path).unwrap());
        let fake = Fake::new(Some(json!({"tracer": "on"})));
        let e = tracer_enabled(&home(&fake).path).unwrap_err();
        assert_eq!(py_kind(&e), Some("AttributeError"));
    }

    #[test]
    fn redirects_are_not_followed() {
        let fake = Fake::on();
        fake.state().answer = Some((302, json!({"detail": "elsewhere"})));
        let e = tracer_call(
            &fake.url(),
            TOKEN,
            "GET",
            "api/features/plan-1234",
            None,
            &[200, 404],
        )
        .unwrap_err();
        assert_eq!(e.code, 5);
        assert!(e.stderr.contains("returned 302: elsewhere"), "{}", e.stderr);
        assert_eq!(fake.requests().len(), 1);
    }

    #[test]
    fn environment_proxies_are_ignored() {
        let fake = Fake::on();
        assert!(tracer_agent().config().proxy().is_none());
        fake.state().answer = Some((200, json!([])));
        assert_eq!(
            tracer_call(&fake.url(), TOKEN, "GET", "api/features", None, &[200]).unwrap(),
            (200, json!([]))
        );
        assert_eq!(fake.requests().len(), 1);
    }

    #[test]
    fn http_client_errors_exit_5() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                let _ = BufReader::new(&stream).read_line(&mut line);
                drop(stream);
            }
        });
        let e = tracer_call(&base, TOKEN, "GET", "api/features", None, &[200]).unwrap_err();
        assert_eq!(e.code, 5);
        assert!(e.stderr.contains("can't reach Tracer"), "{}", e.stderr);
    }

    #[test]
    fn client_error_shapes() {
        let base = dead_origin();
        let e = tracer_call(&base, TOKEN, "GET", "api/features", None, &[200]).unwrap_err();
        assert_eq!(e.code, 5);
        assert!(
            e.stderr.contains(&format!("can't reach Tracer at {base}")),
            "{}",
            e.stderr
        );
        assert!(!e.stderr.contains(TOKEN));
        let fake = Fake::on();
        fake.state().answer = Some((500, json!({"detail": [1, 2]})));
        let e = tracer_call(&fake.url(), TOKEN, "GET", "api/x", None, &[200]).unwrap_err();
        assert_eq!(
            e.stderr,
            "factory.py: Tracer GET api/x returned 500: [1, 2]\n"
        );
        fake.state().answer = Some((401, json!(["no"])));
        let e = tracer_call(&fake.url(), TOKEN, "GET", "api/x", None, &[200]).unwrap_err();
        assert_eq!(
            e.stderr,
            "factory.py: Tracer GET api/x returned 401: [\"no\"] (check token_file)\n"
        );
    }

    const SPEC: &str = "# Spec: Sample feature

## Goal

Something.

## Tasks

1. **First step**
   - Files: `src/one.py` (new), `src/two.py`
   - Change: do one.

2. **Second step.**
   - Files: `/etc/passwd`, `../up.py`
   - Change: do two.

3. Third step:
   - Change: do three.

## Docs to update

Nothing.
";

    #[test]
    fn ticket_label() {
        for v in [json!(null), json!(""), json!(" \u{1b}\u{202e} ")] {
            assert_eq!(tracer_ticket_label(&v), "no ticket");
        }
        assert_eq!(
            tracer_ticket_label(&json!("PLAN-\u{1b}9\n\u{202e}")),
            "PLAN-9"
        );
        assert_eq!(tracer_ticket_label(&json!(42)), "42");
    }

    #[test]
    fn spec_tasks() {
        let tasks = super::spec_tasks(SPEC);
        let titles: Vec<_> = tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["First step", "Second step", "Third step"]);
        let paths: Vec<_> = tasks.iter().map(|t| t.path.as_deref()).collect();
        assert_eq!(paths, [Some("src/one.py"), None, None]);
        assert!(tasks[2].what.starts_with("- Change: do three."));
        let two = super::spec_tasks("# T\n\n## 6. Tasks\n\n1. One\n2. Two\n");
        let got: Vec<_> = two
            .iter()
            .map(|t| (t.title.as_str(), t.what.as_str(), t.path.is_none()))
            .collect();
        assert_eq!(got, [("One", "One", true), ("Two", "Two", true)]);
        assert_eq!(
            super::spec_tasks("## Tasks, in order\n\n1. One\n\n## Next\n\n1. Not a task\n").len(),
            1
        );
        assert!(super::spec_tasks("## Goal\n\nNo tasks here.\n").is_empty());
        assert_eq!(super::spec_tasks("## Tasks\n\n1. `*`\n")[0].title, "Step");
        let many = format!("1. x\n## Tasks\n\n1. y{}", "\n2. z".repeat(3));
        assert_eq!(super::spec_tasks(&many).len(), 4);
    }

    #[test]
    fn stage_statuses() {
        let folder = temp_dir("tracer-statuses");
        let job = json!({"id": "001-sample", "project": "sample"});
        let statuses = |stage: Value, existing: &[Value], escalated: bool| {
            let mut j = job.clone();
            j["stage"] = stage;
            if escalated {
                j["escalated"] = json!(true);
            }
            task_statuses(&j, &folder, 3, existing)
        };
        let all = |s: &'static str| Some(vec![s; 3]);
        assert_eq!(statuses(json!("spec"), &[], false), all("not_started"));
        assert_eq!(
            statuses(json!("build"), &[], false),
            Some(vec!["in_progress", "not_started", "not_started"])
        );
        let done = |s: &[&str]| s.iter().map(|v| json!(v)).collect::<Vec<_>>();
        assert_eq!(
            statuses(
                json!("build"),
                &[json!("done"), json!("done"), json!(null)],
                false
            ),
            Some(vec!["done", "done", "in_progress"])
        );
        assert_eq!(
            statuses(
                json!("build"),
                &done(&["done", "in_progress", "done"]),
                false
            ),
            Some(vec!["done", "in_progress", "done"])
        );
        assert_eq!(statuses(json!("review"), &[], false), all("verifying"));
        assert_eq!(statuses(json!("approve"), &[], false), all("verifying"));
        assert_eq!(statuses(json!("needs-human"), &[], false), all("blocked"));
        assert_eq!(statuses(json!("pr-open"), &[], false), all("verifying"));
        assert_eq!(statuses(json!("pr-open"), &[], true), all("blocked"));
        assert_eq!(statuses(json!("merged"), &[], false), all("done"));
        for stage in [json!("idea"), json!("closed"), json!(null)] {
            assert_eq!(statuses(stage, &[], false), None);
        }
        std::fs::create_dir(folder.join("round-1")).unwrap();
        assert_eq!(
            statuses(
                json!("build"),
                &[json!("done"), json!(null), json!(null)],
                false
            ),
            all("in_progress")
        );
    }

    #[test]
    fn tracer_ids() {
        let ids = |j: Value| super::tracer_ids(&j).unwrap();
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(
            ids(json!({"id": "001-a", "jira": "plan-12"})),
            pair("PLAN-12", "plan-12")
        );
        for jira in [
            json!(null),
            json!(""),
            json!("PLAN-0"),
            json!("12-34"),
            json!("PLAN-1 x"),
        ] {
            assert_eq!(
                ids(json!({"id": "001-a", "jira": jira})),
                pair("FACTORY-1", "factory-1")
            );
        }
        assert_eq!(
            ids(json!({"id": "042-sample-export"})),
            pair("FACTORY-42", "factory-42")
        );
        assert_eq!(ids(json!({"id": "abc"})), pair("ABC", "abc"));
        assert_eq!(
            py_kind(&super::tracer_ids(&json!({"id": 5})).unwrap_err()),
            Some("TypeError")
        );
    }

    #[test]
    fn task_summary() {
        assert!(tracer_task_summary(&json!({})).is_empty());
        assert!(tracer_task_summary(&json!({"tasks": "t1"})).is_empty());
        let x100 = "x".repeat(100);
        let got = tracer_task_summary(&json!({"tasks": [
            "t1", {"id": "t2", "status": "Done!"}, {"id": 3, "status": "done"},
            {"id": "bad id", "status": "done"}, {"id": "t4", "status": "done"},
            {"id": "t5", "title": "   ", "status": "done"}, {"id": "t6", "title": x100, "status": "done"},
            {"id": "t7", "title": "  Ship it  ", "status": "in_progress"}]}));
        assert_eq!(
            got,
            [
                json!({"id": "t4", "title": "t4", "status": "done"}),
                json!({"id": "t5", "title": "t5", "status": "done"}),
                json!({"id": "t6", "title": "x".repeat(80), "status": "done"}),
                json!({"id": "t7", "title": "Ship it", "status": "in_progress"}),
            ]
        );
        let tasks: Vec<_> = (0..35)
            .map(|n| json!({"id": format!("t{n}"), "title": "T", "status": "done"}))
            .collect();
        let many = tracer_task_summary(&json!({ "tasks": tasks }));
        let ids: Vec<_> = many.iter().map(|t| t["id"].as_str().unwrap()).collect();
        let want: Vec<_> = (0..30).map(|n| format!("t{n}")).collect();
        assert_eq!(ids, want);
    }

    #[test]
    fn task_summary_strips_bidi() {
        let got = tracer_task_summary(&json!({"tasks": [
            {"id": "t2", "title": "Write CSV \u{202e}encoder\u{2069}", "status": "in_progress"},
            {"id": "t9", "title": "\u{200f}\u{200e}", "status": "done"},
            {"id": "t10", "title": format!("{}{}", "\u{2066}".repeat(10), "x".repeat(80)), "status": "done"}]}));
        assert_eq!(
            got,
            [
                json!({"id": "t2", "title": "Write CSV encoder", "status": "in_progress"}),
                json!({"id": "t9", "title": "t9", "status": "done"}),
                json!({"id": "t10", "title": "x".repeat(80), "status": "done"}),
            ]
        );
    }

    #[test]
    fn tracer_base_table() {
        for (url, want) in [
            (
                "https://tracer.example.invalid/base/",
                "https://tracer.example.invalid/base",
            ),
            ("HTTPS://T.example.invalid", "HTTPS://T.example.invalid"),
            ("http://localhost:8080/", "http://localhost:8080"),
            ("http://LOCALHOST", "http://LOCALHOST"),
            (
                "https://t.example.invalid:+1",
                "https://t.example.invalid:+1",
            ),
            ("https://[::1]:8443/x", "https://[::1]:8443/x"),
            ("https://t.example.invalid//", "https://t.example.invalid/"),
        ] {
            assert_eq!(tracer_base(url).as_deref(), Ok(want), "{url}");
        }
        for url in [
            "",
            "http://tracer.example.invalid",
            "https://user@t.example.invalid",
            "https://t.example.invalid/?q=1",
            "https://t.example.invalid/#x",
            "https://t.example.invalid:0",
            "https://t.example.invalid:",
            "https://t.example.invalid:99999",
            "https://t.example.invalid:abc",
            "https:t.example.invalid",
            "https://t.example.invalid/a b",
            "https://t.example.invalid\\x",
            "ftp://t.example.invalid",
        ] {
            assert_eq!(tracer_base(url), Err(()), "{url}");
        }
    }

    #[test]
    fn python_helpers() {
        assert_eq!(quote_all("plan 1234/x~é"), "plan%201234%2Fx~%C3%A9");
        assert_eq!(squash(" a \n\t b\u{1c}c "), "a b c");
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!(1.5)), "1.5");
        assert_eq!(py_kind(&py_error("KeyError")), Some("KeyError"));
        assert_eq!(py_kind(&fail("KeyError", 5)), None);
        assert_eq!(py_kind(&fail("boom", 1)), None);
    }
}
