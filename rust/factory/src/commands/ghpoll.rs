//! The daemon's GitHub poller, and the gh host the PR wait commands use to read through it.
//!
//! `checks`, `pr-state` and `watch --pr` read a PR through a handful of read-only gh calls and
//! repeat them on a timer. On a daemon host each such call goes over the daemon's local socket
//! instead (`{"op":"gh","args":[...],"max_age":S,"every":S}`). The daemon answers from its cache
//! when the answer is young enough, and otherwise runs gh once for every caller that asks at the
//! same time. Every key a command has read stays on the daemon's poll list while a command keeps
//! reading it, polled every `every` seconds while its answer shows work still pending, at most
//! once a minute otherwise, and less often after GitHub rate-limits a call. A waiting command
//! sleeps on `{"op":"gh-wait",...}`, which returns once one of its keys gets a new answer or the
//! sleep ends, and then reads the cached answers. The daemon forwards gh's exit code and both
//! streams unchanged, so every command prints and exits as it does with no daemon.
//!
//! With no daemon, a daemon that can't answer, or gh credentials set in the command's own
//! environment, each call runs gh directly, as before.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::commands::checks::{GhCode, GhOut, Host, System, HEAD_CHECKS_QUERY, PR_GRAPHQL_QUERY};
use crate::shared::board_client::{ask, PROTOCOL};
use crate::shared::proc::which;

/// The slowest a kept key is polled while its answer shows nothing pending.
const SLOW: Duration = Duration::from_secs(60);
/// The fastest any key is polled, whatever a command asks.
const FASTEST: Duration = Duration::from_secs(5);
/// How long a key stays on the poll list after a command last read or waited on it.
const LEASE: Duration = Duration::from_secs(45);
/// The first rate-limit backoff, doubled on each further limit up to MAX_BACKOFF.
const MIN_BACKOFF: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);
/// The most keys the daemon keeps; past it a new key is refused and the command runs gh itself.
const MAX_KEYS: usize = 200;
/// The longest single argument and the longest argv, in bytes, the daemon runs gh with. A longer
/// request is refused and the command runs gh itself.
const MAX_ARG: usize = 4096;
const MAX_ARGV: usize = 8192;
/// The most gh processes the daemon runs at once; a read that would start another is refused and
/// the command runs gh itself.
const MAX_GH: usize = 4;
/// Why a read that would start a gh process past MAX_GH gets no answer. The daemon replies
/// `{"busy": true}`, and the command runs that one call itself.
const BUSY: &str = "the daemon already runs as many gh calls as it allows";
/// The longest single gh-wait; a longer sleep waits again.
const MAX_WAIT: f64 = 50.0;
/// How young a cached answer must be for a command's first read of a key.
const FIRST_READ: f64 = 5.0;
/// The age a command accepts on later reads of a key it already holds: whatever the poller keeps,
/// up to three minutes.
/// Capped, so answers stay bounded in age even if the poll thread stops.
const TRUSTED: f64 = 180.0;
/// How long a command waits for a gh reply: another caller's gh call may run first.
const GH_REPLY: Duration = Duration::from_secs(150);
/// Credentials and targets in a command's own environment that the daemon's gh would not see.
const OWN_GH_ENV: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GH_HOST",
    "GH_REPO",
    "GH_CONFIG_DIR",
    "GH_DEBUG",
    "GH_FORCE_TTY",
    "CLICOLOR_FORCE",
];
/// The `--jq` filters the wait commands send. Any other filter could read the daemon's
/// environment (`env`, `$ENV`), so it is refused.
const JQ_FILTERS: &[&str] = &[
    ".state",
    ".behind_by",
    ".default_branch",
    ".data.repository.object",
    ".data.repository.pullRequest",
];

type Key = Vec<String>;
pub(crate) type RunGh = Box<dyn Fn(&[&str]) -> Option<GhOut> + Send + Sync>;

/// True for the read-only gh calls the wait commands make, which the daemon may run and cache.
/// Anything that writes (update-branch, merge, a mutation, an api call with fields) is refused,
/// and a GraphQL call must carry one of the wait commands' own queries.
pub(crate) fn cacheable(args: &[&str]) -> bool {
    let pairs = |rest: &[&str], flags: &[&str]| {
        rest.len() % 2 == 0
            && rest
                .chunks(2)
                .all(|p| flags.contains(&p[0]) && (p[0] != "--jq" || JQ_FILTERS.contains(&p[1])))
    };
    let pr_url = |u: &str| u.starts_with("https://github.com/");
    match args {
        ["pr", "view", url, rest @ ..] => pr_url(url) && pairs(rest, &["--json", "--jq"]),
        ["pr", "checks", url, "--required", rest @ ..] | ["pr", "checks", url, rest @ ..] => {
            pr_url(url) && pairs(rest, &["--json"])
        }
        ["api", "graphql", rest @ ..] => {
            let fields = || rest.chunks(2).filter(|p| p[0] != "--jq").map(|p| p[1]);
            pairs(rest, &["-f", "-F", "--jq"])
                && fields().filter(|f| f.starts_with("query=")).count() == 1
                && fields().all(|f| {
                    [HEAD_CHECKS_QUERY, PR_GRAPHQL_QUERY].contains(&f)
                        || ["o=", "r=", "n=", "sha="].iter().any(|n| {
                            f.strip_prefix(n).is_some_and(|v| {
                                !v.is_empty()
                                    && v.bytes()
                                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                            })
                        })
                })
        }
        // gh fills `{owner}` and `:owner` placeholders from the daemon's working directory.
        ["api", path, "--jq", jq] => {
            path.starts_with("repos/") && !path.contains(['{', ':']) && JQ_FILTERS.contains(jq)
        }
        _ => false,
    }
}

/// A rate limit: GitHub refused the call, so the poller backs off every key.
fn rate_limited(out: &GhOut) -> bool {
    let err = out.stderr.to_lowercase();
    err.contains("rate limit") || err.contains("http 429")
}

