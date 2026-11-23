//! `pickup` and `pickup setup`: launchers, detached launches, stall takeover, the host timer and the
//! board passes.

#[cfg(test)]
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(test)]
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::cli::{not_ported, Call};
use crate::shared::board::{dump_board, escalated, lead_of, load_board, patch_job};
use crate::shared::conf::{fm_get, frontmatter, read_conf};
use crate::shared::consts::{
    BACKLOG_LINE, BACKPRESSURE_NEEDS_YOU, HOLD_MAX_HOURS, HOLD_MIN_IDEAS, IDEA, IN_FLIGHT, JOB_ID,
    MAX_RIDERS, NOTE_INVISIBLE, REVIEW_ROUTE_KEYS, ROUTE_EFFORTS, ROUTE_MODEL, SLUG, TERMINAL,
    UNATTENDED, WINDOWS,
};
use crate::shared::exit::{exit, fail, Exit, Res};
use crate::shared::fs::{is_link, read_text, regular_metadata, resolve, write_atomic};
use crate::shared::home::{factory_home, holder, skill_root, validate_home};
use crate::shared::http::{endpoint_message, normalized_origin, token_message, Response};
use crate::shared::proc::{capture, git, hostname, which, Out};
use crate::shared::project::{project_files, read_paths};
use crate::shared::py::{is_space, os_error, printable_line, repr, splitlines, strip};
use crate::shared::pyjson::{dumps, loads, DEFAULT, INDENT2};
use crate::shared::pyre::{Flags as Re, Pattern};
use crate::shared::time::{format_stamp, now, parse_stamp, unix_now};

pub(crate) struct Io<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

macro_rules! outln {
    ($io:expr, $($t:tt)*) => {{ let _ = writeln!($io.out, $($t)*); }};
}
macro_rules! errln {
    ($io:expr, $($t:tt)*) => {{ let _ = writeln!($io.err, $($t)*); }};
}

const USAGE_PICKUP: &str =
    "usage: pickup [--dry-run] [--triage-now] [--limit N] [--max-inflight N]";
const USAGE_PICKUP_SETUP: &str = "usage: pickup setup [--install]";
const TRIAGE_SWEEP_MINUTES: i64 = 30;
const FETCH_TIMEOUT_SECONDS: u64 = 60;
const SCHEDULERS: &[&str] = &["launchd", "cron", "schtasks", "none"];
const PICKUP_SCHEDULER: &str = if cfg!(target_os = "macos") {
    "launchd"
} else if cfg!(windows) {
    "schtasks"
} else {
    "cron"
};
const ORPHAN_SCAN_FILE: &str = "orphan-scan.json";
const OUTCOMES_RECORD_FILE: &str = "outcomes-record.json";
const RETRO_PUSH_FILE: &str = "retro-push.json";
const ORPHAN_KINDS: &[&str] = &["branch", "pr", "worktree", "workspace"];
const ORPHAN_NOUNS: &[(&str, &str)] = &[
    ("branch", "branches"),
    ("open PR", "open PRs"),
    ("worktree", "worktrees"),
    ("Paseo workspace", "Paseo workspaces"),
];
static HOST_NAME: Pattern = Pattern::new("[A-Za-z0-9][A-Za-z0-9._-]{0,63}", Re::ASCII);
static WORKER_MODEL: Pattern = Pattern::new(r"[A-Za-z0-9._:@/\[\]-]{1,128}", Re::ASCII);
static REPO_PATH: Pattern = Pattern::new("[A-Za-z0-9_./-]{1,200}", Re::ASCII);
static CHECKOUT_SLUG: Pattern = Pattern::new("[A-Za-z0-9_-]+", Re::NONE);
static BUNDLE_HASH: Pattern = Pattern::new("[0-9a-f]{7,64}", Re::NONE);
static POSITIVE: Pattern = Pattern::new("[1-9][0-9]*", Re::NONE);
// The timer runs this binary, so the line it writes names factory without .py.
static CRON_PICKUP: Pattern = Pattern::new(r"factory(?:\.py)?\b.*\bpickup\b", Re::NONE);
static ENV_ASSIGN: Pattern = Pattern::new("(?<![A-Za-z0-9_])[A-Za-z_][A-Za-z0-9_]*=", Re::NONE);
static TRAILING_NUMBER: Pattern = Pattern::new(r"-\d+$", Re::NONE);

/// A point that waits on a sibling slice: exit 2, which no code-5 catch in pickup swallows.
fn deferred(what: &str) -> Exit {
    not_ported(&format!("pickup {what}"))
}

// ---- test seams

#[cfg(test)]
type Api = Rc<dyn Fn(&Path, &str, &str, Option<&[u8]>, &[u16], &[(&str, &str)]) -> Res<Response>>;
#[cfg(test)]
type Hook = Option<Rc<dyn Fn(&Path)>>;
#[cfg(test)]
type Launch = Rc<
    dyn Fn(
        &Path,
        &Entry,
        &str,
        &[String],
        &[(String, String)],
    ) -> Res<(bool, Option<String>, PathBuf)>,
>;

/// One field per function or constant the Python pickup tests patch.
#[cfg(test)]
#[derive(Default)]
struct Fakes {
    api: Option<Api>,
    report_status: Option<Rc<dyn Fn(&Path, &str, Option<&str>, Option<&str>) -> bool>>,
    backlog_triage_pass: Option<Rc<dyn Fn(&Path) -> Res<()>>>,
    checkout_problem: Option<Rc<dyn Fn(&Path, &str, &str) -> Res<Option<String>>>>,
    tracer_follow_pass: Option<Rc<dyn Fn(&Path) -> Res<()>>>,
    tracer_build_pass: Option<Rc<dyn Fn(&Path, Option<&Value>) -> Res<bool>>>,
    gh_cli: Option<Rc<dyn Fn(&[&str]) -> Option<(Option<i32>, String)>>>,
    paseo_run: Option<Rc<dyn Fn(&[&str]) -> (i32, String)>>,
    route_launcher: Option<Rc<dyn Fn(&Path, &Route) -> Res<Vec<String>>>>,
    read_paths: Option<Rc<dyn Fn(&Path) -> Vec<(String, String)>>>,
    read_board: Option<Rc<dyn Fn(&Path) -> Res<Value>>>,
    doctor: Option<Rc<dyn Fn(&Path) -> Res<Value>>>,
    bundle_stamp: Option<Rc<dyn Fn() -> Option<Value>>>,
    setup_run: Option<Rc<dyn Fn(&[&str], Option<&str>) -> (i32, String)>>,
    pickup_pass: Option<Rc<dyn Fn(&mut Io, &Path) -> Res<()>>>,
    orphan_pickup_scan: Hook,
    outcomes_pickup_record: Hook,
    pick_route: Option<Rc<dyn Fn(&Path) -> Res<Option<Route>>>>,
    launch_idea: Option<Launch>,
    retro_catchup_due: Option<Rc<dyn Fn(&Path, &Local) -> bool>>,
    daily_tokens: Option<Rc<dyn Fn(i64) -> Result<i64, String>>>,
    record_agent: Option<Rc<dyn Fn(&Path, &str, &Value) -> Res<Option<Value>>>>,
    pickup_scheduler: Option<&'static str>,
    launch_wait_seconds: Option<f64>,
    exe: Option<String>,
}

#[cfg(test)]
thread_local! {
    static FAKES: RefCell<Fakes> = RefCell::new(Fakes::default());
}

#[cfg(test)]
macro_rules! fake {
    ($name:ident) => {
        FAKES.with(|f| f.borrow().$name.clone())
    };
}

/// The Worker client, through the seam the tests patch.
fn api(
    home: &Path,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    ok: &[u16],
    headers: &[(&str, &str)],
) -> Res<Response> {
    #[cfg(test)]
    if let Some(f) = fake!(api) {
        return f(home, method, path, body, ok, headers);
    }
    crate::shared::http::api(home, method, path, body, ok, headers)
}

#[cfg(not(test))]
use crate::shared::board::read_board as shared_read_board;
#[cfg(not(test))]
use crate::shared::http::cas;

/// The shared `cas` on this file's `api`, so a faked `api` reaches it, as it reaches Python's.
#[cfg(test)]
fn cas<T>(
    home: &Path,
    path: &str,
    mut change: impl FnMut(Option<&str>) -> Res<(Option<String>, T)>,
) -> Res<T> {
    for _ in 0..20 {
        let read = api(home, "GET", path, None, &[200, 404], &[])?;
        let cond = match read.header("ETag") {
            Some(etag) if read.status == 200 => (
                "If-Match",
                etag.strip_prefix("W/").unwrap_or(etag).to_string(),
            ),
            None if read.status == 200 => {
                return Err(fail(format!("{path}: the Worker sent no ETag"), 1))
            }
            _ => ("If-None-Match", "*".to_string()),
        };
        let text = (read.status == 200).then(|| read.text()).transpose()?;
        let (new, result) = change(text.as_deref())?;
        let Some(new) = new else { return Ok(result) };
        let put = api(
            home,
            "PUT",
            path,
            Some(new.as_bytes()),
            &[200, 412],
            &[(cond.0, &cond.1)],
        )?;
        if put.status == 200 {
            return Ok(result);
        }
    }
    Err(fail(
        format!("{path} kept changing under us; gave up after 20 tries"),
        5,
    ))
}

#[cfg(test)]
fn shared_read_board(home: &Path) -> Res<Value> {
    let r = api(home, "GET", "/api/board", None, &[200, 404], &[])?;
    let text = (r.status == 200).then(|| r.text()).transpose()?;
    crate::shared::board::load_board(text.as_deref())
}

fn read_board(home: &Path) -> Res<Value> {
    #[cfg(test)]
    if let Some(f) = fake!(read_board) {
        return f(home);
    }
    shared_read_board(home)
}

/// `read_paths` as a dict: a repeated slug keeps its first place and its last value.
fn paths_of(home: &Path) -> Vec<(String, String)> {
    #[cfg(test)]
    if let Some(f) = fake!(read_paths) {
        return dict(f(home));
    }
    dict(read_paths(home))
}

fn paseo_run(args: &[&str]) -> (i32, String) {
    #[cfg(test)]
    if let Some(f) = fake!(paseo_run) {
        return f(args);
    }
    crate::shared::proc::paseo_run(args)
}

fn pickup_scheduler() -> &'static str {
    #[cfg(test)]
    if let Some(s) = FAKES.with(|f| f.borrow().pickup_scheduler) {
        return s;
    }
    PICKUP_SCHEDULER
}

// ---- Python helpers

/// Pairs as a Python dict builds them: the first place and the last value of each key.
fn dict(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in pairs {
        match out.iter_mut().find(|(key, _)| *key == k) {
            Some(slot) => slot.1 = v,
            None => out.push((k, v)),
        }
    }
    out
}

fn lookup<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// `frontmatter(p)[0] if p.exists() else {}`.
fn meta(p: &Path) -> Res<Vec<(String, String)>> {
    if !p.exists() {
        return Ok(Vec::new());
    }
    frontmatter(p)
        .map(|(m, _)| dict(m))
        .map_err(|e| fail(os_error(&e, Some(p)), 1))
}

fn write_file(p: &Path, text: &str) -> Res<()> {
    write_atomic(p, text.as_bytes()).map_err(|e| fail(os_error(&e, Some(p)), 1))
}

fn mkdirs(p: &Path) -> Res<()> {
    fs::create_dir_all(p).map_err(|e| fail(os_error(&e, Some(p)), 1))
}

fn lexists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

fn is_dir(p: &str) -> bool {
    Path::new(p).is_dir()
}

fn read_json(p: &Path) -> Option<Value> {
    loads(&read_text(p).ok()?).ok()
}

/// Python `v == n` on a JSON value: a number equal to n, or a bool, which is an int.
fn eq_int(v: Option<&Value>, n: i64) -> bool {
    match v {
        Some(Value::Number(x)) => x.as_f64() == Some(n as f64),
        Some(Value::Bool(b)) => i64::from(*b) == n,
        _ => false,
    }
}

/// `type(v) is int`, as an i64.
fn int_of(v: Option<&Value>) -> Option<i64> {
    v.filter(|v| v.is_i64() || v.is_u64())
        .and_then(Value::as_i64)
}

fn get_str<'a>(j: &'a Value, key: &str) -> Option<&'a str> {
    j.get(key).and_then(Value::as_str)
}

/// The job's id when it is one pickup may paste into a command.
fn safe_id(j: &Value) -> Option<&str> {
    get_str(j, "id").filter(|id| JOB_ID.is_full(id) && !id.contains(".."))
}

/// Python's `str.split()`.
fn words(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|w| !w.is_empty()).collect()
}

fn dirname(p: &str) -> String {
    Path::new(p)
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// This binary, resolved: what the timer runs.
fn exe() -> String {
    #[cfg(test)]
    if let Some(e) = FAKES.with(|f| f.borrow().exe.clone()) {
        return e;
    }
    std::env::current_exe()
        .and_then(|p| resolve(&p))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The text a `fail` printed, without its prefix, as `setup_try` reads it.
fn message(e: &Exit) -> String {
    let text = strip(&e.stderr);
    text.strip_prefix("factory.py: ")
        .unwrap_or(text)
        .to_string()
}

// ---- OS helpers

/// Six lowercase hex characters from three random bytes: `secrets.token_hex(3)`.
fn token_hex3() -> String {
    let mut buf = [0u8; 3];
    #[cfg(unix)]
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("read /dev/urandom");
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        extern "system" {
            fn BCryptGenRandom(alg: *mut u8, buf: *mut u8, len: u32, flags: u32) -> i32;
        }
        // SAFETY: buf is writable for its 3 bytes, and the system RNG flag needs no algorithm handle.
        let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), buf.as_mut_ptr(), 3, 0x2) };
        assert_eq!(status, 0, "BCryptGenRandom failed");
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Seconds east of UTC of this host's local time at utc.
#[cfg(unix)]
fn local_offset(utc: i64) -> i64 {
    #[repr(C)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        gmtoff: std::ffi::c_long,
        zone: *const std::ffi::c_char,
    }
    extern "C" {
        fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    }
    let mut tm = std::mem::MaybeUninit::<Tm>::zeroed();
    // SAFETY: t points at a valid time_t and tm at writable storage for one struct tm.
    let done = unsafe { localtime_r(&utc, tm.as_mut_ptr()) };
    if done.is_null() {
        return 0;
    }
    // SAFETY: localtime_r filled tm, since it returned non-null.
    i64::from(unsafe { tm.assume_init() }.gmtoff)
}

#[cfg(windows)]
fn local_offset(utc: i64) -> i64 {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        weekday: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        ms: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn SystemTimeToTzSpecificLocalTime(
            tz: *const u8,
            utc: *const SystemTime,
            local: *mut SystemTime,
        ) -> i32;
    }
    let (y, m, d) = civil(utc.div_euclid(86_400));
    let t = utc.rem_euclid(86_400);
    let at = SystemTime {
        year: y as u16,
        month: m as u16,
        day: d as u16,
        hour: (t / 3600) as u16,
        minute: (t / 60 % 60) as u16,
        second: (t % 60) as u16,
        ..Default::default()
    };
    let mut local = SystemTime::default();
    // SAFETY: both are valid SYSTEMTIMEs, and a null zone means the current one.
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &at, &mut local) } == 0 {
        return 0;
    }
    let wall = days_from_civil(local.year.into(), local.month.into(), local.day.into()) * 86_400
        + i64::from(local.hour) * 3600
        + i64::from(local.minute) * 60
        + i64::from(local.second);
    wall - utc
}

/// (utc seconds, local offset) now.
fn local_now() -> (i64, i64) {
    let utc = unix_now();
    (utc, local_offset(utc))
}

/// The local date as YYYY-MM-DD.
fn local_date(utc: i64, offset: i64) -> String {
    format_stamp(utc + offset)[..10].to_string()
}

fn civil(days: i64) -> (i64, i64, i64) {
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

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// An OS error's text without the ` (os error N)` Rust adds: Python's `e.strerror`.
fn strerror(e: &io::Error) -> String {
    let text = e.to_string();
    match e.raw_os_error() {
        Some(n) => text
            .strip_suffix(&format!(" (os error {n})"))
            .unwrap_or(&text)
            .to_string(),
        None => text,
    }
}

/// Start cmd in its own session so it outlives pickup: `start_new_session=True`.
fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        extern "C" {
            fn setsid() -> i32;
        }
        // SAFETY: setsid is async-signal-safe and changes only the forked child.
        unsafe {
            cmd.pre_exec(|| {
                setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x200 | 0x8);
    }
}

#[cfg(unix)]
fn getuid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { getuid() }
}

#[cfg(not(unix))]
fn getuid() -> u32 {
    0
}

/// Python's `shlex.quote`.
fn shlex_quote(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// Python's `shlex.join`.
fn shlex_join<S: AsRef<str>>(argv: &[S]) -> String {
    argv.iter()
        .map(|a| shlex_quote(a.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Python's `shlex.split` in POSIX mode. None where it raises on an open quote or escape.
fn shlex_split(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\r' | '\n' => out.extend(word.take()),
            '\\' => word.get_or_insert_with(String::new).push(chars.next()?),
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => w.push(c),
                    }
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            c @ ('\\' | '"') => w.push(c),
                            c => w.extend(['\\', c]),
                        },
                        c => w.push(c),
                    }
                }
            }
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    out.extend(word);
    Some(out)
}

/// Python's `subprocess.list2cmdline`.
fn list2cmdline(argv: &[String]) -> String {
    let mut out = String::new();
    for arg in argv {
        if !out.is_empty() {
            out.push(' ');
        }
        let quote = arg.is_empty() || arg.contains([' ', '\t']);
        if quote {
            out.push('"');
        }
        let mut slashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    out.push_str(&"\\".repeat(slashes * 2 + 1));
                    out.push('"');
                    slashes = 0;
                }
                c => {
                    out.push_str(&"\\".repeat(slashes));
                    slashes = 0;
                    out.push(c);
                }
            }
        }
        out.push_str(&"\\".repeat(if quote { slashes * 2 } else { slashes }));
        if quote {
            out.push('"');
        }
    }
    out
}

fn plist_escape(s: &str) -> String {
    s.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn plist_value(out: &mut String, v: &Value, depth: usize) {
    let pad = "\t".repeat(depth);
    match v {
        Value::String(s) => out.push_str(&format!("{pad}<string>{}</string>\n", plist_escape(s))),
        Value::Number(n) => out.push_str(&format!("{pad}<integer>{n}</integer>\n")),
        Value::Bool(b) => out.push_str(&format!("{pad}<{b}/>\n")),
        Value::Array(a) if a.is_empty() => out.push_str(&format!("{pad}<array/>\n")),
        Value::Array(a) => {
            out.push_str(&format!("{pad}<array>\n"));
            for item in a {
                plist_value(out, item, depth + 1);
            }
            out.push_str(&format!("{pad}</array>\n"));
        }
        Value::Object(o) if o.is_empty() => out.push_str(&format!("{pad}<dict/>\n")),
        Value::Object(o) => {
            out.push_str(&format!("{pad}<dict>\n"));
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            for k in keys {
                out.push_str(&format!("{pad}\t<key>{}</key>\n", plist_escape(k)));
                plist_value(out, &o[k], depth + 1);
            }
            out.push_str(&format!("{pad}</dict>\n"));
        }
        Value::Null => {}
    }
}

/// Python's `plistlib.dumps` for strings, integers, arrays and dicts.
fn plist_dumps(v: &Value) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n",
    );
    plist_value(&mut out, v, 0);
    out.push_str("</plist>\n");
    out
}

/// Python's `plistlib.loads` for an XML plist of strings, integers, arrays and dicts, else None.
fn plist_loads(data: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(data).ok()?;
    let mut tags = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let end = rest[start..].find('>')? + start;
        tags.push((&rest[..start], &rest[start + 1..end]));
        rest = &rest[end + 1..];
    }
    if !strip(rest).is_empty() {
        return None;
    }
    tags.retain(|(_, t)| !t.starts_with('?') && !t.starts_with('!'));
    if !tags.first()?.1.starts_with("plist") || tags.last()?.1 != "/plist" {
        return None;
    }
    let mut i = 1;
    let v = plist_parse(&tags, &mut i)?;
    (i == tags.len() - 1).then_some(v)
}

fn unescape(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let end = rest[at..].find(';')? + at;
        let name = &rest[at + 1..end];
        out.push(match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let n = match name.strip_prefix("#x") {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => name.strip_prefix('#')?.parse().ok()?,
                };
                char::from_u32(n)?
            }
        });
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

fn plist_parse(tags: &[(&str, &str)], i: &mut usize) -> Option<Value> {
    let tag = tags.get(*i)?.1;
    *i += 1;
    let text = |i: &mut usize, name: &str| -> Option<String> {
        let (body, close) = tags.get(*i)?;
        *i += 1;
        (*close == format!("/{name}")).then(|| unescape(body))?
    };
    match tag {
        "string" => text(i, "string").map(Value::from),
        "string/" => Some(Value::from("")),
        "integer" => text(i, "integer")?
            .trim()
            .parse::<i64>()
            .ok()
            .map(Value::from),
        "true/" => Some(Value::Bool(true)),
        "false/" => Some(Value::Bool(false)),
        "array/" => Some(Value::Array(Vec::new())),
        "dict/" => Some(Value::Object(Map::new())),
        "array" => {
            let mut items = Vec::new();
            while tags.get(*i)?.1 != "/array" {
                items.push(plist_parse(tags, i)?);
            }
            *i += 1;
            Some(Value::Array(items))
        }
        "dict" => {
            let mut map = Map::new();
            while tags.get(*i)?.1 != "/dict" {
                if tags[*i].1 != "key" {
                    return None;
                }
                *i += 1;
                let key = text(i, "key")?;
                map.insert(key, plist_parse(tags, i)?);
            }
            *i += 1;
            Some(Value::Object(map))
        }
        _ => None,
    }
}

/// `subprocess.run(argv, input=stdin, capture_output=True, text=True, timeout=timeout)`.
fn run_input(argv: &[&str], stdin: &str, timeout: Duration) -> io::Result<Out> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().unwrap();
    let data = stdin.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        let _ = input.write_all(&data);
    });
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    };
    let out = drain(Box::new(child.stdout.take().unwrap()));
    let err = drain(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let _ = writer.join();
    let text = |h: std::thread::JoinHandle<Vec<u8>>| {
        crate::shared::py::universal_newlines(
            String::from_utf8_lossy(&h.join().unwrap()).into_owned(),
        )
    };
    Ok(Out {
        code: status.code().unwrap_or(1),
        stdout: text(out),
        stderr: text(err),
    })
}

