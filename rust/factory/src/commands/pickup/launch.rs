use super::*;

// ---- group B: routes, holds, claims and launches

pub(super) const LAUNCH_WAIT_SECONDS: f64 = 30.0;
pub(super) const LAUNCH_BACKOFF_SECONDS: i64 = 300;
pub(super) const LAUNCH_BACKOFF_CAP_SECONDS: i64 = 14400;
pub(super) const LAUNCH_STOP_FAILURES: i64 = 5;
pub(super) const AGENT_ROWS_MAX: usize = 32;
pub(super) const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
pub(super) const RETRO_PROMPT_FILE: &str = "retro-schedule-prompt.md";
pub(super) const RETRO_CATCHUP_FILE: &str = "retro-catchup.json";
pub(super) const RETRO_WAIT_FILE: &str = "retro-catchup-wait.json";
pub(super) const RETRO_CATCHUP_AFTER: (i64, i64) = (9, 30);
pub(super) const USAGE_PROMPT_LINES: usize = 50;
/// USAGE_TOKENS: the kinds in order, and the usage field each reads.
pub(super) const USAGE_TOKENS: [(&str, &str); 4] = [
    ("input", "input_tokens"),
    ("output", "output_tokens"),
    ("cache_write", "cache_creation_input_tokens"),
    ("cache_read", "cache_read_input_tokens"),
];
pub(super) static NOT_BEFORE: Pattern =
    Pattern::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", Re::NONE);
pub(super) static LIMIT_TEXT: Pattern = Pattern::new(
    "(?:usage|rate|session|weekly) limit|limit (?:reached|exceeded)",
    Re::I,
);
pub(super) static RESET_IN: Pattern = Pattern::new(
    r"(?:resets|try again) in (\d{1,4}) ?(minutes?|mins?|m|hours?|hrs?|h)\b",
    Re::I,
);
pub(super) static RESET_AT: Pattern = Pattern::new(
    r"resets (?:(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) (\d{1,2}), )?(\d{1,2})(?::(\d{2}))?(am|pm) \(([A-Za-z_]+(?:/[A-Za-z0-9_+-]+)*)\)",
    Re::NONE,
);
pub(super) static PLACEHOLDER: Pattern = Pattern::new(
    r"\{(id|project|repo_path|session|provider|model|effort|prompt)\}",
    Re::NONE,
);
pub(super) static RETRO_RUN_NAME: Pattern = Pattern::new(r"(\d{8}T\d{6}Z)-[0-9a-f]{6}", Re::NONE);
pub(super) static RETRO_JOB: Pattern = Pattern::new(r"factory-(\d{2,5})-", Re::NONE);

pub(super) fn new_claim(device: &str, session: &str) -> Value {
    let stamp = now();
    json!({"device": device, "session": session, "since": stamp, "seen": stamp})
}

pub(super) fn claimed_by(c: &Value) -> String {
    format!(
        "claimed by {} session {}",
        text_of(c.get("device")),
        text_of(c.get("session"))
    )
}

/// Python `str(v)` for a JSON value that is usually a string: None for a missing one.
pub(super) fn text_of(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => other.to_string(),
    }
}

pub(super) fn earlier_moves(m: &Value) -> Vec<&Value> {
    m.get("earlier")
        .and_then(Value::as_array)
        .map(|e| e.iter().filter(|x| x.is_object()).collect())
        .unwrap_or_default()
}

/// True when pickup recorded this holder's stop. Only a string counts.
pub(super) fn move_stopped(x: &Value) -> bool {
    x.get("stopped").is_some_and(Value::is_string)
}

/// The move's holders whose host has not stopped them yet, earlier ones first.
pub(super) fn queued_stops(m: Option<&Value>) -> Vec<&Value> {
    let Some(m) = m.filter(|m| m.is_object()) else {
        return Vec::new();
    };
    let mut all = earlier_moves(m);
    all.push(m);
    all.into_iter()
        .filter(|x| truthy(x.get("from")) && !move_stopped(x))
        .collect()
}

pub(super) fn jobs_of(data: &Value) -> &[Value] {
    data["jobs"].as_array().map_or(&[], Vec::as_slice)
}

