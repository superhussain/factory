//! Where the factory home is, and who this process claims jobs as.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::conf::{closed_frontmatter, fm_get, frontmatter};
use super::consts::{JOB_REL, SLUG};
use super::exit::{fail, Res};
use super::fs::{is_link, listable, readable, regular_metadata, resolve};
use super::proc::hostname;
use super::project::read_project_state;
use super::py::{os_error, repr};
use super::pyjson;
use super::pyre::{Flags, Pattern};

static OVERRIDE: OnceLock<PathBuf> = OnceLock::new();
static LOCATED: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
static SHA256: Pattern = Pattern::new("[0-9a-f]{64}", Flags::NONE);

/// `--home`, already resolved. Set once, before the handler runs.
pub fn set_override(p: PathBuf) {
    let _ = OVERRIDE.set(p);
}

fn nonempty(v: Option<&OsStr>) -> Option<&OsStr> {
    v.filter(|v| !v.is_empty())
}

fn config_root_from(xdg: Option<&OsStr>, user_home: Option<&Path>) -> Res<PathBuf> {
    let root = match (nonempty(xdg), user_home) {
        (Some(xdg), _) => PathBuf::from(xdg),
        (None, Some(home)) => home.join(".config"),
        (None, None) => return Err(fail("cannot resolve XDG config root", 2)),
    };
    if !root.is_absolute() {
        return Err(fail("XDG_CONFIG_HOME must be absolute", 2));
    }
    resolve(&root).map_err(|_| fail("cannot resolve XDG config root", 2))
}

/// Python's `config_root`.
pub fn config_root() -> Res<PathBuf> {
    config_root_from(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::home_dir().as_deref(),
    )
}

/// `factory_home` with its inputs given: the selected home (override or FACTORY_HOME), else the
/// location pointer under the config root, else `<config root>/factory`.
pub fn resolve_home(
    selected: Option<&OsStr>,
    xdg: Option<&OsStr>,
    user_home: Option<&Path>,
) -> Res<PathBuf> {
    if let Some(selected) = nonempty(selected) {
        let home = Path::new(selected);
        if !home.is_absolute() {
            return Err(fail(
                "factory home must be absolute. Nothing was written.",
                2,
            ));
        }
        return resolve(home)
            .map_err(|_| fail("cannot resolve factory home. Nothing was written.", 2));
    }
    let root = config_root_from(xdg, user_home)?;
    let pointer = root.join("factory-location.json");
    if fs::symlink_metadata(&pointer).is_err() {
        return Ok(root.join("factory"));
    }
    if let Some((cached, home)) = LOCATED.get() {
        if *cached == pointer {
            return Ok(home.clone());
        }
    }
    let home = read_location(&pointer)?;
    let _ = LOCATED.set((pointer, home.clone()));
    Ok(home)
}

/// Python's `factory_home`.
pub fn factory_home() -> Res<PathBuf> {
    let env = std::env::var_os("FACTORY_HOME");
    let selected = OVERRIDE.get().map(|p| p.as_os_str()).or(env.as_deref());
    resolve_home(
        selected,
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::home_dir().as_deref(),
    )
}

fn location(pointer: &Path) -> Option<PathBuf> {
    regular_metadata(pointer).ok()?;
    let value = pyjson::loads(&super::fs::read_text(pointer).ok()?).ok()?;
    let map = value.as_object()?;
    if map.len() != 2 || map.get("version")?.as_i64() != Some(1) {
        return None;
    }
    let text = map.get("home")?.as_str()?;
    let home = PathBuf::from(text);
    if !home.is_absolute() || resolve(&home).ok()?.as_os_str() != OsStr::new(text) {
        return None;
    }
    Some(home)
}

/// Python's `read_location`.
pub fn read_location(pointer: &Path) -> Res<PathBuf> {
    let home = location(pointer).ok_or_else(|| {
        fail(
            format!("Invalid location pointer: {}.", pointer.display()),
            2,
        )
    })?;
    validate_home(&home, false)?;
    Ok(home)
}

/// The skill folder that holds this binary: the grandparent of `<skill>/runtime/factory`.
pub fn skill_root() -> Res<PathBuf> {
    let exe = std::env::current_exe()
        .and_then(|p| resolve(&p))
        .map_err(|e| fail(os_error(&e, None), 1))?;
    Ok(exe
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("/"))
        .to_path_buf())
}