/// A rate limit or timeout says nothing about the PR, so it never replaces an answer that does.
fn throttled(out: &GhOut) -> bool {
    matches!(out.code, GhCode::Timeout) || rate_limited(out)
}

/// True unless an answer shows settled work: checks or a review still running, or no checks
/// listed yet, keep a key at its commands' own cadence.
fn pending(out: &GhOut) -> bool {
    let compact: String = out.stdout.chars().filter(|c| !c.is_whitespace()).collect();
    matches!(out.code, GhCode::Exit(8))
        || compact == "[]"
        || compact.is_empty()
        || out.stderr.contains("no checks reported")
        || out.stderr.contains("no required checks reported")
        || [
            "IN_PROGRESS",
            "QUEUED",
            "PENDING",
            "\"pending\"",
            "EXPECTED",
            "WAITING",
            "REQUESTED",
            "\"statusCheckRollup\":null",
            "\"statusCheckRollup\":[]",
            "\"statusCheckRollup\":{\"contexts\":null",
            "\"nodes\":[]",
            "\"object\":null",
        ]
        .iter()
        .any(|w| compact.contains(w))
}

fn same(a: &GhOut, b: &GhOut) -> bool {
    let code = |o: &GhOut| match o.code {
        GhCode::Exit(c) => Some(c),
        GhCode::Timeout => None,
    };
    code(a) == code(b) && a.stdout == b.stdout && a.stderr == b.stderr
}

/// True when an environment, read through `set`, holds a gh credential or setting the other side
/// of the socket might not share.
fn own_gh_env(set: &dyn Fn(&str) -> bool) -> bool {
    OWN_GH_ENV.iter().any(|k| set(k))
}

/// The environment that picks gh's account and config. A command whose values differ from the
/// daemon's runs gh itself.
fn gh_env() -> Value {
    let var = |k: &str| std::env::var(k).ok().map_or(Value::Null, Value::from);
    json!({"HOME": var("HOME"), "XDG_CONFIG_HOME": var("XDG_CONFIG_HOME")})
}

struct Entry {
    answer: GhOut,
    version: u64,
    fetched: Instant,
    read: Instant,
    /// The fastest cadence a command holding this key asked for.
    every: Duration,
    pending: bool,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<Key, Entry>,
    /// Keys a gh call is running for, so callers that want the same key share one call.
    inflight: HashSet<Key>,
    versions: u64,
    backoff: Duration,
    backoff_until: Option<Instant>,
    /// GitHub's last rate-limit answer, which a read of a key with no cached answer gets during
    /// the backoff.
    limit: Option<GhOut>,
}

pub struct Poller {
    state: Mutex<Cache>,
    moved: Condvar,
    gh: RunGh,
    /// Whether the daemon's environment sets a variable.
    env: fn(&str) -> bool,
}

/// A key's place among the gh calls in flight, freed if its gh call panics.
/// The release and host profiles set `panic = "abort"`, so only the tests' unwinding builds run this drop.
struct Slot<'a> {
    poller: &'a Poller,
    key: &'a Key,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if let Ok(mut s) = self.poller.state.lock() {
            s.inflight.remove(self.key);
            self.poller.moved.notify_all();
        }
    }
}

static POLLER: OnceLock<Arc<Poller>> = OnceLock::new();
/// The daemon's log, and whether it already holds the line about the daemon's own gh environment.
type Log = Box<dyn Fn(&str) + Send + Sync>;
static LOG: OnceLock<Log> = OnceLock::new();
static WARNED: AtomicBool = AtomicBool::new(false);

impl Poller {
    pub fn new(gh: RunGh) -> Arc<Poller> {
        Arc::new(Poller {
            state: Mutex::new(Cache::default()),
            moved: Condvar::new(),
            gh,
            env: |k| std::env::var_os(k).is_some(),
        })
    }

    /// How often key is polled: its commands' cadence while pending, else at most once a minute.
    fn interval(e: &Entry) -> Duration {
        if e.pending {
            e.every
        } else {
            e.every.max(SLOW)
        }
    }

    /// Runs gh for key, which the caller marked in flight, and stores the answer unless it is a
    /// rate limit or timeout and a real answer is cached. Returns gh's answer, as a direct call
    /// would, with the version the cache holds for key (0 when it holds none).
    fn fetch(&self, key: &Key, every: Duration) -> Option<(GhOut, u64)> {
        let args: Vec<&str> = key.iter().map(String::as_str).collect();
        let slot = Slot { poller: self, key };
        let out = (self.gh)(&args);
        // Freed below, under the same lock that stores the answer.
        std::mem::forget(slot);
        let now = Instant::now();
        let mut s = self.state.lock().unwrap();
        s.inflight.remove(key);
        self.moved.notify_all();
        let out = out?;
        if rate_limited(&out) {
            s.backoff = (s.backoff * 2).clamp(MIN_BACKOFF, MAX_BACKOFF);
            s.backoff_until = Some(now + s.backoff);
            s.limit = Some(out.clone());
        } else if !throttled(&out) {
            s.backoff = Duration::ZERO;
            s.backoff_until = None;
        }
        let next = s.versions + 1;
        let full = s.entries.len() >= MAX_KEYS;
        let version = match s.entries.get_mut(key) {
            // A rate limit or timeout leaves the real answer, and its age, as they were.
            Some(e) if throttled(&out) && !throttled(&e.answer) => e.version,
            Some(e) => {
                e.fetched = now;
                if !same(&e.answer, &out) {
                    e.pending = pending(&out);
                    e.answer = out.clone();
                    e.version = next;
                }
                e.version
            }
            None if full => 0,
            None => {
                s.entries.insert(
                    key.clone(),
                    Entry {
                        pending: pending(&out),
                        answer: out.clone(),
                        version: next,
                        fetched: now,
                        read: now,
                        every,
                    },
                );
                next
            }
        };
        if version == next {
            s.versions = next;
        }
        Some((out, version))
    }

