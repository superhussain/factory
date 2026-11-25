//! The worktree and cleanup commands, plus the checkout, prebuild and stop helpers pickup and ops share.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cli::Call;
use crate::shared::board_client;
use crate::shared::conf::{fm_get, frontmatter};
use crate::shared::exit::{exit, fail, Res};
use crate::shared::fs::{contained, is_link, resolve};
use crate::shared::home::device_name;
use crate::shared::proc::{capture, git, git_ok, origin_git, which};
use crate::shared::project::{project_files, r2_target, read_paths};
use crate::shared::py::{is_space, os_error, repr, strip};
use crate::shared::pyjson::{dumps, loads, INDENT2};
use crate::shared::pyre::{Flags, Pattern};

static REPO_PATH: Pattern = Pattern::new(r"[A-Za-z0-9_./-]{1,200}", Flags::ASCII);
static COMMIT: Pattern = Pattern::new(r"[0-9a-f]{40}", Flags::ASCII);
static GH_PR_PARTS: Pattern = Pattern::new(
    r"https://github\.com/([\w.-]+)/([\w.-]+)/pull/(\d{1,9})",
    Flags::ASCII,
);
static USE_FLAG: Pattern = Pattern::new(r", use --\w+ to .*", Flags::NONE);
static PREBUILD_DIR: Pattern = Pattern::new(r"[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*", Flags::NONE);
// Checked at the worktree root, every folder between it and a listed folder, and the listed folder itself. The
// *mise* globs cover every mise config, rc, lock and conf.d name, such as .miserc.toml and .config/miserc.toml.
const PREBUILD_CONFIG_FILES: &[&str] = &[
    ":(glob)*mise*",
    ":(glob)*mise*/**",
    ":(glob).config/*mise*",
    ":(glob).config/*mise*/**",
    ".tool-versions",
    ".npmrc",
    "pnpm-workspace.yaml",
];
const PREBUILD_DIR_FILES: &[&str] = &[
    "pnpm-lock.yaml",
    "package.json",
    ".pnpmfile.cjs",
    ".pnpmfile.mjs",
];
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const PREBUILD_TIMEOUT: Duration = Duration::from_secs(600);
const LSOF_FALLBACKS: &[&str] = &["/usr/sbin/lsof", "/usr/bin/lsof"];
const GIT_TIMEOUT: Duration = Duration::from_secs(20);
const GH_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_WAIT: Duration = Duration::from_secs(5);
const STOP_POLL: Duration = Duration::from_millis(500);

#[cfg(test)]
thread_local! {
    /// Shorter FETCH_TIMEOUT and GIT_TIMEOUT for this test thread, so a timeout test need not wait them out.
    static TEST_FETCH_TIMEOUT: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
    static TEST_GIT_TIMEOUT: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

fn fetch_timeout() -> Duration {
    #[cfg(test)]
    if let Some(limit) = TEST_FETCH_TIMEOUT.with(std::cell::Cell::get) {
        return limit;
    }
    FETCH_TIMEOUT
}

fn git_timeout() -> Duration {
    #[cfg(test)]
    if let Some(limit) = TEST_GIT_TIMEOUT.with(std::cell::Cell::get) {
        return limit;
    }
    GIT_TIMEOUT
}

fn words(s: &str) -> impl Iterator<Item = &str> {
    s.split(is_space).filter(|w| !w.is_empty())
}

fn os_fail(e: &io::Error, path: &Path) -> crate::shared::exit::Exit {
    fail(os_error(e, Some(path)), 1)
}

/// `check_path(name) or name.startswith("-")`.
fn unsafe_branch(name: &str) -> bool {
    !REPO_PATH.is_full(name)
        || name.starts_with('/')
        || name.split('/').any(|p| p == "..")
        || name.starts_with('-')
}

/// Why repo's local base branch, or the given job branch, holds history origin's base does not descend from, as an
/// "unsafe checkout:" line, or None. It fetches origin's base first, so a stale origin/<base> cannot vouch for itself.
pub(crate) fn checkout_problem(
    home: &Path,
    slug: &str,
    repo: &str,
    branch: Option<&str>,
) -> Res<Option<String>> {
    let project = project_files(home, slug)?.0;
    let base = if project.exists() {
        let meta = frontmatter(&project).map_err(|e| os_fail(&e, &project))?.0;
        fm_get(&meta, "base").unwrap_or("main").to_string()
    } else {
        "main".to_string()
    };
    if unsafe_branch(&base) {
        return Ok(Some(format!(
            "unsafe checkout: {slug} base {base} is not a safe branch name"
        )));
    }
    // git's stderr can carry origin's URL, credentials included, so neither fetch line includes it.
    let refspec = format!("+refs/heads/{base}:refs/remotes/origin/{base}");
    let argv = ["git", "-C", repo, "fetch", "-q", "origin", &refspec];
    match capture(
        &argv,
        None,
        &[("GIT_TERMINAL_PROMPT", "0")],
        Some(fetch_timeout()),
    ) {
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            let secs = fetch_timeout().as_secs();
            return Ok(Some(format!(
                "unsafe checkout: git fetch origin {base} timed out after {secs} s in {repo}"
            )));
        }
        Err(e) => return Err(fail(os_error(&e, None), 1)),
        Ok(r) if r.code != 0 => {
            let code = r.code;
            return Ok(Some(format!(
                "unsafe checkout: git fetch origin {base} failed in {repo} (exit {code})"
            )));
        }
        Ok(_) => {}
    }
    let repo_path = Path::new(repo);
    let roots_out = git(
        repo_path,
        &[
            "rev-list",
            "--max-parents=0",
            &format!("refs/remotes/origin/{base}"),
        ],
    );
    let roots: Vec<&str> = words(&roots_out).collect();
    if roots.is_empty() {
        return Ok(Some(format!(
            "unsafe checkout: origin/{base} has no root commit in {repo}"
        )));
    }
    if git(
        repo_path,
        &["rev-parse", "--verify", "-q", &format!("refs/heads/{base}")],
    )
    .is_empty()
    {
        return Ok(Some(format!(
            "unsafe checkout: {repo} has no local {base} branch; run git -C {repo} branch {base} origin/{base}"
        )));
    }
    let mut refs = vec![base.as_str()];
    if let Some(branch) = branch.filter(|b| !b.is_empty()) {
        if !git(
            repo_path,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}"),
            ],
        )
        .is_empty()
        {
            refs.push(branch);
        }
    }
    for r in refs {
        let out = git(
            repo_path,
            &["rev-list", "--max-parents=0", &format!("refs/heads/{r}")],
        );
        let mut foreign: Vec<&str> = words(&out).filter(|c| !roots.contains(c)).collect();
        foreign.sort_unstable();
        foreign.dedup();
        if let Some(first) = foreign.first() {
            let short: String = first.chars().take(12).collect();
            return Ok(Some(format!(
                "unsafe checkout: {repo} {r} has root {short}, which origin/{base} lacks, so it holds archive \
                 history; repoint it as docs/public-cutover.md \"Other hosts\" says"
            )));
        }
    }
    Ok(None)
}

/// This device's checkout of slug, from paths.md, failing 4 without one.
fn checkout_of(home: &Path, slug: &str) -> Res<String> {
    let paths = read_paths(home)?;
    match fm_get(&paths, slug).filter(|r| !r.is_empty()) {
        Some(repo) => Ok(repo.to_string()),
        None => Err(fail(
            format!(
                "{} has no entry for {slug}: run /factory init in this device's checkout of it",
                home.join("paths.md").display()
            ),
            4,
        )),
    }
}

/// Create or fast-forward a job's worktree from origin/factory/<id>, then return its path.
fn worktree_in(home: &Path, slug: &str, job_id: &str) -> Res<PathBuf> {
    let repo = checkout_of(home, slug)?;
    let branch = format!("factory/{job_id}");
    if let Some(problem) = checkout_problem(home, slug, &repo, Some(&branch))? {
        return Err(fail(problem, 6));
    }
    let wt = home.join("worktrees").join(slug).join(job_id);
    let wt_text = wt.to_string_lossy().into_owned();
    let repo = Path::new(&repo);
    // ls-remote prints nothing when origin lacks the branch.
    if !origin_git(
        repo,
        &["ls-remote", "origin", &format!("refs/heads/{branch}")],
    )?
    .is_empty()
    {
        origin_git(
            repo,
            &[
                "fetch",
                "origin",
                &format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
            ],
        )?;
    }
    let origin = git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("refs/remotes/origin/{branch}"),
        ],
    );
    if !wt.exists() {
        if !git(
            repo,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}"),
            ],
        )
        .is_empty()
        {
            git_ok(repo, &["worktree", "add", &wt_text, &branch])?;
        } else if !origin.is_empty() {
            git_ok(
                repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    &branch,
                    &wt_text,
                    &format!("origin/{branch}"),
                ],
            )?;
        } else {
            return Err(fail(format!("no {branch} branch here or on origin"), 4));
        }
    }
    if !origin.is_empty() {
        let counts = git_ok(
            &wt,
            &[
                "rev-list",
                "--left-right",
                "--count",
                &format!("HEAD...origin/{branch}"),
            ],
        )?;
        let mut counts = words(&counts);
        let (ahead, behind) = (counts.next().unwrap_or(""), counts.next().unwrap_or(""));
        if behind != "0" {
            if ahead != "0" || !git_ok(&wt, &["status", "--porcelain"])?.is_empty() {
                let dirty = if ahead != "0" {
                    ""
                } else {
                    " with uncommitted changes"
                };
                return Err(fail(
                    format!("{wt_text} is {ahead} ahead and {behind} behind origin/{branch}{dirty}. Nothing was changed"),
                    6,
                ));
            }
            git_ok(&wt, &["merge", "--ff-only", &format!("origin/{branch}")])?;
        }
    }
    prebuild(home, slug, job_id, &wt)?;
    Ok(wt)
}

