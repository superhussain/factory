//! `install --source <checkout> --destination <skills root> [--replace]`: copy the checkout's
//! factory skill, with the Rust binary built from that checkout, into the skills root. A port of
//! `tools/install.py` that keeps its checks, messages and atomic swap.

#[cfg(test)]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
#[cfg(test)]
use std::rc::Rc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::cli::Call;
use crate::commands::catch_up::random_hex;
use crate::shared::build::{rust_binary, Host, BINARY};
use crate::shared::exit::{Exit, Res};
use crate::shared::fs::{is_link, readable, resolve};
use crate::shared::proc::capture;
use crate::shared::project::sha;
use crate::shared::pyjson::{dumps, SORTED};
use crate::shared::pyre::{Flags, Pattern};

const USAGE: &str = "factory install: usage: factory install --source <checkout> --destination <skills root> [--replace]\n";
const LEGACY: &str = "huss-factory";
const FILES: [&str; 20] = [
    "SKILL.md",
    "runtime/factory.py",
    "runtime/paseo_stalls.py",
    "runtime/pr_screenshots.py",
    "templates/config.md",
    "templates/project.md",
    "references/execution.md",
    "references/integrations.md",
    "references/pickup.md",
    "references/visuals.md",
    "references/chat-decisions.md",
    "references/project-settings.md",
    "references/dashboard.md",
    "references/board.md",
    "references/needs.md",
    "references/schedules.md",
    "references/walkthrough.html",
    "schedules/explore.md",
    "schedules/resume.md",
    "schedules/retro.md",
];
const COMMANDS: [&str; 15] = [
    "feature",
    "next",
    "idea",
    "deny-notes",
    "babysit",
    "approve",
    "approve-spec",
    "rework",
    "recover",
    "build",
    "init",
    "catch-up",
    "handoff",
    "resume",
    "retro",
];
const AGENTS: [&str; 9] = [
    "approver",
    "builder",
    "explorer",
    "review-code",
    "review-security",
    "review-ui",
    "review-ux",
    "spec-writer",
    "retro",
];
static COMMIT: Pattern = Pattern::new("[0-9a-f]{40}", Flags::NONE);

/// Every file a source skill holds, and nothing else.
pub(crate) fn required() -> BTreeSet<String> {
    FILES
        .iter()
        .map(|f| f.to_string())
        .chain(
            COMMANDS
                .iter()
                .map(|c| format!("references/commands/{c}.md")),
        )
        .chain(AGENTS.iter().map(|a| format!("agents/factory-{a}.md")))
        .collect()
}

fn required_dirs() -> BTreeSet<String> {
    required()
        .iter()
        .filter_map(|f| f.rsplit_once('/').map(|(d, _)| d.to_string()))
        .collect()
}

// ---- test seam: named points where a test makes the next step fail or changes the tree

#[cfg(test)]
thread_local! {
    static HOOK: RefCell<Option<Rc<dyn Fn(&str) -> io::Result<()>>>> = const { RefCell::new(None) };
}

fn hook(_point: &str) -> io::Result<()> {
    #[cfg(test)]
    if let Some(f) = HOOK.with(|h| h.borrow().clone()) {
        return f(_point);
    }
    Ok(())
}

/// Why an install stopped: a message of its own, or the generic refusal naming the current path.
enum Why {
    Fail(String),
    Unsafe,
    Exit(Exit),
}

impl From<io::Error> for Why {
    fn from(_: io::Error) -> Why {
        Why::Unsafe
    }
}

/// (dev, ino, size, mtime ns); Windows has no stable file index, so its creation time stands in.
type Ident = (u64, u64, u64, u128);

fn ident(meta: &fs::Metadata) -> Ident {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino(), meta.size(), mtime)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        (meta.creation_time(), 0, meta.file_size(), mtime)
    }
}

/// `identity`: (dev, ino, mtime) of path itself.
fn identity(path: &Path) -> io::Result<Ident> {
    let (dev, ino, _, mtime) = ident(&fs::symlink_metadata(path)?);
    Ok((dev, ino, 0, mtime))
}

