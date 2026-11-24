use super::*;

/// The jobs dict of <home>/idle-watch.json, {} when the file is missing or malformed.
pub(super) fn read_idle_watch(home: &Path) -> Map<String, Value> {
    read_json(&home.join("idle-watch.json"))
        .filter(|d| d.is_object() && eq_int(d.get("version"), 1))
        .and_then(|d| d.get("jobs").and_then(Value::as_object).cloned())
        .unwrap_or_default()
}

/// Nudge the live Paseo agent of each job prioritized after its session was claimed, once.
pub(super) fn priority_nudge_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    dry: bool,
) -> Res<()> {
    let _ = device;
    let server = host_paseo_server();
    let due = |j: &Value| {
        let stage = get_str(j, "stage");
        let p = j.get("priority");
        let since = j
            .get("claim")
            .filter(|c| c.is_object())
            .and_then(|c| get_str(c, "since"));
        prioritized(j)
            && p.is_some_and(|p| p.get("nudged").is_none())
            && (stage == Some(IDEA) || stage.is_some_and(|s| IN_FLIGHT.contains(&s)))
            && !gated(j)
            && !paused(j)
            && !closing(j)
            && safe_id(j).is_some()
            && server.is_some()
            && j.get("paseo")
                .filter(|l| l.is_object())
                .and_then(|l| get_str(l, "server"))
                .map(String::from)
                == server
            && since.is_some_and(|s| get_str(&j["priority"], "at").unwrap() > s)
    };
    let jobs: Vec<Value> = jobs_of(data).iter().filter(|j| due(j)).cloned().collect();
    if jobs.is_empty() {
        return Ok(());
    }
    let Some(statuses) = paseo_statuses() else {
        errln!(
            io,
            "factory.py: pickup skips priority nudges: paseo did not answer"
        );
        return Ok(());
    };
    for j in &jobs {
        let job_id = get_str(j, "id").unwrap();
        let agent_value = j["paseo"].get("agent");
        let agent = text_of(agent_value);
        let status = agent_value
            .and_then(Value::as_str)
            .and_then(|a| status_of(&statuses, a));
        if matches!(status.as_deref(), None | Some("closed")) {
            if dry {
                outln!(
                    io,
                    "skip nudge {job_id}: agent {agent} is {}",
                    status.as_deref().unwrap_or("missing")
                );
            }
            continue;
        }
        let line = format!("{job_id} {}: agent {agent}", text_of(j.get("project")));
        let waits = awaits_permission(&agent);
        if waits != Some(false) {
            let why = if waits == Some(true) {
                "waits at a permission dialog"
            } else {
                "permission state unknown"
            };
            outln!(io, "skip nudge {line} {why}");
            continue;
        }
        if dry {
            outln!(io, "would nudge {line}");
            continue;
        }
        if !paseo_send(&agent, &PRIORITY_NUDGE.replace("{id}", job_id)) {
            errln!(
                io,
                "factory.py: pickup could not nudge {job_id}: paseo send failed"
            );
            continue;
        }
        let at = j["priority"]["at"].clone();
        let done = cas(home, "/api/board", |text| {
            let mut board = load_board(text)?;
            let p =
                job_index(&board, job_id).and_then(|b| board["jobs"][b].get("priority").cloned());
            if !p.as_ref().is_some_and(|p| {
                p.is_object() && p.get("at") == Some(&at) && p.get("nudged").is_none()
            }) {
                return Ok((None, false));
            }
            patch_job(&mut board, job_id, &json!({"priority": {"nudged": now()}}));
            Ok((Some(dump_board(&board)), true))
        })?;
        if done {
            outln!(io, "nudged {line}");
        }
    }
    Ok(())
}

/// The job when it is in flight, claimed on this host and still its own: the idle and token checks.
pub(super) fn claimed_here(j: &Value, device: &str) -> bool {
    get_str(j, "stage").is_some_and(|s| IN_FLIGHT.contains(&s))
        && j.get("claim")
            .filter(|c| c.is_object())
            .and_then(|c| get_str(c, "device"))
            == Some(device)
        && safe_id(j).is_some()
        && get_str(j, "project").is_some_and(|p| SLUG.is_full(p))
}

