use super::*;

/// A pass that launches at most one session per run: deny notes or resolve.
pub(super) struct OneLaunch<'a> {
    pub(super) noun: &'a str,
    /// The command's own name in the one-per-run line: deny-notes or resolve.
    pub(super) command_name: &'a str,
    pub(super) jobs: Vec<Value>,
    pub(super) skip: &'a dyn Fn(&mut Io, &Value, &mut Cache) -> Res<Option<String>>,
    pub(super) command: &'a dyn Fn(&Value) -> String,
    pub(super) claim: &'a dyn Fn(&str) -> Res<Option<String>>,
}

pub(super) fn one_launch_pass(
    io: &mut Io,
    home: &Path,
    device: &str,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
    pass: OneLaunch,
) -> Res<()> {
    let noun = pass.noun;
    let values = |j: &Value, session: &str, route: &Route| {
        let id = get_str(j, "id").unwrap();
        let project = get_str(j, "project").unwrap();
        let entry = Entry {
            id: id.into(),
            project: project.into(),
            repo_path: lookup(paths, project).unwrap().into(),
            session: session.into(),
        };
        Given::new(
            &entry,
            route_values(route),
            launch_prompt(&format!(
                "/factory {} {id} --session {session}",
                (pass.command)(j)
            )),
        )
    };
    let mut ready = false;
    for j in &pass.jobs {
        let id = get_str(j, "id").unwrap();
        let mut line = (pass.skip)(io, j, settings)?;
        if line.is_none() && ready {
            line = Some(format!(
                "wait {noun} {id} {}: one {} launch per run",
                text_of(j.get("project")),
                pass.command_name
            ));
        } else if line.is_none() {
            ready = true;
            let route = pick_route(home)?;
            let Some(route) = route else {
                if !dry {
                    return Ok(());
                }
                outln!(io, "{}", no_route_line(home)?);
                continue;
            };
            let template = route_launcher(home, &route)?;
            let project = get_str(j, "project").unwrap();
            if dry {
                outln!(
                    io,
                    "would launch {noun} {id} {project} on {}: {}",
                    route.name,
                    shlex_join(&launcher_argv(&template, &values(j, "<session>", &route)))
                );
                continue;
            }
            let Some(tag) = (pass.claim)(id)? else {
                return Ok(());
            };
            let given = values(j, &tag, &route);
            let argv = launcher_argv(&template, &given);
            let env = launch_env(&given, &route.provider, &route.model);
            let (ok, detail, log_path) =
                launch_idea(home, &given_entry(&given), &route.name, &argv, &env)?;
            if ok {
                outln!(
                    io,
                    "launched {noun} {id} {project} on {} session {tag}",
                    route.name
                );
            } else {
                let mut release = || release_pickup_claim(home, id, device, &tag);
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
            return Ok(());
        }
        if dry {
            outln!(io, "{}", line.unwrap());
        }
    }
    Ok(())
}

pub(super) fn given_entry(given: &Given) -> Entry {
    Entry {
        id: given.id.clone(),
        project: given.project.clone(),
        repo_path: given.repo_path.clone(),
        session: given.session.clone(),
    }
}

/// The skip line shared by the one-launch passes: no project, no checkout, a gate, a fresh claim.
pub(super) fn one_launch_skip(
    io: &mut Io,
    home: &Path,
    j: &Value,
    noun: &str,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
    pause_and_close: bool,
    problem: Option<String>,
) -> Res<Option<String>> {
    let id = get_str(j, "id").unwrap();
    let Some(project) = get_str(j, "project").filter(|p| !p.is_empty()) else {
        return Ok(Some(format!("skip {noun} {id}: no project")));
    };
    let head = format!("skip {noun} {id} {project}");
    if !has_checkout(paths, Some(project)) {
        return Ok(Some(format!("{head}: no checkout on this host")));
    }
    if gated(j) {
        return Ok(Some(format!("{head}: waiting on a human")));
    }
    if pause_and_close && paused(j) {
        return Ok(Some(format!("{head}: paused")));
    }
    if pause_and_close && closing(j) {
        return Ok(Some(format!("{head}: closing")));
    }
    if let Some(c) = claims::blocking(Some(j), device, "", minutes)? {
        return Ok(Some(format!("{head}: {}", claimed_by(&c))));
    }
    if let Some(problem) = problem {
        return Ok(Some(format!("{head}: {problem}")));
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
    Ok(settings_reason(&s, device).map(|r| format!("{head}: {r}")))
}

/// Launch at most one session for a denied job whose notes nobody has read yet.
#[allow(clippy::too_many_arguments)]
pub(super) fn deny_notes_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<()> {
    let jobs: Vec<Value> = jobs_of(data)
        .iter()
        .filter(|j| deny_notes_due(j) && safe_id(j).is_some())
        .cloned()
        .collect();
    let skip = |io: &mut Io, j: &Value, settings: &mut Cache| {
        let problem = deny_notes_problem(j).map(|p| format!("deny notes {p}"));
        one_launch_skip(
            io,
            home,
            j,
            "deny notes",
            device,
            minutes,
            paths,
            settings,
            dry,
            false,
            problem,
        )
    };
    let claim = |id: &str| claim_deny_notes(home, id, device, minutes, paths);
    one_launch_pass(
        io,
        home,
        device,
        paths,
        settings,
        dry,
        OneLaunch {
            noun: "deny notes",
            command_name: "deny-notes",
            jobs,
            skip: &skip,
            command: &|_| "deny-notes".into(),
            claim: &claim,
        },
    )
}

/// True when a needs-human job carries a dashboard Rework or Approve as is press.
pub(super) fn resolve_due(j: &Value) -> bool {
    get_str(j, "stage") == Some("needs-human") && dated(j, "resolve") && safe_id(j).is_some()
}

/// Why a due job's resolve may not reach a session, or None.
pub(super) fn resolve_problem(j: &Value) -> Option<String> {
    let r = &j["resolve"];
    let action = get_str(r, "action").filter(|a| ["rework", "approve"].contains(a));
    let Some(action) = action else {
        return Some("action must be rework or approve".into());
    };
    let note_ok =
        get_str(r, "note").is_some_and(|n| (1..=2000).contains(&strip(n).chars().count()));
    (action == "rework" && !note_ok).then(|| "rework note must be 1 to 2000 characters".into())
}

/// Launch at most one /factory rework or approve session for a dashboard Rework or Approve as is press.
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_pass(
    io: &mut Io,
    home: &Path,
    data: &mut Value,
    device: &str,
    minutes: u32,
    paths: &[(String, String)],
    settings: &mut Cache,
    dry: bool,
) -> Res<()> {
    let jobs: Vec<Value> = jobs_of(data)
        .iter()
        .filter(|j| resolve_due(j))
        .cloned()
        .collect();
    let skip = |io: &mut Io, j: &Value, settings: &mut Cache| {
        one_launch_skip(
            io,
            home,
            j,
            "resolve",
            device,
            minutes,
            paths,
            settings,
            dry,
            true,
            resolve_problem(j),
        )
    };
    let claim = |id: &str| {
        claim_for(home, id, device, minutes, paths, &|j| {
            resolve_due(j) && resolve_problem(j).is_none()
        })
    };
    let command = |j: &Value| get_str(&j["resolve"], "action").unwrap().to_string();
    one_launch_pass(
        io,
        home,
        device,
        paths,
        settings,
        dry,
        OneLaunch {
            noun: "resolve",
            command_name: "resolve",
            jobs,
            skip: &skip,
            command: &command,
            claim: &claim,
        },
    )
}