// ---- the frame: flags, halt, status, timers and the daily hooks

struct Flags {
    dry: bool,
    triage_now: bool,
    limit: usize,
    cap: Option<usize>,
}

fn parse_flags(args: &[String]) -> Res<Flags> {
    let usage = || fail(USAGE_PICKUP, 2);
    let (mut dry, mut triage_now, mut limit, mut cap) = (false, false, None, None);
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--dry-run" && !dry {
            dry = true;
            i += 1;
            continue;
        }
        if flag == "--triage-now" && !triage_now {
            triage_now = true;
            i += 1;
            continue;
        }
        let slot = match flag {
            "--limit" => &mut limit,
            "--max-inflight" => &mut cap,
            _ => return Err(usage()),
        };
        let Some(value) = args.get(i + 1).filter(|v| POSITIVE.is_full(v)) else {
            return Err(usage());
        };
        let n = value.parse::<usize>().unwrap_or(usize::MAX);
        if slot.is_some() || flag == "--limit" && n > 5 {
            return Err(usage());
        }
        *slot = Some(n);
        i += 2;
    }
    if dry && triage_now {
        return Err(fail(
            format!("{USAGE_PICKUP}; --triage-now cannot run with --dry-run"),
            2,
        ));
    }
    Ok(Flags {
        dry,
        triage_now,
        limit: limit.unwrap_or(1),
        cap,
    })
}

pub fn pickup(call: &Call) -> Res<()> {
    let (stdout, stderr) = (io::stdout(), io::stderr());
    let (mut out, mut err) = (stdout.lock(), stderr.lock());
    let mut io = Io {
        out: &mut out,
        err: &mut err,
    };
    // pickup takes no other value spelled --home, so one in argv is the global option.
    let overridden = std::env::args().any(|a| a == "--home");
    dispatch(&mut io, &call.args, &factory_home, overridden)
}

fn dispatch(
    io: &mut Io,
    args: &[String],
    home: &dyn Fn() -> Res<PathBuf>,
    overridden: bool,
) -> Res<()> {
    if args.first().map(String::as_str) == Some("setup") {
        let install = match &args[1..] {
            [] => false,
            [flag] if flag == "--install" => true,
            _ => return Err(fail(USAGE_PICKUP_SETUP, 2)),
        };
        return pickup_setup(io, &home()?, install, overridden);
    }
    let flags = parse_flags(args)?;
    run(io, &home()?, &flags)
}

/// One pickup run past the flags: Python's `pickup` after its parser.
fn run(io: &mut Io, home: &Path, flags: &Flags) -> Res<()> {
    let (device, _, minutes) = holder(home)?;
    let started = now();
    let mut failing = cached_status(home).2;
    let result = run_passes(io, home, flags, &device, minutes, &started, &mut failing);
    if !flags.dry {
        report_status(
            home,
            pickup_scheduler(),
            Some(&started),
            failing.as_deref(),
            None,
            true,
        );
    }
    result
}

/// A Worker failure (exit 5) inside a pass costs its message and one line; any other exit stops pickup.
fn stopped(io: &mut Io, e: Exit, what: &str) -> Res<()> {
    if e.code != 5 {
        return Err(e);
    }
    let _ = io.err.write_all(e.stderr.as_bytes());
    errln!(io, "factory.py: {what}");
    Ok(())
}

fn run_passes(
    io: &mut Io,
    home: &Path,
    flags: &Flags,
    device: &str,
    minutes: u32,
    started: &str,
    failing: &mut Option<String>,
) -> Res<()> {
    if let Some(halt) = read_halt(home)? {
        if flags.dry {
            outln!(io, "skip all: {}", halt_line(&halt));
        } else {
            errln!(
                io,
                "factory.py: pickup launches nothing; {}",
                halt_line(&halt)
            );
        }
        return Ok(());
    }
    let mut data = if lexists(&home.join("tracer.json")) {
        None
    } else {
        Some(read_board(home)?)
    };
    if !flags.dry {
        if let Err(e) = tracer_follow_pass(home) {
            stopped(
                io,
                e,
                "Tracer follow pass stopped; followed jobs wait for the next pickup run",
            )?;
        }
        match tracer_build_pass(io, home, data.as_ref().map(|d| &d["jobs"])) {
            Ok(true) => data = None,
            Ok(false) => {}
            Err(e) => stopped(
                io,
                e,
                "Tracer build pass stopped; Building in Tracer marks wait for the next pickup run",
            )?,
        }
        if flags.triage_now || triage_sweep_due(home, started) {
            save_triage_sweep(home, started)?;
            data = None;
            match backlog_triage_pass(home) {
                Ok(()) => *failing = None,
                Err(e) => {
                    stopped(io, e, "backlog triage stopped; unsent lines wait for the next sweep, or run pickup --triage-now")?;
                    failing.get_or_insert_with(|| started.to_string());
                }
            }
        }
        retro_pickup_push(home)?;
        audit_pickup_push(home)?;
        orphan_pickup_scan(io, home)?;
        outcomes_pickup_record(home)?;
    }
    retro_pickup_catchup(io, home, flags.dry);
    pickup_pass(
        io,
        home,
        device,
        minutes,
        flags.dry,
        flags.limit,
        flags.cap,
        data,
    )
}

fn triage_sweep_due(home: &Path, stamp: &str) -> bool {
    let Some(data) = read_json(&home.join("backlog-triage-sweep.json")) else {
        return true;
    };
    if !data.is_object() || !eq_int(data.get("version"), 1) {
        return true;
    }
    let (Some(last), Some(at)) = (
        get_str(&data, "at").and_then(parse_stamp),
        parse_stamp(stamp),
    ) else {
        return true;
    };
    !(0..TRIAGE_SWEEP_MINUTES * 60).contains(&(at - last))
}

fn save_triage_sweep(home: &Path, stamp: &str) -> Res<()> {
    write_file(
        &home.join("backlog-triage-sweep.json"),
        &(dumps(&json!({"version": 1, "at": stamp}), INDENT2) + "\n"),
    )
}

fn check_path(value: &str) -> Option<&'static str> {
    if !REPO_PATH.is_full(value) || value.starts_with('/') || value.split('/').any(|s| s == "..") {
        return Some("must be a relative repo path of [A-Za-z0-9_./-] with no .. segment");
    }
    None
}

#[derive(Clone, Debug, PartialEq)]
struct Route {
    name: String,
    provider: String,
    model: String,
    effort: Option<String>,
}

fn parse_route(value: &str) -> Option<(String, String, Option<String>)> {
    let parts = words(value);
    if !(2..=3).contains(&parts.len())
        || !SLUG.is_full(parts[0])
        || !ROUTE_MODEL.is_full(parts[1])
        || parts.len() == 3 && !ROUTE_EFFORTS.contains(&parts[2])
    {
        return None;
    }
    Some((
        parts[0].to_string(),
        parts[1].to_string(),
        parts.get(2).map(|e| e.to_string()),
    ))
}

fn read_routes(home: &Path) -> Res<Vec<Route>> {
    let path = home.join("routes.md");
    let meta = meta(&path)?;
    if meta.is_empty() {
        return Err(fail(format!("no routes in {}: list this host's dispatch routes, primary first, as <name>: <provider> <model> [<effort>]", path.display()), 2));
    }
    let mut routes = Vec::new();
    for (name, value) in &meta {
        if name == "experiment" || REVIEW_ROUTE_KEYS.contains(&name.as_str()) {
            continue;
        }
        let Some((provider, model, effort)) = parse_route(value).filter(|_| SLUG.is_full(name))
        else {
            return Err(fail(
                format!(
                    "bad route {} in {}: use <name>: <provider> <model> [<effort>]",
                    repr(name),
                    path.display()
                ),
                2,
            ));
        };
        routes.push(Route {
            name: name.clone(),
            provider,
            model,
            effort,
        });
    }
    Ok(routes)
}

fn read_limits(home: &Path) -> Res<Vec<(String, String)>> {
    let path = home.join("route-limits.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bad = || {
        fail(
            format!(
                "cannot read {}: delete it to clear every route limit",
                path.display()
            ),
            2,
        )
    };
    let text = match read_text(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::InvalidData => return Err(bad()),
        Err(e) => return Err(fail(os_error(&e, Some(&path)), 1)),
    };
    let doc = loads(&text).map_err(|_| bad())?;
    let mut limits = Vec::new();
    for (k, v) in doc.as_object().ok_or_else(bad)? {
        let until = v.as_str().ok_or_else(bad)?;
        limits.push((k.clone(), until.to_string()));
    }
    if limits.iter().any(|(_, until)| parse_stamp(until).is_none()) {
        return Err(bad());
    }
    Ok(limits)
}

/// `pickup_config`'s (enabled, hosts, reason).
#[derive(Clone, Debug, PartialEq)]
struct Settings {
    enabled: Option<bool>,
    hosts: Vec<String>,
    reason: Option<String>,
}

impl Settings {
    fn off(reason: &str) -> Settings {
        Settings {
            enabled: None,
            hosts: Vec::new(),
            reason: Some(reason.to_string()),
        }
    }

    /// `enabled and device in hosts`.
    fn lists(&self, device: &str) -> bool {
        self.enabled == Some(true) && self.hosts.iter().any(|h| h == device)
    }
}

fn pickup_config(home: &Path, slug: &str) -> Res<Settings> {
    let path = format!("/api/pipeline/effective/{slug}");
    let r = api(home, "GET", &path, None, &[200, 404, 409], &[])?;
    if r.status == 404 {
        return Ok(Settings::off(
            "no pipeline view; register the project or deploy the current Worker",
        ));
    }
    if r.status == 409 {
        return Ok(Settings::off(
            "stored pipeline configuration no longer validates",
        ));
    }
    if let Some(view) = loads(&r.text()?).ok().as_ref().and_then(Value::as_object) {
        let Some(p) = view.get("pickup") else {
            return Ok(Settings {
                enabled: Some(false),
                hosts: Vec::new(),
                reason: None,
            });
        };
        if let (Some(enabled), Some(hosts)) = (
            p.get("enabled").and_then(Value::as_bool),
            p.get("hosts").and_then(Value::as_array),
        ) {
            let names: Vec<String> = hosts
                .iter()
                .filter_map(|h| h.as_str().filter(|h| HOST_NAME.is_full(h)))
                .map(String::from)
                .collect();
            if names.len() == hosts.len() {
                return Ok(Settings {
                    enabled: Some(enabled),
                    hosts: names,
                    reason: None,
                });
            }
        }
    }
    Ok(Settings::off("malformed pickup in the pipeline view"))
}

fn pickup_settings(home: &Path, slug: &str, repo: &str, device: &str) -> Res<Settings> {
    let settings = pickup_config(home, slug)?;
    if settings.lists(device) {
        if let Some(problem) = checkout_problem(home, slug, repo)? {
            return Ok(Settings::off(&problem));
        }
    }
    Ok(settings)
}

fn read_halt(home: &Path) -> Res<Option<Value>> {
    let r = api(home, "GET", "/api/halt", None, &[200, 404], &[])?;
    if r.status == 404 {
        return Ok(None);
    }
    let record = loads(&r.text()?).ok().filter(Value::is_object);
    let halted = record
        .as_ref()
        .and_then(|r| r.get("halted"))
        .and_then(Value::as_bool);
    match halted {
        None => Ok(Some(
            json!({"halted": true, "reason": "the Worker's halt reply is unreadable"}),
        )),
        Some(halted) => Ok(record.filter(|_| halted)),
    }
}

fn halt_line(record: &Value) -> String {
    let fallback = match get_str(record, "by_kind") {
        Some("person") => "a person",
        Some("host") => "a host",
        _ => "an unknown source",
    };
    let by = get_str(record, "by").unwrap_or(fallback);
    let at = get_str(record, "at").unwrap_or("an unknown time");
    let reason = get_str(record, "reason").unwrap_or("no reason given");
    format!(
        "factory stopped by {} at {}: {}",
        printable_line(by, 80),
        printable_line(at, 40),
        printable_line(reason, 200)
    )
}

fn read_launchers(home: &Path) -> Res<Vec<(String, Vec<String>)>> {
    let path = home.join("launchers.json");
    if !path.exists() {
        return Err(fail(format!("no launchers in {}: map each route in routes.md to a command; see references/pickup.md", path.display()), 2));
    }
    let doc = read_text(&path).ok().and_then(|t| loads(&t).ok());
    let launchers = doc.as_ref().and_then(|doc| {
        let map = doc.as_object()?;
        if map.len() != 2 || int_of(map.get("version")) != Some(1) {
            return None;
        }
        let mut out = Vec::new();
        for (route, argv) in map.get("launchers")?.as_object()? {
            let argv = argv.as_array()?;
            if !SLUG.is_full(route) || !(1..=64).contains(&argv.len()) {
                return None;
            }
            let args = argv
                .iter()
                .map(|a| {
                    a.as_str()
                        .filter(|a| (1..=4096).contains(&a.chars().count()) && !a.contains('\0'))
                        .map(String::from)
                })
                .collect::<Option<Vec<String>>>()?;
            out.push((route.clone(), args));
        }
        Some(out)
    });
    launchers.ok_or_else(|| {
        fail(
            format!(
                "cannot read {}: use {{\"version\": 1, \"launchers\": {{\"<route>\": [\"<program>\", \"<arg>\", ...]}}}}",
                path.display()
            ),
            2,
        )
    })
}

/// The resolved argv template for a route.
fn route_launcher(home: &Path, route: &Route) -> Res<Vec<String>> {
    #[cfg(test)]
    if let Some(f) = fake!(route_launcher) {
        return f(home, route);
    }
    let name = &route.name;
    let path = home.join("launchers.json");
    let Some((_, mut template)) = read_launchers(home)?.into_iter().find(|(r, _)| r == name) else {
        return Err(fail(
            format!("no launcher for route {name} in {}", path.display()),
            2,
        ));
    };
    if !Path::new(&template[0]).is_absolute() {
        let Some(program) = which(&template[0]) else {
            return Err(fail(
                format!(
                    "launcher for route {name} runs {}, which is not on PATH",
                    template[0]
                ),
                2,
            ));
        };
        template[0] = program.to_string_lossy().into_owned();
    }
    if route.effort.is_none() && template.iter().any(|a| a.contains("{effort}")) {
        return Err(fail(
            format!("route {name} sets no effort, but its launcher uses {{effort}}"),
            2,
        ));
    }
    Ok(template)
}

fn host_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "linux") {
        let release = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        if release.to_lowercase().contains("microsoft") {
            "Linux (WSL)"
        } else {
            "Linux"
        }
    } else {
        "Other"
    }
}

/// {commit, tree} from the bundle.json install.py writes beside the runtime, or None.
fn bundle_stamp() -> Option<Value> {
    #[cfg(test)]
    if let Some(f) = fake!(bundle_stamp) {
        return f();
    }
    let stamp = read_json(&skill_root().ok()?.join("bundle.json"))?;
    let map = stamp.as_object()?;
    let ok = map.len() == 2
        && ["commit", "tree"].iter().all(|k| {
            map.get(*k)
                .and_then(Value::as_str)
                .is_some_and(|v| BUNDLE_HASH.is_full(v))
        });
    ok.then_some(stamp)
}

/// When the first merge on origin's base that commit lacks landed, from the first checkout
/// holding commit. None when commit is current or no checkout holds it.
fn bundle_behind_since(home: &Path, commit: &str) -> Option<String> {
    let mut paths = paths_of(home);
    paths.sort();
    for (slug, repo) in paths {
        if !CHECKOUT_SLUG.is_full(&slug) || !is_dir(&repo) {
            continue;
        }
        let Ok((project, _, _)) = project_files(home, &slug) else {
            continue;
        };
        let meta = meta(&project).ok()?;
        let base = lookup(&meta, "base").unwrap_or("main");
        if check_path(base).is_some() || base.starts_with('-') {
            continue;
        }
        let origin = format!("refs/remotes/origin/{base}");
        let repo = Path::new(&repo);
        if git(repo, &["merge-base", commit, &origin]) != commit {
            continue;
        }
        let range = format!("{commit}..{origin}");
        let log = git(repo, &["log", "--first-parent", "--format=%ct", &range]);
        return words(&log)
            .last()
            .and_then(|t| t.parse::<i64>().ok())
            .map(format_stamp);
    }
    None
}

/// (scheduler, last_pickup, triage_failed_since) from the last status pickup sent.
fn cached_status(home: &Path) -> (String, Option<String>, Option<String>) {
    let utc = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .filter(|s| parse_stamp(s).map(format_stamp).as_deref() == Some(*s))
            .map(String::from)
    };
    let cache = read_json(&home.join("host-status.json"));
    if let Some(cache) = cache.as_ref().and_then(Value::as_object) {
        if let (Some(scheduler), Some(last)) = (cache.get("scheduler"), cache.get("last_pickup")) {
            let scheduler = scheduler.as_str().filter(|s| SCHEDULERS.contains(s));
            if let Some(scheduler) =
                scheduler.filter(|_| last.is_null() || utc(Some(last)).is_some())
            {
                return (
                    scheduler.to_string(),
                    utc(Some(last)),
                    utc(cache.get("triage_failed_since")),
                );
            }
        }
    }
    ("none".into(), None, None)
}

/// The status body the Worker's PUT /api/hosts/<id>/status accepts. Holds no path or secret.
fn host_status(
    home: &Path,
    report: &Value,
    scheduler: &str,
    last_pickup: Option<&str>,
    triage_failed_since: Option<&str>,
) -> Res<Value> {
    let failed: Vec<Value> = report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["status"] != "pass")
        .map(|c| c["name"].clone())
        .collect();
    let (routes, limits) = match read_routes(home).and_then(|r| Ok((r, read_limits(home)?))) {
        Ok(pair) => pair,
        // An OS error is not a SystemExit, so Python lets it end report_status.
        Err(e) if e.code == 1 => return Err(e),
        Err(_) => (Vec::new(), Vec::new()),
    };
    let stamp = now();
    let mut checkouts: Vec<String> = paths_of(home)
        .into_iter()
        .filter(|(slug, path)| CHECKOUT_SLUG.is_full(slug) && is_dir(path))
        .map(|(slug, _)| slug)
        .collect();
    checkouts.sort();
    checkouts.truncate(64);
    let routes: Vec<Value> = routes
        .iter()
        .filter(|r| {
            HOST_NAME.is_full(&r.name)
                && HOST_NAME.is_full(&r.provider)
                && WORKER_MODEL.is_full(&r.model)
        })
        .take(16)
        .map(|r| {
            let until = lookup(&limits, &r.name).filter(|u| *u > stamp.as_str());
            json!({"name": r.name, "provider": r.provider, "model": r.model, "effort": r.effort,
                   "limited_until": until})
        })
        .collect();
    let bundle = bundle_stamp();
    let doctor = if failed.is_empty() {
        Value::from("ok")
    } else {
        Value::Array(failed)
    };
    let mut body = json!({
        "os": host_os(), "scheduler": scheduler, "last_pickup": last_pickup, "bundle": bundle,
        "doctor": doctor, "routes": routes, "checkouts": checkouts,
    });
    if let Some(commit) = bundle.as_ref().and_then(|b| get_str(b, "commit")) {
        if let Some(behind) = bundle_behind_since(home, commit) {
            body["bundle_behind_since"] = Value::from(behind);
        }
    }
    if let Some(since) = triage_failed_since.filter(|s| !s.is_empty()) {
        body["triage_failed_since"] = Value::from(since);
    }
    Ok(body)
}

/// PUT this host's status when config.md sets device:. Silent in every outcome.
fn report_status(
    home: &Path,
    scheduler: &str,
    last_pickup: Option<&str>,
    triage_failed_since: Option<&str>,
    report: Option<Value>,
    save: bool,
) -> bool {
    #[cfg(test)]
    if let Some(f) = fake!(report_status) {
        return f(home, scheduler, last_pickup, triage_failed_since);
    }
    let sent = || -> Res<bool> {
        let config = meta(&home.join("config.md"))?;
        let device = lookup(&config, "device").unwrap_or("");
        if !HOST_NAME.is_full(device) {
            return Ok(false);
        }
        let report = match report.filter(|r| r.as_object().is_some_and(|o| !o.is_empty())) {
            Some(r) => r,
            None => doctor(home)?,
        };
        let body = host_status(home, &report, scheduler, last_pickup, triage_failed_since)?;
        api(
            home,
            "PUT",
            &format!("/api/hosts/{device}/status"),
            Some(dumps(&body, DEFAULT).as_bytes()),
            &[200],
            &[("Content-Type", "application/json")],
        )?;
        if save {
            write_file(
                &home.join("host-status.json"),
                &(dumps(&body, INDENT2) + "\n"),
            )?;
        }
        Ok(true)
    };
    sent().unwrap_or(false)
}

/// (returncode, output) of a scheduler command; 127 when it is missing or times out.
fn setup_run(argv: &[&str], stdin: Option<&str>) -> (i32, String) {
    #[cfg(test)]
    if let Some(f) = fake!(setup_run) {
        return f(argv, stdin);
    }
    let timeout = Duration::from_secs(30);
    let done = match stdin {
        Some(input) => run_input(argv, input, timeout),
        None => capture(argv, None, &[], Some(timeout)),
    };
    match done {
        Ok(o) if o.code == 0 => (0, o.stdout),
        Ok(o) => (o.code, o.stdout + &o.stderr),
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            let shown: Vec<String> = argv.iter().map(|a| repr(a)).collect();
            (
                127,
                format!(
                    "Command '[{}]' timed out after 30 seconds",
                    shown.join(", ")
                ),
            )
        }
        Err(e) => (127, os_error(&e, Some(Path::new(argv[0])))),
    }
}

/// (value, None) from r, or (None, the text it failed with).
fn setup_try<T>(r: Res<T>) -> (Option<T>, Option<String>) {
    match r {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(message(&e))),
    }
}

/// Folders the timer PATH needs for a resolved launcher: its program's, and for env the command env runs.
fn launcher_folders(template: &[String]) -> Vec<String> {
    let mut folders = vec![dirname(&template[0])];
    if basename(&template[0]) == "env" {
        let command = template[1..]
            .iter()
            .find(|a| !a.starts_with('-') && ENV_ASSIGN.match_start(a).is_none());
        if let Some(found) = command.and_then(|c| which(c)) {
            folders.push(dirname(&found.to_string_lossy()));
        }
    }
    folders
}

