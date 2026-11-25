//! claim and board, plus the claim helpers hold, handoff and other slices share. hold and handoff wait for the
//! sync slice.

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::{ErrorKind, IsTerminal, Read};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::cli::Call;
use crate::commands::checks::close_stacked_prs;
use crate::commands::intake::{spec_approval_record, COMMAND_APPROVED_NOTE};
use crate::commands::sync::{conflict_exit, pull_job, push_target, sync_problems, sync_state};
use crate::commands::tracer::{tracer_after_board, SPEC_APPROVED_NOTES};
use crate::shared::board::{batch_refs, dump_board, lead_of, load_board, patch_job, read_board};
use crate::shared::consts::{
    BUILD_STATES, IDEA, IN_FLIGHT, JOB_ID, NEW_JOB_ID, PASEO_ID, REF_KINDS, RELEASE, SLUG, TERMINAL,
};
use crate::shared::exit::{fail, Res};
use crate::shared::fs::{is_link, read_text};
use crate::shared::home::{factory_home, holder};
use crate::shared::http::cas;
use crate::shared::paseo::paseo_session;
use crate::shared::proc::{git_ok, origin_git};
use crate::shared::project::{r2_target, safe_job_folder};
use crate::shared::py::{os_error, repr, splitlines, strip, universal_newlines};
use crate::shared::pyjson::{dumps, loads, DEFAULT, INDENT2};
use crate::shared::pyre::{Flags, Pattern};
use crate::shared::pyval::{py_eq, py_int, py_repr, py_str, truthy};
use crate::shared::time::{elapsed_seconds, now, parse_stamp, unix_now};

static ID_DIGITS: Pattern = Pattern::new(r"(\d{3,})-", Flags::ASCII);

/// Give a session that has no tag a fresh one, and tell the orchestrator to reuse it.
pub(crate) fn new_session() {
    if std::env::var("FACTORY_SESSION")
        .unwrap_or_default()
        .is_empty()
    {
        let tag = format!(
            "{:06x}",
            RandomState::new().build_hasher().finish() & 0xff_ffff
        );
        std::env::set_var("FACTORY_SESSION", &tag);
        eprintln!("new session {tag}: pass --session {tag} to every later claim, next, hold, and board call");
    }
}

/// <home>/paseo-claims/<job-id>/<agent>, the marker that this host's Paseo session claimed the job.
pub(crate) fn paseo_claim_path(home: &Path, job_id: &str, agent: &str) -> Option<PathBuf> {
    let ok = JOB_ID.is_full(job_id)
        && !job_id.contains("..")
        && PASEO_ID.is_full(agent)
        && !agent.starts_with('-');
    ok.then(|| home.join("paseo-claims").join(job_id).join(agent))
}

pub(crate) fn record_paseo_claim(home: &Path, job_id: &str, paseo: &Value) {
    let Some(path) = paseo
        .get("agent")
        .and_then(Value::as_str)
        .and_then(|a| paseo_claim_path(home, job_id, a))
    else {
        return;
    };
    if fs::create_dir_all(path.parent().unwrap()).is_ok() {
        let _ = fs::OpenOptions::new().create(true).append(true).open(&path);
    }
}

pub(crate) fn new_claim(device: &str, session: &str, since: Option<&str>) -> Value {
    let stamp = now();
    json!({"device": device, "session": session, "since": since.filter(|s| !s.is_empty()).unwrap_or(&stamp), "seen": stamp})
}

/// The highest id the Worker has archived off the board, so no new job reuses it.
pub(crate) fn archive_floor(data: &Value) -> u128 {
    let Some(Value::Number(n)) = data.get("archived").and_then(|a| a.get("max_id")) else {
        return 0;
    };
    let text = n.to_string();
    if text.bytes().all(|b| b.is_ascii_digit()) {
        text.parse().unwrap_or(u128::MAX)
    } else {
        0
    }
}

pub(crate) fn next_job_id(names: &[String], feature_slug: &str, floor: u128) -> String {
    let used = names
        .iter()
        .filter_map(|n| ID_DIGITS.match_start(n))
        .map(|m| m[1].parse().unwrap_or(u128::MAX));
    format!(
        "{:03}-{feature_slug}",
        used.chain([floor]).max().unwrap_or(0).saturating_add(1)
    )
}

fn claim_stub(
    job_id: &str,
    project: &str,
    stamp: &str,
    claim: Value,
    paseo: Option<&Value>,
    refs: Option<Value>,
) -> Value {
    let mut stub = json!({"id": job_id, "project": project, "updated": stamp, "started": stamp, "claim": claim});
    if let Some(paseo) = paseo {
        stub["paseo"] = paseo.clone();
    }
    if let Some(refs) = refs.filter(|r| truthy(Some(r))) {
        stub["refs"] = refs;
    }
    stub
}

fn listing(folder: &Path) -> Vec<PathBuf> {
    fs::read_dir(folder)
        .map(|d| d.filter_map(|e| Some(e.ok()?.path())).collect())
        .unwrap_or_default()
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reserve the next id on the shared board with a stageless stub. Ids are unique across the whole factory, so
/// `#<id>` links and board lookups stay unambiguous. refs, when given, go on the stub.
pub(crate) fn claim_id(
    home: &Path,
    feature_slug: &str,
    project_slug: &str,
    refs: Option<Value>,
) -> Res<String> {
    for slug in [feature_slug, project_slug] {
        if !SLUG.is_full(slug) {
            return Err(fail(
                format!("bad slug {}: use letters, digits, - and _", repr(slug)),
                2,
            ));
        }
    }
    let paseo = paseo_session();
    let resolved = if paseo.is_some() {
        Some(factory_home()?)
    } else {
        None
    };
    let jobs = home.join("jobs");
    fs::create_dir_all(&jobs).map_err(|e| fail(os_error(&e, Some(&jobs)), 1))?;
    let mut local = Vec::new();
    for entry in listing(&jobs) {
        local.push(name_of(&entry));
        local.extend(listing(&entry).iter().map(|p| name_of(p)));
    }
    new_session();
    let (device, session, _) = holder(home)?;
    let job_id = cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let rows = jobs_of(&data)?;
        let mut names = local.clone();
        names.extend(
            rows.iter()
                .map(|j| py_str(j.get("id").unwrap_or(&json!("")))),
        );
        let job_id = next_job_id(&names, feature_slug, archive_floor(&data));
        let stamp = now();
        data["updated"] = Value::from(stamp.clone());
        let stub = claim_stub(
            &job_id,
            project_slug,
            &stamp,
            new_claim(&device, &session, None),
            paseo.as_ref(),
            refs.clone(),
        );
        data["jobs"].as_array_mut().unwrap().push(stub);
        Ok((Some(dump_board(&data)), job_id))
    })?;
    if let (Some(paseo), Some(resolved)) = (&paseo, &resolved) {
        record_paseo_claim(resolved, &job_id, paseo);
    }
    let folder = jobs.join(project_slug).join(&job_id);
    fs::create_dir_all(&folder).map_err(|e| fail(os_error(&e, Some(&folder)), 1))?;
    Ok(job_id)
}

pub fn claim(call: &Call) -> Res<()> {
    println!(
        "{}",
        claim_id(Path::new(&call.args[0]), &call.args[1], &call.args[2], None)?
    );
    Ok(())
}

/// The board's jobs list. Python raises when it is missing; the binary fails 1.
fn jobs_of(data: &Value) -> Res<&Vec<Value>> {
    data.get("jobs")
        .and_then(Value::as_array)
        .ok_or_else(|| fail("the board has no jobs list", 1))
}

pub(crate) const CLAIM_REFRESH_MINUTES: f64 = 10.0;
const REVIEWERS: [&str; 4] = ["security", "ux", "ui", "code"];
const TRACER_GATE_NOTE: &str = "gate: tracer-scope";
const TRACER_APPROVED_NOTE: &str = "spec approved in Tracer";
const RECEIPT_REFUSAL: &str = "without a Tracer-signed scope receipt";
static NOT_BEFORE: Pattern = Pattern::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", Flags::NONE);
static TRACER_JIRA: Pattern = Pattern::new(
    r"(?<![A-Za-z0-9])[A-Za-z][A-Za-z0-9]*-[1-9][0-9]*",
    Flags::NONE,
);
static TRACER_DIGITS: Pattern = Pattern::new(r"(\d{1,7})-", Flags::ASCII);
static APPROVE: Pattern = Pattern::new(r"APPROVE\b", Flags::NONE);