    /// The cached answer to key when it is at most max_age old, else a fresh one. While a rate
    /// limit holds, no read calls GitHub: a cached answer serves whatever its age, and a key with
    /// none gets the rate-limit answer, kept as version 0. Err is why the daemon can't answer.
    fn read(
        &self,
        key: &Key,
        max_age: Duration,
        every: Duration,
    ) -> Result<(GhOut, u64), &'static str> {
        let mut s = self.state.lock().unwrap();
        loop {
            let limited = s.backoff_until.is_some_and(|t| Instant::now() < t);
            if let Some(e) = s.entries.get_mut(key) {
                e.read = Instant::now();
                e.every = e.every.min(every);
                if limited || e.fetched.elapsed() <= max_age {
                    return Ok((e.answer.clone(), e.version));
                }
            }
            if let (true, Some(out)) = (limited, &s.limit) {
                return Ok((out.clone(), 0));
            }
            if !s.inflight.contains(key) {
                break;
            }
            // Another caller's gh call for this key is running; share its answer.
            s = self.moved.wait(s).unwrap();
        }
        if s.inflight.len() >= MAX_GH {
            return Err(BUSY);
        }
        s.inflight.insert(key.clone());
        drop(s);
        self.fetch(key, every).ok_or("gh is not installed")
    }

    /// Blocks until one of keys holds a version other than the one given, or wait passes.
    /// Returns whether one changed, and the keys the cache no longer holds.
    fn wait(&self, keys: &[(Key, u64)], wait: Duration) -> (bool, Vec<Key>) {
        let end = Instant::now() + wait;
        let mut s = self.state.lock().unwrap();
        loop {
            let now = Instant::now();
            let (mut changed, mut missing) = (false, Vec::new());
            for (key, version) in keys {
                match s.entries.get_mut(key) {
                    Some(e) => {
                        e.read = now;
                        changed |= e.version != *version;
                    }
                    None => missing.push(key.clone()),
                }
            }
            if changed || !missing.is_empty() || now >= end {
                return (changed, missing);
            }
            // Wake at least every 10 s to renew the keys' lease while waiting.
            let nap = (end - now).min(Duration::from_secs(10));
            s = self.moved.wait_timeout(s, nap).unwrap().0;
        }
    }

    /// One pass of the poll loop: drops keys no command holds any more and refreshes those due,
    /// unless GitHub rate-limited the last call. It stops early once stop is true.
    fn tick(&self, stop: &dyn Fn() -> bool) {
        let due: Vec<Key> = {
            let mut s = self.state.lock().unwrap();
            s.entries.retain(|_, e| e.read.elapsed() <= LEASE);
            s.entries
                .iter()
                .filter(|(_, e)| e.fetched.elapsed() >= Self::interval(e))
                .map(|(k, _)| k.clone())
                .collect()
        };
        for key in due {
            if stop() {
                return;
            }
            let every = {
                let mut s = self.state.lock().unwrap();
                if s.backoff_until.is_some_and(|t| Instant::now() < t) {
                    return;
                }
                let due = s
                    .entries
                    .get(&key)
                    .filter(|e| e.fetched.elapsed() >= Self::interval(e))
                    .map(|e| e.every);
                match due {
                    Some(every) if !s.inflight.contains(&key) && s.inflight.len() < MAX_GH => {
                        s.inflight.insert(key.clone());
                        every
                    }
                    _ => continue,
                }
            };
            self.fetch(&key, every);
        }
    }

    /// The reply to a `gh` or `gh-wait` request.
    pub fn answer(&self, request: &Value) -> Value {
        let refused = |error: &str| json!({"v": PROTOCOL, "ok": false, "error": error});
        let secs = |v: &Value, max: f64| {
            Duration::from_secs_f64(v.as_f64().unwrap_or(0.0).clamp(0.0, max))
        };
        let key_of = |v: &Value| -> Option<Key> {
            v.as_array()?
                .iter()
                .map(|a| a.as_str().map(String::from))
                .collect()
        };
        match request["op"].as_str() {
            Some("gh") => {
                let Some(key) = key_of(&request["args"]) else {
                    return refused("gh needs args");
                };
                if key.iter().any(|a| a.len() > MAX_ARG)
                    || key.iter().map(String::len).sum::<usize>() > MAX_ARGV
                {
                    return refused("gh args too long");
                }
                let args: Vec<&str> = key.iter().map(String::as_str).collect();
                if !cacheable(&args) {
                    return refused("not a cached gh read");
                }
                // A daemon with its own gh identity would answer as another account.
                if own_gh_env(&self.env) {
                    if !WARNED.swap(true, Ordering::Relaxed) {
                        if let Some(log) = LOG.get() {
                            log("gh poller: not proxying, the daemon's environment sets a gh credential or setting; commands run gh themselves");
                        }
                    }
                    return refused("the daemon has its own gh credentials or config");
                }
                if request["env"] != gh_env() {
                    return refused("gh runs under another HOME or config");
                }
                let every = secs(&request["every"], 3600.0).max(FASTEST);
                match self.read(&key, secs(&request["max_age"], TRUSTED), every) {
                    Err(BUSY) => json!({"v": PROTOCOL, "ok": true, "busy": true}),
                    Err(why) => refused(why),
                    Ok((out, version)) => {
                        let code = match out.code {
                            GhCode::Exit(c) => json!(c),
                            GhCode::Timeout => json!("timeout"),
                        };
                        json!({"v": PROTOCOL, "ok": true, "version": version, "code": code,
                               "stdout": out.stdout, "stderr": out.stderr})
                    }
                }
            }
            Some("gh-wait") => {
                let keys: Option<Vec<(Key, u64)>> = request["keys"].as_array().and_then(|ks| {
                    ks.iter()
                        .map(|k| Some((key_of(&k["args"])?, k["version"].as_u64()?)))
                        .collect()
                });
                let Some(keys) = keys else {
                    return refused("gh-wait needs keys");
                };
                let (changed, missing) = self.wait(&keys, secs(&request["wait"], MAX_WAIT));
                json!({"v": PROTOCOL, "ok": true, "changed": changed, "missing": missing})
            }
            _ => refused("unknown op"),
        }
    }
}

