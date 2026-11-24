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
    /// Whether a live session on this host holds the job, so it is not pulled automatically.
    held: bool,
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

/// Whether a row's claim names device and was seen within stale_minutes.
fn held_by(row: &Value, device: &str, stale_minutes: u32) -> bool {
    let claim = &row["claim"];
    claim["device"].as_str() == Some(device)
        && claim["seen"]
            .as_str()
            .and_then(elapsed_seconds)
            .is_some_and(|age| age < i64::from(stale_minutes) * 60)
}

/// A job folder the sync may read: a real directory, never a symlink or junction.
fn real_dir(folder: &Path) -> bool {
    !is_link(folder).unwrap_or(true) && folder.is_dir()
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
            let held = held_by(row, &device, stale_minutes);
            let row = row_key(row);
            let job = self.jobs.entry(key).or_default();
            job.active = true;
            job.held = held;
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
            // A held job's pull stays queued until no live session here holds it.
            if !job.held && job.pull_at.is_some_and(|at| at <= now) {
                job.pull_at = match pull_job_files(&self.home, slug, id) {
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
    use crate::shared::http::tests::{home_for, serve};
    use crate::shared::project::sha;
    use serde_json::json;
    use std::fs;
    use std::sync::Mutex;

    const JOB: &str = "042-sample-export";

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
        sync.rows(&[claimed(&home, "build", "2020-01-01T00:00:00Z")], t0);
        assert!(!sync.jobs.values().next().unwrap().held);
        sync.rows(&[row("build", &crate::shared::time::now())], t0);
        assert!(!sync.jobs.values().next().unwrap().held);
        sync.rows(&[claimed(&home, "build", &crate::shared::time::now())], t0);
        assert!(sync.jobs.values().next().unwrap().held);
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
}
