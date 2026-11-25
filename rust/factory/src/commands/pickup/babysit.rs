use super::*;

/// True when a pr-open job carries a dashboard Babysit PR click.
pub(super) fn babysit_due(j: &Value) -> bool {
    get_str(j, "stage") == Some("pr-open") && babysitting(j) && safe_id(j).is_some()
}

pub(super) fn claim_babysit(
    home: &Path,
    job_id: &str,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
) -> Res<Option<String>> {
    claim_for(home, job_id, device, minutes, paths, &|j| {
        babysit_due(j) && !paused(j) && !closing(j)
    })
}

/// argv with a workspace slug babysit-<id> and a title babysit <id>.
pub(super) fn babysit_argv(argv: Vec<String>, job_id: &str) -> Vec<String> {
    let mut argv = argv;
    for (flag, value) in [
        ("--worktree-slug", format!("babysit-{job_id}")),
        ("--title", format!("babysit {job_id}")),
    ] {
        if let Some(at) = argv[..argv.len() - 1].iter().position(|a| a == flag) {
            argv[at + 1] = value;
        }
    }
    let has = |a: &str| argv.iter().any(|x| x == a);
    if basename(&argv[0]) == "paseo"
        && has("--new-workspace")
        && !has("--worktree-slug")
        && has("run")
    {
        let at = argv.iter().position(|a| a == "run").unwrap() + 1;
        argv.splice(
            at..at,
            ["--worktree-slug".to_string(), format!("babysit-{job_id}")],
        );
    }
    argv
}

/// Why pickup on this host may not act on project, or None when it may.
pub(super) fn pickup_reason(
    io: &mut Io,
    home: &Path,
    project: &str,
    paths: &[(String, String)],
    settings: &mut Cache,
    device: &str,
    dry: bool,
) -> Res<Option<String>> {
    if !has_checkout(paths, Some(project)) {
        return Ok(Some("no checkout on this host".into()));
    }
    let s = cached_settings(
        io,
        home,
        project,
        lookup(paths, project).unwrap(),
        settings,
        device,
        dry,
    )?;
    Ok(settings_reason(&s, device))
}

/// The agents' status from this host's Paseo daemon, or None when paseo can't answer.
pub(super) fn paseo_statuses() -> Option<Vec<(Value, Value)>> {
    #[cfg(test)]
    if let Some(f) = fake!(paseo_statuses) {
        return f();
    }
    let exe = which("paseo")?.to_string_lossy().into_owned();
    let home = std::env::var("PASEO_HOME").unwrap_or_default();
    let mut argv = vec![exe.as_str(), "ls", "-a", "-g", "--json"];
    if !home.is_empty() {
        argv.extend(["--home", home.as_str()]);
    }
    let r = capture(&argv, None, &[], Some(Duration::from_secs(20))).ok()?;
    let agents = loads(&r.stdout).ok().filter(|_| r.code == 0)?;
    Some(
        agents
            .as_array()?
            .iter()
            .filter(|a| a.is_object())
            .map(|a| {
                (
                    a.get("id").cloned().unwrap_or(Value::Null),
                    a.get("status").cloned().unwrap_or(Value::Null),
                )
            })
            .collect(),
    )
}

/// An agent's status in a statuses list; Python's dict keeps the last of a repeated id.
pub(super) fn status_of(statuses: &[(Value, Value)], agent: &str) -> Option<String> {
    statuses
        .iter()
        .rev()
        .find(|(id, _)| id.as_str() == Some(agent))
        .and_then(|(_, s)| (!s.is_null()).then(|| text_of(Some(s))))
}

/// True when paseo inspect lists pending permission requests for agent, False when it lists
/// none, None when the answer is anything else.
pub(super) fn awaits_permission(agent: &str) -> Option<bool> {
    #[cfg(test)]
    if let Some(f) = fake!(awaits_permission) {
        return f(agent);
    }
    let (code, out) = paseo_run(&["inspect", agent, "--json"]);
    if code != 0 {
        return None;
    }
    let pending = loads(&out)
        .ok()?
        .as_object()?
        .get("PendingPermissions")?
        .clone();
    pending.as_array().map(|p| !p.is_empty())
}

