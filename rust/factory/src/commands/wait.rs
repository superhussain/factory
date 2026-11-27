//! wait, watch and round-wait: block until job artifacts carry a valid first line.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::cli::Call;
use crate::commands::{checks, claims, ghpoll};
use crate::shared::board::read_board;
use crate::shared::conf::{fm_get, frontmatter};
use crate::shared::exit::{fail, Res};
use crate::shared::fs::is_link;
use crate::shared::project::{project_files, r2_target};
use crate::shared::py::{os_error, printable_line, repr, splitlines, strip, universal_newlines};
use crate::shared::pyjson::{dumps, DEFAULT};
use crate::shared::pyre::{Flags, Pattern};
use crate::shared::pyval::{py_eq, truthy};

static WAIT_ARTIFACT: Pattern = Pattern::new(
    r"spec\.md|spec-evidence\.md|build\.md|decision\.md|walkthrough\.html|build\.walkthrough\.html|round-[1-9][0-9]*/review-(security|ux|ui|code)\.md",
    Flags::NONE,
);
static SKIPPED_REVIEW: Pattern = Pattern::new(r"round-[1-9][0-9]*/review-(ux|ui)\.md", Flags::NONE);
const WAIT_USAGE: &str = "usage: wait <project-slug>/<job-id> <artifact>... [--timeout S] [--hold]";
const WATCH_USAGE: &str =
    "usage: watch <project-slug>/<job-id> [<artifact>...] [--pr] [--timeout S]";
const WATCH_TIMEOUT: u64 = 540;
const WATCH_MAX_TIMEOUT: u64 = 7080;
const WATCH_HOLD_SECONDS: f64 = claims::CLAIM_REFRESH_MINUTES * 60.0;
const PR_STATE_INTERVAL: f64 = 30.0;
const ROUND_WAIT_USAGE: &str = "usage: round-wait <project-slug>/<job-id> <round> [--timeout S]";
const REVIEWERS: [&str; 4] = ["security", "ux", "ui", "code"];
static ROUND: Pattern = Pattern::new("[1-9][0-9]*", Flags::NONE);

/// True when line is a finished first line for artifact.
fn wait_valid(artifact: &str, line: &str) -> bool {
    if [
        "spec.md",
        "spec-evidence.md",
        "build.md",
        "walkthrough.html",
        "build.walkthrough.html",
    ]
    .contains(&artifact)
    {
        return !strip(line).is_empty();
    }
    if artifact == "decision.md" {
        return ["APPROVE", "ESCALATE", "CHANGES"].contains(&line) || line.starts_with("CLOSED: ");
    }
    if line == "VERDICT: SKIPPED" {
        return SKIPPED_REVIEW.is_full(artifact);
    }
    line == "VERDICT: PASS" || line == "VERDICT: CHANGES"
}

/// `read_text(errors="replace").splitlines()[0]`, or "" for an empty file.
fn first_line(p: &Path) -> io::Result<String> {
    let text = universal_newlines(String::from_utf8_lossy(&fs::read(p)?).into_owned());
    Ok(splitlines(&text, false)
        .first()
        .map_or(String::new(), |l| l.to_string()))
}

/// Each present artifact's first line, refusing any linked file, parent or job folder.
fn job_file_lines(folder: &Path, artifacts: &[&str]) -> Res<BTreeMap<String, String>> {
    let mut lines = BTreeMap::new();
    for artifact in artifacts {
        let path = folder.join(artifact);
        for node in [path.as_path(), path.parent().unwrap(), folder] {
            if is_link(node).map_err(|e| fail(os_error(&e, Some(node)), 1))? {
                return Err(fail(
                    format!(
                        "{} is a symlink or junction; nothing was read through it",
                        node.display()
                    ),
                    6,
                ));
            }
        }
        if path.is_file() {
            let line = first_line(&path).map_err(|e| fail(os_error(&e, Some(&path)), 1))?;
            lines.insert(artifact.to_string(), line);
        }
    }
    Ok(lines)
}

/// A `--timeout` value of ASCII digits from min to max.
fn timeout_arg(v: Option<&String>, max: u64, min: u64) -> Option<u64> {
    let v = v?;
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    v.parse::<u64>().ok().filter(|n| (min..=max).contains(n))
}

