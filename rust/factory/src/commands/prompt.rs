//! The prompt command: one role's full agent prompt, printed or written to a private file.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::cli::Call;
use crate::shared::board::{lead_of, read_board};
use crate::shared::conf::{fm_get, frontmatter};
use crate::shared::consts::{
    tag_lookalike, BABYSIT_SENTENCE, NOTE_INVISIBLE, NOTE_LINK, NOTE_PHRASES, NOTE_SECRETS,
    NOTE_TAG, PIPELINE_GATES, PIPELINE_STAGES, PREFERENCES_SENTENCE, PROJECT_SLUG, STEER_AT,
    STEER_MAX, STEER_SENTENCE,
};
use crate::shared::exit::{fail, Exit, Res};
use crate::shared::fs::{
    contained, io_fail, is_link, make_job_temp, mkdir_private, normpath, write_atomic,
};
use crate::shared::home::{runtime_path, skill_root};
use crate::shared::http::api;
use crate::shared::project::{project_files, r2_target};
use crate::shared::py::repr;
use crate::shared::pyjson::loads;
use crate::shared::pyre::{Flags as ReFlags, Pattern};
use crate::shared::pyval::{py_str, truthy};
use crate::shared::time::parse_stamp;
use crate::shared::unicode::nfkc;

const PROMPT_ROLES: [&str; 7] = [
    "spec",
    "build",
    "review-security",
    "review-ux",
    "review-ui",
    "review-code",
    "approve",
];
const PROMPT_USAGE: &str =
    "usage: prompt <project-slug>/<job-id> <role> [--round N] [--port P] [--out <path>]";
const PROMPT_HELP: &str =
    "roles and flags: spec | approve | build [--round N] [--port P] | review-security, review-ux, \
review-ui or review-code --round N. Every role also takes --out <path>.\n\
example: prompt sample/042-sample review-code --round 2";
static IDEA_FILE_NAME: Pattern =
    Pattern::new(r"[0-9a-f]{64}\.(pdf|csv|txt|mp4|webm)", ReFlags::ASCII);

fn role_alias(r: &str) -> &str {
    match r {
        "builder" | "factory-builder" => "build",
        "spec-writer" | "factory-spec-writer" => "spec",
        "approver" | "factory-approver" => "approve",
        "factory-review-security" => "review-security",
        "factory-review-ux" => "review-ux",
        "factory-review-ui" => "review-ui",
        "factory-review-code" => "review-code",
        other => other,
    }
}

fn usage_error(msg: &str) -> Exit {
    fail(format!("{msg}. {PROMPT_USAGE}\n{PROMPT_HELP}"), 2)
}

struct Flags {
    round: Option<String>,
    port: Option<String>,
    out: Option<String>,
}

/// `re.fullmatch(r"[1-9][0-9]*", s)`.
fn positive_int(s: &str) -> bool {
    s.starts_with(|c: char| ('1'..='9').contains(&c)) && s.chars().all(|c| c.is_ascii_digit())
}

fn parse_flags(role: &str, flags: &[String]) -> Res<Flags> {
    if flags.len() % 2 == 1 {
        return Err(usage_error("a flag is missing its value"));
    }
    let mut options = Flags {
        round: None,
        port: None,
        out: None,
    };
    for pair in flags.chunks(2) {
        let slot = match pair[0].as_str() {
            "--round" => &mut options.round,
            "--port" => &mut options.port,
            "--out" => &mut options.out,
            other => return Err(usage_error(&format!("unknown flag {}", repr(other)))),
        };
        if slot.is_some() {
            return Err(usage_error(&format!("{} given twice", pair[0])));
        }
        *slot = Some(pair[1].clone());
    }
    let review = role.starts_with("review-");
    if options.round.is_some() && !(role == "build" || review) {
        return Err(usage_error(&format!("{role} takes no --round")));
    }
    if options.port.is_some() && role != "build" {
        return Err(usage_error(&format!("{role} takes no --port")));
    }
    if review && options.round.is_none() {
        return Err(usage_error(&format!("{role} needs --round N")));
    }
    if options.round.as_deref().is_some_and(|r| !positive_int(r)) {
        return Err(usage_error("--round must be a positive integer"));
    }
    if options
        .port
        .as_deref()
        .is_some_and(|p| !(positive_int(p) && p.parse::<u64>().is_ok_and(|n| n <= 65535)))
    {
        return Err(usage_error("--port must be 1 to 65535"));
    }
    Ok(options)
}

