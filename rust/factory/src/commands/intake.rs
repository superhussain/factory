//! The intake commands: next, backlog, ideas, stuck, unclaim, idea-images, idea-files, deny-notes,
//! inbox, triage, suggest, control, needs, autonomy and halt.

use std::collections::BTreeSet;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use serde_json::{json, Value};

use crate::cli::{not_ported, Call};
use crate::shared::board::{dump_board, escalated, load_board, patch_job, read_board};
use crate::shared::consts::{
    tag_lookalike, BACKLOG_LINE, BACKLOG_NOTES_TAG, IDEA, JOB_ID, NOTES_SENTENCE, NOTE_INVISIBLE,
    NOTE_LINK, NOTE_PHRASES, NOTE_SECRETS, NOTE_TAG, TERMINAL,
};
use crate::shared::exit::{fail, Res};
use crate::shared::fs::{is_link, write_atomic};
use crate::shared::home::{factory_home, holder};
use crate::shared::http::{api, cas};
use crate::shared::project::{local_bytes, r2_target, safe_job_folder, sha};
use crate::shared::py::{os_error, repr, strip};
use crate::shared::pyjson::{dumps, float_repr, loads, DEFAULT, INDENT2};
use crate::shared::pyre::{Flags, Pattern};
use crate::shared::time::{now, parse_stamp, unix_now};
use crate::shared::unicode::nfkc;

const ASCII_M: Flags = Flags {
    ascii: true,
    multiline: true,
    ..Flags::NONE
};

static AUDIT_CONTROL: Pattern = Pattern::new(
    r"[\x00-\x1f\x7f-\x9f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]",
    Flags::NONE,
);
const AUTONOMY_CLASSES: &[&str] = &["internal", "visible"];
const AUTONOMY_COOLDOWN_DAYS: i64 = 14;
const AUTONOMY_DAYS: i64 = 90;
const AUTONOMY_DEFAULT: &str = r#"{"paused": true, "projects": [], "self": []}"#;
const AUTONOMY_MAX_CHANGES: f64 = 0.10;
const AUTONOMY_MAX_REWORK: f64 = 0.10;
const AUTONOMY_MAX_ROUNDS: f64 = 1.75;
const AUTONOMY_MIN_JOBS: usize = 20;
const AUTONOMY_PROMOTE_DAYS: i64 = 7;
const BACKLOG_CONTEXT_EXAMPLE: &str = "a context file looks like this:\nRecommend: approve\n\nWhy this line is worth building, in one or more lines.\n\n## Question: Which option?\nWhat the person decides.\nPick: one\n- Option A (recommended): what it does\n- Option B: what it does";
const BACKLOG_DECIDE_USAGE: &str = "usage: backlog decide <key> <approve|deny|rework> [--answers <file>] [--notes-file <file>] --chat-file <file>";
static BACKLOG_SUFFIX: Pattern = Pattern::new(
    r" \((rework-of|follow-up-of|sub-job-of|depends-on) (\w[\w.-]*)\)$",
    Flags::ASCII,
);
const BACKLOG_UNANSWERED: &str =
    "answer every question to approve or deny; pass --answers with one line per question";
const BACKLOG_USAGE: &str = "usage: backlog add \"<slug>: <feature>\" [--context <file>] | backlog rework <job-id> (feature on stdin) | backlog lesson <slug>/<job-id> --fact-file <file> --why-file <file>";
const CHAT_QUOTE_MAX: usize = 500;
static CLOSE_REASON: Pattern =
    Pattern::new(r"[^\x00-\x1f\x7f-\x9f\u2028\u2029]{1,200}", Flags::NONE);