/// The calls the Python tests patch: `hold`, `time.monotonic` and `time.sleep`.
struct Io<'a> {
    hold: &'a dyn Fn(&str) -> Res<()>,
    now: &'a dyn Fn() -> f64,
    sleep: &'a dyn Fn(f64),
}

/// Print each artifact's first line once all are valid, checking once a second until --timeout.
fn wait_run(
    home: &Path,
    slug: &str,
    job_id: &str,
    target: &str,
    args: &[String],
    io: &Io,
) -> Res<String> {
    let (mut artifacts, mut timeout, mut refresh) = (Vec::new(), None, false);
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--hold" {
            if refresh {
                return Err(fail(format!("--hold given twice. {WAIT_USAGE}"), 2));
            }
            refresh = true;
            i += 1;
        } else if args[i] == "--timeout" {
            match (timeout, timeout_arg(args.get(i + 1), 600, 0)) {
                (None, Some(t)) => timeout = Some(t),
                _ => return Err(fail("--timeout takes one value from 0 to 600 seconds", 2)),
            }
            i += 2;
        } else if WAIT_ARTIFACT.is_full(&args[i]) {
            artifacts.push(args[i].as_str());
            i += 1;
        } else {
            return Err(fail(
                format!("bad artifact {}. {WAIT_USAGE}", repr(&args[i])),
                2,
            ));
        }
    }
    if artifacts.is_empty() {
        return Err(fail(WAIT_USAGE, 2));
    }
    if refresh {
        (io.hold)(target)?;
    }
    let folder = home.join("jobs").join(slug).join(job_id);
    let deadline = (io.now)() + timeout.unwrap_or(WATCH_TIMEOUT) as f64;
    loop {
        let lines = job_file_lines(&folder, &artifacts)?;
        let missing: Vec<&str> = artifacts
            .iter()
            .copied()
            .filter(|a| !lines.contains_key(*a))
            .collect();
        let malformed: Vec<&str> = artifacts
            .iter()
            .copied()
            .filter(|a| lines.get(*a).is_some_and(|l| !wait_valid(a, l)))
            .collect();
        if missing.is_empty() && malformed.is_empty() {
            return Ok(artifacts
                .iter()
                .map(|a| format!("{a}: {}\n", lines[*a]))
                .collect());
        }
        if (io.now)() >= deadline {
            if !missing.is_empty() {
                return Err(fail(format!("still missing: {}", missing.join(", ")), 3));
            }
            let read: Vec<String> = malformed
                .iter()
                .map(|a| format!("{a} reads \"{}\"", printable_line(&lines[*a], 60)))
                .collect();
            return Err(fail(format!("no valid first line: {}", read.join(", ")), 5));
        }
        (io.sleep)(1.0);
    }
}

fn real_hold(target: &str) -> Res<()> {
    claims::hold_target(target, None)
}

/// A set, non-empty FACTORY_SESSION, read as raw OS bytes so a non-UTF-8 value still counts.
fn session_set(value: Option<OsString>) -> bool {
    value.is_some_and(|s| !s.is_empty())
}

pub fn wait_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let origin = Instant::now();
    let now = || origin.elapsed().as_secs_f64();
    let sleep = |s: f64| thread::sleep(Duration::from_secs_f64(s));
    let io = Io {
        hold: &real_hold,
        now: &now,
        sleep: &sleep,
    };
    print!(
        "{}",
        wait_run(&home, &slug, &job_id, &call.args[0], &call.args[1..], &io)?
    );
    Ok(())
}