/// An absolute path strictly below prompts. A `..` left over after normpath, as a backslash path
/// keeps on Windows, is refused because `ancestors` does not collapse it.
fn under_prompts(prompts: &Path, out: &Path) -> bool {
    out.is_absolute()
        && !out.components().any(|c| c == Component::ParentDir)
        && out != prompts
        && contained(prompts, out)
}

fn out_path(home: &Path, out: &str) -> Res<PathBuf> {
    let out = PathBuf::from(normpath(out));
    let prompts = home.join("prompts");
    if !under_prompts(&prompts, &out) {
        return Err(fail(
            format!("--out must be an absolute path under {}", prompts.display()),
            2,
        ));
    }
    Ok(out)
}

/// The decimal text of a JSON int, or None for anything else, floats and bools included.
fn int_text(v: &Value) -> Option<String> {
    let Value::Number(n) = v else { return None };
    let text = n.to_string();
    if text.contains(['.', 'e', 'E']) {
        return None;
    }
    Some(if text == "-0" { "0".into() } else { text })
}

fn has_envelope_tag(text: &str) -> bool {
    let folded: String = nfkc(text).chars().map(tag_lookalike).collect();
    NOTE_TAG.search(text).is_some() || NOTE_TAG.search(&folded).is_some()
}

fn check_untrusted(text: &str) -> Option<String> {
    if NOTE_INVISIBLE.search(text).is_some() {
        return Some("contains control, invisible or bidirectional characters".into());
    }
    if has_envelope_tag(text) {
        return Some("contains the reserved envelope tag".into());
    }
    if NOTE_LINK.search(text).is_some() {
        return Some("links are not allowed".into());
    }
    if NOTE_SECRETS
        .iter()
        .any(|p| p.search_or_stop(text).is_some())
    {
        return Some("looks like a secret".into());
    }
    NOTE_PHRASES
        .iter()
        .find(|(_, p)| p.search(text).is_some())
        .map(|(rule, _)| format!("reads like an instruction to override factory rules ({rule})"))
}

fn check_note(text: &Value) -> Option<String> {
    let Value::String(text) = text else {
        return Some("must be text".into());
    };
    if text.chars().count() > 2000 {
        return Some("over 2000 characters".into());
    }
    if NOTE_INVISIBLE.search(text).is_some() {
        return Some("contains control, invisible or bidirectional characters".into());
    }
    if has_envelope_tag(text) {
        return Some("contains the reserved envelope tag".into());
    }
    if NOTE_LINK.search(text).is_some() {
        return Some("links are not allowed".into());
    }
    None
}

/// An object whose keys are exactly these two.
fn has_keys(v: &Value, keys: [&str; 2]) -> bool {
    v.as_object()
        .is_some_and(|o| o.len() == 2 && keys.iter().all(|k| o.contains_key(*k)))
}

fn steer_block(job_id: &str, steer: &Value) -> Res<Option<String>> {
    if !truthy(steer) {
        return Ok(None);
    }
    let well_formed = steer.as_array().filter(|notes| {
        notes.len() <= STEER_MAX
            && notes.iter().all(|n| {
                has_keys(n, ["note", "at"])
                    && n["at"].as_str().is_some_and(|at| STEER_AT.is_full(at))
            })
    });
    let Some(notes) = well_formed else {
        return Err(fail(
            format!("steer on {job_id} is malformed. Nothing was written."),
            5,
        ));
    };
    let mut envelopes = Vec::new();
    for n in notes {
        let at = n["at"].as_str().unwrap_or_default();
        let reason = check_note(&n["note"])
            .or_else(|| check_untrusted(n["note"].as_str().unwrap_or_default()));
        if let Some(reason) = reason {
            return Err(fail(
                format!("steering note {at} on {job_id} {reason}. Nothing was written."),
                5,
            ));
        }
        envelopes.push(format!(
            "<operator-preferences source=\"steer:{job_id}@{at}\" stage=\"build\" trust=\"data\">\n{}\n</operator-preferences>",
            n["note"].as_str().unwrap_or_default()
        ));
    }
    Ok(Some(format!(
        "{STEER_SENTENCE}\n\n{}",
        envelopes.join("\n\n")
    )))
}

