//! wait and watch: block until job artifacts carry a valid first line. round-wait is not ported yet.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::cli::{not_ported, Call};
use crate::commands::claims;
use crate::shared::board::read_board;
use crate::shared::exit::{fail, Res};
use crate::shared::fs::is_link;
use crate::shared::project::r2_target;
use crate::shared::py::{os_error, printable_line, repr, splitlines, strip, universal_newlines};
use crate::shared::pyjson::{dumps, DEFAULT};
use crate::shared::pyre::{Flags, Pattern};

static WAIT_ARTIFACT: Pattern = Pattern::new(
    r"spec\.md|spec-evidence\.md|build\.md|decision\.md|round-[1-9][0-9]*/review-(security|ux|ui|code)\.md",
    Flags::NONE,
);
static SKIPPED_REVIEW: Pattern = Pattern::new(r"round-[1-9][0-9]*/review-(ux|ui)\.md", Flags::NONE);
const WAIT_USAGE: &str = "usage: wait <project-slug>/<job-id> <artifact>... [--timeout S] [--hold]";
const WATCH_USAGE: &str =
    "usage: watch <project-slug>/<job-id> [<artifact>...] [--pr] [--timeout S]";
const WATCH_TIMEOUT: u64 = 540;

/// True when line is a finished first line for artifact.
fn wait_valid(artifact: &str, line: &str) -> bool {
    if ["spec.md", "spec-evidence.md", "build.md"].contains(&artifact) {
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

pub fn wait_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let hold = |target: &str| {
        claims::hold(&Call {
            args: vec![target.to_string()],
            agents: None,
            connection: None,
        })
    };
    let origin = Instant::now();
    let now = || origin.elapsed().as_secs_f64();
    let sleep = |s: f64| thread::sleep(Duration::from_secs_f64(s));
    let io = Io {
        hold: &hold,
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
            match (timeout, timeout_arg(args.get(i + 1), WATCH_TIMEOUT, 1)) {
                (None, Some(t)) => timeout = Some(t),
                _ => {
                    return Err(fail(
                        format!("--timeout takes one value from 1 to 540 seconds. {WATCH_USAGE}"),
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
    if watch_pr {
        return Err(not_ported("watch --pr"));
    }
    let start = (io.now)();
    let deadline = start + timeout.unwrap_or(WATCH_TIMEOUT) as f64;
    let mut bad_before: BTreeMap<String, String> = BTreeMap::new();
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
        let event = if !artifacts.is_empty()
            && artifacts
                .iter()
                .all(|a| lines.get(*a).is_some_and(|l| wait_valid(a, l)))
        {
            "files"
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
        for a in &artifacts {
            files.insert(
                a.to_string(),
                lines
                    .get(*a)
                    .map_or(Value::Null, |l| Value::from(l.as_str())),
            );
        }
        let elapsed = ((io.now)() - start) as i64;
        let out = json!({"event": event, "files": files, "pr": null, "elapsed": elapsed});
        return Ok(format!("{}\n", dumps(&out, DEFAULT)));
    }
}

pub fn watch_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let session = std::env::var("FACTORY_SESSION").is_ok_and(|s| !s.is_empty());
    let hold = |target: &str| {
        claims::hold(&Call {
            args: vec![target.to_string()],
            agents: None,
            connection: None,
        })
    };
    let origin = Instant::now();
    let now = || origin.elapsed().as_secs_f64();
    let sleep = |s: f64| thread::sleep(Duration::from_secs_f64(s));
    let io = Io {
        hold: &hold,
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

pub fn round_wait_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("round-wait"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

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
            (3, "factory.py: still missing: decision.md\n")
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
    fn watch_pr_not_ported() {
        let (home, _) = home("watch-pr", "042-sample-export");
        let io = Io {
            hold: &no_hold,
            now: &|| 0.0,
            sleep: &|_| {},
        };
        let e = watch(&home, &["--pr"], &io).unwrap_err();
        assert_eq!(
            (e.code, e.stderr.as_str()),
            (
                2,
                "factory.py: watch --pr is not ported to the Rust binary yet\n"
            )
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
