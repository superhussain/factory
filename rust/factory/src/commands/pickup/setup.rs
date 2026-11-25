use super::*;

thread_local! {
    /// While `with_crontab_memo` runs: None until the first `crontab -l`, then its result.
    static CRONTAB_MEMO: RefCell<Option<Option<(i32, String)>>> = const { RefCell::new(None) };
}

/// Runs f with every `crontab -l` after the first answered from the first, so one check of two
/// cron units reads the crontab once, as a check of the pickup timer alone does.
pub(crate) fn with_crontab_memo<T>(f: impl FnOnce() -> T) -> T {
    CRONTAB_MEMO.with(|m| *m.borrow_mut() = Some(None));
    let result = f();
    CRONTAB_MEMO.with(|m| *m.borrow_mut() = None);
    result
}

/// (returncode, output) of a scheduler command; 127 when it is missing or times out.
pub(crate) fn setup_run(argv: &[&str], stdin: Option<&str>) -> (i32, String) {
    if argv == ["crontab", "-l"] {
        if let Some(Some(read)) = CRONTAB_MEMO.with(|m| m.borrow().clone()) {
            return read;
        }
        let read = setup_run_once(argv, stdin);
        CRONTAB_MEMO.with(|m| {
            if let Some(slot) = m.borrow_mut().as_mut() {
                *slot = Some(read.clone());
            }
        });
        return read;
    }
    setup_run_once(argv, stdin)
}

pub(super) fn setup_run_once(argv: &[&str], stdin: Option<&str>) -> (i32, String) {
    #[cfg(test)]
    if let Some(f) = fake!(setup_run) {
        return f(argv, stdin);
    }
    let timeout = Duration::from_secs(30);
    let done = match stdin {
        Some(input) => run_input(argv, input, timeout),
        None => capture(argv, None, &[], Some(timeout)),
    };
    match done {
        Ok(o) if o.code == 0 => (0, o.stdout),
        Ok(o) => (o.code, o.stdout + &o.stderr),
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            let shown: Vec<String> = argv.iter().map(|a| repr(a)).collect();
            (
                127,
                format!(
                    "Command '[{}]' timed out after 30 seconds",
                    shown.join(", ")
                ),
            )
        }
        Err(e) => (127, os_error(&e, Some(Path::new(argv[0])))),
    }
}

/// (value, None) from r, or (None, the text it failed with).
pub(super) fn setup_try<T>(r: Res<T>) -> (Option<T>, Option<String>) {
    match r {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(message(&e))),
    }
}

/// Folders the timer PATH needs for a resolved launcher: its program's, and for env the command env runs.
pub(super) fn launcher_folders(template: &[String]) -> Vec<String> {
    let mut folders = vec![dirname(&template[0])];
    if basename(&template[0]) == "env" {
        let command = template[1..]
            .iter()
            .find(|a| !a.starts_with('-') && ENV_ASSIGN.match_start(a).is_none());
        if let Some(found) = command.and_then(|c| which(c)) {
            folders.push(dirname(&found.to_string_lossy()));
        }
    }
    folders
}

/// What a host scheduler runs for factory: the 5-minute pickup timer, the daily retro-push timer,
/// or the daemon as a service that starts at login and restarts when it ends.
pub(crate) struct Unit {
    /// The unit's name; its log is `<home>/logs/<command>.log`.
    pub(super) command: &'static str,
    /// The factory command words it runs.
    pub(super) args: &'static [&'static str],
    /// The launchd label.
    pub(crate) label: &'static str,
    /// The scheduled task name, and the name of its `.cmd` and systemd unit files.
    pub(super) task: &'static str,
    /// "timer" or "service", as install and status lines name it.
    pub(super) noun: &'static str,
    /// The first words of the line a second install prints.
    pub(super) current: &'static str,
    /// True for a service that starts at login and restarts, false for a timer.
    pub(super) keep_alive: bool,
    /// True for a timer that runs at retro_push_times each day, false for every 5 minutes.
    pub(super) daily: bool,
}

pub(crate) const PICKUP: Unit = Unit {
    command: "pickup",
    args: &["pickup"],
    label: "factory.pickup",
    task: "factory-pickup",
    noun: "timer",
    current: "timer already installed",
    keep_alive: false,
    daily: false,
};

pub(crate) const DAEMON: Unit = Unit {
    command: "daemon",
    args: &["daemon"],
    label: "factory.daemon",
    task: "factory-daemon",
    noun: "service",
    current: "service current",
    keep_alive: true,
    daily: false,
};

/// The daily digest push the daemon takes over where it is installed, as schedules.md sets it up.
pub(crate) const RETRO_PUSH: Unit = Unit {
    command: "retro-push",
    args: &["retro", "push"],
    label: "factory.retro-push",
    task: "factory-retro-push",
    noun: "timer",
    current: "timer already installed",
    keep_alive: false,
    daily: true,
};

/// The local (hour, minute) times of the retro-push timer on scheduler: launchd runs it three
/// times, so a sleeping Mac still pushes before the 09:00 aggregate; cron and Task Scheduler once,
/// at 08:30.
pub(crate) fn retro_push_times(scheduler: &str) -> &'static [(u32, u32)] {
    if scheduler == "launchd" {
        &[(6, 30), (7, 30), (8, 30)]
    } else {
        &[(8, 30)]
    }
}

pub(super) fn timer_path(scheduler: &str, unit: &Unit) -> PathBuf {
    let home = std::env::home_dir().unwrap_or_default();
    match scheduler {
        "launchd" => home
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", unit.label)),
        "systemd" => home
            .join(".config/systemd/user")
            .join(format!("{}.service", unit.task)),
        _ => home.join(".config").join(format!("{}.cmd", unit.task)),
    }
}

/// What a unit runs: this binary, with the PATH its launchers need.
pub(crate) struct TimerValues {
    pub(super) unit: &'static Unit,
    pub(super) argv: Vec<String>,
    pub(super) path: String,
    pub(super) log: String,
    pub(super) exe: String,
}

pub(crate) fn timer_values(home: &Path, unit: &'static Unit) -> TimerValues {
    let exe = exe();
    let mut folders = Vec::new();
    for route in setup_try(read_routes(home)).0.unwrap_or_default() {
        if let Some(template) = setup_try(route_launcher(home, &route)).0 {
            folders.extend(launcher_folders(&template));
        }
    }
    folders.push(dirname(&exe));
    if let Some(gh) = which("gh") {
        folders.push(dirname(&gh.to_string_lossy()));
    }
    if !WINDOWS {
        folders.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(String::from));
    }
    let mut unique: Vec<String> = Vec::new();
    for f in folders {
        if !unique.contains(&f) {
            unique.push(f);
        }
    }
    let mut argv = vec![
        exe.clone(),
        "--home".into(),
        home.to_string_lossy().into_owned(),
    ];
    argv.extend(unit.args.iter().map(|a| a.to_string()));
    TimerValues {
        unit,
        argv,
        path: unique.join(if WINDOWS { ";" } else { ":" }),
        log: home
            .join("logs")
            .join(format!("{}.log", unit.command))
            .to_string_lossy()
            .into_owned(),
        exe,
    }
}

