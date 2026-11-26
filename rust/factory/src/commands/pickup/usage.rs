use super::*;
use crate::commands::catch_up::factory_checkout;

/// A message or tool_result content as text: a string, or its joined text blocks.
pub(super) fn retro_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| get_str(b, "text"))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Local time as (utc, offset) with its weekday (Monday 0), hour, minute and date.
pub(super) struct Local {
    pub(super) utc: i64,
    pub(super) offset: i64,
}

impl Local {
    pub(super) fn wall(&self) -> i64 {
        self.utc + self.offset
    }
    pub(super) fn weekday(&self) -> i64 {
        (self.wall().div_euclid(86_400) + 3).rem_euclid(7)
    }
    pub(super) fn hour_minute(&self) -> (i64, i64) {
        let t = self.wall().rem_euclid(86_400);
        (t / 3600, t / 60 % 60)
    }
    pub(super) fn date(&self) -> String {
        local_date(self.utc, self.offset)
    }
}

/// Whether this retro host missed today's weekday retro.
pub(super) fn retro_catchup_due(home: &Path, local: &Local) -> bool {
    #[cfg(test)]
    if let Some(f) = fake!(retro_catchup_due) {
        return f(home, local);
    }
    let prompt = home.join(RETRO_PROMPT_FILE);
    if is_link(&prompt).unwrap_or(false) || !prompt.is_file() {
        return false;
    }
    if local.weekday() > 4 || local.hour_minute() < RETRO_CATCHUP_AFTER {
        return false;
    }
    let today = local.date();
    let record = read_json(&home.join(RETRO_CATCHUP_FILE));
    if record.as_ref().and_then(Value::as_object).is_some_and(|r| {
        r.len() == 2
            && eq_int(r.get("version"), 1)
            && r.get("date").and_then(Value::as_str) == Some(today.as_str())
    }) {
        return false;
    }
    let Ok(entries) = fs::read_dir(home.join("retro")) else {
        return true;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(m) = RETRO_RUN_NAME.fullmatch(&name) else {
            continue;
        };
        if !entry.path().is_dir() {
            continue;
        }
        let s = m.get(1).unwrap().as_str();
        let stamp = format!(
            "{}-{}-{}T{}:{}:{}Z",
            &s[..4],
            &s[4..6],
            &s[6..8],
            &s[9..11],
            &s[11..13],
            &s[13..15]
        );
        let Some(at) = parse_stamp(&stamp) else {
            continue;
        };
        if local_date(at, local.offset) == today {
            return false;
        }
    }
    true
}

/// Print why the retro catch-up waits, once per local date and reason. A dry run prints every
/// time and records nothing.
pub(super) fn retro_catchup_wait(
    io: &mut Io,
    home: &Path,
    local: &Local,
    reason: &str,
    dry: bool,
) -> Res<()> {
    let record = json!({"version": 1, "date": local.date(), "reason": reason});
    let path = home.join(RETRO_WAIT_FILE);
    if !dry && read_json(&path).as_ref() == Some(&record) {
        return Ok(());
    }
    errln!(io, "retro catch-up waits: {reason}");
    if !dry {
        write_file(&path, &(dumps(&record, DEFAULT) + "\n"))?;
    }
    Ok(())
}

