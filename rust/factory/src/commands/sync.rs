//! Job and project sync with R2: push, pull and sync.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::cli::{not_ported, Call};
use crate::shared::consts::JOB_REL;
use crate::shared::exit::{fail, Exit, Res};
use crate::shared::fs::{is_link, read_text, write_atomic};
use crate::shared::http::api;
use crate::shared::project::{local_bytes, safe_job_folder, sha, strong};
use crate::shared::py::{os_error, printable_line, splitlines, strip};
use crate::shared::pyjson::{dumps, loads, INDENT2_SORTED};
use crate::shared::pyre::{Flags, Pattern};

pub(crate) type Manifest = serde_json::Map<String, Value>;

static GOAL_HEADING: Pattern = Pattern::new(r"^##\s+(?:\d+\.\s*)?Goal\b", Flags::I);
static HEADING: Pattern = Pattern::new(r"^##\s", Flags::NONE);
static PATH_LINE: Pattern = Pattern::new(r"\s*path\s*:", Flags::NONE);

/// Where Python would raise an `OSError`: fail 1 with its text.
fn os_fail(e: io::Error, p: &Path) -> Exit {
    fail(os_error(&e, Some(p)), 1)
}

/// `sorted(folder.rglob("*"))` as (rel with `/`, path), never entering links.
fn walk(folder: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    fn visit(
        dir: &Path,
        parts: &[String],
        found: &mut Vec<(Vec<String>, PathBuf)>,
    ) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let mut rel = parts.to_vec();
            rel.push(entry.file_name().to_string_lossy().into_owned());
            if entry.file_type()?.is_dir() {
                visit(&entry.path(), &rel, found)?;
            }
            found.push((rel, entry.path()));
        }
        Ok(())
    }
    let mut found = Vec::new();
    if folder.is_dir() {
        visit(folder, &[], &mut found)?;
    }
    found.sort();
    Ok(found
        .into_iter()
        .map(|(parts, p)| (parts.join("/"), p))
        .collect())
}

/// Python truthiness of a value that may be missing.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
    }
}

/// `f"{v}"` for a board field that may be missing.
fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// A project file minus its frontmatter path: line, since each device keeps its own.
fn without_path(text: &str) -> String {
    let lines = splitlines(text, true);
    if lines.is_empty() || strip(lines[0]) != "---" {
        return text.to_string();
    }
    match (1..lines.len()).find(|&i| strip(lines[i]) == "---") {
        Some(end) => {
            let head = lines[1..end]
                .iter()
                .filter(|l| PATH_LINE.match_start(l).is_none());
            std::iter::once(&lines[0])
                .chain(head)
                .chain(&lines[end..])
                .copied()
                .collect()
        }
        None => text.to_string(),
    }
}

/// The first paragraph under spec.md's ## Goal heading on one line, cut to 300 characters, or None.
fn spec_goal(text: &str) -> Option<String> {
    let lines = splitlines(text, false);
    let start = lines
        .iter()
        .position(|l| GOAL_HEADING.match_start(l).is_some())?;
    let end = (start + 1..lines.len())
        .find(|&i| HEADING.match_start(lines[i]).is_some())
        .unwrap_or(lines.len());
    let mut block = Vec::new();
    for line in &lines[start + 1..end] {
        let line = strip(line);
        if !line.is_empty() {
            block.push(line);
        } else if !block.is_empty() {
            break;
        }
    }
    Some(printable_line(&block.join(" "), 300)).filter(|g| !g.is_empty())
}

/// `<path>.remote`, where a conflicting R2 copy waits beside its file.
fn dot_remote(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".remote");
    s.into()
}

/// (folder, manifest, {rel: remote etag}) for push and pull. .sync.json records each
/// file's ETag and hash as this device last pushed or pulled it.
pub(crate) fn sync_state(
    home: &Path,
    slug: &str,
    job_id: &str,
) -> Res<(PathBuf, Manifest, BTreeMap<String, String>)> {
    let folder = safe_job_folder(home, slug, job_id)?;
    let path = folder.join(".sync.json");
    let manifest = if path.exists() {
        let text = read_text(&path).map_err(|e| os_fail(e, &path))?;
        match loads(&text).map_err(|e| fail(e, 1))? {
            Value::Object(m) => m,
            _ => return Err(fail(format!("{} is not a JSON object", path.display()), 1)),
        }
    } else {
        Manifest::new()
    };
    let listed = api(
        home,
        "GET",
        &format!("/api/jobs/{slug}/{job_id}/?etags=1"),
        None,
        &[200],
        &[],
    )?
    .text()?;
    let listed = loads(&listed).map_err(|e| fail(e, 1))?;
    let rows = match listed.as_array() {
        Some(rows) if rows.iter().all(Value::is_object) => rows,
        _ => {
            return Err(fail(
                "the Worker lists no ETags: redeploy the factory Worker",
                5,
            ))
        }
    };
    let mut remote = BTreeMap::new();
    for row in rows {
        let Some(name) = row.get("name").and_then(Value::as_str) else {
            return Err(fail("a job listing row has no name string", 1));
        };
        if JOB_REL.is_full(name) {
            let Some(etag) = row.get("etag").and_then(Value::as_str) else {
                return Err(fail(
                    format!("the job listing has no ETag string for {name}"),
                    1,
                ));
            };
            remote.insert(name.to_string(), strong(etag));
        }
    }
    Ok((folder, manifest, remote))
}