/// The reply to a gh request on the daemon's socket, or a refusal when no poller runs.
pub fn answer(request: &Value) -> Value {
    match POLLER.get() {
        Some(p) => p.answer(request),
        None => json!({"v": PROTOCOL, "ok": false, "error": "no gh poller"}),
    }
}

/// Starts the daemon's poll loop on its own thread. It returns once stop reports a signal.
pub fn start(stop: fn() -> Option<&'static str>, log: Log) -> std::thread::JoinHandle<()> {
    let _ = LOG.set(log);
    let poller = POLLER
        .get_or_init(|| Poller::new(Box::new(|args| System::new().gh(args))))
        .clone();
    std::thread::spawn(move || {
        while stop().is_none() {
            poller.tick(&|| stop().is_some());
            std::thread::sleep(Duration::from_secs(1));
        }
    })
}

/// The gh host for a waiting command: reads go through the daemon when it can answer them, and
/// straight to gh otherwise.
pub(crate) struct Cached {
    home: PathBuf,
    direct: Box<dyn Host>,
    /// False once the daemon can't answer, so the rest of the run goes straight to gh.
    live: bool,
    every: f64,
    /// Each key read through the daemon and the version of its last answer.
    held: HashMap<Key, u64>,
    /// The keys read since the last sleep. Only these are waited on, so a key the command read
    /// once, like checks' first state read, leaves the daemon's poll list.
    read: HashSet<Key>,
    /// Set once the command ran a gh call that may write, like update-branch. From then on the
    /// first read of each key asks the daemon for a fresh answer.
    wrote: bool,
}

impl Cached {
    /// A host for a command that re-reads the PR every `every` seconds.
    pub(crate) fn new(home: &Path, every: f64) -> Cached {
        let own_env = own_gh_env(&|k| std::env::var_os(k).is_some());
        Cached::with(home, every, Box::new(System::new()), !own_env)
    }

    fn with(home: &Path, every: f64, direct: Box<dyn Host>, live: bool) -> Cached {
        Cached {
            home: home.to_path_buf(),
            direct,
            live,
            every,
            held: HashMap::new(),
            read: HashSet::new(),
            wrote: false,
        }
    }

    /// The daemon's answer to one gh read: None when it can't answer, Some(None) when it is busy,
    /// so this one call runs gh itself.
    fn ask_gh(&mut self, args: &[&str]) -> Option<Option<GhOut>> {
        let key: Key = args.iter().map(|a| a.to_string()).collect();
        let max_age = if self.held.contains_key(&key) {
            TRUSTED
        } else if self.wrote {
            0.0
        } else {
            FIRST_READ
        };
        let mut request = Map::new();
        request.insert("op".into(), "gh".into());
        request.insert("args".into(), json!(key));
        request.insert("max_age".into(), json!(max_age));
        request.insert("every".into(), json!(self.every));
        request.insert("env".into(), gh_env());
        let (reply, _) = ask(&self.home, request, GH_REPLY).ok()?;
        if reply["busy"] == true {
            return Some(None);
        }
        let code = match &reply["code"] {
            Value::String(s) if s == "timeout" => GhCode::Timeout,
            c => GhCode::Exit(i32::try_from(c.as_i64()?).ok()?),
        };
        let out = GhOut {
            code,
            stdout: reply["stdout"].as_str()?.to_string(),
            stderr: reply["stderr"].as_str()?.to_string(),
        };
        // Version 0 is an answer the daemon could not keep; the next read asks again.
        match reply["version"].as_u64()? {
            0 => self.held.remove(&key),
            v => {
                self.read.insert(key.clone());
                self.held.insert(key, v)
            }
        };
        Some(Some(out))
    }
}

impl Host for Cached {
    fn gh(&mut self, args: &[&str]) -> Option<GhOut> {
        if self.live && cacheable(args) {
            // Without gh on this command's PATH the answer is "not installed", as without a daemon.
            which("gh")?;
            match self.ask_gh(args) {
                Some(Some(out)) => return Some(out),
                Some(None) => {}
                None => self.live = false,
            }
        } else if !cacheable(args) {
            // A write may have moved the PR, so nothing read before it is trusted after it.
            self.wrote = true;
            self.held.clear();
            self.read.clear();
        }
        self.direct.gh(args)
    }

    fn monotonic(&mut self) -> f64 {
        self.direct.monotonic()
    }

    fn sleep(&mut self, secs: f64) {
        let end = self.direct.monotonic() + secs;
        let read = std::mem::take(&mut self.read);
        self.held.retain(|k, _| read.contains(k));
        while self.live && !self.held.is_empty() {
            let left = end - self.direct.monotonic();
            if left <= 0.0 {
                return;
            }
            let wait = left.min(MAX_WAIT);
            let keys: Vec<Value> = self
                .held
                .iter()
                .map(|(k, v)| json!({"args": k, "version": v}))
                .collect();
            let mut request = Map::new();
            request.insert("op".into(), "gh-wait".into());
            request.insert("keys".into(), Value::Array(keys));
            request.insert("wait".into(), json!(wait));
            let limit = Duration::from_secs_f64(wait + 15.0);
            match ask(&self.home, request, limit) {
                Ok((reply, _)) => {
                    // A key the daemon dropped is read afresh next time, not waited on.
                    for k in reply["missing"].as_array().into_iter().flatten() {
                        if let Ok(k) = serde_json::from_value::<Key>(k.clone()) {
                            self.held.remove(&k);
                        }
                    }
                    if reply["changed"] == true
                        || reply["missing"].as_array().is_some_and(|m| !m.is_empty())
                    {
                        return;
                    }
                }
                Err(_) => self.live = false,
            }
        }
        let left = end - self.direct.monotonic();
        if left > 0.0 {
            self.direct.sleep(left);
        }
    }

