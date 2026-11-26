use super::*;

/// (returncode, or None on a timeout, and stdout) of gh, or None when gh is not on PATH.
pub(super) fn gh_cli(args: &[&str]) -> Option<(Option<i32>, String)> {
    #[cfg(test)]
    if let Some(f) = fake!(gh_cli) {
        return f(args);
    }
    let gh = which("gh")?.to_string_lossy().into_owned();
    let argv: Vec<&str> = std::iter::once(gh.as_str())
        .chain(args.iter().copied())
        .collect();
    Some(
        match capture(&argv, None, &[], Some(Duration::from_secs(60))) {
            Ok(o) => (Some(o.code), o.stdout),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => (None, String::new()),
            Err(_) => (Some(1), String::new()),
        },
    )
}

/// The doctor report: local configuration, credentials, and whether projects and board answer.
pub(super) fn doctor(home: &Path) -> Res<Value> {
    #[cfg(test)]
    if let Some(f) = fake!(doctor) {
        return f(home);
    }
    let home = resolve(home).map_err(|e| fail(os_error(&e, Some(home)), 1))?;
    let mut api_url = String::new();
    let mut checks = Vec::new();
    let mut check = |name: &str, valid: bool, message: String| {
        let status = if valid { "pass" } else { "fail" };
        checks.push(json!({"name": name, "status": status, "message": message}));
    };
    check("location", true, "Location resolved.".into());
    let mut config_error = match validate_home(&home, true) {
        Ok(()) => String::new(),
        Err(e) => strip(&e.stderr).to_string(),
    };
    if config_error.is_empty() {
        let config = home.join("config.md");
        let meta = frontmatter(&config)
            .map_err(|e| fail(os_error(&e, Some(&config)), 1))?
            .0;
        let endpoint = fm_get(&meta, "api_url").unwrap_or("");
        if endpoint.is_empty() {
            config_error = format!(
                "Set api_url in {}, then run /factory init again.",
                config.display()
            );
        } else {
            match normalized_origin(endpoint, false) {
                Ok(origin) => api_url = origin,
                Err(()) => config_error = endpoint_message(&config),
            }
        }
    }
    let config_ok = config_error.is_empty();
    check(
        "config",
        config_ok,
        if config_ok {
            "Local configuration is valid.".into()
        } else {
            config_error
        },
    );
    let env_file = home.join(".env");
    let configured = regular_metadata(&env_file).is_ok()
        && read_conf(&env_file).is_ok_and(|env| {
            ["FACTORY_ACCESS_CLIENT_ID", "FACTORY_ACCESS_CLIENT_SECRET"]
                .iter()
                .all(|k| env.get(*k).is_some_and(|v| !v.is_empty()))
        });
    check(
        "credentials",
        configured,
        if configured {
            "Service token is configured.".into()
        } else {
            token_message(&env_file)
        },
    );
    for (name, route) in [("projects", "/projects.json"), ("board", "/api/board")] {
        if !config_ok || !configured {
            check(
                name,
                false,
                "Not attempted because local configuration is invalid.".into(),
            );
            continue;
        }
        let ok: &[u16] = if name == "board" { &[200, 404] } else { &[200] };
        let read = api(&home, "GET", route, None, ok, &[]).and_then(|r| {
            if name == "board" && r.status == 404 {
                return Ok(Some("Board is absent; no board was created."));
            }
            let value = loads(&r.text()?).ok();
            let good = match name {
                "projects" => value.as_ref().is_some_and(Value::is_array),
                _ => value
                    .as_ref()
                    .is_some_and(|v| v.get("jobs").is_some_and(Value::is_array)),
            };
            Ok(good.then_some(if name == "projects" {
                "Projects are reachable."
            } else {
                "Board is reachable."
            }))
        });
        match read {
            Ok(Some(m)) => check(name, true, m.into()),
            _ => check(name, false, format!("Cannot read {route}; check connectivity, Access policy and service-token configuration.")),
        }
    }
    Ok(json!({"version": 1, "home": home.to_string_lossy(), "api_url": api_url, "checks": checks}))
}