/// Keys only factory.py, pickup or the dashboard set, in Python's order: (key, refused only when non-null, why).
const REFUSED: &[(&str, bool, &str)] = &[
    ("claim", true, r#"only hold sets a claim. {"claim": null} releases it"#),
    ("paseo", false, "only factory.py sets paseo"),
    ("paseo_writer", false, "only factory.py sets paseo_writer"),
    ("handoff", false, "only factory.py sets handoff"),
    ("done", false, "only factory.py sets done"),
    ("resumes", false, "only factory.py sets resumes"),
    ("stacked", false, "only factory.py stack sets stacked"),
    ("origin", false, "only factory.py suggest sets origin"),
    ("started", false, "only factory.py sets started"),
    ("goal", false, "only factory.py push sets goal"),
    ("decision", false, "only factory.py push and pickup set decision"),
    ("agents", false, "only factory.py agent and pickup set agents"),
    ("move", false, "only the dashboard, factory.py pickup and factory.py control set move. To relay a person's chat, run factory.py control <job-id> move <host>|rerun --chat-file <file>"),
    ("priority", false, "only the dashboard, factory.py pickup and factory.py control set priority. To relay a person's chat, run factory.py control <job-id> prioritize --chat-file <file>"),
    ("paused", false, "only the dashboard and factory.py control set paused. To relay a person's chat, run factory.py control <job-id> pause|resume --chat-file <file>"),
    ("babysit", true, "only the dashboard and factory.py control set babysit; a session clears it with null. To relay a person's chat, run factory.py control <job-id> babysit --chat-file <file>"),
    ("resolve", true, "only the dashboard sets resolve; a session clears it with null. Nothing was written."),
    ("steer", false, "only the dashboard sets steer. Nothing was written."),
    ("close", false, "only the dashboard, factory.py pickup and factory.py control set close. To relay a person's chat, run factory.py control <job-id> close --reason-file <file> --chat-file <file>"),
    ("github", false, "only factory.py pickup sets github. Nothing was written."),
    ("token_cap", false, "only factory.py pickup sets token_cap. Nothing was written."),
    ("auto_babysit", false, "only factory.py pickup sets auto_babysit. Nothing was written."),
    ("launch_failure", false, "only factory.py pickup sets launch_failure. Nothing was written."),
    ("repeated", false, "only factory.py repeats sets repeated. Nothing was written."),
    ("checked_head", false, "only factory.py checks sets checked_head. Nothing was written."),
    ("handled", false, "only factory.py pr-handled sets handled. Nothing was written."),
    ("spec_approval", false, "only factory.py and the dashboard set spec_approval. Nothing was written."),
];

fn get_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// `value in (a, b, ...)` for string choices.
fn is_one_of(v: Option<&Value>, choices: &[&str]) -> bool {
    v.and_then(Value::as_str)
        .is_some_and(|s| choices.contains(&s))
}

fn or_null(v: Option<&Value>) -> &Value {
    v.unwrap_or(&Value::Null)
}

pub(crate) fn mine(c: &Value, device: &str, session: &str) -> bool {
    get_str(c, "device") == Some(device) && get_str(c, "session") == Some(session)
}

/// Seconds since the claim's seen stamp. Python raises on a bad stamp; the binary fails 1.
fn seen_age(c: &Value) -> Res<f64> {
    let seen = c.get("seen");
    let Some(stamp) = seen.and_then(Value::as_str).and_then(parse_stamp) else {
        return Err(fail(
            format!("bad claim seen stamp {}", py_repr(or_null(seen))),
            1,
        ));
    };
    Ok((unix_now() - stamp) as f64)
}

/// The job's claim if another session holds it and checked in within the last minutes.
pub(crate) fn blocking(
    job: Option<&Value>,
    device: &str,
    session: &str,
    minutes: u32,
) -> Res<Option<Value>> {
    let c = job
        .and_then(|j| j.get("claim"))
        .filter(|c| truthy(Some(*c)));
    if let Some(c) = c.filter(|c| !c.is_object()) {
        let kind = if c.is_array() {
            "list"
        } else if c.is_string() {
            "str"
        } else if c.is_boolean() {
            "bool"
        } else {
            "int"
        };
        return Err(fail(format!("'{kind}' object has no attribute 'get'"), 1));
    }
    let Some(c) = c.filter(|c| !mine(c, device, session)) else {
        return Ok(None);
    };
    Ok((seen_age(c)? < f64::from(minutes) * 60.0).then(|| c.clone()))
}

/// True when the claim's seen stamp is under CLAIM_REFRESH_MINUTES old.
pub(crate) fn seen_recently(c: &Value) -> Res<bool> {
    Ok(seen_age(c)? < CLAIM_REFRESH_MINUTES * 60.0)
}

/// True when after differs from before only in updated stamps, and in claim.seen on rows this session holds
/// whose stored claim was seen under CLAIM_REFRESH_MINUTES ago.
fn same_board(before: &Value, after: &Value, device: &str, session: &str) -> Res<bool> {
    let mut fresh = Vec::new();
    for j in jobs_of(before)? {
        let c = or_null(j.get("claim"));
        if c.is_object() && mine(c, device, session) && seen_recently(c)? {
            fresh.push(or_null(j.get("id")).clone());
        }
    }
    let quiet = |data: &Value| -> Value {
        let mut out = data.as_object().cloned().unwrap_or_default();
        out.shift_remove("updated");
        let rows = data["jobs"].as_array().into_iter().flatten().map(|j| {
            let mut j = j.clone();
            if let Some(row) = j.as_object_mut() {
                row.shift_remove("updated");
                if fresh.iter().any(|id| py_eq(id, or_null(row.get("id")))) {
                    if let Some(c) = row.get_mut("claim").and_then(Value::as_object_mut) {
                        c.shift_remove("seen");
                    }
                }
            }
            j
        });
        out.insert("jobs".into(), Value::Array(rows.collect()));
        Value::Object(out)
    };
    Ok(py_eq(&quiet(before), &quiet(after)))
}

fn earlier_moves(m: &Value) -> Vec<&Value> {
    m.get("earlier")
        .and_then(Value::as_array)
        .map(|e| e.iter().filter(|x| x.is_object()).collect())
        .unwrap_or_default()
}

/// True when the dashboard moved this job away from this holder's session.
pub(crate) fn moved_away(job: Option<&Value>, device: &str, session: &str) -> bool {
    let Some(m) = job.and_then(|j| j.get("move")).filter(|m| m.is_object()) else {
        return false;
    };
    std::iter::once(m)
        .chain(earlier_moves(m))
        .any(|x| get_str(x, "from") == Some(device) && get_str(x, "session") == Some(session))
}

pub(crate) fn moved_message(job_id: &str, m: &Value) -> String {
    format!(
        "{job_id} was moved to {} at {}. Nothing was written. Stop this session.",
        py_str(or_null(m.get("to"))),
        py_str(or_null(m.get("at")))
    )
}

pub(crate) fn held_by(job_id: &str, c: &Value) -> String {
    let field = |k: &str| py_str(or_null(c.get(k)));
    format!(
        "{job_id} is held by {} session {} since {}, last seen {}",
        field("device"),
        field("session"),
        field("since"),
        field("seen")
    )
}

/// True when the dashboard queued a close that pickup has not done yet.
pub(crate) fn closing(j: &Value) -> bool {
    j.get("close")
        .and_then(Value::as_object)
        .is_some_and(|c| c.get("at").is_some_and(Value::is_string) && !c.contains_key("closed"))
}

pub(crate) fn closing_message(job_id: &str) -> String {
    format!("{job_id} has a close queued from the dashboard. Nothing else was written. Stop this session.")
}

/// The stacked riders of lead_id that still have a PR, read before a close clears it.
pub(crate) fn open_stacked(data: &Value, lead_id: &str) -> Vec<Value> {
    data["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| lead_of(r).as_deref() == Some(lead_id))
        .filter(|r| r.get("stacked") == Some(&Value::Bool(true)))
        .filter(|r| get_str(r, "pr").is_some_and(|p| !p.is_empty()))
        .filter(|r| get_str(r, "project").is_some_and(|p| SLUG.is_full(p)))
        .filter(|r| !is_one_of(r.get("stage"), TERMINAL))
        .map(|r| json!({"id": or_null(r.get("id")), "project": or_null(r.get("project")), "pr": or_null(r.get("pr"))}))
        .collect()
}

fn check_refs(refs: Option<&Value>) -> Res<()> {
    let Some(refs) = refs.filter(|r| !r.is_null()) else {
        return Ok(());
    };
    let good = |r: &Value| {
        r.as_object()
            .is_some_and(|m| m.len() == 2 && m.contains_key("id") && m.contains_key("kind"))
            && get_str(r, "id").is_some_and(|id| JOB_ID.is_full(id))
            && is_one_of(r.get("kind"), REF_KINDS)
    };
    if refs.as_array().is_some_and(|list| list.iter().all(good)) {
        return Ok(());
    }
    Err(fail(
        format!(
            r#"refs must be a list of {{"id": "<job-id>", "kind": "{}"}}"#,
            REF_KINDS.join(" | ")
        ),
        2,
    ))
}

fn check_build(build: Option<&Value>) -> Res<()> {
    let Some(build) = build.filter(|b| !b.is_null()) else {
        return Ok(());
    };
    let good = build.as_object().is_some_and(|m| {
        m.keys().all(|k| k == "state" || k == "runs")
            && m.get("state")
                .map_or(true, |s| is_one_of(Some(s), BUILD_STATES))
            && match m.get("runs") {
                None | Some(Value::Null) => true,
                Some(Value::Array(runs)) => runs
                    .iter()
                    .all(|u| u.as_str().is_some_and(|u| u.starts_with("https://"))),
                _ => false,
            }
    });
    if good {
        return Ok(());
    }
    Err(fail(
        format!(
            r#"build must be {{"state": "{}", "runs": ["https://..."]}}"#,
            BUILD_STATES.join(" | ")
        ),
        2,
    ))
}

fn check_tracer(value: Option<&Value>) -> Res<()> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(());
    };
    let good = value.as_object().is_some_and(|m| {
        m.iter().all(|(k, v)| {
            (k == "follow" || k == "gather") && (v.is_null() || py_eq(v, &Value::Bool(true)))
        })
    });
    if good {
        return Ok(());
    }
    Err(fail(
        r#"tracer must be {"follow": true} or null, optionally with "gather": true; only factory.py sets tracer.feature, tracer.url, tracer.scope, tracer.receipt, tracer.building, tracer.stopped and tracer.tasks"#,
        2,
    ))
}

