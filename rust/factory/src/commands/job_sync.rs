//! The daemon's job file sync. It watches the folders of this host's active jobs: each job on the
//! board's mirror that is past idea, not merged or closed, and has a folder here. A folder whose
//! files change and no longer match their last sync is pushed once it has been still for QUIET,
//! through `push_job`, so the daemon applies every rule and refusal `factory push` does. A job
//! whose board row changes is pulled through `pull_job`. A conflict keeps the local file, saves
//! R2's copy beside it as `<file>.remote` and is logged; nothing is overwritten.
//!
//! A job a live session on this host holds (its claim names this device and is not stale) is
//! never pulled automatically. That session may hold a file it read before R2 moved; if the daemon
//! replaced the file under it, the manifest would take R2's new ETag and the session's later save
//! would push over the other host's work. Leaving such a job alone keeps today's protection: its
//! stale save meets a 412 and both copies are kept. The session pulls when it chooses to. Its
//! pushes still go up automatically. A job folder that is a symlink or junction is never followed.
//!
//! The folders are scanned for size and mtime every SCAN rather than watched through OS events:
//! a handful of small folders costs microseconds a scan and needs no platform code. Job file
//! writes reach the Worker with no announcement, so a pull follows a row change, not a file one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

use crate::commands::mirror::Mirror;
use crate::commands::sync::{locally_synced, pull_job_files, push_job, walk};
use crate::shared::consts::{IDEA, JOB_REL, TERMINAL};
use crate::shared::fs::is_link;
use crate::shared::home::{device_name, holder};
use crate::shared::project::valid_job_target;
use crate::shared::time::elapsed_seconds;

/// How often the watched folders are scanned for changes.
const SCAN: Duration = Duration::from_secs(2);
/// How long a folder must go unchanged before its changes are pushed.
const QUIET: Duration = Duration::from_secs(3);
/// How long after a failed push or pull it is first tried again; each further failure doubles
/// the wait, up to RETRY_MAX.
const RETRY: Duration = Duration::from_secs(60);
const RETRY_MAX: Duration = Duration::from_secs(30 * 60);
/// How often the loop checks for a stop signal and the mirror for a new version.
const POLL: Duration = Duration::from_millis(200);

/// Each job file's (path, size, mtime) as one scan saw them.
type Stamp = Vec<(String, u64, Option<SystemTime>)>;

#[derive(Default)]
struct Job {
    /// The board row as last seen, without the claim's heartbeat.
    row: String,
    stamp: Option<Stamp>,
    /// Whether the job is still active on the board. An inactive job is dropped once it has
    /// nothing left to push.
    active: bool,
    /// Until when a live session on this host holds the job, so it is not pulled automatically:
    /// the moment its claim goes stale, even if the board never changes again.
    held_until: Option<Instant>,
    push_at: Option<Instant>,
    pull_at: Option<Instant>,
    /// Failed pushes and pulls in a row, for the retry backoff.
    fails: u32,
}

impl Job {
    /// When to try again after a failure, and counts it.
    fn retry(&mut self, now: Instant) -> Option<Instant> {
        let wait = (RETRY * (1u32 << self.fails.min(5))).min(RETRY_MAX);
        self.fails += 1;
        Some(now + wait)
    }

    /// Whether to keep watching: an active job, or an inactive one with a first push still to
    /// run. A failing push stops once the job leaves the board's active stages.
    fn kept(&self) -> bool {
        self.active || (self.push_at.is_some() && self.fails == 0)
    }
}

pub struct JobSync {
    home: PathBuf,
    /// Watched jobs by `<project>/<id>`.
    jobs: BTreeMap<String, Job>,
    /// The mirror version the rows came from.
    etag: String,
}

/// The row as compared between versions: `claim.seen` moves on every heartbeat, which changes
/// no job file.
fn row_key(row: &Value) -> String {
    let mut row = row.clone();
    if let Some(claim) = row.get_mut("claim").and_then(Value::as_object_mut) {
        claim.shift_remove("seen");
    }
    row.to_string()
}

/// The (project, id) of a board row whose job files may still change, else None.
fn active(row: &Value) -> Option<(&str, &str)> {
    let stage = row.get("stage")?.as_str()?;
    if stage == IDEA || TERMINAL.contains(&stage) {
        return None;
    }
    let (slug, id) = (row.get("project")?.as_str()?, row.get("id")?.as_str()?);
    valid_job_target(slug, id).then_some((slug, id))
}

/// How much longer a row's claim holds the job here: while it names device and was seen within
/// stale_minutes. None once it doesn't.
fn held_for(row: &Value, device: &str, stale_minutes: u32) -> Option<Duration> {
    let claim = &row["claim"];
    if claim["device"].as_str() != Some(device) {
        return None;
    }
    let age = claim["seen"].as_str().and_then(elapsed_seconds)?;
    let left = i64::from(stale_minutes) * 60 - age;
    (left > 0).then(|| Duration::from_secs(left as u64))
}

/// A job folder the sync may read: a real directory, with no symlink or junction at it or at
/// `jobs/` or `jobs/<slug>` above it, the nodes `safe_job_folder` checks.
fn real_dir(folder: &Path) -> bool {
    let slug_dir = folder.parent();
    let jobs_dir = slug_dir.and_then(Path::parent);
    [Some(folder), slug_dir, jobs_dir]
        .into_iter()
        .all(|node| node.is_some_and(|n| !is_link(n).unwrap_or(true)))
        && folder.is_dir()
}

fn stamp(folder: &Path) -> Stamp {
    walk(folder)
        .unwrap_or_default()
        .into_iter()
        .filter(|(rel, _)| JOB_REL.is_full(rel))
        .filter_map(|(rel, path)| {
            let meta = std::fs::symlink_metadata(&path).ok()?;
            meta.is_file()
                .then(|| (rel, meta.len(), meta.modified().ok()))
        })
        .collect()
}

impl JobSync {
    pub fn new(home: &Path) -> JobSync {
        JobSync {
            home: home.to_path_buf(),
            jobs: BTreeMap::new(),
            etag: String::new(),
        }
    }

    fn folder(&self, key: &str) -> PathBuf {
        let (slug, id) = key.split_once('/').unwrap();
        self.home.join("jobs").join(slug).join(id)
    }

    /// Takes a board version's rows. Watches each active job with a folder here and queues a pull
    /// for each one first seen or whose row changed.
    fn rows(&mut self, rows: &[Value], now: Instant) {
        let device = device_name(&self.home);
        let stale_minutes = holder(&self.home).map_or(120, |(_, _, minutes)| minutes);
        for job in self.jobs.values_mut() {
            job.active = false;
        }
        for row in rows {
            let Some((slug, id)) = active(row) else {
                continue;
            };
            let key = format!("{slug}/{id}");
            if !real_dir(&self.folder(&key)) {
                continue;
            }
            let held_until = held_for(row, &device, stale_minutes).map(|left| now + left);
            let row = row_key(row);
            let job = self.jobs.entry(key).or_default();
            job.active = true;
            job.held_until = held_until;
            if job.row != row {
                job.row = row;
                job.pull_at.get_or_insert(now);
            }
        }
        self.jobs.retain(|_, job| job.kept());
    }

