//! The intake commands: next, backlog, ideas, stuck, unclaim, idea-images, idea-files, deny-notes,
//! inbox, triage, suggest, control, needs, autonomy and halt.

use std::collections::BTreeSet;
use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use serde_json::{json, Value};

use crate::cli::{not_ported, Call};
use crate::shared::board::{dump_board, escalated, lead_of, load_board, patch_job, read_board};
use crate::shared::consts::{
    tag_lookalike, BACKLOG_LINE, BACKLOG_NOTES_TAG, IDEA, IN_FLIGHT, JOB_ID, NOTES_SENTENCE,
    NOTE_INVISIBLE, NOTE_LINK, NOTE_PHRASES, NOTE_SECRETS, NOTE_TAG, TERMINAL,
};
use crate::shared::exit::{fail, Res};
use crate::shared::fs::{is_link, write_atomic};
use crate::shared::home::{factory_home, holder};
use crate::shared::http::{api, cas};
use crate::shared::project::{local_bytes, r2_target, safe_job_folder, sha};
use crate::shared::py::{is_space, os_error, repr, rstrip, strip};
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

/// Push the imported files, then refuse when R2 does not hold them. Waits on the sync slice.
fn push_imported(
    _home: &Path,
    _target: &str,
    _slug: &str,
    _job_id: &str,
    _mine: &BTreeSet<String>,
) -> Res<()> {
    Err(not_ported("idea-images"))
}

pub fn idea_images(call: &Call) -> Res<()> {
    home_and(|home| idea_images_in(home, &call.args[0]))
}

fn idea_images_in(home: &Path, target: &str) -> Res<()> {
    let (_, slug, job_id) = r2_target(target, true)?;
    let job = import_job(home, target, &slug, &job_id)?;
    if !truthy(job.get("idea_images")) {
        println!("{target} has no pasted images; nothing to import");
        return Ok(());
    }
    let Some(names) = import_names(&job["idea_images"], &IDEA_IMAGE_FILE) else {
        return Err(fail(
            format!("{job_id}'s idea_images on the board is malformed; nothing was imported"),
            6,
        ));
    };
    let folder = safe_job_folder(home, &slug, &job_id)?;
    std::fs::create_dir_all(&folder).map_err(|e| fail(os_error(&e, Some(&folder)), 1))?;
    let mut got: Vec<(String, Vec<u8>)> = Vec::new();
    for name in &names {
        let r = api(
            home,
            "GET",
            &format!("/api/idea-images/{name}"),
            None,
            &[200, 404],
            &[],
        )?;
        if r.status == 404 {
            return Err(fail(
                format!("idea image {name} is missing from the factory; nothing was written"),
                5,
            ));
        }
        if sha(&r.body) != name[..name.len() - ".png".len()] {
            return Err(fail(
                format!("idea image {name} does not match its name's SHA-256; nothing was written"),
                5,
            ));
        }
        got.push((format!("idea-image-{name}"), r.body));
    }
    let md: String = got
        .iter()
        .enumerate()
        .map(|(i, (rel, _))| format!("![pasted image {}]({rel})\n", i + 1))
        .collect();
    got.push((
        "idea-images.md".into(),
        format!("Pasted image context for this idea:\n\n{md}").into_bytes(),
    ));
    for (rel, data) in &got {
        let path = folder.join(rel);
        let remote = format!("{}.remote", path.display());
        if Path::new(&remote).exists() {
            return Err(fail(format!("{remote} is unresolved; merge it into the file, delete the .remote, then retry"), 6));
        }
        if local_bytes(&path).is_some_and(|have| have != *data) {
            return Err(fail(format!("{} differs from the factory's copy; nothing was overwritten. Inspect and delete it, then retry", path.display()), 6));
        }
    }
    for (rel, data) in &got {
        write_or_fail(&folder.join(rel), data)?;
    }
    println!("imported {} pasted image(s) into {target}", names.len());
    let mine = got.into_iter().map(|(rel, _)| rel).collect();
    push_imported(home, target, &slug, &job_id, &mine)
}

pub fn autonomy_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("autonomy"))
}

pub fn suggest_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("suggest"))
}

// ---- answers to needs.md and backlog questions ----

