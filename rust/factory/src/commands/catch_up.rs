//! `catch-up`: bring this host to origin's skill, pickup timer and Worker, backing up before any
//! local write.

#[cfg(test)]
use std::cell::RefCell;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::rc::Rc;
use std::time::Duration;

use serde_json::Value;

use crate::cli::Call;
use crate::commands::pickup;
use crate::shared::conf::{fm_get, frontmatter};
use crate::shared::exit::{exit, fail, Res};
use crate::shared::fs::{read_text, resolve, temp_root};
use crate::shared::home::factory_home;
use crate::shared::proc::{capture, git, origin_git, Out};
use crate::shared::project::{project_files, read_paths};
use crate::shared::py::{os_error, repr, splitlines, strip};
use crate::shared::pyjson::loads;
use crate::shared::pyre::{Flags, Pattern};
use crate::shared::pyval::{py_str, truthy};
use crate::shared::time::{format_stamp, unix_now};

const CATCH_UP_HOME_FILES: [&str; 5] = [
    "config.md",
    "paths.md",
    "routes.md",
    "launchers.json",
    ".env",
];
static CHECKOUT_SLUG: Pattern = Pattern::new("[A-Za-z0-9_-]+", Flags::NONE);
static REPO_PATH: Pattern = Pattern::new("[A-Za-z0-9_./-]{1,200}", Flags::ASCII);
static GITHUB: Pattern = Pattern::new(r"github\.com[:/]([^/]+/[^/]+?)(?:\.git)?$", Flags::NONE);
const TIMER_TIMEOUT: Duration = Duration::from_secs(120);

macro_rules! outln {
    ($out:expr, $($t:tt)*) => {{ let _ = writeln!($out, $($t)*); }};
}

// ---- test seams

/// One field per name the Python catch-up tests patch.
#[cfg(test)]
#[derive(Default)]
struct Fakes {
    scheduler: Option<&'static str>,
    timer_installed: Option<bool>,
    timer_drift: Option<Option<String>>,
    gh: Option<Rc<dyn Fn(&str, &[&str]) -> (i32, String)>>,
    setup_run: Option<Rc<dyn Fn(&[&str]) -> (i32, String)>>,
    backup: Option<Rc<dyn Fn() -> Result<PathBuf, String>>>,
    user_home: Option<PathBuf>,
    run: Option<Rc<dyn Fn(&[String], Option<Duration>) -> Option<io::Result<Out>>>>,
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

fn scheduler() -> &'static str {
    #[cfg(test)]
    if let Some(s) = fake!(scheduler) {
        return s;
    }
    pickup::pickup_scheduler()
}

fn timer_installed(scheduler: &str) -> bool {
    #[cfg(test)]
    if let Some(t) = fake!(timer_installed) {
        return t;
    }
    pickup::timer_installed(scheduler).0
}

/// `bool(timer_drift(scheduler, timer_values(home)))`.
fn timer_drift(scheduler: &str, home: &Path) -> bool {
    #[cfg(test)]
    if let Some(d) = fake!(timer_drift) {
        return d.is_some_and(|s| !s.is_empty());
    }
    pickup::timer_drift(scheduler, &pickup::timer_values(home)).is_some_and(|s| !s.is_empty())
}

fn setup_run(argv: &[&str]) -> (i32, String) {
    #[cfg(test)]
    if let Some(f) = fake!(setup_run) {
        return f(argv);
    }
    pickup::setup_run(argv, None)
}

/// `Path.home()`.
fn user_home() -> Res<PathBuf> {
    #[cfg(test)]
    if let Some(p) = fake!(user_home) {
        return Ok(p);
    }
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| fail("Could not determine home directory.", 1))
}

/// `subprocess.run(argv, capture_output=True, text=True, timeout=timeout)`. A child that cannot
/// start, or times out, gives code 1 with Python's error text on stderr.
fn run(argv: &[String], timeout: Option<Duration>) -> Out {
    #[cfg(test)]
    if let Some(r) = fake!(run).and_then(|f| f(argv, timeout)) {
        return finished(argv, timeout, r);
    }
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    finished(argv, timeout, capture(&args, None, &[], timeout))
}

fn finished(argv: &[String], timeout: Option<Duration>, r: io::Result<Out>) -> Out {
    let e = match r {
        Ok(o) => return o,
        Err(e) => e,
    };
    let stderr = match timeout {
        Some(t) if e.kind() == io::ErrorKind::TimedOut => {
            let shown: Vec<String> = argv.iter().map(|a| repr(a)).collect();
            format!(
                "Command '[{}]' timed out after {} seconds",
                shown.join(", "),
                t.as_secs()
            )
        }
        _ => os_error(&e, Some(Path::new(&argv[0]))),
    };
    Out {
        code: 1,
        stdout: String::new(),
        stderr,
    }
}

fn git_argv(repo: &Path, args: &[&str]) -> Vec<String> {
    ["git", "-C"]
        .into_iter()
        .map(String::from)
        .chain(std::iter::once(repo.to_string_lossy().into_owned()))
        .chain(args.iter().map(|a| a.to_string()))
        .collect()
}

/// Lowercase hex characters from n random bytes: `secrets.token_hex(n)`.
fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    #[cfg(unix)]
    {
        use std::io::Read;
        fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut buf))
            .expect("read /dev/urandom");
    }
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        extern "system" {
            fn BCryptGenRandom(alg: *mut u8, buf: *mut u8, len: u32, flags: u32) -> i32;
        }
        // SAFETY: buf is writable for its n bytes, and the system RNG flag needs no algorithm handle.
        let status =
            unsafe { BCryptGenRandom(std::ptr::null_mut(), buf.as_mut_ptr(), n as u32, 0x2) };
        assert_eq!(status, 0, "BCryptGenRandom failed");
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// The first seven characters, as Python's `[:7]`.
fn short(s: &str) -> String {
    s.chars().take(7).collect()
}

// ---- the five Python functions

