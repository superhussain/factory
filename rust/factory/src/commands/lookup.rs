//! The lookup commands: where, job and builds.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::cli::Call;
use crate::shared::board::{escalated, read_board};
use crate::shared::conf::{fm_get, frontmatter};
use crate::shared::consts::IN_FLIGHT;
use crate::shared::exit::{fail, Res};
use crate::shared::fs::{resolve, same_path};
use crate::shared::home::{factory_home, holder};
use crate::shared::http::{api, api_url, quote};
use crate::shared::proc::git;
use crate::shared::project::{project_files, pull_project, r2_target, read_paths};
use crate::shared::py::{os_error, splitlines};
use crate::shared::pyjson::{dumps, loads, INDENT2};
use crate::shared::pyre::{Flags, Pattern};
use crate::shared::time::{parse_stamp, unix_now};

static GITHUB_REMOTE: Pattern = Pattern::new(
    r"github\.com[:/]([\w.-]+/[\w.-]+?)(?:\.git)?/?\s",
    Flags::NONE,
);

pub(crate) fn main_checkout(cwd: &Path) -> Option<PathBuf> {
    let common = git(cwd, &["rev-parse", "--git-common-dir"]);
    if common.is_empty() {
        return None;
    }
    let resolved = resolve(&cwd.join(common)).ok()?;
    resolved.parent().map(Path::to_path_buf)
}

/// owner/name for every GitHub remote, origin first.
pub(crate) fn remotes(cwd: &Path) -> Vec<String> {
    let out = git(cwd, &["remote", "-v"]);
    let mut lines = splitlines(&out, false);
    lines.sort_by_key(|l| !l.starts_with("origin"));
    let mut found: Vec<String> = Vec::new();
    for line in lines {
        if let Some(m) = GITHUB_REMOTE.search(line) {
            let name = m.get(1).unwrap().as_str().to_string();
            if !found.contains(&name) {
                found.push(name);
            }
        }
    }
    found
}

fn json_error(e: serde_json::Error) -> crate::shared::exit::Exit {
    fail(format!("the Worker sent JSON that does not parse: {e}"), 1)
}

pub fn where_cmd(call: &Call) -> Res<()> {
    let mut slug = call.args.first().cloned();
    let home = factory_home()?;
    let url = api_url(&home)?;
    let cwd = std::env::current_dir().map_err(|e| fail(os_error(&e, None), 1))?;
    let main = if slug.is_none() {
        main_checkout(&cwd)
    } else {
        None
    };
    let paths = read_paths(&home)?;
    let listed = loads(&api(&home, "GET", "/projects.json", None, &[200], &[])?.text()?)
        .map_err(json_error)?;
    let mut projects: Vec<(String, Value)> = Vec::new();
    for p in listed.as_array().into_iter().flatten() {
        let key = p["slug"].as_str().unwrap_or_default().to_string();
        let mut entry = p.clone();
        entry["path"] = Value::from(fm_get(&paths, &key).unwrap_or(""));
        match projects.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = entry,
            None => projects.push((key, entry)),
        }
    }
    if slug.is_none() {
        if let Some(main) = &main {
            let names: Vec<String> = remotes(main).iter().map(|r| r.to_lowercase()).collect();
            slug = projects
                .iter()
                .find(|(_, m)| {
                    names.contains(
                        &m.get("repo")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_lowercase(),
                    )
                })
                .map(|(s, _)| s.clone());
            if slug.is_none() {
                slug = projects
                    .iter()
                    .find(|(_, m)| {
                        let path = m["path"].as_str().unwrap_or("");
                        !path.is_empty() && same_path(Path::new(path), main)
                    })
                    .map(|(s, _)| s.clone());
            }
        }
    }
    let Some((slug, project)) = slug
        .as_ref()
        .and_then(|s| projects.iter().find(|(k, _)| k == s))
    else {
        let known = projects
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let known = if known.is_empty() {
            "none".to_string()
        } else {
            known
        };
        let subject = match (&slug, &main) {
            (Some(s), _) if !s.is_empty() => s.clone(),
            (_, Some(m)) => m.display().to_string(),
            _ => cwd.display().to_string(),
        };
        return Err(fail(
            format!("no factory project for {subject}. Known projects: {known}. Register a repo by running /factory init in it."),
            4,
        ));
    };
    if pull_project(&home, slug)? {
        return Err(fail(
            format!("Project has local edits. Push {slug} before continuing."),
            6,
        ));
    }
    let (project_file, _, _) = project_files(&home, slug)?;
    let meta = frontmatter(&project_file)
        .map_err(|e| fail(os_error(&e, Some(&project_file)), 1))?
        .0;
    let path = project["path"].as_str().unwrap_or("");
    if path.is_empty() {
        return Err(fail(
            format!(
                "{} has no entry for {slug}: run /factory init in this device's checkout of it",
                home.join("paths.md").display()
            ),
            1,
        ));
    }
    let runtime = std::env::current_exe()
        .and_then(|p| resolve(&p))
        .map_err(|e| fail(os_error(&e, None), 1))?;
    let text = |p: PathBuf| p.display().to_string();
    let out = json!({
        "factory": text(home.clone()),
        "runtime": text(runtime),
        "project_file": text(project_file),
        "repo_path": path,
        "repo": fm_get(&meta, "repo").unwrap_or(""),
        "base": fm_get(&meta, "base").filter(|b| !b.is_empty()).unwrap_or("main"),
        "slug": slug,
        "jobs": text(home.join("jobs").join(slug)),
        "worktrees": text(home.join("worktrees").join(slug)),
        "dashboard": format!("{url}/"),
    });
    println!("{}", dumps(&out, INDENT2));
    Ok(())
}