struct Opt {
    label: String,
    desc: String,
    recommended: bool,
}

/// A question as the dashboard's parseNeeds or parseBacklogContext reads it.
struct Question {
    header: String,
    prompt: String,
    pick: String,
    options: Vec<Opt>,
    labels: Vec<String>,
}

/// One answered question: the picked labels and the other text. None is a "no answer" line.
type Answer = Option<(Vec<String>, String)>;

static ANSWER_ROW: Pattern = Pattern::new(r"- (.+?)(?::\s+(.+))?", Flags::NONE);
static RECOMMENDED: Pattern = Pattern::new(r"(.*) \(recommended\)", Flags::I);

/// `" ".join(s.split())`.
fn collapse(s: &str) -> String {
    s.split(is_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text.replace("\r\n", "\n").replace("\r", "\n")`.
fn newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The questions of a needs.md, as the dashboard's parseNeeds reads them, or [] when it has no
/// question form.
fn needs_questions(text: &str) -> Vec<Question> {
    let text = newlines(text);
    let mut sections: Vec<(Option<&str>, Vec<&str>)> = vec![(None, Vec::new())];
    for line in text.split('\n') {
        if line.starts_with("## ") {
            sections.push((Some(line), Vec::new()));
        } else {
            sections.last_mut().unwrap().1.push(line);
        }
    }
    let mut questions = Vec::new();
    for (head, lines) in sections {
        let Some(head) = head.filter(|h| h.starts_with("## Question:")) else {
            continue;
        };
        let header = strip(&head["## Question:".len()..]);
        let picks: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| l.starts_with("Pick:"))
            .collect();
        if questions.len() == 4
            || header.is_empty()
            || utf16_len(header) > 60
            || picks.len() != 1
            || !["Pick: one", "Pick: many"].contains(&picks[0])
        {
            return Vec::new();
        }
        let at = lines.iter().position(|l| *l == picks[0]).unwrap();
        let prompt = lines[..at]
            .iter()
            .copied()
            .filter(|l| !strip(l).is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let prompt = strip(&prompt).to_string();
        let mut options = Vec::new();
        for row in lines[at + 1..].iter().filter(|l| !strip(l).is_empty()) {
            let Some(m) = ANSWER_ROW.fullmatch(row) else {
                return Vec::new();
            };
            let first = m.get(1).unwrap().as_str();
            let rec = RECOMMENDED.fullmatch(first);
            options.push(Opt {
                label: rec
                    .as_ref()
                    .map_or(first, |r| r.get(1).unwrap().as_str())
                    .to_string(),
                desc: m.get(2).map_or("", |d| d.as_str()).to_string(),
                recommended: rec.is_some(),
            });
        }
        let pick = &picks[0]["Pick: ".len()..];
        if prompt.is_empty()
            || !(2..=4).contains(&options.len())
            || pick == "one" && options.iter().filter(|o| o.recommended).count() > 1
        {
            return Vec::new();
        }
        let labels = options.iter().map(|o| o.label.clone()).collect();
        questions.push(Question {
            header: header.to_string(),
            prompt,
            pick: pick.to_string(),
            options,
            labels,
        });
    }
    questions
}

/// The picks of a ", " separated answer. A whole label wins over the split, so a label that holds
/// ", " stays one pick; the longest label that ends the text or is followed by ", " is taken first.
fn split_picks(text: &str, labels: &[String]) -> Vec<String> {
    let mut picks = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        let mut label: Option<&str> = None;
        for x in labels {
            if (rest == x || rest.starts_with(&format!("{x}, ")))
                && label.is_none_or(|l| x.chars().count() > l.chars().count())
            {
                label = Some(x);
            }
        }
        let piece = label.unwrap_or_else(|| rest.split(", ").next().unwrap());
        if !piece.is_empty() {
            picks.push(piece.to_string());
        }
        i += piece.len() + 2;
    }
    picks
}

/// One answer per question from an answers file of "- <header>: <answer>" lines. Exits 2 when the
/// file does not fit the questions, and with skip_error on a "no answer" line when it is given.
fn parse_answers(path: &str, questions: &[Question], skip_error: Option<&str>) -> Res<Vec<Answer>> {
    let bad = |why: &str| fail(format!("--answers {path}: {why}"), 2);
    let text = std::fs::read(path)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(|| bad("cannot read it as UTF-8 text"))?
        .replace("\r\n", "\n");
    if text.chars().count() > 2000 {
        return Err(bad("longer than 2000 characters"));
    }
    if let Some(why) = check_untrusted(&text) {
        return Err(bad(&why));
    }
    let lines: Vec<&str> = text
        .split('\n')
        .filter(|l| !strip(l).is_empty())
        .map(rstrip)
        .collect();
    let headers: Vec<&str> = questions.iter().map(|q| q.header.as_str()).collect();
    let shape = format!(
        "write one line per question, as - <header>: <answer>, for: {}",
        if headers.is_empty() {
            "no questions".to_string()
        } else {
            headers.join(", ")
        }
    );
    if lines.len() != questions.len() {
        return Err(bad(&shape));
    }
    let mut out = Vec::new();
    for (q, line) in questions.iter().zip(lines) {
        let prefix = format!("- {}: ", q.header);
        let answer = line.strip_prefix(&prefix).map_or("", strip);
        if answer.is_empty() {
            return Err(bad(&shape));
        }
        if answer == "no answer" {
            if let Some(skip) = skip_error {
                return Err(fail(skip, 2));
            }
            out.push(None);
            continue;
        }
        let sep = if answer.starts_with("Other: ") {
            "Other: "
        } else {
            ". Other: "
        };
        let (picked, other) = answer.split_once(sep).unwrap_or((answer, ""));
        let picked = split_picks(picked, &q.labels);
        let other = collapse(other);
        for label in &picked {
            if !q.labels.contains(label) {
                return Err(bad(&format!(
                    "{label} is not an option of {}; pick from {}",
                    q.header,
                    q.labels.join(", ")
                )));
            }
        }
        let distinct: BTreeSet<&String> = picked.iter().collect();
        if distinct.len() != picked.len() || q.pick == "one" && picked.len() > 1 {
            return Err(bad(&if q.pick == "one" {
                format!("{} takes one pick", q.header)
            } else {
                format!("{} repeats a pick", q.header)
            }));
        }
        if picked.is_empty() && other.is_empty() {
            return Err(bad(&shape));
        }
        out.push(Some((picked, other)));
    }
    Ok(out)
}

/// A quote for a record line; a blank chat message reaches here only as Python's None.
fn quoted(quote: Option<&str>) -> Res<String> {
    quote
        .map(collapse)
        .ok_or_else(|| fail("'NoneType' object has no attribute 'split'", 1))
}

/// answers.md as the dashboard's answersMarkdown writes it, naming the chat message as its source.
fn needs_answers_markdown(
    questions: &[Question],
    picks: &[Answer],
    sha12: &str,
    stamp: &str,
    device: &str,
    quote: Option<&str>,
) -> Res<String> {
    let mut blocks = vec![
        "# Answers".to_string(),
        format!("For needs.md sha256 {sha12}, submitted {stamp} from chat by {device}."),
        format!("Chat message: \"{}\"", quoted(quote)?),
    ];
    for (i, (q, answer)) in questions.iter().zip(picks).enumerate() {
        let (picked, other) = answer.as_ref().unwrap();
        let rows: Vec<String> = q
            .options
            .iter()
            .map(|o| {
                format!(
                    "- [{}] {}{}{}",
                    if picked.contains(&o.label) { "x" } else { " " },
                    o.label,
                    if o.recommended { " (recommended)" } else { "" },
                    if o.desc.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", o.desc)
                    }
                )
            })
            .collect();
        blocks.extend([
            format!("## Question {}: {}", i + 1, q.header),
            q.prompt.clone(),
            format!("Pick: {}", q.pick),
            rows.join("\n"),
            if other.is_empty() {
                "Other: none".to_string()
            } else {
                format!("Other:\n> {other}")
            },
        ]);
    }
    Ok(blocks.join("\n\n") + "\n")
}

static ASCII_SLUG: Pattern = Pattern::new(r"[\w-]+", Flags::ASCII);

/// Submit a person's chat answers to a job's needs.md form with the dashboard's writes: answers.md
/// first, then the board's needs cleared unless the job is at needs-human.
fn needs_answer(home: &Path, job_id: &str, answers: &str, chat_file: &str) -> Res<()> {
    if !JOB_ID.is_full(job_id) || job_id.contains("..") {
        return Err(fail(NEEDS_ANSWER_USAGE, 2));
    }
    let quote = chat_quote(
        Some(&read_chat_file(chat_file, "--chat-file")?),
        "--chat-file",
    )?;
    let board = load_board(Some(
        &api(home, "GET", "/api/board", None, &[200], &[])?.text()?,
    ))?;
    let Some(job) = board["jobs"]
        .as_array()
        .and_then(|jobs| jobs.iter().find(|j| get_str(j, "id") == Some(job_id)))
    else {
        return Err(fail(
            format!("no job {job_id} on the board. Nothing was written."),
            4,
        ));
    };
    if !waits_on_you(job) {
        return Err(fail(
            format!("{job_id} is not waiting on you. Nothing was written."),
            2,
        ));
    }
    let project = truthy(job.get("project")).then(|| py_str(&job["project"]));
    if project.as_ref().is_some_and(|p| !ASCII_SLUG.is_full(p)) {
        return Err(fail(
            format!("{job_id} has a bad project on the board. Nothing was written."),
            2,
        ));
    }
    let base = format!(
        "/api/jobs/{}{job_id}/",
        project.map_or(String::new(), |p| format!("{p}/"))
    );
    let r = api(
        home,
        "GET",
        &format!("{base}needs.md"),
        None,
        &[200, 404],
        &[],
    )?;
    if r.status == 404 {
        return Err(fail(
            format!("{job_id} has no needs.md. Nothing was written."),
            4,
        ));
    }
    let text = r.text()?;
    let questions = needs_questions(&text);
    if questions.is_empty() {
        return Err(fail(
            format!("{job_id}'s needs.md has no question form. Nothing was written."),
            2,
        ));
    }
    let picks = parse_answers(answers, &questions, Some(NEEDS_SKIPPED))?;
    let sha12 = &sha(text.as_bytes())[..12];
    let body = needs_answers_markdown(
        &questions,
        &picks,
        sha12,
        &now(),
        &holder(home)?.0,
        quote.as_deref(),
    )?;
    cas(home, &format!("{base}answers.md"), |_| {
        Ok((Some(body.clone()), ()))
    })?;
    let needs = cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(j) = find_job(&mut data, job_id) else {
            return Ok((None, "kept"));
        };
        let needs = if truthy(j.get("needs")) {
            py_str(&j["needs"])
        } else {
            String::new()
        };
        if get_str(j, "stage") == Some("needs-human")
            || !needs.contains(&format!("#{job_id}/needs"))
        {
            return Ok((None, "kept"));
        }
        let stamp = now();
        j.as_object_mut().unwrap().shift_remove("needs");
        j["updated"] = Value::from(stamp.clone());
        data["updated"] = Value::from(stamp);
        Ok((Some(dump_board(&data)), "cleared"))
    })?;
    println!(
        "{}",
        dumps(
            &json!({"id": job_id, "answers": "saved", "needs": needs}),
            DEFAULT
        )
    );
    Ok(())
}

/// `dict(zip(keys, values))` for flag pairs: a repeated flag keeps its last value.
fn flag_pairs<'a>(keys: &'a [String], values: &'a [String]) -> Vec<(&'a str, &'a str)> {
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    for (k, v) in keys.iter().zip(values) {
        match pairs.iter_mut().find(|(key, _)| key == k) {
            Some(slot) => slot.1 = v,
            None => pairs.push((k, v)),
        }
    }
    pairs
}

/// Every other item of a list, from start.
fn every_other(list: &[String], start: usize) -> Vec<String> {
    list.iter().skip(start).step_by(2).cloned().collect()
}

/// The flags given as `dict(zip(list[start::2], list[start + 1::2]))`.
fn flags_of(list: &[String], start: usize) -> Vec<(String, String)> {
    let keys = every_other(list, start);
    let values = every_other(list, start + 1);
    flag_pairs(&keys, &values)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn flag<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// The flag names, sorted, to compare with Python's `set(flags)`.
fn flag_names(pairs: &[(String, String)]) -> BTreeSet<&str> {
    pairs.iter().map(|(k, _)| k.as_str()).collect()
}

pub fn needs_cmd(call: &Call) -> Res<()> {
    home_and(|home| needs_cmd_in(home, &call.args))
}

fn needs_cmd_in(home: &Path, args: &[String]) -> Res<()> {
    let (sub, rest) = (&args[0], &args[1..]);
    let flags = flags_of(rest, 1);
    if sub != "answer" || flag_names(&flags) != BTreeSet::from(["--answers", "--chat-file"]) {
        return Err(fail(NEEDS_ANSWER_USAGE, 2));
    }
    needs_answer(
        home,
        &rest[0],
        flag(&flags, "--answers").unwrap(),
        flag(&flags, "--chat-file").unwrap(),
    )
}

// ---- control ----

/// base with extra's keys added in order, as `{**base, **extra}`.
fn with(mut base: Value, extra: &Value) -> Value {
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

fn earlier_moves(m: &Value) -> Vec<Value> {
    m.get("earlier")
        .and_then(Value::as_array)
        .map(|e| e.iter().filter(|x| x.is_object()).cloned().collect())
        .unwrap_or_default()
}

/// True when pickup recorded this holder's stop. Only a string counts, as in the dashboard.
fn move_stopped(x: &Value) -> bool {
    x.get("stopped").is_some_and(Value::is_string)
}

/// The move's holders whose host has not stopped them yet, earlier ones first.
fn queued_stops(m: &Value) -> Vec<Value> {
    if !m.is_object() {
        return Vec::new();
    }
    earlier_moves(m)
        .into_iter()
        .chain([m.clone()])
        .filter(|x| truthy(x.get("from")) && !move_stopped(x))
        .collect()
}

/// (enabled, hosts) from the project's effective pipeline, or the reason it cannot be read.
fn pickup_config(home: &Path, slug: &str) -> Res<Result<(bool, Vec<Value>), String>> {
    let r = api(
        home,
        "GET",
        &format!("/api/pipeline/effective/{slug}"),
        None,
        &[200, 404, 409],
        &[],
    )?;
    if r.status == 404 {
        return Ok(Err(
            "no pipeline view; register the project or deploy the current Worker".into(),
        ));
    }
    if r.status == 409 {
        return Ok(Err(
            "stored pipeline configuration no longer validates".into()
        ));
    }
    let view = loads(&r.text()?).ok();
    if view
        .as_ref()
        .is_some_and(|v| v.is_object() && v.get("pickup").is_none())
    {
        return Ok(Ok((false, Vec::new())));
    }
    let p = view.as_ref().and_then(|v| v.get("pickup"));
    if let Some(p) = p.filter(|p| p.is_object()) {
        let hosts = p.get("hosts").and_then(Value::as_array);
        if let (Some(Value::Bool(enabled)), Some(hosts)) = (p.get("enabled"), hosts) {
            if hosts
                .iter()
                .all(|h| h.as_str().is_some_and(|h| HOST_NAME.is_full(h)))
            {
                return Ok(Ok((*enabled, hosts.clone())));
            }
        }
    }
    Ok(Err("malformed pickup in the pipeline view".into()))
}

/// (reason, on_host): why host would never pick up a job of slug, and whether the reason is the
/// host itself rather than the pickup view. None when it is active and pickup lists it.
fn pickup_host_problem(home: &Path, slug: &Value, host: &Value) -> Res<Option<(String, bool)>> {
    let hosts = parse(&api(home, "GET", "/api/hosts", None, &[200], &[])?.text()?)?;
    let active = hosts.as_array().into_iter().flatten().any(|h| {
        h.is_object() && h.get("id") == Some(host) && get_str(h, "state") == Some("active")
    });
    if !active {
        return Ok(Some((
            format!("{} is not an active host", py_str(host)),
            true,
        )));
    }
    match pickup_config(home, &py_str(slug))? {
        Err(why) => Ok(Some((why, false))),
        Ok((enabled, listed)) if !enabled || !listed.contains(host) => Ok(Some((
            format!("{} does not run pickup for {}", py_str(host), py_str(slug)),
            true,
        ))),
        Ok(_) => Ok(None),
    }
}

/// Move job to host to, as the dashboard's writeMove does, and drop its claim and Paseo link.
fn write_move(job: &mut Value, to: &Value, stamp: &str, record: &Value) {
    let empty = json!({});
    let claim = job
        .get("claim")
        .filter(|c| c.is_object())
        .cloned()
        .unwrap_or(json!({}));
    let m = job.get("move").filter(|m| m.is_object()).cloned();
    let prior = match &m {
        Some(m) if !truthy(Some(&claim)) && truthy(Some(m)) && !move_stopped(m) => m.clone(),
        _ => json!({}),
    };
    let or = |a: Option<&Value>, b: Option<&Value>| {
        if truthy(a) {
            a.cloned()
        } else {
            b.cloned()
        }
    };
    let origin = or(claim.get("device"), prior.get("from"));
    let session = or(claim.get("session"), prior.get("session"));
    let mut earlier = m.as_ref().map(earlier_moves).unwrap_or_default();
    if let Some(m) = m
        .as_ref()
        .filter(|m| truthy(Some(m)) && truthy(m.get("from")) && !truthy(Some(&prior)))
    {
        let mut entry = json!({});
        for k in ["from", "session", "agent", "server", "at"] {
            if truthy(m.get(k)) {
                entry[k] = m[k].clone();
            }
        }
        if move_stopped(m) {
            entry["stopped"] = m["stopped"].clone();
        }
        earlier.push(entry);
    }
    let paseo = job
        .get("paseo")
        .filter(|p| p.is_object())
        .unwrap_or(&empty)
        .clone();
    let link = if truthy(paseo.get("agent")) {
        &paseo
    } else {
        &prior
    };
    let mut moved = json!({});
    for (k, v) in [
        ("from", origin),
        ("session", session),
        ("agent", link.get("agent").cloned()),
        ("server", link.get("server").cloned()),
    ] {
        if truthy(v.as_ref()) {
            moved[k] = v.unwrap();
        }
    }
    moved["to"] = to.clone();
    moved["at"] = Value::from(stamp);
    let mut moved = with(moved, record);
    if !earlier.is_empty() {
        moved["earlier"] = Value::Array(earlier);
    }
    job["move"] = moved;
    let map = job.as_object_mut().unwrap();
    map.shift_remove("claim");
    map.shift_remove("paseo");
}

/// Make the dashboard's Babysit PR, Move or Rerun after the dashboard's checks, and print the result.
fn control_launch(
    home: &Path,
    job_id: &str,
    action: &str,
    host: Option<&str>,
    record: &Value,
) -> Res<()> {
    let mut problems: Vec<((Value, Value), Option<(String, bool)>)> = Vec::new();
    let word = match action {
        "babysit" => "babysat".to_string(),
        "move" => format!("moved to {}", host.unwrap()),
        _ => "rerun".to_string(),
    };
    let refuse = |why: &str| {
        fail(
            format!("{job_id} cannot be {word}: {why}. Nothing was written."),
            2,
        )
    };
    let (result, to) = cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = find_job(&mut data, job_id) else {
            return Err(fail(
                format!("no job {job_id} on the board. Nothing was written."),
                4,
            ));
        };
        if action == "babysit" && truthy(job.get("babysit")) {
            return Ok((None, ("unchanged", Value::Null)));
        }
        let (stage, lead, slug) = (job.get("stage"), lead_of(job), job.get("project"));
        let empty = json!({});
        let claim = job.get("claim").filter(|c| c.is_object()).unwrap_or(&empty);
        let m = job.get("move").filter(|m| m.is_object()).unwrap_or(&empty);
        let pr = get_str(job, "pr").unwrap_or("");
        if !truthy(slug) {
            return Err(refuse("it has no project"));
        }
        let allowed: &[&str] = if action == "babysit" {
            &["pr-open"]
        } else {
            IN_FLIGHT
        };
        if !has_str(allowed, stage) {
            return Err(refuse(&format!("it is at {}", py_opt(stage))));
        }
        if let Some(lead) = &lead {
            return Err(refuse(&format!("it is a rider of {lead}")));
        }
        let slug = slug.unwrap().clone();
        let mut problem = |to: &Value| -> Res<Option<(String, bool)>> {
            let key = (slug.clone(), to.clone());
            if let Some((_, found)) = problems.iter().find(|(k, _)| *k == key) {
                return Ok(found.clone());
            }
            let found = pickup_host_problem(home, &slug, to)?;
            problems.push((key, found.clone()));
            Ok(found)
        };
        let to: Value;
        if action == "babysit" {
            if !GH_PR.is_full(pr) {
                return Err(refuse("it has no GitHub pull request URL"));
            }
            if truthy(job.get("paused")) {
                return Err(refuse("it is paused"));
            }
            if truthy(job.get("close")) {
                return Err(refuse("it has a close queued"));
            }
            to = Value::Null;
        } else if action == "move" {
            to = Value::from(host.unwrap());
            if !truthy(Some(claim)) && m.get("to") == Some(&to) && m.get("claimed").is_none() {
                return Ok((None, ("unchanged", to)));
            }
            let runs_on = if truthy(claim.get("device")) {
                claim.get("device")
            } else if move_stopped(m) {
                None
            } else {
                m.get("from")
            };
            if runs_on == Some(&to) {
                return Err(refuse(&format!("it already runs on {}", py_str(&to))));
            }
            if queued_stops(m).iter().any(|x| x.get("from") == Some(&to)) {
                return Err(refuse(&format!(
                    "a stop is still queued on {}; pick another host",
                    py_str(&to)
                )));
            }
            if let Some((why, _)) = problem(&to)? {
                return Err(refuse(&why));
            }
        } else {
            if !truthy(Some(claim))
                && truthy(m.get("from"))
                && m.get("to") == m.get("from")
                && m.get("claimed").is_none()
            {
                return Ok((None, ("unchanged", m["to"].clone())));
            }
            to = claim.get("device").cloned().unwrap_or(Value::Null);
            if !truthy(Some(&to)) {
                return Err(refuse("it has no claim, so no host runs it"));
            }
            if truthy(job.get("paused")) {
                return Err(refuse("it is paused"));
            }
            if get_str(job, "needs").is_some_and(|n| !strip(n).is_empty()) {
                return Err(refuse("it is waiting on a person"));
            }
            if let Some((why, on_host)) = problem(&to)? {
                let tail = if on_host {
                    ", so nothing would restart it; move it to another host instead"
                } else {
                    ""
                };
                return Err(refuse(&format!("{why}{tail}")));
            }
        }
        let stamp = now();
        let job = find_job(&mut data, job_id).unwrap();
        if action == "babysit" {
            let pr = get_str(job, "pr").unwrap_or("").to_string();
            job["babysit"] = with(json!({"at": stamp}), record);
            if job.get("priority").is_none_or(Value::is_null) {
                job["priority"] = with(json!({"at": stamp}), record);
            }
            let map = job.as_object_mut().unwrap();
            map.shift_remove("needs");
            map.shift_remove("escalated");
            job["note"] = Value::from(format!(
                "babysitting PR #{}",
                pr.rsplit('/').next().unwrap()
            ));
        } else {
            write_move(job, &to, &stamp, record);
        }
        job["updated"] = Value::from(stamp.clone());
        data["updated"] = Value::from(stamp);
        Ok((Some(dump_board(&data)), ("queued", to)))
    })?;
    let mut out = json!({"id": job_id, "action": action, "result": result});
    if action != "babysit" {
        out["to"] = to;
    }
    println!("{}", dumps(&out, DEFAULT));
    Ok(())
}

pub fn control_cmd(call: &Call) -> Res<()> {
    home_and(|home| control_cmd_in(home, &call.args))
}

fn control_cmd_in(home: &Path, args: &[String]) -> Res<()> {
    let (job_id, action) = (&args[0], args[1].as_str());
    let mut flags = &args[2..];
    let mut host = None;
    if action == "move" {
        if flags.is_empty() || !HOST_NAME.is_full(&flags[0]) {
            return Err(fail(CONTROL_USAGE, 2));
        }
        host = Some(flags[0].as_str());
        flags = &flags[1..];
    }
    let pairs = flags_of(flags, 0);
    let want: BTreeSet<&str> = if action == "close" {
        BTreeSet::from(["--reason-file", "--chat-file"])
    } else {
        BTreeSet::from(["--chat-file"])
    };
    if !JOB_ID.is_full(job_id)
        || job_id.contains("..")
        || !CONTROL_ACTIONS.contains(&action)
        || flags.len() != 2 * want.len()
        || flag_names(&pairs) != want
    {
        return Err(fail(CONTROL_USAGE, 2));
    }
    let quote = chat_quote(
        Some(&read_chat_file(
            flag(&pairs, "--chat-file").unwrap(),
            "--chat-file",
        )?),
        "--chat-file",
    )?;
    let mut reason = String::new();
    if action == "close" {
        let text = read_chat_file(flag(&pairs, "--reason-file").unwrap(), "--reason-file")?;
        reason = strip(&text).to_string();
        if !CLOSE_REASON.is_full(&reason) {
            return Err(fail(
                "--reason-file must be one line of 1 to 200 characters; nothing was changed",
                2,
            ));
        }
    }
    let device = holder(home)?.0;
    let record = json!({"via": "chat", "quote": quote, "by": device});
    if matches!(action, "babysit" | "move" | "rerun") {
        return control_launch(home, job_id, action, host, &record);
    }
    let (result, stored) = cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = find_job(&mut data, job_id) else {
            return Err(fail(
                format!("no job {job_id} on the board. Nothing was written."),
                4,
            ));
        };
        let (stage, lead) = (job.get("stage"), lead_of(job));
        if matches!(action, "prioritize" | "pause" | "close") {
            let barred = if action == "prioritize" {
                !has_str(PRIORITY_STAGES, stage)
            } else {
                has_str(TERMINAL, stage) || action == "close" && get_str(job, "stage") == Some(IDEA)
            };
            let why = if !truthy(job.get("project")) {
                Some("it has no project".to_string())
            } else if barred {
                Some(format!("it is at {}", py_opt(stage)))
            } else {
                lead.map(|lead| format!("it is a rider of {lead}"))
            };
            if let Some(why) = why {
                let word = match action {
                    "prioritize" => "prioritized",
                    "pause" => "paused",
                    _ => "closed",
                };
                return Err(fail(
                    format!("{job_id} cannot be {word}: {why}. Nothing was written."),
                    2,
                ));
            }
        }
        let key = match action {
            "prioritize" | "unprioritize" => "priority",
            "pause" | "resume" => "paused",
            _ => "close",
        };
        let adding = matches!(action, "prioritize" | "pause" | "close");
        if truthy(job.get(key)) == adding {
            return Ok((None, ("unchanged".to_string(), job.get(key).cloned())));
        }
        let stamp = now();
        if action == "close" {
            job[key] = with(json!({"reason": reason, "at": stamp}), &record);
        } else if adding {
            job[key] = with(json!({"at": stamp}), &record);
        } else {
            job.as_object_mut().unwrap().shift_remove(key);
        }
        job["updated"] = Value::from(stamp.clone());
        let stored = job.get(key).cloned();
        data["updated"] = Value::from(stamp);
        let result = if action == "close" {
            "queued".to_string()
        } else {
            format!("{action}d")
        };
        Ok((Some(dump_board(&data)), (result, stored)))
    })?;
    let mut out = json!({"id": job_id, "action": action, "result": result});
    if action == "close" {
        out["reason"] = stored
            .as_ref()
            .and_then(Value::as_object)
            .map_or(Value::Null, |s| {
                s.get("reason").cloned().unwrap_or(Value::Null)
            });
    }
    println!("{}", dumps(&out, DEFAULT));
    Ok(())
}

pub fn triage_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("triage"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_stubs() {
        let home = Path::new("/nonexistent");
        let e = push_imported(home, "s/1-a", "s", "1-a", &BTreeSet::new()).unwrap_err();
        assert_eq!(
            (e.code, e.stderr.as_str()),
            (
                2,
                "factory.py: idea-images is not ported to the Rust binary yet\n"
            )
        );
    }

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