/// Why repo's local base branch holds history origin's base does not descend from, as an
/// "unsafe checkout:" line, or None. It fetches origin's base first.
pub(super) fn checkout_problem(home: &Path, slug: &str, repo: &str) -> Res<Option<String>> {
    #[cfg(test)]
    if let Some(f) = fake!(checkout_problem) {
        return f(home, slug, repo);
    }
    let project = project_files(home, slug)?.0;
    let meta = meta(&project)?;
    let base = lookup(&meta, "base").unwrap_or("main");
    if check_path(base).is_some() || base.starts_with('-') {
        return Ok(Some(format!(
            "unsafe checkout: {slug} base {base} is not a safe branch name"
        )));
    }
    // git's stderr can carry origin's URL, credentials included, so neither fetch line includes it.
    let refspec = format!("+refs/heads/{base}:refs/remotes/origin/{base}");
    let fetched = capture(
        &["git", "-C", repo, "fetch", "-q", "origin", &refspec],
        None,
        &[("GIT_TERMINAL_PROMPT", "0")],
        Some(Duration::from_secs(FETCH_TIMEOUT_SECONDS)),
    );
    match fetched {
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            return Ok(Some(format!("unsafe checkout: git fetch origin {base} timed out after {FETCH_TIMEOUT_SECONDS} s in {repo}")))
        }
        Err(e) => return Err(fail(os_error(&e, Some(Path::new("git"))), 1)),
        Ok(o) if o.code != 0 => {
            return Ok(Some(format!(
                "unsafe checkout: git fetch origin {base} failed in {repo} (exit {})",
                o.code
            )))
        }
        Ok(_) => {}
    }
    let at = Path::new(repo);
    let origin = format!("refs/remotes/origin/{base}");
    let roots_text = git(at, &["rev-list", "--max-parents=0", &origin]);
    let roots = words(&roots_text);
    if roots.is_empty() {
        return Ok(Some(format!(
            "unsafe checkout: origin/{base} has no root commit in {repo}"
        )));
    }
    let local = format!("refs/heads/{base}");
    if git(at, &["rev-parse", "--verify", "-q", &local]).is_empty() {
        return Ok(Some(format!("unsafe checkout: {repo} has no local {base} branch; run git -C {repo} branch {base} origin/{base}")));
    }
    let local_roots = git(at, &["rev-list", "--max-parents=0", &local]);
    let mut foreign: Vec<&str> = words(&local_roots)
        .into_iter()
        .filter(|r| !roots.contains(r))
        .collect();
    foreign.sort();
    foreign.dedup();
    Ok(foreign.first().map(|first| {
        let short: String = first.chars().take(12).collect();
        format!("unsafe checkout: {repo} {base} has root {short}, which origin/{base} lacks, so it holds archive history; repoint it as docs/public-cutover.md \"Other hosts\" says")
    }))
}

/// Why no open board job owns job_id's leftovers, or None when one does.
pub(super) fn orphan_reason(jobs: &[Value], job_id: &str) -> Option<&'static str> {
    let Some(job) = jobs.iter().find(|j| get_str(j, "id") == Some(job_id)) else {
        return Some("no job on the board");
    };
    match get_str(job, "stage") {
        Some("merged") => Some("job merged"),
        Some("closed") => Some("job closed"),
        _ => None,
    }
}

/// The paths.md slug whose checkout shares cwd's git common dir, first by slug, else None.
pub(super) fn workspace_project(cwd: &str, paths: &[(String, String)]) -> Option<String> {
    let common = |path: &str| {
        let out = git(Path::new(path), &["rev-parse", "--git-common-dir"]);
        if out.is_empty() {
            return None;
        }
        let text = resolve(&Path::new(path).join(out))
            .ok()?
            .to_string_lossy()
            .into_owned();
        Some(if WINDOWS {
            text.to_lowercase().replace('/', "\\")
        } else {
            text
        })
    };
    let target = common(cwd)?;
    let mut sorted = paths.to_vec();
    sorted.sort();
    sorted
        .into_iter()
        .find(|(_, p)| common(p).as_ref() == Some(&target))
        .map(|(s, _)| s)
}