/// systemd's quoting for one unit value: `%` doubled, since every value expands specifiers, and
/// in ExecStart `$` doubled too, since only there systemd expands variables. Quoted with C escapes
/// when it holds space, a quote, a backslash or `;`.
pub(super) fn systemd_quote(s: &str, exec: bool) -> String {
    let mut escaped = s.replace('%', "%%");
    if exec {
        escaped = escaped.replace('$', "$$");
    }
    if !s.is_empty() && !s.contains(|c: char| c.is_whitespace() || "\"'\\;".contains(c)) {
        return escaped;
    }
    format!("\"{}\"", escaped.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Refuses a unit whose home, binary, log or PATH holds a line break: systemd and cron read one
/// line per setting, so the rest would become a setting or a crontab line of its own.
pub(super) fn check_unit_paths(scheduler: &str, values: &TimerValues) -> Res<()> {
    if scheduler != "systemd" && scheduler != "cron" {
        return Ok(());
    }
    let broken = values
        .argv
        .iter()
        .chain([&values.log, &values.path])
        .any(|v| v.contains(['\n', '\r']));
    if broken {
        return Err(fail(
            format!(
                "the factory home or binary path holds a line break, so a {scheduler} {} cannot run it; move it to a path without one",
                values.unit.noun
            ),
            2,
        ));
    }
    Ok(())
}

pub(super) fn timer_text(scheduler: &str, values: &TimerValues) -> String {
    let (unit, argv, path, log) = (values.unit, &values.argv, &values.path, &values.log);
    if scheduler == "cron" {
        let rest = format!(
            "PATH={} {} >> {} 2>&1",
            shlex_quote(path),
            shlex_join(argv),
            shlex_quote(log)
        );
        // The daemon holds a lock, so the 5-minute line starts it only when no copy is running.
        if unit.keep_alive {
            return format!("@reboot {rest}\n*/5 * * * * {rest}");
        }
        if unit.daily {
            let (hour, minute) = retro_push_times(scheduler)[0];
            return format!("{minute} {hour} * * * {rest}");
        }
        return format!("*/5 * * * * {rest}");
    }
    if scheduler == "launchd" {
        let mut doc = json!({
            "Label": unit.label, "ProgramArguments": argv, "EnvironmentVariables": {"PATH": path},
            "StandardOutPath": log, "StandardErrorPath": log,
        });
        if unit.keep_alive {
            doc["RunAtLoad"] = json!(true);
            doc["KeepAlive"] = json!(true);
        } else if unit.daily {
            let times: Vec<Value> = retro_push_times(scheduler)
                .iter()
                .map(|(hour, minute)| json!({"Hour": hour, "Minute": minute}))
                .collect();
            doc["StartCalendarInterval"] = Value::Array(times);
        } else {
            doc["StartInterval"] = json!(300);
        }
        return plist_dumps(&doc);
    }
    if scheduler == "systemd" {
        let exec: Vec<String> = argv.iter().map(|a| systemd_quote(a, true)).collect();
        return format!(
            "[Unit]\nDescription=factory {command}\n\n[Service]\nExecStart={}\nEnvironment={}\n\
             Restart=always\nRestartSec=10\nStandardOutput=append:{log}\nStandardError=append:{log}\n\n\
             [Install]\nWantedBy=default.target\n",
            exec.join(" "),
            systemd_quote(&format!("PATH={path}"), false),
            command = unit.command,
            log = log.replace('%', "%%"),
        );
    }
    let run = format!("{} >> \"{log}\" 2>&1", list2cmdline(argv));
    if unit.keep_alive {
        // Task Scheduler starts it once at logon; the loop restarts it 10 seconds after it ends,
        // until uninstall writes the stop marker.
        let stop = daemon::stop_marker(Path::new(&argv[2]));
        let stop = stop.display();
        return format!(
            "@echo off\r\nset \"PATH={path};%PATH%\"\r\n:run\r\nif exist \"{stop}\" exit /b 0\r\n{run}\r\nif exist \"{stop}\" exit /b 0\r\nping -n 11 127.0.0.1 >nul\r\ngoto run\r\n"
        );
    }
    format!("@echo off\r\nset \"PATH={path};%PATH%\"\r\n{run}\r\n")
}

/// The `schtasks /Create` arguments for a unit: every 5 minutes, daily at 08:30, or once at logon.
pub(super) fn schtasks_schedule(unit: &Unit) -> &'static [&'static str] {
    if unit.keep_alive {
        &["/SC", "ONLOGON"]
    } else if unit.daily {
        &["/SC", "DAILY", "/ST", "08:30"]
    } else {
        &["/SC", "MINUTE", "/MO", "5"]
    }
}

/// The `/TR` value for a `.cmd` path: quoted inside, so a path with a space stays one program.
pub(super) fn schtasks_run(cmd: &Path) -> String {
    format!("\"{}\"", cmd.display())
}

/// What the crontab line of a unit does, for the installed detail.
pub(super) fn cron_detail(unit: &Unit) -> String {
    if unit.keep_alive {
        format!(
            "crontab starts the {} at reboot and every 5 minutes",
            unit.command
        )
    } else if unit.daily {
        format!("crontab runs {} daily", unit.command)
    } else {
        format!("crontab runs {} every 5 minutes", unit.command)
    }
}

/// True when the unit is installed, or its launchd plist is left on disk without being loaded:
/// something uninstall_timer still has to remove.
pub(crate) fn timer_left(scheduler: &str, unit: &Unit) -> bool {
    timer_installed(scheduler, unit).0
        || (scheduler == "launchd" && timer_path(scheduler, unit).exists())
}

/// (installed, detail) for this host's unit.
pub(crate) fn timer_installed(scheduler: &str, unit: &Unit) -> (bool, String) {
    let label = unit.label;
    if scheduler == "launchd" {
        let plist = timer_path(scheduler, unit);
        if !plist.exists() {
            return (false, format!("no launchd job {label}"));
        }
        let target = format!("gui/{}/{label}", getuid());
        if setup_run(&["launchctl", "print", &target], None).0 != 0 {
            return (
                false,
                format!("{} exists but is not loaded", plist.display()),
            );
        }
        return (true, format!("launchd job {label} is loaded"));
    }
    if scheduler == "systemd" {
        let file = timer_path(scheduler, unit);
        let name = format!("{}.service", unit.task);
        if !file.exists() {
            return (false, format!("no systemd user unit {name}"));
        }
        if setup_run(&["systemctl", "--user", "is-enabled", &name], None).0 != 0 {
            return (
                false,
                format!("{} exists but is not enabled", file.display()),
            );
        }
        return (true, format!("systemd user unit {name} is enabled"));
    }
    if scheduler == "cron" {
        let (code, out) = setup_run(&["crontab", "-l"], None);
        if code != 0 || !splitlines(&out, false).iter().any(|l| unit_line(l, unit)) {
            return (
                false,
                format!("no factory {} line in the crontab", unit.command),
            );
        }
        if setup_run(&["pgrep", "-x", "cron"], None).0 != 0
            && setup_run(&["pgrep", "-x", "crond"], None).0 != 0
        {
            return (false, "the crontab line is set but cron is not running; on WSL run: sudo service cron start".into());
        }
        return (true, cron_detail(unit));
    }
    if setup_run(&["schtasks", "/Query", "/TN", unit.task], None).0 != 0 {
        return (false, format!("no scheduled task {}", unit.task));
    }
    (true, format!("scheduled task {} exists", unit.task))
}

/// The command a crontab line runs: fields after the schedule, minus VAR= prefixes and redirects.
/// An `@reboot` style schedule is one field, a five-field schedule is five.
pub(super) fn cron_argv(line: &str) -> Option<Vec<String>> {
    let words = shlex_split(line)?;
    let skip = if words.first().is_some_and(|w| w.starts_with('@')) {
        1
    } else {
        5
    };
    let mut words: Vec<String> = words.into_iter().skip(skip).collect();
    while words
        .first()
        .is_some_and(|w| ENV_ASSIGN.match_start(w).is_some())
    {
        words.remove(0);
    }
    let end = words
        .iter()
        .position(|w| w.starts_with('>') || w.starts_with("2>"))
        .unwrap_or(words.len());
    words.truncate(end);
    Some(words)
}

/// True when a crontab line runs the unit's command: after the schedule and any VAR= prefixes, a
/// token named factory.py, factory or factory.exe, then --flag value pairs, then the command.
/// Tokens are read as the shell reads them, so a quoted path or value timer_text wrote still
/// matches. Python's `factory\.py.*\bpickup\b` also matched lines that only mention both words,
/// such as a retro line logging to pickup.log, and install deleted them.
pub(super) fn unit_line(line: &str, unit: &Unit) -> bool {
    let Some(words) = cron_argv(line) else {
        return false;
    };
    let program = |w: &str| {
        let name = w.rsplit(['/', '\\']).next().unwrap_or(w);
        ["factory.py", "factory", "factory.exe"].contains(&name)
    };
    words.iter().enumerate().any(|(at, w)| {
        let mut rest = words[at + 1..].iter();
        program(w)
            && loop {
                match rest.next() {
                    Some(flag) if flag.starts_with("--") => {
                        rest.next();
                    }
                    next => {
                        break next.is_some_and(|n| n == unit.args[0])
                            && rest
                                .by_ref()
                                .take(unit.args.len() - 1)
                                .map(String::as_str)
                                .eq(unit.args[1..].iter().copied())
                    }
                }
            }
    })
}

/// None when the installed unit runs what --install would write, else what differs.
pub(crate) fn timer_drift(scheduler: &str, values: &TimerValues) -> Option<String> {
    let (unit, want) = (values.unit, &values.argv);
    let mut have_path = None;
    let have: Vec<String>;
    if scheduler == "launchd" {
        let plist = timer_path(scheduler, unit);
        let data = fs::read(&plist)
            .ok()
            .and_then(|b| plist_loads(&b))
            .filter(Value::is_object);
        let Some(data) = data else {
            return Some(format!("could not read {}", plist.display()));
        };
        let args = data.get("ProgramArguments").and_then(Value::as_array);
        let Some(args) =
            args.and_then(|a| a.iter().map(|v| v.as_str().map(String::from)).collect())
        else {
            return Some(format!(
                "could not read ProgramArguments in {}",
                plist.display()
            ));
        };
        have = args;
        have_path = data
            .get("EnvironmentVariables")
            .and_then(|e| e.get("PATH"))
            .map(|p| p.as_str().map_or_else(|| p.to_string(), String::from));
    } else if scheduler == "cron" {
        let out = setup_run(&["crontab", "-l"], None).1;
        let lines: Vec<&str> = splitlines(&out, false)
            .into_iter()
            .filter(|l| unit_line(l, unit))
            .collect();
        let argvs: Vec<Option<Vec<String>>> = lines.iter().map(|l| cron_argv(l)).collect();
        if argvs.iter().any(|a| a.as_ref() == Some(want)) {
            return None;
        }
        match argvs.first().cloned().flatten().filter(|a| !a.is_empty()) {
            Some(a) => have = a,
            None => {
                let line = format!(
                    "could not read the crontab line {}",
                    lines.first().unwrap_or(&"")
                );
                return Some(line.trim_end().to_string());
            }
        }
    } else {
        let file = timer_path(scheduler, unit);
        if fs::read(&file).ok().as_deref() == Some(timer_text(scheduler, values).as_bytes()) {
            return None;
        }
        return Some(format!(
            "{} differs from what --install would write",
            file.display()
        ));
    }
    let noun = unit.noun;
    if &have == want {
        if scheduler == "launchd" && have_path.as_deref() != Some(values.path.as_str()) {
            let shown = have_path.filter(|p| !p.is_empty());
            return Some(format!(
                "the {noun} PATH is {}, but --install would write {}",
                shown.as_deref().unwrap_or("unset"),
                values.path
            ));
        }
        return None;
    }
    Some(format!(
        "the {noun} runs {}, but --install would write {}",
        shlex_join(&have),
        shlex_join(want)
    ))
}

/// The crontab lines, or the error that leaves it as is. A missing crontab reads as empty.
pub(super) fn crontab_lines() -> Res<Vec<String>> {
    let (read, listed) = setup_run(&["crontab", "-l"], None);
    if read != 0 && !(read == 1 && listed.contains("no crontab for")) {
        return Err(fail(
            format!(
                "could not read the crontab, so it was left as is: {}",
                strip(&listed)
            ),
            1,
        ));
    }
    Ok(if read == 0 {
        splitlines(&listed, false)
            .into_iter()
            .map(String::from)
            .collect()
    } else {
        Vec::new()
    })
}

/// Write and load a unit; one line saying what changed. A unit that already matches is left alone.
/// How many times install polls `launchctl print` after a bootout, 250 ms apart: about 10 s.
pub(super) const BOOTOUT_POLLS: u32 = 40;

/// A wait between launchctl steps; tests run their stubs without it.
pub(super) fn pause(d: Duration) {
    if !cfg!(test) {
        std::thread::sleep(d);
    }
}

/// Polls for a launchd job to come up after a reload: 20 prints 250 ms apart, about 5 s.
pub(super) const RUNNING_POLLS: u32 = 20;

/// Reloads a launchd job from its plist with bootout then bootstrap: (code, output) of the
/// bootstrap. launchd re-reads the binary's signature only on bootstrap; after the binary is
/// replaced, `kickstart -k` fails with OS_REASON_CODESIGNING ("needs LWCR update"). A job that is
/// not loaded makes bootout fail, which is fine, so its result is ignored.
pub(crate) fn launchd_reload(
    label: &str,
    plist: &Path,
    run: &mut dyn FnMut(&[&str]) -> (i32, String),
) -> (i32, String) {
    let uid = getuid();
    let target = format!("gui/{uid}/{label}");
    run(&["launchctl", "bootout", &target]);
    // bootout returns before launchd lets go of the job; bootstrapping a label that is still
    // loaded fails with "5: Input/output error", so wait for print to stop finding it.
    for _ in 0..BOOTOUT_POLLS {
        if run(&["launchctl", "print", &target]).0 != 0 {
            break;
        }
        pause(Duration::from_millis(250));
    }
    // A disabled label makes bootstrap fail with "5: Input/output error" too.
    run(&["launchctl", "enable", &target]);
    let shown = plist.to_string_lossy();
    let bootstrap = ["launchctl", "bootstrap", &format!("gui/{uid}"), &shown];
    let mut result = run(&bootstrap);
    if result.0 != 0 && result.1.contains("Input/output error") {
        pause(Duration::from_secs(2));
        result = run(&bootstrap);
    }
    result
}

/// The pid of a launchd job once `launchctl print` shows it running with a pid, polled for about
/// 5 s; otherwise why not, from the last print's job state and exit reason.
pub(crate) fn launchd_running(
    label: &str,
    run: &mut dyn FnMut(&[&str]) -> (i32, String),
) -> Result<u32, String> {
    let target = format!("gui/{}/{label}", getuid());
    let mut last = (0, String::new());
    for i in 0..RUNNING_POLLS {
        if i > 0 {
            pause(Duration::from_millis(250));
        }
        last = run(&["launchctl", "print", &target]);
        if last.0 != 0 {
            continue;
        }
        let field = |name: &str| {
            last.1.lines().find_map(|l| {
                let (k, v) = l.trim().split_once(" = ")?;
                (k == name).then(|| v.trim().to_string())
            })
        };
        if field("state").as_deref() == Some("running") {
            if let Some(pid) = field("pid").and_then(|p| p.parse().ok()) {
                return Ok(pid);
            }
        }
    }
    if last.0 != 0 {
        let text = strip(&last.1);
        return Err(format!(
            "{target} is not loaded: {}",
            text.split('\n').next().unwrap_or("")
        ));
    }
    let wanted = ["state", "job state", "last exit code", "last exit reason"];
    let mut reason: Vec<&str> = Vec::new();
    for name in wanted {
        let line = last
            .1
            .lines()
            .map(str::trim)
            .find(|l| l.split_once(" = ").is_some_and(|(k, _)| k == name));
        reason.extend(line);
    }
    if reason.is_empty() {
        reason.push("no pid");
    }
    Err(format!("{target} is not running: {}", reason.join(", ")))
}

pub(super) fn install_timer(home: &Path, scheduler: &str, unit: &'static Unit) -> Res<String> {
    let values = timer_values(home, unit);
    check_unit_paths(scheduler, &values)?;
    mkdirs(&home.join("logs"))?;
    let text = timer_text(scheduler, &values);
    let noun = unit.noun;
    let (code, out, done);
    if scheduler == "launchd" {
        let plist = timer_path(scheduler, unit);
        let target = format!("gui/{}/{}", getuid(), unit.label);
        if fs::read(&plist).ok().as_deref() == Some(text.as_bytes())
            && setup_run(&["launchctl", "print", &target], None).0 == 0
        {
            return Ok(format!("{}: {}", unit.current, plist.display()));
        }
        mkdirs(plist.parent().unwrap())?;
        write_file(&plist, &text)?;
        (code, out) = launchd_reload(unit.label, &plist, &mut |argv| setup_run(argv, None));
        done = format!("installed launchd {noun}: {}", plist.display());
    } else if scheduler == "systemd" {
        let file = timer_path(scheduler, unit);
        let name = format!("{}.service", unit.task);
        let ok = |verb: &str| setup_run(&["systemctl", "--user", verb, &name], None).0 == 0;
        if fs::read(&file).ok().as_deref() == Some(text.as_bytes())
            && ok("is-enabled")
            && ok("is-active")
        {
            return Ok(format!("{}: {}", unit.current, file.display()));
        }
        mkdirs(file.parent().unwrap())?;
        write_file(&file, &text)?;
        let mut result = (0, String::new());
        for verb in ["daemon-reload", "enable", "restart"] {
            let argv: &[&str] = if verb == "daemon-reload" {
                &["systemctl", "--user", verb]
            } else {
                &["systemctl", "--user", verb, &name]
            };
            result = setup_run(argv, None);
            if result.0 != 0 {
                break;
            }
        }
        (code, out) = result;
        done = format!("installed systemd {noun}: {}", file.display());
    } else if scheduler == "cron" {
        let lines = crontab_lines()?;
        let mut wanted: Vec<&str> = lines
            .iter()
            .map(String::as_str)
            .filter(|l| !unit_line(l, unit))
            .collect();
        wanted.extend(text.lines());
        if lines.iter().map(String::as_str).eq(wanted.iter().copied()) {
            return Ok(format!("{}: crontab", unit.current));
        }
        (code, out) = setup_run(&["crontab", "-"], Some(&(wanted.join("\n") + "\n")));
        done = format!("installed cron {noun}: crontab");
    } else {
        let cmd = timer_path(scheduler, unit);
        if unit.keep_alive {
            clear_stop_marker(home)?;
        }
        if fs::read(&cmd).ok().as_deref() == Some(text.as_bytes())
            && setup_run(&["schtasks", "/Query", "/TN", unit.task], None).0 == 0
        {
            return Ok(format!("{}: {}", unit.current, cmd.display()));
        }
        mkdirs(cmd.parent().unwrap())?;
        write_file(&cmd, &text)?;
        let run = schtasks_run(&cmd);
        let mut create = vec!["schtasks", "/Create", "/TN", unit.task];
        create.extend(schtasks_schedule(unit));
        create.extend(["/TR", &run, "/F"]);
        let mut result = setup_run(&create, None);
        if result.0 == 0 && unit.keep_alive {
            result = setup_run(&["schtasks", "/Run", "/TN", unit.task], None);
        }
        (code, out) = result;
        done = format!("installed schtasks {noun}: {}", cmd.display());
    }
    if code != 0 {
        let label = if scheduler == "launchd" {
            format!(" {}", unit.label)
        } else {
            String::new()
        };
        return Err(fail(
            format!(
                "could not install the {scheduler} {noun}{label}: {}",
                strip(&out)
            ),
            1,
        ));
    }
    Ok(done)
}

/// Stop and remove a unit; one line saying what changed.
pub(crate) fn uninstall_timer(home: &Path, scheduler: &str, unit: &Unit) -> Res<String> {
    let noun = unit.noun;
    let fail_with = |out: &str| {
        fail(
            format!("could not remove the {scheduler} {noun}: {}", strip(out)),
            1,
        )
    };
    let remove = |file: &Path| match fs::remove_file(file) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(fail(os_error(&e, Some(file)), 1)),
        _ => Ok(()),
    };
    if scheduler == "launchd" {
        let plist = timer_path(scheduler, unit);
        let target = format!("gui/{}/{}", getuid(), unit.label);
        let loaded = setup_run(&["launchctl", "print", &target], None).0 == 0;
        if !loaded && !plist.exists() {
            return Ok(format!("no launchd {noun} {} to remove", unit.label));
        }
        if loaded {
            let (code, out) = setup_run(&["launchctl", "bootout", &target], None);
            if code != 0 {
                return Err(fail_with(&out));
            }
        }
        remove(&plist)?;
        return Ok(format!("removed launchd {noun}: {}", plist.display()));
    }
    if scheduler == "systemd" {
        let file = timer_path(scheduler, unit);
        if !file.exists() {
            return Ok(format!("no systemd {noun} {}.service to remove", unit.task));
        }
        let name = format!("{}.service", unit.task);
        let (code, out) = setup_run(&["systemctl", "--user", "disable", "--now", &name], None);
        if code != 0 {
            return Err(fail_with(&out));
        }
        remove(&file)?;
        setup_run(&["systemctl", "--user", "daemon-reload"], None);
        return Ok(format!("removed systemd {noun}: {}", file.display()));
    }
    if scheduler == "cron" {
        let lines = crontab_lines()?;
        let kept: Vec<&str> = lines
            .iter()
            .map(String::as_str)
            .filter(|l| !unit_line(l, unit))
            .collect();
        if kept.len() == lines.len() {
            return Ok(format!("no cron {noun} to remove"));
        }
        let text = if kept.is_empty() {
            String::new()
        } else {
            kept.join("\n") + "\n"
        };
        let (code, out) = setup_run(&["crontab", "-"], Some(&text));
        if code != 0 {
            return Err(fail_with(&out));
        }
        return Ok(format!("removed cron {noun}: crontab"));
    }
    let cmd = timer_path(scheduler, unit);
    if setup_run(&["schtasks", "/Query", "/TN", unit.task], None).0 != 0 {
        remove(&cmd)?;
        return Ok(format!("no scheduled task {} to remove", unit.task));
    }
    if unit.keep_alive {
        // /End can leave the .cmd loop running; the marker makes it exit instead of restarting.
        write_stop_marker(home)?;
    }
    setup_run(&["schtasks", "/End", "/TN", unit.task], None);
    let (code, out) = setup_run(&["schtasks", "/Delete", "/TN", unit.task, "/F"], None);
    if code != 0 {
        return Err(fail_with(&out));
    }
    remove(&cmd)?;
    Ok(format!("removed schtasks {noun}: {}", cmd.display()))
}

