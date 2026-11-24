//! `bootstrap up | down | plan --answers <file>`: guided Cloudflare setup that creates or reuses
//! every instance resource by name, run from the root of a factory checkout.
//!
//! Exit 2 for bad input, 1 for a Cloudflare or tool failure, 0 on success. Cloudflare holds the
//! state; resources are matched by name on every run.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Map, Value};

use crate::cli::Call;
use crate::shared::exit::{Exit, Res};
use crate::shared::fs::write_atomic;
use crate::shared::http::{platform_agent, send};
use crate::shared::proc::{capture, hostname, Out};
use crate::shared::pyjson::{dumps, loads, DEFAULT, INDENT2_SORTED};
use crate::shared::pyre::{Flags, Pattern};

const API: &str = "https://api.cloudflare.com/client/v4";
const PER_PAGE: usize = 50;
const USAGE: &str = "usage: factory bootstrap <up|down|plan --answers <file>>\n";
const MISSING_ORG: &str = "Zero Trust is not set up for this account. Open Zero Trust at https://one.dash.cloudflare.com/ once to choose a team name, then rerun.";
const READ_ONLY_APP_FIELDS: [&str; 4] = ["id", "aud", "created_at", "updated_at"];
const ANSWER_KEYS: [&str; 5] = ["version", "account_id", "zone", "hostname", "emails"];

static EMAIL: Pattern = Pattern::new(r"[^@\s,]+@[^@\s,]+\.[^@\s,]+", Flags::NONE);
static ACCOUNT_ID: Pattern = Pattern::new("[0-9a-f]{32}", Flags::NONE);
static DNS: Pattern = Pattern::new(
    r"(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z][a-z0-9-]*",
    Flags::NONE,
);
static PLACEHOLDER: Pattern = Pattern::new("<[a-z][a-z0-9-]*>", Flags::NONE);
static ORIGIN: Pattern = Pattern::new("https://([^/?#]+)", Flags::NONE);

fn hint(field: &str) -> &'static str {
    match field {
        "account_id" => "Your account ID is on any domain's Overview page in https://dash.cloudflare.com/\nHelp: https://developers.cloudflare.com/fundamentals/account/find-account-and-zone-ids/",
        "zone" => "The zone is a domain already active in this account; see the list at https://dash.cloudflare.com/",
        "hostname" => "The hostname is a subdomain of the zone where the board will live. Pick one with no DNS record yet.",
        "emails" => "The emails of the people who may sign in to the board through Cloudflare Access.",
        _ => "Create an API token at https://dash.cloudflare.com/profile/api-tokens with Create Custom Token.\nHelp: https://developers.cloudflare.com/fundamentals/api/get-started/create-token/\nIt needs these permissions:\n  Account: Workers Scripts Edit, Workers R2 Storage Edit, Access: Apps and Policies Edit,\n  Access: Service Tokens Edit, Access: Organizations, Identity Providers, and Groups Read\n  Zone: Workers Routes Edit, DNS Edit, Zone Read\nSet CLOUDFLARE_API_TOKEN to skip this prompt.",
    }
}

/// Why a run stopped: bad input (exit 2), Cloudflare or a tool (exit 1, with the HTTP status when
/// Cloudflare answered), or a cancelled prompt (exit 2).
#[derive(Debug, PartialEq)]
pub(crate) enum Stop {
    Input(String),
    Cloudflare(String, Option<u16>),
    Cancelled,
}

type Done<T> = Result<T, Stop>;

fn bad(field: &str, message: &str) -> Stop {
    Stop::Input(format!("{message}\n{}", hint(field)))
}

fn cf(message: impl Into<String>) -> Stop {
    Stop::Cloudflare(message.into(), None)
}

/// One Cloudflare v4 call: (method, path below /client/v4, body).
pub(crate) type Api<'a> = dyn FnMut(&str, &str, Option<&Value>) -> Done<Value> + 'a;

/// What a run reads and runs outside itself, so a test can stand in for each.
pub(crate) struct Host<'a> {
    pub out: &'a mut dyn Write,
    /// A prompt answer, or None at end of input or an interrupt.
    pub input: &'a mut dyn FnMut(&mut dyn Write, &str) -> Option<String>,
    /// The token from a hidden prompt, or None at end of input.
    pub secret: &'a mut dyn FnMut(&mut dyn Write, &str) -> Option<String>,
    /// argv, its cwd, whether to capture its output (else it inherits this terminal), and
    /// variables added to its environment.
    pub run: &'a mut dyn FnMut(&[String], Option<&Path>, bool, &[(&str, &str)]) -> Out,
    pub device: String,
    pub token_env: Option<String>,
    /// This binary, which `init` runs as.
    pub exe: PathBuf,
}

pub fn bootstrap(call: &Call) -> Res<()> {
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    let valid = matches!(args[..], ["up"] | ["down"] | ["plan", "--answers", _]);
    if !valid {
        return Err(Exit::raw(2, USAGE));
    }
    let root = std::env::current_dir().map_err(|e| Exit::raw(1, format!("{e}\n")))?;
    let mut stdout = io::stdout();
    let stdin = io::stdin();
    let mut input = |out: &mut dyn Write, prompt: &str| -> Option<String> {
        let _ = write!(out, "{prompt}");
        let _ = out.flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
        }
    };
    let mut secret = |out: &mut dyn Write, prompt: &str| -> Option<String> {
        let _ = write!(out, "{prompt}");
        let _ = out.flush();
        let line = read_hidden();
        let _ = writeln!(out);
        line
    };
    let mut run =
        |argv: &[String], cwd: Option<&Path>, captured: bool, env: &[(&str, &str)]| -> Out {
            if captured {
                let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
                return capture(&argv, cwd, env, None).unwrap_or_else(|e| Out {
                    code: 127,
                    stdout: String::new(),
                    stderr: e.to_string(),
                });
            }
            let mut command = Command::new(&argv[0]);
            command
                .args(&argv[1..])
                .stdin(Stdio::null())
                .envs(env.iter().copied());
            if let Some(cwd) = cwd {
                command.current_dir(cwd);
            }
            let code = command.status().map_or(127, |s| s.code().unwrap_or(1));
            Out {
                code,
                stdout: String::new(),
                stderr: String::new(),
            }
        };
    let mut host = Host {
        out: &mut stdout,
        input: &mut input,
        secret: &mut secret,
        run: &mut run,
        device: device_label(&hostname()),
        token_env: std::env::var("CLOUDFLARE_API_TOKEN").ok(),
        exe: std::env::current_exe().unwrap_or_default(),
    };
    let mut api = |token: &str| -> Box<dyn FnMut(&str, &str, Option<&Value>) -> Done<Value>> {
        let token = token.to_string();
        Box::new(move |method, path, body| cloudflare(API, &token, method, path, body))
    };
    let (code, err) = run_bootstrap(&args, &root, &mut host, &mut api);
    if code == 0 {
        Ok(())
    } else {
        Err(Exit::raw(code, err))
    }
}

/// (exit code, stderr) of one bootstrap run.
pub(crate) fn run_bootstrap(
    args: &[&str],
    root: &Path,
    host: &mut Host,
    api: &mut dyn FnMut(&str) -> Box<dyn FnMut(&str, &str, Option<&Value>) -> Done<Value>>,
) -> (i32, String) {
    let result = (|| -> Done<()> {
        if args[0] == "plan" {
            let answers = validate_answers(&read_json(Path::new(args[2]))?)?;
            plan(host, &answers);
            return Ok(());
        }
        if !root.join("tools").join("deploy.mjs").is_file() {
            return Err(Stop::Input(format!(
                "{} is not the root of a factory checkout; run bootstrap from there",
                root.display()
            )));
        }
        let saved_path = root.join("deploy").join("bootstrap.json");
        if args[0] == "up" {
            let saved = if saved_path.exists() {
                read_json(&saved_path)?
            } else {
                json!({})
            };
            let answers = validate_answers(&prompt_answers(host, &saved)?)?;
            fs::create_dir_all(saved_path.parent().unwrap())
                .map_err(|e| cf(format!("cannot create {}: {e}", saved_path.display())))?;
            write_if_changed(&saved_path, &answers)?;
            let token = read_token(host)?;
            let mut client = api(&token);
            return up(host, &answers, &mut *client, root, &token);
        }
        let answers = validate_answers(&read_json(&saved_path)?)?;
        let token = read_token(host)?;
        let mut client = api(&token);
        down(host, &answers, &mut *client, root)
    })();
    match result {
        Ok(()) => (0, String::new()),
        Err(Stop::Input(m)) => (2, format!("invalid input: {m}\n")),
        Err(Stop::Cancelled) => (2, "cancelled\n".into()),
        Err(Stop::Cloudflare(m, _)) => (1, format!("{m}\n")),
    }
}

