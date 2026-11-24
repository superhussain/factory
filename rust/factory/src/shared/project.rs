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
    let bad_job = (!job_id.is_empty() || need_job) && bad_job_id(job_id, cfg!(windows));
    if !SLUG.is_full(slug) || bad_job {
        let tail = if need_job { "" } else { " or <project-slug>" };
        return Err(fail(
            format!(
                "bad target {}: use <project-slug>/<job-id>{tail}",
                repr(target)
            ),
            2,
        ));
    }
    Ok((home, slug.to_string(), job_id.to_string()))
}

/// Windows drops a trailing dot from a path name, so there `a.` would name the folder `a`.
fn bad_job_id(job_id: &str, windows: bool) -> bool {
    !JOB_ID.is_full(job_id) || job_id.contains("..") || windows && job_id.ends_with('.')
}

fn inspect_project_paths(
    folder: &Path,
    files: &[&PathBuf; 3],
) -> Result<Res<()>, (io::Error, PathBuf)> {
    let nodes = std::iter::once(folder).chain(files.iter().map(|p| p.as_path()));
    for node in nodes {
        if is_link(node).map_err(|e| (e, node.to_path_buf()))? {
            return Ok(Err(fail(
                "project paths must not be symlinks or junctions",
                6,
            )));
        }
        let meta = match fs::metadata(node) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err((e, node.to_path_buf())),
        };
        let ok = if node == folder {
            meta.is_dir() && listable(node)
        } else {
            meta.is_file() && readable(node)
        };
        if !ok {
            return Ok(Err(fail(
                "project paths must be readable regular files in a real directory",
                6,
            )));
        }
    }
    fs::create_dir_all(folder).map_err(|e| (e, folder.to_path_buf()))?;
    fs::read_dir(folder)
        .map_err(|e| (e, folder.to_path_buf()))?
        .for_each(drop);
    Ok(Ok(()))
}

