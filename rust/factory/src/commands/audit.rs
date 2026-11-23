//! The transcript commands: audit, usage, outcomes and bench.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{json, Map, Value};

use crate::cli::{not_ported, Call};
use crate::shared::board::{dump_board, load_board, patch_job};
use crate::shared::conf::{fm_get, frontmatter};
use crate::shared::consts::{NEW_JOB_ID, NOTE_SECRETS, PASEO_ID, ROUTE_MODEL};
use crate::shared::exit::{fail, Res};
use crate::shared::fs::read_text;
use crate::shared::home::factory_home;
use crate::shared::http::{access_message, api, cas};
use crate::shared::proc::hostname;
use crate::shared::project::r2_target;
use crate::shared::py::{is_space, os_error, repr, rstrip, strip, universal_newlines};
use crate::shared::pyjson::{dumps, float_repr, loads, DEFAULT};
use crate::shared::pyre::{Captures, Flags, Pattern};

static HOST_NAME: Pattern = Pattern::new(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}", Flags::ASCII);
const RETRO_DENIALS: &[&str] = &[
    "denied by the Claude Code auto mode classifier",
    "Permission to use",
    "doesn't want to proceed",
    "Blocked:",
];
static AUDIT_FLAGS: [(&str, Pattern); 9] = [
    (
        "force-push",
        Pattern::new(
            r"\bgit\b[^;&|\n]*\bpush\b[^;&|\n]*\s(?:--force(?:-with-lease)?|-f)(?=[\s=]|$)",
            Flags::NONE,
        ),
    ),
    (
        "hard-reset",
        Pattern::new(r"\bgit\b[^;&|\n]*\breset\b[^;&|\n]*\s--hard\b", Flags::NONE),
    ),
    (
        "branch-delete",
        Pattern::new(
            r"\bgit\b[^;&|\n]*\bbranch\b[^;&|\n]*\s-D(?=\s|$)",
            Flags::NONE,
        ),
    ),
    (
        "recursive-delete",
        Pattern::new(
            r"(?:^|[\s;&|(])rm\s+(?:-[A-Za-z]+\s+)*(?:-[A-Za-z]*[rR][A-Za-z]*|--recursive)(?=\s|$)",
            Flags::NONE,
        ),
    ),
    (
        "no-verify",
        Pattern::new(r"\bgit\b[^;&|\n]*\s--no-verify\b", Flags::NONE),
    ),
    ("sudo", Pattern::new(r"(?:^|[\s;&|(])sudo\s", Flags::NONE)),
    (
        "pipe-to-shell",
        Pattern::new(
            r"\b(?:curl|wget)\b[^|\n]*\|\s*(?:sudo\s+)?(?:ba|z)?sh\b",
            Flags::NONE,
        ),
    ),
    (
        "deploy",
        Pattern::new(
            r"\bwrangler\s+(?:deploy|publish)\b|\bdeploy\.py\s+deploy\b",
            Flags::NONE,
        ),
    ),
    (
        "pr-merge",
        Pattern::new(r"\bgh\s+pr\s+merge\b", Flags::NONE),
    ),
];
static AUDIT_SECRET_FILE: Pattern = Pattern::new(r#"(?:^|[\s/'\"=])\.env(?![\w.-])"#, Flags::NONE);
static AUDIT_HEREDOC_OPEN: Pattern = Pattern::new(
    r#"(?<!<)<<(-?)\s*(['\"]?)([A-Za-z_][\w.-]*)\2"#,
    Flags::NONE,
);
static AUDIT_RUNS_OUTPUT: Pattern = Pattern::new(
    r#"<\(|\beval\b|(?:^|\s)-(?=[A-Za-z]*c)[A-Za-z]+(?:\s|$)|(?:^|[;&|(])\s*\"?\$\([^()]*$"#,
    Flags::NONE,
);
const AUDIT_FILE_TOOLS: &[&str] = &["Read", "Edit", "Write"];
const AUDIT_MAX_BYTES: usize = 1 << 20;
const AUDIT_WRITE_ONLY: &str = "audit rows are written only by the host's bound service token";

macro_rules! secret_words {
    () => {
        "token|secret|password|passwd|api[_-]?key|credential|mysql_pwd|sshpass|rediscli_auth"
    };
}

static RETRO_HEADERS: Pattern = Pattern::new(
    r#"(?i)\b((?:proxy-)?authorization|(?:set-)?cookie|x-api-key|cf-access-client-(?:id|secret))(\\?[\"']?\s*[:=]\s*\\?[\"']?)(?:\\[^\r\n'\"]|[^\r\n'\"\\])+"#,
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
static RETRO_MYSQL_PASS: Pattern = Pattern::new(
    r#"((?:^|\s)-p)(\"[^\"]*\"|'[^']*'|[^\s\"']\S*)"#,
    Flags::NONE,
);
static RETRO_SSHPASS: Pattern =
    Pattern::new(r"(?<![\w.-])sshpass(?:\.exe)?(?![\w.-])", Flags::NONE);
static RETRO_SSHPASS_PASS: Pattern = Pattern::new(
    r#"((?:^|\s)-p\s*)(\"[^\"]*\"|'[^']*'|[^\s\"']\S*)"#,
    Flags::NONE,
);
static RETRO_REDIS: Pattern =
    Pattern::new(r"(?<![\w.-])redis-cli(?:\.exe)?(?![\w.-])", Flags::NONE);
static RETRO_REDIS_PASS: Pattern = Pattern::new(
    r#"((?:^|\s)(?:-a\s*|--pass\s+))(\"[^\"]*\"|'[^']*'|[^\s\"']\S*)"#,
    Flags::NONE,
);
static RETRO_DOCKER_LOGIN: Pattern = Pattern::new(
    r"(?<![\w.-])docker(?:\.exe)?\s+login(?![\w.-])",
    Flags::NONE,
);
static RETRO_OPENSSL: Pattern =
    Pattern::new(r"(?<![\w.-])openssl(?:\.exe)?(?![\w.-])", Flags::NONE);
static RETRO_OPENSSL_PASS: Pattern = Pattern::new(
    r#"((?:^|\s)-pass(?:in|out)?\s+pass:)(\"[^\"]*\"|'[^']*'|[^\s\"']\S*)"#,
    Flags::NONE,
);
static RETRO_OPENSSL_K: Pattern = Pattern::new(
    r#"((?:^|\s)-[kK]\s+)(\"[^\"]*\"|'[^']*'|[^\s\"']\S*)"#,
    Flags::NONE,
);
static RETRO_OPENSSL_BARE: Pattern = Pattern::new(
    r#"((?:^|\s)-pass(?:in|out)?\s+)(?!(?:pass|env|file|fd):|stdin(?!\S))(\"[^\"]*\"|'[^']*'|[^\s\"']\S*)"#,
    Flags::NONE,
);
static RETRO_COMMAND_SPLIT: Pattern = Pattern::new(
    r#"(?:\"[^\"]*\"|'[^']*'|[^|;&\"']|[\"'])+|[|;&]+"#,
    Flags::NONE,
);
static RETRO_PASSWORD_STDIN: Pattern =
    Pattern::new(r"(?<![\w-])--password-stdin(?![\w-])", Flags::NONE);
static RETRO_ECHO_PASS: Pattern = Pattern::new(
    r#"((?:^|[\s;&(])echo\s+(?:-[neE]{1,3}\s+)?)(\"[^\"]*\"|'[^']*'|[^\s\"'|;&]+)(\s*\|\s*)$"#,
    Flags::NONE,
);
static RETRO_SECRET_WORD: Pattern =
    Pattern::new(concat!("(?i)(?:", secret_words!(), ")"), Flags::NONE);
static RETRO_COUNT: Pattern = Pattern::new(r"\d{1,7}[,;)}\]]*", Flags::NONE);
static RETRO_KEYVALUE: Pattern = Pattern::new(
    concat!(
        r"(?i)(?<![A-Za-z0-9_-])([A-Za-z0-9_-]{0,64}?(?:",
        secret_words!(),
        r#")[A-Za-z0-9_-]{0,64})(\\?[\"']?\s*[=:]\s*)(\"[^\"]*\"|'[^']*'|\S+)"#
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
    r#"(?<![A-Za-z0-9_-])([A-Za-z0-9_-]{65,})(\\?[\"']?\s*[=:]\s*)(\"[^\"]*\"|'[^']*'|\S+)"#,
    Flags::NONE,
);
static RETRO_LONG_FLAG: Pattern = Pattern::new(
    r"(?<![A-Za-z0-9_-])(--[A-Za-z0-9_-]{65,})(\s+)(?!-)(\S+)",
    Flags::NONE,
);
static WORD_DOT_DASH: Pattern = Pattern::new(r"[\w.-]", Flags::NONE);
static WORD_DASH: Pattern = Pattern::new(r"[\w-]", Flags::NONE);

/// Python truthiness of a value that may be missing.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python's `str()` of a value that may be missing.
fn py_str(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(v) => py_repr(v),
        None => "None".into(),
    }
}

/// Python's `repr()` of a parsed JSON value.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => {
            let text = n.to_string();
            if !text.contains(['.', 'e', 'E']) {
                return if text == "-0" { "0".into() } else { text };
            }
            let x: f64 = text.parse().unwrap();
            if x.is_infinite() {
                if x > 0.0 { "inf" } else { "-inf" }.into()
            } else {
                float_repr(x)
            }
        }
        Value::String(s) => repr(s),
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

/// The value when `type(v) is int` and it fits i64.
fn int_of(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => {
            let text = n.to_string();
            if text.contains(['.', 'e', 'E']) {
                None
            } else {
                text.parse().ok()
            }
        }
        _ => None,
    }
}