/// Python's `pipeline_view`, returning only its `prompts`.
fn pipeline_view(slug: &str, data: &Value) -> Res<Vec<(&'static str, String)>> {
    let malformed = |field: &str| fail(format!("malformed pipeline view for {slug}: {field}"), 5);
    let Value::Object(view) = data else {
        return Err(malformed("body"));
    };
    if view.get("project").and_then(Value::as_str) != Some(slug) {
        return Err(malformed("project"));
    }
    if !matches!(
        view.get("merge").and_then(Value::as_str),
        Some("auto" | "human")
    ) {
        return Err(malformed("merge"));
    }
    let rounds = view
        .get("review_rounds")
        .and_then(int_text)
        .and_then(|t| t.parse::<i64>().ok());
    if !rounds.is_some_and(|r| (1..=5).contains(&r)) {
        return Err(malformed("review_rounds"));
    }
    if view.get("reviewers") != Some(&serde_json::json!(["security", "ux", "ui", "code"])) {
        return Err(malformed("reviewers"));
    }
    let gates_ok = view
        .get("gates")
        .and_then(Value::as_array)
        .is_some_and(|gates| {
            let ordered: Vec<Value> = PIPELINE_GATES
                .iter()
                .map(|g| Value::from(*g))
                .filter(|g| gates.contains(g))
                .collect();
            *gates == ordered
        });
    if !gates_ok {
        return Err(malformed("gates"));
    }
    let versions = view.get("versions").and_then(Value::as_object).filter(|v| {
        v.len() == 2
            && v.contains_key("global")
            && v.contains_key("project")
            && v.values()
                .all(|n| int_text(n).is_some_and(|t| !t.starts_with('-')))
    });
    let Some(versions) = versions else {
        return Err(malformed("versions"));
    };
    let notes = view
        .get("stage_notes")
        .and_then(Value::as_object)
        .filter(|notes| {
            notes.iter().all(|(stage, n)| {
                PIPELINE_STAGES.contains(&stage.as_str())
                    && n.as_array().is_some_and(|n| {
                        !n.is_empty() && n.iter().all(|x| has_keys(x, ["source", "text"]))
                    })
            })
        });
    let Some(notes) = notes else {
        return Err(malformed("stage_notes"));
    };
    let mut prompts = Vec::new();
    for &stage in PIPELINE_STAGES {
        let mut envelopes = Vec::new();
        for note in notes
            .get(stage)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let source = note["source"].as_str();
            let matched = source
                .and_then(|s| s.strip_prefix("pipeline:"))
                .and_then(|s| s.split_once('@'))
                .filter(|(who, n)| {
                    (*who == "global" || *who == slug) && (*n == "0" || positive_int(n))
                });
            let Some((who, number)) = matched else {
                return Err(malformed("source"));
            };
            let key = if who == "global" { "global" } else { "project" };
            if int_text(&versions[key]).as_deref() != Some(number) {
                return Err(malformed("source"));
            }
            let source = source.unwrap_or_default();
            if let Some(reason) = check_note(&note["text"]) {
                return Err(fail(format!("pipeline note {source} ({stage}) {reason}; save it again from the dashboard"), 5));
            }
            envelopes.push(format!(
                "<operator-preferences source=\"{source}\" stage=\"{stage}\" trust=\"data\">\n{}\n</operator-preferences>",
                note["text"].as_str().unwrap_or_default()
            ));
        }
        if !envelopes.is_empty() {
            let sentence = if stage == "babysit" {
                BABYSIT_SENTENCE
            } else {
                PREFERENCES_SENTENCE
            };
            prompts.push((stage, format!("{sentence}\n\n{}", envelopes.join("\n\n"))));
        }
    }
    Ok(prompts)
}

