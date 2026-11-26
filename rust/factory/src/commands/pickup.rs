//! `pickup` and `pickup setup`: launchers, detached launches, stall takeover, the host timer and the
//! board passes.

use crate::commands::checks;
use crate::commands::claims::{self, mine};
use crate::commands::daemon;
use crate::commands::ops;
use crate::commands::tracer::SPEC_APPROVED_NOTES;
use crate::shared::pyval::truthy;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(test)]
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Number, Value};

use crate::cli::Call;
use crate::shared::board::{dump_board, escalated, lead_of, load_board, patch_job};
use crate::shared::conf::{fm_get, frontmatter, read_conf};
use crate::shared::consts::{
    tag_lookalike, ADAPTER_ID, BACKLOG_NOTES_TAG, BACKPRESSURE_NEEDS_YOU, HOLD_MAX_HOURS,
    HOLD_MIN_IDEAS, IDEA, IN_FLIGHT, JOB_ID, MAX_RIDERS, NEW_JOB_ID, NOTE_INVISIBLE, NOTE_LINK,
    NOTE_PHRASES, NOTE_SECRETS, NOTE_TAG, PASEO_ID, REVIEW_ROUTE_KEYS, ROUTE_EFFORTS, ROUTE_MODEL,
    SLUG, TERMINAL, UNATTENDED, WINDOWS,
};
use crate::shared::exit::{exit, fail, Exit, Res};
use crate::shared::fs::{is_link, read_text, regular_metadata, resolve, write_atomic};
use crate::shared::home::{factory_home, holder, skill_root, validate_home};
use crate::shared::http::{endpoint_message, normalized_origin, token_message, Response};
use crate::shared::proc::{capture, git, hostname, paseo_bin, paseo_capture, which, Out};
use crate::shared::project::{project_files, read_paths};
use crate::shared::py::{is_space, os_error, printable_line, repr, splitlines, strip};
use crate::shared::pyjson::{dumps, loads, DEFAULT, INDENT2};
use crate::shared::pyre::{Flags as Re, Pattern};
use crate::shared::time::{format_stamp, now, parse_stamp, unix_now};
use crate::shared::unicode::nfkc;

pub(crate) struct Io<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

macro_rules! outln {
    ($io:expr, $($t:tt)*) => {{ let _ = writeln!($io.out, $($t)*); }};
}
macro_rules! errln {
    ($io:expr, $($t:tt)*) => {{ let _ = writeln!($io.err, $($t)*); }};
}

const USAGE_PICKUP: &str =
    "usage: pickup [--dry-run] [--triage-now] [--limit N] [--max-inflight N]";
const USAGE_PICKUP_SETUP: &str = "usage: pickup setup [--install]";
const USAGE_DAEMON_SETUP: &str = "usage: daemon setup [--install | --uninstall]";
const TRIAGE_SWEEP_MINUTES: i64 = 30;
const FETCH_TIMEOUT_SECONDS: u64 = 60;
const SCHEDULERS: &[&str] = &["launchd", "systemd", "cron", "schtasks", "none"];
const PICKUP_SCHEDULER: &str = if cfg!(target_os = "macos") {
    "launchd"
} else if cfg!(windows) {
    "schtasks"
} else {
    "cron"
};
const ORPHAN_SCAN_FILE: &str = "orphan-scan.json";
const OUTCOMES_RECORD_FILE: &str = "outcomes-record.json";
const RETRO_PUSH_FILE: &str = "retro-push.json";
const AUDIT_PUSH_FILE: &str = "audit-push.json";
const ORPHAN_KINDS: &[&str] = &["branch", "pr", "worktree", "workspace"];
const ORPHAN_NOUNS: &[(&str, &str)] = &[
    ("branch", "branches"),
    ("open PR", "open PRs"),
    ("worktree", "worktrees"),
    ("Paseo workspace", "Paseo workspaces"),
];
static HOST_NAME: Pattern = Pattern::new("[A-Za-z0-9][A-Za-z0-9._-]{0,63}", Re::ASCII);
static WORKER_MODEL: Pattern = Pattern::new(r"[A-Za-z0-9._:@/\[\]-]{1,128}", Re::ASCII);
static REPO_PATH: Pattern = Pattern::new("[A-Za-z0-9_./-]{1,200}", Re::ASCII);
static CHECKOUT_SLUG: Pattern = Pattern::new("[A-Za-z0-9_-]+", Re::NONE);
static POSITIVE: Pattern = Pattern::new("[1-9][0-9]*", Re::NONE);
static ENV_ASSIGN: Pattern = Pattern::new("(?<![A-Za-z0-9_])[A-Za-z_][A-Za-z0-9_]*=", Re::NONE);
static COMMIT: Pattern = Pattern::new("[0-9a-f]{40}", Re::ASCII);
static TRAILING_NUMBER: Pattern = Pattern::new(r"-\d+$", Re::NONE);
/// An outcomes record line's `<slug> unknown: ` tail: the first unknown job and its reason.
static OUTCOME_UNKNOWN: Pattern = Pattern::new(r"(\S+) \((.*?)\)(?:, |$)", Re::NONE);