/// Launch the stored retro schedule prompt once on a weekday whose 09:00 slot left no run.
pub(super) fn retro_pickup_catchup(io: &mut Io, home: &Path, dry: bool) {
    let mut catchup = || -> Res<()> {
        let (utc, offset) = local_now();
        let local = Local { utc, offset };
        if !retro_catchup_due(home, &local) {
            return Ok(());
        }
        let job_id = format!("retro-{}", local.date().replace('-', ""));
        let Some(route) = pick_route(home)? else {
            return retro_catchup_wait(io, home, &local, "no available route", dry);
        };
        // The retro runs in the factory repo, never in whichever checkout paths.md lists first.
        let Some((slug, path)) = paths_of(home)?
            .into_iter()
            .find(|(_, p)| factory_checkout(Path::new(p)))
        else {
            return retro_catchup_wait(io, home, &local, "no factory checkout in paths.md", dry);
        };
        let template = route_launcher(home, &route)?;
        if dry {
            outln!(io, "would launch retro catch-up {job_id} on {}", route.name);
            return Ok(());
        }
        let prompt_path = home.join(RETRO_PROMPT_FILE);
        let prompt =
            read_text(&prompt_path).map_err(|e| fail(os_error(&e, Some(&prompt_path)), 1))?;
        let entry = Entry {
            id: job_id.clone(),
            project: slug,
            repo_path: path,
            session: token_hex3(),
        };
        let given = Given::new(&entry, route_values(&route), prompt);
        write_file(
            &home.join(RETRO_CATCHUP_FILE),
            &(dumps(&json!({"version": 1, "date": local.date()}), DEFAULT) + "\n"),
        )?;
        let argv = launcher_argv(&template, &given);
        let env = launch_env(&given, &route.provider, &route.model);
        let (ok, detail, log_path) = launch_idea(home, &entry, &route.name, &argv, &env)?;
        if ok {
            outln!(
                io,
                "launched retro catch-up {job_id} on {} session {}",
                route.name,
                entry.session
            );
        } else {
            errln!(
                io,
                "retro catch-up {job_id} failed to launch ({}); see {}; no retry today",
                detail.unwrap_or_default(),
                log_path.display()
            );
        }
        Ok(())
    };
    if let Err(e) = catchup() {
        let _ = io.err.write_all(e.stderr.as_bytes());
        errln!(
            io,
            "retro catch-up stopped (exit {}); the next pickup run retries it",
            e.code
        );
    }
}

pub(super) fn retro_transcripts() -> PathBuf {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::home_dir().unwrap_or_default().join(".claude"));
    base.join("projects")
}

/// Entries of dir, sorted, or none when it cannot be read.
pub(super) fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = fs::read_dir(dir)
        .map(|e| e.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    items.sort();
    items
}

pub(super) fn jsonl(dir: &Path) -> Vec<PathBuf> {
    sorted_entries(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".jsonl"))
        })
        .collect()
}

/// A transcript and its subagent transcripts, each with whether it is a main session.
pub(super) fn with_subagents(project: &Path, path: &Path) -> Vec<(PathBuf, bool)> {
    let stem = path.file_stem().unwrap_or_default();
    let mut found = vec![(path.to_path_buf(), true)];
    found.extend(
        jsonl(&project.join(stem).join("subagents"))
            .into_iter()
            .map(|s| (s, false)),
    );
    found
}

/// The sorted transcript folders this host keeps from the job's worktrees.
pub(super) fn audit_folders(job_id: &str) -> Vec<PathBuf> {
    let needle = format!("factory-{job_id}");
    sorted_entries(&retro_transcripts())
        .into_iter()
        .filter(|p| {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            p.is_dir()
                && name.match_indices(&needle).any(|(at, _)| {
                    let before = name[..at].chars().last();
                    let after = name[at + needle.len()..].chars().next();
                    before.is_none_or(|c| c == '-') && after.is_none_or(|c| c == '-')
                })
        })
        .collect()
}

/// The lines of a transcript, read as text with replacement, without their endings.
pub(super) fn transcript_lines(path: &Path) -> io::Result<Vec<String>> {
    let text = String::from_utf8_lossy(&fs::read(path)?).into_owned();
    let text = crate::shared::py::universal_newlines(text);
    Ok(text.split_inclusive('\n').map(String::from).collect())
}

/// The text of a transcript's first non-meta user line within USAGE_PROMPT_LINES, else "".
pub(super) fn usage_prompt(path: &Path) -> String {
    let Ok(lines) = transcript_lines(path) else {
        return String::new();
    };
    for line in lines.iter().take(USAGE_PROMPT_LINES) {
        let Ok(e) = loads(line) else { continue };
        if get_str(&e, "type") == Some("user") && !truthy(e.get("isMeta")) {
            let msg = e.get("message").filter(|m| m.is_object());
            return msg.map_or(String::new(), |m| retro_text(m.get("content")));
        }
    }
    String::new()
}