/// One orphan the scan found; the summary needs only its kind, and sorts by its name.
pub(super) struct Orphan {
    pub(super) kind: &'static str,
    pub(super) name: Option<String>,
}

/// (orphans, skipped): factory branches on origin, open factory PRs, job worktrees and
/// factory-<id> Paseo workspaces that no open board job owns. Only reads.
pub(super) fn orphan_scan(home: &Path) -> Res<(Vec<Orphan>, Vec<String>)> {
    #[cfg(test)]
    if let Some(f) = fake!(orphan_scan) {
        return f(home);
    }
    let board = read_board(home)?;
    let jobs: &[Value] = board["jobs"].as_array().map_or(&[], Vec::as_slice);
    let paths = paths_of(home)?;
    let mut orphans = Vec::new();
    let mut skipped = Vec::new();
    let mut add = |kind: &'static str, job_id: &str, name: Option<String>| {
        if orphan_reason(jobs, job_id).is_some() {
            orphans.push(Orphan { kind, name });
        }
    };
    let mut gh_missing = false;
    for (slug, path) in &paths {
        if !is_dir(path) {
            continue;
        }
        let argv = [
            "git",
            "-C",
            path,
            "ls-remote",
            "--heads",
            "origin",
            "refs/heads/factory/*",
        ];
        let listed = capture(
            &argv,
            None,
            &[("GIT_TERMINAL_PROMPT", "0")],
            Some(Duration::from_secs(20)),
        );
        match listed {
            Err(e) if e.kind() == io::ErrorKind::TimedOut => skipped.push(format!(
                "branches {slug}: git ls-remote timed out after 20 s"
            )),
            Err(_) => skipped.push(format!("branches {slug}: could not run git")),
            Ok(o) if o.code != 0 => skipped.push(format!(
                "branches {slug}: git ls-remote failed (exit {})",
                o.code
            )),
            Ok(o) => {
                for line in splitlines(&o.stdout, false) {
                    let reference = line.split_once('\t').map_or(line, |(_, r)| r);
                    if let Some(name) = reference.strip_prefix("refs/heads/") {
                        if let Some(id) = name.strip_prefix("factory/") {
                            add("branch", id, Some(name.to_string()));
                        }
                    }
                }
            }
        }
        if gh_missing {
            continue;
        }
        let md = project_files(home, slug)?.0;
        let repo = lookup(&meta(&md)?, "repo").unwrap_or("").to_string();
        if repo.is_empty() {
            skipped.push(format!("prs {slug}: the project file names no repo"));
            continue;
        }
        let args = [
            "pr",
            "list",
            "-R",
            &repo,
            "--state",
            "open",
            "--json",
            "number,headRefName,url",
            "--limit",
            "500",
        ];
        let Some((code, out)) = gh_cli(&args) else {
            gh_missing = true;
            skipped.push("prs: gh is not on PATH".into());
            continue;
        };
        let prs = loads(&out).ok().filter(|p| code == Some(0) && p.is_array());
        let Some(prs) = prs else {
            let code = code.map_or("timeout".to_string(), |c| c.to_string());
            skipped.push(format!("prs {slug}: gh pr list failed (exit {code})"));
            continue;
        };
        for pr in prs.as_array().into_iter().flatten() {
            if let Some(id) = get_str(pr, "headRefName").and_then(|h| h.strip_prefix("factory/")) {
                add("pr", id, get_str(pr, "url").map(String::from));
            }
        }
    }

    let root = home.join("worktrees");
    let listing = (|| -> Result<(), PathBuf> {
        let list = |dir: &Path| -> Result<Vec<PathBuf>, PathBuf> {
            let mut items = fs::read_dir(dir)
                .and_then(|entries| {
                    entries
                        .map(|e| e.map(|e| e.path()))
                        .collect::<io::Result<Vec<_>>>()
                })
                .map_err(|_| dir.to_path_buf())?;
            items.sort();
            Ok(items)
        };
        let projects = if is_link(&root).map_err(|_| root.clone())? {
            vec![root.clone()]
        } else if root.is_dir() {
            list(&root)?
        } else {
            Vec::new()
        };
        for project in projects {
            if is_link(&project).map_err(|_| project.clone())? {
                skipped.push(format!(
                    "worktrees: {} is a symlink or junction",
                    project.display()
                ));
                continue;
            }
            let folders = if project.is_dir() {
                list(&project)?
            } else {
                Vec::new()
            };
            for folder in folders {
                if is_link(&folder).map_err(|_| folder.clone())? {
                    skipped.push(format!(
                        "worktrees: {} is a symlink or junction",
                        folder.display()
                    ));
                } else if folder.is_dir() {
                    let id = folder.file_name().unwrap_or_default().to_string_lossy();
                    add("worktree", &id, Some(folder.to_string_lossy().into_owned()));
                }
            }
        }
        Ok(())
    })();
    if let Err(at) = listing {
        skipped.push(format!("worktrees: could not read {}", at.display()));
    }

    if paseo_bin().is_none() {
        skipped.push("workspaces: paseo is not on PATH".into());
    } else {
        let (code, out) = paseo_run(&["workspace", "ls", "--json"]);
        let spaces = loads(&out).ok().filter(|s| code == 0 && s.is_array());
        if spaces.is_none() {
            skipped.push(format!(
                "workspaces: paseo workspace ls failed (exit {code})"
            ));
        }
        let ids: Vec<&str> = jobs.iter().filter_map(|j| get_str(j, "id")).collect();
        for space in spaces
            .as_ref()
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(cwd) = get_str(space, "cwd") else {
                continue;
            };
            let trimmed = cwd.trim_end_matches('/');
            let base = basename(if trimmed.is_empty() { cwd } else { trimmed });
            let Some(rest) = base.strip_prefix("factory-") else {
                continue;
            };
            let job_id = if ids.contains(&rest) {
                rest.to_string()
            } else {
                TRAILING_NUMBER.replace_all(rest, |_| String::new())
            };
            let owned = jobs
                .iter()
                .find(|j| get_str(j, "id") == Some(job_id.as_str()))
                .and_then(|j| get_str(j, "project"))
                .is_some_and(|p| !p.is_empty());
            // Python names the workspace's project here; the summary never reads it, but its git
            // calls stay in the same order.
            if !owned {
                workspace_project(cwd, &paths);
            }
            add("workspace", &job_id, Some(cwd.to_string()));
        }
    }
    orphans.sort_by(|a, b| {
        let rank = |o: &Orphan| ORPHAN_KINDS.iter().position(|k| *k == o.kind);
        (rank(a), &a.name).cmp(&(rank(b), &b.name))
    });
    Ok((orphans, skipped))
}