/// Send one message to a Paseo agent without waiting for its turn. True when paseo accepted it.
pub(super) fn paseo_send(agent: &str, message: &str) -> bool {
    #[cfg(test)]
    if let Some(f) = fake!(paseo_send) {
        return f(agent, message);
    }
    let Some(exe) = which("paseo") else {
        return false;
    };
    let exe = exe.to_string_lossy().into_owned();
    let home = std::env::var("PASEO_HOME").unwrap_or_default();
    let mut argv = vec![exe.as_str(), "send", "--no-wait"];
    if !home.is_empty() {
        argv.extend(["--home", home.as_str()]);
    }
    argv.extend([agent, message]);
    capture(&argv, None, &[], Some(Duration::from_secs(20))).is_ok_and(|o| o.code == 0)
}

/// The babysit row for a babysit sent to the job's own Paseo agent.
pub(super) fn sent_row(job: &Value, tag: &str) -> Value {
    let link = json!({"server": job["paseo"]["server"], "agent": job["paseo"]["agent"]});
    let prior = job
        .get("agents")
        .and_then(Value::as_array)
        .and_then(|rows| rows.iter().rev().find(|r| r.get("paseo") == Some(&link)));
    let mut row = json!({"role": "babysit"});
    for k in ["harness", "model", "effort"] {
        if let Some(v) = prior.and_then(|p| p.get(k)).filter(|v| v.is_string()) {
            row[k] = v.clone();
        }
    }
    row["via"] = "paseo".into();
    row["session"] = tag.into();
    row["paseo"] = link;
    row
}

/// The PASEO_AGENT_ID of the session pickup itself runs in, if any.
pub(super) fn own_agent() -> Option<String> {
    std::env::var("PASEO_AGENT_ID").ok()
}

pub(super) fn paseo_agent(j: &Value) -> Option<String> {
    get_str(j.get("paseo")?, "agent")
        .filter(|a| PASEO_ID.is_full(a) && !a.starts_with('-'))
        .map(String::from)
}