/// ({model: {kind: tokens}}, unreadable line count) for one transcript, each message.id counted once.
pub(super) fn usage_scan(
    path: &Path,
    main: bool,
    since: Option<&str>,
) -> io::Result<(Map<String, Value>, usize)> {
    let mut turns: Vec<(String, String, [i64; 4])> = Vec::new();
    let mut bad = 0;
    for line in transcript_lines(path)? {
        if strip(&line).is_empty() {
            continue;
        }
        let Ok(e) = loads(&line) else {
            bad += 1;
            continue;
        };
        if !e.is_object()
            || get_str(&e, "type") != Some("assistant")
            || main && truthy(e.get("isSidechain"))
        {
            continue;
        }
        if let Some(since) = since {
            if !get_str(&e, "timestamp").is_some_and(|t| t >= since) {
                continue;
            }
        }
        let Some(msg) = e.get("message").filter(|m| m.is_object()) else {
            continue;
        };
        let (Some(id), Some(usage)) = (
            get_str(msg, "id"),
            msg.get("usage").filter(|u| u.is_object()),
        ) else {
            continue;
        };
        let model = get_str(msg, "model")
            .filter(|m| ROUTE_MODEL.is_full(m))
            .unwrap_or("other");
        let at = match turns.iter().position(|t| t.0 == id) {
            Some(at) => at,
            None => {
                turns.push((id.to_string(), model.to_string(), [0; 4]));
                turns.len() - 1
            }
        };
        for (k, (_, field)) in USAGE_TOKENS.iter().enumerate() {
            if let Some(v) = int_of(usage.get(*field)) {
                turns[at].2[k] = turns[at].2[k].max(v);
            }
        }
    }
    let mut models: Vec<(String, [i64; 4])> = Vec::new();
    for (_, model, counts) in turns {
        let at = match models.iter().position(|m| m.0 == model) {
            Some(at) => at,
            None => {
                models.push((model, [0; 4]));
                models.len() - 1
            }
        };
        for k in 0..4 {
            models[at].1[k] += counts[k];
        }
    }
    let map = models
        .into_iter()
        .filter(|(_, c)| c.iter().any(|v| *v != 0))
        .map(|(m, c)| (m, counts_value(&c)))
        .collect();
    Ok((map, bad))
}

pub(super) fn counts_value(c: &[i64; 4]) -> Value {
    Value::Object(
        USAGE_TOKENS
            .iter()
            .zip(c)
            .map(|((k, _), v)| (k.to_string(), Value::from(*v)))
            .collect(),
    )
}

