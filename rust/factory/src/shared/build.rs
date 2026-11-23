//! The host-profile build that `install` and `catch-up` share: one private synced source folder, a
//! per-user target folder, and a cache of built binaries keyed by everything that shapes them.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Digest, Sha256};

use super::fs::{is_link, normpath_with, write_atomic};
use super::proc::{capture, executable, which};
use super::project::sha;
use super::py::{os_error, round_int, strip};
use super::pyre::{Flags, Pattern};

/// The binary's path inside an installed skill.
pub const BINARY: &str = if cfg!(windows) {
    "runtime/factory.exe"
} else {
    "runtime/factory"
};
/// The lock file in build-src. `tools/install.py` takes the same one, so the two never build at once.
pub const LOCK: &str = ".build.lock";
const PROFILE: &str = "host";
/// Binaries kept in the cache, newest first.
const KEEP: usize = 3;
pub const NO_CARGO: &str =
    "cargo not found; install rustup from https://rustup.rs, then run the install again";
// A literal include_str! or include_bytes! path, relative to the .rs file that holds it.
static EMBED: Pattern = Pattern::new(r#"include_(?:str|bytes)!\(\s*"([^"]+)"\s*\)"#, Flags::NONE);

/// What the build reads from this host. Tests build their own instead of editing the process environment.
pub struct Host {
    pub home: PathBuf,
    /// A nonempty environment variable.
    pub var: Box<dyn Fn(&str) -> Option<OsString>>,
    /// cargo on PATH.
    pub cargo: Option<PathBuf>,
}

impl Host {
    pub fn current() -> Result<Host, String> {
        let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let home = std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .ok_or("Could not determine home directory.")?;
        Ok(Host {
            home: PathBuf::from(home),
            var: Box::new(|name| std::env::var_os(name).filter(|v| !v.is_empty())),
            cargo: which("cargo"),
        })
    }

    fn text(&self, name: &str) -> String {
        (self.var)(name).map_or_else(String::new, |v| v.to_string_lossy().into_owned())
    }
}

/// The cached host-profile binary built from checkout's rust/ folder, building it first when no
/// cached one matches. stderr gets one line saying which. Err is the reason, without a prefix.
pub fn rust_binary(checkout: &Path, host: &Host, err: &mut dyn Write) -> Result<PathBuf, String> {
    if !checkout.join("rust").join("Cargo.toml").is_file() {
        return Err(format!("{} has no rust/Cargo.toml", checkout.display()));
    }
    // rustup installs into ~/.cargo/bin, which launchd and non-login shells leave off PATH.
    let home_cargo = host.home.join(".cargo").join("bin").join(if cfg!(windows) {
        "cargo.exe"
    } else {
        "cargo"
    });
    let cargo = host
        .cargo
        .clone()
        .or_else(|| executable(&home_cargo).then_some(home_cargo))
        .ok_or(NO_CARGO)?;
    build(checkout, host, &cargo, err).map_err(|e| format!("Rust build failed ({e})"))
}

fn build(
    checkout: &Path,
    host: &Host,
    cargo: &Path,
    err: &mut dyn Write,
) -> Result<PathBuf, String> {
    let name = Path::new(BINARY).file_name().unwrap_or_default();
    let cache = if cfg!(windows) {
        let local = (host.var)("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
        PathBuf::from(local).join("huss-skills").join("factory")
    } else {
        host.home.join(".cache").join("huss-skills").join("factory")
    };
    // A per-user target folder keeps dependencies compiled across catch-up's fresh source worktrees.
    let target = (host.var)("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| cache.join("cargo-target"));
    let files = build_inputs(checkout)?;
    let mut key = Sha256::new();
    for (rel, data) in &files {
        key.update(rel.as_bytes());
        key.update(b"\0");
        key.update(Sha256::digest(data));
    }
    // rustc -vV prints the pinned toolchain's version and its host target triple; cwd picks the pin.
    let rustc = cargo.with_file_name(if cfg!(windows) { "rustc.exe" } else { "rustc" });
    let version = run(
        &[&rustc.to_string_lossy(), "-vV"],
        &checkout.join("rust"),
        &[],
    )?;
    key.update(version.as_bytes());
    key.update(b"\0");
    key.update(PROFILE.as_bytes());
    // Flags and the user's cargo config change the binary without changing any synced file.
    for flag in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"] {
        key.update(format!("\0{flag}={}", host.text(flag)).as_bytes());
    }
    let cargo_home = (host.var)("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| host.home.join(".cargo"));
    for config in ["config.toml", "config"] {
        let path = cargo_home.join(config);
        if path.is_file() {
            key.update(format!("\0{config}\0").as_bytes());
            key.update(fs::read(&path).map_err(|e| os_error(&e, Some(&path)))?);
        }
    }
    let key: String = key.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let bins = private_dir(&cache.join("bin"))?;
    let stored = bins.join(&key);
    let binary = stored.join(name);
    let sum = stored.join(format!("{}.sha256", name.to_string_lossy()));
    if binary.is_file() && sum.is_file() {
        let bytes = fs::read(&binary).map_err(|e| os_error(&e, Some(&binary)))?;
        let want = fs::read_to_string(&sum).map_err(|e| os_error(&e, Some(&sum)))?;
        if sha(&bytes) == strip(&want) {
            executable_by_owner(&binary)?;
            let _ = writeln!(err, "install: Rust binary from cache {}", &key[..8]);
            return Ok(binary);
        }
    }
    make_dirs(&target).map_err(|e| os_error(&e, Some(&target)))?;
    // One stable source path keeps cargo's incremental artifacts valid from one checkout to the next.
    let src = private_dir(&cache.join("build-src"))?;
    let lock_path = src.join(LOCK);
    let lock = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&lock_path)
        .map_err(|e| os_error(&e, Some(&lock_path)))?;
    lock.lock().map_err(|e| os_error(&e, Some(&lock_path)))?;
    sync(&files, &src).map_err(|e| os_error(&e, None))?;
    let start = Instant::now();
    let manifest = src.join("rust").join("Cargo.toml");
    let target_text = target.to_string_lossy();
    // cwd is the workspace so rustup reads its rust-toolchain.toml.
    run(
        &[
            &cargo.to_string_lossy(),
            "build",
            "--profile",
            PROFILE,
            "--locked",
            "--manifest-path",
            &manifest.to_string_lossy(),
        ],
        &src.join("rust"),
        &[("CARGO_TARGET_DIR", &target_text)],
    )?;
    let built_path = target.join(PROFILE).join(name);
    let built = fs::read(&built_path).map_err(|e| os_error(&e, Some(&built_path)))?;
    match fs::create_dir(&stored) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => {
            return Err(os_error(&e, Some(&stored)))
        }
        _ => {}
    }
    // The sha256 goes last, so a reader never matches a half-written binary.
    write_atomic(&binary, &built).map_err(|e| os_error(&e, Some(&binary)))?;
    executable_by_owner(&binary)?;
    write_atomic(&sum, format!("{}\n", sha(&built)).as_bytes())
        .map_err(|e| os_error(&e, Some(&sum)))?;
    let mut others: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(&bins)
        .map_err(|e| os_error(&e, Some(&bins)))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| *p != stored)
        .map(|p| (fs::metadata(&p).and_then(|m| m.modified()), p))
        .map(|(m, p)| m.map(|m| (m, p)).map_err(|e| os_error(&e, None)))
        .collect::<Result<_, _>>()?;
    others.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, old) in others.into_iter().skip(KEEP - 1) {
        fs::remove_dir_all(&old).map_err(|e| os_error(&e, Some(&old)))?;
    }
    drop(lock);
    let _ = writeln!(
        err,
        "install: Rust binary built in {} s",
        round_int(start.elapsed().as_secs_f64())
    );
    Ok(binary)
}

