use super::*;

// ---- group C: the board passes

pub(super) type Cache = HashMap<String, Settings>;

pub(super) const ADAPTER_TIMEOUT_SECONDS: u64 = 120;
pub(super) const TAKEOVER_MINUTES: i64 = 30;
pub(super) const SESSION_STOP_CLOSED_HOURS: i64 = 24;
pub(super) const SYNC_STAGES: [&str; 2] = ["pr-open", "needs-human"];
pub(super) const PR_SYNC_LIMIT: usize = 100;
pub(super) const MERGED_NOTE: &str = "merged on GitHub";
pub(super) const GITHUB_CLOSE_REASON: &str = "PR closed on GitHub";
pub(super) const AUTO_BABYSIT_LIMIT: usize = 20;
pub(super) const PRIORITY_NUDGE: &str = "The owner prioritized factory job {id}. If your turn ended while work on it is still owed, continue with the next step now.";
pub(super) const IDLE_NUDGE_MINUTES: i64 = 20;
pub(super) const IDLE_NUDGE: &str = "Factory job {id} shows no commit, file or board change for 20 minutes while this session sits idle. If work on it is still owed, continue with the next step now. If you are waiting on an agent or command that is still running, run factory hold for the job with your --session tag, then keep waiting.";
pub(super) const PERMISSION_NOTE: &str = "permission: agent {agent} has waited at a permission dialog for {minutes} minutes; answer it in Paseo";
pub(super) const TOKEN_WARN_PERCENT: i64 = 80;
pub(super) const AUTONOMY_DAYS: i64 = 90;
pub(super) const AUTONOMY_CLASSES: [&str; 2] = ["internal", "visible"];
pub(super) const AUTONOMY_HOLD_NOTE: &str = "earned autonomy hold: a person reviews and merges";
pub(super) static CLOSE_REASON: Pattern = Pattern::new(r"[^\x00-\x1f\x7f-\x9f  ]{1,200}", Re::NONE);
pub(super) static GH_PR_PARTS: Pattern = Pattern::new(
    r"https://github\.com/([\w.-]+)/([\w.-]+)/pull/(\d{1,9})",
    Re::ASCII,
);
pub(super) static AUTO_BABYSIT: Pattern = Pattern::new("[1-9]", Re::NONE);

pub(super) fn auto_babysit_limit() -> usize {
    #[cfg(test)]
    if let Some(n) = FAKES.with(|f| f.borrow().auto_babysit_limit) {
        return n;
    }
    AUTO_BABYSIT_LIMIT
}

/// This host's Paseo server id, else None, through the seam the tests patch.
pub(super) fn host_paseo_server() -> Option<String> {
    #[cfg(test)]
    if let Some(f) = fake!(paseo_server) {
        return f();
    }
    crate::shared::paseo::paseo_server()
}

/// The end of an archive-by-hand hint for agent: "" when agent is unsafe to print as an argument.
pub(super) fn paseo_archive_tail(agent: &str) -> String {
    if !PASEO_ID.is_full(agent) || agent.starts_with('-') {
        return String::new();
    }
    let home = std::env::var("PASEO_HOME").unwrap_or_default();
    if !home.is_empty() && !home.chars().all(crate::shared::py::is_printable) {
        return format!(" (left out --home because PASEO_HOME has unprintable characters): paseo archive --force {agent}");
    }
    let flag = if home.is_empty() {
        String::new()
    } else {
        format!("--home {} ", shlex_quote(&home))
    };
    format!(": paseo {flag}archive --force {agent}")
}

/// <home>/paseo-claims/<job-id>/<agent>, the marker that this host's Paseo session claimed the job.
pub(super) fn paseo_claim_path(home: &Path, job_id: &str, agent: &str) -> Option<PathBuf> {
    (JOB_ID.is_full(job_id)
        && !job_id.contains("..")
        && PASEO_ID.is_full(agent)
        && !agent.starts_with('-'))
    .then(|| home.join("paseo-claims").join(job_id).join(agent))
}