fn lexists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn linked(path: &Path) -> io::Result<bool> {
    is_link(path)
}

fn safe_parents(path: &Path) -> Result<(), Why> {
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err(Why::Unsafe);
    }
    for parent in path.ancestors() {
        if lexists(parent) && (linked(parent)? || !parent.is_dir()) {
            return Err(Why::Unsafe);
        }
    }
    Ok(())
}

#[derive(PartialEq)]
struct Tree {
    files: BTreeMap<String, Vec<u8>>,
    ids: BTreeMap<String, Ident>,
}

/// Every file under root and the identity of every entry. A source tree skips dot entries and
/// Python caches, and must hold exactly the required files.
fn tree(root: &Path, source: bool) -> Result<Tree, Why> {
    if linked(root)? || !root.is_dir() {
        return Err(Why::Unsafe);
    }
    let mut out = Tree {
        files: BTreeMap::new(),
        ids: BTreeMap::new(),
    };
    let mut directories = BTreeSet::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((folder, prefix)) = stack.pop() {
        for entry in fs::read_dir(&folder)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let info = fs::symlink_metadata(&path)?;
            if linked(&path)? || !readable(&path) {
                return Err(Why::Unsafe);
            }
            let rel = format!("{prefix}{name}");
            out.ids.insert(rel.clone(), ident(&info));
            if info.is_dir() {
                directories.insert(rel.clone());
                if !(source && (name.starts_with('.') || name == "__pycache__")) {
                    stack.push((path, format!("{rel}/")));
                }
                continue;
            }
            if !info.is_file() {
                return Err(Why::Unsafe);
            }
            if source && (name.starts_with('.') || name.ends_with(".pyc") || name.ends_with(".pyo"))
            {
                continue;
            }
            out.files.insert(rel, fs::read(&path)?);
        }
    }
    if source {
        let dirs: BTreeSet<String> = directories
            .into_iter()
            .filter(|d| {
                !d.split('/')
                    .any(|p| p.starts_with('.') || p == "__pycache__")
            })
            .collect();
        if out.files.keys().cloned().collect::<BTreeSet<_>>() != required()
            || dirs != required_dirs()
        {
            return Err(Why::Unsafe);
        }
    }
    Ok(out)
}