fn check_pipeline(value: Option<&Value>) -> Res<()> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(());
    };
    let natural = |v: &Value| py_int(v).is_some_and(|d| !d.starts_with('-')) && v.is_number();
    let good = value.as_object().is_some_and(|m| {
        m.len() == 2
            && m.contains_key("global")
            && m.contains_key("project")
            && m.values().all(natural)
    });
    if good {
        return Ok(());
    }
    Err(fail(
        r#"pipeline must be {"global": <version>, "project": <version>}"#,
        2,
    ))
}

fn check_not_before(value: Option<&Value>) -> Res<()> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(());
    };
    if value
        .as_str()
        .is_some_and(|s| NOT_BEFORE.is_full(s) && parse_stamp(s).is_some())
    {
        return Ok(());
    }
    Err(fail(
        "not_before must be a UTC time like 2026-11-17T09:00:00Z, or null. Nothing was written.",
        2,
    ))
}

/// Each present artifact's first line, refusing any linked file, parent or job folder.
pub(crate) fn job_file_lines(folder: &Path, artifacts: &[String]) -> Res<Vec<(String, String)>> {
    let mut lines = Vec::new();
    for artifact in artifacts {
        let path = folder.join(artifact);
        for node in [path.as_path(), path.parent().unwrap_or(folder), folder] {
            if is_link(node).map_err(|e| fail(os_error(&e, Some(node)), 1))? {
                return Err(fail(
                    format!(
                        "{} is a symlink or junction; nothing was read through it",
                        node.display()
                    ),
                    6,
                ));
            }
        }
        if path.is_file() {
            let bytes = fs::read(&path).map_err(|e| fail(os_error(&e, Some(&path)), 1))?;
            let text = universal_newlines(String::from_utf8_lossy(&bytes).into_owned());
            let first = splitlines(&text, false)
                .first()
                .copied()
                .unwrap_or("")
                .to_string();
            lines.push((artifact.clone(), first));
        }
    }
    Ok(lines)
}

/// Why a rounds patch cannot be written, or None. It may keep the stored rounds and append pending ones; only
/// round rewrite_round may differ from the stored entry.
pub(crate) fn rounds_refusal(
    job_id: &str,
    found: Option<&Value>,
    rounds: &Value,
    rewrite_round: Option<usize>,
) -> Option<String> {
    let Some(rounds) = rounds.as_array() else {
        return Some(format!(
            "{job_id} rounds must be a list of review rounds. Nothing was written."
        ));
    };
    let stored = found
        .and_then(|f| f.get("rounds"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if rounds.len() < stored.len() {
        return Some(format!(
            "{job_id} cannot drop review rounds: the board has {} and the patch sends {}. Nothing was written.",
            stored.len(),
            rounds.len()
        ));
    }
    let pending = Value::Object(
        REVIEWERS
            .iter()
            .map(|r| (r.to_string(), Value::from("pending")))
            .collect(),
    );
    for (i, entry) in rounds.iter().enumerate() {
        if i < stored.len() && !py_eq(entry, &stored[i]) && Some(i + 1) != rewrite_round {
            return Some(format!(
                "{job_id} cannot rewrite round {} in a board patch. Only factory.py verdict and round-wait change a recorded round. Nothing was written.",
                i + 1
            ));
        }
        if i >= stored.len() && !py_eq(entry, &pending) {
            return Some(format!(
                "{job_id} cannot add round {} with verdicts. A new round starts as {}. Record verdicts with factory.py verdict. Nothing was written.",
                i + 1,
                dumps(&pending, DEFAULT)
            ));
        }
    }
    None
}

/// Why the board cannot record value for reviewer in round n, or None: anything but pending needs
/// round-<n>/review-<reviewer>.md in the job folder to start with that VERDICT: line.
pub(crate) fn verdict_file_refusal(
    home: &Path,
    slug: &str,
    job_id: &str,
    n: usize,
    reviewer: &str,
    value: &str,
) -> Res<Option<String>> {
    if value == "pending" {
        return Ok(None);
    }
    let name = format!("round-{n}/review-{reviewer}.md");
    let lines = job_file_lines(
        &home.join("jobs").join(slug).join(job_id),
        std::slice::from_ref(&name),
    )?;
    Ok(match lines.first() {
        None => Some(format!("{job_id} round {n} {reviewer} cannot be {value}: {name} is not in the job folder. Nothing was written.")),
        Some((_, line)) if *line != format!("VERDICT: {value}") => Some(format!(
            "{job_id} round {n} {reviewer} cannot be {value}: the first line of {name} is not VERDICT: {value}. Nothing was written."
        )),
        Some(_) => None,
    })
}

/// Why a board write cannot move the job to approve or set automerge true, or None.
pub(crate) fn move_refusal(
    home: &Path,
    job_id: &str,
    found: Option<&Value>,
    patch: &Map<String, Value>,
) -> Option<String> {
    if patch.get("stage").and_then(Value::as_str) == Some("approve") {
        let rounds = if patch.contains_key("rounds") {
            patch.get("rounds")
        } else {
            found.and_then(|f| f.get("rounds"))
        };
        let last = rounds
            .and_then(Value::as_array)
            .and_then(|r| r.last())
            .filter(|r| r.is_object());
        let Some(last) = last else {
            return Some(format!("{job_id} cannot move to approve: it has no review round on the board. Nothing was written."));
        };
        let failing: Vec<String> = REVIEWERS
            .iter()
            .filter(|r| !is_one_of(last.get(**r), &["PASS", "SKIPPED"]))
            .map(|r| format!("{r} {}", last.get(*r).map_or("missing".to_string(), py_str)))
            .collect();
        if !failing.is_empty() {
            return Some(format!(
                "{job_id} cannot move to approve: round {} has {}. Every verdict in the latest round must be PASS or SKIPPED. Nothing was written.",
                rounds.and_then(Value::as_array).map_or(0, Vec::len),
                failing.join(", ")
            ));
        }
    }
    if patch.get("automerge") == Some(&Value::Bool(true)) {
        let slug = if patch.contains_key("project") {
            patch.get("project")
        } else {
            found.and_then(|f| f.get("project"))
        };
        let Some(slug) = slug.and_then(Value::as_str).filter(|s| SLUG.is_full(s)) else {
            return Some(format!(
                "{job_id} cannot set automerge true: it has no project, so there is no decision.md to check. Nothing was written."
            ));
        };
        let decision = home
            .join("jobs")
            .join(slug)
            .join(job_id)
            .join("decision.md");
        let unreadable = format!("{job_id} cannot set automerge true: its decision.md cannot be read. Nothing was written.");
        if is_link(&decision).unwrap_or(true) {
            return Some(unreadable);
        }
        let text = match read_text(&decision) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Some(format!(
                    "{job_id} cannot set automerge true: decision.md is not on this host. Run factory.py pull {slug}/{job_id}, then retry. Nothing was written."
                ))
            }
            Err(_) => return Some(unreadable),
        };
        let first = splitlines(&text, false)
            .into_iter()
            .map(strip)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        if APPROVE.match_start(first).is_none() {
            let head: String = first.chars().take(40).collect();
            return Some(format!(
                "{job_id} cannot set automerge true: decision.md starts with {}, not APPROVE. Nothing was written.",
                repr(&head)
            ));
        }
    }
    None
}

/// True when a patch holds a job at the spec-approval gate after a person already approved its scope in Tracer.
fn tracer_gate_passed(job: &Value, patch: &Map<String, Value>) -> bool {
    is_one_of(patch.get("stage"), &["needs-human"])
        && is_one_of(
            patch.get("note"),
            &["gate: spec-approval", TRACER_GATE_NOTE],
        )
        && job.get("tracer").and_then(|t| get_str(t, "scope")) == Some("approved")
}

/// (ticket, feature id): the Jira key when the board has one, else FACTORY-<n> from the job id's leading
/// digits, else the job id.
pub(crate) fn tracer_ids(job: &Value) -> (String, String) {
    if let Some(jira) = get_str(job, "jira").filter(|j| TRACER_JIRA.is_full(j)) {
        return (jira.to_uppercase(), jira.to_lowercase());
    }
    let id = py_str(or_null(job.get("id")));
    if let Some(m) = TRACER_DIGITS.match_start(&id) {
        let n: u64 = m[1].parse().unwrap_or(0);
        return (format!("FACTORY-{n}"), format!("factory-{n}"));
    }
    (id.to_uppercase(), id)
}

/// cas on the board with change(text, signed, sent), where change appends the job id to sent when it moves a
/// job to build on Tracer's receipt. When the Worker refuses that write for its receipt, retry once unsigned so
/// the job waits for a person.
fn receipt_cas<T>(
    home: &Path,
    mut change: impl FnMut(Option<&str>, bool, &mut Vec<String>) -> Res<(Option<String>, T)>,
) -> Res<T> {
    let mut sent = Vec::new();
    match cas(home, "/api/board", |text| change(text, true, &mut sent)) {
        Err(e)
            if !sent.is_empty()
                && e.status.as_ref().is_some_and(|(code, detail)| {
                    *code == 400 && detail.contains(RECEIPT_REFUSAL)
                }) =>
        {
            eprint!("{}", e.stderr);
        }
        other => return other,
    }
    let result = cas(home, "/api/board", |text| {
        change(text, false, &mut Vec::new())
    })?;
    eprintln!(
        "factory.py: the Worker refused Tracer's receipt (check tracer_receipt_key); {} waits at gate: spec-approval instead",
        sent.last().unwrap()
    );
    Ok(result)
}