pub fn worktree(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let wt = worktree_in(&home, &slug, &job_id)?;
    println!("{}", wt.display());
    Ok(())
}

/// `data["version"] == 1`, which True and 1.0 also pass.
fn is_one(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() == Some(1.0),
        _ => false,
    }
}

/// The folders <home>/prebuild.json lists for slug. A dir that could leave the worktree is skipped.
fn prebuild_dirs(home: &Path, slug: &str) -> Vec<String> {
    let path = home.join("prebuild.json");
    let bytes = match fs::read(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        other => other,
    };
    let dirs = bytes
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|text| {
            let data = loads(&text).ok()?;
            let data = data.as_object()?;
            if !is_one(data.get("version")?) {
                return None;
            }
            let Some(dirs) = data.get("prebuild")?.as_object()?.get(slug) else {
                return Some(Vec::new());
            };
            dirs.as_array()?
                .iter()
                .map(|d| d.as_str().map(String::from))
                .collect::<Option<Vec<String>>>()
        });
    let Some(dirs) = dirs else {
        eprintln!(
            "cannot read {}: use {{\"version\": 1, \"prebuild\": {{\"<project>\": [\"<dir>\", ...]}}}}",
            path.display()
        );
        return Vec::new();
    };
    let ok: Vec<String> = dirs
        .iter()
        .filter(|d| PREBUILD_DIR.is_full(d) && !d.split('/').any(|p| p == ".."))
        .cloned()
        .collect();
    for d in &dirs {
        if !ok.contains(d) {
            eprintln!("prebuild skips {d}: not inside the worktree");
        }
    }
    ok
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return -signal;
    }
    status.code().unwrap_or(1)
}

/// argv's exit code, run in cwd with stdout and stderr appended to log, or why it did not finish.
fn run_logged(argv: &[String], cwd: &Path, log: &Path, timeout: Duration) -> Result<i32, String> {
    let folder = log.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(folder).map_err(|e| os_error(&e, Some(folder)))?;
    let out = OpenOptions::new()
        .append(true)
        .create(true)
        .open(log)
        .map_err(|e| os_error(&e, Some(log)))?;
    let err = out.try_clone().map_err(|e| os_error(&e, Some(log)))?;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .map_err(|e| os_error(&e, Some(Path::new(&argv[0]))))?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| os_error(&e, None))? {
            return Ok(exit_code(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("timed out after {} s", timeout.as_secs()));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// The loop of `prebuild`, with mise and pnpm given. Prints each line and returns them.
fn prebuild_with(
    home: &Path,
    slug: &str,
    job_id: &str,
    wt: &Path,
    mise: Option<PathBuf>,
    pnpm: Option<PathBuf>,
) -> Res<Vec<String>> {
    let mut lines = Vec::new();
    let say = |lines: &mut Vec<String>, line: String| {
        eprintln!("{line}");
        lines.push(line);
    };
    let dirs = prebuild_dirs(home, slug);
    if dirs.is_empty() {
        return Ok(lines);
    }
    let project = project_files(home, slug)?.0;
    let meta = frontmatter(&project).map_err(|e| os_fail(&e, &project))?.0;
    let base = fm_get(&meta, "base")
        .filter(|b| !b.is_empty())
        .unwrap_or("main");
    let log = home.join("logs").join(format!("prebuild-{job_id}.log"));
    let wt_text = wt.to_string_lossy();
    for d in &dirs {
        let folder = wt.join(d);
        if folder.join("node_modules").join(".modules.yaml").exists() {
            continue;
        }
        let inside = match (resolve(wt), resolve(&folder)) {
            (Ok(root), Ok(inner)) => inner != root && contained(&root, &inner),
            _ => false,
        };
        if !inside {
            say(
                &mut lines,
                format!("prebuild skips {d}: not inside the worktree"),
            );
            continue;
        }
        if !folder.join("pnpm-lock.yaml").is_file() {
            say(&mut lines, format!("prebuild skips {d}: no pnpm-lock.yaml"));
            continue;
        }
        // The mise, npmrc and pnpm files a builder could change, checked in the working tree, untracked and
        // gitignored ones too.
        let parts: Vec<&str> = d.split('/').collect();
        let mut files: Vec<String> = Vec::new();
        for i in 0..=parts.len() {
            let prefix = if i == 0 {
                String::new()
            } else {
                parts[..i].join("/") + "/"
            };
            for name in PREBUILD_CONFIG_FILES {
                files.push(match name.strip_prefix(":(glob)") {
                    Some(rest) => format!(":(glob){prefix}{rest}"),
                    None => format!("{prefix}{name}"),
                });
            }
        }
        files.extend(PREBUILD_DIR_FILES.iter().map(|name| format!("{d}/{name}")));
        let files: Vec<&str> = files.iter().map(String::as_str).collect();
        let origin_base = format!("refs/remotes/origin/{base}");
        let diff_argv = [
            &[
                "git",
                "-C",
                &wt_text,
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--quiet",
                &origin_base,
                "--",
            ],
            &files[..],
        ]
        .concat();
        let status_argv = [
            &[
                "git",
                "-C",
                &wt_text,
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--ignored",
                "--",
            ],
            &files[..],
        ]
        .concat();
        let diff = capture(&diff_argv, None, &[], None);
        let status = capture(&status_argv, None, &[], None);
        let differs = match (diff, status) {
            (Ok(diff), Ok(status)) => {
                diff.code != 0 || status.code != 0 || !strip(&status.stdout).is_empty()
            }
            _ => true,
        };
        if differs {
            say(
                &mut lines,
                format!("prebuild skips {d}: its dependency files differ from origin/{base}"),
            );
            continue;
        }
        let tail = ["install", "--frozen-lockfile", "--ignore-pnpmfile"].map(String::from);
        let argv: Vec<String> = match (&mise, &pnpm) {
            (Some(mise), _) => [
                mise.to_string_lossy().into_owned(),
                "exec".into(),
                "--".into(),
                "pnpm".into(),
            ]
            .into_iter()
            .chain(tail)
            .collect(),
            (None, Some(pnpm)) => std::iter::once(pnpm.to_string_lossy().into_owned())
                .chain(tail)
                .collect(),
            (None, None) => {
                say(
                    &mut lines,
                    format!("prebuild skips {d}: pnpm is not on PATH"),
                );
                continue;
            }
        };
        match run_logged(&argv, &folder, &log, PREBUILD_TIMEOUT) {
            Ok(0) => say(
                &mut lines,
                format!("prebuilt {d} with pnpm install --frozen-lockfile"),
            ),
            Ok(code) => say(
                &mut lines,
                format!(
                    "prebuild of {d} failed (exit {code}); log: {}",
                    log.display()
                ),
            ),
            Err(why) => say(
                &mut lines,
                format!("prebuild of {d} failed ({why}); log: {}", log.display()),
            ),
        }
    }
    Ok(lines)
}

/// Install each listed folder from its frozen lockfile, only when its dependency files match origin/<base>.
pub(crate) fn prebuild(home: &Path, slug: &str, job_id: &str, wt: &Path) -> Res<()> {
    prebuild_with(home, slug, job_id, wt, which("mise"), which("pnpm")).map(drop)
}

/// `lsof_exe`: found, else the first fallback this account can run.
fn lsof_exe_with(found: Option<PathBuf>, fallbacks: &[&Path]) -> Option<PathBuf> {
    found.or_else(|| {
        fallbacks
            .iter()
            .find(|p| runnable(p))
            .map(|p| p.to_path_buf())
    })
}

fn runnable(p: &Path) -> bool {
    let Ok(meta) = fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    return std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
    #[cfg(not(unix))]
    {
        let _ = meta;
        true
    }
}

/// PIDs whose cwd is inside wt. Windows can't read another process's cwd, so it matches command lines instead,
/// which misses a process started with a relative path.
#[cfg(windows)]
fn worktree_pids(wt: &str, _proc_root: &Path, _lsof: Option<&Path>) -> Vec<u32> {
    let out = crate::shared::proc::run(&[
        "powershell",
        "-NoProfile",
        "-Command",
        "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.ProcessId)`t$($_.ParentProcessId)`t$($_.CommandLine)\" }",
    ]);
    // The worktree path must end at a separator, quote, space, or the end, so 001-x doesn't match 001-xy.
    let needle = wt.to_lowercase();
    let matches = |line: &str| {
        let line = line.to_lowercase();
        line.match_indices(&needle).any(|(i, _)| {
            line[i + needle.len()..]
                .chars()
                .next()
                .is_none_or(|c| matches!(c, '\\' | '/' | '"' | '\'') || is_space(c))
        })
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let (mut parents, mut hits) = (std::collections::HashMap::new(), Vec::new());
    for line in crate::shared::py::splitlines(&out, false) {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() == 3 && digits(parts[0]) {
            let Ok(pid) = parts[0].parse::<u32>() else {
                continue;
            };
            parents.insert(
                pid,
                if digits(parts[1]) {
                    parts[1].parse().unwrap_or(0)
                } else {
                    0
                },
            );
            if matches(parts[2]) {
                hits.push(pid);
            }
        }
    }
    // Skip this process and every ancestor, so a launcher and its calling shell survive.
    let (mut ancestors, mut pid) = (std::collections::HashSet::new(), std::process::id());
    while pid != 0 && ancestors.insert(pid) {
        pid = parents.get(&pid).copied().unwrap_or(0);
    }
    hits.into_iter()
        .filter(|p| !ancestors.contains(p))
        .collect()
}

/// PIDs whose cwd is inside wt, from proc_root, else from lsof.
#[cfg(not(windows))]
fn worktree_pids(wt: &str, proc_root: &Path, lsof: Option<&Path>) -> Vec<u32> {
    let sep = format!("{wt}{}", std::path::MAIN_SEPARATOR);
    let inside = |path: &str| path == wt || path.starts_with(&sep);
    let mut found = Vec::new();
    if proc_root.is_dir() {
        for entry in fs::read_dir(proc_root).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(pid) = name.parse::<u32>() else {
                continue;
            };
            if let Ok(cwd) = fs::read_link(entry.path().join("cwd")) {
                if inside(&cwd.to_string_lossy()) {
                    found.push(pid);
                }
            }
        }
        return found;
    }
    let Some(exe) = lsof else { return found };
    let exe = exe.to_string_lossy();
    let mut pid = None;
    for line in crate::shared::py::splitlines(
        &crate::shared::proc::run(&[&exe, "-a", "-d", "cwd", "-Fpn"]),
        false,
    ) {
        if let Some(rest) = line.strip_prefix('p') {
            pid = rest.parse::<u32>().ok();
        } else if let (Some(rest), Some(p)) = (line.strip_prefix('n'), pid) {
            if inside(rest) {
                found.push(p);
            }
        }
    }
    found
}

/// pid as a positive i32, or None for 0 or a value that does not fit, which kill would read as a process group
/// or every process this account owns.
fn single_pid(pid: u32) -> Option<i32> {
    i32::try_from(pid).ok().filter(|&p| p > 0)
}

/// Send SIGTERM to pid. False when pid is not a single process, so nothing was sent.
#[cfg(unix)]
fn terminate(pid: u32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const SIGTERM: i32 = 15;
    let Some(pid) = single_pid(pid) else {
        return false;
    };
    // SAFETY: kill takes two integers and reads or writes no memory of this process.
    unsafe {
        kill(pid, SIGTERM);
    }
    true
}

#[cfg(windows)]
fn terminate(pid: u32) -> bool {
    if single_pid(pid).is_none() {
        return false;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn TerminateProcess(process: isize, code: u32) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }
    const PROCESS_TERMINATE: u32 = 0x0001;
    // SAFETY: the handle comes from OpenProcess, is used only while open, and is closed once.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle != 0 {
            TerminateProcess(handle, 15);
            CloseHandle(handle);
        }
    }
    true
}