/// The installed bundle's bundle.json entry as an argv prefix under skill, or None when it is
/// missing or invalid: a nonempty list of strings whose first is a relative path without `..`.
pub fn bundle_entry(skill: &Path) -> Option<Vec<String>> {
    let entry = super::fs::read_text(&skill.join("bundle.json"))
        .ok()
        .and_then(|t| pyjson::loads(&t).ok())
        .and_then(|v| v.get("entry").cloned());
    let args: Vec<String> = entry
        .as_ref()
        .and_then(serde_json::Value::as_array)?
        .iter()
        .map(|v| v.as_str().map(String::from))
        .collect::<Option<_>>()?;
    let first = Path::new(args.first().filter(|a| !a.is_empty())?);
    if first.is_absolute()
        || first.has_root()
        || first
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return None;
    }
    let program: PathBuf = skill.join(first).components().collect();
    Some(
        std::iter::once(program.to_string_lossy().into_owned())
            .chain(args[1..].iter().cloned())
            .collect(),
    )
}

/// What `where` and prompts name as the runtime: the bundle's binary when the skill's bundle.json
/// has an entry, else this binary.
pub fn runtime_path() -> Res<PathBuf> {
    let exe = std::env::current_exe()
        .and_then(|p| resolve(&p))
        .map_err(|e| fail(os_error(&e, None), 1))?;
    if let Some(entry) = exe.parent().and_then(Path::parent).and_then(bundle_entry) {
        return Ok(PathBuf::from(&entry[0]));
    }
    Ok(exe)
}

fn valid_manifest(path: &Path) -> Option<()> {
    let manifest = pyjson::loads(&super::fs::read_text(path).ok()?).ok()?;
    for (rel, value) in manifest.as_object()? {
        let entry = value.as_object()?;
        let etag = entry.get("etag")?.as_str()?;
        let sha = entry.get("sha256")?.as_str()?;
        if !JOB_REL.is_full(rel)
            || rel.contains("..")
            || entry.len() != 2
            || etag.is_empty()
            || !SHA256.is_full(sha)
        {
            return None;
        }
    }
    Some(())
}

fn scan_tree(name: &str, top: &Path, current: &mut PathBuf) -> Option<()> {
    let mut stack = vec![top.to_path_buf()];
    while let Some(dir) = stack.pop() {
        *current = dir.clone();
        for entry in fs::read_dir(&dir).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            *current = path.clone();
            // The entry's own type is not followed through a link and needs no lstat on Unix.
            let meta = entry.file_type().ok()?;
            if meta.is_symlink() || (cfg!(windows) && is_link(&path).ok()?) {
                return None;
            }
            if meta.is_dir() {
                if !listable(&path) {
                    return None;
                }
                stack.push(path);
                continue;
            }
            if !meta.is_file() || !readable(&path) {
                return None;
            }
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if name == "projects" && file_name.ends_with(".sync.json") {
                read_project_state(&path).ok()?;
            } else if name == "jobs" && file_name == ".sync.json" {
                valid_manifest(&path)?;
            }
        }
    }
    Some(())
}

fn check_home(home: &Path, deep: bool, current: &mut PathBuf) -> Option<()> {
    if !home.is_absolute() || !home.is_dir() || !listable(home) {
        return None;
    }
    *current = home.join("config.md");
    closed_frontmatter(current).ok()?;
    *current = home.join("paths.md");
    if fs::symlink_metadata(&*current).is_ok() {
        let paths = closed_frontmatter(current).ok()?;
        if !paths
            .iter()
            .all(|(k, v)| SLUG.is_full(k) && Path::new(v).is_absolute())
        {
            return None;
        }
    }
    for name in ["projects", "jobs"] {
        *current = home.join(name);
        if fs::symlink_metadata(&*current).is_err() {
            continue;
        }
        if is_link(current).ok()? || !current.is_dir() {
            return None;
        }
        if !deep {
            continue;
        }
        let top = current.clone();
        scan_tree(name, &top, current)?;
    }
    Some(())
}

/// Python's `validate_home`. deep walks every file under projects/ and jobs/, which adopting a
/// home does; each command resolving the location pointer checks only the top level, since the
/// walk costs seconds on a large home.
pub fn validate_home(home: &Path, deep: bool) -> Res<()> {
    let mut current = home.to_path_buf();
    if check_home(home, deep, &mut current).is_some() {
        return Ok(());
    }
    let relative = match current.strip_prefix(home) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.display().to_string(),
        _ => ".".to_string(),
    };
    Err(fail(format!("Invalid local metadata: {relative}."), 2))
}

