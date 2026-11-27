//! Setup commands: init, adopt-home, doctor, host, with-cloudflare and explore.

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use crate::cli::Call;
use crate::commands::lookup::{main_checkout, remotes};
use crate::commands::{catch_up, pickup};
use crate::shared::board::read_board;
use crate::shared::conf::{
    closed_frontmatter, fm_get, frontmatter, read_conf, read_env_ordered, unquote,
};
use crate::shared::consts::{REVIEW_ROUTE_KEYS, ROUTE_EFFORTS, ROUTE_MODEL, SLUG};
use crate::shared::exit::{exit, fail, Exit, Res};
use crate::shared::fs::{
    contained, is_link, read_text, regular_metadata, resolve, same_path, seed, write_atomic,
};
use crate::shared::home::{
    config_root, factory_home, holder, read_location, skill_root, validate_home,
};
use crate::shared::http::{
    access_message, api, endpoint_message, normalized_origin, token_message,
};
use crate::shared::proc::{git, git_ok, origin_git};
use crate::shared::project::{
    local_bytes, project_files, pull_project, read_project_state, save_project_state,
};
use crate::shared::py::{is_space, lstrip, os_error, repr, splitlines, strip};
use crate::shared::pyjson::{dumps, loads, DEFAULT, INDENT2};
use crate::shared::pyre::{Flags, Pattern};
use crate::shared::time::{format_stamp, now, parse_stamp};

static REPO_PATH: Pattern = Pattern::new(r"[A-Za-z0-9_./-]{1,200}", Flags::ASCII);
static RUN_TAG: Pattern = Pattern::new(r"[A-Za-z0-9_-]{1,64}", Flags::ASCII);
static HOST_NAME: Pattern = Pattern::new(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}", Flags::ASCII);
static WORKER_MODEL: Pattern = Pattern::new(r"[A-Za-z0-9._:@/\[\]-]{1,128}", Flags::ASCII);
static PAIRING_CODE: Pattern = Pattern::new(r"[A-Za-z0-9]{5}-?[A-Za-z0-9]{5}", Flags::ASCII);

const PAIRING_FAILED: &str = "pairing code is wrong, used or expired";
const SCHEDULERS: &[&str] = &["launchd", "systemd", "cron", "schtasks", "none"];
const MAX_EXPLORER_PROJECTS: usize = 1;

const ROLE_NAMES: [&str; 9] = [
    "factory-approver.md",
    "factory-builder.md",
    "factory-explorer.md",
    "factory-review-code.md",
    "factory-review-security.md",
    "factory-review-ui.md",
    "factory-review-ux.md",
    "factory-spec-writer.md",
    "factory-retro.md",
];

const CONFIG_TEMPLATE: &str = include_str!("../../../../skills/factory/templates/config.md");
const PROJECT_TEMPLATE: &str = include_str!("../../../../skills/factory/templates/project.md");

const ROLES: [(&str, &str); 9] = [
    (
        ROLE_NAMES[0],
        include_str!("../../../../skills/factory/agents/factory-approver.md"),
    ),
    (
        ROLE_NAMES[1],
        include_str!("../../../../skills/factory/agents/factory-builder.md"),
    ),
    (
        ROLE_NAMES[2],
        include_str!("../../../../skills/factory/agents/factory-explorer.md"),
    ),
    (
        ROLE_NAMES[3],
        include_str!("../../../../skills/factory/agents/factory-review-code.md"),
    ),
    (
        ROLE_NAMES[4],
        include_str!("../../../../skills/factory/agents/factory-review-security.md"),
    ),
    (
        ROLE_NAMES[5],
        include_str!("../../../../skills/factory/agents/factory-review-ui.md"),
    ),
    (
        ROLE_NAMES[6],
        include_str!("../../../../skills/factory/agents/factory-review-ux.md"),
    ),
    (
        ROLE_NAMES[7],
        include_str!("../../../../skills/factory/agents/factory-spec-writer.md"),
    ),
    (
        ROLE_NAMES[8],
        include_str!("../../../../skills/factory/agents/factory-retro.md"),
    ),
];

fn os_fail(e: io::Error, p: &Path) -> Exit {
    fail(os_error(&e, Some(p)), 1)
}

