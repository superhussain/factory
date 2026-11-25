//! `factory daemon`: one socket on the Worker's `/events/host` route, and the host's only pickup
//! scheduler where it is installed. A pass runs per burst of board or backlog writes, when a local
//! factory agent stops running, right after a pass that launched a job, and after 5 minutes with
//! no pass. It also keeps the board mirror (`mirror.rs`) current from the socket's rows frames and
//! serves it to this host's commands, sends the host status heartbeat every 5 minutes, pushes
//! the retro digest at the retro-push timer's times, in place of that timer, and syncs this host's
//! active job folders with R2 as they change (`job_sync.rs`).

use std::collections::hash_map::RandomState;
use std::fs::{File, OpenOptions, TryLockError};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, IsTerminal};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::ops::ControlFlow;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tungstenite::client::IntoClientRequest;
use tungstenite::http::{HeaderName, HeaderValue};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{HandshakeError, Message, WebSocket};

use crate::cli::Call;
use crate::commands::job_sync;
use crate::commands::mirror::{self, Mirror};
use crate::shared::conf::read_conf;
use crate::shared::exit::{fail, Res};
use crate::shared::fs::is_link;
use crate::shared::home::factory_home;
use crate::shared::http::{access_message, api_url, token_message};
use crate::shared::py::os_error;
use crate::shared::time::{format_stamp, now, unix_now};

const HOST_EVENTS_PATH: &str = "/events/host";
const HOST_403: &str = "HTTP 403: no paired or active host is bound to this service token; pair this host, or check its record under Settings, Hosts";

#[derive(Clone, Copy)]
struct Timing {
    debounce: Duration,
    min_gap: Duration,
    ping_every: Duration,
    silence: Duration,
    max_backoff: Duration,
    connect_timeout: Duration,
    /// How often the local Paseo agent list is read while it holds a factory agent, at first.
    agent_poll: Duration,
    /// The longest gap between reads of an agent list that has not changed.
    agent_poll_max: Duration,
    /// How long after the last pass began a fallback pass is queued.
    fallback: Duration,
    /// Where the read and run loops read the time.
    clock: Clock,
}

/// The read and run loops' clock: the system's, or in tests one that moves only when the test
/// advances it. Socket timeouts and sleeps stay real; the loops compare deadlines on this clock.
#[derive(Clone, Copy)]
enum Clock {
    System,
    #[cfg(test)]
    Fake(&'static std::sync::Mutex<Instant>),
}

impl Clock {
    fn now(self) -> Instant {
        match self {
            Clock::System => Instant::now(),
            #[cfg(test)]
            Clock::Fake(at) => *at.lock().unwrap(),
        }
    }

    fn since(self, at: Instant) -> Duration {
        self.now().saturating_duration_since(at)
    }
}

const LIVE: Timing = Timing {
    debounce: Duration::from_secs(2),
    min_gap: Duration::from_secs(30),
    ping_every: Duration::from_secs(25),
    silence: Duration::from_secs(60),
    max_backoff: Duration::from_secs(30),
    connect_timeout: Duration::from_secs(10),
    agent_poll: Duration::from_secs(15),
    agent_poll_max: Duration::from_secs(60),
    fallback: Duration::from_secs(300),
    clock: Clock::System,
};

/// What the reader thread reports. `Ignored` carries a frame's length, never its body, and `Note`
/// a mirror log line.
enum Event {
    Changed(String),
    Ignored(usize),
    Note(String),
    Closed(String),
}

/// How often the daemon sends the host status heartbeat.
const HEARTBEAT: Duration = Duration::from_secs(300);

/// How long after a failed board fetch the reader tries again.
const REFETCH: Duration = Duration::from_secs(10);

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Reports the stop signal's name once one has arrived, else None.
type Stop = fn() -> Option<&'static str>;

/// How often the waits check for a stop signal.
const POLL: Duration = Duration::from_millis(100);
/// How long a running pickup child gets to finish after a stop signal before it is killed.
const CHILD_GRACE: Duration = Duration::from_secs(30);

/// The stop signal the handler saw, as an index into STOP_NAMES; 0 is none.
static SIGNAL: AtomicUsize = AtomicUsize::new(0);
const STOP_NAMES: [&str; 5] = ["", "SIGINT", "SIGTERM", "CTRL_C", "CTRL_BREAK"];

fn signal_stop() -> Option<&'static str> {
    Some(STOP_NAMES[SIGNAL.load(Ordering::SeqCst)]).filter(|name| !name.is_empty())
}

/// Routes SIGINT and SIGTERM to SIGNAL instead of ending the process.
#[cfg(unix)]
fn catch_stop_signals() {
    extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    extern "C" fn on_signal(sig: i32) {
        SIGNAL.store(if sig == 2 { 1 } else { 2 }, Ordering::SeqCst);
    }
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        signal(2, on_signal as extern "C" fn(i32) as usize);
        signal(15, on_signal as extern "C" fn(i32) as usize);
    }
}

/// Routes CTRL_C and CTRL_BREAK to SIGNAL instead of ending the process.
#[cfg(windows)]
fn catch_stop_signals() {
    extern "system" {
        fn SetConsoleCtrlHandler(handler: extern "system" fn(u32) -> i32, add: i32) -> i32;
    }
    extern "system" fn on_ctrl(kind: u32) -> i32 {
        match kind {
            0 | 1 => {
                SIGNAL.store(kind as usize + 3, Ordering::SeqCst);
                1
            }
            _ => 0,
        }
    }
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        SetConsoleCtrlHandler(on_ctrl, 1);
    }
}

pub fn daemon(call: &Call) -> Res<()> {
    match call.args.first().map(String::as_str) {
        None => run(&factory_home()?),
        Some("setup") => crate::commands::pickup::daemon_setup_cmd(&call.args[1..]),
        Some(_) => Err(fail(
            "usage: daemon | daemon setup [--install | --uninstall]",
            2,
        )),
    }
}

/// The daemon for home. Returns Ok once a stop signal has closed the socket and any pickup child
/// has ended; the lock is released as it returns.
fn run(home: &Path) -> Res<()> {
    let home = home.to_path_buf();
    let origin = api_url(&home)?;
    let headers = access_headers(&home, &origin)?;
    // Another copy already serves this home; a service or cron line that starts a second one
    // gets a quiet exit 0.
    let Some(_lock) = take_lock(&home)? else {
        return Ok(());
    };
    catch_stop_signals();
    let exe = std::env::current_exe().map_err(|e| fail(os_error(&e, None), 1))?;
    let url = host_url(&origin);
    let terminal = io::stderr().is_terminal();
    let service_log = home.join("logs").join("daemon.log");
    // launchd, systemd and cron open the service log for append; on Windows the daemon does.
    #[cfg(windows)]
    let capped = !terminal && own_log(&service_log);
    #[cfg(not(windows))]
    let capped = true;
    // Only a value unix_now reads pins the clock, and only that one is logged.
    if let Some(pinned) = std::env::var("FACTORY_PARITY_NOW")
        .ok()
        .filter(|v| v.parse::<i64>().is_ok())
    {
        log_line(
            &home,
            &format!("clock pinned: FACTORY_PARITY_NOW={pinned} is set, so this daemon reads that time, not the real one"),
        );
    }
    let board = Mirror::new(&home);
    // Without the endpoint, commands read the Worker as before.
    if let Err(e) = mirror::listen(board.clone()) {
        log_line(
            &home,
            &format!("mirror endpoint failed: {}", os_error(&e, None)),
        );
    }
    // Tests that run the whole daemon never send this host's status or push its digest.
    if !cfg!(test) {
        let (exe, home) = (exe.clone(), home.clone());
        std::thread::spawn(move || timers(&exe, &home));
    }
    println!(
        "factory daemon: watching {url}; log {}",
        home.join("logs").join("daemon").display()
    );
    // Job sync runs only on Unix, where sync.rs can lock a job folder against a manual push.
    let sync_home = home.clone();
    let syncer = cfg!(unix).then(|| {
        job_sync::spawn(
            &home,
            board.clone(),
            signal_stop,
            Arc::new(move |msg: &str| log_line(&sync_home, msg)),
        )
    });
    let poller_home = home.clone();
    let gh_poller = crate::commands::ghpoll::start(
        signal_stop,
        Box::new(move |msg| log_line(&poller_home, msg)),
    );
    let mut heart = crate::commands::heartbeat::Heart::default();
    serve(
        &url,
        &headers,
        &LIVE,
        &home.join(".env"),
        Some(board),
        signal_stop,
        &|msg| {
            log_line(&home, msg);
            // A person watching the terminal sees why the socket never opens. Under a service,
            // stderr is <home>/logs/daemon.log, so every line goes there too.
            if !terminal {
                eprintln!("{} {msg}", now());
            } else if msg.starts_with("connect failed: ") {
                eprintln!("factory daemon: {msg}");
            }
        },
        &running_agents,
        &mut |_| {
            if capped {
                cap_log(&service_log);
            }
            let _ = std::fs::write(lock_dir(&home).join(LAST_PASS_FILE), now() + "\n");
            heart.before_pass(&home, &|msg| log_line(&home, msg));
            let (code, stopped, launched) = run_pickup(&exe, &home, signal_stop);
            log_line(&home, &format!("pickup exited {code}"));
            if !terminal {
                eprintln!("{} pickup exited {code}", now());
            }
            if stopped {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(launched > 0)
            }
        },
    );
    mirror::unlisten(&home);
    if let Some(syncer) = syncer {
        let _ = syncer.join();
    }
    let _ = gh_poller.join();
    Ok(())
}

