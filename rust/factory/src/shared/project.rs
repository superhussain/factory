//! Project documents, their sync state, and job folder guards.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::conf::frontmatter;
use super::consts::{JOB_ID, SLUG};
use super::exit::{fail, Res};
use super::fs::{is_link, listable, read_text, readable, write_atomic};
use super::home::factory_home;
use super::http::api;
use super::py::{os_error, repr};
use super::pyjson::{dumps, loads, DEFAULT};
use super::pyre::{Flags, Pattern};

static PROJECT_SLUG: Pattern = Pattern::new("[A-Za-z0-9_-]+", Flags::NONE);
static PROJECT_ETAG: Pattern = Pattern::new(r#""[\x21\x23-\x7e\x80-\xff]+""#, Flags::NONE);
static SHA256: Pattern = Pattern::new("[0-9a-f]{64}", Flags::NONE);

/// (home, slug, job id or "") from `<slug>[/<id>]`, for push and pull.
pub fn r2_target(target: &str, need_job: bool) -> Res<(PathBuf, String, String)> {
    let home = factory_home()?;
    let (slug, job_id) = target.split_once('/').unwrap_or((target, ""));
    let bad_job = (!job_id.is_empty() || need_job) && !(JOB_ID.is_full(job_id) && !job_id.contains(".."));
    if !SLUG.is_full(slug) || bad_job {
        let tail = if need_job { "" } else { " or <project-slug>" };
        return Err(fail(format!("bad target {}: use <project-slug>/<job-id>{tail}", repr(target)), 2));
    }
    Ok((home, slug.to_string(), job_id.to_string()))
}

fn inspect_project_paths(folder: &Path, files: &[&PathBuf; 3]) -> Result<Res<()>, (io::Error, PathBuf)> {
    let nodes = std::iter::once(folder).chain(files.iter().map(|p| p.as_path()));
    for node in nodes {
        if is_link(node).map_err(|e| (e, node.to_path_buf()))? {
            return Ok(Err(fail("project paths must not be symlinks or junctions", 6)));
        }
        let meta = match fs::metadata(node) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err((e, node.to_path_buf())),
        };
        let ok = if node == folder { meta.is_dir() && listable(node) } else { meta.is_file() && readable(node) };
        if !ok {
            return Ok(Err(fail("project paths must be readable regular files in a real directory", 6)));
        }
    }
    fs::create_dir_all(folder).map_err(|e| (e, folder.to_path_buf()))?;
    fs::read_dir(folder).map_err(|e| (e, folder.to_path_buf()))?.for_each(drop);
    Ok(Ok(()))
}

/// (`<slug>.md`, `<slug>.sync.json`, `<slug>.md.remote`) under `projects/`, refusing links.
pub fn project_files(home: &Path, slug: &str) -> Res<(PathBuf, PathBuf, PathBuf)> {
    if !PROJECT_SLUG.is_full(slug) {
        return Err(fail("invalid project slug", 6));
    }
    let folder = home.join("projects");
    let files = (folder.join(format!("{slug}.md")), folder.join(format!("{slug}.sync.json")), folder.join(format!("{slug}.md.remote")));
    match inspect_project_paths(&folder, &[&files.0, &files.1, &files.2]) {
        Ok(checked) => checked?,
        Err((e, path)) => return Err(fail(format!("cannot inspect project paths: {}", os_error(&e, Some(&path))), 6)),
    }
    Ok(files)
}

/// An ETag without the W/ that Cloudflare's edge adds when it compresses a response.
pub fn strong(tag: &str) -> String {
    tag.strip_prefix("W/").unwrap_or(tag).to_string()
}

pub fn project_etag(tag: &str) -> Option<String> {
    let tag = strong(tag);
    PROJECT_ETAG.is_full(&tag).then_some(tag)
}