/// True when the daemon's heartbeat refreshed the job's claim within IDLE_NUDGE_MINUTES: seen
/// is that recent and newer than the job's updated stamp, which `hold` and board writes move with
/// it. The heartbeat refreshes an idle agent only while a subagent it waits on runs.
pub(super) fn beating(j: &Value) -> bool {
    let seen = j
        .get("claim")
        .filter(|c| c.is_object())
        .and_then(|c| stamp_of(c.get("seen")));
    seen.is_some_and(|seen| {
        seen >= unix_now() - IDLE_NUDGE_MINUTES * 60
            && stamp_of(j.get("updated")).is_none_or(|updated| seen > updated)
    })
}

/// Nudge the idle Paseo agent of an in-flight job claimed on this host once its worktree and
/// board job sit unchanged for IDLE_NUDGE_MINUTES, and move it to needs-human when they stay
/// unchanged that long again.
pub(super) fn idle_watch_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    dry: bool,
) -> Res<()> {
    let server = host_paseo_server();
    let due = |j: &Value| {
        claimed_here(j, device)
            && server.is_some()
            && j.get("paseo")
                .filter(|l| l.is_object())
                .and_then(|l| get_str(l, "server"))
                .map(String::from)
                == server
            && !gated(j)
            && !paused(j)
            && !closing(j)
            && lead_of(j).is_none()
    };
    let old = read_idle_watch(home);
    let mut watch = Map::new();
    let jobs: Vec<Value> = jobs_of(data).iter().filter(|j| due(j)).cloned().collect();
    let statuses = if jobs.is_empty() {
        Some(Vec::new())
    } else {
        paseo_statuses()
    };
    let Some(statuses) = statuses else {
        errln!(
            io,
            "factory.py: pickup skips idle checks: paseo did not answer"
        );
        return Ok(());
    };
    let cutoff = format_stamp(unix_now() - IDLE_NUDGE_MINUTES * 60);
    for j in &jobs {
        let (job_id, project) = (get_str(j, "id").unwrap(), get_str(j, "project").unwrap());
        let agent_value = j["paseo"].get("agent").cloned().unwrap_or(Value::Null);
        let agent = text_of(Some(&agent_value));
        let prev = old
            .get(job_id)
            .filter(|p| p.is_object())
            .cloned()
            .unwrap_or(json!({}));
        let idle = agent_value
            .as_str()
            .and_then(|a| status_of(&statuses, a))
            .as_deref()
            == Some("idle")
            && !beating(j);
        if !idle {
            if prev.as_object().is_some_and(|p| !p.is_empty()) {
                watch.insert(job_id.into(), prev);
            }
            continue;
        }
        let wt = home.join("worktrees").join(project).join(job_id);
        let there = wt.exists();
        let (head, tree) = if there {
            let status = git(&wt, &["status", "--porcelain"]);
            (
                git(&wt, &["rev-parse", "HEAD"]),
                crate::shared::project::sha(status.as_bytes())[..12].to_string(),
            )
        } else {
            (String::new(), String::new())
        };
        let sig =
            json!({"agent": agent_value, "head": head, "tree": tree, "updated": j.get("updated")});
        let changed = sig
            .as_object()
            .unwrap()
            .iter()
            .any(|(k, v)| prev.get(k).unwrap_or(&Value::Null) != v);
        if changed || !prev.get("since").is_some_and(Value::is_string) {
            let mut entry = sig;
            entry["since"] = now().into();
            watch.insert(job_id.into(), entry);
            continue;
        }
        let mut entry = prev.clone();
        let line = format!("{job_id} {project}: agent {agent}");
        if entry.get("nudged").is_none() {
            if get_str(&entry, "since").unwrap() >= cutoff.as_str() {
                watch.insert(job_id.into(), entry);
                continue;
            }
            let waits = awaits_permission(&agent);
            if waits != Some(false) {
                let why = if waits == Some(true) {
                    "waits at a permission dialog"
                } else {
                    "permission state unknown"
                };
                outln!(io, "skip idle nudge {line} {why}");
                if waits == Some(true) && entry.get("surfaced").is_none() {
                    let note = PERMISSION_NOTE
                        .replace("{agent}", &agent)
                        .replace("{minutes}", &IDLE_NUDGE_MINUTES.to_string());
                    if dry {
                        outln!(io, "would surface {job_id} {project}: {note}");
                        watch.insert(job_id.into(), entry);
                        continue;
                    }
                    let stage = j.get("stage").cloned();
                    let updated = cas(home, "/api/board", |text| {
                        let mut board = load_board(text)?;
                        let Some(b) = job_index(&board, job_id) else {
                            return Ok((None, None));
                        };
                        let found = &board["jobs"][b];
                        if found.get("stage").cloned() != stage
                            || found
                                .get("claim")
                                .filter(|c| c.is_object())
                                .and_then(|c| get_str(c, "device"))
                                != Some(device)
                        {
                            return Ok((None, None));
                        }
                        if get_str(found, "note") == Some(note.as_str()) {
                            return Ok((None, found.get("updated").cloned()));
                        }
                        let row = patch_job(&mut board, job_id, &json!({"note": note}));
                        Ok((Some(dump_board(&board)), row.get("updated").cloned()))
                    })?;
                    if let Some(updated) = updated.filter(|u| truthy(Some(u))) {
                        entry["surfaced"] = now().into();
                        entry["updated"] = updated;
                        outln!(io, "surfaced {job_id} {project}: {note}");
                    }
                }
            } else if dry {
                outln!(io, "would nudge idle {line}");
            } else if !paseo_send(&agent, &IDLE_NUDGE.replace("{id}", job_id)) {
                errln!(
                    io,
                    "factory.py: pickup could not nudge idle {job_id}: paseo send failed"
                );
            } else {
                entry["nudged"] = now().into();
                outln!(io, "nudged idle {line}");
            }
            watch.insert(job_id.into(), entry);
            continue;
        }
        watch.insert(job_id.into(), entry.clone());
        if !get_str(&entry, "nudged").is_some_and(|n| n < cutoff.as_str()) {
            continue;
        }
        let note = format!(
            "idle: no progress {IDLE_NUDGE_MINUTES} minutes after a nudge to agent {agent}"
        );
        if dry {
            outln!(io, "would move needs-human {job_id} {project}: {note}");
            continue;
        }
        let (stage, updated) = (j.get("stage").cloned(), j.get("updated").cloned());
        let done = cas(home, "/api/board", |text| {
            let mut board = load_board(text)?;
            let Some(b) = job_index(&board, job_id) else {
                return Ok((None, false));
            };
            let found = &board["jobs"][b];
            if found.get("stage").cloned() != stage
                || found.get("updated").cloned() != updated
                || found
                    .get("claim")
                    .filter(|c| c.is_object())
                    .and_then(|c| get_str(c, "device"))
                    != Some(device)
            {
                return Ok((None, false));
            }
            patch_job(
                &mut board,
                job_id,
                &json!({"stage": "needs-human", "note": note, "claim": null}),
            );
            Ok((Some(dump_board(&board)), true))
        })?;
        if done {
            outln!(io, "needs-human {job_id} {project}: {note}");
        }
        watch.shift_remove(job_id);
    }
    if !dry && watch != old {
        write_file(
            &home.join("idle-watch.json"),
            &(dumps(&json!({"version": 1, "jobs": watch}), INDENT2) + "\n"),
        )?;
    }
    Ok(())
}

