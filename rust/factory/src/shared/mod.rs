pub mod board;
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
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
