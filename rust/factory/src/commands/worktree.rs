//! The worktree and cleanup commands, plus the checkout, prebuild and stop helpers pickup and ops share.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cli::{not_ported, Call};
use crate::shared::conf::{fm_get, frontmatter};
use crate::shared::exit::{fail, Res};
use crate::shared::fs::resolve;
use crate::shared::proc::{capture, git, git_ok, origin_git, which};
use crate::shared::project::{project_files, r2_target, read_paths};
use crate::shared::py::{is_space, os_error, strip};
use crate::shared::pyjson::loads;
use crate::shared::pyre::{Flags, Pattern};

static REPO_PATH: Pattern = Pattern::new(r"[A-Za-z0-9_./-]{1,200}", Flags::ASCII);
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
        Some(FETCH_TIMEOUT),
    ) {
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            let secs = FETCH_TIMEOUT.as_secs();
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
    let paths = read_paths(home);
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
            "factory.py: cannot read {}: use {{\"version\": 1, \"prebuild\": {{\"<project>\": [\"<dir>\", ...]}}}}",
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
            (Ok(root), Ok(inner)) => inner != root && inner.starts_with(&root),
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
                    "factory.py: prebuild of {d} failed (exit {code}); log: {}",
                    log.display()
                ),
            ),
            Err(why) => say(
                &mut lines,
                format!(
                    "factory.py: prebuild of {d} failed ({why}); log: {}",
                    log.display()
                ),
            ),
        }
    }
    Ok(lines)
}

/// Install each listed folder from its frozen lockfile, only when its dependency files match origin/<base>.
pub(crate) fn prebuild(home: &Path, slug: &str, job_id: &str, wt: &Path) -> Res<()> {
    prebuild_with(home, slug, job_id, wt, which("mise"), which("pnpm")).map(drop)
}

pub fn cleanup(_call: &Call) -> Res<()> {
    Err(not_ported("cleanup"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;

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
                "factory.py: unsafe checkout: factory base -x is not a safe branch name\n"
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
            (
                4,
                format!("factory.py: no factory/{JOB} branch here or on origin\n")
            )
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
            (6, format!("factory.py: {} is 0 ahead and 1 {tail} with uncommitted changes. Nothing was changed\n", wt.display()))
        );
        sh_git(&wt, &["add", "untracked.txt"]);
        commit(&wt, "local work");
        let e = worktree_in(&t.home, "factory", JOB).unwrap_err();
        assert_eq!(
            (e.code, e.stderr),
            (
                6,
                format!(
                    "factory.py: {} is 1 ahead and 1 {tail}. Nothing was changed\n",
                    wt.display()
                )
            )
        );
        assert_ne!(sh_git(&wt, &["rev-parse", "HEAD"]), before);
    }

    const FAKE_PNPM: &str = "#!/bin/sh\necho \"$* $(pwd)\" >> \"{record}\"\nmkdir -p node_modules && echo done > node_modules/.modules.yaml\n";

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
            fs::write(&pnpm, FAKE_PNPM.replace("{record}", &text(&record))).unwrap();
            chmod(&pnpm, 0o755);
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
                "factory.py: prebuild of sample failed ({why}); log: {}",
                self.log.display()
            )
        }
    }

    impl Drop for Prebuild {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn chmod(p: &Path, mode: u32) {
        #[cfg(unix)]
        fs::set_permissions(p, std::os::unix::fs::PermissionsExt::from_mode(mode)).unwrap();
        #[cfg(not(unix))]
        let _ = (p, mode);
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
        fs::write(&failing, "#!/bin/sh\nexit 1\n").unwrap();
        chmod(&failing, 0o755);
        assert_eq!(t.run(None, Some(&failing)), [t.failed("exit 1")]);
        chmod(&t.pnpm, 0o644);
        let out = t.run(None, Some(&t.pnpm));
        assert_eq!(out.len(), 1);
        assert!(
            out[0]
                .starts_with("factory.py: prebuild of sample failed ([Errno 13] Permission denied"),
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
        fs::write(
            &driver,
            format!(
                "#!/bin/sh\necho ran >> '{}'\necho FORGED\n",
                marker.display()
            ),
        )
        .unwrap();
        chmod(&driver, 0o755);
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
        let argv = ["sh", "-c", "echo started; sleep 5"].map(String::from);
        assert_eq!(
            run_logged(&argv, &dir, &log, Duration::from_secs(1)),
            Err("timed out after 1 s".to_string())
        );
        assert_eq!(fs::read_to_string(&log).unwrap(), "started\n");
        let argv = ["sh", "-c", "echo out; echo err >&2; exit 3"].map(String::from);
        assert_eq!(run_logged(&argv, &dir, &log, Duration::from_secs(5)), Ok(3));
        assert_eq!(fs::read_to_string(&log).unwrap(), "started\nout\nerr\n");
        fs::remove_dir_all(dir).unwrap();
    }
}