/// The one pickup line for a scan's result.
pub(super) fn orphan_summary(orphans: &[Orphan], skipped: &[String]) -> String {
    if orphans.is_empty() && skipped.is_empty() {
        return "orphan scan: nothing orphaned".into();
    }
    let kinds: Vec<String> = ORPHAN_KINDS
        .iter()
        .zip(ORPHAN_NOUNS)
        .filter_map(|(kind, (one, many))| {
            let n = orphans.iter().filter(|o| o.kind == *kind).count();
            (n > 0).then(|| format!("{n} {}", if n == 1 { one } else { many }))
        })
        .collect();
    let mut line = if orphans.is_empty() {
        "orphan scan: nothing orphaned".to_string()
    } else {
        format!(
            "orphan scan: {} orphaned ({})",
            orphans.len(),
            kinds.join(", ")
        )
    };
    if skipped.len() == 1 {
        line += ", 1 source skipped";
    } else if !skipped.is_empty() {
        line += &format!(", {} sources skipped", skipped.len());
    }
    line + "; run factory orphans to list them"
}

/// True when a once-per-local-date hook's record already names today.
pub(super) fn ran_today(path: &Path, today: &str) -> bool {
    read_json(path).is_some_and(|r| {
        r.is_object() && eq_int(r.get("version"), 1) && get_str(&r, "date") == Some(today)
    })
}