pub(super) fn open_job_ids(jobs: &[Value]) -> Vec<&Value> {
    jobs.iter()
        .filter(|j| !get_str(j, "stage").is_some_and(|s| TERMINAL.contains(&s)))
        .filter_map(|j| j.get("id"))
        .collect()
}

/// Fresh in-flight jobs on the board: of one project, or of all when project is None, and only
/// this device's claims when device is set.
pub(super) fn inflight_count(
    data: &Value,
    project: Option<&str>,
    minutes: u32,
    device: Option<&str>,
) -> usize {
    let cutoff = unix_now() - i64::from(minutes) * 60;
    jobs_of(data)
        .iter()
        .filter(|j| {
            if project.is_some_and(|p| !p.is_empty()) && get_str(j, "project") != project {
                return false;
            }
            let claim_device = j.get("claim").and_then(|c| c.get("device"));
            if device.is_some_and(|d| !d.is_empty())
                && claim_device.and_then(Value::as_str) != device
            {
                return false;
            }
            if gated(j) || lead_of(j).is_some() || babysitting(j) || waiting_until(j).is_some() {
                return false;
            }
            let stage = get_str(j, "stage");
            let stamp = if stage == Some(IDEA) || approved_start(j) {
                j.get("claim").and_then(|c| c.get("seen"))
            } else if stage.is_some_and(|s| IN_FLIGHT.contains(&s))
                || stage == Some("pr-open") && !escalated(j)
            {
                j.get("updated")
            } else {
                return false;
            };
            stamp
                .and_then(Value::as_str)
                .and_then(parse_stamp)
                .is_some_and(|seen| seen > cutoff)
        })
        .count()
}

/// One launch entry: the job, its checkout and the session tag it runs as.
#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub(super) id: String,
    pub(super) project: String,
    pub(super) repo_path: String,
    pub(super) session: String,
}

/// routes.md's optional experiment line.
#[derive(Clone, Debug)]
pub(super) struct Experiment {
    pub(super) name: String,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) effort: Option<String>,
    pub(super) percent: u32,
}

/// claim_ideas's keyword arguments.
pub(super) struct Pick<'a> {
    pub(super) max_inflight: Option<usize>,
    pub(super) project: Option<&'a str>,
    pub(super) by_host: bool,
    pub(super) builds: bool,
    pub(super) experiment: Option<&'a Experiment>,
    pub(super) route: Option<&'a Route>,
    pub(super) skip_ids: Vec<String>,
}

