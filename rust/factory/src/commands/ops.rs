//! The ops commands: clean, orphans, archive-workspace and pr-handled. stop is not ported yet.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::cli::{not_ported, Call};
use crate::shared::board::read_board;
use crate::shared::conf::{fm_get, frontmatter};
use crate::shared::consts::{PASEO_ID, TERMINAL};
use crate::shared::exit::{exit, fail, Res};
use crate::shared::fs::{is_link, resolve};
use crate::shared::home::{factory_home, holder};
use crate::shared::proc::{capture, git, paseo_run, which};
use crate::shared::project::{project_files, r2_target, read_paths};
use crate::shared::py::{os_error, repr, splitlines, strip, universal_newlines};
use crate::shared::pyjson::{dumps, loads, DEFAULT, INDENT2};
use crate::shared::pyre::{Flags, Pattern};

const USAGE_CLEAN: &str =
    "usage: clean <project-slug>/<job-id> node_modules | scratch <name> | temp";
static SCRATCH_NAME: Pattern = Pattern::new(r"[A-Za-z0-9][A-Za-z0-9._-]{0,99}", Flags::NONE);
const PR_HANDLED_USAGE: &str = "usage: pr-handled <project-slug>/<job-id> [--rearm] <sig>...";
const HANDLED_KEEP: usize = 32;
static HANDLED_SIG: Pattern =
    Pattern::new(r"(review|thread|check|conflict):[0-9a-f]{16}", Flags::NONE);
static TRAILING_NUM: Pattern = Pattern::new(r"-\d+$", Flags::NONE);
static CREATED: Pattern = Pattern::new("[0-9a-f]{40,64} branch: Created from .*", Flags::NONE);
const ORPHAN_KINDS: &[&str] = &["branch", "pr", "worktree", "workspace"];
const GIT_TIMEOUT: Duration = Duration::from_secs(20);

pub fn stop(_call: &Call) -> Res<()> {
    Err(not_ported("stop"))
}

pub fn pr_handled_cmd(call: &Call) -> Res<()> {
    let (home, _slug, _job_id) = r2_target(&call.args[0], true)?;
    let args = &call.args[1..];
    let sigs = if args[0] == "--rearm" {
        &args[1..]
    } else {
        args
    };
    if !(1..=HANDLED_KEEP).contains(&sigs.len()) || !sigs.iter().all(|s| HANDLED_SIG.is_full(s)) {
        return Err(fail(
            format!(
                "give 1 to {HANDLED_KEEP} signatures like check:0123456789abcdef. {PR_HANDLED_USAGE}"
            ),
            2,
        ));
    }
    holder(&home)?;
    Err(not_ported("pr-handled"))
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

/// An f-string `{value}`: text as is, None, True, False, else its JSON.
fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(v) => dumps(v, DEFAULT),
    }
}

/// `os.path.normpath`, plus `os.path.normcase` on Windows.
fn normpath(p: &str) -> String {
    let (text, sep) = if cfg!(windows) {
        (p.to_lowercase().replace('/', "\\"), '\\')
    } else {
        (p.to_string(), '/')
    };
    if text.is_empty() {
        return ".".into();
    }
    let lead: String = text.chars().take_while(|&c| c == sep).collect();
    let lead = if lead.len() == 2 {
        lead
    } else {
        lead.chars().take(1).collect()
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in text.split(sep) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part != ".." || (lead.is_empty() && parts.is_empty()) || parts.last() == Some(&"..") {
            parts.push(part);
        } else if !parts.is_empty() {
            parts.pop();
        }
    }
    let path = format!("{lead}{}", parts.join(&sep.to_string()));
    if path.is_empty() {
        ".".into()
    } else {
        path
    }
}

/// The last part of a path normpath gave.
fn basename(p: &str) -> &str {
    let sep = if cfg!(windows) { '\\' } else { '/' };
    p.rsplit(sep).next().unwrap_or(p)
}

/// `os.path.abspath` on this platform.
fn abspath(dir: &str, cwd: &Path) -> PathBuf {
    if cfg!(windows) {
        std::path::absolute(cwd.join(dir)).unwrap_or_else(|_| cwd.join(dir))
    } else if dir.starts_with('/') {
        PathBuf::from(normpath(dir))
    } else {
        PathBuf::from(normpath(&format!("{}/{dir}", cwd.display())))
    }
}

