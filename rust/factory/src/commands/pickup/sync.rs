use super::*;

/// The PR URL parts of a job pickup may sync: pr-open or needs-human, not closing, not a rider.
pub(super) fn sync_match(j: &Value) -> Option<(String, String)> {
    let stage = get_str(j, "stage")?;
    if !SYNC_STAGES.contains(&stage)
        || closing(j)
        || lead_of(j).is_some()
        || safe_id(j).is_none()
        || !get_str(j, "project").is_some_and(|p| SLUG.is_full(p))
    {
        return None;
    }
    let m = GH_PR_PARTS.fullmatch(get_str(j, "pr")?)?;
    Some((
        format!("{}/{}", &m[1], &m[2]).to_lowercase(),
        m[3].to_string(),
    ))
}

/// The project file's repo, cached per project.
pub(super) fn project_repo(
    home: &Path,
    project: &str,
    repos: &mut HashMap<String, String>,
) -> Res<String> {
    if !repos.contains_key(project) {
        let md = project_files(home, project)?.0;
        let repo = lookup(&meta(&md)?, "repo").unwrap_or("").to_string();
        repos.insert(project.to_string(), repo);
    }
    Ok(repos[project].to_lowercase())
}

pub(super) fn pr_states(
    repo: &str,
    numbers: &[u64],
    device: &str,
) -> Result<BTreeMap<u64, (Value, Value)>, String> {
    #[cfg(test)]
    if let Some(f) = fake!(pr_states) {
        return f(repo, numbers, device);
    }
    crate::commands::checks::pr_states(repo, numbers, device)
}