fn timer_path(scheduler: &str) -> PathBuf {
    let home = std::env::home_dir().unwrap_or_default();
    if scheduler == "launchd" {
        return home.join("Library/LaunchAgents/factory.pickup.plist");
    }
    home.join(".config").join("factory-pickup.cmd")
}

/// What the pickup timer runs: this binary, with the PATH its launchers need.
struct TimerValues {
    argv: Vec<String>,
    path: String,
    log: String,
    exe: String,
}

fn timer_values(home: &Path) -> TimerValues {
    let exe = exe();
    let mut folders = Vec::new();
    for route in setup_try(read_routes(home)).0.unwrap_or_default() {
        if let Some(template) = setup_try(route_launcher(home, &route)).0 {
            folders.extend(launcher_folders(&template));
        }
    }
    folders.push(dirname(&exe));
    if let Some(gh) = which("gh") {
        folders.push(dirname(&gh.to_string_lossy()));
    }
    if !WINDOWS {
        folders.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(String::from));
    }
    let mut unique: Vec<String> = Vec::new();
    for f in folders {
        if !unique.contains(&f) {
            unique.push(f);
        }
    }
    TimerValues {
        argv: vec![
            exe.clone(),
            "--home".into(),
            home.to_string_lossy().into_owned(),
            "pickup".into(),
        ],
        path: unique.join(if WINDOWS { ";" } else { ":" }),
        log: home
            .join("logs")
            .join("pickup.log")
            .to_string_lossy()
            .into_owned(),
        exe,
    }
}

fn timer_text(scheduler: &str, values: &TimerValues) -> String {
    let (argv, path, log) = (&values.argv, &values.path, &values.log);
    if scheduler == "cron" {
        return format!(
            "*/5 * * * * PATH={} {} >> {} 2>&1",
            shlex_quote(path),
            shlex_join(argv),
            shlex_quote(log)
        );
    }
    if scheduler == "launchd" {
        return plist_dumps(&json!({
            "Label": "factory.pickup", "ProgramArguments": argv, "EnvironmentVariables": {"PATH": path},
            "StartInterval": 300, "StandardOutPath": log, "StandardErrorPath": log,
        }));
    }
    format!(
        "@echo off\r\nset \"PATH={path};%PATH%\"\r\n{} >> \"{log}\" 2>&1\r\n",
        list2cmdline(argv)
    )
}

/// (installed, detail) for this host's pickup timer.
fn timer_installed(scheduler: &str) -> (bool, String) {
    if scheduler == "launchd" {
        let plist = timer_path(scheduler);
        if !plist.exists() {
            return (false, "no launchd job factory.pickup".into());
        }
        let target = format!("gui/{}/factory.pickup", getuid());
        if setup_run(&["launchctl", "print", &target], None).0 != 0 {
            return (
                false,
                format!("{} exists but is not loaded", plist.display()),
            );
        }
        return (true, "launchd job factory.pickup is loaded".into());
    }
    if scheduler == "cron" {
        let (code, out) = setup_run(&["crontab", "-l"], None);
        if code != 0
            || !splitlines(&out, false)
                .iter()
                .any(|l| CRON_PICKUP.search(l).is_some())
        {
            return (false, "no factory pickup line in the crontab".into());
        }
        if setup_run(&["pgrep", "-x", "cron"], None).0 != 0
            && setup_run(&["pgrep", "-x", "crond"], None).0 != 0
        {
            return (false, "the crontab line is set but cron is not running; on WSL run: sudo service cron start".into());
        }
        return (true, "crontab runs pickup every 5 minutes".into());
    }
    if setup_run(&["schtasks", "/Query", "/TN", "factory-pickup"], None).0 != 0 {
        return (false, "no scheduled task factory-pickup".into());
    }
    (true, "scheduled task factory-pickup exists".into())
}

/// The command a crontab line runs: fields after the schedule, minus VAR= prefixes and redirects.
fn cron_argv(line: &str) -> Option<Vec<String>> {
    let mut words: Vec<String> = shlex_split(line)?.into_iter().skip(5).collect();
    while words
        .first()
        .is_some_and(|w| ENV_ASSIGN.match_start(w).is_some())
    {
        words.remove(0);
    }
    let end = words
        .iter()
        .position(|w| w.starts_with('>') || w.starts_with("2>"))
        .unwrap_or(words.len());
    words.truncate(end);
    Some(words)
}

/// None when the installed timer runs what --install would write, else what differs.
fn timer_drift(scheduler: &str, values: &TimerValues) -> Option<String> {
    let want = &values.argv;
    let mut have_path = None;
    let have: Vec<String>;
    if scheduler == "launchd" {
        let plist = timer_path(scheduler);
        let data = fs::read(&plist)
            .ok()
            .and_then(|b| plist_loads(&b))
            .filter(Value::is_object);
        let Some(data) = data else {
            return Some(format!("could not read {}", plist.display()));
        };
        let args = data.get("ProgramArguments").and_then(Value::as_array);
        let Some(args) =
            args.and_then(|a| a.iter().map(|v| v.as_str().map(String::from)).collect())
        else {
            return Some(format!(
                "could not read ProgramArguments in {}",
                plist.display()
            ));
        };
        have = args;
        have_path = data
            .get("EnvironmentVariables")
            .and_then(|e| e.get("PATH"))
            .map(|p| p.as_str().map_or_else(|| p.to_string(), String::from));
    } else if scheduler == "cron" {
        let out = setup_run(&["crontab", "-l"], None).1;
        let lines: Vec<&str> = splitlines(&out, false)
            .into_iter()
            .filter(|l| CRON_PICKUP.search(l).is_some())
            .collect();
        let argvs: Vec<Option<Vec<String>>> = lines.iter().map(|l| cron_argv(l)).collect();
        if argvs.iter().any(|a| a.as_ref() == Some(want)) {
            return None;
        }
        match argvs.first().cloned().flatten().filter(|a| !a.is_empty()) {
            Some(a) => have = a,
            None => {
                let line = format!(
                    "could not read the crontab line {}",
                    lines.first().unwrap_or(&"")
                );
                return Some(line.trim_end().to_string());
            }
        }
    } else {
        let cmd = timer_path(scheduler);
        if fs::read(&cmd).ok().as_deref() == Some(timer_text(scheduler, values).as_bytes()) {
            return None;
        }
        return Some(format!(
            "{} differs from what --install would write",
            cmd.display()
        ));
    }
    if &have == want {
        if scheduler == "launchd" && have_path.as_deref() != Some(values.path.as_str()) {
            let shown = have_path.filter(|p| !p.is_empty());
            return Some(format!(
                "the timer PATH is {}, but --install would write {}",
                shown.as_deref().unwrap_or("unset"),
                values.path
            ));
        }
        return None;
    }
    Some(format!(
        "the timer runs {}, but --install would write {}",
        shlex_join(&have),
        shlex_join(want)
    ))
}

/// Write and load the 5 minute pickup timer; one line saying what changed.
fn install_timer(home: &Path, scheduler: &str) -> Res<String> {
    mkdirs(&home.join("logs"))?;
    let text = timer_text(scheduler, &timer_values(home));
    let (code, out, done);
    if scheduler == "launchd" {
        let (plist, uid) = (timer_path(scheduler), getuid());
        let target = format!("gui/{uid}/factory.pickup");
        if fs::read(&plist).ok().as_deref() == Some(text.as_bytes())
            && setup_run(&["launchctl", "print", &target], None).0 == 0
        {
            return Ok(format!("timer already installed: {}", plist.display()));
        }
        mkdirs(plist.parent().unwrap())?;
        write_file(&plist, &text)?;
        setup_run(&["launchctl", "bootout", &target], None);
        let shown = plist.to_string_lossy();
        (code, out) = setup_run(
            &["launchctl", "bootstrap", &format!("gui/{uid}"), &shown],
            None,
        );
        done = format!("installed launchd timer: {}", plist.display());
    } else if scheduler == "cron" {
        let (read, listed) = setup_run(&["crontab", "-l"], None);
        if read != 0 && !(read == 1 && listed.contains("no crontab for")) {
            return Err(fail(
                format!(
                    "could not read the crontab, so it was left as is: {}",
                    strip(&listed)
                ),
                1,
            ));
        }
        let lines = if read == 0 {
            splitlines(&listed, false)
        } else {
            Vec::new()
        };
        let mut wanted: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| CRON_PICKUP.search(l).is_none())
            .collect();
        wanted.push(&text);
        if lines == wanted {
            return Ok("timer already installed: crontab".into());
        }
        (code, out) = setup_run(&["crontab", "-"], Some(&(wanted.join("\n") + "\n")));
        done = "installed cron timer: crontab".to_string();
    } else {
        let cmd = timer_path(scheduler);
        if fs::read(&cmd).ok().as_deref() != Some(text.as_bytes()) {
            mkdirs(cmd.parent().unwrap())?;
            write_file(&cmd, &text)?;
        }
        let shown = cmd.to_string_lossy();
        let create = [
            "schtasks",
            "/Create",
            "/TN",
            "factory-pickup",
            "/SC",
            "MINUTE",
            "/MO",
            "5",
            "/TR",
            &shown,
            "/F",
        ];
        (code, out) = setup_run(&create, None);
        done = format!("installed schtasks timer: {}", cmd.display());
    }
    if code != 0 {
        return Err(fail(
            format!("could not install the {scheduler} timer: {}", strip(&out)),
            1,
        ));
    }
    Ok(done)
}

/// The lines that set up the pickup timer by hand.
fn timer_fix(scheduler: &str, home: &Path) -> Vec<String> {
    let text = timer_text(scheduler, &timer_values(home));
    let path = timer_path(scheduler);
    if scheduler == "cron" {
        return vec!["Add this line with crontab -e:".into(), text];
    }
    let mut lines = vec![format!("Save as {}:", path.display())];
    lines.extend(splitlines(&text, false).into_iter().map(String::from));
    lines.push(if scheduler == "launchd" {
        format!(
            "Load it: launchctl bootstrap gui/{} {}",
            getuid(),
            path.display()
        )
    } else {
        format!(
            "Then run: schtasks /Create /TN factory-pickup /SC MINUTE /MO 5 /TR \"{}\" /F",
            path.display()
        )
    });
    lines
}

/// One of the 7 setup checks. ok is None when an earlier check it depends on failed.
struct Check {
    name: &'static str,
    ok: Option<bool>,
    detail: String,
    fix: Vec<String>,
    note: Option<String>,
}

fn setup_checks(home: &Path, device: &str, overridden: bool) -> Res<Vec<Check>> {
    let mut argv = vec![exe()];
    if overridden {
        argv.extend(["--home".to_string(), home.to_string_lossy().into_owned()]);
    }
    let run = shlex_join(&argv);
    let skill = skill_root()?;
    let check = |name, ok, detail, fix| Check {
        name,
        ok,
        detail,
        fix,
        note: None,
    };
    let mut checks = Vec::new();

    let failing: Vec<String> = doctor(home)?["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["status"] != "pass")
        .map(|c| c["name"].as_str().unwrap_or_default().to_string())
        .collect();
    checks.push(check(
        "home",
        Some(failing.is_empty()),
        if failing.is_empty() {
            "config, credentials, projects and board pass".into()
        } else {
            format!("failing: {}", failing.join(", "))
        },
        vec![
            format!("Run: {run} doctor"),
            "Then fix each failing check; see docs/troubleshooting.md in the factory repository."
                .into(),
        ],
    ));

    let config = home.join("config.md");
    let named = lookup(&meta(&config)?, "device").unwrap_or("").to_string();
    let full = hostname();
    let short = full.split('.').next().unwrap_or("");
    let valid = !named.is_empty() && HOST_NAME.is_full(&named);
    checks.push(check(
        "device",
        Some(valid),
        if valid {
            format!("device: {device}")
        } else if !named.is_empty() {
            format!(
                "config.md device: {} is not a valid host name",
                repr(&named)
            )
        } else {
            format!("config.md sets no device:, so this host is {device}")
        },
        vec![
            if named.is_empty() {
                format!(
                    "Add this line inside the frontmatter of {}:",
                    config.display()
                )
            } else {
                format!(
                    "Replace the device: line in the frontmatter of {} with:",
                    config.display()
                )
            },
            format!(
                "device: {}",
                if HOST_NAME.is_full(short) {
                    short
                } else {
                    "<host-name>"
                }
            ),
        ],
    ));

    let paths = paths_of(home);
    let slugs: Vec<&(String, String)> = paths.iter().filter(|(_, p)| is_dir(p)).collect();
    let names: Vec<&str> = slugs.iter().map(|(s, _)| s.as_str()).collect();
    checks.push(check(
        "checkout",
        Some(!slugs.is_empty()),
        if slugs.is_empty() {
            "paths.md lists no checkout that exists on this host".into()
        } else {
            names.join(", ")
        },
        vec![format!("Run: {run} init <repo-path>")],
    ));

    let (routes, why) = setup_try(read_routes(home));
    checks.push(check(
        "routes",
        Some(routes.is_some()),
        match &routes {
            Some(r) if !r.is_empty() => r
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            _ => why.unwrap_or_else(|| "None".into()),
        },
        vec![
            format!("Write {}:", home.join("routes.md").display()),
            "---".into(),
            "primary: <provider> <model> <effort>".into(),
            "---".into(),
        ],
    ));

    let (launchers, why) = match &routes {
        None => (None, Some("not checked: fix check 4 first".to_string())),
        Some(routes) => {
            let why = routes
                .iter()
                .find_map(|r| setup_try(route_launcher(home, r)).1);
            (Some(why.is_none()), why)
        }
    };
    let mut example = Map::new();
    for r in routes.iter().flatten() {
        example.insert(r.name.clone(), json!(["<program>", "<arg>", "..."]));
    }
    checks.push(check(
        "launchers",
        launchers,
        why.unwrap_or_else(|| "every route has a launcher".into()),
        vec![
            format!("Write {}:", home.join("launchers.json").display()),
            dumps(&json!({"version": 1, "launchers": example}), DEFAULT),
            format!(
                "Launcher examples: {}",
                skill.join("references").join("pickup.md").display()
            ),
        ],
    ));

    let (listed, detail) = if !failing.is_empty() || slugs.is_empty() {
        let first: Vec<&str> = [("1", !failing.is_empty()), ("3", slugs.is_empty())]
            .iter()
            .filter(|(_, bad)| *bad)
            .map(|(n, _)| *n)
            .collect();
        let plural = if first.len() == 2 { "s" } else { "" };
        (
            None,
            format!(
                "not checked: fix check{plural} {} first",
                first.join(" and ")
            ),
        )
    } else {
        let (mut on, mut parts) = (Vec::new(), Vec::new());
        for (slug, path) in &slugs {
            let (settings, why) = setup_try(pickup_settings(home, slug, path, device));
            let settings = settings.unwrap_or(Settings {
                enabled: None,
                hosts: Vec::new(),
                reason: why,
            });
            if let Some(why) = &settings.reason {
                parts.push(format!("{slug}: {why}"));
            } else if settings.enabled != Some(true) {
                parts.push(format!("{slug}: pickup off"));
            } else if !settings.lists(device) {
                parts.push(format!(
                    "{slug}: pickup lists {}, not {device}",
                    settings.hosts.join(", ")
                ));
            } else {
                on.push(slug.as_str());
            }
        }
        let detail = if on.is_empty() {
            parts.join("; ")
        } else {
            format!("pickup lists {device} for {}", on.join(", "))
        };
        (Some(!on.is_empty()), detail)
    };
    checks.push(check(
        "pipeline",
        listed,
        detail,
        if listed.is_none() {
            Vec::new()
        } else {
            vec![
                "Fix check 2 first if this host name is wrong.".into(),
                format!("Otherwise, in the dashboard, Settings, Pipeline (or Settings, Projects, <project>), turn pickup on and add host {device} exactly as written."),
            ]
        },
    ));

    let scheduler = pickup_scheduler();
    let (installed, detail) = timer_installed(scheduler);
    let note = if installed {
        timer_drift(scheduler, &timer_values(home))
    } else {
        None
    };
    let mut fix = timer_fix(scheduler, home);
    fix.push(format!("Or run: {run} pickup setup --install"));
    let mut timer = check("timer", Some(installed), detail, fix);
    timer.note = note.map(|n| format!("{n}. To update it, run: {run} pickup setup --install"));
    checks.push(timer);
    Ok(checks)
}

/// Check every piece pickup needs on this host and print the fix for each missing one; with
/// install, write and load the timer first. Exits 3 until all 7 pass.
fn pickup_setup(io: &mut Io, home: &Path, install: bool, overridden: bool) -> Res<()> {
    let device = holder(home)?.0;
    let scheduler = pickup_scheduler();
    if install {
        outln!(io, "{}", install_timer(home, scheduler)?);
    }
    outln!(
        io,
        "pickup setup on host {device} ({}, {scheduler})",
        host_os()
    );
    let values = timer_values(home);
    outln!(
        io,
        "skill {}  timer runs {}  home {}",
        skill_root()?.display(),
        values.exe,
        home.display()
    );
    let checks = setup_checks(home, &device, overridden)?;
    for (n, c) in checks.iter().enumerate() {
        let label = if c.ok == Some(true) { "ok" } else { "missing" };
        outln!(io, "{}. {label:<8} {}: {}", n + 1, c.name, c.detail);
        if let Some(note) = &c.note {
            outln!(io, "    note: {note}");
        }
        if c.ok != Some(true) {
            for line in &c.fix {
                outln!(io, "    {line}");
            }
        }
    }
    let passed = checks.iter().filter(|c| c.ok == Some(true)).count();
    let rest = if passed == 7 {
        ""
    } else {
        " Pickup does nothing on this host until every check is ok."
    };
    outln!(io, "{passed} of 7 ok.{rest}");
    outln!(io, "A stored Paseo or Claude schedule prompt does not pick up ideas. Only the host timer runs pickup.");
    Err(exit(if passed == 7 { 0 } else { 3 }))
}

/// (returncode, or None on a timeout, and stdout) of gh, or None when gh is not on PATH.
fn gh_cli(args: &[&str]) -> Option<(Option<i32>, String)> {
    #[cfg(test)]
    if let Some(f) = fake!(gh_cli) {
        return f(args);
    }
    let gh = which("gh")?.to_string_lossy().into_owned();
    let argv: Vec<&str> = std::iter::once(gh.as_str())
        .chain(args.iter().copied())
        .collect();
    Some(
        match capture(&argv, None, &[], Some(Duration::from_secs(60))) {
            Ok(o) => (Some(o.code), o.stdout),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => (None, String::new()),
            Err(_) => (Some(1), String::new()),
        },
    )
}

/// The doctor report: local configuration, credentials, and whether projects and board answer.
fn doctor(home: &Path) -> Res<Value> {
    #[cfg(test)]
    if let Some(f) = fake!(doctor) {
        return f(home);
    }
    let home = resolve(home).map_err(|e| fail(os_error(&e, Some(home)), 1))?;
    let mut api_url = String::new();
    let mut checks = Vec::new();
    let mut check = |name: &str, valid: bool, message: String| {
        let status = if valid { "pass" } else { "fail" };
        checks.push(json!({"name": name, "status": status, "message": message}));
    };
    check("location", true, "Location resolved.".into());
    let mut config_error = match validate_home(&home) {
        Ok(()) => String::new(),
        Err(e) => strip(e.stderr.get("factory.py: ".len()..).unwrap_or("")).to_string(),
    };
    if config_error.is_empty() {
        let config = home.join("config.md");
        let meta = frontmatter(&config)
            .map_err(|e| fail(os_error(&e, Some(&config)), 1))?
            .0;
        let endpoint = fm_get(&meta, "api_url").unwrap_or("");
        if endpoint.is_empty() {
            config_error = format!(
                "Set api_url in {}, then run /factory init again.",
                config.display()
            );
        } else {
            match normalized_origin(endpoint, false) {
                Ok(origin) => api_url = origin,
                Err(()) => config_error = endpoint_message(&config),
            }
        }
    }
    let config_ok = config_error.is_empty();
    check(
        "config",
        config_ok,
        if config_ok {
            "Local configuration is valid.".into()
        } else {
            config_error
        },
    );
    let env_file = home.join(".env");
    let configured = regular_metadata(&env_file).is_ok()
        && read_conf(&env_file).is_ok_and(|env| {
            ["FACTORY_ACCESS_CLIENT_ID", "FACTORY_ACCESS_CLIENT_SECRET"]
                .iter()
                .all(|k| env.get(*k).is_some_and(|v| !v.is_empty()))
        });
    check(
        "credentials",
        configured,
        if configured {
            "Service token is configured.".into()
        } else {
            token_message(&env_file)
        },
    );
    for (name, route) in [("projects", "/projects.json"), ("board", "/api/board")] {
        if !config_ok || !configured {
            check(
                name,
                false,
                "Not attempted because local configuration is invalid.".into(),
            );
            continue;
        }
        let ok: &[u16] = if name == "board" { &[200, 404] } else { &[200] };
        let read = api(&home, "GET", route, None, ok, &[]).and_then(|r| {
            if name == "board" && r.status == 404 {
                return Ok(Some("Board is absent; no board was created."));
            }
            let value = loads(&r.text()?).ok();
            let good = match name {
                "projects" => value.as_ref().is_some_and(Value::is_array),
                _ => value
                    .as_ref()
                    .is_some_and(|v| v.get("jobs").is_some_and(Value::is_array)),
            };
            Ok(good.then_some(if name == "projects" {
                "Projects are reachable."
            } else {
                "Board is reachable."
            }))
        });
        match read {
            Ok(Some(m)) => check(name, true, m.into()),
            _ => check(name, false, format!("Cannot read {route}; check connectivity, Access policy and service-token configuration.")),
        }
    }
    Ok(json!({"version": 1, "home": home.to_string_lossy(), "api_url": api_url, "checks": checks}))
}

/// Why repo's local base branch holds history origin's base does not descend from, as an
/// "unsafe checkout:" line, or None. It fetches origin's base first.
fn checkout_problem(home: &Path, slug: &str, repo: &str) -> Res<Option<String>> {
    #[cfg(test)]
    if let Some(f) = fake!(checkout_problem) {
        return f(home, slug, repo);
    }
    let project = project_files(home, slug)?.0;
    let meta = meta(&project)?;
    let base = lookup(&meta, "base").unwrap_or("main");
    if check_path(base).is_some() || base.starts_with('-') {
        return Ok(Some(format!(
            "unsafe checkout: {slug} base {base} is not a safe branch name"
        )));
    }
    // git's stderr can carry origin's URL, credentials included, so neither fetch line includes it.
    let refspec = format!("+refs/heads/{base}:refs/remotes/origin/{base}");
    let fetched = capture(
        &["git", "-C", repo, "fetch", "-q", "origin", &refspec],
        None,
        &[("GIT_TERMINAL_PROMPT", "0")],
        Some(Duration::from_secs(FETCH_TIMEOUT_SECONDS)),
    );
    match fetched {
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            return Ok(Some(format!("unsafe checkout: git fetch origin {base} timed out after {FETCH_TIMEOUT_SECONDS} s in {repo}")))
        }
        Err(e) => return Err(fail(os_error(&e, Some(Path::new("git"))), 1)),
        Ok(o) if o.code != 0 => {
            return Ok(Some(format!(
                "unsafe checkout: git fetch origin {base} failed in {repo} (exit {})",
                o.code
            )))
        }
        Ok(_) => {}
    }
    let at = Path::new(repo);
    let origin = format!("refs/remotes/origin/{base}");
    let roots_text = git(at, &["rev-list", "--max-parents=0", &origin]);
    let roots = words(&roots_text);
    if roots.is_empty() {
        return Ok(Some(format!(
            "unsafe checkout: origin/{base} has no root commit in {repo}"
        )));
    }
    let local = format!("refs/heads/{base}");
    if git(at, &["rev-parse", "--verify", "-q", &local]).is_empty() {
        return Ok(Some(format!("unsafe checkout: {repo} has no local {base} branch; run git -C {repo} branch {base} origin/{base}")));
    }
    let local_roots = git(at, &["rev-list", "--max-parents=0", &local]);
    let mut foreign: Vec<&str> = words(&local_roots)
        .into_iter()
        .filter(|r| !roots.contains(r))
        .collect();
    foreign.sort();
    foreign.dedup();
    Ok(foreign.first().map(|first| {
        let short: String = first.chars().take(12).collect();
        format!("unsafe checkout: {repo} {base} has root {short}, which origin/{base} lacks, so it holds archive history; repoint it as docs/public-cutover.md \"Other hosts\" says")
    }))
}