pub fn sha(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// Raw bytes of a regular file, or None.
pub fn local_bytes(p: &Path) -> Option<Vec<u8>> {
    if p.is_file() {
        fs::read(p).ok()
    } else {
        None
    }
}

fn valid_state(state: &Value) -> bool {
    let Some(map) = state.as_object() else { return false };
    let etag = map.get("etag").and_then(Value::as_str);
    let digest = map.get("sha256").and_then(Value::as_str);
    map.len() == 3
        && map.get("version").and_then(Value::as_i64) == Some(1)
        && etag.is_some_and(|t| project_etag(t).as_deref() == Some(t))
        && digest.is_some_and(|d| SHA256.is_full(d))
}

pub fn read_project_state(p: &Path) -> Res<Option<Value>> {
    if !p.exists() {
        return Ok(None);
    }
    let text = read_text(p).map_err(|e| fail(format!("cannot read project sync state: {}", os_error(&e, Some(p))), 6))?;
    let state = loads(&text).map_err(|e| fail(format!("cannot read project sync state: {e}"), 6))?;
    if !valid_state(&state) {
        return Err(fail("invalid project sync state", 6));
    }
    Ok(Some(state))
}

pub fn save_project_state(p: &Path, etag: Option<&str>, data: &[u8]) -> Res<()> {
    let Some(tag) = project_etag(etag.unwrap_or("")) else {
        return Err(fail("the Worker returned no valid project ETag: redeploy the factory Worker", 5));
    };
    let text = dumps(&json!({"version": 1, "etag": tag, "sha256": sha(data)}), DEFAULT) + "\n";
    write_atomic(p, text.as_bytes()).map_err(|e| fail(format!("cannot save project sync state: {}", os_error(&e, Some(p))), 6))
}

fn write_or_fail(p: &Path, data: &[u8]) -> Res<()> {
    write_atomic(p, data).map_err(|e| fail(format!("cannot pull local project: {}", os_error(&e, Some(p))), 6))
}

/// Python's `pull_project`: true when the project has local edits that R2 has not moved under.
pub fn pull_project(home: &Path, slug: &str) -> Res<bool> {
    let (document, state_path, remote) = project_files(home, slug)?;
    let state = read_project_state(&state_path)?;
    let original = local_bytes(&document);
    let r = api(home, "GET", &format!("/api/projects/{slug}.md"), None, &[200], &[])?;
    let data = r.body.clone();
    if std::str::from_utf8(&data).is_err() {
        return Err(fail("remote project is not valid UTF-8; local edits were kept", 5));
    }
    let Some(tag) = project_etag(r.header("ETag").unwrap_or("")) else {
        return Err(fail("the Worker returned no valid project ETag; local edits were kept", 5));
    };
    project_files(home, slug)?;
    if local_bytes(&document) != original {
        return Err(fail("Project changed locally during pull. Local edits were kept.", 6));
    }
    let state_field = |key: &str| state.as_ref().and_then(|s| s.get(key)?.as_str().map(String::from));
    if remote.exists() {
        write_or_fail(&remote, &data)?;
        save_project_state(&state_path, Some(&tag), &data)?;
    } else if original.is_none()
        || original.as_deref() == Some(&data[..])
        || original.as_deref().is_some_and(|o| state_field("sha256") == Some(sha(o)))
    {
        write_or_fail(&document, &data)?;
        save_project_state(&state_path, Some(&tag), &data)?;
        return Ok(false);
    } else if state_field("etag") == Some(tag.clone()) {
        return Ok(true);
    } else {
        write_or_fail(&remote, &data)?;
        save_project_state(&state_path, Some(&tag), &data)?;
    }
    Err(fail(
        format!("Project changed in R2. Local edits were kept. Compare {slug}.md.remote, merge into {slug}.md, delete the .remote file, then push {slug}."),
        6,
    ))
}

/// This device's checkout of each project, from paths.md.
pub fn read_paths(home: &Path) -> Vec<(String, String)> {
    let path = home.join("paths.md");
    if !path.exists() {
        return Vec::new();
    }
    frontmatter(&path).map(|(meta, _)| meta).unwrap_or_default()
}

fn link_inside(link: &Path) -> crate::shared::exit::Exit {
    fail(
        format!(
            "{} is a symlink or junction inside the job folder. Inspect and remove the link yourself; nothing was read through it or changed",
            link.display()
        ),
        6,
    )
}

/// os.walk(top) with a link check on every entry, top-down. An error carries its path.
fn walk(folder: &Path, dir: &Path) -> Result<Res<()>, (io::Error, PathBuf)> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound && dir == folder && !folder.exists() => return Ok(Ok(())),
        Err(e) => return Err((e, dir.to_path_buf())),
    };
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    for entry in entries {
        let path = entry.map_err(|e| (e, dir.to_path_buf()))?.path();
        if path.is_dir() {
            dirs.push(path);
        } else {
            files.push(path);
        }
    }
    for path in dirs.iter().chain(&files) {
        if is_link(path).map_err(|e| (e, path.clone()))? {
            return Ok(Err(link_inside(path)));
        }
    }
    for sub in &dirs {
        if let Err(e) = walk(folder, sub)? {
            return Ok(Err(e));
        }
    }
    Ok(Ok(()))
}