static COMMIT: Pattern = Pattern::new("[0-9a-f]{40}", Flags::ASCII);
const CONTROL_ACTIONS: &[&str] = &[
    "prioritize",
    "unprioritize",
    "pause",
    "resume",
    "close",
    "babysit",
    "move",
    "rerun",
];
const CONTROL_USAGE: &str = "usage: control <job-id> <prioritize|unprioritize|pause|resume|babysit|rerun> --chat-file <file> | control <job-id> move <host> --chat-file <file> | control <job-id> close --reason-file <file> --chat-file <file>";
static DEPENDS_ON: Pattern = Pattern::new(r"^Depends on: ([\w-]+: \S.*)$", Flags::M);
const ENV_SECRET_MIN: usize = 12;
static GH_PR: Pattern = Pattern::new(
    r"https://github\.com/([\w.-]+/[\w.-]+)/pull/\d+",
    Flags::ASCII,
);
static HOST_NAME: Pattern = Pattern::new("[A-Za-z0-9][A-Za-z0-9._-]{0,63}", Flags::ASCII);
const HUMAN_KINDS: &[&str] = &[
    "dependency-upgrade",
    "migration",
    "security",
    "product-idea",
];
static IDEA_FILE_NAME: Pattern =
    Pattern::new("[0-9a-f]{64}\\.(pdf|csv|txt|mp4|webm)", Flags::ASCII);
static IDEA_IMAGE_FILE: Pattern = Pattern::new("[0-9a-f]{64}\\.png", Flags::ASCII);
const IDEA_LIMIT: usize = 4000;
static LESSON_MARK: Pattern = Pattern::new(
    r"^Lesson: from \w[\w.-]{0,80}, queued \d{4}-\d\d-\d\d$",
    ASCII_M,
);
const MAX_EXPLORER_PROJECTS: usize = 1;
const MAX_OPEN_PER_PROJECT: usize = 5;
const MAX_PER_DAY: usize = 10;
const MAX_PER_RUN: usize = 5;
const MAX_SUGGESTION_BYTES: usize = 65536;
const NEEDS_ANSWER_USAGE: &str = "usage: needs answer <job-id> --answers <file> --chat-file <file>";
const NEEDS_SKIPPED: &str = "answer every question; needs answer takes one line per question";
static NOT_BEFORE: Pattern = Pattern::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", Flags::NONE);
const PRIORITY_STAGES: &[&str] = &[IDEA, "spec", "build", "review", "approve"];
static REPO_PATH: Pattern = Pattern::new("[A-Za-z0-9_./-]{1,200}", Flags::ASCII);
static RETRO_MARK: Pattern = Pattern::new(
    r"^Retro: run [A-Za-z0-9_-]{1,64}, fingerprint [0-9a-f]{16}, created \d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$",
    Flags::M,
);
static RETRO_SESSION_REF: Pattern = Pattern::new(
    "[A-Za-z0-9][A-Za-z0-9._-]{0,63}/[0-9a-f-]{8,36}",
    Flags::ASCII,
);
const RULE_HOLD_REASONS: &[&str] = &[
    "its author recommends deny",
    "it asks questions a person must answer",
];
static RUN_TAG: Pattern = Pattern::new("[A-Za-z0-9_-]{1,64}", Flags::ASCII);
const SENSITIVE_PATHS: &[&str] = &[
    ".github/*",
    "*/.github/*",
    "*.gitlab-ci*",
    "*.circleci/*",
    "*buildkite*",
    "*jenkinsfile*",
    "skills/*",
    "*/skills/*",
    "*agents/*",
    "*schedules/*",
    "*runtime/*",
    "worker/*",
    "*auth*",
    "*access*",
    "*secret*",
    "*token*",
    "*credential*",
    "*.env*",
    "*deploy*",
    "*wrangler*",
    "*dockerfile*",
    "*.husky/*",
    "*migrations/*",
    "*.sql",
    "*package.json",
    "*pnpm-lock.yaml",
    "*package-lock.json",
    "*yarn.lock",
    "*requirements*.txt",
    "*pyproject.toml",
    "*poetry.lock",
    "*uv.lock",
    "*go.mod",
    "*go.sum",
    "*cargo.toml",
    "*cargo.lock",
    "*gemfile*",
    "mise.toml",
    ".node-version",
    ".npmrc",
];
const SPEC_APPROVED_NOTES: &[&str] = &["spec approved in Tracer", "spec approved on the dashboard"];
const STOP_CHECKS_FILE: &str = "stop-checks.json";
const SUGGESTION_SCHEMA: &str = "factory.suggestions/1";
const SUGGEST_APPROVE_USAGE: &str = "usage: suggest approve <job-id> <A|B|C> [--chat-file <file>]";
const SUGGEST_KINDS: &[&str] = &[
    "bug-fix",
    "test-gap",
    "docs",
    "cleanup",
    "dependency-upgrade",
    "migration",
    "security",
    "product-idea",
    "tooling",
];
const TRIAGE_BATCH_LIMIT: usize = 8;

