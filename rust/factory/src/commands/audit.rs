//! The transcript commands: audit, usage, outcomes and bench.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use serde_json::Value;

use crate::cli::{not_ported, Call};
use crate::shared::consts::NOTE_SECRETS;
use crate::shared::exit::Res;
use crate::shared::py::{is_space, repr, universal_newlines};
use crate::shared::pyjson::float_repr;
use crate::shared::pyre::{Captures, Flags, Pattern};

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

pub fn audit_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("audit"))
}

pub fn usage_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("usage"))
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
}
