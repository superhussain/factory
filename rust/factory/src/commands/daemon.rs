//! `factory daemon`: one socket on the Worker's `/events/host` route, and one pickup pass per
//! burst of board or backlog writes. The 5-minute pickup timer stays the fallback.

use std::collections::hash_map::RandomState;
use std::fs::{File, OpenOptions, TryLockError};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, IsTerminal};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::ops::ControlFlow;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::http::{HeaderName, HeaderValue};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{HandshakeError, Message, WebSocket};

use crate::cli::Call;
use crate::shared::conf::read_conf;
use crate::shared::exit::{fail, Res};
use crate::shared::home::factory_home;
use crate::shared::http::{access_message, api_url, token_message};
use crate::shared::py::os_error;
use crate::shared::time::now;

const HOST_EVENTS_PATH: &str = "/events/host";

#[derive(Clone, Copy)]
struct Timing {
    debounce: Duration,
    min_gap: Duration,
    ping_every: Duration,
    silence: Duration,
    max_backoff: Duration,
    connect_timeout: Duration,
}

const LIVE: Timing = Timing {
    debounce: Duration::from_secs(2),
    min_gap: Duration::from_secs(30),
    ping_every: Duration::from_secs(25),
    silence: Duration::from_secs(60),
    max_backoff: Duration::from_secs(30),
    connect_timeout: Duration::from_secs(10),
};

