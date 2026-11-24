//! Job and project sync with R2: push, pull and sync.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::cli::Call;
use crate::commands::pipeline::decision_verdict;
use crate::shared::board::{dump_board, load_board, read_board};
use crate::shared::consts::{IDEA, JOB_REL, TERMINAL};
use crate::shared::exit::{fail, Exit, Res};
use crate::shared::fs::{is_link, read_text, write_atomic};
use crate::shared::home::factory_home;
use crate::shared::http::{api, cas};
use crate::shared::project::{
    local_bytes, project_files, pull_project, r2_target, read_project_state, safe_job_folder,
    save_project_state, sha, strong,
};
use crate::shared::py::{os_error, printable_line, splitlines, strip};
use crate::shared::pyjson::{dumps, loads, INDENT2_SORTED};
use crate::shared::pyre::{Flags, Pattern};
use crate::shared::pyval::{py_str, truthy};
use crate::shared::time::now;

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
    // PureWindowsPath compares parts case-folded.
    #[cfg(windows)]
    found.sort_by_cached_key(|(parts, _)| {
        parts.iter().map(|p| p.to_lowercase()).collect::<Vec<_>>()
    });
    #[cfg(not(windows))]
    found.sort();
    Ok(found
        .into_iter()
        .map(|(parts, p)| (parts.join("/"), p))
        .collect())
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
        let Some(entry) = manifest.get(rel).filter(|e| truthy(Some(*e))) else {
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

/// `str(UnicodeDecodeError)` for `data.decode("utf-8")`.
fn decode_error(data: &[u8], e: &std::str::Utf8Error) -> String {
    let i = e.valid_up_to();
    let n = e.error_len().unwrap_or(data.len() - i);
    let reason = if matches!(data[i], 0x80..=0xc1 | 0xf5..=0xff) {
        "invalid start byte"
    } else if e.error_len().is_none() {
        "unexpected end of data"
    } else {
        "invalid continuation byte"
    };
    let at = if n > 1 {
        format!("bytes in position {i}-{}", i + n - 1)
    } else {
        format!("byte 0x{:02x} in position {i}", data[i])
    };
    format!("'utf-8' codec can't decode {at}: {reason}")
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

fn push_project(home: &Path, slug: &str) -> Res<()> {
    let (document, state_path, remote) = project_files(home, slug)?;
    if remote.exists() {
        return Err(fail("Project has an unresolved .remote file. Merge it into the project document and delete the .remote file before pushing.", 6));
    }
    let Some(state) = read_project_state(&state_path)? else {
        return Err(fail(
            format!("Project has no saved base version. Run pull {slug} to compare it with R2 before pushing."),
            6,
        ));
    };
    let cannot = |e: String| fail(format!("cannot push local project: {e}"), 6);
    let original = fs::read(&document).map_err(|e| cannot(os_error(&e, Some(&document))))?;
    let text = std::str::from_utf8(&original).map_err(|e| cannot(decode_error(&original, &e)))?;
    let data = without_path(text).into_bytes();
    let etag = state["etag"].as_str().unwrap_or_default();
    let headers = [
        ("If-Match", etag),
        ("Content-Type", "text/markdown; charset=utf-8"),
    ];
    let r = api(
        home,
        "PUT",
        &format!("/api/projects/{slug}.md"),
        Some(&data),
        &[200, 412],
        &headers,
    )?;
    if r.status == 412 {
        return Err(fail(
            format!("Project changed in R2. Local edits were kept. Run pull {slug} to compare and merge before pushing."),
            6,
        ));
    }
    project_files(home, slug)?;
    save_project_state(&state_path, r.header("ETag"), &data)?;
    if data != original
        && fs::read(&document).map_err(|e| cannot(os_error(&e, Some(&document))))? == original
    {
        write_atomic(&document, &data).map_err(|e| cannot(os_error(&e, Some(&document))))?;
    }
    Ok(())
}

/// The board change that copies the spec Goal onto the job's row, or drops the row's
/// goal when the spec has none, and whether anything changed. Leaves job.updated alone.
fn goal_change(
    text: Option<&str>,
    slug: &str,
    job_id: &str,
    goal: Option<&str>,
) -> Res<(Option<String>, bool)> {
    let mut data = load_board(text)?;
    let job = data["jobs"]
        .as_array_mut()
        .and_then(|jobs| {
            jobs.iter_mut()
                .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id))
        })
        .and_then(Value::as_object_mut);
    let Some(job) = job else {
        return Ok((None, false));
    };
    let wanted = goal.map_or(Value::Null, Value::from);
    if job.get("project").and_then(Value::as_str) != Some(slug)
        || job.get("goal").unwrap_or(&Value::Null) == &wanted
    {
        return Ok((None, false));
    }
    if goal.is_none() {
        job.shift_remove("goal");
    } else {
        job.insert("goal".into(), wanted);
    }
    data["updated"] = Value::from(now());
    Ok((Some(dump_board(&data)), true))
}