/// `confirm(expected, prompt, what)`: a person at a terminal types expected, or the command fails 2.
type Confirm = dyn FnMut(&str, &str, &str) -> Res<()>;

/// Python's `human_confirm`: exit 2 unless a human at a terminal types expected.
fn human_confirm(expected: &str, prompt: &str, what: &str) -> Res<()> {
    if !std::io::stdin().is_terminal() {
        return Err(fail(
            format!("{what} needs an interactive terminal; a human runs it"),
            2,
        ));
    }
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| fail(os_error(&e, None), 1))?;
    let line = line.strip_suffix('\n').unwrap_or(&line);
    if strip(line) != expected {
        return Err(fail(
            format!("confirmation did not match {expected}; nothing was changed"),
            2,
        ));
    }
    Ok(())
}

fn home_and<T>(f: impl FnOnce(&Path) -> Res<T>) -> Res<T> {
    f(&factory_home()?)
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

fn get_str<'a>(j: &'a Value, key: &str) -> Option<&'a str> {
    j.get(key).and_then(Value::as_str)
}

/// Python's `repr` of a parsed JSON value.
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => repr(s),
        other => py_str(other),
    }
}

/// Python's `str` of a parsed JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            let text = n.to_string();
            if text.contains(['.', 'e', 'E']) {
                float_repr(text.parse().unwrap())
            } else if text == "-0" {
                "0".into()
            } else {
                text
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", repr(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `str(x)` of a value that may be missing, where missing reads as None.
fn py_opt(v: Option<&Value>) -> String {
    v.map_or_else(|| "None".into(), py_str)
}

fn parse(text: &str) -> Res<Value> {
    loads(text).map_err(|e| fail(format!("the Worker sent JSON that does not parse: {e}"), 1))
}

fn print_json(v: &Value) {
    println!("{}", dumps(v, INDENT2));
}

fn find_job<'a>(data: &'a mut Value, job_id: &str) -> Option<&'a mut Value> {
    data["jobs"]
        .as_array_mut()?
        .iter_mut()
        .find(|j| get_str(j, "id") == Some(job_id))
}

/// `v in list` for a value that may be missing and a list of strings.
fn has_str(list: &[&str], v: Option<&Value>) -> bool {
    v.and_then(Value::as_str).is_some_and(|s| list.contains(&s))
}

/// `urllib.parse.quote(x, safe="")`.
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

/// Length in UTF-16 code units, the way the dashboard's textarea maxlength counts.
fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// text cut to at most limit UTF-16 code units, without half a surrogate pair.
fn utf16_cut(text: &str, limit: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in text.chars() {
        used += c.len_utf16();
        if used > limit {
            break;
        }
        out.push(c);
    }
    out
}