/// The token from a hidden prompt, with echo off on a terminal; None at end of input.
fn read_hidden() -> Option<String> {
    #[cfg(unix)]
    let echo_off = Command::new("stty")
        .arg("-echo")
        .stdin(Stdio::inherit())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    #[cfg(windows)]
    let restore = windows_echo_off();
    let mut line = String::new();
    let read = io::stdin().lock().read_line(&mut line);
    #[cfg(unix)]
    if echo_off {
        let _ = Command::new("stty")
            .arg("echo")
            .stdin(Stdio::inherit())
            .status();
    }
    #[cfg(windows)]
    windows_echo_restore(restore);
    match read {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
    fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: *mut std::ffi::c_void, mode: u32) -> i32;
}

/// The console input mode before echo went off, or None when stdin is not a console.
#[cfg(windows)]
fn windows_echo_off() -> Option<u32> {
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const ENABLE_ECHO_INPUT: u32 = 0x4;
    // SAFETY: the handle comes from GetStdHandle and mode is a valid out pointer.
    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return None;
        }
        SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT);
        Some(mode)
    }
}

#[cfg(windows)]
fn windows_echo_restore(mode: Option<u32>) {
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    if let Some(mode) = mode {
        // SAFETY: the handle comes from GetStdHandle; mode is the one GetConsoleMode returned.
        unsafe {
            SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), mode);
        }
    }
}

/// One Cloudflare v4 call with the token; the JSON result, `{}` for an empty body.
fn cloudflare(
    api: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Done<Value> {
    let headers = [
        ("Authorization".to_string(), format!("Bearer {token}")),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), "factory-bootstrap".to_string()),
    ];
    let data = body.map(|b| dumps(b, DEFAULT).into_bytes());
    let url = format!("{api}/{path}");
    let response = send(
        platform_agent(api),
        method,
        &url,
        &headers,
        data.as_deref(),
        16 << 20,
    )
    .map_err(|e| {
        cf(format!(
            "{method} {path}: cannot reach api.cloudflare.com: {e}"
        ))
    })?;
    let text = String::from_utf8_lossy(&response.body).into_owned();
    if !(200..300).contains(&response.status) {
        let first = loads(&text)
            .ok()
            .and_then(|v| v.get("errors")?.as_array()?.first().cloned())
            .unwrap_or(json!({}));
        let code = first
            .get("code")
            .map_or(response.status.to_string(), |c| show(c));
        let message = first
            .get("message")
            .map_or_else(|| "error".to_string(), show);
        return Err(Stop::Cloudflare(
            format!("{method} {path}: {code} {message}"),
            Some(response.status),
        ));
    }
    if text.is_empty() {
        return Ok(json!({}));
    }
    loads(&text).map_err(|e| cf(format!("{method} {path}: not JSON: {e}")))
}

/// A JSON value as Python's str() prints it inside a message.
fn show(v: &Value) -> String {
    v.as_str().map_or_else(|| v.to_string(), String::from)
}

fn status(stop: &Stop) -> Option<u16> {
    match stop {
        Stop::Cloudflare(_, s) => *s,
        _ => None,
    }
}

fn result_of(v: Value) -> Value {
    v.get("result").cloned().unwrap_or(Value::Null)
}

fn api_list(api: &mut Api, path: &str) -> Done<Vec<Value>> {
    let mut items = Vec::new();
    let separator = if path.contains('?') { "&" } else { "?" };
    for page in 1.. {
        let result = result_of(api(
            "GET",
            &format!("{path}{separator}page={page}&per_page={PER_PAGE}"),
            None,
        )?);
        let batch = result.as_array().cloned().unwrap_or_default();
        let count = batch.len();
        items.extend(batch);
        if count < PER_PAGE {
            break;
        }
    }
    Ok(items)
}

/// tools/deploy.mjs's httpsOrigin check for the host of https://<value>.
fn https_origin(value: &str, name: &str) -> Done<()> {
    let origin = format!("https://{value}");
    let fail = |m: &str| Err(Stop::Input(format!("{name} {m}")));
    if origin.trim().is_empty() {
        return fail("must be nonempty");
    }
    if origin
        .chars()
        .any(|c| (c as u32) < 0x20 || (0x7f..=0x9f).contains(&(c as u32)))
    {
        return fail("must not contain control characters");
    }
    if origin.contains("REPLACE_ME")
        || origin.contains("${")
        || PLACEHOLDER.search(&origin).is_some()
    {
        return fail("contains an unresolved placeholder");
    }
    let Some(caps) = ORIGIN.fullmatch(&origin) else {
        return fail("must be a canonical https origin without path, query or fragment");
    };
    let host = caps.get(1).unwrap().as_str();
    if host.contains('@') {
        return fail("must not contain credentials");
    }
    if host.contains(':') {
        return fail("must not contain an explicit port");
    }
    if host.starts_with("xn--") || host.contains('*') {
        return fail("must not be a wildcard or punycode-ata origin");
    }
    if !DNS.is_full(host) {
        return fail("needs a valid DNS hostname");
    }
    Ok(())
}

fn base_name(hostname: &str) -> String {
    hostname.replace('-', "--").replace('.', "-")
}

pub(crate) fn device_label(hostname: &str) -> String {
    let mut label = String::new();
    let mut gap = false;
    for c in hostname.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap {
                label.push('-');
                gap = false;
            }
            label.push(c);
        } else {
            gap = true;
        }
    }
    if gap {
        label.push('-');
    }
    label
}

pub(crate) struct Names {
    worker: String,
    bucket: String,
    app: String,
    people_policy: String,
    device_policy: String,
    token: String,
}

fn names(hostname: &str, device: &str) -> Names {
    let base = base_name(hostname);
    let app = format!("{hostname} bootstrap");
    Names {
        worker: base.clone(),
        bucket: base,
        people_policy: format!("{app} people"),
        device_policy: format!("{app} device {device}"),
        token: format!("{app} device {device}"),
        app,
    }
}

