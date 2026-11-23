//! claim and board, plus the claim helpers hold, handoff and other slices share. hold and handoff wait for the
//! sync slice.

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::cli::{not_ported, Call};
use crate::shared::board::{dump_board, load_board};
use crate::shared::consts::{JOB_ID, PASEO_ID, SLUG};
use crate::shared::exit::{fail, Res};
use crate::shared::home::{factory_home, holder};
use crate::shared::http::cas;
use crate::shared::py::{os_error, repr, strip, universal_newlines};
use crate::shared::pyjson::{dumps, DEFAULT};
use crate::shared::pyre::{Flags, Pattern};
use crate::shared::time::now;

static ID_DIGITS: Pattern = Pattern::new(r"(\d{3,})-", Flags::ASCII);

/// Give a session that has no tag a fresh one, and tell the orchestrator to reuse it.
pub(crate) fn new_session() {
    if std::env::var("FACTORY_SESSION").unwrap_or_default().is_empty() {
        let tag = format!("{:06x}", RandomState::new().build_hasher().finish() & 0xff_ffff);
        std::env::set_var("FACTORY_SESSION", &tag);
        eprintln!("new session {tag}: pass --session {tag} to every later claim, next, hold, and board call");
    }
}

/// This host's Paseo server id, else None.
fn paseo_server() -> Option<String> {
    let base = match std::env::var_os("PASEO_HOME").filter(|v| !v.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => std::env::home_dir()?.join(".paseo"),
    };
    let text = String::from_utf8(fs::read(base.join("server-id")).ok()?).ok()?;
    let server = strip(&universal_newlines(text)).to_string();
    PASEO_ID.is_full(&server).then_some(server)
}

/// {"server", "agent"} of the Paseo session running this process, else None.
pub(crate) fn paseo_session() -> Option<Value> {
    let agent = std::env::var("PASEO_AGENT_ID").unwrap_or_default();
    let agent = strip(&agent);
    let server = paseo_server()?;
    (PASEO_ID.is_full(agent) && !agent.starts_with('-')).then(|| json!({"server": server, "agent": agent}))
}

/// <home>/paseo-claims/<job-id>/<agent>, the marker that this host's Paseo session claimed the job.
pub(crate) fn paseo_claim_path(home: &Path, job_id: &str, agent: &str) -> Option<PathBuf> {
    let ok = JOB_ID.is_full(job_id) && !job_id.contains("..") && PASEO_ID.is_full(agent) && !agent.starts_with('-');
    ok.then(|| home.join("paseo-claims").join(job_id).join(agent))
}

pub(crate) fn record_paseo_claim(home: &Path, job_id: &str, paseo: &Value) {
    let Some(path) = paseo.get("agent").and_then(Value::as_str).and_then(|a| paseo_claim_path(home, job_id, a)) else {
        return;
    };
    if fs::create_dir_all(path.parent().unwrap()).is_ok() {
        let _ = fs::OpenOptions::new().create(true).append(true).open(&path);
    }
}

pub(crate) fn new_claim(device: &str, session: &str, since: Option<&str>) -> Value {
    let stamp = now();
    json!({"device": device, "session": session, "since": since.filter(|s| !s.is_empty()).unwrap_or(&stamp), "seen": stamp})
}

/// The highest id the Worker has archived off the board, so no new job reuses it.
pub(crate) fn archive_floor(data: &Value) -> u128 {
    let Some(Value::Number(n)) = data.get("archived").and_then(|a| a.get("max_id")) else { return 0 };
    let text = n.to_string();
    if text.bytes().all(|b| b.is_ascii_digit()) { text.parse().unwrap_or(u128::MAX) } else { 0 }
}

pub(crate) fn next_job_id(names: &[String], feature_slug: &str, floor: u128) -> String {
    let used = names.iter().filter_map(|n| ID_DIGITS.match_start(n)).map(|m| m[1].parse().unwrap_or(u128::MAX));
    format!("{:03}-{feature_slug}", used.chain([floor]).max().unwrap_or(0).saturating_add(1))
}

fn claim_stub(job_id: &str, project: &str, stamp: &str, claim: Value, paseo: Option<&Value>, refs: Option<Value>) -> Value {
    let mut stub = json!({"id": job_id, "project": project, "updated": stamp, "started": stamp, "claim": claim});
    if let Some(paseo) = paseo {
        stub["paseo"] = paseo.clone();
    }
    if let Some(refs) = refs.filter(|r| truthy(Some(r))) {
        stub["refs"] = refs;
    }
    stub
}