/// This host's transcripts that ran in the job's worktree, plus each main session or subagent
/// elsewhere whose own first prompt names the job folder or the job's prompt folder.
pub(super) fn usage_transcripts(home: &Path, slug: &str, job_id: &str) -> Vec<(PathBuf, bool)> {
    let root = retro_transcripts();
    if !root.is_dir() {
        return Vec::new();
    }
    let mut needles: Vec<String> = Vec::new();
    for base in ["jobs", "prompts"] {
        let p = home.join(base).join(slug).join(job_id);
        for s in [
            p.to_string_lossy().into_owned(),
            p.to_string_lossy().replace('\\', "/"),
        ] {
            if !needles.contains(&s) {
                needles.push(s);
            }
        }
    }
    let named = |text: &str| {
        needles.iter().any(|n| {
            text.match_indices(n.as_str()).any(|(at, _)| {
                !text[at + n.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
            })
        })
    };
    let worktrees = audit_folders(job_id);
    let mut found = Vec::new();
    for project in sorted_entries(&root).into_iter().filter(|p| p.is_dir()) {
        for path in jsonl(&project) {
            let all = with_subagents(&project, &path);
            if worktrees.contains(&project) {
                found.extend(all);
                continue;
            }
            found.extend(all.into_iter().filter(|(p, _)| named(&usage_prompt(p))));
        }
    }
    found
}

/// Tokens this host's job sessions spent since midnight, or the kind of error the scan hit.
pub(super) fn daily_tokens(midnight: i64) -> Result<i64, String> {
    #[cfg(test)]
    if let Some(f) = fake!(daily_tokens) {
        return f(midnight);
    }
    let root = retro_transcripts();
    if !root.is_dir() {
        return Ok(0);
    }
    let since = &format_stamp(midnight)[..19];
    let mut used = 0;
    for project in sorted_entries(&root) {
        let name = project
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if !project.is_dir() || RETRO_JOB.search(&name).is_none() {
            continue;
        }
        for path in jsonl(&project) {
            for (file, main) in with_subagents(&project, &path) {
                let fresh = fs::metadata(&file)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .is_some_and(|d| d.as_secs_f64() >= midnight as f64);
                if fresh {
                    if let Ok((models, _)) = usage_scan(&file, main, Some(since)) {
                        used += token_count(&Value::Object(models));
                    }
                }
            }
        }
    }
    Ok(used)
}

/// Input, output and cache_write tokens summed over a {model: {kind: tokens}} map.
pub(super) fn token_count(models: &Value) -> i64 {
    models
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(Value::as_object)
        .flat_map(|counts| counts.iter())
        .filter(|(k, _)| ["input", "output", "cache_write"].contains(&k.as_str()))
        .filter_map(|(_, v)| int_of(Some(v)))
        .filter(|v| *v >= 0)
        .sum()
}

/// The start of today in the host's local time zone, as UTC seconds.
pub(super) fn local_midnight() -> i64 {
    let (utc, offset) = local_now();
    (utc + offset).div_euclid(86_400) * 86_400 - offset
}

/// config.md's key as a whole number of at least 1, else None. A bad value prints one line.
pub(super) fn token_cap(io: &mut Io, home: &Path, key: &str) -> Res<Option<i64>> {
    let config = home.join("config.md");
    let meta = if config.is_file() {
        meta(&config)?
    } else {
        Vec::new()
    };
    let Some(value) = lookup(&meta, key) else {
        return Ok(None);
    };
    match value.parse::<i64>() {
        Ok(n) if n >= 1 && value.bytes().all(|b| b.is_ascii_digit()) => Ok(Some(n)),
        _ => {
            errln!(
                io,
                "pickup ignores {key} in {}: must be a whole number of at least 1",
                config.display()
            );
            Ok(None)
        }
    }
}

/// The orchestrator row for one pickup launch, on the values it actually used.
pub(super) fn launch_row(given: &Given, argv: &[String], tag: &str) -> Value {
    let stem = Path::new(&argv[0])
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let mut row = json!({"role": "orchestrator", "harness": given.provider,
        "via": if stem == "paseo" { "paseo" } else { "launcher" }, "model": given.model});
    if !given.effort.is_empty() {
        row["effort"] = Value::from(given.effort.clone());
    }
    row["session"] = Value::from(tag);
    row
}

/// One CAS: append row, stamped with the board's stage, round and time, to job_id's agents,
/// keeping the newest AGENT_ROWS_MAX. The saved row, or None for a missing job.
pub(super) fn record_agent(home: &Path, job_id: &str, row: &Value) -> Res<Option<Value>> {
    #[cfg(test)]
    if let Some(f) = fake!(record_agent) {
        return f(home, job_id, row);
    }
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = jobs_of(&data)
            .iter()
            .find(|j| get_str(j, "id") == Some(job_id))
        else {
            return Ok((None, None));
        };
        let mut full = json!({"at": now(), "stage": job.get("stage")});
        if get_str(job, "stage") == Some("review") && job.get("round").is_some_and(|r| !r.is_null())
        {
            full["round"] = job["round"].clone();
        }
        for (k, v) in row.as_object().unwrap() {
            full[k] = v.clone();
        }
        let mut rows = job
            .get("agents")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        rows.push(full.clone());
        let keep = rows.len().saturating_sub(AGENT_ROWS_MAX);
        patch_job(&mut data, job_id, &json!({"agents": rows[keep..]}));
        Ok((Some(dump_board(&data)), Some(full)))
    })
}

/// record_agent for a launch that already happened: a failed write warns instead of ending the pass.
pub(super) fn record_launch(io: &mut Io, home: &Path, job_id: &str, row: Value) {
    if let Err(e) = record_agent(home, job_id, &row) {
        let _ = io.err.write_all(e.stderr.as_bytes());
        errln!(
            io,
            "{job_id} launched, but its agent row was not recorded; the pass goes on"
        );
    }
}