/// Python's `fetch_pipeline`. With a trial, the project's note for the trial's stage reads the
/// champion while it still holds the challenger.
fn fetch_pipeline(
    home: &Path,
    slug: &str,
    trial: Option<&Value>,
) -> Res<Vec<(&'static str, String)>> {
    let r = api(
        home,
        "GET",
        &format!("/api/pipeline/effective/{slug}"),
        None,
        &[200, 404, 409],
        &[],
    )?;
    if r.status == 404 {
        return Err(fail(
            format!(
                "no pipeline view for {slug}: register the project, or deploy the current Worker"
            ),
            4,
        ));
    }
    if r.status == 409 {
        return Err(fail(format!("pipeline configuration for {slug} no longer validates; save it again from the dashboard"), 5));
    }
    let mut data = loads(&r.text()?)
        .map_err(|_| fail(format!("malformed pipeline view for {slug}: body"), 5))?;
    if let Some(trial) = trial {
        let stage = trial["stage"].as_str().unwrap_or_default();
        let prefix = format!("pipeline:{slug}@");
        let found = data
            .get("stage_notes")
            .and_then(|n| n.get(stage))
            .and_then(Value::as_array)
            .and_then(|entries| {
                entries.iter().position(|n| {
                    n.get("source")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.starts_with(&prefix))
                })
            });
        if let Some(index) = found {
            let challenger = trial.get("challenger").ok_or_else(|| {
                fail(
                    format!("trial {} has no challenger", py_str(&trial["id"])),
                    1,
                )
            })?;
            if data["stage_notes"][stage][index].get("text") == Some(challenger) {
                let champion = trial_champion(home, trial)?;
                let notes = data["stage_notes"].as_object_mut().unwrap();
                let entries = notes[stage].as_array_mut().unwrap();
                if !champion.is_empty() {
                    entries[index]["text"] = Value::from(champion);
                } else {
                    entries.remove(index);
                    if entries.is_empty() {
                        notes.shift_remove(stage);
                    }
                }
            }
        }
    }
    pipeline_view(slug, &data)
}

/// A well-formed trial record: a slug project, a pipeline stage, integer versions and its open_trial id.
fn trial_valid(t: &Value) -> bool {
    let Some(project) = t.get("project").and_then(Value::as_str) else {
        return false;
    };
    let Some(stage) = t
        .get("stage")
        .and_then(Value::as_str)
        .filter(|s| PIPELINE_STAGES.contains(s))
    else {
        return false;
    };
    let Some(version) = t.get("version").and_then(int_text) else {
        return false;
    };
    PROJECT_SLUG.is_full(project)
        && t.get("champion_version")
            .and_then(int_text)
            .is_some_and(|v| !v.starts_with('-'))
        && t.get("id").and_then(Value::as_str)
            == Some(format!("{project}-{stage}-v{version}").as_str())
        && t.get("opened")
            .and_then(Value::as_str)
            .and_then(parse_stamp)
            .is_some()
}

fn open_trial_for<'a>(data: &'a Value, slug: &str, stage: &str) -> Option<&'a Value> {
    data.get("trials")?.as_array()?.iter().find(|t| {
        trial_valid(t)
            && t.get("status").and_then(Value::as_str) == Some("open")
            && t["project"] == slug
            && t["stage"] == stage
    })
}