/// Run hold once when a session tag is set, then return one JSON line when every artifact has a
/// valid first line, --timeout passes, or a file keeps an invalid non-empty first line for two reads.
fn watch_run(
    home: &Path,
    slug: &str,
    job_id: &str,
    target: &str,
    args: &[String],
    session: bool,
    io: &Io,
) -> Res<String> {
    let (mut artifacts, mut timeout, mut watch_pr) = (Vec::new(), None, false);
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--pr" {
            if watch_pr {
                return Err(fail(format!("--pr given twice. {WATCH_USAGE}"), 2));
            }
            watch_pr = true;
            i += 1;
        } else if args[i] == "--timeout" {
            match (timeout, timeout_arg(args.get(i + 1), WATCH_MAX_TIMEOUT, 1)) {
                (None, Some(t)) => timeout = Some(t),
                _ => {
                    return Err(fail(
                        format!("--timeout takes one value from 1 to 7080 seconds. {WATCH_USAGE}"),
                        2,
                    ))
                }
            }
            i += 2;
        } else if WAIT_ARTIFACT.is_full(&args[i]) {
            artifacts.push(args[i].as_str());
            i += 1;
        } else {
            return Err(fail(
                format!("bad artifact {}. {WATCH_USAGE}", repr(&args[i])),
                2,
            ));
        }
    }
    if artifacts.is_empty() && !watch_pr {
        return Err(fail(WATCH_USAGE, 2));
    }
    if session {
        (io.hold)(target)?;
    } else {
        let board = read_board(home)?;
        let job = board["jobs"].as_array().and_then(|jobs| {
            jobs.iter()
                .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id))
        });
        let Some(job) = job else {
            return Err(fail(
                format!("no job {job_id} on the board. Nothing was written."),
                4,
            ));
        };
        let project = match job.get("project") {
            None | Some(Value::Null) => "None".to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => dumps(other, DEFAULT),
        };
        if job.get("project").and_then(Value::as_str) != Some(slug) {
            return Err(fail(
                format!("{job_id} belongs to {project}, not {slug}"),
                2,
            ));
        }
    }
    let folder = home.join("jobs").join(slug).join(job_id);
    let mut pr_leg = if watch_pr {
        let mut host = ghpoll::Cached::new(home, PR_STATE_INTERVAL);
        let pr = checks::board_pr(home, slug, job_id, true)?;
        let md = project_files(home, slug)?.0;
        let meta = frontmatter(&md)
            .map_err(|e| fail(os_error(&e, Some(&md)), 1))?
            .0;
        let base = checks::pr_base(&mut host, &meta, &pr)?;
        let handled = checks::job_handled(home, job_id)?;
        let org = fm_get(&meta, "buildkite_org").unwrap_or("").to_string();
        Some((host, pr, base, handled, org))
    } else {
        None
    };
    let start = (io.now)();
    let deadline = start + timeout.unwrap_or(WATCH_TIMEOUT) as f64;
    let mut bad_before: BTreeMap<String, String> = BTreeMap::new();
    let (mut reading, mut read_at, mut held_at) = (Value::Null, start, start);
    loop {
        let lines = job_file_lines(&folder, &artifacts)?;
        let bad: BTreeMap<String, String> = artifacts
            .iter()
            .filter_map(|a| {
                lines
                    .get(*a)
                    .filter(|l| !strip(l).is_empty() && !wait_valid(a, l))
                    .map(|l| (a.to_string(), l.clone()))
            })
            .collect();
        if let Some((host, pr, base, handled, org)) = &mut pr_leg {
            if (io.now)() >= read_at {
                reading = checks::pr_reading(host, home, pr, base, org, handled)?;
                read_at = (io.now)() + PR_STATE_INTERVAL;
            }
        }
        let event = if !artifacts.is_empty()
            && artifacts
                .iter()
                .all(|a| lines.get(*a).is_some_and(|l| wait_valid(a, l)))
        {
            "files"
        } else if bad.iter().any(|(a, line)| bad_before.get(a) == Some(line)) {
            "malformed"
        } else if truthy(&reading)
            && !matches!(reading["next"].as_str(), Some("wait" | "wait-base"))
        {
            "pr"
        } else if (io.now)() >= deadline {
            "timeout"
        } else {
            bad_before = bad;
            let now = (io.now)();
            if session && now - held_at >= WATCH_HOLD_SECONDS {
                (io.hold)(target)?;
                held_at = now;
            }
            (io.sleep)(1.0);
            continue;
        };
        let mut files = Map::new();
        for a in &artifacts {
            files.insert(
                a.to_string(),
                lines
                    .get(*a)
                    .map_or(Value::Null, |l| Value::from(l.as_str())),
            );
        }
        let elapsed = ((io.now)() - start) as i64;
        let out = json!({"event": event, "files": files, "pr": reading, "elapsed": elapsed});
        return Ok(format!("{}\n", dumps(&out, DEFAULT)));
    }
}

pub fn watch_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let session = session_set(std::env::var_os("FACTORY_SESSION"));
    let origin = Instant::now();
    let now = || origin.elapsed().as_secs_f64();
    let sleep = |s: f64| thread::sleep(Duration::from_secs_f64(s));
    let io = Io {
        hold: &real_hold,
        now: &now,
        sleep: &sleep,
    };
    print!(
        "{}",
        watch_run(
            &home,
            &slug,
            &job_id,
            &call.args[0],
            &call.args[1..],
            session,
            &io
        )?
    );
    Ok(())
}

