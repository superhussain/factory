//! checks, pr-state and pr-queue: reading a job's PR through gh.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cli::{not_ported, Call};
use crate::shared::board::{dump_board, escalated, load_board, patch_job, read_board};
use crate::shared::conf::{fm_get, frontmatter};
use crate::shared::consts::SLUG;
use crate::shared::exit::{fail, Res};
use crate::shared::fs::{is_link, read_text};
use crate::shared::http::cas;
use crate::shared::proc::{capture, which};
use crate::shared::project::{project_files, r2_target};
use crate::shared::py::{os_error, printable_line, splitlines, strip};
use crate::shared::pyjson::{dumps, loads, DEFAULT};
use crate::shared::pyre::{Flags, Pattern};
use crate::shared::time::{parse_stamp, unix_now};

const CHECKS_USAGE: &str = "usage: checks <project-slug>/<job-id> [--timeout S]";
const CHECKS_INTERVAL: f64 = 10.0;
const CHECKS_REGISTER: u64 = 120;
const HEAD_CHECKS_QUERY: &str = "query=query($o:String!,$r:String!,$sha:GitObjectID!){repository(owner:$o,name:$r){object(oid:$sha){...on Commit{statusCheckRollup{contexts(first:100){pageInfo{hasNextPage} nodes{__typename ...on CheckRun{name status conclusion startedAt} ...on StatusContext{context state createdAt}}}}}}}}";
const AUTONOMY_DAYS: i64 = 90;
const AUTONOMY_CLASSES: &[&str] = &["internal", "visible"];
const AUTONOMY_HOLD_NOTE: &str = "earned autonomy hold: a person reviews and merges";

static GH_PR: Pattern = Pattern::new(r"https://github\.com/([\w.-]+/[\w.-]+)/pull/\d+", Flags::ASCII);
static GH_PR_PARTS: Pattern = Pattern::new(r"https://github\.com/([\w.-]+)/([\w.-]+)/pull/(\d{1,9})", Flags::ASCII);
static BRANCH: Pattern = Pattern::new(r"(?!-)[A-Za-z0-9._/-]{1,255}", Flags::NONE);
static COMMIT: Pattern = Pattern::new(r"[0-9a-f]{40}", Flags::NONE);
static NO_CHECKS: Pattern = Pattern::new(r"no (required )?checks reported", Flags::NONE);

#[derive(Clone)]
pub(crate) enum GhCode {
    Exit(i32),
    Timeout,
}

/// What `gh_cli` returns: the exit code, or a timeout, and both streams.
#[derive(Clone)]
pub(crate) struct GhOut {
    pub code: GhCode,
    pub stdout: String,
    pub stderr: String,
}

/// gh and the clock, so tests can answer gh and skip the sleeps.
pub(crate) trait Host {
    /// gh's answer, or None when gh is not installed.
    fn gh(&mut self, args: &[&str]) -> Option<GhOut>;
    fn monotonic(&mut self) -> f64;
    fn sleep(&mut self, secs: f64);
    fn unix_time(&mut self) -> i64;
}

pub(crate) struct System {
    start: Instant,
}

impl System {
    pub(crate) fn new() -> System {
        System { start: Instant::now() }
    }
}

impl Host for System {
    fn gh(&mut self, args: &[&str]) -> Option<GhOut> {
        let gh = which("gh")?;
        let gh = gh.to_string_lossy();
        let argv: Vec<&str> = std::iter::once(gh.as_ref()).chain(args.iter().copied()).collect();
        let empty = |code| GhOut { code, stdout: String::new(), stderr: String::new() };
        Some(match capture(&argv, None, &[], Some(Duration::from_secs(60))) {
            Ok(o) => GhOut { code: GhCode::Exit(o.code), stdout: o.stdout, stderr: o.stderr },
            Err(e) if e.kind() == io::ErrorKind::TimedOut => empty(GhCode::Timeout),
            Err(_) => empty(GhCode::Exit(1)),
        })
    }

    fn monotonic(&mut self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn sleep(&mut self, secs: f64) {
        std::thread::sleep(Duration::from_secs_f64(secs));
    }

    fn unix_time(&mut self) -> i64 {
        unix_now()
    }
}

/// Python's `str()` of a JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        other => dumps(other, DEFAULT),
    }
}

/// Python's `bool()` of a JSON value.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
    }
}

/// `str(v or "")`.
fn str_or_empty(v: Option<&Value>) -> String {
    v.filter(|v| truthy(v)).map(py_str).unwrap_or_default()
}

/// `int(s)` for text that fully matches `[0-9]+`, else None. Past 18 digits it saturates.
fn small_int(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits = s.trim_start_matches('0');
    Some(if digits.len() > 18 { u64::MAX } else { digits.parse().unwrap_or(0) })
}

/// stdout of a gh answer, failing with exit 5 when gh cannot answer.
fn gh_answer(r: Option<GhOut>, what: &str) -> Res<String> {
    let Some(r) = r else { return Err(fail("gh is not installed", 5)) };
    match r.code {
        GhCode::Timeout => Err(fail(format!("{what} timed out after 60 seconds"), 5)),
        GhCode::Exit(0) => Ok(r.stdout),
        GhCode::Exit(code) => Err(fail(format!("{what} exited {code} (check gh auth status)"), 5)),
    }
}