    fn unix_time(&mut self) -> i64 {
        self.direct.unix_time()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const PR: &str = "https://github.com/example/sample/pull/42";

    fn out(code: i32, stdout: &str, stderr: &str) -> GhOut {
        GhOut {
            code: GhCode::Exit(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    /// A poller whose gh answers from `answers` in turn, repeating the last, and counts calls.
    fn poller(answers: Vec<GhOut>) -> (Arc<Poller>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let gh: RunGh = Box::new(move |_| {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            Some(answers[n.min(answers.len() - 1)].clone())
        });
        (Poller::new(gh), calls)
    }

    fn key(args: &[&str]) -> Key {
        args.iter().map(|a| a.to_string()).collect()
    }

    fn view() -> Key {
        key(&["pr", "view", PR, "--json", "state", "--jq", ".state"])
    }

    /// Ages key's cached answer by secs.
    fn age(p: &Poller, k: &Key, secs: u64) {
        let mut s = p.state.lock().unwrap();
        let e = s.entries.get_mut(k).unwrap();
        e.fetched -= Duration::from_secs(secs);
    }

    #[test]
    fn cacheable_takes_the_wait_commands_reads_only() {
        let head = HEAD_CHECKS_QUERY;
        let pull = PR_GRAPHQL_QUERY;
        for args in [
            vec!["pr", "view", PR, "--json", "state", "--jq", ".state"],
            vec!["pr", "view", PR, "--json", "headRefOid,baseRefName"],
            vec![
                "pr",
                "checks",
                PR,
                "--required",
                "--json",
                "name,bucket,link",
            ],
            vec!["pr", "checks", PR, "--json", "name,bucket,link"],
            vec![
                "api",
                "graphql",
                "-f",
                head,
                "-f",
                "o=a",
                "-f",
                "r=b",
                "-f",
                "sha=c",
                "--jq",
                ".data.repository.object",
            ],
            vec![
                "api",
                "graphql",
                "-f",
                pull,
                "-f",
                "o=a",
                "-f",
                "r=b",
                "-F",
                "n=1",
                "--jq",
                ".data.repository.pullRequest",
            ],
            vec!["api", "repos/a/b/compare/main...abc", "--jq", ".behind_by"],
            vec!["api", "repos/a/b", "--jq", ".default_branch"],
        ] {
            assert!(cacheable(&args), "{args:?}");
        }
        for args in [
            vec!["pr", "update-branch", PR],
            vec!["pr", "merge", PR, "--squash"],
            vec!["pr", "view", "123", "--json", "state"],
            vec!["pr", "view", PR, "--web"],
            vec![
                "api",
                "graphql",
                "-f",
                "query=mutation($id:ID!){x}",
                "-f",
                "id=1",
            ],
            vec!["api", "graphql", "-f", head, "-f", "query=mutation{x}"],
            vec![
                "api",
                "graphql",
                "-f",
                "query=query($x:ID){a} mutation M {b}",
                "-f",
                "operationName=M",
            ],
            vec!["api", "graphql", "-f", head, "-F", "query=@mut.graphql"],
            vec!["api", "graphql", "-f", head, "-F", "n=@file"],
            vec!["api", "graphql", "-f", " query=mutation{x}"],
            vec!["api", "graphql", "-f", head, "-F", "o=:owner", "-f", "r=b"],
            vec!["api", "graphql", "-f", head, "-F", "o={owner}", "-f", "r=b"],
            vec!["api", "graphql", "-f", head, "-F", "o=x:owner", "-f", "r=b"],
            vec!["api", "repos/{owner}/{repo}", "--jq", ".default_branch"],
            vec!["api", "repos/:owner/:repo", "--jq", ".default_branch"],
            vec!["api", "repos/cli/cli", "--jq", "env|keys"],
            vec!["api", "repos/cli/cli", "--jq", "$ENV.HOME"],
            vec!["pr", "view", PR, "--json", "state", "--jq", "env"],
            vec!["api", "graphql", "-f", head, "-f", "o=a", "--jq", "input"],
            vec!["api", "repos/a/b/merges", "-f", "base=main"],
            vec!["api", "-X", "PUT", "repos/a/b"],
            vec!["api", "user", "--jq", ".login"],
        ] {
            assert!(!cacheable(&args), "{args:?}");
        }
    }

    #[test]
    fn callers_share_one_gh_call_while_the_answer_is_young() {
        let (p, calls) = poller(vec![out(0, "OPEN\n", "")]);
        let k = view();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let (p, k) = (p.clone(), k.clone());
                std::thread::spawn(move || p.read(&k, Duration::from_secs(5), FASTEST))
            })
            .collect();
        for t in threads {
            let (o, v) = t.join().unwrap().unwrap();
            assert_eq!((o.stdout.as_str(), v), ("OPEN\n", 1));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        age(&p, &k, 6);
        p.read(&k, Duration::from_secs(5), FASTEST).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn answers_pass_through_with_their_exit_code_and_streams() {
        // Other tests swap HOME, which gh_env compares on both sides.
        let _env = crate::shared::test_env_lock();
        let (p, _) = poller(vec![GhOut {
            code: GhCode::Timeout,
            stdout: String::new(),
            stderr: String::new(),
        }]);
        let reply = p.answer(&json!({"op": "gh", "args": view(), "max_age": 0, "env": gh_env()}));
        assert_eq!(reply["code"], "timeout");
        let (p, _) = poller(vec![out(8, "[]", "some checks pending\n")]);
        let args = key(&[
            "pr",
            "checks",
            PR,
            "--required",
            "--json",
            "name,bucket,link",
        ]);
        let reply = p.answer(&json!({"op": "gh", "args": args, "max_age": 0, "env": gh_env()}));
        assert_eq!(
            (
                reply["ok"].clone(),
                reply["code"].clone(),
                reply["stdout"].clone(),
                reply["stderr"].clone()
            ),
            (
                json!(true),
                json!(8),
                json!("[]"),
                json!("some checks pending\n")
            )
        );
        let reply = p.answer(&json!({"op": "gh", "args": ["pr", "update-branch", PR]}));
        assert_eq!(reply["ok"], false);
        let none: RunGh = Box::new(|_| None);
        let reply = Poller::new(none)
            .answer(&json!({"op": "gh", "args": view(), "max_age": 0, "env": gh_env()}));
        assert_eq!(reply["error"], "gh is not installed");
    }

    #[test]
    fn a_tick_polls_pending_keys_at_their_cadence_and_others_once_a_minute() {
        let (p, calls) = poller(vec![out(0, "{\"status\":\"IN_PROGRESS\"}", "")]);
        let k = view();
        p.read(&k, Duration::ZERO, Duration::from_secs(10)).unwrap();
        p.tick(&|| false);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "not due yet");
        age(&p, &k, 10);
        p.tick(&|| false);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "pending: every 10 s");

        let (p, calls) = poller(vec![out(0, "OPEN\n", "")]);
        p.read(&k, Duration::ZERO, Duration::from_secs(10)).unwrap();
        age(&p, &k, 30);
        p.tick(&|| false);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "settled: not before a minute"
        );
        age(&p, &k, 30);
        p.tick(&|| false);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_rate_limit_keeps_the_last_answer_and_backs_off() {
        let limit = out(1, "", "gh: API rate limit exceeded for user\n");
        let (p, calls) = poller(vec![out(0, "OPEN\n", ""), limit, out(0, "MERGED\n", "")]);
        let k = view();
        p.read(&k, Duration::ZERO, FASTEST).unwrap();
        age(&p, &k, 60);
        p.tick(&|| false);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let (o, v) = p
            .read(&k, Duration::from_secs(TRUSTED as u64), FASTEST)
            .unwrap();
        assert_eq!((o.stdout.as_str(), v), ("OPEN\n", 1));
        age(&p, &k, 60);
        p.tick(&|| false);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "backed off");
        p.state.lock().unwrap().backoff_until = Some(Instant::now());
        p.tick(&|| false);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let (o, v) = p
            .read(&k, Duration::from_secs(TRUSTED as u64), FASTEST)
            .unwrap();
        assert_eq!((o.stdout.as_str(), v), ("MERGED\n", 2));
    }

    #[test]
    fn keys_no_command_holds_drop_off_the_poll_list() {
        let (p, calls) = poller(vec![out(0, "OPEN\n", "")]);
        let k = view();
        p.read(&k, Duration::ZERO, FASTEST).unwrap();
        p.state.lock().unwrap().entries.get_mut(&k).unwrap().read -= LEASE + Duration::from_secs(1);
        age(&p, &k, 120);
        p.tick(&|| false);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(p.state.lock().unwrap().entries.is_empty());
    }

    #[test]
    fn a_wait_ends_when_a_held_key_changes() {
        let (p, _) = poller(vec![out(0, "OPEN\n", ""), out(0, "MERGED\n", "")]);
        let k = view();
        let (_, v) = p.read(&k, Duration::ZERO, FASTEST).unwrap();
        let start = Instant::now();
        assert!(!p.wait(&[(k.clone(), v)], Duration::from_millis(200)).0);
        assert!(start.elapsed() >= Duration::from_millis(200));
        let waiter = {
            let (p, k) = (p.clone(), k.clone());
            std::thread::spawn(move || p.wait(&[(k, v)], Duration::from_secs(30)).0)
        };
        std::thread::sleep(Duration::from_millis(100));
        age(&p, &k, 60);
        p.tick(&|| false);
        assert!(waiter.join().unwrap());
    }

    #[test]
    fn no_checks_yet_counts_as_pending() {
        for (code, stdout, stderr) in [
            (0, "{\"statusCheckRollup\": null}", ""),
            (
                0,
                "{\"statusCheckRollup\":{\"contexts\":{\"nodes\":[]}}}",
                "",
            ),
            (0, "[]", ""),
            (1, "", "no checks reported on the 'x' branch"),
            (0, "{\"object\": null}", ""),
        ] {
            assert!(pending(&out(code, stdout, stderr)), "{stdout} {stderr}");
        }
        assert!(!pending(&out(0, "OPEN\n", "")));
        assert!(!pending(&out(0, "[{\"bucket\":\"pass\"}]", "")));
    }

    #[test]
    fn a_timeout_keeps_the_answer_without_backing_off() {
        let timeout = GhOut {
            code: GhCode::Timeout,
            stdout: String::new(),
            stderr: String::new(),
        };
        let (p, calls) = poller(vec![out(0, "OPEN\n", ""), timeout, out(0, "MERGED\n", "")]);
        let k = view();
        p.read(&k, Duration::ZERO, FASTEST).unwrap();
        age(&p, &k, 60);
        p.tick(&|| false);
        assert!(p.state.lock().unwrap().backoff_until.is_none());
        // The kept answer stays old, so the next tick asks again.
        p.tick(&|| false);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let (o, _) = p
            .read(&k, Duration::from_secs(TRUSTED as u64), FASTEST)
            .unwrap();
        assert_eq!(o.stdout, "MERGED\n");
    }

    #[test]
    fn a_wait_reports_keys_the_cache_dropped() {
        let (p, _) = poller(vec![out(0, "OPEN\n", "")]);
        let k = view();
        let start = Instant::now();
        assert_eq!(
            p.wait(&[(k.clone(), 1)], Duration::from_secs(30)),
            (false, vec![k])
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_tick_stops_when_asked() {
        let (p, calls) = poller(vec![out(0, "OPEN\n", "")]);
        let k = view();
        p.read(&k, Duration::ZERO, FASTEST).unwrap();
        age(&p, &k, 60);
        p.tick(&|| true);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn gh_credentials_on_either_side_mean_direct_gh() {
        for k in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GH_HOST",
            "GH_REPO",
            "GH_CONFIG_DIR",
            "GH_FORCE_TTY",
            "CLICOLOR_FORCE",
        ] {
            assert!(own_gh_env(&|v| v == k), "{k}");
        }
        assert!(!own_gh_env(&|v| v == "PATH"));
    }

    /// A daemon whose own environment sets a gh credential or setting refuses every read without
    /// running gh, so the command runs gh under its own identity.
    #[test]
    fn a_daemon_with_its_own_gh_identity_refuses() {
        let (mut p, calls) = poller(vec![out(0, "OPEN\n", "")]);
        Arc::get_mut(&mut p).unwrap().env = |k| k == "GH_TOKEN";
        let reply = p.answer(&json!({"op": "gh", "args": view(), "env": gh_env()}));
        assert_eq!(
            (reply["ok"].clone(), reply["error"].clone()),
            (
                json!(false),
                json!("the daemon has its own gh credentials or config")
            )
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// A request whose argv is over the size caps is refused before gh runs.
    #[test]
    fn an_oversized_argv_is_refused() {
        let (p, calls) = poller(vec![out(0, "OPEN\n", "")]);
        let long_url = format!("https://github.com/{}", "a".repeat(MAX_ARG));
        let near = |n: usize| format!("https://github.com/{}", "a".repeat(n));
        for args in [
            json!(["pr", "view", long_url, "--json", "state"]),
            json!([
                "pr",
                "view",
                near(4070),
                "--json",
                "s".repeat(4090),
                "--jq",
                ".state"
            ]),
        ] {
            let reply = p.answer(&json!({"op": "gh", "args": args, "env": gh_env()}));
            assert_eq!(reply["error"], "gh args too long");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// At most MAX_GH gh calls run at once: a read that would start another is refused until one
    /// ends.
    #[test]
    fn gh_calls_past_the_cap_are_refused() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let (held, seen) = (gate.clone(), calls.clone());
        let gh: RunGh = Box::new(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            let (open, moved) = &*held;
            let open = open.lock().unwrap();
            let _ = moved.wait_timeout_while(open, Duration::from_secs(10), |o| !*o);
            Some(out(0, "OPEN\n", ""))
        });
        let p = Poller::new(gh);
        let pr = |n: usize| {
            key(&[
                "pr",
                "view",
                &format!("https://github.com/example/sample/pull/{n}"),
                "--json",
                "state",
            ])
        };
        let readers: Vec<_> = (0..MAX_GH)
            .map(|n| {
                let (p, k) = (p.clone(), pr(n));
                std::thread::spawn(move || p.read(&k, Duration::ZERO, FASTEST))
            })
            .collect();
        let start = Instant::now();
        while calls.load(Ordering::SeqCst) < MAX_GH {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "the first calls never started"
            );
            std::thread::yield_now();
        }
        assert_eq!(
            p.read(&pr(MAX_GH), Duration::ZERO, FASTEST).err(),
            Some(BUSY)
        );
        assert_eq!(calls.load(Ordering::SeqCst), MAX_GH);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        for r in readers {
            assert_eq!(r.join().unwrap().unwrap().0.stdout, "OPEN\n");
        }
        p.read(&pr(MAX_GH), Duration::ZERO, FASTEST).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), MAX_GH + 1);
    }

    /// A gh call that panics frees its place, so panics never use up the cap.
    #[test]
    fn a_panicking_gh_call_frees_its_slot() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let gh: RunGh = Box::new(move |_| {
            if seen.fetch_add(1, Ordering::SeqCst) < MAX_GH {
                panic!("synthetic gh failure");
            }
            Some(out(0, "OPEN\n", ""))
        });
        let p = Poller::new(gh);
        let pr = |n: usize| {
            key(&[
                "pr",
                "view",
                &format!("https://github.com/example/sample/pull/{n}"),
                "--json",
                "state",
            ])
        };
        for n in 0..MAX_GH {
            let (p, k) = (p.clone(), pr(n));
            let read = std::thread::spawn(move || p.read(&k, Duration::ZERO, FASTEST).is_ok());
            assert!(read.join().is_err());
        }
        assert!(p.state.lock().unwrap().inflight.is_empty());
        let (o, _) = p.read(&pr(MAX_GH), Duration::ZERO, FASTEST).unwrap();
        assert_eq!(o.stdout, "OPEN\n");
    }

    /// A busy daemon sends that one read to gh directly; the command keeps reading through the
    /// daemon afterwards.
    #[cfg(unix)]
    #[test]
    fn a_busy_reply_runs_one_call_directly() {
        use crate::shared::board_client::socket_path;
        use std::io::{BufRead, BufReader, Write};
        if which("gh").is_none() {
            return;
        }
        let home = crate::shared::fs::temp_dir("ghpoll-busy");
        let listener = std::os::unix::net::UnixListener::bind(socket_path(&home)).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let reply = json!({"v": PROTOCOL, "ok": true, "busy": true});
                let _ = reader.get_mut().write_all(format!("{reply}\n").as_bytes());
            }
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let direct = Box::new(Direct {
            calls: calls.clone(),
            clock: 0.0,
        });
        let mut h = Cached::with(&home, 10.0, direct, true);
        let args = ["pr", "view", PR, "--json", "state", "--jq", ".state"];
        assert_eq!(h.gh(&args).unwrap().stdout, "DIRECT\n");
        assert!(h.live, "a busy daemon is asked again next time");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(home).unwrap();
    }