/// Why no open board job owns job_id's leftovers, or None when one does.
fn orphan_reason(jobs: &[Value], job_id: &str) -> Option<&'static str> {
    let Some(job) = jobs.iter().find(|j| get_str(j, "id") == Some(job_id)) else {
        return Some("no job on the board");
    };
    match get_str(job, "stage") {
        Some("merged") => Some("job merged"),
        Some("closed") => Some("job closed"),
        _ => None,
    }
}

/// The paths.md slug whose checkout shares cwd's git common dir, first by slug, else None.
fn workspace_project(cwd: &str, paths: &[(String, String)]) -> Option<String> {
    let common = |path: &str| {
        let out = git(Path::new(path), &["rev-parse", "--git-common-dir"]);
        if out.is_empty() {
            return None;
        }
        let text = resolve(&Path::new(path).join(out))
            .ok()?
            .to_string_lossy()
            .into_owned();
        Some(if WINDOWS {
            text.to_lowercase().replace('/', "\\")
        } else {
            text
        })
    };
    let target = common(cwd)?;
    let mut sorted = paths.to_vec();
    sorted.sort();
    sorted
        .into_iter()
        .find(|(_, p)| common(p).as_ref() == Some(&target))
        .map(|(s, _)| s)
}

/// One orphan the scan found; the summary needs only its kind, and sorts by its name.
struct Orphan {
    kind: &'static str,
    name: Option<String>,
}

/// (orphans, skipped): factory branches on origin, open factory PRs, job worktrees and
/// factory-<id> Paseo workspaces that no open board job owns. Only reads.
fn orphan_scan(home: &Path) -> Res<(Vec<Orphan>, Vec<String>)> {
    let board = read_board(home)?;
    let jobs: &[Value] = board["jobs"].as_array().map_or(&[], Vec::as_slice);
    let paths = paths_of(home);
    let mut orphans = Vec::new();
    let mut skipped = Vec::new();
    let mut add = |kind: &'static str, job_id: &str, name: Option<String>| {
        if orphan_reason(jobs, job_id).is_some() {
            orphans.push(Orphan { kind, name });
        }
    };
    let mut gh_missing = false;
    for (slug, path) in &paths {
        if !is_dir(path) {
            continue;
        }
        let argv = [
            "git",
            "-C",
            path,
            "ls-remote",
            "--heads",
            "origin",
            "refs/heads/factory/*",
        ];
        let listed = capture(
            &argv,
            None,
            &[("GIT_TERMINAL_PROMPT", "0")],
            Some(Duration::from_secs(20)),
        );
        match listed {
            Err(e) if e.kind() == io::ErrorKind::TimedOut => skipped.push(format!(
                "branches {slug}: git ls-remote timed out after 20 s"
            )),
            Err(_) => skipped.push(format!("branches {slug}: could not run git")),
            Ok(o) if o.code != 0 => skipped.push(format!(
                "branches {slug}: git ls-remote failed (exit {})",
                o.code
            )),
            Ok(o) => {
                for line in splitlines(&o.stdout, false) {
                    let reference = line.split_once('\t').map_or(line, |(_, r)| r);
                    if let Some(name) = reference.strip_prefix("refs/heads/") {
                        if let Some(id) = name.strip_prefix("factory/") {
                            add("branch", id, Some(name.to_string()));
                        }
                    }
                }
            }
        }
        if gh_missing {
            continue;
        }
        let md = project_files(home, slug)?.0;
        let repo = lookup(&meta(&md)?, "repo").unwrap_or("").to_string();
        if repo.is_empty() {
            skipped.push(format!("prs {slug}: the project file names no repo"));
            continue;
        }
        let args = [
            "pr",
            "list",
            "-R",
            &repo,
            "--state",
            "open",
            "--json",
            "number,headRefName,url",
            "--limit",
            "500",
        ];
        let Some((code, out)) = gh_cli(&args) else {
            gh_missing = true;
            skipped.push("prs: gh is not on PATH".into());
            continue;
        };
        let prs = loads(&out).ok().filter(|p| code == Some(0) && p.is_array());
        let Some(prs) = prs else {
            let code = code.map_or("timeout".to_string(), |c| c.to_string());
            skipped.push(format!("prs {slug}: gh pr list failed (exit {code})"));
            continue;
        };
        for pr in prs.as_array().into_iter().flatten() {
            if let Some(id) = get_str(pr, "headRefName").and_then(|h| h.strip_prefix("factory/")) {
                add("pr", id, get_str(pr, "url").map(String::from));
            }
        }
    }

    let root = home.join("worktrees");
    let listing = (|| -> Result<(), PathBuf> {
        let list = |dir: &Path| -> Result<Vec<PathBuf>, PathBuf> {
            let mut items = fs::read_dir(dir)
                .and_then(|entries| {
                    entries
                        .map(|e| e.map(|e| e.path()))
                        .collect::<io::Result<Vec<_>>>()
                })
                .map_err(|_| dir.to_path_buf())?;
            items.sort();
            Ok(items)
        };
        let projects = if is_link(&root).map_err(|_| root.clone())? {
            vec![root.clone()]
        } else if root.is_dir() {
            list(&root)?
        } else {
            Vec::new()
        };
        for project in projects {
            if is_link(&project).map_err(|_| project.clone())? {
                skipped.push(format!(
                    "worktrees: {} is a symlink or junction",
                    project.display()
                ));
                continue;
            }
            let folders = if project.is_dir() {
                list(&project)?
            } else {
                Vec::new()
            };
            for folder in folders {
                if is_link(&folder).map_err(|_| folder.clone())? {
                    skipped.push(format!(
                        "worktrees: {} is a symlink or junction",
                        folder.display()
                    ));
                } else if folder.is_dir() {
                    let id = folder.file_name().unwrap_or_default().to_string_lossy();
                    add("worktree", &id, Some(folder.to_string_lossy().into_owned()));
                }
            }
        }
        Ok(())
    })();
    if let Err(at) = listing {
        skipped.push(format!("worktrees: could not read {}", at.display()));
    }

    if which("paseo").is_none() {
        skipped.push("workspaces: paseo is not on PATH".into());
    } else {
        let (code, out) = paseo_run(&["workspace", "ls", "--json"]);
        let spaces = loads(&out).ok().filter(|s| code == 0 && s.is_array());
        if spaces.is_none() {
            skipped.push(format!(
                "workspaces: paseo workspace ls failed (exit {code})"
            ));
        }
        let ids: Vec<&str> = jobs.iter().filter_map(|j| get_str(j, "id")).collect();
        for space in spaces
            .as_ref()
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(cwd) = get_str(space, "cwd") else {
                continue;
            };
            let trimmed = cwd.trim_end_matches('/');
            let base = basename(if trimmed.is_empty() { cwd } else { trimmed });
            let Some(rest) = base.strip_prefix("factory-") else {
                continue;
            };
            let job_id = if ids.contains(&rest) {
                rest.to_string()
            } else {
                TRAILING_NUMBER.replace_all(rest, |_| String::new())
            };
            let owned = jobs
                .iter()
                .find(|j| get_str(j, "id") == Some(job_id.as_str()))
                .and_then(|j| get_str(j, "project"))
                .is_some_and(|p| !p.is_empty());
            // Python names the workspace's project here; the summary never reads it, but its git
            // calls stay in the same order.
            if !owned {
                workspace_project(cwd, &paths);
            }
            add("workspace", &job_id, Some(cwd.to_string()));
        }
    }
    orphans.sort_by(|a, b| {
        let rank = |o: &Orphan| ORPHAN_KINDS.iter().position(|k| *k == o.kind);
        (rank(a), &a.name).cmp(&(rank(b), &b.name))
    });
    Ok((orphans, skipped))
}

/// The one pickup line for a scan's result.
fn orphan_summary(orphans: &[Orphan], skipped: &[String]) -> String {
    if orphans.is_empty() && skipped.is_empty() {
        return "orphan scan: nothing orphaned".into();
    }
    let kinds: Vec<String> = ORPHAN_KINDS
        .iter()
        .zip(ORPHAN_NOUNS)
        .filter_map(|(kind, (one, many))| {
            let n = orphans.iter().filter(|o| o.kind == *kind).count();
            (n > 0).then(|| format!("{n} {}", if n == 1 { one } else { many }))
        })
        .collect();
    let mut line = if orphans.is_empty() {
        "orphan scan: nothing orphaned".to_string()
    } else {
        format!(
            "orphan scan: {} orphaned ({})",
            orphans.len(),
            kinds.join(", ")
        )
    };
    if skipped.len() == 1 {
        line += ", 1 source skipped";
    } else if !skipped.is_empty() {
        line += &format!(", {} sources skipped", skipped.len());
    }
    line + "; run factory.py orphans to list them"
}

/// True when a once-per-local-date hook's record already names today.
fn ran_today(path: &Path, today: &str) -> bool {
    read_json(path).is_some_and(|r| {
        r.is_object() && eq_int(r.get("version"), 1) && get_str(&r, "date") == Some(today)
    })
}

/// Record today, then mark it done; the record is written first, so a stopped run waits a day.
fn claim_today(path: &Path) -> Res<bool> {
    let (utc, offset) = local_now();
    let today = local_date(utc, offset);
    if ran_today(path, &today) {
        return Ok(false);
    }
    write_file(
        path,
        &(dumps(&json!({"version": 1, "date": today}), DEFAULT) + "\n"),
    )?;
    Ok(true)
}

/// Run the orphan scan once per local date and print its summary line.
fn orphan_pickup_scan(io: &mut Io, home: &Path) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(orphan_pickup_scan) {
        f(home);
        return Ok(());
    }
    if !claim_today(&home.join(ORPHAN_SCAN_FILE))? {
        return Ok(());
    }
    match orphan_scan(home) {
        Ok((orphans, skipped)) => outln!(io, "{}", orphan_summary(&orphans, &skipped)),
        Err(e) => {
            let _ = io.err.write_all(e.stderr.as_bytes());
            errln!(
                io,
                "factory.py: orphan scan stopped (exit {}); it runs again tomorrow",
                e.code
            );
        }
    }
    Ok(())
}

/// Run outcomes record once per local date for each paths.md checkout folder that exists.
fn outcomes_pickup_record(home: &Path) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(outcomes_pickup_record) {
        f(home);
        return Ok(());
    }
    if !claim_today(&home.join(OUTCOMES_RECORD_FILE))? {
        return Ok(());
    }
    let mut paths = paths_of(home);
    paths.sort();
    if paths
        .iter()
        .any(|(slug, checkout)| SLUG.is_full(slug) && is_dir(checkout))
    {
        return Err(deferred("outcomes record"));
    }
    Ok(())
}

fn retro_pushed_today(home: &Path) -> bool {
    let (utc, offset) = local_now();
    ran_today(&home.join(RETRO_PUSH_FILE), &local_date(utc, offset))
}

fn retro_pickup_push(home: &Path) -> Res<()> {
    if retro_pushed_today(home) || !retro_transcripts().is_dir() {
        return Ok(());
    }
    Err(deferred("retro push"))
}

fn audit_pickup_push(home: &Path) -> Res<()> {
    let _ = home;
    if !retro_transcripts().is_dir() {
        return Ok(());
    }
    Err(deferred("audit push"))
}

/// True only when the all-projects pipeline document sets tracer.enabled to true.
fn tracer_enabled(home: &Path) -> Res<bool> {
    let r = api(home, "GET", "/api/pipeline/global", None, &[200, 404], &[])?;
    if r.status == 404 {
        return Ok(false);
    }
    let malformed = |code| fail("malformed all-projects pipeline document", code);
    let doc = loads(&r.text()?).map_err(|_| malformed(5))?;
    let doc = doc.as_object().ok_or_else(|| malformed(1))?;
    Ok(doc.get("tracer").and_then(|t| t.get("enabled")) == Some(&Value::Bool(true)))
}

fn tracer_follow_pass(home: &Path) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(tracer_follow_pass) {
        return f(home);
    }
    if !lexists(&home.join("tracer.json")) || !tracer_enabled(home)? {
        return Ok(());
    }
    Err(deferred("tracer follow pass"))
}

/// True when the pass wrote the board. A host without tracer.json and with no Building in
/// Tracer mark has nothing to do.
fn tracer_build_pass(io: &mut Io, home: &Path, jobs: Option<&Value>) -> Res<bool> {
    #[cfg(test)]
    if let Some(f) = fake!(tracer_build_pass) {
        return f(home, jobs);
    }
    if lexists(&home.join("tracer.json")) {
        return Err(deferred("tracer build pass"));
    }
    let read;
    let jobs = match jobs {
        Some(jobs) => jobs,
        None => match read_board(home) {
            Ok(board) => {
                read = board["jobs"].clone();
                &read
            }
            Err(e) => {
                let reason = words(&e.stderr).join(" ").replacen("factory.py: ", "", 1);
                let reason = if reason.is_empty() {
                    "unknown error"
                } else {
                    &reason
                };
                errln!(io, "factory.py: warning: Tracer build check failed: {}. Unchecked jobs keep their Building in Tracer mark.", reason.trim_end_matches('.'));
                return Ok(false);
            }
        },
    };
    if jobs.as_array().into_iter().flatten().any(tracer_building) {
        return Err(deferred("tracer build pass"));
    }
    Ok(false)
}

/// The backlog lines this host already has a final triage verdict for.
fn read_triaged(home: &Path) -> Vec<String> {
    let data = read_json(&home.join("backlog-triage.json"));
    let lines = data.as_ref().and_then(|d| {
        let map = d.as_object()?;
        if map.len() != 2 || int_of(map.get("version")) != Some(1) {
            return None;
        }
        map.get("lines")?
            .as_array()?
            .iter()
            .map(|l| l.as_str().map(String::from))
            .collect::<Option<Vec<_>>>()
    });
    lines.unwrap_or_default()
}

/// Drop verdicts for lines no longer in the backlog. Rating a new line waits on the intake slice.
fn backlog_triage_pass(home: &Path) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(backlog_triage_pass) {
        return f(home);
    }
    let r = api(home, "GET", "/api/backlog", None, &[200, 404], &[])?;
    let text = if r.status == 200 {
        r.text()?
    } else {
        String::new()
    };
    let raws: Vec<&str> = text
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| BACKLOG_LINE.match_start(l).is_some())
        .collect();
    let cached = read_triaged(home);
    let triaged: Vec<String> = cached
        .iter()
        .filter(|l| raws.contains(&l.as_str()))
        .cloned()
        .collect();
    let todo = raws.iter().any(|raw| !triaged.iter().any(|t| t == raw));
    if triaged != cached {
        write_file(
            &home.join("backlog-triage.json"),
            &(dumps(&json!({"version": 1, "lines": triaged}), INDENT2) + "\n"),
        )?;
    }
    if todo {
        return Err(deferred("backlog triage"));
    }
    Ok(())
}

// ---- group B: routes, holds, claims and launches

const LAUNCH_WAIT_SECONDS: f64 = 30.0;
const LAUNCH_BACKOFF_SECONDS: i64 = 300;
const LAUNCH_BACKOFF_CAP_SECONDS: i64 = 14400;
const LAUNCH_STOP_FAILURES: i64 = 5;
const AGENT_ROWS_MAX: usize = 32;
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const SPEC_APPROVED_NOTES: [&str; 2] =
    ["spec approved in Tracer", "spec approved on the dashboard"];
const RETRO_PROMPT_FILE: &str = "retro-schedule-prompt.md";
const RETRO_CATCHUP_FILE: &str = "retro-catchup.json";
const RETRO_WAIT_FILE: &str = "retro-catchup-wait.json";
const RETRO_CATCHUP_AFTER: (i64, i64) = (9, 30);
const USAGE_PROMPT_LINES: usize = 50;
/// USAGE_TOKENS: the kinds in order, and the usage field each reads.
const USAGE_TOKENS: [(&str, &str); 4] = [
    ("input", "input_tokens"),
    ("output", "output_tokens"),
    ("cache_write", "cache_creation_input_tokens"),
    ("cache_read", "cache_read_input_tokens"),
];
static NOT_BEFORE: Pattern = Pattern::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", Re::NONE);
static LIMIT_TEXT: Pattern = Pattern::new(
    "(?:usage|rate|session|weekly) limit|limit (?:reached|exceeded)",
    Re::I,
);
static RESET_IN: Pattern = Pattern::new(
    r"(?:resets|try again) in (\d{1,4}) ?(minutes?|mins?|m|hours?|hrs?|h)\b",
    Re::I,
);
static RESET_AT: Pattern = Pattern::new(
    r"resets (?:(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) (\d{1,2}), )?(\d{1,2})(?::(\d{2}))?(am|pm) \(([A-Za-z_]+(?:/[A-Za-z0-9_+-]+)*)\)",
    Re::NONE,
);
static PLACEHOLDER: Pattern = Pattern::new(
    r"\{(id|project|repo_path|session|provider|model|effort|prompt)\}",
    Re::NONE,
);
static RETRO_RUN_NAME: Pattern = Pattern::new(r"(\d{8}T\d{6}Z)-[0-9a-f]{6}", Re::NONE);
static RETRO_JOB: Pattern = Pattern::new(r"factory-(\d{2,5})-", Re::NONE);

fn new_claim(device: &str, session: &str) -> Value {
    let stamp = now();
    json!({"device": device, "session": session, "since": stamp, "seen": stamp})
}

fn mine(c: &Value, device: &str, session: &str) -> bool {
    get_str(c, "device") == Some(device) && get_str(c, "session") == Some(session)
}

/// Python truthiness of a JSON value.
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

/// The job's claim when another session holds it and checked in within the last minutes. A
/// claim whose seen stamp does not parse counts as held, where Python stops with a traceback.
fn blocking(job: &Value, device: &str, session: &str, minutes: u32) -> Option<Value> {
    let c = job.get("claim").filter(|c| truthy(Some(c)))?;
    if mine(c, device, session) {
        return None;
    }
    match get_str(c, "seen").and_then(parse_stamp) {
        Some(seen) if unix_now() - seen >= i64::from(minutes) * 60 => None,
        _ => Some(c.clone()),
    }
}

fn claimed_by(c: &Value) -> String {
    format!(
        "claimed by {} session {}",
        text_of(c.get("device")),
        text_of(c.get("session"))
    )
}

/// Python `str(v)` for a JSON value that is usually a string: None for a missing one.
fn text_of(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => other.to_string(),
    }
}

fn earlier_moves(m: &Value) -> Vec<&Value> {
    m.get("earlier")
        .and_then(Value::as_array)
        .map(|e| e.iter().filter(|x| x.is_object()).collect())
        .unwrap_or_default()
}

/// True when pickup recorded this holder's stop. Only a string counts.
fn move_stopped(x: &Value) -> bool {
    x.get("stopped").is_some_and(Value::is_string)
}

/// The move's holders whose host has not stopped them yet, earlier ones first.
fn queued_stops(m: Option<&Value>) -> Vec<&Value> {
    let Some(m) = m.filter(|m| m.is_object()) else {
        return Vec::new();
    };
    let mut all = earlier_moves(m);
    all.push(m);
    all.into_iter()
        .filter(|x| truthy(x.get("from")) && !move_stopped(x))
        .collect()
}

fn jobs_of(data: &Value) -> &[Value] {
    data["jobs"].as_array().map_or(&[], Vec::as_slice)
}

fn open_job_ids(jobs: &[Value]) -> Vec<&Value> {
    jobs.iter()
        .filter(|j| !get_str(j, "stage").is_some_and(|s| TERMINAL.contains(&s)))
        .filter_map(|j| j.get("id"))
        .collect()
}

/// Fresh in-flight jobs on the board: of one project, or of all when project is None, and only
/// this device's claims when device is set.
fn inflight_count(
    data: &Value,
    project: Option<&str>,
    minutes: u32,
    device: Option<&str>,
) -> usize {
    let cutoff = unix_now() - i64::from(minutes) * 60;
    jobs_of(data)
        .iter()
        .filter(|j| {
            if project.is_some_and(|p| !p.is_empty()) && get_str(j, "project") != project {
                return false;
            }
            let claim_device = j.get("claim").and_then(|c| c.get("device"));
            if device.is_some_and(|d| !d.is_empty())
                && claim_device.and_then(Value::as_str) != device
            {
                return false;
            }
            if gated(j) || lead_of(j).is_some() || babysitting(j) || waiting_until(j).is_some() {
                return false;
            }
            let stage = get_str(j, "stage");
            let stamp = if stage == Some(IDEA) || approved_start(j) {
                j.get("claim").and_then(|c| c.get("seen"))
            } else if stage.is_some_and(|s| IN_FLIGHT.contains(&s))
                || stage == Some("pr-open") && !escalated(j)
            {
                j.get("updated")
            } else {
                return false;
            };
            stamp
                .and_then(Value::as_str)
                .and_then(parse_stamp)
                .is_some_and(|seen| seen > cutoff)
        })
        .count()
}

