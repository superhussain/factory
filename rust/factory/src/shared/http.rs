//! The factory Worker client: Python's `normalized_origin`, `api_url`, `api` and `cas`.

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use ureq::tls::{PemItem, RootCerts, TlsConfig, TlsProvider};
use ureq::Agent;

use super::conf::{fm_get, frontmatter, read_conf};
use super::consts::{FALLBACK_CA_FILE, READ_CACHED, READ_TTL};
use super::exit::{fail, Exit, Res};
use super::py::{os_error, printable_line};

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// The first header with this name, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The body as strict UTF-8.
    pub fn text(&self) -> Res<String> {
        String::from_utf8(self.body.clone())
            .map_err(|_| fail("the Worker sent a body that is not UTF-8", 1))
    }
}

/// `int(s, 10)` for a port: optional sign, ASCII digits, single underscores between digits.
fn parse_port(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let n: i64 = digits.replace('_', "").parse().ok()?;
    Some(if s.starts_with('-') { -n } else { n })
}

/// Python's `normalized_origin`: an https origin, or http for localhost and 127.0.0.1 tests.
pub fn normalized_origin(url: &str, https_only: bool) -> Result<String, ()> {
    if url.is_empty() || url.chars().any(|c| c as u32 <= 32 || c as u32 == 127) {
        return Err(());
    }
    if url.contains(['\\', '?', '#']) {
        return Err(());
    }
    let mut scheme = String::new();
    let mut rest = url;
    if let Some(i) = url.find(':').filter(|&i| i > 0) {
        let head = &url[..i];
        let tail = &url[i + 1..];
        if head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
            && (tail.is_empty() || !tail.chars().all(|c| c.is_ascii_digit()))
        {
            scheme = head.to_ascii_lowercase();
            rest = tail;
        }
    }
    let (netloc, path) = match rest.strip_prefix("//") {
        Some(after) => after.split_at(after.find('/').unwrap_or(after.len())),
        None => ("", rest),
    };
    if netloc.contains('[') != netloc.contains(']') || netloc.contains('@') || netloc.ends_with(':')
    {
        return Err(());
    }
    let (host, port) = match netloc.strip_prefix('[') {
        Some(bracketed) => {
            let (host, after) = bracketed.split_once(']').ok_or(())?;
            (host, after.strip_prefix(':').unwrap_or(""))
        }
        None => netloc.split_once(':').unwrap_or((netloc, "")),
    };
    let host = host.to_lowercase();
    if host.is_empty() || !(path.is_empty() || path == "/") {
        return Err(());
    }
    if !port.is_empty() {
        let port = parse_port(port).ok_or(())?;
        if !(1..=65535).contains(&port) {
            return Err(());
        }
    }
    if scheme != "https"
        && !(!https_only && scheme == "http" && (host == "localhost" || host == "127.0.0.1"))
    {
        return Err(());
    }
    Ok(url.strip_suffix('/').unwrap_or(url).to_string())
}

pub fn endpoint_message(config: &Path) -> String {
    format!(
        "api_url in {} must be an HTTPS origin (HTTP is allowed only for localhost or 127.0.0.1 tests).",
        config.display()
    )
}

pub fn token_message(env_file: &Path) -> String {
    format!(
        "Set FACTORY_ACCESS_CLIENT_ID and FACTORY_ACCESS_CLIENT_SECRET in {}, then run /factory init again.",
        env_file.display()
    )
}

pub fn access_message(env_file: &Path) -> String {
    format!(
        "Access rejected the service token; check FACTORY_ACCESS_CLIENT_ID/SECRET in {}",
        env_file.display()
    )
}

/// Python's `api_url`: config.md's `api_url`, normalized.
pub fn api_url(home: &Path) -> Res<String> {
    let config = home.join("config.md");
    let url = if config.exists() {
        let meta = frontmatter(&config)
            .map_err(|e| fail(os_error(&e, Some(&config)), 1))?
            .0;
        fm_get(&meta, "api_url").unwrap_or("").to_string()
    } else {
        String::new()
    };
    if url.is_empty() {
        return Err(fail(
            format!(
                "Set api_url in {}, then run /factory init again.",
                config.display()
            ),
            2,
        ));
    }
    normalized_origin(&url, false).map_err(|_| fail(endpoint_message(&config), 2))
}

