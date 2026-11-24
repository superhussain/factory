use super::*;

// ---- the frame: flags, halt, status, timers and the daily hooks

pub(super) struct Flags {
    pub(super) dry: bool,
    pub(super) triage_now: bool,
    pub(super) limit: usize,
    pub(super) cap: Option<usize>,
}

pub(super) fn parse_flags(args: &[String]) -> Res<Flags> {
    let usage = || fail(USAGE_PICKUP, 2);
    let (mut dry, mut triage_now, mut limit, mut cap) = (false, false, None, None);
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--dry-run" && !dry {
            dry = true;
            i += 1;
            continue;
        }
        if flag == "--triage-now" && !triage_now {
            triage_now = true;
            i += 1;
            continue;
        }
        let slot = match flag {
            "--limit" => &mut limit,
            "--max-inflight" => &mut cap,
            _ => return Err(usage()),
        };
        let Some(value) = args.get(i + 1).filter(|v| POSITIVE.is_full(v)) else {
            return Err(usage());
        };
        let n = value.parse::<usize>().unwrap_or(usize::MAX);
        if slot.is_some() || flag == "--limit" && n > 5 {
            return Err(usage());
        }
        *slot = Some(n);
        i += 2;
    }
    if dry && triage_now {
        return Err(fail(
            format!("{USAGE_PICKUP}; --triage-now cannot run with --dry-run"),
            2,
        ));
    }
    Ok(Flags {
        dry,
        triage_now,
        limit: limit.unwrap_or(1),
        cap,
    })
}

pub fn pickup(call: &Call) -> Res<()> {
    let (stdout, stderr) = (io::stdout(), io::stderr());
    let (mut out, mut err) = (stdout.lock(), stderr.lock());
    let mut io = Io {
        out: &mut out,
        err: &mut err,
    };
    // pickup takes no other value spelled --home, so one in argv is the global option.
    let overridden = std::env::args().any(|a| a == "--home");
    dispatch(&mut io, &call.args, &factory_home, overridden)
}

pub(super) fn dispatch(
    io: &mut Io,
    args: &[String],
    home: &dyn Fn() -> Res<PathBuf>,
    overridden: bool,
) -> Res<()> {
    if args.first().map(String::as_str) == Some("setup") {
        let install = match &args[1..] {
            [] => false,
            [flag] if flag == "--install" => true,
            _ => return Err(fail(USAGE_PICKUP_SETUP, 2)),
        };
        return pickup_setup(io, &home()?, install, overridden);
    }
    let flags = parse_flags(args)?;
    run(io, &home()?, &flags)
}

/// One pickup run past the flags: Python's `pickup` after its parser.
pub(super) fn run(io: &mut Io, home: &Path, flags: &Flags) -> Res<()> {
    let (device, _, minutes) = holder(home)?;
    let started = now();
    let cached = cached_status(home).2;
    let mut failing = cached.clone();
    let result = run_passes(io, home, flags, &device, minutes, &started, &mut failing);
    if !flags.dry {
        // Where a daemon holds the lock it is this host's scheduler, and on Linux that may be a
        // systemd service rather than cron. It sends the heartbeat on its own timer, so a pass
        // beside it sends one only when the triage failure changed, which only the pass knows.
        if daemon::running(home) != Some(true) {
            report_status(
                home,
                pickup_scheduler(),
                Some(&started),
                failing.as_deref(),
                None,
                true,
            );
        } else if failing != cached {
            report_status(
                home,
                daemon_scheduler(),
                Some(&started),
                failing.as_deref(),
                None,
                true,
            );
        }
    }
    result
}

/// The daemon's heartbeat: this host's status with the daemon's scheduler, its last pass and the
/// triage failure the last pass cached. Silent in every outcome.
pub(crate) fn daemon_heartbeat(home: &Path) -> bool {
    report_status(
        home,
        daemon_scheduler(),
        daemon::last_pass(home).as_deref(),
        cached_status(home).2.as_deref(),
        None,
        true,
    )
}

/// A Worker failure (exit 5) inside a pass costs its message and one line; any other exit stops pickup.
pub(super) fn stopped(io: &mut Io, e: Exit, what: &str) -> Res<()> {
    if e.code != 5 {
        return Err(e);
    }
    let _ = io.err.write_all(e.stderr.as_bytes());
    errln!(io, "factory.py: {what}");
    Ok(())
}