/// Run at most one /factory babysit for a job the dashboard handed to one: in the job's own
/// Paseo agent when it has one, else in a new session.
#[allow(clippy::too_many_arguments)]
pub(super) fn babysit_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<()> {
    let mut statuses: Option<Option<Vec<(Value, Value)>>> = None;
    let mut job_agent = |j: &Value| -> (Option<String>, Option<String>) {
        let Some(agent) = paseo_agent(j) else {
            return (None, None);
        };
        let same = match (j["paseo"].get("server"), host_paseo_server()) {
            (None | Some(Value::Null), None) => true,
            (Some(Value::String(s)), Some(here)) => *s == here,
            _ => false,
        };
        if !same {
            return (Some(agent), Some("foreign".into()));
        }
        let known = statuses.get_or_insert_with(paseo_statuses);
        let state = match known {
            None => Some("unanswered".into()),
            Some(list) => status_of(list, &agent),
        };
        (Some(agent), state)
    };
    let launch = |io: &mut Io, j: &Value, tag: &str, route: &Option<Route>| -> Res<()> {
        let id = get_str(j, "id").unwrap();
        let Some(route) = route else {
            errln!(
                io,
                "pickup could not launch babysit {id}: no available route"
            );
            release_pickup_claim(home, id, device, tag)?;
            return Ok(());
        };
        let template = route_launcher(home, route)?;
        let project = get_str(j, "project").unwrap();
        let entry = Entry {
            id: id.into(),
            project: project.into(),
            repo_path: lookup(paths, project).unwrap().into(),
            session: tag.into(),
        };
        let given = Given::new(
            &entry,
            route_values(route),
            launch_prompt(&format!("/factory babysit {id} --session {tag}")),
        );
        let argv = babysit_argv(launcher_argv(&template, &given), id);
        let env = launch_env(&given, &route.provider, &route.model);
        let (ok, detail, log_path) = launch_idea(home, &entry, &route.name, &argv, &env)?;
        if ok {
            outln!(
                io,
                "launched babysit {id} {project} on {} session {tag}",
                route.name
            );
            let mut row = launch_row(&given, &argv, tag);
            row["role"] = "babysit".into();
            record_launch(io, home, id, row);
        } else {
            let mut release = || release_pickup_claim(home, id, device, tag);
            launch_failed(
                io,
                home,
                &route.name,
                id,
                &detail.unwrap_or_default(),
                &log_path,
                &mut release,
                false,
                false,
                None,
            )?;
        }
        Ok(())
    };
    let jobs: Vec<Value> = jobs_of(data)
        .iter()
        .filter(|j| babysit_due(j))
        .cloned()
        .collect();
    let mut ready = false;
    for j in &jobs {
        let id = get_str(j, "id").unwrap();
        let project_text = text_of(j.get("project"));
        let mut line;
        match get_str(j, "project").filter(|p| !p.is_empty()) {
            None => line = Some(format!("skip babysit {id}: no project")),
            Some(project) => {
                let head = format!("skip babysit {id} {project}");
                if gated(j) {
                    line = Some(format!("{head}: waiting on a human"));
                } else if paused(j) {
                    line = Some(format!("{head}: paused"));
                } else if closing(j) {
                    line = Some(format!("{head}: closing"));
                } else if let Some(c) = claims::blocking(Some(j), device, "", minutes)? {
                    line = Some(format!("{head}: {}", claimed_by(&c)));
                } else {
                    let (agent, state) = job_agent(j);
                    let agent = agent.unwrap_or_default();
                    line = match state.as_deref() {
                        Some("foreign") => Some(format!(
                            "{head}: agent {agent} lives on server {}",
                            text_of(j["paseo"].get("server"))
                        )),
                        Some("unanswered") => Some(format!("{head}: paseo ls failed")),
                        None | Some("idle") | Some("closed") => {
                            pickup_reason(io, home, project, paths, settings, device, dry)?
                                .map(|r| format!("{head}: {r}"))
                        }
                        Some(other) => Some(format!("{head}: agent {agent} is {other}")),
                    };
                }
            }
        }
        if line.is_none() && ready {
            line = Some(format!(
                "wait babysit {id} {project_text}: one babysit launch per run"
            ));
        } else if line.is_none() {
            let (agent, state) = job_agent(j);
            let agent = agent.unwrap_or_default();
            let waits = if state.as_deref() == Some("idle") {
                awaits_permission(&agent)
            } else {
                Some(false)
            };
            if waits != Some(false) {
                let why = if waits == Some(true) {
                    "waits at a permission dialog"
                } else {
                    "permission state unknown"
                };
                outln!(io, "skip babysit {id} {project_text}: agent {agent} {why}");
                continue;
            }
            ready = true;
            let route = pick_route(home)?;
            if matches!(state.as_deref(), Some("idle") | Some("closed")) {
                if dry {
                    line = Some(format!(
                        "would send babysit {id} {project_text} to agent {agent}"
                    ));
                } else {
                    let Some(tag) = claim_babysit(home, id, device, minutes, paths)? else {
                        return Ok(());
                    };
                    if paseo_send(
                        &agent,
                        &launch_prompt(&format!("/factory babysit {id} --session {tag}")),
                    ) {
                        outln!(
                            io,
                            "sent babysit {id} {project_text} to agent {agent} session {tag}"
                        );
                        record_launch(io, home, id, sent_row(j, &tag));
                        return Ok(());
                    }
                    if state.as_deref() == Some("idle") {
                        errln!(io, "pickup could not send babysit {id} to agent {agent}: paseo send failed; the next run retries");
                        release_pickup_claim(home, id, device, &tag)?;
                        return Ok(());
                    }
                    errln!(io, "pickup could not send babysit {id} to agent {agent}: paseo send failed; launching a new session");
                    return launch(io, j, &tag, &route);
                }
            } else if route.is_none() && !dry {
                return Ok(());
            } else if route.is_none() {
                line = Some(no_route_line(home)?);
            } else if dry {
                let route = route.as_ref().unwrap();
                let project = get_str(j, "project").unwrap();
                let entry = Entry {
                    id: id.into(),
                    project: project.into(),
                    repo_path: lookup(paths, project).unwrap().into(),
                    session: "<session>".into(),
                };
                let given = Given::new(
                    &entry,
                    route_values(route),
                    launch_prompt(&format!("/factory babysit {id} --session <session>")),
                );
                let argv = babysit_argv(launcher_argv(&route_launcher(home, route)?, &given), id);
                line = Some(format!(
                    "would launch babysit {id} {project} on {}: {}",
                    route.name,
                    shlex_join(&argv)
                ));
            } else {
                let Some(tag) = claim_babysit(home, id, device, minutes, paths)? else {
                    return Ok(());
                };
                return launch(io, j, &tag, &route);
            }
        }
        if dry {
            outln!(io, "{}", line.unwrap());
        }
    }
    Ok(())
}