fn listing(folder: &Path) -> Vec<PathBuf> {
    fs::read_dir(folder).map(|d| d.filter_map(|e| Some(e.ok()?.path())).collect()).unwrap_or_default()
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Reserve the next id on the shared board with a stageless stub. Ids are unique across the whole factory, so
/// `#<id>` links and board lookups stay unambiguous. refs, when given, go on the stub.
pub(crate) fn claim_id(home: &Path, feature_slug: &str, project_slug: &str, refs: Option<Value>) -> Res<String> {
    for slug in [feature_slug, project_slug] {
        if !SLUG.is_full(slug) {
            return Err(fail(format!("bad slug {}: use letters, digits, - and _", repr(slug)), 2));
        }
    }
    let paseo = paseo_session();
    let resolved = if paseo.is_some() { Some(factory_home()?) } else { None };
    let jobs = home.join("jobs");
    fs::create_dir_all(&jobs).map_err(|e| fail(os_error(&e, Some(&jobs)), 1))?;
    let mut local = Vec::new();
    for entry in listing(&jobs) {
        local.push(name_of(&entry));
        local.extend(listing(&entry).iter().map(|p| name_of(p)));
    }
    new_session();
    let (device, session, _) = holder(home)?;
    let job_id = cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let rows = jobs_of(&data)?;
        let mut names = local.clone();
        names.extend(rows.iter().map(|j| py_str(j.get("id").unwrap_or(&json!("")))));
        let job_id = next_job_id(&names, feature_slug, archive_floor(&data));
        let stamp = now();
        data["updated"] = Value::from(stamp.clone());
        let stub = claim_stub(&job_id, project_slug, &stamp, new_claim(&device, &session, None), paseo.as_ref(), refs.clone());
        data["jobs"].as_array_mut().unwrap().push(stub);
        Ok((Some(dump_board(&data)), job_id))
    })?;
    if let (Some(paseo), Some(resolved)) = (&paseo, &resolved) {
        record_paseo_claim(resolved, &job_id, paseo);
    }
    let folder = jobs.join(project_slug).join(&job_id);
    fs::create_dir_all(&folder).map_err(|e| fail(os_error(&e, Some(&folder)), 1))?;
    Ok(job_id)
}

pub fn claim(call: &Call) -> Res<()> {
    println!("{}", claim_id(Path::new(&call.args[0]), &call.args[1], &call.args[2], None)?);
    Ok(())
}

/// The board's jobs list. Python raises when it is missing; the binary fails 1.
fn jobs_of(data: &Value) -> Res<&Vec<Value>> {
    data.get("jobs").and_then(Value::as_array).ok_or_else(|| fail("the board has no jobs list", 1))
}

pub fn board(_call: &Call) -> Res<()> {
    Err(not_ported("board"))
}

pub fn hold(_call: &Call) -> Res<()> {
    Err(not_ported("hold"))
}

pub fn handoff(_call: &Call) -> Res<()> {
    Err(not_ported("handoff"))
}

/// Python's `==` on two JSON values: bools and numbers compare as numbers, ints exactly, dicts by key set.
fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| py_eq(x, y)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        (Value::Bool(_) | Value::Number(_), Value::Bool(_) | Value::Number(_)) => match (py_int(a), py_int(b)) {
            (Some(x), Some(y)) => x == y,
            _ => py_float(a) == py_float(b),
        },
        _ => false,
    }
}

