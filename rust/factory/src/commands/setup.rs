//! Setup commands: init, adopt-home, doctor, host, with-cloudflare and explore.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::cli::{not_ported, Call};
use crate::commands::lookup::{main_checkout, remotes};
use crate::shared::conf::{closed_frontmatter, fm_get, frontmatter, read_conf, unquote};
use crate::shared::exit::{fail, Exit, Res};
use crate::shared::fs::{is_link, regular_metadata, resolve, seed, write_atomic};
use crate::shared::home::{config_root, factory_home, skill_root};
use crate::shared::http::{api, endpoint_message, normalized_origin, token_message};
use crate::shared::project::{
    local_bytes, project_files, pull_project, read_paths, read_project_state, save_project_state,
};
use crate::shared::py::{os_error, splitlines, strip};
use crate::shared::pyjson::{dumps, loads, DEFAULT};

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

/// `os.path.abspath`: p joined to the cwd, with `.`, `..` and repeated `/` folded lexically.
fn abspath(p: &str) -> PathBuf {
    let joined = if p.starts_with('/') {
        p.to_string()
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        format!("{}/{p}", cwd.display())
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    PathBuf::from(format!("/{}", parts.join("/")))
}

fn load_connection(path: &Path) -> Res<String> {
    let invalid = || {
        fail(
            format!("{}: invalid connection descriptor", path.display()),
            2,
        )
    };
    regular_metadata(path).map_err(|()| invalid())?;
    let text = crate::shared::fs::read_text(path).map_err(|_| invalid())?;
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
        let overlaps = |other: &Path| resolved.starts_with(other) || other.starts_with(&resolved);
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

/// A project file minus its frontmatter `path:` lines, since each device keeps its own.
fn without_path(text: &str) -> String {
    let lines = splitlines(text, true);
    if lines.first().map(|l| strip(l)) != Some("---") {
        return text.to_string();
    }
    for (i, line) in lines.iter().enumerate().skip(1) {
        if strip(line) == "---" {
            let head = lines[1..i].iter().filter(|l| {
                let rest = l.trim_start_matches(crate::shared::py::is_space);
                !rest.strip_prefix("path").is_some_and(|r| {
                    r.trim_start_matches(crate::shared::py::is_space)
                        .starts_with(':')
                })
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
    let mut paths = dict(read_paths(home));
    paths.push((slug.clone(), main.display().to_string()));
    let paths = dict(paths);
    let listed: String = paths.iter().map(|(k, v)| format!("{k}: {v}\n")).collect();
    let paths_file = home.join("paths.md");
    write_atomic(
        &paths_file,
        format!("---\n{listed}---\n\nThis device's checkout of each project. factory.py init writes it.\n")
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

pub fn adopt_home_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("adopt-home"))
}

pub fn doctor_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("doctor"))
}

pub fn explore(_call: &Call) -> Res<()> {
    Err(not_ported("explore"))
}

pub fn host_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("host"))
}

pub fn with_cloudflare_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("with-cloudflare"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;

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
                format!(
                    "factory.py: {}: invalid connection descriptor\n",
                    path.display()
                )
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

    #[test]
    fn abspath_folds() {
        assert_eq!(abspath("/a/./b/../c//d"), PathBuf::from("/a/c/d"));
        assert_eq!(abspath("/.."), PathBuf::from("/"));
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
}