/// Claim up to limit of the oldest runnable ideas in one board write, and return them. An
/// empty session mints a tag per idea.
#[allow(clippy::too_many_arguments)]
pub(super) fn claim_ideas(
    home: &Path,
    device: &str,
    session: &str,
    minutes: u32,
    paths: &[(String, String)],
    limit: usize,
    pick: &Pick,
) -> Res<Vec<Entry>> {
    let mut exp_limited = false;
    if let Some(experiment) = pick.experiment {
        let limits = read_limits(home)?;
        exp_limited = lookup(&limits, &experiment.name).is_some_and(|u| u > now().as_str());
    }
    claim_cas(home, pick, device, |text| {
        let mut data = load_board(text)?;
        let mut picked: Vec<Entry> = Vec::new();
        let count = match pick.max_inflight {
            None => 0,
            Some(_) if pick.by_host => inflight_count(&data, None, minutes, Some(device)),
            Some(_) => inflight_count(&data, pick.project, minutes, None),
        };
        let budget = match pick.max_inflight {
            None => limit,
            Some(cap) => limit.min(cap.saturating_sub(count)),
        };
        let mut order: Vec<usize> = (0..jobs_of(&data).len()).collect();
        order.sort_by_key(|&i| !prioritized(&jobs_of(&data)[i]));
        for i in order {
            if picked.len() == budget {
                break;
            }
            let j = &data["jobs"][i];
            if !truthy(j.get("project"))
                || gated(j)
                || paused(j)
                || waiting_until(j).is_some()
                || closing(j)
                || waiting_on(j, jobs_of(&data)).is_some()
                || tracer_building(j)
            {
                continue;
            }
            let project = get_str(j, "project").unwrap_or_default().to_string();
            let repo = lookup(paths, &project).map(String::from);
            let startable = get_str(j, "stage") == Some(IDEA)
                || pick.builds && (approved_start(j) || moved_here(j, device));
            let Some(id) = safe_id(j).map(String::from) else {
                continue;
            };
            let Some(repo) = repo.filter(|r| startable && is_dir(r)) else {
                continue;
            };
            if pick.project.is_some_and(|p| p != project)
                || pick.skip_ids.contains(&id)
                || claims::blocking(Some(j), device, session, minutes)?.is_some()
            {
                continue;
            }
            if let Some(experiment) = pick.experiment {
                let record = j.get("experiment");
                let arm = match record.and_then(|r| get_str(r, "arm")) {
                    Some(arm @ ("a" | "b")) if record.is_some_and(Value::is_object) => {
                        arm.to_string()
                    }
                    _ => experiment_arm(&id, experiment.percent).to_string(),
                };
                if arm == "b" && exp_limited {
                    continue;
                }
                if !truthy(record) {
                    let route = pick.route.unwrap();
                    let (provider, model, effort) = if arm == "b" {
                        (&experiment.provider, &experiment.model, &experiment.effort)
                    } else {
                        (&route.provider, &route.model, &route.effort)
                    };
                    data["jobs"][i]["experiment"] = json!({"name": experiment.name, "arm": arm,
                        "provider": provider, "model": model, "effort": effort});
                }
            }
            let tag = if session.is_empty() {
                token_hex3()
            } else {
                session.to_string()
            };
            let claim = new_claim(device, &tag);
            // Replace the claim whole, as hold does. The paseo key stays as it was.
            data["jobs"][i]
                .as_object_mut()
                .unwrap()
                .shift_remove("claim");
            patch_job(&mut data, &id, &json!({"claim": claim}));
            if pick.builds && moved_here(&data["jobs"][i], device) {
                patch_job(
                    &mut data,
                    &id,
                    &json!({"move": {"claimed": claim["since"]}}),
                );
            }
            picked.push(Entry {
                id,
                project,
                repo_path: repo,
                session: tag,
            });
        }
        let text = (!picked.is_empty()).then(|| dump_board(&data));
        Ok((text, picked))
    })
}

/// A job waiting on a person, as the dashboard's waitsOnYou reads it.
pub(super) fn waits_on_you(j: &Value) -> bool {
    let stage = get_str(j, "stage");
    if stage.is_some_and(|s| TERMINAL.contains(&s)) {
        return false;
    }
    stage == Some("needs-human")
        || get_str(j, "needs").is_some_and(|n| !strip(n).is_empty())
        || stage == Some("pr-open") && escalated(j)
}

pub(super) fn read_experiment(home: &Path, route_names: &[String]) -> Res<Option<Experiment>> {
    let path = home.join("routes.md");
    let meta = meta(&path)?;
    let Some(line) = lookup(&meta, "experiment") else {
        return Ok(None);
    };
    let bad = || {
        fail(
            format!(
                "bad experiment in {}: use experiment: <name> <provider> <model> [<effort>] <percent>",
                path.display()
            ),
            2,
        )
    };
    let parts = words(line);
    if !(4..=5).contains(&parts.len()) {
        return Err(bad());
    }
    let (name, provider, model) = (parts[0], parts[1], parts[2]);
    let effort = (parts.len() == 5).then(|| parts[3]);
    let percent = parts[parts.len() - 1];
    let digits = (1..=3).contains(&percent.len()) && percent.bytes().all(|b| b.is_ascii_digit());
    if !SLUG.is_full(name)
        || !SLUG.is_full(provider)
        || !ROUTE_MODEL.is_full(model)
        || route_names.iter().any(|r| r == name)
        || effort.is_some_and(|e| !ROUTE_EFFORTS.contains(&e))
        || !digits
        || percent.parse::<u32>().unwrap_or(999) > 100
    {
        return Err(bad());
    }
    Ok(Some(Experiment {
        name: name.into(),
        provider: provider.into(),
        model: model.into(),
        effort: effort.map(String::from),
        percent: percent.parse().unwrap(),
    }))
}