/// The lines that set up a unit by hand.
pub(super) fn timer_fix(scheduler: &str, home: &Path, unit: &'static Unit) -> Vec<String> {
    let text = timer_text(scheduler, &timer_values(home, unit));
    let path = timer_path(scheduler, unit);
    if scheduler == "cron" {
        let mut lines = vec![if unit.keep_alive {
            "Add these lines with crontab -e:".to_string()
        } else {
            "Add this line with crontab -e:".to_string()
        }];
        lines.extend(text.lines().map(String::from));
        return lines;
    }
    let mut lines = vec![if scheduler == "schtasks" && unit.keep_alive {
        format!(
            "First save this restart loop as {}; the task runs it:",
            path.display()
        )
    } else {
        format!("Save as {}:", path.display())
    }];
    lines.extend(splitlines(&text, false).into_iter().map(String::from));
    lines.push(match scheduler {
        "launchd" => format!(
            "Load it: launchctl bootstrap gui/{} {}",
            getuid(),
            path.display()
        ),
        "systemd" => format!(
            "Then run: systemctl --user daemon-reload && systemctl --user enable --now {}.service",
            unit.task
        ),
        _ => format!(
            "Then run: schtasks /Create /TN {} {} /TR \"\\\"{}\\\"\" /F",
            unit.task,
            schtasks_schedule(unit).join(" "),
            path.display()
        ),
    });
    lines
}