/// The digits of an int or bool, without a sign for zero, else None for a float.
fn py_int(v: &Value) -> Option<String> {
    let text = match v {
        Value::Bool(b) => return Some(u8::from(*b).to_string()),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if text.contains(['.', 'e', 'E']) {
        return None;
    }
    Some(if text == "-0" { "0".into() } else { text })
}

fn py_float(v: &Value) -> f64 {
    match v {
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// Python's `str()` of a JSON value, as an f-string prints it.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python's `repr()` of a JSON value.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(_) => dumps(v, DEFAULT),
        Value::String(s) => repr(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => {
            let items: Vec<String> = map.iter().map(|(k, v)| format!("{}: {}", repr(k), py_repr(v))).collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

/// Python truthiness of a value that may be missing.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
    }
}

type Scan = Result<usize, (&'static str, usize)>;

fn skip_ws(c: &[char], mut idx: usize) -> usize {
    while idx < c.len() && matches!(c[idx], ' ' | '\t' | '\n' | '\r') {
        idx += 1;
    }
    idx
}

/// The `json.JSONDecodeError` text CPython's C scanner raises for s, or "" when it would parse s.
/// Call it only after `pyjson::loads` fails.
fn py_json_error(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let found = if c.first() == Some(&'\u{feff}') {
        Err(("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0))
    } else {
        scan_value(&c, skip_ws(&c, 0)).and_then(|end| {
            let end = skip_ws(&c, end);
            if end == c.len() { Ok(end) } else { Err(("Extra data", end)) }
        })
    };
    let Err((msg, pos)) = found else { return String::new() };
    let lineno = c[..pos].iter().filter(|&&x| x == '\n').count() + 1;
    let colno = match c[..pos].iter().rposition(|&x| x == '\n') {
        Some(nl) => pos - nl,
        None => pos + 1,
    };
    format!("{msg}: line {lineno} column {colno} (char {pos})")
}

fn scan_value(c: &[char], idx: usize) -> Scan {
    let Some(&first) = c.get(idx) else { return Err(("Expecting value", idx)) };
    let rest = &c[idx..];
    let word = |w: &str| rest.len() >= w.chars().count() && rest.iter().zip(w.chars()).all(|(a, b)| *a == b);
    match first {
        '"' => scan_string(c, idx + 1),
        '{' => scan_object(c, idx + 1),
        '[' => scan_array(c, idx + 1),
        'n' if word("null") => Ok(idx + 4),
        't' if word("true") => Ok(idx + 4),
        'f' if word("false") => Ok(idx + 5),
        'N' if word("NaN") => Ok(idx + 3),
        'I' if word("Infinity") => Ok(idx + 8),
        '-' if word("-Infinity") => Ok(idx + 9),
        _ => scan_number(c, idx),
    }
}

fn scan_number(c: &[char], start: usize) -> Scan {
    let digit = |i: usize| c.get(i).is_some_and(char::is_ascii_digit);
    let len = c.len();
    let mut idx = start;
    if c[idx] == '-' {
        idx += 1;
        if idx >= len {
            return Err(("Expecting value", start));
        }
    }
    if ('1'..='9').contains(&c[idx]) {
        idx += 1;
        while digit(idx) {
            idx += 1;
        }
    } else if c[idx] == '0' {
        idx += 1;
    } else {
        return Err(("Expecting value", start));
    }
    if idx + 1 < len && c[idx] == '.' && digit(idx + 1) {
        idx += 2;
        while digit(idx) {
            idx += 1;
        }
    }
    if idx + 1 < len && matches!(c[idx], 'e' | 'E') {
        let e_start = idx;
        idx += 1;
        if idx + 1 < len && matches!(c[idx], '-' | '+') {
            idx += 1;
        }
        while digit(idx) {
            idx += 1;
        }
        if !digit(idx - 1) {
            idx = e_start;
        }
    }
    Ok(idx)
}

fn scan_string(c: &[char], mut end: usize) -> Scan {
    let begin = end - 1;
    let len = c.len();
    let hex = |range: std::ops::Range<usize>| c[range].iter().try_fold(0u32, |n, d| Some(n * 16 + d.to_digit(16)?));
    loop {
        let mut next = end;
        while next < len && !matches!(c[next], '"' | '\\') {
            if (c[next] as u32) <= 0x1f {
                return Err(("Invalid control character at", next));
            }
            next += 1;
        }
        if next == len {
            return Err(("Unterminated string starting at", begin));
        }
        let quote = c[next] == '"';
        next += 1;
        if quote {
            return Ok(next);
        }
        if next == len {
            return Err(("Unterminated string starting at", begin));
        }
        if c[next] != 'u' {
            end = next + 1;
            if !matches!(c[next], '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') {
                return Err(("Invalid \\escape", end - 2));
            }
            continue;
        }
        next += 1;
        end = next + 4;
        if end >= len {
            return Err(("Invalid \\uXXXX escape", next - 1));
        }
        let Some(high) = hex(next..end) else { return Err(("Invalid \\uXXXX escape", end - 5)) };
        if (0xd800..0xdc00).contains(&high) && end + 6 < len && c[end] == '\\' && c[end + 1] == 'u' {
            end += 6;
            let Some(low) = hex(end - 4..end) else { return Err(("Invalid \\uXXXX escape", end - 5)) };
            if !(0xdc00..0xe000).contains(&low) {
                end -= 6;
            }
        }
    }
}

fn scan_object(c: &[char], idx: usize) -> Scan {
    let mut idx = skip_ws(c, idx);
    if c.get(idx) != Some(&'}') {
        loop {
            if c.get(idx) != Some(&'"') {
                return Err(("Expecting property name enclosed in double quotes", idx));
            }
            idx = skip_ws(c, scan_string(c, idx + 1)?);
            if c.get(idx) != Some(&':') {
                return Err(("Expecting ':' delimiter", idx));
            }
            idx = skip_ws(c, scan_value(c, skip_ws(c, idx + 1))?);
            if c.get(idx) == Some(&'}') {
                break;
            }
            if c.get(idx) != Some(&',') {
                return Err(("Expecting ',' delimiter", idx));
            }
            idx = skip_ws(c, idx + 1);
        }
    }
    Ok(idx + 1)
}

fn scan_array(c: &[char], idx: usize) -> Scan {
    let mut idx = skip_ws(c, idx);
    if c.get(idx) != Some(&']') {
        loop {
            idx = skip_ws(c, scan_value(c, idx)?);
            if c.get(idx) == Some(&']') {
                break;
            }
            if c.get(idx) != Some(&',') {
                return Err(("Expecting ',' delimiter", idx));
            }
            idx = skip_ws(c, idx + 1);
        }
    }
    Ok(idx + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn py_json_error_matches_cpython() {
        for (text, want) in [
            ("nope", "Expecting value: line 1 column 1 (char 0)"),
            ("{\"a\" 1}", "Expecting ':' delimiter: line 1 column 6 (char 5)"),
            ("{} x", "Extra data: line 1 column 4 (char 3)"),
            ("\"\\q\"", "Invalid \\escape: line 1 column 2 (char 1)"),
            ("\"a\tb\"", "Invalid control character at: line 1 column 3 (char 2)"),
            ("\n\n  [é", "Expecting value: line 3 column 4 (char 5)"),
            ("[1, 2", "Expecting ',' delimiter: line 1 column 6 (char 5)"),
            ("{\"a\": 1,}", "Expecting property name enclosed in double quotes: line 1 column 9 (char 8)"),
            ("\"abc", "Unterminated string starting at: line 1 column 1 (char 0)"),
            ("\u{feff}{}", "Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)"),
            ("[", "Expecting value: line 1 column 2 (char 1)"),
            ("[1,]", "Expecting value: line 1 column 4 (char 3)"),
            ("1.e5", "Extra data: line 1 column 2 (char 1)"),
            ("\"\\u12\"", "Invalid \\uXXXX escape: line 1 column 3 (char 2)"),
            ("\"\\ud800\\uzzzz\"", "Invalid \\uXXXX escape: line 1 column 9 (char 8)"),
            ("[NaN, -Infinity, 1.5e3, \"\\ud800\\u0041\"]", ""),
        ] {
            assert_eq!(py_json_error(text), want, "{text:?}");
        }
    }

    #[test]
    fn py_eq_follows_python() {
        assert!(py_eq(&json!(1), &json!(true)) && py_eq(&json!(0.0), &json!(false)) && py_eq(&json!(1), &json!(1.0)));
        assert!(!py_eq(&json!(2), &json!(true)) && !py_eq(&json!("1"), &json!(1)) && !py_eq(&json!(null), &json!(false)));
        assert!(py_eq(&json!({"a": [1, {"b": null}]}), &json!({"a": [true, {"b": null}]})));
        assert!(!py_eq(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
        let big = serde_json::from_str::<Value>("123456789012345678901234567890").unwrap();
        assert!(py_eq(&big, &big.clone()) && !py_eq(&big, &serde_json::from_str("123456789012345678901234567891").unwrap()));
    }

    #[test]
    fn py_str_and_truthy_follow_python() {
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!("x")), "x");
        assert_eq!(py_str(&json!(1.0)), "1.0");
        assert_eq!(py_str(&json!({"a": [true, "it's", null]})), r#"{'a': [True, "it's", None]}"#);
        assert!(!truthy(None) && !truthy(Some(&json!(0))) && !truthy(Some(&json!(""))) && !truthy(Some(&json!({}))));
        assert!(truthy(Some(&json!(0.5))) && truthy(Some(&json!([0]))));
    }
}