/// Arm "b" when the job id's stable hash bucket falls under percent, else "a".
pub(super) fn experiment_arm(job_id: &str, percent: u32) -> &'static str {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(job_id.as_bytes());
    let bucket = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 100;
    if bucket < percent {
        "b"
    } else {
        "a"
    }
}

/// This host's first route not marked limited, or None.
pub(super) fn pick_route(home: &Path) -> Res<Option<Route>> {
    #[cfg(test)]
    if let Some(f) = fake!(pick_route) {
        return f(home);
    }
    let routes = read_routes(home)?;
    let stamp = now();
    let limits = read_limits(home)?;
    let active = |name: &str| lookup(&limits, name).is_some_and(|u| u > stamp.as_str());
    Ok(routes.into_iter().find(|r| !active(&r.name)))
}

/// True when name is the experiment, False when it is a route, else exit 2.
pub(super) fn check_limit_name(home: &Path, name: &str) -> Res<bool> {
    let routes: Vec<String> = read_routes(home)?.into_iter().map(|r| r.name).collect();
    let experiment = read_experiment(home, &routes)?;
    if experiment.as_ref().is_some_and(|e| e.name == name) {
        return Ok(true);
    }
    if routes.iter().any(|r| r == name) {
        return Ok(false);
    }
    let path = home.join("routes.md");
    let what = if experiment.is_some() {
        "unknown route or experiment"
    } else {
        "unknown route"
    };
    Err(fail(
        format!("{what} {}: not in {}", repr(name), path.display()),
        2,
    ))
}

/// Mark one route or the experiment limited for minutes from now.
pub(super) fn limit_route(home: &Path, name: &str, minutes: i64) -> Res<(String, bool)> {
    let experiment = check_limit_name(home, name)?;
    let stamp = now();
    let mut limits: Map<String, Value> = Map::new();
    for (n, until) in read_limits(home)? {
        if until > stamp {
            limits.insert(n, Value::from(until));
        }
    }
    let until = format_stamp(unix_now() + minutes * 60);
    limits.insert(name.to_string(), Value::from(until.clone()));
    write_file(
        &home.join("route-limits.json"),
        &(dumps(&Value::Object(limits), INDENT2) + "\n"),
    )?;
    Ok((until, experiment))
}

/// Drop this session's claim on an idea in one board write, so the idea waits again.
pub(super) fn release_claim(home: &Path, job_id: &str, device: &str, session: &str) -> Res<()> {
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = jobs_of(&data)
            .iter()
            .find(|j| get_str(j, "id") == Some(job_id))
        else {
            return Err(fail(format!("no job {job_id} on the board"), 4));
        };
        let stage = job.get("stage");
        if get_str(job, "stage") != Some(IDEA) {
            return Err(fail(
                format!(
                    "{job_id} is at {}, not idea: nothing to release",
                    text_of(stage)
                ),
                6,
            ));
        }
        let claim = job.get("claim").filter(|c| truthy(Some(*c)));
        if !claim.is_some_and(|c| mine(c, device, session)) {
            return Err(fail(format!("{job_id} has no claim from this session"), 6));
        }
        patch_job(&mut data, job_id, &json!({"claim": null}));
        Ok((Some(dump_board(&data)), ()))
    })
}

/// True when pickup holds this idea for a batch: a follow-up or a triage-queued idea nobody added,
/// approved or prioritized from the dashboard.
pub(super) fn batch_hold(j: &Value) -> bool {
    if get_str(j, "stage") != Some(IDEA) || j.get("capture_id").is_some() || prioritized(j) {
        return false;
    }
    let follow_up = j.get("refs").and_then(Value::as_array).is_some_and(|refs| {
        refs.iter()
            .any(|r| get_str(r, "kind") == Some("follow-up-of"))
    });
    follow_up || j.get("backlog").and_then(|b| get_str(b, "by")) == Some("triage")
}

/// A field of the effective pipeline view, or None when it is absent or unreadable.
pub(super) fn view_field(home: &Path, slug: &str, field: &str) -> Res<Option<Value>> {
    let path = format!("/api/pipeline/effective/{slug}");
    let r = api(home, "GET", &path, None, &[200, 404, 409], &[])?;
    if r.status != 200 {
        return Ok(None);
    }
    let view = loads(&r.text()?).ok();
    Ok(view.and_then(|v| v.get(field).cloned()))
}