/// {"transcripts": n, "models": {model: {kind: tokens}}} summed over the job's transcripts here.
pub(super) fn usage_totals(
    io: &mut Io,
    home: &Path,
    slug: &str,
    job_id: &str,
) -> Result<Value, String> {
    #[cfg(test)]
    if let Some(f) = fake!(usage_totals) {
        return f(home, slug, job_id);
    }
    let mut totals: BTreeMap<String, [i64; 4]> = BTreeMap::new();
    let mut read = 0;
    for (path, main) in usage_transcripts(home, slug, job_id) {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let Ok((models, bad)) = usage_scan(&path, main, None) else {
            errln!(io, "factory.py: usage: cannot read {name}");
            continue;
        };
        read += 1;
        if bad > 0 {
            errln!(
                io,
                "factory.py: usage: {bad} unreadable line{} in {name}",
                if bad == 1 { "" } else { "s" }
            );
        }
        for (model, counts) in models {
            let total = totals.entry(model).or_default();
            for (k, (kind, _)) in USAGE_TOKENS.iter().enumerate() {
                total[k] += int_of(counts.get(*kind)).unwrap_or(0);
            }
        }
    }
    let models: Map<String, Value> = totals
        .iter()
        .map(|(m, c)| (m.clone(), counts_value(c)))
        .collect();
    Ok(json!({"transcripts": read, "models": models}))
}

