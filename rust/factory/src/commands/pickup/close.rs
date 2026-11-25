use super::*;

/// Python's `ascii(s)`: repr with every non-ASCII character escaped.
pub(super) fn ascii(s: &str) -> String {
    let r = repr(s);
    let mut out = String::new();
    for c in r.chars() {
        match c as u32 {
            0..=0x7f => out.push(c),
            n @ 0x80..=0xff => out.push_str(&format!("\\x{n:02x}")),
            n @ 0x100..=0xffff => out.push_str(&format!("\\u{n:04x}")),
            n => out.push_str(&format!("\\U{n:08x}")),
        }
    }
    out
}

/// (board key, why) when the dashboard closed or paused this job and its session should stop.
pub(super) fn session_stop_reason(j: &Value, cutoff: &str) -> Option<(&'static str, &'static str)> {
    let stage = get_str(j, "stage");
    let terminal = stage.is_some_and(|s| TERMINAL.contains(&s));
    if closing(j) && !terminal {
        return Some(("close", "the job is closing"));
    }
    let closed = j.get("close").and_then(|c| get_str(c, "closed"));
    if stage == Some("closed") && closed.is_some_and(|c| c >= cutoff) {
        return Some(("close", "the job is closed"));
    }
    if paused(j) && !terminal {
        return Some(("paused", "the job is paused"));
    }
    None
}

/// Stop this host's linked Paseo agent of each job the dashboard closed or paused, once per host.
pub(super) fn session_stop_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    dry: bool,
) -> Res<()> {
    let server = host_paseo_server();
    let cutoff = format_stamp(unix_now() - SESSION_STOP_CLOSED_HOURS * 3600);
    let stopped_here = |entry: Option<&Value>| {
        entry
            .and_then(|e| e.get("session_stop"))
            .is_some_and(|s| s.as_object().is_some_and(|s| s.contains_key(device)))
    };
    let due = |j: &Value| {
        let Some((key, _)) = session_stop_reason(j, &cutoff) else {
            return false;
        };
        let link = j.get("paseo").filter(|l| l.is_object());
        let c = j.get("claim");
        server.is_some()
            && link.is_some_and(|l| get_str(l, "server").map(String::from) == server)
            && paseo_agent(j).is_some_and(|a| Some(&a) != own_agent().as_ref())
            && c.is_none_or(|c| c.is_null() || get_str(c, "device") == Some(device))
            && lead_of(j).is_none()
            && !stopped_here(j.get(key))
            && safe_id(j).is_some()
            && get_str(j, "project").is_some_and(|p| SLUG.is_full(p))
    };
    let due_ids: Vec<usize> = (0..jobs_of(data).len())
        .filter(|&i| due(&data["jobs"][i]))
        .collect();
    if due_ids.is_empty() {
        return Ok(());
    }
    let Some(statuses) = paseo_statuses() else {
        errln!(io, "pickup skips session stops: paseo did not answer");
        return Ok(());
    };
    for i in due_ids {
        let j = data["jobs"][i].clone();
        let (job_id, project, agent) = (
            get_str(&j, "id").unwrap(),
            get_str(&j, "project").unwrap(),
            paseo_agent(&j).unwrap(),
        );
        let (key, why) = session_stop_reason(&j, &cutoff).unwrap();
        if matches!(
            status_of(&statuses, &agent).as_deref(),
            None | Some("closed")
        ) {
            continue;
        }
        let line = format!("session {agent} of {job_id} {project}: {why}");
        if dry {
            outln!(io, "would stop {line}");
            continue;
        }
        if paseo_run(&["stop", &agent]).0 != 0 {
            errln!(
                io,
                "pickup could not stop agent {agent} of {job_id}: paseo stop failed"
            );
            continue;
        }
        let at = j[key].get("at").cloned();
        let done = cas(home, "/api/board", |text| {
            let mut board = load_board(text)?;
            let Some(b) = job_index(&board, job_id) else {
                return Ok((None, false));
            };
            let found = &board["jobs"][b];
            let entry = found.get(key).filter(|e| e.is_object());
            if entry.is_none_or(|e| e.get("at").cloned() != at) || stopped_here(entry) {
                return Ok((None, false));
            }
            let mut patch = json!({key: {"session_stop": {device: {"agent": agent, "at": now()}}}});
            if key == "close"
                && found.get("claim").and_then(|c| get_str(c, "device")) == Some(device)
                && found["claim"].is_object()
            {
                patch["claim"] = Value::Null;
            }
            patch_job(&mut board, job_id, &patch);
            Ok((Some(dump_board(&board)), true))
        })?;
        if done {
            if key == "close" {
                data["jobs"][i]
                    .as_object_mut()
                    .unwrap()
                    .shift_remove("claim");
            }
            outln!(io, "stopped {line}");
        }
    }
    Ok(())
}