/// stdout of gh pr view <pr> <args>, failing with exit 5 when gh cannot answer.
fn gh_pr_view(h: &mut dyn Host, pr: &str, args: &[&str]) -> Res<String> {
    let argv: Vec<&str> = ["pr", "view", pr].into_iter().chain(args.iter().copied()).collect();
    gh_answer(h.gh(&argv), "gh pr view")
}

fn find_job<'a>(data: &'a Value, job_id: &str) -> Option<&'a Value> {
    data["jobs"].as_array()?.iter().find(|j| j.get("id").and_then(Value::as_str) == Some(job_id))
}

fn project_meta(home: &Path, slug: &str) -> Res<Vec<(String, String)>> {
    let path = project_files(home, slug)?.0;
    Ok(frontmatter(&path).map_err(|e| fail(os_error(&e, Some(&path)), 1))?.0)
}

/// The job's PR URL from the board, after checking it is a pull request in the project file repo,
/// or in its archive repo when archive is true.
pub(crate) fn board_pr(home: &Path, slug: &str, job_id: &str, archive: bool) -> Res<String> {
    let data = read_board(home)?;
    let pr = match find_job(&data, job_id).and_then(|j| j.get("pr")) {
        Some(Value::String(pr)) if !pr.is_empty() => pr.clone(),
        _ => return Err(fail(format!("{job_id} has no pr on the board"), 4)),
    };
    let meta = project_meta(home, slug)?;
    let archive_repo = if archive { fm_get(&meta, "archive").unwrap_or("") } else { "" };
    let repos: Vec<&str> = [fm_get(&meta, "repo").unwrap_or(""), archive_repo].into_iter().filter(|r| !r.is_empty()).collect();
    let found = GH_PR.fullmatch(&pr).is_some_and(|m| {
        let repo = m.get(1).unwrap().as_str().to_lowercase();
        repos.iter().any(|r| r.to_lowercase() == repo)
    });
    if !found {
        let names = if repos.is_empty() { "the project file repo".to_string() } else { repos.join(" or ") };
        return Err(fail(format!("{pr} is not a pull request in {names}"), 4));
    }
    Ok(pr)
}

/// Why the approver's escalation holds the job's PR from an agent merge, or None.
fn escalation_hold(home: &Path, slug: &str, job_id: &str) -> Res<Option<&'static str>> {
    let data = read_board(home)?;
    if find_job(&data, job_id).is_some_and(escalated) {
        return Ok(Some("is escalated on the board"));
    }
    let decision = home.join("jobs").join(slug).join(job_id).join("decision.md");
    let unreadable = Ok(Some("has a decision.md that cannot be read"));
    match is_link(&decision) {
        Ok(false) => {}
        _ => return unreadable,
    }
    let text = match read_text(&decision) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return unreadable,
    };
    let first = splitlines(&text, false).into_iter().map(strip).find(|l| !l.is_empty()).unwrap_or("");
    Ok(first.starts_with("ESCALATE").then_some("has a decision.md that says ESCALATE"))
}

fn stamp_of(v: Option<&Value>) -> Option<i64> {
    v?.as_str().and_then(parse_stamp)
}

/// internal when review-plan skipped ux and ui in the first round, visible otherwise, None with no rounds.
pub(crate) fn job_class(j: &Value) -> Option<&'static str> {
    let first = j.get("rounds")?.as_array()?.first()?;
    if !first.is_object() {
        return None;
    }
    let skipped = Some(&Value::from("SKIPPED"));
    Some(if first.get("ux") == skipped && first.get("ui") == skipped { "internal" } else { "visible" })
}

/// The board's well-formed `{"<slug>/<class>": "<UTC time>"}` restores, as times.
fn autonomy_restored(data: &Value) -> BTreeMap<String, i64> {
    let Some(restored) = data.get("autonomy").and_then(|r| r.get("restored")).and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    restored
        .iter()
        .filter_map(|(k, v)| {
            let (slug, class) = k.split_once('/')?;
            let ok = !class.contains('/') && SLUG.is_full(slug) && AUTONOMY_CLASSES.contains(&class);
            ok.then(|| stamp_of(Some(v)).map(|at| (k.clone(), at)))?
        })
        .collect()
}

/// `{(project, class): sorted reverted job ids}` for each class with a revert recorded after its last restore.
pub(crate) fn autonomy_holds(data: &Value) -> BTreeMap<(String, String), Vec<String>> {
    let cutoff = unix_now() - AUTONOMY_DAYS * 86_400;
    let restored = autonomy_restored(data);
    let mut holds: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for j in data["jobs"].as_array().into_iter().flatten() {
        let project = j.get("project").and_then(Value::as_str).unwrap_or("");
        let outcome = j.get("outcome").filter(|o| o.is_object());
        let (Some(cls), Some(outcome)) = (job_class(j), outcome) else { continue };
        if !(j.get("stage").and_then(Value::as_str) == Some("merged")
            && stamp_of(j.get("done")).unwrap_or(cutoff) > cutoff
            && !project.is_empty()
            && outcome.get("result").and_then(Value::as_str) == Some("reverted"))
        {
            continue;
        }
        let lifted = restored.get(&format!("{project}/{cls}"));
        let at = stamp_of(outcome.get("at"));
        if lifted.is_some_and(|lifted| !at.is_some_and(|at| at > *lifted)) {
            continue;
        }
        holds.entry((project.to_string(), cls.to_string())).or_default().push(py_str(j.get("id").unwrap_or(&Value::Null)));
    }
    holds.values_mut().for_each(|ids| ids.sort());
    holds
}