/// One launch entry: the job, its checkout and the session tag it runs as.
#[derive(Clone, Debug)]
struct Entry {
    id: String,
    project: String,
    repo_path: String,
    session: String,
}

/// routes.md's optional experiment line.
#[derive(Clone, Debug)]
struct Experiment {
    name: String,
    provider: String,
    model: String,
    effort: Option<String>,
    percent: u32,
}

/// claim_ideas's keyword arguments.
struct Pick<'a> {
    max_inflight: Option<usize>,
    project: Option<&'a str>,
    by_host: bool,
    builds: bool,
    experiment: Option<&'a Experiment>,
    route: Option<&'a Route>,
    skip_ids: Vec<String>,
}

/// Claim up to limit of the oldest runnable ideas in one board write, and return them. An
/// empty session mints a tag per idea.
#[allow(clippy::too_many_arguments)]
fn claim_ideas(
    home: &Path,
    device: &str,
    session: &str,
    minutes: u32,
    paths: &[(String, String)],
    limit: usize,
    pick: &Pick,
) -> Res<Vec<Entry>> {
    let mut exp_limited = false;
    if let Some(experiment) = pick.experiment {
        let limits = read_limits(home)?;
        exp_limited = lookup(&limits, &experiment.name).is_some_and(|u| u > now().as_str());
    }
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let mut picked: Vec<Entry> = Vec::new();
        let count = match pick.max_inflight {
            None => 0,
            Some(_) if pick.by_host => inflight_count(&data, None, minutes, Some(device)),
            Some(_) => inflight_count(&data, pick.project, minutes, None),
        };
        let budget = match pick.max_inflight {
            None => limit,
            Some(cap) => limit.min(cap.saturating_sub(count)),
        };
        let mut order: Vec<usize> = (0..jobs_of(&data).len()).collect();
        order.sort_by_key(|&i| !prioritized(&jobs_of(&data)[i]));
        for i in order {
            if picked.len() == budget {
                break;
            }
            let j = &data["jobs"][i];
            if !truthy(j.get("project"))
                || gated(j)
                || paused(j)
                || waiting_until(j).is_some()
                || closing(j)
                || waiting_on(j, jobs_of(&data)).is_some()
                || tracer_building(j)
            {
                continue;
            }
            let project = get_str(j, "project").unwrap_or_default().to_string();
            let repo = lookup(paths, &project).map(String::from);
            let startable = get_str(j, "stage") == Some(IDEA)
                || pick.builds && (approved_start(j) || moved_here(j, device));
            let Some(id) = safe_id(j).map(String::from) else {
                continue;
            };
            let Some(repo) = repo.filter(|r| startable && is_dir(r)) else {
                continue;
            };
            if pick.project.is_some_and(|p| p != project)
                || pick.skip_ids.contains(&id)
                || blocking(j, device, session, minutes).is_some()
            {
                continue;
            }
            if let Some(experiment) = pick.experiment {
                let record = j.get("experiment");
                let arm = match record.and_then(|r| get_str(r, "arm")) {
                    Some(arm @ ("a" | "b")) if record.is_some_and(Value::is_object) => {
                        arm.to_string()
                    }
                    _ => experiment_arm(&id, experiment.percent).to_string(),
                };
                if arm == "b" && exp_limited {
                    continue;
                }
                if !truthy(record) {
                    let route = pick.route.unwrap();
                    let (provider, model, effort) = if arm == "b" {
                        (&experiment.provider, &experiment.model, &experiment.effort)
                    } else {
                        (&route.provider, &route.model, &route.effort)
                    };
                    data["jobs"][i]["experiment"] = json!({"name": experiment.name, "arm": arm,
                        "provider": provider, "model": model, "effort": effort});
                }
            }
            let tag = if session.is_empty() {
                token_hex3()
            } else {
                session.to_string()
            };
            let claim = new_claim(device, &tag);
            // Replace the claim whole, as hold does. The paseo key stays as it was.
            data["jobs"][i]
                .as_object_mut()
                .unwrap()
                .shift_remove("claim");
            patch_job(&mut data, &id, &json!({"claim": claim}));
            if pick.builds && moved_here(&data["jobs"][i], device) {
                patch_job(
                    &mut data,
                    &id,
                    &json!({"move": {"claimed": claim["since"]}}),
                );
            }
            picked.push(Entry {
                id,
                project,
                repo_path: repo,
                session: tag,
            });
        }
        let text = (!picked.is_empty()).then(|| dump_board(&data));
        Ok((text, picked))
    })
}

/// A job waiting on a person, as the dashboard's waitsOnYou reads it.
fn waits_on_you(j: &Value) -> bool {
    let stage = get_str(j, "stage");
    if stage.is_some_and(|s| TERMINAL.contains(&s)) {
        return false;
    }
    stage == Some("needs-human")
        || get_str(j, "needs").is_some_and(|n| !strip(n).is_empty())
        || stage == Some("pr-open") && escalated(j)
}

fn read_experiment(home: &Path, route_names: &[String]) -> Res<Option<Experiment>> {
    let path = home.join("routes.md");
    let meta = meta(&path)?;
    let Some(line) = lookup(&meta, "experiment") else {
        return Ok(None);
    };
    let bad = || {
        fail(
            format!(
                "bad experiment in {}: use experiment: <name> <provider> <model> [<effort>] <percent>",
                path.display()
            ),
            2,
        )
    };
    let parts = words(line);
    if !(4..=5).contains(&parts.len()) {
        return Err(bad());
    }
    let (name, provider, model) = (parts[0], parts[1], parts[2]);
    let effort = (parts.len() == 5).then(|| parts[3]);
    let percent = parts[parts.len() - 1];
    let digits = (1..=3).contains(&percent.len()) && percent.bytes().all(|b| b.is_ascii_digit());
    if !SLUG.is_full(name)
        || !SLUG.is_full(provider)
        || !ROUTE_MODEL.is_full(model)
        || route_names.iter().any(|r| r == name)
        || effort.is_some_and(|e| !ROUTE_EFFORTS.contains(&e))
        || !digits
        || percent.parse::<u32>().unwrap_or(999) > 100
    {
        return Err(bad());
    }
    Ok(Some(Experiment {
        name: name.into(),
        provider: provider.into(),
        model: model.into(),
        effort: effort.map(String::from),
        percent: percent.parse().unwrap(),
    }))
}

/// Arm "b" when the job id's stable hash bucket falls under percent, else "a".
fn experiment_arm(job_id: &str, percent: u32) -> &'static str {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(job_id.as_bytes());
    let bucket = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 100;
    if bucket < percent {
        "b"
    } else {
        "a"
    }
}

/// This host's first route not marked limited, or None.
fn pick_route(home: &Path) -> Res<Option<Route>> {
    #[cfg(test)]
    if let Some(f) = fake!(pick_route) {
        return f(home);
    }
    let routes = read_routes(home)?;
    let stamp = now();
    let limits = read_limits(home)?;
    let active = |name: &str| lookup(&limits, name).is_some_and(|u| u > stamp.as_str());
    Ok(routes.into_iter().find(|r| !active(&r.name)))
}

/// True when name is the experiment, False when it is a route, else exit 2.
fn check_limit_name(home: &Path, name: &str) -> Res<bool> {
    let routes: Vec<String> = read_routes(home)?.into_iter().map(|r| r.name).collect();
    let experiment = read_experiment(home, &routes)?;
    if experiment.as_ref().is_some_and(|e| e.name == name) {
        return Ok(true);
    }
    if routes.iter().any(|r| r == name) {
        return Ok(false);
    }
    let path = home.join("routes.md");
    let what = if experiment.is_some() {
        "unknown route or experiment"
    } else {
        "unknown route"
    };
    Err(fail(
        format!("{what} {}: not in {}", repr(name), path.display()),
        2,
    ))
}

/// Mark one route or the experiment limited for minutes from now.
fn limit_route(home: &Path, name: &str, minutes: i64) -> Res<(String, bool)> {
    let experiment = check_limit_name(home, name)?;
    let stamp = now();
    let mut limits: Map<String, Value> = Map::new();
    for (n, until) in read_limits(home)? {
        if until > stamp {
            limits.insert(n, Value::from(until));
        }
    }
    let until = format_stamp(unix_now() + minutes * 60);
    limits.insert(name.to_string(), Value::from(until.clone()));
    write_file(
        &home.join("route-limits.json"),
        &(dumps(&Value::Object(limits), INDENT2) + "\n"),
    )?;
    Ok((until, experiment))
}

/// Drop this session's claim on an idea in one board write, so the idea waits again.
fn release_claim(home: &Path, job_id: &str, device: &str, session: &str) -> Res<()> {
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = jobs_of(&data)
            .iter()
            .find(|j| get_str(j, "id") == Some(job_id))
        else {
            return Err(fail(format!("no job {job_id} on the board"), 4));
        };
        let stage = job.get("stage");
        if get_str(job, "stage") != Some(IDEA) {
            return Err(fail(
                format!(
                    "{job_id} is at {}, not idea: nothing to release",
                    text_of(stage)
                ),
                6,
            ));
        }
        let claim = job.get("claim").filter(|c| truthy(Some(c)));
        if !claim.is_some_and(|c| mine(c, device, session)) {
            return Err(fail(format!("{job_id} has no claim from this session"), 6));
        }
        patch_job(&mut data, job_id, &json!({"claim": null}));
        Ok((Some(dump_board(&data)), ()))
    })
}

/// True when pickup holds this idea for a batch: a follow-up or a triage-queued idea nobody added,
/// approved or prioritized from the dashboard.
fn batch_hold(j: &Value) -> bool {
    if get_str(j, "stage") != Some(IDEA) || j.get("capture_id").is_some() || prioritized(j) {
        return false;
    }
    let follow_up = j.get("refs").and_then(Value::as_array).is_some_and(|refs| {
        refs.iter()
            .any(|r| get_str(r, "kind") == Some("follow-up-of"))
    });
    follow_up || j.get("backlog").and_then(|b| get_str(b, "by")) == Some("triage")
}

/// A field of the effective pipeline view, or None when it is absent or unreadable.
fn view_field(home: &Path, slug: &str, field: &str) -> Res<Option<Value>> {
    let path = format!("/api/pipeline/effective/{slug}");
    let r = api(home, "GET", &path, None, &[200, 404, 409], &[])?;
    if r.status != 200 {
        return Ok(None);
    }
    let view = loads(&r.text()?).ok();
    Ok(view.and_then(|v| v.get(field).cloned()))
}

fn hold_config(home: &Path, slug: &str) -> Res<(i64, i64)> {
    let hold = view_field(home, slug, "hold")?.filter(Value::is_object);
    let least = hold.as_ref().and_then(|h| int_of(h.get("min_ideas")));
    let hours = hold.as_ref().and_then(|h| int_of(h.get("max_wait_hours")));
    match (least, hours) {
        (Some(l), Some(h)) if (1..=MAX_RIDERS as i64).contains(&l) && (1..=168).contains(&h) => {
            Ok((l, h))
        }
        _ => Ok((HOLD_MIN_IDEAS as i64, HOLD_MAX_HOURS as i64)),
    }
}

fn backpressure_config(home: &Path, slug: &str) -> Res<i64> {
    let bp = view_field(home, slug, "backpressure")?.filter(Value::is_object);
    match bp.as_ref().and_then(|b| int_of(b.get("needs_you"))) {
        Some(n) if (1..=50).contains(&n) => Ok(n),
        _ => Ok(BACKPRESSURE_NEEDS_YOU as i64),
    }
}

/// {id: skip line} for plain ideas pickup does not launch while the project's own waiting jobs
/// reach its backpressure.needs_you.
fn backpressure_holds(
    home: &Path,
    jobs: &[&Value],
    waiting: &HashMap<Option<String>, i64>,
    runnable: &dyn Fn(&Value) -> bool,
) -> Res<HashMap<String, String>> {
    let (mut limits, mut lines) = (HashMap::new(), HashMap::new());
    for j in jobs {
        if get_str(j, "stage") != Some(IDEA) || prioritized(j) || !runnable(j) {
            continue;
        }
        let slug = get_str(j, "project").unwrap_or_default().to_string();
        let count = waiting.get(&Some(slug.clone())).copied().unwrap_or(0);
        if count < 1 {
            continue;
        }
        if !limits.contains_key(&slug) {
            limits.insert(slug.clone(), backpressure_config(home, &slug)?);
        }
        let limit = limits[&slug];
        if count >= limit {
            let id = get_str(j, "id").unwrap_or_default();
            lines.insert(
                id.to_string(),
                format!(
                    "skip {id} {slug}: backpressure ({count} waiting on you, pauses at {limit})"
                ),
            );
        }
    }
    Ok(lines)
}

/// {id: skip line} for the held ideas pickup does not launch this run.
fn batch_holds(
    home: &Path,
    jobs: &[&Value],
    runnable: &dyn Fn(&Value) -> bool,
) -> Res<HashMap<String, String>> {
    let mut groups: Vec<(String, Vec<&Value>)> = Vec::new();
    let mut starters: HashMap<String, String> = HashMap::new();
    for j in jobs {
        let slug = get_str(j, "project").unwrap_or_default().to_string();
        let id = get_str(j, "id").unwrap_or_default();
        let batch = j.get("priority").and_then(|p| p.get("batch")) == Some(&Value::Bool(true));
        if get_str(j, "stage") == Some(IDEA)
            && prioritized(j)
            && batch
            && !(gated(j)
                || paused(j)
                || closing(j)
                || waiting_until(j).is_some()
                || tracer_building(j))
        {
            starters
                .entry(slug.clone())
                .or_insert_with(|| id.to_string());
        }
        if batch_hold(j) && runnable(j) {
            match groups.iter_mut().find(|(s, _)| *s == slug) {
                Some((_, held)) => held.push(j),
                None => groups.push((slug, vec![j])),
            }
        }
    }
    let (stamp, mut lines) = (unix_now(), HashMap::new());
    let id = |j: &Value| get_str(j, "id").unwrap_or_default().to_string();
    for (slug, held) in groups {
        if let Some(starter) = starters.get(&slug) {
            for j in &held {
                lines.insert(
                    id(j),
                    format!("skip {} {slug}: batches behind {starter}", id(j)),
                );
            }
            continue;
        }
        let (least, hours) = hold_config(home, &slug)?;
        let age = |j: &Value| {
            get_str(j, "updated")
                .and_then(parse_stamp)
                .map_or(hours * 3600, |u| stamp - u)
        };
        let oldest = held.iter().map(|j| age(j)).max().unwrap();
        if held.len() as i64 >= least || oldest >= hours * 3600 {
            let first = id(held[0]);
            for j in &held[1..] {
                lines.insert(
                    id(j),
                    format!("skip {} {slug}: batches behind {first}", id(j)),
                );
            }
            continue;
        }
        let waited = oldest.max(0) / 3600;
        for j in &held {
            lines.insert(
                id(j),
                format!(
                    "skip {} {slug}: waits for a batch ({} of {least} held, oldest {waited}h of {hours}h)",
                    id(j),
                    held.len()
                ),
            );
        }
    }
    Ok(lines)
}

/// This host's max_inflight from the host registry (0 pauses pickup), or None.
fn host_max_inflight(home: &Path, device: &str) -> Res<Option<usize>> {
    let r = api(home, "GET", "/api/hosts", None, &[200, 404], &[])?;
    if r.status == 404 {
        return Ok(None);
    }
    let hosts = loads(&r.text()?).ok();
    let record = hosts
        .as_ref()
        .and_then(Value::as_array)
        .and_then(|h| h.iter().find(|h| get_str(h, "id") == Some(device)));
    let cap = record.and_then(|r| int_of(r.get("max_inflight")));
    Ok(cap.filter(|c| (0..=5).contains(c)).map(|c| c as usize))
}

/// The values a launcher's placeholders take.
#[derive(Clone, Debug)]
struct Given {
    id: String,
    project: String,
    repo_path: String,
    session: String,
    provider: String,
    model: String,
    effort: String,
    prompt: String,
}

impl Given {
    fn new(entry: &Entry, route: (&str, &str, &str), prompt: String) -> Given {
        Given {
            id: entry.id.clone(),
            project: entry.project.clone(),
            repo_path: entry.repo_path.clone(),
            session: entry.session.clone(),
            provider: route.0.into(),
            model: route.1.into(),
            effort: route.2.into(),
            prompt,
        }
    }
}

fn route_values(route: &Route) -> (&str, &str, &str) {
    (
        &route.provider,
        &route.model,
        route.effort.as_deref().unwrap_or(""),
    )
}

/// The template with each known {placeholder} replaced; unknown braces stay literal.
fn launcher_argv(template: &[String], given: &Given) -> Vec<String> {
    template
        .iter()
        .map(|arg| {
            PLACEHOLDER.replace_all(arg, |c| {
                match c.get(1).unwrap().as_str() {
                    "id" => &given.id,
                    "project" => &given.project,
                    "repo_path" => &given.repo_path,
                    "session" => &given.session,
                    "provider" => &given.provider,
                    "model" => &given.model,
                    "effort" => &given.effort,
                    _ => &given.prompt,
                }
                .clone()
            })
        })
        .collect()
}

fn launch_prompt(command: &str) -> String {
    format!("{command}\n\n{UNATTENDED}")
}

fn launch_wait_seconds() -> f64 {
    #[cfg(test)]
    if let Some(s) = FAKES.with(|f| f.borrow().launch_wait_seconds) {
        return s;
    }
    LAUNCH_WAIT_SECONDS
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return -signal;
    }
    status.code().unwrap_or(1)
}

/// Start one launcher detached, with its output in a private log, and wait up to
/// LAUNCH_WAIT_SECONDS. (ok, detail, log path): still running or exit 0 is ok.
fn launch_idea(
    home: &Path,
    entry: &Entry,
    route: &str,
    argv: &[String],
    env: &[(String, String)],
) -> Res<(bool, Option<String>, PathBuf)> {
    #[cfg(test)]
    if let Some(f) = fake!(launch_idea) {
        return f(home, entry, route, argv, env);
    }
    let _ = route;
    let folder = home.join("logs").join("pickup");
    mkdirs(&folder)?;
    let log_path = folder.join(format!("{}-{}.log", entry.id, entry.session));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let log = options
        .open(&log_path)
        .map_err(|e| fail(os_error(&e, Some(&log_path)), 1))?;
    let err_log = log
        .try_clone()
        .map_err(|e| fail(os_error(&e, Some(&log_path)), 1))?;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(&entry.repo_path)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err_log);
    detach(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let name = basename(&argv[0]);
            return Ok((
                false,
                Some(format!("cannot start {name}: {}", strerror(&e))),
                log_path,
            ));
        }
    };
    let deadline = Instant::now() + Duration::from_secs_f64(launch_wait_seconds());
    loop {
        if let Some(status) = child.try_wait().map_err(|e| fail(os_error(&e, None), 1))? {
            let code = exit_code(status);
            return Ok((code == 0, Some(format!("exit {code}")), log_path));
        }
        if Instant::now() >= deadline {
            // The child is left running, detached; dropping the handle does not stop it.
            return Ok((true, None, log_path));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Seconds pickup parks a job after its nth launch failure in a row: 300, doubling, capped at 4 hours.
fn launch_backoff_seconds(n: i64) -> i64 {
    (LAUNCH_BACKOFF_SECONDS * 2i64.pow((n - 1).clamp(0, 6) as u32)).min(LAUNCH_BACKOFF_CAP_SECONDS)
}

/// A launch failure detail as one board-safe line of at most 200 characters.
fn launch_reason(detail: &str) -> String {
    let line = words(detail).join(" ");
    NOTE_INVISIBLE
        .replace_all(&line, |_| String::new())
        .chars()
        .take(200)
        .collect()
}

/// (count, until) or (count, None) for a stop at needs-human, or None.
type Parked = Option<(i64, Option<String>)>;

/// Drop this session's claim and park the job after a failed launch, in one board write. From the
/// fifth failure in a row, move it to needs-human with a needs ask instead.
fn back_off_launch(
    home: &Path,
    job_id: &str,
    device: &str,
    session: &str,
    reason: &str,
) -> Res<Parked> {
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = jobs_of(&data)
            .iter()
            .find(|j| get_str(j, "id") == Some(job_id))
        else {
            return Ok((None, None));
        };
        if !job
            .get("claim")
            .is_some_and(|c| truthy(Some(c)) && mine(c, device, session))
        {
            return Ok((None, None));
        }
        let prior = job
            .get("launch_failure")
            .filter(|r| r.is_object())
            .and_then(|r| int_of(r.get("count")))
            .filter(|n| *n >= 1)
            .unwrap_or(0);
        let count = prior + 1;
        let at = unix_now();
        let failure = json!({"count": count, "reason": reason, "at": format_stamp(at)});
        if count >= LAUNCH_STOP_FAILURES {
            let needs = format!("Pickup launch failed {count} times in a row on host {device}. Last reason: {reason}. Fix the launcher, then clear this ask and move the job back to {}.", text_of(job.get("stage")));
            patch_job(
                &mut data,
                job_id,
                &json!({"claim": null, "not_before": null,
                "stage": "needs-human", "needs": needs, "launch_failure": failure}),
            );
            return Ok((Some(dump_board(&data)), Some((count, None))));
        }
        let until = format_stamp(at + launch_backoff_seconds(count));
        patch_job(
            &mut data,
            job_id,
            &json!({"claim": null, "not_before": until, "launch_failure": failure}),
        );
        Ok((Some(dump_board(&data)), Some((count, Some(until)))))
    })
}