/// `tempfile.gettempdir()`: the first candidate folder a probe file can be written to.
fn temp_root() -> Res<PathBuf> {
    let mut dirs: Vec<String> = ["TMPDIR", "TEMP", "TMP"]
        .iter()
        .filter_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
        .collect();
    if cfg!(windows) {
        let profile = std::env::var("USERPROFILE").unwrap_or_else(|_| "~".into());
        let root = std::env::var("SYSTEMROOT").unwrap_or_else(|_| "%SYSTEMROOT%".into());
        dirs.push(format!(r"{profile}\AppData\Local\Temp"));
        dirs.push(format!(r"{root}\Temp"));
        dirs.extend([r"c:\temp", r"c:\tmp", r"\temp", r"\tmp"].map(String::from));
    } else {
        dirs.extend(["/tmp", "/var/tmp", "/usr/tmp"].map(String::from));
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    dirs.push(cwd.to_string_lossy().into_owned());
    for dir in &dirs {
        let dir = abspath(dir, &cwd);
        for n in 0..100 {
            let probe = dir.join(format!(".factory-probe-{}-{n}", std::process::id()));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&probe)
            {
                Ok(mut file) => {
                    let wrote = file.write_all(b"blat");
                    drop(file);
                    if fs::remove_file(&probe).is_ok() && wrote.is_ok() {
                        return Ok(dir);
                    }
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e)
                    if cfg!(windows)
                        && e.kind() == io::ErrorKind::PermissionDenied
                        && dir.is_dir() =>
                {
                    continue
                }
                Err(_) => break,
            }
        }
    }
    let listed: Vec<String> = dirs.iter().map(|d| repr(d)).collect();
    Err(fail(
        format!(
            "No usable temporary directory found in [{}]",
            listed.join(", ")
        ),
        1,
    ))
}

/// `exc.strerror or str(exc)`.
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

/// `is_link`, where Python lets an lstat error other than a missing path escape.
fn link(p: &Path) -> Res<bool> {
    is_link(p).map_err(|e| fail(os_error(&e, Some(p)), 1))
}

/// Remove one folder, or say why it stays.
fn remove(path: &Path, removed: &mut Vec<String>, left: &mut Vec<String>) {
    match fs::remove_dir_all(path) {
        Ok(()) => removed.push(path.display().to_string()),
        Err(e) => left.push(format!("{}: {}", path.display(), strerror(&e))),
    }
}

fn clean_temp(path: &Path) -> Res<(Vec<String>, Vec<String>)> {
    let (mut removed, mut left) = (Vec::new(), Vec::new());
    if link(path)? {
        left.push(format!(
            "{}: a symlink or junction, left in place",
            path.display()
        ));
    } else if fs::symlink_metadata(path).is_ok() && !path.is_dir() {
        left.push(format!("{}: not a folder, left in place", path.display()));
    } else if path.is_dir() {
        remove(path, &mut removed, &mut left);
    }
    Ok((removed, left))
}

fn clean_worktree(wt: &Path, what: &str, name: Option<&str>) -> Res<(Vec<String>, Vec<String>)> {
    let parent = wt.parent().unwrap();
    for path in [parent.parent().unwrap(), parent, wt] {
        if link(path)? {
            return Err(fail(
                format!(
                    "{} is a symlink or junction; nothing was removed",
                    path.display()
                ),
                6,
            ));
        }
    }
    if !wt.is_dir() {
        return Err(fail(
            format!("no worktree {}; nothing was removed", wt.display()),
            4,
        ));
    }
    let wt_text = wt.to_string_lossy();
    let (mut removed, mut left) = (Vec::new(), Vec::new());
    let targets: Vec<String> = if what == "node_modules" {
        let listed = capture(
            &[
                "git",
                "-C",
                &wt_text,
                "ls-files",
                "-z",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
            ],
            None,
            &[],
            Some(GIT_TIMEOUT),
        );
        let out = match listed {
            Ok(o) if o.code == 0 => o.stdout,
            _ => {
                return Err(fail(
                    format!(
                        "git could not list ignored folders in {}; nothing was removed",
                        wt.display()
                    ),
                    1,
                ))
            }
        };
        let mut targets: Vec<String> = out
            .split('\0')
            .filter(|entry| {
                !entry.is_empty()
                    && Path::new(entry.trim_end_matches('/')).file_name()
                        == Some("node_modules".as_ref())
            })
            .map(|entry| entry.trim_end_matches('/').to_string())
            .collect();
        targets.sort();
        targets
    } else {
        let name = name.unwrap();
        let rel = format!(".context/{name}");
        let path = wt.join(".context").join(name);
        if fs::symlink_metadata(&path).is_ok() {
            let ignored = capture(
                &["git", "-C", &wt_text, "check-ignore", "-q", &rel],
                None,
                &[],
                Some(GIT_TIMEOUT),
            )
            .is_ok_and(|o| o.code == 0);
            if ignored {
                vec![rel]
            } else {
                left.push(format!(
                    "{}: not ignored by git, left in place",
                    path.display()
                ));
                Vec::new()
            }
        } else {
            Vec::new()
        }
    };
    for rel in &targets {
        let parts: Vec<&std::ffi::OsStr> = Path::new(rel).iter().collect();
        let path = wt.join(parts.iter().collect::<PathBuf>());
        let mut linked = false;
        for i in 1..=parts.len() {
            if link(&wt.join(parts[..i].iter().collect::<PathBuf>()))? {
                linked = true;
                break;
            }
        }
        if linked {
            left.push(format!(
                "{}: a symlink or junction, left in place",
                path.display()
            ));
        } else if !path.is_dir() {
            left.push(format!("{}: not a folder, left in place", path.display()));
        } else {
            remove(&path, &mut removed, &mut left);
        }
    }
    Ok((removed, left))
}