pub(super) fn run_passes(
    io: &mut Io,
    home: &Path,
    flags: &Flags,
    device: &str,
    minutes: u32,
    started: &str,
    failing: &mut Option<String>,
) -> Res<()> {
    if let Some(halt) = read_halt(home)? {
        if flags.dry {
            outln!(io, "skip all: {}", halt_line(&halt));
        } else {
            errln!(
                io,
                "factory.py: pickup launches nothing; {}",
                halt_line(&halt)
            );
        }
        return Ok(());
    }
    let mut data = if lexists(&home.join("tracer.json")) {
        None
    } else {
        Some(read_board(home)?)
    };
    if !flags.dry {
        if let Err(e) = tracer_follow_pass(home) {
            stopped(
                io,
                e,
                "Tracer follow pass stopped; followed jobs wait for the next pickup run",
            )?;
        }
        match tracer_build_pass(io, home, data.as_ref().map(|d| &d["jobs"])) {
            Ok(true) => data = None,
            Ok(false) => {}
            Err(e) => stopped(
                io,
                e,
                "Tracer build pass stopped; Building in Tracer marks wait for the next pickup run",
            )?,
        }
        if flags.triage_now || triage_sweep_due(home, started) {
            save_triage_sweep(home, started)?;
            data = None;
            match backlog_triage_pass(home) {
                Ok(()) => *failing = None,
                Err(e) => {
                    stopped(io, e, "backlog triage stopped; unsent lines wait for the next sweep, or run pickup --triage-now")?;
                    failing.get_or_insert_with(|| started.to_string());
                }
            }
        }
        retro_pickup_push(io, home)?;
        audit_pickup_push(io, home, data.as_ref())?;
        orphan_pickup_scan(io, home)?;
        outcomes_pickup_record(io, home)?;
    }
    retro_pickup_catchup(io, home, flags.dry);
    pickup_pass(
        io,
        home,
        device,
        minutes,
        flags.dry,
        flags.limit,
        flags.cap,
        data,
    )
}

pub(super) fn triage_sweep_due(home: &Path, stamp: &str) -> bool {
    let Some(data) = read_json(&home.join("backlog-triage-sweep.json")) else {
        return true;
    };
    if !data.is_object() || !eq_int(data.get("version"), 1) {
        return true;
    }
    let (Some(last), Some(at)) = (
        get_str(&data, "at").and_then(parse_stamp),
        parse_stamp(stamp),
    ) else {
        return true;
    };
    !(0..TRIAGE_SWEEP_MINUTES * 60).contains(&(at - last))
}

pub(super) fn save_triage_sweep(home: &Path, stamp: &str) -> Res<()> {
    write_file(
        &home.join("backlog-triage-sweep.json"),
        &(dumps(&json!({"version": 1, "at": stamp}), INDENT2) + "\n"),
    )
}

pub(super) fn check_path(value: &str) -> Option<&'static str> {
    if !REPO_PATH.is_full(value) || value.starts_with('/') || value.split('/').any(|s| s == "..") {
        return Some("must be a relative repo path of [A-Za-z0-9_./-] with no .. segment");
    }
    None
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Route {
    pub(super) name: String,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) effort: Option<String>,
}

pub(super) fn parse_route(value: &str) -> Option<(String, String, Option<String>)> {
    let parts = words(value);
    if !(2..=3).contains(&parts.len())
        || !SLUG.is_full(parts[0])
        || !ROUTE_MODEL.is_full(parts[1])
        || parts.len() == 3 && !ROUTE_EFFORTS.contains(&parts[2])
    {
        return None;
    }
    Some((
        parts[0].to_string(),
        parts[1].to_string(),
        parts.get(2).map(|e| e.to_string()),
    ))
}

pub(super) fn read_routes(home: &Path) -> Res<Vec<Route>> {
    let path = home.join("routes.md");
    let meta = meta(&path)?;
    if meta.is_empty() {
        return Err(fail(format!("no routes in {}: list this host's dispatch routes, primary first, as <name>: <provider> <model> [<effort>]", path.display()), 2));
    }
    let mut routes = Vec::new();
    for (name, value) in &meta {
        if name == "experiment" || REVIEW_ROUTE_KEYS.contains(&name.as_str()) {
            continue;
        }
        let Some((provider, model, effort)) = parse_route(value).filter(|_| SLUG.is_full(name))
        else {
            return Err(fail(
                format!(
                    "bad route {} in {}: use <name>: <provider> <model> [<effort>]",
                    repr(name),
                    path.display()
                ),
                2,
            ));
        };
        routes.push(Route {
            name: name.clone(),
            provider,
            model,
            effort,
        });
    }
    Ok(routes)
}