pub(super) fn paseo_claimed(home: &Path, job_id: &str, agent: &str) -> bool {
    paseo_claim_path(home, job_id, agent).is_some_and(|p| p.is_file())
}

pub(super) fn forget_paseo_claim(home: &Path, job_id: &str, agent: &str) {
    if let Some(path) = paseo_claim_path(home, job_id, agent) {
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(path.parent().unwrap());
    }
}

/// Whether text holds an operator-preferences tag as written or once NFKC and the look-alike fold apply.
pub(super) fn has_envelope_tag(text: &str) -> bool {
    let folded: String = nfkc(text).chars().map(tag_lookalike).collect();
    NOTE_TAG.search(text).is_some() || NOTE_TAG.search(&folded).is_some()
}

/// The reason for the first admission rule text breaks, or None.
pub(super) fn check_untrusted(text: &str) -> Option<String> {
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

pub(super) fn stamp_of(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_str).and_then(parse_stamp)
}

/// None when stalls.json is absent, else (adapter argv resolved, takeover_minutes).
pub(super) fn read_stalls(home: &Path) -> Res<Option<(Vec<String>, i64)>> {
    let path = home.join("stalls.json");
    if !path.exists() {
        return Ok(None);
    }
    let doc = read_text(&path).ok().and_then(|t| loads(&t).ok());
    let parsed = doc.as_ref().and_then(|doc| {
        let map = doc.as_object()?;
        let keys_ok = map.contains_key("version")
            && map.contains_key("adapter")
            && map
                .keys()
                .all(|k| ["version", "adapter", "takeover_minutes"].contains(&k.as_str()));
        if !keys_ok || int_of(map.get("version")) != Some(1) {
            return None;
        }
        let adapter = map["adapter"].as_array()?;
        if !(1..=16).contains(&adapter.len()) {
            return None;
        }
        let adapter = adapter
            .iter()
            .map(|a| {
                a.as_str()
                    .filter(|a| (1..=4096).contains(&a.chars().count()) && !a.contains('\0'))
                    .map(String::from)
            })
            .collect::<Option<Vec<String>>>()?;
        let minutes = match map.get("takeover_minutes") {
            None => TAKEOVER_MINUTES,
            Some(m) => int_of(Some(m)).filter(|m| (1..=1440).contains(m))?,
        };
        Some((adapter, minutes))
    });
    let Some((mut adapter, minutes)) = parsed else {
        return Err(fail(format!("cannot read {}: use {{\"version\": 1, \"adapter\": [\"<program>\", \"<arg>\", ...], \"takeover_minutes\": 30}}", path.display()), 2));
    };
    if !Path::new(&adapter[0]).is_absolute() {
        let Some(program) = which(&adapter[0]) else {
            return Err(fail(
                format!(
                    "stall adapter {} in {} is not on PATH",
                    adapter[0],
                    path.display()
                ),
                2,
            ));
        };
        adapter[0] = program.to_string_lossy().into_owned();
    }
    Ok(Some((adapter, minutes)))
}

/// True when one adapter stall entry holds exactly the documented fields.
pub(super) fn stall_shape(s: &Value) -> bool {
    let Some(map) = s.as_object() else {
        return false;
    };
    map.len() == 4
        && ["session", "reset", "provider", "model"]
            .iter()
            .all(|k| map.contains_key(*k))
        && stamp_of(map.get("reset")).is_some()
        && get_str(s, "session").is_some_and(|v| PASEO_ID.is_full(v))
        && get_str(s, "provider").is_some_and(|v| SLUG.is_full(v))
        && get_str(s, "model").is_some_and(|m| m.is_empty() || ROUTE_MODEL.is_full(m))
}

/// Run the stall adapter with extra arguments.
pub(super) fn adapter_run(adapter: &[String], extra: &[&str]) -> io::Result<Out> {
    let argv: Vec<&str> = adapter
        .iter()
        .map(String::as_str)
        .chain(extra.iter().copied())
        .collect();
    capture(
        &argv,
        None,
        &[],
        Some(Duration::from_secs(ADAPTER_TIMEOUT_SECONDS)),
    )
}