/// text as a markdown blockquote, one > per line.
fn blockquote(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            if line.is_empty() {
                ">".to_string()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Python's `check_text`; value is None when it is not a string.
fn check_text(value: Option<&str>, lo: usize, hi: usize, multiline: bool) -> Option<String> {
    let Some(value) = value.filter(|v| (lo..=hi).contains(&v.chars().count())) else {
        return Some(format!("must be a string of {lo}-{hi} characters"));
    };
    if ["```", "~~~", "://"].iter().any(|t| value.contains(t)) {
        return Some("must not hold a code fence or a link".into());
    }
    if value.chars().any(|c| {
        let n = c as u32;
        (n < 32 || (127..160).contains(&n) || c == '\u{2028}' || c == '\u{2029}')
            && !(multiline && c == '\n')
    }) {
        return Some(format!(
            "must not hold control characters{}",
            if multiline { "" } else { " or line breaks" }
        ));
    }
    check_untrusted(value)
}

/// Whether text holds an operator-preferences tag as written or once NFKC and the look-alike fold apply.
fn has_envelope_tag(text: &str) -> bool {
    let folded: String = nfkc(text).chars().map(tag_lookalike).collect();
    NOTE_TAG.search(text).is_some() || NOTE_TAG.search(&folded).is_some()
}

/// The reason for the first admission rule (3-7) text breaks, or None.
fn check_untrusted(text: &str) -> Option<String> {
    if NOTE_INVISIBLE.search(text).is_some() {
        return Some("contains control, invisible or bidirectional characters".into());
    }
    if has_envelope_tag(text) {
        return Some("contains the reserved envelope tag".into());
    }
    if NOTE_LINK.search(text).is_some() {
        return Some("links are not allowed".into());
    }
    if NOTE_SECRETS.iter().any(|p| p.search(text).is_some()) {
        return Some("looks like a secret".into());
    }
    for (rule, p) in NOTE_PHRASES.iter() {
        if p.search(text).is_some() {
            return Some(format!(
                "reads like an instruction to override factory rules ({rule})"
            ));
        }
    }
    None
}

/// The set of registered project slugs.
fn registered(home: &Path) -> Res<BTreeSet<String>> {
    let listed = parse(&api(home, "GET", "/projects.json", None, &[200], &[])?.text()?)?;
    Ok(listed
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| py_str(&p["slug"]))
        .collect())
}

/// The board's autonomy record with defaults filled in. A malformed record counts as paused.
fn load_autonomy(data: &Value) -> Value {
    let mut record = loads(AUTONOMY_DEFAULT).unwrap();
    let stored = data.get("autonomy");
    let is_dict = stored.is_none_or(Value::is_object);
    if let Some(Value::Object(map)) = stored {
        for (k, v) in map {
            record[k] = v.clone();
        }
    }
    let lists_ok = ["projects", "self"].iter().all(|k| {
        record[*k]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
    });
    if !is_dict
        || !record["paused"].is_boolean()
        || !lists_ok
        || record["projects"].as_array().unwrap().len() > MAX_EXPLORER_PROJECTS
    {
        eprintln!("factory.py: the board's autonomy record is malformed, so autonomy counts as paused; run autonomy pause to rewrite it");
        return loads(AUTONOMY_DEFAULT).unwrap();
    }
    record
}

/// A board timestamp as seconds, or None when it is missing or unparseable.
fn stamp_of(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_str).and_then(parse_stamp)
}

/// The text of a file flag that carries person text, refused when it is a link, empty or not UTF-8.
fn read_chat_file(path: &str, flag: &str) -> Res<String> {
    if is_link(Path::new(path)).map_err(|e| fail(os_error(&e, Some(Path::new(path))), 1))? {
        return Err(fail(
            format!("{flag} {path} is a symlink or junction; nothing was changed"),
            6,
        ));
    }
    let text = std::fs::read(path)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(|| {
            fail(
                format!("{flag} {path}: cannot read it as UTF-8 text; nothing was changed"),
                2,
            )
        })?
        .replace("\r\n", "\n");
    if strip(&text).is_empty() {
        return Err(fail(
            format!("{flag} {path} is empty; nothing was changed"),
            2,
        ));
    }
    Ok(text)
}

/// `str.strip(chars)` for an ASCII set.
fn strip_set<'a>(s: &'a str, set: &str) -> &'a str {
    s.trim_matches(|c| set.contains(c))
}

static NON_SPACE: Pattern = Pattern::new(r"\S+", Flags::NONE);
static FENCE: Pattern = Pattern::new("`{3,}|~{3,}", Flags::NONE);
static HOST_CHARS: Pattern = Pattern::new("[A-Za-z0-9.-]*", Flags::NONE);
const HOST_TAIL: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-";

/// text with code fence markers dropped and each link shown as (link: <host>).
fn neutralize_chat(text: &str) -> String {
    let show = |m: &crate::shared::pyre::Captures| {
        let token = m.get(0).unwrap().as_str();
        let link = NOTE_LINK.search(token);
        if !token.contains("://") && link.is_none() {
            return token.to_string();
        }
        let rest = if let Some((_, after)) = token.split_once("://") {
            after
        } else {
            let head = &token[..link.unwrap().get(0).unwrap().start()];
            let kept = head.trim_end_matches(|c| HOST_TAIL.contains(c)).len();
            &token[kept..]
        };
        let rest = rest.split('/').next().unwrap();
        let rest = rest.rsplit('@').next().unwrap();
        let host = HOST_CHARS
            .match_start(rest)
            .map_or("", |c| c.get(0).unwrap().as_str())
            .to_lowercase();
        let host = strip_set(&host, ".-");
        let host = host.strip_prefix("www.").unwrap_or(host);
        if host.is_empty() {
            "(link)".to_string()
        } else {
            format!("(link: {host})")
        }
    };
    NON_SPACE.replace_all(&FENCE.replace_all(text, |_| String::new()), show)
}