pub(super) fn read_limits(home: &Path) -> Res<Vec<(String, String)>> {
    let path = home.join("route-limits.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bad = || {
        fail(
            format!(
                "cannot read {}: delete it to clear every route limit",
                path.display()
            ),
            2,
        )
    };
    let text = match read_text(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::InvalidData => return Err(bad()),
        Err(e) => return Err(fail(os_error(&e, Some(&path)), 1)),
    };
    let doc = loads(&text).map_err(|_| bad())?;
    let mut limits = Vec::new();
    for (k, v) in doc.as_object().ok_or_else(bad)? {
        let until = v.as_str().ok_or_else(bad)?;
        limits.push((k.clone(), until.to_string()));
    }
    if limits.iter().any(|(_, until)| parse_stamp(until).is_none()) {
        return Err(bad());
    }
    Ok(limits)
}

/// `pickup_config`'s (enabled, hosts, reason).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Settings {
    pub(super) enabled: Option<bool>,
    pub(super) hosts: Vec<String>,
    pub(super) reason: Option<String>,
}

impl Settings {
    pub(super) fn off(reason: &str) -> Settings {
        Settings {
            enabled: None,
            hosts: Vec::new(),
            reason: Some(reason.to_string()),
        }
    }

    /// `enabled and device in hosts`.
    pub(super) fn lists(&self, device: &str) -> bool {
        self.enabled == Some(true) && self.hosts.iter().any(|h| h == device)
    }
}

pub(super) fn pickup_config(home: &Path, slug: &str) -> Res<Settings> {
    let path = format!("/api/pipeline/effective/{slug}");
    let r = api(home, "GET", &path, None, &[200, 404, 409], &[])?;
    if r.status == 404 {
        return Ok(Settings::off(
            "no pipeline view; register the project or deploy the current Worker",
        ));
    }
    if r.status == 409 {
        return Ok(Settings::off(
            "stored pipeline configuration no longer validates",
        ));
    }
    if let Some(view) = loads(&r.text()?).ok().as_ref().and_then(Value::as_object) {
        let Some(p) = view.get("pickup") else {
            return Ok(Settings {
                enabled: Some(false),
                hosts: Vec::new(),
                reason: None,
            });
        };
        if let (Some(enabled), Some(hosts)) = (
            p.get("enabled").and_then(Value::as_bool),
            p.get("hosts").and_then(Value::as_array),
        ) {
            let names: Vec<String> = hosts
                .iter()
                .filter_map(|h| h.as_str().filter(|h| HOST_NAME.is_full(h)))
                .map(String::from)
                .collect();
            if names.len() == hosts.len() {
                return Ok(Settings {
                    enabled: Some(enabled),
                    hosts: names,
                    reason: None,
                });
            }
        }
    }
    Ok(Settings::off("malformed pickup in the pipeline view"))
}

pub(super) fn pickup_settings(home: &Path, slug: &str, repo: &str, device: &str) -> Res<Settings> {
    let settings = pickup_config(home, slug)?;
    if settings.lists(device) {
        if let Some(problem) = checkout_problem(home, slug, repo)? {
            return Ok(Settings::off(&problem));
        }
    }
    Ok(settings)
}

pub(super) fn read_halt(home: &Path) -> Res<Option<Value>> {
    let r = api(home, "GET", "/api/halt", None, &[200, 404], &[])?;
    if r.status == 404 {
        return Ok(None);
    }
    let record = loads(&r.text()?).ok().filter(Value::is_object);
    let halted = record
        .as_ref()
        .and_then(|r| r.get("halted"))
        .and_then(Value::as_bool);
    match halted {
        None => Ok(Some(
            json!({"halted": true, "reason": "the Worker's halt reply is unreadable"}),
        )),
        Some(halted) => Ok(record.filter(|_| halted)),
    }
}

pub(super) fn halt_line(record: &Value) -> String {
    let fallback = match get_str(record, "by_kind") {
        Some("person") => "a person",
        Some("host") => "a host",
        _ => "an unknown source",
    };
    let by = get_str(record, "by").unwrap_or(fallback);
    let at = get_str(record, "at").unwrap_or("an unknown time");
    let reason = get_str(record, "reason").unwrap_or("no reason given");
    format!(
        "factory stopped by {} at {}: {}",
        printable_line(by, 80),
        printable_line(at, 40),
        printable_line(reason, 200)
    )
}