/// `Path(arg).name`.
fn py_name(arg: &str) -> String {
    arg.split('/')
        .rfind(|part| !part.is_empty() && *part != ".")
        .unwrap_or("")
        .to_string()
}

/// `f"{n:,}"`.
fn thousands(n: i64) -> String {
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

/// The Python exception class an OS error raises as.
fn exc_name(e: &io::Error) -> &'static str {
    match e.kind() {
        io::ErrorKind::PermissionDenied => "PermissionError",
        io::ErrorKind::NotFound => "FileNotFoundError",
        _ => "OSError",
    }
}

/// A file's lines as text mode with `errors="replace"` reads them, each keeping its `\n`. With
/// `Some(n)`, reading stops once n line breaks are in.
fn read_lines(path: &Path, max: Option<usize>) -> io::Result<Vec<String>> {
    let mut file = File::open(path)?;
    let mut data = Vec::new();
    match max {
        None => {
            file.read_to_end(&mut data)?;
        }
        Some(max) => {
            let mut chunk = vec![0u8; 1 << 16];
            let (mut at, mut breaks) = (0, 0);
            while breaks < max {
                let n = file.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&chunk[..n]);
                while at < data.len() {
                    match data[at] {
                        b'\n' => breaks += 1,
                        // A final \r waits for the next chunk, which may hold its \n.
                        b'\r' if at + 1 == data.len() => break,
                        b'\r' if data[at + 1] != b'\n' => breaks += 1,
                        _ => {}
                    }
                    at += 1;
                }
            }
        }
    }
    let text = universal_newlines(String::from_utf8_lossy(&data).into_owned());
    let lines = text.split_inclusive('\n').map(String::from);
    Ok(match max {
        Some(max) => lines.take(max).collect(),
        None => lines.collect(),
    })
}

/// `re.search(r"(?:^|-)factory-<id>(?:-|$)", name)`.
fn folder_names_job(name: &str, job_id: &str) -> bool {
    let needle = format!("factory-{job_id}");
    name.match_indices(&needle).any(|(i, _)| {
        let after = &name[i + needle.len()..];
        (i == 0 || name[..i].ends_with('-'))
            && (after.is_empty() || after.starts_with('-') || after == "\n")
    })
}

/// Whether `c` is in the class `pattern` matches one character of.
fn char_in(pattern: &Pattern, c: char) -> bool {
    pattern.is_full(c.encode_utf8(&mut [0; 4]))
}

/// `re.search("(?:n1|n2)(?![\w-])", text)` for literal needles.
fn names_job(text: &str, needles: &[String]) -> bool {
    needles.iter().any(|n| {
        let mut from = 0;
        while let Some(i) = text[from..].find(n.as_str()) {
            let at = from + i;
            if !text[at + n.len()..]
                .chars()
                .next()
                .is_some_and(|c| char_in(&WORD_DASH, c))
            {
                return true;
            }
            from = at + text[at..].chars().next().unwrap().len_utf8();
        }
        false
    })
}

