//! The transcript commands: audit, usage, outcomes and bench.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
#[cfg(test)]
use std::{cell::RefCell, rc::Rc};

use serde_json::{json, Map, Value};

use crate::cli::Call;
use crate::commands::checks::{pr_merge_commits, pr_states};
use crate::commands::pipeline::fetch_base;
use crate::shared::board::{dump_board, load_board, patch_job, read_board};
use crate::shared::conf::{fm_get, frontmatter};
use crate::shared::consts::{JOB_ID, NEW_JOB_ID, PASEO_ID, ROUTE_MODEL, SLUG};
use crate::shared::exit::{exit, fail, Res, LIMIT};
use crate::shared::fs::{create_dir_private, read_text, resolve, temp_root};
use crate::shared::home::{device_name, factory_home};
use crate::shared::http::{access_message, api, cas};
use crate::shared::paseo::paseo_server;
use crate::shared::proc::{capture, git, hostname, which};
use crate::shared::project::{project_files, r2_target, read_paths};
use crate::shared::py::{
    is_space, os_error, round_half_even, round_int, rstrip, splitlines, strip, universal_newlines,
};
use crate::shared::pyjson::{dumps, loads, DEFAULT, INDENT2};
use crate::shared::pyre::{Captures, Flags, Pattern};
use crate::shared::pyval::{py_str, truthy};
use crate::shared::time::{now, parse_stamp, unix_now};

static HOST_NAME: Pattern = Pattern::new(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}", Flags::ASCII);
const RETRO_DENIALS: &[&str] = &[
    "denied by the Claude Code auto mode classifier",
    "Permission to use",
    "doesn't want to proceed",
    "Blocked:",
];
static AUDIT_FLAGS: [(&str, Pattern); 8] = [
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
];
static AUDIT_SECRET_FILE: Pattern = Pattern::new(r#"(?:^|[\s/'\"=])\.env(?![\w.-])"#, Flags::NONE);

/// Put only the secret-file flag pattern at a zero backtrack limit in this test thread, so a
/// test outside this module can make a transcript scan hit the limit.
#[cfg(test)]
pub(crate) fn limit_secret_file_for_test() {
    crate::shared::pyre::TEST_LIMIT.with(|l| l.set(Some(0)));
    crate::shared::pyre::TEST_LIMIT_ONLY.with(|l| l.set(Some(&AUDIT_SECRET_FILE)));
}
static AUDIT_PR_MERGE: Pattern = Pattern::new(
    r#"(?:^|[;&|(){}\n`])[ \t]*(?:(?:!|if|then|else|elif|do|time|env|command|exec|sudo|xargs)(?:[ \t]+-[^\s;&|(){}`]*)*[ \t]+|[A-Za-z_]\w*=(?:"\$\([^()\n`"]*\)"|[^\s;&|(){}`]*)[ \t]+)*(?:gh(?:[ \t]+(?:-R|--repo)(?:[ \t]+|=)[^\s;&|(){}`]+)?\s+(?:pr\s+merge\b|api\b(?=(?:[^;&|\n(){}`]|\{\w+\})*(?:-X\s*|--method[\s=]+)PUT\b)(?:[^;&|\n(){}`]|\{\w+\})*\bpulls/(?:\d+|\$\{?[A-Za-z_]\w*\}?)/merge\b)|(?:""|''|\$(?:\{[A-Za-z_]\w*\}|[A-Za-z_])[^\s;&|(){}`'"]*|(?:[^\s;&|(){}`'"]*/)?factory)[ \t]+pr-merge(?![\w-]))"#,
    Flags::NONE,
);
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
/// The target of the row a push adds when a flag pattern could not finish reading the transcripts.
const AUDIT_SKIPPED: &str =
    "audit push skipped: a flag pattern hit its backtrack limit, so newer calls are not shown";

static WORD_DOT_DASH: Pattern = Pattern::new(r"[\w.-]", Flags::NONE);
static WORD_DASH: Pattern = Pattern::new(r"[\w-]", Flags::NONE);

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

/// raw as it prints: home as ~, retro-redacted, whitespace collapsed, control characters as ?, at
/// most 120 characters.
fn audit_target(raw: &str, home: &str) -> String {
    let redacted = crate::commands::retro::retro_redact(&fold_home(raw, home));
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
/// Err holds why the runs-output check could not finish, since a body blanked wrongly would hide its flags.
fn audit_strip_data_heredocs(command: &str) -> Result<String, String> {
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
                && AUDIT_RUNS_OUTPUT.search_or_err(before)?.is_none()
                && !(after.contains('|') || after.contains(">("));
            pending.push((group(&c, 3).to_string(), data, !group(&c, 2).is_empty()));
        }
    }
    Ok(out.join("\n"))
}