pub(super) fn read_launchers(home: &Path) -> Res<Vec<(String, Vec<String>)>> {
    let path = home.join("launchers.json");
    if !path.exists() {
        return Err(fail(format!("no launchers in {}: map each route in routes.md to a command; see references/pickup.md", path.display()), 2));
    }
    let doc = read_text(&path).ok().and_then(|t| loads(&t).ok());
    let launchers = doc.as_ref().and_then(|doc| {
        let map = doc.as_object()?;
        if map.len() != 2 || int_of(map.get("version")) != Some(1) {
            return None;
        }
        let mut out = Vec::new();
        for (route, argv) in map.get("launchers")?.as_object()? {
            let argv = argv.as_array()?;
            if !SLUG.is_full(route) || !(1..=64).contains(&argv.len()) {
                return None;
            }
            let args = argv
                .iter()
                .map(|a| {
                    a.as_str()
                        .filter(|a| (1..=4096).contains(&a.chars().count()) && !a.contains('\0'))
                        .map(String::from)
                })
                .collect::<Option<Vec<String>>>()?;
            out.push((route.clone(), args));
        }
        Some(out)
    });
    launchers.ok_or_else(|| {
        fail(
            format!(
                "cannot read {}: use {{\"version\": 1, \"launchers\": {{\"<route>\": [\"<program>\", \"<arg>\", ...]}}}}",
                path.display()
            ),
            2,
        )
    })
}

/// The resolved argv template for a route.
pub(super) fn route_launcher(home: &Path, route: &Route) -> Res<Vec<String>> {
    #[cfg(test)]
    if let Some(f) = fake!(route_launcher) {
        return f(home, route);
    }
    let name = &route.name;
    let path = home.join("launchers.json");
    let Some((_, mut template)) = read_launchers(home)?.into_iter().find(|(r, _)| r == name) else {
        return Err(fail(
            format!("no launcher for route {name} in {}", path.display()),
            2,
        ));
    };
    if !Path::new(&template[0]).is_absolute() {
        let Some(program) = which(&template[0]) else {
            return Err(fail(
                format!(
                    "launcher for route {name} runs {}, which is not on PATH",
                    template[0]
                ),
                2,
            ));
        };
        template[0] = program.to_string_lossy().into_owned();
    }
    if route.effort.is_none() && template.iter().any(|a| a.contains("{effort}")) {
        return Err(fail(
            format!("route {name} sets no effort, but its launcher uses {{effort}}"),
            2,
        ));
    }
    Ok(template)
}

pub(super) fn host_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "linux") {
        let release = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        if release.to_lowercase().contains("microsoft") {
            "Linux (WSL)"
        } else {
            "Linux"
        }
    } else {
        "Other"
    }
}

/// {commit, tree} from the bundle.json `factory install` writes beside the runtime, or None.
pub(super) fn bundle_stamp() -> Option<Value> {
    #[cfg(test)]
    if let Some(f) = fake!(bundle_stamp) {
        return f();
    }
    crate::commands::setup::stamp_of(&read_json(&skill_root().ok()?.join("bundle.json"))?)
}

/// When the first merge on origin's base that commit lacks landed, from the first checkout
/// holding commit. None when commit is current or no checkout holds it.
pub(super) fn bundle_behind_since(home: &Path, commit: &str) -> Option<String> {
    let mut paths = paths_of(home).ok()?;
    paths.sort();
    for (slug, repo) in paths {
        if !CHECKOUT_SLUG.is_full(&slug) || !is_dir(&repo) {
            continue;
        }
        let Ok((project, _, _)) = project_files(home, &slug) else {
            continue;
        };
        let meta = meta(&project).ok()?;
        let base = lookup(&meta, "base").unwrap_or("main");
        if check_path(base).is_some() || base.starts_with('-') {
            continue;
        }
        let origin = format!("refs/remotes/origin/{base}");
        let repo = Path::new(&repo);
        if git(repo, &["merge-base", commit, &origin]) != commit {
            continue;
        }
        let range = format!("{commit}..{origin}");
        let log = git(repo, &["log", "--first-parent", "--format=%ct", &range]);
        return words(&log)
            .last()
            .and_then(|t| t.parse::<i64>().ok())
            .map(format_stamp);
    }
    None
}