fn config_meta(home: &Path) -> Res<(PathBuf, Vec<(String, String)>)> {
    let config = home.join("config.md");
    if !config.exists() {
        return Ok((config, Vec::new()));
    }
    let meta = frontmatter(&config)
        .map_err(|e| fail(os_error(&e, Some(&config)), 1))?
        .0;
    Ok((config, meta))
}

/// config.md's `device:`, else the hostname.
pub fn device_name(home: &Path) -> String {
    let meta = config_meta(home).map(|(_, meta)| meta).unwrap_or_default();
    match fm_get(&meta, "device") {
        Some(device) if !device.is_empty() => device.to_string(),
        _ => hostname(),
    }
}

/// (device, session, claim_stale_minutes) this process claims jobs as.
pub fn holder(home: &Path) -> Res<(String, String, u32)> {
    let (config, meta) = config_meta(home)?;
    let session = std::env::var("FACTORY_SESSION").unwrap_or_default();
    if !session.is_empty() && !SLUG.is_full(&session) {
        return Err(fail(
            format!(
                "bad session tag {}: use letters, digits, - and _",
                repr(&session)
            ),
            2,
        ));
    }
    let minutes = fm_get(&meta, "claim_stale_minutes")
        .filter(|m| !m.is_empty())
        .unwrap_or("120");
    if !minutes.bytes().all(|b| b.is_ascii_digit()) {
        return Err(fail(
            format!(
                "claim_stale_minutes in {} must be a whole number of minutes, not {}",
                config.display(),
                repr(minutes)
            ),
            2,
        ));
    }
    Ok((
        device_name(home),
        session,
        minutes.parse().unwrap_or(u32::MAX),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;
    use serde_json::{json, Value};

    fn err(r: Res<PathBuf>) -> String {
        r.unwrap_err().stderr
    }

    #[test]
    fn bundle_entry_accepts_only_a_relative_path_inside_the_skill() {
        let skill =
            std::env::temp_dir().join(format!("factory-bundle-entry-{}", std::process::id()));
        fs::create_dir_all(&skill).unwrap();
        let write = |entry: Value| {
            fs::write(
                skill.join("bundle.json"),
                json!({"commit": "c", "entry": entry}).to_string(),
            )
            .unwrap()
        };
        assert_eq!(bundle_entry(&skill), None);
        for bad in [
            json!(["../factory"]),
            json!(["/usr/bin/factory"]),
            json!([]),
            json!([""]),
            json!([1]),
            json!(null),
        ] {
            write(bad.clone());
            assert_eq!(bundle_entry(&skill), None, "{bad}");
        }
        write(json!(["runtime/factory", "-q"]));
        assert_eq!(
            bundle_entry(&skill),
            Some(vec![
                skill.join("runtime/factory").to_string_lossy().into_owned(),
                "-q".into()
            ])
        );
        fs::remove_dir_all(&skill).unwrap();
    }

    #[test]
    fn runtime_is_this_binary_without_a_bundle_entry() {
        let exe = resolve(&std::env::current_exe().unwrap()).unwrap();
        assert_eq!(runtime_path().unwrap(), exe);
    }

    #[test]
    fn selected_home_must_be_absolute() {
        assert_eq!(
            err(resolve_home(Some(OsStr::new("relative")), None, None)),
            "factory.py: factory home must be absolute. Nothing was written.\n"
        );
        let dir = temp_dir("home-selected");
        assert_eq!(
            resolve_home(Some(dir.join("a/../b").as_os_str()), None, None).unwrap(),
            dir.join("b")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn config_root_comes_from_xdg_or_user_home() {
        let dir = temp_dir("home-root");
        assert_eq!(
            resolve_home(Some(OsStr::new("")), Some(dir.as_os_str()), None).unwrap(),
            dir.join("factory")
        );
        assert_eq!(
            resolve_home(None, None, Some(&dir)).unwrap(),
            dir.join(".config/factory")
        );
        assert_eq!(
            err(resolve_home(None, Some(OsStr::new("rel")), None)),
            "factory.py: XDG_CONFIG_HOME must be absolute\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pointer_must_name_a_valid_home() {
        let dir = temp_dir("home-pointer");
        let home = dir.join("home");
        fs::create_dir(&home).unwrap();
        fs::write(home.join("config.md"), "---\napi_url: https://x\n---\n").unwrap();
        let pointer = dir.join("factory-location.json");
        fs::write(
            &pointer,
            format!(
                "{{\"version\": 1, \"home\": {}}}",
                pyjson::dumps(&Value::from(home.to_str().unwrap()), pyjson::DEFAULT)
            ),
        )
        .unwrap();
        assert_eq!(
            resolve_home(None, Some(dir.as_os_str()), None).unwrap(),
            home
        );
        assert!(read_location(&pointer).is_ok());
        fs::write(&pointer, "{\"version\": 1.0, \"home\": \"/x\"}").unwrap();
        assert_eq!(
            read_location(&pointer).unwrap_err().stderr,
            format!(
                "factory.py: Invalid location pointer: {}.\n",
                pointer.display()
            )
        );
        fs::write(home.join("paths.md"), "---\nbad key: /x\n---\n").unwrap();
        assert_eq!(
            validate_home(&home, true).unwrap_err().stderr,
            "factory.py: Invalid local metadata: paths.md.\n"
        );
        fs::write(home.join("paths.md"), "---\nsample: /x\n---\n").unwrap();
        fs::create_dir_all(home.join("jobs/sample/001-x")).unwrap();
        fs::write(
            home.join("jobs/sample/001-x/.sync.json"),
            "{\"../x.md\": {\"etag\": \"e\", \"sha256\": \"\"}}",
        )
        .unwrap();
        assert_eq!(
            validate_home(&home, true).unwrap_err().stderr,
            "factory.py: Invalid local metadata: jobs/sample/001-x/.sync.json.\n"
        );
        // The pointer check reads only the top level, so the bad manifest below it passes.
        assert!(validate_home(&home, false).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn holder_checks_minutes() {
        let dir = temp_dir("home-holder");
        fs::write(
            dir.join("config.md"),
            "---\ndevice: box\nclaim_stale_minutes: soon\n---\n",
        )
        .unwrap();
        assert_eq!(
            holder(&dir).unwrap_err().stderr,
            format!(
                "factory.py: claim_stale_minutes in {}/config.md must be a whole number of minutes, not 'soon'\n",
                dir.display()
            )
        );
        fs::write(dir.join("config.md"), "---\ndevice: box\n---\n").unwrap();
        assert_eq!(device_name(&dir), "box");
        assert_eq!(holder(&dir).unwrap().2, 120);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Ported from tests/cli/test_location.py: a symlinked pointer, a symlinked metadata folder in the
    /// home and a FIFO under jobs/ are each refused with exit 2.
    #[cfg(unix)]
    #[test]
    fn pointer_and_metadata_links_and_a_fifo_are_refused() {
        let dir = temp_dir("home-links");
        let home = dir.join("home");
        fs::create_dir(&home).unwrap();
        fs::write(home.join("config.md"), "---\napi_url:\n---\nBody\n").unwrap();
        let data = dir.join("pointer-data");
        fs::write(
            &data,
            format!(
                "{{\"version\": 1, \"home\": {}}}",
                pyjson::dumps(&Value::from(home.to_str().unwrap()), pyjson::DEFAULT)
            ),
        )
        .unwrap();
        assert_eq!(read_location(&data).unwrap(), home);
        let pointer = dir.join("factory-location.json");
        std::os::unix::fs::symlink(&data, &pointer).unwrap();
        let e = read_location(&pointer).unwrap_err();
        assert_eq!(
            (e.code, e.stderr),
            (
                2,
                format!(
                    "factory.py: Invalid location pointer: {}.\n",
                    pointer.display()
                )
            )
        );
        std::os::unix::fs::symlink(&dir, home.join("projects")).unwrap();
        for deep in [false, true] {
            let e = validate_home(&home, deep).unwrap_err();
            assert_eq!(
                (e.code, e.stderr.as_str()),
                (2, "factory.py: Invalid local metadata: projects.\n")
            );
        }
        fs::remove_file(home.join("projects")).unwrap();
        let job = home.join("jobs/sample/001-test");
        fs::create_dir_all(&job).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(job.join("pipe"))
            .status()
            .unwrap()
            .success());
        let e = validate_home(&home, true).unwrap_err();
        assert_eq!(
            (e.code, e.stderr.as_str()),
            (
                2,
                "factory.py: Invalid local metadata: jobs/sample/001-test/pipe.\n"
            )
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