/// `re.sub(re.escape(home) + r"(?![\w.-])", "~", raw)`.
fn fold_home(raw: &str, home: &str) -> String {
    if home.is_empty() {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let rest = &raw[i..];
        if rest.starts_with(home)
            && !rest[home.len()..]
                .chars()
                .next()
                .is_some_and(|c| char_in(&WORD_DOT_DASH, c))
        {
            out.push('~');
            i += home.len();
        } else {
            let c = rest.chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// One character of `AUDIT_CONTROL`.
fn is_audit_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{61c}' | '\u{200e}' | '\u{200f}'
        | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

fn group<'t>(c: &Captures<'t>, n: usize) -> &'t str {
    c.get(n).map_or("", |m| m.as_str())
}

/// `pattern.sub(f, s, count=count)`, where count 0 replaces every match.
fn sub(pattern: &Pattern, s: &str, count: usize, f: impl Fn(&Captures) -> String) -> String {
    if count == 0 {
        return pattern.replace_all(s, |c| f(c));
    }
    match pattern.search(s) {
        Some(c) => {
            let m = c.get(0).unwrap();
            format!("{}{}{}", &s[..m.start()], f(&c), &s[m.end()..])
        }
        None => s.to_string(),
    }
}

/// Whether every secret word in name is token followed by s, as in max_tokens.
fn retro_count_name(name: &str) -> bool {
    RETRO_SECRET_WORD
        .find_iter(name)
        .all(|m| m.as_str().to_lowercase() == "token" && name[m.end()..].starts_with(['s', 'S']))
}

fn retro_secret_pair(c: &Captures) -> String {
    let (name, sep, value) = (group(c, 1), group(c, 2), group(c, 3));
    if RETRO_SECRET_WORD.search(name).is_none()
        || (RETRO_COUNT.is_full(value) && retro_count_name(name))
    {
        return group(c, 0).to_string();
    }
    format!("{name}{sep}[redacted]")
}

/// One command with every -k, -K and bare -pass, -passin or -passout value after its openssl as
/// [redacted].
fn retro_openssl_part(part: &str) -> String {
    let Some(hit) = RETRO_OPENSSL.search(part) else {
        return part.to_string();
    };
    let end = hit.get(0).unwrap().end();
    let redact = |c: &Captures| format!("{}[redacted]", group(c, 1));
    let rest = sub(
        &RETRO_OPENSSL_BARE,
        &sub(&RETRO_OPENSSL_K, &part[end..], 0, redact),
        0,
        redact,
    );
    format!("{}{rest}", &part[..end])
}

/// line with each tool's password argument as [redacted], as Python's `retro_tool_pass` does.
fn retro_tool_pass(line: &str) -> String {
    let mut line = line.to_string();
    let redact = |c: &Captures| format!("{}[redacted]", group(c, 1));
    for (tool, flag, count) in [
        (&RETRO_MYSQL, &RETRO_MYSQL_PASS, 0),
        (&RETRO_SSHPASS, &RETRO_SSHPASS_PASS, 1),
        (&RETRO_REDIS, &RETRO_REDIS_PASS, 1),
        (&RETRO_DOCKER_LOGIN, &RETRO_SSHPASS_PASS, 1),
        (&RETRO_OPENSSL, &RETRO_OPENSSL_PASS, 0),
    ] {
        if let Some(hit) = tool.search(&line) {
            let end = hit.get(0).unwrap().end();
            line = format!("{}{}", &line[..end], sub(flag, &line[end..], count, redact));
        }
    }
    line = RETRO_COMMAND_SPLIT
        .find_iter(&line)
        .map(|m| retro_openssl_part(m.as_str()))
        .collect();
    if let Some(hit) = RETRO_DOCKER_LOGIN.search(&line) {
        let hit = hit.get(0).unwrap();
        if RETRO_PASSWORD_STDIN
            .find_iter(&line)
            .any(|m| m.start() >= hit.end())
        {
            let head = sub(&RETRO_ECHO_PASS, &line[..hit.start()], 0, |c| {
                format!("{}[redacted]{}", group(c, 1), group(c, 3))
            });
            line = format!("{head}{}", &line[hit.start()..]);
        }
    }
    line
}

fn retro_redact(text: &str) -> String {
    let text = sub(&RETRO_HEADERS, text, 0, |c| {
        format!("{}{}[redacted]", group(c, 1), group(c, 2))
    });
    let text = sub(&RETRO_URL_CREDS, &text, 0, |_| "://[redacted]@".into());
    let text = sub(&RETRO_BEARER, &text, 0, |c| {
        format!("{}[redacted]", group(c, 1))
    });
    let text = sub(&RETRO_USER_PASS, &text, 0, |c| {
        format!("{}[redacted]", group(c, 1))
    });
    let mut text = text
        .split('\n')
        .map(retro_tool_pass)
        .collect::<Vec<_>>()
        .join("\n");
    for pattern in [
        &RETRO_KEYVALUE,
        &RETRO_FLAG,
        &RETRO_LONG_KEYVALUE,
        &RETRO_LONG_FLAG,
    ] {
        text = sub(pattern, &text, 0, retro_secret_pair);
    }
    for pattern in &NOTE_SECRETS {
        text = sub(pattern, &text, 0, |_| "[redacted]".into());
    }
    text
}

/// raw as it prints: home as ~, retro-redacted, whitespace collapsed, control characters as ?, at
/// most 120 characters.
fn audit_target(raw: &str, home: &str) -> String {
    let redacted = retro_redact(&fold_home(raw, home));
    redacted
        .split(is_space)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|c| if is_audit_control(c) { '?' } else { c })
        .take(120)
        .collect()
}

fn user_home() -> String {
    std::env::home_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

fn env_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(v) => Some(PathBuf::from(v)),
        None => std::env::home_dir().map(|h| h.join(fallback)),
    }
}

/// This host's Paseo server id, else None.
fn paseo_server() -> Option<String> {
    let text = read_text(&env_dir("PASEO_HOME", ".paseo")?.join("server-id")).ok()?;
    let server = strip(&text);
    PASEO_ID.is_full(server).then(|| server.to_string())
}

/// A message or tool_result content as text: a string, or its joined text blocks.
fn retro_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.as_object()?.get("text")?.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// The config.md frontmatter of home, or none when the file is missing.
fn config_meta(config: &Path) -> Res<Vec<(String, String)>> {
    if !config.is_file() {
        return Ok(Vec::new());
    }
    Ok(frontmatter(config)
        .map_err(|e| fail(os_error(&e, Some(config)), 1))?
        .0)
}

fn retro_host() -> Res<String> {
    let meta = config_meta(&factory_home()?.join("config.md"))?;
    Ok(match fm_get(&meta, "device") {
        Some(device) if !device.is_empty() => device.to_string(),
        _ => hostname(),
    })
}

fn retro_transcripts() -> PathBuf {
    env_dir("CLAUDE_CONFIG_DIR", ".claude")
        .unwrap_or_default()
        .join("projects")
}