/// The stripped chat message, None when it is blank, or exit 2 when it breaks a text rule.
fn chat_quote(text: Option<&str>, flag: &str) -> Res<Option<String>> {
    let text = neutralize_chat(strip(text.unwrap_or("")));
    let text = strip(&text);
    if text.is_empty() {
        return Ok(None);
    }
    if let Some(why) = check_text(Some(text), 1, CHAT_QUOTE_MAX, true) {
        return Err(fail(format!("{flag} {why}; nothing was changed"), 2));
    }
    Ok(Some(text.to_string()))
}

// ---- claims ----

fn mine(c: &Value, device: &str, session: &str) -> bool {
    get_str(c, "device") == Some(device) && get_str(c, "session") == Some(session)
}

/// The job's claim if another session holds it and checked in within the last minutes.
fn blocking(job: &Value, device: &str, session: &str, minutes: u32) -> Res<Option<Value>> {
    let Some(c) = job.get("claim").filter(|c| truthy(Some(c))) else {
        return Ok(None);
    };
    if mine(c, device, session) {
        return Ok(None);
    }
    let seen = stamp_of(c.get("seen")).ok_or_else(|| {
        fail(
            format!(
                "time data {} does not match format '%Y-%m-%dT%H:%M:%SZ'",
                py_opt(c.get("seen"))
            ),
            1,
        )
    })?;
    Ok((unix_now() - seen < i64::from(minutes) * 60).then(|| c.clone()))
}

fn held_by(job_id: &str, c: &Value) -> String {
    format!(
        "{job_id} is held by {} session {} since {}, last seen {}",
        py_opt(c.get("device")),
        py_opt(c.get("session")),
        py_opt(c.get("since")),
        py_opt(c.get("seen"))
    )
}

/// Drop this session's claim on an idea in one board write, so the idea waits again.
fn release_claim(home: &Path, job_id: &str, device: &str, session: &str) -> Res<()> {
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = find_job(&mut data, job_id) else {
            return Err(fail(format!("no job {job_id} on the board"), 4));
        };
        if get_str(job, "stage") != Some(IDEA) {
            return Err(fail(
                format!(
                    "{job_id} is at {}, not idea: nothing to release",
                    py_opt(job.get("stage"))
                ),
                6,
            ));
        }
        let claim = job.get("claim");
        if !truthy(claim) || !mine(claim.unwrap(), device, session) {
            return Err(fail(format!("{job_id} has no claim from this session"), 6));
        }
        patch_job(&mut data, job_id, &json!({"claim": null}));
        Ok((Some(dump_board(&data)), ()))
    })
}

// ---- unclaim ----

pub fn unclaim(call: &Call) -> Res<()> {
    home_and(|home| unclaim_in(home, &call.args))
}

fn unclaim_in(home: &Path, args: &[String]) -> Res<()> {
    let job_id = &args[0];
    let session = std::env::var("FACTORY_SESSION").unwrap_or_default();
    if session.is_empty() {
        return Err(fail(
            "unclaim needs --session <tag>, the tag ideas printed",
            2,
        ));
    }
    if !JOB_ID.is_full(job_id) || job_id.contains("..") {
        return Err(fail(format!("bad job id {}", repr(job_id)), 2));
    }
    release_claim(home, job_id, &holder(home)?.0, &session)
}

// ---- halt ----

pub fn halt_cmd(call: &Call) -> Res<()> {
    home_and(|home| halt_cmd_in(home, &call.args))
}

fn halt_cmd_in(home: &Path, args: &[String]) -> Res<()> {
    if args == ["status"] {
        let r = api(home, "GET", "/api/halt", None, &[200, 404], &[])?;
        if r.status == 404 {
            return Err(fail(
                "this Worker has no /api/halt route; deploy the Worker first",
                4,
            ));
        }
        print_json(&parse(&r.text()?)?);
        return Ok(());
    }
    if args.len() == 3 && args[0] == "set" && args[1] == "--reason" {
        if let Some(error) = check_text(Some(&args[2]), 1, 200, false) {
            return Err(fail(format!("--reason {error}"), 2));
        }
        let body = dumps(&json!({"halted": true, "reason": args[2]}), DEFAULT);
        let r = api(
            home,
            "PUT",
            "/api/halt",
            Some(body.as_bytes()),
            &[200],
            &[("Content-Type", "application/json")],
        )?;
        print_json(&parse(&r.text()?)?);
        return Ok(());
    }
    Err(fail("usage: halt status | halt set --reason <text>; only a signed-in person resumes the factory, from the dashboard", 2))
}