fn record_goal(home: &Path, slug: &str, job_id: &str, goal: Option<String>) -> Res<()> {
    if cas(home, "/api/board", |text| {
        goal_change(text, slug, job_id, goal.as_deref())
    })? {
        println!(
            "{}",
            if goal.is_some() {
                "recorded the spec Goal on the board"
            } else {
                "cleared the board goal: spec.md has no Goal section"
            }
        );
    }
    Ok(())
}

/// The board with decision.md's verdict and decider stamped on the job's row, or the row's decision
/// dropped when the file has no verdict line, and whether anything changed. Leaves job.updated alone.
fn decision_change(
    text: Option<&str>,
    slug: &str,
    job_id: &str,
    decision: Option<&Value>,
) -> Res<(Option<String>, bool)> {
    let mut data = load_board(text)?;
    let job = data["jobs"]
        .as_array_mut()
        .and_then(|jobs| {
            jobs.iter_mut()
                .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id))
        })
        .and_then(Value::as_object_mut);
    let Some(job) = job else {
        return Ok((None, false));
    };
    if job.get("project").and_then(Value::as_str) != Some(slug) {
        return Ok((None, false));
    }
    let stored = job.get("decision").filter(|s| s.is_object());
    match decision {
        None if job.get("decision").is_none() => return Ok((None, false)),
        None => {
            job.shift_remove("decision");
        }
        Some(d)
            if stored.is_some_and(|s| {
                s.get("verdict") == d.get("verdict") && s.get("decider") == d.get("decider")
            }) =>
        {
            return Ok((None, false))
        }
        Some(d) => {
            let mut stamped = d.clone();
            stamped["at"] = Value::from(now());
            job.insert("decision".into(), stamped);
        }
    }
    data["updated"] = Value::from(now());
    Ok((Some(dump_board(&data)), true))
}

fn record_decision(
    home: &Path,
    slug: &str,
    job_id: &str,
    decision: Option<Value>,
    out: &mut dyn Write,
) -> Res<()> {
    if cas(home, "/api/board", |text| {
        decision_change(text, slug, job_id, decision.as_ref())
    })? {
        match &decision {
            Some(d) => writeln!(
                out,
                "recorded the decision.md verdict on the board: {}, decided by {}",
                py_str(d.get("verdict")),
                py_str(d.get("decider"))
            )
            .ok(),
            None => writeln!(
                out,
                "cleared the board decision: decision.md has no verdict line"
            )
            .ok(),
        };
    }
    Ok(())
}