/// Drop a job's launch_failure after a launch succeeds; a failed write warns instead of ending the pass.
fn clear_launch_failure(io: &mut Io, home: &Path, job_id: &str) {
    let cleared = cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        if !jobs_of(&data)
            .iter()
            .any(|j| get_str(j, "id") == Some(job_id) && j.get("launch_failure").is_some())
        {
            return Ok((None, ()));
        }
        patch_job(&mut data, job_id, &json!({"launch_failure": null}));
        Ok((Some(dump_board(&data)), ()))
    });
    if let Err(e) = cleared {
        let _ = io.err.write_all(e.stderr.as_bytes());
        errln!(io, "factory.py: {job_id} launched, but its launch failure was not cleared; the pass goes on");
    }
}

/// The TZif data for zone: (transition times, their type index, (utoff, isdst) types, footer).
struct Zone {
    times: Vec<i64>,
    index: Vec<usize>,
    types: Vec<(i64, bool)>,
    rule: Option<TzRule>,
}

/// A POSIX TZ footer: standard and DST offsets east of UTC, and the DST start and end rules.
struct TzRule {
    std: i64,
    dst: Option<(i64, (u32, u32, u32, u32, i64), (u32, u32, u32, u32, i64))>,
}

fn read_zone(name: &str) -> Option<Zone> {
    if Path::new(name).is_absolute() || name.contains("..") || WINDOWS {
        return None;
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("TZDIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect();
    dirs.extend(
        [
            "/usr/share/zoneinfo",
            "/usr/lib/zoneinfo",
            "/usr/share/lib/zoneinfo",
            "/etc/zoneinfo",
        ]
        .map(PathBuf::from),
    );
    let data = dirs.iter().find_map(|d| fs::read(d.join(name)).ok())?;
    parse_tzif(&data)
}

fn parse_tzif(data: &[u8]) -> Option<Zone> {
    let header = |at: usize| -> Option<[usize; 6]> {
        if data.get(at..at + 4)? != b"TZif" {
            return None;
        }
        let mut counts = [0usize; 6];
        for (i, c) in counts.iter_mut().enumerate() {
            let b = data.get(at + 20 + i * 4..at + 24 + i * 4)?;
            *c = u32::from_be_bytes(b.try_into().ok()?) as usize;
        }
        Some(counts)
    };
    let [isut, isstd, leap, time, typ, chars] = header(0)?;
    let v1 = 44 + time * 5 + typ * 6 + chars + leap * 8 + isstd + isut;
    let (start, size, counts) = if data[4] >= b'2' {
        (v1 + 44, 8, header(v1)?)
    } else {
        (44, 4, [isut, isstd, leap, time, typ, chars])
    };
    let [isut, isstd, leap, time, typ, chars] = counts;
    let mut at = start;
    let mut times = Vec::new();
    for _ in 0..time {
        let b = data.get(at..at + size)?;
        times.push(if size == 8 {
            i64::from_be_bytes(b.try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(b.try_into().ok()?))
        });
        at += size;
    }
    let index: Vec<usize> = data
        .get(at..at + time)?
        .iter()
        .map(|b| *b as usize)
        .collect();
    at += time;
    let mut types = Vec::new();
    for _ in 0..typ {
        let b = data.get(at..at + 6)?;
        types.push((
            i64::from(i32::from_be_bytes(b[..4].try_into().ok()?)),
            b[4] != 0,
        ));
        at += 6;
    }
    if index.iter().any(|i| *i >= types.len()) || types.is_empty() {
        return None;
    }
    at += chars + leap * (size + 4) + isstd + isut;
    let rule = if size == 8 {
        let footer = std::str::from_utf8(data.get(at..)?).ok()?;
        let footer = footer.strip_prefix('\n')?.split('\n').next()?;
        parse_tz_rule(footer)
    } else {
        None
    };
    Some(Zone {
        times,
        index,
        types,
        rule,
    })
}

/// A POSIX TZ string such as `EST5EDT,M3.2.0,M11.1.0`, or None when it is empty or unsupported.
fn parse_tz_rule(s: &str) -> Option<TzRule> {
    fn name(s: &str) -> Option<&str> {
        if let Some(rest) = s.strip_prefix('<') {
            return Some(&rest[rest.find('>')? + 1..]);
        }
        let n = s
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(s.len());
        (n >= 3).then(|| &s[n..])
    }
    fn offset(s: &str) -> Option<(i64, &str)> {
        let (sign, body) = match s.as_bytes().first()? {
            b'-' => (-1, &s[1..]),
            b'+' => (1, &s[1..]),
            _ => (1, s),
        };
        let end = body
            .find(|c: char| !(c.is_ascii_digit() || c == ':'))
            .unwrap_or(body.len());
        let mut secs = 0;
        for (i, part) in body[..end].split(':').enumerate() {
            secs += part.parse::<i64>().ok()? * [3600, 60, 1].get(i)?;
        }
        Some((sign * secs, &body[end..]))
    }
    fn date(s: &str) -> Option<((u32, u32, u32, u32, i64), &str)> {
        let end = s.find(',').unwrap_or(s.len());
        let (spec, time) = s[..end]
            .split_once('/')
            .map_or((&s[..end], None), |(a, b)| (a, Some(b)));
        let time = match time {
            Some(t) => offset(t).filter(|(_, rest)| rest.is_empty())?.0,
            None => 7200,
        };
        let rest = &s[end..];
        let parsed = if let Some(m) = spec.strip_prefix('M') {
            let mut parts = m.split('.').map(|p| p.parse::<u32>().ok());
            (0, parts.next()??, parts.next()??, parts.next()??, time)
        } else if let Some(j) = spec.strip_prefix('J') {
            (1, j.parse().ok()?, 0, 0, time)
        } else {
            (2, spec.parse().ok()?, 0, 0, time)
        };
        Some((parsed, rest))
    }
    let rest = name(s)?;
    let (std, rest) = offset(rest)?;
    if rest.is_empty() {
        return Some(TzRule {
            std: -std,
            dst: None,
        });
    }
    let rest = name(rest)?;
    let (dst, rest) = match rest.as_bytes().first() {
        Some(b',') | None => (std - 3600, rest),
        _ => offset(rest)?,
    };
    let rest = rest.strip_prefix(',')?;
    let (start, rest) = date(rest)?;
    let (end, rest) = date(rest.strip_prefix(',')?)?;
    rest.is_empty().then_some(TzRule {
        std: -std,
        dst: Some((-dst, start, end)),
    })
}

/// Seconds since 1970 of a rule's transition in year y, as local wall time.
fn rule_wall(y: i64, (kind, a, b, c, time): (u32, u32, u32, u32, i64)) -> i64 {
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let day = match kind {
        0 => {
            let first = days_from_civil(y, i64::from(a), 1);
            let weekday = (first + 4).rem_euclid(7);
            let mut day = first + (i64::from(c) - weekday).rem_euclid(7) + 7 * (i64::from(b) - 1);
            while civil(day).1 != i64::from(a) {
                day -= 7;
            }
            day
        }
        1 => {
            let n = i64::from(a);
            days_from_civil(y, 1, 1) + n - 1 + i64::from(leap && n >= 60)
        }
        _ => days_from_civil(y, 1, 1) + i64::from(a),
    };
    day * 86_400 + time
}

/// Seconds east of UTC in zone at utc, from its TZif data and then its footer rule.
fn zone_offset(zone: &str, utc: i64) -> Option<i64> {
    let z = read_zone(zone)?;
    Some(zone_at(&z, utc))
}

fn zone_at(z: &Zone, utc: i64) -> i64 {
    let after_last = z.times.last().is_none_or(|last| utc >= *last);
    if let (true, Some(rule)) = (after_last, &z.rule) {
        let Some((dst, start, end)) = rule.dst else {
            return rule.std;
        };
        let y = civil((utc + rule.std).div_euclid(86_400)).0;
        let begin = rule_wall(y, start) - rule.std;
        let finish = rule_wall(y, end) - dst;
        let in_dst = if begin < finish {
            (begin..finish).contains(&utc)
        } else {
            !(finish..begin).contains(&utc)
        };
        return if in_dst { dst } else { rule.std };
    }
    match z.times.iter().rposition(|t| *t <= utc) {
        Some(i) => z.types[z.index[i]].0,
        None => z.types.iter().find(|t| !t.1).unwrap_or(&z.types[0]).0,
    }
}

/// The UTC instant of a wall time in zone, with fold 0: the offset before a transition wins in
/// both an overlap and a gap.
fn zone_wall_to_utc(zone: &str, wall: i64) -> Option<i64> {
    let z = read_zone(zone)?;
    let early = zone_at(&z, wall - 14 * 3600);
    let late = zone_at(&z, wall + 14 * 3600);
    if zone_at(&z, wall - early) == early {
        return Some(wall - early);
    }
    if zone_at(&z, wall - late) == late {
        return Some(wall - late);
    }
    Some(wall - early)
}

/// Minutes until a usage limit in launcher output resets, from 1 to 1440, else 60.
fn limit_minutes(text: &str, now: i64) -> i64 {
    let mut minutes = 60;
    if let Some(after) = RESET_IN.search(text) {
        let n: i64 = after.get(1).unwrap().as_str().parse().unwrap();
        let hours = after
            .get(2)
            .unwrap()
            .as_str()
            .to_lowercase()
            .starts_with('h');
        minutes = n * if hours { 60 } else { 1 };
    } else if let Some(at) = RESET_AT.search(text) {
        let group = |i| at.get(i).map(|m| m.as_str());
        let reset = || -> Option<i64> {
            let zone = group(6)?;
            let local = now + zone_offset(zone, now)?;
            let (y, m, d) = civil(local.div_euclid(86_400));
            let hour =
                group(3)?.parse::<i64>().ok()? % 12 + if group(5) == Some("pm") { 12 } else { 0 };
            let minute = group(4).map_or(Some(0), |m| m.parse::<i64>().ok())?;
            if minute > 59 {
                return None;
            }
            let wall = |y: i64, m: i64, d: i64| -> Option<i64> {
                let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
                let len = [
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
                ];
                if d < 1 || d > len[(m - 1) as usize] {
                    return None;
                }
                zone_wall_to_utc(
                    zone,
                    days_from_civil(y, m, d) * 86_400 + hour * 3600 + minute * 60,
                )
            };
            match group(1) {
                None => {
                    let reset = wall(y, m, d)?;
                    if reset > now {
                        return Some(reset);
                    }
                    let (y, m, d) = civil(days_from_civil(y, m, d) + 1);
                    wall(y, m, d)
                }
                Some(month) => {
                    let month = MONTHS.iter().position(|n| *n == month)? as i64 + 1;
                    let day: i64 = group(2)?.parse().ok()?;
                    let reset = wall(y, month, day)?;
                    if reset < now - 86_400 {
                        return wall(y + 1, month, day);
                    }
                    Some(reset)
                }
            }
        };
        if let Some(reset) = reset() {
            minutes = (reset - now + 59).div_euclid(60);
        }
    }
    minutes.clamp(1, 1440)
}

/// The launcher's FACTORY_ environment, added to this process's own.
fn launch_env(given: &Given, provider: &str, model: &str) -> Vec<(String, String)> {
    [
        ("FACTORY_JOB_ID", given.id.as_str()),
        ("FACTORY_PROJECT", &given.project),
        ("FACTORY_REPO_PATH", &given.repo_path),
        ("FACTORY_SESSION", &given.session),
        ("FACTORY_PROVIDER", provider),
        ("FACTORY_MODEL", model),
        ("FACTORY_EFFORT", &given.effort),
        ("FACTORY_PROMPT", &given.prompt),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// The last 4096 bytes of a launch log, decoded with replacement.
fn log_tail(log_path: &Path) -> Res<String> {
    let data = fs::read(log_path).map_err(|e| fail(os_error(&e, Some(log_path)), 1))?;
    let tail = &data[data.len().saturating_sub(4096)..];
    Ok(String::from_utf8_lossy(tail).into_owned())
}

/// After a failed launch: mark the route limited when its log says so, else exit 1, or with
/// fatal false log the failure and return. release drops the claim first, except that a non-limit
/// failure with backoff parks the job instead.
#[allow(clippy::too_many_arguments)]
fn launch_failed(
    io: &mut Io,
    home: &Path,
    name: &str,
    job_id: &str,
    detail: &str,
    log_path: &Path,
    release: &mut dyn FnMut() -> Res<Parked>,
    fatal: bool,
    experiment: bool,
    backoff: Option<&mut dyn FnMut() -> Res<Parked>>,
) -> Res<()> {
    let tail = log_tail(log_path)?;
    if LIMIT_TEXT.search(&tail).is_some() {
        let until = limit_route(home, name, limit_minutes(&tail, unix_now()))?.0;
        release()?;
        if experiment {
            outln!(io, "limited experiment {name} until {until}");
        } else {
            outln!(io, "limited {name} until {until}");
        }
        outln!(io, "released {job_id}");
        return Ok(());
    }
    let parked = match backoff {
        Some(backoff) => backoff()?,
        None => release()?,
    };
    outln!(io, "released {job_id}");
    match parked {
        Some((count, None)) => outln!(
            io,
            "needs-human {job_id} after {count} launch failures in a row"
        ),
        Some((count, Some(until))) => outln!(
            io,
            "backed off {job_id} until {until} (launch failure {count})"
        ),
        None => {}
    }
    let message = format!(
        "launch of {job_id} on {name} failed: {detail}; see {}",
        log_path.display()
    );
    if fatal {
        return Err(fail(message, 1));
    }
    errln!(io, "factory.py: {message}");
    Ok(())
}

fn dated(j: &Value, key: &str) -> bool {
    j.get(key)
        .is_some_and(|v| v.get("at").is_some_and(Value::is_string))
}

/// True when the dashboard asked pickup to take this job first.
fn prioritized(j: &Value) -> bool {
    dated(j, "priority")
}

/// True when the dashboard handed this job's PR to a babysit session.
fn babysitting(j: &Value) -> bool {
    dated(j, "babysit")
}

/// True when the dashboard paused pickup for this job.
fn paused(j: &Value) -> bool {
    dated(j, "paused")
}

/// The job's not_before stamp while it is still in the future, else None.
fn waiting_until(j: &Value) -> Option<String> {
    get_str(j, "not_before")
        .filter(|t| NOT_BEFORE.is_full(t) && *t > now().as_str())
        .map(String::from)
}

/// The first depends-on id of an idea whose job is still open on the board, else None.
fn waiting_on(j: &Value, jobs: &[Value]) -> Option<String> {
    if get_str(j, "stage") != Some(IDEA) {
        return None;
    }
    let open = open_job_ids(jobs);
    j.get("refs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| get_str(r, "kind") == Some("depends-on"))
        .filter_map(|r| r.get("id"))
        .find(|id| open.contains(id))
        .map(|id| text_of(Some(id)))
}

/// True when the dashboard queued a close that pickup has not done yet.
fn closing(j: &Value) -> bool {
    dated(j, "close") && j["close"].get("closed").is_none()
}

/// True when the dashboard moved this in-flight job to this host, no pickup claimed it yet, and
/// no stop of it is still queued on this host.
fn moved_here(j: &Value, device: &str) -> bool {
    let Some(m) = j.get("move").filter(|m| m.is_object()) else {
        return false;
    };
    get_str(m, "to") == Some(device)
        && m.get("claimed").is_none()
        && get_str(j, "stage").is_some_and(|s| IN_FLIGHT.contains(&s))
        && !queued_stops(Some(m))
            .iter()
            .any(|x| get_str(x, "from") == Some(device))
}

/// Python's `f"{n:,}"`.
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
        format!("-{out}")
    } else {
        out
    }
}

/// {job id: skip line} for every free job once this host's job tokens since local midnight reach
/// daily_token_cap.
fn daily_cap_holds(
    io: &mut Io,
    home: &Path,
    device: &str,
    jobs: &[&Value],
    free: &dyn Fn(&Value) -> bool,
    dry: bool,
) -> Res<HashMap<String, String>> {
    let cap = token_cap(io, home, "daily_token_cap")?;
    let held: Vec<&&Value> = match cap {
        Some(_) => jobs.iter().filter(|j| free(j)).collect(),
        None => Vec::new(),
    };
    let (Some(cap), false) = (cap, held.is_empty()) else {
        return Ok(HashMap::new());
    };
    let used = match daily_tokens(local_midnight()) {
        Ok(used) => used,
        Err(kind) => {
            errln!(
                io,
                "factory.py: pickup skips the daily token cap: scan failed ({kind})"
            );
            return Ok(HashMap::new());
        }
    };
    if used < cap {
        return Ok(HashMap::new());
    }
    let reached = format!(
        "daily token cap reached ({}/{} since local midnight)",
        commas(used),
        commas(cap)
    );
    if !dry {
        errln!(
            io,
            "factory.py: pickup launches no new job on {device}: {reached}"
        );
    }
    Ok(held
        .into_iter()
        .map(|j| {
            let id = get_str(j, "id").unwrap_or_default();
            (
                id.to_string(),
                format!(
                    "skip {id} {}: host {device} {reached}",
                    text_of(j.get("project"))
                ),
            )
        })
        .collect())
}

/// One pickup run after flags and the holder are resolved. Reads the board unless given it.
#[allow(clippy::too_many_arguments)]
fn pickup_pass(
    io: &mut Io,
    home: &Path,
    device: &str,
    minutes: u32,
    dry: bool,
    limit: usize,
    cap: Option<usize>,
    data: Option<Value>,
) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(pickup_pass) {
        return f(io, home);
    }
    let paths = paths_of(home);
    let mut data = match data {
        Some(data) => data,
        None => read_board(home)?,
    };
    let stalls = read_stalls(home)?;
    let mut settings: HashMap<String, Settings> = HashMap::new();
    let adapter = stalls.as_ref().map(|s| s.0.clone());
    move_stop_pass(
        io,
        home,
        &mut data,
        device,
        &paths,
        &mut settings,
        dry,
        adapter.as_deref(),
    )?;
    session_stop_pass(io, home, &mut data, device, dry)?;
    pr_sync_pass(
        io,
        home,
        &mut data,
        device,
        minutes,
        &paths,
        &mut settings,
        dry,
    )?;
    autonomy_hold_pass(io, home, &mut data, dry)?;
    close_pass(io, home, &mut data, device, minutes, dry)?;
    close_cleanup_pass(io, home, &mut data, device, &paths, &mut settings, dry)?;
    priority_nudge_pass(io, home, &mut data, device, dry)?;
    idle_watch_pass(io, home, &mut data, device, dry)?;
    token_cap_pass(io, home, &mut data, device, dry)?;
    auto_babysit_pass(
        io,
        home,
        &mut data,
        device,
        minutes,
        &paths,
        &mut settings,
        dry,
    )?;
    babysit_pass(
        io,
        home,
        &mut data,
        device,
        minutes,
        &paths,
        &mut settings,
        dry,
    )?;
    let taken = match &stalls {
        Some(_) => takeover(
            io,
            home,
            &mut data,
            device,
            minutes,
            &paths,
            &mut settings,
            dry,
        )?,
        None => 0,
    };
    deny_notes_pass(
        io,
        home,
        &mut data,
        device,
        minutes,
        &paths,
        &mut settings,
        dry,
    )?;
    resolve_pass(
        io,
        home,
        &mut data,
        device,
        minutes,
        &paths,
        &mut settings,
        dry,
    )?;
    if !dry && taken >= limit {
        return Ok(());
    }
    let mut jobs: Vec<&Value> = jobs_of(&data)
        .iter()
        .filter(|j| {
            (get_str(j, "stage") == Some(IDEA) || approved_start(j) || moved_here(j, device))
                && safe_id(j).is_some()
        })
        .collect();
    jobs.sort_by_key(|j| !prioritized(j));
    let id_of = |j: &Value| safe_id(j).unwrap().to_string();
    let starts: Vec<String> = jobs
        .iter()
        .filter(|j| approved_start(j) || moved_here(j, device))
        .map(|j| id_of(j))
        .collect();
    let (mut experiment, mut exp_limited) = (None, None);
    // Only an experiment line needs routes.md this early; without one, a host with no routes stays silent.
    let routes_md = home.join("routes.md");
    if !jobs.is_empty() && lookup(&meta(&routes_md)?, "experiment").is_some() {
        let names: Vec<String> = read_routes(home)?.into_iter().map(|r| r.name).collect();
        let e = read_experiment(home, &names)?.unwrap();
        let limits = read_limits(home)?;
        exp_limited = lookup(&limits, &e.name)
            .filter(|u| *u > now().as_str())
            .map(String::from);
        experiment = Some(e);
    }
    let job_arm = |j: Option<&Value>| -> Option<String> {
        let j = j?;
        let record = j.get("experiment").filter(|r| r.is_object());
        match record.and_then(|r| get_str(r, "arm")) {
            Some(arm @ ("a" | "b")) => Some(arm.to_string()),
            _ => experiment
                .as_ref()
                .map(|e| experiment_arm(&id_of(j), e.percent).to_string()),
        }
    };
    let all = jobs_of(&data);
    let base_skip = |j: &Value| -> Option<String> {
        let id = id_of(j);
        let Some(project) = get_str(j, "project").filter(|p| !p.is_empty()) else {
            return Some(format!("skip {id}: no project"));
        };
        if !lookup(&paths, project).is_some_and(is_dir) {
            return Some(format!("skip {id} {project}: no checkout on this host"));
        }
        if gated(j) {
            return Some(format!("skip {id} {project}: waiting on a human"));
        }
        if paused(j) {
            return Some(format!("skip {id} {project}: paused"));
        }
        if closing(j) {
            return Some(format!("skip {id} {project}: closing"));
        }
        if let Some(until) = waiting_until(j) {
            return Some(format!("skip {id} {project}: not before {until}"));
        }
        if let Some(other) = waiting_on(j, all) {
            return Some(format!("skip {id} {project}: waits on {other}"));
        }
        if tracer_building(j) {
            return Some(format!("skip {id} {project}: building in Tracer"));
        }
        if let Some(c) = blocking(j, device, "", minutes) {
            return Some(format!("skip {id} {project}: {}", claimed_by(&c)));
        }
        if let (Some(e), Some(until)) = (&experiment, &exp_limited) {
            if job_arm(Some(j)).as_deref() == Some("b") {
                return Some(format!(
                    "skip {id} {project}: experiment {} limited until {until}",
                    e.name
                ));
            }
        }
        None
    };
    let base: HashMap<String, Option<String>> =
        jobs.iter().map(|j| (id_of(j), base_skip(j))).collect();
    let runnable = |j: &Value| base[&id_of(j)].is_none();
    let mut waiting: HashMap<Option<String>, i64> = HashMap::new();
    for j in all {
        if waits_on_you(j) {
            *waiting
                .entry(get_str(j, "project").map(String::from))
                .or_default() += 1;
        }
    }
    let mut holds = batch_holds(home, &jobs, &runnable)?;
    holds.extend(backpressure_holds(home, &jobs, &waiting, &runnable)?);
    holds.extend(daily_cap_holds(io, home, device, &jobs, &runnable, dry)?);
    let skip = |j: &Value| base[&id_of(j)].is_some() || holds.contains_key(&id_of(j));

    let mut eligible: Vec<String> = Vec::new();
    for j in &jobs {
        let slug = get_str(j, "project").unwrap_or_default().to_string();
        if skip(j) || eligible.contains(&slug) {
            continue;
        }
        if !settings.contains_key(&slug) {
            let found = pickup_settings(home, &slug, lookup(&paths, &slug).unwrap(), device)?;
            if let (Some(reason), false) = (&found.reason, dry) {
                errln!(io, "factory.py: pickup skips {slug}: {reason}");
            }
            settings.insert(slug.clone(), found);
        }
        if settings[&slug].lists(device) {
            eligible.push(slug);
        }
    }

    let host_cap = if eligible.is_empty() {
        None
    } else {
        host_max_inflight(home, device)?
    };
    let by_host = host_cap.is_some();
    let cap = match host_cap {
        Some(h) => cap.map_or(h, |c| c.min(h)),
        None => cap.unwrap_or(1),
    };
    let route = if eligible.is_empty() {
        None
    } else {
        pick_route(home)?
    };
    let template = match &route {
        Some(route) => route_launcher(home, route)?,
        None => Vec::new(),
    };
    let values = |entry: &Entry| {
        let verb = if starts.contains(&entry.id) {
            "recover"
        } else {
            "idea"
        };
        let route = route.as_ref().unwrap();
        Given::new(
            entry,
            route_values(route),
            launch_prompt(&format!(
                "/factory {verb} {} --session {}",
                entry.id, entry.session
            )),
        )
    };
    let job_given = |j: Option<&Value>, entry: &Entry| -> (Option<String>, Given) {
        let mut given = values(entry);
        let arm = job_arm(j);
        if arm.as_deref() == Some("b") {
            let record = j
                .and_then(|j| j.get("experiment"))
                .filter(|r| r.is_object() && truthy(r.get("provider")) && truthy(r.get("model")));
            if let Some(r) = record {
                given.provider = text_of(r.get("provider"));
                given.model = text_of(r.get("model"));
                given.effort = get_str(r, "effort").unwrap_or_default().to_string();
            } else if let Some(e) = &experiment {
                given.provider = e.provider.clone();
                given.model = e.model.clone();
                given.effort = e.effort.clone().unwrap_or_default();
            }
        }
        (arm, given)
    };

    if dry {
        let (mut would, mut no_route) = (Vec::<(String, usize)>::new(), false);
        for j in &jobs {
            let slug = get_str(j, "project").unwrap_or_default().to_string();
            let mut line = base[&id_of(j)].clone();
            if line.is_none() {
                let id = id_of(j);
                if !settings.contains_key(&slug) {
                    let found =
                        pickup_settings(home, &slug, lookup(&paths, &slug).unwrap(), device)?;
                    settings.insert(slug.clone(), found);
                }
                let s = &settings[&slug];
                let count = if by_host {
                    inflight_count(&data, None, minutes, Some(device))
                        + would.iter().map(|(_, n)| n).sum::<usize>()
                } else {
                    inflight_count(&data, Some(&slug), minutes, None) + lookup_count(&would, &slug)
                };
                line = Some(if let Some(reason) = &s.reason {
                    format!("skip {id} {slug}: {reason}")
                } else if s.enabled != Some(true) {
                    format!("skip {id} {slug}: pickup off")
                } else if !s.hosts.iter().any(|h| h == device) {
                    format!("skip {id} {slug}: pickup does not list host {device}")
                } else if let Some(hold) = holds.get(&id) {
                    hold.clone()
                } else if by_host && cap == 0 {
                    format!("skip {id} {slug}: host {device} paused (max in flight 0)")
                } else if count >= cap && by_host {
                    format!("skip {id} {slug}: host {device} in-flight cap full ({count}/{cap})")
                } else if count >= cap {
                    format!("skip {id} {slug}: in-flight cap full ({count}/{cap})")
                } else if route.is_none() {
                    if no_route {
                        continue;
                    }
                    no_route = true;
                    no_route_line(home)?
                } else {
                    match would.iter_mut().find(|(s, _)| *s == slug) {
                        Some(slot) => slot.1 += 1,
                        None => would.push((slug.clone(), 1)),
                    }
                    let entry = Entry {
                        id: id.clone(),
                        project: slug.clone(),
                        repo_path: lookup(&paths, &slug).unwrap().to_string(),
                        session: "<session>".into(),
                    };
                    let (arm, given) = job_given(Some(j), &entry);
                    let shown =
                        arm.map_or(String::new(), |a| format!(" arm {a} model {}", given.model));
                    format!(
                        "would launch {id} {slug} on {}{shown}: {}",
                        route.as_ref().unwrap().name,
                        shlex_join(&launcher_argv(&template, &given))
                    )
                });
            }
            outln!(io, "{}", line.unwrap());
        }
        if stalls.is_some() {
            outln!(io, "host {device}: would take over {taken}");
        }
        let total: usize = would.iter().map(|(_, n)| n).sum();
        outln!(io, "host {device}: would launch {total}");
        return Ok(());
    }
    let Some(route) = route.clone() else {
        return Ok(());
    };
    let skip_ids: Vec<String> = holds.keys().cloned().collect();
    let mut launches = taken;
    while launches < limit {
        let mut picked = Vec::new();
        for slug in &eligible {
            let pick = Pick {
                max_inflight: Some(cap),
                project: Some(slug),
                by_host,
                builds: true,
                experiment: experiment.as_ref(),
                route: Some(&route),
                skip_ids: skip_ids.clone(),
            };
            picked = claim_ideas(home, device, "", minutes, &paths, 1, &pick)?;
            if !picked.is_empty() {
                break;
            }
        }
        let Some(entry) = picked.into_iter().next() else {
            return Ok(());
        };
        let j = jobs_of(&data)
            .iter()
            .find(|x| get_str(x, "id") == Some(entry.id.as_str()));
        let (arm, given) = job_given(j, &entry);
        let argv = launcher_argv(&template, &given);
        let env = launch_env(&given, &given.provider, &given.model);
        let (ok, detail, log_path) = launch_idea(home, &entry, &route.name, &argv, &env)?;
        if ok {
            outln!(
                io,
                "launched {} {} on {} session {}",
                entry.id,
                entry.project,
                route.name,
                entry.session
            );
            record_launch(
                io,
                home,
                &entry.id,
                launch_row(&given, &argv, &entry.session),
            );
            if j.is_some_and(|j| j.get("launch_failure").is_some()) {
                clear_launch_failure(io, home, &entry.id);
            }
            launches += 1;
            continue;
        }
        let b = arm.as_deref() == Some("b") && experiment.is_some();
        let limit_name = if b {
            experiment.as_ref().unwrap().name.clone()
        } else {
            route.name.clone()
        };
        let detail = detail.unwrap_or_default();
        let reason = launch_reason(&detail);
        let mut release = || release_claim(home, &entry.id, device, &entry.session).map(|_| None);
        let mut backoff = || back_off_launch(home, &entry.id, device, &entry.session, &reason);
        return launch_failed(
            io,
            home,
            &limit_name,
            &entry.id,
            &detail,
            &log_path,
            &mut release,
            true,
            b,
            Some(&mut backoff),
        );
    }
    Ok(())
}

fn lookup_count(pairs: &[(String, usize)], key: &str) -> usize {
    pairs.iter().find(|(k, _)| k == key).map_or(0, |(_, n)| *n)
}

/// The dry-run line when every route is limited.
fn no_route_line(home: &Path) -> Res<String> {
    let stamp = now();
    let limits = read_limits(home)?;
    let routes = read_routes(home)?;
    let shown: Vec<String> = routes
        .iter()
        .map(|r| {
            let until = lookup(&limits, &r.name).filter(|u| *u > stamp.as_str());
            format!("{} until {}", r.name, until.unwrap_or_default())
        })
        .collect();
    Ok(format!(
        "no route: no available route: {}",
        shown.join(", ")
    ))
}

/// True when Tracer scope approval or the dashboard's Approve spec moved this job to build and no
/// session has started it.
fn approved_start(j: &Value) -> bool {
    get_str(j, "stage") == Some("build")
        && get_str(j, "note").is_some_and(|n| SPEC_APPROVED_NOTES.contains(&n))
}

/// True while a job waits on a human: it has an ask, or it is an unapproved explorer card.
fn gated(j: &Value) -> bool {
    if get_str(j, "needs").is_some_and(|n| !strip(n).is_empty()) {
        return true;
    }
    j.get("origin").is_some_and(|o| {
        o.is_object() && get_str(o, "gate") == Some("human") && !truthy(o.get("approved"))
    })
}

/// True when pickup marked this job as held by a Tracer build agent.
fn tracer_building(j: &Value) -> bool {
    j.get("tracer").and_then(|t| t.get("building")) == Some(&Value::Bool(true))
}

/// A message or tool_result content as text: a string, or its joined text blocks.
fn retro_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| get_str(b, "text"))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Local time as (utc, offset) with its weekday (Monday 0), hour, minute and date.
struct Local {
    utc: i64,
    offset: i64,
}

impl Local {
    fn wall(&self) -> i64 {
        self.utc + self.offset
    }
    fn weekday(&self) -> i64 {
        (self.wall().div_euclid(86_400) + 3).rem_euclid(7)
    }
    fn hour_minute(&self) -> (i64, i64) {
        let t = self.wall().rem_euclid(86_400);
        (t / 3600, t / 60 % 60)
    }
    fn date(&self) -> String {
        local_date(self.utc, self.offset)
    }
}

/// Whether this retro host missed today's weekday retro.
fn retro_catchup_due(home: &Path, local: &Local) -> bool {
    #[cfg(test)]
    if let Some(f) = fake!(retro_catchup_due) {
        return f(home, local);
    }
    let prompt = home.join(RETRO_PROMPT_FILE);
    if is_link(&prompt).unwrap_or(false) || !prompt.is_file() {
        return false;
    }
    if local.weekday() > 4 || local.hour_minute() < RETRO_CATCHUP_AFTER {
        return false;
    }
    let today = local.date();
    let record = read_json(&home.join(RETRO_CATCHUP_FILE));
    if record.as_ref().and_then(Value::as_object).is_some_and(|r| {
        r.len() == 2
            && eq_int(r.get("version"), 1)
            && r.get("date").and_then(Value::as_str) == Some(today.as_str())
    }) {
        return false;
    }
    let Ok(entries) = fs::read_dir(home.join("retro")) else {
        return true;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(m) = RETRO_RUN_NAME.fullmatch(&name) else {
            continue;
        };
        if !entry.path().is_dir() {
            continue;
        }
        let s = m.get(1).unwrap().as_str();
        let stamp = format!(
            "{}-{}-{}T{}:{}:{}Z",
            &s[..4],
            &s[4..6],
            &s[6..8],
            &s[9..11],
            &s[11..13],
            &s[13..15]
        );
        let Some(at) = parse_stamp(&stamp) else {
            continue;
        };
        if local_date(at, local.offset) == today {
            return false;
        }
    }
    true
}

/// Print why the retro catch-up waits, once per local date and reason. A dry run prints every
/// time and records nothing.
fn retro_catchup_wait(io: &mut Io, home: &Path, local: &Local, reason: &str, dry: bool) -> Res<()> {
    let record = json!({"version": 1, "date": local.date(), "reason": reason});
    let path = home.join(RETRO_WAIT_FILE);
    if !dry && read_json(&path).as_ref() == Some(&record) {
        return Ok(());
    }
    errln!(io, "factory.py: retro catch-up waits: {reason}");
    if !dry {
        write_file(&path, &(dumps(&record, DEFAULT) + "\n"))?;
    }
    Ok(())
}

/// Launch the stored retro schedule prompt once on a weekday whose 09:00 slot left no run.
fn retro_pickup_catchup(io: &mut Io, home: &Path, dry: bool) {
    let mut catchup = || -> Res<()> {
        let (utc, offset) = local_now();
        let local = Local { utc, offset };
        if !retro_catchup_due(home, &local) {
            return Ok(());
        }
        let job_id = format!("retro-{}", local.date().replace('-', ""));
        let Some(route) = pick_route(home)? else {
            return retro_catchup_wait(io, home, &local, "no available route", dry);
        };
        let Some((slug, path)) = paths_of(home).into_iter().find(|(_, p)| is_dir(p)) else {
            return retro_catchup_wait(io, home, &local, "no checkout in paths.md", dry);
        };
        let template = route_launcher(home, &route)?;
        if dry {
            outln!(io, "would launch retro catch-up {job_id} on {}", route.name);
            return Ok(());
        }
        let prompt_path = home.join(RETRO_PROMPT_FILE);
        let prompt =
            read_text(&prompt_path).map_err(|e| fail(os_error(&e, Some(&prompt_path)), 1))?;
        let entry = Entry {
            id: job_id.clone(),
            project: slug,
            repo_path: path,
            session: token_hex3(),
        };
        let given = Given::new(&entry, route_values(&route), prompt);
        write_file(
            &home.join(RETRO_CATCHUP_FILE),
            &(dumps(&json!({"version": 1, "date": local.date()}), DEFAULT) + "\n"),
        )?;
        let argv = launcher_argv(&template, &given);
        let env = launch_env(&given, &route.provider, &route.model);
        let (ok, detail, log_path) = launch_idea(home, &entry, &route.name, &argv, &env)?;
        if ok {
            outln!(
                io,
                "launched retro catch-up {job_id} on {} session {}",
                route.name,
                entry.session
            );
        } else {
            errln!(
                io,
                "factory.py: retro catch-up {job_id} failed to launch ({}); see {}; no retry today",
                detail.unwrap_or_default(),
                log_path.display()
            );
        }
        Ok(())
    };
    if let Err(e) = catchup() {
        let _ = io.err.write_all(e.stderr.as_bytes());
        errln!(
            io,
            "factory.py: retro catch-up stopped (exit {}); the next pickup run retries it",
            e.code
        );
    }
}

fn retro_transcripts() -> PathBuf {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::home_dir().unwrap_or_default().join(".claude"));
    base.join("projects")
}