/// `sorted(folder.glob(prefix + "*" + suffix))`, by name. A folder that cannot be listed has none.
fn glob(folder: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.len() >= prefix.len() + suffix.len()
                && name.starts_with(prefix)
                && name.ends_with(suffix)
        })
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The one input field that says what a tool call acted on, or "" for a tool without one.
fn audit_target_raw(name: &str, inp: &Map<String, Value>) -> String {
    let text = |key: &str| inp.get(key).and_then(Value::as_str).unwrap_or("");
    match name {
        "Bash" => text("command").into(),
        _ if AUDIT_FILE_TOOLS.contains(&name) => text("file_path").into(),
        "Grep" | "Glob" => {
            let path = text("path");
            let tail = if path.is_empty() {
                String::new()
            } else {
                format!(" in {path}")
            };
            format!("{}{tail}", text("pattern"))
        }
        "WebFetch" => text("url").into(),
        "WebSearch" => text("query").into(),
        "Agent" | "Task" => format!("{}: {}", text("subagent_type"), text("description")),
        _ => String::new(),
    }
}

/// command with the body lines of cat and tee heredocs blanked, unless Bash runs or expands them.
fn audit_strip_data_heredocs(command: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut pending: Vec<(String, bool, bool)> = Vec::new();
    let mut depth: i64 = 0;
    for line in command.split('\n') {
        if let Some((delimiter, data, quoted)) = pending.first() {
            if strip(line) == delimiter {
                pending.remove(0);
                depth = 0;
                out.push(line);
            } else if !quoted && (depth != 0 || line.contains("$(")) {
                depth = (depth + line.matches('(').count() as i64
                    - line.matches(')').count() as i64)
                    .max(0);
                out.push(line);
            } else {
                out.push(if *data && (*quoted || !line.contains('`')) {
                    ""
                } else {
                    line
                });
            }
            continue;
        }
        out.push(line);
        for m in AUDIT_HEREDOC_OPEN.find_iter(line) {
            let c = AUDIT_HEREDOC_OPEN.match_start(&line[m.start()..]).unwrap();
            let (before, after) = (&line[..m.start()], &line[m.end()..]);
            let segment = before.rsplit([';', '&', '|', '(']).next().unwrap_or("");
            let first = segment.split(is_space).find(|w| !w.is_empty());
            let data = matches!(first, Some("cat" | "tee"))
                && after.chars().next().is_none_or(is_space)
                && AUDIT_RUNS_OUTPUT.search(before).is_none()
                && !(after.contains('|') || after.contains(">("));
            pending.push((group(&c, 3).to_string(), data, !group(&c, 2).is_empty()));
        }
    }
    out.join("\n")
}

/// The names of the risky shapes one call matches, matched on the raw target. main drops pr-merge.
fn audit_flags(
    name: &str,
    inp: &Map<String, Value>,
    raw: &str,
    denied: bool,
    main: bool,
) -> Vec<&'static str> {
    let mut flags: Vec<&'static str> = AUDIT_FLAGS
        .iter()
        .filter(|(flag, pattern)| {
            name == "Bash" && !(main && *flag == "pr-merge") && pattern.search(raw).is_some()
        })
        .map(|(flag, _)| *flag)
        .collect();
    if name == "Bash" && inp.get("dangerouslyDisableSandbox") == Some(&Value::Bool(true)) {
        flags.push("sandbox-off");
    }
    let path = match name {
        "Bash" => Some(audit_strip_data_heredocs(raw)),
        "Grep" => inp.get("path").and_then(Value::as_str).map(String::from),
        _ if AUDIT_FILE_TOOLS.contains(&name) => Some(raw.to_string()),
        _ => None,
    };
    if path.is_some_and(|p| AUDIT_SECRET_FILE.search(&p).is_some()) {
        flags.push("secret-file");
    }
    if denied {
        flags.push("denied");
    }
    flags
}

/// {time, tool, flags, target} for every tool_use block in one transcript. main skips isSidechain lines.
fn audit_scan(path: &Path, main: bool) -> Res<Vec<Map<String, Value>>> {
    let lines = read_lines(path, None).map_err(|e| fail(os_error(&e, Some(path)), 1))?;
    let (mut calls, mut denied) = (Vec::new(), Vec::new());
    for line in &lines {
        let Ok(Value::Object(e)) = loads(line) else {
            continue;
        };
        if main && truthy(e.get("isSidechain")) {
            continue;
        }
        let kind = e.get("type").and_then(Value::as_str);
        let content = e
            .get("message")
            .and_then(Value::as_object)
            .and_then(|m| m.get("content"));
        let blocks = content.and_then(Value::as_array).into_iter().flatten();
        for b in blocks.filter_map(Value::as_object) {
            let block = b.get("type").and_then(Value::as_str);
            if kind == Some("assistant") && block == Some("tool_use") {
                let stamp = e.get("timestamp").and_then(Value::as_str).unwrap_or("");
                let inp = b
                    .get("input")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                calls.push((
                    b.get("id").cloned().unwrap_or(Value::Null),
                    stamp.replace('T', " ").chars().take(19).collect::<String>(),
                    py_str(b.get("name")),
                    inp,
                ));
            } else if kind == Some("user")
                && block == Some("tool_result")
                && b.get("is_error") == Some(&Value::Bool(true))
            {
                let text = retro_text(b.get("content").unwrap_or(&Value::Null));
                if RETRO_DENIALS.iter().any(|phrase| text.contains(phrase)) {
                    denied.push(b.get("tool_use_id").cloned().unwrap_or(Value::Null));
                }
            }
        }
    }
    let home = user_home();
    Ok(calls
        .into_iter()
        .map(|(id, stamp, name, inp)| {
            let raw = audit_target_raw(&name, &inp);
            let flags = audit_flags(&name, &inp, &raw, denied.contains(&id), main);
            let target = audit_target(&raw, &home);
            let target = if target.is_empty() {
                "-".into()
            } else {
                target
            };
            let row = json!({"time": stamp, "tool": name, "flags": flags, "target": target});
            row.as_object().unwrap().clone()
        })
        .collect())
}