/// What the reader thread reports. `Ignored` carries a frame's length, never its body.
enum Event {
    Changed(String),
    Ignored(usize),
    Closed(String),
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

pub fn daemon(_call: &Call) -> Res<()> {
    let home = factory_home()?;
    let origin = api_url(&home)?;
    let headers = access_headers(&home, &origin)?;
    let _lock = take_lock(&home)?;
    let exe = std::env::current_exe().map_err(|e| fail(os_error(&e, None), 1))?;
    let url = host_url(&origin);
    println!(
        "factory daemon: watching {url}; log {}",
        home.join("logs").join("daemon").display()
    );
    let terminal = io::stderr().is_terminal();
    serve(
        &url,
        &headers,
        &LIVE,
        &home.join(".env"),
        &|msg| {
            log_line(&home, msg);
            // A person watching the terminal sees why the socket never opens.
            if terminal && msg.starts_with("connect failed: ") {
                eprintln!("factory daemon: {msg}");
            }
        },
        &mut |_| {
            let code = run_pickup(&exe, &home);
            log_line(&home, &format!("pickup exited {code}"));
            ControlFlow::Continue(())
        },
    );
    Ok(())
}

fn host_url(origin: &str) -> String {
    let origin = match origin.strip_prefix("https://") {
        Some(rest) => format!("wss://{rest}"),
        None => origin.replacen("http://", "ws://", 1),
    };
    origin + HOST_EVENTS_PATH
}

/// `User-Agent: factory` and the `.env` Access headers. An `https` origin needs both keys.
fn access_headers(home: &Path, origin: &str) -> Res<Vec<(String, String)>> {
    let env_file = home.join(".env");
    let env = if env_file.exists() {
        read_conf(&env_file).map_err(|e| fail(os_error(&e, Some(&env_file)), 1))?
    } else {
        Default::default()
    };
    let mut headers = vec![("User-Agent".to_string(), "factory".to_string())];
    for (header, var) in [
        ("CF-Access-Client-Id", "FACTORY_ACCESS_CLIENT_ID"),
        ("CF-Access-Client-Secret", "FACTORY_ACCESS_CLIENT_SECRET"),
    ] {
        match env.get(var).filter(|v| !v.is_empty()) {
            Some(value) => headers.push((header.to_string(), value.clone())),
            None if origin.starts_with("https://") => {
                return Err(fail(token_message(&env_file), 2))
            }
            None => {}
        }
    }
    Ok(headers)
}

/// The delay before reconnect attempt `attempt`: doubling from 1 s up to `cap`, scaled by
/// a factor from 0.5 to 1 that `roll` picks.
fn backoff(attempt: u32, cap: Duration, roll: u64) -> Duration {
    let base = (Duration::from_secs(1) * (1u32 << attempt.min(16))).min(cap);
    base / 2 + base / 2 * (roll % 1001) as u32 / 1000
}

fn roll() -> u64 {
    RandomState::new().build_hasher().finish()
}

/// When a pending pass may start: `debounce` after its first event, and never sooner than
/// `min_gap` after the last pass began.
fn due(first: Instant, last_start: Option<Instant>, t: &Timing) -> Instant {
    let at = first + t.debounce;
    match last_start {
        Some(start) => at.max(start + t.min_gap),
        None => at,
    }
}

fn connect(
    url: &str,
    headers: &[(String, String)],
    t: &Timing,
    env_file: &Path,
) -> Result<Socket, String> {
    let mut request = url.into_client_request().map_err(|e| e.to_string())?;
    for (name, value) in headers {
        request.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes()).map_err(|e| e.to_string())?,
            HeaderValue::from_str(value).map_err(|e| e.to_string())?,
        );
    }
    let uri = request.uri();
    let host = uri.host().unwrap_or_default().to_string();
    let port = uri
        .port_u16()
        .unwrap_or(if uri.scheme_str() == Some("wss") {
            443
        } else {
            80
        });
    let mut stream = Err(format!("no address for {host}"));
    for addr in (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
    {
        stream = TcpStream::connect_timeout(&addr, t.connect_timeout).map_err(|e| e.to_string());
        if stream.is_ok() {
            break;
        }
    }
    let stream = stream?;
    let tcp = stream.try_clone().map_err(|e| e.to_string())?;
    // tungstenite panics when a TLS handshake read times out, so the handshake runs with no
    // socket timeout and this watchdog shuts the socket down at connect_timeout instead.
    let watchdog = stream.try_clone().map_err(|e| e.to_string())?;
    let (done, wait) = mpsc::channel::<()>();
    let limit = t.connect_timeout;
    std::thread::spawn(move || {
        if wait.recv_timeout(limit) == Err(RecvTimeoutError::Timeout) {
            let _ = watchdog.shutdown(Shutdown::Both);
        }
    });
    let result = tungstenite::client_tls(request, stream);
    if done.send(()).is_err() {
        return Err("timed out".to_string());
    }
    let socket = match result {
        Ok((socket, _)) => socket,
        Err(HandshakeError::Interrupted(_)) => return Err("timed out".to_string()),
        Err(HandshakeError::Failure(tungstenite::Error::Http(response))) => {
            return Err(match response.status().as_u16() {
                301 | 302 | 303 | 307 | 308 | 401 | 403 => access_message(env_file),
                404 => format!(
                    "HTTP 404: this Worker has no {HOST_EVENTS_PATH} route; deploy the Worker first"
                ),
                503 => "HTTP 503: the events room is full".to_string(),
                status => format!("HTTP {status}"),
            })
        }
        Err(HandshakeError::Failure(e)) => return Err(e.to_string()),
    };
    tcp.set_read_timeout(Some(tick(t)))
        .and_then(|_| tcp.set_write_timeout(Some(t.connect_timeout)))
        .map_err(|e| e.to_string())?;
    Ok(socket)
}

/// The key of a `{"type":"changed","key":K}` frame.
fn changed_key(text: &str) -> Option<String> {
    let frame: serde_json::Value = serde_json::from_str(text).ok()?;
    (frame.get("type")?.as_str()? == "changed").then_some(())?;
    Some(frame.get("key")?.as_str()?.to_string())
}

/// The read timeout: short enough that silence is noticed within a fifth of `ping_every`.
fn tick(t: &Timing) -> Duration {
    t.ping_every / 5
}