// ---- inbox ----

/// A job waiting on a person, as the dashboard's waitsOnYou reads it.
fn waits_on_you(j: &Value) -> bool {
    if has_str(TERMINAL, j.get("stage")) {
        return false;
    }
    get_str(j, "stage") == Some("needs-human")
        || get_str(j, "needs").is_some_and(|n| !strip(n).is_empty())
        || get_str(j, "stage") == Some("pr-open") && escalated(j)
}

/// The indented block under lines[i], with its indent and blank edge lines removed.
fn context_below(lines: &[&str], i: usize) -> String {
    let mut held: Vec<&str> = Vec::new();
    for below in &lines[i + 1..] {
        if !below.is_empty() && !below.starts_with("  ") {
            break;
        }
        held.push(below.get(2..).unwrap_or(""));
    }
    trim_blank_edges(&mut held);
    held.join("\n")
}

fn trim_blank_edges(held: &mut Vec<&str>) {
    while held.first().is_some_and(|l| strip(l).is_empty()) {
        held.remove(0);
    }
    while held.last().is_some_and(|l| strip(l).is_empty()) {
        held.pop();
    }
}

/// `[line.rstrip("\r") for line in text.split("\n")]`.
fn backlog_rows(text: &str) -> Vec<&str> {
    text.split('\n').map(|l| l.trim_end_matches('\r')).collect()
}

/// (raw line, slug, feature, context) for each well-formed unchecked backlog line.
fn unchecked_lines(text: &str) -> Vec<(String, String, String, String)> {
    let lines = backlog_rows(text);
    let mut found = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(m) = BACKLOG_LINE.match_start(line) else {
            continue;
        };
        found.push((
            line.to_string(),
            m[1].to_string(),
            m[2].to_string(),
            context_below(&lines, i),
        ));
    }
    found
}

/// The dashboard Inbox badge's number with every project shown.
fn inbox_count(jobs: &[Value], backlog_text: &str) -> usize {
    jobs.iter().filter(|j| waits_on_you(j)).count()
        + unchecked_lines(backlog_text)
            .iter()
            .filter(|(_, slug, _, _)| slug.is_ascii())
            .count()
}

static INBOX_ID: Pattern = Pattern::new("[A-Za-z0-9-]{1,128}", Flags::NONE);

pub fn inbox(_call: &Call) -> Res<()> {
    home_and(inbox_in)
}

fn inbox_in(home: &Path) -> Res<()> {
    let board = read_board(home)?;
    let jobs = board["jobs"].as_array().cloned().unwrap_or_default();
    let r = api(home, "GET", "/api/backlog", None, &[200, 404], &[])?;
    let text = if r.status == 200 {
        r.text()?
    } else {
        String::new()
    };
    let text_of = |v: Option<&Value>| {
        if truthy(v) {
            py_str(v.unwrap())
        } else {
            String::new()
        }
    };
    let waiting: Vec<Value> = jobs
        .iter()
        .filter(|j| get_str(j, "id").is_some_and(|id| INBOX_ID.is_full(id)) && waits_on_you(j))
        .take(500)
        .map(|j| {
            json!({"id": j["id"], "project": utf16_cut(&text_of(j.get("project")), 128),
                   "title": utf16_cut(&text_of(j.get("title")), 200)})
        })
        .collect();
    let out = json!({"count": inbox_count(&jobs, &text), "jobs": waiting});
    println!("{}", dumps(&out, DEFAULT));
    Ok(())
}

// ---- deny-notes ----

/// True when a closed, denied job carries notes nobody has read yet.
fn deny_notes_due(j: &Value) -> bool {
    let Some(backlog) = j.get("backlog").filter(|b| b.is_object()) else {
        return false;
    };
    if get_str(j, "stage") != Some("closed") || get_str(backlog, "decision") != Some("denied") {
        return false;
    }
    get_str(backlog, "notes").is_some_and(|n| !strip(n).is_empty())
        && backlog.get("notes_read").is_none()
}