/// (slug, repo) of the first paths.md checkout, by slug, that holds the factory installer and skill.
fn catch_up_repo(home: &Path) -> Res<(String, PathBuf)> {
    let mut paths: Vec<(String, String)> = Vec::new();
    for (slug, repo) in read_paths(home)? {
        match paths.iter_mut().find(|(s, _)| *s == slug) {
            Some(slot) => slot.1 = repo,
            None => paths.push((slug, repo)),
        }
    }
    paths.sort();
    for (slug, repo) in paths {
        let repo = PathBuf::from(repo);
        if CHECKOUT_SLUG.is_full(&slug)
            && repo.join("tools").join("install.py").is_file()
            && repo
                .join("skills")
                .join("factory")
                .join("SKILL.md")
                .is_file()
        {
            return Ok((slug, repo));
        }
    }
    Err(fail(
        "no factory checkout in paths.md; run factory.py init <factory checkout>",
        1,
    ))
}

/// The interpreter that runs a Python bundle: python3, or the py launcher on Windows.
fn python() -> Vec<String> {
    if cfg!(windows) {
        vec!["py".into(), "-3".into()]
    } else {
        vec!["python3".into()]
    }
}

/// The argv prefix that runs an installed bundle: its bundle.json entry when valid, else its runtime.
fn catch_up_entry(skill: &Path) -> Vec<String> {
    if let Some(entry) = crate::shared::home::bundle_entry(skill) {
        return entry;
    }
    let mut argv = python();
    argv.push(
        skill
            .join("runtime")
            .join("factory.py")
            .to_string_lossy()
            .into_owned(),
    );
    argv
}

/// {commit, tree} from the bundle.json install.py writes in skill, or None. A Rust build adds entry
/// and binary_sha256 beside them.
fn bundle_stamp(skill: &Path) -> Option<Value> {
    crate::commands::setup::stamp_of(&loads(&read_text(&skill.join("bundle.json")).ok()?).ok()?)
}

/// `shutil.copy2`: the file's bytes and mode, then its mtime. dst may be a folder.
fn copy2(src: &Path, dst: &Path) -> Result<(), String> {
    let dst = if dst.is_dir() {
        dst.join(src.file_name().unwrap_or_default())
    } else {
        dst.to_path_buf()
    };
    fs::copy(src, &dst).map_err(|e| os_error(&e, Some(src)))?;
    let mtime = fs::metadata(src)
        .and_then(|m| m.modified())
        .map_err(|e| os_error(&e, Some(src)))?;
    fs::File::options()
        .write(true)
        .open(&dst)
        .and_then(|f| f.set_modified(mtime))
        .map_err(|e| os_error(&e, Some(&dst)))
}

/// `shutil.copytree(src, dst, symlinks=True, ignore=ignore_patterns("__pycache__"))`.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| os_error(&e, Some(dst)))?;
    let entries = fs::read_dir(src).map_err(|e| os_error(&e, Some(src)))?;
    for entry in entries {
        let entry = entry.map_err(|e| os_error(&e, Some(src)))?;
        let name = entry.file_name();
        if name == "__pycache__" {
            continue;
        }
        let (from, to) = (entry.path(), dst.join(&name));
        let kind = entry.file_type().map_err(|e| os_error(&e, Some(&from)))?;
        if kind.is_symlink() {
            let target = fs::read_link(&from).map_err(|e| os_error(&e, Some(&from)))?;
            #[cfg(unix)]
            let made = std::os::unix::fs::symlink(&target, &to);
            #[cfg(windows)]
            let made = if from.is_dir() {
                std::os::windows::fs::symlink_dir(&target, &to)
            } else {
                std::os::windows::fs::symlink_file(&target, &to)
            };
            made.map_err(|e| os_error(&e, Some(&to)))?;
        } else if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            copy2(&from, &to)?;
        }
    }
    let mode = fs::metadata(src).map_err(|e| os_error(&e, Some(src)))?;
    fs::set_permissions(dst, mode.permissions()).map_err(|e| os_error(&e, Some(dst)))
}

/// A new private folder under <home>/backups holding the installed skill, this host's timers and
/// the home config.
fn catch_up_backup(home: &Path, skill: &Path) -> Result<PathBuf, String> {
    #[cfg(test)]
    if let Some(f) = fake!(backup) {
        return f();
    }
    let stamp = format_stamp(unix_now()).replace(['-', ':'], "") + "-" + &random_hex(3);
    let backups = home.join("backups");
    let folder = backups.join(format!("catch-up-{stamp}"));
    fs::create_dir_all(&backups).map_err(|e| os_error(&e, Some(&backups)))?;
    fs::create_dir(&folder).map_err(|e| os_error(&e, Some(&folder)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700))
            .map_err(|e| os_error(&e, Some(&folder)))?;
    }
    copy_tree(skill, &folder.join("skill").join("factory"))?;
    if scheduler() == "cron" {
        let (code, out) = setup_run(&["crontab", "-l"]);
        if code == 0 {
            let path = folder.join("crontab.txt");
            fs::write(&path, out).map_err(|e| os_error(&e, Some(&path)))?;
        }
    }
    let user = user_home().map_err(|e| {
        strip(&e.stderr)
            .trim_start_matches("factory.py: ")
            .to_string()
    })?;
    let sources = [
        (
            user.join("Library").join("LaunchAgents"),
            ("factory.", ".plist"),
            "LaunchAgents",
        ),
        (user.join(".config"), ("factory-", ".cmd"), "config"),
    ];
    for (source, (prefix, suffix), name) in sources {
        let mut found: Vec<PathBuf> = fs::read_dir(&source)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        let n = e.file_name().to_string_lossy().into_owned();
                        n.len() >= prefix.len() + suffix.len()
                            && n.starts_with(prefix)
                            && n.ends_with(suffix)
                    })
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        found.sort();
        for path in found {
            let to = folder.join(name);
            match fs::create_dir(&to) {
                Err(e) if e.kind() != io::ErrorKind::AlreadyExists || !to.is_dir() => {
                    return Err(os_error(&e, Some(&to)))
                }
                _ => {}
            }
            copy2(&path, &to)?;
        }
    }
    let saved = folder.join("home");
    fs::create_dir(&saved).map_err(|e| os_error(&e, Some(&saved)))?;
    for name in CATCH_UP_HOME_FILES {
        if home.join(name).is_file() {
            copy2(&home.join(name), &saved.join(name))?;
        }
    }
    Ok(folder)
}

/// (returncode, output) of gh on the GitHub repository owner/name slug.
fn catch_up_gh(slug: &str, args: &[&str]) -> (i32, String) {
    #[cfg(test)]
    if let Some(f) = fake!(gh) {
        return f(slug, args);
    }
    let argv: Vec<&str> = std::iter::once("gh")
        .chain(args.iter().copied())
        .chain(["-R", slug])
        .collect();
    pickup::setup_run(&argv, None)
}