/// One of the 7 setup checks. ok is None when an earlier check it depends on failed.
pub(super) struct Check {
    pub(super) name: &'static str,
    pub(super) ok: Option<bool>,
    pub(super) detail: String,
    pub(super) fix: Vec<String>,
    pub(super) note: Option<String>,
}

/// The checks; daemon is the installed daemon service's detail, or None when it is not installed.
pub(super) fn setup_checks(
    home: &Path,
    device: &str,
    overridden: bool,
    daemon: Option<&str>,
) -> Res<Vec<Check>> {
    let mut argv = vec![exe()];
    if overridden {
        argv.extend(["--home".to_string(), home.to_string_lossy().into_owned()]);
    }
    let run = shlex_join(&argv);
    let skill = skill_root()?;
    let check = |name, ok, detail, fix| Check {
        name,
        ok,
        detail,
        fix,
        note: None,
    };
    let mut checks = Vec::new();

    let failing: Vec<String> = doctor(home)?["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["status"] != "pass")
        .map(|c| c["name"].as_str().unwrap_or_default().to_string())
        .collect();
    checks.push(check(
        "home",
        Some(failing.is_empty()),
        if failing.is_empty() {
            "config, credentials, projects and board pass".into()
        } else {
            format!("failing: {}", failing.join(", "))
        },
        vec![
            format!("Run: {run} doctor"),
            "Then fix each failing check; see docs/troubleshooting.md in the factory repository."
                .into(),
        ],
    ));

    let config = home.join("config.md");
    let named = lookup(&meta(&config)?, "device").unwrap_or("").to_string();
    let full = hostname();
    let short = full.split('.').next().unwrap_or("");
    let valid = !named.is_empty() && HOST_NAME.is_full(&named);
    checks.push(check(
        "device",
        Some(valid),
        if valid {
            format!("device: {device}")
        } else if !named.is_empty() {
            format!(
                "config.md device: {} is not a valid host name",
                repr(&named)
            )
        } else {
            format!("config.md sets no device:, so this host is {device}")
        },
        vec![
            if named.is_empty() {
                format!(
                    "Add this line inside the frontmatter of {}:",
                    config.display()
                )
            } else {
                format!(
                    "Replace the device: line in the frontmatter of {} with:",
                    config.display()
                )
            },
            format!(
                "device: {}",
                if HOST_NAME.is_full(short) {
                    short
                } else {
                    "<host-name>"
                }
            ),
        ],
    ));

    let paths = paths_of(home)?;
    let slugs: Vec<&(String, String)> = paths.iter().filter(|(_, p)| is_dir(p)).collect();
    let names: Vec<&str> = slugs.iter().map(|(s, _)| s.as_str()).collect();
    checks.push(check(
        "checkout",
        Some(!slugs.is_empty()),
        if slugs.is_empty() {
            "paths.md lists no checkout that exists on this host".into()
        } else {
            names.join(", ")
        },
        vec![format!("Run: {run} init <repo-path>")],
    ));

    let (routes, why) = setup_try(read_routes(home));
    checks.push(check(
        "routes",
        Some(routes.is_some()),
        match &routes {
            Some(r) if !r.is_empty() => r
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            _ => why.unwrap_or_else(|| "None".into()),
        },
        vec![
            format!("Write {}:", home.join("routes.md").display()),
            "---".into(),
            "primary: <provider> <model> <effort>".into(),
            "---".into(),
        ],
    ));

    let (launchers, why) = match &routes {
        None => (None, Some("not checked: fix check 4 first".to_string())),
        Some(routes) => {
            let why = routes
                .iter()
                .find_map(|r| setup_try(route_launcher(home, r)).1);
            (Some(why.is_none()), why)
        }
    };
    let mut example = Map::new();
    for r in routes.iter().flatten() {
        example.insert(r.name.clone(), json!(["<program>", "<arg>", "..."]));
    }
    checks.push(check(
        "launchers",
        launchers,
        why.unwrap_or_else(|| "every route has a launcher".into()),
        vec![
            format!("Write {}:", home.join("launchers.json").display()),
            dumps(&json!({"version": 1, "launchers": example}), DEFAULT),
            format!(
                "Launcher examples: {}",
                skill.join("references").join("pickup.md").display()
            ),
        ],
    ));

    let (listed, detail) = if !failing.is_empty() || slugs.is_empty() {
        let first: Vec<&str> = [("1", !failing.is_empty()), ("3", slugs.is_empty())]
            .iter()
            .filter(|(_, bad)| *bad)
            .map(|(n, _)| *n)
            .collect();
        let plural = if first.len() == 2 { "s" } else { "" };
        (
            None,
            format!(
                "not checked: fix check{plural} {} first",
                first.join(" and ")
            ),
        )
    } else {
        let (mut on, mut parts) = (Vec::new(), Vec::new());
        for (slug, path) in &slugs {
            let (settings, why) = setup_try(pickup_settings(home, slug, path, device));
            let settings = settings.unwrap_or(Settings {
                enabled: None,
                hosts: Vec::new(),
                reason: why,
            });
            if let Some(why) = &settings.reason {
                parts.push(format!("{slug}: {why}"));
            } else if settings.enabled != Some(true) {
                parts.push(format!("{slug}: pickup off"));
            } else if !settings.lists(device) {
                parts.push(format!(
                    "{slug}: pickup lists {}, not {device}",
                    settings.hosts.join(", ")
                ));
            } else {
                on.push(slug.as_str());
            }
        }
        let detail = if on.is_empty() {
            parts.join("; ")
        } else {
            format!("pickup lists {device} for {}", on.join(", "))
        };
        (Some(!on.is_empty()), detail)
    };
    checks.push(check(
        "pipeline",
        listed,
        detail,
        if listed.is_none() {
            Vec::new()
        } else {
            vec![
                "Fix check 2 first if this host name is wrong.".into(),
                format!("Otherwise, in the dashboard, Settings, Pipeline (or Settings, Projects, <project>), turn pickup on and add host {device} exactly as written."),
            ]
        },
    ));

    // An installed daemon service is this host's scheduler, and no pickup timer is needed.
    if let Some(detail) = daemon {
        checks.push(check(
            "timer",
            Some(true),
            format!("scheduler: daemon ({detail})"),
            Vec::new(),
        ));
        return Ok(checks);
    }
    let scheduler = pickup_scheduler();
    let (installed, detail) = timer_installed(scheduler, &PICKUP);
    let note = if installed {
        timer_drift(scheduler, &timer_values(home, &PICKUP))
    } else {
        None
    };
    let mut fix = timer_fix(scheduler, home, &PICKUP);
    fix.push(format!("Or run: {run} pickup setup --install"));
    let mut timer = check("timer", Some(installed), detail, fix);
    timer.note = note.map(|n| format!("{n}. To update it, run: {run} pickup setup --install"));
    checks.push(timer);
    Ok(checks)
}

/// Check every piece pickup needs on this host and print the fix for each missing one; with
/// install, write and load the timer first, unless the daemon service runs pickup on this host.
/// Exits 3 until all 7 pass.
pub(super) fn pickup_setup(io: &mut Io, home: &Path, install: bool, overridden: bool) -> Res<()> {
    let device = holder(home)?.0;
    let (has_daemon, daemon_detail) = timer_installed(daemon_scheduler(), &DAEMON);
    let daemon = has_daemon.then_some(daemon_detail);
    let scheduler = if daemon.is_some() {
        "daemon"
    } else {
        pickup_scheduler()
    };
    if install && daemon.is_some() {
        // A timer left beside the daemon goes, loaded or not, as `daemon setup --install`
        // removes it.
        let timer = pickup_scheduler();
        if timer_left(timer, &PICKUP) {
            let mut files = timer_backup(timer, &PICKUP);
            if timer == "cron" {
                files.push((
                    "crontab.txt".into(),
                    setup_run(&["crontab", "-l"], None).1.into_bytes(),
                ));
            }
            backup_files(io, home, files)?;
            outln!(io, "{}", uninstall_timer(home, timer, &PICKUP)?);
        } else {
            outln!(
                io,
                "no pickup timer installed: the daemon service runs pickup"
            );
        }
    } else if install {
        outln!(io, "{}", install_timer(home, scheduler, &PICKUP)?);
    }
    outln!(
        io,
        "pickup setup on host {device} ({}, {scheduler})",
        host_os()
    );
    let values = timer_values(home, &PICKUP);
    outln!(
        io,
        "skill {}  timer runs {}  home {}",
        skill_root()?.display(),
        values.exe,
        home.display()
    );
    let checks = setup_checks(home, &device, overridden, daemon.as_deref())?;
    for (n, c) in checks.iter().enumerate() {
        let label = if c.ok == Some(true) { "ok" } else { "missing" };
        outln!(io, "{}. {label:<8} {}: {}", n + 1, c.name, c.detail);
        if let Some(note) = &c.note {
            outln!(io, "    note: {note}");
        }
        if c.ok != Some(true) {
            for line in &c.fix {
                outln!(io, "    {line}");
            }
        }
    }
    let passed = checks.iter().filter(|c| c.ok == Some(true)).count();
    let rest = if passed == 7 {
        ""
    } else {
        " Pickup does nothing on this host until every check is ok."
    };
    outln!(io, "{passed} of 7 ok.{rest}");
    let runner = if daemon.is_some() {
        "daemon service"
    } else {
        "host timer"
    };
    outln!(io, "A stored Paseo or Claude schedule prompt does not pick up ideas. Only the {runner} runs pickup.");
    Err(exit(if passed == 7 { 0 } else { 3 }))
}

/// The scheduler that runs the daemon service: launchd on macOS, Task Scheduler on Windows, and on
/// Linux and WSL systemd when this user's systemd instance answers, else cron.
pub(crate) fn daemon_scheduler() -> &'static str {
    #[cfg(test)]
    if let Some(s) = fake!(daemon_scheduler) {
        return s;
    }
    if cfg!(target_os = "macos") || cfg!(windows) {
        return PICKUP_SCHEDULER;
    }
    linux_scheduler()
}