/// Archive the Paseo sessions of, and stop the worktree of, each job the dashboard moved away
/// from this host, and record it. Only when the project's pickup is on and lists this host.
#[allow(clippy::too_many_arguments)]
pub(super) fn move_stop_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
    adapter: Option<&[String]>,
) -> Res<()> {
    for i in 0..ops::board_jobs(data)?.len() {
        let j = data["jobs"][i].clone();
        let m = j.get("move");
        let mine: Vec<Value> = queued_stops(m)
            .into_iter()
            .filter(|x| get_str(x, "from") == Some(device))
            .cloned()
            .collect();
        let (Some(job_id), Some(project)) = (
            safe_id(&j),
            get_str(&j, "project").filter(|p| SLUG.is_full(p)),
        ) else {
            continue;
        };
        if mine.is_empty() {
            continue;
        }
        let m = m.unwrap();
        let to = get_str(m, "to");
        let line = if to == Some(device) {
            format!("{job_id} {project}: rerun on {device}")
        } else {
            format!("{job_id} {project}: moved to {}", text_of(m.get("to")))
        };
        if let Some(reason) = pickup_reason(io, home, project, paths, settings, device, dry)? {
            if dry {
                outln!(io, "skip stop {job_id} {project}: {reason}");
            } else {
                errln!(io, "pickup skips the stop of {job_id} {project}: {reason}");
            }
            continue;
        }
        let (mut archives, mut kept, mut refused): (Vec<String>, Vec<String>, Vec<String>) =
            Default::default();
        let server = host_paseo_server();
        for x in &mine {
            let agent = get_str(x, "agent").filter(|a| !a.is_empty());
            let same_server = server.is_some() && get_str(x, "server").map(String::from) == server;
            let (Some(agent), true, true) = (
                agent,
                adapter.is_some() && same_server,
                own_agent().as_deref() != agent,
            ) else {
                continue;
            };
            let list = if !ADAPTER_ID.is_full(agent) {
                &mut refused
            } else if paseo_claimed(home, job_id, agent) {
                &mut archives
            } else {
                &mut kept
            };
            if !list.iter().any(|a| a == agent) {
                list.push(agent.to_string());
            }
        }
        let names = archives.join(", ");
        let noun = if archives.len() > 1 {
            "sessions"
        } else {
            "session"
        };
        if dry {
            if archives.is_empty() {
                outln!(io, "would stop {line}");
            } else {
                outln!(io, "would archive {noun} {names} and stop {line}");
            }
            continue;
        }
        let mut failure = None;
        for a in &archives {
            let why = match adapter_run(adapter.unwrap(), &["archive", a, "--force"]) {
                Ok(p) if p.code != 0 => Some(format!("exit {}", p.code)),
                Ok(_) => None,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => Some("timed out".into()),
                Err(e) => Some(os_error(&e, Some(Path::new(&adapter.unwrap()[0])))),
            };
            if let Some(why) = why {
                failure = Some((a.clone(), why));
                break;
            }
        }
        let wt = home.join("worktrees").join(project).join(job_id);
        let alive = if wt.exists() {
            stop_pids(&wt)
        } else {
            Some(Vec::new())
        };
        if let Some((agent, why)) = failure {
            errln!(
                io,
                "pickup could not archive session {agent} of {job_id}: {why}"
            );
            continue;
        }
        match alive {
            Some(pids) if !pids.is_empty() => {
                let pids: Vec<String> = pids.iter().map(u32::to_string).collect();
                errln!(
                    io,
                    "pickup could not stop {job_id}: processes still running: {}",
                    pids.join(" ")
                );
                continue;
            }
            None => {
                errln!(
                    io,
                    "pickup could not stop {job_id}: processes were not checked"
                );
                continue;
            }
            Some(_) => {}
        }
        let ats: Vec<Option<Value>> = mine.iter().map(|x| x.get("at").cloned()).collect();
        let done = cas(home, "/api/board", |text| {
            let mut board = load_board(text)?;
            ops::board_jobs(&board)?;
            let Some(at) = job_index(&board, job_id) else {
                return Ok((None, false));
            };
            let stamp = now();
            let mut hit = false;
            let job = &mut board["jobs"][at];
            let Some(m) = job.get_mut("move").filter(|m| m.is_object()) else {
                return Ok((None, false));
            };
            let mut stop = |x: &mut Value| {
                if x.is_object()
                    && truthy(x.get("from"))
                    && !move_stopped(x)
                    && get_str(x, "from") == Some(device)
                    && ats.contains(&x.get("at").cloned())
                {
                    x["stopped"] = stamp.clone().into();
                    if get_str(x, "agent").is_some_and(|a| archives.iter().any(|b| b == a)) {
                        x["archived"] = stamp.clone().into();
                    }
                    hit = true;
                }
            };
            if let Some(earlier) = m.get_mut("earlier").and_then(Value::as_array_mut) {
                earlier.iter_mut().for_each(&mut stop);
            }
            stop(m);
            if !hit {
                return Ok((None, false));
            }
            patch_job(&mut board, job_id, &json!({}));
            Ok((Some(dump_board(&board)), true))
        })?;
        if done {
            for a in &archives {
                forget_paseo_claim(home, job_id, a);
            }
            let stamp = now();
            if let Some(m) = data["jobs"][i].get_mut("move") {
                let mut stop = |x: &mut Value| {
                    if x.is_object()
                        && truthy(x.get("from"))
                        && !move_stopped(x)
                        && get_str(x, "from") == Some(device)
                    {
                        x["stopped"] = stamp.clone().into();
                    }
                };
                if let Some(earlier) = m.get_mut("earlier").and_then(Value::as_array_mut) {
                    earlier.iter_mut().for_each(&mut stop);
                }
                stop(m);
            }
            if archives.is_empty() {
                outln!(io, "stopped {line}");
            } else {
                outln!(io, "archived {noun} {names} and stopped {line}");
            }
            for a in &kept {
                errln!(io, "pickup kept session {a} of {job_id}: no claim recorded on this host, archive it by hand if it still runs{}", paseo_archive_tail(a));
            }
            for a in &refused {
                let shown = if PASEO_ID.is_full(a) {
                    a.clone()
                } else {
                    ascii(&a.chars().take(64).collect::<String>())
                };
                errln!(io, "pickup skipped archive of session {shown} of {job_id}: not an id the stall adapter accepts, archive it by hand if it still runs{}", paseo_archive_tail(a));
            }
        }
    }
    Ok(())
}