/// Move each pr-open or needs-human job whose pull request GitHub reports merged to merged, and
/// queue a close for each whose pull request GitHub reports closed. One gh api graphql call per
/// repo, and one board write for the whole pass.
#[allow(clippy::too_many_arguments)]
pub(super) fn pr_sync_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<()> {
    let mut repos = HashMap::new();
    let mut groups: Vec<(String, Vec<(Value, u64)>)> = Vec::new();
    for j in ops::board_jobs(data)?.to_vec() {
        let Some((repo, n)) = sync_match(&j) else {
            continue;
        };
        let (job_id, project) = (get_str(&j, "id").unwrap(), get_str(&j, "project").unwrap());
        if let Some(c) = claims::blocking(Some(&j), device, "", minutes)? {
            if dry {
                outln!(io, "skip sync {job_id} {project}: {}", claimed_by(&c));
            }
            continue;
        }
        if project_repo(home, project, &mut repos)? != repo {
            continue;
        }
        if let Some(reason) = pickup_reason(io, home, project, paths, settings, device, dry)? {
            if dry {
                outln!(io, "skip sync {job_id} {project}: {reason}");
            }
            continue;
        }
        let entry = (j.clone(), n.parse().unwrap());
        match groups.iter_mut().find(|(r, _)| *r == repo) {
            Some(slot) => slot.1.push(entry),
            None => groups.push((repo, vec![entry])),
        }
    }
    let mut pending: Vec<(Value, Value, String)> = Vec::new();
    for (repo, mut jobs) in groups {
        if jobs.len() > PR_SYNC_LIMIT {
            errln!(
                io,
                "factory.py: pickup syncs the first {PR_SYNC_LIMIT} of {} pull requests in {repo}",
                jobs.len()
            );
            jobs.truncate(PR_SYNC_LIMIT);
        }
        let numbers: Vec<u64> = jobs.iter().map(|(_, n)| *n).collect();
        let states = match pr_states(&repo, &numbers, device) {
            Ok(states) => states,
            Err(why) => {
                errln!(io, "factory.py: pickup skips the PR sync for {repo}: {why}");
                continue;
            }
        };
        for (j, n) in jobs {
            let Some((state, head)) = states.get(&n) else {
                continue;
            };
            let merged = match state.as_str() {
                Some("MERGED") => true,
                Some("CLOSED") => false,
                _ => continue,
            };
            let line = format!(
                "{} {}: {}",
                text_of(j.get("id")),
                text_of(j.get("project")),
                if merged {
                    MERGED_NOTE
                } else {
                    GITHUB_CLOSE_REASON
                }
            );
            if dry {
                outln!(io, "would sync {line}");
                continue;
            }
            let patch = if merged {
                let mut github = json!({"merged": now()});
                if let Some(head) = head.as_str().filter(|h| COMMIT.is_full(h)) {
                    github["head"] = head.into();
                }
                json!({"stage": "merged", "needs": null, "automerge": null, "babysit": null,
                       "claim": null, "note": MERGED_NOTE, "github": github})
            } else {
                json!({"close": {"reason": GITHUB_CLOSE_REASON, "at": now(), "via": "github"},
                       "babysit": null})
            };
            pending.push((j, patch, line));
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    let rows = cas(home, "/api/board", |text| {
        let mut board = load_board(text)?;
        ops::board_jobs(&board)?;
        let mut rows = Vec::new();
        for (j, patch, line) in &pending {
            let job_id = get_str(j, "id").unwrap();
            let Some(found) = job_index(&board, job_id).map(|b| &board["jobs"][b]) else {
                continue;
            };
            if sync_match(found).is_none()
                || found["pr"] != j["pr"]
                || found["project"] != j["project"]
                || claims::blocking(Some(found), device, "", minutes)?.is_some()
            {
                continue;
            }
            let row = patch_job(&mut board, job_id, patch);
            rows.push((j.clone(), row, line.clone()));
        }
        Ok(((!rows.is_empty()).then(|| dump_board(&board)), rows))
    })?;
    for (j, row, line) in rows {
        let jobs = data["jobs"].as_array_mut().unwrap();
        if let Some(slot) = jobs.iter_mut().find(|x| **x == j) {
            *slot = row;
        }
        outln!(io, "synced {line}");
    }
    Ok(())
}

/// The project file's auto_babysit as an int from 1 to 9, else 0.
pub(super) fn auto_babysit_cap(home: &Path, slug: &str) -> Res<i64> {
    let md = project_files(home, slug)?.0;
    let value = lookup(&meta(&md)?, "auto_babysit")
        .unwrap_or("")
        .to_string();
    Ok(if AUTO_BABYSIT.is_full(&value) {
        value.parse().unwrap()
    } else {
        0
    })
}

/// True when the project's effective pipeline has a Babysit stage note.
pub(super) fn babysit_noted(home: &Path, slug: &str) -> Res<bool> {
    let notes = view_field(home, slug, "stage_notes")?;
    Ok(notes
        .and_then(|n| n.get("babysit").cloned())
        .and_then(|b| b.as_array().map(|b| !b.is_empty()))
        .unwrap_or(false))
}

/// The PR URL parts of a pr-open job pickup may babysit on its own, else None.
pub(super) fn auto_babysit_match(j: &Value) -> Option<(String, String)> {
    if get_str(j, "stage") != Some("pr-open")
        || safe_id(j).is_none()
        || !get_str(j, "project").is_some_and(|p| SLUG.is_full(p))
        || babysitting(j)
        || gated(j)
        || paused(j)
        || closing(j)
        || escalated(j)
        || lead_of(j).is_some()
    {
        return None;
    }
    let m = GH_PR_PARTS.fullmatch(get_str(j, "pr")?)?;
    Some((
        format!("{}/{}", &m[1], &m[2]).to_lowercase(),
        m[3].to_string(),
    ))
}

/// (count, head, reason) of the job's last automatic babysit, (0, null, null) before the first.
pub(super) fn auto_babysit_record(j: &Value) -> (i64, Value, Value) {
    let Some(a) = j.get("auto_babysit").filter(|a| a.is_object()) else {
        return (0, Value::Null, Value::Null);
    };
    let field = |k: &str| a.get(k).cloned().unwrap_or(Value::Null);
    (
        int_of(a.get("count")).unwrap_or(0),
        field("head"),
        field("reason"),
    )
}

/// When the job was last auto-babysat, or "" before the first, so it sorts first.
pub(super) fn auto_babysit_at(j: &Value) -> &str {
    j.get("auto_babysit")
        .and_then(|a| get_str(a, "at"))
        .unwrap_or("")
}

pub(super) fn auto_babysit_copy(reason: &str) -> &'static str {
    match reason {
        "fix-reviews" => "has an open review thread",
        "rebase" => "has a merge conflict",
        _ => "has a failing required check",
    }
}

pub(super) type Signals = BTreeMap<u64, (Value, Option<&'static str>)>;

pub(super) fn pr_signals(
    repo: &str,
    numbers: &[u64],
    device: &str,
    handled: &BTreeMap<u64, Map<String, Value>>,
) -> Result<Signals, String> {
    #[cfg(test)]
    if let Some(f) = fake!(pr_signals) {
        return f(repo, numbers, device, handled);
    }
    crate::commands::checks::pr_signals(repo, numbers, device, handled)
}

/// Set babysit on each pr-open job whose PR fails a required check, has an open counted review
/// thread or a merge conflict, in projects whose project file sets auto_babysit. At most
/// auto_babysit times per job, and never twice for the same head and reason. One gh api graphql
/// call per repo, and one board write for the whole pass.
#[allow(clippy::too_many_arguments)]
pub(super) fn auto_babysit_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(auto_babysit_pass) {
        return f(io, home);
    }
    let (mut caps, mut repos, mut noted) = (HashMap::new(), HashMap::new(), HashMap::new());
    let mut groups: Vec<(String, Vec<(Value, u64, i64)>)> = Vec::new();
    for j in ops::board_jobs(data)?.to_vec() {
        let Some((repo, n)) = auto_babysit_match(&j) else {
            continue;
        };
        let (job_id, project) = (get_str(&j, "id").unwrap(), get_str(&j, "project").unwrap());
        if !caps.contains_key(project) {
            caps.insert(project.to_string(), auto_babysit_cap(home, project)?);
        }
        let cap = caps[project];
        if cap == 0 {
            continue;
        }
        let head = format!("skip auto-babysit {job_id} {project}");
        if let Some(c) = claims::blocking(Some(&j), device, "", minutes)? {
            if dry {
                outln!(io, "{head}: {}", claimed_by(&c));
            }
            continue;
        }
        if project_repo(home, project, &mut repos)? != repo {
            continue;
        }
        if let Some(reason) = pickup_reason(io, home, project, paths, settings, device, dry)? {
            if dry {
                outln!(io, "{head}: {reason}");
            }
            continue;
        }
        if !noted.contains_key(project) {
            noted.insert(project.to_string(), babysit_noted(home, project)?);
        }
        if !noted[project] {
            if dry {
                outln!(io, "{head}: no Babysit note");
            }
            continue;
        }
        if auto_babysit_record(&j).0 >= cap {
            if dry {
                outln!(io, "{head}: {cap} of {cap} automatic babysits used");
            }
            continue;
        }
        let entry = (j.clone(), n.parse().unwrap(), cap);
        match groups.iter_mut().find(|(r, _)| *r == repo) {
            Some(slot) => slot.1.push(entry),
            None => groups.push((repo, vec![entry])),
        }
    }
    let limit = auto_babysit_limit();
    let mut pending = Vec::new();
    for (repo, mut jobs) in groups {
        jobs.sort_by(|a, b| auto_babysit_at(&a.0).cmp(auto_babysit_at(&b.0)));
        if jobs.len() > limit {
            errln!(io, "factory.py: pickup checks {limit} of {} auto-babysit candidates for {repo} this run, oldest automatic babysit first", jobs.len());
        }
        jobs.truncate(limit);
        let numbers: Vec<u64> = jobs.iter().map(|(_, n, _)| *n).collect();
        let handled: BTreeMap<u64, Map<String, Value>> = jobs
            .iter()
            .map(|(j, n, _)| (*n, checks::valid_handled(j.get("handled"))))
            .collect();
        let signals = match pr_signals(&repo, &numbers, device, &handled) {
            Ok(signals) => signals,
            Err(why) => {
                errln!(
                    io,
                    "factory.py: pickup skips auto-babysit for {repo}: {why}"
                );
                continue;
            }
        };
        for (j, n, cap) in jobs {
            let Some((head, Some(reason))) = signals.get(&n).cloned() else {
                continue;
            };
            let (job_id, project) = (text_of(j.get("id")), text_of(j.get("project")));
            let (count, last_head, last_reason) = auto_babysit_record(&j);
            if head == last_head && last_reason == reason {
                if dry {
                    let Some(head) = head.as_str() else {
                        return Err(fail(
                            format!("auto-babysit {job_id}: PR head is not a string"),
                            1,
                        ));
                    };
                    let short: String = head.chars().take(7).collect();
                    outln!(
                        io,
                        "skip auto-babysit {job_id} {project}: already babysat {reason} at {short}"
                    );
                }
                continue;
            }
            let copy = auto_babysit_copy(reason);
            if dry {
                outln!(
                    io,
                    "would auto-babysit {job_id} {project}: PR #{n} {copy} ({} of {cap})",
                    count + 1
                );
                continue;
            }
            pending.push((j, n, cap, head, reason, copy));
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    let rows = cas(home, "/api/board", |text| {
        let mut board = load_board(text)?;
        ops::board_jobs(&board)?;
        let mut rows = Vec::new();
        for (j, n, cap, head, reason, copy) in &pending {
            let job_id = get_str(j, "id").unwrap();
            let Some(found) = job_index(&board, job_id).map(|b| &board["jobs"][b]) else {
                continue;
            };
            if auto_babysit_match(found).is_none()
                || found["pr"] != j["pr"]
                || found["project"] != j["project"]
                || claims::blocking(Some(found), device, "", minutes)?.is_some()
            {
                continue;
            }
            let (count, last_head, last_reason) = auto_babysit_record(found);
            if count >= *cap || (*head == last_head && last_reason == *reason) {
                continue;
            }
            let stamp = now();
            let row = patch_job(
                &mut board,
                job_id,
                &json!({
                    "babysit": {"at": stamp, "via": "pickup", "by": device, "reason": reason},
                    "auto_babysit": {"count": count + 1, "head": head, "reason": reason, "at": stamp},
                    "note": format!("auto-babysitting PR #{n}: it {copy}")}),
            );
            rows.push((j.clone(), *n, *cap, *copy, row));
        }
        Ok(((!rows.is_empty()).then(|| dump_board(&board)), rows))
    })?;
    for (j, n, cap, copy, row) in rows {
        let jobs = data["jobs"].as_array_mut().unwrap();
        if let Some(slot) = jobs.iter_mut().find(|x| **x == j) {
            *slot = row.clone();
        }
        outln!(
            io,
            "auto-babysat {} {}: PR #{n} {copy} ({} of {cap})",
            text_of(j.get("id")),
            text_of(j.get("project")),
            text_of(row["auto_babysit"].get("count"))
        );
    }
    Ok(())
}

/// internal when review-plan skipped ux and ui in the first round, visible otherwise, None with no rounds.
pub(super) fn job_class(job: &Value) -> Option<&'static str> {
    let first = job.get("rounds")?.as_array()?.first()?.as_object()?;
    let skipped = first.get("ux") == first.get("ui")
        && first.get("ui").and_then(Value::as_str) == Some("SKIPPED");
    Some(if skipped { "internal" } else { "visible" })
}

/// The board's well-formed {"<slug>/<class>": "<UTC time>"} restores.
pub(super) fn autonomy_restored(data: &Value) -> Map<String, Value> {
    let restored = data
        .get("autonomy")
        .filter(|r| r.is_object())
        .and_then(|r| r.get("restored"))
        .and_then(Value::as_object);
    restored
        .into_iter()
        .flatten()
        .filter(|(k, v)| {
            k.matches('/').count() == 1
                && k.split_once('/').is_some_and(|(slug, cls)| {
                    SLUG.is_full(slug) && AUTONOMY_CLASSES.contains(&cls)
                })
                && stamp_of(Some(v)).is_some()
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// {(project, class): sorted reverted job ids} for each class with a revert after its last restore.
pub(super) fn autonomy_holds(data: &Value) -> Vec<((String, &'static str), Vec<String>)> {
    let cutoff = unix_now() - AUTONOMY_DAYS * 86_400;
    let restored = autonomy_restored(data);
    let mut holds: Vec<((String, &'static str), Vec<String>)> = Vec::new();
    for j in jobs_of(data) {
        let (Some(project), Some(cls)) = (
            get_str(j, "project").filter(|p| !p.is_empty()),
            job_class(j),
        ) else {
            continue;
        };
        let outcome = j.get("outcome").filter(|o| o.is_object());
        let recent = stamp_of(j.get("done")).is_none_or(|d| d > cutoff);
        if get_str(j, "stage") != Some("merged")
            || !recent
            || outcome.and_then(|o| get_str(o, "result")) != Some("reverted")
        {
            continue;
        }
        let lifted = stamp_of(restored.get(&format!("{project}/{cls}")));
        let at = stamp_of(outcome.unwrap().get("at"));
        if lifted.is_some_and(|l| !at.is_some_and(|a| a > l)) {
            continue;
        }
        let key = (project.to_string(), cls);
        let id = text_of(j.get("id"));
        match holds.iter_mut().find(|(k, _)| *k == key) {
            Some((_, ids)) => ids.push(id),
            None => holds.push((key, vec![id])),
        }
    }
    for (_, ids) in &mut holds {
        ids.sort();
    }
    holds
}

/// Why earned autonomy holds the job's project and class from an agent merge, or None.
pub(super) fn autonomy_hold(data: &Value, job: &Value) -> Option<String> {
    let (project, cls) = (get_str(job, "project")?, job_class(job)?);
    let holds = autonomy_holds(data);
    let (_, held) = holds.iter().find(|((p, c), _)| p == project && *c == cls)?;
    Some(format!("earned autonomy holds {project} {cls} PRs after reverted {}; a person lifts it with factory.py autonomy restore {project} {cls}", held.join(", ")))
}

/// Set automerge false and needs on a held pr-open job. The needs line, or None with no write.
pub(super) fn autonomy_hold_write(home: &Path, job_id: &str) -> Res<Option<String>> {
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = jobs_of(&data)
            .iter()
            .find(|j| get_str(j, "id") == Some(job_id))
            .cloned()
        else {
            return Ok((None, None));
        };
        let held = get_str(&job, "stage") == Some("pr-open")
            && job.get("automerge") == Some(&Value::Bool(true))
            && !escalated(&job);
        let why = if held {
            autonomy_hold(&data, &job)
        } else {
            None
        };
        let number = get_str(&job, "pr")
            .and_then(|p| GH_PR_PARTS.fullmatch(p))
            .map(|m| m[3].to_string());
        let (Some(why), Some(number)) = (why, number) else {
            return Ok((None, None));
        };
        let needs = format!("review and merge PR #{number}: {why}");
        patch_job(
            &mut data,
            job_id,
            &json!({"automerge": false, "needs": needs, "note": AUTONOMY_HOLD_NOTE}),
        );
        Ok((Some(dump_board(&data)), Some(needs)))
    })
}

/// Hold each automerging pr-open job whose project and class earned autonomy holds after a revert.
pub(super) fn autonomy_hold_pass(io: &mut Io, home: &Path, data: &mut Value, dry: bool) -> Res<()> {
    let holds = autonomy_holds(data);
    if holds.is_empty() {
        return Ok(());
    }
    for i in 0..jobs_of(data).len() {
        let j = &data["jobs"][i];
        let number = get_str(j, "pr")
            .and_then(|p| GH_PR_PARTS.fullmatch(p))
            .map(|m| m[3].to_string());
        let key = (get_str(j, "project").unwrap_or_default(), job_class(j));
        let held = holds
            .iter()
            .any(|((p, c), _)| key.0 == p && key.1 == Some(*c));
        let (Some(number), Some(job_id), true) = (
            number,
            get_str(j, "id").map(String::from),
            get_str(j, "stage") == Some("pr-open")
                && j.get("automerge") == Some(&Value::Bool(true))
                && !escalated(j)
                && held,
        ) else {
            continue;
        };
        let line = format!(
            "{job_id} {} PR #{number} for a person",
            text_of(j.get("project"))
        );
        if dry {
            outln!(io, "would hold {line}");
            continue;
        }
        if let Some(needs) = autonomy_hold_write(home, &job_id)? {
            let j = &mut data["jobs"][i];
            j["automerge"] = false.into();
            j["needs"] = needs.into();
            j["note"] = AUTONOMY_HOLD_NOTE.into();
            outln!(io, "held {line}");
        }
    }
    Ok(())
}