fn print_lists(removed: Vec<String>, left: Vec<String>) -> Res<()> {
    let failed = !left.is_empty();
    println!(
        "{}",
        dumps(&json!({"removed": removed, "left": left}), INDENT2)
    );
    if failed {
        return Err(exit(1));
    }
    Ok(())
}

pub fn clean_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let what = call.args[1].as_str();
    let rest = &call.args[2..];
    let usable = match what {
        "node_modules" | "temp" => rest.is_empty(),
        "scratch" => rest.len() == 1 && SCRATCH_NAME.is_full(&rest[0]),
        _ => false,
    };
    if !usable {
        return Err(fail(USAGE_CLEAN, 2));
    }
    let (removed, left) = if what == "temp" {
        clean_temp(&temp_root()?.join(format!("factory-{slug}-{job_id}")))?
    } else {
        let wt = home.join("worktrees").join(&slug).join(&job_id);
        clean_worktree(&wt, what, rest.first().map(String::as_str))?
    };
    print_lists(removed, left)
}

/// (exit code as text, stdout) of gh <args>: ("1", "") when it cannot start, ("timeout", "") past 60 seconds.
fn gh_cli(gh: &Path, args: &[&str]) -> (String, String) {
    let gh = gh.to_string_lossy();
    let argv: Vec<&str> = std::iter::once(gh.as_ref())
        .chain(args.iter().copied())
        .collect();
    match capture(&argv, None, &[], Some(Duration::from_secs(60))) {
        Ok(o) => (o.code.to_string(), o.stdout),
        Err(e) if e.kind() == io::ErrorKind::TimedOut => ("timeout".into(), String::new()),
        Err(_) => ("1".into(), String::new()),
    }
}

/// Why no open board job owns job_id's leftovers, or None when one does.
fn orphan_reason(jobs: &[Value], job_id: &str) -> Option<&'static str> {
    let Some(job) = jobs
        .iter()
        .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id))
    else {
        return Some("no job on the board");
    };
    let stage = job.get("stage").and_then(Value::as_str);
    if !stage.is_some_and(|s| TERMINAL.contains(&s)) {
        return None;
    }
    Some(if stage == Some("merged") {
        "job merged"
    } else {
        "job closed"
    })
}

/// The paths.md slug whose checkout shares cwd's git common dir, first by slug, else None.
fn workspace_project(cwd: &str, paths: &[(String, String)]) -> Option<String> {
    let common = |path: &str| {
        let out = git(Path::new(path), &["rev-parse", "--git-common-dir"]);
        (!out.is_empty()).then(|| resolve(&Path::new(path).join(&out)).ok())?
    };
    let target = common(cwd)?;
    let mut sorted = paths.to_vec();
    sorted.sort();
    sorted
        .into_iter()
        .find(|(_, path)| common(path).as_ref() == Some(&target))
        .map(|(slug, _)| slug)
}

/// (orphans, skipped): factory branches on origin, open factory PRs, job worktrees and factory-<id>
/// Paseo workspaces that no open board job owns. Only reads.
pub(crate) fn orphan_scan(home: &Path) -> Res<(Vec<Value>, Vec<String>)> {
    let board = read_board(home)?;
    let jobs = board_jobs(&board)?;
    let paths = read_paths(home);
    let gh = which("gh");
    scan(home, jobs, &paths, gh.as_deref(), which("paseo").is_some())
}