/// Why earned autonomy holds the job's project and class from an agent merge, or None.
pub(crate) fn autonomy_hold(data: &Value, job: &Value) -> Option<String> {
    let project = job.get("project")?.as_str()?;
    let cls = job_class(job)?;
    let holds = autonomy_holds(data);
    let held = holds.get(&(project.to_string(), cls.to_string()))?;
    Some(format!(
        "earned autonomy holds {project} {cls} PRs after reverted {}; a person lifts it with factory.py autonomy restore {project} {cls}",
        held.join(", ")
    ))
}

/// Set automerge false and needs on a held pr-open job. Returns the needs line, or None with no write.
pub(crate) fn autonomy_hold_write(home: &Path, job_id: &str) -> Res<Option<String>> {
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = find_job(&data, job_id) else { return Ok((None, None)) };
        let held = job.get("stage").and_then(Value::as_str) == Some("pr-open")
            && job.get("automerge") == Some(&Value::Bool(true))
            && !escalated(job);
        let why = if held { autonomy_hold(&data, job) } else { None };
        let number = job.get("pr").and_then(Value::as_str).and_then(|pr| GH_PR_PARTS.fullmatch(pr)).map(|m| m[3].to_string());
        let (Some(why), Some(number)) = (why, number) else { return Ok((None, None)) };
        let needs = format!("review and merge PR #{number}: {why}");
        patch_job(&mut data, job_id, &json!({"automerge": false, "needs": needs, "note": AUTONOMY_HOLD_NOTE}));
        Ok((Some(dump_board(&data)), Some(needs)))
    })
}

/// One check as gh lists it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Check {
    pub name: String,
    pub bucket: String,
    pub link: Option<String>,
}

/// Python's `<` between two `when` values, or None where Python raises TypeError.
fn when_cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Number(a), Value::Number(b)) => a.as_f64()?.partial_cmp(&b.as_f64()?),
        _ if a == b => Some(std::cmp::Ordering::Equal),
        _ => None,
    }
}