#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
pub(super) fn linux_scheduler() -> &'static str {
    if setup_run(&["systemctl", "--user", "show-environment"], None).0 == 0 {
        "systemd"
    } else {
        "cron"
    }
}

/// One word for the daemon: running while a copy holds its lock, else stopped or not installed.
/// installed when the lock could not be checked.
pub(super) fn service_state(installed: bool, running: Option<bool>) -> &'static str {
    match (running, installed) {
        (Some(true), _) => "running",
        (Some(false), true) => "stopped",
        (None, true) => "installed",
        (_, false) => "not installed",
    }
}

/// The daemon's state for the doctor line. An installed daemon is the pickup scheduler, so the line
/// says so and when its last pass started.
pub(crate) fn daemon_state(home: &Path) -> String {
    let installed = timer_installed(daemon_scheduler(), &DAEMON).0;
    let state = service_state(installed, daemon::running(home));
    if !installed {
        return state.to_string();
    }
    let last = daemon::last_pass(home).unwrap_or_else(|| "none yet".into());
    format!("{state}; scheduler for pickup; last pass {last}")
}

#[cfg(test)]
pub(crate) fn test_uid() -> u32 {
    getuid()
}

/// Writes the Windows stop marker, so the restart loop exits instead of starting the daemon again.
pub(super) fn write_stop_marker(home: &Path) -> Res<()> {
    let marker = daemon::stop_marker(home);
    mkdirs(marker.parent().unwrap())?;
    write_file(&marker, "")
}