/// `open(p, encoding="utf-8", newline="").read()`: strict UTF-8, line endings kept.
fn read_raw(p: &Path) -> io::Result<String> {
    String::from_utf8(fs::read(p)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A list of pairs as a dict builds it: first position, last value.
fn dict(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (key, value) in pairs {
        match out.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => out.push((key, value)),
        }
    }
    out
}

/// `read_paths` as Python runs it: a paths.md that exists but cannot be read or decoded fails 1.
fn read_paths_strict(home: &Path) -> Res<Vec<(String, String)>> {
    let path = home.join("paths.md");
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(frontmatter(&path).map_err(|e| os_fail(e, &path))?.0)
}

/// `frontmatter` on text already read.
fn frontmatter_text(text: &str) -> (Vec<(String, String)>, String) {
    let lines = splitlines(text, true);
    if lines.first().map(|l| strip(l)) != Some("---") {
        return (Vec::new(), text.to_string());
    }
    let mut meta = Vec::new();
    for (i, line) in lines.iter().enumerate().skip(1) {
        let line = strip(line);
        if line == "---" {
            return (meta, lines[i + 1..].concat());
        }
        if !line.is_empty() && !line.starts_with('#') {
            if let Some((key, value)) = line.split_once(':') {
                meta.push((strip(key).to_string(), unquote(strip(value)).to_string()));
            }
        }
    }
    (Vec::new(), text.to_string())
}

/// `os.path.abspath` against the cwd, on this platform's separators and drives.
fn abspath(p: &str) -> PathBuf {
    crate::shared::fs::abspath(p, &std::env::current_dir().unwrap_or_default())
}

fn load_connection(path: &Path) -> Res<String> {
    let invalid = || {
        fail(
            format!("{}: invalid connection descriptor", path.display()),
            2,
        )
    };
    regular_metadata(path).map_err(|()| invalid())?;
    let text = read_text(path).map_err(|_| invalid())?;
    let data = loads(&text).map_err(|_| invalid())?;
    let map = data.as_object().ok_or_else(invalid)?;
    if map.len() != 2
        || !map.contains_key("api_url")
        || map.get("version").and_then(Value::as_i64) != Some(1)
    {
        return Err(invalid());
    }
    let url = map["api_url"].as_str().ok_or_else(invalid)?;
    normalized_origin(url, true).map_err(|()| invalid())
}

/// config.md text with `api_url: endpoint` set in its frontmatter, or None when the block never closes.
fn set_api_url(text: &str, endpoint: &str) -> Option<String> {
    let mut lines: Vec<String> = splitlines(text, true)
        .into_iter()
        .map(String::from)
        .collect();
    let end = (1..lines.len()).find(|&i| lines[i].trim_end_matches(['\r', '\n']) == "---")?;
    let newline = if lines[0].ends_with("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let line = format!("api_url: {endpoint}{newline}");
    match (1..end).find(|&i| lines[i].split(':').next() == Some("api_url")) {
        Some(i) => lines[i] = line,
        None => lines.insert(end, line),
    }
    Some(lines.concat())
}

/// The verbatim `---` header of a role file, or None when it never closes or `...` ends it.
fn native_header(text: &str) -> Option<&str> {
    let lines = splitlines(text, true);
    if lines.first()?.trim_end_matches(['\r', '\n']) != "---" {
        return None;
    }
    let mut len = lines[0].len();
    for line in &lines[1..] {
        len += line.len();
        match line.trim_end_matches(['\r', '\n']) {
            "---" => return Some(&text[..len]),
            "..." => return None,
            _ => {}
        }
    }
    None
}

fn preflight(current: &Path) -> Exit {
    fail(
        format!(
            "{}: cannot preflight native role; check paths and frontmatter delimiters",
            current.display()
        ),
        2,
    )
}

/// Each runtime's native agents folder, checked: absolute, no link or file on the way, and
/// overlapping neither the skill nor another runtime's folder.
fn agent_dirs(home: &Path, runtimes: &[&str]) -> Res<Vec<(String, PathBuf)>> {
    let config = home.join("config.md");
    let meta = if config.exists() {
        frontmatter(&config).map_err(|e| os_fail(e, &config))?.0
    } else {
        Vec::new()
    };
    let source_root = skill_root()?;
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    for runtime in runtimes {
        let default = if *runtime == "claude" {
            let user = std::env::home_dir().unwrap_or_default();
            resolve(&user)
                .map_err(|_| preflight(&user))?
                .join(".claude/agents")
        } else {
            config_root()?.join("opencode/agents")
        };
        let directory = match fm_get(&meta, &format!("{runtime}_agents_dir")) {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => default,
        };
        if !directory.is_absolute() {
            return Err(preflight(&directory));
        }
        for parent in directory.ancestors() {
            if is_link(parent).map_err(|_| preflight(&directory))?
                || parent.exists() && !parent.is_dir()
            {
                return Err(preflight(&directory));
            }
        }
        let resolved = resolve(&directory).map_err(|_| preflight(&directory))?;
        let overlaps = |other: &Path| contained(other, &resolved) || contained(&resolved, other);
        if overlaps(&source_root) || dirs.iter().any(|(_, d)| overlaps(d)) {
            return Err(preflight(&directory));
        }
        if resolved.exists() {
            for entry in fs::read_dir(&resolved).map_err(|_| preflight(&directory))? {
                entry.map_err(|_| preflight(&directory))?;
            }
        }
        dirs.push((runtime.to_string(), resolved));
    }
    Ok(dirs)
}

/// Refresh every role body in each folder, keeping an existing file's native header. Every
/// file is checked before any is written. Returns how many files were written.
fn install_roles(dirs: &[(String, PathBuf)]) -> Res<usize> {
    let mut sources = Vec::new();
    for (name, text) in ROLES {
        let header = native_header(text).ok_or_else(|| preflight(Path::new(name)))?;
        let description = fm_get(&frontmatter_text(text).0, "description")
            .ok_or_else(|| preflight(Path::new(name)))?
            .to_string();
        sources.push((name, header, &text[header.len()..], description));
    }
    let mut plans: Vec<(PathBuf, String)> = Vec::new();
    for (runtime, directory) in dirs {
        for (name, source_header, body, description) in &sources {
            let current = directory.join(name);
            let header = if fs::symlink_metadata(&current).is_ok() {
                regular_metadata(&current).map_err(|()| preflight(&current))?;
                let text = read_raw(&current).map_err(|_| preflight(&current))?;
                native_header(&text)
                    .ok_or_else(|| preflight(&current))?
                    .to_string()
            } else if runtime == "claude" {
                source_header.to_string()
            } else {
                format!(
                    "---\ndescription: {}\nmode: \"all\"\n---\n",
                    dumps(&Value::from(description.as_str()), DEFAULT)
                )
            };
            let separator = if header.ends_with('\n') { "" } else { "\n" };
            plans.push((current, format!("{header}{separator}{body}")));
        }
    }
    for (destination, text) in &plans {
        let parent = destination.parent().unwrap_or(Path::new("/"));
        fs::create_dir_all(parent)
            .and_then(|()| write_atomic(destination, text.as_bytes()))
            .map_err(|_| {
                fail(
                    format!("{}: cannot refresh native role body", destination.display()),
                    2,
                )
            })?;
    }
    Ok(plans.len())
}

/// True when v is not a relative repo path of `[A-Za-z0-9_./-]` with no `..` segment.
fn check_path(v: &str) -> bool {
    !REPO_PATH.is_full(v) || v.starts_with('/') || v.split('/').any(|s| s == "..")
}

/// A project file minus its frontmatter `path:` lines, since each device keeps its own.
fn without_path(text: &str) -> String {
    let lines = splitlines(text, true);
    if lines.first().map(|l| strip(l)) != Some("---") {
        return text.to_string();
    }
    for (i, line) in lines.iter().enumerate().skip(1) {
        if strip(line) == "---" {
            let head = lines[1..i].iter().filter(|l| {
                !lstrip(l)
                    .strip_prefix("path")
                    .is_some_and(|r| lstrip(r).starts_with(':'))
            });
            return std::iter::once(&lines[0])
                .chain(head)
                .chain(&lines[i..])
                .copied()
                .collect();
        }
    }
    text.to_string()
}

/// The rest of `init` once the home is configured: register the checkout at repo_path.
fn register_project(home: &Path, repo_path: &str) -> Res<()> {
    let start = abspath(repo_path);
    let Some(main) = main_checkout(&start) else {
        println!(
            "{} is not a git repo, so no project was registered.",
            start.display()
        );
        return Ok(());
    };
    let repo = remotes(&main).into_iter().next().unwrap_or_default();
    let name = if repo.is_empty() {
        main.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        repo.rsplit('/').next().unwrap_or_default().to_string()
    };
    // The Worker and claim only accept word characters and hyphens.
    let (mut slug, mut replaced) = (String::new(), false);
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            slug.push(c);
            replaced = false;
        } else if !replaced {
            slug.push('-');
            replaced = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    let (meta, body) = frontmatter_text(PROJECT_TEMPLATE);
    let mut meta = dict(meta);
    meta.retain(|(k, _)| k != "path");
    meta.extend([
        ("repo".to_string(), repo.clone()),
        ("base".to_string(), "main".to_string()),
        ("title".to_string(), name.clone()),
    ]);
    let meta = dict(meta);
    let comments: String = PROJECT_TEMPLATE
        .split("---\n")
        .nth(1)
        .map(|block| {
            splitlines(block, false)
                .into_iter()
                .filter(|l| l.starts_with('#'))
                .map(|l| format!("{l}\n"))
                .collect()
        })
        .unwrap_or_default();
    let (project, state, remote_file) = project_files(home, &slug)?;
    read_project_state(&state)?;
    let fields: String = meta.iter().map(|(k, v)| format!("{k}: {v}\n")).collect();
    let text = format!("---\n{fields}{comments}---\n{body}");
    let original = local_bytes(&project);
    let route = format!("/api/projects/{slug}.md");
    let status = api(home, "GET", &route, None, &[200, 404], &[])?.status;
    if status == 200 {
        if pull_project(home, &slug)? {
            return Err(fail(
                format!("Project has local edits. Push {slug} before continuing."),
                6,
            ));
        }
        println!(
            "project file registered on another device, cached at {}",
            project.display()
        );
    } else {
        if remote_file.exists() {
            return Err(fail("Project has an unresolved .remote file. Merge it into the project document and delete the .remote file before pushing.", 6));
        }
        let register =
            |e: &dyn std::fmt::Display| fail(format!("cannot register local project: {e}"), 6);
        let source = match &original {
            Some(bytes) => String::from_utf8(bytes.clone()).map_err(|e| register(&e))?,
            None => text,
        };
        let data = without_path(&source).into_bytes();
        if local_bytes(&project) != original {
            return Err(fail(
                "Project changed locally during registration. Local edits were kept.",
                6,
            ));
        }
        let write = |data: &[u8]| {
            write_atomic(&project, data).map_err(|e| register(&os_error(&e, Some(&project))))
        };
        if original.is_none() {
            write(&data)?;
        }
        let put = api(
            home,
            "PUT",
            &route,
            Some(&data),
            &[200, 412],
            &[
                ("If-None-Match", "*"),
                ("Content-Type", "text/markdown; charset=utf-8"),
            ],
        )?;
        if put.status == 412 {
            return Err(fail(format!("Project changed in R2. Local edits were kept. Run pull {slug} to compare and merge before pushing."), 6));
        }
        save_project_state(&state, put.header("ETag"), &data)?;
        if original.is_some()
            && local_bytes(&project) == original
            && original.as_deref() != Some(&data[..])
        {
            write(&data)?;
        }
        println!("project file: {}", project.display());
    }
    let mut paths = dict(read_paths_strict(home)?);
    paths.push((slug.clone(), main.display().to_string()));
    let paths = dict(paths);
    let listed: String = paths.iter().map(|(k, v)| format!("{k}: {v}\n")).collect();
    let paths_file = home.join("paths.md");
    write_atomic(
        &paths_file,
        format!(
            "---\n{listed}---\n\nThis device's checkout of each project. factory init writes it.\n"
        )
        .as_bytes(),
    )
    .map_err(|e| os_fail(e, &paths_file))?;
    println!(
        "checkout path: {slug}: {} in {}",
        main.display(),
        paths_file.display()
    );
    Ok(())
}

pub fn init(call: &Call) -> Res<()> {
    let repo_path = call.args.first().map_or(".", String::as_str);
    let agents = call.agents.as_deref().unwrap_or("none");
    let home = factory_home()?;
    let config = home.join("config.md");
    let endpoint = match &call.connection {
        Some(connection) => Some(load_connection(Path::new(connection))?),
        None => None,
    };
    let mut existing = String::new();
    if fs::symlink_metadata(&config).is_ok() {
        let meta = closed_frontmatter(&config)
            .map_err(|()| fail("Invalid local metadata: config.md.", 2))?;
        existing = fm_get(&meta, "api_url").unwrap_or("").to_string();
    }
    if !existing.is_empty() {
        let current =
            normalized_origin(&existing, false).map_err(|()| fail(endpoint_message(&config), 2))?;
        if endpoint.as_ref().is_some_and(|e| *e != current) {
            return Err(fail(
                format!(
                    "Connection api_url disagrees with {}; no files were changed.",
                    config.display()
                ),
                2,
            ));
        }
    }
    fs::create_dir_all(&home).map_err(|e| os_fail(e, &home))?;
    seed(&config, CONFIG_TEMPLATE.as_bytes(), 0o644).map_err(|e| os_fail(e, &config))?;
    if let (Some(endpoint), true) = (&endpoint, existing.is_empty()) {
        let text = read_raw(&config).map_err(|e| os_fail(e, &config))?;
        let updated = set_api_url(&text, endpoint).ok_or_else(|| {
            fail(
                format!("{}: the frontmatter never closes", config.display()),
                1,
            )
        })?;
        let mode = fs::metadata(&config)
            .map_err(|e| os_fail(e, &config))?
            .permissions();
        write_atomic(&config, updated.as_bytes()).map_err(|e| os_fail(e, &config))?;
        #[cfg(unix)]
        fs::set_permissions(&config, mode).map_err(|e| os_fail(e, &config))?;
        #[cfg(not(unix))]
        let _ = mode;
    }
    // The Access service token goes here and nowhere else, so keep it private to this user.
    let env_file = home.join(".env");
    seed(
        &env_file,
        b"FACTORY_ACCESS_CLIENT_ID=\nFACTORY_ACCESS_CLIENT_SECRET=\n",
        0o600,
    )
    .map_err(|e| os_fail(e, &env_file))?;
    if agents != "none" {
        let runtimes: &[&str] = match agents {
            "both" => &["claude", "opencode"],
            "claude" => &["claude"],
            _ => &["opencode"],
        };
        let count = install_roles(&agent_dirs(&home, runtimes)?)?;
        println!("factory agents: {count} role prompt bodies refreshed; existing native headers kept verbatim");
    }
    println!("factory home: {}", home.display());
    let env = read_conf(&env_file).map_err(|e| os_fail(e, &env_file))?;
    let meta = frontmatter(&config).map_err(|e| os_fail(e, &config))?.0;
    let endpoint = fm_get(&meta, "api_url").unwrap_or("");
    if endpoint.is_empty() {
        println!(
            "Set api_url in {}, then run /factory init again.",
            config.display()
        );
    }
    let set = |key: &str| env.get(key).is_some_and(|v| !v.is_empty());
    if !(set("FACTORY_ACCESS_CLIENT_ID") && set("FACTORY_ACCESS_CLIENT_SECRET")) {
        println!("{}", token_message(&env_file));
        return Ok(());
    }
    if endpoint.is_empty() {
        return Ok(());
    }
    register_project(&home, repo_path)
}

/// Point the location file at an existing, valid home. A second run with the same home changes nothing.
fn adopt_home(existing: &Path, pointer: &Path) -> Res<()> {
    if !existing.is_absolute() {
        return Err(fail(
            format!(
                "{}: adoption requires an absolute directory",
                existing.display()
            ),
            2,
        ));
    }
    let atomic = || {
        fail(
            format!(
                "{}: cannot publish factory location atomically",
                pointer.display()
            ),
            2,
        )
    };
    let home = resolve(existing).map_err(|_| atomic())?;
    validate_home(&home, true)?;
    let folder = pointer.parent().unwrap_or(Path::new("/"));
    let default = folder.join("factory");
    if fs::symlink_metadata(default.join("config.md")).is_ok() && !same_path(&default, &home) {
        return Err(fail(
            format!(
                "{}: a different initialized factory home exists",
                default.display()
            ),
            2,
        ));
    }
    if fs::symlink_metadata(pointer).is_ok() {
        if read_location(pointer)? != home {
            return Err(fail(
                format!(
                    "{}: a different factory home is selected",
                    pointer.display()
                ),
                2,
            ));
        }
        return Ok(());
    }
    fs::create_dir_all(folder).map_err(|_| atomic())?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let (temporary, mut file) = loop {
        let token = RandomState::new().build_hasher().finish() as u32;
        let temporary = folder.join(format!(".factory-location-{token:08x}"));
        match options.open(&temporary) {
            Ok(file) => break (temporary, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(atomic()),
        }
    };
    let text = dumps(
        &json!({"version": 1, "home": home.display().to_string()}),
        DEFAULT,
    ) + "\n";
    let mut published = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if published.is_ok() {
        published = fs::hard_link(&temporary, pointer);
    }
    let removed = fs::remove_file(&temporary);
    match published {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if read_location(pointer)? != home {
                return Err(fail(
                    format!(
                        "{}: a different factory home won publication",
                        pointer.display()
                    ),
                    2,
                ));
            }
        }
        Err(_) => return Err(atomic()),
        Ok(()) => {}
    }
    removed.map_err(|_| atomic())
}

pub fn adopt_home_cmd(call: &Call) -> Res<()> {
    let existing = Path::new(&call.args[0]);
    adopt_home(existing, &config_root()?.join("factory-location.json"))?;
    let home = resolve(existing).map_err(|e| fail(os_error(&e, None), 1))?;
    println!("{}", home.display());
    Ok(())
}

/// The five-check report: location, config, credentials, then one GET each of projects and board.
fn doctor(home: &Path) -> Value {
    let home = resolve(home).unwrap_or_else(|_| home.to_path_buf());
    let mut checks: Vec<Value> = Vec::new();
    let mut check = |name: &str, valid: bool, message: &str| {
        checks.push(json!({
            "name": name,
            "status": if valid { "pass" } else { "fail" },
            "message": message,
        }));
    };
    check("location", true, "Location resolved.");
    let mut origin = String::new();
    let mut config_error = match validate_home(&home, true) {
        Ok(()) => String::new(),
        Err(e) => strip(&e.stderr).to_string(),
    };
    if config_error.is_empty() {
        let config = home.join("config.md");
        let meta = frontmatter(&config).map(|(m, _)| m).unwrap_or_default();
        let endpoint = fm_get(&meta, "api_url").unwrap_or("");
        if endpoint.is_empty() {
            config_error = format!(
                "Set api_url in {}, then run /factory init again.",
                config.display()
            );
        } else {
            match normalized_origin(endpoint, false) {
                Ok(url) => origin = url,
                Err(()) => config_error = endpoint_message(&config),
            }
        }
    }
    let valid = if config_error.is_empty() {
        "Local configuration is valid."
    } else {
        &config_error
    };
    check("config", config_error.is_empty(), valid);
    let env_file = home.join(".env");
    let configured = regular_metadata(&env_file).is_ok()
        && read_conf(&env_file).is_ok_and(|env| {
            ["FACTORY_ACCESS_CLIENT_ID", "FACTORY_ACCESS_CLIENT_SECRET"]
                .iter()
                .all(|k| env.get(*k).is_some_and(|v| !v.is_empty()))
        });
    let message = if configured {
        "Service token is configured.".to_string()
    } else {
        token_message(&env_file)
    };
    check("credentials", configured, &message);
    for (name, route) in [("projects", "/projects.json"), ("board", "/api/board")] {
        if !config_error.is_empty() || !configured {
            check(
                name,
                false,
                "Not attempted because local configuration is invalid.",
            );
            continue;
        }
        let ok: &[u16] = if name == "board" { &[200, 404] } else { &[200] };
        let read = api(&home, "GET", route, None, ok, &[]).ok().and_then(|r| {
            if name == "board" && r.status == 404 {
                return Some("Board is absent; no board was created.");
            }
            let value = loads(&r.text().ok()?).ok()?;
            let valid = if name == "projects" {
                value.is_array()
            } else {
                value.get("jobs").is_some_and(Value::is_array)
            };
            valid.then_some(if name == "projects" {
                "Projects are reachable."
            } else {
                "Board is reachable."
            })
        });
        match read {
            Some(message) => check(name, true, message),
            None => check(
                name,
                false,
                &format!("Cannot read {route}; check connectivity, Access policy and service-token configuration."),
            ),
        }
    }
    json!({
        "version": 1,
        "home": home.display().to_string(),
        "api_url": origin,
        "checks": checks,
    })
}

pub fn doctor_cmd(_call: &Call) -> Res<()> {
    let Ok(home) = factory_home() else {
        let report = json!({"version": 1, "home": "", "api_url": "", "checks": [
            {"name": "location", "status": "fail", "message": "Invalid factory location."}]});
        println!("{}", dumps(&report, DEFAULT));
        return Err(exit(2));
    };
    let report = doctor(&home);
    let checks = report["checks"].as_array().cloned().unwrap_or_default();
    let failed = |checks: &[Value]| checks.iter().any(|c| c["status"] == "fail");
    if !failed(&checks[..3]) {
        report_status(&home, cached_status(&home), Some(&report));
    }
    println!("{}", dumps(&report, DEFAULT));
    let stderr = io::stderr();
    for line in catch_up::stale_prompts(&home) {
        let _ = writeln!(stderr.lock(), "doctor: {line}");
    }
    daemon_line(&mut stderr.lock(), stderr.is_terminal(), || {
        pickup::daemon_state(&home)
    });
    clock_line(
        &mut stderr.lock(),
        stderr.is_terminal(),
        std::env::var("FACTORY_PARITY_NOW")
            .ok()
            .filter(|v| v.parse::<i64>().is_ok()),
    );
    if failed(&checks[..3]) {
        return Err(exit(2));
    }
    if failed(&checks) {
        return Err(exit(5));
    }
    Ok(())
}

/// `daemon: <running|stopped|installed|not installed>`, and for an installed daemon that it is the
/// pickup scheduler and when its last pass ran, for a person at a terminal. factory.py has no
/// daemon and its doctor prints only the JSON report, so the line stays out of the report and off
/// pipes, where agents and the parity runs read doctor's output.
fn daemon_line(err: &mut dyn Write, terminal: bool, state: impl FnOnce() -> String) {
    if terminal {
        let _ = writeln!(err, "daemon: {}", state());
    }
}

/// A note, for a person at a terminal, that an exported FACTORY_PARITY_NOW pins this host's
/// clock. Like the daemon line it stays off pipes, so the parity runs, which set it, see nothing.
fn clock_line(err: &mut dyn Write, terminal: bool, pinned: Option<String>) {
    if let (true, Some(secs)) = (terminal, pinned) {
        let _ = writeln!(
            err,
            "clock: FACTORY_PARITY_NOW={secs} pins the time factory reads; unset it outside the parity harness"
        );
    }
}

/// True when s is a UTC stamp `strptime` reads and `strftime` writes back unchanged.
fn utc(s: &str) -> bool {
    parse_stamp(s).map(format_stamp).as_deref() == Some(s)
}

/// (scheduler, last_pickup, triage_failed_since) from the last status pickup sent, else ("none", None, None).
fn cached_status(home: &Path) -> (String, Option<String>, Option<String>) {
    let cache = read_text(&home.join("host-status.json"))
        .ok()
        .and_then(|text| loads(&text).ok());
    if let Some(cache) = cache.as_ref().and_then(Value::as_object) {
        if let (Some(scheduler), Some(last)) = (cache.get("scheduler"), cache.get("last_pickup")) {
            let scheduler = scheduler.as_str().filter(|s| SCHEDULERS.contains(s));
            if let Some(scheduler) =
                scheduler.filter(|_| last.is_null() || last.as_str().is_some_and(utc))
            {
                let failing = cache
                    .get("triage_failed_since")
                    .and_then(Value::as_str)
                    .filter(|f| utc(f));
                return (
                    scheduler.to_string(),
                    last.as_str().map(String::from),
                    failing.map(String::from),
                );
            }
        }
    }
    ("none".to_string(), None, None)
}

/// (provider, model, effort) from `<provider> <model> [<effort>]`, or None when it is malformed.
fn parse_route(value: &str) -> Option<(String, String, Option<String>)> {
    let parts: Vec<&str> = value.split(is_space).filter(|p| !p.is_empty()).collect();
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

/// [(name, provider, model, effort)] from this host's routes.md, in preference order.
fn read_routes(home: &Path) -> Res<Vec<(String, String, String, Option<String>)>> {
    let path = home.join("routes.md");
    let meta = if path.exists() {
        dict(frontmatter(&path).map_err(|e| os_fail(e, &path))?.0)
    } else {
        Vec::new()
    };
    if meta.is_empty() {
        return Err(fail(format!("no routes in {}: list this host's dispatch routes, primary first, as <name>: <provider> <model> [<effort>]", path.display()), 2));
    }
    let mut routes = Vec::new();
    for (name, value) in meta {
        // The optional experiment and review lines are config, not routes.
        if name == "experiment" || REVIEW_ROUTE_KEYS.contains(&name.as_str()) {
            continue;
        }
        match parse_route(&value).filter(|_| SLUG.is_full(&name)) {
            Some((provider, model, effort)) => routes.push((name, provider, model, effort)),
            None => {
                return Err(fail(
                    format!(
                        "bad route {} in {}: use <name>: <provider> <model> [<effort>]",
                        repr(&name),
                        path.display()
                    ),
                    2,
                ))
            }
        }
    }
    Ok(routes)
}

/// {route name: limited until} from this host's route-limits.json.
fn read_limits(home: &Path) -> Res<Vec<(String, String)>> {
    let path = home.join("route-limits.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    // A decode error is a ValueError in Python, so it fails 2 like bad JSON; any other read error fails 1.
    let text = match read_text(&path) {
        Err(e) if e.kind() != io::ErrorKind::InvalidData => return Err(os_fail(e, &path)),
        read => read.ok(),
    };
    let limits = text.and_then(|text| {
        let value = loads(&text).ok()?;
        value
            .as_object()?
            .iter()
            .map(|(k, v)| {
                let until = v.as_str()?;
                parse_stamp(until)?;
                Some((k.clone(), until.to_string()))
            })
            .collect::<Option<Vec<_>>>()
    });
    limits.ok_or_else(|| {
        fail(
            format!(
                "cannot read {}: delete it to clear every route limit",
                path.display()
            ),
            2,
        )
    })
}

fn host_os() -> &'static str {
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

/// bundle.json's text as {commit, tree}, or None when it has any other shape. A Rust build adds
/// entry and binary_sha256 beside them.
fn stamp_shape(text: &str) -> Option<Value> {
    stamp_of(&loads(text).ok()?)
}

/// A parsed bundle.json as {commit, tree}: commit and tree 7 to 64 lowercase hex, plus an optional
/// entry (list of strings) and binary_sha256 (64 hex), and no other key.
pub(crate) fn stamp_of(stamp: &Value) -> Option<Value> {
    let map = stamp.as_object()?;
    let hex = |v: Option<&Value>, sizes: std::ops::RangeInclusive<usize>| {
        v.and_then(Value::as_str).is_some_and(|s| {
            sizes.contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
    };
    (map.keys()
        .all(|k| ["commit", "tree", "entry", "binary_sha256"].contains(&k.as_str()))
        && hex(map.get("commit"), 7..=64)
        && hex(map.get("tree"), 7..=64)
        && map
            .get("entry")
            .is_none_or(|e| e.as_array().is_some_and(|a| a.iter().all(Value::is_string)))
        && map
            .get("binary_sha256")
            .is_none_or(|v| hex(Some(v), 64..=64)))
    .then(|| json!({"commit": map["commit"], "tree": map["tree"]}))
}

/// {commit, tree} from the bundle.json `factory install` writes beside the skill, or None.
fn bundle_stamp() -> Option<Value> {
    stamp_shape(&read_text(&skill_root().ok()?.join("bundle.json")).ok()?)
}

fn checkout_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// When the first merge on origin's base that commit lacks landed, from the first checkout
/// holding commit. None when commit is current or no checkout holds it.
fn bundle_behind_since(home: &Path, commit: &str) -> Res<Option<String>> {
    let mut paths = dict(read_paths_strict(home)?);
    paths.sort();
    for (slug, repo) in paths {
        let repo = Path::new(&repo);
        if !checkout_slug(&slug) || !repo.is_dir() {
            continue;
        }
        let Ok((project, _, _)) = project_files(home, &slug) else {
            continue;
        };
        let meta = if project.exists() {
            frontmatter(&project).map_err(|e| os_fail(e, &project))?.0
        } else {
            Vec::new()
        };
        let base = fm_get(&meta, "base").unwrap_or("main");
        if check_path(base) || base.starts_with('-') {
            continue;
        }
        let origin = format!("refs/remotes/origin/{base}");
        if git(repo, &["merge-base", commit, &origin]) != commit {
            continue;
        }
        let times = git(
            repo,
            &[
                "log",
                "--first-parent",
                "--format=%ct",
                &format!("{commit}..{origin}"),
            ],
        );
        return Ok(times
            .split_whitespace()
            .last()
            .and_then(|t| t.parse().ok())
            .map(format_stamp));
    }
    Ok(None)
}

/// The status body the Worker's PUT /api/hosts/<id>/status accepts. Holds no path or secret.
/// None when a local file it reads cannot be read or decoded, where Python raises.
fn host_status(
    home: &Path,
    report: &Value,
    scheduler: &str,
    last: Option<&str>,
    triage: Option<&str>,
) -> Option<Value> {
    let failed: Vec<Value> = report["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["status"] != "pass")
        .map(|c| c["name"].clone())
        .collect();
    // Python catches only fail's SystemExit (exit 2) here; a read error (exit 1) stops the status.
    let (routes, limits) = match read_routes(home).and_then(|r| Ok((r, read_limits(home)?))) {
        Ok(read) => read,
        Err(e) if e.code == 2 => (Vec::new(), Vec::new()),
        Err(_) => return None,
    };
    let stamp = now();
    let routes: Vec<Value> = routes
        .iter()
        .filter(|(name, provider, model, _)| {
            HOST_NAME.is_full(name) && HOST_NAME.is_full(provider) && WORKER_MODEL.is_full(model)
        })
        .take(16)
        .map(|(name, provider, model, effort)| {
            let until = fm_get(&limits, name).filter(|u| *u > stamp.as_str());
            json!({"name": name, "provider": provider, "model": model, "effort": effort,
                   "limited_until": until})
        })
        .collect();
    let mut checkouts: Vec<String> = dict(read_paths_strict(home).ok()?)
        .into_iter()
        .filter(|(slug, path)| checkout_slug(slug) && Path::new(path).is_dir())
        .map(|(slug, _)| slug)
        .collect();
    checkouts.sort();
    checkouts.truncate(64);
    let doctor = if failed.is_empty() {
        json!("ok")
    } else {
        json!(failed)
    };
    let mut body = json!({
        "os": host_os(), "scheduler": scheduler, "last_pickup": last, "bundle": bundle_stamp(),
        "doctor": doctor, "routes": routes, "checkouts": checkouts,
    });
    let commit = body["bundle"]["commit"].as_str().map(String::from);
    if let Some(commit) = commit {
        if let Some(behind) = bundle_behind_since(home, &commit).ok()? {
            body["bundle_behind_since"] = json!(behind);
        }
    }
    if let Some(triage) = triage {
        body["triage_failed_since"] = json!(triage);
    }
    Some(body)
}

/// PUT this host's status when config.md sets device:. Silent in every outcome, so a caller's
/// output and exit code stay as they were.
fn report_status(
    home: &Path,
    cached: (String, Option<String>, Option<String>),
    report: Option<&Value>,
) -> bool {
    let config = home.join("config.md");
    let device = if config.exists() {
        match frontmatter(&config) {
            Ok((meta, _)) => fm_get(&meta, "device").unwrap_or("").to_string(),
            Err(_) => return false,
        }
    } else {
        String::new()
    };
    if !HOST_NAME.is_full(&device) {
        return false;
    }
    let fresh;
    let report = match report {
        Some(report) => report,
        None => {
            fresh = doctor(home);
            &fresh
        }
    };
    let (scheduler, last, triage) = cached;
    let Some(body) = host_status(home, report, &scheduler, last.as_deref(), triage.as_deref())
    else {
        return false;
    };
    api(
        home,
        "PUT",
        &format!("/api/hosts/{device}/status"),
        Some(dumps(&body, DEFAULT).as_bytes()),
        &[200],
        &[("Content-Type", "application/json")],
    )
    .is_ok()
}

/// config.md text with `device: id` set in its frontmatter, every other byte kept, or None when
/// there is no frontmatter.
fn device_line(text: &str, id: &str) -> Option<String> {
    let mut lines: Vec<String> = splitlines(text, true)
        .into_iter()
        .map(String::from)
        .collect();
    let end = (1..lines.len()).find(|&i| strip(&lines[i]) == "---");
    if strip(lines.first()?) != "---" {
        return None;
    }
    let end = end?;
    let ending = |line: &str| line[line.trim_end_matches(['\r', '\n']).len()..].to_string();
    let eol = ending(&lines[0]);
    let found = (1..end).find(|&i| {
        let line = &lines[i];
        line.contains(':')
            && !strip(line).starts_with('#')
            && strip(line.split(':').next().unwrap_or("")) == "device"
    });
    match found {
        Some(i) => lines[i] = format!("device: {id}{}", ending(&lines[i])),
        None => lines.insert(end, format!("device: {id}{eol}")),
    }
    Some(lines.concat())
}

/// The board's autonomy record with defaults filled in. A malformed record counts as paused.
fn load_autonomy(board: &Value) -> Value {
    let default = json!({"paused": true, "projects": [], "self": []});
    let stored = board.get("autonomy").cloned().unwrap_or_else(|| json!({}));
    let mut record = default.clone();
    if let (Some(record), Some(stored)) = (record.as_object_mut(), stored.as_object()) {
        for (key, value) in stored {
            record.insert(key.clone(), value.clone());
        }
    }
    let strings = |key: &str| {
        record[key]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
    };
    if !stored.is_object()
        || !record["paused"].is_boolean()
        || !strings("projects")
        || !strings("self")
        || record["projects"].as_array().map_or(0, Vec::len) > MAX_EXPLORER_PROJECTS
    {
        eprintln!("the board's autonomy record is malformed, so autonomy counts as paused; run autonomy pause to rewrite it");
        return default;
    }
    record
}

/// `%Y%m%dT%H%M%SZ` now, a dash, and six random hex digits.
fn run_tag() -> String {
    let stamp: String = now().chars().filter(|c| *c != '-' && *c != ':').collect();
    let token = RandomState::new().build_hasher().finish() & 0xff_ffff;
    format!("{stamp}-{token:06x}")
}

/// (name, data) for each regular file and folder in a tar stream, folders with None data.
/// Links, other types and names `check_path` refuses are skipped. A size field that is not
/// octal, or a body that runs past the end, fails as tarfile does.
fn tar_members(data: &[u8]) -> Result<Vec<(String, Option<Vec<u8>>)>, ()> {
    let mut members = Vec::new();
    let (mut pos, mut pax_path) = (0, None::<String>);
    while pos + 512 <= data.len() {
        let header = &data[pos..pos + 512];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let field = |a: usize, b: usize| {
            let f = &header[a..b];
            &f[..f.iter().position(|c| *c == 0).unwrap_or(f.len())]
        };
        let size = std::str::from_utf8(field(124, 136)).map_err(|_| ())?.trim();
        let size = if size.is_empty() {
            0
        } else {
            usize::from_str_radix(size, 8).map_err(|_| ())?
        };
        let body = data.get(pos + 512..pos + 512 + size).ok_or(())?;
        pos += 512 + size.div_ceil(512) * 512;
        let kind = header[156];
        if kind == b'x' {
            let mut records = body;
            while let Some(space) = records.iter().position(|b| *b == b' ') {
                let len = std::str::from_utf8(&records[..space])
                    .ok()
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|n| *n > space && *n <= records.len());
                let Some(len) = len else { break };
                let record = &records[space + 1..len];
                let record = record.strip_suffix(b"\n").unwrap_or(record);
                if let Some(value) = record.strip_prefix(b"path=") {
                    pax_path = Some(String::from_utf8_lossy(value).into_owned());
                }
                records = &records[len..];
            }
            continue;
        }
        if kind == b'g' {
            continue;
        }
        let mut name = String::from_utf8_lossy(field(0, 100)).into_owned();
        let prefix = field(345, 500);
        if &header[257..262] == b"ustar" && !prefix.is_empty() {
            name = format!("{}/{name}", String::from_utf8_lossy(prefix));
        }
        let name = pax_path.take().unwrap_or(name);
        let name = name.trim_end_matches('/').to_string();
        let entry = match kind {
            b'0' | 0 | b'7' => Some(body.to_vec()),
            b'5' => None,
            _ => continue,
        };
        if !check_path(&name) {
            members.push((name, entry));
        }
    }
    Ok(members)
}

/// Write commit's regular files and folders into dest, with no .git and no links.
fn snapshot(repo: &Path, commit: &str, dest: &Path) -> Res<()> {
    let failed = || fail(format!("git archive failed in {}", repo.display()), 1);
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["archive", "--format=tar", commit])
        .output()
        .map_err(|_| failed())?;
    if !out.status.success() {
        return Err(failed());
    }
    let members = tar_members(&out.stdout).map_err(|()| {
        fail(
            format!("git archive in {} sent a malformed tar", repo.display()),
            1,
        )
    })?;
    let parent = dest.parent().unwrap_or(Path::new("/"));
    fs::create_dir_all(parent)
        .and_then(|()| fs::create_dir(dest))
        .map_err(|e| os_fail(e, dest))?;
    for (name, data) in members {
        let path = dest.join(&name);
        match data {
            None => fs::create_dir_all(&path),
            Some(bytes) => fs::create_dir_all(path.parent().unwrap_or(dest))
                .and_then(|()| fs::write(&path, bytes)),
        }
        .map_err(|e| os_fail(e, &path))?;
    }
    Ok(())
}

/// Snapshot an allowlisted project's base for a read-only explorer, or remove that snapshot.
pub fn explore(call: &Call) -> Res<()> {
    let (slug, rest) = (&call.args[0], &call.args[1..]);
    if !SLUG.is_full(slug)
        || !rest.is_empty() && !(rest.len() == 2 && rest[0] == "done" && RUN_TAG.is_full(&rest[1]))
    {
        return Err(fail("usage: explore <slug> | explore <slug> done <run>", 2));
    }
    let home = factory_home()?;
    let folder = home.join("explore").join(slug);
    if let [_, run] = rest {
        let target = folder.join(run);
        let linked = [&home.join("explore"), &folder, &target]
            .iter()
            .any(|p| is_link(p).unwrap_or(true));
        if linked || !target.is_dir() {
            return Err(fail(format!("no explorer checkout {slug}/{run}"), 4));
        }
        fs::remove_dir_all(&target).map_err(|e| os_fail(e, &target))?;
        println!("removed {}", target.display());
        return Ok(());
    }
    let autonomy = load_autonomy(&read_board(&home)?);
    if autonomy["paused"] == true {
        return Err(fail("autonomy is paused; no explorer run", 3));
    }
    if !autonomy["projects"]
        .as_array()
        .is_some_and(|p| p.iter().any(|s| s == slug.as_str()))
    {
        return Err(fail(
            format!("{slug} is not on the autonomy allowlist; no explorer run"),
            3,
        ));
    }
    let paths = read_paths_strict(&home)?;
    let repo = fm_get(&paths, slug).unwrap_or("");
    if repo.is_empty() {
        return Err(fail(
            format!(
                "{} has no entry for {slug}: run /factory init in this device's checkout of it",
                home.join("paths.md").display()
            ),
            4,
        ));
    }
    if pull_project(&home, slug)? {
        return Err(fail(
            format!("Project has local edits. Push {slug} before continuing."),
            6,
        ));
    }
    let project = project_files(&home, slug)?.0;
    let meta = frontmatter(&project).map_err(|e| os_fail(e, &project))?.0;
    let base = fm_get(&meta, "base")
        .filter(|b| !b.is_empty())
        .unwrap_or("main");
    if check_path(base) || base.starts_with('-') {
        return Err(fail(
            format!("{slug}'s base {base} is not a safe branch name"),
            2,
        ));
    }
    let repo = Path::new(repo);
    origin_git(
        repo,
        &[
            "fetch",
            "origin",
            &format!("+refs/heads/{base}:refs/remotes/origin/{base}"),
        ],
    )?;
    let commit = git_ok(
        repo,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/remotes/origin/{base}^{{commit}}"),
        ],
    )?;
    let run = run_tag();
    let checkout = folder.join(&run);
    let out = folder.join(format!("{run}.json"));
    snapshot(repo, &commit, &checkout)?;
    let printed = json!({"project": slug, "run": run, "commit": commit,
        "checkout": checkout.display().to_string(), "out": out.display().to_string()});
    println!("{}", dumps(&printed, INDENT2));
    Ok(())
}

/// Pair this device with a host a person added in Settings. The code goes only into the POST body.
pub fn host_cmd(call: &Call) -> Res<()> {
    let (sub, host_id, code) = (&call.args[0], &call.args[1], &call.args[2]);
    if sub != "pair" {
        return Err(fail("usage: host pair <id> <code>", 2));
    }
    if !HOST_NAME.is_full(host_id) {
        return Err(fail(
            "usage: host pair <id> <code>: the id is 1 to 64 letters, digits, ., - and _",
            2,
        ));
    }
    if !PAIRING_CODE.is_full(code) {
        return Err(fail(
            "usage: host pair <id> <code>: the code looks like XXXXX-XXXXX",
            2,
        ));
    }
    let home = factory_home()?;
    let device = holder(&home)?.0;
    if *host_id != device {
        return Err(fail(format!("host id {host_id} does not match this device's name {device}. Board claims use {device}, so add the host in Settings as {device}."), 2));
    }
    let r = api(
        &home,
        "POST",
        &format!("/api/hosts/{host_id}/pair"),
        Some(dumps(&json!({"code": code}), DEFAULT).as_bytes()),
        &[200, 403, 409],
        &[("Content-Type", "application/json")],
    )?;
    let text = r.text()?;
    if r.status == 403 && strip(&text) == PAIRING_FAILED {
        return Err(fail(
            format!("{PAIRING_FAILED}. Ask for a new code in Settings, Hosts."),
            6,
        ));
    }
    if r.status == 403 {
        let env_file = abspath(&home.join(".env").to_string_lossy());
        return Err(fail(access_message(&env_file), 5));
    }
    if r.status == 409 {
        return Err(fail("this service token is already bound to another host. Each host needs its own Access service token.", 6));
    }
    let config = home.join("config.md");
    let text = read_raw(&config).map_err(|e| os_fail(e, &config))?;
    let updated = device_line(&text, host_id)
        .ok_or_else(|| fail("config.md has no frontmatter; run /factory init again", 2))?;
    if updated != text {
        write_atomic(&config, updated.as_bytes()).map_err(|e| os_fail(e, &config))?;
    }
    report_status(&home, cached_status(&home), None);
    println!("paired {host_id}. config.md now sets device: {host_id}. Confirm the host in Settings, Hosts.");
    Ok(())
}

/// The nonempty `CLOUDFLARE_*` keys of env_file, refusing a link, a folder, an unreadable file
/// or one without `CLOUDFLARE_API_TOKEN`.
fn cloudflare_env(env_file: &Path) -> Res<Vec<(String, String)>> {
    let shown = env_file.display();
    let not_regular = || {
        fail(
            format!("{shown} is a link or not a regular file, so it was not read. Replace it with a regular file."),
            5,
        )
    };
    let unreadable = || {
        fail(
            format!("cannot read {shown}: check its permissions and that it is UTF-8 text."),
            5,
        )
    };
    if is_link(env_file).map_err(|_| unreadable())? {
        return Err(not_regular());
    }
    if fs::symlink_metadata(env_file).is_err() {
        return Err(fail(format!("no {shown}. docs/cloudflare.md, Device token, shows how to store CLOUDFLARE_API_TOKEN there."), 5));
    }
    if !fs::metadata(env_file).map_err(|_| unreadable())?.is_file() {
        return Err(not_regular());
    }
    let loaded: Vec<(String, String)> = read_env_ordered(env_file)
        .map_err(|_| unreadable())?
        .into_iter()
        .filter(|(k, v)| k.starts_with("CLOUDFLARE_") && !v.is_empty())
        .collect();
    if !loaded.iter().any(|(k, _)| k == "CLOUDFLARE_API_TOKEN") {
        return Err(fail(format!("CLOUDFLARE_API_TOKEN missing from {shown}. docs/cloudflare.md, Device token, shows how to store it."), 5));
    }
    Ok(loaded)
}

/// Run argv with env added, passing its exit code on; a signal s exits 128 + s.
fn run_child(argv: &[String], env: &[(String, String)]) -> Res<()> {
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .envs(env.iter().map(|(k, v)| (k, v)))
        .status()
        .map_err(|_| fail(format!("cannot run {}", argv[0]), 127))?;
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return Err(exit(128 + signal));
    }
    match status.code().unwrap_or(1) {
        0 => Ok(()),
        code => Err(exit(code)),
    }
}

pub fn with_cloudflare_cmd(call: &Call) -> Res<()> {
    let argv = &call.args;
    if argv.len() < 2 || argv[0] != "--" {
        return Err(fail(
            "usage: factory with-cloudflare -- <command> [<arg>...]",
            2,
        ));
    }
    let env = cloudflare_env(&factory_home()?.join(".env"))?;
    run_child(&argv[1..], &env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;

    /// Parity writes the host-status "os" field as {os}, so only this test catches a wrong label.
    #[test]
    fn host_os_labels_each_platform() {
        let os = host_os();
        #[cfg(target_os = "macos")]
        assert_eq!(os, "macOS");
        #[cfg(windows)]
        assert_eq!(os, "Windows");
        #[cfg(target_os = "linux")]
        {
            let wsl = fs::read_to_string("/proc/sys/kernel/osrelease")
                .unwrap_or_default()
                .to_lowercase()
                .contains("microsoft");
            assert_eq!(os, if wsl { "Linux (WSL)" } else { "Linux" });
        }
        #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
        assert_eq!(os, "Other");
    }

    /// The daemon line goes only to a terminal, so piped doctor output stays factory.py's.
    #[test]
    fn doctor_daemon_line_only_on_a_terminal() {
        let mut err = Vec::new();
        daemon_line(&mut err, false, || {
            unreachable!("piped doctor checks no daemon")
        });
        assert!(err.is_empty());
        daemon_line(&mut err, true, || "running".into());
        assert_eq!(String::from_utf8(err).unwrap(), "daemon: running\n");
    }

    /// An exported FACTORY_PARITY_NOW gets a line at a terminal, and nothing on a pipe.
    #[test]
    fn doctor_names_a_pinned_clock_only_on_a_terminal() {
        let mut err = Vec::new();
        clock_line(&mut err, false, Some("1700000000".into()));
        clock_line(&mut err, true, None);
        assert!(err.is_empty());
        clock_line(&mut err, true, Some("1700000000".into()));
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "clock: FACTORY_PARITY_NOW=1700000000 pins the time factory reads; unset it outside the parity harness\n"
        );
    }

    #[cfg(unix)]
    fn names(dir: &Path) -> Vec<String> {
        let mut found: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        found.sort();
        found
    }

    #[cfg(unix)]
    #[test]
    fn test_explore_snapshot() {
        let root = temp_dir("setup-explore-snapshot");
        let repo = root.join("repo");
        fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        fs::write(repo.join("a.txt"), "a\n").unwrap();
        fs::write(repo.join(".env.example"), "KEY=\n").unwrap();
        std::os::unix::fs::symlink("/etc/hosts", repo.join("link")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "one"]);
        fs::write(repo.join("b.txt"), "b\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "two"]);
        let head = git(&["rev-parse", "HEAD"]);
        let dest = root.join("explore/sample/run-1");
        snapshot(&repo, &head, &dest).unwrap();
        assert_eq!(names(&dest), [".env.example", "a.txt", "b.txt"]);
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "a\n");
        assert!(!dest.join("link").exists() && fs::symlink_metadata(dest.join("link")).is_err());
        assert!(!dest.join(".git").exists());
        assert!(!root.join("explore/sample/run-1.json").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_explore_done_refuses_linked_parents() {
        let _lock = crate::shared::test_env_lock();
        let root = temp_dir("setup-explore-linked");
        let home = root.join("home");
        let elsewhere = root.join("elsewhere");
        fs::create_dir_all(elsewhere.join("sample/run-1")).unwrap();
        fs::create_dir(&home).unwrap();
        let saved = std::env::var_os("FACTORY_HOME");
        std::env::set_var("FACTORY_HOME", &home);
        let call = Call {
            args: ["sample", "done", "run-1"].map(String::from).to_vec(),
            agents: None,
            connection: None,
        };
        let mut results = Vec::new();
        for link in ["explore", "sample"] {
            let (at, target) = if link == "explore" {
                (home.join("explore"), elsewhere.clone())
            } else {
                fs::create_dir(home.join("explore")).unwrap();
                (home.join("explore/sample"), elsewhere.join("sample"))
            };
            std::os::unix::fs::symlink(target, &at).unwrap();
            results.push(explore(&call).unwrap_err());
            fs::remove_file(&at).unwrap();
        }
        match saved {
            Some(v) => std::env::set_var("FACTORY_HOME", v),
            None => std::env::remove_var("FACTORY_HOME"),
        }
        for e in results {
            assert_eq!(
                (e.code, e.stderr.as_str()),
                (4, "no explorer checkout sample/run-1\n")
            );
        }
        assert!(elsewhere.join("sample/run-1").is_dir());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn set_api_url_keeps_crlf() {
        let text =
            "---\r\n# retained\r\nunknown: legacy\r\napi_url:\r\ndevice: local\r\n---\r\nBody\r\n";
        assert_eq!(
            set_api_url(text, "https://example.invalid").unwrap(),
            "---\r\n# retained\r\nunknown: legacy\r\napi_url: https://example.invalid\r\ndevice: local\r\n---\r\nBody\r\n"
        );
    }

    #[test]
    fn set_api_url_inserts_key() {
        assert_eq!(
            set_api_url(
                "---\nunknown: retained\n---\nBody\n",
                "https://example.invalid"
            )
            .unwrap(),
            "---\nunknown: retained\napi_url: https://example.invalid\n---\nBody\n"
        );
        assert_eq!(set_api_url("---\nopen: yes\n", "https://x.invalid"), None);
    }

    #[test]
    fn load_connection_rejects() {
        let dir = temp_dir("setup-connection");
        let path = dir.join("connection.json");
        for data in [
            &b"\xff"[..],
            b"[]",
            b"{}",
            br#"{"version":true,"api_url":"https://example.invalid"}"#,
            br#"{"version":1,"api_url":"http://localhost"}"#,
            br#"{"version":1,"api_url":"https://example.invalid","token":"synthetic"}"#,
        ] {
            fs::write(&path, data).unwrap();
            let e = load_connection(&path).unwrap_err();
            assert_eq!(e.code, 2);
            assert_eq!(
                e.stderr,
                format!("{}: invalid connection descriptor\n", path.display())
            );
        }
        fs::write(
            &path,
            r#"{"version":1.0,"api_url":"https://example.invalid"}"#,
        )
        .unwrap();
        assert!(load_connection(&path).is_err());
        fs::write(
            &path,
            r#"{"version":1,"api_url":"https://example.invalid/"}"#,
        )
        .unwrap();
        assert_eq!(load_connection(&path).unwrap(), "https://example.invalid");
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn abspath_folds() {
        assert_eq!(abspath("/a/./b/../c//d"), PathBuf::from("/a/c/d"));
        assert_eq!(abspath("/.."), PathBuf::from("/"));
    }

    #[cfg(windows)]
    #[test]
    fn abspath_keeps_a_windows_drive() {
        assert_eq!(abspath(r"C:\repo"), PathBuf::from(r"C:\repo"));
        assert_eq!(abspath(r"C:\a\.\b\..\c"), PathBuf::from(r"C:\a\c"));
        assert_eq!(abspath("C:/a/b"), PathBuf::from(r"C:\a\b"));
    }

    #[test]
    fn native_header_shapes() {
        for header in [
            "---\ndescription: \"Local tuning of the builder\"\nmodel: provider/model # pinned locally\nmode: all\n---\n",
            "---\nmodel: \"provider/model#fast\"\nmode: all\n---\n",
            "---\r\ndescription: \"CRLF role\"\r\nmodel: provider/model\r\nmode: all\r\n---\r\n",
            "---\ndescription: >-\n  First line of a local description\n  second line\nmodel: provider/model\nvariant: fast\nmode: all\npermission:\n  bash: allow\ntemperature: 0.2\n---\n",
        ] {
            assert_eq!(native_header(&format!("{header}body\n")), Some(header));
        }
        let at_eof = "---\nmodel: provider/model\nmode: all\n---";
        assert_eq!(native_header(at_eof), Some(at_eof));
        for refused in [
            "plain text, no frontmatter\n",
            "---\nmodel: provider/model\n",
            "---\nmode: all\n...\nbody\n",
        ] {
            assert_eq!(native_header(refused), None);
        }
    }

    #[test]
    fn roles_embed_headers() {
        assert_eq!(ROLES.map(|(name, _)| name), ROLE_NAMES);
        for (name, text) in ROLES {
            assert!(native_header(text).is_some(), "{name}");
            assert!(
                fm_get(&frontmatter_text(text).0, "description").is_some_and(|d| !d.is_empty()),
                "{name}"
            );
        }
    }

    #[test]
    fn install_roles_keeps_model() {
        let dir = temp_dir("setup-roles");
        let agents = dir.join("agents");
        let dirs = vec![("opencode".to_string(), agents.clone())];
        assert_eq!(install_roles(&dirs).unwrap(), 9);
        let builder = agents.join("factory-builder.md");
        let first = fs::read_to_string(&builder).unwrap();
        assert!(first.starts_with("---\ndescription: \"Factory loop builder."));
        let pinned = first.replacen("---\n", "---\nmodel: provider/model\n", 1);
        fs::write(
            &builder,
            pinned.replace("You build one", "Stale body. You build one"),
        )
        .unwrap();
        fs::write(agents.join("unrelated.md"), "keep me").unwrap();
        assert_eq!(install_roles(&dirs).unwrap(), 9);
        assert_eq!(fs::read_to_string(&builder).unwrap(), pinned);
        assert_eq!(
            fs::read_to_string(agents.join("unrelated.md")).unwrap(),
            "keep me"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    fn tree_bytes(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut rows: Vec<(String, Vec<u8>)> = fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let path = e.unwrap().path();
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    fs::read(&path).unwrap(),
                )
            })
            .collect();
        rows.sort();
        rows
    }

    #[test]
    fn malformed_last_role_prevents_all_writes_for_either_runtime() {
        let dir = temp_dir("setup-roles-malformed");
        let dirs = vec![
            ("claude".to_string(), dir.join("claude")),
            ("opencode".to_string(), dir.join("opencode")),
        ];
        assert_eq!(install_roles(&dirs).unwrap(), 18);
        for (_, directory) in &dirs {
            let last = directory.join(ROLE_NAMES[ROLE_NAMES.len() - 1]);
            let saved = fs::read(&last).unwrap();
            for invalid in [
                &b"plain"[..],
                b"---\nmissing close",
                b"---\n...\n---\n",
                b" ---\n---\n",
            ] {
                fs::write(&last, invalid).unwrap();
                let before: Vec<_> = dirs.iter().map(|(_, d)| tree_bytes(d)).collect();
                // Every role is stale-able, so a partial write would change another file.
                let first = directory.join(ROLE_NAMES[0]);
                fs::write(&first, "---\n---\nstale body\n").unwrap();
                let before_first = fs::read(&first).unwrap();
                assert_eq!(install_roles(&dirs).unwrap_err().code, 2);
                assert_eq!(fs::read(&first).unwrap(), before_first);
                assert_eq!(fs::read(&last).unwrap(), invalid);
                for ((_, d), rows) in dirs.iter().zip(&before) {
                    let mut now = tree_bytes(d);
                    let mut was = rows.clone();
                    now.retain(|(n, _)| n != ROLE_NAMES[0]);
                    was.retain(|(n, _)| n != ROLE_NAMES[0]);
                    assert_eq!(now, was);
                }
            }
            fs::write(&last, saved).unwrap();
            fs::write(directory.join(ROLE_NAMES[0]), "---\n---\n").unwrap();
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn source_and_destination_links_refused() {
        let dir = temp_dir("setup-roles-link");
        let agents = dir.join("agents");
        let dirs = vec![("claude".to_string(), agents.clone())];
        assert_eq!(install_roles(&dirs).unwrap(), 9);
        let victim = agents.join(ROLE_NAMES[ROLE_NAMES.len() - 1]);
        let target = dir.join("elsewhere.md");
        fs::rename(&victim, &target).unwrap();
        std::os::unix::fs::symlink(&target, &victim).unwrap();
        let before = tree_bytes(&agents);
        assert_eq!(install_roles(&dirs).unwrap_err().code, 2);
        assert_eq!(tree_bytes(&agents), before);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn adopt_home_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("setup-adopt");
        let (home, other) = (dir.join("home"), dir.join("other"));
        for h in [&home, &other] {
            fs::create_dir(h).unwrap();
            fs::write(
                h.join("config.md"),
                "---\napi_url: https://example.invalid\n---\n",
            )
            .unwrap();
        }
        let pointer = dir.join("config/factory-location.json");
        adopt_home(&home, &pointer).unwrap();
        let text = fs::read_to_string(&pointer).unwrap();
        assert_eq!(
            text,
            format!("{{\"version\": 1, \"home\": \"{}\"}}\n", home.display())
        );
        assert_eq!(
            fs::metadata(&pointer).unwrap().permissions().mode() & 0o777,
            0o600
        );
        adopt_home(&home, &pointer).unwrap();
        assert_eq!(fs::read_to_string(&pointer).unwrap(), text);
        assert_eq!(fs::read_dir(dir.join("config")).unwrap().count(), 1);
        let e = adopt_home(&other, &pointer).unwrap_err();
        assert_eq!(e.code, 2);
        assert_eq!(
            e.stderr,
            format!(
                "{}: a different factory home is selected\n",
                pointer.display()
            )
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// A home with the config.md the Python LocationTest.state helper wrote.
    #[cfg(unix)]
    fn location_home(dir: &Path, name: &str) -> PathBuf {
        let home = dir.join(name);
        fs::create_dir(&home).unwrap();
        fs::write(
            home.join("config.md"),
            b"---\napi_url:\nunknown: retained\n---\nBody\n",
        )
        .unwrap();
        resolve(&home).unwrap()
    }

    /// Every path under `root` with its bytes and mode, to compare a tree before and after.
    #[cfg(unix)]
    fn tree_snapshot(root: &Path) -> Vec<(String, Vec<u8>, u32)> {
        use std::os::unix::fs::PermissionsExt;
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>, u32)>) {
            let mut names: Vec<_> = fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            names.sort();
            for path in names {
                let meta = fs::symlink_metadata(&path).unwrap();
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                let bytes = if meta.is_file() {
                    fs::read(&path).unwrap()
                } else {
                    Vec::new()
                };
                out.push((rel, bytes, meta.permissions().mode()));
                if meta.is_dir() {
                    walk(root, &path, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_pointer_and_trusted_default_alias() {
        use crate::shared::home::resolve_home;
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("setup-alias");
        let home = location_home(&dir, "existing");
        let pointer = dir.join("config/factory-location.json");
        adopt_home(&home, &pointer).unwrap();
        std::os::unix::fs::symlink(&home, dir.join("config/factory")).unwrap();
        adopt_home(&home, &pointer).unwrap();
        fs::set_permissions(&pointer, fs::Permissions::from_mode(0o000)).unwrap();
        let e = resolve_home(None, Some(dir.join("config").as_os_str()), None).unwrap_err();
        fs::set_permissions(&pointer, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(e.code, 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn metadata_validation_keeps_dirty_remote_and_empty_manifests() {
        let dir = temp_dir("setup-metadata");
        let home = location_home(&dir, "existing");
        let pointer = dir.join("config/factory-location.json");
        fs::write(
            home.join("paths.md"),
            format!("---\nsample: {}\n---\n", dir.join("not-present").display()),
        )
        .unwrap();
        fs::create_dir(home.join("projects")).unwrap();
        let projects = home.join("projects");
        fs::write(
            projects.join("sample.md"),
            "---\npath: legacy\nunknown: retained\n---\nDirty",
        )
        .unwrap();
        fs::write(projects.join("sample.md.remote"), "remote").unwrap();
        let state = |version: &str, etag: &str| {
            format!(
                "{{\"version\": {version}, \"etag\": {etag}, \"sha256\": \"{}\"}}",
                "0".repeat(64)
            )
        };
        let metadata = projects.join("sample.sync.json");
        fs::write(&metadata, state("1", "\"\\\"base\\\"\"")).unwrap();
        let job = home.join("jobs/sample/001-test");
        fs::create_dir_all(&job).unwrap();
        fs::write(job.join(".sync.json"), "{}").unwrap();
        fs::write(job.join("build.md.remote"), "other device").unwrap();
        let before = tree_snapshot(&home);
        adopt_home(&home, &pointer).unwrap();
        assert_eq!(tree_snapshot(&home), before);
        for bad in [
            "{}".to_string(),
            state("true", "\"\\\"base\\\"\""),
            state("1", "\"W/\\\"base\\\"\""),
        ] {
            fs::write(&metadata, bad).unwrap();
            assert_eq!(validate_home(&home, true).unwrap_err().code, 2);
        }
        fs::write(&metadata, state("1", "\"\\\"base\\\"\"")).unwrap();
        let sha = "0".repeat(64);
        let upper = "A".repeat(64);
        for bad in [
            "[]".to_string(),
            format!("{{\"../escape.md\": {{\"etag\": \"x\", \"sha256\": \"{sha}\"}}}}"),
            format!("{{\"build.md\": {{\"etag\": \"\", \"sha256\": \"{sha}\"}}}}"),
            format!("{{\"build.md\": {{\"etag\": \"x\", \"sha256\": \"{upper}\"}}}}"),
        ] {
            fs::write(job.join(".sync.json"), bad).unwrap();
            assert_eq!(validate_home(&home, true).unwrap_err().code, 2);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_metadata_and_unlistable_tree() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("setup-unreadable");
        let home = location_home(&dir, "existing");
        let pointer = dir.join("config/factory-location.json");
        let config = home.join("config.md");
        fs::set_permissions(&config, fs::Permissions::from_mode(0o000)).unwrap();
        let first = adopt_home(&home, &pointer);
        fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(first.unwrap_err().code, 2);
        let folder = home.join("jobs");
        fs::create_dir(&folder).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o000)).unwrap();
        let second = adopt_home(&home, &pointer);
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(second.unwrap_err().code, 2);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Python forced fsync, link and write failures; the Rust code has no hook for those, so a
    /// pointer name past the filename limit makes the final hard link fail after the temporary exists.
    #[cfg(unix)]
    #[test]
    fn interrupted_publication_cleans_only_own_temporary() {
        let dir = temp_dir("setup-interrupted");
        let home = location_home(&dir, "existing");
        let folder = dir.join("config");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("unrelated"), "keep").unwrap();
        let pointer = folder.join("p".repeat(300));
        let e = adopt_home(&home, &pointer).unwrap_err();
        assert_eq!(e.code, 2);
        assert!(e
            .stderr
            .ends_with(": cannot publish factory location atomically\n"));
        let left: Vec<_> = fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("unrelated")]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn simultaneous_different_adopters_and_identical_winner() {
        for round in 0..20 {
            let dir = temp_dir("setup-race");
            let home = location_home(&dir, "existing");
            let other = location_home(&dir, "other");
            let pointer = dir.join("config/factory-location.json");
            fs::create_dir(dir.join("config")).unwrap();
            let different = round % 2 == 0;
            let picks = [
                home.clone(),
                if different {
                    other.clone()
                } else {
                    home.clone()
                },
            ];
            let results: Vec<i32> = std::thread::scope(|s| {
                let handles: Vec<_> = picks
                    .iter()
                    .map(|pick| {
                        let pointer = &pointer;
                        s.spawn(move || adopt_home(pick, pointer).map_or_else(|e| e.code, |()| 0))
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            });
            let mut sorted = results.clone();
            sorted.sort();
            assert_eq!(sorted, if different { vec![0, 2] } else { vec![0, 0] });
            let chosen = read_location(&pointer).unwrap();
            assert!(chosen == home || chosen == other);
            assert_eq!(fs::read_dir(dir.join("config")).unwrap().count(), 1);
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn real_git_worktree_and_state_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("setup-gitwt");
        let home = location_home(&dir, "existing");
        let repo = dir.join("repo");
        fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            out.stdout
        };
        git(&["init"]);
        git(&["commit", "--allow-empty", "-m", "initial"]);
        let worktree = home.join("worktrees/sample/001-test");
        git(&[
            "worktree",
            "add",
            "-b",
            "factory/001-test",
            worktree.to_str().unwrap(),
        ]);
        let env = home.join(".env");
        fs::write(
            &env,
            "FACTORY_ACCESS_CLIENT_SECRET=synthetic-private\nunknown=retained\n",
        )
        .unwrap();
        fs::set_permissions(&env, fs::Permissions::from_mode(0o640)).unwrap();
        let before = tree_snapshot(&home);
        let admin = tree_snapshot(&repo.join(".git"));
        let listed = git(&["worktree", "list", "--porcelain"]);
        adopt_home(&home, &dir.join("config/factory-location.json")).unwrap();
        assert_eq!(tree_snapshot(&home), before);
        assert_eq!(tree_snapshot(&repo.join(".git")), admin);
        assert_eq!(git(&["worktree", "list", "--porcelain"]), listed);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cached_status_rules() {
        let dir = temp_dir("setup-cached");
        let none = ("none".to_string(), None, None);
        assert_eq!(cached_status(&dir), none);
        let write = |text: &str| fs::write(dir.join("host-status.json"), text).unwrap();
        write(
            r#"{"scheduler": "cron", "last_pickup": "2026-01-01T00:00:00Z", "triage_failed_since": "2026-01-02T00:00:00Z"}"#,
        );
        assert_eq!(
            cached_status(&dir),
            (
                "cron".to_string(),
                Some("2026-01-01T00:00:00Z".to_string()),
                Some("2026-01-02T00:00:00Z".to_string())
            )
        );
        write(r#"{"scheduler": "cron", "last_pickup": null, "triage_failed_since": "soon"}"#);
        assert_eq!(cached_status(&dir), ("cron".to_string(), None, None));
        write(r#"{"scheduler": "systemd", "last_pickup": null}"#);
        assert_eq!(cached_status(&dir), ("systemd".to_string(), None, None));
        for bad in [
            r#"{"scheduler": "anacron", "last_pickup": null}"#,
            r#"{"scheduler": "cron", "last_pickup": "2026-1-1T00:00:00Z"}"#,
            r#"{"scheduler": "cron"}"#,
            r#"["cron"]"#,
            "not json",
        ] {
            write(bad);
            assert_eq!(cached_status(&dir), none, "{bad}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bundle_behind_since_reads_origin_base() {
        let dir = temp_dir("setup-behind");
        let repo = dir.join("studio");
        fs::create_dir(&repo).unwrap();
        let git = |args: &[&str], date: Option<&str>| {
            let mut c = std::process::Command::new("git");
            c.arg("-C")
                .arg(&repo)
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                ])
                .args(args);
            if let Some(date) = date {
                c.env("GIT_COMMITTER_DATE", date);
            }
            let out = c.output().unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"], None);
        let mut commits = Vec::new();
        for date in [
            "2025-12-29T16:00:00Z",
            "2025-12-30T14:00:00Z",
            "2026-01-01T06:00:00Z",
        ] {
            git(&["commit", "-q", "--allow-empty", "-m", date], Some(date));
            commits.push(git(&["rev-parse", "HEAD"], None));
        }
        git(
            &["update-ref", "refs/remotes/origin/main", &commits[2]],
            None,
        );
        fs::write(
            dir.join("paths.md"),
            format!("---\nstudio: {}\n---\n", repo.display()),
        )
        .unwrap();
        assert_eq!(
            bundle_behind_since(&dir, &commits[0]).unwrap(),
            Some("2025-12-30T14:00:00Z".to_string())
        );
        assert_eq!(bundle_behind_since(&dir, &commits[2]).unwrap(), None);
        assert_eq!(bundle_behind_since(&dir, &"0".repeat(40)).unwrap(), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bundle_stamp_shape() {
        let good = r#"{"commit": "abcdef0", "tree": "0123456789abcdef"}"#;
        assert_eq!(stamp_shape(good), Some(loads(good).unwrap()));
        let sha = "0".repeat(64);
        let rust = format!(
            r#"{{"commit": "abcdef0", "tree": "0123456789abcdef", "entry": ["runtime/factory"], "binary_sha256": "{sha}"}}"#
        );
        assert_eq!(stamp_shape(&rust), Some(loads(good).unwrap()));
        let entry_only = r#"{"commit": "abcdef0", "tree": "0123456789abcdef", "entry": []}"#;
        assert_eq!(stamp_shape(entry_only), Some(loads(good).unwrap()));
        for bad in [
            r#"{"commit": "abcdef0", "tree": "0123456", "extra": "0123456"}"#,
            r#"{"commit": "abcdef0", "tree": "0123456", "entry": "runtime/factory"}"#,
            r#"{"commit": "abcdef0", "tree": "0123456", "entry": [1]}"#,
            r#"{"commit": "abcdef0", "tree": "0123456", "binary_sha256": "abcdef0"}"#,
            r#"{"commit": "ABCDEF0", "tree": "0123456"}"#,
            r#"{"commit": "abcdef", "tree": "0123456"}"#,
            r#"{"commit": 1234567, "tree": "0123456"}"#,
            "[]",
        ] {
            assert_eq!(stamp_shape(bad), None, "{bad}");
        }
    }

    #[test]
    fn device_line_cases() {
        assert_eq!(
            device_line("---\r\ndevice: old\r\nx: 1\r\n---\r\nbody\r\n", "parity").unwrap(),
            "---\r\ndevice: parity\r\nx: 1\r\n---\r\nbody\r\n"
        );
        assert_eq!(
            device_line("---\r\napi_url: https://x\r\n---", "parity").unwrap(),
            "---\r\napi_url: https://x\r\ndevice: parity\r\n---"
        );
        assert_eq!(
            device_line("---\n# device: old\n---", "parity").unwrap(),
            "---\n# device: old\ndevice: parity\n---"
        );
        assert_eq!(device_line("no frontmatter\n", "parity"), None);
        assert_eq!(device_line("---\nopen: yes\n", "parity"), None);
        assert_eq!(device_line("", "parity"), None);
    }

    #[cfg(unix)]
    #[test]
    fn cloudflare_env_refusals() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("setup-cloudflare");
        let env = dir.join(".env");
        let refused = |message: String| {
            let e = cloudflare_env(&env).unwrap_err();
            assert_eq!((e.code, e.stderr), (5, format!("{message}\n")));
        };
        let shown = env.display();
        refused(format!("no {shown}. docs/cloudflare.md, Device token, shows how to store CLOUDFLARE_API_TOKEN there."));
        let not_regular = format!("{shown} is a link or not a regular file, so it was not read. Replace it with a regular file.");
        fs::write(dir.join("real"), "CLOUDFLARE_API_TOKEN=tok\n").unwrap();
        std::os::unix::fs::symlink(dir.join("real"), &env).unwrap();
        refused(not_regular.clone());
        fs::remove_file(&env).unwrap();
        fs::create_dir(&env).unwrap();
        refused(not_regular);
        fs::remove_dir(&env).unwrap();
        fs::write(&env, "CLOUDFLARE_API_TOKEN=\nCLOUDFLARE_ACCOUNT_ID=acct\n").unwrap();
        refused(format!("CLOUDFLARE_API_TOKEN missing from {shown}. docs/cloudflare.md, Device token, shows how to store it."));
        fs::write(&env, "CLOUDFLARE_API_TOKEN=tok\n").unwrap();
        fs::set_permissions(&env, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(&env).is_err() {
            refused(format!(
                "cannot read {shown}: check its permissions and that it is UTF-8 text."
            ));
        }
        fs::set_permissions(&env, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&env, b"CLOUDFLARE_API_TOKEN=\xff\n").unwrap();
        refused(format!(
            "cannot read {shown}: check its permissions and that it is UTF-8 text."
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unsearchable_home() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("setup-cloudflare-unsearchable");
        let env = dir.join(".env");
        fs::write(&env, "CLOUDFLARE_API_TOKEN=tok\n").unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
        // root can still search a mode 0 folder, so only assert when the OS refuses.
        if fs::symlink_metadata(&env).is_err() {
            let e = cloudflare_env(&env).unwrap_err();
            assert_eq!(
                (e.code, e.stderr),
                (
                    5,
                    format!(
                        "cannot read {}: check its permissions and that it is UTF-8 text.\n",
                        env.display()
                    )
                )
            );
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn run_child_codes() {
        let argv = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let code = |list: &[&str]| run_child(&argv(list), &[]).map_err(|e| (e.code, e.stderr));
        assert_eq!(code(&["sh", "-c", "exit 7"]), Err((7, String::new())));
        assert_eq!(
            code(&["sh", "-c", "kill -TERM $$"]),
            Err((143, String::new()))
        );
        assert_eq!(
            code(&["/no/such/command"]),
            Err((127, "cannot run /no/such/command\n".to_string()))
        );
        let dir = temp_dir("setup-child");
        let env_file = dir.join(".env");
        fs::write(
            &env_file,
            "CLOUDFLARE_API_TOKEN=tok\nFACTORY_ACCESS_CLIENT_SECRET=secret\n",
        )
        .unwrap();
        let env = cloudflare_env(&env_file).unwrap();
        assert_eq!(
            env,
            vec![("CLOUDFLARE_API_TOKEN".to_string(), "tok".to_string())]
        );
        let check =
            r#"test "$CLOUDFLARE_API_TOKEN" = tok && test -z "$FACTORY_ACCESS_CLIENT_SECRET""#;
        assert!(run_child(&argv(&["sh", "-c", check]), &env).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_autonomy_rules() {
        let paused = json!({"paused": true, "projects": [], "self": []});
        assert_eq!(load_autonomy(&json!({"jobs": []})), paused);
        for bad in [
            json!({"paused": "no"}),
            json!({"paused": false, "projects": ["a", "b"]}),
            json!({"paused": false, "self": "x"}),
            json!({"paused": false, "projects": [1]}),
            json!([]),
        ] {
            assert_eq!(load_autonomy(&json!({"autonomy": bad})), paused);
        }
        assert_eq!(
            load_autonomy(
                &json!({"autonomy": {"paused": false, "projects": ["sample"], "note": 1}})
            ),
            json!({"paused": false, "projects": ["sample"], "self": [], "note": 1})
        );
    }

    #[test]
    fn run_tag_shape() {
        static SHAPE: Pattern = Pattern::new(r"\d{8}T\d{6}Z-[0-9a-f]{6}", Flags::ASCII);
        let tag = run_tag();
        assert!(SHAPE.is_full(&tag), "{tag}");
        assert!(RUN_TAG.is_full(&tag));
    }

    #[cfg(unix)]
    #[test]
    fn tar_from_git_archive() {
        let dir = temp_dir("setup-tar");
        let repo = dir.join("repo");
        let long = format!("deep/{}.txt", "x".repeat(111));
        assert_eq!(long.len(), 120);
        fs::create_dir_all(repo.join("deep")).unwrap();
        fs::write(repo.join("a.txt"), "a\n").unwrap();
        fs::write(repo.join(".env.example"), "KEY=\n").unwrap();
        fs::write(repo.join("bad name.txt"), "b\n").unwrap();
        fs::write(repo.join(&long), "long\n").unwrap();
        std::os::unix::fs::symlink("a.txt", repo.join("link")).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                ])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "--no-verify", "-m", "snapshot"]);
        let dest = dir.join("out/run");
        snapshot(&repo, "HEAD", &dest).unwrap();
        let mut found: Vec<String> = Vec::new();
        let mut stack = vec![dest.clone()];
        while let Some(folder) = stack.pop() {
            for entry in fs::read_dir(folder).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                assert!(!meta.file_type().is_symlink());
                if meta.is_dir() {
                    stack.push(path.clone());
                }
                found.push(path.strip_prefix(&dest).unwrap().display().to_string());
            }
        }
        found.sort();
        assert_eq!(found, vec![".env.example", "a.txt", "deep", long.as_str()]);
        assert_eq!(fs::read_to_string(dest.join(&long)).unwrap(), "long\n");
        assert!(snapshot(&repo, "no-such-commit", &dir.join("out/other")).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    /// One ustar header block for name, followed by body padded to 512 bytes.
    fn tar_entry(name: &str, kind: u8, body: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[124..135].copy_from_slice(format!("{:011o}", body.len()).as_bytes());
        header[156] = kind;
        header[257..262].copy_from_slice(b"ustar");
        let mut entry = header.to_vec();
        entry.extend_from_slice(body);
        entry.resize(512 + body.len().div_ceil(512) * 512, 0);
        entry
    }

    #[test]
    fn tar_refuses_hostile_members() {
        let mut data = Vec::new();
        for entry in [
            tar_entry("../x", b'0', b"up"),
            tar_entry("/etc/x", b'0', b"abs"),
            tar_entry("pax", b'x', b"13 path=../x\n"),
            tar_entry("innocent.txt", b'0', b"renamed"),
            tar_entry("link", b'2', b""),
            tar_entry("good.txt", b'0', b"g"),
        ] {
            data.extend(entry);
        }
        data.extend([0u8; 1024]);
        assert_eq!(
            tar_members(&data).unwrap(),
            vec![("good.txt".to_string(), Some(b"g".to_vec()))]
        );
        let mut bad_size = tar_entry("a.txt", b'0', b"a");
        bad_size[124..135].copy_from_slice(b"zzzzzzzzzzz");
        assert!(tar_members(&bad_size).is_err());
        let mut truncated = tar_entry("a.txt", b'0', &[b'a'; 600]);
        truncated.truncate(700);
        assert!(tar_members(&truncated).is_err());
    }

    #[test]
    fn strict_paths_refuse_bad_bytes() {
        let dir = temp_dir("setup-paths");
        assert_eq!(read_paths_strict(&dir).unwrap(), Vec::new());
        fs::write(dir.join("paths.md"), b"---\nsample: /srv/\xff\n---\n").unwrap();
        assert_eq!(read_paths_strict(&dir).unwrap_err().code, 1);
        assert_eq!(
            fs::read(dir.join("paths.md")).unwrap(),
            b"---\nsample: /srv/\xff\n---\n"
        );
        let report = json!({"checks": []});
        assert_eq!(host_status(&dir, &report, "none", None, None), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn host_status_refuses_unreadable_routes() {
        let dir = temp_dir("setup-status");
        let report = json!({"checks": []});
        let routes =
            |dir: &Path| host_status(dir, &report, "none", None, None).map(|b| b["routes"].clone());
        fs::write(
            dir.join("routes.md"),
            "---\nprimary: provider-a model-a\n---\n",
        )
        .unwrap();
        assert_eq!(routes(&dir).unwrap().as_array().unwrap().len(), 1);
        fs::write(dir.join("route-limits.json"), b"\xff").unwrap();
        assert_eq!(routes(&dir), Some(json!([])));
        fs::remove_file(dir.join("route-limits.json")).unwrap();
        fs::create_dir(dir.join("route-limits.json")).unwrap();
        assert_eq!(routes(&dir), None);
        fs::remove_dir(dir.join("route-limits.json")).unwrap();
        fs::write(dir.join("routes.md"), "---\nprimary: one-word\n---\n").unwrap();
        assert_eq!(routes(&dir), Some(json!([])));
        fs::write(
            dir.join("routes.md"),
            b"---\nprimary: provider-a \xff\n---\n",
        )
        .unwrap();
        assert_eq!(routes(&dir), None);
        fs::remove_dir_all(dir).unwrap();
    }
}