/// {Claude session id: Paseo agent id} from the agent files on this host. Skips files it cannot read.
fn audit_paseo_agents() -> HashMap<String, String> {
    let mut agents = HashMap::new();
    let Some(root) = env_dir("PASEO_HOME", ".paseo") else {
        return agents;
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for folder in glob(&root.join("agents"), "", "") {
        if folder.is_dir() {
            paths.extend(glob(&folder, "", ".json"));
        }
    }
    paths.sort();
    for path in paths {
        let Some(data) = read_text(&path).ok().and_then(|t| loads(&t).ok()) else {
            continue;
        };
        let session = data.get("persistence").and_then(|p| p.get("sessionId"));
        if let (Some(Value::String(session)), Some(Value::String(agent))) =
            (session, data.get("id"))
        {
            if PASEO_ID.is_full(agent) {
                agents.insert(session.clone(), agent.clone());
            }
        }
    }
    agents
}

/// The sorted transcript folders this host keeps from the job's worktrees.
fn audit_folders(job_id: &str) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(retro_transcripts())? {
        let path = entry?.path();
        if path.is_dir() && folder_names_job(&file_name(&path), job_id) {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// (rows, main session count) for every transcript this host keeps from the job's worktree.
fn audit_rows(job_id: &str) -> Res<(Vec<Map<String, Value>>, usize)> {
    let root = retro_transcripts();
    if !root.is_dir() {
        return Err(fail(
            format!("no Claude Code transcripts at {}", root.display()),
            4,
        ));
    }
    let (mut rows, mut sessions) = (Vec::new(), 0);
    let (server, agents) = (paseo_server(), audit_paseo_agents());
    let folders = audit_folders(job_id).map_err(|e| fail(os_error(&e, Some(&root)), 1))?;
    for project in folders {
        for path in glob(&project, "", ".jsonl") {
            sessions += 1;
            let session_id = stem(&path);
            let session: String = session_id.chars().take(8).collect();
            let link = match (&server, agents.get(&session_id)) {
                (Some(server), Some(agent)) => Some(json!({"server": server, "agent": agent})),
                _ => None,
            };
            let mut add = |agent: &str, scanned: Vec<Map<String, Value>>| {
                for row in scanned {
                    let mut full = Map::new();
                    full.insert("session".into(), Value::from(session.clone()));
                    full.insert("agent".into(), Value::from(agent));
                    full.extend(row);
                    if let Some(link) = &link {
                        full.insert("paseo".into(), link.clone());
                    }
                    rows.push(full);
                }
            };
            add("main", audit_scan(&path, true)?);
            for sub in glob(&project.join(&session_id).join("subagents"), "", ".jsonl") {
                let meta = sub.with_file_name(format!("{}.meta.json", stem(&sub)));
                let kind = match read_text(&meta).ok().and_then(|t| loads(&t).ok()) {
                    Some(Value::Object(m)) if truthy(m.get("agentType")) => {
                        py_str(m.get("agentType"))
                    }
                    _ => "unknown".into(),
                };
                let kind: String = kind
                    .chars()
                    .map(|c| if is_audit_control(c) { '?' } else { c })
                    .collect();
                add(&kind, audit_scan(&sub, false)?);
            }
        }
    }
    if sessions == 0 {
        return Err(fail(
            format!("no Claude Code session on this host ran in a factory-{job_id} worktree"),
            4,
        ));
    }
    let key = |r: &Map<String, Value>, k: &str| r[k].as_str().unwrap_or("").to_string();
    rows.sort_by_key(|r| (key(r, "time"), key(r, "session")));
    Ok((rows, sessions))
}

/// How many of the oldest lines to drop so the rest fit in max bytes.
fn fit(lines: &[String], max: usize) -> usize {
    let mut size: usize = lines.iter().map(String::len).sum();
    let mut dropped = 0;
    while size > max {
        size -= lines[dropped].len();
        dropped += 1;
    }
    dropped
}

/// PUT this host's audit rows for the job to the Worker under its device name, oldest rows dropped
/// past 1 MiB.
pub(crate) fn audit_push(job_id: &str) -> Res<()> {
    let home = factory_home()?;
    let config = home.join("config.md");
    let meta = config_meta(&config)?;
    let device = fm_get(&meta, "device").unwrap_or("");
    let hint = format!(
        "set device: in {}; pair this host with factory.py host pair <id> <code>",
        config.display()
    );
    if !HOST_NAME.is_full(device) {
        return Err(fail(hint, 2));
    }
    let rows = audit_rows(job_id)?.0;
    let lines: Vec<String> = rows
        .into_iter()
        .map(|row| {
            let mut full = Map::new();
            full.insert("host".into(), Value::from(device));
            full.insert("job".into(), Value::from(job_id));
            full.extend(row);
            dumps(&Value::Object(full), DEFAULT) + "\n"
        })
        .collect();
    let dropped = fit(&lines, AUDIT_MAX_BYTES);
    let key = format!("audit/{job_id}/{device}.jsonl");
    let r = api(
        &home,
        "PUT",
        &format!("/api/{key}"),
        Some(lines[dropped..].concat().as_bytes()),
        &[200, 403],
        &[("Content-Type", "application/x-ndjson")],
    )?;
    if r.status == 403 && strip(&r.text()?) == AUDIT_WRITE_ONLY {
        return Err(fail(format!("{AUDIT_WRITE_ONLY}. {hint}"), 2));
    }
    if r.status == 403 {
        return Err(fail(access_message(&home.join(".env")), 5));
    }
    let n = lines.len();
    let calls = if n == 1 { "call" } else { "calls" };
    if dropped > 0 {
        println!(
            "pushed {} of {n} {calls} to {key}; the oldest {dropped} did not fit in 1 MiB",
            n - dropped
        );
    } else {
        println!("pushed {n} {calls} to {key}");
    }
    Ok(())
}

/// The newest mtime in nanoseconds of the job's session and subagent transcripts here, or None
/// with no session.
#[allow(dead_code)] // pickup calls it
pub(crate) fn audit_stamp(job_id: &str) -> Option<i128> {
    let folders = audit_folders(job_id).ok()?;
    let mut files: Vec<PathBuf> = Vec::new();
    for folder in &folders {
        files.extend(glob(folder, "", ".jsonl"));
    }
    if files.is_empty() {
        return None;
    }
    for folder in &folders {
        for sub in glob(folder, "", "") {
            files.extend(glob(&sub.join("subagents"), "", ".jsonl"));
        }
    }
    let mut newest = None;
    for path in files {
        let modified = fs::metadata(path).ok()?.modified().ok()?;
        let ns = match modified.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        };
        newest = newest.max(Some(ns));
    }
    newest
}

pub fn audit_cmd(call: &Call) -> Res<()> {
    let args = &call.args;
    if args.len() == 2 {
        if args[0] != "push" || !NEW_JOB_ID.is_full(&args[1]) {
            return Err(fail(
                "usage: factory.py audit push <job-id>, a full job id such as 1017-add-x",
                2,
            ));
        }
        return audit_push(&args[1]);
    }
    let job_id = &args[0];
    if !NEW_JOB_ID.is_full(job_id) {
        return Err(fail(
            "usage: factory.py audit <job-id>, a full job id such as 1017-add-x",
            2,
        ));
    }
    let (rows, sessions) = audit_rows(job_id)?;
    let flagged = rows
        .iter()
        .filter(|r| r["flags"].as_array().is_some_and(|f| !f.is_empty()))
        .count();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    println!(
        "job {job_id} on {}: {} call{} in {sessions} session{}, {flagged} flagged",
        retro_host()?,
        rows.len(),
        plural(rows.len()),
        plural(sessions)
    );
    let columns = ["time", "session", "agent", "tool", "flags", "target"];
    let mut lines: Vec<Vec<String>> = vec![columns.iter().map(|c| c.to_string()).collect()];
    for r in &rows {
        let text = |k: &str| r[k].as_str().unwrap_or("").to_string();
        let flags: Vec<&str> = r["flags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let flags = if flags.is_empty() {
            "-".to_string()
        } else {
            flags.join(",")
        };
        lines.push(vec![
            text("time"),
            text("session"),
            text("agent"),
            text("tool"),
            flags,
            text("target"),
        ]);
    }
    let widths: Vec<usize> = (0..columns.len())
        .map(|i| lines.iter().map(|l| l[i].chars().count()).max().unwrap())
        .collect();
    for line in &lines {
        let cells: Vec<String> = line
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect();
        println!("{}", rstrip(&cells.join("  ")));
    }
    Ok(())
}

/// The kinds a usage count keeps, with the transcript field each comes from.
const USAGE_TOKENS: [(&str, &str); 4] = [
    ("input", "input_tokens"),
    ("output", "output_tokens"),
    ("cache_write", "cache_creation_input_tokens"),
    ("cache_read", "cache_read_input_tokens"),
];
const USAGE_PROMPT_LINES: usize = 50;

type Counts = [i64; 4];

fn add_counts(models: &mut Vec<(String, Counts)>, model: &str, counts: &Counts) {
    let i = match models.iter().position(|(m, _)| m == model) {
        Some(i) => i,
        None => {
            models.push((model.to_string(), [0; 4]));
            models.len() - 1
        }
    };
    for (total, n) in models[i].1.iter_mut().zip(counts) {
        *total += n;
    }
}

/// The text of a transcript's first non-meta user line within USAGE_PROMPT_LINES, else "".
fn usage_prompt(path: &Path) -> String {
    let Ok(lines) = read_lines(path, Some(USAGE_PROMPT_LINES)) else {
        return String::new();
    };
    for line in &lines {
        let Ok(Value::Object(e)) = loads(line) else {
            continue;
        };
        if e.get("type").and_then(Value::as_str) == Some("user") && !truthy(e.get("isMeta")) {
            return match e.get("message") {
                Some(Value::Object(msg)) => retro_text(msg.get("content").unwrap_or(&Value::Null)),
                _ => String::new(),
            };
        }
    }
    String::new()
}

/// ([(model, counts)], unreadable line count) for one transcript, each message id counted once.
/// main skips isSidechain lines. since keeps only lines whose timestamp is at or after it.
fn usage_scan(
    path: &Path,
    main: bool,
    since: Option<&str>,
) -> io::Result<(Vec<(String, Counts)>, usize)> {
    let mut turns: Vec<(String, String, Counts)> = Vec::new();
    let mut bad = 0;
    for line in read_lines(path, None)? {
        if strip(&line).is_empty() {
            continue;
        }
        let Ok(e) = loads(&line) else {
            bad += 1;
            continue;
        };
        let Value::Object(e) = e else { continue };
        if e.get("type").and_then(Value::as_str) != Some("assistant")
            || (main && truthy(e.get("isSidechain")))
        {
            continue;
        }
        if let Some(since) = since {
            if !e
                .get("timestamp")
                .and_then(Value::as_str)
                .is_some_and(|t| t >= since)
            {
                continue;
            }
        }
        let Some(msg) = e.get("message").and_then(Value::as_object) else {
            continue;
        };
        let (Some(id), Some(usage)) = (
            msg.get("id").and_then(Value::as_str),
            msg.get("usage").and_then(Value::as_object),
        ) else {
            continue;
        };
        let model = msg
            .get("model")
            .and_then(Value::as_str)
            .filter(|m| ROUTE_MODEL.is_full(m))
            .unwrap_or("other");
        let i = match turns.iter().position(|(t, ..)| t == id) {
            Some(i) => i,
            None => {
                turns.push((id.to_string(), model.to_string(), [0; 4]));
                turns.len() - 1
            }
        };
        for (best, (_, field)) in turns[i].2.iter_mut().zip(USAGE_TOKENS) {
            if let Some(value) = int_of(usage.get(field)) {
                *best = (*best).max(value);
            }
        }
    }
    let mut models = Vec::new();
    for (_, model, counts) in &turns {
        add_counts(&mut models, model, counts);
    }
    models.retain(|(_, c)| c.iter().any(|n| *n != 0));
    Ok((models, bad))
}

/// [(path, main)] for this host's transcripts that ran in the job's worktree, plus each main
/// session or subagent elsewhere whose own first prompt names the job folder or prompt folder.
fn usage_transcripts(home: &Path, slug: &str, job_id: &str) -> io::Result<Vec<(PathBuf, bool)>> {
    let root = retro_transcripts();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut needles: Vec<String> = Vec::new();
    for base in ["jobs", "prompts"] {
        let p = home.join(base).join(slug).join(job_id);
        for s in [
            p.display().to_string(),
            p.display().to_string().replace('\\', "/"),
        ] {
            if !needles.contains(&s) {
                needles.push(s);
            }
        }
    }
    needles.sort();
    let worktrees = audit_folders(job_id)?;
    let mut projects = Vec::new();
    for entry in fs::read_dir(&root)? {
        let path = entry?.path();
        if path.is_dir() {
            projects.push(path);
        }
    }
    projects.sort();
    let mut found = Vec::new();
    for project in projects {
        for path in glob(&project, "", ".jsonl") {
            let subs = glob(&project.join(stem(&path)).join("subagents"), "", ".jsonl");
            if worktrees.contains(&project) {
                found.push((path, true));
                found.extend(subs.into_iter().map(|s| (s, false)));
                continue;
            }
            if names_job(&usage_prompt(&path), &needles) {
                found.push((path, true));
            }
            found.extend(
                subs.into_iter()
                    .filter(|s| names_job(&usage_prompt(s), &needles))
                    .map(|s| (s, false)),
            );
        }
    }
    Ok(found)
}

/// {"transcripts": n, "models": {model: {kind: tokens}}} summed over the job's transcripts here.
/// Warns on stderr about unreadable files and lines.
pub(crate) fn usage_totals(home: &Path, slug: &str, job_id: &str) -> io::Result<Value> {
    let mut totals: Vec<(String, Counts)> = Vec::new();
    let mut read = 0;
    for (path, main) in usage_transcripts(home, slug, job_id)? {
        let Ok((models, bad)) = usage_scan(&path, main, None) else {
            eprintln!("factory.py: usage: cannot read {}", file_name(&path));
            continue;
        };
        read += 1;
        if bad > 0 {
            let s = if bad == 1 { "" } else { "s" };
            eprintln!(
                "factory.py: usage: {bad} unreadable line{s} in {}",
                file_name(&path)
            );
        }
        for (model, counts) in &models {
            add_counts(&mut totals, model, counts);
        }
    }
    totals.sort_by(|a, b| a.0.cmp(&b.0));
    let mut models = Map::new();
    for (model, counts) in totals {
        let kinds: Map<String, Value> = USAGE_TOKENS
            .iter()
            .zip(counts)
            .map(|((key, _), n)| (key.to_string(), Value::from(n)))
            .collect();
        models.insert(model, Value::Object(kinds));
    }
    Ok(json!({"transcripts": read, "models": models}))
}

/// Write this host's token totals to the board job under tokens.<device>, keeping other hosts'
/// entries.
pub(crate) fn usage_record(
    home: &Path,
    slug: &str,
    job_id: &str,
    device: &str,
    totals: &Value,
) -> Res<()> {
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = data["jobs"].as_array().and_then(|jobs| {
            jobs.iter()
                .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id))
        }) else {
            return Err(fail(
                format!("no job {slug}/{job_id} on the board. Nothing was written."),
                4,
            ));
        };
        if job.get("project").and_then(Value::as_str) != Some(slug) {
            return Err(fail(
                format!("{job_id} is not assigned to {slug}. Nothing was written."),
                2,
            ));
        }
        let mut tokens = job
            .get("tokens")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let job = patch_job(&mut data, job_id, &json!({}));
        let mut entry = Map::new();
        entry.insert("at".into(), job["updated"].clone());
        entry.extend(totals.as_object().cloned().unwrap_or_default());
        tokens.insert(device.to_string(), Value::Object(entry));
        let slot = data["jobs"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id))
            .unwrap();
        slot["tokens"] = Value::Object(tokens);
        Ok((Some(dump_board(&data)), ()))
    })
}