/// Why a due job's notes may not reach a session, or None.
fn deny_notes_problem(j: &Value) -> Option<String> {
    let backlog = &j["backlog"];
    let notes = backlog["notes"].as_str().unwrap();
    if notes.chars().count() > 2000 {
        return Some("over 2000 characters".into());
    }
    let Some(line) = get_str(backlog, "line")
        .filter(|l| l.chars().count() <= 2000 && NOTE_INVISIBLE.search(l).is_none())
    else {
        return Some("has a malformed backlog line".into());
    };
    if BACKLOG_NOTES_TAG.search(line).is_some() || BACKLOG_NOTES_TAG.search(notes).is_some() {
        return Some("contains the reserved envelope tag".into());
    }
    if let Some(problem) = check_untrusted(notes) {
        return Some(problem);
    }
    check_untrusted(line).map(|p| format!("has an unsafe backlog line ({p})"))
}

fn deny_notes_prompt(j: &Value) -> String {
    let backlog = &j["backlog"];
    format!(
        "{NOTES_SENTENCE}\n\n<backlog-notes job=\"{}\" trust=\"data\">\nDenied item: {}\n\nNotes:\n{}\n</backlog-notes>",
        py_str(&j["id"]),
        py_str(&backlog["line"]),
        py_str(&backlog["notes"])
    )
}

pub fn deny_notes_cmd(call: &Call) -> Res<()> {
    home_and(|home| deny_notes_cmd_in(home, &call.args))
}

fn deny_notes_cmd_in(home: &Path, args: &[String]) -> Res<()> {
    let job_id = &args[0];
    let sub = args.get(1).map(String::as_str);
    if !JOB_ID.is_full(job_id) || job_id.contains("..") || !matches!(sub, None | Some("done")) {
        return Err(fail("usage: deny-notes <job-id> [done]", 2));
    }
    let due = |data: &mut Value| -> Res<Value> {
        let Some(j) = find_job(data, job_id) else {
            return Err(fail(
                format!("no job {job_id} on the board. Nothing was written."),
                4,
            ));
        };
        if !deny_notes_due(j) {
            return Err(fail(format!("{job_id} has no unread deny notes"), 2));
        }
        Ok(j.clone())
    };
    if sub.is_none() {
        let j = due(&mut read_board(home)?)?;
        if let Some(problem) = deny_notes_problem(&j) {
            return Err(fail(
                format!("deny notes on {job_id} {problem}; they stay unread"),
                5,
            ));
        }
        print_json(
            &json!({"id": job_id, "project": j.get("project").cloned().unwrap_or(Value::Null),
                           "line": j["backlog"]["line"], "prompt": deny_notes_prompt(&j)}),
        );
        return Ok(());
    }
    let (device, session, minutes) = holder(home)?;
    let result = cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let j = due(&mut data)?;
        if let Some(held) = blocking(&j, &device, &session, minutes)? {
            return Err(fail(
                format!("{}. Nothing was written.", held_by(job_id, &held)),
                6,
            ));
        }
        let stamp = now();
        let j = find_job(&mut data, job_id).unwrap();
        j["backlog"]["notes_read"] = Value::from(stamp.clone());
        j.as_object_mut().unwrap().shift_remove("claim");
        j["updated"] = Value::from(stamp.clone());
        let j = j.clone();
        data["updated"] = Value::from(stamp);
        Ok((Some(dump_board(&data)), j))
    })?;
    print_json(&result);
    Ok(())
}

// ---- idea-files and idea-images ----

/// The board job for target, refusing a missing job or one assigned elsewhere.
fn import_job(home: &Path, target: &str, slug: &str, job_id: &str) -> Res<Value> {
    let board = read_board(home)?;
    let Some(job) = board["jobs"]
        .as_array()
        .and_then(|jobs| jobs.iter().find(|j| get_str(j, "id") == Some(job_id)))
    else {
        return Err(fail(
            format!("no job {target} on the board. Nothing was written."),
            4,
        ));
    };
    if get_str(job, "project") != Some(slug) {
        return Err(fail(format!("{job_id} is not assigned to {slug}; project-less ideas have no job artifacts to import into"), 2));
    }
    Ok(job.clone())
}