/// `urllib.parse.quote(s, safe="/")`.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The proxy urllib would use for this origin: `<scheme>_proxy` (lowercase first), unless `no_proxy`
/// names the host. `ALL_PROXY` and the other scheme's proxy are ignored, as urllib ignores them.
fn proxy_url(origin: &str, var: impl Fn(&str) -> Option<String>) -> Option<String> {
    let (scheme, host) = origin.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let env = |name: &str| {
        var(&name.to_lowercase())
            .or_else(|| var(&name.to_uppercase()))
            .filter(|v| !v.is_empty())
    };
    let proxy = env(&format!("{scheme}_proxy"))?;
    let no_proxy = env("no_proxy").unwrap_or_default();
    if no_proxy == "*" {
        return None;
    }
    let host_only = match host.rsplit_once(':') {
        Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    };
    let names_host = |h: &str, name: &str| {
        let (h, name) = (h.to_lowercase(), name.to_lowercase());
        h == name || h.len() > name.len() + 1 && h.ends_with(&format!(".{name}"))
    };
    for name in no_proxy
        .split(',')
        .map(str::trim)
        .map(|n| n.trim_start_matches('.'))
    {
        if !name.is_empty() && (names_host(host_only, name) || names_host(host, name)) {
            return None;
        }
    }
    Some(proxy)
}

fn new_agent(roots: RootCerts, origin: &str) -> Agent {
    let tls = TlsConfig::builder()
        .provider(TlsProvider::NativeTls)
        .root_certs(roots)
        .build();
    let proxy = proxy_url(origin, |name| std::env::var(name).ok())
        .and_then(|url| ureq::Proxy::new(&url).ok());
    let limit = Some(Duration::from_secs(30));
    Agent::config_builder()
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .user_agent("factory")
        .accept("")
        .proxy(proxy)
        .timeout_connect(limit)
        .timeout_send_request(limit)
        .timeout_recv_response(limit)
        .timeout_per_read(limit)
        .timeout_per_write(limit)
        .tls_config(tls)
        .build()
        .new_agent()
}

fn platform_agent(origin: &str) -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| new_agent(RootCerts::PlatformVerifier, origin))
}

fn fallback_agent(origin: &str) -> Option<Agent> {
    let pem = std::fs::read(FALLBACK_CA_FILE).ok()?;
    let certs: Vec<_> = ureq::tls::parse_pem(&pem)
        .filter_map(|item| match item {
            Ok(PemItem::Certificate(cert)) => Some(cert),
            _ => None,
        })
        .collect();
    Some(new_agent(RootCerts::new_with_certs(&certs), origin))
}

type Cache = Mutex<HashMap<(String, String), (Instant, Response)>>;

fn reads() -> &'static Cache {
    static READS: OnceLock<Cache> = OnceLock::new();
    READS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn chain(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}

fn cert_error(e: &ureq::Error) -> bool {
    let text = chain(e);
    ["certificate verify failed", "UnknownIssuer", "not trusted"]
        .iter()
        .any(|s| text.contains(s))
}

fn host_not_found() -> &'static str {
    if cfg!(windows) {
        "<urlopen error [Errno 11001] getaddrinfo failed>"
    } else if cfg!(target_os = "macos") {
        "<urlopen error [Errno 8] nodename nor servname provided, or not known>"
    } else {
        "<urlopen error [Errno -2] Name or service not known>"
    }
}

/// What Python's urllib prints for a failed request, on one line of at most 200 characters.
pub fn urlopen_error(e: &ureq::Error) -> String {
    let detail = match e {
        ureq::Error::Io(io) if io.raw_os_error().is_some() => {
            format!("<urlopen error {}>", os_error(io, None))
        }
        ureq::Error::Timeout(ureq::Timeout::Connect) => "<urlopen error timed out>".to_string(),
        ureq::Error::Timeout(_) => "timed out".to_string(),
        ureq::Error::HostNotFound => host_not_found().to_string(),
        other => format!("<urlopen error {other}>"),
    };
    printable_line(&detail, 200)
}

fn send(
    agent: &Agent,
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Result<Response, ureq::Error> {
    let mut request = ureq::http::Request::builder().method(method).uri(url);
    for (name, value) in headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = match body {
        Some(body) => agent.run(request.body(body).map_err(ureq::Error::Http)?)?,
        None => agent.run(request.body(()).map_err(ureq::Error::Http)?)?,
    };
    let status = response.status().as_u16();
    let got = response
        .headers()
        .iter()
        // http.client decodes header values as Latin-1.
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.as_bytes().iter().map(|&b| b as char).collect(),
            )
        })
        .collect();
    let body = response
        .into_body()
        .with_config()
        .limit(u64::MAX)
        .read_to_vec()?;
    Ok(Response {
        status,
        headers: got,
        body,
    })
}