/// A sha256 over every file's path and content hash, in path order.
fn tree_hash(content: &BTreeMap<String, Vec<u8>>) -> String {
    let mut h = Sha256::new();
    for (rel, data) in content {
        h.update(rel.as_bytes());
        h.update(b"\0");
        h.update(Sha256::digest(data));
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// {commit, tree} naming the source's Git commit and a hash of the copied files, or None when the
/// source is not in a Git checkout.
fn stamp(
    source: &Path,
    content: &BTreeMap<String, Vec<u8>>,
) -> Option<serde_json::Map<String, Value>> {
    let source = source.to_string_lossy();
    let head = capture(
        &["git", "-C", &source, "rev-parse", "--verify", "HEAD"],
        None,
        &[],
        None,
    )
    .ok()?;
    let commit = head.stdout.trim();
    if head.code != 0 || !COMMIT.is_full(commit) {
        return None;
    }
    let mut map = serde_json::Map::new();
    map.insert("commit".into(), json!(commit));
    map.insert("tree".into(), json!(tree_hash(content)));
    Some(map)
}

fn drop_legacy(destination: &Path, out: &mut dyn Write, err: &mut dyn Write) -> Result<(), Why> {
    let path = destination.join(LEGACY);
    if !lexists(&path) {
        return Ok(());
    }
    if !linked(&path)? {
        let _ = writeln!(
            err,
            "install.py: {} is a folder, not a link; remove it by hand so agents load only factory",
            path.display()
        );
        return Ok(());
    }
    hook("legacy")?;
    if fs::remove_file(&path).is_err() {
        fs::remove_dir(&path)?;
    }
    let _ = writeln!(out, "removed {}", path.display());
    Ok(())
}

/// The bytes of the binary built from a checkout, or the message `install` exits with.
pub(crate) type Build<'a> = &'a dyn Fn(&Path, &mut dyn Write) -> Result<Vec<u8>, String>;

/// Paths an install attempt leaves behind until it ends.
struct Attempt {
    current: PathBuf,
    stage: Option<PathBuf>,
    lock: Option<(PathBuf, Ident)>,
}

/// Install checkout's skills/factory, with build's binary, as destination/factory. Prints the
/// target, and each backup or legacy link it removes, to out.
pub(crate) fn install_skill(
    checkout: &Path,
    destination: &Path,
    replace: bool,
    build: Build,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Res<PathBuf> {
    let source = checkout.join("skills").join("factory");
    let mut attempt = Attempt {
        current: source.clone(),
        stage: None,
        lock: None,
    };
    let result = attempt_install(&mut attempt, &source, destination, replace, build, out, err);
    let mut stderr = match &result {
        Ok(_) => String::new(),
        Err(Why::Fail(m)) => format!("install.py: {m}\n"),
        Err(Why::Unsafe) => format!(
            "install.py: {}: cannot safely install factory\n",
            attempt.current.display()
        ),
        Err(Why::Exit(e)) => e.stderr.clone(),
    };
    let cleaned = (|| -> io::Result<()> {
        if let Some(stage) = &attempt.stage {
            if lexists(stage) {
                fs::remove_dir_all(stage)?;
            }
        }
        if let Some((lock, id)) = &attempt.lock {
            if lexists(lock) && identity_of_lock(lock)? == *id {
                fs::remove_file(lock)?;
            }
        }
        Ok(())
    })();
    if cleaned.is_err() {
        stderr.push_str(&format!(
            "install.py: {}: cannot clean this installation attempt\n",
            destination.display()
        ));
    }
    match result {
        Ok(target) if cleaned.is_ok() => Ok(target),
        _ => Err(Exit::raw(2, stderr)),
    }
}

fn identity_of_lock(lock: &Path) -> io::Result<Ident> {
    let (dev, ino, _, _) = ident(&fs::symlink_metadata(lock)?);
    Ok((dev, ino, 0, 0))
}

fn attempt_install(
    attempt: &mut Attempt,
    source: &Path,
    destination: &Path,
    replace: bool,
    build: Build,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<PathBuf, Why> {
    safe_parents(source)?;
    safe_parents(destination)?;
    let (source, destination) = (resolve(source)?, resolve(destination)?);
    if source == destination || destination.starts_with(&source) || source.starts_with(&destination)
    {
        return Err(Why::Unsafe);
    }
    let mut content = tree(&source, true)?.files;
    let checkout = source.parent().and_then(Path::parent).unwrap_or(&source);
    let binary =
        build(checkout, err).map_err(|e| Why::Exit(Exit::raw(2, format!("install: {e}\n"))))?;
    let binary_sha256 = sha(&binary);
    content.insert(BINARY.to_string(), binary);
    if let Some(mut bundle) = stamp(&source, &content) {
        // catch_up_entry runs a skill-relative entry.
        bundle.insert("entry".into(), json!([BINARY]));
        bundle.insert("binary_sha256".into(), json!(binary_sha256));
        let text = dumps(&Value::Object(bundle), SORTED) + "\n";
        content.insert("bundle.json".into(), text.into_bytes());
    }
    let target = destination.join("factory");
    attempt.current = target.clone();
    if lexists(&target) && (linked(&target)? || !target.is_dir()) {
        return Err(Why::Unsafe);
    }
    fs::create_dir_all(&destination)?;
    let lock = destination.join(".factory-install.lock");
    attempt.current = lock.clone();
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(&lock)?;
    let (dev, ino, _, _) = ident(&file.metadata()?);
    attempt.lock = Some((lock, (dev, ino, 0, 0)));
    drop(file);
    attempt.current = target.clone();
    let prior_identity = if lexists(&target) {
        Some(identity(&target)?)
    } else {
        None
    };
    let prior = match prior_identity {
        Some(_) => Some(tree(&target, false)?),
        None => None,
    };
    hook("prior")?;
    if let Some(prior) = &prior {
        let mut expected: BTreeSet<String> = content.keys().cloned().collect();
        expected.extend(required_dirs());
        if prior.files == content && prior.ids.keys().cloned().collect::<BTreeSet<_>>() == expected
        {
            let _ = writeln!(out, "{}", target.display());
            attempt.current = destination.join(LEGACY);
            drop_legacy(&destination, out, err)?;
            return Ok(target);
        }
        if !replace {
            return Err(Why::Fail(format!(
                "{}: existing install differs; use --replace explicitly",
                target.display()
            )));
        }
    }
    hook("stage")?;
    let stage = destination.join(format!(".factory-stage-{}", random_hex(4)));
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&stage)?;
    attempt.stage = Some(stage.clone());
    for (name, data) in &content {
        let path = stage.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, data)?;
        #[cfg(unix)]
        if name == BINARY {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }
    }
    let mut backup = None;
    if let Some(prior_identity) = prior_identity {
        if !lexists(&target)
            || identity(&target)? != prior_identity
            || prior.as_ref() != Some(&tree(&target, false)?)
        {
            return Err(Why::Fail(format!(
                "{}: destination changed during installation",
                target.display()
            )));
        }
        let path = destination.join(format!("factory.backup-{}", random_hex(16)));
        if lexists(&path) {
            return Err(Why::Fail(format!(
                "{}: backup already exists",
                path.display()
            )));
        }
        fs::rename(&target, &path)?;
        backup = Some(path);
        hook("backed-up")?;
    }
    if lexists(&target) {
        return Err(Why::Fail(format!(
            "{}: competing destination appeared; retained prior backup",
            target.display()
        )));
    }
    let switched = hook("switch").and_then(|_| fs::rename(&stage, &target));
    if let Err(e) = switched {
        if let Some(backup) = &backup {
            if !lexists(&target) {
                fs::rename(backup, &target)?;
            }
        }
        return Err(e.into());
    }
    attempt.stage = None;
    let _ = writeln!(out, "{}", target.display());
    if let Some(backup) = backup {
        match hook("rmtree-backup").and_then(|_| fs::remove_dir_all(&backup)) {
            Ok(()) => {
                let _ = writeln!(out, "removed {}", backup.display());
            }
            Err(_) => {
                let _ = writeln!(
                    err,
                    "install.py: could not remove {}; remove it by hand",
                    backup.display()
                );
            }
        }
    }
    attempt.current = destination.join(LEGACY);
    drop_legacy(&destination, out, err)?;
    Ok(target)
}

/// (source checkout, skills root, replace) from the operands, or None when they do not fit.
fn parse(args: &[String]) -> Option<(PathBuf, PathBuf, bool)> {
    let (mut source, mut destination, mut replace) = (None, None, false);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--source" if source.is_none() => source = Some(PathBuf::from(rest.next()?)),
            "--destination" if destination.is_none() => {
                destination = Some(PathBuf::from(rest.next()?))
            }
            "--replace" if !replace => replace = true,
            _ => return None,
        }
    }
    Some((source?, destination?, replace))
}