/// The rollup's checks, keeping per name a pending run over a finished one, then its latest. None when malformed.
fn rollup_checks(rollup: &Value) -> Option<Vec<Check>> {
    let nodes = rollup.get("contexts")?.as_object()?.get("nodes")?.as_array()?;
    let mut latest: BTreeMap<String, ((bool, Value), &'static str)> = BTreeMap::new();
    for node in nodes {
        let node = node.as_object()?;
        let (check, when, state, bucket) = match node.get("__typename")? {
            Value::String(t) if t == "CheckRun" => {
                let (check, when, state) = (node.get("name")?, node.get("startedAt")?, node.get("status")?);
                let bucket = if state == "COMPLETED" {
                    match node.get("conclusion")? {
                        Value::Array(_) | Value::Object(_) => return None,
                        Value::String(c) if c == "SUCCESS" => "pass",
                        Value::String(c) if c == "SKIPPED" || c == "NEUTRAL" => "skipping",
                        Value::String(c) if c == "CANCELLED" => "cancel",
                        _ => "fail",
                    }
                } else {
                    "pending"
                };
                (check, when, state, bucket)
            }
            Value::String(t) if t == "StatusContext" => {
                let (check, when, state) = (node.get("context")?, node.get("createdAt")?, node.get("state")?);
                let bucket = match state.as_str() {
                    Some("SUCCESS") => "pass",
                    Some("PENDING" | "EXPECTED") => "pending",
                    _ => "fail",
                };
                (check, when, state, bucket)
            }
            _ => return None,
        };
        let (Value::String(check), true) = (check, state.is_string()) else { return None };
        let rank = (bucket == "pending", if truthy(when) { when.clone() } else { Value::from("") });
        let newer = match latest.get(check) {
            None => true,
            Some(((pending, seen), _)) => match rank.0.cmp(pending) {
                std::cmp::Ordering::Equal => when_cmp(&rank.1, seen)? == std::cmp::Ordering::Greater,
                order => order == std::cmp::Ordering::Greater,
            },
        };
        if newer {
            latest.insert(check.clone(), (rank, bucket));
        }
    }
    Some(latest.into_iter().map(|(name, (_, bucket))| Check { name, bucket: bucket.into(), link: None }).collect())
}

/// (checks, problem) for commit sha's checks from one gh api graphql call.
fn head_checks(h: &mut dyn Host, pr: &str, sha: &str) -> (Vec<Check>, Option<String>) {
    let repo = GH_PR.fullmatch(pr).unwrap()[1].to_string();
    let (owner, name) = repo.split_once('/').unwrap();
    let (o, r, s) = (format!("o={owner}"), format!("r={name}"), format!("sha={sha}"));
    let args = ["api", "graphql", "-f", HEAD_CHECKS_QUERY, "-f", &o, "-f", &r, "-f", &s, "--jq", ".data.repository.object"];
    let Some(result) = h.gh(&args) else { return (vec![], Some("gh is not installed".into())) };
    match result.code {
        GhCode::Timeout => return (vec![], Some("timed out after 60 seconds".into())),
        GhCode::Exit(0) => {}
        GhCode::Exit(code) => return (vec![], Some(format!("exited {code} with no check list"))),
    }
    let unusable = || (vec![], Some("printed no check list".to_string()));
    let Ok(Value::Object(object)) = loads(&result.stdout) else { return unusable() };
    let rollup = match object.get("statusCheckRollup") {
        None => return unusable(),
        Some(Value::Null) => return (vec![], None),
        Some(rollup) => rollup,
    };
    let more = rollup.get("contexts").and_then(|c| c.get("pageInfo")).and_then(|p| p.get("hasNextPage"));
    match more {
        None => return unusable(),
        Some(more) if truthy(more) => return (vec![], Some("listed more than 100 checks".into())),
        Some(_) => {}
    }
    match rollup_checks(rollup) {
        Some(checks) => (checks, None),
        None => unusable(),
    }
}

/// How many commits of base the PR head sha lacks, from GitHub's compare API.
fn behind_by(h: &mut dyn Host, pr: &str, base: &str, sha: &Value) -> Res<u64> {
    let sha = sha.as_str().filter(|s| COMMIT.is_full(s)).ok_or_else(|| fail("gh pr view printed an unusable headRefOid", 5))?;
    let repo = &GH_PR.fullmatch(pr).unwrap()[1];
    let path = format!("repos/{repo}/compare/{base}...{sha}");
    let behind = gh_answer(h.gh(&["api", &path, "--jq", ".behind_by"]), "gh api compare")?;
    small_int(strip(&behind)).ok_or_else(|| fail("gh api compare printed no behind_by", 5))
}

/// (behind_by, head sha, base): how many commits of its base the PR's head lacks.
fn pr_behind(h: &mut dyn Host, pr: &str) -> Res<(u64, String, String)> {
    let view = loads(&gh_pr_view(h, pr, &["--json", "headRefOid,baseRefName"])?).ok();
    let field = |k: &str| view.as_ref().and_then(|v| v.get(k)).and_then(Value::as_str).map(str::to_string);
    let (sha, base) = (field("headRefOid"), field("baseRefName"));
    let (Some(sha), Some(base)) = (sha, base) else {
        return Err(fail("gh pr view printed an unusable headRefOid or baseRefName", 5));
    };
    if !(COMMIT.is_full(&sha) && BRANCH.is_full(&base)) {
        return Err(fail("gh pr view printed an unusable headRefOid or baseRefName", 5));
    }
    Ok((behind_by(h, pr, &base, &Value::from(sha.as_str()))?, sha, base))
}

/// The `while True` of checks: read the head's checks every 10 seconds until they settle or timeout passes.
fn checks_loop(h: &mut dyn Host, pr: &str, timeout: u64, out: &mut dyn Write, err: &mut dyn Write) -> Res<()> {
    let start = h.monotonic();
    let mut since = start;
    let mut usable = false;
    let mut updated_from: Option<String> = None;
    let timeout_f = timeout as f64;
    loop {
        let (behind, sha, base) = pr_behind(h, pr)?;
        let (checks, problem) = head_checks(h, pr, &sha);
        match &problem {
            Some(p) => {
                writeln!(err, "factory.py: warning: gh api graphql gave no usable check list ({p})").ok();
            }
            None => usable = true,
        }
        let pending: Vec<&str> = checks.iter().filter(|c| c.bucket == "pending").map(|c| c.name.as_str()).collect();
        let fresh = updated_from.as_deref() != Some(sha.as_str());
        if !checks.is_empty() && pending.is_empty() {
            let failing: Vec<&str> =
                checks.iter().filter(|c| c.bucket != "pass" && c.bucket != "skipping").map(|c| c.name.as_str()).collect();
            if !failing.is_empty() || (behind == 0 && fresh) {
                for c in &checks {
                    writeln!(out, "{} {}", c.bucket, c.name).ok();
                }
                if !failing.is_empty() {
                    return Err(fail(format!("failing checks: {}", failing.join(", ")), 1));
                }
                return Ok(());
            }
            if behind != 0 && fresh {
                let n = pr.rsplit('/').next().unwrap();
                let Some(result) = h.gh(&["pr", "update-branch", pr]) else { return Err(fail("gh is not installed", 5)) };
                match result.code {
                    GhCode::Timeout => return Err(fail("gh pr update-branch timed out after 60 seconds", 5)),
                    GhCode::Exit(0) => {}
                    GhCode::Exit(_) => {
                        let why = if result.stderr.is_empty() { &result.stdout } else { &result.stderr };
                        return Err(fail(
                            format!(
                                "gh pr update-branch could not bring PR #{n} up to date with {base}: {}. Merge {base} into the branch and push.",
                                printable_line(why, 200)
                            ),
                            6,
                        ));
                    }
                }
                let s = if behind == 1 { "" } else { "s" };
                writeln!(
                    err,
                    "factory.py: PR #{n} was {behind} commit{s} behind {base}; gh pr update-branch merged {base} into it, so checks run again on the new head."
                )
                .ok();
                updated_from = Some(sha);
                since = h.monotonic();
            }
        }
        let elapsed = h.monotonic() - start;
        let waited = h.monotonic() - since;
        if checks.is_empty() && !usable && waited >= CHECKS_REGISTER.min(timeout) as f64 {
            return Err(fail("gh api graphql never gave a usable check list", 5));
        }
        if checks.is_empty() && problem.is_none() && waited >= CHECKS_REGISTER as f64 {
            writeln!(out, "no checks reported").ok();
            return Ok(());
        }
        if elapsed >= timeout_f {
            if let Some(p) = problem {
                return Err(fail(format!("gh api graphql gave no usable check list by the deadline ({p})"), 5));
            }
            let why = if !pending.is_empty() {
                format!("still pending: {}", pending.join(", "))
            } else if updated_from.is_some() {
                "still waiting for checks on the updated head".into()
            } else {
                "no checks listed yet".into()
            };
            return Err(fail(why, 3));
        }
        h.sleep(CHECKS_INTERVAL.min((timeout_f - elapsed).max(0.0)));
    }
}

pub fn checks_cmd(call: &Call) -> Res<()> {
    let args = &call.args[1..];
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let timeout = match args {
        [] => 300,
        [flag, value] if flag == "--timeout" && small_int(value).is_some_and(|n| n <= 600) => small_int(value).unwrap(),
        _ => return Err(fail(format!("--timeout takes one value from 0 to 600 seconds. {CHECKS_USAGE}"), 2)),
    };
    let pr = board_pr(&home, &slug, &job_id, false)?;
    if let Some(why) = escalation_hold(&home, &slug, &job_id)? {
        return Err(fail(format!("{job_id} {why}, so a person reviews and merges {pr}. Nothing merges."), 7));
    }
    let data = read_board(&home)?;
    let empty = json!({});
    if let Some(why) = autonomy_hold(&data, find_job(&data, &job_id).unwrap_or(&empty)) {
        autonomy_hold_write(&home, &job_id)?;
        return Err(fail(format!("{job_id} is held: {why}. A person reviews and merges {pr}. Nothing merges."), 8));
    }
    let mut host = System::new();
    let state = gh_pr_view(&mut host, &pr, &["--json", "state", "--jq", ".state"])?;
    let state = strip(&state);
    if state != "OPEN" {
        return Err(fail(format!("{pr} is {}, not OPEN", if state.is_empty() { "in no state" } else { state }), 4));
    }
    checks_loop(&mut host, &pr, timeout, &mut io::stdout(), &mut io::stderr())
}

pub fn pr_state_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("pr-state"))
}