pub fn board(call: &Call) -> Res<()> {
    let mut stdin = std::io::stdin();
    let mut bytes = Vec::new();
    if !stdin.is_terminal() {
        stdin
            .read_to_end(&mut bytes)
            .map_err(|e| fail(os_error(&e, None), 1))?;
    }
    let raw =
        universal_newlines(String::from_utf8(bytes).map_err(|_| fail("stdin must be UTF-8", 2))?);
    let raw = strip(&raw);
    if raw.is_empty() {
        return Err(fail(
            "usage: board <factory-home> <job-id> with a JSON object on stdin. stdin was empty, so nothing was written.",
            2,
        ));
    }
    let patch = loads(raw).map_err(|e| {
        let msg = py_json_error(raw);
        fail(
            format!(
                "stdin is not JSON: {}",
                if msg.is_empty() { e.to_string() } else { msg }
            ),
            2,
        )
    })?;
    let Value::Object(patch) = patch else {
        return Err(fail("stdin must be a JSON object", 2));
    };
    board_write(
        Path::new(&call.args[0]),
        &call.args[1],
        patch,
        &WriteOpts {
            show: true,
            verdict: None,
            rewrite_round: None,
        },
    )?;
    Ok(())
}

/// One reviewer's verdict for round n, which the verdict command records through `board_write`.
#[allow(dead_code)]
pub(crate) struct Verdict {
    pub slug: String,
    pub n: usize,
    pub reviewer: String,
    pub value: String,
}

pub(crate) struct WriteOpts {
    pub show: bool,
    pub verdict: Option<Verdict>,
    pub rewrite_round: Option<usize>,
}

enum Held {
    Claim(Value),
    Closing,
}