fn save_manifest(folder: &Path, manifest: &Manifest) -> Res<()> {
    fs::create_dir_all(folder).map_err(|e| os_fail(e, folder))?;
    let path = folder.join(".sync.json");
    let text = dumps(&Value::Object(manifest.clone()), INDENT2_SORTED) + "\n";
    write_atomic(&path, text.as_bytes()).map_err(|e| os_fail(e, &path))
}

/// (bytes, manifest entry) for one job file in R2.
fn fetch(home: &Path, slug: &str, job_id: &str, rel: &str) -> Res<(Vec<u8>, Value)> {
    let r = api(
        home,
        "GET",
        &format!("/api/jobs/{slug}/{job_id}/{rel}"),
        None,
        &[200],
        &[],
    )?;
    let Some(tag) = r.header("ETag").filter(|t| !t.is_empty()) else {
        return Err(fail(
            "the Worker returned no ETag: redeploy the factory Worker",
            5,
        ));
    };
    let entry = json!({"etag": strong(tag), "sha256": sha(&r.body)});
    Ok((r.body, entry))
}

/// rel changed both here and in R2. Adopt R2's version if it matches the local bytes.
/// Otherwise save it beside the file as <rel>.remote, which push never uploads, and
/// record its ETag, so a push after merging replaces exactly that version. True on a
/// conflict.
fn reconcile(
    home: &Path,
    slug: &str,
    job_id: &str,
    folder: &Path,
    manifest: &mut Manifest,
    rel: &str,
    data: Option<&[u8]>,
) -> Res<bool> {
    let (body, entry) = fetch(home, slug, job_id, rel)?;
    manifest.insert(rel.to_string(), entry);
    if data == Some(&body[..]) {
        return Ok(false);
    }
    let remote = dot_remote(&folder.join(rel));
    write_atomic(&remote, &body).map_err(|e| os_fail(e, &remote))?;
    Ok(true)
}

/// Why each local artifact is not byte-identical to what R2 has: symlinks or
/// junctions, local-only files, local edits, files missing from or newer in R2, and
/// remote files with no copy here, as "<file> <reason>" strings.
pub(crate) fn sync_problems(
    folder: &Path,
    manifest: &Manifest,
    remote: &BTreeMap<String, String>,
) -> Res<Vec<String>> {
    let mut problems = Vec::new();
    let mut local = BTreeMap::new();
    for (rel, p) in walk(folder).map_err(|e| os_fail(e, folder))? {
        if is_link(&p).map_err(|e| os_fail(e, &p))? {
            problems.push(format!("{rel} is a symlink or junction"));
        } else if p.is_file() && JOB_REL.is_full(&rel) {
            local.insert(rel, p);
        }
    }
    for (rel, path) in &local {
        let Some(entry) = manifest.get(rel).filter(|e| truthy(Some(e))) else {
            problems.push(format!(
                "{rel} is local-only; the sync manifest has no entry for it"
            ));
            continue;
        };
        let stored = |key: &str| entry.get(key).filter(|v| !v.is_null());
        if local_bytes(path).is_none_or(|data| stored("sha256") != Some(&Value::from(sha(&data)))) {
            problems.push(format!("{rel} differs from the last sync with R2"));
        } else if remote.get(rel).map(|t| Value::from(t.as_str())).as_ref() != stored("etag") {
            problems.push(format!(
                "{rel} is missing from R2 or has a newer ETag there"
            ));
        }
    }
    for rel in remote.keys().filter(|rel| !local.contains_key(*rel)) {
        problems.push(format!("{rel} has no local copy"));
    }
    Ok(problems)
}