/// Sends the heartbeat at start and every HEARTBEAT, and runs `retro push` once at each of the
/// retro-push timer's local times, or at the first check after one, as launchd runs a time a
/// sleeping Mac missed. A time that passed before the daemon started is left to pickup's push on
/// its first run of the day. Runs until a stop signal.
fn timers(exe: &Path, home: &Path) {
    let times =
        crate::commands::pickup::retro_push_times(crate::commands::pickup::pickup_scheduler());
    let local = || {
        let utc = unix_now();
        format_stamp(utc + crate::commands::pickup::local_offset(utc))
    };
    let mut pushed = retro_slot(&local(), times);
    let mut beat: Option<Instant> = None;
    while signal_stop().is_none() {
        if beat.is_none_or(|at| at.elapsed() >= HEARTBEAT) {
            beat = Some(Instant::now());
            crate::commands::pickup::daemon_heartbeat(home);
        }
        let slot = retro_slot(&local(), times);
        if slot.is_some() && slot != pushed {
            pushed = slot;
            log_line(home, "retro push");
            log_line(
                home,
                &format!("retro push exited {}", run_retro_push(exe, home)),
            );
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// The latest of times at or before the local stamp `YYYY-MM-DDTHH:MM:SSZ`, as `YYYY-MM-DD HH:MM`,
/// or None before the first of the day.
fn retro_slot(local: &str, times: &[(u32, u32)]) -> Option<String> {
    let (date, clock) = (&local[..10], &local[11..16]);
    times
        .iter()
        .map(|(hour, minute)| format!("{hour:02}:{minute:02}"))
        .filter(|at| at.as_str() <= clock)
        .last()
        .map(|at| format!("{date} {at}"))
}

/// Runs `exe --home <home> retro push` with its output appended to the day log, and returns its
/// exit code.
fn run_retro_push(exe: &Path, home: &Path) -> i32 {
    let status = day_log(home).and_then(|(log, _)| {
        Command::new(exe)
            .arg("--home")
            .arg(home)
            .args(["retro", "push"])
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

/// The size past which a pass trims daemon.log, and the tail it keeps.
const LOG_CAP: u64 = 5 << 20;
const LOG_KEEP: u64 = 1 << 20;

/// Trims the service log at path to its last LOG_KEEP bytes, from a line start, once it passes
/// LOG_CAP. Every writer has it open for append, so the daemon's writes go on at the new end. A
/// link there is left alone, so the trim never truncates another file.
fn cap_log(path: &Path) {
    if is_link(path).unwrap_or(true) {
        return;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, crate::shared::fs::O_NOFOLLOW);
    let Ok(mut file) = options.open(path) else {
        return;
    };
    let len = file.metadata().map_or(0, |m| m.len());
    if len <= LOG_CAP {
        return;
    }
    let mut tail = Vec::new();
    let read = io::Seek::seek(&mut file, io::SeekFrom::Start(len - LOG_KEEP))
        .and_then(|_| io::Read::read_to_end(&mut io::Read::take(&mut file, LOG_KEEP), &mut tail));
    if read.is_err() {
        return;
    }
    let start = tail.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1);
    let _ = file
        .set_len(0)
        .and_then(|_| io::Seek::seek(&mut file, io::SeekFrom::Start(0)))
        .and_then(|_| io::Write::write_all(&mut file, &tail[start..]));
}

/// Points this process's stdout and stderr at its own append-mode handle on the service log. The
/// `.cmd` loop starts the daemon with both on `daemon-boot.log` through `>>`, so output from
/// before this call, such as a startup error, lands there. False when the log can't be opened.
#[cfg(windows)]
fn own_log(path: &Path) -> bool {
    use std::os::windows::io::IntoRawHandle;
    extern "system" {
        fn SetStdHandle(which: u32, handle: *mut std::ffi::c_void) -> i32;
    }
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    let Ok(file) = OpenOptions::new().create(true).append(true).open(path) else {
        return false;
    };
    let handle = file.into_raw_handle();
    // SAFETY: the handle stays open for the life of the process, as the standard handles need.
    unsafe {
        SetStdHandle(STD_OUTPUT_HANDLE, handle) != 0 && SetStdHandle(STD_ERROR_HANDLE, handle) != 0
    }
}

/// The file beside the lock that holds when the daemon's last pass began.
const LAST_PASS_FILE: &str = "last-pass";

/// When the daemon's last pickup pass began, as a UTC stamp.
pub(crate) fn last_pass(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(lock_dir(home).join(LAST_PASS_FILE)).ok()?;
    Some(text.trim().to_string()).filter(|s| !s.is_empty())
}

/// The ids of this host's running factory agents: Paseo agents titled `factory <id>` or
/// `babysit <id>`, as pickup's launches name them, since `paseo ls` shows no labels. None when
/// paseo can't answer.
fn running_agents() -> Option<Vec<String>> {
    // Tests that run the whole daemon never read this host's real agents.
    if cfg!(test) {
        return None;
    }
    let agents = crate::shared::paseo::paseo_ls(&[])?;
    Some(factory_running(&agents))
}

/// The ids of the running agents in a `paseo ls --json` list titled `factory <id>` or
/// `babysit <id>`.
fn factory_running(agents: &serde_json::Value) -> Vec<String> {
    agents
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| {
            a["status"] == "running"
                && a["name"]
                    .as_str()
                    .is_some_and(|n| n.starts_with("factory ") || n.starts_with("babysit "))
        })
        .filter_map(|a| a["id"].as_str().map(String::from))
        .collect()
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
            // Hang up first, so a handshake that fails on the shutdown always reads as timed out.
            drop(wait);
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
                301 | 302 | 303 | 307 | 308 | 401 => access_message(env_file),
                // Access passed the token; the Worker found no paired or active host bound to it.
                403 => format!(
                    "{HOST_403}; if its service token was revoked or rotated in Access, check FACTORY_ACCESS_CLIENT_ID/SECRET in {}",
                    env_file.display()
                ),
                404 => format!(
                    "HTTP 404: this Worker has no {HOST_EVENTS_PATH} route; deploy the Worker first"
                ),
                503 => "HTTP 503: the events room is full".to_string(),
                status => format!("HTTP {status}"),
            });
        }
        Err(HandshakeError::Failure(e)) => return Err(e.to_string()),
    };
    tcp.set_read_timeout(Some(tick(t)))
        .and_then(|_| tcp.set_write_timeout(Some(t.connect_timeout)))
        .map_err(|e| e.to_string())?;
    Ok(socket)
}

/// A frame the daemon acts on: `{"type":"changed","key":K}`, or a board rows frame.
enum Frame {
    Changed(String),
    Rows(serde_json::Value),
}

fn frame(text: &str) -> Option<Frame> {
    let frame: serde_json::Value = serde_json::from_str(text).ok()?;
    match frame.get("type")?.as_str()? {
        "changed" => Some(Frame::Changed(frame.get("key")?.as_str()?.to_string())),
        "rows" => Some(Frame::Rows(frame)),
        _ => None,
    }
}