/// `proc_name(pid) != ""`.
fn proc_alive(pid: u32) -> bool {
    let pid = pid.to_string();
    if cfg!(windows) {
        let filter = format!("PID eq {pid}");
        let out = crate::shared::proc::run(&["tasklist", "/FI", &filter, "/NH"]);
        return !out.is_empty() && out.contains(&pid);
    }
    !strip(&crate::shared::proc::run(&[
        "ps", "-p", &pid, "-o", "comm=",
    ]))
    .is_empty()
}

/// `stop_pids` with the proc folder and lsof given.
fn stop_pids_with(wt: &Path, proc_root: &Path, lsof: Option<PathBuf>) -> Option<Vec<u32>> {
    // A link at wt or its parent would point the signals at another folder, and an empty or
    // unresolved wt would match every process.
    let linked = [Some(wt), wt.parent()]
        .into_iter()
        .flatten()
        .any(|p| !p.as_os_str().is_empty() && is_link(p).unwrap_or(true));
    let resolved = resolve(wt);
    let cause = if wt.as_os_str().is_empty() {
        "the path is empty"
    } else if linked {
        "it is a link or sits under one"
    } else if resolved.is_err() {
        "it cannot be resolved"
    } else {
        ""
    };
    let real = match resolved {
        Ok(real) if cause.is_empty() => real,
        _ => {
            eprintln!(
                "refusing to stop processes in {}: {cause}; no process was signalled",
                repr(&wt.to_string_lossy())
            );
            return None;
        }
    };
    if !cfg!(windows) && !proc_root.is_dir() && lsof.is_none() {
        eprintln!(
            "lsof not found, so processes in {} were not checked",
            real.display()
        );
        return None;
    }
    // Windows has no parent_id in std; its worktree_pids already skips every ancestor.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut skip = vec![std::process::id()];
    #[cfg(unix)]
    skip.push(std::os::unix::process::parent_id());
    let mut pids = worktree_pids(&real.to_string_lossy(), proc_root, lsof.as_deref());
    pids.sort_unstable();
    pids.dedup();
    pids.retain(|p| !skip.contains(p));
    // A pid that cannot be signalled counts as still running, so cleanup keeps the worktree.
    let (signalled, unsent): (Vec<u32>, Vec<u32>) = pids.into_iter().partition(|&p| terminate(p));
    let deadline = Instant::now() + STOP_WAIT;
    loop {
        let running: Vec<u32> = signalled
            .iter()
            .copied()
            .filter(|&p| proc_alive(p))
            .collect();
        if running.is_empty() || Instant::now() > deadline {
            let mut alive = [unsent, running].concat();
            alive.sort_unstable();
            return Some(alive);
        }
        thread::sleep(STOP_POLL);
    }
}

/// The PIDs still running in wt after SIGTERM, or None when processes were not checked.
pub(crate) fn stop_pids(wt: &Path) -> Option<Vec<u32>> {
    let fallbacks: Vec<&Path> = LSOF_FALLBACKS.iter().map(Path::new).collect();
    stop_pids_with(
        wt,
        Path::new("/proc"),
        lsof_exe_with(which("lsof"), &fallbacks),
    )
}

/// (exit code, stdout, stderr) of program -C repo args. A timeout or a git that cannot start gives (None, "",
/// reason), so the caller keeps what it was about to remove.
fn run_git_with(
    program: &str,
    repo: &str,
    args: &[&str],
    timeout: Option<Duration>,
) -> (Option<i32>, String, String) {
    let argv = [&[program, "-C", repo], args].concat();
    match capture(&argv, None, &[("GIT_TERMINAL_PROMPT", "0")], timeout) {
        Ok(r) => (Some(r.code), strip(&r.stdout).to_string(), r.stderr),
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            let secs = timeout.unwrap_or_default().as_secs();
            (
                None,
                String::new(),
                format!("git {} timed out after {secs} s", args[0]),
            )
        }
        Err(_) => (
            None,
            String::new(),
            format!("could not run git {}", args[0]),
        ),
    }
}

fn run_git(repo: &str, args: &[&str], timeout: Option<Duration>) -> (Option<i32>, String, String) {
    run_git_with("git", repo, args, timeout)
}

fn first_error(repo: &str, args: &[&str], timeout: Option<Duration>) -> Option<String> {
    match run_git(repo, args, timeout) {
        (None, _, err) => Some(err),
        (Some(0), _, _) => None,
        (Some(code), _, err) => {
            let err = USE_FLAG.replace_all(&err, |_| String::new());
            let first = crate::shared::py::splitlines(strip(&err), false)
                .first()
                .map(|l| l.to_string());
            Some(first.unwrap_or_else(|| format!("git exited {code}")))
        }
    }
}

fn not_merged(repo: &str, detail: Option<&str>) -> String {
    let head = run_git(
        repo,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        Some(git_timeout()),
    )
    .1;
    let head = if head.is_empty() { "HEAD" } else { &head };
    match detail {
        Some(detail) => format!("not merged into {head} and {detail}"),
        None => format!("not merged into {head}"),
    }
}

/// None after deleting a branch whose tip origin holds, else the reason it stays.
fn pushed_tip_error(repo: &str, branch: &str) -> Option<String> {
    let r = format!("refs/heads/{branch}");
    let tip = run_git(repo, &["rev-parse", &r], Some(git_timeout())).1;
    if tip.is_empty() {
        return Some(not_merged(repo, None));
    }
    let (code, out, _) = run_git(repo, &["ls-remote", "origin", &r], Some(git_timeout()));
    if code != Some(0) {
        return Some(not_merged(repo, Some("origin could not be reached")));
    }
    if out.is_empty() {
        return Some(not_merged(repo, Some("origin has no such branch")));
    }
    if words(&out).collect::<Vec<_>>() != [tip.as_str(), r.as_str()] {
        return Some(not_merged(repo, Some("origin is at a different commit")));
    }
    delete_unless_checked_out(repo, branch, &tip)
}

/// None after deleting the branch, guarded by tip, unless a worktree has it checked out.
fn delete_unless_checked_out(repo: &str, branch: &str, tip: &str) -> Option<String> {
    let listed = run_git(
        repo,
        &["worktree", "list", "--porcelain"],
        Some(git_timeout()),
    )
    .1;
    let checked_out = format!("branch refs/heads/{branch}");
    let mut path = None;
    for line in crate::shared::py::splitlines(&listed, false) {
        if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(rest);
        } else if line == checked_out {
            return Some(format!("checked out in {}", path.unwrap_or("None")));
        }
    }
    first_error(
        repo,
        &["update-ref", "-d", &format!("refs/heads/{branch}"), tip],
        Some(git_timeout()),
    )
}

/// None after deleting a branch whose tip merged_head contains, else the reason it stays.
fn merged_head_error(
    repo: &str,
    branch: &str,
    drop_pushed: bool,
    merged_head: &str,
) -> Option<String> {
    let tip = run_git(
        repo,
        &["rev-parse", &format!("refs/heads/{branch}")],
        Some(git_timeout()),
    )
    .1;
    if !tip.is_empty()
        && run_git(
            repo,
            &["merge-base", "--is-ancestor", &tip, merged_head],
            Some(git_timeout()),
        )
        .0 == Some(0)
    {
        return delete_unless_checked_out(repo, branch, &tip);
    }
    if drop_pushed {
        pushed_tip_error(repo, branch)
    } else {
        Some(not_merged(repo, None))
    }
}