pub fn pr_queue_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("pr-queue"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PR: &str = "https://github.com/example/sample/pull/42";
    const WARN: &str = "factory.py: warning: gh api graphql gave no usable check list (exited 1 with no check list)\n";
    const NEVER: &str = "factory.py: gh api graphql never gave a usable check list\n";
    const DEADLINE: &str = "factory.py: gh api graphql gave no usable check list by the deadline (exited 1 with no check list)\n";
    const UPDATED: &str = "factory.py: PR #42 was 1 commit behind main; gh pr update-branch merged main into it, so checks run again on the new head.\n";

    fn old() -> String {
        "a1".repeat(20)
    }

    fn new() -> String {
        "b2".repeat(20)
    }

    fn reply(code: i32, stdout: &str, stderr: &str) -> Option<GhOut> {
        Some(GhOut { code: GhCode::Exit(code), stdout: stdout.into(), stderr: stderr.into() })
    }

    fn ok(stdout: &str) -> Option<GhOut> {
        reply(0, stdout, "")
    }

    fn timeout() -> Option<GhOut> {
        Some(GhOut { code: GhCode::Timeout, stdout: String::new(), stderr: String::new() })
    }

    fn miss() -> Option<GhOut> {
        reply(1, "", "")
    }

    /// gh answers from rules: the first rule whose argv is a prefix of the call gives its next reply, keeping the last.
    #[derive(Default)]
    struct Fake {
        rules: Vec<(Vec<String>, Vec<Option<GhOut>>)>,
        calls: Vec<Vec<String>>,
        now: f64,
        unix: i64,
    }

    impl Fake {
        fn on(mut self, prefix: &[&str], replies: Vec<Option<GhOut>>) -> Fake {
            self.rules.push((prefix.iter().map(|s| s.to_string()).collect(), replies));
            self
        }

        fn count(&self, word: &str) -> usize {
            self.calls.iter().filter(|c| c.iter().any(|a| a == word)).count()
        }
    }

    impl Host for Fake {
        fn gh(&mut self, args: &[&str]) -> Option<GhOut> {
            self.calls.push(args.iter().map(|s| s.to_string()).collect());
            let (_, replies) = self
                .rules
                .iter_mut()
                .find(|(prefix, _)| prefix.len() <= args.len() && prefix.iter().zip(args).all(|(p, a)| p == a))
                .unwrap_or_else(|| panic!("no rule for {args:?}"));
            if replies.len() > 1 {
                replies.remove(0)
            } else {
                replies[0].clone()
            }
        }

        fn monotonic(&mut self) -> f64 {
            self.now
        }

        fn sleep(&mut self, secs: f64) {
            self.now += secs;
        }

        fn unix_time(&mut self) -> i64 {
            self.unix
        }
    }

    fn run(name: &str, status: &str, conclusion: Value, started: Value) -> Value {
        json!({"__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion, "startedAt": started})
    }

    fn done(name: &str, conclusion: &str) -> Value {
        run(name, "COMPLETED", conclusion.into(), "2026-01-01T00:00:00Z".into())
    }

    fn status(context: &str, state: &str, created: &str) -> Value {
        json!({"__typename": "StatusContext", "context": context, "state": state, "createdAt": created})
    }

    fn rollup(nodes: Vec<Value>, more: bool) -> Option<GhOut> {
        ok(&json!({"statusCheckRollup": {"contexts": {"pageInfo": {"hasNextPage": more}, "nodes": nodes}}}).to_string())
    }

    /// head_checks' gh reply listing these (bucket, name) checks.
    fn listed(checks: &[(&str, &str)]) -> Option<GhOut> {
        let nodes = checks
            .iter()
            .map(|&(bucket, name)| match bucket {
                "pending" => run(name, "IN_PROGRESS", Value::Null, "2026-01-01T00:00:00Z".into()),
                "pass" => done(name, "SUCCESS"),
                "fail" => done(name, "FAILURE"),
                "cancel" => done(name, "CANCELLED"),
                _ => done(name, "SKIPPED"),
            })
            .collect();
        rollup(nodes, false)
    }

    fn behind_fake(behind: &[(u64, &str)], polls: Vec<Option<GhOut>>, update: Option<GhOut>) -> Fake {
        let views = behind.iter().map(|(_, sha)| ok(&json!({"headRefOid": sha, "baseRefName": "main"}).to_string())).collect();
        let mut fake = Fake::default().on(&["pr", "view", PR, "--json", "headRefOid,baseRefName"], views);
        for (n, sha) in behind {
            let path = format!("repos/example/sample/compare/main...{sha}");
            fake = fake.on(&["api", &path], vec![ok(&format!("{n}\n"))]);
        }
        fake.on(&["api", "graphql"], polls).on(&["pr", "update-branch", PR], vec![update])
    }

    /// (exit code, stdout, stderr) of checks_loop.
    fn run_loop(fake: &mut Fake, timeout: u64) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = match checks_loop(fake, PR, timeout, &mut out, &mut err) {
            Ok(()) => 0,
            Err(e) => {
                err.extend(e.stderr.as_bytes());
                e.code
            }
        };
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    fn checks(polls: Vec<Option<GhOut>>, timeout: u64, behind: &[(u64, &str)]) -> ((i32, String, String), Fake) {
        let mut fake = behind_fake(behind, polls, reply(0, "", ""));
        (run_loop(&mut fake, timeout), fake)
    }

    fn up_to_date() -> Vec<(u64, &'static str)> {
        vec![(0, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")]
    }

    fn got(code: i32, out: &str, err: &str) -> (i32, String, String) {
        (code, out.to_string(), err.to_string())
    }

    // ChecksTest.test_behind_pr_updates_and_waits
    #[test]
    fn loop_updates_then_waits() {
        let polls = vec![listed(&[("pass", "ci")]), listed(&[]), listed(&[("pending", "ci")]), listed(&[("pass", "ci")])];
        let (result, fake) = checks(polls, 540, &[(1, &old()), (0, &new())]);
        assert_eq!(result, got(0, "pass ci\n", UPDATED));
        assert_eq!(fake.count("update-branch"), 1);
        assert_eq!(fake.count("headRefOid,baseRefName"), 4);
        assert_eq!(fake.now, 30.0);
    }

    // ChecksTest.test_stale_head_after_update_is_pending
    #[test]
    fn stale_head_after_update_is_pending() {
        let (result, fake) = checks(vec![listed(&[("pass", "ci")])], 540, &[(1, &old()), (1, &old()), (0, &new())]);
        assert_eq!(result, got(0, "pass ci\n", UPDATED));
        assert_eq!(fake.count("update-branch"), 1);
        assert_eq!(fake.now, 20.0);
    }

    // ChecksTest.test_new_head_passing_exits_0_on_first_reading
    #[test]
    fn new_head_passes_on_first_reading() {
        let (result, fake) = checks(vec![listed(&[("pass", "ci")])], 540, &[(1, &old()), (0, &new())]);
        assert_eq!(result, got(0, "pass ci\n", UPDATED));
        assert_eq!(fake.count("headRefOid,baseRefName"), 2);
        assert_eq!(fake.now, 10.0);
    }

    // ChecksTest.test_checks_are_read_for_the_head_sha
    #[test]
    fn checks_read_for_head_sha() {
        let polls = vec![listed(&[("pass", "ci")]), listed(&[]), listed(&[("pass", "ci")])];
        let (_, fake) = checks(polls, 540, &[(1, &old()), (1, &old()), (0, &new())]);
        let shas: Vec<String> = fake.calls.iter().filter(|c| c[1] == "graphql").map(|c| c[9].clone()).collect();
        assert_eq!(shas, [format!("sha={}", old()), format!("sha={}", old()), format!("sha={}", new())]);
        let views = fake.calls.iter().filter(|c| c[0] == "pr" && c[1] == "view").count();
        assert_eq!(views, 3);
    }

    // ChecksTest.test_register_window_restarts_after_update
    #[test]
    fn register_window_restarts() {
        let mut polls = vec![listed(&[("pending", "ci")]); 10];
        polls.push(listed(&[("pass", "ci")]));
        polls.extend(vec![listed(&[]); 11]);
        polls.push(listed(&[("pass", "ci")]));
        let (old, new) = (old(), new());
        let mut behind = vec![(1, old.as_str()); 11];
        behind.push((0, &new));
        let (result, fake) = checks(polls, 540, &behind);
        assert_eq!(result, got(0, "pass ci\n", UPDATED));
        assert_eq!(fake.now, 220.0);
    }

    // ChecksTest.test_all_pass_prints_each_check_and_exits_0
    #[test]
    fn warn_then_pass() {
        let polls = vec![miss(), listed(&[("pending", "test"), ("pass", "lint")]), listed(&[("pass", "test"), ("pass", "lint")])];
        let (result, fake) = checks(polls, 300, &up_to_date());
        assert_eq!(result, got(0, "pass lint\npass test\n", WARN));
        assert_eq!(fake.calls[0][..2], ["pr", "view"]);
        assert_eq!(fake.now, 20.0);
    }

    // ChecksTest.test_no_checks_after_120_seconds_exits_0
    #[test]
    fn no_checks_after_120_seconds() {
        let (result, fake) = checks(vec![listed(&[])], 540, &up_to_date());
        assert_eq!(result, got(0, "no checks reported\n", ""));
        assert_eq!(fake.now, 120.0);
    }

    // ChecksTest.test_no_usable_reply_by_120_seconds_exits_5
    #[test]
    fn never_usable_by_120_seconds() {
        for (poll, problem) in [
            (miss(), "exited 1 with no check list"),
            (timeout(), "timed out after 60 seconds"),
            (ok("not json"), "printed no check list"),
            (rollup(vec![done("ci", "SUCCESS")], true), "listed more than 100 checks"),
            (None, "gh is not installed"),
        ] {
            let (result, fake) = checks(vec![poll], 540, &up_to_date());
            let warn = format!("factory.py: warning: gh api graphql gave no usable check list ({problem})\n");
            assert_eq!(result, (5, String::new(), warn.repeat(13) + NEVER), "{problem}");
            assert_eq!(fake.now, 120.0);
        }
    }

    // ChecksTest.test_unusable_replies_after_a_usable_one_exit_5_at_deadline
    #[test]
    fn unusable_after_usable_at_deadline() {
        let (result, fake) = checks(vec![miss(), listed(&[]), miss()], 540, &up_to_date());
        assert_eq!(result, (5, String::new(), WARN.repeat(54) + DEADLINE));
        assert_eq!(fake.now, 540.0);
        let (result, _) = checks(vec![listed(&[]), miss()], 30, &up_to_date());
        assert_eq!(result, (5, String::new(), WARN.repeat(3) + DEADLINE));
    }

    // ChecksTest.test_pending_then_unusable_replies_exit_5
    #[test]
    fn pending_then_unusable() {
        let (result, fake) = checks(vec![listed(&[("pending", "ci")]), miss()], 540, &up_to_date());
        assert_eq!(result, (5, String::new(), WARN.repeat(54) + DEADLINE));
        assert_eq!(fake.now, 540.0);
    }

    // ChecksTest.test_usable_empty_reply_after_misses_reports_none
    #[test]
    fn usable_empty_after_misses() {
        let mut polls = vec![listed(&[])];
        polls.extend(vec![miss(); 12]);
        polls.push(listed(&[]));
        let (result, fake) = checks(polls, 540, &up_to_date());
        assert_eq!(result, (0, "no checks reported\n".to_string(), WARN.repeat(12)));
        assert_eq!(fake.now, 130.0);
    }

    // ChecksTest.test_last_sleep_stops_at_the_deadline
    #[test]
    fn last_sleep_stops_at_deadline() {
        let (result, fake) = checks(vec![listed(&[("pending", "test")])], 25, &up_to_date());
        assert_eq!(result, got(3, "", "factory.py: still pending: test\n"));
        assert_eq!(fake.now, 25.0);
    }

    // ChecksTest.test_none_listed_at_deadline_before_120_seconds_exits_3
    #[test]
    fn none_listed_at_30_seconds() {
        let (result, fake) = checks(vec![listed(&[])], 30, &up_to_date());
        assert_eq!(result, got(3, "", "factory.py: no checks listed yet\n"));
        assert_eq!(fake.now, 30.0);
        let (result, _) = checks(vec![miss()], 30, &up_to_date());
        assert_eq!(result, (5, String::new(), WARN.repeat(4) + NEVER));
    }

    // ChecksTest.test_update_failure_exits_6, the timeout and missing gh subtests
    #[test]
    fn update_branch_timeout_or_missing_gh() {
        for (update, why) in [(timeout(), "gh pr update-branch timed out after 60 seconds"), (None, "gh is not installed")] {
            let mut fake = behind_fake(&[(2, &old())], vec![listed(&[("pass", "ci")])], update);
            assert_eq!(run_loop(&mut fake, 300), (5, String::new(), format!("factory.py: {why}\n")));
        }
    }

    // ChecksTest.test_gh_problem_exits_5, the timeout and missing gh subtests
    #[test]
    fn state_view_timeout_or_missing_gh() {
        for (view, why) in [(None, "gh is not installed"), (timeout(), "gh pr view timed out after 60 seconds")] {
            let mut fake = Fake::default().on(&["pr", "view"], vec![view]);
            let e = gh_pr_view(&mut fake, PR, &["--json", "state", "--jq", ".state"]).err().unwrap();
            assert_eq!((e.code, e.stderr), (5, format!("factory.py: {why}\n")));
            assert_eq!(fake.calls, [["pr", "view", PR, "--json", "state", "--jq", ".state"]]);
        }
    }

    fn head(reply: Option<GhOut>) -> ((Vec<(String, String)>, Option<String>), Fake) {
        let mut fake = Fake::default().on(&["api", "graphql"], vec![reply]);
        let (checks, problem) = head_checks(&mut fake, PR, &new());
        ((checks.into_iter().map(|c| (c.name, c.bucket)).collect(), problem), fake)
    }

    fn named(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(n, b)| (n.to_string(), b.to_string())).collect()
    }

    // HeadChecksTest.test_unusable_replies and test_query_names_the_sha
    #[test]
    fn head_checks_unusable() {
        let ((checks, problem), fake) = head(rollup(vec![done("ci", "SUCCESS")], false));
        assert_eq!((checks, problem), (named(&[("ci", "pass")]), None));
        let sha = format!("sha={}", new());
        let expected = ["api", "graphql", "-f", HEAD_CHECKS_QUERY, "-f", "o=example", "-f", "r=sample", "-f", &sha, "--jq", ".data.repository.object"];
        assert_eq!(fake.calls, [expected]);
        let nodes_dict = r#"{"statusCheckRollup": {"contexts": {"pageInfo": {"hasNextPage": false}, "nodes": {}}}}"#;
        for (reply, problem) in [
            (None, "gh is not installed"),
            (timeout(), "timed out after 60 seconds"),
            (miss(), "exited 1 with no check list"),
            (ok("not json"), "printed no check list"),
            (ok("null"), "printed no check list"),
            (ok("[]"), "printed no check list"),
            (ok(nodes_dict), "printed no check list"),
            (rollup(vec![done("ci", "SUCCESS")], true), "listed more than 100 checks"),
        ] {
            assert_eq!(head(reply).0, (vec![], Some(problem.to_string())), "{problem}");
        }
        assert_eq!(head(ok(r#"{"statusCheckRollup": null}"#)).0, (vec![], None));
    }

    // HeadChecksTest.test_malformed_list_counts_as_none_listed
    #[test]
    fn head_checks_malformed() {
        let mut named_7 = done("ci", "SUCCESS");
        named_7["name"] = 7.into();
        for node in [
            json!("pass test"),
            Value::Null,
            json!({"__typename": "CheckRun", "status": "COMPLETED", "startedAt": "x"}),
            named_7,
            json!({"__typename": "StatusContext", "context": "ci", "state": "SUCCESS"}),
            json!({"__typename": "Other", "name": "ci"}),
        ] {
            let reply = rollup(vec![done("lint", "SUCCESS"), node.clone()], false);
            assert_eq!(head(reply).0, (vec![], Some("printed no check list".to_string())), "{node}");
        }
    }

    // HeadChecksTest.test_pending_run_wins_over_earlier_pass
    #[test]
    fn head_checks_pending_wins() {
        let nodes = vec![
            run("ci", "COMPLETED", "SUCCESS".into(), "2026-01-01T00:00:05Z".into()),
            run("ci", "QUEUED", Value::Null, Value::Null),
            run("lint", "IN_PROGRESS", Value::Null, "2026-01-01T00:00:01Z".into()),
            run("lint", "COMPLETED", "FAILURE".into(), "2026-01-01T00:00:09Z".into()),
        ];
        assert_eq!(head(rollup(nodes, false)).0, (named(&[("ci", "pending"), ("lint", "pending")]), None));
        let nodes = vec![
            run("ci", "COMPLETED", "FAILURE".into(), "2026-01-01T00:00:10Z".into()),
            run("ci", "COMPLETED", "SUCCESS".into(), "2026-01-01T00:00:05Z".into()),
            status("lint", "FAILURE", "2026-01-01T00:00:01Z"),
            status("lint", "SUCCESS", "2026-01-01T00:00:02Z"),
        ];
        assert_eq!(head(rollup(nodes, false)).0, (named(&[("ci", "fail"), ("lint", "pass")]), None));
    }

    // PrBehindTest.test_bad_output_exits_5
    #[test]
    fn pr_behind_bad_outputs() {
        let view = |sha: &str, base: &str| ok(&json!({"headRefOid": sha, "baseRefName": base}).to_string());
        let good = view(&old(), "main");
        for (view, compare) in [
            (view(&"A1".repeat(20), "main"), ok("1\n")),
            (view(&old(), "-main"), ok("1\n")),
            (ok("not json"), ok("1\n")),
            (good.clone(), ok("null\n")),
            (good.clone(), ok("one\n")),
            (good.clone(), miss()),
            (None, ok("1\n")),
        ] {
            let mut fake = Fake::default().on(&["pr", "view"], vec![view]).on(&["api"], vec![compare]);
            assert_eq!(pr_behind(&mut fake, PR).err().unwrap().code, 5);
        }
        let mut fake = Fake::default().on(&["pr", "view"], vec![good]).on(&["api"], vec![ok("1\n")]);
        assert_eq!(pr_behind(&mut fake, PR).ok().unwrap(), (1, old(), "main".to_string()));
    }
}