    /// Rescans each watched folder. One whose files changed queues a push QUIET from now, so a
    /// burst of writes pushes once, after the last.
    fn scan(&mut self, now: Instant) {
        let keys: Vec<String> = self.jobs.keys().cloned().collect();
        for key in keys {
            let folder = self.folder(&key);
            if !real_dir(&folder) {
                self.jobs.remove(&key);
                continue;
            }
            let stamp = stamp(&folder);
            let job = self.jobs.get_mut(&key).unwrap();
            if job.stamp.as_ref() != Some(&stamp) {
                job.stamp = Some(stamp);
                job.push_at = Some(now + QUIET);
                job.fails = 0;
            }
        }
    }

    /// Runs each pull and push that is due, pulls first. A push whose folder already matches its
    /// last sync, as after a pull, sends nothing. Stops between jobs once `stop` is true.
    fn run_due(&mut self, now: Instant, log: &dyn Fn(&str), stop: &dyn Fn() -> bool) {
        for (key, job) in self.jobs.iter_mut() {
            if stop() {
                return;
            }
            let (slug, id) = key.split_once('/').unwrap();
            // A held job's pull stays queued until no live session here holds it: until the claim
            // moves or its deadline passes, whichever comes first.
            let held = job.held_until.is_some_and(|until| now < until);
            if !held && job.pull_at.is_some_and(|at| at <= now) {
                job.pull_at = match pull_job_files(&self.home, slug, id, true) {
                    Ok((line, _, got)) => {
                        if !got.is_empty() {
                            log(&format!("job sync {key}: pulled {}", got.join(", ")));
                        }
                        // The rest of the summary: unpushed files and conflicts.
                        if let Some((_, rest)) = line.split_once(", ") {
                            log(&format!("job sync {key}: {rest}"));
                        }
                        job.fails = 0;
                        None
                    }
                    Err(e) => {
                        log(&format!(
                            "job sync {key}: pull failed: {}",
                            e.stderr.trim_end()
                        ));
                        job.retry(now)
                    }
                };
            }
            if !job.push_at.is_some_and(|at| at <= now) {
                continue;
            }
            let folder = self.home.join("jobs").join(slug).join(id);
            if !real_dir(&folder) || locally_synced(&folder) {
                job.push_at = None;
                continue;
            }
            let mut out = Vec::new();
            let result = push_job(&self.home, slug, id, &mut out);
            for line in String::from_utf8_lossy(&out).lines() {
                // The count line is replaced by the names logged below.
                if !(line.starts_with("pushed ") && line.ends_with(" files")) {
                    log(&format!("job sync {key}: {line}"));
                }
            }
            job.push_at = match result {
                Ok(pushed) => {
                    if !pushed.is_empty() {
                        log(&format!("job sync {key}: pushed {}", pushed.join(", ")));
                    }
                    job.fails = 0;
                    None
                }
                // A conflict is settled by a person merging the .remote copy; that edit queues
                // the next push.
                Err(e) if e.code == 6 => {
                    log(&format!("job sync {key}: {}", e.stderr.trim_end()));
                    None
                }
                Err(e) => {
                    log(&format!(
                        "job sync {key}: push failed: {}",
                        e.stderr.trim_end()
                    ));
                    job.retry(now)
                }
            };
        }
        self.jobs.retain(|_, job| job.kept());
    }
}