/// `cleanup_job` once `stop_pids` gave alive.
fn cleanup_after_stop(
    repo: &str,
    wt: &Path,
    branch: &str,
    drop_pushed: bool,
    merged_head: Option<&str>,
    alive: Option<Vec<u32>>,
) -> (Vec<String>, Vec<String>) {
    let (mut removed, mut left) = (Vec::new(), Vec::new());
    let wt_text = wt.to_string_lossy();
    if wt.exists() {
        let error = match alive {
            None => Some("processes were not checked".to_string()),
            Some(alive) if alive.is_empty() => {
                first_error(repo, &["worktree", "remove", &wt_text], None)
            }
            Some(alive) => {
                let pids: Vec<String> = alive.iter().map(u32::to_string).collect();
                Some(format!("processes still running: {}", pids.join(" ")))
            }
        };
        match error {
            Some(error) => {
                left.push(format!("worktree {wt_text}: {error}"));
                left.push(format!(
                    "branch {branch}: kept because its worktree remains"
                ));
            }
            None => removed.push(format!("worktree {wt_text}")),
        }
    } else {
        first_error(repo, &["worktree", "prune"], Some(git_timeout()));
    }
    if left.is_empty() {
        let r = format!("refs/heads/{branch}");
        let (code, _, err) = run_git(
            repo,
            &["rev-parse", "--verify", "-q", &r],
            Some(git_timeout()),
        );
        let mut error = if code.is_none() { Some(err) } else { None };
        if code == Some(0) {
            error = match run_git(
                repo,
                &["merge-base", "--is-ancestor", &r, "HEAD"],
                Some(git_timeout()),
            ) {
                (None, _, err) => Some(err),
                (Some(0), _, _) => {
                    let upstream = format!("branch.{branch}.merge");
                    if !run_git(repo, &["config", "--get", &upstream], Some(git_timeout()))
                        .1
                        .is_empty()
                    {
                        first_error(
                            repo,
                            &["branch", "--unset-upstream", branch],
                            Some(git_timeout()),
                        )
                        .or_else(|| {
                            first_error(repo, &["branch", "-d", branch], Some(git_timeout()))
                        })
                    } else {
                        first_error(repo, &["branch", "-d", branch], Some(git_timeout()))
                    }
                }
                (Some(_), _, _) => match merged_head.filter(|h| !h.is_empty()) {
                    Some(head) => merged_head_error(repo, branch, drop_pushed, head),
                    None if drop_pushed => pushed_tip_error(repo, branch),
                    None => Some(not_merged(repo, None)),
                },
            };
            if error.is_none() {
                removed.push(format!("branch {branch}"));
            }
        }
        if let Some(error) = error {
            left.push(format!("branch {branch}: {error}"));
        }
    }
    (removed, left)
}

/// Stop and remove a job's worktree, then delete its branch when merged. With drop_pushed, also delete an unmerged
/// branch that origin holds at the same commit and no worktree has checked out. With merged_head, also delete a
/// branch whose tip that commit contains and no worktree has checked out. (removed, left).
pub(crate) fn cleanup_job(
    repo: &str,
    wt: &Path,
    branch: &str,
    drop_pushed: bool,
    merged_head: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let alive = if wt.exists() { stop_pids(wt) } else { None };
    cleanup_after_stop(repo, wt, branch, drop_pushed, merged_head, alive)
}

#[derive(Debug, PartialEq)]
enum GhCode {
    Exit(i32),
    Timeout,
}

/// (exit code, stdout) of gh args: Exit(1) with "" when it cannot start, Timeout past 60 seconds, None without gh.
fn gh_cli(args: &[&str]) -> Option<(GhCode, String)> {
    let gh = which("gh")?;
    let gh = gh.to_string_lossy();
    let argv = [&[gh.as_ref()], args].concat();
    Some(match capture(&argv, None, &[], Some(GH_TIMEOUT)) {
        Ok(r) => (GhCode::Exit(r.code), r.stdout),
        Err(e) if e.kind() == io::ErrorKind::TimedOut => (GhCode::Timeout, String::new()),
        Err(_) => (GhCode::Exit(1), String::new()),
    })
}

/// The state and head commit of repo's pull request number from one gh api graphql call: Ok(Some((state, head
/// oid))), Ok(None) when GitHub returns null for it, or Err(why).
fn pr_state(repo: &str, number: u32, device: &str) -> Result<Option<(Value, Value)>, String> {
    let (owner, name) = repo.split_once('/').unwrap_or((repo, ""));
    let query = format!(
        "query=query {{ repository(owner: \"{owner}\", name: \"{name}\") {{ p{number}: pullRequest(number: {number}) \
         {{ state headRefOid }} }} }}"
    );
    let (code, out) = match gh_cli(&["api", "graphql", "-f", &query]) {
        None => return Err(format!("gh is not installed on {device}")),
        Some((GhCode::Timeout, _)) => {
            return Err(format!(
                "gh api graphql timed out after 60 seconds on {device}"
            ))
        }
        Some((GhCode::Exit(code), out)) => (code, out),
    };
    let found = loads(&out)
        .ok()
        .and_then(|v| v.get("data")?.get("repository").cloned());
    let Some(found) = found.filter(Value::is_object) else {
        if code != 0 {
            return Err(format!(
                "gh api graphql exited {code} (check gh auth status on {device})"
            ));
        }
        return Err(format!(
            "gh api graphql returned no repository (check the project file's repo and gh auth status on {device})"
        ));
    };
    Ok(found
        .get(format!("p{number}"))
        .filter(|p| p.is_object())
        .map(|p| {
            let field = |k: &str| p.get(k).cloned().unwrap_or(Value::Null);
            (field("state"), field("headRefOid"))
        }))
}

/// The head commit GitHub reports for a merged pull request URL, or None. A failed lookup warns and returns None.
fn pr_merged_head(home: &Path, pr: &str) -> Option<String> {
    let m = GH_PR_PARTS.fullmatch(pr)?;
    let number: u32 = m[3].parse().ok()?;
    match pr_state(&format!("{}/{}", &m[1], &m[2]), number, &device_name(home)) {
        Err(why) => {
            eprintln!(
                "cleanup could not read pull request {pr} ({why}), so it keeps a branch HEAD does not contain"
            );
            None
        }
        Ok(Some((Value::String(state), Value::String(head))))
            if state == "MERGED" && COMMIT.is_full(&head) =>
        {
            Some(head)
        }
        Ok(_) => None,
    }
}

/// The board's jobs, or why they could not be read.
fn board_jobs(home: &Path) -> Result<Vec<Value>, String> {
    let r = match board_client::read(home) {
        Ok(r) => r,
        Err(e) => {
            let reason = words(&e.stderr).collect::<Vec<_>>().join(" ");
            return Err(if reason.is_empty() {
                "unknown error".to_string()
            } else {
                reason
            });
        }
    };
    let text = r.text.unwrap_or_default();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let board = loads(&text).ok();
    match board
        .as_ref()
        .and_then(|b| b.get("jobs"))
        .and_then(Value::as_array)
    {
        Some(jobs) => Ok(jobs.clone()),
        None => Err("malformed board".to_string()),
    }
}

/// The board's github.head for a merged job, else its merged pull request's head from GitHub, or None. An
/// unreadable board warns and returns None.
fn board_merged_head(home: &Path, slug: &str, job_id: &str) -> Res<Option<String>> {
    let jobs = match board_jobs(home) {
        Ok(jobs) => jobs,
        Err(reason) => {
            eprintln!(
                "cleanup could not read the board ({reason}), so it keeps a branch HEAD does not contain"
            );
            return Ok(None);
        }
    };
    let str_of = |j: &Value, k: &str| j.get(k).and_then(Value::as_str).map(String::from);
    let Some(job) = jobs.iter().find(|j| {
        j.is_object()
            && str_of(j, "id").as_deref() == Some(job_id)
            && str_of(j, "project").as_deref() == Some(slug)
    }) else {
        return Ok(None);
    };
    let merged = str_of(job, "stage").as_deref() == Some("merged");
    if let Some(github) = job.get("github").filter(|g| g.is_object()) {
        let head = str_of(github, "head");
        if merged
            && github.get("merged").is_some_and(Value::is_string)
            && head.as_deref().is_some_and(|h| COMMIT.is_full(h))
        {
            return Ok(head);
        }
    }
    let Some(pr) = str_of(job, "pr").filter(|_| merged) else {
        return Ok(None);
    };
    let Some(m) = GH_PR_PARTS.fullmatch(&pr) else {
        return Ok(None);
    };
    let md = project_files(home, slug)?.0;
    let repo = if md.exists() {
        let meta = frontmatter(&md).map_err(|e| os_fail(&e, &md))?.0;
        fm_get(&meta, "repo").unwrap_or("").to_string()
    } else {
        String::new()
    };
    let pr_repo = format!("{}/{}", &m[1], &m[2]);
    if pr_repo.to_lowercase() == repo.to_lowercase() {
        return Ok(pr_merged_head(home, &pr));
    }
    let why = if !md.exists() {
        format!("{} is missing", md.display())
    } else if repo.is_empty() {
        format!("{} has no repo:", md.display())
    } else {
        format!("it is in {pr_repo}, not the project file's repo: {repo}")
    };
    eprintln!("cleanup skipped the GitHub lookup for {pr}: {why}");
    Ok(None)
}

/// Remove the job's prompts folder unless its worktree remains or a link is on its path.
fn cleanup_prompts(
    home: &Path,
    slug: &str,
    job_id: &str,
    removed: &mut Vec<String>,
    left: &mut Vec<String>,
) -> Res<()> {
    let folder = home.join("prompts").join(slug).join(job_id);
    if fs::symlink_metadata(&folder).is_err() {
        return Ok(());
    }
    let shown = folder.display();
    let linked = [
        home.join("prompts"),
        home.join("prompts").join(slug),
        folder.clone(),
    ]
    .iter()
    .any(|p| is_link(p).unwrap_or(true));
    if left.iter().any(|entry| entry.starts_with("worktree ")) {
        left.push(format!(
            "prompts {shown}: kept because its worktree remains"
        ));
    } else if linked {
        left.push(format!(
            "prompts {shown}: a symlink or junction, left in place"
        ));
    } else if folder.is_dir() {
        fs::remove_dir_all(&folder).map_err(|e| os_fail(&e, &folder))?;
        removed.push(format!("prompts {shown}"));
    }
    Ok(())
}