/// Refetches the mirror and reports the result as a log line. Returns when to try again, if it
/// failed.
fn refetch(board: &Mirror, tx: &Sender<Event>, clock: Clock) -> Option<Instant> {
    let (note, retry) = match board.fetch() {
        Ok(jobs) => (format!("mirror: fetched the board ({jobs} jobs)"), None),
        Err(e) => (
            format!("mirror: fetch failed: {e}"),
            Some(clock.now() + REFETCH),
        ),
    };
    let _ = tx.send(Event::Note(note));
    retry
}

/// The read timeout: short enough that silence is noticed within a fifth of `ping_every`.
fn tick(t: &Timing) -> Duration {
    t.ping_every / 5
}

/// Reads frames until the socket closes. It sends `ping` after `ping_every` with no frame or
/// ping, and always sends `Closed` before it stops. Once `stop` reports a signal it sends the
/// Worker a close frame and stops. With a mirror, it fetches the board first, applies each rows
/// frame, refetches on a gap or a board `changed` frame, and marks the mirror down as it stops.
fn read_loop(
    mut socket: Socket,
    t: &Timing,
    stop: Stop,
    tx: Sender<Event>,
    board: Option<Arc<Mirror>>,
) {
    let mut retry = board.as_ref().and_then(|b| {
        b.connected();
        refetch(b, &tx, t.clock)
    });
    let mut last = t.clock.now();
    let mut pinged = last;
    loop {
        if stop().is_some() {
            let _ = socket.close(None);
            let _ = socket.flush();
            if let Some(b) = &board {
                b.closed();
            }
            let _ = tx.send(Event::Closed("stopped".to_string()));
            return;
        }
        // A failed fetch retries after REFETCH; a mirror with no fetch or frame for
        // mirror::REFRESH, say after a lost announcement, refetches once.
        if let Some(b) = &board {
            let due = match retry {
                Some(at) => t.clock.now() >= at,
                None => b.stale(),
            };
            if due {
                retry = refetch(b, &tx, t.clock);
            }
        }
        let event = match socket.read() {
            Ok(Message::Close(frame)) => Event::Closed(format!(
                "closed by the Worker ({})",
                frame.map_or(1005, |f| u16::from(f.code))
            )),
            Ok(message) => {
                last = t.clock.now();
                match &message {
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Text(text) if text.as_str() == "pong" => continue,
                    Message::Text(text) => match (frame(text.as_str()), &board) {
                        (Some(Frame::Changed(key)), Some(b)) if key == "board.json" => {
                            retry = refetch(b, &tx, t.clock);
                            Event::Changed(key)
                        }
                        (Some(Frame::Changed(key)), _) => Event::Changed(key),
                        // A rows frame is a board write too, so it starts a pass.
                        (Some(Frame::Rows(rows)), b) => {
                            if let Some(b) = b {
                                if !b.apply(&rows) {
                                    retry = refetch(b, &tx, t.clock);
                                }
                            }
                            Event::Changed("board.json".to_string())
                        }
                        (None, _) => Event::Ignored(message.len()),
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
                if t.clock.since(last) >= t.silence {
                    Event::Closed(format!("no frame for {:.1}s", t.silence.as_secs_f64()))
                } else if t.clock.since(last.max(pinged)) < t.ping_every {
                    continue;
                } else {
                    pinged = t.clock.now();
                    match socket.send(Message::text("ping")) {
                        Ok(()) => continue,
                        Err(e) => Event::Closed(e.to_string()),
                    }
                }
            }
            Err(e) => Event::Closed(e.to_string()),
        };
        let closed = matches!(event, Event::Closed(_));
        if closed {
            if let Some(b) = &board {
                b.closed();
            }
        }
        if tx.send(event).is_err() || closed {
            return;
        }
    }
}

/// Keeps one socket open, reconnecting with backoff, and calls `pass` once per burst.
/// Each connect queues a `connect` pass. While connected it also queues a pass when an agent id
/// `agents` listed at its last read is gone from the next read, when
/// `pass` continues with true (it launched a job, so the host may have room for another), and
/// when no pass has begun for `fallback`. While it cannot connect, it runs a fallback pass at
/// start and then every `fallback`. Returns when `pass` breaks, or when `stop` reports a
/// signal: then it logs `daemon: stopped (<signal>)` unless `pass` saw the signal first, and waits
/// for the reader's close frame.
#[allow(clippy::too_many_arguments)]
fn serve(
    url: &str,
    headers: &[(String, String)],
    t: &Timing,
    env_file: &Path,
    board: Option<Arc<Mirror>>,
    stop: Stop,
    log: &dyn Fn(&str),
    agents: &dyn Fn() -> Option<Vec<String>>,
    pass: &mut dyn FnMut(&str) -> ControlFlow<(), bool>,
) {
    let mut attempt = 0;
    let mut last_start: Option<Instant> = None;
    // The factory agents the last read saw running. The list is read at start and after a pass
    // that launched (watch), and again while it holds an agent: every agent_poll, doubling up to
    // agent_poll_max while it stays the same or the read fails. A read that finds none stops the
    // reads.
    let mut running: Vec<String> = Vec::new();
    let mut watch = true;
    let mut polled: Option<Instant> = None;
    let mut gap = t.agent_poll;
    loop {
        if let Some(signal) = stop() {
            log(&format!("daemon: stopped ({signal})"));
            return;
        }
        let socket = match connect(url, headers, t, env_file) {
            Ok(socket) => socket,
            Err(reason) => {
                let wait = backoff(attempt, t.max_backoff, roll());
                attempt += 1;
                log(&format!(
                    "connect failed: {reason}; retry in {:.1}s",
                    wait.as_secs_f64()
                ));
                // Without the socket, pickup still runs every fallback, and once at start, so
                // takeover, schedules and the heartbeat keep going through an events outage.
                if last_start.is_none_or(|at| t.clock.since(at) >= t.fallback) {
                    last_start = Some(t.clock.now());
                    log("pickup pass (fallback)");
                    if pass("fallback").is_break() {
                        return;
                    }
                }
                nap(wait, stop, t.clock);
                continue;
            }
        };
        log(&format!("connected {url}"));
        let opened = t.clock.now();
        let (tx, rx) = mpsc::channel();
        let timing = *t;
        let reader_board = board.clone();
        std::thread::spawn(move || read_loop(socket, &timing, stop, tx, reader_board));
        let mut pending = Some((opened, "connect".to_string()));
        let reason = loop {
            if let Some(signal) = stop() {
                log(&format!("daemon: stopped ({signal})"));
                wait_closed(&rx, t);
                return;
            }
            if (watch || !running.is_empty()) && polled.is_none_or(|at| t.clock.since(at) >= gap) {
                polled = Some(t.clock.now());
                if let Some(now_running) = agents() {
                    watch = false;
                    let gone = running.iter().find(|id| !now_running.contains(id)).cloned();
                    if let Some(id) = gone {
                        log(&format!("agent finished {id}"));
                        pending
                            .get_or_insert_with(|| (t.clock.now(), format!("agent finished {id}")));
                    }
                    gap = if now_running == running {
                        (gap * 2).min(t.agent_poll_max)
                    } else {
                        t.agent_poll
                    };
                    running = now_running;
                } else {
                    gap = (gap * 2).min(t.agent_poll_max);
                }
            }
            if pending.is_none() && last_start.is_some_and(|at| t.clock.since(at) >= t.fallback) {
                pending = Some((t.clock.now(), "fallback".to_string()));
            }
            let wait = pending.as_ref().map_or(POLL, |(first, _)| {
                due(*first, last_start, t).saturating_duration_since(t.clock.now())
            });
            match rx.recv_timeout(wait.min(POLL)) {
                Ok(Event::Changed(key)) => {
                    log(&format!("changed {key}"));
                    pending.get_or_insert_with(|| (t.clock.now(), format!("changed {key}")));
                }
                Ok(Event::Ignored(n)) => log(&format!("frame ignored ({n} bytes)")),
                Ok(Event::Note(note)) => log(&note),
                // The reader closed for a stop signal; the check above logs it.
                Ok(Event::Closed(_)) if stop().is_some() => {}
                Ok(Event::Closed(reason)) => break reason,
                Err(RecvTimeoutError::Timeout) => {
                    if !pending
                        .as_ref()
                        .is_some_and(|(first, _)| due(*first, last_start, t) <= t.clock.now())
                    {
                        continue;
                    }
                    let (_, reason) = pending.take().unwrap();
                    last_start = Some(t.clock.now());
                    log(&format!("pickup pass ({reason})"));
                    match pass(&reason) {
                        ControlFlow::Break(()) => {
                            if stop().is_some() {
                                wait_closed(&rx, t);
                            }
                            return;
                        }
                        ControlFlow::Continue(true) => {
                            pending = Some((t.clock.now(), "room left".to_string()));
                            (watch, polled, gap) = (true, None, t.agent_poll);
                        }
                        ControlFlow::Continue(false) => {}
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    unreachable!("the reader sends Closed before it stops")
                }
            }
        };
        if t.clock.since(opened) >= t.silence {
            attempt = 0;
        }
        let wait = backoff(attempt, t.max_backoff, roll());
        attempt += 1;
        log(&format!(
            "closed: {reason}; retry in {:.1}s",
            wait.as_secs_f64()
        ));
        nap(wait, stop, t.clock);
    }
}

/// Sleeps for `wait` on clock, or less once `stop` reports a signal.
fn nap(wait: Duration, stop: Stop, clock: Clock) {
    let end = clock.now() + wait;
    while stop().is_none() {
        let left = end.saturating_duration_since(clock.now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(POLL));
    }
}

/// Waits for the reader to send its close frame, which it does within one read timeout.
fn wait_closed(rx: &Receiver<Event>, t: &Timing) {
    let end = t.clock.now() + tick(t) + Duration::from_secs(1);
    loop {
        let left = end.saturating_duration_since(t.clock.now());
        if left.is_zero() {
            return;
        }
        match rx.recv_timeout(left.min(POLL)) {
            Ok(Event::Closed(_)) | Err(RecvTimeoutError::Disconnected) => return,
            _ => {}
        }
    }
}

/// Today's log, `<home>/logs/daemon/<UTC date>.log`, opened for append and created 0600, and its
/// path.
fn day_log(home: &Path) -> io::Result<(File, std::path::PathBuf)> {
    let dir = home.join("logs").join("daemon");
    std::fs::create_dir_all(&dir)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let path = dir.join(format!("{}.log", &now()[..10]));
    Ok((options.open(&path)?, path))
}

/// How many `launched ` lines path holds past byte start: the jobs one pickup run launched.
fn launches_since(path: &Path, start: u64) -> usize {
    let mut text = Vec::new();
    let read = File::open(path).and_then(|mut file| {
        io::Seek::seek(&mut file, io::SeekFrom::Start(start))?;
        io::Read::read_to_end(&mut file, &mut text)
    });
    if read.is_err() {
        return 0;
    }
    String::from_utf8_lossy(&text)
        .lines()
        .filter(|line| line.starts_with("launched "))
        .count()
}

fn log_line(home: &Path, msg: &str) {
    if let Ok((mut file, _)) = day_log(home) {
        let _ = io::Write::write_all(&mut file, format!("{} {msg}\n", now()).as_bytes());
    }
}

/// Runs `exe --home <home> pickup` with its output appended to the day log, and returns (exit
/// code, whether a stop signal came while it ran, how many jobs it launched). On a stop signal it
/// logs `daemon: stopped (<signal>)`, gives the child CHILD_GRACE to finish, then kills it.
fn run_pickup(exe: &Path, home: &Path, stop: Stop) -> (i32, bool, usize) {
    let child = day_log(home).and_then(|(log, path)| {
        let start = log.metadata()?.len();
        let child = Command::new(exe)
            .arg("--home")
            .arg(home)
            .arg("pickup")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        Ok((child, path, start))
    });
    let (mut child, path, start) = match child {
        Ok(child) => child,
        Err(e) => {
            log_line(home, &os_error(&e, Some(exe)));
            return (1, false, 0);
        }
    };
    let mut deadline: Option<Instant> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let launched = launches_since(&path, start);
                return (status.code().unwrap_or(1), deadline.is_some(), launched);
            }
            Ok(None) => {}
            Err(e) => {
                log_line(home, &os_error(&e, Some(exe)));
                return (1, deadline.is_some(), 0);
            }
        }
        match (deadline, stop()) {
            (None, Some(signal)) => {
                log_line(home, &format!("daemon: stopped ({signal})"));
                deadline = Some(Instant::now() + CHILD_GRACE);
            }
            (Some(end), _) if Instant::now() >= end => {
                let _ = child.kill();
            }
            _ => {}
        }
        std::thread::sleep(POLL);
    }
}