/// Entries of dir, sorted, or none when it cannot be read.
fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = fs::read_dir(dir)
        .map(|e| e.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    items.sort();
    items
}

fn jsonl(dir: &Path) -> Vec<PathBuf> {
    sorted_entries(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".jsonl"))
        })
        .collect()
}

/// A transcript and its subagent transcripts, each with whether it is a main session.
fn with_subagents(project: &Path, path: &Path) -> Vec<(PathBuf, bool)> {
    let stem = path.file_stem().unwrap_or_default();
    let mut found = vec![(path.to_path_buf(), true)];
    found.extend(
        jsonl(&project.join(stem).join("subagents"))
            .into_iter()
            .map(|s| (s, false)),
    );
    found
}

/// The sorted transcript folders this host keeps from the job's worktrees.
fn audit_folders(job_id: &str) -> Vec<PathBuf> {
    let needle = format!("factory-{job_id}");
    sorted_entries(&retro_transcripts())
        .into_iter()
        .filter(|p| {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            p.is_dir()
                && name.match_indices(&needle).any(|(at, _)| {
                    let before = name[..at].chars().last();
                    let after = name[at + needle.len()..].chars().next();
                    before.is_none_or(|c| c == '-') && after.is_none_or(|c| c == '-')
                })
        })
        .collect()
}

/// The lines of a transcript, read as text with replacement, without their endings.
fn transcript_lines(path: &Path) -> io::Result<Vec<String>> {
    let text = String::from_utf8_lossy(&fs::read(path)?).into_owned();
    let text = crate::shared::py::universal_newlines(text);
    Ok(text.split_inclusive('\n').map(String::from).collect())
}

/// The text of a transcript's first non-meta user line within USAGE_PROMPT_LINES, else "".
fn usage_prompt(path: &Path) -> String {
    let Ok(lines) = transcript_lines(path) else {
        return String::new();
    };
    for line in lines.iter().take(USAGE_PROMPT_LINES) {
        let Ok(e) = loads(line) else { continue };
        if get_str(&e, "type") == Some("user") && !truthy(e.get("isMeta")) {
            let msg = e.get("message").filter(|m| m.is_object());
            return msg.map_or(String::new(), |m| retro_text(m.get("content")));
        }
    }
    String::new()
}

/// ({model: {kind: tokens}}, unreadable line count) for one transcript, each message.id counted once.
fn usage_scan(
    path: &Path,
    main: bool,
    since: Option<&str>,
) -> io::Result<(Map<String, Value>, usize)> {
    let mut turns: Vec<(String, String, [i64; 4])> = Vec::new();
    let mut bad = 0;
    for line in transcript_lines(path)? {
        if strip(&line).is_empty() {
            continue;
        }
        let Ok(e) = loads(&line) else {
            bad += 1;
            continue;
        };
        if !e.is_object()
            || get_str(&e, "type") != Some("assistant")
            || main && truthy(e.get("isSidechain"))
        {
            continue;
        }
        if let Some(since) = since {
            if !get_str(&e, "timestamp").is_some_and(|t| t >= since) {
                continue;
            }
        }
        let Some(msg) = e.get("message").filter(|m| m.is_object()) else {
            continue;
        };
        let (Some(id), Some(usage)) = (
            get_str(msg, "id"),
            msg.get("usage").filter(|u| u.is_object()),
        ) else {
            continue;
        };
        let model = get_str(msg, "model")
            .filter(|m| ROUTE_MODEL.is_full(m))
            .unwrap_or("other");
        let at = match turns.iter().position(|t| t.0 == id) {
            Some(at) => at,
            None => {
                turns.push((id.to_string(), model.to_string(), [0; 4]));
                turns.len() - 1
            }
        };
        for (k, (_, field)) in USAGE_TOKENS.iter().enumerate() {
            if let Some(v) = int_of(usage.get(*field)) {
                turns[at].2[k] = turns[at].2[k].max(v);
            }
        }
    }
    let mut models: Vec<(String, [i64; 4])> = Vec::new();
    for (_, model, counts) in turns {
        let at = match models.iter().position(|m| m.0 == model) {
            Some(at) => at,
            None => {
                models.push((model, [0; 4]));
                models.len() - 1
            }
        };
        for k in 0..4 {
            models[at].1[k] += counts[k];
        }
    }
    let map = models
        .into_iter()
        .filter(|(_, c)| c.iter().any(|v| *v != 0))
        .map(|(m, c)| (m, counts_value(&c)))
        .collect();
    Ok((map, bad))
}

fn counts_value(c: &[i64; 4]) -> Value {
    Value::Object(
        USAGE_TOKENS
            .iter()
            .zip(c)
            .map(|((k, _), v)| (k.to_string(), Value::from(*v)))
            .collect(),
    )
}

/// This host's transcripts that ran in the job's worktree, plus each main session or subagent
/// elsewhere whose own first prompt names the job folder or the job's prompt folder.
fn usage_transcripts(home: &Path, slug: &str, job_id: &str) -> Vec<(PathBuf, bool)> {
    let root = retro_transcripts();
    if !root.is_dir() {
        return Vec::new();
    }
    let mut needles: Vec<String> = Vec::new();
    for base in ["jobs", "prompts"] {
        let p = home.join(base).join(slug).join(job_id);
        for s in [
            p.to_string_lossy().into_owned(),
            p.to_string_lossy().replace('\\', "/"),
        ] {
            if !needles.contains(&s) {
                needles.push(s);
            }
        }
    }
    let named = |text: &str| {
        needles.iter().any(|n| {
            text.match_indices(n.as_str()).any(|(at, _)| {
                !text[at + n.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
            })
        })
    };
    let worktrees = audit_folders(job_id);
    let mut found = Vec::new();
    for project in sorted_entries(&root).into_iter().filter(|p| p.is_dir()) {
        for path in jsonl(&project) {
            let all = with_subagents(&project, &path);
            if worktrees.contains(&project) {
                found.extend(all);
                continue;
            }
            found.extend(all.into_iter().filter(|(p, _)| named(&usage_prompt(p))));
        }
    }
    found
}

/// Tokens this host's job sessions spent since midnight, or the kind of error the scan hit.
fn daily_tokens(midnight: i64) -> Result<i64, String> {
    #[cfg(test)]
    if let Some(f) = fake!(daily_tokens) {
        return f(midnight);
    }
    let root = retro_transcripts();
    if !root.is_dir() {
        return Ok(0);
    }
    let since = &format_stamp(midnight)[..19];
    let mut used = 0;
    for project in sorted_entries(&root) {
        let name = project
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if !project.is_dir() || RETRO_JOB.search(&name).is_none() {
            continue;
        }
        for path in jsonl(&project) {
            for (file, main) in with_subagents(&project, &path) {
                let fresh = fs::metadata(&file)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .is_some_and(|d| d.as_secs_f64() >= midnight as f64);
                if fresh {
                    if let Ok((models, _)) = usage_scan(&file, main, Some(since)) {
                        used += token_count(&Value::Object(models));
                    }
                }
            }
        }
    }
    Ok(used)
}

/// Input, output and cache_write tokens summed over a {model: {kind: tokens}} map.
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

/// The start of today in the host's local time zone, as UTC seconds.
fn local_midnight() -> i64 {
    let (utc, offset) = local_now();
    (utc + offset).div_euclid(86_400) * 86_400 - offset
}

/// config.md's key as a whole number of at least 1, else None. A bad value prints one line.
fn token_cap(io: &mut Io, home: &Path, key: &str) -> Res<Option<i64>> {
    let config = home.join("config.md");
    let meta = if config.is_file() {
        meta(&config)?
    } else {
        Vec::new()
    };
    let Some(value) = lookup(&meta, key) else {
        return Ok(None);
    };
    match value.parse::<i64>() {
        Ok(n) if n >= 1 && value.bytes().all(|b| b.is_ascii_digit()) => Ok(Some(n)),
        _ => {
            errln!(
                io,
                "factory.py: pickup ignores {key} in {}: must be a whole number of at least 1",
                config.display()
            );
            Ok(None)
        }
    }
}

/// The orchestrator row for one pickup launch, on the values it actually used.
fn launch_row(given: &Given, argv: &[String], tag: &str) -> Value {
    let stem = Path::new(&argv[0])
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let mut row = json!({"role": "orchestrator", "harness": given.provider,
        "via": if stem == "paseo" { "paseo" } else { "launcher" }, "model": given.model});
    if !given.effort.is_empty() {
        row["effort"] = Value::from(given.effort.clone());
    }
    row["session"] = Value::from(tag);
    row
}

/// One CAS: append row, stamped with the board's stage, round and time, to job_id's agents,
/// keeping the newest AGENT_ROWS_MAX. The saved row, or None for a missing job.
fn record_agent(home: &Path, job_id: &str, row: &Value) -> Res<Option<Value>> {
    #[cfg(test)]
    if let Some(f) = fake!(record_agent) {
        return f(home, job_id, row);
    }
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = jobs_of(&data)
            .iter()
            .find(|j| get_str(j, "id") == Some(job_id))
        else {
            return Ok((None, None));
        };
        let mut full = json!({"at": now(), "stage": job.get("stage")});
        if get_str(job, "stage") == Some("review") && job.get("round").is_some_and(|r| !r.is_null())
        {
            full["round"] = job["round"].clone();
        }
        for (k, v) in row.as_object().unwrap() {
            full[k] = v.clone();
        }
        let mut rows = job
            .get("agents")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        rows.push(full.clone());
        let keep = rows.len().saturating_sub(AGENT_ROWS_MAX);
        patch_job(&mut data, job_id, &json!({"agents": rows[keep..]}));
        Ok((Some(dump_board(&data)), Some(full)))
    })
}