/// Reads frames until the socket closes. It sends `ping` after `ping_every` with no frame or
/// ping, and always sends `Closed` before it stops.
fn read_loop(mut socket: Socket, t: &Timing, tx: Sender<Event>) {
    let mut last = Instant::now();
    let mut pinged = last;
    loop {
        let event = match socket.read() {
            Ok(Message::Close(frame)) => Event::Closed(format!(
                "closed by the Worker ({})",
                frame.map_or(1005, |f| u16::from(f.code))
            )),
            Ok(message) => {
                last = Instant::now();
                match &message {
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Text(text) if text.as_str() == "pong" => continue,
                    Message::Text(text) => match changed_key(text.as_str()) {
                        Some(key) => Event::Changed(key),
                        None => Event::Ignored(message.len()),
                    },
                    _ => Event::Ignored(message.len()),
                }
            }
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if last.elapsed() >= t.silence {
                    Event::Closed(format!("no frame for {:.1}s", t.silence.as_secs_f64()))
                } else if last.max(pinged).elapsed() < t.ping_every {
                    continue;
                } else {
                    pinged = Instant::now();
                    match socket.send(Message::text("ping")) {
                        Ok(()) => continue,
                        Err(e) => Event::Closed(e.to_string()),
                    }
                }
            }
            Err(e) => Event::Closed(e.to_string()),
        };
        let closed = matches!(event, Event::Closed(_));
        if tx.send(event).is_err() || closed {
            return;
        }
    }
}

/// Keeps one socket open, reconnecting with backoff, and calls `pass` once per burst.
/// Each connect queues a `connect` pass. Returns when `pass` breaks.
fn serve(
    url: &str,
    headers: &[(String, String)],
    t: &Timing,
    env_file: &Path,
    log: &dyn Fn(&str),
    pass: &mut dyn FnMut(&str) -> ControlFlow<()>,
) {
    let mut attempt = 0;
    let mut last_start = None;
    loop {
        let socket = match connect(url, headers, t, env_file) {
            Ok(socket) => socket,
            Err(reason) => {
                let wait = backoff(attempt, t.max_backoff, roll());
                attempt += 1;
                log(&format!(
                    "connect failed: {reason}; retry in {:.1}s",
                    wait.as_secs_f64()
                ));
                std::thread::sleep(wait);
                continue;
            }
        };
        log(&format!("connected {url}"));
        let opened = Instant::now();
        let (tx, rx) = mpsc::channel();
        let timing = *t;
        std::thread::spawn(move || read_loop(socket, &timing, tx));
        let mut pending = Some((opened, "connect".to_string()));
        let reason = loop {
            let wait = pending.as_ref().map_or(Duration::MAX, |(first, _)| {
                due(*first, last_start, t).saturating_duration_since(Instant::now())
            });
            match rx.recv_timeout(wait) {
                Ok(Event::Changed(key)) => {
                    log(&format!("changed {key}"));
                    pending.get_or_insert_with(|| (Instant::now(), format!("changed {key}")));
                }
                Ok(Event::Ignored(n)) => log(&format!("frame ignored ({n} bytes)")),
                Ok(Event::Closed(reason)) => break reason,
                Err(RecvTimeoutError::Timeout) => {
                    let (_, reason) = pending.take().unwrap();
                    last_start = Some(Instant::now());
                    log(&format!("pickup pass ({reason})"));
                    if pass(&reason).is_break() {
                        return;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    unreachable!("the reader sends Closed before it stops")
                }
            }
        };
        if opened.elapsed() >= t.silence {
            attempt = 0;
        }
        let wait = backoff(attempt, t.max_backoff, roll());
        attempt += 1;
        log(&format!(
            "closed: {reason}; retry in {:.1}s",
            wait.as_secs_f64()
        ));
        std::thread::sleep(wait);
    }
}

/// Today's log, `<home>/logs/daemon/<UTC date>.log`, opened for append and created 0600.
fn day_log(home: &Path) -> io::Result<File> {
    let dir = home.join("logs").join("daemon");
    std::fs::create_dir_all(&dir)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(dir.join(format!("{}.log", &now()[..10])))
}

fn log_line(home: &Path, msg: &str) {
    if let Ok(mut file) = day_log(home) {
        let _ = io::Write::write_all(&mut file, format!("{} {msg}\n", now()).as_bytes());
    }
}

/// Runs `exe --home <home> pickup` with its output appended to the day log.
fn run_pickup(exe: &Path, home: &Path) -> i32 {
    let status = day_log(home).and_then(|log| {
        Command::new(exe)
            .arg("--home")
            .arg(home)
            .arg("pickup")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .status()
    });
    match status {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            log_line(home, &os_error(&e, Some(exe)));
            1
        }
    }
}