/// Runs the sync on its own thread until `stop` reports a signal.
pub fn spawn(
    home: &Path,
    mirror: Arc<Mirror>,
    stop: fn() -> Option<&'static str>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) -> JoinHandle<()> {
    let mut sync = JobSync::new(home);
    std::thread::spawn(move || {
        let mut scanned: Option<Instant> = None;
        while stop().is_none() {
            let now = Instant::now();
            if let Some((etag, rows)) = mirror.jobs_since(&sync.etag) {
                sync.etag = etag;
                sync.rows(&rows, now);
            }
            if scanned.is_none_or(|at| now.duration_since(at) >= SCAN) {
                scanned = Some(now);
                sync.scan(now);
            }
            sync.run_due(now, &*log, &|| stop().is_some());
            std::thread::sleep(POLL);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::http::tests::{home_for, serve, Seen};
    use crate::shared::project::sha;
    use serde_json::json;
    use std::fs;
    use std::sync::Mutex;

    const JOB: &str = "042-sample-export";

    type Reply = (u16, String, Vec<(&'static str, String)>);

    /// A loopback server that answers each request by its method and path, and how many times
    /// that method and path came before, so a test's replies never fall out of step when the code
    /// under test makes more or fewer requests.
    fn serve_routes(
        route: impl Fn(&str, &str, usize) -> Reply + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<Seen>>>) {
        serve_requests(move |r, nth| route(&r.method, &r.path, nth))
    }

    /// `serve_routes`, with the whole request, headers and body included, for the route.
    fn serve_requests(
        route: impl Fn(&Seen, usize) -> Reply + Send + 'static,
    ) -> (String, Arc<Mutex<Vec<Seen>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, path) = (
                    parts.next().unwrap().to_string(),
                    parts.next().unwrap().to_string(),
                );
                let mut headers = Vec::new();
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    let (k, v) = h.split_once(':').unwrap();
                    headers.push((k.to_string(), v.trim().to_string()));
                }
                let length = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .map_or(0, |(_, v)| v.parse().unwrap());
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request = Seen {
                    method,
                    path,
                    headers,
                    body,
                };
                let nth = counts
                    .entry(format!("{} {}", request.method, request.path))
                    .or_default();
                let (status, reply, extra) = route(&request, *nth);
                *nth += 1;
                log.lock().unwrap().push(request);
                let mut head = format!("HTTP/1.0 {status} X\r\n");
                for (k, v) in extra {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                let _ = stream.write_all(format!("{head}\r\n{reply}").as_bytes());
            }
        });
        (origin, seen)
    }

    /// A routed reply: a body and its ETag.
    fn file(body: &str, etag: &str) -> Reply {
        (200, body.to_string(), vec![("ETag", format!("\"{etag}\""))])
    }

    fn listing(files: &[(&str, &str)]) -> Reply {
        let rows: Vec<Value> = files
            .iter()
            .map(|(name, etag)| json!({"name": name, "etag": format!("\"{etag}\"")}))
            .collect();
        (200, Value::from(rows).to_string(), vec![])
    }

    /// One job's files in a fake R2, by name: (body, ETag without quotes).
    type Store = Arc<Mutex<BTreeMap<String, (String, String)>>>;

    /// A fake R2 holding files for the job. A PUT honours `If-Match` and `If-None-Match: *` as
    /// R2 does, so a stale condition really meets a 412, and a PUT that lands gets a new ETag.
    fn fake_r2(files: &[(&str, &str, &str)]) -> (String, Arc<Mutex<Vec<Seen>>>, Store) {
        let store: Store = Arc::new(Mutex::new(
            files
                .iter()
                .map(|(name, body, etag)| (name.to_string(), (body.to_string(), etag.to_string())))
                .collect(),
        ));
        let r2 = store.clone();
        let job = format!("/api/jobs/alpha/{JOB}/");
        let puts = std::sync::atomic::AtomicUsize::new(0);
        let (origin, seen) = serve_requests(move |r, _| {
            let mut files = r2.lock().unwrap();
            let rel = r.path.strip_prefix(&job).unwrap_or(&r.path);
            let current = files.get(rel).map(|(_, etag)| format!("\"{etag}\""));
            match r.method.as_str() {
                "GET" if rel == "?etags=1" => {
                    let rows: Vec<(&str, &str)> = files
                        .iter()
                        .map(|(name, (_, etag))| (name.as_str(), etag.as_str()))
                        .collect();
                    listing(&rows)
                }
                "GET" => files
                    .get(rel)
                    .map_or((404, String::new(), vec![]), |(body, etag)| {
                        file(body, etag)
                    }),
                "PUT" => {
                    let allowed = match (r.header("If-Match"), r.header("If-None-Match")) {
                        (Some(tag), _) => current.as_deref() == Some(tag),
                        (None, Some("*")) => current.is_none(),
                        _ => true,
                    };
                    if !allowed {
                        return (412, String::new(), vec![]);
                    }
                    let n = puts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let etag = format!("put{n}");
                    let body = String::from_utf8(r.body.clone()).unwrap();
                    files.insert(rel.to_string(), (body, etag.clone()));
                    (200, String::new(), vec![("ETag", format!("\"{etag}\""))])
                }
                _ => (404, String::new(), vec![]),
            }
        });
        (origin, seen, store)
    }

    /// R2's body for a file.
    fn in_r2(store: &Store, rel: &str) -> String {
        store.lock().unwrap()[rel].0.clone()
    }

    fn manifest(f: &Path) -> Value {
        crate::shared::pyjson::loads(&fs::read_to_string(f.join(".sync.json")).unwrap()).unwrap()
    }

    fn row(stage: &str, seen: &str) -> Value {
        json!({"id": JOB, "project": "alpha", "stage": stage,
               "claim": {"device": "mac", "seen": seen}})
    }

    fn folder(home: &Path) -> PathBuf {
        home.join("jobs/alpha").join(JOB)
    }

    fn with_job(name: &str, origin: &str) -> PathBuf {
        let home = home_for(name, origin);
        fs::create_dir_all(folder(&home)).unwrap();
        home
    }

    fn logger() -> (Arc<Mutex<Vec<String>>>, impl Fn(&str)) {
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = lines.clone();
        (lines, move |l: &str| {
            sink.lock().unwrap().push(l.to_string())
        })
    }

    #[test]
    fn watches_active_local_jobs_and_pulls_on_row_change() {
        let home = with_job("job-sync-rows", "http://127.0.0.1:9");
        let mut sync = JobSync::new(&home);
        let now = Instant::now();
        let other = json!({"id": "043-elsewhere", "project": "alpha", "stage": "build"});
        sync.rows(
            &[
                row("build", "t1"),
                other,
                json!({"id": "044-x", "project": "alpha", "stage": "idea"}),
            ],
            now,
        );
        assert_eq!(
            sync.jobs.keys().collect::<Vec<_>>(),
            [&format!("alpha/{JOB}")]
        );
        let job = sync.jobs.values_mut().next().unwrap();
        assert_eq!(job.pull_at, Some(now));
        job.pull_at = None;
        // A heartbeat alone pulls nothing; a stage change does.
        sync.rows(&[row("build", "t2")], now);
        assert_eq!(sync.jobs.values().next().unwrap().pull_at, None);
        sync.rows(&[row("review", "t2")], now);
        assert_eq!(sync.jobs.values().next().unwrap().pull_at, Some(now));
        // A merged job with nothing left to push is dropped.
        sync.rows(&[row("merged", "t2")], now);
        assert!(sync.jobs.is_empty());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn pushes_after_the_folder_goes_quiet() {
        let (origin, seen) = serve(vec![
            (200, "[]", vec![]),
            (200, "", vec![("ETag", "\"b1\"")]),
        ]);
        let home = with_job("job-sync-push", &origin);
        let mut sync = JobSync::new(&home);
        let (lines, log) = logger();
        let t0 = Instant::now();
        sync.rows(&[row("build", "t1")], t0);
        sync.jobs.values_mut().next().unwrap().pull_at = None;
        sync.scan(t0);
        // An empty folder is in sync, so its first scan pushes nothing.
        sync.run_due(t0 + QUIET, &log, &|| false);
        assert!(seen.lock().unwrap().is_empty());
        fs::write(folder(&home).join("build.md"), "Built\n").unwrap();
        fs::create_dir_all(folder(&home).join("scratch")).unwrap();
        fs::write(folder(&home).join("scratch/notes.md"), "local\n").unwrap();
        let t1 = t0 + SCAN;
        sync.scan(t1);
        sync.run_due(t1 + QUIET - Duration::from_millis(1), &log, &|| false);
        assert!(
            seen.lock().unwrap().is_empty(),
            "pushed before the debounce"
        );
        sync.run_due(t1 + QUIET, &log, &|| false);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            (
                seen[1].method.as_str(),
                seen[1].path.as_str(),
                seen[1].header("If-None-Match")
            ),
            (
                "PUT",
                "/api/jobs/alpha/042-sample-export/build.md",
                Some("*")
            )
        );
        assert_eq!(seen[1].body, b"Built\n");
        assert_eq!(
            *lines.lock().unwrap(),
            [format!("job sync alpha/{JOB}: pushed build.md")]
        );
        assert!(locally_synced(&folder(&home)));
        assert_eq!(sync.jobs.values().next().unwrap().push_at, None);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn conflict_keeps_both_copies_and_logs() {
        let (origin, _seen) = serve(vec![
            (200, r#"[{"name":"build.md","etag":"\"b0\""}]"#, vec![]),
            (412, "", vec![]),
            (200, "Remote\n", vec![("ETag", "\"b1\"")]),
        ]);
        let home = with_job("job-sync-conflict", &origin);
        let state = json!({"build.md": {"etag": "\"b0\"", "sha256": sha(b"Old\n")}});
        fs::write(folder(&home).join(".sync.json"), state.to_string()).unwrap();
        fs::write(folder(&home).join("build.md"), "Local\n").unwrap();
        let mut sync = JobSync::new(&home);
        let (lines, log) = logger();
        let t0 = Instant::now();
        sync.rows(&[row("build", "t1")], t0);
        sync.jobs.values_mut().next().unwrap().pull_at = None;
        sync.scan(t0);
        sync.run_due(t0 + QUIET, &log, &|| false);
        assert_eq!(
            fs::read(folder(&home).join("build.md")).unwrap(),
            b"Local\n"
        );
        assert_eq!(
            fs::read(folder(&home).join("build.md.remote")).unwrap(),
            b"Remote\n"
        );
        let lines = lines.lock().unwrap();
        assert!(
            lines.iter().any(|l| l.contains("conflict in build.md")),
            "{lines:?}"
        );
        assert_eq!(sync.jobs.values().next().unwrap().push_at, None);
        assert!(!locally_synced(&folder(&home)));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn pull_fetches_a_changed_file_and_failures_retry() {
        let (origin, seen) = serve(vec![
            (200, r#"[{"name":"spec.md","etag":"\"s1\""}]"#, vec![]),
            (200, "Spec\n", vec![("ETag", "\"s1\"")]),
        ]);
        let home = with_job("job-sync-pull", &origin);
        let mut sync = JobSync::new(&home);
        let (lines, log) = logger();
        let t0 = Instant::now();
        sync.rows(&[row("build", "t1")], t0);
        sync.scan(t0);
        sync.run_due(t0, &log, &|| false);
        assert_eq!(fs::read(folder(&home).join("spec.md")).unwrap(), b"Spec\n");
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert_eq!(
            *lines.lock().unwrap(),
            [format!("job sync alpha/{JOB}: pulled spec.md")]
        );
        // The pulled file matches its sync, so the scan that sees it pushes nothing.
        sync.scan(t0 + SCAN);
        sync.run_due(t0 + SCAN + QUIET, &log, &|| false);
        assert_eq!(seen.lock().unwrap().len(), 2);
        // The server has no more replies, so the next pull fails and waits RETRY.
        sync.rows(&[row("review", "t1")], t0);
        sync.run_due(t0, &log, &|| false);
        assert_eq!(sync.jobs.values().next().unwrap().pull_at, Some(t0 + RETRY));
        // The next failure waits twice as long, and a stop between jobs runs nothing.
        sync.run_due(t0 + RETRY, &log, &|| false);
        assert_eq!(
            sync.jobs.values().next().unwrap().pull_at,
            Some(t0 + RETRY * 3)
        );
        sync.run_due(t0 + RETRY * 3, &log, &|| true);
        assert_eq!(lines.lock().unwrap().len(), 3);
        // A failing job that leaves the active stages is dropped.
        sync.jobs.values_mut().next().unwrap().push_at = Some(t0);
        sync.rows(&[row("closed", "t1")], t0);
        assert!(sync.jobs.is_empty());
        assert!(lines.lock().unwrap()[1].contains("pull failed"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn pull_keeps_a_file_written_during_the_fetch() {
        use crate::shared::http::tests::serve_hooked;
        let target: Arc<Mutex<PathBuf>> = Arc::default();
        let hook = target.clone();
        let (origin, _seen) = serve_hooked(
            vec![
                (200, r#"[{"name":"build.md","etag":"\"b1\""}]"#, vec![]),
                (200, "Remote\n", vec![("ETag", "\"b1\"")]),
            ],
            move |index| {
                if index == 1 {
                    fs::write(&*hook.lock().unwrap(), "Agent\n").unwrap();
                }
            },
        );
        let home = with_job("job-sync-pull-race", &origin);
        *target.lock().unwrap() = folder(&home).join("build.md");
        let (line, conflicts) = crate::commands::sync::pull_job(&home, "alpha", JOB).unwrap();
        assert_eq!(conflicts, ["build.md"], "{line}");
        assert_eq!(
            fs::read(folder(&home).join("build.md")).unwrap(),
            b"Agent\n"
        );
        assert_eq!(
            fs::read(folder(&home).join("build.md.remote")).unwrap(),
            b"Remote\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    /// A row whose claim names this home's device, seen `seen`.
    fn claimed(home: &Path, stage: &str, seen: &str) -> Value {
        json!({"id": JOB, "project": "alpha", "stage": stage,
               "claim": {"device": device_name(home), "session": "s1", "seen": seen}})
    }

    #[test]
    fn held_job_is_not_pulled_so_a_stale_save_meets_a_412() {
        // R2 moved build.md to b2 on another host. A session here read b1 and saves over it.
        let (origin, seen) = serve(vec![
            (200, r#"[{"name":"build.md","etag":"\"b2\""}]"#, vec![]),
            (412, "", vec![]),
            (200, "Other host\n", vec![("ETag", "\"b2\"")]),
        ]);
        let home = with_job("job-sync-held", &origin);
        let f = folder(&home);
        fs::write(f.join("build.md"), "Read here\n").unwrap();
        let state = json!({"build.md": {"etag": "\"b1\"", "sha256": sha(b"Read here\n")}});
        fs::write(f.join(".sync.json"), state.to_string()).unwrap();
        let mut sync = JobSync::new(&home);
        let (lines, log) = logger();
        let t0 = Instant::now();
        sync.rows(&[claimed(&home, "build", &crate::shared::time::now())], t0);
        sync.scan(t0);
        sync.run_due(t0 + QUIET, &log, &|| false);
        assert!(seen.lock().unwrap().is_empty(), "a held job was pulled");
        assert!(sync.jobs.values().next().unwrap().pull_at.is_some());
        // The session saves its stale copy; the daemon pushes it with the old ETag.
        fs::write(f.join("build.md"), "Stale save\n").unwrap();
        sync.scan(t0 + SCAN);
        sync.run_due(t0 + SCAN + QUIET, &log, &|| false);
        assert_eq!(seen.lock().unwrap()[1].header("If-Match"), Some("\"b1\""));
        assert_eq!(fs::read(f.join("build.md")).unwrap(), b"Stale save\n");
        assert_eq!(
            fs::read(f.join("build.md.remote")).unwrap(),
            b"Other host\n"
        );
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("conflict in build.md")));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn stale_claim_or_other_device_still_pulls() {
        let home = with_job("job-sync-stale-claim", "http://127.0.0.1:9");
        let mut sync = JobSync::new(&home);
        let t0 = Instant::now();
        let held_until = |sync: &JobSync| sync.jobs.values().next().unwrap().held_until;
        sync.rows(&[claimed(&home, "build", "2020-01-01T00:00:00Z")], t0);
        assert_eq!(held_until(&sync), None);
        sync.rows(&[row("build", &crate::shared::time::now())], t0);
        assert_eq!(held_until(&sync), None);
        sync.rows(&[claimed(&home, "build", &crate::shared::time::now())], t0);
        assert!(held_until(&sync).is_some_and(|at| at > t0 + Duration::from_secs(119 * 60)));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_claim_that_goes_stale_on_a_quiet_board_releases_its_pull() {
        let (origin, seen, _store) = fake_r2(&[]);
        let home = with_job("job-sync-quiet-stale", &origin);
        let mut sync = JobSync::new(&home);
        let (_lines, log) = logger();
        let t0 = Instant::now();
        // Seen 119 minutes ago: one minute from stale at the default 120.
        let seen_at = crate::shared::time::format_stamp(crate::shared::time::unix_now() - 119 * 60);
        sync.rows(&[claimed(&home, "build", &seen_at)], t0);
        sync.run_due(t0, &log, &|| false);
        assert!(seen.lock().unwrap().is_empty(), "a held job was pulled");
        // No new board version arrives. Once the claim's deadline passes, the queued pull runs.
        sync.run_due(t0 + Duration::from_secs(61), &log, &|| false);
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert_eq!(sync.jobs.values().next().unwrap().pull_at, None);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_secret_is_never_uploaded() {
        let (origin, seen) = serve(vec![
            (200, "[]", vec![]),
            (200, "", vec![("ETag", "\"b1\"")]),
        ]);
        let home = with_job("job-sync-secret", &origin);
        let f = folder(&home);
        fs::write(f.join("build.md"), "Built\n").unwrap();
        let token = format!("token ghp_{}\n", "a".repeat(30));
        fs::write(f.join("notes.md"), &token).unwrap();
        let mut sync = JobSync::new(&home);
        let (lines, log) = logger();
        let t0 = Instant::now();
        sync.rows(&[row("build", "t1")], t0);
        sync.jobs.values_mut().next().unwrap().pull_at = None;
        sync.scan(t0);
        sync.run_due(t0 + QUIET, &log, &|| false);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|r| !r.path.ends_with("notes.md")));
        let lines = lines.lock().unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("notes.md: not uploaded: it looks like it holds a secret")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("refused notes.md")),
            "{lines:?}"
        );
        // Refused is settled by a person, so it isn't retried until the folder changes.
        assert_eq!(sync.jobs.values().next().unwrap().push_at, None);
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_project_or_jobs_folder_is_never_watched() {
        for linked in ["jobs/alpha", "jobs"] {
            let home = home_for("job-sync-parent-link", "http://127.0.0.1:9");
            fs::create_dir_all(home.join("elsewhere").join(JOB)).unwrap();
            let target = if linked == "jobs" {
                fs::create_dir_all(home.join("real/alpha").join(JOB)).unwrap();
                home.join("real")
            } else {
                fs::create_dir_all(home.join("jobs")).unwrap();
                home.join("elsewhere")
            };
            std::os::unix::fs::symlink(target, home.join(linked)).unwrap();
            assert!(folder(&home).is_dir());
            let mut sync = JobSync::new(&home);
            sync.rows(&[row("build", "t1")], Instant::now());
            assert!(sync.jobs.is_empty(), "{linked}");
            fs::remove_dir_all(home).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_job_folder_is_never_watched() {
        let home = home_for("job-sync-link", "http://127.0.0.1:9");
        fs::create_dir_all(home.join("jobs/alpha")).unwrap();
        fs::create_dir_all(home.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(home.join("elsewhere"), folder(&home)).unwrap();
        let mut sync = JobSync::new(&home);
        sync.rows(&[row("build", "t1")], Instant::now());
        assert!(sync.jobs.is_empty());
        fs::remove_dir_all(home).unwrap();
    }

    /// A home whose build.md is "B0\n", synced at ETag b0, for a job this host does not hold.
    fn synced_b0(name: &str, origin: &str) -> PathBuf {
        let home = with_job(name, origin);
        let f = folder(&home);
        fs::write(f.join("build.md"), "B0\n").unwrap();
        let state = json!({"build.md": {"etag": "\"b0\"", "sha256": sha(b"B0\n")}});
        fs::write(f.join(".sync.json"), state.to_string()).unwrap();
        home
    }

    #[test]
    fn blind_write_after_an_auto_pull_keeps_both_copies() {
        let (origin, seen) = serve(vec![
            // The auto-pull: R2 holds another host's b1.
            (200, r#"[{"name":"build.md","etag":"\"b1\""}]"#, vec![]),
            (200, "B1 from the other host\n", vec![("ETag", "\"b1\"")]),
            // A second auto-pull: R2 moved again to b2.
            (200, r#"[{"name":"build.md","etag":"\"b2\""}]"#, vec![]),
            (200, "B2 from the other host\n", vec![("ETag", "\"b2\"")]),
            // The push of the blind write.
            (200, r#"[{"name":"build.md","etag":"\"b2\""}]"#, vec![]),
            (412, "", vec![]),
            (200, "B2 from the other host\n", vec![("ETag", "\"b2\"")]),
        ]);
        let home = synced_b0("job-sync-blind-write", &origin);
        let f = folder(&home);
        let mut sync = JobSync::new(&home);
        let (lines, log) = logger();
        let t0 = Instant::now();
        // 1. The daemon pulls b1 over the unchanged b0.
        sync.rows(&[row("build", "t1")], t0);
        sync.scan(t0);
        sync.run_due(t0, &log, &|| false);
        assert_eq!(
            fs::read(f.join("build.md")).unwrap(),
            b"B1 from the other host\n"
        );
        // A later row change pulls b2 over the unchanged b1; the base stays b0.
        sync.rows(&[row("review", "t1")], t0);
        sync.run_due(t0, &log, &|| false);
        assert_eq!(
            fs::read(f.join("build.md")).unwrap(),
            b"B2 from the other host\n"
        );
        // 2. A session that read b0 earlier writes it back with a shell redirect.
        fs::write(f.join("build.md"), "B0 plus a stale edit\n").unwrap();
        // 3. The next push sends If-Match b0, not b1, so it meets a 412.
        sync.scan(t0 + SCAN);
        sync.run_due(t0 + SCAN + QUIET, &log, &|| false);
        let seen = seen.lock().unwrap();
        assert_eq!(
            (seen[5].method.as_str(), seen[5].header("If-Match")),
            ("PUT", Some("\"b0\""))
        );
        // Nothing is lost: the stale write stays local and b2 waits beside it, still in R2.
        assert_eq!(
            fs::read(f.join("build.md")).unwrap(),
            b"B0 plus a stale edit\n"
        );
        assert_eq!(
            fs::read(f.join("build.md.remote")).unwrap(),
            b"B2 from the other host\n"
        );
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("conflict in build.md")));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn interrupted_auto_pull_still_conflicts_a_stale_write() {
        let job = format!("/api/jobs/alpha/{JOB}");
        let (origin, seen) = serve_routes(move |method, path, nth| {
            match (method, path.strip_prefix(&job).unwrap_or(path)) {
                ("GET", "/?etags=1") => listing(&[("build.md", "b1"), ("notes.md", "n1")]),
                ("GET", "/build.md") => file("B1\n", "b1"),
                // The first pull's notes.md fetch fails after build.md is written.
                ("GET", "/notes.md") if nth == 0 => (500, String::new(), vec![]),
                ("GET", "/notes.md") => file("Notes\n", "n1"),
                ("PUT", "/build.md") => (412, String::new(), vec![]),
                _ => (404, String::new(), vec![]),
            }
        });
        let home = synced_b0("job-sync-interrupted", &origin);
        let f = folder(&home);
        assert!(crate::commands::sync::pull_job_files(&home, "alpha", JOB, true).is_err());
        assert_eq!(fs::read(f.join("build.md")).unwrap(), b"B1\n");
        crate::commands::sync::pull_job_files(&home, "alpha", JOB, true).unwrap();
        fs::write(f.join("build.md"), "B0 plus a stale edit\n").unwrap();
        let e = push_job(&home, "alpha", JOB, &mut Vec::new()).unwrap_err();
        let seen = seen.lock().unwrap();
        let put = seen.iter().find(|r| r.method == "PUT").unwrap();
        assert_eq!(put.header("If-Match"), Some("\"b0\""));
        assert_eq!(e.code, 6);
        assert_eq!(
            fs::read(f.join("build.md")).unwrap(),
            b"B0 plus a stale edit\n"
        );
        assert_eq!(fs::read(f.join("build.md.remote")).unwrap(), b"B1\n");
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn failed_manual_pull_write_never_lets_old_bytes_push() {
        use std::os::unix::fs::PermissionsExt;
        let job = format!("/api/jobs/alpha/{JOB}");
        let (origin, seen) = serve_routes(move |method, path, _| {
            match (method, path.strip_prefix(&job).unwrap_or(path)) {
                ("GET", "/?etags=1") => listing(&[("round-1/review.md", "r1")]),
                ("GET", "/round-1/review.md") => file("R1\n", "r1"),
                ("PUT", "/round-1/review.md") => (412, String::new(), vec![]),
                _ => (404, String::new(), vec![]),
            }
        });
        let home = with_job("job-sync-manual-write-fails", &origin);
        let f = folder(&home);
        let round = f.join("round-1");
        fs::create_dir_all(&round).unwrap();
        fs::write(round.join("review.md"), "R0\n").unwrap();
        let state = json!({"round-1/review.md": {"etag": "\"r0\"", "sha256": sha(b"R0\n")}});
        fs::write(f.join(".sync.json"), state.to_string()).unwrap();
        // A deliberate pull whose write fails: round-1/ is read-only.
        fs::set_permissions(&round, fs::Permissions::from_mode(0o555)).unwrap();
        let pulled = crate::commands::sync::pull_job(&home, "alpha", JOB);
        fs::set_permissions(&round, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(pulled.is_err());
        assert_eq!(fs::read(round.join("review.md")).unwrap(), b"R0\n");
        // Nobody edits. The old bytes are the ones the pull meant to replace, so nothing goes up.
        assert_eq!(
            push_job(&home, "alpha", JOB, &mut Vec::new()).ok(),
            Some(vec![])
        );
        assert!(seen.lock().unwrap().iter().all(|r| r.method != "PUT"));
        assert!(!round.join("review.md.remote").exists());
        // An edit made from the old bytes still conditions on r0, their own version, and conflicts.
        fs::write(round.join("review.md"), "R0 plus an edit\n").unwrap();
        let e = push_job(&home, "alpha", JOB, &mut Vec::new()).unwrap_err();
        let seen = seen.lock().unwrap();
        let put = seen.iter().find(|r| r.method == "PUT").unwrap();
        assert_eq!(put.header("If-Match"), Some("\"r0\""));
        assert_eq!(e.code, 6);
        assert_eq!(
            fs::read(round.join("review.md")).unwrap(),
            b"R0 plus an edit\n"
        );
        assert_eq!(fs::read(round.join("review.md.remote")).unwrap(), b"R1\n");
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn manual_pull_clears_the_base_once_written() {
        let job = format!("/api/jobs/alpha/{JOB}");
        let (origin, _seen) = serve_routes(move |method, path, _| {
            match (method, path.strip_prefix(&job).unwrap_or(path)) {
                ("GET", "/?etags=1") => listing(&[("build.md", "b1")]),
                ("GET", "/build.md") => file("B1\n", "b1"),
                _ => (404, String::new(), vec![]),
            }
        });
        let home = synced_b0("job-sync-manual-clears", &origin);
        let f = folder(&home);
        crate::commands::sync::pull_job(&home, "alpha", JOB).unwrap();
        let manifest =
            crate::shared::pyjson::loads(&fs::read_to_string(f.join(".sync.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["build.md"]["etag"], "\"b1\"");
        assert!(manifest["build.md"].get("base").is_none());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn blind_create_after_an_auto_pull_keeps_both_copies() {
        let (origin, seen) = serve(vec![
            (200, r#"[{"name":"notes.md","etag":"\"n1\""}]"#, vec![]),
            (200, "Other host's notes\n", vec![("ETag", "\"n1\"")]),
            (200, r#"[{"name":"notes.md","etag":"\"n1\""}]"#, vec![]),
            (412, "", vec![]),
            (200, "Other host's notes\n", vec![("ETag", "\"n1\"")]),
        ]);
        let home = with_job("job-sync-blind-create", &origin);
        let f = folder(&home);
        let manifest = || {
            crate::shared::pyjson::loads(&fs::read_to_string(f.join(".sync.json")).unwrap())
                .unwrap()
        };
        crate::commands::sync::pull_job_files(&home, "alpha", JOB, true).unwrap();
        assert_eq!(manifest()["notes.md"]["base"], Value::Null);
        // A session that never saw the file creates it from scratch.
        fs::write(f.join("notes.md"), "Mine\n").unwrap();
        let e = push_job(&home, "alpha", JOB, &mut Vec::new()).unwrap_err();
        assert_eq!(e.code, 6);
        assert_eq!(seen.lock().unwrap()[3].header("If-None-Match"), Some("*"));
        assert_eq!(fs::read(f.join("notes.md")).unwrap(), b"Mine\n");
        assert_eq!(
            fs::read(f.join("notes.md.remote")).unwrap(),
            b"Other host's notes\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_deliberate_pull_clears_the_base_so_edits_push() {
        let (origin, seen) = serve(vec![
            (200, r#"[{"name":"build.md","etag":"\"b1\""}]"#, vec![]),
            (200, "B1\n", vec![("ETag", "\"b1\"")]),
            // The session's own pull: nothing to fetch, but it takes b1 as its base.
            (200, r#"[{"name":"build.md","etag":"\"b1\""}]"#, vec![]),
            (200, r#"[{"name":"build.md","etag":"\"b1\""}]"#, vec![]),
            (200, "", vec![("ETag", "\"b2\"")]),
        ]);
        let home = synced_b0("job-sync-deliberate", &origin);
        let f = folder(&home);
        crate::commands::sync::pull_job_files(&home, "alpha", JOB, true).unwrap();
        // An unchanged auto-pulled folder has nothing to push.
        assert!(locally_synced(&f));
        crate::commands::sync::pull_job(&home, "alpha", JOB).unwrap();
        fs::write(f.join("build.md"), "B1 plus my edit\n").unwrap();
        push_job(&home, "alpha", JOB, &mut Vec::new()).unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(
            (seen[4].method.as_str(), seen[4].header("If-Match")),
            ("PUT", Some("\"b1\""))
        );
        assert!(!f.join("build.md.remote").exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn locally_synced_reads_the_manifest() {
        let home = with_job("job-sync-local", "http://127.0.0.1:9");
        let f = folder(&home);
        assert!(locally_synced(&f), "nothing to push");
        fs::write(f.join("spec.md"), "Spec\n").unwrap();
        assert!(!locally_synced(&f), "no manifest entry");
        let state = json!({"spec.md": {"etag": "\"s1\"", "sha256": sha(b"Spec\n")}});
        fs::write(f.join(".sync.json"), state.to_string()).unwrap();
        fs::write(f.join("notes.txt"), "ignored\n").unwrap();
        assert!(locally_synced(&f));
        fs::write(f.join("spec.md.remote"), "R2\n").unwrap();
        assert!(!locally_synced(&f), "a conflict waits");
        fs::remove_file(f.join("spec.md.remote")).unwrap();
        fs::write(f.join("build.md"), "new\n").unwrap();
        assert!(!locally_synced(&f), "a local-only file");
        fs::remove_dir_all(home).unwrap();
    }

    /// A home whose round-1/review.md is "R0\n", synced at ETag r0.
    fn synced_r0(name: &str, origin: &str) -> PathBuf {
        let home = with_job(name, origin);
        let f = folder(&home);
        fs::create_dir_all(f.join("round-1")).unwrap();
        fs::write(f.join("round-1/review.md"), "R0\n").unwrap();
        let state = json!({"round-1/review.md": {"etag": "\"r0\"", "sha256": sha(b"R0\n")}});
        fs::write(f.join(".sync.json"), state.to_string()).unwrap();
        home
    }

    /// A pull cut short after it saved the manifest and before its write landed: round-1/ is
    /// read-only, as a full disk or a locked file would leave it.
    #[cfg(unix)]
    fn cut_short_pull(home: &Path, auto: bool) {
        use std::os::unix::fs::PermissionsExt;
        let round = folder(home).join("round-1");
        fs::set_permissions(&round, fs::Permissions::from_mode(0o555)).unwrap();
        let pulled = pull_job_files(home, "alpha", JOB, auto);
        fs::set_permissions(&round, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(pulled.is_err());
        assert_eq!(fs::read_to_string(round.join("review.md")).unwrap(), "R0\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_retried_pull_heals_one_cut_short_before_its_write() {
        for auto in [true, false] {
            let (origin, _seen, store) = fake_r2(&[("round-1/review.md", "R1\n", "r1")]);
            let home = synced_r0(&format!("job-sync-heal-{auto}"), &origin);
            let f = folder(&home);
            let review = f.join("round-1/review.md");
            cut_short_pull(&home, auto);
            // The retry knows the old bytes are the ones the cut-short pull meant to replace.
            let (line, _, _) = pull_job_files(&home, "alpha", JOB, auto).unwrap();
            assert_eq!(line, "pulled 1 files", "auto {auto}");
            assert_eq!(fs::read_to_string(&review).unwrap(), "R1\n");
            assert!(locally_synced(&f));
            if auto {
                // The base is still r0, so a session's write of the old copy still conflicts.
                fs::write(&review, "R0 plus a stale edit\n").unwrap();
                let e = push_job(&home, "alpha", JOB, &mut Vec::new()).unwrap_err();
                assert_eq!(e.code, 6);
                assert_eq!(in_r2(&store, "round-1/review.md"), "R1\n");
                assert_eq!(
                    fs::read_to_string(f.join("round-1/review.md.remote")).unwrap(),
                    "R1\n"
                );
            } else {
                // A deliberate pull took R2's version, so an edit pushes against it.
                fs::write(&review, "R1 plus an edit\n").unwrap();
                assert_eq!(
                    push_job(&home, "alpha", JOB, &mut Vec::new()).map_err(|e| e.stderr),
                    Ok(vec!["round-1/review.md".to_string()])
                );
                assert_eq!(in_r2(&store, "round-1/review.md"), "R1 plus an edit\n");
            }
            fs::remove_dir_all(home).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_retried_auto_pull_heals_after_an_earlier_one_landed() {
        let (origin, _seen, store) = fake_r2(&[("round-1/review.md", "R1\n", "r1")]);
        let home = synced_r0("job-sync-heal-twice", &origin);
        let f = folder(&home);
        let review = f.join("round-1/review.md");
        pull_job_files(&home, "alpha", JOB, true).unwrap();
        assert_eq!(fs::read_to_string(&review).unwrap(), "R1\n");
        store.lock().unwrap().insert(
            "round-1/review.md".to_string(),
            ("R2\n".to_string(), "r2".to_string()),
        );
        // The second pull is cut short over R1, the bytes the first pull wrote, not the base's R0.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(f.join("round-1"), fs::Permissions::from_mode(0o555)).unwrap();
        let pulled = pull_job_files(&home, "alpha", JOB, true);
        fs::set_permissions(f.join("round-1"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(pulled.is_err());
        let (line, _, _) = pull_job_files(&home, "alpha", JOB, true).unwrap();
        assert_eq!(line, "pulled 1 files");
        assert_eq!(fs::read_to_string(&review).unwrap(), "R2\n");
        // The base stays r0 through all three pulls.
        fs::write(&review, "R0 plus a stale edit\n").unwrap();
        assert_eq!(
            push_job(&home, "alpha", JOB, &mut Vec::new())
                .unwrap_err()
                .code,
            6
        );
        assert_eq!(in_r2(&store, "round-1/review.md"), "R2\n");
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn an_entry_without_the_pre_pull_hash_keeps_the_old_behavior() {
        let (origin, _seen, _store) = fake_r2(&[("round-1/review.md", "R1\n", "r1")]);
        let home = synced_r0("job-sync-old-manifest", &origin);
        let f = folder(&home);
        // A manifest a cut-short pull left before the hash was recorded.
        let state = json!({"round-1/review.md":
            {"etag": "\"r1\"", "sha256": sha(b"R1\n"), "base": "\"r0\""}});
        fs::write(f.join(".sync.json"), state.to_string()).unwrap();
        let (line, _, _) = pull_job_files(&home, "alpha", JOB, true).unwrap();
        assert_eq!(line, "pulled 0 files, unpushed: round-1/review.md");
        assert_eq!(
            fs::read_to_string(f.join("round-1/review.md")).unwrap(),
            "R0\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_deleted_file_an_auto_pull_restores_keeps_its_etag_as_base() {
        // R2 unchanged: an edit after the restore pushes.
        let (origin, _seen, store) = fake_r2(&[("build.md", "B0\n", "b0")]);
        let home = synced_b0("job-sync-restore", &origin);
        let f = folder(&home);
        fs::remove_file(f.join("build.md")).unwrap();
        pull_job_files(&home, "alpha", JOB, true).unwrap();
        assert_eq!(fs::read_to_string(f.join("build.md")).unwrap(), "B0\n");
        assert_eq!(manifest(&f)["build.md"]["base"], "\"b0\"");
        fs::write(f.join("build.md"), "B0 plus an edit\n").unwrap();
        assert_eq!(
            push_job(&home, "alpha", JOB, &mut Vec::new()).map_err(|e| e.stderr),
            Ok(vec!["build.md".to_string()])
        );
        assert_eq!(in_r2(&store, "build.md"), "B0 plus an edit\n");
        fs::remove_dir_all(home).unwrap();
        // R2 moved: a write from the copy deleted here still conflicts.
        let (origin, _seen, store) = fake_r2(&[("build.md", "B1\n", "b1")]);
        let home = synced_b0("job-sync-restore-moved", &origin);
        let f = folder(&home);
        fs::remove_file(f.join("build.md")).unwrap();
        pull_job_files(&home, "alpha", JOB, true).unwrap();
        fs::write(f.join("build.md"), "B0 plus a stale edit\n").unwrap();
        assert_eq!(
            push_job(&home, "alpha", JOB, &mut Vec::new())
                .unwrap_err()
                .code,
            6
        );
        assert_eq!(in_r2(&store, "build.md"), "B1\n");
        assert_eq!(
            fs::read_to_string(f.join("build.md.remote")).unwrap(),
            "B1\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn an_edit_in_the_rename_window_is_kept() {
        use crate::commands::sync::BEFORE_REPLACE;
        for auto in [true, false] {
            let (origin, _seen, store) = fake_r2(&[("build.md", "B1\n", "b1")]);
            let home = synced_b0(&format!("job-sync-rename-window-{auto}"), &origin);
            let f = folder(&home);
            // A session saves its edit after the pull saved the manifest, before the replace.
            BEFORE_REPLACE.set(Some(|path: &Path| {
                fs::write(path, "B0 plus an edit\n").unwrap()
            }));
            let pulled = pull_job_files(&home, "alpha", JOB, auto);
            BEFORE_REPLACE.set(None);
            let (_, conflicts, got) = pulled.unwrap();
            assert_eq!(
                fs::read_to_string(f.join("build.md")).unwrap(),
                "B0 plus an edit\n",
                "auto {auto}"
            );
            assert_eq!((conflicts, got), (vec!["build.md".to_string()], vec![]));
            assert_eq!(
                fs::read_to_string(f.join("build.md.remote")).unwrap(),
                "B1\n"
            );
            // Once a person merges and deletes the .remote, the push conditions on R2's b1.
            fs::write(f.join("build.md"), "B1 plus an edit\n").unwrap();
            fs::remove_file(f.join("build.md.remote")).unwrap();
            assert_eq!(
                push_job(&home, "alpha", JOB, &mut Vec::new()).map_err(|e| e.stderr),
                Ok(vec!["build.md".to_string()])
            );
            assert_eq!(in_r2(&store, "build.md"), "B1 plus an edit\n");
            fs::remove_dir_all(home).unwrap();
        }
    }

    #[test]
    fn a_merge_back_to_the_old_bytes_survives_a_pull_cut_short_after_a_conflict() {
        use crate::commands::sync::BEFORE_REPLACE;
        let (origin, _seen) = serve_requests(|r, nth| match (r.method.as_str(), r.path.as_str()) {
            (_, p) if p.ends_with("?etags=1") => listing(&[("build.md", "b1"), ("spec.md", "s1")]),
            (_, p) if p.ends_with("/build.md") => file("B1\n", "b1"),
            (_, p) if p.ends_with("/spec.md") && nth == 0 => (500, String::new(), vec![]),
            (_, p) if p.ends_with("/spec.md") => file("S1\n", "s1"),
            _ => (404, String::new(), vec![]),
        });
        let home = synced_b0("job-sync-merge-back", &origin);
        let f = folder(&home);
        BEFORE_REPLACE.set(Some(|path: &Path| {
            if path.ends_with("build.md") {
                fs::write(path, "B0 plus an edit\n").unwrap()
            }
        }));
        let pulled = pull_job_files(&home, "alpha", JOB, true);
        BEFORE_REPLACE.set(None);
        // build.md conflicted, then the spec.md fetch failed before the loop's last save.
        assert!(pulled.is_err());
        assert!(manifest(&f)["build.md"].get("pre_pull_sha256").is_none());
        // A person settles the conflict by keeping the old bytes and deletes the .remote.
        fs::write(f.join("build.md"), "B0\n").unwrap();
        fs::remove_file(f.join("build.md.remote")).unwrap();
        pull_job_files(&home, "alpha", JOB, true).unwrap();
        assert_eq!(fs::read_to_string(f.join("build.md")).unwrap(), "B0\n");
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_push_before_the_healing_pull_uploads_nothing() {
        let (origin, seen, store) = fake_r2(&[("round-1/review.md", "R1\n", "r1")]);
        let home = synced_r0("job-sync-push-before-heal", &origin);
        let f = folder(&home);
        cut_short_pull(&home, true);
        let mut out = Vec::new();
        assert_eq!(
            push_job(&home, "alpha", JOB, &mut out).map_err(|e| e.stderr),
            Ok(vec![])
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "round-1/review.md: remote newer, run pull\npushed 0 files\n"
        );
        assert!(seen.lock().unwrap().iter().all(|r| r.method != "PUT"));
        assert!(!f.join("round-1/review.md.remote").exists());
        // The retried pull still heals, and R2 keeps R1.
        let (line, _, _) = pull_job_files(&home, "alpha", JOB, true).unwrap();
        assert_eq!(line, "pulled 1 files");
        assert_eq!(
            fs::read_to_string(f.join("round-1/review.md")).unwrap(),
            "R1\n"
        );
        assert_eq!(in_r2(&store, "round-1/review.md"), "R1\n");
        fs::remove_dir_all(home).unwrap();
    }
}