pub fn job(call: &Call) -> Res<()> {
    let job_id = &call.args[0];
    let home = factory_home()?;
    let board = read_board(&home)?;
    let found = board["jobs"].as_array().and_then(|jobs| {
        jobs.iter()
            .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id))
    });
    let found = match found {
        Some(j) => j.clone(),
        None => {
            let r = api(
                &home,
                "GET",
                &format!("/api/board/archive?id={}", quote(job_id)),
                None,
                &[200, 404],
                &[],
            )?;
            if r.status == 404 {
                return Err(fail(
                    format!("no job {job_id} on the board or in the archive. Nothing was written."),
                    4,
                ));
            }
            let archive = loads(&r.text()?).map_err(json_error)?;
            archive["jobs"]
                .get(0)
                .cloned()
                .ok_or_else(|| fail(format!("the archive answered with no job for {job_id}"), 1))?
        }
    };
    println!("{}", dumps(&found, INDENT2));
    Ok(())
}

pub fn builds(call: &Call) -> Res<()> {
    let slug = &call.args[0];
    r2_target(slug, false)?;
    let home = factory_home()?;
    let minutes = holder(&home)?.2;
    let cutoff = unix_now() - i64::from(minutes) * 60;
    let board = read_board(&home)?;
    let jobs: Vec<&Value> = board["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|j| j.get("project").and_then(Value::as_str) == Some(slug))
        .collect();
    let fresh = |j: &Value| {
        j.get("updated")
            .and_then(Value::as_str)
            .and_then(parse_stamp)
            .is_some_and(|seen| seen > cutoff)
    };
    let build_state = |j: &Value| {
        j.get("build")
            .and_then(|b| b.get("state"))
            .and_then(Value::as_str)
            .map(String::from)
    };
    let id = |j: &&Value| j.get("id").cloned().unwrap_or(Value::Null);
    let stage = |j: &Value| {
        j.get("stage")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let queued: Vec<Value> = jobs
        .iter()
        .filter(|j| build_state(j).as_deref() == Some("queued"))
        .map(id)
        .collect();
    let moving: Vec<&Value> = jobs
        .iter()
        .copied()
        .filter(|j| {
            IN_FLIGHT.contains(&stage(j).as_str()) || stage(j) == "pr-open" && !escalated(j)
        })
        .collect();
    let flight: Vec<Value> = moving.iter().filter(|j| fresh(j)).map(id).collect();
    let stale: Vec<Value> = moving.iter().filter(|j| !fresh(j)).map(id).collect();
    let running: Vec<Value> = jobs
        .iter()
        .filter(|j| build_state(j).as_deref() == Some("running"))
        .map(id)
        .collect();
    let due = !queued.is_empty() && flight.is_empty() && running.is_empty();
    let out = json!({"queued": queued, "in_flight": flight, "stale": stale, "running": running, "due": due});
    println!("{}", dumps(&out, INDENT2));
    Ok(())
}