pub(super) const STALLS_USAGE: &str = "usage: factory stalls list | archive <id> [--force]\n";
pub(super) const STALLS_PASEO_SECONDS: u64 = 20;
pub(super) static STALL_LIMIT: Pattern = Pattern::new(
    r"You['’]ve hit your (session|weekly) limit · resets (?:(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) (\d{1,2}), )?(\d{1,2})(?::(\d{2}))?(am|pm) \(([A-Za-z_]+(?:/[A-Za-z0-9_+-]+)*)\)",
    Re::NONE,
);

/// `factory stalls`: the stall adapter pickup's stalls.json names. It reports and archives
/// Paseo sessions stalled on a Claude usage limit. Exit 0 ok, 1 failure, 2 usage, 3 archive
/// refused because the session is no longer stalled.
pub fn stalls_cmd(call: &Call) -> Res<()> {
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    match args[..] {
        ["list"] => stalls_list(),
        ["archive", id] if ADAPTER_ID.is_full(id) => stalls_archive(id),
        ["archive", id, "--force"] if ADAPTER_ID.is_full(id) => stalls_archive_force(id),
        _ => Err(Exit::raw(2, STALLS_USAGE)),
    }
}

pub(super) fn stalls_fail(msg: &str) -> Exit {
    Exit::raw(1, format!("factory stalls: {msg}\n"))
}

/// (exit code, stdout) from one paseo call, or 1 when paseo fails or is slow.
pub(super) fn stalls_paseo(args: &[&str]) -> Res<(i32, String)> {
    let Some(program) = which("paseo") else {
        return Err(stalls_fail("paseo is not on PATH"));
    };
    let program = program.to_string_lossy().into_owned();
    let home = std::env::var("PASEO_HOME").unwrap_or_default();
    let mut argv = vec![program.as_str()];
    if !home.is_empty() {
        argv.extend(["--home", home.as_str()]);
    }
    argv.extend(args);
    Ok(
        match capture(
            &argv,
            None,
            &[],
            Some(Duration::from_secs(STALLS_PASEO_SECONDS)),
        ) {
            Ok(out) => (out.code, out.stdout),
            Err(_) => (1, String::new()),
        },
    )
}

/// (limit line groups, inspect info) while the agent sits idle and unarchived on a Claude limit
/// line, else None.
pub(super) fn stall_of(agent: &str) -> Res<Option<(Vec<Option<String>>, Value)>> {
    let (code, text) = stalls_paseo(&["logs", agent, "--tail", "1", "--filter", "text"])?;
    if code != 0 {
        return Ok(None);
    }
    let Some(caps) = STALL_LIMIT.fullmatch(strip(&text)) else {
        return Ok(None);
    };
    let groups = (1..=7)
        .map(|i| caps.get(i).map(|m| m.as_str().to_string()))
        .collect();
    let (code, raw) = stalls_paseo(&["inspect", agent, "--json"])?;
    if code != 0 {
        return Ok(None);
    }
    let Some(info) = loads(&raw).ok().filter(Value::is_object) else {
        return Ok(None);
    };
    if info.get("Status") != Some(&json!("idle")) || info.get("Archived") != Some(&json!(false)) {
        return Ok(None);
    }
    Ok(Some((groups, info)))
}