/// Validate a board patch, then merge it into the job's row in one CAS write. Prints the job when opts.show.
pub(crate) fn board_write(
    home: &Path,
    job_id: &str,
    mut patch: Map<String, Value>,
    opts: &WriteOpts,
) -> Res<Value> {
    for (key, non_null, why) in REFUSED {
        if patch.get(*key).is_some_and(|v| !(*non_null && v.is_null())) {
            return Err(fail(why, 2));
        }
    }
    if let Some(note) = patch
        .get("note")
        .and_then(Value::as_str)
        .filter(|n| SPEC_APPROVED_NOTES.contains(n))
    {
        return Err(fail(
            format!("only a person approves a spec, on the dashboard or in Tracer, so factory.py board cannot set the note '{note}'. Nothing was written."),
            2,
        ));
    }
    if patch.contains_key("id") {
        return Err(fail("the job id comes from the <job-id> argument, so the patch cannot set id. Nothing was written.", 2));
    }
    if patch
        .get("backlog")
        .and_then(Value::as_object)
        .is_some_and(|b| b.contains_key("notes_read"))
    {
        return Err(fail(
            "only factory.py deny-notes sets backlog.notes_read",
            2,
        ));
    }
    for key in ["escalated", "automerge"] {
        let v = or_null(patch.get(key));
        if !(v.is_null() || py_eq(v, &Value::Bool(true)) || py_eq(v, &Value::Bool(false))) {
            return Err(fail(format!("{key} must be true, false, or null"), 2));
        }
    }
    if patch
        .get("needs")
        .is_some_and(|n| !n.is_null() && !n.is_string())
    {
        return Err(fail("needs must be a string or null", 2));
    }
    check_refs(patch.get("refs"))?;
    let self_ref = json!({"id": job_id, "kind": "depends-on"});
    if patch
        .get("refs")
        .and_then(Value::as_array)
        .is_some_and(|refs| refs.iter().any(|r| py_eq(r, &self_ref)))
    {
        return Err(fail(format!("a job cannot depend on itself: drop the depends-on ref to {job_id}. Nothing was written."), 2));
    }
    check_build(patch.get("build"))?;
    check_tracer(patch.get("tracer"))?;
    check_pipeline(patch.get("pipeline"))?;
    check_not_before(patch.get("not_before"))?;
    let (device, session, minutes) = holder(home)?;
    let paseo = paseo_session();
    let resolved = if paseo.is_some() {
        Some(factory_home()?)
    } else {
        None
    };
    let mirrors = opts.verdict.is_some()
        || patch.contains_key("stage")
        || patch.contains_key("rounds")
        || patch.get("tracer").and_then(|t| t.get("follow")) == Some(&Value::Bool(true));
    let (job, held, stacked) = receipt_cas(home, |text, signed, sent| {
        let mut data = load_board(text)?;
        jobs_of(&data)?;
        let before = data.clone();
        let index = |data: &Value| {
            data["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .position(|j| get_str(j, "id") == Some(job_id))
        };
        let found = index(&data).map(|i| data["jobs"][i].clone());
        let found = found.as_ref();
        if let Some(Verdict {
            slug,
            n,
            reviewer,
            value,
        }) = &opts.verdict
        {
            if let Some(f) = found.filter(|f| get_str(f, "project") != Some(slug)) {
                return Err(fail(
                    format!(
                        "{job_id} belongs to {}, not {slug}",
                        py_str(or_null(f.get("project")))
                    ),
                    2,
                ));
            }
            let rounds = found
                .and_then(|f| f.get("rounds"))
                .and_then(Value::as_array);
            let Some(rounds) =
                rounds.filter(|r| r.get(n.wrapping_sub(1)).is_some_and(Value::is_object))
            else {
                return Err(fail(
                    format!("{job_id} has no rounds entry {n} on the board. Nothing was written."),
                    4,
                ));
            };
            if let Some(refusal) = verdict_file_refusal(home, slug, job_id, *n, reviewer, value)? {
                return Err(fail(refusal, 4));
            }
            let mut rounds = rounds.clone();
            rounds[n - 1][reviewer.as_str()] = Value::from(value.as_str());
            patch.insert("rounds".into(), Value::Array(rounds));
        }
        if found.is_none() {
            let floor = archive_floor(&data);
            if let Some(m) = ID_DIGITS.match_start(job_id) {
                if m[1].parse::<u128>().is_ok_and(|n| n <= floor) {
                    return Err(fail(
                        format!("ids up to {floor} belong to archived jobs, so {job_id} cannot go back on the board. Read it with factory.py job {job_id}. Claim new ids with claim. Nothing was written."),
                        2,
                    ));
                }
            }
            if !NEW_JOB_ID.is_full(job_id) {
                return Err(fail(
                    format!("bad job id {}: a new job id is digits, a dash and a lowercase slug, like 003-rate-limiting. Nothing was written.", repr(job_id)),
                    2,
                ));
            }
            if !truthy(patch.get("stage")) {
                return Err(fail(
                    format!("{job_id} is not on the board, and a new row must set stage. Claim new ids with claim. Nothing was written."),
                    2,
                ));
            }
        }
        if let Some(c) = blocking(found, &device, &session, minutes)? {
            return Ok((None, (None, Some(Held::Claim(c)), Vec::new())));
        }
        if moved_away(found, &device, &session) {
            return Err(fail(moved_message(job_id, &found.unwrap()["move"]), 6));
        }
        if let Some(f) = found.filter(|f| closing(f)) {
            if !mine(or_null(f.get("claim")), &device, &session) {
                return Err(fail(closing_message(job_id), 6));
            }
            let i = index(&data).unwrap();
            data["jobs"][i]
                .as_object_mut()
                .unwrap()
                .shift_remove("claim");
            patch_job(&mut data, job_id, &json!({}));
            return Ok((
                Some(dump_board(&data)),
                (None, Some(Held::Closing), Vec::new()),
            ));
        }
        let origin = found
            .and_then(|f| f.get("origin"))
            .filter(|o| o.is_object());
        if let Some(origin) = origin {
            if get_str(origin, "gate") == Some("human")
                && !truthy(origin.get("approved"))
                && patch.contains_key("stage")
                && !is_one_of(patch.get("stage"), &[IDEA, "closed"])
            {
                return Err(fail(format!("{job_id} waits on a human decision"), 2));
            }
        }
        if patch.contains_key("stage") && !truthy(patch.get("stage")) {
            return Err(fail(
                format!(
                    r#"a board write cannot clear {job_id}'s stage. Close the job with {{"stage": "closed"}} instead. Nothing was written."#
                ),
                2,
            ));
        }
        let refusal = match patch.get("rounds") {
            Some(rounds) if opts.verdict.is_none() => {
                rounds_refusal(job_id, found, rounds, opts.rewrite_round)
            }
            _ => None,
        };
        if let Some(refusal) = refusal.or_else(|| move_refusal(home, job_id, found, &patch)) {
            return Err(fail(refusal, 2));
        }
        if let Some(refs) = patch.get("refs") {
            if batch_refs(refs) != batch_refs(or_null(found.and_then(|f| f.get("refs")))) {
                return Err(fail(
                    format!("only factory.py batch and the lead's mirror add or remove a batched-into ref. A refs patch for {job_id} must keep exactly the batched-into refs it has now, none for a new row. Nothing was written."),
                    2,
                ));
            }
        }
        let stacked = match found {
            Some(f)
                if is_one_of(patch.get("stage"), &["closed"])
                    && !is_one_of(f.get("stage"), TERMINAL)
                    && lead_of(f).is_none() =>
            {
                open_stacked(&data, job_id)
            }
            _ => Vec::new(),
        };
        let (babysit, priority) = (
            found.and_then(|f| f.get("babysit")),
            found.and_then(|f| f.get("priority")),
        );
        let pressed = patch.get("babysit").is_some_and(Value::is_null)
            && babysit.is_some_and(Value::is_object)
            && priority.is_some_and(Value::is_object)
            && py_eq(
                or_null(priority.unwrap().get("at")),
                or_null(babysit.unwrap().get("at")),
            );
        let approving = found.is_some_and(|f| {
            get_str(f, "stage") == Some("needs-human")
                && get_str(f, "note") == Some("gate: spec-approval")
        }) && is_one_of(patch.get("stage"), &["build"])
            && is_one_of(patch.get("note"), &[COMMAND_APPROVED_NOTE]);
        patch_job(&mut data, job_id, &Value::Object(patch.clone()));
        let i = index(&data).unwrap();
        let job = data["jobs"][i].as_object_mut().unwrap();
        if approving {
            let record = spec_approval_record("command", &job["updated"]);
            job.insert("spec_approval".into(), record);
        }
        if pressed {
            job.shift_remove("priority");
        }
        let held_here =
            |job: &Map<String, Value>| mine(or_null(job.get("claim")), &device, &session);
        if is_one_of(patch.get("stage"), RELEASE) {
            job.shift_remove("claim");
        } else if patch.get("not_before").is_some_and(Value::is_string)
            && !is_one_of(job.get("stage"), TERMINAL)
            && held_here(job)
        {
            job.shift_remove("claim");
        } else if held_here(job) {
            let updated = job["updated"].clone();
            job["claim"]["seen"] = updated;
            if let Some(paseo) = &paseo {
                job.insert("paseo".into(), paseo.clone());
            }
        }
        if let Some(paseo) = &paseo {
            if !job.get("paseo").is_some_and(Value::is_object)
                && !job.get("paseo_writer").is_some_and(|w| py_eq(w, paseo))
            {
                job.insert("paseo_writer".into(), paseo.clone());
            }
        }
        if tracer_gate_passed(&Value::Object(job.clone()), &patch) {
            if !signed {
                job["tracer"]
                    .as_object_mut()
                    .unwrap()
                    .shift_remove("receipt");
            }
            let tracer = job["tracer"].clone();
            if truthy(tracer.get("receipt")) {
                job.insert("stage".into(), Value::from("build"));
                job.insert("note".into(), Value::from(TRACER_APPROVED_NOTE));
                job.shift_remove("needs");
                let record = spec_approval_record("tracer", &job["updated"]);
                job.insert("spec_approval".into(), record);
                sent.push(job_id.to_string());
            } else {
                let feature = match tracer.get("feature").filter(|f| truthy(Some(*f))) {
                    Some(f) => py_str(f),
                    None => tracer_ids(&Value::Object(job.clone())).1,
                };
                job.insert("note".into(), Value::from("gate: spec-approval"));
                let needs = format!("Tracer approved scope for {feature} without a signed receipt: run /factory approve-spec {job_id} or press Approve spec to build");
                job.insert("needs".into(), Value::from(needs));
            }
        }
        if same_board(&before, &data, &device, &session)? {
            let row = before["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|j| get_str(j, "id") == Some(job_id))
                .cloned();
            return Ok((None, (row, None, Vec::new())));
        }
        let job = data["jobs"][i].clone();
        Ok((Some(dump_board(&data)), (Some(job), None, stacked)))
    })?;
    match held {
        Some(Held::Closing) => return Err(fail(closing_message(job_id), 6)),
        Some(Held::Claim(c)) => {
            return Err(fail(
                format!("{}. Nothing was written. Pass --session <tag> if this session holds it, or take it over with hold <project>/<id> take", held_by(job_id, &c)),
                6,
            ))
        }
        None => {}
    }
    let job = job.unwrap_or(Value::Null);
    if let (Some(paseo), Some(resolved)) = (&paseo, &resolved) {
        if truthy(Some(&job)) && job.get("paseo").is_some_and(|p| py_eq(p, paseo)) {
            record_paseo_claim(resolved, job_id, paseo);
        }
    }
    if opts.show {
        println!("{}", dumps(&job, INDENT2));
    }
    close_stacked_prs(home, job_id, &stacked, &device)?;
    if mirrors {
        tracer_after_board(home, &job);
    }
    Ok(job)
}

/// True when hold would only refresh this session's claim, seen within CLAIM_REFRESH_MINUTES, with no row left
/// to link.
fn fresh_hold(
    job: &Value,
    device: &str,
    session: &str,
    paseo: Option<&Value>,
    take: Option<&str>,
) -> Res<bool> {
    let c = or_null(job.get("claim"));
    if take.is_some() || !truthy(c) || !mine(c, device, session) || job.get("started").is_none() {
        return Ok(false);
    }
    let ready = job
        .get("handoff")
        .and_then(|h| h.get("state"))
        .and_then(Value::as_str)
        == Some("ready");
    if ready || !py_eq(or_null(job.get("paseo")), paseo.unwrap_or(&Value::Null)) {
        return Ok(false);
    }
    let unlinked = job
        .get("agents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|row| {
            row.is_object()
                && get_str(row, "session") == Some(session)
                && row.get("paseo").is_none()
        });
    if paseo.is_some() && unlinked {
        return Ok(false);
    }
    seen_recently(c)
}

/// Set paseo on each of job's agents rows that session recorded and no session has linked yet.
pub(crate) fn link_agent_rows(job: &mut Value, session: &str, paseo: &Value) {
    let Some(rows) = job.get_mut("agents").and_then(Value::as_array_mut) else {
        return;
    };
    for row in rows {
        if row.is_object() && get_str(row, "session") == Some(session) && row.get("paseo").is_none()
        {
            row["paseo"] = paseo.clone();
        }
    }
}

enum Hold {
    Missing,
    Held(Value),
    Ok(Option<Value>, Value, bool),
}

/// Claim target for this session, or take it over with take. released accepts a handoff packet.
pub(crate) fn hold_target(target: &str, take: Option<&str>) -> Res<()> {
    if !matches!(take, None | Some("take" | "released")) {
        return Err(fail(
            "usage: hold <project-slug>/<job-id> [take|released]",
            2,
        ));
    }
    let (home, slug, job_id) = r2_target(target, true)?;
    new_session();
    let (device, session, minutes) = holder(&home)?;
    let paseo = paseo_session();
    let result = cas(&home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(i) = jobs_of(&data)?
            .iter()
            .position(|j| get_str(j, "id") == Some(&job_id))
        else {
            return Ok((None, Hold::Missing));
        };
        let job = data["jobs"][i].clone();
        if get_str(&job, "project") != Some(&slug) {
            return Err(fail(
                format!(
                    "{job_id} belongs to {}, not {slug}",
                    py_str(job.get("project"))
                ),
                2,
            ));
        }
        if take != Some("take") && moved_away(Some(&job), &device, &session) {
            return Err(fail(moved_message(&job_id, &job["move"]), 6));
        }
        if closing(&job) {
            return Err(fail(closing_message(&job_id), 6));
        }
        let stage = job.get("stage");
        let asked = get_str(&job, "stage") == Some("pr-open")
            && job
                .get("babysit")
                .is_some_and(|b| b.get("at").is_some_and(Value::is_string));
        let in_flight = stage
            .and_then(Value::as_str)
            .is_some_and(|s| IN_FLIGHT.contains(&s));
        if take == Some("released") && !in_flight {
            return Err(fail(
                format!(
                    "hold released is only for {}; {} stays held",
                    IN_FLIGHT.join(", "),
                    py_str(stage)
                ),
                2,
            ));
        }
        let packet = job
            .get("handoff")
            .filter(|h| truthy(*h))
            .cloned()
            .unwrap_or(json!({}));
        let round = |v: &Value| v.get("round").cloned().unwrap_or(json!(0));
        if take == Some("released")
            && get_str(&packet, "state") == Some("ready")
            && !(py_eq(or_null(packet.get("stage")), or_null(stage))
                && py_eq(&round(&packet), &round(&job)))
        {
            return Err(fail(
                format!(
                    "the board moved since the handoff: it recorded stage {} round {}, but the board now has stage {} round {}. Nothing was claimed",
                    py_str(packet.get("stage")),
                    py_str(&round(&packet)),
                    py_str(stage),
                    py_str(&round(&job))
                ),
                6,
            ));
        }
        let claim = or_null(job.get("claim"));
        let mut held = blocking(Some(&job), &device, &session, minutes)?;
        if take == Some("released") && truthy(claim) && !mine(claim, &device, &session) {
            held = Some(claim.clone());
        }
        if let Some(held) = held.filter(|_| take != Some("take")) {
            return Ok((None, Hold::Held(held)));
        }
        if fresh_hold(&job, &device, &session, paseo.as_ref(), take)? {
            return Ok((None, Hold::Ok(None, job["started"].clone(), asked)));
        }
        let c = job
            .get("claim")
            .filter(|c| truthy(*c))
            .cloned()
            .unwrap_or(json!({}));
        let keep = truthy(&c) && mine(&c, &device, &session);
        data["jobs"][i]
            .as_object_mut()
            .unwrap()
            .shift_remove("claim");
        let since = if keep {
            c.get("since").and_then(Value::as_str)
        } else {
            None
        };
        patch_job(
            &mut data,
            &job_id,
            &json!({"claim": new_claim(&device, &session, since)}),
        );
        let job = &mut data["jobs"][i];
        match &paseo {
            Some(paseo) => {
                job["paseo"] = paseo.clone();
                link_agent_rows(job, &session, paseo);
            }
            None => {
                job.as_object_mut().unwrap().shift_remove("paseo");
            }
        }
        if get_str(or_null(job.get("handoff")), "state") == Some("ready") {
            let handoff = job["handoff"].as_object_mut().unwrap();
            handoff.insert("state".into(), json!("accepted"));
            handoff.insert("to".into(), new_claim(&device, &session, None));
            handoff.insert("accepted".into(), json!(now()));
        }
        let started = job["started"].clone();
        let other = (!keep).then_some(c);
        Ok((Some(dump_board(&data)), Hold::Ok(other, started, asked)))
    })?;
    let (other, started, asked) = match result {
        Hold::Missing => {
            return Err(fail(
                format!("no job {job_id} on the board. Nothing was written."),
                4,
            ))
        }
        Hold::Held(c) => {
            return Err(fail(
                format!(
                    "{}. Take it over only if the user says so, with hold {target} take",
                    held_by(&job_id, &c)
                ),
                6,
            ))
        }
        Hold::Ok(other, started, asked) => (other, started, asked),
    };
    println!("held {target} as {device} session {session}");
    if let Some(paseo) = &paseo {
        record_paseo_claim(&home, &job_id, paseo);
    }
    let elapsed = started
        .as_str()
        .and_then(elapsed_seconds)
        .ok_or_else(|| fail(format!("bad started stamp {}", py_repr(&started)), 1))?;
    println!("elapsed {elapsed}s");
    if asked {
        println!("babysit requested: Babysit PR was pressed on {job_id}; run /factory babysit {job_id} --session {session} in this session now");
    }
    if let Some(other) = other.filter(|o| truthy(o)) {
        println!(
            "took over from {} session {}, last seen {}",
            py_str(other.get("device")),
            py_str(other.get("session")),
            py_str(other.get("seen"))
        );
    }
    let (line, conflicts) = pull_job(&home, &slug, &job_id)?;
    println!("{line}");
    if !conflicts.is_empty() {
        return Err(conflict_exit(&conflicts));
    }
    if take == Some("released") {
        let (folder, manifest, remote) = sync_state(&home, &slug, &job_id)?;
        if let Some(problem) = sync_problems(&folder, &manifest, &remote)?.first() {
            return Err(fail(
                format!("this device's job artifacts do not match R2: {problem}. The claim may already be held by this session and is not rolled back. Reconcile the named artifacts locally (merge or delete, then push), or stop at needs-human. Do not dispatch any work on unsynced artifacts"),
                6,
            ));
        }
    }
    Ok(())
}

