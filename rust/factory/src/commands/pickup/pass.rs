use super::*;

/// The launcher's FACTORY_ environment, added to this process's own.
pub(super) fn launch_env(given: &Given, provider: &str, model: &str) -> Vec<(String, String)> {
    [
        ("FACTORY_JOB_ID", given.id.as_str()),
        ("FACTORY_PROJECT", &given.project),
        ("FACTORY_REPO_PATH", &given.repo_path),
        ("FACTORY_SESSION", &given.session),
        ("FACTORY_PROVIDER", provider),
        ("FACTORY_MODEL", model),
        ("FACTORY_EFFORT", &given.effort),
        ("FACTORY_PROMPT", &given.prompt),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// The last 4096 bytes of a launch log, decoded with replacement.
pub(super) fn log_tail(log_path: &Path) -> Res<String> {
    let data = fs::read(log_path).map_err(|e| fail(os_error(&e, Some(log_path)), 1))?;
    let tail = &data[data.len().saturating_sub(4096)..];
    Ok(String::from_utf8_lossy(tail).into_owned())
}

/// After a failed launch: mark the route limited when its log says so, else exit 1, or with
/// fatal false log the failure and return. release drops the claim first, except that a non-limit
/// failure with backoff parks the job instead.
#[allow(clippy::too_many_arguments)]
pub(super) fn launch_failed(
    io: &mut Io,
    home: &Path,
    name: &str,
    job_id: &str,
    detail: &str,
    log_path: &Path,
    release: &mut dyn FnMut() -> Res<Parked>,
    fatal: bool,
    experiment: bool,
    backoff: Option<&mut dyn FnMut() -> Res<Parked>>,
) -> Res<()> {
    let tail = log_tail(log_path)?;
    if LIMIT_TEXT.search(&tail).is_some() {
        let until = limit_route(home, name, limit_minutes(&tail, unix_now()))?.0;
        release()?;
        if experiment {
            outln!(io, "limited experiment {name} until {until}");
        } else {
            outln!(io, "limited {name} until {until}");
        }
        outln!(io, "released {job_id}");
        return Ok(());
    }
    let parked = match backoff {
        Some(backoff) => backoff()?,
        None => release()?,
    };
    outln!(io, "released {job_id}");
    match parked {
        Some((count, None)) => outln!(
            io,
            "needs-human {job_id} after {count} launch failures in a row"
        ),
        Some((count, Some(until))) => outln!(
            io,
            "backed off {job_id} until {until} (launch failure {count})"
        ),
        None => {}
    }
    let message = format!(
        "launch of {job_id} on {name} failed: {detail}; see {}",
        log_path.display()
    );
    if fatal {
        return Err(fail(message, 1));
    }
    errln!(io, "{message}");
    Ok(())
}

pub(super) fn dated(j: &Value, key: &str) -> bool {
    j.get(key)
        .is_some_and(|v| v.get("at").is_some_and(Value::is_string))
}

/// True when the dashboard asked pickup to take this job first.
pub(super) fn prioritized(j: &Value) -> bool {
    dated(j, "priority")
}

/// True when the dashboard handed this job's PR to a babysit session.
pub(super) fn babysitting(j: &Value) -> bool {
    dated(j, "babysit")
}

/// True when the dashboard paused pickup for this job.
pub(super) fn paused(j: &Value) -> bool {
    dated(j, "paused")
}

/// The job's not_before stamp while it is still in the future, else None.
pub(super) fn waiting_until(j: &Value) -> Option<String> {
    get_str(j, "not_before")
        .filter(|t| NOT_BEFORE.is_full(t) && *t > now().as_str())
        .map(String::from)
}

/// The first depends-on id of an idea whose job is still open on the board and is not a draft
/// PR, else None.
pub(super) fn waiting_on(j: &Value, jobs: &[Value]) -> Option<String> {
    if get_str(j, "stage") != Some(IDEA) {
        return None;
    }
    let open = open_job_ids(jobs);
    let drafts: Vec<&Value> = jobs
        .iter()
        .filter(|x| draft_pr(x))
        .filter_map(|x| x.get("id"))
        .collect();
    j.get("refs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| get_str(r, "kind") == Some("depends-on"))
        .filter_map(|r| r.get("id"))
        .find(|id| open.contains(id) && !drafts.contains(id))
        .map(|id| text_of(Some(id)))
}

/// True when the dashboard queued a close that pickup has not done yet.
pub(super) fn closing(j: &Value) -> bool {
    dated(j, "close") && j["close"].get("closed").is_none()
}

/// True when the dashboard moved this in-flight job to this host, no pickup claimed it yet, and
/// no stop of it is still queued on this host.
pub(super) fn moved_here(j: &Value, device: &str) -> bool {
    let Some(m) = j.get("move").filter(|m| m.is_object()) else {
        return false;
    };
    get_str(m, "to") == Some(device)
        && m.get("claimed").is_none()
        && get_str(j, "stage").is_some_and(|s| IN_FLIGHT.contains(&s))
        && !queued_stops(Some(m))
            .iter()
            .any(|x| get_str(x, "from") == Some(device))
}

/// Python's `f"{n:,}"`.
pub(super) fn commas(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// {job id: skip line} for every free job once this host's job tokens since local midnight reach
/// daily_token_cap.
pub(super) fn daily_cap_holds(
    io: &mut Io,
    home: &Path,
    device: &str,
    jobs: &[&Value],
    free: &dyn Fn(&Value) -> bool,
    dry: bool,
) -> Res<HashMap<String, String>> {
    let cap = token_cap(io, home, "daily_token_cap")?;
    let held: Vec<&&Value> = match cap {
        Some(_) => jobs.iter().filter(|j| free(j)).collect(),
        None => Vec::new(),
    };
    let (Some(cap), false) = (cap, held.is_empty()) else {
        return Ok(HashMap::new());
    };
    let used = match daily_tokens(local_midnight()) {
        Ok(used) => used,
        Err(kind) => {
            errln!(io, "pickup skips the daily token cap: scan failed ({kind})");
            return Ok(HashMap::new());
        }
    };
    if used < cap {
        return Ok(HashMap::new());
    }
    let reached = format!(
        "daily token cap reached ({}/{} since local midnight)",
        commas(used),
        commas(cap)
    );
    if !dry {
        errln!(io, "pickup launches no new job on {device}: {reached}");
    }
    Ok(held
        .into_iter()
        .map(|j| {
            let id = get_str(j, "id").unwrap_or_default();
            (
                id.to_string(),
                format!(
                    "skip {id} {}: host {device} {reached}",
                    text_of(j.get("project"))
                ),
            )
        })
        .collect())
}

/// One pickup run after flags and the holder are resolved. Reads the board unless given it.
#[allow(clippy::too_many_arguments)]
pub(super) fn pickup_pass(
    io: &mut Io,
    home: &Path,
    device: &str,
    minutes: u32,
    dry: bool,
    limit: usize,
    cap: Option<usize>,
    data: Option<Value>,
) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(pickup_pass) {
        return f(io, home);
    }
    let paths = paths_of(home)?;
    let mut data = match data {
        Some(data) => data,
        None => read_board(home)?,
    };
    let stalls = read_stalls(home)?;
    let mut settings: HashMap<String, Settings> = HashMap::new();
    let adapter = stalls.as_ref().map(|s| s.0.clone());
    move_stop_pass(
        io,
        home,
        &mut data,
        device,
        &paths,
        &mut settings,
        dry,
        adapter.as_deref(),
    )?;
    session_stop_pass(io, home, &mut data, device, dry)?;
    pr_sync_pass(
        io,
        home,
        &mut data,
        device,
        minutes,
        &paths,
        &mut settings,
        dry,
    )?;
    needs_human_recheck_pass(io, home, &mut data, device, dry)?;
    autonomy_hold_pass(io, home, &mut data, dry)?;
    close_pass(io, home, &mut data, device, minutes, dry)?;
    close_cleanup_pass(io, home, &mut data, device, &paths, &mut settings, dry)?;
    priority_nudge_pass(io, home, &mut data, device, dry)?;
    idle_watch_pass(io, home, &mut data, device, dry)?;
    token_cap_pass(io, home, &mut data, device, dry)?;
    auto_babysit_pass(
        io,
        home,
        &mut data,
        device,
        minutes,
        &paths,
        &mut settings,
        dry,
    )?;
    babysit_pass(
        io,
        home,
        &mut data,
        device,
        minutes,
        &paths,
        &mut settings,
        dry,
    )?;
    let taken = match &stalls {
        Some(_) => takeover(
            io,
            home,
            &mut data,
            device,
            minutes,
            &paths,
            &mut settings,
            dry,
        )?,
        None => 0,
    };
    deny_notes_pass(
        io,
        home,
        &mut data,
        device,
        minutes,
        &paths,
        &mut settings,
        dry,
    )?;
    resolve_pass(
        io,
        home,
        &mut data,
        device,
        minutes,
        &paths,
        &mut settings,
        dry,
    )?;
    if !dry && taken >= limit {
        return Ok(());
    }
    let mut jobs: Vec<&Value> = jobs_of(&data)
        .iter()
        .filter(|j| {
            (get_str(j, "stage") == Some(IDEA) || approved_start(j) || moved_here(j, device))
                && safe_id(j).is_some()
        })
        .collect();
    jobs.sort_by_key(|j| !prioritized(j));
    let id_of = |j: &Value| safe_id(j).unwrap().to_string();
    let starts: Vec<String> = jobs
        .iter()
        .filter(|j| approved_start(j) || moved_here(j, device))
        .map(|j| id_of(j))
        .collect();
    let (mut experiment, mut exp_limited) = (None, None);
    // Only an experiment line needs routes.md this early; without one, a host with no routes stays silent.
    let routes_md = home.join("routes.md");
    if !jobs.is_empty() && lookup(&meta(&routes_md)?, "experiment").is_some() {
        let names: Vec<String> = read_routes(home)?.into_iter().map(|r| r.name).collect();
        let e = read_experiment(home, &names)?.unwrap();
        let limits = read_limits(home)?;
        exp_limited = lookup(&limits, &e.name)
            .filter(|u| *u > now().as_str())
            .map(String::from);
        experiment = Some(e);
    }
    let job_arm = |j: Option<&Value>| -> Option<String> {
        let j = j?;
        let record = j.get("experiment").filter(|r| r.is_object());
        match record.and_then(|r| get_str(r, "arm")) {
            Some(arm @ ("a" | "b")) => Some(arm.to_string()),
            _ => experiment
                .as_ref()
                .map(|e| experiment_arm(&id_of(j), e.percent).to_string()),
        }
    };
    let all = jobs_of(&data);
    let base_skip = |j: &Value| -> Res<Option<String>> {
        let id = id_of(j);
        let Some(project) = get_str(j, "project").filter(|p| !p.is_empty()) else {
            return Ok(Some(format!("skip {id}: no project")));
        };
        if !lookup(&paths, project).is_some_and(is_dir) {
            return Ok(Some(format!(
                "skip {id} {project}: no checkout on this host"
            )));
        }
        if gated(j) {
            return Ok(Some(format!("skip {id} {project}: waiting on a human")));
        }
        if paused(j) {
            return Ok(Some(format!("skip {id} {project}: paused")));
        }
        if closing(j) {
            return Ok(Some(format!("skip {id} {project}: closing")));
        }
        if let Some(until) = waiting_until(j) {
            return Ok(Some(format!("skip {id} {project}: not before {until}")));
        }
        if let Some(other) = waiting_on(j, all) {
            return Ok(Some(format!("skip {id} {project}: waits on {other}")));
        }
        if tracer_building(j) {
            return Ok(Some(format!("skip {id} {project}: building in Tracer")));
        }
        if let Some(c) = claims::blocking(Some(j), device, "", minutes)? {
            return Ok(Some(format!("skip {id} {project}: {}", claimed_by(&c))));
        }
        if let (Some(e), Some(until)) = (&experiment, &exp_limited) {
            if job_arm(Some(j)).as_deref() == Some("b") {
                return Ok(Some(format!(
                    "skip {id} {project}: experiment {} limited until {until}",
                    e.name
                )));
            }
        }
        Ok(None)
    };
    let base: HashMap<String, Option<String>> = jobs
        .iter()
        .map(|j| Ok((id_of(j), base_skip(j)?)))
        .collect::<Res<_>>()?;
    let runnable = |j: &Value| base[&id_of(j)].is_none();
    let mut waiting: HashMap<Option<String>, i64> = HashMap::new();
    for j in all {
        if waits_on_you(j) {
            *waiting
                .entry(get_str(j, "project").map(String::from))
                .or_default() += 1;
        }
    }
    let mut holds = batch_holds(home, &jobs, &runnable)?;
    holds.extend(backpressure_holds(home, &jobs, &waiting, &runnable)?);
    holds.extend(daily_cap_holds(io, home, device, &jobs, &runnable, dry)?);
    let skip = |j: &Value| base[&id_of(j)].is_some() || holds.contains_key(&id_of(j));

    let mut eligible: Vec<String> = Vec::new();
    for j in &jobs {
        let slug = get_str(j, "project").unwrap_or_default().to_string();
        if skip(j) || eligible.contains(&slug) {
            continue;
        }
        if !settings.contains_key(&slug) {
            let found = pickup_settings(home, &slug, lookup(&paths, &slug).unwrap(), device)?;
            if let (Some(reason), false) = (&found.reason, dry) {
                errln!(io, "pickup skips {slug}: {reason}");
            }
            settings.insert(slug.clone(), found);
        }
        if settings[&slug].lists(device) {
            eligible.push(slug);
        }
    }

    let host_cap = if eligible.is_empty() {
        None
    } else {
        host_max_inflight(home, device)?
    };
    let by_host = host_cap.is_some();
    let cap = match host_cap {
        Some(h) => cap.map_or(h, |c| c.min(h)),
        None => cap.unwrap_or(1),
    };
    let route = if eligible.is_empty() {
        None
    } else {
        pick_route(home)?
    };
    let template = match &route {
        Some(route) => route_launcher(home, route)?,
        None => Vec::new(),
    };
    let values = |entry: &Entry| {
        let verb = if starts.contains(&entry.id) {
            "recover"
        } else {
            "idea"
        };
        let route = route.as_ref().unwrap();
        Given::new(
            entry,
            route_values(route),
            launch_prompt(&format!(
                "/factory {verb} {} --session {}",
                entry.id, entry.session
            )),
        )
    };
    let job_given = |j: Option<&Value>, entry: &Entry| -> (Option<String>, Given) {
        let mut given = values(entry);
        let arm = job_arm(j);
        if arm.as_deref() == Some("b") {
            let record = j
                .and_then(|j| j.get("experiment"))
                .filter(|r| r.is_object() && truthy(r.get("provider")) && truthy(r.get("model")));
            if let Some(r) = record {
                given.provider = text_of(r.get("provider"));
                given.model = text_of(r.get("model"));
                given.effort = get_str(r, "effort").unwrap_or_default().to_string();
            } else if let Some(e) = &experiment {
                given.provider = e.provider.clone();
                given.model = e.model.clone();
                given.effort = e.effort.clone().unwrap_or_default();
            }
        }
        (arm, given)
    };

    if dry {
        let (mut would, mut no_route) = (Vec::<(String, usize)>::new(), false);
        for j in &jobs {
            let slug = get_str(j, "project").unwrap_or_default().to_string();
            let mut line = base[&id_of(j)].clone();
            if line.is_none() {
                let id = id_of(j);
                if !settings.contains_key(&slug) {
                    let found =
                        pickup_settings(home, &slug, lookup(&paths, &slug).unwrap(), device)?;
                    settings.insert(slug.clone(), found);
                }
                let s = &settings[&slug];
                let count = if by_host {
                    inflight_count(&data, None, minutes, Some(device))
                        + would.iter().map(|(_, n)| n).sum::<usize>()
                } else {
                    inflight_count(&data, Some(&slug), minutes, None) + lookup_count(&would, &slug)
                };
                line = Some(if let Some(reason) = &s.reason {
                    format!("skip {id} {slug}: {reason}")
                } else if s.enabled != Some(true) {
                    format!("skip {id} {slug}: pickup off")
                } else if !s.hosts.iter().any(|h| h == device) {
                    format!("skip {id} {slug}: pickup does not list host {device}")
                } else if let Some(hold) = holds.get(&id) {
                    hold.clone()
                } else if by_host && cap == 0 {
                    format!("skip {id} {slug}: host {device} paused (max in flight 0)")
                } else if count >= cap && by_host {
                    format!("skip {id} {slug}: host {device} in-flight cap full ({count}/{cap})")
                } else if count >= cap {
                    format!("skip {id} {slug}: in-flight cap full ({count}/{cap})")
                } else if route.is_none() {
                    if no_route {
                        continue;
                    }
                    no_route = true;
                    no_route_line(home)?
                } else {
                    match would.iter_mut().find(|(s, _)| *s == slug) {
                        Some(slot) => slot.1 += 1,
                        None => would.push((slug.clone(), 1)),
                    }
                    let entry = Entry {
                        id: id.clone(),
                        project: slug.clone(),
                        repo_path: lookup(&paths, &slug).unwrap().to_string(),
                        session: "<session>".into(),
                    };
                    let (arm, given) = job_given(Some(j), &entry);
                    let shown =
                        arm.map_or(String::new(), |a| format!(" arm {a} model {}", given.model));
                    format!(
                        "would launch {id} {slug} on {}{shown}: {}",
                        route.as_ref().unwrap().name,
                        shlex_join(&launcher_argv(&template, &given))
                    )
                });
            }
            outln!(io, "{}", line.unwrap());
        }
        if stalls.is_some() {
            outln!(io, "host {device}: would take over {taken}");
        }
        let total: usize = would.iter().map(|(_, n)| n).sum();
        outln!(io, "host {device}: would launch {total}");
        return Ok(());
    }
    let Some(route) = route.clone() else {
        return Ok(());
    };
    let skip_ids: Vec<String> = holds.keys().cloned().collect();
    let mut launches = taken;
    while launches < limit {
        let mut picked = Vec::new();
        for slug in &eligible {
            let pick = Pick {
                max_inflight: Some(cap),
                project: Some(slug),
                by_host,
                builds: true,
                experiment: experiment.as_ref(),
                route: Some(&route),
                skip_ids: skip_ids.clone(),
            };
            picked = claim_ideas(home, device, "", minutes, &paths, 1, &pick)?;
            if !picked.is_empty() {
                break;
            }
        }
        let Some(entry) = picked.into_iter().next() else {
            return Ok(());
        };
        let j = jobs_of(&data)
            .iter()
            .find(|x| get_str(x, "id") == Some(entry.id.as_str()));
        let (arm, given) = job_given(j, &entry);
        let argv = launcher_argv(&template, &given);
        let env = launch_env(&given, &given.provider, &given.model);
        let (ok, detail, log_path) = launch_idea(home, &entry, &route.name, &argv, &env)?;
        if ok {
            outln!(
                io,
                "launched {} {} on {} session {}",
                entry.id,
                entry.project,
                route.name,
                entry.session
            );
            record_launch(
                io,
                home,
                &entry.id,
                launch_row(&given, &argv, &entry.session),
            );
            if j.is_some_and(|j| j.get("launch_failure").is_some()) {
                clear_launch_failure(io, home, &entry.id);
            }
            launches += 1;
            continue;
        }
        let b = arm.as_deref() == Some("b") && experiment.is_some();
        let limit_name = if b {
            experiment.as_ref().unwrap().name.clone()
        } else {
            route.name.clone()
        };
        let detail = detail.unwrap_or_default();
        let reason = launch_reason(&detail);
        let mut release = || release_claim(home, &entry.id, device, &entry.session).map(|_| None);
        let mut backoff = || back_off_launch(home, &entry.id, device, &entry.session, &reason);
        return launch_failed(
            io,
            home,
            &limit_name,
            &entry.id,
            &detail,
            &log_path,
            &mut release,
            true,
            b,
            Some(&mut backoff),
        );
    }
    Ok(())
}

pub(super) fn lookup_count(pairs: &[(String, usize)], key: &str) -> usize {
    pairs.iter().find(|(k, _)| k == key).map_or(0, |(_, n)| *n)
}

/// The dry-run line when every route is limited.
pub(super) fn no_route_line(home: &Path) -> Res<String> {
    let stamp = now();
    let limits = read_limits(home)?;
    let routes = read_routes(home)?;
    let shown: Vec<String> = routes
        .iter()
        .map(|r| {
            let until = lookup(&limits, &r.name).filter(|u| *u > stamp.as_str());
            format!("{} until {}", r.name, until.unwrap_or_default())
        })
        .collect();
    Ok(format!(
        "no route: no available route: {}",
        shown.join(", ")
    ))
}

/// True when Tracer scope approval or the dashboard's Approve spec moved this job to build and no
/// session has started it.
pub(super) fn approved_start(j: &Value) -> bool {
    get_str(j, "stage") == Some("build")
        && get_str(j, "note").is_some_and(|n| SPEC_APPROVED_NOTES.contains(&n))
}

/// True while a job waits on a human: it has an ask, or it is an unapproved explorer card.
pub(super) fn gated(j: &Value) -> bool {
    if get_str(j, "needs").is_some_and(|n| !strip(n).is_empty()) {
        return true;
    }
    j.get("origin").is_some_and(|o| {
        o.is_object() && get_str(o, "gate") == Some("human") && !truthy(o.get("approved"))
    })
}

/// True when pickup marked this job as held by a Tracer build agent.
pub(super) fn tracer_building(j: &Value) -> bool {
    j.get("tracer").and_then(|t| t.get("building")) == Some(&Value::Bool(true))
}