pub fn cleanup(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let repo = checkout_of(&home, &slug)?;
    let merged_head = board_merged_head(&home, &slug, &job_id)?;
    let wt = home.join("worktrees").join(&slug).join(&job_id);
    let (mut removed, mut left) = cleanup_job(
        &repo,
        &wt,
        &format!("factory/{job_id}"),
        false,
        merged_head.as_deref(),
    );
    cleanup_prompts(&home, &slug, &job_id, &mut removed, &mut left)?;
    println!(
        "{}",
        dumps(&json!({"removed": removed, "left": left}), INDENT2)
    );
    if left.is_empty() {
        Ok(())
    } else {
        Err(exit(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;

    #[cfg(unix)]
    #[test]
    fn root_worktree_matches_only_cwd_root() {
        let proc_root = temp_dir("pids-root");
        for (pid, cwd) in [("11", "/"), ("12", "/home/u/proj"), ("13", "/tmp")] {
            fs::create_dir(proc_root.join(pid)).unwrap();
            std::os::unix::fs::symlink(cwd, proc_root.join(pid).join("cwd")).unwrap();
        }
        assert_eq!(worktree_pids("/", &proc_root, None), [11]);
        let mut under = worktree_pids("/home", &proc_root, None);
        under.sort();
        assert_eq!(under, [12]);
    }

    const JOB: &str = "812-dark-mode";

    /// git with a fixed identity and no hooks, panicking on failure.
    fn sh_git(cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(["-c", "core.hooksPath=/dev/null", "-C"])
            .arg(cwd)
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

    fn commit(cwd: &Path, message: &str) -> String {
        sh_git(cwd, &["commit", "-q", "--allow-empty", "-m", message]);
        sh_git(cwd, &["rev-parse", "HEAD"])
    }

    fn text(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    /// A home, a bare origin with one main commit, the seed that pushed it, and a clone.
    struct Lineage {
        root: PathBuf,
        home: PathBuf,
        seed: PathBuf,
        repo: PathBuf,
    }

    impl Lineage {
        fn new(name: &str) -> Lineage {
            let root = temp_dir(name);
            let (home, origin, seed, repo) = (
                root.join("home"),
                root.join("origin.git"),
                root.join("seed"),
                root.join("repo"),
            );
            fs::create_dir_all(home.join("projects")).unwrap();
            fs::write(
                home.join("paths.md"),
                format!("---\nfactory: {}\n---\n", repo.display()),
            )
            .unwrap();
            sh_git(&root, &["init", "-q", "--bare", &text(&origin)]);
            sh_git(&root, &["init", "-q", &text(&seed)]);
            for r in [&origin, &seed] {
                sh_git(r, &["symbolic-ref", "HEAD", "refs/heads/main"]);
            }
            commit(&seed, "public root");
            sh_git(&seed, &["remote", "add", "origin", &text(&origin)]);
            sh_git(&seed, &["push", "-q", "origin", "main"]);
            sh_git(&root, &["clone", "-q", &text(&origin), &text(&repo)]);
            Lineage {
                root,
                home,
                seed,
                repo,
            }
        }

        fn problem(&self, branch: Option<&str>) -> Option<String> {
            checkout_problem(&self.home, "factory", &text(&self.repo), branch).unwrap()
        }

        fn archive_root(&self) -> String {
            sh_git(&self.repo, &["checkout", "-q", "--orphan", "archive"]);
            let root = commit(&self.repo, "archive root");
            sh_git(&self.repo, &["checkout", "-q", "main"]);
            root
        }

        fn foreign(&self, r: &str, root: &str) -> String {
            format!(
                "unsafe checkout: {} {r} has root {}, which origin/main lacks, so it holds archive history; repoint \
                 it as docs/public-cutover.md \"Other hosts\" says",
                self.repo.display(),
                &root[..12]
            )
        }

        fn base(&self, base: &str) {
            fs::write(
                self.home.join("projects/factory.md"),
                format!("---\nbase: {base}\n---\n"),
            )
            .unwrap();
        }
    }

    impl Drop for Lineage {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn clean_clone_is_safe() {
        let t = Lineage::new("wt-clean");
        assert_eq!(t.problem(None), None);
        assert_eq!(t.problem(Some(&format!("factory/{JOB}"))), None);
    }

    #[test]
    fn foreign_root_is_caught() {
        let t = Lineage::new("wt-foreign-main");
        let root = t.archive_root();
        sh_git(&t.repo, &["checkout", "-q", "archive"]);
        sh_git(&t.repo, &["branch", "-f", "main", "archive"]);
        assert_eq!(t.problem(None), Some(t.foreign("main", &root)));

        let t = Lineage::new("wt-foreign-merge");
        let root = t.archive_root();
        sh_git(
            &t.repo,
            &[
                "merge",
                "-q",
                "--allow-unrelated-histories",
                "-m",
                "merge",
                "archive",
            ],
        );
        assert_eq!(t.problem(None), Some(t.foreign("main", &root)));

        let t = Lineage::new("wt-foreign-job");
        let root = t.archive_root();
        let branch = format!("factory/{JOB}");
        sh_git(&t.repo, &["branch", &branch, "archive"]);
        assert_eq!(t.problem(Some(&branch)), Some(t.foreign(&branch, &root)));
    }

    #[test]
    fn rewritten_origin_is_caught() {
        let t = Lineage::new("wt-rewritten");
        let old = sh_git(&t.repo, &["rev-parse", "main"]);
        sh_git(&t.seed, &["checkout", "-q", "--orphan", "public"]);
        commit(&t.seed, "rewritten root");
        sh_git(&t.seed, &["push", "-q", "-f", "origin", "public:main"]);
        assert_eq!(
            sh_git(&t.repo, &["rev-parse", "refs/remotes/origin/main"]),
            old
        );
        assert_eq!(t.problem(None), Some(t.foreign("main", &old)));
    }

    #[test]
    fn failed_fetch_leaves_the_url_out() {
        let t = Lineage::new("wt-fetch-fails");
        let gone = t.root.join("secret-token").join("gone.git");
        sh_git(&t.repo, &["remote", "set-url", "origin", &text(&gone)]);
        let line = t.problem(None).unwrap();
        let head = format!(
            "unsafe checkout: git fetch origin main failed in {} (exit ",
            t.repo.display()
        );
        assert!(line.starts_with(&head) && line.ends_with(')'), "{line}");
        assert!(!line.contains("secret-token"));
    }

    #[test]
    fn unsafe_and_project_base() {
        let t = Lineage::new("wt-base");
        for base in ["-x", "../x", "a b", ""] {
            t.base(base);
            assert_eq!(
                t.problem(None),
                Some(format!(
                    "unsafe checkout: factory base {base} is not a safe branch name"
                ))
            );
        }
        t.base("trunk");
        assert_eq!(
            t.problem(None),
            Some(format!(
                "unsafe checkout: git fetch origin trunk failed in {} (exit 128)",
                t.repo.display()
            ))
        );
        t.base("-x");
        let e = worktree_in(&t.home, "factory", JOB).unwrap_err();
        assert_eq!(
            (e.code, e.stderr.as_str()),
            (
                6,
                "unsafe checkout: factory base -x is not a safe branch name\n"
            )
        );
        assert!(!t.home.join("worktrees").exists());
        let missing = Lineage::new("wt-missing-base");
        sh_git(&missing.repo, &["checkout", "-q", "-b", "other"]);
        sh_git(&missing.repo, &["branch", "-q", "-D", "main"]);
        let repo = missing.repo.display();
        assert_eq!(
            missing.problem(None),
            Some(format!("unsafe checkout: {repo} has no local main branch; run git -C {repo} branch main origin/main"))
        );
    }

    #[test]
    fn fetch_timeout_is_60s() {
        assert_eq!(FETCH_TIMEOUT, Duration::from_secs(60));
        assert_eq!(fetch_timeout(), FETCH_TIMEOUT);
    }

    const FAKE_GIT_CHILD: &str = "FACTORY_FAKE_GIT_CHILD";

    /// Re-run this test in a child process whose PATH starts with a `git` wrapper that handles `fetch` with
    /// `fetch_body` and passes every other call to the real git, so the parent's own git calls stay untouched.
    #[cfg(unix)]
    fn in_fake_git_child(name: &str, verb: &str, fetch_body: &str) {
        let path = std::env::var("PATH").unwrap();
        let real = path
            .split(':')
            .map(|d| Path::new(d).join("git"))
            .find(|p| p.is_file())
            .unwrap();
        let dir = temp_dir(&format!(
            "fake-git-{verb}-{}",
            name.rsplit("::").next().unwrap()
        ));
        crate::shared::build::tests::script(
            &dir.join("git"),
            &format!(
                "for a in \"$@\"; do\n  if [ \"$a\" = {verb} ]; then\n{fetch_body}\n  fi\ndone\nexec {} \"$@\"\n",
                real.display()
            ),
        );
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env(FAKE_GIT_CHILD, dir.display().to_string())
            .env("PATH", format!("{}:{path}", dir.display()))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn fetch_has_no_prompt_and_a_timeout() {
        let name = "commands::worktree::tests::fetch_has_no_prompt_and_a_timeout";
        let Ok(dir) = std::env::var(FAKE_GIT_CHILD) else {
            in_fake_git_child(
                name,
                "fetch",
                "    echo \"$GIT_TERMINAL_PROMPT\" > \"$(dirname \"$0\")/prompt\"",
            );
            return;
        };
        let t = Lineage::new("wt-no-prompt");
        assert_eq!(t.problem(None), None);
        let seen = fs::read_to_string(Path::new(&dir).join("prompt")).unwrap();
        assert_eq!(seen.trim(), "0");
    }

    #[cfg(unix)]
    #[test]
    fn hung_fetch_times_out() {
        let name = "commands::worktree::tests::hung_fetch_times_out";
        if std::env::var(FAKE_GIT_CHILD).is_err() {
            in_fake_git_child(name, "fetch", "    exec sleep 300");
            return;
        }
        TEST_FETCH_TIMEOUT.with(|t| t.set(Some(Duration::from_secs(1))));
        let t = Lineage::new("wt-hung-fetch");
        let started = std::time::Instant::now();
        assert_eq!(
            t.problem(None),
            Some(format!(
                "unsafe checkout: git fetch origin main timed out after 1 s in {}",
                t.repo.display()
            ))
        );
        assert!(started.elapsed() >= Duration::from_secs(1));
    }

    /// Push one more commit to origin's job branch from the seed.
    fn push_job(t: &Lineage, message: &str) -> String {
        let branch = format!("factory/{JOB}");
        sh_git(&t.seed, &["checkout", "-q", "-B", &branch]);
        let head = commit(&t.seed, message);
        sh_git(&t.seed, &["push", "-q", "origin", &branch]);
        head
    }

    #[test]
    fn worktree_add_and_fast_forward() {
        let t = Lineage::new("wt-add");
        let e = worktree_in(&t.home, "factory", JOB).unwrap_err();
        assert_eq!(
            (e.code, e.stderr),
            (4, format!("no factory/{JOB} branch here or on origin\n"))
        );
        let first = push_job(&t, "job work");
        let wt = worktree_in(&t.home, "factory", JOB).unwrap();
        assert_eq!(wt, t.home.join("worktrees/factory").join(JOB));
        assert_eq!(sh_git(&wt, &["rev-parse", "HEAD"]), first);
        let second = push_job(&t, "more job work");
        assert_eq!(worktree_in(&t.home, "factory", JOB).unwrap(), wt);
        assert_eq!(sh_git(&wt, &["rev-parse", "HEAD"]), second);
        sh_git(&t.repo, &["worktree", "remove", &text(&wt)]);
        assert_eq!(worktree_in(&t.home, "factory", JOB).unwrap(), wt);
        assert_eq!(sh_git(&wt, &["rev-parse", "HEAD"]), second);
    }

    #[test]
    fn worktree_refuses_ahead_and_behind() {
        let t = Lineage::new("wt-diverged");
        push_job(&t, "job work");
        let wt = worktree_in(&t.home, "factory", JOB).unwrap();
        let before = push_job(&t, "origin moves");
        fs::write(wt.join("untracked.txt"), "x\n").unwrap();
        let e = worktree_in(&t.home, "factory", JOB).unwrap_err();
        let tail = format!("behind origin/factory/{JOB}");
        assert_eq!(
            (e.code, e.stderr),
            (
                6,
                format!(
                    "{} is 0 ahead and 1 {tail} with uncommitted changes. Nothing was changed\n",
                    wt.display()
                )
            )
        );
        sh_git(&wt, &["add", "untracked.txt"]);
        commit(&wt, "local work");
        let e = worktree_in(&t.home, "factory", JOB).unwrap_err();
        assert_eq!(
            (e.code, e.stderr),
            (
                6,
                format!(
                    "{} is 1 ahead and 1 {tail}. Nothing was changed\n",
                    wt.display()
                )
            )
        );
        assert_ne!(sh_git(&wt, &["rev-parse", "HEAD"]), before);
    }

    #[cfg(unix)]
    const FAKE_PNPM: &str = "echo \"$* $(pwd)\" >> \"{record}\"\nmkdir -p node_modules && echo done > node_modules/.modules.yaml\n";

    /// A home whose factory project lists prebuild folders, and a clone of an origin holding sample/.
    struct Prebuild {
        root: PathBuf,
        home: PathBuf,
        wt: PathBuf,
        record: PathBuf,
        pnpm: PathBuf,
        log: PathBuf,
    }

    impl Prebuild {
        fn new(name: &str) -> Prebuild {
            let root = temp_dir(name);
            let (home, origin, seed, wt) = (
                root.join("home"),
                root.join("origin.git"),
                root.join("seed"),
                root.join("wt"),
            );
            fs::create_dir_all(home.join("projects")).unwrap();
            fs::write(home.join("projects/factory.md"), "---\nbase: main\n---\n").unwrap();
            sh_git(&root, &["init", "-q", "--bare", &text(&origin)]);
            sh_git(&root, &["init", "-q", &text(&seed)]);
            for r in [&origin, &seed] {
                sh_git(r, &["symbolic-ref", "HEAD", "refs/heads/main"]);
            }
            fs::create_dir_all(seed.join("sample/inner")).unwrap();
            fs::write(
                seed.join("sample/pnpm-lock.yaml"),
                "lockfileVersion: '9.0'\n",
            )
            .unwrap();
            fs::write(seed.join("sample/package.json"), "{}\n").unwrap();
            fs::write(
                seed.join("sample/inner/pnpm-lock.yaml"),
                "lockfileVersion: '9.0'\n",
            )
            .unwrap();
            fs::write(seed.join("mise.toml"), "[tools]\nnode = '24'\n").unwrap();
            sh_git(&seed, &["add", "."]);
            sh_git(&seed, &["commit", "-q", "-m", "sample"]);
            sh_git(&seed, &["remote", "add", "origin", &text(&origin)]);
            sh_git(&seed, &["push", "-q", "origin", "main"]);
            sh_git(&root, &["clone", "-q", &text(&origin), &text(&wt)]);
            let record = root.join("calls.txt");
            let pnpm = root.join("pnpm");
            #[cfg(unix)]
            crate::shared::build::tests::script(
                &pnpm,
                &FAKE_PNPM.replace("{record}", &text(&record)),
            );
            let log = home.join("logs").join(format!("prebuild-{JOB}.log"));
            Prebuild {
                root,
                home,
                wt,
                record,
                pnpm,
                log,
            }
        }

        fn config(&self, dirs: &str) {
            fs::write(
                self.home.join("prebuild.json"),
                format!("{{\"version\": 1, \"prebuild\": {{\"factory\": {dirs}}}}}"),
            )
            .unwrap();
        }

        fn run(&self, mise: Option<&Path>, pnpm: Option<&Path>) -> Vec<String> {
            prebuild_with(
                &self.home,
                "factory",
                JOB,
                &self.wt,
                mise.map(Path::to_path_buf),
                pnpm.map(Path::to_path_buf),
            )
            .unwrap()
        }

        fn calls(&self) -> Vec<String> {
            fs::read_to_string(&self.record)
                .map(|t| t.lines().map(String::from).collect())
                .unwrap_or_default()
        }

        fn failed(&self, why: &str) -> String {
            format!(
                "prebuild of sample failed ({why}); log: {}",
                self.log.display()
            )
        }
    }

    impl Drop for Prebuild {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(unix)]
    fn chmod(p: &Path, mode: u32) {
        fs::set_permissions(p, std::os::unix::fs::PermissionsExt::from_mode(mode)).unwrap();
    }

    const PREBUILT: &str = "prebuilt sample with pnpm install --frozen-lockfile";

    #[test]
    fn prebuild_dirs_reads_only_its_project() {
        let home = temp_dir("wt-prebuild-dirs");
        assert!(prebuild_dirs(&home, "factory").is_empty());
        fs::write(
            home.join("prebuild.json"),
            r#"{"version": 1, "prebuild": {"other": ["sample"]}}"#,
        )
        .unwrap();
        assert!(prebuild_dirs(&home, "factory").is_empty());
        for bad in [
            "{not json",
            r#"{"version": 2, "prebuild": {}}"#,
            r#"{"version": 1, "prebuild": {"factory": [1]}}"#,
            "[]",
        ] {
            fs::write(home.join("prebuild.json"), bad).unwrap();
            assert!(prebuild_dirs(&home, "factory").is_empty(), "{bad}");
        }
        fs::write(
            home.join("prebuild.json"),
            r#"{"version": 1, "prebuild": {"factory": ["a/b", "../x", "/abs", "c"]}}"#,
        )
        .unwrap();
        assert_eq!(prebuild_dirs(&home, "factory"), ["a/b", "c"]);
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn prebuild_installs_once() {
        let t = Prebuild::new("wt-prebuild-once");
        t.config(r#"["sample"]"#);
        assert_eq!(t.run(None, Some(&t.pnpm)), [PREBUILT]);
        assert_eq!(
            t.calls(),
            [format!(
                "install --frozen-lockfile --ignore-pnpmfile {}",
                t.wt.join("sample").display()
            )]
        );
        assert!(t.log.exists());
        assert!(t.run(None, Some(&t.pnpm)).is_empty());
        assert_eq!(t.calls().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn prebuild_skips_changed_dependency_files() {
        let edits: [(&str, &str, bool); 9] = [
            ("sample/pnpm-lock.yaml", "lockfileVersion: '9.1'\n", true),
            ("mise.toml", "[env]\n_.source = 'x.sh'\n", true),
            ("sample/.pnpmfile.cjs", "module.exports = {}\n", false),
            ("mise.local.toml", "[env]\n_.source = 'x.sh'\n", false),
            (".git/info/exclude", "mise.local.toml\n", false),
            ("sample/.config/mise/config.toml", "[env]\n", false),
            (
                ".miserc.toml",
                "override_config_filenames = ['x.toml']\n",
                false,
            ),
            (
                "sample/.config/miserc.toml",
                "override_config_filenames = ['x.toml']\n",
                false,
            ),
            ("sample/mise.toml", "[env]\n_.source = 'x.sh'\n", false),
        ];
        for (i, (file, body, committed)) in edits.into_iter().enumerate() {
            let t = Prebuild::new(&format!("wt-prebuild-edit-{i}"));
            let dir = if file == "sample/mise.toml" {
                "sample/inner"
            } else {
                "sample"
            };
            t.config(&format!("[\"{dir}\"]"));
            let path = t.wt.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
            if file == ".git/info/exclude" {
                fs::write(t.wt.join("mise.local.toml"), "[env]\n_.source = 'x.sh'\n").unwrap();
                assert_eq!(sh_git(&t.wt, &["status", "--porcelain"]), "");
            }
            if committed {
                sh_git(&t.wt, &["commit", "-q", "-am", "builder edit"]);
            }
            let expected =
                format!("prebuild skips {dir}: its dependency files differ from origin/main");
            assert_eq!(t.run(None, Some(&t.pnpm)), [expected], "{file}");
            assert!(t.calls().is_empty(), "{file}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn prebuild_runs_pnpm_through_mise() {
        let t = Prebuild::new("wt-prebuild-mise");
        t.config(r#"["sample"]"#);
        assert_eq!(t.run(Some(&t.pnpm), Some(&t.pnpm)), [PREBUILT]);
        let folder = t.wt.join("sample");
        assert_eq!(
            t.calls(),
            [format!(
                "exec -- pnpm install --frozen-lockfile --ignore-pnpmfile {}",
                folder.display()
            )]
        );
    }

    #[cfg(unix)]
    #[test]
    fn prebuild_reports_failures() {
        let t = Prebuild::new("wt-prebuild-fails");
        t.config(r#"["sample"]"#);
        let failing = t.root.join("failing-pnpm");
        crate::shared::build::tests::script(&failing, "exit 1\n");
        assert_eq!(t.run(None, Some(&failing)), [t.failed("exit 1")]);
        chmod(&t.pnpm, 0o644);
        let out = t.run(None, Some(&t.pnpm));
        assert_eq!(out.len(), 1);
        assert!(
            out[0].starts_with("prebuild of sample failed ([Errno 13] Permission denied"),
            "{}",
            out[0]
        );
        assert!(
            out[0].ends_with(&format!("; log: {}", t.log.display())),
            "{}",
            out[0]
        );
    }

    #[test]
    fn prebuild_without_pnpm_skips() {
        let t = Prebuild::new("wt-prebuild-no-pnpm");
        t.config(r#"["sample"]"#);
        assert_eq!(
            t.run(None, None),
            ["prebuild skips sample: pnpm is not on PATH"]
        );
        t.config(r#"["missing"]"#);
        assert_eq!(
            t.run(None, None),
            ["prebuild skips missing: no pnpm-lock.yaml"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn prebuild_ignores_diff_drivers() {
        let t = Prebuild::new("wt-prebuild-drivers");
        t.config(r#"["sample"]"#);
        let marker = t.root.join("driver-ran");
        let driver = t.root.join("driver.sh");
        crate::shared::build::tests::script(
            &driver,
            &format!("echo ran >> '{}'\necho FORGED\n", marker.display()),
        );
        sh_git(&t.wt, &["config", "diff.external", &text(&driver)]);
        sh_git(&t.wt, &["config", "diff.forge.textconv", &text(&driver)]);
        fs::write(t.wt.join(".git/info/attributes"), "* diff=forge\n").unwrap();
        assert_eq!(t.run(None, Some(&t.pnpm)), [PREBUILT]);
        assert!(!marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn run_logged_times_out() {
        assert_eq!(PREBUILD_TIMEOUT, Duration::from_secs(600));
        let dir = temp_dir("wt-run-logged");
        let log = dir.join("logs/run.log");
        let argv = ["sh", "-c", "sleep 30"].map(String::from);
        assert_eq!(
            run_logged(&argv, &dir, &log, Duration::from_secs(1)),
            Err("timed out after 1 s".to_string())
        );
        assert_eq!(fs::read_to_string(&log).unwrap(), "");
        let argv = ["sh", "-c", "echo out; echo err >&2; exit 3"].map(String::from);
        assert_eq!(
            run_logged(&argv, &dir, &log, Duration::from_secs(60)),
            Ok(3)
        );
        assert_eq!(fs::read_to_string(&log).unwrap(), "out\nerr\n");
        fs::remove_dir_all(dir).unwrap();
    }

    const BRANCH: &str = "factory/001-test";
    const LEFT: &str = "branch factory/001-test: not merged into main";

    /// A checkout with main, and a job worktree whose branch holds one commit main lacks.
    struct Checkout {
        home: PathBuf,
        repo: PathBuf,
        wt: PathBuf,
    }

    impl Checkout {
        fn new(name: &str) -> Checkout {
            let home = temp_dir(name);
            let repo = home.join("checkout");
            fs::create_dir(&repo).unwrap();
            sh_git(&repo, &["init", "-q"]);
            sh_git(&repo, &["symbolic-ref", "HEAD", "refs/heads/main"]);
            commit(&repo, "base");
            let wt = home.join("worktrees/sample/001-test");
            sh_git(&repo, &["worktree", "add", "-q", &text(&wt), "-b", BRANCH]);
            fs::write(wt.join("work.txt"), "work\n").unwrap();
            sh_git(&wt, &["add", "work.txt"]);
            sh_git(&wt, &["commit", "-q", "-m", "work"]);
            Checkout { home, repo, wt }
        }

        fn merge(&self) {
            sh_git(
                &self.repo,
                &["merge", "-q", "--no-ff", "-m", "merge", BRANCH],
            );
        }

        fn origin(&self, push: bool) {
            let origin = self.home.join("origin.git");
            sh_git(&self.home, &["init", "-q", "--bare", &text(&origin)]);
            sh_git(&self.repo, &["remote", "add", "origin", &text(&origin)]);
            if push {
                sh_git(&self.repo, &["push", "-q", "origin", BRANCH]);
            }
        }

        fn more(&self) {
            fs::write(self.wt.join("more.txt"), "more\n").unwrap();
            sh_git(&self.wt, &["add", "more.txt"]);
            sh_git(&self.wt, &["commit", "-q", "-m", "more"]);
        }

        fn clean(
            &self,
            drop_pushed: bool,
            merged_head: Option<&str>,
        ) -> (Vec<String>, Vec<String>) {
            cleanup_job(
                &text(&self.repo),
                &self.wt,
                BRANCH,
                drop_pushed,
                merged_head,
            )
        }

        fn branch_exists(&self) -> bool {
            !sh_git(&self.repo, &["branch", "--list", BRANCH]).is_empty()
        }

        fn shown_wt(&self) -> String {
            format!("worktree {}", self.wt.display())
        }
    }

    impl Drop for Checkout {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn cleanup_job_removes_a_merged_job() {
        let t = Checkout::new("wt-clean-merged");
        t.merge();
        assert_eq!(
            t.clean(false, None),
            (vec![t.shown_wt(), format!("branch {BRANCH}")], vec![])
        );
        assert!(!t.wt.exists() && !t.branch_exists());

        let t = Checkout::new("wt-clean-pruned");
        t.merge();
        fs::remove_dir_all(&t.wt).unwrap();
        assert_eq!(
            t.clean(false, None),
            (vec![format!("branch {BRANCH}")], vec![])
        );
        assert!(!sh_git(&t.repo, &["worktree", "list"]).contains(&text(&t.wt)));
    }

    #[test]
    fn dirty_worktree_is_kept() {
        let t = Checkout::new("wt-clean-dirty");
        t.merge();
        fs::write(t.wt.join("untracked.txt"), "keep me\n").unwrap();
        let (removed, left) = t.clean(false, None);
        assert!(removed.is_empty());
        assert_eq!(left.len(), 2);
        assert!(
            left[0].starts_with(&format!("{}: fatal: ", t.shown_wt())),
            "{}",
            left[0]
        );
        assert!(!left[0].contains("--force"));
        assert!(
            left[0].ends_with("contains modified or untracked files"),
            "{}",
            left[0]
        );
        assert_eq!(
            left[1],
            format!("branch {BRANCH}: kept because its worktree remains")
        );
        assert_eq!(
            fs::read_to_string(t.wt.join("untracked.txt")).unwrap(),
            "keep me\n"
        );
        assert!(t.branch_exists());
    }

    #[test]
    fn running_process_keeps_everything() {
        let t = Checkout::new("wt-clean-running");
        t.merge();
        let out = cleanup_after_stop(&text(&t.repo), &t.wt, BRANCH, false, None, Some(vec![4242]));
        let left = vec![
            format!("{}: processes still running: 4242", t.shown_wt()),
            format!("branch {BRANCH}: kept because its worktree remains"),
        ];
        assert_eq!(out, (vec![], left));
        assert!(t.wt.exists() && t.branch_exists());
    }

    #[test]
    fn refused_stop_keeps_everything() {
        let t = Checkout::new("wt-clean-refused");
        t.merge();
        let out = cleanup_after_stop(&text(&t.repo), &t.wt, BRANCH, false, None, None);
        let left = vec![
            format!("{}: processes were not checked", t.shown_wt()),
            format!("branch {BRANCH}: kept because its worktree remains"),
        ];
        assert_eq!(out, (vec![], left));
        assert!(t.wt.exists() && t.branch_exists());
    }

    #[test]
    fn git_not_runnable() {
        let out = run_git_with("/nonexistent/git", ".", &["worktree", "prune"], None);
        assert_eq!(
            out,
            (
                None,
                String::new(),
                "could not run git worktree".to_string()
            )
        );
        let out = run_git_with(
            "/nonexistent/git",
            ".",
            &["rev-parse", "HEAD"],
            Some(git_timeout()),
        );
        assert_eq!(out.2, "could not run git rev-parse");
    }

    #[cfg(unix)]
    #[test]
    fn git_timeout_is_reported() {
        assert_eq!(GIT_TIMEOUT, Duration::from_secs(20));
        assert_eq!(git_timeout(), GIT_TIMEOUT);
        let dir = temp_dir("wt-slow-git");
        let slow = dir.join("git");
        crate::shared::build::tests::script(&slow, "sleep 5\n");
        let out = run_git_with(
            &text(&slow),
            ".",
            &["merge-base", "--is-ancestor"],
            Some(Duration::from_secs(1)),
        );
        assert_eq!(
            out,
            (
                None,
                String::new(),
                "git merge-base timed out after 1 s".to_string()
            )
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// Only `git worktree remove` runs without the git cap, here cut to 2 s: a wrapper that stalls it for 3 s
    /// still lets the removal finish, where a capped call would leave the worktree.
    #[cfg(unix)]
    #[test]
    fn only_worktree_remove_waits_without_a_cap() {
        let name = "commands::worktree::tests::only_worktree_remove_waits_without_a_cap";
        if std::env::var(FAKE_GIT_CHILD).is_err() {
            in_fake_git_child(name, "remove", "    sleep 3");
            return;
        }
        TEST_GIT_TIMEOUT.with(|t| t.set(Some(Duration::from_secs(2))));
        let t = Checkout::new("wt-no-cap");
        t.merge();
        let started = std::time::Instant::now();
        assert_eq!(
            t.clean(false, None),
            (vec![t.shown_wt(), format!("branch {BRANCH}")], vec![])
        );
        assert!(started.elapsed() >= Duration::from_secs(3));
    }

    #[test]
    fn bad_claim_minutes_still_reads_the_pr() {
        let dir = temp_dir("wt-bad-claim");
        fs::write(
            dir.join("config.md"),
            "---\ndevice: host-a\nclaim_stale_minutes: soon\n---\n",
        )
        .unwrap();
        assert_eq!(device_name(&dir), "host-a");
        fs::remove_dir_all(dir).unwrap();
    }

    mod cleanup_job_drop_pushed {
        use super::*;

        const UNREACHABLE: &str =
            "branch factory/001-test: not merged into main and origin could not be reached";

        #[test]
        fn merged_head_keeps_a_branch_ahead_of_the_pr() {
            let t = Checkout::new("wt-drop-ahead-of-pr");
            let merged = sh_git(&t.wt, &["rev-parse", "HEAD"]);
            t.more();
            assert_eq!(
                t.clean(true, Some(&merged)),
                (vec![t.shown_wt()], vec![UNREACHABLE.to_string()])
            );
            assert!(t.branch_exists());
        }

        #[test]
        fn merged_head_keeps_a_branch_checked_out_elsewhere() {
            let t = Checkout::new("wt-drop-elsewhere");
            let merged = sh_git(&t.wt, &["rev-parse", "HEAD"]);
            sh_git(&t.repo, &["worktree", "remove", "--force", &text(&t.wt)]);
            let elsewhere = t.home.join("elsewhere");
            sh_git(
                &t.repo,
                &["worktree", "add", "-q", &text(&elsewhere), BRANCH],
            );
            let (removed, left) = t.clean(true, Some(&merged));
            assert_eq!((removed.len(), left.len()), (0, 1));
            assert_eq!(
                left[0],
                format!("branch {BRANCH}: checked out in {}", elsewhere.display())
            );
            assert!(t.branch_exists());
        }

        #[test]
        fn cleanup_job_keeps_a_pushed_branch_by_default() {
            let t = Checkout::new("wt-drop-default");
            t.origin(true);
            assert_eq!(
                t.clean(false, None),
                (vec![t.shown_wt()], vec![LEFT.to_string()])
            );
            assert!(t.branch_exists());
        }

        #[test]
        fn cleanup_skips_ls_remote_without_a_branch_tip() {
            // rev-parse echoes an unknown ref on stdout, so only a folder outside any repository gives an empty
            // tip. A reached ls-remote there would add "and origin could not be reached".
            let dir = temp_dir("wt-drop-no-tip");
            assert_eq!(
                pushed_tip_error(&text(&dir), BRANCH),
                Some("not merged into HEAD".to_string())
            );
            fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn cleanup_deletes_a_branch_origin_holds() {
            let t = Checkout::new("wt-drop-origin-holds");
            t.origin(true);
            assert_eq!(
                t.clean(true, None),
                (vec![t.shown_wt(), format!("branch {BRANCH}")], vec![])
            );
            assert!(!t.branch_exists());
        }

        #[test]
        fn cleanup_keeps_a_branch_ahead_of_origin() {
            let t = Checkout::new("wt-drop-ahead");
            t.origin(true);
            t.more();
            let left = format!("{LEFT} and origin is at a different commit");
            assert_eq!(t.clean(true, None), (vec![t.shown_wt()], vec![left]));
            assert!(t.branch_exists());
        }

        #[test]
        fn cleanup_keeps_a_branch_origin_lacks() {
            let t = Checkout::new("wt-drop-lacks");
            t.origin(false);
            let left = format!("{LEFT} and origin has no such branch");
            assert_eq!(t.clean(true, None), (vec![t.shown_wt()], vec![left]));
            assert!(t.branch_exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn stop_pids_without_lsof_is_unchecked() {
        let dir = temp_dir("wt-no-lsof");
        assert_eq!(stop_pids_with(&dir, &dir.join("no-proc"), None), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stop_pids_never_signals_a_group() {
        assert_eq!(
            (single_pid(0), single_pid(u32::MAX), single_pid(7)),
            (None, None, Some(7))
        );
        let dir = temp_dir("wt-bad-lsof");
        let lsof = dir.join("lsof");
        let wt = dir.display();
        crate::shared::build::tests::script(
            &lsof,
            &format!("printf 'p4294967295\\nn{wt}\\np0\\nn{wt}\\n'\n"),
        );
        let alive = stop_pids_with(&dir, &dir.join("no-proc"), Some(lsof));
        assert_eq!(alive, Some(vec![0, 4294967295]));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn stop_pids_refuses_a_linked_or_empty_worktree() {
        let dir = temp_dir("wt-linked-stop");
        let real = dir.join("real/001-test");
        fs::create_dir_all(&real).unwrap();
        let lsof = dir.join("lsof");
        let cwd = std::env::current_dir().unwrap();
        // Unsignallable pids, so a call that was not refused lists them as alive.
        crate::shared::build::tests::script(
            &lsof,
            &format!(
                "printf 'p4294967295\\nn{}\\np4294967294\\nn{}\\n'\n",
                real.display(),
                cwd.display()
            ),
        );
        let stop = |wt: &Path| stop_pids_with(wt, &dir.join("no-proc"), Some(lsof.clone()));
        assert_eq!(stop(&real), Some(vec![4294967295]));
        std::os::unix::fs::symlink(&real, dir.join("link")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("slug")).unwrap();
        assert_eq!(stop(&dir.join("link")), None);
        assert_eq!(stop(&dir.join("slug/001-test")), None);
        assert_eq!(stop(Path::new("")), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn lsof_falls_back() {
        let dir = temp_dir("wt-lsof");
        let lsof = dir.join("lsof");
        crate::shared::build::tests::script(&lsof, "");
        let plain = dir.join("plain");
        fs::write(&plain, "").unwrap();
        let fallbacks = [dir.join("missing"), plain, lsof.clone()];
        let fallbacks: Vec<&Path> = fallbacks.iter().map(PathBuf::as_path).collect();
        assert_eq!(lsof_exe_with(None, &fallbacks), Some(lsof));
        assert_eq!(
            lsof_exe_with(Some(PathBuf::from("/found/lsof")), &fallbacks),
            Some(PathBuf::from("/found/lsof"))
        );
        assert_eq!(lsof_exe_with(None, &fallbacks[..2]), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stop_pids_ends_a_child_in_the_worktree() {
        let dir = temp_dir("wt-stop-child");
        let mut child = Command::new("sleep")
            .arg("30")
            .current_dir(&dir)
            .spawn()
            .unwrap();
        let pid = child.id();
        let reaper = thread::spawn(move || child.wait().unwrap());
        assert_eq!(stop_pids_with(&dir, Path::new("/proc"), None), Some(vec![]));
        assert!(!reaper.join().unwrap().success());
        assert!(!proc_alive(pid));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn linked_prompt_folder_is_kept() {
        let home = temp_dir("wt-linked-prompts");
        let target = home.join("elsewhere");
        fs::create_dir_all(target.join("001-test")).unwrap();
        fs::write(target.join("001-test/keep.txt"), "keep\n").unwrap();
        let folder = home.join("prompts/sample/001-test");
        for (link, points_to) in [
            (folder.clone(), target.join("001-test")),
            (folder.parent().unwrap().to_path_buf(), target.clone()),
        ] {
            let _ = fs::remove_dir_all(home.join("prompts"));
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&points_to, &link).unwrap();
            let (mut removed, mut left) = (Vec::new(), Vec::new());
            cleanup_prompts(&home, "sample", "001-test", &mut removed, &mut left).unwrap();
            assert!(removed.is_empty());
            assert_eq!(
                left,
                [format!(
                    "prompts {}: a symlink or junction, left in place",
                    folder.display()
                )]
            );
            assert_eq!(
                fs::read_to_string(target.join("001-test/keep.txt")).unwrap(),
                "keep\n"
            );
        }
        let _ = fs::remove_dir_all(home.join("prompts"));
        fs::create_dir_all(&folder).unwrap();
        let (mut removed, mut left) = (Vec::new(), vec!["worktree x: busy".to_string()]);
        cleanup_prompts(&home, "sample", "001-test", &mut removed, &mut left).unwrap();
        assert_eq!(
            left[1],
            format!(
                "prompts {}: kept because its worktree remains",
                folder.display()
            )
        );
        left.clear();
        cleanup_prompts(&home, "sample", "001-test", &mut removed, &mut left).unwrap();
        assert_eq!(
            (removed, left.len()),
            (vec![format!("prompts {}", folder.display())], 0)
        );
        assert!(!folder.exists() && folder.parent().unwrap().is_dir());
        fs::remove_dir_all(home).unwrap();
    }
}