/// Record today, then mark it done; the record is written first, so a stopped run waits a day.
pub(super) fn claim_today(path: &Path) -> Res<bool> {
    let (utc, offset) = local_now();
    let today = local_date(utc, offset);
    if ran_today(path, &today) {
        return Ok(false);
    }
    write_file(
        path,
        &(dumps(&json!({"version": 1, "date": today}), DEFAULT) + "\n"),
    )?;
    Ok(true)
}

/// Run the orphan scan once per local date and print its summary line.
pub(super) fn orphan_pickup_scan(io: &mut Io, home: &Path) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(orphan_pickup_scan) {
        f(home);
        return Ok(());
    }
    if !claim_today(&home.join(ORPHAN_SCAN_FILE))? {
        return Ok(());
    }
    match orphan_scan(home) {
        Ok((orphans, skipped)) => outln!(io, "{}", orphan_summary(&orphans, &skipped)),
        Err(e) => {
            let _ = io.err.write_all(e.stderr.as_bytes());
            errln!(
                io,
                "orphan scan stopped (exit {}); it runs again tomorrow",
                e.code
            );
        }
    }
    Ok(())
}

pub(super) fn outcomes(slug: &str, out: &mut String) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(outcomes) {
        return f(slug, out);
    }
    crate::commands::audit::outcomes(true, Some(slug), out)
}

/// Run outcomes record once per local date for each paths.md checkout folder that exists, printing
/// one line per project. paths.md is read before the date is recorded, and a project that stops
/// retries the next local date.
pub(super) fn outcomes_pickup_record(io: &mut Io, home: &Path) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(outcomes_pickup_record) {
        f(home);
        return Ok(());
    }
    let mut paths = paths_of(home)?;
    paths.sort();
    paths.retain(|(slug, checkout)| SLUG.is_full(slug) && is_dir(checkout));
    if !claim_today(&home.join(OUTCOMES_RECORD_FILE))? {
        return Ok(());
    }
    for (slug, _) in &paths {
        let mut out = String::new();
        if let Err(e) = outcomes(slug, &mut out) {
            let _ = io.err.write_all(e.stderr.as_bytes());
            errln!(
                io,
                "outcomes record {slug} stopped (exit {}); it runs again tomorrow",
                e.code
            );
            continue;
        }
        let lines = splitlines(&out, false);
        let prefix = format!("{slug} unknown: ");
        if let Some(m) = lines.iter().find_map(|line| {
            line.strip_prefix(&prefix)
                .and_then(|rest| OUTCOME_UNKNOWN.match_start(rest))
        }) {
            errln!(
                io,
                "outcomes record {slug}: {} is unknown: {}",
                &m[1],
                &m[2]
            );
        }
        let Some(last) = lines.last() else {
            return Err(fail(format!("outcomes record {slug} printed nothing"), 1));
        };
        outln!(io, "outcomes record {slug}: {last}");
    }
    Ok(())
}

pub(super) fn retro_pushed_today(home: &Path) -> bool {
    let (utc, offset) = local_now();
    ran_today(&home.join(RETRO_PUSH_FILE), &local_date(utc, offset))
}

pub(super) fn retro_push() -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(retro_push) {
        return f();
    }
    crate::commands::retro::retro_push()
}