/// Whether the job is in the trial's arm a: started at or after the trial opened, and hashed to a.
fn in_arm_a(data: &Value, trial: &Value, job_id: &str) -> bool {
    let Some(opened) = trial
        .get("opened")
        .and_then(Value::as_str)
        .and_then(parse_stamp)
    else {
        return false;
    };
    let started = data["jobs"].as_array().into_iter().flatten().any(|j| {
        j.get("id").and_then(Value::as_str) == Some(job_id)
            && j.get("project") == trial.get("project")
            && j.get("started")
                .and_then(Value::as_str)
                .and_then(parse_stamp)
                .is_some_and(|s| s >= opened)
    });
    started && experiment_arm(&format!("{}/{job_id}", py_str(&trial["id"])), 50) == 'a'
}

/// Arm b when the key's stable hash bucket falls under percent, else a.
fn experiment_arm(key: &str, percent: u32) -> char {
    let digest = Sha256::digest(key.as_bytes());
    let bucket = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 100;
    if bucket < percent {
        'b'
    } else {
        'a'
    }
}

/// The trial stage's note in the pipeline version the edit replaced, from Worker history, or "".
fn trial_champion(home: &Path, trial: &Value) -> Res<String> {
    let version = int_text(&trial["champion_version"]).unwrap_or_default();
    if version == "0" {
        return Ok(String::new());
    }
    let project = trial["project"].as_str().unwrap_or_default();
    let r = api(
        home,
        "GET",
        &format!("/api/pipeline/history/projects/{project}/{version}"),
        None,
        &[200, 404],
        &[],
    )?;
    if r.status != 200 {
        return Ok(String::new());
    }
    let malformed = || {
        fail(
            format!("malformed pipeline history for {project} version {version}"),
            1,
        )
    };
    let Value::Object(body) = loads(&r.text()?).map_err(|_| malformed())? else {
        return Err(malformed());
    };
    let note = body
        .get("stage_notes")
        .and_then(|n| n.get(trial["stage"].as_str().unwrap_or_default()));
    Ok(note.and_then(Value::as_str).unwrap_or_default().to_string())
}

/// Write the prompt to out, refusing a link below home and making missing folders 0700.
fn write_out(home: &Path, out: &Path, text: &str) -> Res<()> {
    for node in out.ancestors() {
        if node == home {
            break;
        }
        if is_link(node).map_err(|e| io_fail(e, node))? {
            return Err(fail(
                format!(
                    "{} is a symlink or junction; nothing was written",
                    node.display()
                ),
                6,
            ));
        }
    }
    let missing: Vec<&Path> = out.ancestors().skip(1).filter(|p| !p.exists()).collect();
    for parent in missing.into_iter().rev() {
        mkdir_private(parent).map_err(|e| io_fail(e, parent))?;
    }
    write_atomic(out, text.as_bytes()).map_err(|e| io_fail(e, out))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| io_fail(e, out))?;
    }
    Ok(())
}

