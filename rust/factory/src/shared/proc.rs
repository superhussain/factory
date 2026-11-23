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

/// (exit code, stdout) of one paseo command, or (1, "") when it cannot run.
pub fn paseo_run(args: &[&str]) -> (i32, String) {
    let home = std::env::var("PASEO_HOME").unwrap_or_default();
    let mut argv = vec!["paseo"];
    if !home.is_empty() {
        argv.extend(["--home", home.as_str()]);
    }
    argv.extend(args);
    match capture(&argv, None, &[], Some(Duration::from_secs(20))) {
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
    let suffixes: Vec<String> = if cfg!(windows) {
        let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        std::iter::once(String::new())
            .chain(
                pathext
                    .split(';')
                    .filter(|s| !s.is_empty())
                    .map(String::from),
            )
            .collect()
    } else {
        vec![String::new()]
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
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
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
}