/// Push this host's digest on pickup's first run of a local date with no recorded push. Best
/// effort: a failed push costs one warning, and the next pickup run retries it.
pub(super) fn retro_pickup_push(io: &mut Io, home: &Path) -> Res<()> {
    if retro_pushed_today(home) || !retro_transcripts().is_dir() {
        return Ok(());
    }
    if let Err(e) = retro_push() {
        let _ = io.err.write_all(e.stderr.as_bytes());
        errln!(
            io,
            "retro push stopped (exit {}); the next pickup run retries it",
            e.code
        );
    }
    Ok(())
}

pub(super) fn audit_stamp(job_id: &str) -> Option<i128> {
    #[cfg(test)]
    if let Some(f) = fake!(audit_stamp) {
        return f(job_id);
    }
    crate::commands::audit::audit_stamp(job_id)
}

pub(super) fn audit_push(job_id: &str) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(audit_push) {
        return f(job_id);
    }
    crate::commands::audit::audit_push(job_id, false)
}

pub(super) fn usage_record(
    home: &Path,
    slug: &str,
    job_id: &str,
    device: &str,
    totals: &Value,
) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(usage_record) {
        return f(home, slug, job_id, device, totals);
    }
    crate::commands::audit::usage_record(home, slug, job_id, device, totals)
}

/// {job id: stamp} from <home>/audit-push.json, or {} for a missing or malformed record.
pub(super) fn audit_pushed(home: &Path) -> Map<String, Value> {
    let record = read_json(&home.join(AUDIT_PUSH_FILE)).unwrap_or_default();
    let jobs = if eq_int(record.get("version"), 1) {
        record.get("jobs").and_then(Value::as_object)
    } else {
        None
    };
    match jobs {
        Some(jobs)
            if jobs
                .values()
                .all(|v| v.as_number().and_then(Number::as_i128).is_some()) =>
        {
            jobs.clone()
        }
        _ => Map::new(),
    }
}

pub(super) fn audit_push_stopped(io: &mut Io, e: Exit) -> Res<()> {
    let _ = io.err.write_all(e.stderr.as_bytes());
    errln!(
        io,
        "audit push stopped (exit {}); the next pickup run retries it",
        e.code
    );
    Ok(())
}

/// Push this host's audit rows for each in-flight or pr-open job whose transcripts changed since
/// the last recorded push, and refresh this host's token share. Best effort: a failed push costs one
/// warning, and the next pickup run retries it.
pub(super) fn audit_pickup_push(io: &mut Io, home: &Path, data: Option<&Value>) -> Res<()> {
    if !retro_transcripts().is_dir() {
        return Ok(());
    }
    let (record, mut kept) = (audit_pushed(home), Map::new());
    let read;
    let board = match data {
        Some(board) => board,
        None => match read_board(home) {
            Ok(board) => {
                read = board;
                &read
            }
            Err(e) => return audit_push_stopped(io, e),
        },
    };
    let jobs = ops::board_jobs(board)?;
    let pushed = (|| -> Res<()> {
        for job in jobs {
            let stage = get_str(job, "stage");
            if !stage.is_some_and(|s| IN_FLIGHT.contains(&s) || s == "pr-open") {
                continue;
            }
            let Some(job_id) = get_str(job, "id").filter(|id| NEW_JOB_ID.is_full(id)) else {
                continue;
            };
            let Some(stamp) = audit_stamp(job_id) else {
                continue;
            };
            if record
                .get(job_id)
                .and_then(Value::as_number)
                .and_then(Number::as_i128)
                != Some(stamp)
            {
                match audit_push(job_id) {
                    // A transcript a flag pattern could not finish reading pushes only a skip
                    // marker, and only changed transcripts retry it, so one oversized call cannot
                    // stop pickup.
                    Err(e) if e.code == crate::shared::exit::LIMIT => {
                        // Pickup goes on and cannot shorten a transcript, so drop the stop advice.
                        let why = e.stderr.trim_end();
                        let why = why.split("; stopping").next().unwrap_or(why);
                        errln!(
                            io,
                            "audit push skipped {job_id} until its transcripts change: {why}"
                        );
                    }
                    pushed => pushed?,
                }
                usage_pickup_record(io, home, job)?;
            }
            kept.insert(job_id.to_string(), Value::from(stamp));
        }
        Ok(())
    })();
    if let Err(e) = pushed {
        return audit_push_stopped(io, e);
    }
    if kept != record {
        write_file(
            &home.join(AUDIT_PUSH_FILE),
            &(dumps(&json!({"version": 1, "jobs": kept}), DEFAULT) + "\n"),
        )?;
    }
    Ok(())
}