/// Input, output and cache_write tokens summed over a {model: {kind: tokens}} map. cache_read and
/// negative counts are left out.
fn token_count(models: &Value) -> i64 {
    models
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(Value::as_object)
        .flat_map(|counts| counts.iter())
        .filter(|(k, _)| ["input", "output", "cache_write"].contains(&k.as_str()))
        .filter_map(|(_, v)| int_of(Some(v)))
        .filter(|v| *v >= 0)
        .sum()
}

pub fn usage_cmd(call: &Call) -> Res<()> {
    let args = &call.args;
    let record = args.len() == 2;
    if record && args[0] != "record" {
        return Err(fail(
            "usage: factory.py usage [record] <project-slug>/<job-id>",
            2,
        ));
    }
    let (home, slug, job_id) = r2_target(args.last().unwrap(), true)?;
    let mut host = retro_host()?;
    if record {
        let config = home.join("config.md");
        host = fm_get(&config_meta(&config)?, "device")
            .unwrap_or("")
            .to_string();
        if !HOST_NAME.is_full(&host) {
            return Err(fail(
                format!(
                    "set device: in {}; pair this host with factory.py host pair <id> <code>",
                    config.display()
                ),
                2,
            ));
        }
    }
    let totals = match usage_totals(&home, &slug, &job_id) {
        Ok(totals) => totals,
        Err(e) => {
            eprintln!(
                "factory.py: usage: scan failed ({}: {}); nothing recorded",
                exc_name(&e),
                os_error(&e, Some(&retro_transcripts()))
            );
            return Ok(());
        }
    };
    let n = totals["transcripts"].as_u64().unwrap_or(0);
    if n == 0 {
        println!("no Claude Code transcripts on {host} name {job_id}; nothing recorded");
        return Ok(());
    }
    println!(
        "job {job_id} on {host}: {n} transcript{}",
        if n == 1 { "" } else { "s" }
    );
    let mut lines: Vec<Vec<String>> = vec![std::iter::once("model")
        .chain(USAGE_TOKENS.iter().map(|(k, _)| *k))
        .map(String::from)
        .collect()];
    for (model, counts) in totals["models"].as_object().unwrap() {
        let mut line = vec![model.clone()];
        for (key, _) in USAGE_TOKENS {
            line.push(thousands(counts[key].as_i64().unwrap_or(0)));
        }
        lines.push(line);
    }
    let widths: Vec<usize> = (0..lines[0].len())
        .map(|i| lines.iter().map(|l| l[i].chars().count()).max().unwrap())
        .collect();
    for line in &lines {
        let cells: Vec<String> = line
            .iter()
            .zip(&widths)
            .enumerate()
            .map(|(i, (cell, w))| {
                if i == 0 {
                    format!("{cell:<w$}")
                } else {
                    format!("{cell:>w$}")
                }
            })
            .collect();
        println!("{}", cells.join("  "));
    }
    if record {
        usage_record(&home, &slug, &job_id, &host, &totals)?;
        println!("recorded {host} tokens on {job_id}");
    }
    Ok(())
}