/// The reset instant in seconds from the limit line groups and the message time in
/// microseconds, or None when the zone is unknown: that agent is left out.
pub(super) fn stall_reset(groups: &[Option<String>], message_us: i128) -> Option<i64> {
    let zone = groups[6].as_deref()?;
    let at = message_us.div_euclid(1_000_000) as i64;
    let local = at + zone_offset(zone, at)?;
    let (y, m, d) = civil(local.div_euclid(86_400));
    let hour = groups[3].as_deref()?.parse::<i64>().ok()? % 12
        + if groups[5].as_deref() == Some("pm") {
            12
        } else {
            0
        };
    let minute: i64 = groups[4].as_deref().unwrap_or("0").parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    let wall = |y: i64, m: i64, d: i64| -> Option<i128> {
        if d < 1 || d > crate::commands::retro::month_days(y, m) {
            return None;
        }
        let utc = zone_wall_to_utc(
            zone,
            days_from_civil(y, m, d) * 86_400 + hour * 3600 + minute * 60,
        )?;
        Some(i128::from(utc) * 1_000_000)
    };
    let reset = match groups[1].as_deref() {
        None => {
            let reset = wall(y, m, d)?;
            if reset > message_us {
                reset
            } else {
                let (y, m, d) = civil(days_from_civil(y, m, d) + 1);
                wall(y, m, d)?
            }
        }
        Some(month) => {
            let month = MONTHS.iter().position(|n| *n == month)? as i64 + 1;
            let day: i64 = groups[2].as_deref()?.parse().ok()?;
            let reset = wall(y, month, day)?;
            if reset < message_us - 86_400 * 1_000_000 {
                wall(y + 1, month, day)?
            } else {
                reset
            }
        }
    };
    Some(reset.div_euclid(1_000_000) as i64)
}

pub(super) fn stalls_list() -> Res<()> {
    let (code, raw) = stalls_paseo(&["ls", "-g", "--json"])?;
    let agents = (code == 0).then(|| loads(&raw).ok()).flatten();
    let Some(Value::Array(agents)) = agents else {
        return Err(stalls_fail("paseo ls failed"));
    };
    let mut stalls = Vec::new();
    for agent in &agents {
        let Some(id) = get_str(agent, "id") else {
            continue;
        };
        let head = get_str(agent, "provider").map_or("", |p| p.split('/').next().unwrap_or(""));
        if agent.get("status") != Some(&json!("idle"))
            || !(head == "claude" || head.starts_with("claude-"))
            || !ADAPTER_ID.is_full(id)
        {
            continue;
        }
        let Some((groups, info)) = stall_of(id)? else {
            continue;
        };
        let Some(message_us) = crate::commands::retro::when(info.get("UpdatedAt")) else {
            continue;
        };
        let Some(reset) = stall_reset(&groups, message_us) else {
            continue;
        };
        let model = info
            .get("Model")
            .filter(|m| truthy(Some(*m)))
            .cloned()
            .unwrap_or(json!(""));
        stalls.push(json!({"session": id, "reset": format_stamp(reset),
            "provider": info.get("Provider").cloned().unwrap_or(Value::Null), "model": model}));
    }
    let doc = json!({"version": 1, "stalls": stalls});
    let _ = writeln!(io::stdout(), "{}", dumps(&doc, DEFAULT));
    Ok(())
}

/// Archive only a session that is still stalled; a woken one exits 3 and is kept.
pub(super) fn stalls_archive(id: &str) -> Res<()> {
    if stall_of(id)?.is_none() {
        return Err(exit(3));
    }
    let (code, _) = stalls_paseo(&["archive", id])?;
    if code != 0 {
        return Err(stalls_fail(&format!("paseo archive exit {code}")));
    }
    Ok(())
}

/// Archive a session whatever it is doing, unless it is already archived.
pub(super) fn stalls_archive_force(id: &str) -> Res<()> {
    let (code, raw) = stalls_paseo(&["inspect", id, "--json"])?;
    let info = (code == 0).then(|| loads(&raw).ok()).flatten();
    if info.is_some_and(|i| i.get("Archived") == Some(&json!(true))) {
        return Ok(());
    }
    let (code, _) = stalls_paseo(&["archive", "--force", id])?;
    if code != 0 {
        return Err(stalls_fail(&format!("paseo archive exit {code}")));
    }
    Ok(())
}