pub fn hold(call: &Call) -> Res<()> {
    hold_target(&call.args[0], call.args.get(1).map(String::as_str))
}

pub fn handoff(call: &Call) -> Res<()> {
    let target = &call.args[0];
    let (home, slug, job_id) = r2_target(target, true)?;
    let (device, session, _) = holder(&home)?;
    let owned = |data: &Value| -> Res<usize> {
        let i = jobs_of(data)?
            .iter()
            .position(|j| get_str(j, "id") == Some(&job_id));
        let Some(i) = i.filter(|i| get_str(&data["jobs"][*i], "project") == Some(&slug)) else {
            return Err(fail(format!("no job {target}"), 4));
        };
        let job = &data["jobs"][i];
        let claim = job
            .get("claim")
            .filter(|c| truthy(*c))
            .cloned()
            .unwrap_or(json!({}));
        if session.is_empty() || !mine(&claim, &device, &session) {
            return Err(fail(
                format!("handoff requires {target}'s current claim and --session tag"),
                6,
            ));
        }
        if !get_str(job, "stage").is_some_and(|s| IN_FLIGHT.contains(&s)) {
            return Err(fail(
                "handoff is only for spec, build, review, or approve; human and PR gates stay held",
                2,
            ));
        }
        Ok(i)
    };
    let board = read_board(&home)?;
    let before = board["jobs"][owned(&board)?].clone();
    let folder = safe_job_folder(&home, &slug, &job_id)?;
    let checkpoint = folder.join("handoff.md");
    let written = !is_link(&checkpoint).unwrap_or(true)
        && checkpoint.is_file()
        && !strip(&read_text(&checkpoint).map_err(|e| fail(os_error(&e, Some(&checkpoint)), 1))?)
            .is_empty();
    if !written {
        return Err(fail(
            format!(
                "write a nonempty {} before handing off",
                checkpoint.display()
            ),
            2,
        ));
    }
    let wt = home.join("worktrees").join(&slug).join(&job_id);
    let branch = format!("factory/{job_id}");
    if git_ok(&wt, &["branch", "--show-current"])? != branch {
        return Err(fail(
            format!("handoff needs {branch} checked out in {}", wt.display()),
            6,
        ));
    }
    if !git_ok(&wt, &["status", "--porcelain"])?.is_empty() {
        return Err(fail(
            format!(
                "{} has uncommitted files; commit them before handoff",
                wt.display()
            ),
            6,
        ));
    }
    let head = git_ok(&wt, &["rev-parse", "HEAD"])?;
    let remote = origin_git(
        &wt,
        &["ls-remote", "origin", &format!("refs/heads/{branch}")],
    )?;
    if remote.split_whitespace().next() != Some(head.as_str()) {
        return Err(fail(
            format!("push {branch} to origin before handoff; its remote head must match {head}"),
            6,
        ));
    }
    push_target(target)?;
    let (folder, manifest, remote_files) = sync_state(&home, &slug, &job_id)?;
    if let Some(problem) = sync_problems(&folder, &manifest, &remote_files)?.first() {
        return Err(fail(
            format!("{problem}; pull and reconcile before handing off"),
            6,
        ));
    }
    let without_goal = |j: &Value| {
        let mut j = j.as_object().cloned().unwrap_or_default();
        j.shift_remove("goal");
        Value::Object(j)
    };
    let result = cas(&home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let i = owned(&data)?;
        let job = data["jobs"][i].clone();
        if !py_eq(&without_goal(&job), &without_goal(&before)) {
            return Err(fail(
                "job changed while publishing the handoff; inspect it before retrying",
                6,
            ));
        }
        let mut packet = json!({"state": "ready", "from": job["claim"], "head": head,
            "stage": job["stage"], "round": job.get("round").cloned().unwrap_or(json!(0)), "at": now()});
        if truthy(job.get("paseo")) {
            packet["paseo"] = job["paseo"].clone();
        }
        data["jobs"][i]
            .as_object_mut()
            .unwrap()
            .shift_remove("handoff");
        let job = patch_job(
            &mut data,
            &job_id,
            &json!({"claim": null, "handoff": packet}),
        );
        Ok((Some(dump_board(&data)), job))
    })?;
    println!("{}", dumps(&result, INDENT2));
    Ok(())
}