pub(crate) fn push_target(target: &str) -> Res<()> {
    let (home, slug, job_id) = r2_target(target, false)?;
    if job_id.is_empty() {
        push_project(&home, &slug)?;
        println!("pushed projects/{slug}.md");
        return Ok(());
    }
    let (folder, mut manifest, remote) = sync_state(&home, &slug, &job_id)?;
    // Skip symlinks, so a stray link can't upload a file from outside the job folder.
    let rels = walk(&folder)
        .map_err(|e| os_fail(e, &folder))?
        .into_iter()
        .filter(|(rel, p)| {
            fs::symlink_metadata(p).is_ok_and(|m| m.is_file()) && JOB_REL.is_full(rel)
        });
    let (mut pushed, mut conflicts, mut spec_pushed, mut spec_stale, mut decision_pushed) =
        (0, Vec::new(), false, false, false);
    for (rel, path) in rels {
        let data = fs::read(&path).map_err(|e| os_fail(e, &path))?;
        let entry = manifest.get(&rel).filter(|e| truthy(Some(*e))).cloned();
        if dot_remote(&path).exists() {
            conflicts.push(rel);
            continue;
        }
        if let Some(entry) = &entry {
            let Some(tag) = remote.get(&rel) else {
                println!("{rel}: deleted in R2, not uploaded again");
                continue;
            };
            if entry.get("sha256") == Some(&Value::from(sha(&data))) {
                if entry.get("etag") != Some(&Value::from(tag.as_str())) {
                    println!("{rel}: remote newer, run pull");
                    spec_stale = spec_stale || rel == "spec.md";
                }
                continue;
            }
        } else if remote.contains_key(&rel) {
            // First push after upgrading: adopt R2's copy if it matches.
            if reconcile(
                &home,
                &slug,
                &job_id,
                &folder,
                &mut manifest,
                &rel,
                Some(&data),
            )? {
                conflicts.push(rel);
            }
            continue;
        }
        let cond = match &entry {
            Some(entry) => match entry.get("etag").and_then(Value::as_str) {
                Some(etag) => ("If-Match", etag.to_string()),
                None => {
                    return Err(fail(
                        format!("the sync manifest entry for {rel} has no etag"),
                        1,
                    ))
                }
            },
            None => ("If-None-Match", "*".to_string()),
        };
        let r = api(
            &home,
            "PUT",
            &format!("/api/jobs/{slug}/{job_id}/{rel}"),
            Some(&data),
            &[200, 412],
            &[(cond.0, &cond.1)],
        )?;
        if r.status == 412 {
            if reconcile(
                &home,
                &slug,
                &job_id,
                &folder,
                &mut manifest,
                &rel,
                Some(&data),
            )? {
                conflicts.push(rel);
            }
            continue;
        }
        let Some(tag) = r.header("ETag").filter(|t| !t.is_empty()) else {
            return Err(fail(
                "the Worker returned no ETag: redeploy the factory Worker",
                5,
            ));
        };
        manifest.insert(
            rel.clone(),
            json!({"etag": strong(tag), "sha256": sha(&data)}),
        );
        pushed += 1;
        spec_pushed = spec_pushed || rel == "spec.md";
        decision_pushed = decision_pushed || rel == "decision.md";
    }
    let pending = folder.join(".goal-pending");
    if spec_pushed {
        write_atomic(&pending, b"").map_err(|e| os_fail(e, &pending))?;
    }
    let goal_due = spec_pushed || pending.exists();
    save_manifest(&folder, &manifest)?;
    println!("pushed {pushed} files");
    let spec = folder.join("spec.md");
    if goal_due && spec.exists() && !conflicts.iter().any(|c| c == "spec.md") && !spec_stale {
        let text = read_text(&spec).map_err(|e| os_fail(e, &spec))?;
        record_goal(&home, &slug, &job_id, spec_goal(&text))?;
        match fs::remove_file(&pending) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(os_fail(e, &pending)),
            _ => {}
        }
    }
    if decision_pushed && !conflicts.iter().any(|c| c == "decision.md") {
        let path = folder.join("decision.md");
        let text = read_text(&path).map_err(|e| os_fail(e, &path))?;
        record_decision(
            &home,
            &slug,
            &job_id,
            decision_verdict(&text),
            &mut io::stdout(),
        )?;
    }
    if !conflicts.is_empty() {
        return Err(conflict_exit(&conflicts));
    }
    Ok(())
}

pub fn push(call: &Call) -> Res<()> {
    push_target(&call.args[0])
}