/// The board's jobs, refusing anything but a list of objects, where Python would raise.
fn board_jobs(board: &Value) -> Res<&[Value]> {
    match board.get("jobs") {
        Some(Value::Array(jobs)) if jobs.iter().all(Value::is_object) => Ok(jobs),
        _ => Err(fail("the board's jobs is not a list of objects", 1)),
    }
}

fn scan(
    home: &Path,
    jobs: &[Value],
    paths: &[(String, String)],
    gh: Option<&Path>,
    paseo: bool,
) -> Res<(Vec<Value>, Vec<String>)> {
    let mut orphans: Vec<Value> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut add = |kind: &str, project: Value, job_id: &str, name: Value| {
        if let Some(reason) = orphan_reason(jobs, job_id) {
            orphans.push(
                json!({"kind": kind, "project": project, "job": job_id, "name": name, "reason": reason}),
            );
        }
    };

    let mut gh_missing = false;
    for (slug, path) in paths {
        if !Path::new(path).is_dir() {
            continue;
        }
        let listed = capture(
            &[
                "git",
                "-C",
                path,
                "ls-remote",
                "--heads",
                "origin",
                "refs/heads/factory/*",
            ],
            None,
            &[("GIT_TERMINAL_PROMPT", "0")],
            Some(GIT_TIMEOUT),
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
                        if let Some(job_id) = name.strip_prefix("factory/") {
                            add("branch", json!(slug), job_id, json!(name));
                        }
                    }
                }
            }
        }
        if gh_missing {
            continue;
        }
        let md = project_files(home, slug)?.0;
        let repo = if md.exists() {
            let meta = frontmatter(&md).map(|(m, _)| m).unwrap_or_default();
            fm_get(&meta, "repo").unwrap_or("").to_string()
        } else {
            String::new()
        };
        if repo.is_empty() {
            skipped.push(format!("prs {slug}: the project file names no repo"));
            continue;
        }
        let Some(gh) = gh else {
            gh_missing = true;
            skipped.push("prs: gh is not on PATH".into());
            continue;
        };
        let (code, out) = gh_cli(
            gh,
            &[
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
            ],
        );
        let prs = if code == "0" { loads(&out).ok() } else { None };
        let Some(Value::Array(prs)) = prs else {
            skipped.push(format!("prs {slug}: gh pr list failed (exit {code})"));
            continue;
        };
        for pr in &prs {
            let head = pr.get("headRefName").and_then(Value::as_str);
            if let Some(job_id) = head.and_then(|h| h.strip_prefix("factory/")) {
                add(
                    "pr",
                    json!(slug),
                    job_id,
                    pr.get("url").cloned().unwrap_or(Value::Null),
                );
            }
        }
    }

    let root = home.join("worktrees");
    let mut worktrees = |skipped: &mut Vec<String>| -> Result<(), PathBuf> {
        let list = |dir: &Path| -> Result<Vec<PathBuf>, PathBuf> {
            let mut found = fs::read_dir(dir)
                .and_then(|entries| {
                    entries
                        .map(|e| e.map(|e| e.path()))
                        .collect::<io::Result<Vec<_>>>()
                })
                .map_err(|_| dir.to_path_buf())?;
            found.sort();
            Ok(found)
        };
        let linked = |p: &Path| is_link(p).map_err(|_| p.to_path_buf());
        let projects = if linked(&root)? {
            vec![root.clone()]
        } else if root.is_dir() {
            list(&root)?
        } else {
            Vec::new()
        };
        for project in projects {
            if linked(&project)? {
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
            let project_name = project
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            for folder in folders {
                if linked(&folder)? {
                    skipped.push(format!(
                        "worktrees: {} is a symlink or junction",
                        folder.display()
                    ));
                } else if folder.is_dir() {
                    let name = folder.file_name().unwrap().to_string_lossy();
                    add(
                        "worktree",
                        json!(project_name),
                        &name,
                        json!(folder.display().to_string()),
                    );
                }
            }
        }
        Ok(())
    };
    if let Err(path) = worktrees(&mut skipped) {
        skipped.push(format!("worktrees: could not read {}", path.display()));
    }

    if !paseo {
        skipped.push("workspaces: paseo is not on PATH".into());
    } else {
        let (code, out) = paseo_run(&["workspace", "ls", "--json"]);
        let spaces = if code == 0 { loads(&out).ok() } else { None };
        let spaces = match spaces {
            Some(Value::Array(spaces)) => spaces,
            _ => {
                skipped.push(format!(
                    "workspaces: paseo workspace ls failed (exit {code})"
                ));
                Vec::new()
            }
        };
        for space in &spaces {
            let Some(cwd) = space.get("cwd").and_then(Value::as_str) else {
                continue;
            };
            let normal = normpath(cwd);
            let Some(rest) = basename(&normal).strip_prefix("factory-") else {
                continue;
            };
            let known = jobs
                .iter()
                .any(|j| j.get("id").and_then(Value::as_str) == Some(rest));
            let job_id = if known {
                rest.to_string()
            } else {
                TRAILING_NUM.replace_all(rest, |_| String::new())
            };
            let job = jobs
                .iter()
                .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id.as_str()));
            let project = job.and_then(|j| j.get("project"));
            let project = if truthy(project) {
                project.cloned().unwrap()
            } else {
                json!(workspace_project(cwd, paths))
            };
            add("workspace", project, &job_id, json!(cwd));
        }
    }

    let key = |o: &Value| {
        let kind = o["kind"].as_str().unwrap_or_default();
        let index = ORPHAN_KINDS.iter().position(|k| *k == kind).unwrap_or(0);
        (index, o["name"].as_str().unwrap_or_default().to_string())
    };
    orphans.sort_by_key(key);
    Ok((orphans, skipped))
}