/// Python's `api`: one request to the factory Worker. Fails 5 on an auth or network failure or a
/// status outside ok. The service token goes only into the request headers.
pub fn api(
    home: &Path,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    ok: &[u16],
    headers: &[(&str, &str)],
) -> Res<Response> {
    let env_file = home.join(".env");
    let env = if env_file.exists() {
        read_conf(&env_file).map_err(|e| fail(os_error(&e, Some(&env_file)), 1))?
    } else {
        Default::default()
    };
    let mut sent: Vec<(String, String)> = vec![("User-Agent".into(), "factory".into())];
    let mut set = |name: &str, value: &str| {
        sent.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        sent.push((name.to_string(), value.to_string()));
    };
    for (name, value) in headers {
        set(name, value);
    }
    for (header, var) in [
        ("CF-Access-Client-Id", "FACTORY_ACCESS_CLIENT_ID"),
        ("CF-Access-Client-Secret", "FACTORY_ACCESS_CLIENT_SECRET"),
    ] {
        if let Some(value) = env.get(var).filter(|v| !v.is_empty()) {
            set(header, value);
        }
    }
    // urllib gives every request with a body this Content-Type unless the caller set one.
    if body.is_some()
        && !sent
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("Content-Type"))
    {
        sent.push((
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
    }
    let origin = api_url(home)?;
    let url = format!("{origin}{path}");
    let key = (origin, path.to_string());
    let cached = method == "GET" && READ_CACHED.is_full(path);
    if method != "GET" && (path.starts_with("/api/pipeline/") || path.starts_with("/api/projects/"))
    {
        reads().lock().unwrap().clear();
    } else if cached {
        if let Some((at, hit)) = reads().lock().unwrap().get(&key) {
            if at.elapsed() < Duration::from_secs(READ_TTL) {
                return Ok(hit.clone());
            }
        }
    }
    let mut result = send(platform_agent(&key.0), method, &url, &sent, body);
    if result.as_ref().is_err_and(cert_error) {
        let ssl_env = std::env::var_os("SSL_CERT_FILE").is_some_and(|v| !v.is_empty())
            || std::env::var_os("SSL_CERT_DIR").is_some_and(|v| !v.is_empty());
        let fallback = if Path::new(FALLBACK_CA_FILE).is_file() && !ssl_env {
            fallback_agent(&key.0)
        } else {
            None
        };
        if let Some(agent) = fallback {
            result = send(&agent, method, &url, &sent, body);
        }
    }
    if let Err(e) = &result {
        let hint = if cert_error(e) {
            ". Set SSL_CERT_FILE to your system CA bundle and retry."
        } else {
            ""
        };
        return Err(fail(
            format!("can't reach {url}: {}{hint}", urlopen_error(e)),
            5,
        ));
    }
    let response = result.unwrap();
    let status = response.status;
    if !ok.contains(&status) && [301, 302, 303, 307, 308, 401, 403].contains(&status) {
        return Err(fail(access_message(&env_file), 5));
    }
    if !ok.contains(&status) {
        let detail = printable_line(&String::from_utf8_lossy(&response.body), 2000);
        let short: String = detail.chars().take(200).collect();
        return Err(Exit {
            code: 5,
            stderr: format!("factory.py: {method} {url} returned {status}: {short}\n"),
            status: Some((status, detail)),
        });
    }
    if cached && status == 200 {
        reads()
            .lock()
            .unwrap()
            .insert(key, (Instant::now(), response.clone()));
    }
    Ok(response)
}

fn jitter() -> Duration {
    let mut h = RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    Duration::from_millis(50 + h.finish() % 451)
}

/// Python's `cas`: read-modify-write one R2 file, retrying when another device wrote it first.
/// change(text, or None if absent) returns (new text or None to skip the write, result).
pub fn cas<T>(
    home: &Path,
    path: &str,
    mut change: impl FnMut(Option<&str>) -> Res<(Option<String>, T)>,
) -> Res<T> {
    for _ in 0..20 {
        let read = api(home, "GET", path, None, &[200, 404], &[])?;
        let cond = if read.status == 200 {
            let etag = read
                .header("ETag")
                .ok_or_else(|| fail(format!("{path}: the Worker sent no ETag"), 1))?;
            (
                "If-Match",
                etag.strip_prefix("W/").unwrap_or(etag).to_string(),
            )
        } else {
            ("If-None-Match", "*".to_string())
        };
        let text = if read.status == 200 {
            Some(read.text()?)
        } else {
            None
        };
        let (new, result) = change(text.as_deref())?;
        let Some(new) = new else { return Ok(result) };
        if api(
            home,
            "PUT",
            path,
            Some(new.as_bytes()),
            &[200, 412],
            &[(cond.0, &cond.1)],
        )?
        .status
            == 200
        {
            return Ok(result);
        }
        std::thread::sleep(jitter());
    }
    Err(fail(
        format!("{path} kept changing under us; gave up after 20 tries"),
        5,
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    pub(crate) struct Seen {
        pub method: String,
        pub path: String,
        pub headers: Vec<(String, String)>,
        pub body: Vec<u8>,
    }

    impl Seen {
        pub(crate) fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        }
    }

    /// A loopback server that answers each request with the next reply, as raw HTTP/1.0.
    pub(crate) fn serve(
        replies: Vec<(u16, &'static str, Vec<(&'static str, &'static str)>)>,
    ) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for (status, body, extra) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, path) = (
                    parts.next().unwrap().to_string(),
                    parts.next().unwrap().to_string(),
                );
                let mut headers = Vec::new();
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    let (k, v) = h.split_once(':').unwrap();
                    headers.push((k.to_string(), v.trim().to_string()));
                }
                let length = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .map_or(0, |(_, v)| v.parse().unwrap());
                let mut data = vec![0; length];
                reader.read_exact(&mut data).unwrap();
                log.lock().unwrap().push(Seen {
                    method,
                    path,
                    headers,
                    body: data,
                });
                let mut head = format!("HTTP/1.0 {status} X\r\n");
                for (k, v) in extra {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                stream
                    .write_all(format!("{head}\r\n{body}").as_bytes())
                    .unwrap();
            }
        });
        (origin, seen)
    }

    pub(crate) fn home_for(name: &str, origin: &str) -> std::path::PathBuf {
        let home = temp_dir(name);
        std::fs::write(
            home.join("config.md"),
            format!("---\napi_url: {origin}\n---\n"),
        )
        .unwrap();
        std::fs::write(
            home.join(".env"),
            "FACTORY_ACCESS_CLIENT_ID=test-id\nFACTORY_ACCESS_CLIENT_SECRET=test-secret\n",
        )
        .unwrap();
        home
    }

    #[test]
    fn redirect_is_an_access_failure() {
        let (origin, _) = serve(vec![(
            302,
            "",
            vec![("Location", "https://example.invalid/login")],
        )]);
        let home = home_for("http-302", &origin);
        let e = api(&home, "GET", "/api/board", None, &[200], &[]).unwrap_err();
        assert_eq!(e.code, 5);
        assert_eq!(
            e.stderr,
            format!("factory.py: {}\n", access_message(&home.join(".env")))
        );
        assert!(!e.stderr.contains("test-secret"));
    }

    #[test]
    fn status_outside_ok_keeps_the_detail() {
        let (origin, seen) = serve(vec![(500, "boom\nbang", vec![])]);
        let home = home_for("http-500", &origin);
        let e = api(&home, "GET", "/api/board", None, &[200], &[]).unwrap_err();
        assert_eq!(
            e.stderr,
            format!("factory.py: GET {origin}/api/board returned 500: boom bang\n")
        );
        assert_eq!(e.status, Some((500, "boom bang".to_string())));
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].header("CF-Access-Client-Id"), Some("test-id"));
        assert_eq!(
            seen[0].header("CF-Access-Client-Secret"),
            Some("test-secret")
        );
        assert_eq!(seen[0].header("User-Agent"), Some("factory"));
        assert_eq!(seen[0].header("Accept"), None);
        assert_eq!(seen[0].header("Content-Type"), None);
    }

    #[test]
    fn body_gets_urllib_content_type() {
        let (origin, seen) = serve(vec![(200, "", vec![]), (200, "", vec![])]);
        let home = home_for("http-ctype", &origin);
        api(&home, "PUT", "/api/x", Some(b"a=1"), &[200], &[]).unwrap();
        api(
            &home,
            "PUT",
            "/api/x",
            Some(b"{}"),
            &[200],
            &[("Content-Type", "application/json")],
        )
        .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0].header("Content-Type"),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(seen[0].body, b"a=1");
        assert_eq!(seen[1].header("Content-Type"), Some("application/json"));
    }

    #[test]
    fn cas_strips_weak_etags_and_retries_412() {
        let (origin, seen) = serve(vec![
            (200, "old", vec![("ETag", "W/\"x\"")]),
            (412, "", vec![]),
            (404, "", vec![]),
            (200, "", vec![]),
        ]);
        let home = home_for("http-cas", &origin);
        let mut calls = Vec::new();
        let got = cas(&home, "/api/board", |text| {
            calls.push(text.map(str::to_string));
            Ok((Some("new".to_string()), calls.len()))
        })
        .unwrap();
        assert_eq!(got, 2);
        assert_eq!(calls, vec![Some("old".to_string()), None]);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[1].method, "PUT");
        assert_eq!(seen[1].header("If-Match"), Some("\"x\""));
        assert_eq!(seen[3].header("If-None-Match"), Some("*"));
        assert_eq!(seen[3].body, b"new");
    }

    #[test]
    fn refused_connection_reads_like_urllib() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        drop(listener);
        let home = home_for("http-refused", &origin);
        let e = api(&home, "GET", "/projects.json", None, &[200], &[]).unwrap_err();
        assert_eq!(e.code, 5);
        #[cfg(target_os = "linux")]
        assert_eq!(
            e.stderr,
            format!("factory.py: can't reach {origin}/projects.json: <urlopen error [Errno 111] Connection refused>\n")
        );
        assert!(!e.stderr.contains("test-secret"));
    }

    #[test]
    fn proxy_follows_the_origin_scheme_like_urllib() {
        let pick = |origin: &str, env: &[(&str, &str)]| {
            proxy_url(origin, |name| {
                env.iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            })
        };
        let p = "http://p:3128";
        assert_eq!(pick("http://127.0.0.1:9", &[("HTTPS_PROXY", p)]), None);
        assert_eq!(
            pick(
                "http://127.0.0.1:9",
                &[("HTTP_PROXY", p), ("NO_PROXY", "localhost,127.0.0.1")]
            ),
            None
        );
        assert_eq!(
            pick("http://127.0.0.1:9", &[("HTTP_PROXY", p)]).as_deref(),
            Some(p)
        );
        assert_eq!(
            pick("https://f.example", &[("HTTPS_PROXY", p)]).as_deref(),
            Some(p)
        );
        assert_eq!(
            pick(
                "https://f.example",
                &[
                    ("https_proxy", "http://low:1"),
                    ("HTTPS_PROXY", "http://up:2")
                ]
            )
            .as_deref(),
            Some("http://low:1")
        );
        assert_eq!(
            pick(
                "https://f.example",
                &[("HTTPS_PROXY", p), ("no_proxy", ".example")]
            ),
            None
        );
        assert_eq!(
            pick(
                "https://f.example:8443",
                &[("HTTPS_PROXY", p), ("NO_PROXY", "*")]
            ),
            None
        );
        assert_eq!(pick("https://f.example", &[("ALL_PROXY", p)]), None);
        assert_eq!(
            pick(
                "https://notexample",
                &[("HTTPS_PROXY", p), ("NO_PROXY", "example")]
            )
            .as_deref(),
            Some(p)
        );
        assert_eq!(pick("https://f.example", &[("HTTPS_PROXY", "")]), None);
    }

    #[test]
    fn origins_match_python() {
        for good in [
            "https://f.example",
            "https://f.example/",
            "HTTPS://F.example:443",
            "http://localhost:8787",
            "http://127.0.0.1:9/",
            "https://[::1]:8",
        ] {
            assert!(normalized_origin(good, false).is_ok(), "{good}");
        }
        assert_eq!(
            normalized_origin("https://f.example/", false).unwrap(),
            "https://f.example"
        );
        for bad in [
            "",
            "http://f.example",
            "https://f.example/x",
            "https://u@f.example",
            "https://f.example?x",
            "https://f.example#x",
            "https://f.example:",
            "https://f.example:0",
            "https://f.example:70000",
            "https://f.example:x",
            "https://f\\x",
            "https:// f.example",
            "https:///x",
            "ftp://f.example",
            "https://[::1",
            "f.example",
        ] {
            assert!(normalized_origin(bad, false).is_err(), "{bad}");
        }
        assert!(normalized_origin("http://localhost", true).is_err());
        assert_eq!(quote("009 gone/é~"), "009%20gone/%C3%A9~");
    }
}