/// Download what changed in R2 and keep local edits. (summary line, conflicted files).
pub(crate) fn pull_job(home: &Path, slug: &str, job_id: &str) -> Res<(String, Vec<String>)> {
    let (folder, mut manifest, remote) = sync_state(home, slug, job_id)?;
    let (mut got, mut unpushed, mut conflicts) = (0, Vec::new(), Vec::new());
    for (rel, tag) in &remote {
        let path = folder.join(rel);
        let data = local_bytes(&path);
        let entry = manifest.get(rel).filter(|e| truthy(Some(*e))).cloned();
        let tag_value = Value::from(tag.as_str());
        let stored = |key: &str| entry.as_ref().and_then(|e| e.get(key));
        if dot_remote(&path).exists() {
            // Still unmerged. Refresh the saved copy if R2 moved again.
            if stored("etag") != Some(&tag_value) {
                reconcile(
                    home,
                    slug,
                    job_id,
                    &folder,
                    &mut manifest,
                    rel,
                    data.as_deref(),
                )?;
            }
            conflicts.push(rel.clone());
        } else if data
            .as_ref()
            .is_none_or(|d| entry.is_some() && stored("sha256") == Some(&Value::from(sha(d))))
        {
            if data.is_some() && stored("etag") == Some(&tag_value) {
                continue;
            }
            let (body, fetched) = fetch(home, slug, job_id, rel)?;
            manifest.insert(rel.clone(), fetched);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| os_fail(e, parent))?;
            }
            write_atomic(&path, &body).map_err(|e| os_fail(e, &path))?;
            got += 1;
        } else if entry.is_some() && stored("etag") == Some(&tag_value) {
            unpushed.push(rel.clone());
        } else if reconcile(
            home,
            slug,
            job_id,
            &folder,
            &mut manifest,
            rel,
            data.as_deref(),
        )? {
            conflicts.push(rel.clone());
        }
    }
    save_manifest(&folder, &manifest)?;
    let mut line = format!("pulled {got} files");
    if !unpushed.is_empty() {
        line += &format!(", unpushed: {}", unpushed.join(", "));
    }
    if !conflicts.is_empty() {
        line += &format!(", conflict: {}", conflicts.join(", "));
    }
    Ok((line, conflicts))
}

pub fn pull(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], false)?;
    if job_id.is_empty() {
        let dirty = pull_project(&home, &slug)?;
        if dirty {
            println!("project {slug} has unpushed local edits");
        } else {
            println!("pulled project {slug}");
        }
        return Ok(());
    }
    let (line, conflicts) = pull_job(&home, &slug, &job_id)?;
    println!("{line}");
    if !conflicts.is_empty() {
        return Err(conflict_exit(&conflicts));
    }
    Ok(())
}

