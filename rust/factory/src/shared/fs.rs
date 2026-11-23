//! Files as `factory.py` reads and writes them.

use std::collections::hash_map::RandomState;
use std::fs::{self, File, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use super::py::universal_newlines;

/// `open(p, encoding="utf-8").read()`: strict UTF-8 with universal newlines.
pub fn read_text(p: &Path) -> io::Result<String> {
    let text = String::from_utf8(fs::read(p)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
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
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
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

/// `os.access(p, os.R_OK)`.
pub fn readable(p: &Path) -> bool {
    File::open(p).is_ok()
}

/// `os.access(p, os.R_OK | os.X_OK)` for a directory.
pub fn listable(p: &Path) -> bool {
    fs::read_dir(p).is_ok()
}

/// `Path.resolve()`: the longest existing ancestor canonicalized, then the rest appended.
pub fn resolve(p: &Path) -> io::Result<PathBuf> {
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir()?.join(p) };
    let parts: Vec<Component> = abs.components().collect();
    for i in (1..=parts.len()).rev() {
        let Ok(mut base) = fs::canonicalize(parts[..i].iter().collect::<PathBuf>()) else { continue };
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
        let text = resolve(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned();
        if cfg!(windows) {
            text.to_lowercase().replace('/', "\\")
        } else {
            text
        }
    };
    norm(a) == norm(b)
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

    #[cfg(unix)]
    #[test]
    fn write_atomic_leaves_mode_0600_and_no_temp() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("atomic");
        let target = dir.join("state.json");
        write_atomic(&target, b"one").unwrap();
        write_atomic(&target, b"two").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
        assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
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