/// Removes the Windows stop marker, so the restart loop runs the daemon again.
pub(crate) fn clear_stop_marker(home: &Path) -> Res<()> {
    let marker = daemon::stop_marker(home);
    match fs::remove_file(&marker) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(fail(os_error(&e, Some(&marker)), 1)),
        _ => Ok(()),
    }
}

/// The command that ends one process, or None for a pid kill would misread: 0 is this process
/// group, and past i32::MAX the value wraps to a negative group or every process.
pub(super) fn kill_argv(pid: u32) -> Option<Vec<String>> {
    if pid == 0 || pid > i32::MAX as u32 {
        return None;
    }
    let pid = pid.to_string();
    let argv: &[&str] = if WINDOWS {
        &["taskkill", "/PID", &pid, "/T", "/F"]
    } else {
        &["kill", &pid]
    };
    Some(argv.iter().map(|a| a.to_string()).collect())
}

/// The kill command for the daemon that holds the lock: None when no daemon holds it.
pub(super) fn locked_kill(home: &Path) -> Result<Option<Vec<String>>, String> {
    match daemon::locked_pid(home)? {
        None => Ok(None),
        Some(pid) => kill_argv(pid)
            .map(Some)
            .ok_or_else(|| format!("refusing to signal pid {pid}")),
    }
}

