//! retro: digest, push, pull, aggregate and admit.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use serde_json::Value;

use crate::cli::{not_ported, Call};
use crate::shared::consts::tag_lookalike;
use crate::shared::consts::NOTE_TAG;
use crate::shared::exit::Res;
use crate::shared::py::repr;
use crate::shared::pyjson::float_repr;
use crate::shared::pyre::{Flags, Pattern};
use crate::shared::time::{format_stamp, unix_now};
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

pub fn retro_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("retro"))
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

/// `type(v) is int` for a parsed JSON value.
fn is_int(v: &Value) -> bool {
    matches!(v, Value::Number(n) if !n.to_string().contains(['.', 'e', 'E']))
}

/// `str(x)` on a parsed JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => py_repr(v),
    }
}

/// `repr(x)` on a parsed JSON value.
fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => {
            let text = n.to_string();
            if is_int(v) {
                if text == "-0" {
                    "0".into()
                } else {
                    text
                }
            } else {
                float_repr(text.parse().unwrap())
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
#[cfg(unix)]
fn local_today() -> String {
    #[repr(C)]
    struct Tm {
        sec: std::ffi::c_int,
        min: std::ffi::c_int,
        hour: std::ffi::c_int,
        mday: std::ffi::c_int,
        mon: std::ffi::c_int,
        year: std::ffi::c_int,
        rest: [u64; 6],
    }
    extern "C" {
        fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    }
    let now = unix_now();
    let mut tm = Tm {
        sec: 0,
        min: 0,
        hour: 0,
        mday: 0,
        mon: 0,
        year: 0,
        rest: [0; 6],
    };
    // SAFETY: now is a valid time_t, and tm is larger than the platform's struct tm, which
    // localtime_r fills and does not keep.
    unsafe { localtime_r(&now, &mut tm) };
    format!("{:04}-{:02}-{:02}", tm.year + 1900, tm.mon + 1, tm.mday)
}

/// `datetime.now().date().isoformat()`.
#[cfg(windows)]
fn local_today() -> String {
    #[repr(C)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        rest: [u16; 4],
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(out: *mut SystemTime);
    }
    let mut st = SystemTime {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        rest: [0; 4],
    };
    // SAFETY: st is a SYSTEMTIME that GetLocalTime fills and does not keep.
    unsafe { GetLocalTime(&mut st) };
    format!("{:04}-{:02}-{:02}", st.year, st.month, st.day)
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
    use serde_json::json;

    #[test]
    fn py_str_matches_python() {
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!("x")), "x");
        assert_eq!(py_str(&json!(1017)), "1017");
        assert_eq!(py_str(&serde_json::from_str("2.50").unwrap()), "2.5");
        assert_eq!(py_str(&serde_json::from_str("-0").unwrap()), "0");
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
