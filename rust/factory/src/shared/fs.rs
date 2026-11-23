//! Files as `factory.py` reads and writes them.

use std::collections::hash_map::RandomState;
use std::fs::{self, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use super::exit::{fail, Exit, Res};
use super::py::{os_error, repr, universal_newlines};

/// `open(p, encoding="utf-8").read()`: strict UTF-8 with universal newlines.
pub fn read_text(p: &Path) -> io::Result<String> {
    let text = String::from_utf8(fs::read(p)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(universal_newlines(text))
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

/// `write_atomic` and `write_atomic_private`: a 0600 temp file in the same folder, renamed over p.
pub fn write_atomic(p: &Path, data: &[u8]) -> io::Result<()> {
    let folder = p.parent().unwrap_or(Path::new("."));
    let (tmp, mut file) = loop {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        let tmp = folder.join(format!(".tmp-{:016x}", h.finish()));
        match private_options().open(&tmp) {
            Ok(file) => break (tmp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let written = file.write_all(data).and_then(|_| {
        drop(file);
        fs::rename(&tmp, p)
    });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// `seed`: write p only if it does not exist. True if written.
pub fn seed(p: &Path, data: &[u8], mode: u32) -> io::Result<bool> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
    #[cfg(not(unix))]
    let _ = mode;
    match options.open(p) {
        Ok(mut f) => f.write_all(data).map(|_| true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// `p.mkdir(mode=0o700, exist_ok=True)`.
pub fn mkdir_private(p: &Path) -> io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(p) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && p.is_dir() => Ok(()),
        other => other,
    }
}

/// True when p is a symlink or a Windows reparse point. A missing path is not a link.
pub fn is_link(p: &Path) -> io::Result<bool> {
    let meta = match fs::symlink_metadata(p) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Ok(true);
        }
    }
    Ok(meta.file_type().is_symlink())
}

/// `regular_metadata`: a readable regular file that is not a link.
pub fn regular_metadata(p: &Path) -> Result<(), ()> {
    if is_link(p).map_err(|_| ())? || !fs::metadata(p).map_err(|_| ())?.is_file() || !readable(p) {
        return Err(());
    }
    Ok(())
}

/// `os.access(p, mode)`: access(2), which opens nothing. Opening every file of a large home cost
/// seconds where an endpoint agent inspects each open.
#[cfg(unix)]
fn access(p: &Path, mode: i32) -> bool {
    use std::os::unix::ffi::OsStrExt;
    extern "C" {
        fn access(path: *const std::ffi::c_char, mode: i32) -> i32;
    }
    let Ok(path) = std::ffi::CString::new(p.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: path is a NUL-terminated string that outlives the call.
    unsafe { access(path.as_ptr(), mode) == 0 }
}

/// `os.access(p, os.R_OK)`.
pub fn readable(p: &Path) -> bool {
    #[cfg(unix)]
    return access(p, 4);
    #[cfg(not(unix))]
    if p.is_dir() {
        fs::read_dir(p).is_ok()
    } else {
        fs::File::open(p).is_ok()
    }
}

/// `os.access(p, os.R_OK | os.X_OK)` for a directory.
pub fn listable(p: &Path) -> bool {
    #[cfg(unix)]
    return access(p, 4 | 1);
    #[cfg(not(unix))]
    fs::read_dir(p).is_ok()
}

/// `Path.resolve()`: the longest existing ancestor canonicalized, then the rest appended.
pub fn resolve(p: &Path) -> io::Result<PathBuf> {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()?.join(p)
    };
    let parts: Vec<Component> = abs.components().collect();
    for i in (1..=parts.len()).rev() {
        let Ok(mut base) = fs::canonicalize(parts[..i].iter().collect::<PathBuf>()) else {
            continue;
        };
        for part in &parts[i..] {
            match part {
                Component::ParentDir => {
                    base.pop();
                }
                Component::Normal(name) => base.push(name),
                _ => {}
            }
        }
        return Ok(verbatim_free(base));
    }
    Ok(abs)
}

#[cfg(windows)]
fn verbatim_free(p: PathBuf) -> PathBuf {
    let text = p.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p,
    }
}

#[cfg(not(windows))]
fn verbatim_free(p: PathBuf) -> PathBuf {
    p
}

/// `same_path`: both sides resolved, compared case-insensitively on Windows.
pub fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let text = resolve(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .into_owned();
        if cfg!(windows) {
            text.to_lowercase().replace('/', "\\")
        } else {
            text
        }
    };
    norm(a) == norm(b)
}

/// `os.path.normpath`: posix rules, or ntpath's separators when windows is true. It never folds case.
pub fn normpath_with(p: &str, windows: bool) -> String {
    let (text, sep) = if windows {
        (p.replace('/', "\\"), '\\')
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

/// `os.path.normpath` on this platform.
pub fn normpath(p: &str) -> String {
    normpath_with(p, cfg!(windows))
}

/// True when p, normalized, keeps no `..` and lies at or below base.
pub fn contained(base: &Path, p: &Path) -> bool {
    let p = PathBuf::from(normpath(&p.to_string_lossy()));
    !p.components().any(|c| c == Component::ParentDir) && p.starts_with(base)
}

/// `os.path.abspath` on this platform.
pub fn abspath(dir: &str, cwd: &Path) -> PathBuf {
    if cfg!(windows) {
        std::path::absolute(cwd.join(dir)).unwrap_or_else(|_| cwd.join(dir))
    } else if dir.starts_with('/') {
        PathBuf::from(normpath(dir))
    } else {
        PathBuf::from(normpath(&format!("{}/{dir}", cwd.display())))
    }
}

#[cfg(test)]
thread_local! {
    /// The folder temp_root returns in this test thread, when set.
    pub(crate) static TEMP_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// `tempfile.gettempdir()`: the first candidate folder a probe file can be written to.
pub fn temp_root() -> Res<PathBuf> {
    #[cfg(test)]
    if let Some(root) = TEMP_ROOT.with(|r| r.borrow().clone()) {
        return Ok(root);
    }
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
                    let wrote = io::Write::write_all(&mut file, b"blat");
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

fn refused(path: &Path) -> Exit {
    fail(
        format!(
            "{} is a link or a file; remove it, then run prompt again",
            path.display()
        ),
        6,
    )
}

pub(crate) fn io_fail(e: io::Error, path: &Path) -> Exit {
    fail(os_error(&e, Some(path)), 1)
}

#[cfg(unix)]
extern "C" {
    fn getuid() -> u32;
}

/// `os.getuid()`.
#[cfg(unix)]
pub(crate) fn uid() -> u32 {
    // SAFETY: getuid takes no arguments, cannot fail and touches no memory.
    unsafe { getuid() }
}

#[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
const O_DIRECTORY_NOFOLLOW: i32 = 0o40000 | 0o100000;
#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "aarch64", target_arch = "arm"))
))]
const O_DIRECTORY_NOFOLLOW: i32 = 0o200000 | 0o400000;
#[cfg(target_os = "macos")]
const O_DIRECTORY_NOFOLLOW: i32 = 0x100000 | 0x100;

/// Create the job's temp folder private to this account, refusing a name someone else holds.
pub fn make_job_temp(slug: &str, job_id: &str) -> Res<PathBuf> {
    let path = temp_root()?.join(format!("factory-{slug}-{job_id}"));
    if is_link(&path).map_err(|e| io_fail(e, &path))?
        || fs::symlink_metadata(&path).is_ok() && !path.is_dir()
    {
        return Err(refused(&path));
    }
    // SAFETY: getuid takes no arguments, cannot fail and touches no memory.
    #[cfg(unix)]
    let uid = Some(unsafe { getuid() });
    #[cfg(not(unix))]
    let uid = None;
    create_and_secure(&path, uid)?;
    Ok(path)
}

/// Make path a 0700 folder, then refuse it unless it is a real folder, owned by uid when given.
fn create_and_secure(path: &Path, uid: Option<u32>) -> Res<()> {
    match mkdir_private(path) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(refused(path)),
        Err(e) => return Err(io_fail(e, path)),
        Ok(()) => {}
    }
    let st = fs::symlink_metadata(path).map_err(|e| io_fail(e, path))?;
    if is_link(path).map_err(|e| io_fail(e, path))? || !st.is_dir() {
        return Err(refused(path));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        if uid.is_some_and(|uid| st.uid() != uid) {
            return Err(fail(
                format!(
                    "{} belongs to another account; remove it, then run prompt again",
                    path.display()
                ),
                6,
            ));
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_DIRECTORY_NOFOLLOW)
            .open(path)
            .map_err(|_| refused(path))?;
        let opened = file.metadata().map_err(|e| io_fail(e, path))?;
        if (opened.dev(), opened.ino()) != (st.dev(), st.ino()) {
            return Err(refused(path));
        }
        file.set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(|e| io_fail(e, path))?;
    }
    #[cfg(not(unix))]
    {
        let _ = uid;
        let mut perms = st.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(path, perms).map_err(|e| io_fail(e, path))?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("factory-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    resolve(&dir).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_covers_directories_and_files() {
        let dir = std::env::temp_dir().join(format!("factory-readable-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f");
        fs::write(&file, b"x").unwrap();
        assert!(readable(&dir));
        assert!(readable(&file));
        assert!(!readable(&dir.join("missing")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn normpath_matches_posix_and_nt() {
        for (given, posix, nt) in [
            ("", ".", "."),
            ("a/./b/../c/", "a/c", "a\\c"),
            ("///x", "/x", "\\x"),
            ("../a/..", "..", ".."),
            ("A\\B/..", ".", "A"),
        ] {
            assert_eq!(normpath_with(given, false), posix, "{given}");
            assert_eq!(normpath_with(given, true), nt, "{given}");
        }
        assert_eq!(normpath_with("//x/../y", false), "//y");
    }

    #[cfg(unix)]
    #[test]
    fn contained_needs_base_and_no_parent() {
        let base = Path::new("/w/job");
        assert!(contained(base, Path::new("/w/job")));
        assert!(contained(base, Path::new("/w/job/a/../b")));
        assert!(!contained(base, Path::new("/w/job/../other")));
        assert!(!contained(base, Path::new("/w/jobs")));
        assert!(!contained(base, Path::new("../w/job")));
    }

    #[cfg(unix)]
    #[test]
    fn temp_refuses_link() {
        let real = temp_dir("prompt-link");
        let job = format!("{}-link", std::process::id());
        let path = temp_root()
            .unwrap()
            .join(format!("factory-prompt-test-{job}"));
        let _ = fs::remove_file(&path);
        std::os::unix::fs::symlink(&real, &path).unwrap();
        let e = make_job_temp("prompt-test", &job).unwrap_err();
        let want = format!(
            "factory.py: {} is a link or a file; remove it, then run prompt again\n",
            path.display()
        );
        assert_eq!((e.code, e.stderr), (6, want));
        assert!(fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_file(&path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn temp_refuses_link_after_precheck() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("prompt-planted-link");
        let real = dir.join("elsewhere");
        fs::create_dir(&real).unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.join("factory-sample-001");
        std::os::unix::fs::symlink(&real, &path).unwrap();
        assert_eq!(create_and_secure(&path, None).unwrap_err().code, 6);
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[cfg(unix)]
    #[test]
    fn temp_refuses_file_after_precheck() {
        let dir = temp_dir("prompt-planted-file");
        let path = dir.join("factory-sample-001");
        fs::write(&path, "x\n").unwrap();
        assert_eq!(create_and_secure(&path, None).unwrap_err().code, 6);
        assert_eq!(fs::read_to_string(&path).unwrap(), "x\n");
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(dir.join("missing"), &path).unwrap();
        assert_eq!(create_and_secure(&path, None).unwrap_err().code, 6);
        assert!(fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn temp_refuses_other_owner() {
        let path = temp_dir("prompt-owner").join("factory-sample-001");
        // SAFETY: getuid takes no arguments, cannot fail and touches no memory.
        let uid = unsafe { getuid() };
        let e = create_and_secure(&path, Some(uid + 1)).unwrap_err();
        let want = format!(
            "factory.py: {} belongs to another account; remove it, then run prompt again\n",
            path.display()
        );
        assert_eq!((e.code, e.stderr), (6, want));
        create_and_secure(&path, Some(uid)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_leaves_mode_0600_and_no_temp() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("atomic");
        let target = dir.join("state.json");
        write_atomic(&target, b"one").unwrap();
        write_atomic(&target, b"two").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        assert!(seed(&dir.join("seeded"), b"x", 0o644).unwrap());
        assert!(!seed(&dir.join("seeded"), b"y", 0o644).unwrap());
        mkdir_private(&dir.join("sub")).unwrap();
        mkdir_private(&dir.join("sub")).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn is_link_sees_symlinks_only() {
        let dir = temp_dir("link");
        fs::write(dir.join("file"), "x").unwrap();
        std::os::unix::fs::symlink(dir.join("file"), dir.join("link")).unwrap();
        assert!(is_link(&dir.join("link")).unwrap());
        assert!(!is_link(&dir.join("file")).unwrap());
        assert!(!is_link(&dir.join("missing")).unwrap());
        assert!(regular_metadata(&dir.join("file")).is_ok());
        assert!(regular_metadata(&dir.join("link")).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resolve_keeps_a_missing_tail() {
        let dir = temp_dir("resolve");
        assert_eq!(resolve(&dir.join("a/b/../c")).unwrap(), dir.join("a/c"));
        assert_eq!(resolve(&dir.join("missing/..")).unwrap(), dir);
        assert!(same_path(&dir.join("x/../y"), &dir.join("y")));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_text_translates_newlines() {
        let dir = temp_dir("read");
        fs::write(dir.join("f"), b"a\r\nb\rc").unwrap();
        assert_eq!(read_text(&dir.join("f")).unwrap(), "a\nb\nc");
        fs::write(dir.join("bad"), b"\xff").unwrap();
        assert!(read_text(&dir.join("bad")).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