/// Warn once when an in-flight job claimed on this host passes TOKEN_WARN_PERCENT of config.md's
/// job_token_cap, and park it at needs-human and stop its Paseo agent when it reaches the cap.
pub(super) fn token_cap_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    dry: bool,
) -> Res<()> {
    let Some(cap) = token_cap(io, home, "job_token_cap")? else {
        return Ok(());
    };
    let jobs: Vec<Value> = jobs_of(data)
        .iter()
        .filter(|j| claimed_here(j, device) && !closing(j))
        .cloned()
        .collect();
    for j in &jobs {
        let (job_id, project) = (get_str(j, "id").unwrap(), get_str(j, "project").unwrap());
        let fresh = match usage_totals(io, home, project, job_id) {
            Ok(fresh) => fresh,
            Err(kind) => {
                errln!(
                    io,
                    "factory.py: pickup skips the token cap of {job_id}: scan failed ({kind})"
                );
                continue;
            }
        };
        let others: i64 = j
            .get("tokens")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(host, e)| *host != device && e.get("models").is_some_and(Value::is_object))
            .map(|(_, e)| token_count(&e["models"]))
            .sum();
        let total = token_count(&fresh["models"]) + others;
        let state = j
            .get("token_cap")
            .filter(|s| s.is_object())
            .cloned()
            .unwrap_or(json!({}));
        let base = int_of(state.get("base")).unwrap_or(0);
        let used = total - base;
        let park = used >= cap;
        let (patch, line, dry_line);
        if park {
            let note = format!(
                "tokens: {} reached the {} job cap on {device}; a resumed session gets another {}",
                commas(used),
                commas(cap),
                commas(cap)
            );
            patch = json!({"stage": "needs-human", "note": note, "claim": null, "token_cap": {"base": total, "warned": null}});
            line = format!("needs-human {job_id} {project}: {note}");
            dry_line = format!("would move {line}");
        } else if used * 100 >= cap * TOKEN_WARN_PERCENT && state.get("warned").is_none() {
            let note = format!(
                "tokens: {} of the {} job cap ({}%) on {device}",
                commas(used),
                commas(cap),
                (used * 100).div_euclid(cap)
            );
            patch = json!({"note": note, "token_cap": {"base": base, "warned": now()}});
            line = format!("token warning {job_id} {project}: {note}");
            dry_line = format!("would warn tokens {job_id} {project}: {note}");
        } else {
            continue;
        }
        if dry {
            outln!(io, "{dry_line}");
            continue;
        }
        let stage = j.get("stage").cloned();
        let done = cas(home, "/api/board", |text| {
            let mut board = load_board(text)?;
            let Some(b) = job_index(&board, job_id) else {
                return Ok((None, false));
            };
            let found = &board["jobs"][b];
            if found.get("stage").cloned() != stage
                || found
                    .get("claim")
                    .filter(|c| c.is_object())
                    .and_then(|c| get_str(c, "device"))
                    != Some(device)
            {
                return Ok((None, false));
            }
            patch_job(&mut board, job_id, &patch);
            let found = &mut board["jobs"][b];
            let mut tokens = found
                .get("tokens")
                .filter(|t| t.is_object())
                .cloned()
                .unwrap_or(json!({}));
            let mut entry = json!({"at": found["updated"]});
            for (k, v) in fresh.as_object().unwrap() {
                entry[k] = v.clone();
            }
            tokens[device] = entry;
            found["tokens"] = tokens;
            Ok((Some(dump_board(&board)), true))
        })?;
        if !done {
            continue;
        }
        outln!(io, "{line}");
        let link = j.get("paseo").filter(|l| l.is_object());
        let server = host_paseo_server();
        if park
            && server.is_some()
            && link.and_then(|l| get_str(l, "server")).map(String::from) == server
        {
            if let Some(agent) = paseo_agent(j) {
                if paseo_run(&["stop", &agent]).0 != 0 {
                    errln!(io, "factory.py: pickup could not stop agent {agent} of {job_id}: paseo stop failed");
                }
            }
        }
    }
    Ok(())
}