/// The commands that restart an installed daemon service so it runs a newly installed binary, in
/// order; empty when there is nothing to restart. Cron has no service, so it ends the daemon that
/// holds the lock and the caller starts the new binary with spawn_daemon. On Windows /End can
/// leave the .cmd loop and its daemon running, so the stop marker is written first, the daemon is
/// ended by pid, and the caller clears the marker before /Run starts the task again.
pub(crate) fn daemon_restart_argvs(
    scheduler: &str,
    home: &Path,
) -> Result<Vec<Vec<String>>, String> {
    let argv = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<String>>();
    Ok(match scheduler {
        // launchd restarts through launchd_reload, since kickstart -k cannot spawn a new binary.
        "launchd" => Vec::new(),
        "systemd" => vec![argv(&[
            "systemctl",
            "--user",
            "restart",
            &format!("{}.service", DAEMON.task),
        ])],
        "schtasks" => {
            // Read the pid first: a failed read must not leave the marker behind.
            let kill = locked_kill(home)?;
            write_stop_marker(home).map_err(|e| message(&e))?;
            let mut argvs = vec![argv(&["schtasks", "/End", "/TN", DAEMON.task])];
            argvs.extend(kill);
            argvs.push(argv(&["schtasks", "/Run", "/TN", DAEMON.task]));
            argvs
        }
        _ => locked_kill(home)?.into_iter().collect(),
    })
}

/// Starts the daemon now, detached, by running the command of its `*/5` crontab line through sh,
/// as cron would. The line names the installed binary; this process's own path may be gone once
/// catch-up has replaced the binary.
pub(crate) fn spawn_daemon() -> Result<(), String> {
    let lines = crontab_lines().map_err(|e| message(&e))?;
    let command = lines
        .iter()
        .filter(|l| unit_line(l, &DAEMON))
        .find_map(|l| l.strip_prefix("*/5 * * * * "))
        .ok_or("no */5 daemon line in the crontab; run factory daemon setup --install")?;
    // Only what cron gives a job, so no token or session variable of the caller reaches the
    // daemon or its pickup children.
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let logname = std::env::var("LOGNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", command])
        .env_clear()
        .env("HOME", &home)
        .env("LOGNAME", logname)
        .env("SHELL", "/bin/sh")
        .env("PATH", "/usr/bin:/bin")
        .current_dir(&home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach(&mut cmd);
    cmd.spawn().map(drop).map_err(|e| os_error(&e, None))
}

/// How many times a restart polls for the old daemon to let go of the lock, 250 ms apart: about
/// 35 s, since a stopping daemon gives a running pickup child 30 s.
const STOP_POLLS: u32 = 140;

/// Waits for the daemon that held the lock, pid, to let go of it after its kill. locked is
/// daemon::locked_pid for this home.
pub(crate) fn daemon_stopped(
    pid: u32,
    locked: &mut dyn FnMut() -> Result<Option<u32>, String>,
) -> Result<(), String> {
    for i in 0..STOP_POLLS {
        if i > 0 {
            pause(Duration::from_millis(250));
        }
        if locked()?.is_none() {
            return Ok(());
        }
    }
    let stop = kill_argv(pid).map_or_else(|| format!("pid {pid}"), |a| a.join(" "));
    Err(format!(
        "the old daemon (pid {pid}) still holds its lock 35 s after the kill; stop it with {stop}, then run catch-up again"
    ))
}

/// The pid of the daemon that holds the lock once one does, polled for about 5 s. locked is
/// daemon::locked_pid for home. A new daemon writes its pid file just after it takes the lock,
/// truncating it first, so an unreadable pid or the killed one means not yet.
pub(crate) fn daemon_pid(
    home: &Path,
    killed: Option<u32>,
    locked: &mut dyn FnMut() -> Result<Option<u32>, String>,
) -> Result<u32, String> {
    for i in 0..RUNNING_POLLS {
        if i > 0 {
            pause(Duration::from_millis(250));
        }
        if let Ok(Some(pid)) = locked() {
            if Some(pid) != killed {
                return Ok(pid);
            }
        }
    }
    Err(format!(
        "no daemon holds the lock 5 s after the restart; see {}",
        home.join("logs").join("daemon.log").display()
    ))
}