/// The stalls from one adapter list call, or the reason it gave none.
pub(super) fn list_stalls(adapter: &[String]) -> Result<Vec<Value>, String> {
    let p = match adapter_run(adapter, &["list"]) {
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            return Err("stall adapter timed out".into())
        }
        Err(e) => return Err(os_error(&e, Some(Path::new(&adapter[0])))),
        Ok(p) => p,
    };
    if p.code != 0 {
        return Err(format!("stall adapter exit {}", p.code));
    }
    let doc = loads(&p.stdout).ok();
    let stalls = doc.as_ref().and_then(Value::as_object).and_then(|d| {
        if d.len() != 2 || int_of(d.get("version")) != Some(1) {
            return None;
        }
        let stalls = d.get("stalls")?.as_array()?;
        stalls.iter().all(stall_shape).then(|| stalls.clone())
    });
    stalls.ok_or_else(|| "stall adapter malformed output".into())
}

pub(super) fn job_index(data: &Value, job_id: &str) -> Option<usize> {
    jobs_of(data)
        .iter()
        .position(|j| get_str(j, "id") == Some(job_id))
}

/// Takeover's job rule: an idea, an in-flight job, or an automerging pr-open job not escalated.
pub(super) fn resumable(job: &Value) -> bool {
    let stage = get_str(job, "stage");
    stage == Some(IDEA)
        || stage.is_some_and(|s| IN_FLIGHT.contains(&s))
        || stage == Some("pr-open")
            && job.get("automerge") == Some(&Value::Bool(true))
            && !escalated(job)
}

/// One CAS: replace job_id's claim with (device, tag) and drop its paseo link, after re-checking
/// the takeover job rules. (old claim, old paseo), or None when a rule failed.
pub(super) fn claim_stalled(
    home: &Path,
    job_id: &str,
    session_id: &str,
    device: &str,
    tag: &str,
) -> Res<Option<(Value, Value)>> {
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let empty = json!({});
        let at = job_index(&data, job_id);
        let job = at.map_or(&empty, |i| &data["jobs"][i]);
        let claim = job
            .get("claim")
            .filter(|c| truthy(Some(*c)))
            .cloned()
            .unwrap_or(json!({}));
        let paseo = job.get("paseo").filter(|p| p.is_object()).cloned();
        let ok = paseo
            .as_ref()
            .is_some_and(|p| get_str(p, "agent") == Some(session_id))
            && get_str(&claim, "device") == Some(device)
            && resumable(job)
            && !gated(job)
            && !paused(job)
            && !closing(job);
        let (Some(i), Some(paseo), true) = (at, paseo, ok) else {
            return Ok((None, None));
        };
        data["jobs"][i]
            .as_object_mut()
            .unwrap()
            .shift_remove("claim");
        patch_job(&mut data, job_id, &json!({"claim": new_claim(device, tag)}));
        data["jobs"][i]
            .as_object_mut()
            .unwrap()
            .shift_remove("paseo");
        Ok((Some(dump_board(&data)), Some((claim, paseo))))
    })
}

/// One CAS: put back old_claim and old_paseo while the claim is still (device, tag).
pub(super) fn restore_claim(
    home: &Path,
    job_id: &str,
    device: &str,
    tag: &str,
    old: &(Value, Value),
) -> Res<()> {
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(i) = job_index(&data, job_id) else {
            return Ok((None, ()));
        };
        let claim = data["jobs"][i].get("claim").cloned().unwrap_or(json!({}));
        if !(get_str(&claim, "device") == Some(device) && get_str(&claim, "session") == Some(tag)) {
            return Ok((None, ()));
        }
        data["jobs"][i]["claim"] = old.0.clone();
        data["jobs"][i]["paseo"] = old.1.clone();
        Ok((Some(dump_board(&data)), ()))
    })
}

/// pickup_settings, cached per project, with the stderr line a fresh reason costs outside a dry run.
pub(super) fn cached_settings(
    io: &mut Io,
    home: &Path,
    project: &str,
    repo: &str,
    settings: &mut Cache,
    device: &str,
    dry: bool,
) -> Res<Settings> {
    if !settings.contains_key(project) {
        let found = pickup_settings(home, project, repo, device)?;
        if let (Some(reason), false) = (&found.reason, dry) {
            errln!(io, "pickup skips {project}: {reason}");
        }
        settings.insert(project.to_string(), found);
    }
    Ok(settings[project].clone())
}