pub(super) fn hold_config(home: &Path, slug: &str) -> Res<(i64, i64)> {
    let hold = view_field(home, slug, "hold")?.filter(Value::is_object);
    let least = hold.as_ref().and_then(|h| int_of(h.get("min_ideas")));
    let hours = hold.as_ref().and_then(|h| int_of(h.get("max_wait_hours")));
    match (least, hours) {
        (Some(l), Some(h)) if (1..=MAX_RIDERS as i64).contains(&l) && (1..=168).contains(&h) => {
            Ok((l, h))
        }
        _ => Ok((HOLD_MIN_IDEAS as i64, HOLD_MAX_HOURS as i64)),
    }
}

pub(super) fn backpressure_config(home: &Path, slug: &str) -> Res<i64> {
    let bp = view_field(home, slug, "backpressure")?.filter(Value::is_object);
    match bp.as_ref().and_then(|b| int_of(b.get("needs_you"))) {
        Some(n) if (1..=50).contains(&n) => Ok(n),
        _ => Ok(BACKPRESSURE_NEEDS_YOU as i64),
    }
}

/// {id: skip line} for plain ideas pickup does not launch while the project's own waiting jobs
/// reach its backpressure.needs_you.
pub(super) fn backpressure_holds(
    home: &Path,
    jobs: &[&Value],
    waiting: &HashMap<Option<String>, i64>,
    runnable: &dyn Fn(&Value) -> bool,
) -> Res<HashMap<String, String>> {
    let (mut limits, mut lines) = (HashMap::new(), HashMap::new());
    for j in jobs {
        if get_str(j, "stage") != Some(IDEA) || prioritized(j) || !runnable(j) {
            continue;
        }
        let slug = get_str(j, "project").unwrap_or_default().to_string();
        let count = waiting.get(&Some(slug.clone())).copied().unwrap_or(0);
        if count < 1 {
            continue;
        }
        if !limits.contains_key(&slug) {
            limits.insert(slug.clone(), backpressure_config(home, &slug)?);
        }
        let limit = limits[&slug];
        if count >= limit {
            let id = get_str(j, "id").unwrap_or_default();
            lines.insert(
                id.to_string(),
                format!(
                    "skip {id} {slug}: backpressure ({count} waiting on you, pauses at {limit})"
                ),
            );
        }
    }
    Ok(lines)
}

/// {id: skip line} for the held ideas pickup does not launch this run.
pub(super) fn batch_holds(
    home: &Path,
    jobs: &[&Value],
    runnable: &dyn Fn(&Value) -> bool,
) -> Res<HashMap<String, String>> {
    let mut groups: Vec<(String, Vec<&Value>)> = Vec::new();
    let mut starters: HashMap<String, String> = HashMap::new();
    for j in jobs {
        let slug = get_str(j, "project").unwrap_or_default().to_string();
        let id = get_str(j, "id").unwrap_or_default();
        let batch = j.get("priority").and_then(|p| p.get("batch")) == Some(&Value::Bool(true));
        if get_str(j, "stage") == Some(IDEA)
            && prioritized(j)
            && batch
            && !(gated(j)
                || paused(j)
                || closing(j)
                || waiting_until(j).is_some()
                || tracer_building(j))
        {
            starters
                .entry(slug.clone())
                .or_insert_with(|| id.to_string());
        }
        if batch_hold(j) && runnable(j) {
            match groups.iter_mut().find(|(s, _)| *s == slug) {
                Some((_, held)) => held.push(j),
                None => groups.push((slug, vec![j])),
            }
        }
    }
    let (stamp, mut lines) = (unix_now(), HashMap::new());
    let id = |j: &Value| get_str(j, "id").unwrap_or_default().to_string();
    for (slug, held) in groups {
        if let Some(starter) = starters.get(&slug) {
            for j in &held {
                lines.insert(
                    id(j),
                    format!("skip {} {slug}: batches behind {starter}", id(j)),
                );
            }
            continue;
        }
        let (least, hours) = hold_config(home, &slug)?;
        let age = |j: &Value| {
            get_str(j, "updated")
                .and_then(parse_stamp)
                .map_or(hours * 3600, |u| stamp - u)
        };
        let oldest = held.iter().map(|j| age(j)).max().unwrap();
        if held.len() as i64 >= least || oldest >= hours * 3600 {
            let first = id(held[0]);
            for j in &held[1..] {
                lines.insert(
                    id(j),
                    format!("skip {} {slug}: batches behind {first}", id(j)),
                );
            }
            continue;
        }
        let waited = oldest.max(0) / 3600;
        for j in &held {
            lines.insert(
                id(j),
                format!(
                    "skip {} {slug}: waits for a batch ({} of {least} held, oldest {waited}h of {hours}h)",
                    id(j),
                    held.len()
                ),
            );
        }
    }
    Ok(lines)
}

