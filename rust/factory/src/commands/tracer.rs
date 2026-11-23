//! The Tracer mirror: `tracer sync`, the board hook and the pickup passes.

use std::path::Path;

use serde_json::{json, Value};

use crate::cli::{not_ported, Call};
use crate::shared::consts::NOTE_INVISIBLE;
use crate::shared::exit::{fail, Exit, Res};
use crate::shared::fs::read_text;
use crate::shared::py::{is_space, splitlines, strip};
use crate::shared::pyjson::{dumps, DEFAULT};
use crate::shared::pyre::{Flags, Pattern};

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

pub fn tracer_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("tracer"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;

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