/// record_agent for a launch that already happened: a failed write warns instead of ending the pass.
fn record_launch(io: &mut Io, home: &Path, job_id: &str, row: Value) {
    if let Err(e) = record_agent(home, job_id, &row) {
        let _ = io.err.write_all(e.stderr.as_bytes());
        errln!(
            io,
            "factory.py: {job_id} launched, but its agent row was not recorded; the pass goes on"
        );
    }
}

// ---- group C: the board passes

type Cache = HashMap<String, Settings>;

fn read_stalls(home: &Path) -> Res<Option<(Vec<String>, i64)>> {
    // group C placeholder
    let _ = home;
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn move_stop_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
    adapter: Option<&[String]>,
) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, device, paths, settings, dry, adapter);
    Ok(())
}

fn session_stop_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    dry: bool,
) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, device, dry);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn pr_sync_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, device, minutes, paths, settings, dry);
    Ok(())
}

fn autonomy_hold_pass(io: &mut Io, home: &Path, data: &mut Value, dry: bool) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, dry);
    Ok(())
}

fn close_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    dry: bool,
) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, device, minutes, dry);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn close_cleanup_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, device, paths, settings, dry);
    Ok(())
}

fn priority_nudge_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    dry: bool,
) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, device, dry);
    Ok(())
}

fn idle_watch_pass(io: &mut Io, home: &Path, data: &mut Value, device: &str, dry: bool) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, device, dry);
    Ok(())
}

fn token_cap_pass(io: &mut Io, home: &Path, data: &mut Value, device: &str, dry: bool) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, device, dry);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn auto_babysit_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, device, minutes, paths, settings, dry);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn babysit_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, device, minutes, paths, settings, dry);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn takeover(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<usize> {
    // group C placeholder
    let _ = (io, home, data, device, minutes, paths, settings, dry);
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn deny_notes_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, device, minutes, paths, settings, dry);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<()> {
    // group C placeholder
    let _ = (io, home, data, device, minutes, paths, settings, dry);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    pub(super) static ENV_LOCK: Mutex<()> = Mutex::new(());
    const DEVICE: &str = "host-a";
    const IDEA_TEXT: &str = "$(touch pwned)";
    const PLACEHOLDERS: [&str; 10] = [
        "{id}",
        "{project}",
        "{repo_path}",
        "{session}",
        "{provider}",
        "{model}",
        "{effort}",
        "{prompt}",
        "{unknown}",
        "x-{id}-y",
    ];
    /// The fake launcher: it records its pid, cwd, argv and FACTORY_ environment, prints a line,
    /// sleeps and exits as fake.conf says.
    const FAKE: &str = r#"#!/bin/sh
dir=$(dirname "$0")
. "$dir/fake.conf"
{
  printf 'R%s\000' "$$"
  printf 'C%s\000' "$(pwd -P)"
  for a in "$@"; do printf 'A%s\000' "$a"; done
  for k in FACTORY_JOB_ID FACTORY_PROJECT FACTORY_REPO_PATH FACTORY_SESSION FACTORY_PROVIDER FACTORY_MODEL FACTORY_EFFORT FACTORY_PROMPT; do
    eval "set_=\${$k+x}"
    if [ "$set_" = x ]; then eval "printf 'E%s=%s\000' $k \"\$$k\""; fi
  done
} >> "$dir/records"
printf '%s\n' "$line"
sleep "$sleep"
exit "$code"
"#;

    static COUNT: AtomicUsize = AtomicUsize::new(0);

    /// A temp folder `factory-pickup-<pid>-<n>`, removed on drop.
    pub(super) struct TempHome(pub PathBuf);

    impl TempHome {
        pub fn new() -> TempHome {
            let n = COUNT.fetch_add(1, Ordering::SeqCst);
            let dir =
                std::env::temp_dir().join(format!("factory-pickup-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempHome(resolve(&dir).unwrap())
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub(super) fn stamp(minutes: i64) -> String {
        format_stamp(unix_now() + minutes * 60)
    }

    pub(super) fn with(base: Value, extra: Value) -> Value {
        let mut base = base;
        for (k, v) in extra.as_object().unwrap() {
            if v.is_null() {
                base.as_object_mut().unwrap().shift_remove(k);
            } else {
                base[k] = v.clone();
            }
        }
        base
    }

    pub(super) fn idea(id: &str, project: impl Into<Value>, extra: Value) -> Value {
        with(
            json!({"id": id, "stage": "idea", "project": project.into(), "title": IDEA_TEXT,
                   "idea": IDEA_TEXT, "updated": "2026-01-01T00:00:00Z"}),
            extra,
        )
    }

    pub(super) fn on() -> Value {
        json!({"enabled": true, "hosts": ["other-host", DEVICE]})
    }

    /// An effective pipeline view whose pickup is the project's, else the global one's.
    pub(super) fn effective(slug: &str, global: Option<Value>, project: Option<Value>) -> Value {
        let pick = |doc: &Option<Value>| doc.as_ref().and_then(|d| d.get("pickup").cloned());
        let pickup = pick(&project)
            .or_else(|| pick(&global))
            .unwrap_or(json!({"enabled": false, "hosts": []}));
        json!({"project": slug, "merge": "auto", "review_rounds": 2,
               "reviewers": ["security", "ux", "ui", "code"], "gates": [], "pickup": pickup,
               "stage_notes": {}, "versions": {"global": 1, "project": 1}, "floors": []})
    }

    pub(super) fn view_on(slug: &str) -> (u16, String) {
        (
            200,
            effective(slug, Some(json!({"pickup": on()})), None).to_string(),
        )
    }

    pub(super) fn host(id: &str, max_inflight: Value) -> Value {
        json!({"id": id, "label": "Host A", "state": "active", "max_inflight": max_inflight})
    }

    pub(super) fn held(device: &str) -> Value {
        json!({"device": device, "session": "s1", "since": stamp(-5), "seen": stamp(-5)})
    }

    pub(super) fn ok(status: u16, body: &str) -> Res<Response> {
        Ok(Response {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        })
    }

    type Extra = Rc<dyn Fn(&str, &str, Option<&[u8]>) -> Option<Res<Response>>>;

    /// The Worker the Python PickupFixture fakes: a board, pipeline views, hosts and halt.
    #[derive(Default)]
    pub(super) struct State {
        pub board: Value,
        pub views: HashMap<String, (u16, String)>,
        pub hosts: Option<Value>,
        pub halt: Option<String>,
        pub calls: Vec<(String, String)>,
        pub extra: Option<Extra>,
    }

    pub(super) fn request(
        state: &RefCell<State>,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        ok_codes: &[u16],
        headers: &[(&str, &str)],
    ) -> Res<Response> {
        if path == "/api/halt" {
            assert_eq!((method, ok_codes), ("GET", &[200u16, 404][..]));
            return match state.borrow().halt.clone() {
                None => ok(404, ""),
                Some(text) => ok(200, &text),
            };
        }
        state
            .borrow_mut()
            .calls
            .push((method.to_string(), path.to_string()));
        if let Some(extra) = state.borrow().extra.clone() {
            if let Some(reply) = extra(method, path, body) {
                return reply;
            }
        }
        if path == "/api/board" {
            if method == "GET" {
                let text = crate::shared::board::dump_board(&state.borrow().board);
                return Ok(Response {
                    status: 200,
                    headers: vec![("ETag".into(), "\"board\"".into())],
                    body: text.into_bytes(),
                });
            }
            assert_eq!((method, headers), ("PUT", &[("If-Match", "\"board\"")][..]));
            state.borrow_mut().board = loads(std::str::from_utf8(body.unwrap()).unwrap()).unwrap();
            return ok(200, "");
        }
        if path == "/api/pipeline/global" {
            return ok(200, r#"{"schema": 1, "tracer": {"enabled": true}}"#);
        }
        if path == "/api/hosts" {
            assert_eq!((method, ok_codes), ("GET", &[200u16, 404][..]));
            return match &state.borrow().hosts {
                None => ok(404, ""),
                Some(hosts) => ok(200, &hosts.to_string()),
            };
        }
        let slug = path.strip_prefix("/api/pipeline/effective/");
        let view = slug.and_then(|s| state.borrow().views.get(s).cloned());
        match view {
            Some((status, text)) if method == "GET" => {
                assert_eq!(ok_codes, &[200, 404, 409]);
                ok(status, &text)
            }
            _ => panic!("unexpected request {method} {path}"),
        }
    }

    pub(super) struct Record {
        pub pid: u32,
        pub cwd: String,
        pub argv: Vec<String>,
        pub env: Vec<(String, String)>,
    }

    impl Record {
        pub fn env(&self, key: &str) -> &str {
            lookup(&self.env, key).unwrap()
        }
    }

    /// Python's PickupFixture: a home, two checkouts, routes, launchers and a faked Worker.
    pub(super) struct Fx {
        _env: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
        pub tmp: TempHome,
        pub root: PathBuf,
        pub home: PathBuf,
        pub repo_a: PathBuf,
        pub repo_b: PathBuf,
        pub routes: PathBuf,
        pub fake: PathBuf,
        pub state: Rc<RefCell<State>>,
        pub sweeps: Rc<RefCell<usize>>,
        pub statuses: Rc<RefCell<Vec<String>>>,
    }

    impl Fx {
        pub fn new() -> Fx {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let tmp = TempHome::new();
            let root = tmp.0.clone();
            let home = root.join("home");
            let (repo_a, repo_b) = (root.join("repo-a"), root.join("repo-b"));
            for folder in [&home, &repo_a, &repo_b] {
                fs::create_dir(folder).unwrap();
            }
            fs::write(
                home.join("config.md"),
                format!("---\ndevice: {DEVICE}\n---\n"),
            )
            .unwrap();
            let (a, b) = (repo_a.display(), repo_b.display());
            fs::write(home.join("paths.md"), format!("---\nalpha: {a}\nbeta: {b}\ndelta: {b}\necho: {b}\nfox: {b}\ngolf: {b}\nhotel: {b}\nmissing: {}\n---\n", root.join("gone").display())).unwrap();
            let routes = home.join("routes.md");
            fs::write(
                &routes,
                "---\nprimary: provider-a model-a high\nfallback: provider-b model-b\n---\n",
            )
            .unwrap();
            let fake = root.join("fake_launcher.sh");
            fs::write(&fake, FAKE).unwrap();
            let mut saved = Vec::new();
            for (key, value) in [
                ("HOME", Some(root.clone().into_os_string())),
                (
                    "CLAUDE_CONFIG_DIR",
                    Some(root.join("claude").into_os_string()),
                ),
                ("PASEO_HOME", Some(root.join("paseo").into_os_string())),
                ("PASEO_AGENT_ID", None),
                ("FACTORY_SESSION", None),
                ("PATH", std::env::var_os("PATH")),
            ] {
                saved.push((key, std::env::var_os(key)));
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
            let fx = Fx {
                _env: guard,
                saved,
                tmp,
                root,
                home,
                repo_a,
                repo_b,
                routes,
                fake,
                state: Rc::new(RefCell::new(State {
                    board: json!({"updated": "2026-01-01T00:00:00Z", "jobs": []}),
                    ..State::default()
                })),
                sweeps: Rc::new(RefCell::new(0)),
                statuses: Rc::new(RefCell::new(Vec::new())),
            };
            fx.configure("started", 0, 0);
            let fake = fx.fake.display().to_string();
            let mut primary = vec!["/bin/sh".to_string(), fake.clone()];
            primary.extend(PLACEHOLDERS.map(String::from));
            fx.write_launchers(json!({"primary": primary, "fallback": ["/bin/sh", fake, "{id}"]}));
            let (state, sweeps, statuses) =
                (fx.state.clone(), fx.sweeps.clone(), fx.statuses.clone());
            FAKES.with(|f| {
                *f.borrow_mut() = Fakes {
                    api: Some(Rc::new(move |_, method, path, body, ok, headers| {
                        request(&state, method, path, body, ok, headers)
                    })),
                    report_status: Some(Rc::new(move |_, scheduler, _, _| {
                        statuses.borrow_mut().push(scheduler.to_string());
                        true
                    })),
                    backlog_triage_pass: Some(Rc::new(move |_| {
                        *sweeps.borrow_mut() += 1;
                        Ok(())
                    })),
                    checkout_problem: Some(Rc::new(|_, _, _| Ok(None))),
                    orphan_pickup_scan: Some(Rc::new(|_| {})),
                    outcomes_pickup_record: Some(Rc::new(|_| {})),
                    ..Fakes::default()
                }
            });
            fx
        }

        pub fn configure(&self, line: &str, code: i32, sleep: u32) {
            fs::write(
                self.root.join("fake.conf"),
                format!("line={}\ncode={code}\nsleep={sleep}\n", shlex_quote(line)),
            )
            .unwrap();
        }

        pub fn write_launchers(&self, launchers: Value) {
            let doc = json!({"version": 1, "launchers": launchers});
            fs::write(self.home.join("launchers.json"), doc.to_string()).unwrap();
        }

        pub fn jobs(&self, jobs: Value) {
            self.state.borrow_mut().board["jobs"] = jobs;
        }

        pub fn view(&self, slug: &str, status: u16, view: Value) {
            let text = view.as_str().map_or_else(|| view.to_string(), String::from);
            self.state
                .borrow_mut()
                .views
                .insert(slug.to_string(), (status, text));
        }

        pub fn run_args(&self, args: &[&str], overridden: bool) -> (i32, String, String) {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            let home = self.home.clone();
            let result = {
                let mut io = Io {
                    out: &mut out,
                    err: &mut err,
                };
                dispatch(&mut io, &args, &move || Ok(home.clone()), overridden)
            };
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

        pub fn pickup(&self, args: &[&str]) -> (i32, String, String) {
            self.run_args(args, false)
        }

        pub fn calls(&self) -> Vec<(String, String)> {
            self.state.borrow().calls.clone()
        }

        pub fn clear_calls(&self) {
            self.state.borrow_mut().calls.clear();
        }

        pub fn puts(&self) -> Vec<(String, String)> {
            self.calls().into_iter().filter(|c| c.0 == "PUT").collect()
        }

        pub fn launched(&self) -> Vec<Record> {
            let Ok(data) = fs::read(self.root.join("records")) else {
                return Vec::new();
            };
            let mut records: Vec<Record> = Vec::new();
            for field in data.split(|b| *b == 0).filter(|f| !f.is_empty()) {
                let text = String::from_utf8_lossy(&field[1..]).into_owned();
                match field[0] {
                    b'R' => records.push(Record {
                        pid: text.parse().unwrap(),
                        cwd: String::new(),
                        argv: Vec::new(),
                        env: Vec::new(),
                    }),
                    b'C' => records.last_mut().unwrap().cwd = text,
                    b'A' => records.last_mut().unwrap().argv.push(text),
                    _ => {
                        let (k, v) = text.split_once('=').unwrap();
                        records
                            .last_mut()
                            .unwrap()
                            .env
                            .push((k.to_string(), v.to_string()));
                    }
                }
            }
            records
        }

        pub fn clear_launched(&self) {
            let _ = fs::remove_file(self.root.join("records"));
        }

        pub fn assert_silent(&self, result: (i32, String, String)) {
            assert_eq!(result, (0, String::new(), String::new()));
            assert_eq!(self.puts(), Vec::<(String, String)>::new());
            assert!(self.launched().is_empty());
        }

        pub fn job(&self, id: &str) -> Value {
            self.state.borrow().board["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|j| j["id"] == id)
                .unwrap()
                .clone()
        }

        pub fn set_fakes(&self, f: impl FnOnce(&mut Fakes)) {
            FAKES.with(|fakes| f(&mut fakes.borrow_mut()));
        }

        pub fn set_env(&self, key: &str, value: impl AsRef<std::ffi::OsStr>) {
            std::env::set_var(key, value);
        }
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            FAKES.with(|f| *f.borrow_mut() = Fakes::default());
            for (key, value) in &self.saved {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    /// A snapshot of every path and file under root, as Python's support.snapshot.
    pub(super) fn snapshot(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(p) = stack.pop() {
            if p.is_dir() {
                out.push((p.clone(), None));
                for e in fs::read_dir(&p).unwrap() {
                    stack.push(e.unwrap().path());
                }
            } else {
                out.push((p.clone(), fs::read(&p).ok()));
            }
        }
        out.sort();
        out
    }

    fn lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    mod deferred {
        use super::*;

        fn assert_deferred(result: (i32, String, String), what: &str) {
            assert_eq!(
                result,
                (
                    2,
                    String::new(),
                    format!("factory.py: pickup {what} is not ported to the Rust binary yet\n")
                )
            );
        }

        #[test]
        fn tracer_follow_pass() {
            let fx = Fx::new();
            fs::write(fx.home.join("tracer.json"), "{}").unwrap();
            assert_deferred(fx.pickup(&[]), "tracer follow pass");
        }

        #[test]
        fn tracer_build_pass() {
            let fx = Fx::new();
            fx.jobs(json!([idea(
                "001-tracer",
                "alpha",
                json!({"tracer": {"building": true}})
            )]));
            assert_deferred(fx.pickup(&[]), "tracer build pass");
            fs::write(fx.home.join("tracer.json"), "{}").unwrap();
            fx.set_fakes(|f| f.tracer_follow_pass = Some(Rc::new(|_| Ok(()))));
            assert_deferred(fx.pickup(&[]), "tracer build pass");
        }

        #[test]
        fn backlog_triage() {
            let fx = Fx::new();
            fx.set_fakes(|f| {
                f.backlog_triage_pass = None;
                f.pickup_pass = Some(Rc::new(|_, _| Ok(())));
            });
            fs::write(
                fx.home.join("backlog-triage.json"),
                r#"{"version": 1, "lines": ["- [ ] alpha: gone"]}"#,
            )
            .unwrap();
            let backlog = Rc::new(RefCell::new(String::new()));
            let text = backlog.clone();
            fx.state.borrow_mut().extra = Some(Rc::new(move |_, path, _| {
                (path == "/api/backlog").then(|| ok(200, &text.borrow()))
            }));
            assert_eq!(fx.pickup(&[]).0, 0);
            assert_eq!(
                read_text(&fx.home.join("backlog-triage.json")).unwrap(),
                "{\n  \"version\": 1,\n  \"lines\": []\n}\n"
            );
            *backlog.borrow_mut() = "# backlog\n\n- [ ] alpha: export the board\n".into();
            assert_deferred(fx.pickup(&["--triage-now"]), "backlog triage");
        }

        #[test]
        fn retro_push() {
            let fx = Fx::new();
            fs::create_dir_all(fx.root.join("claude/projects")).unwrap();
            fs::write(
                fx.home.join(RETRO_PUSH_FILE),
                r#"{"version": 1, "date": "2026-01-01"}"#,
            )
            .unwrap();
            assert_deferred(fx.pickup(&[]), "retro push");
        }

        #[test]
        fn audit_push() {
            let fx = Fx::new();
            fs::create_dir_all(fx.root.join("claude/projects")).unwrap();
            let (utc, offset) = local_now();
            fs::write(
                fx.home.join(RETRO_PUSH_FILE),
                json!({"version": 1, "date": local_date(utc, offset)}).to_string(),
            )
            .unwrap();
            assert_deferred(fx.pickup(&[]), "audit push");
        }

        #[test]
        fn outcomes_record() {
            let fx = Fx::new();
            fx.set_fakes(|f| {
                f.outcomes_pickup_record = None;
                f.pickup_pass = Some(Rc::new(|_, _| Ok(())));
            });
            assert_deferred(fx.pickup(&[]), "outcomes record");
            let record: Value = read_json(&fx.home.join(OUTCOMES_RECORD_FILE)).unwrap();
            assert_eq!(record["version"], 1);
            assert_eq!(
                fx.pickup(&[]).0,
                0,
                "the record holds the date, so it waits a day"
            );
        }
    }

    mod pickup_test {
        use super::*;

        #[test]
        fn test_usage() {
            let fx = Fx::new();
            let usage =
                "factory.py: usage: pickup [--dry-run] [--triage-now] [--limit N] [--max-inflight N]";
            for args in [
                &["--limit", "0"][..],
                &["--limit", "6"],
                &["--limit"],
                &["--limit", "x"],
                &["--max-inflight", "0"],
                &["--max-inflight", "-1"],
                &["--max-inflight"],
                &["--dry-run", "--dry-run"],
                &["--bogus"],
                &["extra"],
                &["--limit", "1", "--limit", "1"],
                &["--triage-now", "--dry-run"],
                &["--triage-now", "--triage-now"],
            ] {
                let (code, out, err) = fx.pickup(args);
                assert_eq!((code, out.as_str()), (2, ""), "{args:?}");
                let mut want = usage.to_string();
                if args == ["--triage-now", "--dry-run"] {
                    want += "; --triage-now cannot run with --dry-run";
                }
                assert_eq!(err, want + "\n", "{args:?}");
            }
            let session = crate::cli::main(vec!["pickup".into(), "--session".into()]);
            assert_eq!(session, 2);
            assert!(fx.calls().is_empty());
        }
    }
}