/// Pull every active job of one project, or of every project. It skips merged and
/// closed jobs, whose files no longer change. pull <project>/<id> fetches one finished job.
pub fn sync(call: &Call) -> Res<()> {
    let slug = call.args.first().filter(|s| !s.is_empty());
    if let Some(slug) = slug {
        r2_target(slug, false)?;
    }
    let home = factory_home()?;
    let mut conflicted = Vec::new();
    let board = read_board(&home)?;
    for j in board["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|j| j.is_object())
    {
        // Stubs and ideas have no job files, and merged or closed jobs no longer change.
        let skip = match j.get("stage") {
            None | Some(Value::Null) => true,
            Some(Value::String(s)) => s == IDEA || TERMINAL.contains(&s.as_str()),
            Some(_) => false,
        };
        if skip || slug.is_some_and(|s| j.get("project").and_then(Value::as_str) != Some(s)) {
            continue;
        }
        let (_, project, job_id) = r2_target(
            &format!("{}/{}", py_str(j.get("project")), py_str(j.get("id"))),
            true,
        )?;
        let (line, conflicts) = pull_job(&home, &project, &job_id)?;
        println!("{project}/{job_id}: {line}");
        conflicted.extend(
            conflicts
                .iter()
                .map(|rel| format!("{project}/{job_id}/{rel}")),
        );
    }
    if !conflicted.is_empty() {
        return Err(conflict_exit(&conflicted));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;
    use crate::shared::http::tests::home_for;

    fn job_home(name: &str, origin: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let home = home_for(name, origin);
        let folder = home.join("jobs/alpha/042-sample-export");
        fs::create_dir_all(&folder).unwrap();
        for (rel, data) in files {
            fs::write(folder.join(rel), data).unwrap();
        }
        home
    }

    fn push_with_home(home: &Path, target: &str) -> Res<()> {
        let _lock = crate::shared::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let old = std::env::var_os("FACTORY_HOME");
        std::env::set_var("FACTORY_HOME", home);
        let result = push_target(target);
        match old {
            Some(v) => std::env::set_var("FACTORY_HOME", v),
            None => std::env::remove_var("FACTORY_HOME"),
        }
        result
    }

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
        let first = if cfg!(windows) {
            ["a.md", "Z.md"]
        } else {
            ["Z.md", "a.md"]
        };
        assert_eq!(
            rels,
            [first[0], first[1], "round-1", "round-1/x.md", "round-1.md"]
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

    #[test]
    fn goal_change_keeps_key_order() {
        let keys = |text: &str| {
            let board = loads(text).unwrap();
            board["jobs"][0]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        let with_goal =
            r#"{"updated":"","jobs":[{"id":"1-a","project":"s","goal":"g","stage":"build"}]}"#;
        let (text, changed) = goal_change(Some(with_goal), "s", "1-a", None).unwrap();
        assert!(changed);
        assert_eq!(keys(&text.unwrap()), ["id", "project", "stage"]);
        let without = r#"{"updated":"","jobs":[{"id":"1-a","project":"s","stage":"build"}]}"#;
        let (text, _) = goal_change(Some(without), "s", "1-a", Some("g")).unwrap();
        assert_eq!(keys(&text.unwrap()), ["id", "project", "stage", "goal"]);
        assert_eq!(
            goal_change(Some(with_goal), "s", "1-a", Some("g")).unwrap(),
            (None, false)
        );
    }

    #[test]
    fn push_project_reports_bad_utf8() {
        let home = home_for("sync-utf8", "http://127.0.0.1:9");
        fs::create_dir_all(home.join("projects")).unwrap();
        fs::write(home.join("projects/sample.md"), b"\xff").unwrap();
        let state = json!({"version": 1, "etag": "\"e1\"", "sha256": sha(b"")});
        fs::write(
            home.join("projects/sample.sync.json"),
            dumps(&state, INDENT2_SORTED),
        )
        .unwrap();
        let e = push_project(&home, "sample").unwrap_err();
        assert_eq!(e.code, 6);
        assert_eq!(
            e.stderr,
            "factory.py: cannot push local project: 'utf-8' codec can't decode byte 0xff in position 0: invalid start byte\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn local_edit_during_get_and_put() {
        use crate::shared::http::tests::serve_hooked;
        use std::sync::{Arc, Mutex};
        let doc: Arc<Mutex<PathBuf>> = Arc::default();
        let hook_doc = doc.clone();
        let (origin, seen) = serve_hooked(
            vec![
                (
                    200,
                    "---\nrepo: octo/sample\n---\n",
                    vec![("ETag", "\"e1\"")],
                ),
                (
                    200,
                    "---\nrepo: octo/sample\n---\nRemote",
                    vec![("ETag", "\"e2\"")],
                ),
                (200, "", vec![("ETag", "\"e3\"")]),
            ],
            // Request 1 is the second pull's GET and request 2 is the push's PUT.
            move |index| {
                let doc = hook_doc.lock().unwrap();
                match index {
                    1 => fs::write(&*doc, "---\nrepo: octo/sample\n---\nTyping").unwrap(),
                    2 => fs::write(&*doc, "---\nrepo: octo/sample\n---\nTypingMore").unwrap(),
                    _ => {}
                }
            },
        );
        let home = home_for("sync-local-edit", &origin);
        *doc.lock().unwrap() = home.join("projects/sample.md");
        let state_path = home.join("projects/sample.sync.json");
        pull_project(&home, "sample").unwrap();
        let before = fs::read(&state_path).unwrap();
        let e = pull_project(&home, "sample").unwrap_err();
        assert_eq!(e.code, 6);
        assert_eq!(
            e.stderr,
            "factory.py: Project changed locally during pull. Local edits were kept.\n"
        );
        let draft = b"---\nrepo: octo/sample\n---\nTyping";
        assert_eq!(fs::read(&*doc.lock().unwrap()).unwrap(), draft);
        assert_eq!(fs::read(&state_path).unwrap(), before);
        push_project(&home, "sample").unwrap();
        assert_eq!(seen.lock().unwrap()[2].body, draft);
        assert_eq!(
            fs::read(&*doc.lock().unwrap()).unwrap(),
            b"---\nrepo: octo/sample\n---\nTypingMore"
        );
        let state = read_project_state(&state_path).unwrap().unwrap();
        assert_eq!(state["sha256"], sha(draft));
        fs::remove_dir_all(home).unwrap();
    }

    const PERSON_APPROVE: &str = r#"{"verdict": "APPROVE", "decider": "person"}"#;

    #[test]
    fn push_records_decision() {
        use crate::shared::http::tests::serve;
        let board = r#"{"updated":"2026-11-23T14:00:00Z","jobs":[{"id":"042-sample-export","project":"alpha","stage":"needs-human","updated":"2026-11-23T14:00:00Z"}]}"#;
        let (origin, seen) = serve(vec![
            (200, board, vec![("ETag", "\"b1\"")]),
            (200, "{}", vec![("ETag", "\"b2\"")]),
        ]);
        let home = home_for("sync-decision", &origin);
        let mut out = Vec::new();
        record_decision(
            &home,
            "alpha",
            "042-sample-export",
            Some(loads(PERSON_APPROVE).unwrap()),
            &mut out,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "recorded the decision.md verdict on the board: APPROVE, decided by person\n"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(
            (seen[1].method.as_str(), seen[1].header("If-Match")),
            ("PUT", Some("\"b1\""))
        );
        let put = loads(std::str::from_utf8(&seen[1].body).unwrap()).unwrap();
        let job = &put["jobs"][0];
        assert_eq!(
            (&job["decision"]["verdict"], &job["decision"]["decider"]),
            (&json!("APPROVE"), &json!("person"))
        );
        assert_eq!(job["decision"]["at"], put["updated"]);
        assert_eq!(job["updated"], json!("2026-11-23T14:00:00Z"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn push_skips_unchanged_decision() {
        use crate::shared::http::tests::serve;
        let board = r#"{"updated":"","jobs":[{"id":"042-sample-export","project":"alpha","decision":{"verdict":"APPROVE","decider":"person","at":"2026-11-23T15:00:00Z"}}]}"#;
        let (origin, seen) = serve(vec![(200, board, vec![("ETag", "\"b1\"")])]);
        let home = home_for("sync-decision-same", &origin);
        let mut out = Vec::new();
        record_decision(
            &home,
            "alpha",
            "042-sample-export",
            Some(loads(PERSON_APPROVE).unwrap()),
            &mut out,
        )
        .unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(out.is_empty());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn push_prints_cleared_decision() {
        use crate::shared::http::tests::serve;
        let board = r#"{"updated":"","jobs":[{"id":"042-sample-export","project":"alpha","decision":{"verdict":"APPROVE","decider":"model","at":"x"}}]}"#;
        let (origin, seen) = serve(vec![
            (200, board, vec![("ETag", "\"b1\"")]),
            (200, "{}", vec![("ETag", "\"b2\"")]),
        ]);
        let home = home_for("sync-decision-cleared", &origin);
        let mut out = Vec::new();
        record_decision(&home, "alpha", "042-sample-export", None, &mut out).unwrap();
        assert_eq!(seen.lock().unwrap()[1].method, "PUT");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "cleared the board decision: decision.md has no verdict line\n"
        );
        fs::remove_dir_all(home).unwrap();
    }

    const DECISION: &[u8] = b"APPROVE\nA person approved this job with /factory approve.\n";

    // `factory_home` prefers the process-wide `--home` override, which another
    // test may already have set, so the push tests run in a fresh process.
    fn in_child(test: &str) -> bool {
        if std::env::var_os("FACTORY_PUSH_TARGET_CHILD").is_some() {
            return true;
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                &format!("commands::sync::tests::{test}"),
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("FACTORY_PUSH_TARGET_CHILD", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        false
    }

    #[test]
    fn push_target_records_decision() {
        if !in_child("push_target_records_decision") {
            return;
        }
        use crate::shared::http::tests::serve;
        let board = r#"{"updated":"2026-11-23T14:00:00Z","jobs":[{"id":"042-sample-export","project":"alpha","stage":"needs-human"}]}"#;
        let (origin, seen) = serve(vec![
            (200, "[]", vec![]),
            (200, "", vec![("ETag", "\"d1\"")]),
            (200, board, vec![("ETag", "\"b1\"")]),
            (200, "{}", vec![("ETag", "\"b2\"")]),
        ]);
        let home = job_home("sync-push-decision", &origin, &[("decision.md", DECISION)]);
        push_with_home(&home, "alpha/042-sample-export").unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert_eq!(
            (
                seen[1].method.as_str(),
                seen[1].path.as_str(),
                seen[1].header("If-None-Match")
            ),
            (
                "PUT",
                "/api/jobs/alpha/042-sample-export/decision.md",
                Some("*")
            )
        );
        assert_eq!(seen[1].body, DECISION);
        assert_eq!(
            (
                seen[3].method.as_str(),
                seen[3].path.as_str(),
                seen[3].header("If-Match")
            ),
            ("PUT", "/api/board", Some("\"b1\""))
        );
        let put = loads(std::str::from_utf8(&seen[3].body).unwrap()).unwrap();
        let decision = &put["jobs"][0]["decision"];
        assert_eq!(
            (&decision["verdict"], &decision["decider"]),
            (&json!("APPROVE"), &json!("person"))
        );
        let manifest = read_text(&home.join("jobs/alpha/042-sample-export/.sync.json")).unwrap();
        assert_eq!(loads(&manifest).unwrap()["decision.md"]["etag"], "\"d1\"");
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn push_target_skips_decision_on_conflict() {
        if !in_child("push_target_skips_decision_on_conflict") {
            return;
        }
        use crate::shared::http::tests::serve;
        let (origin, seen) = serve(vec![
            (200, r#"[{"name":"decision.md","etag":"\"d0\""}]"#, vec![]),
            (412, "", vec![]),
            (200, "CHANGES\nR2 copy\n", vec![("ETag", "\"d1\"")]),
        ]);
        let state = json!({"decision.md": {"etag": "\"d0\"", "sha256": sha(b"old\n")}});
        let home = job_home(
            "sync-push-conflict",
            &origin,
            &[
                ("decision.md", DECISION),
                (".sync.json", dumps(&state, INDENT2_SORTED).as_bytes()),
            ],
        );
        let e = push_with_home(&home, "alpha/042-sample-export").unwrap_err();
        assert_eq!(e.code, 6);
        assert!(e.stderr.contains("conflict in decision.md"));
        assert_eq!(
            fs::read(home.join("jobs/alpha/042-sample-export/decision.md.remote")).unwrap(),
            b"CHANGES\nR2 copy\n"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[1].header("If-Match"), Some("\"d0\""));
        assert!(seen.iter().all(|s| s.path != "/api/board"));
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn push_clears_decision_without_a_verdict() {
        let stored = r#"{"updated":"","jobs":[{"id":"042-sample-export","project":"alpha","decision":{"verdict":"APPROVE","decider":"model","at":"x"},"stage":"build"}]}"#;
        let (text, changed) =
            decision_change(Some(stored), "alpha", "042-sample-export", None).unwrap();
        assert!(changed);
        let job = &loads(&text.unwrap()).unwrap()["jobs"][0];
        assert!(job.get("decision").is_none() && job["stage"] == "build");
        let none = r#"{"updated":"","jobs":[{"id":"042-sample-export","project":"alpha"}]}"#;
        assert_eq!(
            decision_change(Some(none), "alpha", "042-sample-export", None).unwrap(),
            (None, false)
        );
        let decision = loads(PERSON_APPROVE).unwrap();
        assert_eq!(
            decision_change(Some(none), "beta", "042-sample-export", Some(&decision)).unwrap(),
            (None, false)
        );
        let (text, _) =
            decision_change(Some(stored), "alpha", "042-sample-export", Some(&decision)).unwrap();
        assert_eq!(
            loads(&text.unwrap()).unwrap()["jobs"][0]["decision"]["decider"],
            "person"
        );
    }

    #[test]
    fn decode_error_matches_python() {
        let cases: [(&[u8], &str); 5] = [
            (b"\xe2\x82", "bytes in position 0-1: unexpected end of data"),
            (
                b"\xe2\x82x",
                "bytes in position 0-1: invalid continuation byte",
            ),
            (
                b"\xf0\x90\x80",
                "bytes in position 0-2: unexpected end of data",
            ),
            (
                b"\xe0\x80",
                "byte 0xe0 in position 0: invalid continuation byte",
            ),
            (
                b"a\xe2x",
                "byte 0xe2 in position 1: invalid continuation byte",
            ),
        ];
        for (data, want) in cases {
            let e = std::str::from_utf8(data).unwrap_err();
            assert_eq!(
                decode_error(data, &e),
                format!("'utf-8' codec can't decode {want}")
            );
        }
    }
}