/// Refresh this host's token share on a board job from pickup. Skips a job without a valid
/// project, a host without a valid device:, a scan with no tokens and an unchanged share.
pub(super) fn usage_pickup_record(io: &mut Io, home: &Path, job: &Value) -> Res<()> {
    let job_id = get_str(job, "id").unwrap_or_default();
    let config = home.join("config.md");
    let meta = if config.is_file() {
        meta(&config)?
    } else {
        Vec::new()
    };
    let device = lookup(&meta, "device").unwrap_or("");
    let Some(slug) = get_str(job, "project").filter(|s| SLUG.is_full(s)) else {
        return Ok(());
    };
    if !HOST_NAME.is_full(device) {
        return Ok(());
    }
    let totals = match usage_totals(io, home, slug, job_id) {
        Ok(totals) => totals,
        Err(why) => {
            errln!(
                io,
                "usage: scan failed on {job_id} ({why}); nothing recorded"
            );
            return Ok(());
        }
    };
    let old: Map<String, Value> = job
        .get("tokens")
        .and_then(Value::as_object)
        .and_then(|t| t.get(device))
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(k, _)| *k != "at")
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if !truthy(&totals["models"]) || Value::Object(old) == totals {
        return Ok(());
    }
    usage_record(home, slug, job_id, device, &totals)?;
    outln!(io, "recorded {device} tokens on {job_id}");
    Ok(())
}

pub(super) fn stop_pids(wt: &Path) -> Option<Vec<u32>> {
    #[cfg(test)]
    if let Some(f) = fake!(stop_pids) {
        return f(wt);
    }
    crate::commands::worktree::stop_pids(wt)
}

pub(super) fn tracer_follow_pass(home: &Path) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(tracer_follow_pass) {
        return f(home);
    }
    crate::commands::tracer::tracer_follow_pass(home)
}

/// True when the pass wrote the board. A host without tracer.json and with no Building in
/// Tracer mark has nothing to do.
pub(super) fn tracer_build_pass(io: &mut Io, home: &Path, jobs: Option<&Value>) -> Res<bool> {
    #[cfg(test)]
    if let Some(f) = fake!(tracer_build_pass) {
        return f(home, jobs);
    }
    if !lexists(&home.join("tracer.json")) {
        let read;
        let jobs = match jobs {
            Some(jobs) => jobs,
            None => match read_board(home) {
                Ok(board) => {
                    read = board["jobs"].clone();
                    &read
                }
                Err(e) => {
                    let reason = words(&e.stderr).join(" ");
                    let reason = if reason.is_empty() {
                        "unknown error"
                    } else {
                        &reason
                    };
                    errln!(io, "warning: Tracer build check failed: {}. Unchecked jobs keep their Building in Tracer mark.", reason.trim_end_matches('.'));
                    return Ok(false);
                }
            },
        };
        if !jobs.as_array().into_iter().flatten().any(tracer_building) {
            return Ok(false);
        }
    }
    crate::commands::tracer::tracer_build_pass(
        home,
        jobs.and_then(Value::as_array).map(Vec::as_slice),
    )
}

pub(super) fn backlog_triage_pass(home: &Path) -> Res<()> {
    #[cfg(test)]
    if let Some(f) = fake!(backlog_triage_pass) {
        return f(home);
    }
    crate::commands::intake::backlog_triage_pass(home)
}