/// Run hold once, then wait until all four review files of round N have a valid first line, writing the
/// board's rounds entry each time its verdicts change, and return one JSON line. A file that keeps an
/// invalid non-empty first line for two reads, or --timeout passing, also ends it.
fn round_wait_run(
    home: &Path,
    slug: &str,
    job_id: &str,
    target: &str,
    args: &[String],
    io: &Io,
) -> Res<String> {
    if !ROUND.is_full(&args[0]) {
        return Err(fail(
            format!("bad round {}. {ROUND_WAIT_USAGE}", repr(&args[0])),
            2,
        ));
    }
    let n: usize = args[0].parse().unwrap_or(usize::MAX);
    let rest = &args[1..];
    let mut timeout = WATCH_TIMEOUT;
    if !rest.is_empty() {
        match timeout_arg(rest.get(1), WATCH_TIMEOUT, 1) {
            Some(t) if rest.len() == 2 && rest[0] == "--timeout" => timeout = t,
            _ => {
                return Err(fail(
                    format!("--timeout takes one value from 1 to 540 seconds. {ROUND_WAIT_USAGE}"),
                    2,
                ))
            }
        }
    }
    (io.hold)(target)?;
    let board_rounds = || -> Res<Vec<Value>> {
        let board = read_board(home)?;
        let job = board["jobs"].as_array().and_then(|jobs| {
            jobs.iter()
                .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id))
        });
        match job.and_then(|j| j.get("rounds")).and_then(Value::as_array) {
            Some(rounds) if rounds.len() >= n => Ok(rounds.clone()),
            _ => Err(fail(
                format!("{job_id} has no rounds entry {n} on the board. Nothing was written."),
                4,
            )),
        }
    };
    let reviewers_of = |entry: &Value| -> Map<String, Value> {
        entry
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(k, _)| REVIEWERS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    let mut written = Value::Object(reviewers_of(&board_rounds()?[n - 1]));
    let folder = home.join("jobs").join(slug).join(job_id);
    let artifacts: Vec<String> = REVIEWERS
        .iter()
        .map(|r| format!("round-{n}/review-{r}.md"))
        .collect();
    let names: Vec<&str> = artifacts.iter().map(String::as_str).collect();
    let start = (io.now)();
    let deadline = start + timeout as f64;
    let mut bad_before: BTreeMap<String, String> = BTreeMap::new();
    loop {
        let lines = job_file_lines(&folder, &names)?;
        let valid: BTreeMap<&str, &String> = names
            .iter()
            .filter_map(|a| lines.get(*a).filter(|l| wait_valid(a, l)).map(|l| (*a, l)))
            .collect();
        let mut verdicts = Map::new();
        for (r, a) in REVIEWERS.iter().zip(&names) {
            let v = valid.get(a).map_or("pending", |l| &l["VERDICT: ".len()..]);
            verdicts.insert(r.to_string(), Value::from(v));
        }
        let verdicts = Value::Object(verdicts);
        if !py_eq(&verdicts, &written) {
            let mut rounds = board_rounds()?;
            let mut entry: Map<String, Value> = rounds[n - 1]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, _)| !REVIEWERS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            entry.extend(verdicts.as_object().unwrap().clone());
            rounds[n - 1] = Value::Object(entry);
            let mut patch = Map::new();
            patch.insert("rounds".into(), Value::Array(rounds));
            let opts = claims::WriteOpts {
                show: false,
                verdict: None,
                rewrite_round: Some(n),
            };
            claims::board_write(home, job_id, patch, &opts)?;
            written = verdicts.clone();
        }
        let bad: BTreeMap<String, String> = lines
            .iter()
            .filter(|(a, l)| !strip(l).is_empty() && !valid.contains_key(a.as_str()))
            .map(|(a, l)| (a.clone(), l.clone()))
            .collect();
        let event = if valid.len() == names.len() {
            "round"
        } else if bad.iter().any(|(a, line)| bad_before.get(a) == Some(line)) {
            "malformed"
        } else if (io.now)() >= deadline {
            "timeout"
        } else {
            bad_before = bad;
            (io.sleep)(1.0);
            continue;
        };
        let mut files = Map::new();
        for a in &names {
            files.insert(
                a.to_string(),
                lines
                    .get(*a)
                    .map_or(Value::Null, |l| Value::from(l.as_str())),
            );
        }
        let elapsed = ((io.now)() - start) as i64;
        let out = json!({"event": event, "round": n, "verdicts": verdicts, "files": files, "elapsed": elapsed});
        return Ok(format!("{}\n", dumps(&out, DEFAULT)));
    }
}