// ---- test seams

#[cfg(test)]
type Api = Rc<dyn Fn(&Path, &str, &str, Option<&[u8]>, &[u16], &[(&str, &str)]) -> Res<Response>>;
#[cfg(test)]
type Hook = Option<Rc<dyn Fn(&Path)>>;
#[cfg(test)]
type Launch = Rc<
    dyn Fn(
        &Path,
        &Entry,
        &str,
        &[String],
        &[(String, String)],
    ) -> Res<(bool, Option<String>, PathBuf)>,
>;

/// One field per function or constant the Python pickup tests patch.
#[cfg(test)]
#[derive(Default)]
struct Fakes {
    api: Option<Api>,
    report_status: Option<Rc<dyn Fn(&Path, &str, Option<&str>, Option<&str>) -> bool>>,
    backlog_triage_pass: Option<Rc<dyn Fn(&Path) -> Res<()>>>,
    checkout_problem: Option<Rc<dyn Fn(&Path, &str, &str) -> Res<Option<String>>>>,
    tracer_follow_pass: Option<Rc<dyn Fn(&Path) -> Res<()>>>,
    tracer_build_pass: Option<Rc<dyn Fn(&Path, Option<&Value>) -> Res<bool>>>,
    gh_cli: Option<Rc<dyn Fn(&[&str]) -> Option<(Option<i32>, String)>>>,
    paseo_run: Option<Rc<dyn Fn(&[&str]) -> (i32, String)>>,
    route_launcher: Option<Rc<dyn Fn(&Path, &Route) -> Res<Vec<String>>>>,
    read_paths: Option<Rc<dyn Fn(&Path) -> Vec<(String, String)>>>,
    read_board: Option<Rc<dyn Fn(&Path) -> Res<Value>>>,
    doctor: Option<Rc<dyn Fn(&Path) -> Res<Value>>>,
    bundle_stamp: Option<Rc<dyn Fn() -> Option<Value>>>,
    setup_run: Option<Rc<dyn Fn(&[&str], Option<&str>) -> (i32, String)>>,
    pickup_pass: Option<Rc<dyn Fn(&mut Io, &Path) -> Res<()>>>,
    orphan_pickup_scan: Hook,
    orphan_scan: Option<Rc<dyn Fn(&Path) -> Res<(Vec<Orphan>, Vec<String>)>>>,
    outcomes_pickup_record: Hook,
    pick_route: Option<Rc<dyn Fn(&Path) -> Res<Option<Route>>>>,
    launch_idea: Option<Launch>,
    retro_catchup_due: Option<Rc<dyn Fn(&Path, &Local) -> bool>>,
    daily_tokens: Option<Rc<dyn Fn(i64) -> Result<i64, String>>>,
    record_agent: Option<Rc<dyn Fn(&Path, &str, &Value) -> Res<Option<Value>>>>,
    paseo_server: Option<Rc<dyn Fn() -> Option<String>>>,
    paseo_statuses: Option<Rc<dyn Fn() -> Option<Vec<(Value, Value)>>>>,
    paseo_send: Option<Rc<dyn Fn(&str, &str) -> bool>>,
    awaits_permission: Option<Rc<dyn Fn(&str) -> Option<bool>>>,
    usage_totals: Option<Rc<dyn Fn(&Path, &str, &str) -> Result<Value, String>>>,
    auto_babysit_pass: Option<Rc<dyn Fn(&mut Io, &Path) -> Res<()>>>,
    auto_babysit_limit: Option<usize>,
    retro_push: Option<Rc<dyn Fn() -> Res<()>>>,
    audit_stamp: Option<Rc<dyn Fn(&str) -> Option<i128>>>,
    audit_push: Option<Rc<dyn Fn(&str) -> Res<()>>>,
    usage_record: Option<Rc<dyn Fn(&Path, &str, &str, &str, &Value) -> Res<()>>>,
    outcomes: Option<Rc<dyn Fn(&str, &mut String) -> Res<()>>>,
    stop_pids: Option<Rc<dyn Fn(&Path) -> Option<Vec<u32>>>>,
    close_pull_request: Option<Rc<dyn Fn(&Path, &Value, &str) -> Res<(&'static str, String)>>>,
    close_stacked_prs: Option<Rc<dyn Fn(&Path, &str, &[Value], &str) -> Res<()>>>,
    pr_states:
        Option<Rc<dyn Fn(&str, &[u64], &str) -> Result<BTreeMap<u64, (Value, Value)>, String>>>,
    pr_signals: Option<
        Rc<
            dyn Fn(
                &str,
                &[u64],
                &str,
                &BTreeMap<u64, Map<String, Value>>,
            ) -> Result<Signals, String>,
        >,
    >,
    cleanup_job:
        Option<Rc<dyn Fn(&str, &Path, &str, bool, Option<&str>) -> (Vec<String>, Vec<String>)>>,
    pickup_scheduler: Option<&'static str>,
    daemon_scheduler: Option<&'static str>,
    launch_wait_seconds: Option<f64>,
    exe: Option<String>,
}

#[cfg(test)]
thread_local! {
    static FAKES: RefCell<Fakes> = RefCell::new(Fakes::default());
}

#[cfg(test)]
macro_rules! fake {
    ($name:ident) => {
        FAKES.with(|f| f.borrow().$name.clone())
    };
}

/// The Worker client, through the seam the tests patch.
fn api(
    home: &Path,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    ok: &[u16],
    headers: &[(&str, &str)],
) -> Res<Response> {
    #[cfg(test)]
    if let Some(f) = fake!(api) {
        return f(home, method, path, body, ok, headers);
    }
    crate::shared::http::api(home, method, path, body, ok, headers)
}

#[cfg(not(test))]
use crate::shared::board::read_board as shared_read_board;
#[cfg(not(test))]
use crate::shared::http::cas;

/// The shared `cas` on this file's `api`, so a faked `api` reaches it, as it reaches Python's.
#[cfg(test)]
fn cas<T>(
    home: &Path,
    path: &str,
    mut change: impl FnMut(Option<&str>) -> Res<(Option<String>, T)>,
) -> Res<T> {
    for _ in 0..20 {
        let read = api(home, "GET", path, None, &[200, 404], &[])?;
        let cond = match read.header("ETag") {
            Some(etag) if read.status == 200 => (
                "If-Match",
                etag.strip_prefix("W/").unwrap_or(etag).to_string(),
            ),
            None if read.status == 200 => {
                return Err(fail(format!("{path}: the Worker sent no ETag"), 1))
            }
            _ => ("If-None-Match", "*".to_string()),
        };
        let text = (read.status == 200).then(|| read.text()).transpose()?;
        let (new, result) = change(text.as_deref())?;
        let Some(new) = new else { return Ok(result) };
        let put = api(
            home,
            "PUT",
            path,
            Some(new.as_bytes()),
            &[200, 412],
            &[(cond.0, &cond.1)],
        )?;
        if put.status == 200 {
            return Ok(result);
        }
    }
    Err(fail(
        format!("{path} kept changing under us; gave up after 20 tries"),
        5,
    ))
}

/// The board `cas` for a claim, through this file's `api` seam. The claim decides from rows it
/// doesn't write, so these go in as reads and the Worker refuses the write if one changed: every
/// open row, and under a cap every row the cap counts, merged or closed too, since a reopened
/// one would count again.
fn claim_cas<T>(
    home: &Path,
    pick: &Pick,
    device: &str,
    change: impl FnMut(Option<&str>) -> Res<(Option<String>, T)>,
) -> Res<T> {
    let scope = |j: &Value| {
        crate::shared::board_client::open_row(j)
            || match pick.max_inflight {
                None => false,
                Some(_) if pick.by_host => {
                    j.get("claim")
                        .and_then(|c| c.get("device"))
                        .and_then(Value::as_str)
                        == Some(device)
                }
                Some(_) => pick
                    .project
                    .is_none_or(|p| get_str(j, "project") == Some(p)),
            }
    };
    crate::shared::board_client::cas_via(home, Some(&scope), &api, change)
}

#[cfg(test)]
fn shared_read_board(home: &Path) -> Res<Value> {
    let r = api(home, "GET", "/api/board", None, &[200, 404], &[])?;
    let text = (r.status == 200).then(|| r.text()).transpose()?;
    crate::shared::board::load_board(text.as_deref())
}

fn read_board(home: &Path) -> Res<Value> {
    #[cfg(test)]
    if let Some(f) = fake!(read_board) {
        return f(home);
    }
    shared_read_board(home)
}

/// `read_paths` as a dict: a repeated slug keeps its first place and its last value.
fn paths_of(home: &Path) -> Res<Vec<(String, String)>> {
    #[cfg(test)]
    if let Some(f) = fake!(read_paths) {
        return Ok(dict(f(home)));
    }
    Ok(dict(read_paths(home)?))
}

fn paseo_run(args: &[&str]) -> (i32, String) {
    #[cfg(test)]
    if let Some(f) = fake!(paseo_run) {
        return f(args);
    }
    crate::shared::proc::paseo_run(args)
}

pub(crate) fn pickup_scheduler() -> &'static str {
    #[cfg(test)]
    if let Some(s) = FAKES.with(|f| f.borrow().pickup_scheduler) {
        return s;
    }
    PICKUP_SCHEDULER
}

mod babysit;
mod close;
mod frame;
mod helpers;
mod launch;
mod nudge;
mod pass;
mod passes;
mod setup;
mod stalls;
mod sync;
#[cfg(test)]
mod tests;
mod upkeep;
mod usage;
mod zone;

use self::{
    babysit::*, close::*, launch::*, nudge::*, pass::*, passes::*, sync::*, upkeep::*, usage::*,
    zone::*,
};
pub(crate) use self::{frame::*, helpers::*, launch::RETRO_PROMPT_FILE, setup::*, stalls::*};