/// The job folder for artifact I/O, refusing a job whose paths include links or cannot be listed.
pub fn safe_job_folder(home: &Path, slug: &str, job_id: &str) -> Res<PathBuf> {
    let folder = home.join("jobs").join(slug).join(job_id);
    let inspect = || -> Result<Res<()>, (io::Error, PathBuf)> {
        for node in [home.join("jobs"), home.join("jobs").join(slug), folder.clone()] {
            if is_link(&node).map_err(|e| (e, node.clone()))? {
                return Ok(Err(fail(
                    format!(
                        "{} is a symlink or junction: factory job paths must be real directories. Inspect and remove the link yourself; nothing was read through it or changed",
                        node.display()
                    ),
                    6,
                )));
            }
        }
        walk(&folder, &folder)
    };
    match inspect() {
        Ok(checked) => checked.map(|_| folder),
        Err((e, path)) => Err(fail(
            format!(
                "cannot inspect job paths under {}: {}. Fix that first; nothing was read through it or changed",
                folder.display(),
                os_error(&e, Some(&path))
            ),
            6,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;
    use crate::shared::http::tests::{home_for, serve};

    #[test]
    fn etags_and_hashes() {
        assert_eq!(strong("W/\"x\""), "\"x\"");
        assert_eq!(project_etag("W/\"abc\"").as_deref(), Some("\"abc\""));
        assert_eq!(project_etag("\"a b\""), None);
        assert_eq!(project_etag(""), None);
        assert_eq!(sha(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn pull_writes_document_and_state() {
        let (origin, _) = serve(vec![(200, "---\nrepo: octo/sample\n---\n", vec![("ETag", "\"e1\"")])]);
        let home = home_for("project-pull", &origin);
        assert!(!pull_project(&home, "sample").unwrap());
        assert_eq!(fs::read_to_string(home.join("projects/sample.md")).unwrap(), "---\nrepo: octo/sample\n---\n");
        let state = read_project_state(&home.join("projects/sample.sync.json")).unwrap().unwrap();
        assert_eq!(state["etag"], "\"e1\"");
        assert_eq!(
            fs::read_to_string(home.join("projects/sample.sync.json")).unwrap(),
            format!("{{\"version\": 1, \"etag\": \"\\\"e1\\\"\", \"sha256\": \"{}\"}}\n", sha(b"---\nrepo: octo/sample\n---\n"))
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn job_folder_refuses_links() {
        let home = temp_dir("project-jobs");
        assert_eq!(safe_job_folder(&home, "s", "1-a").unwrap(), home.join("jobs/s/1-a"));
        fs::create_dir_all(home.join("jobs/s/1-a/round-1")).unwrap();
        std::os::unix::fs::symlink("/", home.join("jobs/s/1-a/round-1/escape")).unwrap();
        let e = safe_job_folder(&home, "s", "1-a").unwrap_err();
        assert_eq!(e.code, 6);
        assert!(e.stderr.contains("round-1/escape is a symlink or junction inside the job folder"));
        assert_eq!(project_files(&home, "bad slug").unwrap_err().stderr, "factory.py: invalid project slug\n");
        fs::remove_dir_all(home).unwrap();
    }
}