type Scan = Result<usize, (&'static str, usize)>;

fn skip_ws(c: &[char], mut idx: usize) -> usize {
    while idx < c.len() && matches!(c[idx], ' ' | '\t' | '\n' | '\r') {
        idx += 1;
    }
    idx
}

/// The `json.JSONDecodeError` text CPython's C scanner raises for s, or "" when it would parse s.
/// Call it only after `pyjson::loads` fails.
fn py_json_error(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let found = if c.first() == Some(&'\u{feff}') {
        Err(("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0))
    } else {
        scan_value(&c, skip_ws(&c, 0)).and_then(|end| {
            let end = skip_ws(&c, end);
            if end == c.len() {
                Ok(end)
            } else {
                Err(("Extra data", end))
            }
        })
    };
    let Err((msg, pos)) = found else {
        return String::new();
    };
    let lineno = c[..pos].iter().filter(|&&x| x == '\n').count() + 1;
    let colno = match c[..pos].iter().rposition(|&x| x == '\n') {
        Some(nl) => pos - nl,
        None => pos + 1,
    };
    format!("{msg}: line {lineno} column {colno} (char {pos})")
}

fn scan_value(c: &[char], idx: usize) -> Scan {
    let Some(&first) = c.get(idx) else {
        return Err(("Expecting value", idx));
    };
    let rest = &c[idx..];
    let word = |w: &str| {
        rest.len() >= w.chars().count() && rest.iter().zip(w.chars()).all(|(a, b)| *a == b)
    };
    match first {
        '"' => scan_string(c, idx + 1),
        '{' => scan_object(c, idx + 1),
        '[' => scan_array(c, idx + 1),
        'n' if word("null") => Ok(idx + 4),
        't' if word("true") => Ok(idx + 4),
        'f' if word("false") => Ok(idx + 5),
        'N' if word("NaN") => Ok(idx + 3),
        'I' if word("Infinity") => Ok(idx + 8),
        '-' if word("-Infinity") => Ok(idx + 9),
        _ => scan_number(c, idx),
    }
}

fn scan_number(c: &[char], start: usize) -> Scan {
    let digit = |i: usize| c.get(i).is_some_and(char::is_ascii_digit);
    let len = c.len();
    let mut idx = start;
    if c[idx] == '-' {
        idx += 1;
        if idx >= len {
            return Err(("Expecting value", start));
        }
    }
    if ('1'..='9').contains(&c[idx]) {
        idx += 1;
        while digit(idx) {
            idx += 1;
        }
    } else if c[idx] == '0' {
        idx += 1;
    } else {
        return Err(("Expecting value", start));
    }
    if idx + 1 < len && c[idx] == '.' && digit(idx + 1) {
        idx += 2;
        while digit(idx) {
            idx += 1;
        }
    }
    if idx + 1 < len && matches!(c[idx], 'e' | 'E') {
        let e_start = idx;
        idx += 1;
        if idx + 1 < len && matches!(c[idx], '-' | '+') {
            idx += 1;
        }
        while digit(idx) {
            idx += 1;
        }
        if !digit(idx - 1) {
            idx = e_start;
        }
    }
    Ok(idx)
}

fn scan_string(c: &[char], mut end: usize) -> Scan {
    let begin = end - 1;
    let len = c.len();
    let hex = |range: std::ops::Range<usize>| {
        c[range]
            .iter()
            .try_fold(0u32, |n, d| Some(n * 16 + d.to_digit(16)?))
    };
    loop {
        let mut next = end;
        while next < len && !matches!(c[next], '"' | '\\') {
            if (c[next] as u32) <= 0x1f {
                return Err(("Invalid control character at", next));
            }
            next += 1;
        }
        if next == len {
            return Err(("Unterminated string starting at", begin));
        }
        let quote = c[next] == '"';
        next += 1;
        if quote {
            return Ok(next);
        }
        if next == len {
            return Err(("Unterminated string starting at", begin));
        }
        if c[next] != 'u' {
            end = next + 1;
            if !matches!(c[next], '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') {
                return Err(("Invalid \\escape", end - 2));
            }
            continue;
        }
        next += 1;
        end = next + 4;
        if end >= len {
            return Err(("Invalid \\uXXXX escape", next - 1));
        }
        let Some(high) = hex(next..end) else {
            return Err(("Invalid \\uXXXX escape", end - 5));
        };
        if (0xd800..0xdc00).contains(&high) && end + 6 < len && c[end] == '\\' && c[end + 1] == 'u'
        {
            end += 6;
            let Some(low) = hex(end - 4..end) else {
                return Err(("Invalid \\uXXXX escape", end - 5));
            };
            if !(0xdc00..0xe000).contains(&low) {
                end -= 6;
            }
        }
    }
}

fn scan_object(c: &[char], idx: usize) -> Scan {
    let mut idx = skip_ws(c, idx);
    if c.get(idx) != Some(&'}') {
        loop {
            if c.get(idx) != Some(&'"') {
                return Err(("Expecting property name enclosed in double quotes", idx));
            }
            idx = skip_ws(c, scan_string(c, idx + 1)?);
            if c.get(idx) != Some(&':') {
                return Err(("Expecting ':' delimiter", idx));
            }
            idx = skip_ws(c, scan_value(c, skip_ws(c, idx + 1))?);
            if c.get(idx) == Some(&'}') {
                break;
            }
            if c.get(idx) != Some(&',') {
                return Err(("Expecting ',' delimiter", idx));
            }
            idx = skip_ws(c, idx + 1);
        }
    }
    Ok(idx + 1)
}

fn scan_array(c: &[char], idx: usize) -> Scan {
    let mut idx = skip_ws(c, idx);
    if c.get(idx) != Some(&']') {
        loop {
            idx = skip_ws(c, scan_value(c, idx)?);
            if c.get(idx) == Some(&']') {
                break;
            }
            if c.get(idx) != Some(&',') {
                return Err(("Expecting ',' delimiter", idx));
            }
            idx = skip_ws(c, idx + 1);
        }
    }
    Ok(idx + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::shared::fs::temp_dir;
    use crate::shared::http::tests::{home_for, serve};

    #[test]
    fn blocking_fails_closed_on_a_malformed_claim() {
        for job in [
            json!({"claim": "x"}),
            json!({"claim": [1]}),
            json!({"claim": {"device": "d", "session": "o", "seen": 5}}),
            json!({"claim": {"device": "d", "session": "o"}}),
        ] {
            assert_eq!(
                blocking(Some(&job), "d", "s", 120).unwrap_err().code,
                1,
                "{job}"
            );
        }
        assert_eq!(
            blocking(Some(&json!({"claim": {}})), "d", "s", 120).unwrap(),
            None
        );
    }

    /// Run f with these environment variables set, restoring them after. Tests that read the Paseo or session
    /// variables hold the same lock.
    fn with_env<T>(vars: &[(&str, &str)], f: impl FnOnce() -> T) -> T {
        let _lock = crate::shared::test_env_lock();
        let saved: Vec<_> = vars
            .iter()
            .map(|(k, _)| (*k, std::env::var_os(k)))
            .collect();
        for (k, v) in vars {
            std::env::set_var(k, v);
        }
        let out = f();
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        out
    }

    /// A PASEO_HOME folder with a server id, so paseo_session links once PASEO_AGENT_ID is set.
    fn paseo_home(name: &str) -> PathBuf {
        let paseo = temp_dir(name);
        fs::write(paseo.join("server-id"), "srv_new\n").unwrap();
        paseo
    }

    fn refused_home(name: &str) -> PathBuf {
        let (origin, _) = serve(vec![]);
        home_for(name, &origin)
    }

    #[test]
    fn test_board_refuses_decision() {
        let home = refused_home("claims-refuses-decision");
        let opts = WriteOpts {
            show: false,
            verdict: None,
            rewrite_round: None,
        };
        let patch = json!({"decision": {"verdict": "APPROVE", "decider": "person"}})
            .as_object()
            .unwrap()
            .clone();
        let e = board_write(&home, "042-sample-export", patch, &opts).unwrap_err();
        assert_eq!(
            (e.code, e.stderr.as_str()),
            (
                2,
                "factory.py: only factory.py push and pickup set decision\n"
            )
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn test_board_refuses_checked_head() {
        let home = refused_home("claims-refuses-checked-head");
        let opts = WriteOpts {
            show: false,
            verdict: None,
            rewrite_round: None,
        };
        let patch = json!({"checked_head": "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"})
            .as_object()
            .unwrap()
            .clone();
        let e = board_write(&home, "042-sample-export", patch, &opts).unwrap_err();
        assert_eq!(
            (e.code, e.stderr.as_str()),
            (
                2,
                "factory.py: only factory.py checks sets checked_head. Nothing was written.\n"
            )
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn test_board_refuses_agents() {
        let home = refused_home("claims-refuses-agents");
        let opts = WriteOpts {
            show: false,
            verdict: None,
            rewrite_round: None,
        };
        let patch = json!({"agents": []}).as_object().unwrap().clone();
        let e = board_write(&home, "042-sample-export", patch, &opts).unwrap_err();
        assert_eq!(
            (e.code, e.stderr.as_str()),
            (
                2,
                "factory.py: only factory.py agent and pickup set agents\n"
            )
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn test_board_refuses_repeated() {
        let home = refused_home("claims-refuses-repeated");
        let opts = WriteOpts {
            show: false,
            verdict: None,
            rewrite_round: None,
        };
        let patch = json!({"repeated": []}).as_object().unwrap().clone();
        let e = board_write(&home, "042-csv-export", patch, &opts).unwrap_err();
        assert_eq!(
            (e.code, e.stderr.as_str()),
            (
                2,
                "factory.py: only factory.py repeats sets repeated. Nothing was written.\n"
            )
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn archive_floor_reads_only_a_positive_int() {
        for (archived, floor) in [
            (json!({"max_id": 416}), 416),
            (json!({"max_id": true}), 0),
            (json!({"max_id": "416"}), 0),
            (json!({"max_id": -1}), 0),
            (json!({"max_id": 416.0}), 0),
            (json!([416]), 0),
            (Value::Null, 0),
        ] {
            assert_eq!(
                archive_floor(&json!({"jobs": [], "archived": archived})),
                floor,
                "{archived}"
            );
        }
        assert_eq!(archive_floor(&json!({"jobs": []})), 0);
        assert_eq!(next_job_id(&["415-add-export".into()], "x", 416), "417-x");
        assert_eq!(next_job_id(&["420-later".into()], "x", 416), "421-x");
        assert_eq!(
            next_job_id(&["sample".into(), "12-short".into()], "x", 0),
            "001-x"
        );
    }

    #[test]
    fn claim_puts_refs_on_the_stub() {
        let refs = json!([{"id": "041-recipe-import", "kind": "sub-job-of"}, {"id": "052-parse-recipe-urls", "kind": "depends-on"}]);
        let claim = new_claim("d", "s", None);
        let stub = claim_stub(
            "417-store-imported-recipes",
            "sample",
            "2026-11-01T00:00:00Z",
            claim.clone(),
            None,
            Some(refs.clone()),
        );
        assert_eq!((&stub["refs"], stub.get("stage")), (&refs, None));
        let bare = claim_stub(
            "418-no-refs",
            "sample",
            "2026-11-01T00:00:00Z",
            claim,
            None,
            Some(json!([])),
        );
        assert!(bare.get("refs").is_none());
        assert_eq!(
            bare.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["id", "project", "updated", "started", "claim"]
        );
    }

    #[test]
    fn rewrite_round_opens_only_that_round() {
        let (r1, r2) = (json!({"code": "PASS"}), json!({"code": "CHANGES"}));
        let found = json!({"rounds": [r1, r2]});
        assert_eq!(
            rounds_refusal(
                "042-x",
                Some(&found),
                &json!([r1, {"code": "PASS"}]),
                Some(2)
            ),
            None
        );
        assert_eq!(
            rounds_refusal("042-x", Some(&found), &json!([{"code": "CHANGES"}, {"code": "PASS"}]), Some(2)).unwrap(),
            "042-x cannot rewrite round 1 in a board patch. Only factory.py verdict and round-wait change a recorded round. Nothing was written."
        );
        assert!(
            rounds_refusal("042-x", Some(&found), &json!([r1, {"code": "PASS"}]), None).is_some()
        );
    }

    #[test]
    fn new_session_tag_is_six_hex() {
        let tag = with_env(&[("FACTORY_SESSION", "")], || {
            new_session();
            std::env::var("FACTORY_SESSION").unwrap()
        });
        assert!(
            tag.len() == 6
                && tag
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "{tag}"
        );
        with_env(&[("FACTORY_SESSION", "kept")], || {
            new_session();
            assert_eq!(std::env::var("FACTORY_SESSION").unwrap(), "kept");
        });
    }

    const UNRESOLVED: &str = "factory.py: factory home must be absolute. Nothing was written.\n";

    #[test]
    fn board_resolves_home_before_the_write() {
        let paseo = paseo_home("claims-paseo-board");
        let home = refused_home("claims-home-board");
        let opts = WriteOpts {
            show: false,
            verdict: None,
            rewrite_round: None,
        };
        let patch = json!({"note": "checking"}).as_object().unwrap().clone();
        let vars = [
            ("PASEO_HOME", paseo.to_str().unwrap()),
            ("PASEO_AGENT_ID", "agent_new"),
            ("FACTORY_HOME", "relative"),
            ("FACTORY_SESSION", "s"),
        ];
        let e = with_env(&vars, || board_write(&home, "001-test", patch, &opts)).unwrap_err();
        assert_eq!((e.code, e.stderr.as_str()), (2, UNRESOLVED));
    }

    #[test]
    fn claim_id_resolves_home_before_the_write() {
        let paseo = paseo_home("claims-paseo-claim");
        let home = refused_home("claims-home-claim");
        let vars = [
            ("PASEO_HOME", paseo.to_str().unwrap()),
            ("PASEO_AGENT_ID", "agent_new"),
            ("FACTORY_HOME", "relative"),
            ("FACTORY_SESSION", "s"),
        ];
        let e = with_env(&vars, || claim_id(&home, "sample-export", "sample", None)).unwrap_err();
        assert_eq!((e.code, e.stderr.as_str()), (2, UNRESOLVED));
        assert!(!home.join("jobs").exists());
    }

    #[test]
    fn claim_id_resolves_home_before_the_folder_and_session() {
        let paseo = paseo_home("claims-paseo-fresh");
        let fresh = temp_dir("claims-fresh").join("fresh");
        let vars = [
            ("PASEO_HOME", paseo.to_str().unwrap()),
            ("PASEO_AGENT_ID", "agent_new"),
            ("FACTORY_HOME", "relative"),
            ("FACTORY_SESSION", ""),
        ];
        let session = with_env(&vars, || {
            assert!(claim_id(&fresh, "sample-export", "sample", None).is_err());
            std::env::var("FACTORY_SESSION").unwrap()
        });
        assert_eq!(session, "");
        assert!(!fresh.exists());
    }

    #[test]
    fn py_json_error_matches_cpython() {
        for (text, want) in [
            ("nope", "Expecting value: line 1 column 1 (char 0)"),
            (
                "{\"a\" 1}",
                "Expecting ':' delimiter: line 1 column 6 (char 5)",
            ),
            ("{} x", "Extra data: line 1 column 4 (char 3)"),
            ("\"\\q\"", "Invalid \\escape: line 1 column 2 (char 1)"),
            (
                "\"a\tb\"",
                "Invalid control character at: line 1 column 3 (char 2)",
            ),
            ("\n\n  [é", "Expecting value: line 3 column 4 (char 5)"),
            ("[1, 2", "Expecting ',' delimiter: line 1 column 6 (char 5)"),
            (
                "{\"a\": 1,}",
                "Expecting property name enclosed in double quotes: line 1 column 9 (char 8)",
            ),
            (
                "\"abc",
                "Unterminated string starting at: line 1 column 1 (char 0)",
            ),
            (
                "\u{feff}{}",
                "Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)",
            ),
            ("[", "Expecting value: line 1 column 2 (char 1)"),
            ("[1,]", "Expecting value: line 1 column 4 (char 3)"),
            ("1.e5", "Extra data: line 1 column 2 (char 1)"),
            (
                "\"\\u12\"",
                "Invalid \\uXXXX escape: line 1 column 3 (char 2)",
            ),
            (
                "\"\\ud800\\uzzzz\"",
                "Invalid \\uXXXX escape: line 1 column 9 (char 8)",
            ),
            ("[NaN, -Infinity, 1.5e3, \"\\ud800\\u0041\"]", ""),
        ] {
            assert_eq!(py_json_error(text), want, "{text:?}");
        }
    }

    #[test]
    fn py_eq_follows_python() {
        assert!(
            py_eq(&json!(1), &json!(true))
                && py_eq(&json!(0.0), &json!(false))
                && py_eq(&json!(1), &json!(1.0))
        );
        assert!(
            !py_eq(&json!(2), &json!(true))
                && !py_eq(&json!("1"), &json!(1))
                && !py_eq(&json!(null), &json!(false))
        );
        assert!(py_eq(
            &json!({"a": [1, {"b": null}]}),
            &json!({"a": [true, {"b": null}]})
        ));
        assert!(!py_eq(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
        let big = serde_json::from_str::<Value>("123456789012345678901234567890").unwrap();
        assert!(
            py_eq(&big, &big.clone())
                && !py_eq(
                    &big,
                    &serde_json::from_str("123456789012345678901234567891").unwrap()
                )
        );
    }

    #[test]
    fn py_str_and_truthy_follow_python() {
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!("x")), "x");
        assert_eq!(py_str(&json!(1.0)), "1.0");
        assert_eq!(
            py_str(&json!({"a": [true, "it's", null]})),
            r#"{'a': [True, "it's", None]}"#
        );
        assert!(
            !truthy(None)
                && !truthy(Some(&json!(0)))
                && !truthy(Some(&json!("")))
                && !truthy(Some(&json!({})))
        );
        assert!(truthy(Some(&json!(0.5))) && truthy(Some(&json!([0]))));
    }

    /// Ported from tests/cli/test_stage_guard.py AutomergeGuardTest: automerge true needs a readable,
    /// regular decision.md on this host whose first line is APPROVE.
    #[cfg(unix)]
    #[test]
    fn automerge_guard_refusals() {
        let home = temp_dir("claims-automerge-guard");
        let job = "042-sample-export";
        let folder = home.join("jobs/sample").join(job);
        let found = json!({"id": job, "project": "sample", "stage": "pr-open"});
        let patch = |v: Value| v.as_object().unwrap().clone();
        let set = patch(json!({"stage": "pr-open", "automerge": true}));
        let why = |found: &Value, p: &Map<String, Value>| move_refusal(&home, job, Some(found), p);
        let refused = |why: &str| {
            Some(format!(
                "{job} cannot set automerge true: {why} Nothing was written."
            ))
        };
        assert_eq!(
            why(&found, &set),
            refused("decision.md is not on this host. Run factory.py pull sample/042-sample-export, then retry.")
        );
        fs::create_dir_all(&folder).unwrap();
        for (text, head) in [
            ("APPROVED\n", "'APPROVED'"),
            (
                "\n  ESCALATE: code reviewer asked for a person\nmore\n",
                "'ESCALATE: code reviewer asked for a pers'",
            ),
        ] {
            fs::write(folder.join("decision.md"), text).unwrap();
            assert_eq!(
                why(&found, &set),
                refused(&format!("decision.md starts with {head}, not APPROVE."))
            );
        }
        fs::write(folder.join("decision.md"), b"\xff\xfe").unwrap();
        assert_eq!(
            why(&found, &set),
            refused("its decision.md cannot be read.")
        );
        fs::remove_file(folder.join("decision.md")).unwrap();
        fs::write(home.join("elsewhere.md"), "APPROVE\n").unwrap();
        std::os::unix::fs::symlink(home.join("elsewhere.md"), folder.join("decision.md")).unwrap();
        assert_eq!(
            why(&found, &set),
            refused("its decision.md cannot be read.")
        );
        fs::remove_file(folder.join("decision.md")).unwrap();
        let projectless = json!({"id": job, "stage": "pr-open"});
        let no_project = refused("it has no project, so there is no decision.md to check.");
        assert_eq!(why(&projectless, &set), no_project);
        assert_eq!(
            why(
                &projectless,
                &patch(json!({"automerge": true, "project": "../beta"}))
            ),
            no_project
        );
        fs::write(
            folder.join("decision.md"),
            "APPROVE\nA person approved this job with /factory approve.\n",
        )
        .unwrap();
        assert_eq!(why(&found, &set), None);
        fs::remove_file(folder.join("decision.md")).unwrap();
        for value in [json!(false), Value::Null] {
            assert_eq!(why(&found, &patch(json!({"automerge": value}))), None);
        }
        fs::remove_dir_all(home).unwrap();
    }
}