fn is_device_name(n: &Names, name: &str) -> bool {
    name.strip_prefix(&format!("{} device ", n.app))
        .is_some_and(|d| {
            !d.is_empty()
                && d.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

fn str_of<'v>(v: &'v Value, key: &str) -> &'v str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn validate_answers(data: &Value) -> Done<Value> {
    let Some(map) = data.as_object() else {
        return Err(Stop::Input("answers must be a JSON object".into()));
    };
    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
    keys.sort();
    let mut wanted = ANSWER_KEYS.to_vec();
    wanted.sort();
    if keys != wanted {
        return Err(Stop::Input(format!(
            "answers need exactly the keys {}",
            ANSWER_KEYS.join(", ")
        )));
    }
    if !data["version"].is_number() || data["version"].as_f64() != Some(1.0) {
        return Err(Stop::Input("version must be the number 1".into()));
    }
    if !data["account_id"]
        .as_str()
        .is_some_and(|a| ACCOUNT_ID.is_full(a))
    {
        return Err(bad(
            "account_id",
            "account_id must be 32 hexadecimal characters",
        ));
    }
    for key in ["zone", "hostname"] {
        let Some(value) = data[key].as_str() else {
            return Err(bad(key, &format!("{key} must be a string")));
        };
        if let Err(Stop::Input(m)) = https_origin(value, key) {
            return Err(bad(key, &m));
        }
    }
    let (zone, hostname) = (str_of(data, "zone"), str_of(data, "hostname"));
    if !hostname.ends_with(&format!(".{zone}")) {
        return Err(bad(
            "hostname",
            &format!("hostname {hostname} is not inside zone {zone}"),
        ));
    }
    let base = base_name(hostname);
    if base.chars().count() > 63 {
        return Err(bad(
            "hostname",
            &format!("derived name {base} is longer than 63 characters; choose a shorter hostname"),
        ));
    }
    let emails = data["emails"].as_array().filter(|e| !e.is_empty());
    let Some(emails) = emails else {
        return Err(bad("emails", "emails must be a nonempty list"));
    };
    for email in emails {
        if !email.as_str().is_some_and(|e| EMAIL.is_full(e)) {
            return Err(bad(
                "emails",
                &format!("not an email address: {}", dumps(email, DEFAULT)),
            ));
        }
    }
    Ok(data.clone())
}

fn prompt_answers(host: &mut Host, saved: &Value) -> Done<Value> {
    let ask = |host: &mut Host, field: &str, label: &str, default: &str| -> Done<String> {
        let _ = writeln!(host.out, "{}", hint(field));
        let prompt = if default.is_empty() {
            format!("{label}: ")
        } else {
            format!("{label} [{default}]: ")
        };
        let value = (host.input)(host.out, &prompt).ok_or(Stop::Cancelled)?;
        let value = value.trim();
        Ok(if value.is_empty() { default } else { value }.to_string())
    };
    let account = ask(
        host,
        "account_id",
        "Cloudflare account ID",
        str_of(saved, "account_id"),
    )?;
    let zone = ask(
        host,
        "zone",
        "Zone, for example example.test",
        str_of(saved, "zone"),
    )?;
    let fallback = if zone.is_empty() {
        String::new()
    } else {
        format!("factory.{zone}")
    };
    let saved_host = str_of(saved, "hostname");
    let hostname = ask(
        host,
        "hostname",
        "Factory hostname",
        if saved_host.is_empty() {
            &fallback
        } else {
            saved_host
        },
    )?;
    let saved_emails: Vec<&str> = saved
        .get("emails")
        .and_then(Value::as_array)
        .map(|e| e.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let emails = ask(
        host,
        "emails",
        "Allowed emails, comma separated",
        &saved_emails.join(", "),
    )?;
    let emails: Vec<&str> = emails
        .split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .collect();
    Ok(
        json!({"version": 1, "account_id": account, "zone": zone, "hostname": hostname, "emails": emails}),
    )
}

fn read_json(path: &Path) -> Done<Value> {
    let text = fs::read_to_string(path)
        .map_err(|e| Stop::Input(format!("cannot read {}: {}", path.display(), strerror(&e))))?;
    loads(&text).map_err(|e| Stop::Input(format!("{} is not valid JSON: {e}", path.display())))
}

/// The OS message for e, as Python's `strerror`.
fn strerror(e: &io::Error) -> String {
    let text = e.to_string();
    text.split(" (os error").next().unwrap_or(&text).to_string()
}

fn plan(host: &mut Host, answers: &Value) {
    let n = names(str_of(answers, "hostname"), &host.device);
    let emails: Vec<&str> = answers["emails"]
        .as_array()
        .map(|e| e.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let out = &mut host.out;
    let _ = writeln!(out, "account  {}", str_of(answers, "account_id"));
    let _ = writeln!(out, "origin   https://{}", str_of(answers, "hostname"));
    let _ = writeln!(out, "worker   {}", n.worker);
    let _ = writeln!(out, "bucket   {}", n.bucket);
    let _ = writeln!(out, "app      {}", n.app);
    let _ = writeln!(out, "policy   {} ({})", n.people_policy, emails.join(", "));
    let _ = writeln!(out, "policy   {}", n.device_policy);
    let _ = writeln!(out, "token    {}", n.token);
}

fn read_token(host: &mut Host) -> Done<String> {
    let token = match host.token_env.clone().filter(|t| !t.is_empty()) {
        Some(token) => token,
        None => {
            let _ = writeln!(host.out, "{}", hint("token"));
            (host.secret)(host.out, "Cloudflare API token: ").ok_or(Stop::Cancelled)?
        }
    };
    if token.is_empty() {
        return Err(Stop::Input("a Cloudflare API token is required".into()));
    }
    Ok(token)
}

/// Point this device at origin through `factory init`, run from an empty folder; the factory home.
fn run_init(host: &mut Host, origin: &str) -> Done<PathBuf> {
    let empty = crate::shared::fs::temp_root()
        .map_err(|e| cf(e.stderr.trim().to_string()))?
        .join(format!(
            "factory-bootstrap-{}",
            crate::commands::catch_up::random_hex(4)
        ));
    fs::create_dir(&empty).map_err(|e| cf(format!("cannot create {}: {e}", empty.display())))?;
    let connection = empty.join("connection.json");
    let written = fs::write(
        &connection,
        dumps(&json!({"version": 1, "api_url": origin}), DEFAULT),
    );
    let result = written.map(|_| {
        let argv = [
            host.exe.to_string_lossy().into_owned(),
            "init".into(),
            "--connection".into(),
            connection.to_string_lossy().into_owned(),
        ];
        (host.run)(&argv, Some(&empty), true, &[])
    });
    let _ = fs::remove_file(&connection);
    let _ = fs::remove_dir(&empty);
    let result = result.map_err(|e| cf(format!("cannot write {}: {e}", connection.display())))?;
    if result.code != 0 {
        return Err(cf(format!("factory init failed: {}", result.stderr.trim())));
    }
    result
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("factory home: "))
        .map(PathBuf::from)
        .ok_or_else(|| cf("factory init did not report the factory home"))
}

pub(crate) fn read_env(path: &Path) -> Map<String, Value> {
    let mut values = Map::new();
    for line in fs::read_to_string(path).unwrap_or_default().lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let b = value.as_bytes();
        let value = if b.len() >= 2 && b[0] == b[b.len() - 1] && (b[0] == b'"' || b[0] == b'\'') {
            &value[1..value.len() - 1]
        } else {
            value
        };
        values.insert(key.trim().to_string(), Value::from(value));
    }
    values
}

fn write_env_credentials(home: &Path, client_id: &str, secret: &str) -> io::Result<()> {
    let path = home.join(".env");
    let mut wanted = vec![
        ("FACTORY_ACCESS_CLIENT_ID", client_id),
        ("FACTORY_ACCESS_CLIENT_SECRET", secret),
    ];
    let mut lines = Vec::new();
    for line in fs::read_to_string(&path)?.lines() {
        let key = line.split('=').next().unwrap_or("").trim();
        match wanted.iter().position(|(k, _)| *k == key) {
            Some(i) => {
                let (k, v) = wanted.remove(i);
                lines.push(format!("{k}={v}"));
            }
            None => lines.push(line.to_string()),
        }
    }
    lines.extend(wanted.iter().map(|(k, v)| format!("{k}={v}")));
    write_atomic(&path, (lines.join("\n") + "\n").as_bytes())
}

fn ensure_bucket(host: &mut Host, api: &mut Api, account: &str, name: &str) -> Done<()> {
    match api(
        "GET",
        &format!("accounts/{account}/r2/buckets/{name}"),
        None,
    ) {
        Ok(_) => {
            let _ = writeln!(host.out, "bucket {name}: kept");
        }
        Err(e) if status(&e) == Some(404) => {
            api(
                "POST",
                &format!("accounts/{account}/r2/buckets"),
                Some(&json!({"name": name})),
            )?;
            let _ = writeln!(host.out, "bucket {name}: created");
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

fn access_issuer(api: &mut Api, account: &str) -> Done<String> {
    let domain = match api(
        "GET",
        &format!("accounts/{account}/access/organizations"),
        None,
    ) {
        Ok(v) => str_of(&result_of(v), "auth_domain").to_string(),
        Err(e) if status(&e) == Some(404) => String::new(),
        Err(e) => return Err(e),
    };
    if domain.is_empty() {
        return Err(cf(MISSING_ORG));
    }
    Ok(format!("https://{domain}"))
}

fn ensure_token(
    host: &mut Host,
    api: &mut Api,
    account: &str,
    name: &str,
    home: &Path,
) -> Done<String> {
    let env = home.join(".env");
    for token in api_list(api, &format!("accounts/{account}/access/service_tokens"))? {
        if str_of(&token, "name") == name {
            if read_env(&env).get("FACTORY_ACCESS_CLIENT_ID") != token.get("client_id") {
                return Err(cf(format!("Service token {name} already exists and {} does not hold it. Delete it under Zero Trust > Access > Service credentials, then rerun.", env.display())));
            }
            let _ = writeln!(host.out, "token {name}: kept");
            return Ok(str_of(&token, "id").to_string());
        }
    }
    let token = result_of(api(
        "POST",
        &format!("accounts/{account}/access/service_tokens"),
        Some(&json!({"name": name})),
    )?);
    let id = str_of(&token, "id").to_string();
    if let Err(e) = write_env_credentials(
        home,
        str_of(&token, "client_id"),
        str_of(&token, "client_secret"),
    ) {
        let failed = format!("cannot write {}: {}.", env.display(), strerror(&e));
        if let Err(delete) = api(
            "DELETE",
            &format!("accounts/{account}/access/service_tokens/{id}"),
            None,
        ) {
            let shown = match delete {
                Stop::Cloudflare(m, _) | Stop::Input(m) => m,
                Stop::Cancelled => "cancelled".into(),
            };
            return Err(cf(format!("{failed} Also could not delete the new service token {name} ({shown}); delete it under Zero Trust > Access > Service credentials, then rerun up.")));
        }
        return Err(cf(format!(
            "{failed} Deleted the new service token {name}; fix the file, then rerun up."
        )));
    }
    let _ = writeln!(
        host.out,
        "token {name}: created, credentials written to {}",
        env.display()
    );
    Ok(id)
}

fn ensure_policy(
    host: &mut Host,
    api: &mut Api,
    account: &str,
    name: &str,
    decision: &str,
    include: Value,
) -> Done<String> {
    let body = json!({"name": name, "decision": decision, "include": include});
    for policy in api_list(api, &format!("accounts/{account}/access/policies"))? {
        if str_of(&policy, "name") == name {
            let id = str_of(&policy, "id").to_string();
            api(
                "PUT",
                &format!("accounts/{account}/access/policies/{id}"),
                Some(&body),
            )?;
            let _ = writeln!(host.out, "policy {name}: updated");
            return Ok(id);
        }
    }
    let policy = result_of(api(
        "POST",
        &format!("accounts/{account}/access/policies"),
        Some(&body),
    )?);
    let _ = writeln!(host.out, "policy {name}: created");
    Ok(str_of(&policy, "id").to_string())
}

fn precedences(ids: &[String]) -> Value {
    Value::Array(
        ids.iter()
            .enumerate()
            .map(|(i, id)| json!({"id": id, "precedence": i + 1}))
            .collect(),
    )
}

fn ensure_app(
    host: &mut Host,
    api: &mut Api,
    account: &str,
    hostname: &str,
    n: &Names,
    ids: &[String],
    same_site: Option<&str>,
) -> Done<String> {
    for app in api_list(api, &format!("accounts/{account}/access/apps"))? {
        if app.get("domain").and_then(Value::as_str) != Some(hostname) {
            continue;
        }
        if app.get("name").and_then(Value::as_str) != Some(n.app.as_str()) {
            let name = app.get("name").and_then(Value::as_str).unwrap_or("");
            return Err(cf(format!("An Access application for {hostname} already exists with the name \"{name}\", so bootstrap did not create it. Rename it to \"{hostname} bootstrap\" to let bootstrap manage it, or delete it, then rerun.")));
        }
        let path = format!("accounts/{account}/access/apps/{}", str_of(&app, "id"));
        let app = result_of(api("GET", &path, None)?);
        let Some(policies) = app.get("policies").and_then(Value::as_array) else {
            return Err(cf(format!(
                "GET {path}: no policies list in the response; nothing was changed"
            )));
        };
        if !policies.iter().all(|p| p.get("precedence").is_some()) {
            return Err(cf(format!(
                "GET {path}: a policy has no precedence in the response; nothing was changed"
            )));
        }
        let mut ordered = policies.clone();
        ordered.sort_by(|a, b| {
            let key = |p: &Value| p["precedence"].as_f64().unwrap_or(0.0);
            key(a).total_cmp(&key(b))
        });
        let mut current: Vec<String> = ordered
            .iter()
            .map(|p| str_of(p, "id").to_string())
            .collect();
        let missing: Vec<String> = ids
            .iter()
            .filter(|id| !current.contains(id))
            .cloned()
            .collect();
        let stale = same_site.is_some_and(|s| {
            app.get("same_site_cookie_attribute")
                .and_then(Value::as_str)
                != Some(s)
        });
        if missing.is_empty() && !stale {
            let _ = writeln!(host.out, "app {hostname}: kept");
            return Ok(str_of(&app, "aud").to_string());
        }
        let mut body: Map<String, Value> = app
            .as_object()
            .map(|m| {
                m.iter()
                    .filter(|(k, _)| !READ_ONLY_APP_FIELDS.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();
        current.extend(missing);
        body.insert("policies".into(), precedences(&current));
        if let Some(s) = same_site {
            body.insert("same_site_cookie_attribute".into(), Value::from(s));
        }
        let app = result_of(api("PUT", &path, Some(&Value::Object(body)))?);
        let _ = writeln!(host.out, "app {hostname}: updated");
        return Ok(str_of(&app, "aud").to_string());
    }
    let mut body = json!({"name": n.app, "domain": hostname, "type": "self_hosted", "policies": precedences(ids)});
    if let Some(s) = same_site {
        body["same_site_cookie_attribute"] = Value::from(s);
    }
    let app = result_of(api(
        "POST",
        &format!("accounts/{account}/access/apps"),
        Some(&body),
    )?);
    let _ = writeln!(host.out, "app {hostname}: created");
    Ok(str_of(&app, "aud").to_string())
}

fn write_if_changed(path: &Path, document: &Value) -> Done<()> {
    let same = fs::read_to_string(path)
        .ok()
        .and_then(|t| loads(&t).ok())
        .is_some_and(|v| &v == document);
    if !same {
        write_atomic(path, (dumps(document, INDENT2_SORTED) + "\n").as_bytes())
            .map_err(|e| cf(format!("cannot write {}: {}", path.display(), strerror(&e))))?;
    }
    Ok(())
}

fn existing_profile(root: &Path, answers: &Value) -> Done<Option<Value>> {
    let path = root.join("deploy").join("profile.json");
    if !path.exists() {
        return Ok(None);
    }
    let existing = read_json(&path)?;
    let origin = format!("https://{}", str_of(answers, "hostname"));
    for (key, value) in [
        ("account_id", str_of(answers, "account_id")),
        ("origin", origin.as_str()),
    ] {
        let have = existing.get(key).cloned().unwrap_or(Value::Null);
        if have.as_str() != Some(value) {
            return Err(Stop::Input(format!(
                "{} has {key} {}, but the answers say {value}; edit or move it, then rerun",
                path.display(),
                dumps(&have, DEFAULT)
            )));
        }
    }
    Ok(Some(existing))
}

/// tools/deploy.mjs's embedOrigins check, so a bad list stops before any Cloudflare call.
fn embed_origins(value: &Value) -> Done<bool> {
    let Some(list) = value.as_array().filter(|l| l.iter().all(Value::is_string)) else {
        return Err(Stop::Input(
            "embed_origins must be a list of origin strings".into(),
        ));
    };
    let mut seen = std::collections::BTreeSet::new();
    for origin in list.iter().filter_map(Value::as_str) {
        if !seen.insert(origin) {
            return Err(Stop::Input(
                "embed_origins must not contain duplicates".into(),
            ));
        }
        if origin == "paseo://app" {
            continue;
        }
        let Some(rest) = origin.strip_prefix("https://") else {
            return Err(Stop::Input(
                "embed_origins entry must be a canonical https origin or paseo://app".into(),
            ));
        };
        https_origin(rest, "embed_origins entry")?;
    }
    Ok(!list.is_empty())
}

/// token reaches the deploy child too, since Wrangler reads CLOUDFLARE_API_TOKEN from its environment.
fn up(host: &mut Host, answers: &Value, api: &mut Api, root: &Path, token: &str) -> Done<()> {
    let account = str_of(answers, "account_id").to_string();
    let hostname = str_of(answers, "hostname").to_string();
    let origin = format!("https://{hostname}");
    let n = names(&hostname, &host.device);
    let existing = existing_profile(root, answers)?;
    let embed = match existing.as_ref().and_then(|e| e.get("embed_origins")) {
        Some(v) => embed_origins(v)?,
        None => false,
    };
    let same_site = embed.then_some("none");
    api("GET", &format!("accounts/{account}"), None)?;
    let zone = str_of(answers, "zone");
    if api_list(api, &format!("zones?name={zone}&account.id={account}"))?.is_empty() {
        return Err(cf(format!("zone {zone} is not in account {account}")));
    }
    let home = run_init(host, &origin)?;
    let bucket = existing
        .as_ref()
        .map_or(n.bucket.clone(), |e| str_of(e, "bucket_name").to_string());
    ensure_bucket(host, api, &account, &bucket)?;
    let issuer = access_issuer(api, &account)?;
    let token_id = ensure_token(host, api, &account, &n.token, &home)?;
    let emails: Vec<Value> = answers["emails"]
        .as_array()
        .map(|e| {
            e.iter()
                .map(|email| json!({"email": {"email": email}}))
                .collect()
        })
        .unwrap_or_default();
    let people = ensure_policy(
        host,
        api,
        &account,
        &n.people_policy,
        "allow",
        Value::Array(emails),
    )?;
    ensure_policy(
        host,
        api,
        &account,
        &n.device_policy,
        "non_identity",
        json!([{"service_token": {"token_id": token_id}}]),
    )?;
    let mut devices: Vec<(String, String)> =
        api_list(api, &format!("accounts/{account}/access/policies"))?
            .iter()
            .filter(|p| {
                str_of(p, "decision") == "non_identity" && is_device_name(&n, str_of(p, "name"))
            })
            .map(|p| (str_of(p, "name").to_string(), str_of(p, "id").to_string()))
            .collect();
    devices.sort();
    let mut ids = vec![people];
    ids.extend(devices.into_iter().map(|(_, id)| id));
    let audience = ensure_app(host, api, &account, &hostname, &n, &ids, same_site)?;
    let path = root.join("deploy").join("profile.json");
    let mut profile = existing.unwrap_or_else(|| {
        json!({"version": 1, "account_id": account, "worker_name": n.worker, "bucket_name": n.bucket,
            "origin": origin, "title": "Factory", "image_origins": [], "agent_links": {}})
    });
    profile["issuer"] = Value::from(issuer);
    profile["audience"] = Value::from(audience);
    fs::create_dir_all(root.join("deploy")).map_err(|e| {
        cf(format!(
            "cannot create {}: {e}",
            root.join("deploy").display()
        ))
    })?;
    write_if_changed(&path, &profile)?;
    let _ = writeln!(host.out, "profile {}", path.display());
    let argv = [
        "node".to_string(),
        root.join("tools")
            .join("deploy.mjs")
            .to_string_lossy()
            .into_owned(),
        "deploy".into(),
        "--profile".into(),
        path.to_string_lossy().into_owned(),
    ];
    if (host.run)(&argv, None, false, &[("CLOUDFLARE_API_TOKEN", token)]).code != 0 {
        return Err(cf(
            "tools/deploy.mjs deploy failed; fix the cause above, then rerun up",
        ));
    }
    let emails: Vec<&str> = answers["emails"]
        .as_array()
        .map(|e| e.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let out = &mut host.out;
    let _ = writeln!(out, "Factory is live at {origin}");
    let _ = writeln!(
        out,
        "Next: open it in a browser and sign in as one of {}.",
        emails.join(", ")
    );
    let _ = writeln!(out, "Then follow docs/getting-started.md to install the skill, add a project and start a first job.");
    let _ = writeln!(
        out,
        "To add another device, run factory bootstrap up from a checkout on that host."
    );
    let _ = writeln!(
        out,
        "For push notifications and the installable app, see docs/notifications.md."
    );
    let _ = writeln!(out, "Store this API token as CLOUDFLARE_API_TOKEN in {} so agents here can manage the instance; see Device token in docs/cloudflare.md.", home.join(".env").display());
    Ok(())
}

fn down(host: &mut Host, answers: &Value, api: &mut Api, root: &Path) -> Done<()> {
    let account = str_of(answers, "account_id").to_string();
    let hostname = str_of(answers, "hostname").to_string();
    let profile_path = root.join("deploy").join("profile.json");
    let profile = read_json(&profile_path)?;
    let n = names(&hostname, "");
    let typed = (host.input)(
        host.out,
        &format!("Type {hostname} to delete this instance: "),
    )
    .ok_or(Stop::Cancelled)?;
    if typed.trim() != hostname {
        return Err(Stop::Input(format!(
            "confirmation did not match {hostname}; nothing was deleted"
        )));
    }
    for domain in api_list(
        api,
        &format!("accounts/{account}/workers/domains?hostname={hostname}"),
    )? {
        if str_of(&domain, "hostname") == hostname {
            api(
                "DELETE",
                &format!(
                    "accounts/{account}/workers/domains/{}",
                    str_of(&domain, "id")
                ),
                None,
            )?;
            let _ = writeln!(host.out, "custom domain {hostname}: deleted");
        }
    }
    let worker = str_of(&profile, "worker_name").to_string();
    match api(
        "DELETE",
        &format!("accounts/{account}/workers/scripts/{worker}?force=true"),
        None,
    ) {
        Ok(_) => {
            let _ = writeln!(host.out, "worker {worker}: deleted");
        }
        Err(e) if status(&e) == Some(404) => {}
        Err(e) => return Err(e),
    }
    for app in api_list(api, &format!("accounts/{account}/access/apps"))? {
        if app.get("domain").and_then(Value::as_str) != Some(hostname.as_str()) {
            continue;
        }
        let name = app.get("name").and_then(Value::as_str).unwrap_or("");
        if name != n.app {
            let _ = writeln!(
                host.out,
                "app {hostname}: kept, named \"{name}\", which bootstrap did not create"
            );
            continue;
        }
        api(
            "DELETE",
            &format!("accounts/{account}/access/apps/{}", str_of(&app, "id")),
            None,
        )?;
        let _ = writeln!(host.out, "app {hostname}: deleted");
    }
    for policy in api_list(api, &format!("accounts/{account}/access/policies"))? {
        let (name, decision) = (str_of(&policy, "name"), str_of(&policy, "decision"));
        let device = decision == "non_identity" && is_device_name(&n, name);
        if device || (name == n.people_policy && decision == "allow") {
            api(
                "DELETE",
                &format!(
                    "accounts/{account}/access/policies/{}",
                    str_of(&policy, "id")
                ),
                None,
            )?;
            let _ = writeln!(host.out, "policy {name}: deleted");
        }
    }
    for token in api_list(api, &format!("accounts/{account}/access/service_tokens"))? {
        let name = str_of(&token, "name");
        if is_device_name(&n, name) {
            api(
                "DELETE",
                &format!(
                    "accounts/{account}/access/service_tokens/{}",
                    str_of(&token, "id")
                ),
                None,
            )?;
            let _ = writeln!(host.out, "token {name}: deleted");
        }
    }
    let bucket = str_of(&profile, "bucket_name").to_string();
    match api(
        "DELETE",
        &format!("accounts/{account}/r2/buckets/{bucket}"),
        None,
    ) {
        Ok(_) => {
            let _ = writeln!(host.out, "bucket {bucket}: deleted");
        }
        Err(e) if status(&e) == Some(404) => {
            let _ = writeln!(host.out, "bucket {bucket}: already deleted");
        }
        Err(e) => {
            let shown = match e {
                Stop::Cloudflare(m, _) | Stop::Input(m) => m,
                Stop::Cancelled => "cancelled".into(),
            };
            let _ = writeln!(host.out, "bucket {bucket}: kept, Cloudflare said {shown}");
        }
    }
    let _ = writeln!(host.out, "These local files still name the deleted instance: {}, {}, and the factory home config.md api_url and .env credentials, here and on every other device.", profile_path.display(), root.join("deploy").join("bootstrap.json").display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;
    use std::cell::RefCell;
    use std::rc::Rc;

    const ACCOUNT: &str = "00000000000000000000000000000000";
    const API_TOKEN: &str = "synthetic-api-token";
    const SECRET: &str = "synthetic-client-secret";
    const ANSWERS: [&str; 4] = [
        ACCOUNT,
        "example.test",
        "factory.example.test",
        "ada@example.test, grace@example.test",
    ];
    const AGAIN: [&str; 4] = ["", "", "", ""];
    const KINDS: [&str; 4] = ["service_tokens", "policies", "apps", "domains"];

    /// Enough of the v4 API for bootstrap: R2 buckets, Access, service tokens and Workers.
    #[derive(Default)]
    struct Fake {
        org: Option<String>,
        calls: Vec<(String, String, Option<Value>)>,
        buckets: Map<String, Value>,
        items: Map<String, Value>,
        scripts: Vec<String>,
        next_id: usize,
        fail: Vec<(String, String)>,
    }

    fn err<T>(method: &str, path: &str, status: u16, code: u32, message: &str) -> Done<T> {
        Err(Stop::Cloudflare(
            format!("{method} {path}: {code} {message}"),
            Some(status),
        ))
    }

    impl Fake {
        fn new() -> Fake {
            let mut fake = Fake {
                org: Some("example.cloudflareaccess.com".into()),
                ..Fake::default()
            };
            for kind in KINDS {
                fake.items.insert(kind.into(), json!({}));
            }
            fake
        }

        fn store(&mut self, kind: &str) -> &mut Map<String, Value> {
            self.items[kind].as_object_mut().unwrap()
        }

        fn call(&mut self, method: &str, path: &str, body: Option<&Value>) -> Done<Value> {
            self.calls.push((method.into(), path.into(), body.cloned()));
            let (route, query) = path.split_once('?').unwrap_or((path, ""));
            let params: Map<String, Value> = query
                .split('&')
                .filter_map(|p| p.split_once('='))
                .map(|(k, v)| (k.to_string(), Value::from(v)))
                .collect();
            let parts: Vec<&str> = route.split('/').collect();
            if parts == ["accounts", ACCOUNT] && method == "GET" {
                return Ok(json!({"success": true, "result": {"id": ACCOUNT}}));
            }
            if parts == ["zones"] {
                let zones = if params.get("name") == Some(&json!("example.test")) {
                    json!([{"name": "example.test"}])
                } else {
                    json!([])
                };
                return Ok(json!({"success": true, "result": zones}));
            }
            if parts[..2] != ["accounts", ACCOUNT] {
                return err(
                    method,
                    path,
                    403,
                    9109,
                    "Unauthorized to access requested resource",
                );
            }
            let rest = &parts[2..];
            let kind = rest[1].to_string();
            if let Some(i) = self
                .fail
                .iter()
                .position(|(m, k)| m == method && *k == kind)
            {
                self.fail.remove(i);
                return err(method, path, 500, 10000, "synthetic failure");
            }
            if rest[..2] == ["r2", "buckets"] {
                if rest.len() == 2 && method == "POST" {
                    let name = body.unwrap()["name"].as_str().unwrap().to_string();
                    self.buckets.insert(name.clone(), json!(0));
                    return Ok(json!({"success": true, "result": {"name": name}}));
                }
                let name = rest[2];
                let Some(count) = self.buckets.get(name).cloned() else {
                    return err(
                        method,
                        path,
                        404,
                        10006,
                        "The specified bucket does not exist.",
                    );
                };
                if method == "DELETE" {
                    if count != json!(0) {
                        return err(
                            method,
                            path,
                            409,
                            10008,
                            "The bucket you tried to delete is not empty.",
                        );
                    }
                    self.buckets.remove(name);
                }
                return Ok(json!({"success": true, "result": {"name": name}}));
            }
            if rest == ["access", "organizations"] {
                return match &self.org {
                    None => err(method, path, 404, 12130, "access.api.error.not_found"),
                    Some(org) => Ok(json!({"success": true, "result": {"auth_domain": org}})),
                };
            }
            if rest.len() == 3 && rest[..2] == ["workers", "scripts"] && method == "DELETE" {
                let Some(i) = self.scripts.iter().position(|s| s == rest[2]) else {
                    return err(
                        method,
                        path,
                        404,
                        10007,
                        "This Worker does not exist on your account.",
                    );
                };
                self.scripts.remove(i);
                return Ok(json!({"success": true, "result": null}));
            }
            if method == "GET" && rest.len() == 2 {
                let found: Vec<Value> = self
                    .store(&kind)
                    .values()
                    .filter(|item| {
                        kind != "domains" || item.get("hostname") == params.get("hostname")
                    })
                    .map(|item| {
                        let mut item = item.clone();
                        item.as_object_mut().unwrap().shift_remove("client_secret");
                        item
                    })
                    .collect();
                return Ok(json!({"success": true, "result": found}));
            }
            if method == "POST" {
                self.next_id += 1;
                let id = format!("id-{}", self.next_id);
                let mut item = body.unwrap().clone();
                item["id"] = json!(id);
                if kind == "service_tokens" {
                    item["client_id"] = json!(format!("client-{id}.access"));
                    item["client_secret"] = json!(SECRET);
                }
                if kind == "apps" {
                    item["aud"] = json!(format!("aud-{id}"));
                }
                self.store(&kind).insert(id, item.clone());
                return Ok(json!({"success": true, "result": item}));
            }
            let id = rest[rest.len() - 1].to_string();
            let Some(current) = self.store(&kind).get(&id).cloned() else {
                return err(method, path, 404, 12130, "not found");
            };
            if method == "GET" {
                return Ok(json!({"success": true, "result": current}));
            }
            if method == "PUT" {
                let mut item = body.unwrap().clone();
                item["id"] = json!(id);
                if kind == "apps" {
                    item["aud"] = current["aud"].clone();
                }
                self.store(&kind).insert(id, item.clone());
                return Ok(json!({"success": true, "result": item}));
            }
            let used = json!({"service_token": {"token_id": id}});
            if kind == "service_tokens"
                && self.items["policies"]
                    .as_object()
                    .unwrap()
                    .values()
                    .any(|p| p["include"].as_array().is_some_and(|i| i.contains(&used)))
            {
                return err(method, path, 400, 12139, "access.api.error.service_token_in_use: cannot delete service token because it is used by a policy, group, or app SCIM configuration");
            }
            self.store(&kind).shift_remove(&id);
            if kind == "domains" {
                return Ok(json!({}));
            }
            Ok(json!({"success": true, "result": {"id": id}}))
        }

        fn methods(&self, since: usize) -> Vec<String> {
            self.calls[since..].iter().map(|c| c.0.clone()).collect()
        }
    }

    struct T {
        tmp: PathBuf,
        root: PathBuf,
        home: PathBuf,
        fake: Rc<RefCell<Fake>>,
        runs: Rc<RefCell<Vec<(Vec<String>, Option<PathBuf>, Vec<(String, String)>)>>>,
    }

    impl T {
        fn new(name: &str) -> T {
            let tmp = temp_dir(name);
            let root = tmp.join("checkout");
            fs::create_dir_all(root.join("tools")).unwrap();
            fs::write(root.join("tools").join("deploy.mjs"), "").unwrap();
            T {
                home: tmp.join("home"),
                root,
                tmp,
                fake: Rc::new(RefCell::new(Fake::new())),
                runs: Rc::default(),
            }
        }

        /// One run: (exit code, stdout, stderr).
        fn main(
            &self,
            args: &[&str],
            inputs: &[&str],
            device: &str,
            home: Option<&Path>,
            token_env: bool,
        ) -> (i32, String, String) {
            let home = home.unwrap_or(&self.home).to_path_buf();
            let mut answers: Vec<String> = inputs.iter().rev().map(|s| s.to_string()).collect();
            let mut out = Vec::new();
            let mut input = |out: &mut dyn Write, prompt: &str| -> Option<String> {
                let answer = answers.pop()?;
                let _ = writeln!(out, "{prompt}{answer}");
                Some(answer)
            };
            let mut secret = |out: &mut dyn Write, prompt: &str| -> Option<String> {
                let _ = writeln!(out, "{prompt}");
                Some(API_TOKEN.to_string())
            };
            let (fake, runs) = (self.fake.clone(), self.runs.clone());
            let mut run = move |argv: &[String],
                                cwd: Option<&Path>,
                                _captured: bool,
                                env: &[(&str, &str)]|
                  -> Out {
                let env = env
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                runs.borrow_mut()
                    .push((argv.to_vec(), cwd.map(Path::to_path_buf), env));
                let ok = |stdout: String| Out {
                    code: 0,
                    stdout,
                    stderr: String::new(),
                };
                if argv.iter().any(|a| a == "init") {
                    fs::create_dir_all(&home).unwrap();
                    let env = home.join(".env");
                    if !env.exists() {
                        write_atomic(
                            &env,
                            b"FACTORY_ACCESS_CLIENT_ID=\nFACTORY_ACCESS_CLIENT_SECRET=\n",
                        )
                        .unwrap();
                    }
                    return ok(format!("factory home: {}\n", home.display()));
                }
                let at = argv.iter().position(|a| a == "--profile").unwrap();
                let profile = loads(&fs::read_to_string(&argv[at + 1]).unwrap()).unwrap();
                let mut fake = fake.borrow_mut();
                let worker = str_of(&profile, "worker_name").to_string();
                if !fake.scripts.contains(&worker) {
                    fake.scripts.push(worker);
                }
                if fake.store("domains").is_empty() {
                    let host = str_of(&profile, "origin")
                        .trim_start_matches("https://")
                        .to_string();
                    fake.store("domains")
                        .insert("dom-1".into(), json!({"id": "dom-1", "hostname": host}));
                }
                ok(String::new())
            };
            let mut host = Host {
                out: &mut out,
                input: &mut input,
                secret: &mut secret,
                run: &mut run,
                device: device.into(),
                token_env: token_env.then(|| API_TOKEN.to_string()),
                exe: PathBuf::from("/opt/skill/runtime/factory"),
            };
            let fake = self.fake.clone();
            let mut api =
                move |_token: &str| -> Box<dyn FnMut(&str, &str, Option<&Value>) -> Done<Value>> {
                    let fake = fake.clone();
                    Box::new(move |m, p, b| fake.borrow_mut().call(m, p, b))
                };
            let (code, err) = run_bootstrap(args, &self.root, &mut host, &mut api);
            (code, String::from_utf8(out).unwrap(), err)
        }

        fn up(&self, inputs: &[&str]) -> (i32, String, String) {
            self.main(&["up"], inputs, "laptop", None, true)
        }

        fn up_desk(&self) -> (i32, String, String) {
            self.main(
                &["up"],
                &AGAIN,
                "desk-top",
                Some(&self.tmp.join("home2")),
                true,
            )
        }

        fn down(&self, typed: &str) -> (i32, String, String) {
            self.main(&["down"], &[typed], "laptop", None, true)
        }

        fn items(&self, kind: &str) -> Map<String, Value> {
            self.fake.borrow().items[kind].as_object().unwrap().clone()
        }

        fn policy_ids(&self) -> Map<String, Value> {
            self.items("policies")
                .values()
                .map(|p| (str_of(p, "name").to_string(), p["id"].clone()))
                .collect()
        }

        fn app(&self) -> Value {
            let apps = self.items("apps");
            assert_eq!(apps.len(), 1);
            apps.values().next().unwrap().clone()
        }

        fn files(&self) -> Vec<(PathBuf, Vec<u8>)> {
            let mut files = Vec::new();
            for base in [&self.root, &self.home] {
                let mut stack = vec![base.to_path_buf()];
                while let Some(dir) = stack.pop() {
                    for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            stack.push(p)
                        } else {
                            files.push((p.clone(), fs::read(&p).unwrap()))
                        }
                    }
                }
            }
            files.sort();
            files
        }
    }

    fn fixture() -> Value {
        loads(
            &fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/bootstrap-valid.json"
            ))
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn plan_bad_answers_and_usage() {
        let t = T::new("bootstrap-plan");
        let plan = |doc: &Value| {
            let path = t.tmp.join("answers.json");
            fs::write(&path, dumps(doc, DEFAULT)).unwrap();
            t.main(
                &["plan", "--answers", path.to_str().unwrap()],
                &[],
                "laptop",
                None,
                true,
            )
        };
        let (code, out, _) = plan(&fixture());
        assert_eq!(code, 0);
        assert!(out.contains("worker   factory-example-test\n"));
        assert!(out.contains("app      factory.example.test bootstrap\n"));
        assert!(out.contains("policy   factory.example.test bootstrap people (ada@example.test, grace@example.test)\n"));
        for (change, message) in [
            (
                json!({"hostname": "factory.other.test"}),
                "is not inside zone",
            ),
            (json!({"hostname": "example.test"}), "is not inside zone"),
            (
                json!({"emails": ["ada.example.test"]}),
                "not an email address",
            ),
            (json!({"emails": []}), "emails must be a nonempty list"),
            (
                json!({"account_id": "0".repeat(31)}),
                "account_id must be 32 hexadecimal",
            ),
            (
                json!({"account_id": "A".repeat(32)}),
                "account_id must be 32 hexadecimal",
            ),
            (json!({"zone": "exa mple.test"}), "zone"),
            (json!({"version": 2}), "version must be the number 1"),
            (json!({"token": "x"}), "answers need exactly the keys"),
            (
                json!({"hostname": format!("{}.example.test", "a".repeat(52))}),
                "is longer than 63 characters",
            ),
        ] {
            let mut doc = fixture();
            for (k, v) in change.as_object().unwrap() {
                doc[k] = v.clone();
            }
            let (code, out, err) = plan(&doc);
            assert_eq!((code, out.as_str()), (2, ""), "{change}");
            assert!(err.contains(message), "{err}");
        }
    }

    #[test]
    fn names_and_device_label() {
        assert_eq!(device_label("Ada's MacBook.local"), "ada-s-macbook-local");
        let n = names("factory.example.test", "laptop");
        assert_eq!(
            [
                n.worker.as_str(),
                &n.bucket,
                &n.app,
                &n.people_policy,
                &n.device_policy,
                &n.token
            ],
            [
                "factory-example-test",
                "factory-example-test",
                "factory.example.test bootstrap",
                "factory.example.test bootstrap people",
                "factory.example.test bootstrap device laptop",
                "factory.example.test bootstrap device laptop"
            ]
        );
        let (dot, dash) = (
            names("a.b.example.test", "laptop"),
            names("a-b.example.test", "laptop"),
        );
        assert_eq!(
            (dot.worker.as_str(), dash.worker.as_str()),
            ("a-b-example-test", "a--b-example-test")
        );
        assert!(!is_device_name(&dot, &dash.token) && !is_device_name(&dash, &dot.token));
    }

    #[test]
    fn fresh_up_creates_everything_and_deploys() {
        let t = T::new("bootstrap-fresh");
        let (code, out, err) = t.up(&ANSWERS);
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            t.fake.borrow().buckets,
            json!({"factory-example-test": 0})
                .as_object()
                .unwrap()
                .clone()
        );
        let tokens = t.items("service_tokens");
        let token = tokens.values().next().unwrap();
        assert_eq!(
            token["name"],
            "factory.example.test bootstrap device laptop"
        );
        let env = read_env(&t.home.join(".env"));
        assert_eq!(env["FACTORY_ACCESS_CLIENT_ID"], token["client_id"]);
        assert_eq!(env["FACTORY_ACCESS_CLIENT_SECRET"], SECRET);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(t.home.join(".env"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let ids = t.policy_ids();
        let app = t.app();
        assert_eq!(
            app["policies"],
            json!([{"id": ids["factory.example.test bootstrap people"], "precedence": 1}, {"id": ids["factory.example.test bootstrap device laptop"], "precedence": 2}])
        );
        let profile =
            loads(&fs::read_to_string(t.root.join("deploy/profile.json")).unwrap()).unwrap();
        assert_eq!(
            profile,
            json!({"version": 1, "account_id": ACCOUNT, "worker_name": "factory-example-test", "bucket_name": "factory-example-test", "origin": "https://factory.example.test", "title": "Factory", "issuer": "https://example.cloudflareaccess.com", "audience": app["aud"], "image_origins": [], "agent_links": {}})
        );
        let saved =
            loads(&fs::read_to_string(t.root.join("deploy/bootstrap.json")).unwrap()).unwrap();
        assert_eq!(saved, fixture());
        assert!(out.contains("Factory is live at https://factory.example.test"));
        assert!(out
            .trim_end()
            .lines()
            .last()
            .unwrap()
            .starts_with("Store this API token"));
        assert!(!out.contains(API_TOKEN) && !out.contains(SECRET));
        let runs = t.runs.borrow();
        assert_eq!(
            runs[0].0[1..3],
            ["init".to_string(), "--connection".to_string()]
        );
        assert_ne!(runs[0].1.as_deref(), Some(t.root.as_path()));
        assert_eq!(
            runs[1].0,
            [
                "node".to_string(),
                t.root.join("tools/deploy.mjs").to_string_lossy().into(),
                "deploy".into(),
                "--profile".into(),
                t.root.join("deploy/profile.json").to_string_lossy().into()
            ]
        );
        let token = [("CLOUDFLARE_API_TOKEN".to_string(), API_TOKEN.to_string())];
        assert!(runs[0].2.is_empty(), "init gets no token");
        assert_eq!(runs[1].2, token, "the deploy child gets the token");
        let posts: Vec<String> = t
            .fake
            .borrow()
            .calls
            .iter()
            .filter(|c| c.0 == "POST")
            .map(|c| c.1.rsplit('/').next().unwrap().to_string())
            .collect();
        assert_eq!(
            posts,
            ["buckets", "service_tokens", "policies", "policies", "apps"]
        );
    }

    #[test]
    fn first_run_prompts_in_order_and_defaults_the_hostname() {
        let t = T::new("bootstrap-prompts");
        let (code, out, err) = t.main(
            &["up"],
            &[ACCOUNT, "example.test", "", "ada@example.test"],
            "laptop",
            None,
            false,
        );
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            t.runs.borrow()[1].2,
            [("CLOUDFLARE_API_TOKEN".to_string(), API_TOKEN.to_string())],
            "a token typed at the prompt reaches the deploy child"
        );
        let expected = [
            "Your account ID is on any domain's Overview page",
            &format!("Cloudflare account ID: {ACCOUNT}"),
            "Zone, for example example.test: example.test",
            "Factory hostname [factory.example.test]: \n",
            "Allowed emails, comma separated: ada@example.test",
            "Create an API token at https://dash.cloudflare.com/profile/api-tokens",
            "Set CLOUDFLARE_API_TOKEN to skip this prompt.",
            "Cloudflare API token: ",
        ];
        let positions: Vec<usize> = expected
            .iter()
            .map(|e| out.find(e).unwrap_or_else(|| panic!("{e}")))
            .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{out}");
        assert!(!out.contains(API_TOKEN));
        let saved =
            loads(&fs::read_to_string(t.root.join("deploy/bootstrap.json")).unwrap()).unwrap();
        assert_eq!(saved["hostname"], "factory.example.test");
        let (_, out, _) = t.up(&AGAIN);
        assert!(!out.contains("api-tokens"));
    }

    #[test]
    fn bad_input_cancel_and_other_origin_exit_2_before_any_write() {
        let t = T::new("bootstrap-input");
        let (code, _, err) = t.up(&["not-an-account", "example.test", "", "ada@example.test"]);
        assert_eq!(code, 2);
        assert!(
            err.contains("account_id must be 32 hexadecimal characters")
                && err.contains("find-account-and-zone-ids")
        );
        assert_eq!(
            t.up(&[ACCOUNT]),
            (2, t.up(&[ACCOUNT]).1, "cancelled\n".into())
        );
        assert!(!t.root.join("deploy").exists());
        fs::create_dir_all(t.root.join("deploy")).unwrap();
        fs::write(
            t.root.join("deploy/profile.json"),
            dumps(
                &json!({"account_id": ACCOUNT, "origin": "https://other.example.test"}),
                DEFAULT,
            ),
        )
        .unwrap();
        let (code, _, err) = t.up(&ANSWERS);
        assert_eq!(code, 2);
        assert!(
            err.contains("origin") && err.contains("edit or move"),
            "{err}"
        );
        assert!(!t.fake.borrow().methods(0).contains(&"POST".to_string()));
        assert!(t.runs.borrow().is_empty());
        let (code, _, err) = t.main(&["apply"], &[], "laptop", None, true);
        assert_eq!(code, 2, "{err}");
    }

    #[test]
    fn rerun_changes_nothing_and_a_second_device_joins() {
        let t = T::new("bootstrap-rerun");
        assert_eq!(t.up(&ANSWERS).0, 0);
        let (before, since) = (t.files(), t.fake.borrow().calls.len());
        let (code, out, err) = t.up(&AGAIN);
        assert_eq!(code, 0, "{err}");
        assert!(!t.fake.borrow().methods(since).contains(&"POST".to_string()));
        assert_eq!(t.files(), before);
        assert!(out.contains("token factory.example.test bootstrap device laptop: kept"));
        assert!(out.contains("app factory.example.test: kept"));
        let mut app = t.app();
        app["session_duration"] = json!("1h");
        let id = app["id"].as_str().unwrap().to_string();
        t.fake.borrow_mut().store("apps").insert(id, app);
        let (code, _, err) = t.up_desk();
        assert_eq!(code, 0, "{err}");
        let ids = t.policy_ids();
        let app = t.app();
        let order: Vec<&Value> = app["policies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| &p["id"])
            .collect();
        assert_eq!(
            order,
            [
                &ids["factory.example.test bootstrap people"],
                &ids["factory.example.test bootstrap device laptop"],
                &ids["factory.example.test bootstrap device desk-top"]
            ]
        );
        assert_eq!(app["session_duration"], "1h");
        let fake = t.fake.borrow();
        let put = fake
            .calls
            .iter()
            .find(|c| c.0 == "PUT" && c.1.contains("/apps/"))
            .unwrap()
            .2
            .clone()
            .unwrap();
        assert!(put.get("id").is_none() && put.get("aud").is_none());
    }

    #[test]
    fn foreign_app_conflicting_token_and_missing_org_exit_1() {
        let t = T::new("bootstrap-refuse");
        t.fake.borrow_mut().store("apps").insert("mine".into(), json!({"id": "mine", "name": "Factory", "domain": "factory.example.test", "aud": "aud-mine", "policies": []}));
        let (code, _, err) = t.up(&ANSWERS);
        assert_eq!(code, 1);
        assert_eq!(err, "An Access application for factory.example.test already exists with the name \"Factory\", so bootstrap did not create it. Rename it to \"factory.example.test bootstrap\" to let bootstrap manage it, or delete it, then rerun.\n");
        assert_eq!(t.runs.borrow().len(), 1, "no deploy");

        let t = T::new("bootstrap-token");
        t.fake.borrow_mut().store("service_tokens").insert("old".into(), json!({"id": "old", "name": "factory.example.test bootstrap device laptop", "client_id": "client-old.access"}));
        let (code, _, err) = t.up(&ANSWERS);
        assert_eq!(code, 1);
        assert_eq!(err, format!("Service token factory.example.test bootstrap device laptop already exists and {} does not hold it. Delete it under Zero Trust > Access > Service credentials, then rerun.\n", t.home.join(".env").display()));
        assert_eq!(
            fs::read_to_string(t.home.join(".env")).unwrap(),
            "FACTORY_ACCESS_CLIENT_ID=\nFACTORY_ACCESS_CLIENT_SECRET=\n"
        );

        let t = T::new("bootstrap-org");
        t.fake.borrow_mut().org = None;
        let (code, _, err) = t.up(&ANSWERS);
        assert_eq!(
            (code, err.as_str()),
            (1, format!("{MISSING_ORG}\n").as_str())
        );
        assert!(t.items("service_tokens").is_empty());
    }

    #[test]
    fn embed_origins_set_same_site_none_once_and_bad_ones_stop_first() {
        let t = T::new("bootstrap-embed");
        t.up(&ANSWERS);
        let path = t.root.join("deploy/profile.json");
        let set = |origins: Value| {
            let mut profile = loads(&fs::read_to_string(&path).unwrap()).unwrap();
            profile["embed_origins"] = origins;
            fs::write(&path, dumps(&profile, DEFAULT)).unwrap();
        };
        set(json!(["https://paseo.example.test"]));
        let since = t.fake.borrow().calls.len();
        assert_eq!(t.up(&AGAIN).0, 0);
        assert_eq!(t.app()["same_site_cookie_attribute"], "none");
        assert_eq!(
            t.fake.borrow().calls[since..]
                .iter()
                .filter(|c| c.0 == "PUT" && c.1.contains("/apps/"))
                .count(),
            1
        );
        let since = t.fake.borrow().calls.len();
        assert!(t.up(&AGAIN).1.contains("app factory.example.test: kept"));
        assert!(!t.fake.borrow().calls[since..]
            .iter()
            .any(|c| c.0 == "PUT" && c.1.contains("/apps/")));
        set(json!(["http://paseo.example.test"]));
        let since = t.fake.borrow().calls.len();
        let (code, _, err) = t.up(&AGAIN);
        assert_eq!(code, 2);
        assert!(err.contains("embed_origins entry must be a canonical https origin"));
        assert_eq!(t.fake.borrow().calls.len(), since);
    }

    #[test]
    fn down_deletes_in_order_keeps_foreign_items_and_finishes_on_a_rerun() {
        let t = T::new("bootstrap-down");
        t.up(&ANSWERS);
        let since = t.fake.borrow().calls.len();
        let (code, _, err) = t.down("factory.example");
        assert_eq!(code, 2);
        assert!(err.contains("nothing was deleted"));
        assert!(!t
            .fake
            .borrow()
            .methods(since)
            .contains(&"DELETE".to_string()));
        t.up_desk();
        t.fake
            .borrow_mut()
            .buckets
            .insert("factory-example-test".into(), json!(3));
        t.fake.borrow_mut().store("policies").insert("foreign".into(), json!({"id": "foreign", "name": "factory.example.test bootstrap device evil", "decision": "allow", "include": []}));
        t.fake.borrow_mut().fail = vec![("DELETE".into(), "service_tokens".into())];
        let files = t.files();
        let (code, _, err) = t.down("factory.example.test");
        assert_eq!(code, 1);
        assert!(err.contains("synthetic failure"));
        assert_eq!(t.items("service_tokens").len(), 2);
        let since = t.fake.borrow().calls.len();
        let (code, out, err) = t.down("factory.example.test");
        assert_eq!(code, 0, "{err}");
        let deletes: Vec<String> = t.fake.borrow().calls[since..]
            .iter()
            .filter(|c| c.0 == "DELETE")
            .map(|c| {
                c.1.split('?')
                    .next()
                    .unwrap()
                    .rsplit('/')
                    .nth(1)
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            deletes,
            ["scripts", "service_tokens", "service_tokens", "buckets"]
        );
        assert!(out.contains("token factory.example.test bootstrap device desk-top: deleted"));
        assert!(out.contains(&format!("bucket factory-example-test: kept, Cloudflare said DELETE accounts/{ACCOUNT}/r2/buckets/factory-example-test: 10008 The bucket you tried to delete is not empty.")));
        assert_eq!(t.items("policies").keys().collect::<Vec<_>>(), ["foreign"]);
        assert!(
            t.items("apps").is_empty()
                && t.items("domains").is_empty()
                && t.items("service_tokens").is_empty()
        );
        assert!(t.fake.borrow().scripts.is_empty());
        assert_eq!(t.files(), files, "local files stay");
        t.fake.borrow_mut().buckets.clear();
        assert!(t
            .down("factory.example.test")
            .1
            .contains("bucket factory-example-test: already deleted"));
    }

    #[test]
    fn client_sends_its_headers_and_reports_errors_on_one_line() {
        let (api, seen) = crate::shared::http::tests::serve(vec![
            (200, r#"{"success": true, "result": []}"#, vec![]),
            (
                403,
                r#"{"errors": [{"code": 10000, "message": "Authentication error"}]}"#,
                vec![],
            ),
            (200, "", vec![]),
        ]);
        let path = format!("accounts/{ACCOUNT}");
        assert_eq!(
            cloudflare(&api, API_TOKEN, "GET", &path, None).unwrap(),
            json!({"success": true, "result": []})
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].path, format!("/{path}"));
        assert_eq!(seen[0].header("user-agent"), Some("factory-bootstrap"));
        assert_eq!(
            seen[0].header("authorization"),
            Some(&*format!("Bearer {API_TOKEN}"))
        );
        drop(seen);
        assert_eq!(
            cloudflare(&api, API_TOKEN, "GET", &path, None),
            Err(Stop::Cloudflare(
                format!("GET {path}: 10000 Authentication error"),
                Some(403)
            ))
        );
        assert_eq!(
            cloudflare(&api, API_TOKEN, "DELETE", &path, None).unwrap(),
            json!({})
        );
        let Err(Stop::Cloudflare(message, None)) =
            cloudflare("http://127.0.0.1:9", API_TOKEN, "GET", &path, None)
        else {
            panic!("an unreachable API is a Cloudflare stop with no status");
        };
        assert!(
            message.starts_with(&format!("GET {path}: cannot reach api.cloudflare.com: ")),
            "{message}"
        );
    }
}