/// The queued close's reason when it is one line of 1 to 200 characters, else None.
pub(super) fn close_reason(j: &Value) -> Option<String> {
    let r = j
        .get("close")
        .and_then(|c| get_str(c, "reason"))
        .map(strip)
        .unwrap_or("");
    CLOSE_REASON.is_full(r).then(|| r.to_string())
}

/// Hosts the board records as holding the job: its claim and each move's from, sorted.
pub(super) fn recorded_holders(j: &Value) -> Vec<String> {
    let mut found: Vec<&Value> = Vec::new();
    if let Some(c) = j.get("claim").filter(|c| c.is_object()) {
        found.extend(c.get("device"));
    }
    if let Some(m) = j.get("move").filter(|m| m.is_object()) {
        let mut all = earlier_moves(m);
        all.push(m);
        found.extend(all.into_iter().filter_map(|x| x.get("from")));
    }
    let mut hosts: Vec<String> = found
        .into_iter()
        .filter_map(Value::as_str)
        .filter(|h| HOST_NAME.is_full(h))
        .map(String::from)
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

pub(super) fn close_pull_request(
    home: &Path,
    j: &Value,
    device: &str,
) -> Res<(&'static str, String)> {
    #[cfg(test)]
    if let Some(f) = fake!(close_pull_request) {
        return f(home, j, device);
    }
    crate::commands::checks::close_pull_request(home, j, device, None)
}

pub(super) fn close_stacked_prs(
    home: &Path,
    lead_id: &str,
    riders: &[Value],
    device: &str,
) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(close_stacked_prs) {
        return f(home, lead_id, riders, device);
    }
    crate::commands::checks::close_stacked_prs(home, lead_id, riders, device)
}

/// What decision.md's Evidence says of the pull request, per close_pull_request state.
pub(super) const PR_EVIDENCE: [(&str, &str); 4] = [
    (
        "closed",
        "closed by pickup with a comment that links this page",
    ),
    ("merged", "already merged"),
    ("already closed", "already closed"),
    ("open", "still open"),
];