/// This host's max_inflight from the host registry (0 pauses pickup), or None.
pub(super) fn host_max_inflight(home: &Path, device: &str) -> Res<Option<usize>> {
    let r = api(home, "GET", "/api/hosts", None, &[200, 404], &[])?;
    if r.status == 404 {
        return Ok(None);
    }
    let hosts = loads(&r.text()?).ok();
    let record = hosts
        .as_ref()
        .and_then(Value::as_array)
        .and_then(|h| h.iter().find(|h| get_str(h, "id") == Some(device)));
    let cap = record.and_then(|r| int_of(r.get("max_inflight")));
    Ok(cap.filter(|c| (0..=5).contains(c)).map(|c| c as usize))
}

/// The values a launcher's placeholders take.
#[derive(Clone, Debug)]
pub(super) struct Given {
    pub(super) id: String,
    pub(super) project: String,
    pub(super) repo_path: String,
    pub(super) session: String,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) effort: String,
    pub(super) prompt: String,
}

impl Given {
    pub(super) fn new(entry: &Entry, route: (&str, &str, &str), prompt: String) -> Given {
        Given {
            id: entry.id.clone(),
            project: entry.project.clone(),
            repo_path: entry.repo_path.clone(),
            session: entry.session.clone(),
            provider: route.0.into(),
            model: route.1.into(),
            effort: route.2.into(),
            prompt,
        }
    }
}

pub(super) fn route_values(route: &Route) -> (&str, &str, &str) {
    (
        &route.provider,
        &route.model,
        route.effort.as_deref().unwrap_or(""),
    )
}

/// The template with each known {placeholder} replaced; unknown braces stay literal.
pub(super) fn launcher_argv(template: &[String], given: &Given) -> Vec<String> {
    template
        .iter()
        .map(|arg| {
            PLACEHOLDER.replace_all(arg, |c| {
                match c.get(1).unwrap().as_str() {
                    "id" => &given.id,
                    "project" => &given.project,
                    "repo_path" => &given.repo_path,
                    "session" => &given.session,
                    "provider" => &given.provider,
                    "model" => &given.model,
                    "effort" => &given.effort,
                    _ => &given.prompt,
                }
                .clone()
            })
        })
        .collect()
}

pub(super) fn launch_prompt(command: &str) -> String {
    format!("{command}\n\n{UNATTENDED}")
}

pub(super) fn launch_wait_seconds() -> f64 {
    #[cfg(test)]
    if let Some(s) = FAKES.with(|f| f.borrow().launch_wait_seconds) {
        return s;
    }
    LAUNCH_WAIT_SECONDS
}

pub(super) fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return -signal;
    }
    status.code().unwrap_or(1)
}