    /// During a rate-limit backoff no read calls GitHub: a held key gets its cached answer even
    /// when it asks for none older than 0 s, and a new key gets the rate-limit answer, unkept.
    #[test]
    fn reads_during_a_backoff_never_call_github() {
        let limit = out(1, "", "gh: API rate limit exceeded for user\n");
        let (p, calls) = poller(vec![out(0, "OPEN\n", ""), limit.clone()]);
        let k = view();
        p.read(&k, Duration::ZERO, FASTEST).unwrap();
        age(&p, &k, 60);
        p.tick(&|| false);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let (o, v) = p.read(&k, Duration::ZERO, FASTEST).unwrap();
        assert_eq!((o.stdout.as_str(), v), ("OPEN\n", 1));
        let other = key(&["pr", "view", PR, "--json", "headRefOid"]);
        let (o, v) = p.read(&other, Duration::ZERO, FASTEST).unwrap();
        assert_eq!((o.stderr, v), (limit.stderr, 0));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // Once the backoff ends, reads call GitHub again.
        p.state.lock().unwrap().backoff_until = Some(Instant::now());
        p.read(&other, Duration::ZERO, FASTEST).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn another_home_runs_gh_itself() {
        let (p, calls) = poller(vec![out(0, "OPEN\n", "")]);
        let env = json!({"HOME": "/elsewhere", "XDG_CONFIG_HOME": null});
        let reply = p.answer(&json!({"op": "gh", "args": view(), "env": env}));
        assert_eq!(reply["ok"], false);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// A direct host that counts gh calls and keeps a fake clock.
    struct Direct {
        calls: Arc<AtomicUsize>,
        clock: f64,
    }

    impl Host for Direct {
        fn gh(&mut self, _: &[&str]) -> Option<GhOut> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Some(out(0, "DIRECT\n", ""))
        }
        fn monotonic(&mut self) -> f64 {
            self.clock
        }
        fn sleep(&mut self, secs: f64) {
            self.clock += secs;
        }
        fn unix_time(&mut self) -> i64 {
            0
        }
    }

    #[test]
    fn without_a_daemon_every_call_runs_gh_directly() {
        let home = crate::shared::fs::temp_dir("ghpoll-no-daemon");
        let calls = Arc::new(AtomicUsize::new(0));
        let direct = Box::new(Direct {
            calls: calls.clone(),
            clock: 0.0,
        });
        let mut h = Cached::with(&home, 10.0, direct, true);
        let args = ["pr", "view", PR, "--json", "state", "--jq", ".state"];
        if which("gh").is_some() {
            assert_eq!(h.gh(&args).unwrap().stdout, "DIRECT\n");
            assert!(!h.live);
        }
        h.gh(&args);
        h.sleep(10.0);
        assert_eq!(h.monotonic(), 10.0);
        assert!(calls.load(Ordering::SeqCst) >= 1);
        std::fs::remove_dir_all(home).unwrap();
    }

    /// End to end over a real socket: a daemon-side poller answers, a second read is a cache hit,
    /// and a sleep ends early when the poller sees a new answer.
    #[cfg(unix)]
    #[test]
    fn reads_and_waits_go_through_the_daemon_socket() {
        use crate::shared::board_client::socket_path;
        use std::io::{BufRead, BufReader, Write};
        if which("gh").is_none() {
            return;
        }
        // Other tests swap HOME, which gh_env compares on both sides of the socket: a swap
        // between them is a refusal, and the command then runs gh itself.
        let _env = crate::shared::test_env_lock();
        let home = crate::shared::fs::temp_dir("ghpoll-socket");
        let (p, calls) = poller(vec![
            out(0, "{\"s\":\"PENDING\"}", ""),
            out(0, "{\"s\":\"DONE\"}", ""),
        ]);
        let listener = std::os::unix::net::UnixListener::bind(socket_path(&home)).unwrap();
        let served = p.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let p = served.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let reply = p.answer(&serde_json::from_str(&line).unwrap());
                    reader
                        .get_mut()
                        .write_all(format!("{reply}\n").as_bytes())
                        .unwrap();
                });
            }
        });
        let direct_calls = Arc::new(AtomicUsize::new(0));
        let direct = Box::new(Direct {
            calls: direct_calls.clone(),
            clock: 0.0,
        });
        let mut h = Cached::with(&home, 10.0, direct, true);
        let args = ["pr", "view", PR, "--json", "headRefOid,baseRefName"];
        assert_eq!(h.gh(&args).unwrap().stdout, "{\"s\":\"PENDING\"}");
        assert_eq!(h.gh(&args).unwrap().stdout, "{\"s\":\"PENDING\"}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let k = key(&args);
        let ager = {
            let p = p.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(200));
                age(&p, &k, 60);
                p.tick(&|| false);
            })
        };
        h.sleep(10.0);
        ager.join().unwrap();
        assert_eq!(
            h.direct.monotonic(),
            0.0,
            "woke on the change, not the clock"
        );
        assert_eq!(h.gh(&args).unwrap().stdout, "{\"s\":\"DONE\"}");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(direct_calls.load(Ordering::SeqCst), 0);
        assert!(h.live);
        // A key read before a sleep stays held; one not read since the last sleep does not.
        h.sleep(0.0);
        assert_eq!(h.held.len(), 1);
        h.sleep(0.0);
        assert!(h.held.is_empty());
        // A write never goes to the daemon, and the next read of a key asks gh afresh even though
        // the daemon's answer is young.
        h.gh(&args);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a young answer serves a first read"
        );
        h.gh(&["pr", "update-branch", PR]);
        assert_eq!(direct_calls.load(Ordering::SeqCst), 1);
        assert!(h.held.is_empty());
        h.gh(&args);
        assert_eq!(calls.load(Ordering::SeqCst), 3, "fresh after the write");
        h.gh(&args);
        assert_eq!(calls.load(Ordering::SeqCst), 3, "then held again");
        std::fs::remove_dir_all(home).unwrap();
    }
}