/// Why settings keep pickup from acting on a project on this host, or None.
pub(super) fn settings_reason(s: &Settings, device: &str) -> Option<String> {
    if let Some(why) = &s.reason {
        return Some(why.clone());
    }
    if s.lists(device) {
        return None;
    }
    Some(if s.enabled != Some(true) {
        "pickup off".into()
    } else {
        format!("pickup does not list host {device}")
    })
}

/// Take over at most one stalled session whose usage limit is far off: 1 when a takeover
/// launched, else 0.
#[allow(clippy::too_many_arguments)]
pub(super) fn takeover(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<usize> {
    let _ = minutes;
    let Some((adapter, threshold)) = read_stalls(home)? else {
        return Ok(0);
    };
    let mut stalls = match list_stalls(&adapter) {
        Ok(stalls) => stalls,
        Err(reason) => {
            if dry {
                outln!(io, "skip takeover: {reason}");
            } else {
                errln!(io, "pickup skips takeover: {reason}");
            }
            return Ok(0);
        }
    };
    let host_cap = if stalls.is_empty() {
        None
    } else {
        host_max_inflight(home, device)?
    };
    let routes = read_routes(home)?;
    let stamp = now();
    let limited: Vec<String> = read_limits(home)?
        .into_iter()
        .filter(|(_, u)| *u > stamp)
        .map(|(n, _)| n)
        .collect();
    let skipped = |io: &mut Io, session: &str, job_id: &str, line: &str| {
        if dry {
            if job_id.is_empty() {
                outln!(io, "skip stall {session}: {line}");
            } else {
                outln!(io, "skip stall {session} {job_id}: {line}");
            }
        }
    };
    stalls.sort_by(|a, b| get_str(b, "reset").cmp(&get_str(a, "reset")));
    for stall in &stalls {
        let session = get_str(stall, "session").unwrap();
        // Python subtracts a now with microseconds and truncates, so a whole-second reset reads
        // one second short. The half second stands in for the microseconds unix_now drops.
        let now = unix_now() as f64 + 0.5;
        let seconds = (stamp_of(stall.get("reset")).unwrap() as f64 - now).trunc() as i64;
        if seconds.div_euclid(60) <= threshold {
            let line = format!(
                "resets in {} min, within {threshold}; left to the limit-resume sweep",
                seconds.div_euclid(60)
            );
            skipped(io, session, "", &line);
            continue;
        }
        let job = jobs_of(data).iter().find(|j| {
            safe_id(j).is_some()
                && j.get("paseo")
                    .filter(|p| p.is_object())
                    .and_then(|p| get_str(p, "agent"))
                    == Some(session)
        });
        let Some(job) = job.cloned() else {
            skipped(io, session, "", "no job links this session");
            continue;
        };
        let job_id = safe_id(&job).unwrap().to_string();
        let slug = get_str(&job, "project").map(String::from);
        let claim = job.get("claim").filter(|c| truthy(Some(*c))).cloned();
        if claim.as_ref().and_then(|c| get_str(c, "device")) != Some(device) {
            let line = claim.as_ref().map_or("unclaimed".into(), claimed_by);
            skipped(io, session, &job_id, &line);
            continue;
        }
        if !resumable(&job) {
            let line = format!("stage {} is not resumable", text_of(job.get("stage")));
            skipped(io, session, &job_id, &line);
            continue;
        }
        let rule = if gated(&job) {
            Some("waiting on a human")
        } else if paused(&job) {
            Some("paused")
        } else if closing(&job) {
            Some("closing")
        } else if tracer_building(&job) {
            Some("building in Tracer")
        } else {
            None
        };
        if let Some(line) = rule {
            skipped(io, session, &job_id, line);
            continue;
        }
        let Some((slug, repo)) = slug
            .and_then(|s| Some((s.clone(), lookup(paths, &s)?.to_string())))
            .filter(|(_, r)| is_dir(r))
        else {
            skipped(io, session, &job_id, "no checkout on this host");
            continue;
        };
        let s = cached_settings(io, home, &slug, &repo, settings, device, true)?;
        if let Some(line) = settings_reason(&s, device) {
            skipped(io, session, &job_id, &line);
            continue;
        }
        if host_cap == Some(0) {
            skipped(
                io,
                session,
                &job_id,
                &format!("host {device} paused (max in flight 0)"),
            );
            continue;
        }
        let stalled = routes.iter().find(|r| {
            Some(r.provider.as_str()) == get_str(stall, "provider")
                && Some(r.model.as_str()) == get_str(stall, "model")
        });
        let Some(stalled) = stalled else {
            skipped(io, session, &job_id, "runs no route in routes.md");
            continue;
        };
        let fallback = routes
            .iter()
            .find(|r| r.name != stalled.name && !limited.contains(&r.name));
        let Some(fallback) = fallback else {
            skipped(io, session, &job_id, "no fallback route");
            continue;
        };
        let verb = if get_str(&job, "stage") == Some(IDEA) {
            "idea"
        } else {
            "recover"
        };
        let prompt = format!("/factory {verb} {job_id} --session ");
        if dry {
            let entry = Entry {
                id: job_id.clone(),
                project: slug.clone(),
                repo_path: repo.clone(),
                session: "<session>".into(),
            };
            let given = Given::new(
                &entry,
                route_values(fallback),
                launch_prompt(&format!("{prompt}<session>")),
            );
            let template = route_launcher(home, fallback)?;
            outln!(
                io,
                "would take over {job_id} {slug} from {session} on {}: {}",
                fallback.name,
                shlex_join(&launcher_argv(&template, &given))
            );
            return Ok(1);
        }
        let until = limit_route(
            home,
            &stalled.name,
            (-(-seconds).div_euclid(60)).clamp(1, 1440),
        )?
        .0;
        outln!(io, "limited {} until {until}", stalled.name);
        let Some(live) = pick_route(home)? else {
            return Ok(0);
        };
        let template = route_launcher(home, &live)?;
        let tag = token_hex3();
        let Some(old) = claim_stalled(home, &job_id, session, device, &tag)? else {
            return Ok(0);
        };
        let entry = Entry {
            id: job_id.clone(),
            project: slug.clone(),
            repo_path: repo.clone(),
            session: tag.clone(),
        };
        let given = Given::new(
            &entry,
            route_values(&live),
            launch_prompt(&format!("{prompt}{tag}")),
        );
        let (code, detail) = match adapter_run(&adapter, &["archive", session]) {
            Ok(p) => (Some(p.code), None),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => (None, Some("timed out".to_string())),
            Err(e) => (None, Some(os_error(&e, Some(Path::new(&adapter[0]))))),
        };
        if code == Some(3) {
            restore_claim(home, &job_id, device, &tag, &old)?;
            outln!(io, "kept {job_id}: session {session} is no longer stalled");
            return Ok(0);
        }
        if code != Some(0) {
            restore_claim(home, &job_id, device, &tag, &old)?;
            let why = detail.unwrap_or_else(|| format!("exit {}", code.unwrap()));
            return Err(fail(
                format!("takeover of {job_id} undone: archive of {session} failed: {why}"),
                1,
            ));
        }
        let argv = launcher_argv(&template, &given);
        let env = launch_env(&given, &given.provider, &given.model);
        let (ok, why, log_path) = launch_idea(home, &entry, &live.name, &argv, &env)?;
        if ok {
            outln!(
                io,
                "took over {job_id} {slug} from {session} on {} session {tag}",
                live.name
            );
            record_launch(io, home, &job_id, launch_row(&given, &argv, &tag));
            return Ok(1);
        }
        let tail = log_tail(&log_path)?;
        if LIMIT_TEXT.search(&tail).is_some() {
            let until = limit_route(home, &live.name, limit_minutes(&tail, unix_now()))?.0;
            outln!(io, "limited {} until {until}", live.name);
        }
        if get_str(&job, "stage") == Some(IDEA) {
            release_claim(home, &job_id, device, &tag)?;
            outln!(io, "released {job_id}");
        }
        return Err(fail(
            format!(
                "takeover launch of {job_id} on {} failed: {}; see {}",
                live.name,
                why.unwrap_or_default(),
                log_path.display()
            ),
            1,
        ));
    }
    Ok(0)
}

/// True when a closed, denied job carries notes nobody has read yet.
pub(super) fn deny_notes_due(j: &Value) -> bool {
    let Some(backlog) = j.get("backlog").filter(|b| b.is_object()) else {
        return false;
    };
    get_str(j, "stage") == Some("closed")
        && get_str(backlog, "decision") == Some("denied")
        && get_str(backlog, "notes").is_some_and(|n| !strip(n).is_empty())
        && backlog.get("notes_read").is_none()
}

/// Why a due job's notes may not reach a session, or None.
pub(super) fn deny_notes_problem(j: &Value) -> Option<String> {
    let backlog = &j["backlog"];
    let notes = get_str(backlog, "notes").unwrap();
    if notes.chars().count() > 2000 {
        return Some("over 2000 characters".into());
    }
    let Some(line) = get_str(backlog, "line")
        .filter(|l| l.chars().count() <= 2000 && NOTE_INVISIBLE.search(l).is_none())
    else {
        return Some("has a malformed backlog line".into());
    };
    if BACKLOG_NOTES_TAG.search(line).is_some() || BACKLOG_NOTES_TAG.search(notes).is_some() {
        return Some("contains the reserved envelope tag".into());
    }
    if let Some(problem) = check_untrusted(notes) {
        return Some(problem);
    }
    check_untrusted(line).map(|p| format!("has an unsafe backlog line ({p})"))
}

pub(super) fn has_checkout(paths: &[(String, String)], project: Option<&str>) -> bool {
    project.and_then(|p| lookup(paths, p)).is_some_and(is_dir)
}

/// Claim a job for one session in one board write when rule still holds on the fresh board, and
/// return the new tag, else None. Only the claim and the updated stamps change.
pub(super) fn claim_for(
    home: &Path,
    job_id: &str,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    rule: &dyn Fn(&Value) -> bool,
) -> Res<Option<String>> {
    let tag = token_hex3();
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(i) = job_index(&data, job_id) else {
            return Ok((None, None));
        };
        let j = &data["jobs"][i];
        if !rule(j)
            || gated(j)
            || !has_checkout(paths, get_str(j, "project"))
            || claims::blocking(Some(j), device, "", minutes)?.is_some()
        {
            return Ok((None, None));
        }
        let stamp = now();
        data["jobs"][i]["claim"] = new_claim(device, &tag);
        data["jobs"][i]["updated"] = Value::from(stamp.clone());
        data["updated"] = Value::from(stamp);
        Ok((Some(dump_board(&data)), Some(tag.clone())))
    })
}

pub(super) fn claim_deny_notes(
    home: &Path,
    job_id: &str,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
) -> Res<Option<String>> {
    claim_for(home, job_id, device, minutes, paths, &|j| {
        deny_notes_due(j) && deny_notes_problem(j).is_none()
    })
}

/// Drop this session's claim on a job after a failed launch, so the next pickup may try again.
pub(super) fn release_pickup_claim(
    home: &Path,
    job_id: &str,
    device: &str,
    session: &str,
) -> Res<Parked> {
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(i) = job_index(&data, job_id) else {
            return Ok((None, None));
        };
        if !mine(
            data["jobs"][i].get("claim").unwrap_or(&json!({})),
            device,
            session,
        ) {
            return Ok((None, None));
        }
        let stamp = now();
        data["jobs"][i]
            .as_object_mut()
            .unwrap()
            .shift_remove("claim");
        data["jobs"][i]["updated"] = Value::from(stamp.clone());
        data["updated"] = Value::from(stamp);
        Ok((Some(dump_board(&data)), None))
    })
}
