//! Subprocesses and host facts.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::exit::{fail, Res};
use super::py::{strip, universal_newlines};

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

fn code_of(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return -signal;
    }
    status.code().unwrap_or(1)
}

fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

fn wait(child: &mut Child, timeout: Option<Duration>) -> io::Result<ExitStatus> {
    let Some(timeout) = timeout else {
        return child.wait();
    };
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// `subprocess.run(argv, capture_output=True, text=True)`, with stdin closed.
pub fn capture(
    argv: &[&str],
    cwd: Option<&Path>,
    env: &[(&str, &str)],
    timeout: Option<Duration>,
) -> io::Result<Out> {
    let mut command = Command::new(argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.envs(env.iter().copied());
    let mut child = command.spawn()?;
    let out = drain(child.stdout.take().unwrap());
    let err = drain(child.stderr.take().unwrap());
    let status = wait(&mut child, timeout)?;
    let text = |h: thread::JoinHandle<Vec<u8>>| {
        universal_newlines(String::from_utf8_lossy(&h.join().unwrap()).into_owned())
    };
    Ok(Out {
        code: code_of(status),
        stdout: text(out),
        stderr: text(err),
    })
}

/// `run`: stdout, or "" when the command cannot start.
pub fn run(argv: &[&str]) -> String {
    capture(argv, None, &[], None)
        .map(|o| o.stdout)
        .unwrap_or_default()
}

fn git_out(cwd: &Path, args: &[&str]) -> Option<Out> {
    let cwd = cwd.to_string_lossy();
    let argv: Vec<&str> = ["git", "-C", &cwd]
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    capture(&argv, None, &[], None).ok()
}

/// git's stdout, or "" if it fails (cwd isn't a repo, say).
pub fn git(cwd: &Path, args: &[&str]) -> String {
    match git_out(cwd, args) {
        Some(o) if o.code == 0 => strip(&o.stdout).to_string(),
        _ => String::new(),
    }
}

/// git's stdout, failing 1 with its stderr if it fails.
pub fn git_ok(cwd: &Path, args: &[&str]) -> Res<String> {
    match git_out(cwd, args) {
        Some(o) if o.code == 0 => Ok(strip(&o.stdout).to_string()),
        Some(o) => Err(fail(
            format!("git {}: {}", args.join(" "), strip(&o.stderr)),
            1,
        )),
        None => Err(fail(format!("git {}: cannot run git", args.join(" ")), 1)),
    }
}

/// git's stdout for a command that talks to origin, failing 5 without git's stderr, which can
/// carry origin's URL and credentials.
pub fn origin_git(repo: &Path, args: &[&str]) -> Res<String> {
    let out = git_out(repo, args);
    match out {
        Some(o) if o.code == 0 => Ok(strip(&o.stdout).to_string()),
        other => {
            let code = other.map_or(1, |o| o.code);
            let repo = repo.display();
            Err(fail(
                format!(
                    "git {} origin failed in {repo} (exit {code}). Check the network, then run git -C {repo} fetch origin to see git's error",
                    args[0]
                ),
                5,
            ))
        }
    }
}

/// The paseo program every Paseo call runs: the path `which` finds, so Windows gets the desktop
/// app's `paseo.cmd`, which a bare `paseo` misses. On Windows with no paseo on PATH, the app's
/// bundled `paseo.cmd` under %LOCALAPPDATA%.
pub fn paseo_bin() -> Option<String> {
    which("paseo")
        .or_else(|| {
            cfg!(windows)
                .then(|| {
                    let local =
                        std::env::var_os("LOCALAPPDATA").filter(|v| Path::new(v).is_absolute())?;
                    app_paseo(&local)
                })
                .flatten()
        })
        .map(|p| p.to_string_lossy().into_owned())
}

/// The Paseo desktop app's bundled `paseo.cmd` under the folder %LOCALAPPDATA% names, when it exists.
fn app_paseo(local: &std::ffi::OsStr) -> Option<PathBuf> {
    let p = ["Programs", "Paseo", "resources", "bin", "paseo.cmd"]
        .iter()
        .fold(PathBuf::from(local), |p, part| p.join(part));
    p.is_file().then_some(p)
}

pub const BATCH_REFUSED: &str =
    "paseo on Windows runs through paseo.cmd, which can't pass this argument safely";

/// Whether bin is a `.cmd` or `.bat` file, which Windows runs through cmd.exe.
pub fn is_batch(bin: &str) -> bool {
    Path::new(bin)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// Whether bin is a batch file and one of args holds a character cmd.exe would change on its way
/// through `%*` to Paseo: a newline, a quote, or one of `%^&|<>`.
pub fn batch_unsafe(bin: &str, args: &[&str]) -> bool {
    is_batch(bin)
        && args
            .iter()
            .any(|a| a.contains(['\n', '\r', '"', '%', '^', '&', '|', '<', '>']))
}

/// `capture` for a paseo argv, which refuses with BATCH_REFUSED, printed to stderr too, rather than
/// send an argument paseo.cmd would mangle.
pub fn paseo_capture(argv: &[&str], timeout: Duration) -> io::Result<Out> {
    if batch_unsafe(argv[0], &argv[1..]) {
        eprintln!("factory: {BATCH_REFUSED}");
        return Err(io::Error::new(io::ErrorKind::InvalidInput, BATCH_REFUSED));
    }
    capture(argv, None, &[], Some(timeout))
}

/// (exit code, stdout) of one paseo command, or (1, "") when it cannot run.
pub fn paseo_run(args: &[&str]) -> (i32, String) {
    match paseo_bin() {
        Some(bin) => paseo_run_with(&bin, args),
        None => (1, String::new()),
    }
}

/// `paseo_run` with the paseo binary at bin.
pub fn paseo_run_with(bin: &str, args: &[&str]) -> (i32, String) {
    let home = std::env::var("PASEO_HOME").unwrap_or_default();
    let mut argv = vec![bin];
    if !home.is_empty() {
        argv.extend(["--home", home.as_str()]);
    }
    argv.extend(args);
    match paseo_capture(&argv, Duration::from_secs(20)) {
        Ok(o) => (o.code, o.stdout),
        Err(_) => (1, String::new()),
    }
}

pub(crate) fn executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    return meta.is_file()
        && std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
    #[cfg(not(unix))]
    meta.is_file()
}

/// `shutil.which`.
pub fn which(name: &str) -> Option<PathBuf> {
    let pathext = cfg!(windows)
        .then(|| std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into()));
    which_in(
        name,
        std::env::var_os("PATH").as_deref(),
        pathext.as_deref(),
    )
}

/// `which` over path, with Windows' PATHEXT rules when pathext is given. As in `shutil.which`, a
/// name that ends in a PATHEXT extension is tried bare first, and any other name only with each
/// suffix, so an extensionless npm shim never wins over its `.cmd`.
fn which_in(name: &str, path: Option<&std::ffi::OsStr>, pathext: Option<&str>) -> Option<PathBuf> {
    let suffixes: Vec<String> = match pathext {
        Some(pathext) => {
            let exts: Vec<&str> = pathext.split(';').filter(|s| !s.is_empty()).collect();
            let upper = name.to_uppercase();
            let bare = exts.iter().any(|e| upper.ends_with(&e.to_uppercase()));
            bare.then(String::new)
                .into_iter()
                .chain(exts.iter().map(|e| e.to_string()))
                .collect()
        }
        None => vec![String::new()],
    };
    let candidates = |dir: &Path| {
        suffixes
            .iter()
            .map(|s| dir.join(format!("{name}{s}")))
            .collect::<Vec<_>>()
    };
    if Path::new(name)
        .parent()
        .is_some_and(|p| !p.as_os_str().is_empty())
    {
        return candidates(Path::new(""))
            .into_iter()
            .find(|p| executable(p));
    }
    std::env::split_paths(path?)
        .flat_map(|dir| candidates(&dir))
        .find(|p| executable(p))
}

#[cfg(unix)]
pub fn hostname() -> String {
    extern "C" {
        fn gethostname(name: *mut std::ffi::c_char, len: usize) -> std::ffi::c_int;
    }
    let mut buf = [0u8; 256];
    // SAFETY: buf is writable for its full length, and gethostname writes at most len bytes.
    let rc = unsafe { gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(windows)]
pub fn hostname() -> String {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetComputerNameExW(kind: i32, buffer: *mut u16, size: *mut u32) -> i32;
    }
    const PHYSICAL_DNS_HOSTNAME: i32 = 5;
    let mut buf = [0u16; 256];
    let mut size = buf.len() as u32;
    // SAFETY: buf holds size UTF-16 units, and the call writes at most that many and updates size.
    let ok = unsafe { GetComputerNameExW(PHYSICAL_DNS_HOSTNAME, buf.as_mut_ptr(), &mut size) };
    if ok == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..size as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn capture_translates_newlines() {
        let out = capture(
            &["sh", "-c", "printf 'a\\r\\nb'; printf 'e' >&2; exit 3"],
            None,
            &[],
            None,
        )
        .unwrap();
        assert_eq!(
            (out.code, out.stdout.as_str(), out.stderr.as_str()),
            (3, "a\nb", "e")
        );
    }

    #[cfg(unix)]
    #[test]
    fn capture_times_out() {
        let err = capture(&["sleep", "5"], None, &[], Some(Duration::from_millis(50)))
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[cfg(unix)]
    #[test]
    fn host_facts() {
        assert!(!hostname().is_empty());
        assert!(which("sh").is_some());
        assert!(which("no-such-tool-here").is_none());
        assert_eq!(git(Path::new("/"), &["rev-parse", "--no-such-flag"]), "");
    }

    #[test]
    fn batch_paseo_refuses_what_cmd_would_change() {
        let cmd = r"C:\Paseo\resources\bin\paseo.cmd";
        assert!(!batch_unsafe(
            cmd,
            &["schedule", "inspect", "0a1b2c3d", "--json"]
        ));
        assert!(!batch_unsafe(
            cmd,
            &["send", "agent-1", "a plain message, with spaces."]
        ));
        for bad in [
            "line one\nline two",
            "a\rb",
            "say \"hi\"",
            "100%",
            "a^b",
            "a&b",
            "a|b",
            "<a>",
        ] {
            assert!(batch_unsafe(cmd, &["send", "agent-1", bad]), "{bad:?}");
            assert!(batch_unsafe("PASEO.BAT", &[bad]), "{bad:?}");
            assert!(!batch_unsafe("/usr/local/bin/paseo", &[bad]), "{bad:?}");
        }
        let refused = paseo_capture(&[cmd, "send", "agent-1", "a\nb"], Duration::from_secs(1));
        assert_eq!(refused.err().unwrap().to_string(), BATCH_REFUSED);
    }

    #[test]
    fn app_paseo_is_the_bundled_cmd_when_present() {
        let local = crate::shared::fs::temp_dir("app-paseo");
        assert_eq!(app_paseo(local.as_os_str()), None);
        let bin = local.join("Programs/Paseo/resources/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("paseo.cmd"), "@echo off\r\n").unwrap();
        assert_eq!(app_paseo(local.as_os_str()), Some(bin.join("paseo.cmd")));
        std::fs::remove_dir_all(local).unwrap();
    }

    /// An npm global install puts an extensionless sh shim beside paseo.cmd. With PATHEXT rules the
    /// .cmd wins, as in `shutil.which`; a name that already ends in a PATHEXT extension is tried bare.
    /// Without them, as on POSIX, the bare name is the only candidate.
    #[test]
    fn which_skips_a_bare_shim_under_pathext() {
        let dir = crate::shared::fs::temp_dir("which-pathext");
        for name in ["paseo", "paseo.cmd"] {
            std::fs::write(dir.join(name), "synthetic\n").unwrap();
            #[cfg(unix)]
            std::fs::set_permissions(
                dir.join(name),
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            )
            .unwrap();
        }
        let path = Some(dir.as_os_str());
        // Lower case, since Windows matches names without case and the test may run on POSIX.
        let pathext = Some(".com;.exe;.bat;.cmd");
        assert_eq!(
            which_in("paseo", path, pathext),
            Some(dir.join("paseo.cmd"))
        );
        assert_eq!(
            which_in("paseo.cmd", path, pathext),
            Some(dir.join("paseo.cmd"))
        );
        assert_eq!(which_in("paseo", path, None), Some(dir.join("paseo")));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