pub(crate) fn conflict_exit(conflicts: &[String]) -> Exit {
    fail(
        format!(
            "conflict in {}: changed both here and in R2. Local copies were kept and nothing was overwritten. R2's copy of each is saved beside it as <file>.remote. Merge it into the local file and delete the .remote, then push. For an image, keep one copy: delete the .remote, or move it over the image",
            conflicts.join(", ")
        ),
        6,
    )
}

pub fn push(_call: &Call) -> Res<()> {
    Err(not_ported("push"))
}

pub fn pull(_call: &Call) -> Res<()> {
    Err(not_ported("pull"))
}

pub fn sync(_call: &Call) -> Res<()> {
    Err(not_ported("sync"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;
    use crate::shared::http::tests::home_for;

    #[test]
    fn walk_sorts_by_component() {
        let dir = temp_dir("sync-walk");
        for rel in ["a.md", "Z.md", "round-1.md", "round-1/x.md"] {
            fs::create_dir_all(dir.join(rel).parent().unwrap()).unwrap();
            fs::write(dir.join(rel), "x").unwrap();
        }
        let rels: Vec<String> = walk(&dir)
            .unwrap()
            .into_iter()
            .map(|(rel, _)| rel)
            .collect();
        assert_eq!(
            rels,
            ["Z.md", "a.md", "round-1", "round-1/x.md", "round-1.md"]
        );
        assert!(walk(&dir.join("missing")).unwrap().is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn spec_goal_matches_python() {
        assert_eq!(spec_goal("## Goals\n\nx\n"), None);
        assert_eq!(
            spec_goal("## 1. goal\n\n  A\nb  \n\nc\n").as_deref(),
            Some("A b")
        );
        assert_eq!(
            spec_goal(&format!("## Goal\n{}\n", "é".repeat(400)))
                .unwrap()
                .chars()
                .count(),
            300
        );
        assert_eq!(spec_goal("## Goal\n\n## Scope\n"), None);
    }

    #[test]
    fn without_path_matches_python() {
        assert_eq!(
            without_path("---\r\nrepo: a\r\n  Path : x\r\npath: y\r\n---\r\npath: z\r\n"),
            "---\r\nrepo: a\r\n  Path : x\r\n---\r\npath: z\r\n"
        );
        assert_eq!(without_path("---\npath: x\n"), "---\npath: x\n");
        assert_eq!(
            without_path("x\n---\npath: y\n---\n"),
            "x\n---\npath: y\n---\n"
        );
    }

    #[test]
    fn truthy_and_py_str() {
        for falsy in [
            json!(null),
            json!(false),
            json!(0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            assert!(!truthy(Some(&falsy)));
        }
        assert!(!truthy(None));
        assert!(truthy(Some(&json!({"etag": "\"x\""}))) && truthy(Some(&json!(1))));
        assert_eq!(py_str(None), "None");
        assert_eq!(py_str(Some(&json!(true))), "True");
        assert_eq!(py_str(Some(&json!("sample"))), "sample");
    }

    #[test]
    fn sync_problems_reports_each_reason() {
        let dir = temp_dir("sync-problems");
        for rel in ["a.md", "b.md", "c.md"] {
            fs::write(dir.join(rel), rel).unwrap();
        }
        let mut manifest = Manifest::new();
        manifest.insert("a.md".into(), json!({}));
        manifest.insert(
            "b.md".into(),
            json!({"etag": "\"b1\"", "sha256": sha(b"old")}),
        );
        manifest.insert(
            "c.md".into(),
            json!({"etag": "\"c1\"", "sha256": sha(b"c.md")}),
        );
        let remote: BTreeMap<String, String> =
            [("b.md", "\"b1\""), ("c.md", "\"c2\""), ("d.md", "\"d1\"")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into();
        let mut want = vec![
            "a.md is local-only; the sync manifest has no entry for it",
            "b.md differs from the last sync with R2",
            "c.md is missing from R2 or has a newer ETag there",
            "d.md has no local copy",
        ];
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/", dir.join("link.md")).unwrap();
            want.insert(0, "link.md is a symlink or junction");
        }
        assert_eq!(sync_problems(&dir, &manifest, &remote).unwrap(), want);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn sync_state_refuses_symlinked_job_folder() {
        let home = home_for("sync-link", "http://127.0.0.1:9");
        fs::create_dir_all(home.join("jobs/sample")).unwrap();
        fs::create_dir_all(home.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(home.join("elsewhere"), home.join("jobs/sample/001-test"))
            .unwrap();
        let e = sync_state(&home, "sample", "001-test").unwrap_err();
        assert_eq!(e.code, 6);
        assert!(e.stderr.contains(
            "001-test is a symlink or junction: factory job paths must be real directories"
        ));
        fs::remove_dir_all(home).unwrap();
    }
}