fn take_lock(home: &Path) -> Res<File> {
    let dir = home.join("logs").join("daemon");
    let path = dir.join("daemon.lock");
    let file = std::fs::create_dir_all(&dir)
        .and_then(|_| {
            OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
        })
        .map_err(|e| fail(os_error(&e, Some(&path)), 1))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(fail(
            format!("another factory daemon is running for {}", home.display()),
            3,
        )),
        Err(TryLockError::Error(e)) => Err(fail(os_error(&e, Some(&path)), 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::fs::temp_dir;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread::sleep;
    use tungstenite::handshake::server::{ErrorResponse, Request, Response};

    const TEST: Timing = Timing {
        debounce: Duration::from_millis(50),
        min_gap: Duration::from_millis(200),
        ping_every: Duration::from_millis(100),
        silence: Duration::from_millis(300),
        max_backoff: Duration::from_millis(100),
        connect_timeout: Duration::from_secs(1),
    };

    type Seen = Arc<Mutex<Vec<Vec<(String, String)>>>>;

    /// A loopback socket server. The first `reject` handshake gets that status. Every accepted
    /// socket runs `behave`. Each handshake's headers are recorded.
    fn fake(
        reject: Option<u16>,
        behave: impl Fn(WebSocket<TcpStream>) + Send + Sync + 'static,
    ) -> (String, Seen) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let behave = Arc::new(behave);
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let stream = stream.unwrap();
                let log = log.clone();
                let behave = behave.clone();
                std::thread::spawn(move || {
                    let callback = |request: &Request, response: Response| {
                        log.lock().unwrap().push(
                            request
                                .headers()
                                .iter()
                                .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
                                .collect(),
                        );
                        match reject.filter(|_| n == 0) {
                            Some(status) => {
                                let mut refused = ErrorResponse::new(None);
                                *refused.status_mut() = status.try_into().unwrap();
                                Err(refused)
                            }
                            None => Ok(response),
                        }
                    };
                    if let Ok(socket) = tungstenite::accept_hdr(stream, callback) {
                        behave(socket);
                    }
                });
            }
        });
        (origin, seen)
    }

    fn send(socket: &mut WebSocket<TcpStream>, text: &str) {
        socket.send(Message::text(text)).unwrap();
    }

    /// Answers each `ping` with `pong` for `time`, as the room's auto-response does.
    fn answer_pings(socket: &mut WebSocket<TcpStream>, time: Duration) {
        let start = Instant::now();
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        while start.elapsed() < time {
            if let Ok(Message::Text(text)) = socket.read() {
                if text.as_str() == "ping" {
                    send(socket, "pong");
                }
            }
        }
    }

    const BOARD: &str = r#"{"type":"changed","key":"board.json"}"#;
    const BACKLOG: &str = r#"{"type":"changed","key":"backlog.md"}"#;
    const ROWS: &str = r#"{"type":"rows","rows":"synthetic-row-body"}"#;

    fn home(name: &str, origin: &str) -> std::path::PathBuf {
        crate::shared::http::tests::home_for(&format!("daemon-{name}"), origin)
    }

    fn log_text(home: &Path) -> String {
        let dir = home.join("logs").join("daemon");
        let mut text = String::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "log") {
                text.push_str(&std::fs::read_to_string(path).unwrap());
            }
        }
        text
    }

    /// Runs `serve` against origin until `passes` passes have started, and returns their reasons.
    fn run(home: &Path, origin: &str, passes: usize) -> Vec<String> {
        let headers = access_headers(home, origin).unwrap();
        let mut seen = Vec::new();
        serve(
            &host_url(origin),
            &headers,
            &TEST,
            &home.join(".env"),
            &|msg| log_line(home, msg),
            &mut |reason| {
                seen.push(reason.to_string());
                if seen.len() == passes {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
        );
        seen
    }

    #[test]
    fn host_url_maps_scheme() {
        assert_eq!(
            host_url("https://f.example.test"),
            "wss://f.example.test/events/host"
        );
        assert_eq!(
            host_url("http://127.0.0.1:8249"),
            "ws://127.0.0.1:8249/events/host"
        );
    }

    #[test]
    fn backoff_doubles_jitters_and_caps() {
        let cap = Duration::from_secs(30);
        assert_eq!(backoff(0, cap, 0), Duration::from_millis(500));
        assert_eq!(backoff(0, cap, 1000), Duration::from_secs(1));
        assert_eq!(backoff(1, cap, 1000), Duration::from_secs(2));
        assert!(backoff(20, cap, roll()) <= cap);
    }

    #[test]
    fn due_waits_for_debounce_and_gap() {
        let first = Instant::now();
        assert_eq!(due(first, None, &TEST), first + TEST.debounce);
        assert_eq!(due(first, Some(first), &TEST), first + TEST.min_gap);
    }

    #[test]
    fn sends_access_headers_and_user_agent() {
        let (origin, seen) = fake(None, |_| sleep(Duration::from_secs(1)));
        let home = home("headers", &origin);
        assert_eq!(run(&home, &origin, 1), ["connect"]);
        let seen = seen.lock().unwrap();
        let get = |name: &str| {
            seen[0]
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("CF-Access-Client-Id").as_deref(), Some("test-id"));
        assert_eq!(
            get("CF-Access-Client-Secret").as_deref(),
            Some("test-secret")
        );
        assert_eq!(get("User-Agent").as_deref(), Some("factory"));
    }

    #[test]
    fn burst_runs_one_pass() {
        let (origin, _) = fake(None, |mut socket| {
            sleep(Duration::from_millis(100));
            for _ in 0..5 {
                send(&mut socket, BOARD);
            }
            answer_pings(&mut socket, Duration::from_millis(600));
            send(&mut socket, BACKLOG);
            sleep(Duration::from_secs(1));
        });
        let home = home("burst", &origin);
        assert_eq!(
            run(&home, &origin, 3),
            ["connect", "changed board.json", "changed backlog.md"]
        );
    }

    #[test]
    fn rows_frame_runs_no_pass() {
        let (origin, _) = fake(None, |mut socket| {
            sleep(Duration::from_millis(100));
            send(&mut socket, ROWS);
            answer_pings(&mut socket, Duration::from_millis(400));
            send(&mut socket, BOARD);
            sleep(Duration::from_secs(1));
        });
        let home = home("rows", &origin);
        assert_eq!(run(&home, &origin, 2), ["connect", "changed board.json"]);
        let log = log_text(&home);
        let ignored = format!(" frame ignored ({} bytes)\n", ROWS.len());
        assert!(log.contains(&ignored), "{log}");
        assert!(!log.contains("synthetic-row-body"));
    }

    #[test]
    fn close_reconnects_with_a_connect_pass() {
        let (origin, seen) = fake(None, |mut socket| {
            sleep(Duration::from_millis(100));
            let _ = socket.close(None);
            let _ = socket.flush();
            sleep(Duration::from_millis(200));
        });
        let home = home("close", &origin);
        assert_eq!(run(&home, &origin, 2), ["connect", "connect"]);
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert!(log_text(&home).contains("closed: closed by the Worker (1005); retry in "));
    }

    #[test]
    fn silent_socket_reconnects() {
        let (origin, seen) = fake(None, |_| sleep(Duration::from_secs(2)));
        let home = home("silent", &origin);
        assert_eq!(run(&home, &origin, 2), ["connect", "connect"]);
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert!(log_text(&home).contains("closed: no frame for 0.3s; retry in "));
    }

    #[test]
    fn pong_keeps_socket_open() {
        let (origin, seen) = fake(None, |mut socket| {
            answer_pings(&mut socket, Duration::from_secs(1));
            send(&mut socket, BOARD);
            sleep(Duration::from_secs(1));
        });
        let home = home("pong", &origin);
        assert_eq!(run(&home, &origin, 2), ["connect", "changed board.json"]);
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(!log_text(&home).contains("closed:"));
    }

    #[test]
    fn handshake_403_logs_access_message() {
        let (origin, _) = fake(Some(403), |_| sleep(Duration::from_secs(1)));
        let home = home("403", &origin);
        assert_eq!(run(&home, &origin, 1), ["connect"]);
        let message = access_message(&home.join(".env"));
        assert!(log_text(&home).contains(&format!("connect failed: {message}; retry in ")));
    }

    #[test]
    fn second_daemon_exits_3() {
        let home = temp_dir("daemon-lock");
        let _first = take_lock(&home).unwrap();
        let e = take_lock(&home).unwrap_err();
        assert_eq!(e.code, 3);
        assert_eq!(
            e.stderr,
            format!(
                "factory.py: another factory daemon is running for {}\n",
                home.display()
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn pickup_child_output_reaches_log() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_dir("daemon-pickup");
        let exe = home.join("fake-factory");
        std::fs::write(&exe, "#!/bin/sh\necho \"args: $*\"\nexit 7\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(run_pickup(&exe, &home), 7);
        assert!(log_text(&home).contains(&format!("args: --home {} pickup\n", home.display())));
        let dir = home.join("logs").join("daemon");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "log") {
                let mode = std::fs::metadata(path).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600);
            }
        }
    }

    #[test]
    fn log_never_holds_the_secret() {
        let (origin, _) = fake(Some(403), |mut socket| {
            sleep(Duration::from_millis(100));
            send(&mut socket, r#"{"type":"rows","rows":"x"}"#);
            send(&mut socket, BOARD);
            sleep(Duration::from_millis(300));
            let _ = socket.close(None);
            let _ = socket.flush();
            sleep(Duration::from_millis(200));
        });
        let home = home("secret", &origin);
        assert_eq!(
            run(&home, &origin, 3),
            ["connect", "changed board.json", "connect"]
        );
        let log = log_text(&home);
        assert!(log.contains("connect failed: ") && log.contains("frame ignored ("));
        assert!(!log.contains("test-secret"), "{log}");
    }

    #[test]
    fn missing_token_fails_2() {
        let home = temp_dir("daemon-token");
        let e = access_headers(&home, "https://f.example.test").unwrap_err();
        assert_eq!(e.code, 2);
        assert_eq!(
            e.stderr,
            format!("factory.py: {}\n", token_message(&home.join(".env")))
        );
    }

    #[test]
    fn stalled_tls_handshake_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let held: Vec<_> = listener.incoming().take(1).collect();
            sleep(Duration::from_secs(2));
            drop(held);
        });
        let t = Timing {
            connect_timeout: Duration::from_millis(300),
            ..TEST
        };
        let start = Instant::now();
        let url = format!("wss://127.0.0.1:{port}{HOST_EVENTS_PATH}");
        let e = connect(&url, &[], &t, Path::new(".env")).err().unwrap();
        assert_eq!(e, "timed out");
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn connect_tries_every_address() {
        let (origin, seen) = fake(None, |_| sleep(Duration::from_secs(1)));
        let url = host_url(&origin).replace("127.0.0.1", "localhost");
        assert!(connect(&url, &[], &TEST, Path::new(".env")).is_ok());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn silence_closes_on_time() {
        let (origin, _) = fake(None, |_| sleep(Duration::from_secs(1)));
        let t = Timing {
            silence: Duration::from_millis(240),
            ..TEST
        };
        let socket = connect(&host_url(&origin), &[], &t, Path::new(".env")).unwrap();
        let (tx, rx) = mpsc::channel();
        let start = Instant::now();
        std::thread::spawn(move || read_loop(socket, &t, tx));
        let Ok(Event::Closed(reason)) = rx.recv() else {
            panic!("expected Closed")
        };
        assert_eq!(reason, "no frame for 0.2s");
        let took = start.elapsed();
        assert!(
            took >= t.silence && took < Duration::from_millis(290),
            "{took:?}"
        );
    }
}