/// catch-up runs the cached binary in place, and a stored one may predate the exec bit.
fn executable_by_owner(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|e| os_error(&e, Some(path)))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// args' stdout, or Err carrying the last line it wrote to stderr.
fn run(args: &[&str], cwd: &Path, env: &[(&str, &str)]) -> Result<String, String> {
    let out =
        capture(args, Some(cwd), env, None).map_err(|e| os_error(&e, Some(Path::new(args[0]))))?;
    if out.code != 0 {
        let last = out.stderr.lines().rev().find(|l| !strip(l).is_empty());
        return Err(last.map_or_else(|| format!("exit {}", out.code), |l| strip(l).to_string()));
    }
    Ok(out.stdout)
}

/// Every file under dir, as paths relative to base, skipping base's top-level `skip` folder.
fn files_under(dir: &Path, base: &Path, skip: &str) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if dir != base || path.file_name().is_none_or(|n| n != skip) {
                found.extend(files_under(&path, base, skip)?);
            }
        } else {
            found.push(path.strip_prefix(base).unwrap_or(&path).to_path_buf());
        }
    }
    Ok(found)
}

fn posix(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The bytes of every file under rust/ but target/, and of every file outside it the crate embeds,
/// by checkout-relative path.
pub fn build_inputs(checkout: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let rust = checkout.join("rust");
    let read = |rel: &str| {
        let path = checkout.join(rel);
        fs::read(&path).map_err(|e| os_error(&e, Some(&path)))
    };
    let mut files = BTreeMap::new();
    for rel in files_under(&rust, &rust, "target").map_err(|e| os_error(&e, Some(&rust)))? {
        let rel = format!("rust/{}", posix(&rel));
        files.insert(rel.clone(), read(&rel)?);
    }
    let sources: Vec<String> = files
        .keys()
        .filter(|r| r.ends_with(".rs"))
        .cloned()
        .collect();
    for rel in sources {
        let text = String::from_utf8_lossy(&files[&rel]).into_owned();
        let folder = rel.rsplit_once('/').map_or("", |(f, _)| f);
        for found in EMBED.find_iter(&text) {
            let Some(embedded) = EMBED.fullmatch(found.as_str()).and_then(|c| c.get(1)) else {
                continue;
            };
            let path = normpath_with(&format!("{folder}/{}", embedded.as_str()), false);
            let data = read(&path)?;
            files.insert(path, data);
        }
    }
    Ok(files)
}

/// `os.makedirs(path, mode=0o700, exist_ok=True)`.
fn make_dirs(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

/// path, created 0700, refused when it is a link, not a folder, another user's or open to others.
pub fn private_dir(path: &Path) -> Result<PathBuf, String> {
    make_dirs(path).map_err(|e| os_error(&e, Some(path)))?;
    let info = fs::symlink_metadata(path).map_err(|e| os_error(&e, Some(path)))?;
    let mut private = info.is_dir() && !is_link(path).unwrap_or(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        private = private && info.uid() == super::fs::uid() && info.mode() & 0o077 == 0;
    }
    if !private {
        return Err(format!("{} is not a private folder", path.display()));
    }
    Ok(path.to_path_buf())
}

/// Make root hold exactly files beside its lock, writing only files whose bytes differ so the
/// unchanged ones keep the mtimes cargo's fingerprints recorded.
pub fn sync(files: &BTreeMap<String, Vec<u8>>, root: &Path) -> io::Result<()> {
    for (rel, data) in files {
        let path = root.join(rel);
        if !path.is_file() || fs::read(&path)? != *data {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, data)?;
        }
    }
    for rel in files_under(root, root, "")? {
        let key = posix(&rel);
        if key != LOCK && !files.contains_key(&key) {
            fs::remove_file(root.join(rel))?;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    pub(crate) fn script(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    pub(crate) fn fake_rustc(folder: &Path, version: &str) {
        script(
            &folder.join("rustc"),
            &format!("printf 'rustc {version}\\nhost: x86_64-unknown-linux-gnu\\n'\n"),
        );
    }

    /// A host whose home is root, with vars as its environment and cargo, when given, on its PATH.
    pub(crate) fn host(root: &Path, vars: &[(&str, &str)], cargo: Option<PathBuf>) -> Host {
        let vars: HashMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        Host {
            home: root.to_path_buf(),
            var: Box::new(move |name| vars.get(name).cloned()),
            cargo,
        }
    }

    /// A git checkout under root holding a copy of the skill and rust/Cargo.toml.
    pub(crate) fn checkout(root: &Path) -> PathBuf {
        let checkout = root.join("checkout");
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let skill = checkout.join("skills/factory");
        fs::create_dir_all(&skill).unwrap();
        let status = Command::new("cp")
            .arg("-R")
            .arg(repo.join("skills/factory/."))
            .arg(&skill)
            .status()
            .unwrap();
        assert!(status.success());
        let _ = fs::remove_dir_all(skill.join("runtime/__pycache__"));
        fs::create_dir(checkout.join("rust")).unwrap();
        fs::write(checkout.join("rust/Cargo.toml"), "[workspace]\n").unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "-A"],
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "synthetic",
            ],
        ] {
            let o = Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(args)
                .output()
                .unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        }
        checkout
    }

    /// A fake cargo in root/bin that appends a line to root/runs and builds a binary holding stub.
    fn fake_cargo(root: &Path, stub: &str) -> PathBuf {
        let cargo = root.join("bin/cargo");
        script(
            &cargo,
            &format!(
                "echo >> '{}/runs' && mkdir -p \"$CARGO_TARGET_DIR/host\" && printf '{stub}' > \"$CARGO_TARGET_DIR/host/factory\"\n",
                root.display()
            ),
        );
        cargo
    }

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn unchanged_inputs_reuse_the_cached_binary_and_three_are_kept() {
        let root = temp_dir("build-cache");
        let checkout = checkout(&root);
        let lib = checkout.join("rust/lib.rs");
        let bins = root.join(".cache/huss-skills/factory/bin");
        let runs = || {
            fs::read_to_string(root.join("runs"))
                .unwrap()
                .lines()
                .count()
        };
        fake_rustc(&root.join("bin"), "1.100.0");
        let rust = |stub: &str, text: &str, vars: &[(&str, &str)]| {
            let cargo = fake_cargo(&root, stub);
            fs::write(&lib, text).unwrap();
            let mut err = Vec::new();
            let path = rust_binary(&checkout, &host(&root, vars, Some(cargo)), &mut err).unwrap();
            (fs::read(path).unwrap(), String::from_utf8(err).unwrap())
        };
        assert_eq!(rust("one", "1", &[]).0, b"one");
        let (binary, err) = rust("other", "1", &[]);
        assert_eq!((binary, runs()), (b"one".to_vec(), 1));
        let key: Vec<String> = fs::read_dir(&bins)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            err,
            format!("install: Rust binary from cache {}\n", &key[0][..8])
        );
        assert_eq!(mode(&bins), 0o700);
        assert_eq!(
            mode(&root.join(".cache/huss-skills/factory/cargo-target")),
            0o700
        );
        assert_eq!(
            mode(&root.join(".cache/huss-skills/factory/build-src")),
            0o700
        );
        assert_eq!(mode(&bins.join(&key[0]).join("factory")), 0o700);
        let old = fs::File::open(bins.join(&key[0])).unwrap();
        old.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        for n in ["2", "3", "4"] {
            assert_eq!(
                rust(n, n, &[]),
                (
                    n.as_bytes().to_vec(),
                    "install: Rust binary built in 0 s\n".into()
                )
            );
        }
        assert_eq!((runs(), fs::read_dir(&bins).unwrap().count()), (4, 3));
        assert!(!bins.join(&key[0]).exists());
        // The rustc version and host triple are part of the key.
        fake_rustc(&root.join("bin"), "9.9.9");
        assert_eq!(rust("5", "4", &[]).0, b"5");
        // A stored binary whose hash no longer matches is rebuilt, not installed.
        let stored = fs::read_dir(&bins)
            .unwrap()
            .map(|e| e.unwrap().path())
            .max_by_key(|p| fs::metadata(p).unwrap().modified().unwrap())
            .unwrap();
        fs::write(stored.join("factory"), "tampered").unwrap();
        assert_eq!(rust("6", "4", &[]).0, b"6");
        assert_eq!(runs(), 6);
        let mut names: Vec<String> = fs::read_dir(&stored)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["factory", "factory.sha256"]);
        assert_eq!(fs::read_dir(&bins).unwrap().count(), 3);
        // Rust flags and the user's cargo config are part of the key too.
        assert_eq!(
            rust("7", "4", &[("RUSTFLAGS", "-C target-cpu=native")]).0,
            b"7"
        );
        assert_eq!(rust("8", "4", &[]).0, b"6");
        fs::create_dir(root.join("cargo-home")).unwrap();
        fs::write(
            root.join("cargo-home/config.toml"),
            "[build]\nrustflags = ['-Copt-level=1']\n",
        )
        .unwrap();
        let cargo_home = root.join("cargo-home");
        assert_eq!(
            rust("9", "4", &[("CARGO_HOME", cargo_home.to_str().unwrap())]).0,
            b"9"
        );
        assert_eq!(runs(), 8);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_held_build_lock_makes_the_next_build_wait() {
        let root = temp_dir("build-lock");
        let checkout = checkout(&root);
        fake_rustc(&root.join("bin"), "1.100.0");
        let cargo = root.join("bin/cargo");
        script(
            &cargo,
            &format!(
                "touch '{}/ran' && mkdir -p \"$CARGO_TARGET_DIR/host\" && printf lock > \"$CARGO_TARGET_DIR/host/factory\"\n",
                root.display()
            ),
        );
        let src = private_dir(&root.join(".cache/huss-skills/factory/build-src")).unwrap();
        let lock = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(src.join(LOCK))
            .unwrap();
        lock.lock().unwrap();
        let (r, c) = (root.clone(), checkout.clone());
        let thread = std::thread::spawn(move || {
            let mut err = Vec::new();
            let path = rust_binary(&c, &host(&r, &[], Some(cargo)), &mut err).unwrap();
            (fs::read(path).unwrap(), String::from_utf8(err).unwrap())
        });
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!thread.is_finished());
        assert!(!root.join("ran").exists());
        drop(lock);
        assert_eq!(
            thread.join().unwrap(),
            (
                b"lock".to_vec(),
                "install: Rust binary built in 0 s\n".into()
            )
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_build_and_missing_cargo_are_errors() {
        let root = temp_dir("build-fail");
        let checkout = checkout(&root);
        fake_rustc(&root.join("bin"), "1.100.0");
        let cargo = root.join("bin/cargo");
        script(&cargo, "echo 'error: synthetic failure' >&2\nexit 101\n");
        let mut err = Vec::new();
        assert_eq!(
            rust_binary(&checkout, &host(&root, &[], Some(cargo)), &mut err),
            Err("Rust build failed (error: synthetic failure)".into())
        );
        assert_eq!(
            rust_binary(&checkout, &host(&root, &[], None), &mut err),
            Err(NO_CARGO.into())
        );
        assert!(err.is_empty());
        fs::remove_file(checkout.join("rust/Cargo.toml")).unwrap();
        assert_eq!(
            rust_binary(&checkout, &host(&root, &[], None), &mut err),
            Err(format!("{} has no rust/Cargo.toml", checkout.display()))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cargo_off_path_falls_back_to_home_cargo_bin() {
        let root = temp_dir("build-home-cargo");
        let checkout = checkout(&root);
        let cargo = root.join(".cargo/bin/cargo");
        script(
            &cargo,
            "mkdir -p \"$CARGO_TARGET_DIR/host\" && printf home > \"$CARGO_TARGET_DIR/host/factory\"\n",
        );
        fake_rustc(cargo.parent().unwrap(), "1.100.0");
        let mut err = Vec::new();
        let path = rust_binary(&checkout, &host(&root, &[], None), &mut err).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"home");
        assert_eq!(err, b"install: Rust binary built in 0 s\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn own_cargo_target_dir_is_used() {
        let root = temp_dir("build-own-target");
        let checkout = checkout(&root);
        fake_rustc(&root.join("bin"), "1.100.0");
        let cargo = root.join("bin/cargo");
        script(
            &cargo,
            "mkdir -p \"$CARGO_TARGET_DIR/host\" && printf stub > \"$CARGO_TARGET_DIR/host/factory\"\nprintf '%s\\n' \"$PWD\" \"$@\" > \"$CARGO_TARGET_DIR/args\"\n",
        );
        let own = root.join("own-target");
        let vars = [("CARGO_TARGET_DIR", own.to_str().unwrap())];
        rust_binary(&checkout, &host(&root, &vars, Some(cargo)), &mut Vec::new()).unwrap();
        let src = root.join(".cache/huss-skills/factory/build-src");
        assert_eq!(
            fs::read_to_string(own.join("args")).unwrap(),
            format!(
                "{}\nbuild\n--profile\nhost\n--locked\n--manifest-path\n{}\n",
                src.join("rust").display(),
                src.join("rust/Cargo.toml").display()
            )
        );
        assert_eq!(
            fs::read_to_string(src.join("rust/Cargo.toml")).unwrap(),
            "[workspace]\n"
        );
        assert!(!checkout.join("rust/target").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sync_rewrites_only_changed_files_and_drops_vanished_ones() {
        let root = temp_dir("build-sync").join("src");
        let mut files: BTreeMap<String, Vec<u8>> = [
            ("rust/a.rs", "a"),
            ("rust/b.rs", "b"),
            ("skills/factory/agents/x.md", "x"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
        .collect();
        sync(&files, &root).unwrap();
        fs::write(root.join(LOCK), "").unwrap();
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
        for name in ["rust/a.rs", "rust/b.rs"] {
            fs::File::options()
                .write(true)
                .open(root.join(name))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        files.remove("skills/factory/agents/x.md");
        files.insert("rust/b.rs".into(), b"b2".to_vec());
        sync(&files, &root).unwrap();
        let mtime = |n: &str| fs::metadata(root.join(n)).unwrap().modified().unwrap();
        assert_eq!(mtime("rust/a.rs"), old);
        assert_eq!(fs::read(root.join("rust/b.rs")).unwrap(), b"b2");
        assert_ne!(mtime("rust/b.rs"), old);
        assert!(!root.join("skills/factory/agents/x.md").exists());
        assert!(root.join(LOCK).exists());
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn build_inputs_hold_every_file_the_crate_embeds() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let files = build_inputs(&repo).unwrap();
        static ANY: Pattern = Pattern::new(r"include_(?:str|bytes)!", Flags::NONE);
        let mut embedded = 0;
        // This file names the macros in its own comments and patterns.
        let scanner = "rust/factory/src/shared/build.rs";
        for rel in files.keys().filter(|r| r.ends_with(".rs") && *r != scanner) {
            let text = String::from_utf8_lossy(&files[rel]).into_owned();
            // An include the scan cannot read, such as include_str!(concat!(...)), fails here.
            assert_eq!(
                ANY.find_iter(&text).count(),
                EMBED.find_iter(&text).count(),
                "{rel}"
            );
            embedded += EMBED.find_iter(&text).count();
        }
        assert!(files.contains_key("skills/factory/agents/factory-builder.md"));
        assert!(files.contains_key("skills/factory/templates/config.md"));
        assert!(embedded > 10);
        assert!(!files.keys().any(|r| r.starts_with("rust/target/")));
    }

    #[test]
    fn build_folder_must_be_private() {
        let root = temp_dir("build-private");
        let folder = root.join("build-src");
        assert_eq!(private_dir(&folder).unwrap(), folder);
        assert_eq!(mode(&folder), 0o700);
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&folder).is_err());
        fs::create_dir(root.join("elsewhere")).unwrap();
        fs::set_permissions(root.join("elsewhere"), fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(root.join("elsewhere"), root.join("link")).unwrap();
        assert!(private_dir(&root.join("link")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