pub fn round_wait_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let origin = Instant::now();
    let now = || origin.elapsed().as_secs_f64();
    let sleep = |s: f64| thread::sleep(Duration::from_secs_f64(s));
    let io = Io {
        hold: &real_hold,
        now: &now,
        sleep: &sleep,
    };
    print!(
        "{}",
        round_wait_run(&home, &slug, &job_id, &call.args[0], &call.args[1..], &io)?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    #[cfg(unix)]
    #[test]
    fn non_utf8_session_counts_as_set() {
        use std::os::unix::ffi::OsStrExt;
        assert!(session_set(Some(
            std::ffi::OsStr::from_bytes(b"\xff").to_os_string()
        )));
        assert!(!session_set(Some(OsString::new())));
        assert!(!session_set(None));
    }

    use crate::shared::exit::exit;
    use crate::shared::fs::temp_dir;

    /// A temp home holding `jobs/sample/<job>/round-1`, and that job folder.
    fn home(name: &str, job: &str) -> (PathBuf, PathBuf) {
        let home = temp_dir(name);
        let folder = home.join("jobs/sample").join(job);
        fs::create_dir_all(folder.join("round-1")).unwrap();
        (home, folder)
    }

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn no_hold(_: &str) -> Res<()> {
        Ok(())
    }

    fn wait(home: &Path, args: &[&str], io: &Io) -> Res<String> {
        wait_run(
            home,
            "sample",
            "001-sample-job",
            "sample/001-sample-job",
            &strings(args),
            io,
        )
    }

    #[test]
    fn wait_default_timeout_is_540() {
        let (home, _) = home("wait-default", "001-sample-job");
        let clock = Cell::new(0.0);
        let io = Io {
            hold: &no_hold,
            now: &|| clock.get(),
            sleep: &|s| clock.set(clock.get() + s),
        };
        let e = wait(&home, &["decision.md"], &io).unwrap_err();
        assert_eq!(
            (e.code, e.stderr.as_str()),
            (3, "still missing: decision.md\n")
        );
        assert_eq!(clock.get(), 540.0);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn wait_sees_a_file_that_lands() {
        let (home, folder) = home("wait-lands", "001-sample-job");
        let clock = Cell::new(0.0);
        let sleep = |s: f64| {
            clock.set(clock.get() + s);
            if clock.get() == 3.0 {
                fs::write(folder.join("decision.md"), "APPROVE\n").unwrap();
            }
        };
        let io = Io {
            hold: &no_hold,
            now: &|| clock.get(),
            sleep: &sleep,
        };
        assert_eq!(
            wait(&home, &["decision.md", "--timeout", "5"], &io).unwrap(),
            "decision.md: APPROVE\n"
        );
        assert_eq!(clock.get(), 3.0);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn wait_hold_runs_once_before_reading() {
        let (home, folder) = home("wait-hold", "001-sample-job");
        let decision = folder.join("decision.md");
        fs::write(&decision, "APPROVE\n").unwrap();
        let seen = RefCell::new(Vec::new());
        let hold = |target: &str| {
            seen.borrow_mut()
                .push((target.to_string(), fs::read_to_string(&decision).unwrap()));
            fs::write(&decision, "CHANGES\n").unwrap();
            Ok(())
        };
        let io = Io {
            hold: &hold,
            now: &|| 0.0,
            sleep: &|_| {},
        };
        assert_eq!(
            wait(&home, &["decision.md", "--hold", "--timeout", "0"], &io).unwrap(),
            "decision.md: CHANGES\n"
        );
        assert_eq!(
            *seen.borrow(),
            [("sample/001-sample-job".to_string(), "APPROVE\n".to_string())]
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn wait_hold_exit_stops() {
        let (home, folder) = home("wait-hold-exit", "001-sample-job");
        fs::write(folder.join("decision.md"), "APPROVE\n").unwrap();
        let io = Io {
            hold: &|_| Err(exit(6)),
            now: &|| 0.0,
            sleep: &|_| {},
        };
        assert_eq!(
            wait(&home, &["decision.md", "--hold"], &io)
                .unwrap_err()
                .code,
            6
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn wait_linked_file_exits_6() {
        let (home, folder) = home("wait-link", "001-sample-job");
        fs::write(home.join("elsewhere.md"), "APPROVE\n").unwrap();
        std::os::unix::fs::symlink(home.join("elsewhere.md"), folder.join("decision.md")).unwrap();
        let io = Io {
            hold: &no_hold,
            now: &|| 0.0,
            sleep: &|_| {},
        };
        let e = wait(&home, &["decision.md", "--timeout", "0"], &io).unwrap_err();
        assert_eq!(e.code, 6);
        assert!(e.stderr.contains("symlink or junction"));
        fs::remove_dir_all(home).unwrap();
    }

    fn watch(home: &Path, args: &[&str], io: &Io) -> Res<String> {
        watch_run(
            home,
            "sample",
            "042-sample-export",
            "sample/042-sample-export",
            &strings(args),
            true,
            io,
        )
    }

    #[test]
    fn watch_file_lands_at_212() {
        let (home, folder) = home("watch-lands", "042-sample-export");
        let clock = Cell::new(0.0);
        let sleep = |s: f64| {
            clock.set(clock.get() + s);
            if clock.get() == 212.0 {
                fs::write(folder.join("spec.md"), "# Spec: sample export\n").unwrap();
            }
        };
        let io = Io {
            hold: &no_hold,
            now: &|| clock.get(),
            sleep: &sleep,
        };
        assert_eq!(
            watch(&home, &["spec.md"], &io).unwrap(),
            "{\"event\": \"files\", \"files\": {\"spec.md\": \"# Spec: sample export\"}, \"pr\": null, \"elapsed\": 212}\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn watch_walkthrough_artifacts() {
        let (home, folder) = home("watch-walkthrough", "042-sample-export");
        fs::write(
            folder.join("walkthrough.html"),
            "<!doctype html>\n<title>Spec walkthrough</title>\n",
        )
        .unwrap();
        fs::write(
            folder.join("build.walkthrough.html"),
            "<!doctype html>\n<title>Build walkthrough</title>\n",
        )
        .unwrap();
        let io = Io {
            hold: &no_hold,
            now: &|| 0.0,
            sleep: &|_| {},
        };
        assert_eq!(
            watch(&home, &["walkthrough.html", "build.walkthrough.html"], &io).unwrap(),
            "{\"event\": \"files\", \"files\": {\"walkthrough.html\": \"<!doctype html>\", \"build.walkthrough.html\": \"<!doctype html>\"}, \"pr\": null, \"elapsed\": 0}\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn watch_default_timeout_540() {
        let (home, _) = home("watch-default", "042-sample-export");
        let clock = Cell::new(0.0);
        let io = Io {
            hold: &no_hold,
            now: &|| clock.get(),
            sleep: &|s| clock.set(clock.get() + s),
        };
        assert_eq!(
            watch(&home, &["decision.md"], &io).unwrap(),
            "{\"event\": \"timeout\", \"files\": {\"decision.md\": null}, \"pr\": null, \"elapsed\": 540}\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn watch_malformed_fixed_in_time() {
        let (home, folder) = home("watch-fixed", "042-sample-export");
        fs::write(folder.join("decision.md"), "Looks good\n").unwrap();
        let clock = Cell::new(0.0);
        let sleep = |s: f64| {
            clock.set(clock.get() + s);
            if clock.get() == 1.0 {
                fs::write(folder.join("decision.md"), "APPROVE\n").unwrap();
            }
        };
        let io = Io {
            hold: &no_hold,
            now: &|| clock.get(),
            sleep: &sleep,
        };
        assert_eq!(
            watch(&home, &["decision.md"], &io).unwrap(),
            "{\"event\": \"files\", \"files\": {\"decision.md\": \"APPROVE\"}, \"pr\": null, \"elapsed\": 1}\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn watch_hold_once_first() {
        let (home, folder) = home("watch-hold", "042-sample-export");
        let clock = Cell::new(0.0);
        let holds = RefCell::new(Vec::new());
        let hold = |target: &str| {
            holds.borrow_mut().push((
                target.to_string(),
                folder.join("spec.md").exists(),
                clock.get(),
            ));
            Ok(())
        };
        let sleep = |s: f64| {
            clock.set(clock.get() + s);
            if clock.get() == 3.0 {
                fs::write(folder.join("spec.md"), "# Spec: sample export\n").unwrap();
            }
        };
        let io = Io {
            hold: &hold,
            now: &|| clock.get(),
            sleep: &sleep,
        };
        assert!(watch(&home, &["spec.md"], &io)
            .unwrap()
            .starts_with("{\"event\": \"files\""));
        assert_eq!(
            *holds.borrow(),
            [("sample/042-sample-export".to_string(), false, 0.0)]
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn watch_hold_exit_stops() {
        let (home, folder) = home("watch-hold-exit", "042-sample-export");
        fs::write(folder.join("spec.md"), "# Spec: sample export\n").unwrap();
        let io = Io {
            hold: &|_| Err(exit(6)),
            now: &|| 0.0,
            sleep: &|_| {},
        };
        assert_eq!(watch(&home, &["spec.md", "--pr"], &io).unwrap_err().code, 6);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn watch_accepts_timeout_7080() {
        let (home, _) = home("watch-7080", "042-sample-export");
        let clock = Cell::new(0.0);
        let io = Io {
            hold: &no_hold,
            now: &|| clock.get(),
            sleep: &|s| clock.set(clock.get() + s),
        };
        assert_eq!(
            watch(&home, &["decision.md", "--timeout", "7080"], &io).unwrap(),
            "{\"event\": \"timeout\", \"files\": {\"decision.md\": null}, \"pr\": null, \"elapsed\": 7080}\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn watch_rejects_timeout_7081() {
        let (home, _) = home("watch-7081", "042-sample-export");
        let io = Io {
            hold: &no_hold,
            now: &|| 0.0,
            sleep: &|_| {},
        };
        let e = watch(&home, &["decision.md", "--timeout", "7081"], &io).unwrap_err();
        assert_eq!(
            (e.code, e.stderr),
            (
                2,
                format!("--timeout takes one value from 1 to 7080 seconds. {WATCH_USAGE}\n")
            )
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn watch_holds_every_600_seconds() {
        let (home, _) = home("watch-rehold", "042-sample-export");
        let clock = Cell::new(0.0);
        let holds = RefCell::new(Vec::new());
        let hold = |_: &str| {
            holds.borrow_mut().push(clock.get());
            Ok(())
        };
        let io = Io {
            hold: &hold,
            now: &|| clock.get(),
            sleep: &|s| clock.set(clock.get() + s),
        };
        assert!(watch(&home, &["decision.md", "--timeout", "1300"], &io)
            .unwrap()
            .starts_with("{\"event\": \"timeout\""));
        assert_eq!(*holds.borrow(), [0.0, 600.0, 1200.0]);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn watch_without_session_never_holds() {
        use crate::shared::http::tests::{home_for, serve};
        let board =
            json!({"updated": "", "jobs": [{"id": "042-sample-export", "project": "sample"}]})
                .to_string();
        let board: &'static str = Box::leak(board.into_boxed_str());
        let (origin, _) = serve(vec![(200, board, vec![("ETag", "\"e1\"")])]);
        let home = home_for("watch-no-session", &origin);
        let clock = Cell::new(0.0);
        let holds = RefCell::new(Vec::new());
        let hold = |_: &str| {
            holds.borrow_mut().push(clock.get());
            Ok(())
        };
        let io = Io {
            hold: &hold,
            now: &|| clock.get(),
            sleep: &|s| clock.set(clock.get() + s),
        };
        assert!(watch_run(
            &home,
            "sample",
            "042-sample-export",
            "sample/042-sample-export",
            &strings(&["decision.md", "--timeout", "1300"]),
            false,
            &io,
        )
        .unwrap()
        .starts_with("{\"event\": \"timeout\""));
        assert!(holds.borrow().is_empty());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn round_wait_refuses_bad_round_and_timeout() {
        let (home, _) = home("round-args", "042-sample-export");
        let io = Io {
            hold: &no_hold,
            now: &|| 0.0,
            sleep: &|_| {},
        };
        let run = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            round_wait_run(
                &home,
                "sample",
                "042-sample-export",
                "sample/042-sample-export",
                &args,
                &io,
            )
            .unwrap_err()
        };
        let e = run(&["0"]);
        assert_eq!(
            (e.code, e.stderr),
            (2, format!("bad round '0'. {ROUND_WAIT_USAGE}\n"))
        );
        for args in [
            &["1", "--timeout"][..],
            &["1", "--timeout", "0"],
            &["1", "--timeout", "541"],
            &["1", "-t", "5"],
        ] {
            let e = run(args);
            assert_eq!(
                (e.code, e.stderr),
                (
                    2,
                    format!(
                        "--timeout takes one value from 1 to 540 seconds. {ROUND_WAIT_USAGE}\n"
                    )
                ),
                "{args:?}"
            );
        }
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn writes_as_verdicts_land() {
        use crate::shared::http::tests::{home_for, serve};
        let round = |security: &str, code: &str| json!({"security": security, "ux": "SKIPPED", "ui": "SKIPPED", "code": code});
        let round_1 = json!({"security": "PASS", "ux": "PASS", "ui": "SKIPPED", "code": "CHANGES"});
        let board = json!({"updated": "", "jobs": [{
            "id": "042-sample-export", "project": "sample", "title": "Sample export",
            "stage": "review", "round": 2, "rounds": [round_1, round("pending", "pending")],
        }]})
        .to_string();
        let board: &'static str = Box::leak(board.into_boxed_str());
        let (origin, seen) = serve(vec![(200, board, vec![("ETag", "\"e1\"")]); 24]);
        let home = home_for("round-wait-writes", &origin);
        let folder = home.join("jobs/sample/042-sample-export/round-2");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("review-ux.md"), "VERDICT: SKIPPED\n").unwrap();
        fs::write(folder.join("review-ui.md"), "VERDICT: SKIPPED\n").unwrap();
        let clock = Cell::new(0.0);
        let sleep = |s: f64| {
            clock.set(clock.get() + s);
            if clock.get() == 212.0 {
                fs::write(folder.join("review-security.md"), "VERDICT: CHANGES\n").unwrap();
            }
            if clock.get() == 305.0 {
                fs::write(folder.join("review-code.md"), "VERDICT: PASS\n").unwrap();
            }
        };
        let io = Io {
            hold: &no_hold,
            now: &|| clock.get(),
            sleep: &sleep,
        };
        round_wait_run(
            &home,
            "sample",
            "042-sample-export",
            "sample/042-sample-export",
            &strings(&["2"]),
            &io,
        )
        .unwrap();
        let puts: Vec<Value> = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "PUT")
            .map(|r| serde_json::from_slice::<Value>(&r.body).unwrap()["jobs"][0]["rounds"].clone())
            .collect();
        assert_eq!(
            puts,
            [
                json!([round_1, round("CHANGES", "pending")]),
                json!([round_1, round("CHANGES", "PASS")]),
            ]
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn watch_linked_file_exits_6() {
        let (home, folder) = home("watch-link", "042-sample-export");
        fs::write(home.join("elsewhere.md"), "# Spec: elsewhere\n").unwrap();
        std::os::unix::fs::symlink(home.join("elsewhere.md"), folder.join("spec.md")).unwrap();
        let io = Io {
            hold: &no_hold,
            now: &|| 0.0,
            sleep: &|_| {},
        };
        let e = watch(&home, &["spec.md"], &io).unwrap_err();
        assert_eq!(e.code, 6);
        assert!(e.stderr.contains("symlink or junction"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn wait_valid_table() {
        let rows: &[(&str, &str, bool)] = &[
            ("spec.md", "# Spec", true),
            ("spec.md", " \t", false),
            ("spec-evidence.md", "", false),
            ("build.md", "Routing", true),
            ("decision.md", "APPROVE", true),
            ("decision.md", "ESCALATE", true),
            ("decision.md", "CHANGES", true),
            ("decision.md", "CLOSED: superseded", true),
            ("decision.md", "CLOSED:", false),
            ("decision.md", "MERGE", false),
            ("round-1/review-ux.md", "VERDICT: SKIPPED", true),
            ("round-12/review-ui.md", "VERDICT: SKIPPED", true),
            ("round-1/review-security.md", "VERDICT: SKIPPED", false),
            ("round-1/review-code.md", "VERDICT: SKIPPED", false),
            ("round-1/review-code.md", "VERDICT: PASS", true),
            ("round-1/review-code.md", "VERDICT: CHANGES", true),
            ("round-1/review-code.md", "VERDICT: pass", false),
        ];
        for (artifact, line, valid) in rows {
            assert_eq!(wait_valid(artifact, line), *valid, "{artifact} {line:?}");
        }
    }
}