/// `check_path`: why value is not a safe relative repo path, or None.
fn check_path(value: &str) -> Option<&'static str> {
    if !REPO_PATH.is_full(value) || value.starts_with('/') || value.split('/').any(|s| s == "..") {
        return Some("must be a relative repo path of [A-Za-z0-9_./-] with no .. segment");
    }
    None
}

pub fn catch_up(_call: &Call) -> Res<()> {
    let home = factory_home()?;
    let exe = std::env::current_exe()
        .and_then(|p| resolve(&p))
        .map_err(|e| fail(os_error(&e, None), 1))?;
    let here = exe.parent().unwrap_or(Path::new("/"));
    let stdout = io::stdout();
    catch_up_cmd(&mut stdout.lock(), &home, here)
}

/// Bring this host to origin's skill, pickup timer and Worker, backing up before any local write.
/// Exit 0 when done or nothing is new, 1 when a step failed, 5 when the fetch failed.
fn catch_up_cmd(out: &mut dyn Write, home: &Path, here: &Path) -> Res<()> {
    let skill = here.parent().unwrap_or(here);
    let (slug, repo) = catch_up_repo(home)?;
    let resolved = resolve(&repo).unwrap_or_else(|_| repo.clone());
    if here.ancestors().skip(1).any(|p| p == resolved) {
        return Err(fail(
            format!(
                "catch-up runs from the installed skill, not from the checkout {}",
                repo.display()
            ),
            2,
        ));
    }
    let project = project_files(home, &slug)?.0;
    let base = if project.exists() {
        let meta = frontmatter(&project)
            .map_err(|e| fail(os_error(&e, Some(&project)), 1))?
            .0;
        fm_get(&meta, "base").unwrap_or("main").to_string()
    } else {
        "main".to_string()
    };
    if check_path(&base).is_some() || base.starts_with('-') {
        return Err(fail(
            format!("{slug} base {base} is not a safe branch name"),
            1,
        ));
    }
    origin_git(
        &repo,
        &[
            "fetch",
            "-q",
            "origin",
            &format!("+refs/heads/{base}:refs/remotes/origin/{base}"),
        ],
    )?;
    let head = git(
        &repo,
        &["rev-parse", &format!("refs/remotes/origin/{base}")],
    );
    let h7 = short(&head);
    outln!(
        out,
        "catch-up: factory checkout {}, origin/{base} at {h7}",
        repo.display()
    );
    let (mut failed, mut wrote) = (0, false);

    let branch = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let why = if branch == "HEAD" {
        Some("detached HEAD".to_string())
    } else if !git(&repo, &["status", "--porcelain"]).is_empty() {
        Some("dirty".to_string())
    } else if branch != base {
        Some(format!("on branch {branch}"))
    } else {
        None
    };
    if let Some(why) = why {
        outln!(out, "checkout: left as is ({why})");
    } else if git(&repo, &["rev-parse", "HEAD"]) == head {
        outln!(out, "checkout: already at {h7}");
    } else {
        let r = run(&git_argv(&repo, &["merge", "-q", "--ff-only", &head]), None);
        if r.code != 0 {
            failed += 1;
            let mut lines: Vec<&str> = splitlines(&r.stderr, false)
                .into_iter()
                .filter(|l| !strip(l).is_empty())
                .collect();
            if lines.is_empty() {
                lines.push("");
            }
            let line = lines
                .iter()
                .find(|l| l.starts_with("fatal:"))
                .unwrap_or(&lines[0]);
            outln!(out, "checkout: {line}");
        } else {
            wrote = true;
            outln!(
                out,
                "checkout: fast-forwarded to {}",
                short(&git(&repo, &["rev-parse", "HEAD"]))
            );
        }
    }

    let stamp = bundle_stamp(skill);
    let installed = stamp.as_ref().map(|s| py_str(s.get("commit")));
    let current = installed.as_deref() == Some(head.as_str());
    let scheduler = scheduler();
    let timer = timer_installed(scheduler);
    let stale = timer && (!current || timer_drift(scheduler, home));
    if current {
        outln!(out, "skill: current at {h7}");
    } else {
        let have = installed.map_or_else(|| "unknown".to_string(), |c| short(&c));
        outln!(out, "skill: {have} installed, {h7} on origin/{base}");
    }
    if current && !stale {
        if timer {
            outln!(out, "pickup timer: current");
        } else {
            outln!(out, "pickup timer: none on this host, left alone");
        }
    } else {
        let backup = match catch_up_backup(home, skill) {
            Ok(folder) => {
                wrote = true;
                outln!(out, "backup: {}", folder.display());
                Some(folder)
            }
            Err(e) => {
                failed += 1;
                outln!(out, "backup: {e}");
                None
            }
        };
        let mut ok = backup.is_some();
        if ok && !current {
            let tmp = temp_root()?.join(format!("factory-catch-up-{}", random_hex(4)));
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder
                .create(&tmp)
                .map_err(|e| fail(os_error(&e, Some(&tmp)), 1))?;
            let src = tmp.join("src");
            let src_text = src.to_string_lossy().into_owned();
            let mut r = run(
                &git_argv(
                    &repo,
                    &["worktree", "add", "-q", "--detach", &src_text, &head],
                ),
                None,
            );
            if r.code == 0 {
                let mut argv = python();
                argv.extend([
                    "-B".to_string(),
                    src.join("tools")
                        .join("install.py")
                        .to_string_lossy()
                        .into_owned(),
                    "--destination".to_string(),
                    skill
                        .parent()
                        .unwrap_or(skill)
                        .to_string_lossy()
                        .into_owned(),
                    "--replace".to_string(),
                ]);
                r = run(&argv, None);
            }
            if src.exists() {
                run(&git_argv(&repo, &["worktree", "remove", &src_text]), None);
            }
            if !src.exists() {
                fs::remove_dir(&tmp).map_err(|e| fail(os_error(&e, Some(&tmp)), 1))?;
            }
            ok = r.code == 0;
            if ok {
                outln!(out, "skill: installed {h7} into {}", skill.display());
            } else {
                failed += 1;
                let text = if r.stderr.is_empty() {
                    &r.stdout
                } else {
                    &r.stderr
                };
                outln!(out, "skill: {}", strip(text));
            }
        }
        if backup.is_some() && !ok && timer {
            outln!(out, "pickup timer: skipped, skill install failed");
        } else if ok && !timer {
            outln!(out, "pickup timer: none on this host, left alone");
        } else if ok {
            let home_text = home.to_string_lossy();
            let mut argv = catch_up_entry(skill);
            argv.extend(["--home", &home_text, "pickup", "setup", "--install"].map(String::from));
            let r = run(&argv, Some(TIMER_TIMEOUT));
            let text = if r.code == 0 || r.code == 3 {
                r.stdout
            } else {
                r.stderr + &r.stdout
            };
            let text = strip(&text);
            // pickup setup --install exits 3 after installing the timer while another pickup check is missing.
            if r.code == 0 || r.code == 3 {
                outln!(
                    out,
                    "pickup timer: {}",
                    text.split('\n').next().unwrap_or("")
                );
                if r.code == 3 {
                    outln!(out, "pickup timer: some pickup checks are missing; run factory.py pickup setup to see them");
                }
            } else {
                failed += 1;
                outln!(out, "pickup timer: {text}");
            }
        }
    }

    let url = git(&repo, &["config", "--get", "remote.origin.url"]);
    let found = GITHUB.search(&url);
    let workflow = format!("{head}:.github/workflows/deploy-main.yml");
    if let Some(found) = found {
        let gh_slug = found.get(1).map_or("", |m| m.as_str());
        if run(&git_argv(&repo, &["cat-file", "-e", &workflow]), None).code != 0 {
            outln!(
                out,
                "worker: skipped, origin/{base} has no .github/workflows/deploy-main.yml"
            );
        } else {
            let (mut code, text) = catch_up_gh(
                gh_slug,
                &[
                    "run",
                    "list",
                    "--workflow",
                    "deploy-main.yml",
                    "--commit",
                    &head,
                    "--json",
                    "databaseId,status,conclusion,attempt,url",
                    "--limit",
                    "1",
                ],
            );
            let mut runs = None;
            if code == 0 {
                match loads(&text) {
                    Ok(v) => runs = Some(v),
                    Err(_) => code = 1,
                }
            }
            if code == 127 {
                outln!(out, "worker: skipped, gh did not run: {}", strip(&text));
            } else if code != 0 {
                failed += 1;
                outln!(out, "worker: {}", strip(&text));
            } else if !truthy(runs.as_ref()) {
                outln!(
                    out,
                    "worker: no deploy run for {h7} yet; run catch-up again later"
                );
            } else {
                let first = runs
                    .as_ref()
                    .and_then(Value::as_array)
                    .and_then(|r| r.first())
                    .filter(|r| {
                        let int = |k: &str| r.get(k).is_some_and(|v| v.is_i64() || v.is_u64());
                        r.get("status").is_some_and(Value::is_string)
                            && r.get("url").is_some_and(Value::is_string)
                            && r.get("conclusion")
                                .is_some_and(|c| c.is_string() || c.is_null())
                            && int("attempt")
                            && int("databaseId")
                    })
                    .ok_or_else(|| fail("gh run list printed runs of an unexpected shape", 1))?;
                let (status, run_url) = (py_str(first.get("status")), py_str(first.get("url")));
                let conclusion = py_str(first.get("conclusion"));
                let attempt = &first["attempt"];
                if status != "completed" {
                    outln!(out, "worker: deploy run {run_url} is {status}");
                } else if conclusion == "success" {
                    outln!(out, "worker: deployed at {h7}");
                } else if attempt.as_i64().is_some_and(|a| a < 2) {
                    let id = py_str(first.get("databaseId"));
                    let (code, text) = catch_up_gh(gh_slug, &["run", "rerun", &id]);
                    if code != 0 {
                        failed += 1;
                        outln!(out, "worker: {}", strip(&text));
                    } else {
                        wrote = true;
                        outln!(out, "worker: reran deploy run {run_url}");
                    }
                } else {
                    failed += 1;
                    outln!(
                        out,
                        "worker: deploy run {run_url} ended {conclusion} after {} attempts; check it by hand",
                        py_str(Some(attempt))
                    );
                }
            }
        }
    } else {
        outln!(out, "worker: skipped, origin is not on GitHub");
    }

    if failed > 0 {
        outln!(out, "catch-up: {failed} step(s) failed");
        return Err(exit(1));
    }
    if wrote {
        outln!(out, "catch-up: done");
    } else {
        outln!(out, "catch-up: nothing new");
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::shared::fs::{temp_dir, TEMP_ROOT};
    use serde_json::json;
    use std::collections::VecDeque;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    const GITHUB_URL: &str = "https://github.com/example/factory.git";
    const RUN_URL: &str = "https://github.com/example/factory/actions/runs/7";
    static BACKUP_NAME: Pattern = Pattern::new(r"catch-up-\d{8}T\d{6}Z-[0-9a-f]{6}", Flags::NONE);

    type Calls = Rc<RefCell<Vec<Vec<String>>>>;

    fn sh_git(cwd: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8(o.stdout).unwrap().trim().to_string()
    }

    fn text(p: &Path) -> &str {
        p.to_str().unwrap()
    }

    fn runs(status: &str, conclusion: &str, attempt: i64) -> (i32, String) {
        let run = serde_json::json!([{"databaseId": 7, "status": status, "conclusion": conclusion,
                                      "attempt": attempt, "url": RUN_URL}]);
        (0, run.to_string())
    }

    fn deployed() -> (i32, String) {
        runs("completed", "success", 1)
    }

    /// A src repo pushed to a bare origin.git, a checkout cloned from it, and a skill installed
    /// from commit A while origin is at commit B.
    struct T {
        root: PathBuf,
        src: PathBuf,
        origin: PathBuf,
        checkout: PathBuf,
        skill: PathBuf,
        home: PathBuf,
        a: String,
        b: String,
        gh_calls: Calls,
        gh_replies: Rc<RefCell<VecDeque<(i32, String)>>>,
    }

    impl Drop for T {
        fn drop(&mut self) {
            FAKES.with(|f| *f.borrow_mut() = Fakes::default());
            TEMP_ROOT.with(|r| *r.borrow_mut() = None);
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    impl T {
        fn new(name: &str) -> T {
            let root = temp_dir(&format!("catch-up-{name}"));
            TEMP_ROOT.with(|r| *r.borrow_mut() = Some(root.clone()));
            let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let (src, origin, checkout) = (
                root.join("src"),
                root.join("origin.git"),
                root.join("checkout"),
            );
            fs::create_dir_all(src.join("tools")).unwrap();
            fs::copy(repo.join("tools/install.py"), src.join("tools/install.py")).unwrap();
            copy_tree(&repo.join("skills/factory"), &src.join("skills/factory")).unwrap();
            fs::create_dir_all(src.join(".github/workflows")).unwrap();
            fs::write(
                src.join(".github/workflows/deploy-main.yml"),
                "name: deploy-main\n",
            )
            .unwrap();
            sh_git(&root, &["init", "-q", text(&src)]);
            sh_git(&src, &["symbolic-ref", "HEAD", "refs/heads/main"]);
            let a = commit(&src, "A");
            sh_git(&root, &["clone", "-q", "--bare", text(&src), text(&origin)]);
            sh_git(&src, &["remote", "add", "origin", text(&origin)]);
            sh_git(&root, &["clone", "-q", text(&origin), text(&checkout)]);
            sh_git(
                &checkout,
                &[
                    "config",
                    &format!("url.{}.insteadOf", origin.display()),
                    GITHUB_URL,
                ],
            );
            sh_git(&checkout, &["remote", "set-url", "origin", GITHUB_URL]);
            let skills = root.join("skills");
            let installed = Command::new("python3")
                .arg("-B")
                .arg(src.join("tools/install.py"))
                .arg("--destination")
                .arg(&skills)
                .output()
                .unwrap();
            assert!(installed.status.success());
            let skill_md = src.join("skills/factory/SKILL.md");
            let mut body = fs::read_to_string(&skill_md).unwrap();
            body.push_str("\nCommit B.\n");
            fs::write(&skill_md, body).unwrap();
            let b = commit(&src, "B");

            let home = root.join("state");
            fs::create_dir(&home).unwrap();
            fs::write(
                home.join("paths.md"),
                format!("---\nfactory: {}\n---\n", checkout.display()),
            )
            .unwrap();
            fs::write(home.join("config.md"), "---\ndevice: example-host\n---\n").unwrap();
            fs::write(home.join(".env"), "FACTORY_ACCESS_CLIENT_ID=synthetic-id\n").unwrap();
            fs::set_permissions(home.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();

            let gh_calls: Calls = Rc::default();
            let gh_replies: Rc<RefCell<VecDeque<(i32, String)>>> = Rc::default();
            let (calls, replies) = (gh_calls.clone(), gh_replies.clone());
            let user = root.clone();
            FAKES.with(|f| {
                *f.borrow_mut() = Fakes {
                    scheduler: Some("cron"),
                    timer_installed: Some(false),
                    timer_drift: Some(Some("synthetic drift".into())),
                    gh: Some(Rc::new(move |slug, args| {
                        let call = std::iter::once(slug)
                            .chain(args.iter().copied())
                            .map(String::from)
                            .collect();
                        calls.borrow_mut().push(call);
                        replies.borrow_mut().pop_front().unwrap_or_else(deployed)
                    })),
                    setup_run: Some(Rc::new(|_| (0, "# synthetic crontab\n".into()))),
                    user_home: Some(user),
                    ..Fakes::default()
                }
            });
            T {
                skill: skills.join("factory"),
                root,
                src,
                origin,
                checkout,
                home,
                a,
                b,
                gh_calls,
                gh_replies,
            }
        }

        fn commit(&self, message: &str) -> String {
            commit(&self.src, message)
        }

        fn edit_installer(&self, old: &str, new: &str) {
            let path = self.src.join("tools/install.py");
            let body = fs::read_to_string(&path).unwrap();
            assert!(body.contains(old));
            fs::write(&path, body.replace(old, new)).unwrap();
        }

        fn call(&self) -> (i32, String, String) {
            self.call_from(&self.skill.join("runtime"))
        }

        fn call_from(&self, here: &Path) -> (i32, String, String) {
            let mut out = Vec::new();
            let (code, err) = match catch_up_cmd(&mut out, &self.home, here) {
                Ok(()) => (0, String::new()),
                Err(e) => (e.code, e.stderr),
            };
            (code, String::from_utf8(out).unwrap(), err)
        }

        fn lines(&self) -> (i32, Vec<String>) {
            let (code, out, _) = self.call();
            (code, out.lines().map(String::from).collect())
        }

        fn installed(&self) -> String {
            let stamp: Value =
                serde_json::from_str(&fs::read_to_string(self.skill.join("bundle.json")).unwrap())
                    .unwrap();
            stamp["commit"].as_str().unwrap().to_string()
        }

        fn backups(&self) -> Vec<PathBuf> {
            let mut found: Vec<PathBuf> = fs::read_dir(self.home.join("backups"))
                .map(|d| d.map(|e| e.unwrap().path()).collect())
                .unwrap_or_default();
            found.sort();
            found
        }

        fn worktrees(&self) -> usize {
            sh_git(&self.checkout, &["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count()
        }

        /// Fakes the timer child: it records each argv and replies with code and out.
        fn timer_calls(&self, out: &str, code: i32) -> Calls {
            let calls: Calls = Rc::default();
            let (seen, out) = (calls.clone(), out.to_string());
            FAKES.with(|f| {
                f.borrow_mut().run = Some(Rc::new(move |argv, timeout| {
                    if !argv.iter().any(|a| a == "pickup") {
                        return None;
                    }
                    assert_eq!(timeout, Some(Duration::from_secs(120)));
                    seen.borrow_mut().push(argv.to_vec());
                    Some(Ok(Out {
                        code,
                        stdout: out.clone(),
                        stderr: String::new(),
                    }))
                }))
            });
            calls
        }

        fn timer(&self, installed: bool) {
            FAKES.with(|f| f.borrow_mut().timer_installed = Some(installed));
        }

        fn drift(&self, drift: Option<&str>) {
            FAKES.with(|f| f.borrow_mut().timer_drift = Some(drift.map(String::from)));
        }
    }

    fn commit(src: &Path, message: &str) -> String {
        sh_git(src, &["add", "-A"]);
        sh_git(src, &["commit", "-q", "-m", message]);
        if message != "A" {
            sh_git(src, &["push", "-q", "origin", "main"]);
        }
        sh_git(src, &["rev-parse", "HEAD"])
    }

    fn default_timer_reply() -> (&'static str, i32) {
        ("installed cron timer: crontab\n1. ok home: synthetic\n", 3)
    }

    #[test]
    fn installs_origin_head_with_backup() {
        let t = T::new("installs");
        fs::write(
            t.home.join("paths.md"),
            format!("---\nother: {}\n---\n", t.root.display()),
        )
        .unwrap();
        assert_eq!(
            t.call(),
            (
                1,
                String::new(),
                "factory.py: no factory checkout in paths.md; run factory.py init <factory checkout>\n"
                    .into()
            )
        );
        fs::write(
            t.home.join("paths.md"),
            format!("---\nfactory: {}\n---\n", t.checkout.display()),
        )
        .unwrap();
        assert_eq!(
            t.call_from(&t.checkout.join("skills/factory/runtime")),
            (
                2,
                String::new(),
                format!(
                    "factory.py: catch-up runs from the installed skill, not from the checkout {}\n",
                    t.checkout.display()
                )
            )
        );
        assert!(t.backups().is_empty());

        let (code, out, err) = t.call();
        assert_eq!((code, err.as_str()), (0, ""));
        let backups = t.backups();
        assert_eq!(backups.len(), 1);
        let backup = &backups[0];
        let (a7, b7) = (&t.a[..7], &t.b[..7]);
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                format!(
                    "catch-up: factory checkout {}, origin/main at {b7}",
                    t.checkout.display()
                ),
                format!("checkout: fast-forwarded to {b7}"),
                format!("skill: {a7} installed, {b7} on origin/main"),
                format!("backup: {}", backup.display()),
                format!("skill: installed {b7} into {}", t.skill.display()),
                "pickup timer: none on this host, left alone".into(),
                format!("worker: deployed at {b7}"),
                "catch-up: done".into(),
            ]
        );
        assert!(BACKUP_NAME.is_full(backup.file_name().unwrap().to_str().unwrap()));
        assert_eq!(t.installed(), t.b);
        let read = |p: PathBuf| fs::read_to_string(p).unwrap();
        assert!(read(t.skill.join("SKILL.md")).contains("Commit B."));
        assert!(!read(backup.join("skill/factory/SKILL.md")).contains("Commit B."));
        let saved: Value =
            serde_json::from_str(&read(backup.join("skill/factory/bundle.json"))).unwrap();
        assert_eq!(saved["commit"], t.a.as_str());
        assert_eq!(
            read(backup.join("home/paths.md")),
            read(t.home.join("paths.md"))
        );
        assert_eq!(read(backup.join("crontab.txt")), "# synthetic crontab\n");
        let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(backup.clone()), 0o700);
        assert_eq!(mode(backup.join("home/.env")), 0o600);
        assert!(!backup.join("home/routes.md").exists());
        assert_eq!(t.worktrees(), 1);
    }

    #[test]
    fn leaves_detached_dirty_checkout() {
        let t = T::new("leaves");
        let state = || {
            (
                sh_git(&t.checkout, &["rev-parse", "HEAD"]),
                sh_git(&t.checkout, &["status", "--porcelain"]),
                sh_git(&t.checkout, &["branch", "--show-current"]),
                sh_git(&t.checkout, &["worktree", "list", "--porcelain"]),
            )
        };
        sh_git(&t.checkout, &["checkout", "-q", "--detach"]);
        fs::write(t.checkout.join("tools/install.py"), "# local edit\n").unwrap();
        fs::write(t.checkout.join("untracked.txt"), "synthetic\n").unwrap();
        for why in ["detached HEAD", "dirty", "on branch topic"] {
            if why == "dirty" {
                sh_git(&t.checkout, &["checkout", "-q", "main"]);
            } else if why == "on branch topic" {
                sh_git(&t.checkout, &["checkout", "-q", "--", "."]);
                fs::remove_file(t.checkout.join("untracked.txt")).unwrap();
                sh_git(&t.checkout, &["checkout", "-q", "-b", "topic"]);
            }
            let before = state();
            let (code, out, err) = t.call();
            assert_eq!((code, err.as_str()), (0, ""), "{why}");
            assert!(
                out.lines()
                    .any(|l| l == format!("checkout: left as is ({why})")),
                "{why}: {out}"
            );
            assert_eq!(state(), before, "{why}");
            assert_eq!(before.0, t.a, "{why}");
        }
        assert_eq!(t.installed(), t.b);
    }

    #[test]
    fn fast_forwards_clean_checkout() {
        let t = T::new("fast-forwards");
        assert_eq!(sh_git(&t.checkout, &["rev-parse", "HEAD"]), t.a);
        let (code, lines) = t.lines();
        assert_eq!(code, 0);
        assert!(lines.contains(&format!("checkout: fast-forwarded to {}", &t.b[..7])));
        assert_eq!(sh_git(&t.checkout, &["rev-parse", "HEAD"]), t.b);
        assert_eq!(sh_git(&t.checkout, &["status", "--porcelain"]), "");
        assert!(t
            .lines()
            .1
            .contains(&format!("checkout: already at {}", &t.b[..7])));
    }

    #[test]
    fn second_run_nothing_new() {
        let t = T::new("second-run");
        assert_eq!(t.call().0, 0);
        let backups = t.backups();
        let (code, out, err) = t.call();
        assert_eq!((code, err.as_str()), (0, ""));
        let b7 = &t.b[..7];
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                format!(
                    "catch-up: factory checkout {}, origin/main at {b7}",
                    t.checkout.display()
                ),
                format!("checkout: already at {b7}"),
                format!("skill: current at {b7}"),
                "pickup timer: none on this host, left alone".into(),
                format!("worker: deployed at {b7}"),
                "catch-up: nothing new".into(),
            ]
        );
        assert_eq!(t.backups(), backups);
    }

    #[test]
    fn runs_origin_installer() {
        let t = T::new("origin-installer");
        let marker = t.root.join("marker");
        t.edit_installer(
            "    args = parser.parse_args(argv)\n",
            &format!(
                "    args = parser.parse_args(argv)\n    Path({}).write_text(__file__)\n",
                repr(text(&marker))
            ),
        );
        let c = t.commit("C");
        sh_git(&t.checkout, &["checkout", "-q", "--detach"]);
        let (code, out, _) = t.call();
        assert_eq!(code, 0, "{out}");
        let ran = PathBuf::from(fs::read_to_string(&marker).unwrap());
        assert!(ran.ends_with("src/tools/install.py"), "{}", ran.display());
        let tmp = ran.parent().unwrap().parent().unwrap().parent().unwrap();
        assert!(tmp
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("factory-catch-up-"));
        assert_eq!(tmp.parent().unwrap(), temp_root().unwrap());
        assert!(!tmp.exists());
        assert_eq!(t.installed(), c);
        assert_eq!(t.worktrees(), 1);
    }

    #[test]
    fn timer_uses_bundle_entry() {
        let t = T::new("bundle-entry");
        t.timer(true);
        let (reply, reply_code) = default_timer_reply();
        let calls = t.timer_calls(reply, reply_code);
        let home = t.home.to_string_lossy().into_owned();
        let tail: Vec<String> = ["--home", &home, "pickup", "setup", "--install"]
            .map(String::from)
            .to_vec();
        let (code, lines) = t.lines();
        assert_eq!(code, 0);
        assert_eq!(
            lines[lines.len() - 4..lines.len() - 2],
            [
                "pickup timer: installed cron timer: crontab",
                "pickup timer: some pickup checks are missing; run factory.py pickup setup to see them",
            ]
        );
        let runtime = |skill: &Path| {
            let mut argv = python();
            argv.push(text(&skill.join("runtime/factory.py")).to_string());
            argv
        };
        assert_eq!(
            *calls.borrow(),
            [[runtime(&t.skill), tail.clone()].concat()]
        );

        t.drift(None);
        let (code, lines) = t.lines();
        assert_eq!(
            (code, &lines[lines.len() - 3..]),
            (
                0,
                &[
                    "pickup timer: current".to_string(),
                    format!("worker: deployed at {}", &t.b[..7]),
                    "catch-up: nothing new".into(),
                ][..]
            )
        );
        t.drift(Some("the timer runs an old runtime"));
        let (code, lines) = t.lines();
        assert_eq!((code, calls.borrow().len(), t.backups().len()), (0, 2, 2));
        assert_eq!(lines.last().unwrap(), "catch-up: done");

        t.edit_installer(
            r#"json.dumps({"commit": commit, "tree": tree.hexdigest()}"#,
            r#"json.dumps({"commit": commit, "tree": tree.hexdigest(), "entry": ["bin/factory", "-q"]}"#,
        );
        t.commit("C");
        assert_eq!(t.call().0, 0);
        assert_eq!(
            calls.borrow().last().unwrap(),
            &[
                vec![text(&t.skill.join("bin/factory")).to_string(), "-q".into()],
                tail
            ]
            .concat()
        );

        let folder = t.root.join("entries");
        fs::create_dir(&folder).unwrap();
        let fallback = runtime(&folder);
        assert_eq!(catch_up_entry(&folder), fallback);
        for entry in [
            r#"["/usr/bin/factory"]"#,
            r#"["../factory"]"#,
            r#"["bin/../../factory"]"#,
            "[]",
            r#"[""]"#,
            "[1]",
            r#""bin/factory""#,
            "null",
        ] {
            fs::write(
                folder.join("bundle.json"),
                format!(r#"{{"commit": "{}", "entry": {entry}}}"#, t.b),
            )
            .unwrap();
            assert_eq!(catch_up_entry(&folder), fallback, "{entry}");
        }
    }

    #[test]
    fn rust_bundle_keeps_its_stamp_and_entry() {
        let skill = temp_dir("catch-up-rust-bundle");
        let (commit, tree) = ("a".repeat(40), "b".repeat(64));
        let bundle = json!({"commit": commit, "tree": tree, "entry": ["runtime/factory"],
                            "binary_sha256": "c".repeat(64)});
        fs::write(skill.join("bundle.json"), bundle.to_string()).unwrap();
        assert_eq!(
            bundle_stamp(&skill),
            Some(json!({"commit": commit, "tree": tree}))
        );
        assert_eq!(
            catch_up_entry(&skill),
            vec![skill.join("runtime/factory").to_string_lossy().into_owned()]
        );
        let unknown = json!({"commit": commit, "tree": tree, "extra": "c"});
        fs::write(skill.join("bundle.json"), unknown.to_string()).unwrap();
        assert_eq!(bundle_stamp(&skill), None);
        fs::remove_dir_all(skill).unwrap();
    }

    #[test]
    fn timer_skipped_without_timer() {
        let t = T::new("no-timer");
        let (reply, reply_code) = default_timer_reply();
        let calls = t.timer_calls(reply, reply_code);
        let (code, lines) = t.lines();
        assert_eq!(code, 0);
        assert!(lines.contains(&"pickup timer: none on this host, left alone".to_string()));
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn failed_timer_and_fast_forward() {
        let t = T::new("failed-timer");
        t.timer(true);
        let calls = t.timer_calls("could not install the cron timer: synthetic", 1);
        fs::write(t.checkout.join("local.txt"), "synthetic\n").unwrap();
        sh_git(&t.checkout, &["add", "local.txt"]);
        sh_git(&t.checkout, &["commit", "-q", "-m", "local"]);
        let local = sh_git(&t.checkout, &["rev-parse", "HEAD"]);
        let (code, lines) = t.lines();
        assert_eq!(code, 1);
        assert!(lines[1].starts_with("checkout: fatal: "), "{}", lines[1]);
        assert!(lines.contains(&"pickup timer: could not install the cron timer: synthetic".into()));
        assert_eq!(lines.last().unwrap(), "catch-up: 2 step(s) failed");
        assert_eq!(calls.borrow().len(), 1);
        assert_eq!(sh_git(&t.checkout, &["rev-parse", "HEAD"]), local);
        assert_eq!(t.installed(), t.b);

        // Newer git prints hint lines before fatal; older git prints only the fatal line.
        let hints = "hint: Diverging branches can't be fast-forwarded, you need to either:\nhint:\nhint:   git merge --no-ff\n";
        let timer_run = FAKES.with(|f| f.borrow().run.clone()).unwrap();
        t.drift(None);
        for (stderr, line) in [
            (
                format!("{hints}fatal: Not possible to fast-forward, aborting.\n"),
                "checkout: fatal: Not possible to fast-forward, aborting.",
            ),
            (
                "\nerror: synthetic merge error\n".to_string(),
                "checkout: error: synthetic merge error",
            ),
        ] {
            let timer_run = timer_run.clone();
            FAKES.with(|f| {
                f.borrow_mut().run = Some(Rc::new(move |argv, timeout| {
                    if argv.iter().any(|a| a == "merge") {
                        return Some(Ok(Out {
                            code: 128,
                            stdout: String::new(),
                            stderr: stderr.clone(),
                        }));
                    }
                    timer_run(argv, timeout)
                }))
            });
            let (code, lines) = t.lines();
            assert_eq!((code, lines[1].as_str()), (1, line));
        }
    }

    #[test]
    fn worker_states() {
        let t = T::new("worker");
        assert_eq!(t.call().0, 0);
        let b7 = &t.b[..7];
        assert_eq!(
            t.gh_calls.borrow().last().unwrap(),
            &[
                "example/factory",
                "run",
                "list",
                "--workflow",
                "deploy-main.yml",
                "--commit",
                &t.b,
                "--json",
                "databaseId,status,conclusion,attempt,url",
                "--limit",
                "1"
            ]
        );
        let failed1 = "catch-up: 1 step(s) failed";
        let nothing = "catch-up: nothing new";
        let reran = format!("worker: reran deploy run {RUN_URL}");
        let cases: Vec<(Vec<(i32, String)>, String, i32, &str)> = vec![
            (
                vec![deployed()],
                format!("worker: deployed at {b7}"),
                0,
                nothing,
            ),
            (
                vec![runs("queued", "", 1)],
                format!("worker: deploy run {RUN_URL} is queued"),
                0,
                nothing,
            ),
            (
                vec![runs("in_progress", "", 1)],
                format!("worker: deploy run {RUN_URL} is in_progress"),
                0,
                nothing,
            ),
            (
                vec![(0, "[]".into())],
                format!("worker: no deploy run for {b7} yet; run catch-up again later"),
                0,
                nothing,
            ),
            (
                vec![runs("completed", "failure", 1), (0, String::new())],
                reran.clone(),
                0,
                "catch-up: done",
            ),
            (
                vec![runs("completed", "cancelled", 1), (0, String::new())],
                reran,
                0,
                "catch-up: done",
            ),
            (
                vec![runs("completed", "failure", 2)],
                format!(
                    "worker: deploy run {RUN_URL} ended failure after 2 attempts; check it by hand"
                ),
                1,
                failed1,
            ),
            (
                vec![(127, "No such file or directory: 'gh'".into())],
                "worker: skipped, gh did not run: No such file or directory: 'gh'".into(),
                0,
                nothing,
            ),
            (
                vec![(1, "synthetic gh error".into())],
                "worker: synthetic gh error".into(),
                1,
                failed1,
            ),
            (
                vec![(0, "not json".into())],
                "worker: not json".into(),
                1,
                failed1,
            ),
            (
                vec![
                    runs("completed", "failure", 1),
                    (1, "synthetic rerun error".into()),
                ],
                "worker: synthetic rerun error".into(),
                1,
                failed1,
            ),
        ];
        for (replies, line, exit_code, last) in cases {
            t.gh_calls.borrow_mut().clear();
            let count = replies.len();
            *t.gh_replies.borrow_mut() = replies.into();
            let (code, lines) = t.lines();
            assert_eq!(
                (code, &lines[lines.len() - 2..]),
                (exit_code, &[line.clone(), last.to_string()][..]),
                "{line}"
            );
            assert_eq!(t.gh_calls.borrow().len(), count, "{line}");
            if count == 2 {
                assert_eq!(
                    t.gh_calls.borrow().last().unwrap(),
                    &["example/factory", "run", "rerun", "7"]
                );
            }
        }

        t.gh_calls.borrow_mut().clear();
        sh_git(
            &t.checkout,
            &["remote", "set-url", "origin", text(&t.origin)],
        );
        let lines = t.lines().1;
        assert_eq!(
            lines[lines.len() - 2],
            "worker: skipped, origin is not on GitHub"
        );
        sh_git(&t.checkout, &["remote", "set-url", "origin", GITHUB_URL]);
        fs::remove_dir_all(t.src.join(".github")).unwrap();
        t.commit("C");
        let lines = t.lines().1;
        assert_eq!(
            lines[lines.len() - 2],
            "worker: skipped, origin/main has no .github/workflows/deploy-main.yml"
        );
        assert!(t.gh_calls.borrow().is_empty());
    }

    #[test]
    fn failed_install_keeps_backup() {
        let t = T::new("failed-install");
        t.timer(true);
        let (reply, reply_code) = default_timer_reply();
        let calls = t.timer_calls(reply, reply_code);
        fs::write(
            t.src.join("tools/install.py"),
            "import sys\nsys.exit('install.py: synthetic failure')\n",
        )
        .unwrap();
        t.commit("C");
        let (code, out, err) = t.call();
        assert_eq!((code, err.as_str()), (1, ""));
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines.contains(&"skill: install.py: synthetic failure"),
            "{out}"
        );
        assert_eq!(lines.last().unwrap(), &"catch-up: 1 step(s) failed");
        assert!(lines.contains(&"pickup timer: skipped, skill install failed"));
        assert!(calls.borrow().is_empty());
        let backups = t.backups();
        assert_eq!(backups.len(), 1);
        assert!(backups[0].join("skill/factory/SKILL.md").is_file());
        assert_eq!(t.installed(), t.a);
        assert_eq!(t.worktrees(), 1);
    }

    #[test]
    fn failed_backup_skips_install_and_timer() {
        let t = T::new("failed-backup");
        t.timer(true);
        let (reply, reply_code) = default_timer_reply();
        let calls = t.timer_calls(reply, reply_code);
        FAKES.with(|f| f.borrow_mut().backup = Some(Rc::new(|| Err("synthetic disk full".into()))));
        let (code, out, err) = t.call();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!((code, err.as_str()), (1, ""));
        assert!(lines.contains(&"backup: synthetic disk full"));
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("skill: installed") || l.starts_with("pickup timer:")),
            "{out}"
        );
        assert_eq!(lines.last().unwrap(), &"catch-up: 1 step(s) failed");
        assert!(calls.borrow().is_empty());
        assert_eq!(t.installed(), t.a);
    }
}