pub fn outcomes_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("outcomes"))
}

pub fn bench_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("bench"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn value_rules() {
        for (v, want) in [
            (json!(null), false),
            (json!(false), false),
            (json!(0), false),
            (json!(0.0), false),
            (json!(""), false),
            (json!([]), false),
            (json!({}), false),
            (json!(1), true),
            (json!("x"), true),
            (json!([0]), true),
        ] {
            assert_eq!(truthy(Some(&v)), want, "{v}");
        }
        assert!(!truthy(None));
        assert_eq!(py_str(None), "None");
        assert_eq!(py_str(Some(&json!("a'b"))), "a'b");
        assert_eq!(py_str(Some(&json!(5))), "5");
        assert_eq!(
            py_repr(&json!({"a": [1, 2.5, true, null, "x"]})),
            "{'a': [1, 2.5, True, None, 'x']}"
        );
        let big: Value = serde_json::from_str("1e400").unwrap();
        assert_eq!(py_repr(&big), "inf");
        assert_eq!(int_of(Some(&json!(7))), Some(7));
        for v in [json!(7.0), json!(true), json!("7")] {
            assert_eq!(int_of(Some(&v)), None, "{v}");
        }
        let huge: Value = serde_json::from_str("99999999999999999999").unwrap();
        assert_eq!(int_of(Some(&huge)), None);
        assert_eq!(py_name("/a/b/"), "b");
        assert_eq!(py_name("a/./"), "a");
        assert_eq!(py_name("odd name"), "odd name");
        assert_eq!(py_name(""), "");
        assert_eq!(thousands(3_400_000), "3,400,000");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(-1200), "-1,200");
        assert_eq!(
            exc_name(&io::Error::from(io::ErrorKind::PermissionDenied)),
            "PermissionError"
        );
        assert_eq!(
            exc_name(&io::Error::from(io::ErrorKind::NotFound)),
            "FileNotFoundError"
        );
        assert_eq!(exc_name(&io::Error::other("x")), "OSError");
        assert!(folder_names_job("-home-u-factory-1017-add-x", "1017-add-x"));
        assert!(folder_names_job("factory-1017-add-x-2", "1017-add-x"));
        assert!(!folder_names_job(
            "-home-u-factory-1017-add-xy",
            "1017-add-x"
        ));
        assert!(!folder_names_job("xfactory-1017-add-x", "1017-add-x"));
        assert!(names_job(
            "see /h/jobs/a/1017-add-x.",
            &["/h/jobs/a/1017-add-x".into()]
        ));
        assert!(!names_job(
            "/h/jobs/a/1017-add-xé",
            &["/h/jobs/a/1017-add-x".into()]
        ));
        assert_eq!(
            fold_home("/home/u /home/u.x /home/uu", "/home/u"),
            "~ /home/u.x /home/uu"
        );
        assert!(is_audit_control('\u{1b}') && is_audit_control('\u{9b}'));
        assert!(is_audit_control('\u{202e}') && is_audit_control('\u{2069}'));
        assert!(!is_audit_control('a') && !is_audit_control('\u{a0}'));
    }

    #[test]
    fn read_lines_stops_early() {
        let dir = std::env::temp_dir().join(format!("audit-lines-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        std::fs::write(&path, b"a\r\nb\rc\n\xffd\n").unwrap();
        assert_eq!(
            read_lines(&path, None).unwrap(),
            ["a\n", "b\n", "c\n", "\u{fffd}d\n"]
        );
        assert_eq!(read_lines(&path, Some(2)).unwrap(), ["a\n", "b\n"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn target_folds_home_and_redacts() {
        let t = |raw: &str| audit_target(raw, "/home/u");
        assert_eq!(
            t("curl -H 'Authorization: Bearer abc123' https://get.example.test/i.sh | sh"),
            "curl -H 'Authorization: [redacted]' https://get.example.test/i.sh | sh"
        );
        assert_eq!(
            t("mysql -u root -phunter2 db"), // gitleaks:allow synthetic credentials
            "mysql -u root -p[redacted] db"
        );
        assert_eq!(t("/home/u/app/.env"), "~/app/.env");
        assert_eq!(t("ls /home/u/a /home/user2/x"), "ls ~/a /home/user2/x");
        assert_eq!(t("/home/u"), "~");
        assert_eq!(t("cd /home/u && ls"), "cd ~ && ls");
        assert_eq!(t("ls \u{202e}gnp.exe \u{2066}x\u{2069}"), "ls ?gnp.exe ?x?");
        assert_eq!(t("a \n  b"), "a b");
        assert_eq!(t(&"x".repeat(300)).chars().count(), 120);
        assert_eq!(
            t("echo ok\x1b[1A\x1b[2K\rgit status\u{9b}"),
            "echo ok?[1A?[2K git status?"
        );
    }

    #[test]
    fn runs_output_is_linear() {
        let start = std::time::Instant::now();
        let text = format!(" -{}!", "c".repeat(100_000));
        assert!(AUDIT_RUNS_OUTPUT.search(&text).is_none());
        assert!(start.elapsed().as_secs_f64() < 1.0);
    }

    #[test]
    fn pr_merge_main_only() {
        let merge = json!({"command": "gh pr merge 7"});
        let merge = merge.as_object().unwrap();
        let raw = audit_target_raw("Bash", merge);
        assert!(audit_flags("Bash", merge, &raw, false, true).is_empty());
        assert_eq!(audit_flags("Bash", merge, &raw, false, false), ["pr-merge"]);
        let push = json!({"command": "gh pr merge 7 && git push --force"});
        let push = push.as_object().unwrap();
        let raw = audit_target_raw("Bash", push);
        assert_eq!(audit_flags("Bash", push, &raw, false, true), ["force-push"]);
    }

    #[test]
    fn push_trims_oldest() {
        let lines: Vec<String> = (0..6).map(|n| format!("{{\"row\": {n}}}\n")).collect();
        let full: usize = lines.iter().map(String::len).sum();
        assert_eq!(fit(&lines, full), 0);
        assert_eq!(fit(&lines, full - 1), 1);
    }

    #[test]
    fn names_job_boundary() {
        let needles = ["1017-add-x".to_string()];
        assert!(names_job("jobs/a/1017-add-x/spec.md", &needles));
        assert!(names_job("jobs/a/1017-add-x", &needles));
        assert!(!names_job("jobs/a/1017-add-xy", &needles));
        assert!(!names_job("jobs/a/1017-add-x-2", &needles));
    }

    fn turn(id: &str, model: &str, n: [i64; 4], side: bool, stamp: Option<&str>) -> String {
        let mut line = json!({"type": "assistant", "isSidechain": side, "message": {"id": id, "model": model, "usage": {
            "input_tokens": n[0], "output_tokens": n[1], "cache_creation_input_tokens": n[2],
            "cache_read_input_tokens": n[3]}}});
        if let Some(stamp) = stamp {
            line["timestamp"] = Value::from(stamp);
        }
        line.to_string() + "\n"
    }

    #[test]
    fn usage_scan_dedupes() {
        const OPUS: &str = "claude-opus-5-5";
        let dir = std::env::temp_dir().join(format!("audit-usage-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let text = [
            turn("a", OPUS, [1, 2, 3, 4], false, None),
            turn("a", OPUS, [1, 5, 3, 4], false, None),
            turn("b", OPUS, [10, 0, 0, 0], false, None),
            turn("c", "<synthetic>", [0, 7, 0, 0], false, None),
            turn("d", "zero-model", [0, 0, 0, 0], false, None),
            "{broken\n".into(),
            "\n".into(),
            turn("e", OPUS, [9, 9, 9, 9], true, None),
        ]
        .concat();
        fs::write(&path, text).unwrap();
        let (models, bad) = usage_scan(&path, true, None).unwrap();
        assert_eq!(bad, 1);
        assert_eq!(
            models,
            [
                (OPUS.to_string(), [11, 5, 3, 4]),
                ("other".to_string(), [0, 7, 0, 0])
            ]
        );
        assert_eq!(usage_scan(&path, false, None).unwrap().0[0].1[0], 20);

        let d2 = turn(
            "d2",
            OPUS,
            [10, 20, 30, 40],
            false,
            Some("2026-01-01T00:00:00.250Z"),
        );
        let day = [
            turn(
                "d1",
                OPUS,
                [1, 2, 3, 4],
                false,
                Some("2025-12-31T23:59:59.900Z"),
            ),
            d2.clone(),
            d2,
        ]
        .concat();
        fs::write(&path, day).unwrap();
        assert_eq!(
            usage_scan(&path, true, Some("2026-01-01T00:00:00")).unwrap(),
            (vec![(OPUS.to_string(), [10, 20, 30, 40])], 0)
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