/// Start one launcher detached, with its output in a private log, and wait up to
/// LAUNCH_WAIT_SECONDS. (ok, detail, log path): still running or exit 0 is ok.
pub(super) fn launch_idea(
    home: &Path,
    entry: &Entry,
    route: &str,
    argv: &[String],
    env: &[(String, String)],
) -> Res<(bool, Option<String>, PathBuf)> {
    #[cfg(test)]
    if let Some(f) = fake!(launch_idea) {
        return f(home, entry, route, argv, env);
    }
    let _ = route;
    let folder = home.join("logs").join("pickup");
    mkdirs(&folder)?;
    let log_path = folder.join(format!("{}-{}.log", entry.id, entry.session));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let log = options
        .open(&log_path)
        .map_err(|e| fail(os_error(&e, Some(&log_path)), 1))?;
    let err_log = log
        .try_clone()
        .map_err(|e| fail(os_error(&e, Some(&log_path)), 1))?;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(&entry.repo_path)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err_log);
    detach(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let name = basename(&argv[0]);
            return Ok((
                false,
                Some(format!("cannot start {name}: {}", strerror(&e))),
                log_path,
            ));
        }
    };
    let deadline = Instant::now() + Duration::from_secs_f64(launch_wait_seconds());
    loop {
        if let Some(status) = child.try_wait().map_err(|e| fail(os_error(&e, None), 1))? {
            let code = exit_code(status);
            return Ok((code == 0, Some(format!("exit {code}")), log_path));
        }
        if Instant::now() >= deadline {
            // The child is left running, detached; dropping the handle does not stop it.
            return Ok((true, None, log_path));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Seconds pickup parks a job after its nth launch failure in a row: 300, doubling, capped at 4 hours.
pub(super) fn launch_backoff_seconds(n: i64) -> i64 {
    (LAUNCH_BACKOFF_SECONDS * 2i64.pow((n - 1).clamp(0, 6) as u32)).min(LAUNCH_BACKOFF_CAP_SECONDS)
}

/// A launch failure detail as one board-safe line of at most 200 characters.
pub(super) fn launch_reason(detail: &str) -> String {
    let line = words(detail).join(" ");
    NOTE_INVISIBLE
        .replace_all(&line, |_| String::new())
        .chars()
        .take(200)
        .collect()
}

/// (count, until) or (count, None) for a stop at needs-human, or None.
pub(super) type Parked = Option<(i64, Option<String>)>;

/// Drop this session's claim and park the job after a failed launch, in one board write. From the
/// fifth failure in a row, move it to needs-human with a needs ask instead.
pub(super) fn back_off_launch(
    home: &Path,
    job_id: &str,
    device: &str,
    session: &str,
    reason: &str,
) -> Res<Parked> {
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = jobs_of(&data)
            .iter()
            .find(|j| get_str(j, "id") == Some(job_id))
        else {
            return Ok((None, None));
        };
        if !job
            .get("claim")
            .is_some_and(|c| truthy(Some(c)) && mine(c, device, session))
        {
            return Ok((None, None));
        }
        let prior = job
            .get("launch_failure")
            .filter(|r| r.is_object())
            .and_then(|r| int_of(r.get("count")))
            .filter(|n| *n >= 1)
            .unwrap_or(0);
        let count = prior + 1;
        let at = unix_now();
        let failure = json!({"count": count, "reason": reason, "at": format_stamp(at)});
        if count >= LAUNCH_STOP_FAILURES {
            let needs = format!("Pickup launch failed {count} times in a row on host {device}. Last reason: {reason}. Fix the launcher, then clear this ask and move the job back to {}.", text_of(job.get("stage")));
            patch_job(
                &mut data,
                job_id,
                &json!({"claim": null, "not_before": null,
                "stage": "needs-human", "needs": needs, "launch_failure": failure}),
            );
            return Ok((Some(dump_board(&data)), Some((count, None))));
        }
        let until = format_stamp(at + launch_backoff_seconds(count));
        patch_job(
            &mut data,
            job_id,
            &json!({"claim": null, "not_before": until, "launch_failure": failure}),
        );
        Ok((Some(dump_board(&data)), Some((count, Some(until)))))
    })
}

/// Drop a job's launch_failure after a launch succeeds; a failed write warns instead of ending the pass.
pub(super) fn clear_launch_failure(io: &mut Io, home: &Path, job_id: &str) {
    let cleared = cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        if !jobs_of(&data)
            .iter()
            .any(|j| get_str(j, "id") == Some(job_id) && j.get("launch_failure").is_some())
        {
            return Ok((None, ()));
        }
        patch_job(&mut data, job_id, &json!({"launch_failure": null}));
        Ok((Some(dump_board(&data)), ()))
    });
    if let Err(e) = cleared {
        let _ = io.err.write_all(e.stderr.as_bytes());
        errln!(io, "factory.py: {job_id} launched, but its launch failure was not cleared; the pass goes on");
    }
}