fn lock_dir(home: &Path) -> std::path::PathBuf {
    home.join("logs").join("daemon")
}

/// The daemon lock, with this process's pid written beside it, or None when another copy holds it.
pub(crate) fn take_lock(home: &Path) -> Res<Option<File>> {
    let dir = lock_dir(home);
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
        Ok(()) => {
            let pid = dir.join("daemon.pid");
            std::fs::write(&pid, format!("{}\n", std::process::id()))
                .map_err(|e| fail(os_error(&e, Some(&pid)), 1))?;
            Ok(Some(file))
        }
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(fail(os_error(&e, Some(&path)), 1)),
    }
}

/// Whether a daemon holds the lock for home; None when that cannot be checked.
pub(crate) fn running(home: &Path) -> Option<bool> {
    let file = match OpenOptions::new()
        .write(true)
        .open(lock_dir(home).join("daemon.lock"))
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Some(false),
        Err(_) => return None,
    };
    match file.try_lock() {
        Ok(()) => Some(false),
        Err(TryLockError::WouldBlock) => Some(true),
        Err(TryLockError::Error(_)) => None,
    }
}

/// The pid the daemon wrote beside its lock, when it is a real process id: 1 to i32::MAX. kill
/// reads 0 as this process group and a value past i32::MAX as a negative group or every process,
/// so anything else in the file is no pid.
pub(crate) fn pid(home: &Path) -> Option<u32> {
    let n: i64 = std::fs::read_to_string(lock_dir(home).join("daemon.pid"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    (1..=i64::from(i32::MAX)).contains(&n).then_some(n as u32)
}

/// The pid of the daemon that holds the lock: None when no daemon holds it, an error when one
/// does but its pid file holds no valid pid.
pub(crate) fn locked_pid(home: &Path) -> Result<Option<u32>, String> {
    if running(home) != Some(true) {
        return Ok(None);
    }
    pid(home)
        .map(Some)
        .ok_or_else(|| "a daemon runs, but its pid file holds no valid pid".to_string())
}

/// The file that ends the Windows restart loop: the `.cmd` exits instead of starting the daemon
/// again while it exists.
pub(crate) fn stop_marker(home: &Path) -> std::path::PathBuf {
    lock_dir(home).join("stop")
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
        agent_poll: Duration::from_millis(50),
        agent_poll_max: Duration::from_millis(200),
        fallback: Duration::from_secs(60),
        clock: Clock::System,
    };

    type Seen = Arc<Mutex<Vec<Vec<(String, String)>>>>;

    /// A loopback socket server. The first `reject` handshake gets that status. Every accepted
    /// socket runs `behave`. Each handshake's headers are recorded. Only a handshake counts as
    /// first: another test probing a port it freed, which this listener now holds, reaches the
    /// accept but not the handshake.
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
            for stream in listener.incoming() {
                let stream = stream.unwrap();
                let log = log.clone();
                let behave = behave.clone();
                std::thread::spawn(move || {
                    let callback = |request: &Request, response: Response| {
                        let mut log = log.lock().unwrap();
                        log.push(
                            request
                                .headers()
                                .iter()
                                .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
                                .collect(),
                        );
                        match reject.filter(|_| log.len() == 1) {
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
    const OTHER: &str = r#"{"type":"note","body":"synthetic-row-body"}"#;

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

    fn no_stop() -> Option<&'static str> {
        None
    }

    /// t on a fake clock, which moves only when the test advances it.
    fn on_fake_clock(t: Timing) -> (Timing, &'static Mutex<Instant>) {
        let at = Box::leak(Box::new(Mutex::new(Instant::now())));
        let clock = Clock::Fake(at);
        (Timing { clock, ..t }, at)
    }

    /// TEST for the driven tests: the socket never closes for silence, since their servers send
    /// no pongs, and a handshake on a loaded machine gets LIVE's connect timeout.
    const DRIVEN: Timing = Timing {
        silence: Duration::from_secs(3600),
        connect_timeout: Duration::from_secs(10),
        ..TEST
    };

    /// A loopback socket server that hands each accepted socket to the test.
    fn held() -> (String, Seen, Receiver<WebSocket<TcpStream>>) {
        let (tx, rx) = mpsc::channel();
        let (origin, seen) = fake(None, move |socket| {
            let _ = tx.send(socket);
        });
        (origin, seen, rx)
    }

    /// Moves a fake clock and reads the daemon's log lines, so a test steps `serve` by what it
    /// logged rather than by how long it ran.
    struct Driver {
        clock: &'static Mutex<Instant>,
        lines: Receiver<String>,
    }

    impl Driver {
        fn advance(&self, by: Duration) {
            *self.clock.lock().unwrap() += by;
        }

        /// Reads log lines until one starts with line.
        fn wait_for(&self, line: &str) {
            loop {
                let next = self.lines.recv_timeout(Duration::from_secs(60));
                match next {
                    Ok(next) if next.starts_with(line) => return,
                    Ok(_) => {}
                    Err(e) => panic!("no log line {line:?}: {e}"),
                }
            }
        }

        /// Advances the clock 10 ms at a time until a log line starts with line.
        fn advance_until(&self, line: &str) {
            for _ in 0..6000 {
                self.advance(Duration::from_millis(10));
                while let Ok(next) = self.lines.recv_timeout(Duration::from_millis(20)) {
                    if next.starts_with(line) {
                        return;
                    }
                }
            }
            panic!("no log line {line:?} within 60 s on the clock");
        }
    }

    /// Runs `serve` on its own thread with t on a fake clock until `passes` passes have started.
    /// Returns the driver, and the thread, which returns the passes' reasons. The passes counted
    /// from 1 in `launching` report a launch.
    fn drive(
        home: &Path,
        origin: &str,
        passes: usize,
        t: Timing,
        board: Option<Arc<Mirror>>,
        agents: impl Fn() -> Option<Vec<String>> + Send + 'static,
        launching: &'static [usize],
    ) -> (Driver, std::thread::JoinHandle<Vec<String>>) {
        let (t, clock) = on_fake_clock(t);
        let (tx, lines) = mpsc::channel();
        let (home, origin) = (home.to_path_buf(), origin.to_string());
        let serving = std::thread::spawn(move || {
            let headers = access_headers(&home, &origin).unwrap();
            let mut seen = Vec::new();
            serve(
                &host_url(&origin),
                &headers,
                &t,
                &home.join(".env"),
                board,
                no_stop,
                &|msg| {
                    log_line(&home, msg);
                    let _ = tx.send(msg.to_string());
                },
                &agents,
                &mut |reason| {
                    seen.push(reason.to_string());
                    if seen.len() == passes {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(launching.contains(&seen.len()))
                    }
                },
            );
            seen
        });
        (Driver { clock, lines }, serving)
    }

    /// A real SIGTERM to the daemon: it logs the stop line, sends the Worker a close frame and
    /// returns Ok, which `cli::main` turns into exit 0. No pickup pass runs, since the signal
    /// lands well inside the 2 s debounce after connect.
    #[cfg(unix)]
    #[test]
    fn sigterm_logs_stop_closes_and_returns_ok() {
        use std::sync::atomic::AtomicBool;
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
            fn signal(sig: i32, handler: usize) -> usize;
        }
        let closed = Arc::new(AtomicBool::new(false));
        let saw = closed.clone();
        let (origin, _) = fake(None, move |mut socket| {
            while let Ok(message) = socket.read() {
                if let Message::Close(_) = message {
                    saw.store(true, Ordering::SeqCst);
                }
            }
        });
        let home = home("sigterm", &origin);
        let watched = home.clone();
        let killer = std::thread::spawn(move || {
            let dir = watched.join("logs").join("daemon");
            while !(dir.is_dir() && log_text(&watched).contains(" connected ")) {
                sleep(Duration::from_millis(20));
            }
            // SAFETY: kill sends SIGTERM to this process, whose handler only stores to an atomic.
            unsafe { kill(std::process::id() as i32, 15) };
        });
        let result = super::run(&home);
        killer.join().unwrap();
        // SAFETY: SIG_DFL (0) restores the default action for the rest of the test process.
        unsafe {
            signal(2, 0);
            signal(15, 0);
        }
        SIGNAL.store(0, Ordering::SeqCst);
        assert!(result.is_ok());
        let log = log_text(&home);
        assert!(log.contains(" daemon: stopped (SIGTERM)\n"), "{log}");
        assert!(!log.contains("pickup pass"), "{log}");
        sleep(Duration::from_millis(200));
        assert!(closed.load(Ordering::SeqCst));
        take_lock(&home).unwrap().unwrap();
    }

    /// Runs `serve` against origin until `passes` passes have started, and returns their reasons.
    fn run(home: &Path, origin: &str, passes: usize) -> Vec<String> {
        run_with(home, origin, passes, &TEST, &|| None, &[])
    }

    /// `run` with timing t, a fake Paseo agent list, and the passes (counted from 1) that report
    /// a launch.
    fn run_with(
        home: &Path,
        origin: &str,
        passes: usize,
        t: &Timing,
        agents: &dyn Fn() -> Option<Vec<String>>,
        launching: &[usize],
    ) -> Vec<String> {
        let headers = access_headers(home, origin).unwrap();
        let mut seen = Vec::new();
        serve(
            &host_url(origin),
            &headers,
            t,
            &home.join(".env"),
            None,
            no_stop,
            &|msg| log_line(home, msg),
            agents,
            &mut |reason| {
                seen.push(reason.to_string());
                if seen.len() == passes {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(launching.contains(&seen.len()))
                }
            },
        );
        seen
    }

    /// A socket that answers pings for 3 s and sends nothing else.
    fn quiet() -> String {
        fake(None, |mut socket| {
            answer_pings(&mut socket, Duration::from_secs(3));
        })
        .0
    }

    /// A factory agent that stops running starts a pass; one that keeps running, an agent that
    /// starts, and a failed read do not.
    #[test]
    fn agent_finish_runs_a_pass() {
        let (origin, _, sockets) = held();
        let home = home("agent", &origin);
        let reads = AtomicUsize::new(0);
        let agents = move || match reads.fetch_add(1, Ordering::SeqCst) + 1 {
            1 => Some(vec!["a1".to_string(), "a2".to_string()]),
            2 => None,
            3..=6 => Some(vec!["a1".into(), "a2".into(), "a3".into()]),
            _ => Some(vec!["a2".into(), "a3".into()]),
        };
        let (d, serving) = drive(&home, &origin, 2, DRIVEN, None, agents, &[]);
        let start = *d.clock.lock().unwrap();
        let _socket = sockets.recv().unwrap();
        d.advance_until("pickup pass (connect)");
        d.advance_until("pickup pass (agent finished a1)");
        assert!(d.clock.lock().unwrap().duration_since(start) >= DRIVEN.min_gap);
        assert_eq!(serving.join().unwrap(), ["connect", "agent finished a1"]);
        assert!(log_text(&home).contains(" agent finished a1\n"));
    }

    /// The list is read at start and after a pass that launched; a read that finds no factory
    /// agent stops the reads until the next launch.
    #[test]
    fn agent_reads_stop_when_none_run() {
        let (origin, _, sockets) = held();
        let home = home("agent-idle", &origin);
        let t = Timing {
            fallback: Duration::from_millis(600),
            ..DRIVEN
        };
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = reads.clone();
        let agents = move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Some(Vec::new())
        };
        let (d, serving) = drive(&home, &origin, 3, t, None, agents, &[1]);
        let _socket = sockets.recv().unwrap();
        d.advance_until("pickup pass (connect)");
        d.advance_until("pickup pass (room left)");
        d.advance_until("pickup pass (fallback)");
        assert_eq!(
            serving.join().unwrap(),
            ["connect", "room left", "fallback"]
        );
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    /// While the same agents keep running, the gap between reads doubles up to agent_poll_max.
    #[test]
    fn agent_reads_back_off_while_nothing_changes() {
        let origin = quiet();
        let home = home("agent-backoff", &origin);
        let t = Timing {
            fallback: Duration::from_millis(1000),
            ..TEST
        };
        let reads = std::cell::Cell::new(0);
        let agents = || {
            reads.set(reads.get() + 1);
            Some(vec!["a1".to_string()])
        };
        let seen = run_with(&home, &origin, 2, &t, &agents, &[]);
        assert_eq!(seen, ["connect", "fallback"]);
        // Reads at 0, 50, 150, 350 ms, then every 200 ms: about 8 in 1 s, not 20.
        assert!((5..=10).contains(&reads.get()), "{}", reads.get());
    }

    /// A paseo ls that keeps failing backs off the same way, so a broken paseo is not run every
    /// agent_poll.
    #[test]
    fn failed_agent_reads_back_off() {
        let origin = quiet();
        let home = home("agent-fails", &origin);
        let t = Timing {
            fallback: Duration::from_millis(1000),
            ..TEST
        };
        let reads = std::cell::Cell::new(0);
        let agents = || {
            reads.set(reads.get() + 1);
            if reads.get() == 1 {
                Some(vec!["a1".to_string()])
            } else {
                None
            }
        };
        let seen = run_with(&home, &origin, 2, &t, &agents, &[]);
        assert_eq!(seen, ["connect", "fallback"]);
        // Reads at 0, 50, 150, 350 ms, then every 200 ms: about 8 in 1 s, not 20.
        assert!((5..=10).contains(&reads.get()), "{}", reads.get());
    }

    /// With no socket, a fallback pass runs at start and then every fallback.
    #[test]
    fn fallback_runs_while_the_socket_is_down() {
        // Nothing listens on port 0, whereas a freed port can go to another test's server.
        let origin = "http://127.0.0.1:0";
        let home = home("down", origin);
        let t = Timing {
            fallback: Duration::from_millis(300),
            ..DRIVEN
        };
        let (d, serving) = drive(&home, origin, 2, t, None, || None, &[]);
        let start = *d.clock.lock().unwrap();
        d.advance_until("pickup pass (fallback)");
        d.advance_until("pickup pass (fallback)");
        assert!(d.clock.lock().unwrap().duration_since(start) >= t.fallback);
        assert_eq!(serving.join().unwrap(), ["fallback", "fallback"]);
        assert!(log_text(&home).contains(" pickup pass (fallback)\n"));
    }

    #[test]
    fn factory_running_keeps_running_factory_agents() {
        let list = serde_json::json!([
            {"id": "a1", "name": "factory 2224-daemon-owns-pickup", "status": "running"},
            {"id": "a2", "name": "factory 2181-run-the-notes", "status": "idle"},
            {"id": "a3", "name": "explore the codebase", "status": "running"},
            {"id": "a4", "name": "factory 2230-board", "status": "closed"},
            {"id": "a5", "name": "babysit 2224", "status": "running"},
        ]);
        assert_eq!(factory_running(&list), ["a1", "a5"]);
        assert!(factory_running(&serde_json::json!({})).is_empty());
    }

    /// With no other trigger, a pass starts once `fallback` has passed since the last one began.
    #[test]
    fn fallback_runs_after_a_quiet_spell() {
        let origin = quiet();
        let home = home("fallback", &origin);
        let t = Timing {
            fallback: Duration::from_millis(500),
            ..TEST
        };
        let start = Instant::now();
        let seen = run_with(&home, &origin, 3, &t, &|| None, &[]);
        assert_eq!(seen, ["connect", "fallback", "fallback"]);
        let took = start.elapsed();
        assert!(
            took >= t.fallback * 2 && took < Duration::from_secs(2),
            "{took:?}"
        );
    }

    /// A pass that launched a job queues another at once, held only by the minimum gap; a pass
    /// that launched nothing queues none.
    #[test]
    fn launch_runs_a_room_left_pass() {
        let (origin, _, sockets) = held();
        let home = home("room", &origin);
        let t = Timing {
            fallback: Duration::from_millis(1500),
            ..DRIVEN
        };
        let (d, serving) = drive(&home, &origin, 3, t, None, || None, &[1]);
        let start = *d.clock.lock().unwrap();
        let _socket = sockets.recv().unwrap();
        d.advance_until("pickup pass (connect)");
        d.advance_until("pickup pass (room left)");
        d.advance_until("pickup pass (fallback)");
        assert!(
            d.clock.lock().unwrap().duration_since(start) >= t.fallback,
            "the second pass queued no third"
        );
        assert_eq!(
            serving.join().unwrap(),
            ["connect", "room left", "fallback"]
        );
    }

    #[test]
    fn last_pass_reads_the_stamp() {
        let home = temp_dir("daemon-last-pass");
        assert_eq!(last_pass(&home), None);
        std::fs::create_dir_all(lock_dir(&home)).unwrap();
        std::fs::write(
            lock_dir(&home).join(LAST_PASS_FILE),
            "2026-11-23T10:00:00Z\n",
        )
        .unwrap();
        assert_eq!(last_pass(&home).as_deref(), Some("2026-11-23T10:00:00Z"));
    }

    /// The latest retro-push time passed today names the slot; a new slot is a push due.
    #[test]
    fn retro_slot_names_the_latest_time_passed() {
        let mac = [(6, 30), (7, 30), (8, 30)];
        assert_eq!(retro_slot("2026-11-24T06:29:59Z", &mac), None);
        assert_eq!(
            retro_slot("2026-11-24T06:30:00Z", &mac).as_deref(),
            Some("2026-11-24 06:30")
        );
        assert_eq!(
            retro_slot("2026-11-24T23:59:00Z", &mac).as_deref(),
            Some("2026-11-24 08:30")
        );
        assert_eq!(retro_slot("2026-11-24T08:29:00Z", &[(8, 30)]), None);
        assert_eq!(
            retro_slot("2026-11-24T10:00:00Z", &[(8, 30)]).as_deref(),
            Some("2026-11-24 08:30")
        );
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

    /// Five changed frames that land before the debounce ends start one pass.
    #[test]
    fn burst_runs_one_pass() {
        let (origin, _, sockets) = held();
        let home = home("burst", &origin);
        let (d, serving) = drive(&home, &origin, 3, DRIVEN, None, || None, &[]);
        let mut socket = sockets.recv().unwrap();
        d.advance_until("pickup pass (connect)");
        for _ in 0..5 {
            send(&mut socket, BOARD);
        }
        for _ in 0..5 {
            d.wait_for("changed board.json");
        }
        d.advance_until("pickup pass (changed board.json)");
        send(&mut socket, BACKLOG);
        d.wait_for("changed backlog.md");
        d.advance_until("pickup pass (changed backlog.md)");
        assert_eq!(
            serving.join().unwrap(),
            ["connect", "changed board.json", "changed backlog.md"]
        );
    }

    /// A rows frame is a board write, so it starts a pass like a changed frame; an unknown frame
    /// is logged by length only.
    #[test]
    fn rows_frame_runs_a_pass() {
        let (origin, _, sockets) = held();
        let home = home("rows", &origin);
        let (d, serving) = drive(&home, &origin, 2, DRIVEN, None, || None, &[]);
        let mut socket = sockets.recv().unwrap();
        send(&mut socket, OTHER);
        d.wait_for(&format!("frame ignored ({} bytes)", OTHER.len()));
        d.advance_until("pickup pass (connect)");
        send(&mut socket, ROWS);
        d.wait_for("changed board.json");
        d.advance_until("pickup pass (changed board.json)");
        assert_eq!(serving.join().unwrap(), ["connect", "changed board.json"]);
        let log = log_text(&home);
        let ignored = format!(" frame ignored ({} bytes)\n", OTHER.len());
        assert!(log.contains(&ignored), "{log}");
        assert!(!log.contains("synthetic-row-body"));
    }

    /// The reader fetches the board at connect, applies a rows frame that follows it, and
    /// refetches once for a frame that doesn't. The log names job counts, never rows.
    #[test]
    fn mirror_follows_frames_and_refetches_on_a_gap() {
        use crate::shared::http::tests::serve as worker;
        let first = r#"{"updated":"","jobs":[{"id":"1999-synthetic-build","stage":"build"}]}"#;
        let later = r#"{"updated":"","jobs":[{"id":"1999-synthetic-build","stage":"merged"},{"id":"2001-add-csv-export","stage":"idea"},{"id":"2002-z"}]}"#;
        let (api_origin, gets) = worker(vec![
            (200, first, vec![("ETag", "\"e1\"")]),
            (200, later, vec![("ETag", "\"e9\"")]),
        ]);
        let (origin, _, sockets) = held();
        let home = home("mirror", &api_origin);
        let board = Mirror::new(&home);
        let (d, serving) = drive(&home, &origin, 3, DRIVEN, Some(board.clone()), || None, &[]);
        let mut socket = sockets.recv().unwrap();
        d.wait_for("mirror: fetched the board (1 jobs)");
        d.advance_until("pickup pass (connect)");
        send(
            &mut socket,
            r#"{"type":"rows","etag":"\"e2\"","prev":"\"e1\"","rows":[{"id":"2001-add-csv-export","stage":"idea"}],"removed":[]}"#,
        );
        d.wait_for("changed board.json");
        d.advance_until("pickup pass (changed board.json)");
        send(
            &mut socket,
            r#"{"type":"rows","etag":"\"e5\"","prev":"\"e4\"","rows":[],"removed":[]}"#,
        );
        d.wait_for("mirror: fetched the board (3 jobs)");
        d.wait_for("changed board.json");
        d.advance_until("pickup pass (changed board.json)");
        assert_eq!(
            serving.join().unwrap(),
            ["connect", "changed board.json", "changed board.json"]
        );
        assert_eq!(
            gets.lock().unwrap().len(),
            2,
            "one fetch at connect, one for the gap"
        );
        let log = log_text(&home);
        assert!(
            log.contains(" mirror: fetched the board (1 jobs)\n"),
            "{log}"
        );
        assert!(
            log.contains(" mirror: fetched the board (3 jobs)\n"),
            "{log}"
        );
        assert!(!log.contains("add-csv-export"), "{log}");
        let snap = std::fs::read_to_string(mirror::snapshot_path(&home)).unwrap();
        assert!(snap.starts_with(r#"{"etag":"\"e9\"""#), "{snap}");
    }

    /// A close from the Worker reconnects after the backoff, and the new socket queues a connect
    /// pass.
    #[test]
    fn close_reconnects_with_a_connect_pass() {
        let (origin, seen, sockets) = held();
        let home = home("close", &origin);
        let (d, serving) = drive(&home, &origin, 2, DRIVEN, None, || None, &[]);
        let mut first = sockets.recv().unwrap();
        d.advance_until("pickup pass (connect)");
        first.close(None).unwrap();
        first.flush().unwrap();
        d.wait_for("closed: closed by the Worker (1005); retry in ");
        d.advance_until("connected ");
        let _second = sockets.recv().unwrap();
        d.advance_until("pickup pass (connect)");
        assert_eq!(serving.join().unwrap(), ["connect", "connect"]);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn silent_socket_reconnects() {
        let (origin, seen) = fake(None, |_| sleep(Duration::from_secs(2)));
        let home = home("silent", &origin);
        assert_eq!(run(&home, &origin, 2), ["connect", "connect"]);
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert!(log_text(&home).contains("closed: no frame for 0.3s; retry in "));
    }

    /// Pongs keep a socket open past many silence spells on the clock. The test answers each
    /// ping before it moves the clock again, so the socket never goes a silence without a frame.
    #[test]
    fn pong_keeps_socket_open() {
        let (origin, seen, sockets) = held();
        let home = home("pong", &origin);
        let t = Timing {
            silence: TEST.silence,
            ..DRIVEN
        };
        let (d, serving) = drive(&home, &origin, 2, t, None, || None, &[]);
        let mut socket = sockets.recv().unwrap();
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        // Moves the clock a ping_every at a time until the reader pings, then answers. The reader
        // stamps a pong when it reads it, which may be after the clock moved, so a ping can take
        // a second move. Only three moves with no ping could reach silence.
        let step = |socket: &mut WebSocket<TcpStream>| {
            sleep(Duration::from_millis(50));
            loop {
                d.advance(t.ping_every);
                match socket.read() {
                    Ok(Message::Text(text)) if text.as_str() == "ping" => {
                        return send(socket, "pong")
                    }
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(e))
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) => {}
                    Err(e) => panic!("{e}"),
                }
            }
        };
        // The reader runs once it has logged a frame, so every ping below is one it sends.
        send(&mut socket, OTHER);
        d.wait_for("frame ignored");
        for _ in 0..10 {
            step(&mut socket);
        }
        d.wait_for("pickup pass (connect)");
        send(&mut socket, BOARD);
        d.wait_for("changed board.json");
        step(&mut socket);
        d.wait_for("pickup pass (changed board.json)");
        assert_eq!(serving.join().unwrap(), ["connect", "changed board.json"]);
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(!log_text(&home).contains("closed:"));
    }

    #[test]
    fn handshake_401_logs_access_message() {
        let (origin, _) = fake(Some(401), |_| sleep(Duration::from_secs(1)));
        let home = home("401", &origin);
        assert_eq!(run(&home, &origin, 2), ["fallback", "connect"]);
        let message = access_message(&home.join(".env"));
        assert!(log_text(&home).contains(&format!("connect failed: {message}; retry in ")));
    }

    /// A 403 comes from the Worker's host binding check, past Access, so it names the host record.
    #[test]
    fn handshake_403_logs_host_record_message() {
        let (origin, _) = fake(Some(403), |_| sleep(Duration::from_secs(1)));
        let home = home("403", &origin);
        assert_eq!(run(&home, &origin, 2), ["fallback", "connect"]);
        let log = log_text(&home);
        assert!(
            log.contains(&format!(
                "connect failed: {HOST_403}; if its service token was revoked or rotated in Access, check FACTORY_ACCESS_CLIENT_ID/SECRET in {}; retry in ",
                home.join(".env").display()
            )),
            "{log}"
        );
        assert!(!log.contains("Access rejected"), "{log}");
    }

    /// Past LOG_CAP the log keeps its last LOG_KEEP bytes from a line start, and a writer that
    /// opened it for append, as the service did, goes on at the new end.
    #[test]
    fn service_log_is_capped() {
        let path = temp_dir("daemon-log-cap").join("daemon.log");
        let line = format!("{}\n", "x".repeat(99));
        std::fs::write(&path, line.repeat(60_000)).unwrap();
        let mut service = OpenOptions::new().append(true).open(&path).unwrap();
        cap_log(&path);
        io::Write::write_all(&mut service, b"after\n").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.len() as u64 <= LOG_KEEP + 6, "{}", text.len());
        assert!(text.starts_with(&line) && text.ends_with(&format!("{line}after\n")));
        // Under the cap, nothing changes.
        std::fs::write(&path, &line).unwrap();
        cap_log(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), line);
    }

    /// A daemon.log that is a symlink is not followed, so its target keeps every byte.
    #[cfg(unix)]
    #[test]
    fn service_log_link_is_left_alone() {
        let dir = temp_dir("daemon-log-link");
        let target = dir.join("other.txt");
        let text = "y".repeat((LOG_CAP + 10) as usize);
        std::fs::write(&target, &text).unwrap();
        let path = dir.join("daemon.log");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        cap_log(&path);
        assert_eq!(std::fs::metadata(&target).unwrap().len(), text.len() as u64);
    }

    #[test]
    fn lock_reports_running_and_pid() {
        let home = temp_dir("daemon-lock");
        assert_eq!(running(&home), Some(false));
        let _first = take_lock(&home).unwrap().unwrap();
        assert!(take_lock(&home).unwrap().is_none());
        assert_eq!(running(&home), Some(true));
        assert_eq!(pid(&home), Some(std::process::id()));
    }

    #[test]
    fn pid_rejects_values_kill_would_misread() {
        let home = temp_dir("daemon-pid");
        let file = lock_dir(&home).join("daemon.pid");
        std::fs::create_dir_all(lock_dir(&home)).unwrap();
        for (text, want) in [
            ("0\n", None),
            ("-1\n", None),
            ("2147483648\n", None),
            ("4294967295\n", None),
            ("99999999999999999999\n", None),
            ("12ab\n", None),
            ("", None),
            ("2147483647\n", Some(i32::MAX as u32)),
            ("4242\n", Some(4242)),
        ] {
            std::fs::write(&file, text).unwrap();
            assert_eq!(pid(&home), want, "{text:?}");
        }
        assert_eq!(locked_pid(&home), Ok(None), "no lock, no pid to signal");
        let _lock = take_lock(&home).unwrap().unwrap();
        std::fs::write(&file, "0\n").unwrap();
        assert!(locked_pid(&home).is_err());
    }

    /// A second daemon for the same home, as a service restart or the cron line starts, returns
    /// Ok (exit 0) before it connects, prints or logs anything.
    #[test]
    fn second_daemon_exits_0_quietly() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let home = home("lock-held", &origin);
        let _first = take_lock(&home).unwrap().unwrap();
        assert!(super::run(&home).is_ok());
        listener.set_nonblocking(true).unwrap();
        assert!(listener.accept().is_err(), "the second daemon connected");
        let logs: Vec<_> = std::fs::read_dir(lock_dir(&home))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().ends_with(".log"))
            .collect();
        assert!(logs.is_empty(), "{logs:?}");
    }

    #[cfg(unix)]
    #[test]
    fn pickup_child_output_reaches_log() {
        use std::os::unix::fs::PermissionsExt;
        let home = temp_dir("daemon-pickup");
        let exe = home.join("fake-factory");
        std::fs::write(&exe, "#!/bin/sh\necho \"args: $*\"\nexit 7\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(run_pickup(&exe, &home, no_stop), (7, false, 0));
        assert!(log_text(&home).contains(&format!("args: --home {} pickup\n", home.display())));
        let dir = home.join("logs").join("daemon");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "log") {
                let mode = std::fs::metadata(path).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600);
            }
        }
        // Only this run's launched lines count, not an earlier run's in the same day log.
        std::fs::write(
            &exe,
            "#!/bin/sh\necho 'launched 1 alpha on primary session abc'\necho 'skip 2 alpha: paused'\n",
        )
        .unwrap();
        assert_eq!(run_pickup(&exe, &home, no_stop), (0, false, 1));
        assert_eq!(run_pickup(&exe, &home, no_stop), (0, false, 1));
    }

    /// A bare connection, as another test makes when it probes a port it freed, does not use up
    /// the first handshake's rejection.
    #[test]
    fn a_stray_connection_leaves_the_rejection_for_the_daemon() {
        let (origin, _) = fake(Some(403), |_| sleep(Duration::from_secs(1)));
        let port: u16 = origin.rsplit(':').next().unwrap().parse().unwrap();
        drop(TcpStream::connect(("127.0.0.1", port)).unwrap());
        sleep(Duration::from_millis(100));
        let home = home("stray", &origin);
        assert_eq!(run(&home, &origin, 2), ["fallback", "connect"]);
    }

    #[test]
    fn log_never_holds_the_secret() {
        let (origin, _) = fake(Some(403), |mut socket| {
            sleep(Duration::from_millis(100));
            send(&mut socket, r#"{"type":"note","rows":"x"}"#);
            send(&mut socket, BOARD);
            sleep(Duration::from_millis(300));
            let _ = socket.close(None);
            let _ = socket.flush();
            sleep(Duration::from_millis(200));
        });
        let home = home("secret", &origin);
        // The fallback pass at the failed first connect holds the connect pass to the minimum
        // gap, so the board frame may join it: only the first two passes are fixed.
        assert_eq!(run(&home, &origin, 3)[..2], ["fallback", "connect"]);
        let log = log_text(&home);
        assert!(log.contains("connect failed: ") && log.contains("frame ignored ("));
        assert!(!log.contains("test-secret"), "{log}");
    }

    #[test]
    fn missing_token_fails_2() {
        let home = temp_dir("daemon-token");
        let e = access_headers(&home, "https://f.example.test").unwrap_err();
        assert_eq!(e.code, 2);
        assert_eq!(e.stderr, format!("{}\n", token_message(&home.join(".env"))));
    }

    /// The server holds the connection open until connect returns, so only the watchdog can end the
    /// handshake, however slowly a loaded machine runs it. Without one, the hold ends after 60 s
    /// with a closed socket, not "timed out".
    #[test]
    fn stalled_tls_handshake_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (release, held_until) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            let held: Vec<_> = listener.incoming().take(1).collect();
            let _ = held_until.recv_timeout(Duration::from_secs(60));
            drop(held);
        });
        let t = Timing {
            connect_timeout: Duration::from_millis(300),
            ..TEST
        };
        let url = format!("wss://127.0.0.1:{port}{HOST_EVENTS_PATH}");
        let e = connect(&url, &[], &t, Path::new(".env")).err().unwrap();
        let _ = release.send(());
        assert_eq!(e, "timed out");
    }

    #[test]
    fn connect_tries_every_address() {
        let (origin, seen) = fake(None, |_| sleep(Duration::from_secs(1)));
        let url = host_url(&origin).replace("127.0.0.1", "localhost");
        assert!(connect(&url, &[], &TEST, Path::new(".env")).is_ok());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    /// The reader closes once silence has passed since the last frame, and not a moment before,
    /// however many read timeouts that takes.
    #[test]
    fn silence_closes_on_time() {
        let (origin, _, sockets) = held();
        let (t, clock) = on_fake_clock(Timing {
            silence: Duration::from_millis(240),
            connect_timeout: DRIVEN.connect_timeout,
            ..TEST
        });
        let socket = connect(&host_url(&origin), &[], &t, Path::new(".env")).unwrap();
        let mut server = sockets.recv().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || read_loop(socket, &t, no_stop, tx, None));
        send(&mut server, OTHER);
        assert!(matches!(rx.recv(), Ok(Event::Ignored(_))));
        *clock.lock().unwrap() += t.silence - Duration::from_millis(1);
        // Ten read timeouts on an idle machine, on a clock 1 ms short of silence.
        assert!(matches!(
            rx.recv_timeout(tick(&t) * 10),
            Err(RecvTimeoutError::Timeout)
        ));
        *clock.lock().unwrap() += Duration::from_millis(1);
        let Ok(Event::Closed(reason)) = rx.recv_timeout(Duration::from_secs(60)) else {
            panic!("expected Closed")
        };
        assert_eq!(reason, "no frame for 0.2s");
    }

    /// Each pass refreshes live claims first, in one board write: the first pass writes the two
    /// live sessions' seen in one PATCH and leaves the dead one, and the next pass, which reads
    /// them fresh, writes nothing and never asks paseo.
    #[test]
    fn each_pass_refreshes_live_claims_once() {
        use crate::commands::heartbeat::{report, tests as hb, Heart};
        let old = hb::minutes_ago(30);
        let fresh = hb::minutes_ago(0);
        let jobs = |seen: &str| {
            vec![
                hb::job(
                    "001-a",
                    "build",
                    hb::claim("mac", "s1", seen),
                    Some((hb::SERVER, "a1")),
                ),
                hb::job(
                    "002-b",
                    "build",
                    hb::claim("mac", "s2", &old),
                    Some((hb::SERVER, "dead")),
                ),
                hb::job(
                    "003-c",
                    "review",
                    hb::claim("mac", "s3", seen),
                    Some((hb::SERVER, "orch")),
                ),
            ]
        };
        let mut after = hb::board_of(jobs(&fresh));
        after["jobs"][1]["claim"]["seen"] = hb::minutes_ago(5).into();
        let (api_origin, requests) = crate::shared::http::tests::serve(vec![
            hb::get(&hb::board_of(jobs(&old))),
            (200, "{}", vec![]),
            hb::get(&after),
        ]);
        let home = home("heartbeat", &api_origin);
        hb::mark(&home, &hb::board_of(jobs(&old)));
        let origin = quiet();
        let fake = hb::Fake::new(
            serde_json::json!([
                hb::agent("a1", "running", "factory 001-a"),
                hb::agent("dead", "closed", "factory 002-b"),
                hb::agent("orch", "idle", "factory 003-c"),
                hb::agent("sub", "running", "x"),
            ]),
            vec![(
                "orch",
                serde_json::json!([hb::agent("sub", "running", "x")]),
            )],
        );
        let mut heart = Heart::default();
        let headers = access_headers(&home, &origin).unwrap();
        let mut passes = Vec::new();
        serve(
            &host_url(&origin),
            &headers,
            &TEST,
            &home.join(".env"),
            None,
            no_stop,
            &|msg| log_line(&home, msg),
            &|| None,
            &mut |reason| {
                let result = heart.beat_with(
                    &home,
                    "mac",
                    hb::SERVER,
                    &|p| fake.list(p),
                    &crate::shared::http::api,
                );
                report(result, &|msg| log_line(&home, msg));
                passes.push(reason.to_string());
                // The first pass reports a launch, so a second follows at min_gap.
                if passes.len() == 2 {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(true)
                }
            },
        );
        assert_eq!(passes, ["connect", "room left"]);
        // Pass one lists the agents and asks for the idle orchestrator's children; pass two
        // finds every claim fresh and asks nothing.
        assert_eq!(fake.asked.borrow().len(), 2);
        let requests = requests.lock().unwrap();
        let methods: Vec<&str> = requests.iter().map(|r| r.method.as_str()).collect();
        assert_eq!(methods, ["GET", "PATCH", "GET"]);
        let log = log_text(&home);
        assert!(
            log.contains(" heartbeat: refreshed seen on 001-a 003-c\n"),
            "{log}"
        );
        assert_eq!(log.matches("heartbeat:").count(), 1, "{log}");
    }
}