pub fn install(call: &Call) -> Res<()> {
    let (checkout, destination, replace) = parse(&call.args).ok_or_else(|| Exit::raw(2, USAGE))?;
    let host = Host::current().map_err(|e| Exit::raw(2, format!("install: {e}\n")))?;
    let build = |checkout: &Path, err: &mut dyn Write| {
        let path = rust_binary(checkout, &host, err)?;
        fs::read(&path).map_err(|e| crate::shared::py::os_error(&e, Some(&path)))
    };
    install_skill(
        &checkout,
        &destination,
        replace,
        &build,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
    .map(|_| ())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::shared::build::tests::{checkout, fake_rustc, host, script};
    use crate::shared::fs::temp_dir;
    use std::os::unix::fs::PermissionsExt;

    /// A temp root holding a git checkout of the skill; installs go to root/skills.
    struct T {
        root: PathBuf,
        checkout: PathBuf,
        destination: PathBuf,
    }

    impl Drop for T {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = None);
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn stub(_: &Path, _: &mut dyn Write) -> Result<Vec<u8>, String> {
        Ok(b"stub".to_vec())
    }

    impl T {
        fn new(name: &str) -> T {
            let root = temp_dir(&format!("install-{name}"));
            let checkout = checkout(&root);
            T {
                destination: root.join("skills"),
                root,
                checkout,
            }
        }

        fn skill(&self) -> PathBuf {
            self.checkout.join("skills/factory")
        }

        fn target(&self) -> PathBuf {
            self.destination.join("factory")
        }

        /// (stdout lines, stderr) of a successful install.
        fn install(&self, replace: bool) -> (Vec<String>, String) {
            let (out, err) = self.attempt(&self.checkout, &self.destination, replace);
            (out.unwrap(), err)
        }

        /// The install's stdout lines, or its exit, and its stderr.
        fn attempt(
            &self,
            checkout: &Path,
            destination: &Path,
            replace: bool,
        ) -> (Result<Vec<String>, Exit>, String) {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let result = install_skill(checkout, destination, replace, &stub, &mut out, &mut err);
            let lines = String::from_utf8(out)
                .unwrap()
                .lines()
                .map(String::from)
                .collect();
            (result.map(|_| lines), String::from_utf8(err).unwrap())
        }

        fn refused(&self, checkout: &Path, destination: &Path, replace: bool) -> String {
            let exit = self.attempt(checkout, destination, replace).0.unwrap_err();
            assert_eq!(exit.code, 2);
            exit.stderr
        }

        fn hook(&self, f: impl Fn(&str) -> io::Result<()> + 'static) {
            HOOK.with(|h| *h.borrow_mut() = Some(Rc::new(f)));
        }

        fn backups(&self) -> Vec<PathBuf> {
            let mut found: Vec<PathBuf> = fs::read_dir(&self.destination)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("factory.backup-")
                })
                .collect();
            found.sort();
            found
        }
    }

    /// Every path under root with its bytes, or None for a folder.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut found = BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(folder) = stack.pop() {
            for entry in fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() && !path.is_symlink() {
                    stack.push(path.clone());
                    found.insert(path, None);
                } else {
                    found.insert(path.clone(), fs::read(&path).ok());
                }
            }
        }
        found
    }

    fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
        match tree(root, false) {
            Ok(t) => t.files,
            Err(_) => panic!("unreadable tree"),
        }
    }

    #[test]
    fn exact_copy_idempotence_and_unrelated_skills() {
        let t = T::new("exact");
        fs::create_dir_all(t.destination.join("other")).unwrap();
        fs::write(t.destination.join("other/SKILL.md"), "other skill").unwrap();
        let (lines, _) = t.install(false);
        assert_eq!(lines, [t.target().display().to_string()]);
        let mut installed = files(&t.target());
        let mut names: BTreeSet<String> = required();
        names.extend([BINARY.to_string(), "bundle.json".to_string()]);
        assert_eq!(installed.keys().cloned().collect::<BTreeSet<_>>(), names);
        assert_eq!(installed.remove(BINARY).unwrap(), b"stub");
        assert_eq!(
            fs::metadata(t.target().join(BINARY))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        installed.remove("bundle.json");
        match tree(&t.skill(), true) {
            Ok(source) => assert_eq!(installed, source.files),
            Err(_) => panic!("source refused"),
        }
        let before = snapshot(&t.destination);
        assert_eq!(t.install(false).0, [t.target().display().to_string()]);
        assert_eq!(snapshot(&t.destination), before);
        assert_eq!(
            fs::read_to_string(t.destination.join("other/SKILL.md")).unwrap(),
            "other skill"
        );
        assert!(!t.destination.join(".factory-install.lock").exists());
    }

    #[test]
    fn git_checkout_stamps_commit_tree_entry_and_binary_hash() {
        let t = T::new("stamp");
        t.install(false);
        let stamp = fs::read_to_string(t.target().join("bundle.json")).unwrap();
        let head = String::from_utf8(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&t.checkout)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let mut content = files(&t.target());
        content.remove("bundle.json");
        assert_eq!(
            stamp,
            format!(
                "{{\"binary_sha256\": \"{}\", \"commit\": \"{}\", \"entry\": [\"runtime/factory\"], \"tree\": \"{}\"}}\n",
                sha(b"stub"),
                head.trim(),
                tree_hash(&content)
            )
        );
        assert_eq!(
            crate::commands::setup::stamp_of(&serde_json::from_str(&stamp).unwrap()),
            Some(json!({"commit": head.trim(), "tree": tree_hash(&content)}))
        );
    }

    #[test]
    fn refusal_keeps_install_and_replacement_removes_its_backup() {
        let t = T::new("refusal");
        let older = t.destination.join("factory.backup-older");
        fs::create_dir_all(&older).unwrap();
        fs::write(older.join("keep"), "keep").unwrap();
        t.install(false);
        fs::write(t.target().join("SKILL.md"), "locally edited").unwrap();
        let old = snapshot(&t.target());
        assert_eq!(
            t.refused(&t.checkout, &t.destination, false),
            format!(
                "install.py: {}: existing install differs; use --replace explicitly\n",
                t.target().display()
            )
        );
        assert_eq!(snapshot(&t.target()), old);
        let (lines, _) = t.install(true);
        let backup = PathBuf::from(lines[1].strip_prefix("removed ").unwrap());
        assert_eq!(lines[0], t.target().display().to_string());
        assert!(backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("factory.backup-"));
        assert!(!lexists(&backup));
        fs::write(t.target().join("SKILL.md"), "second edit").unwrap();
        t.install(true);
        assert_eq!(t.backups(), [older.clone()]);
        assert_eq!(fs::read_to_string(older.join("keep")).unwrap(), "keep");
    }

    #[test]
    fn backup_removal_failure_keeps_install_and_warns() {
        let t = T::new("backup-removal");
        t.install(false);
        fs::write(t.target().join("SKILL.md"), "old contents").unwrap();
        t.hook(|point| match point {
            "rmtree-backup" => Err(io::Error::other("locked")),
            _ => Ok(()),
        });
        let (lines, err) = t.install(true);
        assert_eq!(lines, [t.target().display().to_string()]);
        let backups = t.backups();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            err,
            format!(
                "install.py: could not remove {}; remove it by hand\n",
                backups[0].display()
            )
        );
        assert_eq!(
            fs::read_to_string(backups[0].join("SKILL.md")).unwrap(),
            "old contents"
        );
        assert_ne!(
            fs::read_to_string(t.target().join("SKILL.md")).unwrap(),
            "old contents"
        );
        assert!(!t.destination.join(".factory-install.lock").exists());
    }

    #[test]
    fn exclusive_lock_is_never_broken() {
        let t = T::new("lock");
        fs::create_dir_all(&t.destination).unwrap();
        let lock = t.destination.join(".factory-install.lock");
        fs::write(&lock, "owned by another installer").unwrap();
        let before = snapshot(&t.destination);
        assert_eq!(
            t.refused(&t.checkout, &t.destination, false),
            format!(
                "install.py: {}: cannot safely install factory\n",
                lock.display()
            )
        );
        assert_eq!(snapshot(&t.destination), before);
    }

    #[test]
    fn switch_failure_restores_old_target() {
        let t = T::new("switch");
        t.install(false);
        fs::write(t.target().join("SKILL.md"), "old contents").unwrap();
        let old = snapshot(&t.target());
        t.hook(|point| match point {
            "switch" => Err(io::Error::other("switch interrupted")),
            _ => Ok(()),
        });
        t.refused(&t.checkout, &t.destination, true);
        assert_eq!(snapshot(&t.target()), old);
        assert!(!fs::read_dir(&t.destination).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".factory-stage-")));
        assert!(!t.destination.join(".factory-install.lock").exists());
    }

    #[test]
    fn competing_target_is_not_overwritten_and_backup_survives() {
        let t = T::new("competing");
        t.install(false);
        fs::write(t.target().join("SKILL.md"), "old contents").unwrap();
        let target = t.target();
        t.hook(move |point| {
            if point == "backed-up" {
                fs::create_dir(&target)?;
                fs::write(target.join("other"), "competing target")?;
            }
            Ok(())
        });
        assert!(t
            .refused(&t.checkout, &t.destination, true)
            .ends_with("competing destination appeared; retained prior backup\n"));
        assert_eq!(
            fs::read_to_string(t.target().join("other")).unwrap(),
            "competing target"
        );
        let backups = t.backups();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            fs::read_to_string(backups[0].join("SKILL.md")).unwrap(),
            "old contents"
        );
    }

    #[test]
    fn detected_destination_edit_and_staging_failure_leave_old_install() {
        let t = T::new("edit");
        t.install(false);
        fs::write(t.target().join("SKILL.md"), "old").unwrap();
        let skill_md = t.target().join("SKILL.md");
        t.hook(move |point| {
            if point == "prior" {
                fs::write(&skill_md, "new external edit")?;
            }
            Ok(())
        });
        assert!(t
            .refused(&t.checkout, &t.destination, true)
            .ends_with("destination changed during installation\n"));
        assert_eq!(
            fs::read_to_string(t.target().join("SKILL.md")).unwrap(),
            "new external edit"
        );
        assert!(t.backups().is_empty());
        t.hook(|point| match point {
            "stage" => Err(io::Error::other("no space")),
            _ => Ok(()),
        });
        t.refused(&t.checkout, &t.destination, true);
        assert_eq!(
            fs::read_to_string(t.target().join("SKILL.md")).unwrap(),
            "new external edit"
        );
    }

    #[test]
    fn unsafe_paths_inventory_and_source_cache_exclusion() {
        let t = T::new("unsafe");
        t.refused(&t.checkout, Path::new("relative"), false);
        t.refused(&t.checkout, &t.skill().join("inside"), false);
        let cached = t.skill().join("__pycache__");
        fs::create_dir(&cached).unwrap();
        fs::write(cached.join("factory.pyc"), "not shipped").unwrap();
        fs::write(t.skill().join(".development"), "not shipped").unwrap();
        t.install(false);
        assert!(!t.target().join("__pycache__").exists());
        assert!(!t.target().join(".development").exists());
        let extra = t.skill().join("agents/factory-ux.md");
        fs::write(&extra, "unexpected role").unwrap();
        t.refused(&t.checkout, &t.destination, true);
        fs::remove_file(&extra).unwrap();
        fs::remove_file(t.skill().join("SKILL.md")).unwrap();
        t.refused(&t.checkout, &t.destination, true);
        std::os::unix::fs::symlink(t.target().join("SKILL.md"), t.skill().join("SKILL.md"))
            .unwrap();
        t.refused(&t.checkout, &t.destination, true);
        let alias = t.root.join("alias");
        std::os::unix::fs::symlink(&t.destination, &alias).unwrap();
        t.refused(&t.checkout, &alias, true);
    }

    #[test]
    fn file_targets_and_parent_traversal_are_refused_without_writes() {
        let t = T::new("file-target");
        fs::create_dir_all(&t.destination).unwrap();
        fs::write(t.target(), "not a skill directory").unwrap();
        let before = snapshot(&t.destination);
        t.refused(&t.checkout, &t.destination, false);
        assert_eq!(snapshot(&t.destination), before);
        t.refused(&t.checkout, &t.root.join("part/../skills"), false);
    }

    #[test]
    fn legacy_link_removed_on_install_and_identical_rerun() {
        let t = T::new("legacy-link");
        let old = t.root.join("old/huss-factory");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("SKILL.md"), "retired runtime").unwrap();
        fs::create_dir_all(&t.destination).unwrap();
        let link = t.destination.join(LEGACY);
        for _ in 0..2 {
            std::os::unix::fs::symlink(&old, &link).unwrap();
            let (lines, _) = t.install(false);
            assert_eq!(
                lines,
                [
                    t.target().display().to_string(),
                    format!("removed {}", link.display())
                ]
            );
            assert!(!lexists(&link));
            assert_eq!(
                fs::read_to_string(old.join("SKILL.md")).unwrap(),
                "retired runtime"
            );
        }
    }

    #[test]
    fn legacy_folder_kept_and_removal_failure_exits_2() {
        let t = T::new("legacy-folder");
        let folder = t.destination.join(LEGACY);
        fs::create_dir_all(&folder).unwrap();
        let (lines, err) = t.install(false);
        assert_eq!(lines, [t.target().display().to_string()]);
        assert!(folder.is_dir());
        assert_eq!(
            err,
            format!(
                "install.py: {} is a folder, not a link; remove it by hand so agents load only factory\n",
                folder.display()
            )
        );
        fs::remove_dir(&folder).unwrap();
        std::os::unix::fs::symlink(t.root.join("old"), &folder).unwrap();
        t.hook(|point| match point {
            "legacy" => Err(io::Error::from(io::ErrorKind::PermissionDenied)),
            _ => Ok(()),
        });
        assert_eq!(
            t.refused(&t.checkout, &t.destination, false),
            format!(
                "install.py: {}: cannot safely install factory\n",
                folder.display()
            )
        );
        assert!(folder.is_symlink());
        assert!(!t.destination.join(".factory-install.lock").exists());
    }

    #[test]
    fn failed_build_leaves_the_install_untouched() {
        let t = T::new("no-cargo");
        t.install(false);
        let before = snapshot(&t.destination);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let missing = host(&t.root, &[], None);
        let build = |checkout: &Path, err: &mut dyn Write| {
            rust_binary(checkout, &missing, err).map(|_| Vec::new())
        };
        let exit = install_skill(
            &t.checkout,
            &t.destination,
            true,
            &build,
            &mut out,
            &mut err,
        )
        .unwrap_err();
        assert_eq!(
            (exit.code, exit.stderr.as_str()),
            (
                2,
                format!("install: {}\n", crate::shared::build::NO_CARGO).as_str()
            )
        );
        assert_eq!(snapshot(&t.destination), before);
    }

    #[test]
    fn built_binary_is_installed_with_its_entry() {
        let t = T::new("built");
        fake_rustc(&t.root.join("bin"), "1.100.0");
        let cargo = t.root.join("bin/cargo");
        script(
            &cargo,
            "mkdir -p \"$CARGO_TARGET_DIR/host\" && printf built > \"$CARGO_TARGET_DIR/host/factory\"\n",
        );
        let machine = host(&t.root, &[], Some(cargo));
        let build = |checkout: &Path, err: &mut dyn Write| {
            let path = rust_binary(checkout, &machine, err)?;
            Ok(fs::read(path).unwrap())
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        install_skill(
            &t.checkout,
            &t.destination,
            false,
            &build,
            &mut out,
            &mut err,
        )
        .unwrap();
        assert_eq!(err, b"install: Rust binary built in 0 s\n");
        assert_eq!(fs::read(t.target().join(BINARY)).unwrap(), b"built");
        let bundle: Value =
            serde_json::from_str(&fs::read_to_string(t.target().join("bundle.json")).unwrap())
                .unwrap();
        assert_eq!(bundle["entry"], json!(["runtime/factory"]));
        assert_eq!(bundle["binary_sha256"], json!(sha(b"built")));
        let mut err = Vec::new();
        install_skill(
            &t.checkout,
            &t.destination,
            false,
            &build,
            &mut Vec::new(),
            &mut err,
        )
        .unwrap();
        assert!(String::from_utf8(err)
            .unwrap()
            .starts_with("install: Rust binary from cache "));
    }

    #[test]
    fn operands_parse_in_any_order_once() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            parse(&args(&[
                "--destination",
                "/d",
                "--replace",
                "--source",
                "/s"
            ])),
            Some((PathBuf::from("/s"), PathBuf::from("/d"), true))
        );
        assert_eq!(
            parse(&args(&["--source", "/s", "--destination", "/d"])),
            Some((PathBuf::from("/s"), PathBuf::from("/d"), false))
        );
        for bad in [
            &["--source", "/s", "--destination"][..],
            &["--source", "/s", "--source", "/d"],
            &["--source", "/s", "--destination", "/d", "--force"],
            &["--source", "/s", "--replace", "--replace"],
        ] {
            assert_eq!(parse(&args(bad)), None, "{bad:?}");
        }
    }
}