/// (`<slug>.md`, `<slug>.sync.json`, `<slug>.md.remote`) under `projects/`, refusing links.
pub fn project_files(home: &Path, slug: &str) -> Res<(PathBuf, PathBuf, PathBuf)> {
    if !PROJECT_SLUG.is_full(slug) {
        return Err(fail("invalid project slug", 6));
    }
    let folder = home.join("projects");
    let files = (
        folder.join(format!("{slug}.md")),
        folder.join(format!("{slug}.sync.json")),
        folder.join(format!("{slug}.md.remote")),
    );
    match inspect_project_paths(&folder, &[&files.0, &files.1, &files.2]) {
        Ok(checked) => checked?,
        Err((e, path)) => {
            return Err(fail(
                format!(
                    "cannot inspect project paths: {}",
                    os_error(&e, Some(&path))
                ),
                6,
            ))
        }
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
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
    let Some(map) = state.as_object() else {
        return false;
    };
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
    let text = read_text(p).map_err(|e| {
        fail(
            format!("cannot read project sync state: {}", os_error(&e, Some(p))),
            6,
        )
    })?;
    let state =
        loads(&text).map_err(|e| fail(format!("cannot read project sync state: {e}"), 6))?;
    if !valid_state(&state) {
        return Err(fail("invalid project sync state", 6));
    }
    Ok(Some(state))
}

pub fn save_project_state(p: &Path, etag: Option<&str>, data: &[u8]) -> Res<()> {
    let Some(tag) = project_etag(etag.unwrap_or("")) else {
        return Err(fail(
            "the Worker returned no valid project ETag: redeploy the factory Worker",
            5,
        ));
    };
    let text = dumps(
        &json!({"version": 1, "etag": tag, "sha256": sha(data)}),
        DEFAULT,
    ) + "\n";
    write_atomic(p, text.as_bytes()).map_err(|e| {
        fail(
            format!("cannot save project sync state: {}", os_error(&e, Some(p))),
            6,
        )
    })
}

fn write_or_fail(p: &Path, data: &[u8]) -> Res<()> {
    write_atomic(p, data).map_err(|e| {
        fail(
            format!("cannot pull local project: {}", os_error(&e, Some(p))),
            6,
        )
    })
}

/// Python's `pull_project`: true when the project has local edits that R2 has not moved under.
pub fn pull_project(home: &Path, slug: &str) -> Res<bool> {
    let (document, state_path, remote) = project_files(home, slug)?;
    let state = read_project_state(&state_path)?;
    let original = local_bytes(&document);
    let r = api(
        home,
        "GET",
        &format!("/api/projects/{slug}.md"),
        None,
        &[200],
        &[],
    )?;
    let data = r.body.clone();
    if std::str::from_utf8(&data).is_err() {
        return Err(fail(
            "remote project is not valid UTF-8; local edits were kept",
            5,
        ));
    }
    let Some(tag) = project_etag(r.header("ETag").unwrap_or("")) else {
        return Err(fail(
            "the Worker returned no valid project ETag; local edits were kept",
            5,
        ));
    };
    project_files(home, slug)?;
    if local_bytes(&document) != original {
        return Err(fail(
            "Project changed locally during pull. Local edits were kept.",
            6,
        ));
    }
    let state_field = |key: &str| {
        state
            .as_ref()
            .and_then(|s| s.get(key)?.as_str().map(String::from))
    };
    if remote.exists() {
        write_or_fail(&remote, &data)?;
        save_project_state(&state_path, Some(&tag), &data)?;
    } else if original.is_none()
        || original.as_deref() == Some(&data[..])
        || original
            .as_deref()
            .is_some_and(|o| state_field("sha256") == Some(sha(o)))
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
/// A read or decode error fails 1, as Python raises, so no caller rewrites paths.md from an empty list.
pub fn read_paths(home: &Path) -> Res<Vec<(String, String)>> {
    let path = home.join("paths.md");
    if !path.exists() {
        return Ok(Vec::new());
    }
    frontmatter(&path)
        .map(|(meta, _)| meta)
        .map_err(|e| fail(os_error(&e, Some(&path)), 1))
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
        Err(e) if e.kind() == io::ErrorKind::NotFound && dir == folder && !folder.exists() => {
            return Ok(Ok(()))
        }
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
        for node in [
            home.join("jobs"),
            home.join("jobs").join(slug),
            folder.clone(),
        ] {
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
    use crate::shared::http::tests::{edit_during, home_for, serve, serve_hooked};
    use std::sync::{Arc, Mutex};

    #[test]
    fn trailing_dot_refused_on_windows() {
        assert!(!bad_job_id("001-a.", false));
        assert!(bad_job_id("001-a.", true));
        assert!(!bad_job_id("001-a.b", true));
    }

    #[test]
    fn etags_and_hashes() {
        assert_eq!(strong("W/\"x\""), "\"x\"");
        assert_eq!(project_etag("W/\"abc\"").as_deref(), Some("\"abc\""));
        assert_eq!(project_etag("\"a b\""), None);
        assert_eq!(project_etag(""), None);
        assert_eq!(
            sha(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn pull_writes_document_and_state() {
        let (origin, _) = serve(vec![(
            200,
            "---\nrepo: octo/sample\n---\n",
            vec![("ETag", "\"e1\"")],
        )]);
        let home = home_for("project-pull", &origin);
        assert!(!pull_project(&home, "sample").unwrap());
        assert_eq!(
            fs::read_to_string(home.join("projects/sample.md")).unwrap(),
            "---\nrepo: octo/sample\n---\n"
        );
        let state = read_project_state(&home.join("projects/sample.sync.json"))
            .unwrap()
            .unwrap();
        assert_eq!(state["etag"], "\"e1\"");
        assert_eq!(
            fs::read_to_string(home.join("projects/sample.sync.json")).unwrap(),
            format!(
                "{{\"version\": 1, \"etag\": \"\\\"e1\\\"\", \"sha256\": \"{}\"}}\n",
                sha(b"---\nrepo: octo/sample\n---\n")
            )
        );
        fs::remove_dir_all(home).unwrap();
    }

    /// A local edit that lands while the GET is in flight is kept: pull exits 6 and rewrites nothing.
    #[test]
    fn pull_keeps_an_edit_made_during_the_get() {
        let draft = "---\nrepo: octo/sample\n---\nBodyTyping\n";
        let slot = Arc::new(Mutex::new(PathBuf::new()));
        let (origin, _) = serve_hooked(
            vec![
                (
                    200,
                    "---\nrepo: octo/sample\n---\nBody\n",
                    vec![("ETag", "\"e1\"")],
                ),
                (
                    200,
                    "---\nrepo: octo/sample\n---\nBodyRemote\n",
                    vec![("ETag", "\"e2\"")],
                ),
            ],
            vec![
                None,
                edit_during(&slot, "projects/sample.md", draft.to_string()),
            ],
        );
        let home = home_for("project-pull-edit", &origin);
        *slot.lock().unwrap() = home.clone();
        assert!(!pull_project(&home, "sample").unwrap());
        let state = home.join("projects/sample.sync.json");
        let before = fs::read(&state).unwrap();
        let e = pull_project(&home, "sample").unwrap_err();
        assert_eq!(e.code, 6);
        assert!(
            e.stderr
                .contains("Project changed locally during pull. Local edits were kept."),
            "{}",
            e.stderr
        );
        assert_eq!(
            fs::read_to_string(home.join("projects/sample.md")).unwrap(),
            draft
        );
        assert_eq!(fs::read(&state).unwrap(), before);
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn job_folder_refuses_links() {
        let home = temp_dir("project-jobs");
        assert_eq!(
            safe_job_folder(&home, "s", "1-a").unwrap(),
            home.join("jobs/s/1-a")
        );
        fs::create_dir_all(home.join("jobs/s/1-a/round-1")).unwrap();
        std::os::unix::fs::symlink("/", home.join("jobs/s/1-a/round-1/escape")).unwrap();
        let e = safe_job_folder(&home, "s", "1-a").unwrap_err();
        assert_eq!(e.code, 6);
        assert!(e
            .stderr
            .contains("round-1/escape is a symlink or junction inside the job folder"));
        assert_eq!(
            project_files(&home, "bad slug").unwrap_err().stderr,
            "factory.py: invalid project slug\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    /// project_files refuses a linked document, a directory as document, a linked state file, an
    /// unreadable document and a linked projects folder, each with exit 6.
    #[cfg(unix)]
    #[test]
    fn project_files_refuses_links_nonregular_and_unreadable_paths() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let home = temp_dir("project-files-refusals");
        let folder = home.join("projects");
        let (document, state, _) = project_files(&home, "sample").unwrap();
        let refused = |want: &str| {
            let e = project_files(&home, "sample").unwrap_err();
            assert_eq!(e.code, 6, "{}", e.stderr);
            assert!(e.stderr.contains(want), "{}", e.stderr);
        };
        let links = "project paths must not be symlinks or junctions";
        let regular = "project paths must be readable regular files in a real directory";

        symlink(home.join("outside"), &document).unwrap();
        refused(links);
        fs::remove_file(&document).unwrap();

        fs::create_dir(&document).unwrap();
        refused(regular);
        fs::remove_dir(&document).unwrap();

        symlink(home.join("outside"), &state).unwrap();
        refused(links);
        fs::remove_file(&state).unwrap();

        fs::write(&document, "draft").unwrap();
        fs::set_permissions(&document, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::File::open(&document).is_ok();
        let result = project_files(&home, "sample");
        fs::set_permissions(&document, fs::Permissions::from_mode(0o644)).unwrap();
        if !readable {
            let e = result.unwrap_err();
            assert_eq!(e.code, 6);
            assert!(e.stderr.contains(regular), "{}", e.stderr);
        }
        fs::remove_file(&document).unwrap();

        fs::remove_dir(&folder).unwrap();
        symlink(&home, &folder).unwrap();
        refused(links);
        fs::remove_file(&folder).unwrap();
        fs::remove_dir_all(home).unwrap();
    }

    /// A factory home reached through a symlink alias is trusted: only jobs, jobs/<slug> and the job
    /// folder are checked for links, so a link above the home does not refuse the job.
    #[cfg(unix)]
    #[test]
    fn job_folder_trusts_a_symlinked_factory_home() {
        let home = temp_dir("project-jobs-alias");
        fs::create_dir_all(home.join("jobs/s/1-a/round-1")).unwrap();
        let alias = temp_dir("project-jobs-alias-link").join("alias");
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        assert_eq!(
            safe_job_folder(&alias, "s", "1-a").unwrap(),
            alias.join("jobs/s/1-a")
        );
        fs::remove_file(&alias).unwrap();
        fs::remove_dir_all(alias.parent().unwrap()).unwrap();
        fs::remove_dir_all(home).unwrap();
    }

    /// Ported from tests/cli/test_factory.py's released-hold cases: hold refuses through this guard
    /// when the project folder, the sync manifest or a conflict copy is a link, or a round folder
    /// cannot be listed, and leaves the link and its target alone.
    #[cfg(unix)]
    #[test]
    fn job_folder_refuses_project_manifest_and_conflict_links_and_unlistable_rounds() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let home = temp_dir("project-jobs-links");
        let folder = home.join("jobs/s/1-a");
        fs::create_dir_all(folder.join("round-1")).unwrap();
        fs::write(folder.join("round-1/review-code.md"), "VERDICT: PASS\n").unwrap();
        let refused = |want: &str| {
            let e = safe_job_folder(&home, "s", "1-a").unwrap_err();
            assert_eq!(e.code, 6, "{}", e.stderr);
            assert!(e.stderr.contains(want), "{}", e.stderr);
        };

        fs::rename(home.join("jobs/s"), home.join("real-s")).unwrap();
        symlink(home.join("real-s"), home.join("jobs/s")).unwrap();
        refused("jobs/s is a symlink or junction: factory job paths must be real directories");
        assert!(home.join("jobs/s").is_symlink());
        fs::remove_file(home.join("jobs/s")).unwrap();
        fs::rename(home.join("real-s"), home.join("jobs/s")).unwrap();

        fs::write(home.join("outside-manifest.json"), "{}\n").unwrap();
        symlink(
            home.join("outside-manifest.json"),
            folder.join(".sync.json"),
        )
        .unwrap();
        refused("1-a/.sync.json is a symlink or junction inside the job folder");
        assert!(folder.join(".sync.json").is_symlink());
        fs::remove_file(folder.join(".sync.json")).unwrap();

        let conflict = folder.join("round-1/review-code.md.remote");
        symlink(home.join("no-such.remote"), &conflict).unwrap();
        refused("review-code.md.remote is a symlink or junction inside the job folder");
        assert!(conflict.is_symlink());
        fs::remove_file(&conflict).unwrap();
        assert!(safe_job_folder(&home, "s", "1-a").is_ok());

        // A traversal-only round folder lists as empty, so it must fail rather than read as no artifacts.
        let round = folder.join("round-1");
        fs::set_permissions(&round, fs::Permissions::from_mode(0o111)).unwrap();
        let listable = fs::read_dir(&round).is_ok();
        let result = safe_job_folder(&home, "s", "1-a");
        fs::set_permissions(&round, fs::Permissions::from_mode(0o755)).unwrap();
        if !listable {
            let e = result.unwrap_err();
            assert_eq!(e.code, 6);
            assert!(
                e.stderr.contains("cannot inspect job paths"),
                "{}",
                e.stderr
            );
        }
        assert_eq!(
            fs::read_to_string(round.join("review-code.md")).unwrap(),
            "VERDICT: PASS\n"
        );

        // A write-and-execute-only job folder lists as empty, so a link hidden in it must fail the
        // guard rather than let a pull write through it.
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o333)).unwrap();
        let listable = fs::read_dir(&folder).is_ok();
        let result = safe_job_folder(&home, "s", "1-a");
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        if !listable {
            let e = result.unwrap_err();
            assert_eq!(e.code, 6);
            assert!(
                e.stderr.contains("cannot inspect job paths"),
                "{}",
                e.stderr
            );
        }
        fs::remove_dir_all(home).unwrap();
    }
}