/// (scheduler, last_pickup, triage_failed_since) from the last status pickup sent.
pub(super) fn cached_status(home: &Path) -> (String, Option<String>, Option<String>) {
    let utc = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .filter(|s| parse_stamp(s).map(format_stamp).as_deref() == Some(*s))
            .map(String::from)
    };
    let cache = read_json(&home.join("host-status.json"));
    if let Some(cache) = cache.as_ref().and_then(Value::as_object) {
        if let (Some(scheduler), Some(last)) = (cache.get("scheduler"), cache.get("last_pickup")) {
            let scheduler = scheduler.as_str().filter(|s| SCHEDULERS.contains(s));
            if let Some(scheduler) =
                scheduler.filter(|_| last.is_null() || utc(Some(last)).is_some())
            {
                return (
                    scheduler.to_string(),
                    utc(Some(last)),
                    utc(cache.get("triage_failed_since")),
                );
            }
        }
    }
    ("none".into(), None, None)
}

/// The status body the Worker's PUT /api/hosts/<id>/status accepts. Holds no path or secret.
pub(super) fn host_status(
    home: &Path,
    report: &Value,
    scheduler: &str,
    last_pickup: Option<&str>,
    triage_failed_since: Option<&str>,
) -> Res<Value> {
    let failed: Vec<Value> = report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["status"] != "pass")
        .map(|c| c["name"].clone())
        .collect();
    let (routes, limits) = match read_routes(home).and_then(|r| Ok((r, read_limits(home)?))) {
        Ok(pair) => pair,
        // An OS error is not a SystemExit, so Python lets it end report_status.
        Err(e) if e.code == 1 => return Err(e),
        Err(_) => (Vec::new(), Vec::new()),
    };
    let stamp = now();
    let mut checkouts: Vec<String> = paths_of(home)?
        .into_iter()
        .filter(|(slug, path)| CHECKOUT_SLUG.is_full(slug) && is_dir(path))
        .map(|(slug, _)| slug)
        .collect();
    checkouts.sort();
    checkouts.truncate(64);
    let routes: Vec<Value> = routes
        .iter()
        .filter(|r| {
            HOST_NAME.is_full(&r.name)
                && HOST_NAME.is_full(&r.provider)
                && WORKER_MODEL.is_full(&r.model)
        })
        .take(16)
        .map(|r| {
            let until = lookup(&limits, &r.name).filter(|u| *u > stamp.as_str());
            json!({"name": r.name, "provider": r.provider, "model": r.model, "effort": r.effort,
                   "limited_until": until})
        })
        .collect();
    let bundle = bundle_stamp();
    let doctor = if failed.is_empty() {
        Value::from("ok")
    } else {
        Value::Array(failed)
    };
    let mut body = json!({
        "os": host_os(), "scheduler": scheduler, "last_pickup": last_pickup, "bundle": bundle,
        "doctor": doctor, "routes": routes, "checkouts": checkouts,
    });
    if let Some(commit) = bundle.as_ref().and_then(|b| get_str(b, "commit")) {
        if let Some(behind) = bundle_behind_since(home, commit) {
            body["bundle_behind_since"] = Value::from(behind);
        }
    }
    if let Some(since) = triage_failed_since.filter(|s| !s.is_empty()) {
        body["triage_failed_since"] = Value::from(since);
    }
    Ok(body)
}

/// PUT this host's status when config.md sets device:. Silent in every outcome.
pub(super) fn report_status(
    home: &Path,
    scheduler: &str,
    last_pickup: Option<&str>,
    triage_failed_since: Option<&str>,
    report: Option<Value>,
    save: bool,
) -> bool {
    #[cfg(test)]
    if let Some(f) = fake!(report_status) {
        return f(home, scheduler, last_pickup, triage_failed_since);
    }
    let sent = || -> Res<bool> {
        let config = meta(&home.join("config.md"))?;
        let device = lookup(&config, "device").unwrap_or("");
        if !HOST_NAME.is_full(device) {
            return Ok(false);
        }
        let report = match report.filter(|r| r.as_object().is_some_and(|o| !o.is_empty())) {
            Some(r) => r,
            None => doctor(home)?,
        };
        let body = host_status(home, &report, scheduler, last_pickup, triage_failed_since)?;
        api(
            home,
            "PUT",
            &format!("/api/hosts/{device}/status"),
            Some(dumps(&body, DEFAULT).as_bytes()),
            &[200],
            &[("Content-Type", "application/json")],
        )?;
        if save {
            write_file(
                &home.join("host-status.json"),
                &(dumps(&body, INDENT2) + "\n"),
            )?;
        }
        Ok(true)
    };
    sent().unwrap_or(false)
}