/// Ends the daemon that holds the lock, for schedulers that do not stop it themselves.
pub(super) fn stop_daemon(home: &Path) -> Option<String> {
    let argv = match locked_kill(home) {
        Ok(None) => return None,
        Ok(Some(argv)) => argv,
        Err(e) => return Some(format!("{e}; stop it by hand")),
    };
    let pid = argv[if WINDOWS { 2 } else { 1 }].clone();
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    Some(match setup_run(&args, None) {
        (0, _) => format!("stopped the running daemon (pid {pid})"),
        (_, out) => format!("could not stop the daemon (pid {pid}): {}", strip(&out)),
    })
}

/// Whether a daemon holds the lock, read after an install or uninstall has had up to 5 seconds
/// to start or stop it, so the state line does not report the moment before the change.
pub(super) fn settled_running(home: &Path, action: ServiceAction, installed: bool) -> Option<bool> {
    let want = match action {
        ServiceAction::Install if installed => Some(true),
        ServiceAction::Uninstall => Some(false),
        _ => None,
    };
    let mut running = daemon::running(home);
    if cfg!(test) || want.is_none() {
        return running;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while running != want && running.is_some() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
        running = daemon::running(home);
    }
    running
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum ServiceAction {
    Show,
    Install,
    Uninstall,
}

/// `daemon setup [--install | --uninstall]`.
pub(crate) fn daemon_setup_cmd(args: &[String]) -> Res<()> {
    let action = match args {
        [] => ServiceAction::Show,
        [flag] if flag == "--install" => ServiceAction::Install,
        [flag] if flag == "--uninstall" => ServiceAction::Uninstall,
        _ => return Err(fail(USAGE_DAEMON_SETUP, 2)),
    };
    let (stdout, stderr) = (io::stdout(), io::stderr());
    let (mut out, mut err) = (stdout.lock(), stderr.lock());
    let mut io = Io {
        out: &mut out,
        err: &mut err,
    };
    let overridden = std::env::args().any(|a| a == "--home");
    daemon_setup(&mut io, &factory_home()?, action, overridden)
}

/// The files that hold a launchd or schtasks unit, for a backup before it is removed.
pub(super) fn timer_backup(scheduler: &str, unit: &Unit) -> Vec<(String, Vec<u8>)> {
    if scheduler == "launchd" {
        let plist = timer_path(scheduler, unit);
        return fs::read(&plist)
            .map(|bytes| vec![(format!("{}.plist", unit.label), bytes)])
            .unwrap_or_default();
    }
    if scheduler == "schtasks" {
        let (code, xml) = setup_run(&["schtasks", "/Query", "/TN", unit.task, "/XML"], None);
        if code == 0 {
            return vec![(format!("{}.xml", unit.task), xml.into_bytes())];
        }
    }
    Vec::new()
}

/// Saves files into a new `<home>/backups/daemon-setup-<stamp>` folder and prints each path.
pub(super) fn backup_files(io: &mut Io, home: &Path, files: Vec<(String, Vec<u8>)>) -> Res<()> {
    if files.is_empty() {
        return Ok(());
    }
    let folder =
        crate::commands::catch_up::backup_folder(home, "daemon-setup").map_err(|e| fail(e, 1))?;
    for (name, bytes) in files {
        let path = folder.join(name);
        fs::write(&path, bytes).map_err(|e| fail(os_error(&e, Some(&path)), 1))?;
        outln!(io, "backup: {}", path.display());
    }
    Ok(())
}

/// Print the daemon service's state and how to set it up; with install or uninstall, change it
/// first. Exits 0 when the service is current (or, after uninstall, gone), else 3.
pub(super) fn daemon_setup(
    io: &mut Io,
    home: &Path,
    action: ServiceAction,
    overridden: bool,
) -> Res<()> {
    let scheduler = daemon_scheduler();
    check_unit_paths(scheduler, &timer_values(home, &DAEMON))?;
    // The daemon runs pickup, so the pickup timer goes when it is installed and comes back when it
    // is removed: the host always has exactly one scheduler. Each change happens after the other
    // scheduler is in place. A crontab is backed up before either change rewrites it.
    let mut files = Vec::new();
    if action != ServiceAction::Show && (scheduler == "cron" || pickup_scheduler() == "cron") {
        let (code, text) = setup_run(&["crontab", "-l"], None);
        if code == 0 {
            files.push(("crontab.txt".to_string(), text.into_bytes()));
        }
    }
    if action == ServiceAction::Install && pickup_scheduler() != "cron" {
        files.extend(timer_backup(pickup_scheduler(), &PICKUP));
        files.extend(timer_backup(pickup_scheduler(), &RETRO_PUSH));
    }
    backup_files(io, home, files)?;
    // The daemon pushes the retro digest too, so install removes the retro-push timer as well.
    match action {
        ServiceAction::Install => {
            outln!(io, "{}", install_timer(home, scheduler, &DAEMON)?);
            outln!(
                io,
                "{}",
                uninstall_timer(home, pickup_scheduler(), &PICKUP)?
            );
            outln!(
                io,
                "retro-push timer: {}",
                uninstall_timer(home, pickup_scheduler(), &RETRO_PUSH)?
            );
        }
        // The retro-push timer is not put back: the host may never have had one, or had its own
        // times, which the install's backup keeps.
        ServiceAction::Uninstall => {
            outln!(io, "{}", install_timer(home, pickup_scheduler(), &PICKUP)?);
            outln!(io, "{}", uninstall_timer(home, scheduler, &DAEMON)?);
            if scheduler == "cron" || scheduler == "schtasks" {
                if let Some(line) = stop_daemon(home) {
                    outln!(io, "{line}");
                }
            }
        }
        ServiceAction::Show => {}
    }
    let (installed, detail) = timer_installed(scheduler, &DAEMON);
    let state = service_state(installed, settled_running(home, action, installed));
    outln!(io, "daemon setup ({}, {scheduler}): {state}", host_os());
    outln!(io, "    {detail}");
    if let Some(stamp) = daemon::last_pass(home) {
        outln!(io, "    last pass: {stamp}");
    }
    let mut argv = vec![exe()];
    if overridden {
        argv.extend(["--home".to_string(), home.to_string_lossy().into_owned()]);
    }
    let run = shlex_join(&argv);
    let drift = if installed {
        timer_drift(scheduler, &timer_values(home, &DAEMON))
    } else {
        None
    };
    if let Some(note) = &drift {
        outln!(
            io,
            "    note: {note}. To update it, run: {run} daemon setup --install"
        );
    }
    if !installed && action != ServiceAction::Uninstall {
        for line in timer_fix(scheduler, home, &DAEMON) {
            outln!(io, "    {line}");
        }
        outln!(io, "    Or run: {run} daemon setup --install");
    }
    if installed || action != ServiceAction::Uninstall {
        outln!(
            io,
            "    log: {}",
            home.join("logs").join("daemon.log").display()
        );
    }
    let ok = if action == ServiceAction::Uninstall {
        !installed
    } else {
        installed && drift.is_none()
    };
    Err(exit(if ok { 0 } else { 3 }))
}