pub fn prompt_cmd(call: &Call) -> Res<()> {
    let role = role_alias(&call.args[1]).to_string();
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    if !PROMPT_ROLES.contains(&role.as_str()) {
        return Err(usage_error(&format!("bad role {}", repr(&role))));
    }
    let options = parse_flags(&role, &call.args[2..])?;
    let out = options
        .out
        .as_deref()
        .map(|o| out_path(&home, o))
        .transpose()?;

    let data = read_board(&home)?;
    let jobs: &[Value] = data["jobs"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let Some(job) = jobs
        .iter()
        .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id.as_str()))
    else {
        return Err(fail(
            format!("no job {job_id} on the board. Nothing was written."),
            4,
        ));
    };
    let project_file = project_files(&home, &slug)?.0;
    let documents = project_file.with_file_name(format!("{slug}.documents.md"));
    if !documents.is_file() {
        return Err(fail(
            format!(
                "no {}: run factory.py documents {slug} first",
                documents.display()
            ),
            4,
        ));
    }
    let review = role.starts_with("review-");
    let stage = if review { "review" } else { role.as_str() };
    let trial = open_trial_for(&data, &slug, stage).filter(|t| in_arm_a(&data, t, &job_id));
    let pipeline = fetch_pipeline(&home, &slug, trial)?;
    let meta = frontmatter(&project_file)
        .map_err(|e| io_fail(e, &project_file))?
        .0;
    let base = fm_get(&meta, "base")
        .filter(|b| !b.is_empty())
        .unwrap_or("main");
    let folder = home.join("jobs").join(&slug).join(&job_id);
    let worktrees = home.join("worktrees").join(&slug);
    let riders: Vec<&Value> = jobs
        .iter()
        .filter(|j| lead_of(j).as_deref() == Some(job_id.as_str()))
        .collect();
    let stacked = riders
        .iter()
        .find(|r| r.get("stacked") == Some(&Value::Bool(true)))
        .map(|r| py_str(&r["id"]));
    let tree_id = match &stacked {
        Some(s) if review || role == "approve" => s.clone(),
        _ => job_id.clone(),
    };
    let references = skill_root()?.join("references");
    let runtime = runtime_path()?;
    let temp = make_job_temp(&slug, &job_id)?;
    let mut lines = vec![
        format!("Job id: {job_id}"),
        format!("Job folder: {}", folder.display()),
        format!(
            "Worktree: {} (branch factory/{tree_id}, base branch {base})",
            worktrees.join(&tree_id).display()
        ),
        format!("Project file: {}", project_file.display()),
        format!("Project documents: {}", documents.display()),
        format!(
            "Visuals reference: {}",
            references.join("visuals.md").display()
        ),
        format!(
            "Walkthrough template: {}",
            references.join("walkthrough.html").display()
        ),
        format!("Runtime: {}", runtime.display()),
        format!("Temp folder: {}", temp.display()),
    ];
    let round_folder = options
        .round
        .as_ref()
        .map(|r| folder.join(format!("round-{r}")));
    // Python's `j.get("idea") or j.get("title")`.
    let idea_or_title = |j: &Value| {
        let idea = j.get("idea").unwrap_or(&Value::Null);
        if truthy(idea) {
            idea.clone()
        } else {
            j.get("title").cloned().unwrap_or(Value::Null)
        }
    };
    if role == "spec" {
        let feature = idea_or_title(job);
        if !truthy(&feature) {
            return Err(fail(format!("{job_id} has no idea or title to spec"), 4));
        }
        let images = folder.join("idea-images.md");
        if images.is_file() {
            lines.push(format!("Idea images: {}", images.display()));
        }
        if let Some(names) = job.get("idea_files").and_then(Value::as_array) {
            for (n, name) in names.iter().enumerate() {
                let Some(name) = name.as_str().filter(|n| IDEA_FILE_NAME.is_full(n)) else {
                    continue;
                };
                let file = folder.join(format!("idea-file-{name}"));
                if file.is_file() {
                    lines.push(format!("Idea file {}: {}", n + 1, file.display()));
                }
            }
        }
        lines.extend(["".into(), "Feature text:".into(), py_str(&feature)]);
        if !riders.is_empty() {
            lines.extend(["".into(), "Batched ideas (spec each one):".into()]);
            lines.extend(
                riders
                    .iter()
                    .map(|r| format!("- {}: {}", py_str(&r["id"]), py_str(&idea_or_title(r)))),
            );
        }
    } else if role == "build" {
        lines.push(format!("Builder name: builder-{job_id}"));
        if let Some(port) = &options.port {
            lines.push(format!("Dev-server port: {port}"));
        }
        if let Some(round_folder) = &round_folder {
            lines.push(format!("Round folder: {}", round_folder.display()));
        }
        if let Some(stacked) = &stacked {
            lines.push(format!(
                "Rider worktree: {} (branch factory/{stacked})",
                worktrees.join(stacked).display()
            ));
        }
        if let Some(block) = steer_block(&job_id, job.get("steer").unwrap_or(&Value::Null))? {
            lines.extend(["".into(), block]);
        }
    } else if review {
        let round_folder = round_folder.unwrap_or_default();
        lines.push(format!("Round folder: {}", round_folder.display()));
        lines.push(format!(
            "Your file: {}",
            round_folder.join(format!("{role}.md")).display()
        ));
    } else {
        lines.push(format!(
            "Your file: {}",
            folder.join("decision.md").display()
        ));
    }
    if let Some((_, text)) = pipeline.iter().find(|(s, _)| *s == stage) {
        lines.extend(["".into(), text.clone()]);
    }
    let text = lines.join("\n") + "\n";
    match out {
        None => print!("{text}"),
        Some(out) => {
            write_out(&home, &out, &text)?;
            println!("{}", out.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "hit the regex backtrack limit; stopping")]
    fn secret_check_stops_at_the_backtrack_limit() {
        crate::shared::pyre::TEST_LIMIT.with(|l| l.set(Some(0)));
        check_untrusted("ghp_ eyJ plain text");
    }
    #[cfg(unix)]
    use crate::shared::fs::temp_dir;
    use serde_json::json;
    use std::fs;

    #[test]
    fn normpath_matches_python() {
        let rows = [
            ("/a//b/./c/../d", "/a/b/d"),
            ("//x/y", "//x/y"),
            ("///x", "/x"),
            ("/..", "/"),
            ("a/../..", ".."),
            ("", "."),
        ];
        for (given, want) in rows {
            assert_eq!(normpath(given), want, "{given}");
        }
    }

    #[test]
    fn out_refuses_parent_components() {
        let prompts = Path::new("/h/prompts");
        assert!(under_prompts(prompts, Path::new("/h/prompts/sample/x.txt")));
        assert!(!under_prompts(prompts, Path::new("/h/prompts/../../x.txt")));
    }

    #[cfg(windows)]
    #[test]
    fn out_refuses_windows_dotdot() {
        let e = out_path(Path::new(r"C:\h"), r"C:\h\prompts\..\..\x").unwrap_err();
        assert_eq!(e.code, 2);
    }

    #[test]
    fn envelope_tag_folds_lookalikes() {
        let tagged = [
            "\u{ff1c}operator-preferences",
            "<\u{ff4f}perator-preferences",
            "<\u{3bf}\u{3c1}erator-preferences",
            "</ operator-preferences",
        ];
        for text in tagged {
            assert!(has_envelope_tag(text), "{text}");
        }
        assert!(!has_envelope_tag("operator preferences"));
    }

    fn trial(over: Value) -> Value {
        let mut t = json!({"project": "sample", "stage": "spec", "version": 8, "champion_version": 7,
                           "opened": "2026-01-01T00:00:00Z", "status": "open"});
        for (k, v) in over.as_object().unwrap() {
            t[k] = v.clone();
        }
        if t.get("id").is_none() {
            t["id"] = Value::from(format!(
                "{}-{}-v{}",
                py_str(&t["project"]),
                py_str(&t["stage"]),
                py_str(&t["version"])
            ));
        }
        t
    }

    #[test]
    fn malformed_trial_is_ignored() {
        let data = json!({"trials": [trial(json!({"id": "sample-spec-v9"})), trial(json!({"stage": "../x"})),
                                     trial(json!({"project": "../sample"})), trial(json!({"champion_version": "7"})),
                                     trial(json!({"project": "café"}))]});
        assert!(open_trial_for(&data, "sample", "spec").is_none());
        assert!(open_trial_for(&data, "café", "spec").is_none());
        assert!(
            open_trial_for(&json!({ "trials": [trial(json!({}))] }), "sample", "spec").is_some()
        );
    }

    #[test]
    fn experiment_arm_matches_python() {
        assert_eq!(experiment_arm("sample-spec-v8/2100-job", 50), 'b');
        assert_eq!(experiment_arm("sample-spec-v8/2102-job", 50), 'a');
    }

    #[cfg(unix)]
    #[test]
    fn out_refuses_linked_prompts() {
        let home = temp_dir("prompt-out-link");
        let real = home.join("elsewhere");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, home.join("prompts")).unwrap();
        let e = write_out(&home, &home.join("prompts").join("spec.txt"), "text\n").unwrap_err();
        assert_eq!(e.code, 6);
        assert!(e.stderr.contains("symlink or junction"));
        assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
    }
}
