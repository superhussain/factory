pub mod board;
pub mod board_client;
pub mod build;
pub mod conf;
pub mod consts;
pub mod exit;
pub mod fs;
pub mod home;
pub mod http;
mod nfkc_table;
pub mod paseo;
pub mod proc;
pub mod project;
pub mod py;
pub mod pyjson;
pub mod pyre;
pub mod pyval;
pub mod redact;
pub mod time;
pub mod unicode;
mod unicode_printable;

/// Held by every test that sets PASEO_HOME, PASEO_AGENT_ID, FACTORY_HOME or FACTORY_SESSION, since tests share
/// one process environment.
#[cfg(test)]
static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The libtest thread name of the last test to take TEST_ENV_LOCK.
#[cfg(test)]
static TEST_ENV_HOLDER: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Runs the test `module::name` again alone in a child test process. Returns true in that child,
/// where the test body runs, and false in the parent once the child passed. A test whose effects
/// leak to every thread uses it: one that empties PATH, which parallel tests need to find git, or
/// one that takes a lock whose descriptor a parallel test's fork would copy.
#[cfg(test)]
pub(crate) fn in_own_process(module: &str, name: &str) -> bool {
    const ALONE: &str = "FACTORY_TEST_ALONE";
    if std::env::var_os(ALONE).is_some() {
        return true;
    }
    let test = format!("{}::{name}", module.split_once("::").unwrap().1);
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &test, "--test-threads=1"])
        .env(ALONE, "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && text.contains("1 passed"),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    false
}

/// Takes TEST_ENV_LOCK, or panics naming its holder after 300 s.
#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    test_env_lock_within(std::time::Duration::from_secs(300))
}

#[cfg(test)]
pub(crate) fn test_env_lock_within(
    wait: std::time::Duration,
) -> std::sync::MutexGuard<'static, ()> {
    use std::sync::TryLockError;
    let start = std::time::Instant::now();
    loop {
        let guard = match TEST_ENV_LOCK.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(e)) => e.into_inner(),
            Err(TryLockError::WouldBlock) => {
                if start.elapsed() > wait {
                    let holder = TEST_ENV_HOLDER.lock().unwrap_or_else(|e| e.into_inner());
                    panic!("TEST_ENV_LOCK still held after {wait:?} by {holder}");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            }
        };
        let name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();
        *TEST_ENV_HOLDER.lock().unwrap_or_else(|e| e.into_inner()) = name;
        return guard;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn env_lock_wait_names_the_holder() {
        let (taken_tx, taken_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = std::thread::Builder::new()
            .name("holder-test".to_string())
            .spawn(move || {
                let _guard = test_env_lock();
                taken_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
            .unwrap();
        taken_rx.recv().unwrap();
        let waited = std::panic::catch_unwind(|| test_env_lock_within(Duration::from_millis(200)));
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        let message = *waited.unwrap_err().downcast::<String>().unwrap();
        assert_eq!(
            message,
            "TEST_ENV_LOCK still held after 200ms by holder-test"
        );
    }
}