/// command with quoted text blanked, unless a double-quoted span runs $( or a backtick; a quoted word with no space or separator loses only its quotes.
fn audit_strip_quotes(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let mut chars = command.char_indices();
    while let Some((start, c)) = chars.next() {
        match c {
            '\\' => {
                out.push(c);
                out.extend(chars.next().map(|(_, n)| n));
            }
            '\'' | '"' => {
                let mut end = None;
                while let Some((i, n)) = chars.next() {
                    if n == c {
                        end = Some(i);
                        break;
                    }
                    if c == '"' && n == '\\' {
                        chars.next();
                    }
                }
                let Some(end) = end else {
                    out.push_str(&command[start..]);
                    break;
                };
                let body = &command[start + 1..end];
                if c == '"' && (body.contains("$(") || body.contains('`')) {
                    out.push_str(&command[start..=end]);
                } else if !body.is_empty()
                    && !body.contains(|b: char| b.is_whitespace() || ";&|()<>`".contains(b))
                {
                    out.push_str(body);
                } else {
                    out.push(c);
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// The names of the risky shapes one call matches. pr-merge needs a merge that runs, by gh or the runtime; main drops it.
/// Err holds why a pattern could not finish reading the call, so no row passes unflagged.
fn audit_flags(
    name: &str,
    inp: &Map<String, Value>,
    raw: &str,
    denied: bool,
    main: bool,
) -> Result<Vec<&'static str>, String> {
    let mut flags: Vec<&'static str> = Vec::new();
    if name == "Bash" {
        for (flag, pattern) in &AUDIT_FLAGS {
            if pattern.search_or_err(raw)?.is_some() {
                flags.push(flag);
            }
        }
    }
    let stripped = match name {
        "Bash" => Some(audit_strip_data_heredocs(raw)?),
        _ => None,
    };
    if !main {
        if let Some(s) = &stripped {
            if AUDIT_PR_MERGE
                .search_or_err(&audit_strip_quotes(s))?
                .is_some()
            {
                flags.push("pr-merge");
            }
        }
    }
    if name == "Bash" && inp.get("dangerouslyDisableSandbox") == Some(&Value::Bool(true)) {
        flags.push("sandbox-off");
    }
    let path = match name {
        "Bash" => stripped,
        "Grep" => inp.get("path").and_then(Value::as_str).map(String::from),
        _ if AUDIT_FILE_TOOLS.contains(&name) => Some(raw.to_string()),
        _ => None,
    };
    if let Some(p) = path {
        if AUDIT_SECRET_FILE.search_or_err(&p)?.is_some() {
            flags.push("secret-file");
        }
    }
    if denied {
        flags.push("denied");
    }
    Ok(flags)
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
    calls
        .into_iter()
        .map(|(id, stamp, name, inp)| {
            let raw = audit_target_raw(&name, &inp);
            let flags = audit_flags(&name, &inp, &raw, denied.contains(&id), main)
                .map_err(|why| fail(format!("{}: {why}", path.display()), LIMIT))?;
            let target = audit_target(&raw, &home);
            let target = if target.is_empty() {
                "-".into()
            } else {
                target
            };
            let row = json!({"time": stamp, "tool": name, "flags": flags, "target": target});
            Ok(row.as_object().unwrap().clone())
        })
        .collect()
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

/// This host's rows the Worker holds for the job, without an earlier skip marker.
fn audit_pushed_rows(home: &Path, job_id: &str, device: &str) -> Res<Vec<Map<String, Value>>> {
    let r = api(
        home,
        "GET",
        &format!("/api/audit/{job_id}"),
        None,
        &[200],
        &[],
    )?;
    Ok(r.text()?
        .lines()
        .filter_map(|line| match loads(line) {
            Ok(Value::Object(row)) => Some(row),
            _ => None,
        })
        .filter(|row| {
            row.get("host").and_then(Value::as_str) == Some(device)
                && row.get("target").and_then(Value::as_str) != Some(AUDIT_SKIPPED)
        })
        .collect())
}

/// PUT this host's audit rows for the job to the Worker under its device name, oldest rows dropped
/// past 1 MiB. A scan past the backtrack limit keeps the rows pushed before, adds an AUDIT_SKIPPED
/// row, and still exits LIMIT.
pub(crate) fn audit_push(job_id: &str) -> Res<()> {
    let home = factory_home()?;
    let config = home.join("config.md");
    let meta = config_meta(&config)?;
    let device = fm_get(&meta, "device").unwrap_or("");
    let hint = format!(
        "set device: in {}; pair this host with factory host pair <id> <code>",
        config.display()
    );
    if !HOST_NAME.is_full(device) {
        return Err(fail(hint, 2));
    }
    let (rows, skipped) = match audit_rows(job_id) {
        Ok((rows, _)) => (rows, None),
        Err(e) if e.code == LIMIT => {
            let mut rows = audit_pushed_rows(&home, job_id, device)?;
            let time: String = now().replace('T', " ").chars().take(19).collect();
            let marker = json!({"time": time, "session": "-", "agent": "-", "tool": "-", "flags": [], "target": AUDIT_SKIPPED});
            rows.push(marker.as_object().unwrap().clone());
            (rows, Some(e))
        }
        Err(e) => return Err(e),
    };
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
    if let Some(e) = skipped {
        return Err(e);
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
                "usage: factory audit push <job-id>, a full job id such as 1017-add-x",
                2,
            ));
        }
        return audit_push(&args[1]);
    }
    let job_id = &args[0];
    if !NEW_JOB_ID.is_full(job_id) {
        return Err(fail(
            "usage: factory audit <job-id>, a full job id such as 1017-add-x",
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
/// It reads only up to that line, as Python's lazy file iteration does: the lines before it can
/// hold megabytes of tool output.
fn usage_prompt(path: &Path) -> String {
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    let mut reader = io::BufReader::new(file);
    let (mut raw, mut seen) = (Vec::new(), 0);
    while seen < USAGE_PROMPT_LINES {
        raw.clear();
        if !matches!(reader.read_until(b'\n', &mut raw), Ok(n) if n > 0) {
            break;
        }
        let text = String::from_utf8_lossy(&raw);
        let text = text.strip_suffix('\n').unwrap_or(&text);
        let text = text.strip_suffix('\r').unwrap_or(text);
        // A lone \r ends a line too, as in Python's universal newlines.
        for line in text.split('\r') {
            if seen == USAGE_PROMPT_LINES {
                break;
            }
            seen += 1;
            if let Some(prompt) = usage_prompt_line(line) {
                return prompt;
            }
        }
    }
    String::new()
}

/// The prompt text of a non-meta user transcript line, else None.
fn usage_prompt_line(line: &str) -> Option<String> {
    let Ok(Value::Object(e)) = loads(line) else {
        return None;
    };
    if e.get("type").and_then(Value::as_str) != Some("user") || truthy(e.get("isMeta")) {
        return None;
    }
    Some(match e.get("message") {
        Some(Value::Object(msg)) => retro_text(msg.get("content").unwrap_or(&Value::Null)),
        _ => String::new(),
    })
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
            eprintln!("usage: cannot read {}", file_name(&path));
            continue;
        };
        read += 1;
        if bad > 0 {
            let s = if bad == 1 { "" } else { "s" };
            eprintln!("usage: {bad} unreadable line{s} in {}", file_name(&path));
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
        let Some(job) = board_jobs(&data)?
            .iter()
            .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id))
        else {
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
            "usage: factory usage [record] <project-slug>/<job-id>",
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
                    "set device: in {}; pair this host with factory host pair <id> <code>",
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
                "usage: scan failed ({}: {}); nothing recorded",
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

const OUTCOME_DAYS: i64 = 90;
const OUTCOME_RESULTS: [&str; 4] = ["held", "reworked", "reverted", "unknown"];
const OUTCOME_FIX_DAYS: i64 = 7;
static OUTCOME_SUBJECT: Pattern = Pattern::new("(?:fix|revert)", Flags::I);
static GH_PR_PARTS: Pattern = Pattern::new(
    r"https://github\.com/([\w.-]+)/([\w.-]+)/pull/(\d{1,9})",
    Flags::ASCII,
);

/// {job id: (result, evidence)}.
type Results = HashMap<String, (String, Vec<String>)>;

/// The board's job list. Fails where Python would raise: no `jobs` list, or an entry that is not
/// an object.
fn board_jobs(data: &Value) -> Res<&Vec<Value>> {
    data.get("jobs")
        .and_then(Value::as_array)
        .filter(|jobs| jobs.iter().all(Value::is_object))
        .ok_or_else(|| fail("the board's jobs are not a list of objects", 1))
}

/// `str(j["id"])`, failing where Python raises KeyError.
fn job_id(j: &Value) -> Res<String> {
    match j.get("id") {
        Some(id) => Ok(py_str(Some(id))),
        None => Err(fail("a board job has no id", 1)),
    }
}

/// (owner/name, number) of a GitHub pull request URL. A truthy non-string pr fails, as Python's
/// `fullmatch` raises on it.
fn pr_parts(pr: Option<&Value>) -> Res<Option<(String, i64)>> {
    let text = match pr {
        Some(Value::String(s)) => s.as_str(),
        _ if truthy(pr) => return Err(fail("a board job's pr is not a string", 1)),
        _ => "",
    };
    Ok(GH_PR_PARTS.fullmatch(text).map(|c| {
        (
            format!("{}/{}", group(&c, 1), group(&c, 2)),
            group(&c, 3).parse().unwrap(),
        )
    }))
}

/// The board text with each known result written to its job's outcome, touching only jobs whose
/// result or evidence changed, and never a job's updated. Returns how many jobs changed.
fn outcomes_change(
    text: Option<&str>,
    results: &Results,
    stamp: &str,
) -> Res<(Option<String>, usize)> {
    let mut data = load_board(text)?;
    board_jobs(&data)?;
    let mut changed = 0;
    for j in data["jobs"].as_array_mut().into_iter().flatten() {
        let Some((result, evidence)) = j
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| results.get(id))
        else {
            continue;
        };
        if result == "unknown" {
            continue;
        }
        let old = j.get("outcome").and_then(Value::as_object);
        if old.and_then(|o| o.get("result")) == Some(&json!(result))
            && old.and_then(|o| o.get("evidence")) == Some(&json!(evidence))
        {
            continue;
        }
        j["outcome"] = json!({"result": result, "at": stamp, "evidence": evidence});
        changed += 1;
    }
    if changed == 0 {
        return Ok((None, 0));
    }
    data["updated"] = Value::from(stamp);
    Ok((Some(dump_board(&data)), changed))
}

/// (result, evidence) for one merge commit, read from the commits on origin/<base> after it. An unknown
/// result's evidence is its one reason.
fn outcome_of(
    checkout: &Path,
    base: &str,
    oid: &str,
    rework_ids: &[String],
) -> (String, Vec<String>) {
    let origin = format!("origin/{base}");
    if git(
        checkout,
        &["branch", "-r", "--contains", oid, "--list", &origin],
    )
    .is_empty()
    {
        return (
            "unknown".into(),
            vec![format!(
                "merge commit {} is not on {origin}",
                &oid[..12.min(oid.len())]
            )],
        );
    }
    let merged: i64 = git(checkout, &["show", "-s", "--format=%ct", oid])
        .parse()
        .unwrap_or(0);
    let parent = format!("{oid}^1");
    let diff = git(checkout, &["diff", "--name-only", &parent, oid]);
    let touched: BTreeSet<&str> = splitlines(&diff, false).into_iter().collect();
    let range = format!("{oid}..refs/remotes/origin/{base}");
    let log = git(
        checkout,
        &[
            "log",
            "--no-merges",
            "--name-only",
            "--format=%x1e%H%x1f%ct%x1f%B%x1f",
            &range,
        ],
    );
    let commits: Vec<Vec<String>> = log
        .split('\x1e')
        .filter(|c| !c.is_empty())
        .map(|c| {
            let mut parts: Vec<String> = format!("{c}\x1f")
                .split('\x1f')
                .take(4)
                .map(String::from)
                .collect();
            parts.resize(4, String::new());
            parts
        })
        .collect();
    let short = |sha: &str| sha.chars().take(12).collect::<String>();
    for c in &commits {
        if c[2].contains(&format!("This reverts commit {oid}")) {
            return ("reverted".into(), vec![format!("commit {}", short(&c[0]))]);
        }
    }
    let mut evidence: Vec<String> = rework_ids
        .iter()
        .map(|i| format!("rework-of {i}"))
        .collect();
    for c in &commits {
        let at: i64 = c[1].parse().unwrap_or(i64::MIN);
        let subject = c[2].split('\n').next().unwrap_or("");
        let files: BTreeSet<&str> = splitlines(&c[3], false).into_iter().collect();
        if merged <= at
            && at <= merged + OUTCOME_FIX_DAYS * 86_400
            && OUTCOME_SUBJECT.search(subject).is_some()
            && !touched.is_disjoint(&files)
        {
            evidence.push(format!("commit {}", short(&c[0])));
        }
    }
    evidence.sort();
    if evidence.is_empty() {
        ("held".into(), evidence)
    } else {
        ("reworked".into(), evidence)
    }
}

/// Why outcomes cannot read origin/<base> in checkout on device, or None once the fetch worked.
fn outcome_checkout_problem(
    checkout: &str,
    base: &str,
    repo: &str,
    device: &str,
) -> Option<String> {
    let path = Path::new(checkout);
    if !path.is_dir() {
        return Some(format!("no folder at {checkout} on {device}"));
    }
    let top = git(path, &["rev-parse", "--show-toplevel"]);
    let same = !top.is_empty()
        && matches!((resolve(Path::new(&top)), resolve(path)), (Ok(a), Ok(b)) if a == b);
    if !same {
        return Some(format!("{checkout} is not a git checkout on {device}"));
    }
    fetch_base(path, base, repo).map(|why| format!("{why} in {checkout} on {device}"))
}

/// `outcomes_cmd` after its argument check: each line it prints, plus `\n`, goes to out.
pub(crate) fn outcomes(record: bool, slug: Option<&str>, out: &mut String) -> Res<()> {
    let home = factory_home()?;
    let data = read_board(&home)?;
    let cutoff = unix_now() - OUTCOME_DAYS * 86_400;
    let recent: Vec<&Value> = board_jobs(&data)?
        .iter()
        .filter(|j| {
            j.get("stage").and_then(Value::as_str) == Some("merged")
                && j.get("done")
                    .and_then(Value::as_str)
                    .and_then(parse_stamp)
                    .is_some_and(|done| done > cutoff)
        })
        .collect();
    let project = |j: &Value| {
        j.get("project")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
            .map(String::from)
    };
    let id = |j: &Value| py_str(j.get("id"));
    let mut orphans: Vec<String> = recent
        .iter()
        .filter(|j| project(j).is_none())
        .map(|j| id(j))
        .collect();
    orphans.sort();
    let jobs: Vec<&Value> = recent
        .iter()
        .copied()
        .filter(|j| project(j).is_some_and(|p| slug.is_none_or(|s| p == s)))
        .collect();
    let skipped = (!orphans.is_empty() && slug.is_none()).then(|| {
        format!(
            "merged jobs with no project, skipped: {}",
            orphans.join(", ")
        )
    });
    let mut line = |text: &str| {
        out.push_str(text);
        out.push('\n');
    };
    if jobs.is_empty() {
        line(&format!("no jobs merged in the last {OUTCOME_DAYS} days"));
        if let Some(skipped) = &skipped {
            line(skipped);
        }
        return Ok(());
    }
    let mut rework: HashMap<String, Vec<String>> = HashMap::new();
    for j in board_jobs(&data)? {
        let refs = j
            .get("refs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|r| r.is_object());
        for r in refs {
            if r.get("kind").and_then(Value::as_str) == Some("rework-of") {
                rework
                    .entry(py_str(r.get("id")))
                    .or_default()
                    .push(job_id(j)?);
            }
        }
    }
    let (paths, device) = (read_paths(&home)?, retro_host()?);
    let mut results = Results::new();
    let mut slugs: Vec<String> = jobs.iter().filter_map(|j| project(j)).collect();
    slugs.sort();
    slugs.dedup();
    for slug in &slugs {
        let md = project_files(&home, slug)?.0;
        let meta = if md.exists() {
            frontmatter(&md)
                .map_err(|e| fail(os_error(&e, Some(&md)), 1))?
                .0
        } else {
            Vec::new()
        };
        let repo = fm_get(&meta, "repo").unwrap_or("");
        let base = fm_get(&meta, "base")
            .filter(|b| !b.is_empty())
            .unwrap_or("main");
        let checkout = fm_get(&paths, slug).unwrap_or("");
        let mut prs: Vec<(i64, Vec<String>)> = Vec::new();
        for j in jobs.iter().filter(|j| project(j).as_deref() == Some(slug)) {
            let unknown = |why: String| ("unknown".to_string(), vec![why]);
            let parts = pr_parts(j.get("pr"))?;
            let id = job_id(j)?;
            if checkout.is_empty() {
                results.insert(
                    id,
                    unknown(format!("no checkout of {slug} in paths.md on {device}")),
                );
                continue;
            }
            match parts {
                Some((pr_repo, n)) if pr_repo.to_lowercase() == repo.to_lowercase() => {
                    match prs.iter_mut().find(|(m, _)| *m == n) {
                        Some((_, ids)) => ids.push(id),
                        None => prs.push((n, vec![id])),
                    }
                }
                _ => {
                    let repo = if repo.is_empty() {
                        "the project file repo"
                    } else {
                        repo
                    };
                    results.insert(id, unknown(format!("PR is not in {repo}")));
                }
            }
        }
        if prs.is_empty() {
            continue;
        }
        if let Some(problem) = outcome_checkout_problem(checkout, base, repo, &device) {
            for ids in prs.into_iter().map(|(_, ids)| ids) {
                for i in ids {
                    results.insert(i, ("unknown".into(), vec![problem.clone()]));
                }
            }
            continue;
        }
        let mut numbers: Vec<u64> = prs.iter().map(|(n, _)| *n as u64).collect();
        numbers.sort();
        let oids = pr_merge_commits(repo, &numbers, &device);
        for (n, ids) in prs {
            let got = match &oids {
                Err(why) => ("unknown".to_string(), vec![why.clone()]),
                Ok(oids) => match oids.get(&(n as u64)).cloned().flatten() {
                    None => (
                        "unknown".to_string(),
                        vec!["no merge commit from GitHub".to_string()],
                    ),
                    Some(oid) => {
                        let reworks: Vec<String> = ids
                            .iter()
                            .flat_map(|i| rework.get(i).cloned().unwrap_or_default())
                            .collect();
                        outcome_of(Path::new(checkout), base, &oid, &reworks)
                    }
                },
            };
            for i in ids {
                results.insert(i, got.clone());
            }
        }
    }
    line(&format!(
        "outcomes of jobs merged in the last {OUTCOME_DAYS} days"
    ));
    let result_of = |j: &Value| results[&id(j)].0.clone();
    let mut rows: Vec<Vec<String>> = vec![std::iter::once("project")
        .chain(OUTCOME_RESULTS)
        .map(String::from)
        .collect()];
    for slug in &slugs {
        let mut row = vec![slug.clone()];
        for r in OUTCOME_RESULTS {
            let n = jobs
                .iter()
                .filter(|j| project(j).as_deref() == Some(slug) && result_of(j) == r)
                .count();
            row.push(n.to_string());
        }
        rows.push(row);
    }
    let widths: Vec<usize> = (0..rows[0].len())
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap())
        .collect();
    for row in &rows {
        let cells: Vec<String> = row
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
        line(&cells.join("  "));
    }
    for slug in &slugs {
        for r in OUTCOME_RESULTS {
            let mut ids: Vec<String> = jobs
                .iter()
                .filter(|j| project(j).as_deref() == Some(slug) && result_of(j) == r)
                .map(|j| id(j))
                .collect();
            ids.sort();
            if ids.is_empty() {
                continue;
            }
            let listed: Vec<String> = ids
                .iter()
                .map(|i| {
                    if r == "unknown" {
                        format!("{i} ({})", results[i].1[0])
                    } else {
                        i.clone()
                    }
                })
                .collect();
            line(&format!("{slug} {r}: {}", listed.join(", ")));
        }
    }
    if let Some(skipped) = &skipped {
        line(skipped);
    }
    if record {
        let changed = cas(&home, "/api/board", |text| {
            outcomes_change(text, &results, &now())
        })?;
        line(&if changed > 0 {
            format!("recorded {changed} outcomes on the board")
        } else {
            "outcomes unchanged; nothing written".to_string()
        });
    }
    Ok(())
}

pub fn outcomes_cmd(call: &Call) -> Res<()> {
    let args = &call.args;
    let record = args.first().is_some_and(|a| a == "record");
    let rest = if record { &args[1..] } else { &args[..] };
    if rest.len() > 1 || rest.first().is_some_and(|s| !SLUG.is_full(s)) {
        return Err(fail("usage: factory outcomes [record] [<project-slug>]", 2));
    }
    let mut out = String::new();
    let result = outcomes(record, rest.first().map(String::as_str), &mut out);
    print!("{out}");
    result
}

static COMMIT: Pattern = Pattern::new("[0-9a-f]{40}", Flags::ASCII);
static CITATION: Pattern = Pattern::new(
    r"(?<![\w./:-])((?:[\w.-]+/)*[\w-][\w.-]*\.[A-Za-z][A-Za-z0-9]{0,9}):([0-9]+)(?:-([0-9]+))?((?:,[0-9]+)*)(?![\w])",
    Flags::NONE,
);
static ARM_NAME: Pattern = Pattern::new(r"[\w.-]+", Flags::ASCII);
const BENCH_WEIGHTS: [(&str, i64); 3] = [("high", 3), ("medium", 2), ("low", 1)];
const BENCH_USAGE: &str = "usage: factory bench score <cases-file> <arm-folder>... | bench replay <cases-file> <arm-folder> --role <reviewer> [--model <model>] | bench cases <project-slug>";
const BENCH_DRAFT_USAGE: &str = "usage: factory bench cases <project-slug>";
const ROLE_ALIASES: [(&str, &str); 10] = [
    ("builder", "build"),
    ("factory-builder", "build"),
    ("spec-writer", "spec"),
    ("factory-spec-writer", "spec"),
    ("approver", "approve"),
    ("factory-approver", "approve"),
    ("factory-review-security", "review-security"),
    ("factory-review-ux", "review-ux"),
    ("factory-review-ui", "review-ui"),
    ("factory-review-code", "review-code"),
];
const REVIEW_ROLES: [&str; 4] = ["review-security", "review-ux", "review-ui", "review-code"];

type Citation = (String, String, Vec<i64>, Option<(i64, i64)>);

/// [(citation, path, lines, span)] for each distinct path:line a review cites, in order of first
/// appearance. span is (start, end) for a path:a-b range, else None.
fn find_citations(text: &str) -> Vec<Citation> {
    let mut found: Vec<Citation> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let number = |s: &str| s.parse::<i64>().unwrap_or(i64::MAX);
    for m in CITATION.find_iter(text) {
        let c = CITATION.match_start(&text[m.start()..]).unwrap();
        let whole = group(&c, 0).to_string();
        let mut path = group(&c, 1);
        while let Some(rest) = path.strip_prefix("./") {
            path = rest;
        }
        if seen.contains(&whole) || path.split('/').any(|p| p == "..") {
            continue;
        }
        seen.push(whole.clone());
        let mut lines = vec![number(group(&c, 2))];
        let end = c.get(3).map(|e| number(e.as_str()));
        lines.extend(end);
        lines.extend(
            group(&c, 4)
                .split(',')
                .filter(|x| !x.is_empty())
                .map(number),
        );
        let span = end.map(|_| (lines[0], lines[1]));
        found.push((whole, path.to_string(), lines, span));
    }
    found
}

/// The validated case list of a reviewer-benchmark cases file, failing on the first bad field.
fn bench_cases(path: &str) -> Res<Vec<Value>> {
    if !Path::new(path).is_file() {
        return Err(fail(format!("bench: no cases file at {path}"), 4));
    }
    let data = read_text(Path::new(path)).ok().and_then(|t| loads(&t).ok());
    let cases = match data.as_ref().and_then(|d| d.get("cases")) {
        Some(Value::Array(cases))
            if data.as_ref().is_some_and(Value::is_object) && !cases.is_empty() =>
        {
            cases.clone()
        }
        _ => {
            return Err(fail(
                format!(r#"bench: {path} is not a cases file: needs {{"cases": [...]}}"#),
                2,
            ))
        }
    };
    let mut seen: Vec<String> = Vec::new();
    for (n, c) in cases.iter().enumerate() {
        let empty = Map::new();
        let c = c.as_object().unwrap_or(&empty);
        let text = |k: &str| c.get(k).and_then(Value::as_str);
        let reason = match text("id") {
            None => Some("id must look like 1018-add-y".to_string()),
            Some(id) if !JOB_ID.is_full(id) => Some("id must look like 1018-add-y".to_string()),
            Some(id) if seen.iter().any(|s| s == id) => Some(format!("duplicate id {id}")),
            _ if !text("pr").is_some_and(|p| GH_PR_PARTS.is_full(p)) => {
                Some("pr must be a GitHub pull request URL".into())
            }
            _ if !text("head").is_some_and(|h| COMMIT.is_full(h)) => {
                Some("head must be a 40-character commit".into())
            }
            _ if !matches!(text("source"), Some("reverted" | "reworked")) => {
                Some("source must be reverted or reworked".into())
            }
            _ => match c.get("defects").and_then(Value::as_array) {
                Some(defects) if !defects.is_empty() => {
                    defects.iter().enumerate().find_map(|(m, d)| {
                        let m = m + 1;
                        let d = d.as_object().unwrap_or(&empty);
                        let path = d.get("path").and_then(Value::as_str);
                        let lines = d.get("lines").and_then(Value::as_array);
                        let ints: Option<Vec<i64>> = lines
                            .map(|l| l.iter().map(|x| int_of(Some(x))).collect::<Option<_>>())
                            .flatten();
                        if !path.is_some_and(|p| {
                            !p.is_empty()
                                && !p.starts_with('/')
                                && !p.contains('\\')
                                && !p.split('/').any(|s| s == "..")
                        }) {
                            Some(format!(
                                "defect {m}: path must be a relative repository path"
                            ))
                        } else if !ints.is_some_and(|l| l.len() == 2 && 1 <= l[0] && l[0] <= l[1]) {
                            Some(format!(
                                "defect {m}: lines must be [start, end] with 1 <= start <= end"
                            ))
                        } else if !d
                            .get("severity")
                            .and_then(Value::as_str)
                            .is_some_and(|s| BENCH_WEIGHTS.iter().any(|(w, _)| *w == s))
                        {
                            Some(format!("defect {m}: severity must be high, medium or low"))
                        } else {
                            None
                        }
                    })
                }
                _ => Some("defects must be a non-empty list".into()),
            },
        };
        if let Some(reason) = reason {
            return Err(fail(format!("bench: {path}: case {}: {reason}", n + 1), 2));
        }
        seen.push(text("id").unwrap().to_string());
    }
    Ok(cases)
}

/// True when a cite names the defect's path, or its basename with no folder, and a cited line or
/// span overlaps the defect's lines.
fn bench_hit(path: &str, start: i64, end: i64, citations: &[Citation]) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    citations.iter().any(|(_, cited, lines, span)| {
        (cited == path || (!cited.contains('/') && cited == base))
            && (lines.iter().any(|n| start <= *n && *n <= end)
                || span.is_some_and(|(a, b)| a <= end && b >= start))
    })
}

/// Recall, tokens and seconds for one arm folder of saved reviews, scored against the cases.
fn bench_arm(folder: &str, cases: &[Value]) -> Res<Value> {
    let mut severity: Map<String, Value> = BENCH_WEIGHTS
        .iter()
        .map(|(s, _)| (s.to_string(), json!({"caught": 0, "of": 0})))
        .collect();
    let (mut weight, mut caught_weight, mut caught, mut tokens, mut runs) =
        (0i64, 0i64, 0i64, 0i64, 0i64);
    let mut seconds = 0.0f64;
    let (mut missed, mut missing, mut no_run) = (
        Vec::<String>::new(),
        Vec::<String>::new(),
        Vec::<String>::new(),
    );
    for c in cases {
        let id = c["id"].as_str().unwrap();
        let case_dir = Path::new(folder).join(id);
        if !case_dir.is_dir() {
            missing.push(id.into());
        }
        let mut citations = Vec::new();
        for review in glob(&case_dir, "review-", ".md") {
            if !review.is_file() {
                continue;
            }
            let text = read_lines(&review, None)
                .map_err(|e| fail(os_error(&e, Some(&review)), 1))?
                .concat();
            if splitlines(&text, false).first() == Some(&"VERDICT: CHANGES") {
                citations.extend(find_citations(&text));
            }
        }
        for d in c["defects"].as_array().unwrap() {
            let sev = d["severity"].as_str().unwrap();
            let w = BENCH_WEIGHTS.iter().find(|(s, _)| *s == sev).unwrap().1;
            weight += w;
            let slot = &mut severity[sev];
            slot["of"] = Value::from(slot["of"].as_i64().unwrap() + 1);
            let path = d["path"].as_str().unwrap();
            let (start, end) = (
                int_of(d["lines"].get(0)).unwrap(),
                int_of(d["lines"].get(1)).unwrap(),
            );
            if bench_hit(path, start, end, &citations) {
                caught += 1;
                caught_weight += w;
                slot["caught"] = Value::from(slot["caught"].as_i64().unwrap() + 1);
            } else {
                missed.push(format!("{id} {path}:{start}-{end}"));
            }
        }
        let run = read_text(&case_dir.join("run.json"))
            .ok()
            .and_then(|t| loads(&t).ok());
        let secs = run
            .as_ref()
            .and_then(|r| r.get("seconds"))
            .filter(|s| s.is_number())
            .and_then(Value::as_f64)
            .filter(|s| (0.0..f64::INFINITY).contains(s));
        match (
            secs,
            run.as_ref()
                .and_then(|r| r.get("models"))
                .filter(|m| m.is_object()),
        ) {
            (Some(secs), Some(models)) if run.as_ref().is_some_and(Value::is_object) => {
                tokens += token_count(models);
                seconds += secs;
                runs += 1;
            }
            _ => no_run.push(id.into()),
        }
    }
    let recall = round_half_even(caught_weight as f64 / weight as f64, 2);
    Ok(
        json!({"arm": py_name(folder), "weighted_recall": recall, "caught": caught,
        "severity": severity, "missed": missed, "tokens": tokens, "seconds": round_int(seconds),
        "runs": runs, "missing": missing, "no_run": no_run}),
    )
}

/// Each case's job folder, project and checkout, failing on the first case that cannot run. The
/// merge commits come from GitHub, which the checks slice reads.
fn bench_replay_plan(home: &Path, cases: &[Value]) -> Res<Vec<ReplayRow>> {
    let (paths, device) = (read_paths(home)?, device_name(home));
    let mut rows = Vec::new();
    let mut prs: Vec<(String, Vec<u64>)> = Vec::new();
    for c in cases {
        let id = c["id"].as_str().unwrap();
        let mut jobs: Vec<PathBuf> = glob(&home.join("jobs"), "", "")
            .into_iter()
            .filter(|p| p.is_dir() && p.join(id).is_dir() && p.join(id).join("spec.md").exists())
            .map(|p| p.join(id))
            .collect();
        jobs.sort();
        if jobs.is_empty() {
            return Err(fail(
                format!(
                    "bench: {id}: no job folder with spec.md under {}; pull it with factory pull <project>/{id}",
                    home.join("jobs").display()
                ),
                4,
            ));
        }
        if jobs.len() > 1 {
            return Err(fail(
                format!("bench: {id}: job folders in more than one project"),
                2,
            ));
        }
        let slug = file_name(jobs[0].parent().unwrap());
        let project = project_files(home, &slug)?.0;
        let meta = if project.exists() {
            frontmatter(&project)
                .map_err(|e| fail(os_error(&e, Some(&project)), 1))?
                .0
        } else {
            Vec::new()
        };
        let repo = fm_get(&meta, "repo").unwrap_or("");
        if repo.is_empty() {
            return Err(fail(
                format!(
                    "bench: {id}: project {slug} has no repo; pull it with factory pull {slug}"
                ),
                4,
            ));
        }
        let (pr_repo, number) = pr_parts(c.get("pr"))?.unwrap();
        if pr_repo.to_lowercase() != repo.to_lowercase() {
            return Err(fail(
                format!("bench: {id}: pr is not in {repo}, the repo of project {slug}"),
                2,
            ));
        }
        if fm_get(&paths, &slug).unwrap_or("").is_empty() {
            return Err(fail(
                format!("bench: {id}: no checkout of {slug} in paths.md on {device}"),
                4,
            ));
        }
        match prs.iter_mut().find(|(r, _)| r == repo) {
            Some((_, numbers)) => numbers.push(number as u64),
            None => prs.push((repo.to_string(), vec![number as u64])),
        }
        rows.push((
            ReplayRow {
                id: id.to_string(),
                head: py_str(c.get("head")),
                job: jobs[0].clone(),
                project,
                checkout: PathBuf::from(fm_get(&paths, &slug).unwrap_or("")),
                merge: String::new(),
            },
            repo.to_string(),
            number as u64,
        ));
    }
    let mut merges = Vec::new();
    for (repo, mut numbers) in prs {
        numbers.sort();
        numbers.dedup();
        let oids = bench_merge_commits(&repo, &numbers, &device)
            .map_err(|why| fail(format!("bench: {why}"), 5))?;
        merges.push((repo, oids));
    }
    let mut out = Vec::new();
    for (mut row, repo, number) in rows {
        let merge = merges
            .iter()
            .find(|(r, _)| *r == repo)
            .and_then(|(_, o)| o.get(&number).cloned().flatten())
            .filter(|m| !m.is_empty());
        let Some(merge) = merge else {
            let pr = format!("https://github.com/{repo}/pull/{number}");
            let pr = cases
                .iter()
                .find(|c| py_str(c.get("id")) == row.id)
                .map_or(pr, |c| py_str(c.get("pr")));
            return Err(fail(
                format!("bench: {}: GitHub reports no merge commit for {pr}", row.id),
                4,
            ));
        };
        row.merge = merge;
        out.push(row);
    }
    Ok(out)
}

/// One case bench replay runs, with its job folder, project file, checkout and merge commit.
struct ReplayRow {
    id: String,
    head: String,
    job: PathBuf,
    project: PathBuf,
    checkout: PathBuf,
    merge: String,
}

const BENCH_REPLAY_SECONDS: u64 = 3600;

#[cfg(test)]
#[derive(Default)]
struct BenchFakes {
    merge_commits:
        Option<Rc<dyn Fn(&str, &[u64], &str) -> Result<BTreeMap<u64, Option<String>>, String>>>,
    replay_seconds: Option<u64>,
    elapsed: Option<u64>,
    /// What bench replay prints, stdout then stderr, which the test harness would swallow.
    out: Vec<u8>,
    err: Vec<u8>,
}

#[cfg(test)]
thread_local! {
    static BENCH_FAKES: RefCell<BenchFakes> = RefCell::new(BenchFakes::default());
}

fn bench_merge_commits(
    repo: &str,
    numbers: &[u64],
    device: &str,
) -> Result<BTreeMap<u64, Option<String>>, String> {
    #[cfg(test)]
    if let Some(f) = BENCH_FAKES.with(|b| b.borrow().merge_commits.clone()) {
        return f(repo, numbers, device);
    }
    pr_merge_commits(repo, numbers, device)
}

fn bench_replay_seconds() -> u64 {
    #[cfg(test)]
    if let Some(s) = BENCH_FAKES.with(|b| b.borrow().replay_seconds) {
        return s;
    }
    BENCH_REPLAY_SECONDS
}

/// Seconds since start, rounded as Python's round() does.
fn bench_elapsed(start: Instant) -> i64 {
    #[cfg(test)]
    if let Some(s) = BENCH_FAKES.with(|b| b.borrow().elapsed) {
        return s as i64;
    }
    round_half_even(start.elapsed().as_secs_f64(), 0) as i64
}

/// One line of bench replay's stdout.
fn bench_say(line: &str) {
    #[cfg(test)]
    BENCH_FAKES.with(|b| b.borrow_mut().out.extend(format!("{line}\n").bytes()));
    #[cfg(not(test))]
    println!("{line}");
}

/// One line of bench replay's stderr.
fn bench_warn(line: &str) {
    #[cfg(test)]
    BENCH_FAKES.with(|b| b.borrow_mut().err.extend(format!("{line}\n").bytes()));
    #[cfg(not(test))]
    eprintln!("{line}");
}
const BENCH_REPLAY_TOOLS: [&str; 7] = [
    "Read",
    "Grep",
    "Glob",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git show:*)",
    "Bash(git blame:*)",
];
const BENCH_REPLAY_DENIED: [&str; 1] = ["Bash(git *--out*)"];

/// The --agents JSON holding the factory-<role> agent file, with its tools and model, which claude cannot find with no setting sources.
fn bench_replay_agents(home: &Path, role: &str) -> Res<String> {
    let config = home.join("config.md");
    let meta = if config.exists() {
        frontmatter(&config)
            .map_err(|e| fail(os_error(&e, Some(&config)), 1))?
            .0
    } else {
        Vec::new()
    };
    let dir = match fm_get(&meta, "claude_agents_dir") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => std::env::home_dir()
            .unwrap_or_default()
            .join(".claude/agents"),
    };
    let path = dir.join(format!("factory-{role}.md"));
    let (agent, body) = if path.is_file() {
        frontmatter(&path).map_err(|e| fail(os_error(&e, Some(&path)), 1))?
    } else {
        (Vec::new(), String::new())
    };
    let Some(description) = fm_get(&agent, "description").filter(|d| !d.is_empty()) else {
        return Err(fail(
            format!(
                "bench: replay needs the factory-{role} agent at {}",
                path.display()
            ),
            4,
        ));
    };
    let mut entry = json!({"description": description, "prompt": body});
    let tools: Vec<&str> = fm_get(&agent, "tools")
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if !tools.is_empty() {
        entry["tools"] = json!(tools);
    }
    if let Some(model) = fm_get(&agent, "model").filter(|m| !m.is_empty()) {
        entry["model"] = json!(model);
    }
    Ok(dumps(&json!({ format!("factory-{role}"): entry }), DEFAULT))
}

/// True when checkout holds commit oid, fetching it from origin once when it does not.
fn bench_have_commit(checkout: &Path, oid: &str) -> bool {
    let dir = checkout.to_string_lossy();
    let object = format!("{oid}^{{commit}}");
    let have = || {
        capture(
            &["git", "-C", &dir, "cat-file", "-e", &object],
            None,
            &[],
            None,
        )
        .is_ok_and(|r| r.code == 0)
    };
    if have() {
        return true;
    }
    let _ = capture(
        &["git", "-C", &dir, "fetch", "-q", "origin", oid],
        None,
        &[("GIT_TERMINAL_PROMPT", "0")],
        Some(std::time::Duration::from_secs(60)),
    );
    have()
}

/// The reviewer prompt for one replayed case.
fn bench_replay_prompt(row: &ReplayRow, tmp: &Path, base: &str, role: &str) -> String {
    let (job, wt) = (tmp.join("job"), tmp.join("wt"));
    let stem = row
        .project
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let documents = row.project.with_file_name(format!("{stem}.documents.md"));
    let mut lines = vec![
        format!("Job id: {}", row.id),
        format!("Job folder: {}", job.display()),
        format!(
            "Worktree: {} (detached at {}, base commit {base})",
            wt.display(),
            row.head
        ),
        format!("Project file: {}", row.project.display()),
    ];
    if documents.is_file() {
        lines.push(format!("Project documents: {}", documents.display()));
    }
    lines.extend([
        format!("Round folder: {}", job.join("round-1").display()),
        format!("Your file: {}", job.join("round-1").join(format!("{role}.md")).display()),
        String::new(),
        format!("This is a reviewer benchmark replay. The pull request already merged; review it as if it were open. There is no branch factory/{}: run git diff {base}...{} wherever your instructions name the branch diff. There is no pre-review.md, and Bash runs only git diff, git log, git show and git blame. Write only Your file.", row.id, row.head),
    ]);
    lines.join("\n") + "\n"
}

/// {model: {kind: tokens}} summed over one Claude Code session's main transcript and its subagents.
fn bench_session_usage(session: &str) -> Value {
    let root = retro_transcripts();
    let mut files: Vec<(PathBuf, bool)> = Vec::new();
    for project in glob(&root, "", "") {
        let main = project.join(format!("{session}.jsonl"));
        if main.is_file() {
            files.push((main, true));
        }
    }
    for project in glob(&root, "", "") {
        for sub in glob(&project.join(session).join("subagents"), "", ".jsonl") {
            files.push((sub, false));
        }
    }
    let mut totals: Vec<(String, Counts)> = Vec::new();
    for (path, main) in files {
        if let Ok((models, _)) = usage_scan(&path, main, None) {
            for (model, counts) in models {
                add_counts(&mut totals, &model, &counts);
            }
        }
    }
    let mut out = Map::new();
    for (model, counts) in totals {
        let kinds: Map<String, Value> = USAGE_TOKENS
            .iter()
            .zip(counts)
            .map(|((k, _), n)| (k.to_string(), json!(n)))
            .collect();
        out.insert(model, Value::Object(kinds));
    }
    Value::Object(out)
}

/// A random version 4 UUID.
fn uuid4() -> String {
    let word = || {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        h.finish()
    };
    let (a, b) = (word(), word());
    let b = (b & 0x3fff_ffff_ffff_ffff) | 0x8000_0000_0000_0000;
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0x0fff,
        b >> 48,
        b & 0xffff_ffff_ffff
    )
}

/// Run argv in cwd with input on stdin and stdout and stderr in log, killing it past seconds. None on a timeout.
fn run_logged(
    argv: &[String],
    input: &str,
    cwd: &Path,
    log: fs::File,
    seconds: u64,
) -> io::Result<Option<i32>> {
    let mut child = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(cwd)
        .env("CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS", "0")
        .stdin(std::process::Stdio::piped())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let text = input.to_string();
    let feeder = std::thread::spawn(move || {
        let _ = io::Write::write_all(&mut stdin, text.as_bytes());
    });
    let deadline = Instant::now() + std::time::Duration::from_secs(seconds);
    loop {
        if let Some(status) = child.try_wait()? {
            let _ = feeder.join();
            #[cfg(unix)]
            let code = std::os::unix::process::ExitStatusExt::signal(&status).map(|s| -s);
            #[cfg(not(unix))]
            let code = None;
            return Ok(Some(status.code().or(code).unwrap_or(1)));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The allow rule that lets the replayed reviewer write only path, in Claude Code's //absolute form.
fn bench_review_rule(path: &Path) -> String {
    let mut p = path.to_string_lossy().replace('\\', "/");
    let b = p.as_bytes();
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/' {
        p = format!("/{}{}", (b[0] as char).to_ascii_lowercase(), &p[2..]);
    }
    format!("Edit(/{p})")
}

/// (Some(true), line) when the reviewer wrote its review into arm/<id>, (None, line) when one is already there,
/// (Some(false), line) when the case failed. run.json and replay.log stay once claude ran.
fn bench_replay_case(
    row: &ReplayRow,
    arm: &Path,
    role: &str,
    model: Option<&str>,
    agents: &str,
) -> Res<(Option<bool>, String)> {
    let (out, name, checkout) = (arm.join(&row.id), format!("{role}.md"), &row.checkout);
    if out.join(&name).exists() {
        return Ok((None, format!("skipped, {name} already exists")));
    }
    for oid in [&row.head, &row.merge] {
        if !bench_have_commit(checkout, oid) {
            return Ok((
                Some(false),
                format!("commit {oid} is not in {} after fetch", checkout.display()),
            ));
        }
    }
    let parent = format!("{}^1", row.merge);
    let base = git(checkout, &["merge-base", &parent, &row.head]);
    if base.is_empty() {
        return Ok((
            Some(false),
            format!(
                "no merge base of {parent} and {} in {}",
                row.head,
                checkout.display()
            ),
        ));
    }
    let tmp = temp_root()?.join(format!("factory-bench-{}-{}", row.id, &uuid4()[..8]));
    create_dir_private(&tmp).map_err(|e| fail(os_error(&e, Some(&tmp)), 1))?;
    let tmp = resolve(&tmp).map_err(|e| fail(os_error(&e, Some(&tmp)), 1))?;
    let (dir, wt) = (checkout.to_string_lossy().into_owned(), tmp.join("wt"));
    let wt_text = wt.to_string_lossy().into_owned();
    let added = capture(
        &[
            "git", "-C", &dir, "worktree", "add", "--detach", &wt_text, &row.head,
        ],
        None,
        &[],
        None,
    )
    .map_or(1, |r| r.code);
    if added != 0 {
        let _ = fs::remove_dir_all(&tmp);
        return Ok((Some(false), format!("git worktree add exited {added}")));
    }
    let result = (|| -> Res<(Option<bool>, String)> {
        let io_fail = |e: io::Error, p: &Path| fail(os_error(&e, Some(p)), 1);
        let round = tmp.join("job").join("round-1");
        fs::create_dir_all(&round).map_err(|e| io_fail(e, &round))?;
        for doc in ["spec.md", "build.md"] {
            if row.job.join(doc).is_file() {
                fs::copy(row.job.join(doc), tmp.join("job").join(doc))
                    .map_err(|e| io_fail(e, &row.job))?;
            }
        }
        let session = uuid4();
        let claude = which("claude")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let job_dir = tmp.join("job").to_string_lossy().into_owned();
        let mut argv: Vec<String> = [
            claude.as_str(),
            "-p",
            "--agent",
            &format!("factory-{role}"),
            "--agents",
            agents,
            "--session-id",
            &session,
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--permission-mode",
            "dontAsk",
            "--allowedTools",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        argv.extend(BENCH_REPLAY_TOOLS.iter().map(|s| s.to_string()));
        argv.push(bench_review_rule(&round.join(&name)));
        argv.push("--disallowedTools".to_string());
        argv.extend(BENCH_REPLAY_DENIED.iter().map(|s| s.to_string()));
        argv.extend(["--add-dir".to_string(), job_dir]);
        if let Some(model) = model {
            argv.extend(["--model".to_string(), model.to_string()]);
        }
        fs::create_dir_all(&out).map_err(|e| io_fail(e, &out))?;
        let log_path = out.join("replay.log");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let log = options.open(&log_path).map_err(|e| io_fail(e, &log_path))?;
        let start = Instant::now();
        let prompt = bench_replay_prompt(row, &tmp, &base, role);
        let limit = bench_replay_seconds();
        let code = run_logged(&argv, &prompt, &wt, log, limit)
            .map_err(|e| fail(os_error(&e, Some(Path::new(&argv[0]))), 1))?;
        let seconds = bench_elapsed(start);
        let models = bench_session_usage(&session);
        let run = json!({"role": role, "model": model, "head": row.head, "base": base, "exit": code,
                         "seconds": seconds, "models": models});
        let run_path = out.join("run.json");
        fs::write(&run_path, dumps(&run, DEFAULT)).map_err(|e| io_fail(e, &run_path))?;
        let review = round.join(&name);
        let first = if review.is_file() {
            let raw = fs::read(&review).map_err(|e| io_fail(e, &review))?;
            splitlines(&String::from_utf8_lossy(&raw), false)
                .first()
                .map(|l| l.to_string())
        } else {
            None
        };
        Ok(match code {
            None => (Some(false), format!("claude timed out after {limit} s")),
            Some(code) if code != 0 => {
                (Some(false), format!("claude exited {code}, see replay.log"))
            }
            _ => match first.filter(|l| l.starts_with("VERDICT: ")) {
                None => (
                    Some(false),
                    format!("no {name} with a VERDICT line, see replay.log"),
                ),
                Some(first) => {
                    fs::copy(&review, out.join(&name)).map_err(|e| io_fail(e, &review))?;
                    (
                        Some(true),
                        format!(
                            "wrote {name} ({first}), {seconds} s, {} tokens",
                            token_count(&models)
                        ),
                    )
                }
            },
        })
    })();
    let removed = capture(
        &["git", "-C", &dir, "worktree", "remove", &wt_text],
        None,
        &[],
        None,
    )
    .map_or(1, |r| r.code);
    if removed != 0 {
        bench_warn(&format!(
            "bench: left {}; remove it with git -C {dir} worktree remove {wt_text}",
            tmp.display()
        ));
    } else {
        let _ = fs::remove_dir_all(&tmp);
    }
    result
}

/// bench replay <cases-file> <arm-folder> --role <reviewer> [--model <model>].
fn bench_replay(args: &[String]) -> Res<()> {
    let usage = || fail(BENCH_USAGE, 2);
    let rest = args.get(2..).unwrap_or(&[]);
    if args.len() < 2 || args[..2].iter().any(|a| a.starts_with("--")) || rest.len() % 2 == 1 {
        return Err(usage());
    }
    let mut flags: Vec<(&str, &str)> = Vec::new();
    for pair in rest.chunks(2) {
        let (flag, value) = (pair[0].as_str(), pair[1].as_str());
        if !["--role", "--model"].contains(&flag) || flags.iter().any(|(f, _)| *f == flag) {
            return Err(usage());
        }
        flags.push((flag, value));
    }
    let get = |name: &str| flags.iter().find(|(f, _)| *f == name).map(|(_, v)| *v);
    let Some(role) = get("--role") else {
        return Err(usage());
    };
    let role = ROLE_ALIASES
        .iter()
        .find(|(a, _)| *a == role)
        .map_or(role, |(_, r)| *r);
    if !REVIEW_ROLES.contains(&role) {
        return Err(fail(
            "bench: --role must be review-security, review-ux, review-ui or review-code",
            2,
        ));
    }
    if get("--model").is_some_and(|m| !ROUTE_MODEL.is_full(m)) {
        return Err(fail(
            "bench: --model must be 1 to 200 letters, digits or _ . / : @ [ ] -",
            2,
        ));
    }
    let cases = bench_cases(&args[0])?;
    let (arm, name) = (Path::new(&args[1]), py_name(&args[1]));
    if !ARM_NAME.is_full(&name) {
        return Err(fail(
            format!("bench: arm name {name} must be unique letters, digits, ., _ or -"),
            2,
        ));
    }
    if arm.exists() && !arm.is_dir() {
        return Err(fail(format!("bench: {} is not a folder", arm.display()), 2));
    }
    if which("claude").is_none() {
        return Err(fail("bench: replay needs claude on PATH", 4));
    }
    let home = factory_home()?;
    let rows = bench_replay_plan(&home, &cases)?;
    let agents = bench_replay_agents(&home, role)?;
    let (mut wrote, mut skipped, mut failed) = (0, 0, 0);
    let model = get("--model");
    for row in &rows {
        if !arm.join(&row.id).join(format!("{role}.md")).exists() {
            bench_say(&format!("{}: running {role} ...", row.id));
        }
        let (ok, line) = bench_replay_case(row, arm, role, model, &agents)?;
        match ok {
            Some(true) => wrote += 1,
            None => skipped += 1,
            Some(false) => failed += 1,
        }
        let tag = if ok == Some(false) { "failed: " } else { "" };
        bench_say(&format!("{}: {tag}{line}", row.id));
    }
    bench_say(&format!(
        "replayed {wrote}, skipped {skipped}, failed {failed} of {} cases into {name}",
        rows.len()
    ));
    if failed > 0 {
        return Err(exit(1));
    }
    Ok(())
}

/// bench cases <project-slug>: draft cases for the project's reverted or reworked jobs. The PR
/// heads come from GitHub, which the checks slice reads.
fn bench_draft(args: &[String]) -> Res<()> {
    if args.len() != 1 || !SLUG.is_full(&args[0]) {
        return Err(fail(BENCH_DRAFT_USAGE, 2));
    }
    let (slug, home) = (&args[0], factory_home()?);
    let md = project_files(&home, slug)?.0;
    if !md.exists() {
        return Err(fail(format!("bench: no project file for {slug}"), 4));
    }
    let meta = frontmatter(&md)
        .map_err(|e| fail(os_error(&e, Some(&md)), 1))?
        .0;
    let repo = fm_get(&meta, "repo").unwrap_or("");
    let board = read_board(&home)?;
    let mut jobs: Vec<(String, &Value)> = Vec::new();
    for j in board_jobs(&board)?.iter().filter(|j| {
        j.get("project").and_then(Value::as_str) == Some(slug)
            && matches!(
                j.get("outcome")
                    .and_then(Value::as_object)
                    .and_then(|o| o.get("result"))
                    .and_then(Value::as_str),
                Some("reverted" | "reworked")
            )
    }) {
        jobs.push((job_id(j)?, j));
    }
    jobs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut prs: Vec<(String, i64)> = Vec::new();
    for (id, j) in &jobs {
        match pr_parts(j.get("pr"))? {
            Some((pr_repo, n)) if pr_repo.to_lowercase() == repo.to_lowercase() => {
                prs.push((id.clone(), n))
            }
            _ => {
                let repo = if repo.is_empty() {
                    "the project file repo"
                } else {
                    repo
                };
                eprintln!("bench cases skips {id}: PR is not in {repo}");
            }
        }
    }
    let numbers: BTreeSet<u64> = prs.iter().map(|(_, n)| *n as u64).collect();
    let numbers: Vec<u64> = numbers.into_iter().collect();
    let device = retro_host()?;
    let mut heads = std::collections::BTreeMap::new();
    for chunk in numbers.chunks(50) {
        let found =
            pr_states(repo, chunk, &device).map_err(|why| fail(format!("bench: {why}"), 5))?;
        heads.extend(found.into_iter().map(|(n, (_, head))| (n, head)));
    }
    let mut cases = Vec::new();
    for (id, j) in &jobs {
        let Some((_, n)) = prs.iter().find(|(i, _)| i == id) else {
            continue;
        };
        match heads
            .get(&(*n as u64))
            .and_then(Value::as_str)
            .filter(|h| COMMIT.is_full(h))
        {
            Some(head) => cases.push(json!({"id": j["id"], "pr": j["pr"], "head": head,
                                            "source": j["outcome"]["result"], "defects": []})),
            None => eprintln!("bench cases skips {id}: no head commit from GitHub"),
        }
    }
    if cases.is_empty() {
        return Err(fail(
            format!("bench: no reverted or reworked jobs to draft in {slug}"),
            3,
        ));
    }
    println!("{}", dumps(&json!({"cases": cases}), INDENT2));
    Ok(())
}

pub fn bench_cmd(call: &Call) -> Res<()> {
    let args = &call.args;
    match args.first().map(String::as_str) {
        Some("replay") => return bench_replay(&args[1..]),
        Some("cases") => return bench_draft(&args[1..]),
        _ => {}
    }
    if args.len() < 3 || args[0] != "score" {
        return Err(fail(BENCH_USAGE, 2));
    }
    let cases = bench_cases(&args[1])?;
    let mut names: Vec<String> = Vec::new();
    for folder in &args[2..] {
        if !Path::new(folder).is_dir() {
            return Err(fail(format!("bench: no arm folder at {folder}"), 4));
        }
        let name = py_name(folder);
        if !ARM_NAME.is_full(&name) || names.contains(&name) {
            return Err(fail(
                format!("bench: arm name {name} must be unique letters, digits, ., _ or -"),
                2,
            ));
        }
        names.push(name);
    }
    let arms = args[2..]
        .iter()
        .map(|f| bench_arm(f, &cases))
        .collect::<Res<Vec<_>>>()?;
    let weights: Map<String, Value> = BENCH_WEIGHTS
        .iter()
        .map(|(s, w)| (s.to_string(), Value::from(*w)))
        .collect();
    let defects: usize = cases
        .iter()
        .map(|c| c["defects"].as_array().unwrap().len())
        .sum();
    let out = json!({"cases": cases.len(), "defects": defects, "weights": weights, "arms": arms});
    println!("{}", dumps(&out, INDENT2));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run audit_flags on command with only pattern at a zero backtrack limit.
    fn flags_at_limit(
        pattern: &'static Pattern,
        command: &str,
        main: bool,
    ) -> Result<Vec<&'static str>, String> {
        crate::shared::pyre::TEST_LIMIT.with(|l| l.set(Some(0)));
        crate::shared::pyre::TEST_LIMIT_ONLY.with(|l| l.set(Some(pattern)));
        audit_flags("Bash", &Map::new(), command, false, main)
    }

    /// audit_flags on command gives the limit reason when only pattern is at a zero limit.
    fn stops_at_limit(pattern: &'static Pattern, command: &str, main: bool) {
        let why = flags_at_limit(pattern, command, main).unwrap_err();
        assert!(why.contains(crate::shared::pyre::LIMIT_HIT), "{why}");
    }

    /// The AUDIT_FLAGS pattern named flag.
    fn flag_pattern(flag: &str) -> &'static Pattern {
        &AUDIT_FLAGS.iter().find(|(f, _)| *f == flag).unwrap().1
    }

    #[test]
    fn force_push_flag_stops_at_the_backtrack_limit() {
        stops_at_limit(flag_pattern("force-push"), "git push x --force", true);
    }

    #[test]
    fn hard_reset_flag_stops_at_the_backtrack_limit() {
        stops_at_limit(flag_pattern("hard-reset"), "git reset --hard", true);
    }

    #[test]
    fn branch_delete_flag_stops_at_the_backtrack_limit() {
        stops_at_limit(flag_pattern("branch-delete"), "git branch -D x", true);
    }

    #[test]
    fn recursive_delete_flag_stops_at_the_backtrack_limit() {
        stops_at_limit(flag_pattern("recursive-delete"), "rm -rf x", true);
    }

    #[test]
    fn no_verify_flag_stops_at_the_backtrack_limit() {
        stops_at_limit(flag_pattern("no-verify"), "git commit --no-verify", true);
    }

    #[test]
    fn runs_output_check_stops_at_the_backtrack_limit() {
        stops_at_limit(&AUDIT_RUNS_OUTPUT, "cat <<EOF\ngit push --force\nEOF", true);
    }

    /// sudo has no lookaround, so it runs on the linear engine and never reaches the limit.
    #[test]
    fn sudo_flag_never_reaches_the_backtrack_limit() {
        assert_eq!(
            flags_at_limit(flag_pattern("sudo"), "sudo ls", true),
            Ok(vec!["sudo"])
        );
    }

    #[test]
    fn pipe_to_shell_flag_stops_at_the_backtrack_limit() {
        stops_at_limit(flag_pattern("pipe-to-shell"), "curl x | sh", true);
    }

    #[test]
    fn deploy_flag_stops_at_the_backtrack_limit() {
        stops_at_limit(flag_pattern("deploy"), "wrangler deploy", true);
    }

    #[test]
    fn pr_merge_flag_stops_at_the_backtrack_limit() {
        stops_at_limit(&AUDIT_PR_MERGE, "gh pr merge 7", false);
    }

    #[test]
    fn secret_file_flag_stops_at_the_backtrack_limit() {
        stops_at_limit(&AUDIT_SECRET_FILE, "cat .env", true);
    }

    #[cfg(unix)]
    #[test]
    fn outcome_of_reads_reverts_fixes_and_holds() {
        let dir = crate::shared::fs::temp_dir("outcome-of");
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let commit = |file: &str, text: &str, message: &str| {
            std::fs::write(dir.join(file), text).unwrap();
            run(&["add", "-A"]);
            run(&["commit", "-q", "-m", message]);
            run(&["rev-parse", "HEAD"])
        };
        run(&["init", "-q"]);
        commit("a.py", "1\n", "start");
        let merge = commit("a.py", "2\n", "the change");
        run(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        assert_eq!(
            outcome_of(&dir, "main", &merge, &[]),
            ("held".into(), vec![])
        );
        let fix = commit("a.py", "3\n", "Fix the change");
        commit("b.py", "x\n", "fix an unrelated file");
        run(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        assert_eq!(
            outcome_of(&dir, "main", &merge, &["1001-redo".into()]),
            (
                "reworked".into(),
                vec![
                    format!("commit {}", &fix[..12]),
                    "rework-of 1001-redo".into()
                ]
            )
        );
        let revert = commit(
            "a.py",
            "1\n",
            &format!("Revert\n\nThis reverts commit {merge}."),
        );
        run(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        assert_eq!(
            outcome_of(&dir, "main", &merge, &[]),
            ("reverted".into(), vec![format!("commit {}", &revert[..12])])
        );
        assert_eq!(
            outcome_of(&dir, "trunk", &merge, &[]).1,
            [format!(
                "merge commit {} is not on origin/trunk",
                &merge[..12]
            )]
        );
    }
    /// A repo whose commits carry the given Unix dates, and the helpers to add commits to it.
    #[cfg(unix)]
    fn dated_repo(name: &str) -> std::path::PathBuf {
        let dir = crate::shared::fs::temp_dir(name);
        dated_git(&dir, 0, &["init", "-q"]);
        dir
    }

    #[cfg(unix)]
    fn dated_git(dir: &Path, date: i64, args: &[&str]) -> String {
        let when = format!("@{date} +0000");
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .env("GIT_AUTHOR_DATE", &when)
            .env("GIT_COMMITTER_DATE", &when)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[cfg(unix)]
    fn dated_commit(dir: &Path, date: i64, file: &str, message: &str) -> String {
        std::fs::write(dir.join(file), format!("{message}\n")).unwrap();
        dated_git(dir, date, &["add", "-A"]);
        dated_git(dir, date, &["commit", "-q", "-m", message]);
        dated_git(dir, date, &["rev-parse", "HEAD"])
    }

    #[cfg(unix)]
    #[test]
    fn rework_ref() {
        let dir = dated_repo("outcome-rework-ref");
        dated_commit(&dir, 1_000, "x.txt", "start");
        let merge = dated_commit(&dir, 2_000, "x.txt", "add x");
        dated_git(
            &dir,
            2_000,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        assert_eq!(
            outcome_of(&dir, "main", &merge, &["1022-redo-x".into()]),
            ("reworked".into(), vec!["rework-of 1022-redo-x".into()])
        );
    }

    #[cfg(unix)]
    #[test]
    fn fix_after_seven_days_holds() {
        let dir = dated_repo("outcome-fix-after-seven");
        let t0 = 1_767_600_000;
        dated_commit(&dir, t0 - 100, "w.txt", "start");
        let merge = dated_commit(&dir, t0, "w.txt", "add w");
        dated_commit(&dir, t0 + 7 * 86_400 + 1, "w.txt", "Fix w");
        dated_git(
            &dir,
            t0,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        assert_eq!(
            outcome_of(&dir, "main", &merge, &[]),
            ("held".into(), vec![])
        );
        let dir = dated_repo("outcome-fix-on-seven");
        dated_commit(&dir, t0 - 100, "w.txt", "start");
        let merge = dated_commit(&dir, t0, "w.txt", "add w");
        let fix = dated_commit(&dir, t0 + 7 * 86_400, "w.txt", "Fix w");
        dated_git(
            &dir,
            t0,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        assert_eq!(
            outcome_of(&dir, "main", &merge, &[]),
            ("reworked".into(), vec![format!("commit {}", &fix[..12])])
        );
    }

    #[cfg(unix)]
    #[test]
    fn fix_before_the_merge_holds() {
        let dir = dated_repo("outcome-fix-before-merge");
        let t0 = 1_767_600_000;
        dated_commit(&dir, t0 - 100, "x.txt", "start");
        let merge = dated_commit(&dir, t0, "x.txt", "add x");
        dated_commit(&dir, t0 - 3_600, "x.txt", "fix x early");
        dated_git(
            &dir,
            t0,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        assert_eq!(
            outcome_of(&dir, "main", &merge, &[]),
            ("held".into(), vec![])
        );
    }

    use crate::shared::pyval::py_repr;
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
    fn usage_prompt_counts_universal_lines() {
        let dir = std::env::temp_dir().join(format!("audit-prompt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let user = r#"{"type": "user", "message": {"content": "hi é"}}"#;
        let meta = r#"{"type": "user", "isMeta": true, "message": {"content": "meta"}}"#;
        std::fs::write(&path, format!("\u{ff}bad\r{meta}\r\n{user}\n")).unwrap();
        assert_eq!(usage_prompt(&path), "hi \u{e9}");
        let filler = "{}\r".repeat(USAGE_PROMPT_LINES - 1);
        std::fs::write(&path, format!("{filler}{user}\n")).unwrap();
        assert_eq!(usage_prompt(&path), "hi \u{e9}");
        std::fs::write(&path, format!("{filler}{{}}\n{user}\n")).unwrap();
        assert_eq!(usage_prompt(&path), "");
        std::fs::remove_dir_all(&dir).unwrap();
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
        crate::shared::assert_linear(" -", "c", "!", 100_000, |text| {
            assert!(AUDIT_RUNS_OUTPUT.search(text).is_none())
        });
    }

    #[test]
    fn pr_merge_main_only() {
        let merge = json!({"command": "gh pr merge 7"});
        let merge = merge.as_object().unwrap();
        let raw = audit_target_raw("Bash", merge);
        assert!(audit_flags("Bash", merge, &raw, false, true)
            .unwrap()
            .is_empty());
        assert_eq!(
            audit_flags("Bash", merge, &raw, false, false).unwrap(),
            ["pr-merge"]
        );
        let push = json!({"command": "gh pr merge 7 && git push --force"});
        let push = push.as_object().unwrap();
        let raw = audit_target_raw("Bash", push);
        assert_eq!(
            audit_flags("Bash", push, &raw, false, true).unwrap(),
            ["force-push"]
        );
        let runtime = json!({"command": "\"$RUNTIME\" pr-merge factory/1017-add-x"});
        let runtime = runtime.as_object().unwrap();
        let raw = audit_target_raw("Bash", runtime);
        assert!(audit_flags("Bash", runtime, &raw, false, true)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn pr_merge_runtime_subcommand() {
        let flags = |command: &str| {
            let inp = json!({ "command": command });
            let inp = inp.as_object().unwrap();
            audit_flags("Bash", inp, &audit_target_raw("Bash", inp), false, false).unwrap()
        };
        for command in [
            "\"$RUNTIME\" pr-merge factory/1017-add-x",
            "$RUNTIME pr-merge factory/1017-add-x",
            "${RUNTIME} pr-merge factory/1017-add-x",
            "~/.agents/skills/factory/runtime/factory pr-merge factory/1017-add-x",
            "factory pr-merge factory/1017-add-x",
            "cd x && \"$RUNTIME\" pr-merge factory/1017-add-x",
            "exec \"$RUNTIME\" pr-merge factory/1017-add-x",
            "\"$HOME/bin/runtime\" pr-merge factory/1017-add-x",
            "\"$R/factory.py\" pr-merge x",
        ] {
            assert_eq!(flags(command), ["pr-merge"], "{command}");
        }
        for command in [
            "grep -n pr-merge skills/factory/SKILL.md",
            "echo \"$RUNTIME pr-merge x\"",
            "cat >> build.md <<'EOF'\n\"$RUNTIME\" pr-merge factory/1017-add-x\nEOF",
            "\"$RUNTIME\" pr-state factory/1017-add-x",
            "myfactory pr-merge x",
        ] {
            assert!(flags(command).is_empty(), "{command}");
        }
    }

    #[test]
    fn documented_command_not_flagged() {
        let command = "python3 ~/.claude/skills/factory/runtime/factory.py with-cloudflare -- sh -c 'printf \"Authorization: Bearer %s\\n\" \"$CLOUDFLARE_API_TOKEN\" | curl -H @- https://api.cloudflare.com/client/v4/user/tokens/verify'";
        let inp = json!({ "command": command });
        let inp = inp.as_object().unwrap();
        assert!(
            audit_flags("Bash", inp, &audit_target_raw("Bash", inp), false, false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn pr_merge_executed_only() {
        let flags = |command: &str| {
            let inp = json!({ "command": command });
            let inp = inp.as_object().unwrap();
            audit_flags("Bash", inp, &audit_target_raw("Bash", inp), false, false).unwrap()
        };
        for command in [
            "cat >> build.md <<'EOF'\nNext: orchestrator runs gh pr merge 7\nEOF",
            "grep -n \"gh pr merge\" docs/device.md",
            "echo 'gh pr merge 7'",
            "git commit -m \"say gh pr merge\"",
            "gh api repos/o/r/pulls/7/merge",
            "gh api \"repos/o/r/pulls/$N/merge\"",
            "grep -n \"pr-merge\" f",
        ] {
            assert!(flags(command).is_empty(), "{command}");
        }
        for command in [
            "gh pr merge 7 --merge",
            "gh api -X PUT repos/o/r/pulls/7/merge",
            "cd x && gh pr merge 7",
            "GH_TOKEN=x gh pr merge 7",
            "out=$(gh pr merge 7)",
            "bash <<EOF\ngh pr merge 7\nEOF",
            "{ gh pr merge 7; }",
            "x=`gh pr merge 7`",
            "gh api -X PUT repos/{owner}/{repo}/pulls/7/merge",
            "gh api -X PUT \"repos/o/r/pulls/7/merge\"",
            "gh api -X PUT repos/o/r/pulls/$N/merge",
            "gh \"pr\" merge 7",
            "xargs gh pr merge",
            "gh -R o/r pr merge 7",
            "GH_TOKEN=\"$(cat t)\" gh pr merge 7",
            "gh api -X PUT repos/o/r/pulls/${N}/merge",
            "gh api -X PUT \"repos/{owner}/{repo}/pulls/7/merge\"",
            "echo 7 | xargs -n1 gh pr merge",
            "gh --repo=o/r pr merge 7",
        ] {
            assert_eq!(flags(command), ["pr-merge"], "{command}");
        }
        assert_eq!(
            flags(&format!("{}x\ngh pr merge 7", "\n".repeat(30_000))),
            ["pr-merge"]
        );
        assert_eq!(
            flags("gh pr merge 7 && git push --force"),
            ["force-push", "pr-merge"]
        );
        assert_eq!(flags("sudo -E gh pr merge 7"), ["sudo", "pr-merge"]);
    }

    #[test]
    fn pr_merge_is_linear() {
        for (prefix, unit, count) in [
            (";", "a=", 50_000),
            ("gh api ", "x", 100_000),
            ("", "(a=", 33_000),
            ("", "(gh api ", 12_500),
            ("", "{gh api {a}", 9_000),
            ("", "`a=", 33_000),
            ("", "\n", 100_000),
            ("", "\nenv", 25_000),
            ("", "\na=", 33_000),
            ("(", "a/", 50_000),
            ("", "(a/", 33_000),
            ("", "($a ", 25_000),
            ("", "sudo -E ", 25_000),
            ("", "xargs -n1 ", 20_000),
            ("", ";a=\"$(", 25_000),
            ("", "a=\"$(x)\" ", 20_000),
            ("", "gh -R ", 25_000),
            ("", "gh api pulls/$", 15_000),
            ("", "($a/", 30_000),
        ] {
            crate::shared::assert_linear(prefix, unit, "", count, |text| {
                let stripped = audit_strip_quotes(&audit_strip_data_heredocs(text).unwrap());
                assert!(AUDIT_PR_MERGE.search(&stripped).is_none());
            });
        }
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

    #[test]
    fn test_sweep_session_counts_each_subagent_once() {
        const OPUS: &str = "claude-opus-5-5";
        let _lock = crate::shared::test_env_lock();
        let root = std::env::temp_dir().join(format!("audit-sweep-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let projects = root.join("claude/projects");
        let home = root.join("home");
        let prompt = |text: String| {
            json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}})
                .to_string()
                + "\n"
        };
        let write = |rel: &str, text: String| {
            let path = projects.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        let h = home.display();
        let wt = "-home-u--paseo-worktrees-ab12-factory-1017-add-x";
        write(&format!("{wt}/s1.jsonl"), prompt("build it".into()));
        write(
            &format!("{wt}/s1/subagents/agent-a1.jsonl"),
            prompt("review it".into()),
        );
        write("-home-u-dev-factory/o1.jsonl", prompt("orchestrate".into()));
        write(
            "-home-u-dev-factory/o1/subagents/agent-b1.jsonl",
            prompt(format!(
                "Your full prompt is in {h}/prompts/alpha/1017-add-x/spec.txt"
            )),
        );
        write(
            "-home-u-dev-factory/o1/subagents/agent-b2.jsonl",
            prompt(format!("Job folder: {h}/jobs/alpha/1017-add-xy")),
        );
        write(
            "-home-u-dev-factory/o2.jsonl",
            prompt(format!("Sweep {h}/jobs/alpha/1017-add-x"))
                + &turn("m7", OPUS, [1, 1, 1, 1], false, None),
        );
        write(
            "-home-u-dev-factory/o2/subagents/agent-c1.jsonl",
            prompt(format!("Prompt: {h}/prompts/alpha/1017-add-x/b.txt")),
        );
        write(
            "-home-u-dev-factory/o2/subagents/agent-c2.jsonl",
            prompt(format!("Prompt: {h}/prompts/alpha/1018-add-y/b.txt")),
        );
        let saved = std::env::var_os("CLAUDE_CONFIG_DIR");
        std::env::set_var("CLAUDE_CONFIG_DIR", root.join("claude"));
        let found = |job: &str| -> std::collections::BTreeSet<(String, bool)> {
            usage_transcripts(&home, "alpha", job)
                .unwrap()
                .into_iter()
                .map(|(p, main)| {
                    (
                        p.strip_prefix(&projects)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                        main,
                    )
                })
                .collect()
        };
        let (first, second) = (found("1017-add-x"), found("1018-add-y"));
        match saved {
            Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }
        let want = |items: &[(&str, bool)]| -> std::collections::BTreeSet<(String, bool)> {
            items.iter().map(|(p, m)| (p.to_string(), *m)).collect()
        };
        assert_eq!(
            first,
            want(&[
                (
                    "-home-u--paseo-worktrees-ab12-factory-1017-add-x/s1.jsonl",
                    true
                ),
                (
                    "-home-u--paseo-worktrees-ab12-factory-1017-add-x/s1/subagents/agent-a1.jsonl",
                    false
                ),
                ("-home-u-dev-factory/o1/subagents/agent-b1.jsonl", false),
                ("-home-u-dev-factory/o2.jsonl", true),
                ("-home-u-dev-factory/o2/subagents/agent-c1.jsonl", false),
            ])
        );
        assert_eq!(
            second,
            want(&[("-home-u-dev-factory/o2/subagents/agent-c2.jsonl", false)])
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn outcomes_change_writes_known() {
        let board = json!({"updated": "2026-01-09T12:00:00Z", "jobs": [
            {"id": "1017-add-x", "updated": "u"}, {"id": "1018-add-y", "updated": "u"},
            {"id": "1019-add-z", "updated": "u"}, {"id": "1020-add-w", "updated": "u"},
            {"id": "1021-same", "updated": "u", "outcome": {"result": "held", "at": "old", "evidence": ["e"]}}]})
        .to_string();
        let mut results = Results::new();
        for (id, result) in [
            ("1017-add-x", "held"),
            ("1018-add-y", "reworked"),
            ("1019-add-z", "reverted"),
            ("1020-add-w", "unknown"),
            ("1021-same", "held"),
        ] {
            results.insert(id.into(), (result.into(), vec!["e".into()]));
        }
        let (text, changed) =
            outcomes_change(Some(&board), &results, "2026-02-01T00:00:00Z").unwrap();
        assert_eq!(changed, 3);
        let data: Value = serde_json::from_str(&text.unwrap()).unwrap();
        assert_eq!(data["updated"], "2026-02-01T00:00:00Z");
        assert_eq!(
            data["jobs"][0]["outcome"],
            json!({"result": "held", "at": "2026-02-01T00:00:00Z", "evidence": ["e"]})
        );
        assert_eq!(data["jobs"][0]["updated"], "u");
        assert!(data["jobs"][3].get("outcome").is_none());
        assert_eq!(data["jobs"][4]["outcome"]["at"], "old");
        results.retain(|id, _| id == "1021-same");
        assert_eq!(
            outcomes_change(Some(&board), &results, "x").unwrap(),
            (None, 0)
        );
    }

    #[test]
    fn malformed_board_fails() {
        for board in [
            json!({"jobs": null}),
            json!({"updated": ""}),
            json!([]),
            json!({"jobs": ["x", {"id": "a"}]}),
        ] {
            assert_eq!(board_jobs(&board).unwrap_err().code, 1, "{board}");
        }
        assert_eq!(board_jobs(&json!({"jobs": [{}]})).unwrap().len(), 1);
        assert_eq!(job_id(&json!({"project": "alpha"})).unwrap_err().code, 1);
        assert_eq!(job_id(&json!({"id": "1017-add-x"})).unwrap(), "1017-add-x");
        assert_eq!(pr_parts(Some(&json!(7))).unwrap_err().code, 1);
        for falsy in [json!(0), json!(""), json!(null), json!([])] {
            assert_eq!(pr_parts(Some(&falsy)).unwrap(), None, "{falsy}");
        }
        assert_eq!(
            pr_parts(Some(&json!("https://github.com/example/alpha/pull/7"))).unwrap(),
            Some(("example/alpha".to_string(), 7))
        );
    }

    #[test]
    fn usage_record_malformed_board_writes_nothing() {
        use crate::shared::http::tests::{home_for, serve};
        for (name, board) in [
            ("null", r#"{"jobs": null}"#),
            (
                "entry",
                r#"{"jobs": ["x", {"id": "1017-add-x", "project": "alpha"}]}"#,
            ),
        ] {
            let (origin, seen) = serve(vec![(200, board, vec![("ETag", "\"a\"")])]);
            let home = home_for(&format!("usage-record-{name}"), &origin);
            let e = usage_record(&home, "alpha", "1017-add-x", "host-a", &json!({})).unwrap_err();
            assert_eq!(e.code, 1, "{name}");
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].method, "GET");
        }
    }
}

/// Python's BenchReplayCase: repo alpha with branch factory/1018-add-y merged into main, a home with
/// its job folder, a cases file, and a fake claude on PATH.
#[cfg(all(test, unix))]
mod bench_replay_fixture {
    use super::*;
    use crate::shared::fs::temp_dir;
    use std::ffi::OsString;
    use std::sync::MutexGuard;

    pub(super) const FAKE_CLAUDE: &str = r#"#!/bin/sh
cat > "$FAKE_LOG.stdin"
printf '%s\n' "$@" > "$FAKE_LOG.argv"
pwd > "$FAKE_LOG.cwd"
ls -ld .. > "$FAKE_LOG.parent"
printf '%s' "$CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS" > "$FAKE_LOG.ceiling"
sleep "${FAKE_SLEEP:-0}"
[ -n "$FAKE_DIRTY" ] && : > dirty.txt
your=$(sed -n 's/^Your file: //p' "$FAKE_LOG.stdin")
[ -n "$FAKE_NO_REVIEW" ] || printf 'VERDICT: CHANGES\n\n- `worker/export.js:44` drops the header row.\n' > "$your"
while [ $# -gt 0 ]; do [ "$1" = --session-id ] && session=$2; shift; done
mkdir -p "$CLAUDE_CONFIG_DIR/projects/fake"
printf '%s\n' '{"type": "assistant", "message": {"id": "m1", "model": "claude-opus-5-5", "usage": {"input_tokens": 200000, "output_tokens": 20000, "cache_creation_input_tokens": 30000, "cache_read_input_tokens": 999}}}' > "$CLAUDE_CONFIG_DIR/projects/fake/$session.jsonl"
echo fake review done
exit "${FAKE_EXIT:-0}"
"#;
    pub(super) const REVIEW: &str =
        "VERDICT: CHANGES\n\n- `worker/export.js:44` drops the header row.\n";
    pub(super) const RUNNING: &str = "1018-add-y: running review-code ...\n";
    pub(super) const AGENTS: &str = r#"{"factory-review-code": {"description": "Code reviewer.", "prompt": "Review the diff.\n", "tools": ["Read", "Grep", "Glob", "Bash", "Write"], "model": "opus"}}"#;

    pub(super) fn summary(wrote: u32, skipped: u32, failed: u32) -> String {
        format!(
            "replayed {wrote}, skipped {skipped}, failed {failed} of 1 cases into opus-medium\n"
        )
    }

    pub(super) fn models() -> Value {
        json!({"claude-opus-5-5": {"input": 200000, "output": 20000, "cache_write": 30000, "cache_read": 999}})
    }

    /// What the fake claude recorded of its one call.
    pub(super) struct Call {
        pub argv: Vec<String>,
        pub cwd: PathBuf,
        pub stdin: String,
        pub ceiling: String,
    }

    pub(super) struct Bench {
        _env: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
        pub root: PathBuf,
        pub repo: PathBuf,
        pub home: PathBuf,
        pub project: PathBuf,
        pub agent: PathBuf,
        pub job: PathBuf,
        pub cases: PathBuf,
        pub arm: PathBuf,
        pub log: PathBuf,
        pub base: String,
        pub head: String,
        pub merges: Rc<RefCell<Result<BTreeMap<u64, Option<String>>, String>>>,
        pub gh_calls: Rc<RefCell<Vec<(String, Vec<u64>, String)>>>,
    }

    impl Bench {
        pub fn new(name: &str) -> Bench {
            let env = crate::shared::test_env_lock();
            let root = temp_dir(&format!("bench-{name}"));
            let repo = root.join("alpha");
            fs::create_dir(&repo).unwrap();
            let git = |args: &[&str]| sh_git(&repo, args);
            git(&["init", "-q"]);
            git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
            fs::create_dir(repo.join("worker")).unwrap();
            fs::write(repo.join("worker/export.js"), "export const rows = [];\n").unwrap();
            git(&["add", "."]);
            git(&["commit", "-qm", "start"]);
            let base = git(&["rev-parse", "HEAD"]);
            git(&["checkout", "-qb", "factory/1018-add-y"]);
            fs::write(repo.join("worker/export.js"), "export const rows = [1];\n").unwrap();
            git(&["commit", "-qam", "add y"]);
            let head = git(&["rev-parse", "HEAD"]);
            git(&["checkout", "-q", "main"]);
            git(&[
                "merge",
                "-q",
                "--no-ff",
                "-m",
                "Merge pull request #18",
                "factory/1018-add-y",
            ]);
            let merge = git(&["rev-parse", "HEAD"]);

            let home = root.join("home");
            fs::create_dir_all(home.join("projects")).unwrap();
            let project = home.join("projects/alpha.md");
            fs::write(&project, "---\nrepo: example/alpha\n---\n").unwrap();
            fs::write(
                home.join("config.md"),
                format!(
                    "---\ndevice: host-a\nclaude_agents_dir: {}\n---\n",
                    root.join("agents").display()
                ),
            )
            .unwrap();
            let agent = root.join("agents/factory-review-code.md");
            fs::create_dir(root.join("agents")).unwrap();
            fs::write(
                &agent,
                "---\ndescription: Code reviewer.\ntools: Read, Grep, Glob, Bash, Write\nmodel: opus\n---\nReview the diff.\n",
            )
            .unwrap();
            fs::write(
                home.join("paths.md"),
                format!("---\nalpha: {}\n---\n", repo.display()),
            )
            .unwrap();
            let job = home.join("jobs/alpha/1018-add-y");
            fs::create_dir_all(&job).unwrap();
            fs::write(job.join("spec.md"), "# Add y\n").unwrap();
            fs::write(job.join("build.md"), "# Build\n").unwrap();

            let bin = root.join("bin");
            fs::create_dir(&bin).unwrap();
            let claude = bin.join("claude");
            fs::write(&claude, FAKE_CLAUDE).unwrap();
            fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            let log = root.join("claude-call");
            let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
            let mut saved = Vec::new();
            for (key, value) in [
                ("FACTORY_HOME", Some(home.display().to_string())),
                (
                    "CLAUDE_CONFIG_DIR",
                    Some(root.join("claude").display().to_string()),
                ),
                ("FAKE_LOG", Some(log.display().to_string())),
                ("PATH", Some(path)),
                ("FAKE_SLEEP", None),
                ("FAKE_EXIT", None),
                ("FAKE_NO_REVIEW", None),
                ("FAKE_DIRTY", None),
            ] {
                saved.push((key, std::env::var_os(key)));
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
            let cases = root.join("reviewer-cases.json");
            fs::write(
                &cases,
                json!({"cases": [{"id": "1018-add-y", "pr": "https://github.com/example/alpha/pull/18",
                    "head": head, "source": "reworked",
                    "defects": [{"path": "worker/export.js", "lines": [40, 52], "severity": "high"}]}]})
                .to_string(),
            )
            .unwrap();
            let merges = Rc::new(RefCell::new(Ok(BTreeMap::from([(18, Some(merge))]))));
            let gh_calls = Rc::new(RefCell::new(Vec::new()));
            let (answer, calls) = (merges.clone(), gh_calls.clone());
            BENCH_FAKES.with(|b| {
                *b.borrow_mut() = BenchFakes {
                    merge_commits: Some(Rc::new(move |repo, numbers, device| {
                        calls.borrow_mut().push((
                            repo.to_string(),
                            numbers.to_vec(),
                            device.to_string(),
                        ));
                        answer.borrow().clone()
                    })),
                    elapsed: Some(300),
                    ..BenchFakes::default()
                }
            });
            Bench {
                _env: env,
                saved,
                arm: root.join("opus-medium"),
                root,
                repo,
                home,
                project,
                agent,
                job,
                cases,
                log,
                base,
                head,
                merges,
                gh_calls,
            }
        }

        pub fn git(&self, args: &[&str]) -> String {
            sh_git(&self.repo, args)
        }

        /// bench replay on the cases file and arm with flags: (code, stdout, stderr).
        pub fn replay_to(&self, arm: &Path, flags: &[&str]) -> (i32, String, String) {
            let mut args = vec![self.cases.display().to_string(), arm.display().to_string()];
            args.extend(flags.iter().map(|f| f.to_string()));
            self.run(&args)
        }

        pub fn replay(&self, flags: &[&str]) -> (i32, String, String) {
            self.replay_to(&self.arm, flags)
        }

        pub fn run(&self, args: &[String]) -> (i32, String, String) {
            let result = bench_replay(args);
            let (out, mut err) = BENCH_FAKES.with(|b| {
                let mut b = b.borrow_mut();
                (std::mem::take(&mut b.out), std::mem::take(&mut b.err))
            });
            let code = match result {
                Ok(()) => 0,
                Err(e) => {
                    err.extend(e.stderr.bytes());
                    e.code
                }
            };
            (
                code,
                String::from_utf8(out).unwrap(),
                String::from_utf8(err).unwrap(),
            )
        }

        pub fn call(&self) -> Option<Call> {
            let read = |ext: &str| fs::read_to_string(format!("{}.{ext}", self.log.display())).ok();
            let argv = read("argv")?;
            Some(Call {
                argv: argv.lines().map(String::from).collect(),
                cwd: PathBuf::from(read("cwd").unwrap().trim_end()),
                stdin: read("stdin").unwrap(),
                ceiling: read("ceiling").unwrap(),
            })
        }

        pub fn case_dir(&self) -> PathBuf {
            self.arm.join("1018-add-y")
        }

        pub fn run_json(&self) -> Value {
            loads(&fs::read_to_string(self.case_dir().join("run.json")).unwrap()).unwrap()
        }

        pub fn worktrees(&self) -> usize {
            self.git(&["worktree", "list"]).lines().count()
        }
    }

    impl Drop for Bench {
        fn drop(&mut self) {
            BENCH_FAKES.with(|b| *b.borrow_mut() = BenchFakes::default());
            for (key, value) in &self.saved {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    pub(super) fn sh_git(repo: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-C",
            ])
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }
}

#[cfg(all(test, unix))]
mod bench_replay_test {
    use super::bench_replay_fixture::*;
    use super::*;

    #[test]
    fn test_running_example() {
        let b = Bench::new("running-example");
        assert_eq!(
            b.replay(&["--role", "review-code", "--model", "claude-opus-5-5"]),
            (
                0,
                format!("{RUNNING}1018-add-y: wrote review-code.md (VERDICT: CHANGES), 300 s, 250000 tokens\n{}", summary(1, 0, 0)),
                String::new()
            )
        );
        let dir = b.case_dir();
        assert_eq!(
            fs::read_to_string(dir.join("review-code.md")).unwrap(),
            REVIEW
        );
        assert_eq!(
            b.run_json(),
            json!({"role": "review-code", "model": "claude-opus-5-5", "head": b.head, "base": b.base,
                   "exit": 0, "seconds": 300, "models": models()})
        );
        assert_eq!(
            fs::read_to_string(dir.join("run.json")).unwrap(),
            dumps(&b.run_json(), DEFAULT)
        );
        assert_eq!(
            fs::read_to_string(dir.join("replay.log")).unwrap(),
            "fake review done\n"
        );
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &fs::metadata(dir.join("replay.log")).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            *b.gh_calls.borrow(),
            [("example/alpha".to_string(), vec![18], "host-a".to_string())]
        );
        assert_eq!(b.worktrees(), 1);
    }

    #[test]
    fn test_temp_folder_is_private() {
        let b = Bench::new("private-temp");
        assert_eq!(b.replay(&["--role", "review-code"]).0, 0);
        let parent = fs::read_to_string(format!("{}.parent", b.log.display())).unwrap();
        assert!(parent.starts_with("drwx------"), "{parent}");
    }

    #[test]
    fn test_review_rule() {
        assert_eq!(
            bench_review_rule(Path::new("/private/var/t/job/round-1/review-code.md")),
            "Edit(//private/var/t/job/round-1/review-code.md)"
        );
        assert_eq!(
            bench_review_rule(Path::new(r"C:\Users\a\Temp\job\round-1\review-code.md")),
            "Edit(//c/Users/a/Temp/job/round-1/review-code.md)"
        );
    }

    #[test]
    fn test_claude_call() {
        let b = Bench::new("claude-call");
        let documents = b.home.join("projects/alpha.documents.md");
        fs::write(&documents, "notes\n").unwrap();
        assert_eq!(
            b.replay(&["--role", "review-code", "--model", "claude-opus-5-5"])
                .0,
            0
        );
        let call = b.call().unwrap();
        let tmp = call.cwd.parent().unwrap().to_path_buf();
        assert!(file_name(&tmp).starts_with("factory-bench-1018-add-y-"));
        assert_eq!(file_name(&call.cwd), "wt");
        let at = call.argv.iter().position(|a| a == "--session-id").unwrap();
        let session = call.argv[at + 1].clone();
        let job = tmp.join("job").display().to_string();
        let rule = format!("Edit(/{job}/round-1/review-code.md)");
        assert_eq!(
            call.argv,
            [
                "-p",
                "--agent",
                "factory-review-code",
                "--agents",
                AGENTS,
                "--session-id",
                &session,
                "--setting-sources",
                "",
                "--strict-mcp-config",
                "--permission-mode",
                "dontAsk",
                "--allowedTools",
                "Read",
                "Grep",
                "Glob",
                "Bash(git diff:*)",
                "Bash(git log:*)",
                "Bash(git show:*)",
                "Bash(git blame:*)",
                &rule,
                "--disallowedTools",
                "Bash(git *--out*)",
                "--add-dir",
                &job,
                "--model",
                "claude-opus-5-5"
            ]
        );
        assert_eq!(call.ceiling, "0");
        assert_eq!(
            call.stdin,
            [
                "Job id: 1018-add-y".to_string(),
                format!("Job folder: {job}"),
                format!(
                    "Worktree: {} (detached at {}, base commit {})",
                    tmp.join("wt").display(),
                    b.head,
                    b.base
                ),
                format!("Project file: {}", b.project.display()),
                format!("Project documents: {}", documents.display()),
                format!("Round folder: {}", tmp.join("job/round-1").display()),
                format!("Your file: {}", tmp.join("job/round-1/review-code.md").display()),
                String::new(),
                format!("This is a reviewer benchmark replay. The pull request already merged; review it as if it were open. There is no branch factory/1018-add-y: run git diff {}...{} wherever your instructions name the branch diff. There is no pre-review.md, and Bash runs only git diff, git log, git show and git blame. Write only Your file.\n", b.base, b.head),
            ]
            .join("\n")
        );
        assert!(!tmp.exists());
        assert_eq!(b.worktrees(), 1);
    }

    #[test]
    fn test_alias_without_model() {
        let b = Bench::new("alias");
        let (code, _, err) = b.replay(&["--role", "factory-review-code"]);
        assert_eq!((code, err.as_str()), (0, ""));
        let call = b.call().unwrap();
        assert!(!call.argv.iter().any(|a| a == "--model"));
        assert!(!call.stdin.contains("Project documents"));
        assert_eq!(b.run_json()["model"], Value::Null);
        assert_eq!(b.worktrees(), 1);
    }

    #[test]
    fn test_agent_without_tools() {
        let b = Bench::new("no-tools");
        fs::write(
            &b.agent,
            "---\ndescription: Code reviewer.\nmodel: opus\n---\nReview the diff.\n",
        )
        .unwrap();
        assert_eq!(b.replay(&["--role", "review-code"]).0, 0);
        let argv = b.call().unwrap().argv;
        let at = argv.iter().position(|a| a == "--agents").unwrap();
        let agents: Value = serde_json::from_str(&argv[at + 1]).unwrap();
        assert_eq!(
            agents["factory-review-code"],
            json!({"description": "Code reviewer.", "prompt": "Review the diff.\n", "model": "opus"})
        );
    }

    #[test]
    fn test_temp_root_resolved() {
        let b = Bench::new("temp-root");
        let (real, link) = (b.root.join("real-tmp"), b.root.join("tmp-link"));
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        crate::shared::fs::TEMP_ROOT.with(|r| *r.borrow_mut() = Some(link));
        let code = b.replay(&["--role", "review-code"]).0;
        crate::shared::fs::TEMP_ROOT.with(|r| *r.borrow_mut() = None);
        assert_eq!(code, 0);
        let call = b.call().unwrap();
        let at = call.argv.iter().position(|a| a == "--add-dir").unwrap();
        let add_dir = PathBuf::from(&call.argv[at + 1]);
        assert_eq!(add_dir.parent().unwrap().parent().unwrap(), real);
        assert_eq!(add_dir.parent(), call.cwd.parent());
    }

    #[test]
    fn test_score_reads_replay() {
        let b = Bench::new("score");
        assert_eq!(b.replay(&["--role", "review-code"]).0, 0);
        let cases = bench_cases(&b.cases.display().to_string()).unwrap();
        let arm = bench_arm(&b.arm.display().to_string(), &cases).unwrap();
        assert_eq!(
            (&arm["caught"], &arm["weighted_recall"], &arm["missed"]),
            (&json!(1), &json!(1.0), &json!([]))
        );
        assert_eq!(
            (&arm["tokens"], &arm["seconds"], &arm["runs"]),
            (&json!(250000), &json!(300), &json!(1))
        );
        assert_eq!(b.worktrees(), 1);
    }

    #[test]
    fn test_rerun_skips() {
        let b = Bench::new("rerun");
        assert_eq!(b.replay(&["--role", "review-code"]).0, 0);
        fs::remove_file(format!("{}.argv", b.log.display())).unwrap();
        assert_eq!(
            b.replay(&["--role", "review-code"]),
            (
                0,
                format!(
                    "1018-add-y: skipped, review-code.md already exists\n{}",
                    summary(0, 1, 0)
                ),
                String::new()
            )
        );
        assert!(b.call().is_none(), "claude ran again");
        assert_eq!(b.worktrees(), 1);
    }

    #[test]
    fn test_claude_failure_keeps_run_json() {
        let b = Bench::new("claude-failure");
        std::env::set_var("FAKE_EXIT", "3");
        assert_eq!(
            b.replay(&["--role", "review-code"]),
            (
                1,
                format!(
                    "{RUNNING}1018-add-y: failed: claude exited 3, see replay.log\n{}",
                    summary(0, 0, 1)
                ),
                String::new()
            )
        );
        assert!(!b.case_dir().join("review-code.md").exists());
        let run = b.run_json();
        assert_eq!(
            (&run["exit"], &run["seconds"], &run["models"]),
            (&json!(3), &json!(300), &models())
        );
        assert_eq!(b.worktrees(), 1);
    }

    #[test]
    fn test_no_review_written() {
        let b = Bench::new("no-review");
        std::env::set_var("FAKE_NO_REVIEW", "1");
        assert_eq!(
            b.replay(&["--role", "review-code"]),
            (
                1,
                format!("{RUNNING}1018-add-y: failed: no review-code.md with a VERDICT line, see replay.log\n{}", summary(0, 0, 1)),
                String::new()
            )
        );
        assert!(!b.case_dir().join("review-code.md").exists());
        assert_eq!(b.run_json()["exit"], 0);
        assert_eq!(b.worktrees(), 1);
    }

    #[test]
    fn test_timeout() {
        let b = Bench::new("timeout");
        std::env::set_var("FAKE_SLEEP", "5");
        BENCH_FAKES.with(|f| f.borrow_mut().replay_seconds = Some(1));
        assert_eq!(
            b.replay(&["--role", "review-code"]),
            (
                1,
                format!(
                    "{RUNNING}1018-add-y: failed: claude timed out after 1 s\n{}",
                    summary(0, 0, 1)
                ),
                String::new()
            )
        );
        assert!(!b.case_dir().join("review-code.md").exists());
        let run = b.run_json();
        assert_eq!((&run["exit"], &run["models"]), (&Value::Null, &json!({})));
        assert_eq!(b.worktrees(), 1);
    }

    #[test]
    fn test_commit_missing_after_fetch() {
        let b = Bench::new("missing-commit");
        let absent = "0".repeat(40);
        let mut cases: Value = loads(&fs::read_to_string(&b.cases).unwrap()).unwrap();
        cases["cases"][0]["head"] = json!(absent);
        fs::write(&b.cases, cases.to_string()).unwrap();
        assert_eq!(
            b.replay(&["--role", "review-code"]),
            (
                1,
                format!(
                    "{RUNNING}1018-add-y: failed: commit {absent} is not in {} after fetch\n{}",
                    b.repo.display(),
                    summary(0, 0, 1)
                ),
                String::new()
            )
        );
        assert!(b.call().is_none());
        assert_eq!(b.worktrees(), 1);
    }

    #[test]
    fn test_no_merge_base() {
        let b = Bench::new("no-merge-base");
        let tree = b.git(&["rev-parse", "HEAD^{tree}"]);
        let first = b.git(&["commit-tree", &tree, "-m", "unrelated"]);
        let merge = b.git(&["commit-tree", &tree, "-p", &first, "-m", "unrelated merge"]);
        *b.merges.borrow_mut() = Ok(BTreeMap::from([(18, Some(merge.clone()))]));
        assert_eq!(
            b.replay(&["--role", "review-code"]),
            (
                1,
                format!(
                    "{RUNNING}1018-add-y: failed: no merge base of {merge}^1 and {} in {}\n{}",
                    b.head,
                    b.repo.display(),
                    summary(0, 0, 1)
                ),
                String::new()
            )
        );
        assert!(b.call().is_none());
        assert!(!b.case_dir().exists());
        assert_eq!(b.worktrees(), 1);
    }

    /// A worktree git refuses to remove stays, with its tmp folder, and bench names both.
    #[test]
    fn test_failed_worktree_remove_leaves_tmp() {
        let b = Bench::new("dirty-worktree");
        std::env::set_var("FAKE_DIRTY", "1");
        let (code, out, err) = b.replay(&["--role", "review-code"]);
        assert_eq!(code, 0, "{out}");
        let tmp = b.call().unwrap().cwd.parent().unwrap().to_path_buf();
        let wt = tmp.join("wt");
        assert_eq!(
            err,
            format!(
                "bench: left {}; remove it with git -C {} worktree remove {}\n",
                tmp.display(),
                b.repo.display(),
                wt.display()
            )
        );
        assert!(wt.join("dirty.txt").exists());
        b.git(&["worktree", "remove", "--force", &wt.display().to_string()]);
        fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(b.worktrees(), 1);
    }
}

#[cfg(all(test, unix))]
mod bench_replay_error_test {
    use super::bench_replay_fixture::*;
    use super::*;

    fn assert_fails(b: &Bench, arm: Option<&Path>, flags: &[&str], code: i32, message: &str) {
        assert_eq!(
            b.replay_to(arm.unwrap_or(&b.arm), flags),
            (code, String::new(), format!("{message}\n")),
            "{flags:?}"
        );
        assert!(b.call().is_none());
    }

    #[test]
    fn test_missing_agent() {
        let b = Bench::new("missing-agent");
        let message = format!(
            "bench: replay needs the factory-review-code agent at {}",
            b.agent.display()
        );
        fs::write(&b.agent, "---\nmodel: opus\n---\nReview the diff.\n").unwrap();
        assert_fails(&b, None, &["--role", "review-code"], 4, &message);
        fs::remove_file(&b.agent).unwrap();
        assert_fails(&b, None, &["--role", "review-code"], 4, &message);
    }

    #[test]
    fn test_bad_args() {
        let b = Bench::new("bad-args");
        for flags in [
            &[][..],
            &["--role"],
            &["--model", "claude-opus-5-5"],
            &["--role", "review-code", "--role", "review-code"],
            &["--role", "review-code", "--effort", "high"],
            &["--role", "review-code", "extra"],
        ] {
            assert_fails(&b, None, flags, 2, BENCH_USAGE);
        }
        assert_eq!(
            b.run(&[b.cases.display().to_string()]),
            (2, String::new(), format!("{BENCH_USAGE}\n"))
        );
    }

    #[test]
    fn test_role_not_a_reviewer() {
        let b = Bench::new("bad-role");
        assert_fails(
            &b,
            None,
            &["--role", "build"],
            2,
            "bench: --role must be review-security, review-ux, review-ui or review-code",
        );
    }

    #[test]
    fn test_bad_model() {
        let b = Bench::new("bad-model");
        let long = "x".repeat(201);
        for model in ["claude opus", "", &long] {
            assert_fails(
                &b,
                None,
                &["--role", "review-code", "--model", model],
                2,
                "bench: --model must be 1 to 200 letters, digits or _ . / : @ [ ] -",
            );
        }
    }

    #[test]
    fn test_arm_name() {
        let b = Bench::new("arm-name");
        assert_fails(
            &b,
            Some(&b.root.join("odd name")),
            &["--role", "review-code"],
            2,
            "bench: arm name odd name must be unique letters, digits, ., _ or -",
        );
    }

    #[test]
    fn test_arm_path_is_a_file() {
        let b = Bench::new("arm-file");
        fs::write(&b.arm, "not a folder\n").unwrap();
        assert_fails(
            &b,
            None,
            &["--role", "review-code"],
            2,
            &format!("bench: {} is not a folder", b.arm.display()),
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_no_claude() {
        // Empties PATH, so it runs alone: parallel tests need PATH to find git.
        if !crate::shared::in_own_process(module_path!(), "test_no_claude") {
            return;
        }
        let b = Bench::new("no-claude");
        std::env::set_var(
            "PATH",
            crate::shared::path_without(
                "claude",
                &std::env::var_os("PATH").unwrap(),
                &b.root.join("path"),
            ),
        );
        assert_fails(
            &b,
            None,
            &["--role", "review-code"],
            4,
            "bench: replay needs claude on PATH",
        );
    }

    #[test]
    fn test_no_job_folder() {
        let b = Bench::new("no-job-folder");
        fs::remove_file(b.job.join("spec.md")).unwrap();
        assert_fails(
            &b,
            None,
            &["--role", "review-code"],
            4,
            &format!("bench: 1018-add-y: no job folder with spec.md under {}; pull it with factory pull <project>/1018-add-y", b.home.join("jobs").display()),
        );
    }

    #[test]
    fn test_two_job_folders() {
        let b = Bench::new("two-job-folders");
        let other = b.home.join("jobs/beta/1018-add-y");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("spec.md"), "# Add y\n").unwrap();
        assert_fails(
            &b,
            None,
            &["--role", "review-code"],
            2,
            "bench: 1018-add-y: job folders in more than one project",
        );
    }

    #[test]
    fn test_repo_mismatch() {
        let b = Bench::new("repo-mismatch");
        fs::write(&b.project, "---\nrepo: example/other\n---\n").unwrap();
        assert_fails(
            &b,
            None,
            &["--role", "review-code"],
            2,
            "bench: 1018-add-y: pr is not in example/other, the repo of project alpha",
        );
    }

    #[test]
    fn test_project_without_repo() {
        let b = Bench::new("no-repo");
        for text in [Some("---\nbase: main\n---\n"), None] {
            match text {
                Some(text) => fs::write(&b.project, text).unwrap(),
                None => fs::remove_file(&b.project).unwrap(),
            }
            assert_fails(
                &b,
                None,
                &["--role", "review-code"],
                4,
                "bench: 1018-add-y: project alpha has no repo; pull it with factory pull alpha",
            );
        }
    }

    #[test]
    fn test_no_checkout() {
        let b = Bench::new("no-checkout");
        fs::write(b.home.join("paths.md"), "---\nbeta: /tmp/beta\n---\n").unwrap();
        assert_fails(
            &b,
            None,
            &["--role", "review-code"],
            4,
            "bench: 1018-add-y: no checkout of alpha in paths.md on host-a",
        );
    }

    #[test]
    fn test_gh_failed() {
        let b = Bench::new("gh-failed");
        *b.merges.borrow_mut() = Err("gh is not installed on host-a".into());
        assert_fails(
            &b,
            None,
            &["--role", "review-code"],
            5,
            "bench: gh is not installed on host-a",
        );
    }

    #[test]
    fn test_no_merge_commit() {
        let b = Bench::new("no-merge-commit");
        *b.merges.borrow_mut() = Ok(BTreeMap::from([(18, None)]));
        assert_fails(
            &b,
            None,
            &["--role", "review-code"],
            4,
            "bench: 1018-add-y: GitHub reports no merge commit for https://github.com/example/alpha/pull/18",
        );
    }
}