pub fn orphans_cmd(_call: &Call) -> Res<()> {
    let (orphans, skipped) = orphan_scan(&factory_home()?)?;
    println!(
        "{}",
        dumps(&json!({"orphans": orphans, "skipped": skipped}), INDENT2)
    );
    Ok(())
}

fn paseo_server() -> Option<String> {
    let base = match std::env::var_os("PASEO_HOME").filter(|v| !v.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => std::env::home_dir()?.join(".paseo"),
    };
    let text = String::from_utf8(fs::read(base.join("server-id")).ok()?).ok()?;
    let server = strip(&universal_newlines(text)).to_string();
    PASEO_ID.is_full(&server).then_some(server)
}

/// (server, agent) of the Paseo session running this process, else None.
fn paseo_session() -> Option<(String, String)> {
    let agent = std::env::var("PASEO_AGENT_ID").unwrap_or_default();
    let agent = strip(&agent);
    let server = paseo_server()?;
    (PASEO_ID.is_full(agent) && !agent.starts_with('-')).then(|| (server, agent.to_string()))
}

/// Why archive-workspace stops before reading the board, if it does.
fn early_reason(session: bool, paseo: Option<&Path>) -> Option<&'static str> {
    if !session {
        Some("not a Paseo session")
    } else if paseo.is_none() {
        Some("paseo is not on PATH")
    } else {
        None
    }
}

/// What a workspace holds that no remote, other local branch or the branch's creation point keeps.
fn workspace_leftovers(cwd: &str) -> Vec<String> {
    let git = |verb: &str, args: &[&str]| -> Result<Vec<String>, String> {
        let argv: Vec<&str> = ["git", "-C", cwd, verb]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        match capture(&argv, None, &[], Some(GIT_TIMEOUT)) {
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                Err(format!("git {verb} failed: timed out after 20 s"))
            }
            Err(_) => Err(format!("git {verb} failed: could not run git")),
            Ok(o) if o.code != 0 => Err(format!("git {verb} failed: exit {}", o.code)),
            Ok(o) => Ok(splitlines(&o.stdout, false)
                .into_iter()
                .map(String::from)
                .collect()),
        }
    };
    let mut found: Vec<String> = match git("status", &["--porcelain"]) {
        Err(failed) => vec![failed],
        Ok(lines) => lines.iter().map(|l| format!("uncommitted: {l}")).collect(),
    };
    match git("branch", &["--show-current"]) {
        Err(failed) => found.push(failed),
        Ok(branch) => {
            let mut args: Vec<String> = ["--format=%h %s", "HEAD", "--not", "--remotes"]
                .map(String::from)
                .to_vec();
            if let Some(name) = branch.first() {
                args.push(format!("--exclude={name}"));
            }
            args.push("--branches".into());
            if let Some(name) = branch.first() {
                let reference = format!("refs/heads/{name}");
                match git("reflog", &["show", "--format=%H %gs", &reference]) {
                    Err(failed) => found.push(failed),
                    Ok(lines) => {
                        if let Some(last) = lines.last().filter(|l| CREATED.is_full(l)) {
                            args.push(last.split(' ').next().unwrap().to_string());
                        }
                    }
                }
            }
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            match git("log", &args) {
                Err(failed) => found.push(failed),
                Ok(lines) => found.extend(lines.iter().map(|l| format!("commit: {l}"))),
            }
        }
    }
    let more = found.len().saturating_sub(20);
    found.truncate(20);
    if more > 0 {
        found.push(format!("and {more} more"));
    }
    found
}