/// decision.md for a job closed from the dashboard or by a GitHub close. outcome is
/// close_pull_request's (state, why), or None without a pr.
pub(super) fn close_decision(
    j: &Value,
    reason: &str,
    device: &str,
    earlier: Option<&str>,
    outcome: Option<(&str, String)>,
    pickup: &Settings,
) -> String {
    let at = text_of(j["close"].get("at"));
    let stage = text_of(j.get("stage"));
    let pr = text_of(j.get("pr"));
    let open_pr = match &outcome {
        Some(("open", why)) => format!("- Pull request {pr} stays open because {why}. Close it on GitHub if it should not merge.\n"),
        _ => String::new(),
    };
    let holders = recorded_holders(j);
    let uncovered: Vec<&String> = holders.iter().filter(|h| !pickup.lists(h)).collect();
    let (project, id) = (text_of(j.get("project")), text_of(j.get("id")));
    let task = format!("`factory cleanup {project}/{id}`");
    let quoted = |hosts: &[&String]| {
        hosts
            .iter()
            .map(|h| format!("`{h}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let manual = if let (false, Some(unread)) = (holders.is_empty(), &pickup.reason) {
        let all: Vec<&String> = holders.iter().collect();
        format!("- The board records {} as holding this job, but {device} could not read the project's pickup hosts: {unread}. Run {task} by hand on each of those hosts that pickup does not cover.\n", quoted(&all))
    } else if !uncovered.is_empty() {
        let one = uncovered.len() == 1;
        let them = if one { "it" } else { "them" };
        let state = if pickup.enabled == Some(false) {
            "is off".to_string()
        } else {
            format!("does not list {them}")
        };
        format!("- {} {task} run by hand: {}. The board records {them} as holding this job, and the project's pickup {state}.\n", if one { "Host that needs" } else { "Hosts that need" }, quoted(&uncovered))
    } else {
        String::new()
    };
    let github = get_str(&j["close"], "via") == Some("github");
    let (summary, requested, unreviewed) = if github {
        (
            format!("GitHub reported pull request {pr} closed without merging, so factory pickup closed the job at {at} while at stage `{stage}`."),
            format!("GitHub reported the pull request closed. Pickup queued the close at {at}."),
            "No agent reviewed this close, and nobody gave a reason. Start a new job with a `rework-of` ref if the work is still wanted.".to_string(),
        )
    } else {
        (
            format!("Closed from the dashboard at {at} while at stage `{stage}`. The owner's reason: {reason}."),
            format!("Close requested from the dashboard at {at}."),
            "No agent reviewed this close. The reason is the owner's own words.".to_string(),
        )
    };
    let round = j.get("round").map_or("0".to_string(), |r| text_of(Some(r)));
    let branch = get_str(j, "branch")
        .filter(|b| !b.is_empty())
        .unwrap_or("none");
    let shown_pr = match outcome {
        Some((state, _)) => {
            let evidence = PR_EVIDENCE.iter().find(|(k, _)| *k == state).unwrap().1;
            format!("{pr}, {evidence}")
        }
        None => "none".into(),
    };
    let text = format!("CLOSED: {reason}\n\n**Summary.** {summary} Nothing else is owed unless Left out says so.\n\n## Evidence\n\n- {requested}\n- Stage when closed: `{stage}`, round {round}.\n- Branch: `{branch}`.\n- Pull request: {shown_pr}.\n- Written by factory pickup on {device}.\n\n## Left out\n\n- {unreviewed}\n{open_pr}- The next pickup run on each host that holds the worktree or branch runs `factory cleanup {project}/{id}` there when the project's pickup is on and lists that host. On any other host, run `factory cleanup {project}/{id}` by hand. It keeps a dirty worktree, and a branch that is not merged unless origin holds the same commit.\n{manual}");
    match earlier {
        Some(e) if !e.is_empty() => format!("{text}\n## Earlier decision\n\n{e}"),
        _ => text,
    }
}

/// Close each job the dashboard queued a close for: decision.md first, then the stage.
pub(super) fn close_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    dry: bool,
) -> Res<()> {
    let mut configs: Cache = HashMap::new();
    for i in 0..ops::board_jobs(data)?.len() {
        let j = data["jobs"][i].clone();
        let terminal = get_str(&j, "stage").is_some_and(|s| TERMINAL.contains(&s));
        let (Some(job_id), Some(project)) = (
            safe_id(&j),
            get_str(&j, "project").filter(|p| SLUG.is_full(p)),
        ) else {
            continue;
        };
        if !closing(&j) || terminal {
            continue;
        }
        let Some(reason) = close_reason(&j) else {
            errln!(io, "pickup skips the close of {job_id}: its reason must be 1 to 200 characters, with no line breaks or control characters");
            continue;
        };
        if let Some(c) = claims::blocking(Some(&j), device, "", minutes)? {
            if dry {
                outln!(io, "skip close {job_id} {project}: {}", claimed_by(&c));
            }
            continue;
        }
        let pr = get_str(&j, "pr").filter(|p| !p.is_empty());
        if dry {
            let mut line = format!("would close {job_id} {project}");
            if let Some(pr) = pr {
                line += &format!(" and pull request {pr}");
            }
            for r in claims::open_stacked(data, job_id) {
                line += &format!(" and stacked pull request {}", text_of(r.get("pr")));
            }
            outln!(io, "{line}");
            continue;
        }
        let outcome = if get_str(&j["close"], "via") == Some("github") {
            Some(("already closed", String::new()))
        } else if pr.is_some() {
            Some(close_pull_request(home, &j, device)?)
        } else {
            None
        };
        match &outcome {
            Some(("closed", _)) => outln!(io, "closed pull request {} for {job_id}", pr.unwrap()),
            Some(("open", why)) => {
                errln!(io, "pickup leaves pull request {} open: {why}", pr.unwrap())
            }
            _ => {}
        }
        if !configs.contains_key(project) {
            configs.insert(project.to_string(), pickup_config(home, project)?);
        }
        let config = configs[project].clone();
        cas(
            home,
            &format!("/api/jobs/{project}/{job_id}/decision.md"),
            |text| {
                if text.is_some_and(|t| t.starts_with("CLOSED:")) {
                    return Ok((None, ()));
                }
                Ok((
                    Some(close_decision(
                        &j,
                        &reason,
                        device,
                        text,
                        outcome.clone(),
                        &config,
                    )),
                    (),
                ))
            },
        )?;
        let at = j["close"].get("at").cloned();
        let (done, stacked) = cas(home, "/api/board", |text| {
            let mut board = load_board(text)?;
            ops::board_jobs(&board)?;
            let Some(b) = job_index(&board, job_id) else {
                return Ok((None, (false, Vec::new())));
            };
            let found = &board["jobs"][b];
            if !closing(found)
                || found["close"].get("at").cloned() != at
                || get_str(found, "stage").is_some_and(|s| TERMINAL.contains(&s))
                || claims::blocking(Some(found), device, "", minutes)?.is_some()
            {
                return Ok((None, (false, Vec::new())));
            }
            board["jobs"][b]
                .as_object_mut()
                .unwrap()
                .shift_remove("claim");
            let stacked = claims::open_stacked(&board, job_id);
            patch_job(
                &mut board,
                job_id,
                &json!({"stage": "closed", "note": reason, "needs": null, "close": {"closed": now()},
                    "decision": {"verdict": "CLOSED", "decider": "person", "at": at}}),
            );
            Ok((Some(dump_board(&board)), (true, stacked)))
        })?;
        if done {
            outln!(io, "closed {job_id} {project}");
            close_stacked_prs(home, job_id, &stacked, device)?;
        }
    }
    Ok(())
}

pub(super) fn cleanup_job(
    repo: &str,
    wt: &Path,
    branch: &str,
    drop_pushed: bool,
    merged_head: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    #[cfg(test)]
    if let Some(f) = fake!(cleanup_job) {
        return f(repo, wt, branch, drop_pushed, merged_head);
    }
    crate::commands::worktree::cleanup_job(repo, wt, branch, drop_pushed, merged_head)
}

/// True when this host already recorded its cleanup of a job under j[key].
pub(super) fn cleaned_here(j: &Value, device: &str, key: &str) -> bool {
    j[key]
        .get("cleanup")
        .and_then(Value::as_object)
        .is_some_and(|d| d.contains_key(device))
}

/// Run cleanup once per host on each dashboard-closed job, and each job pickup synced as merged,
/// whose worktree or branch this host still holds, and record what it left.
#[allow(clippy::too_many_arguments)]
pub(super) fn close_cleanup_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<()> {
    for j in ops::board_jobs(data)?.to_vec() {
        let stage = get_str(&j, "stage");
        let (key, field) = if stage == Some("merged") {
            ("github", "merged")
        } else {
            ("close", "closed")
        };
        let c = j.get(key).filter(|c| c.is_object());
        let (Some(job_id), Some(project), Some(at)) = (
            safe_id(&j),
            get_str(&j, "project").filter(|p| SLUG.is_full(p)),
            c.and_then(|c| get_str(c, field)),
        ) else {
            continue;
        };
        if !stage.is_some_and(|s| TERMINAL.contains(&s)) {
            continue;
        }
        if cleaned_here(&j, device, key) || !has_checkout(paths, Some(project)) {
            continue;
        }
        let repo = lookup(paths, project).unwrap();
        let wt = home.join("worktrees").join(project).join(job_id);
        let branch = format!("factory/{job_id}");
        if !wt.exists()
            && git(
                Path::new(repo),
                &[
                    "rev-parse",
                    "--verify",
                    "-q",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .is_empty()
        {
            continue;
        }
        if let Some(reason) = pickup_reason(io, home, project, paths, settings, device, dry)? {
            if dry {
                outln!(io, "skip cleanup {job_id} {project}: {reason}");
            }
            continue;
        }
        if dry {
            outln!(io, "would clean up {job_id} {project}");
            continue;
        }
        let merged_head = c
            .and_then(|c| get_str(c, "head"))
            .filter(|h| key == "github" && COMMIT.is_full(h));
        let (_, left) = cleanup_job(repo, &wt, &branch, true, merged_head);
        let done = cas(home, "/api/board", |text| {
            let mut board = load_board(text)?;
            ops::board_jobs(&board)?;
            let Some(b) = job_index(&board, job_id) else {
                return Ok((None, false));
            };
            let found = &board["jobs"][b];
            if get_str(found, "stage") != stage
                || !found.get(key).is_some_and(Value::is_object)
                || get_str(&found[key], field) != Some(at)
                || cleaned_here(found, device, key)
            {
                return Ok((None, false));
            }
            let mut patch = Map::new();
            patch.insert(
                key.into(),
                json!({"cleanup": {device: {"at": now(), "left": left}}}),
            );
            patch.insert("port".into(), Value::Null);
            patch_job(&mut board, job_id, &Value::Object(patch));
            Ok((Some(dump_board(&board)), true))
        })?;
        if done {
            if left.is_empty() {
                outln!(io, "cleaned up {job_id} {project}");
            } else {
                outln!(
                    io,
                    "cleaned up {job_id} {project}, left: {}",
                    left.join("; ")
                );
            }
        }
    }
    Ok(())
}