/// The board's list of names, when it is 1 to 4 distinct strings that each fully match pattern.
fn import_names(names: &Value, pattern: &Pattern) -> Option<Vec<String>> {
    let names: Vec<String> = names
        .as_array()?
        .iter()
        .map(|n| n.as_str().filter(|n| pattern.is_full(n)).map(String::from))
        .collect::<Option<_>>()?;
    let distinct: BTreeSet<&String> = names.iter().collect();
    ((1..=4).contains(&names.len()) && distinct.len() == names.len()).then_some(names)
}

fn write_or_fail(path: &Path, data: &[u8]) -> Res<()> {
    write_atomic(path, data).map_err(|e| fail(os_error(&e, Some(path)), 1))
}

pub fn idea_files(call: &Call) -> Res<()> {
    home_and(|home| idea_files_in(home, &call.args[0]))
}

fn idea_files_in(home: &Path, target: &str) -> Res<()> {
    let (_, slug, job_id) = r2_target(target, true)?;
    let job = import_job(home, target, &slug, &job_id)?;
    if !truthy(job.get("idea_files")) {
        println!("{target} has no attached files; nothing to import");
        return Ok(());
    }
    let Some(names) = import_names(&job["idea_files"], &IDEA_FILE_NAME) else {
        return Err(fail(
            format!("{job_id}'s idea_files on the board is malformed; nothing was imported"),
            6,
        ));
    };
    let folder = safe_job_folder(home, &slug, &job_id)?;
    std::fs::create_dir_all(&folder).map_err(|e| fail(os_error(&e, Some(&folder)), 1))?;
    let mut got = Vec::new();
    for name in &names {
        let r = api(
            home,
            "GET",
            &format!("/api/idea-files/{name}"),
            None,
            &[200, 404],
            &[],
        )?;
        if r.status == 404 {
            return Err(fail(
                format!("idea file {name} is missing from the factory; nothing was written"),
                5,
            ));
        }
        if sha(&r.body) != name.split('.').next().unwrap() {
            return Err(fail(
                format!("idea file {name} does not match its name's SHA-256; nothing was written"),
                5,
            ));
        }
        got.push((folder.join(format!("idea-file-{name}")), r.body));
    }
    for (path, data) in &got {
        if local_bytes(path).is_some_and(|have| have != *data) {
            return Err(fail(format!("{} differs from the factory's copy; nothing was overwritten. Inspect and delete it, then retry", path.display()), 6));
        }
    }
    for (path, data) in &got {
        write_or_fail(path, data)?;
    }
    println!("imported {} attached file(s) into {target}", names.len());
    Ok(())
}

pub fn next_item(_call: &Call) -> Res<()> {
    Err(not_ported("next"))
}

pub fn backlog_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("backlog"))
}

pub fn ideas(_call: &Call) -> Res<()> {
    Err(not_ported("ideas"))
}

pub fn stuck(_call: &Call) -> Res<()> {
    Err(not_ported("stuck"))
}

pub fn idea_images(_call: &Call) -> Res<()> {
    Err(not_ported("idea-images"))
}

pub fn autonomy_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("autonomy"))
}

pub fn suggest_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("suggest"))
}

pub fn control_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("control"))
}

pub fn needs_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("needs"))
}

pub fn triage_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("triage"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_board() {
        assert_eq!(inbox_count(&[], ""), 0);
    }

    #[test]
    fn text_helpers_match_python() {
        assert_eq!(utf16_cut("x😀y", 2), "x");
        assert_eq!(utf16_cut("x😀y", 3), "x😀");
        assert_eq!(utf16_len("x😀"), 3);
        assert_eq!(quote_all("a b/é~"), "a%20b%2F%C3%A9~");
        assert_eq!(blockquote("a\n\nb"), "> a\n>\n> b");
        assert_eq!(
            py_str(&json!({"a": [true, null, 1.0, "x'"]})),
            "{'a': [True, None, 1.0, \"x'\"]}"
        );
        assert_eq!(
            check_text(Some("a\nb"), 1, 10, false).as_deref(),
            Some("must not hold control characters or line breaks")
        );
        assert_eq!(check_text(Some("a\nb"), 1, 10, true), None);
        assert_eq!(
            check_text(None, 1, 10, false).as_deref(),
            Some("must be a string of 1-10 characters")
        );
    }
}