pub fn archive_workspace(call: &Call) -> Res<()> {
    let flags = &call.args[1..];
    let check = flags == ["--check"];
    if !flags.is_empty() && !check {
        return Err(fail(
            "usage: archive-workspace <project-slug>/<job-id> [--check]",
            2,
        ));
    }
    let (home, _slug, job_id) = r2_target(&call.args[0], true)?;
    let session = paseo_session();
    if let Some(reason) = early_reason(session.is_some(), which("paseo").as_deref()) {
        println!(
            "{}",
            dumps(&json!({"archived": null, "reason": reason}), DEFAULT)
        );
        return Ok(());
    }
    let (_, agent) = session.unwrap();
    let board = read_board(&home)?;
    let found = board_jobs(&board)?
        .iter()
        .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id.as_str()));
    let Some(found) = found else {
        return Err(fail(
            format!("no job {job_id} on the board. Nothing was written."),
            4,
        ));
    };
    let stage = found.get("stage");
    if !stage
        .and_then(Value::as_str)
        .is_some_and(|s| s == "merged" || s == "closed")
    {
        return Err(fail(
            format!("{job_id} is at {}, not merged or closed", py_str(stage)),
            2,
        ));
    }
    let (code, out) = paseo_run(&["inspect", &agent, "--json"]);
    let cwd = if code == 0 {
        loads(&out)
            .ok()
            .and_then(|v| v.get("Cwd")?.as_str().map(String::from))
    } else {
        None
    };
    let Some(cwd) = cwd else {
        return Err(fail(format!("paseo inspect failed: exit {code}"), 1));
    };
    let (code, out) = paseo_run(&["workspace", "ls", "--json"]);
    let spaces = if code == 0 { loads(&out).ok() } else { None };
    let Some(Value::Array(spaces)) = spaces else {
        return Err(fail(format!("paseo workspace ls failed: exit {code}"), 1));
    };
    let here = normpath(&cwd);
    let space = spaces.iter().find(|w| {
        w.get("cwd")
            .and_then(Value::as_str)
            .is_some_and(|c| normpath(c) == here)
    });
    let Some(space) = space else {
        let reason = format!("no Paseo workspace at {cwd}");
        println!(
            "{}",
            dumps(&json!({"archived": null, "reason": reason}), DEFAULT)
        );
        return Ok(());
    };
    let wks = space.get("workspaceId");
    let id = wks
        .and_then(Value::as_str)
        .filter(|w| PASEO_ID.is_full(w) && !w.starts_with('-'));
    let ours = basename(&here)
        .strip_prefix(&format!("factory-{job_id}"))
        .is_some_and(|rest| {
            rest.is_empty()
                || rest
                    .strip_prefix('-')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        });
    let (Some(wks), true, true) = (
        id,
        space.get("isolation").and_then(Value::as_str) == Some("worktree"),
        ours,
    ) else {
        let reason = format!(
            "workspace {} is not this job's worktree workspace",
            py_str(wks)
        );
        println!(
            "{}",
            dumps(&json!({"archived": null, "reason": reason}), DEFAULT)
        );
        return Ok(());
    };
    let leftovers = workspace_leftovers(&cwd);
    if check {
        println!(
            "{}",
            dumps(
                &json!({"archived": null, "would_archive": wks, "leftovers": leftovers}),
                DEFAULT
            )
        );
        return Ok(());
    }
    println!(
        "{}",
        dumps(&json!({"archived": wks, "leftovers": leftovers}), DEFAULT)
    );
    let _ = io::stdout().flush();
    let (code, _) = paseo_run(&["workspace", "archive", wks]);
    if code != 0 {
        return Err(fail(
            format!("paseo workspace archive {wks} exit {code}"),
            1,
        ));
    }
    let (code, out) = paseo_run(&["inspect", &agent, "--json"]);
    let archived = code == 0
        && loads(&out)
            .ok()
            .is_some_and(|v| v.get("Archived") == Some(&Value::Bool(true)));
    if !archived {
        let (code, _) = paseo_run(&["archive", &agent, "--force"]);
        if code != 0 {
            return Err(fail(
                format!("workspace {wks} archived, but agent archive failed: paseo archive {agent} exit {code}"),
                1,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;

    fn run_git(wt: &Path, args: &[&str]) {
        let wt = wt.to_string_lossy();
        let base = [
            "git",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-C",
            &wt,
        ];
        let argv: Vec<&str> = base.iter().chain(args).copied().collect();
        assert_eq!(capture(&argv, None, &[], None).unwrap().code, 0, "{args:?}");
    }

    /// A worktree at <home>/worktrees/sample/001-test that ignores node_modules and .context.
    fn worktree(name: &str) -> (PathBuf, PathBuf) {
        let home = temp_dir(name);
        let wt = home.join("worktrees/sample/001-test");
        fs::create_dir_all(wt.join("sub")).unwrap();
        run_git(&wt, &["init", "-q"]);
        fs::write(
            wt.join(".gitignore"),
            "/node_modules/\n/sub/node_modules/\n/.context/\n",
        )
        .unwrap();
        fs::write(wt.join("sub/tracked.txt"), "tracked\n").unwrap();
        run_git(&wt, &["add", "."]);
        run_git(&wt, &["commit", "-q", "-m", "base"]);
        (home, wt)
    }

    fn folder(wt: &Path, rel: &str) -> PathBuf {
        let path = wt.join(rel);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("file.js"), "x\n").unwrap();
        path
    }

    fn text(p: &Path) -> String {
        p.display().to_string()
    }

    #[test]
    fn clean_node_modules_removes_ignored_folders() {
        let (home, wt) = worktree("clean-modules");
        let (top, sub) = (folder(&wt, "node_modules"), folder(&wt, "sub/node_modules"));
        folder(&wt, ".context/capture-1");
        let got = clean_worktree(&wt, "node_modules", None).unwrap();
        assert_eq!(got, (vec![text(&top), text(&sub)], vec![]));
        assert!(!top.exists() && !sub.exists());
        assert!(wt.join("sub/tracked.txt").is_file() && wt.join(".gitignore").is_file());
        assert!(wt.join(".context/capture-1/file.js").is_file());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn clean_scratch_removes_one_folder() {
        let (home, wt) = worktree("clean-scratch");
        let (one, other) = (
            folder(&wt, ".context/capture-1"),
            folder(&wt, ".context/capture-2"),
        );
        let modules = folder(&wt, "node_modules");
        let got = clean_worktree(&wt, "scratch", Some("capture-1")).unwrap();
        assert_eq!(got, (vec![text(&one)], vec![]));
        assert!(!one.exists() && other.is_dir() && modules.is_dir());
        let none = clean_worktree(&wt, "scratch", Some("absent")).unwrap();
        assert_eq!(none, (vec![], vec![]));
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn clean_links_are_left_in_place() {
        use std::os::unix::fs::symlink;
        let (home, wt) = worktree("clean-links");
        let outside = home.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.txt"), "keep\n").unwrap();
        symlink(&outside, wt.join("node_modules")).unwrap();
        fs::write(wt.join(".gitignore"), "/node_modules\n/.context/\n").unwrap();
        fs::create_dir(wt.join(".context")).unwrap();
        symlink(&outside, wt.join(".context/capture-1")).unwrap();
        let got = clean_worktree(&wt, "node_modules", None).unwrap();
        let left = format!(
            "{}: a symlink or junction, left in place",
            text(&wt.join("node_modules"))
        );
        assert_eq!(got, (vec![], vec![left]));
        let got = clean_worktree(&wt, "scratch", Some("capture-1")).unwrap();
        let left = format!(
            "{}: a symlink or junction, left in place",
            text(&wt.join(".context/capture-1"))
        );
        assert_eq!(got, (vec![], vec![left]));
        assert!(outside.join("keep.txt").is_file());
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn clean_temp_link_is_left() {
        let home = temp_dir("clean-temp-link");
        let path = home.join("factory-sample-009-x");
        assert_eq!(clean_temp(&path).unwrap(), (vec![], vec![]));
        fs::write(&path, "x\n").unwrap();
        let left = format!("{}: not a folder, left in place", text(&path));
        assert_eq!(clean_temp(&path).unwrap(), (vec![], vec![left]));
        fs::remove_file(&path).unwrap();
        let outside = home.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.txt"), "keep\n").unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        let left = format!("{}: a symlink or junction, left in place", text(&path));
        assert_eq!(clean_temp(&path).unwrap(), (vec![], vec![left]));
        assert!(outside.join("keep.txt").is_file());
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn clean_worktree_link_exits_6() {
        let (home, wt) = worktree("clean-wt-link");
        let link = home.join("worktrees/sample/003-link");
        std::os::unix::fs::symlink(&wt, &link).unwrap();
        let modules = folder(&wt, "node_modules");
        let e = clean_worktree(&link, "node_modules", None).unwrap_err();
        assert_eq!(e.code, 6);
        assert_eq!(
            e.stderr,
            format!(
                "factory.py: {} is a symlink or junction; nothing was removed\n",
                text(&link)
            )
        );
        assert!(modules.is_dir());
        let missing = home.join("worktrees/sample/002-absent");
        let e = clean_worktree(&missing, "node_modules", None).unwrap_err();
        assert_eq!(e.code, 4);
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn clean_reports_the_os_error() {
        use std::os::unix::fs::PermissionsExt;
        let (home, wt) = worktree("clean-os-error");
        let path = folder(&wt, ".context/capture-1");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();
        let root = fs::write(path.join("probe"), "x").is_ok();
        let got = (!root).then(|| clean_worktree(&wt, "scratch", Some("capture-1")).unwrap());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir_all(home).unwrap();
        if let Some(got) = got {
            let left = format!("{}: Permission denied", text(&path));
            assert_eq!(got, (vec![], vec![left]));
        }
    }

    #[test]
    fn orphans_skip_missing_tools() {
        let home = temp_dir("orphans-tools");
        let bare = home.join("bare");
        fs::create_dir_all(home.join("projects")).unwrap();
        fs::create_dir(&bare).unwrap();
        for slug in ["a", "b"] {
            fs::write(
                home.join(format!("projects/{slug}.md")),
                "---\nrepo: example/a\n---\n",
            )
            .unwrap();
        }
        let bare = text(&bare);
        let paths = vec![("a".to_string(), bare.clone()), ("b".to_string(), bare)];
        let (orphans, skipped) = scan(&home, &[], &paths, None, false).unwrap();
        assert!(orphans.is_empty());
        assert_eq!(
            skipped,
            [
                "branches a: git ls-remote failed (exit 128)",
                "prs: gh is not on PATH",
                "branches b: git ls-remote failed (exit 128)",
                "workspaces: paseo is not on PATH",
            ]
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn orphans_skip_unreadable_worktrees() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_dir("orphans-unreadable");
        let project = home.join("worktrees/factory");
        fs::create_dir_all(project.join("2031-stale")).unwrap();
        fs::set_permissions(&project, fs::Permissions::from_mode(0o000)).unwrap();
        let root = fs::read_dir(&project).is_ok();
        let got = (!root).then(|| scan(&home, &[], &[], None, false).unwrap());
        fs::set_permissions(&project, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir_all(&home).unwrap();
        if let Some((orphans, skipped)) = got {
            assert!(orphans.is_empty());
            assert_eq!(
                skipped,
                [
                    format!("worktrees: could not read {}", text(&project)),
                    "workspaces: paseo is not on PATH".to_string(),
                ]
            );
        }
    }

    #[test]
    fn archive_early_reasons() {
        let paseo = Some(Path::new("/bin/paseo"));
        assert_eq!(early_reason(false, paseo), Some("not a Paseo session"));
        assert_eq!(early_reason(true, None), Some("paseo is not on PATH"));
        assert_eq!(early_reason(true, paseo), None);
    }

    #[test]
    fn orphans_and_archive_refuse_a_malformed_board() {
        let merged = json!({"id": "042-dark-mode", "project": "sample", "stage": "merged"});
        for board in [
            json!({}),
            json!([1]),
            json!({"jobs": {}}),
            json!({"jobs": ["x"]}),
            json!({"jobs": [5, merged]}),
        ] {
            let e = board_jobs(&board).unwrap_err();
            assert_eq!(e.code, 1, "{board}");
            assert_eq!(
                e.stderr,
                "factory.py: the board's jobs is not a list of objects\n"
            );
        }
        let board = json!({"jobs": [merged]});
        assert_eq!(board_jobs(&board).unwrap(), &[merged]);
    }

    #[cfg(unix)]
    #[test]
    fn archive_normpath_matches_python() {
        let rows = [
            ("/a//b/./c/../d", "/a/b/d"),
            ("//x/y", "//x/y"),
            ("///x", "/x"),
            ("/..", "/"),
            ("a/../..", ".."),
            (
                "/work/factory-042-dark-mode/",
                "/work/factory-042-dark-mode",
            ),
            ("", "."),
        ];
        for (given, want) in rows {
            assert_eq!(normpath(given), want, "{given}");
        }
        assert_eq!(basename(&normpath("/work/factory-1/")), "factory-1");
    }

    #[test]
    fn archive_py_str_formats_like_python() {
        assert_eq!(py_str(None), "None");
        assert_eq!(py_str(Some(&Value::Null)), "None");
        assert_eq!(py_str(Some(&json!("-rf"))), "-rf");
        assert_eq!(py_str(Some(&json!(true))), "True");
        assert_eq!(py_str(Some(&json!(7))), "7");
    }
}
