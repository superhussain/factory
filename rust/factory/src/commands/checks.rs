//! checks, pr-state and pr-queue: reading a job's PR through gh.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::cli::Call;
use crate::shared::board::{dump_board, escalated, load_board, patch_job, read_board};
use crate::shared::conf::{fm_get, frontmatter, read_conf};
use crate::shared::consts::SLUG;
use crate::shared::exit::{exit, fail, Res};
use crate::shared::fs::{is_link, read_text};
use crate::shared::http::{api_url, cas, platform_agent, urlopen_detail};
use crate::shared::proc::{capture, which};
use crate::shared::project::{project_files, r2_target, sha};
use crate::shared::py::{os_error, printable_line, splitlines, strip};
use crate::shared::pyjson::{dumps, loads, DEFAULT};
use crate::shared::pyre::{Flags, Pattern};
use crate::shared::pyval::{py_str, truthy};
use crate::shared::redact::redact_values;
use crate::shared::time::{parse_stamp, unix_now};

const CHECKS_USAGE: &str = "usage: checks <project-slug>/<job-id> [--timeout S]";
const CHECKS_INTERVAL: f64 = 10.0;
const CHECKS_REGISTER: u64 = 120;
const HEAD_CHECKS_QUERY: &str = "query=query($o:String!,$r:String!,$sha:GitObjectID!){repository(owner:$o,name:$r){object(oid:$sha){...on Commit{statusCheckRollup{contexts(first:100){pageInfo{hasNextPage} nodes{__typename ...on CheckRun{name status conclusion startedAt} ...on StatusContext{context state createdAt}}}}}}}}";
const PR_STATE_USAGE: &str = "usage: pr-state <project-slug>/<job-id> [--wait S]";
const PR_STATE_FIELDS: &str =
    "state,reviewDecision,latestReviews,mergeStateStatus,autoMergeRequest,commits,author,statusCheckRollup,baseRefName,headRefOid";
const PR_REVIEWERS: &[&str] = &["OWNER", "MEMBER", "COLLABORATOR"];
// gh pr view --json has neither isInMergeQueue nor review threads, so both come from GraphQL.
const PR_GRAPHQL_QUERY: &str = "query=query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r){pullRequest(number:$n){isInMergeQueue isMergeQueueEnabled mergeQueueEntry{state} reviewThreads(last:100){nodes{isResolved path comments(first:100){nodes{authorAssociation body author{__typename login}}}}} latestReviews(first:100){nodes{author{__typename login}}}}}}";
const PR_STATE_INTERVAL: f64 = 30.0;
const HANDLED_CAP: u64 = 3;
const BUILDKITE_API: &str = "https://api.buildkite.com/v2";
const BUILDKITE_TAIL_LINES: usize = 80;
const BUILDKITE_TAIL_CHARS: usize = 8000;
const BUILDKITE_MAX_BYTES: usize = 16 * 1024 * 1024;
const ENV_SECRET_MIN: usize = 12;
const PR_ENQUEUE_MUTATION: &str = "query=mutation($id:ID!,$sha:GitObjectID!){enqueuePullRequest(input:{pullRequestId:$id,expectedHeadOid:$sha}){mergeQueueEntry{state position}}}";
// factory.py keeps it beside the others; the arity gate prints the usage.
#[allow(dead_code)]
const PR_QUEUE_USAGE: &str = "usage: pr-queue <project-slug>/<job-id>";
const PR_QUEUE_INTERVAL: f64 = 10.0;
const PR_QUEUE_TIMEOUT: f64 = 120.0;
const PR_MERGE_QUERY: &str = "query=query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r){pullRequest(number:$n){state isDraft mergeable mergeStateStatus headRefOid baseRefName isMergeQueueEnabled}}}";
const PR_MERGE_INTERVAL: f64 = 5.0;
const PR_MERGE_SETTLE: f64 = 30.0;
const AUTONOMY_DAYS: i64 = 90;
const AUTONOMY_CLASSES: &[&str] = &["internal", "visible"];
const AUTONOMY_HOLD_NOTE: &str = "earned autonomy hold: a person reviews and merges";

static GH_PR: Pattern = Pattern::new(
    r"https://github\.com/([\w.-]+/[\w.-]+)/pull/\d+",
    Flags::ASCII,
);
static GH_PR_PARTS: Pattern = Pattern::new(
    r"https://github\.com/([\w.-]+)/([\w.-]+)/pull/(\d{1,9})",
    Flags::ASCII,
);
static BRANCH: Pattern = Pattern::new(r"(?!-)[A-Za-z0-9._/-]{1,255}", Flags::NONE);
static COMMIT: Pattern = Pattern::new(r"[0-9a-f]{40}", Flags::NONE);
static NO_CHECKS: Pattern = Pattern::new(r"no (required )?checks reported", Flags::NONE);
static HANDLED_SIG: Pattern =
    Pattern::new(r"(review|thread|check|conflict):[0-9a-f]{16}", Flags::NONE);
static BUILDKITE_BUILD: Pattern = Pattern::new(
    r"https://buildkite\.com/([A-Za-z0-9-]{1,100})/([A-Za-z0-9_-]{1,100})/builds/([0-9]{1,12})(?:#([0-9a-f-]{36}))?",
    Flags::ASCII,
);
static BUILDKITE_JOB: Pattern = Pattern::new(r"[0-9a-f-]{36}", Flags::NONE);
static BUILDKITE_STRIP: Pattern =
    Pattern::new(r"\x1b_bk;t=[0-9]+\x07|\x1b\[[0-9;?]*[A-Za-z]", Flags::NONE);
static MERGE_ASYNC_ID: Pattern = Pattern::new(
    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
    Flags::NONE,
);

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
        System {
            start: Instant::now(),
        }
    }
}

impl Host for System {
    fn gh(&mut self, args: &[&str]) -> Option<GhOut> {
        let gh = which("gh")?;
        let gh = gh.to_string_lossy();
        let argv: Vec<&str> = std::iter::once(gh.as_ref())
            .chain(args.iter().copied())
            .collect();
        let empty = |code| GhOut {
            code,
            stdout: String::new(),
            stderr: String::new(),
        };
        Some(
            match capture(&argv, None, &[], Some(Duration::from_secs(60))) {
                Ok(o) => GhOut {
                    code: GhCode::Exit(o.code),
                    stdout: o.stdout,
                    stderr: o.stderr,
                },
                Err(e) if e.kind() == io::ErrorKind::TimedOut => empty(GhCode::Timeout),
                Err(_) => empty(GhCode::Exit(1)),
            },
        )
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

/// `str(v or "")`.
fn str_or_empty(v: Option<&Value>) -> String {
    v.filter(|v| truthy(*v)).map(py_str).unwrap_or_default()
}

/// `int(s)` for text that fully matches `[0-9]+`, else None. Past 18 digits it saturates.
fn small_int(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits = s.trim_start_matches('0');
    Some(if digits.len() > 18 {
        u64::MAX
    } else {
        digits.parse().unwrap_or(0)
    })
}

/// stdout of a gh answer, failing with exit 5 when gh cannot answer.
fn gh_answer(r: Option<GhOut>, what: &str) -> Res<String> {
    let Some(r) = r else {
        return Err(fail("gh is not installed", 5));
    };
    match r.code {
        GhCode::Timeout => Err(fail(format!("{what} timed out after 60 seconds"), 5)),
        GhCode::Exit(0) => Ok(r.stdout),
        GhCode::Exit(code) => Err(fail(
            format!("{what} exited {code} (check gh auth status)"),
            5,
        )),
    }
}

/// stdout of gh pr view <pr> <args>, failing with exit 5 when gh cannot answer.
fn gh_pr_view(h: &mut dyn Host, pr: &str, args: &[&str]) -> Res<String> {
    let argv: Vec<&str> = ["pr", "view", pr]
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    gh_answer(h.gh(&argv), "gh pr view")
}

fn find_job<'a>(data: &'a Value, job_id: &str) -> Option<&'a Value> {
    data["jobs"]
        .as_array()?
        .iter()
        .find(|j| j.get("id").and_then(Value::as_str) == Some(job_id))
}

fn project_meta(home: &Path, slug: &str) -> Res<Vec<(String, String)>> {
    let path = project_files(home, slug)?.0;
    Ok(frontmatter(&path)
        .map_err(|e| fail(os_error(&e, Some(&path)), 1))?
        .0)
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
    let archive_repo = if archive {
        fm_get(&meta, "archive").unwrap_or("")
    } else {
        ""
    };
    let repos: Vec<&str> = [fm_get(&meta, "repo").unwrap_or(""), archive_repo]
        .into_iter()
        .filter(|r| !r.is_empty())
        .collect();
    let found = GH_PR.fullmatch(&pr).is_some_and(|m| {
        let repo = m.get(1).unwrap().as_str().to_lowercase();
        repos.iter().any(|r| r.to_lowercase() == repo)
    });
    if !found {
        let names = if repos.is_empty() {
            "the project file repo".to_string()
        } else {
            repos.join(" or ")
        };
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
    let decision = home
        .join("jobs")
        .join(slug)
        .join(job_id)
        .join("decision.md");
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
    let first = splitlines(&text, false)
        .into_iter()
        .map(strip)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    Ok(first
        .starts_with("ESCALATE")
        .then_some("has a decision.md that says ESCALATE"))
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
    Some(
        if first.get("ux") == skipped && first.get("ui") == skipped {
            "internal"
        } else {
            "visible"
        },
    )
}

/// The board's well-formed `{"<slug>/<class>": "<UTC time>"}` restores, as times.
fn autonomy_restored(data: &Value) -> BTreeMap<String, i64> {
    let Some(restored) = data
        .get("autonomy")
        .and_then(|r| r.get("restored"))
        .and_then(Value::as_object)
    else {
        return BTreeMap::new();
    };
    restored
        .iter()
        .filter_map(|(k, v)| {
            let (slug, class) = k.split_once('/')?;
            let ok =
                !class.contains('/') && SLUG.is_full(slug) && AUTONOMY_CLASSES.contains(&class);
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
        let (Some(cls), Some(outcome)) = (job_class(j), outcome) else {
            continue;
        };
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
        holds
            .entry((project.to_string(), cls.to_string()))
            .or_default()
            .push(py_str(j.get("id").unwrap_or(&Value::Null)));
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
        let Some(job) = find_job(&data, job_id) else {
            return Ok((None, None));
        };
        let held = job.get("stage").and_then(Value::as_str) == Some("pr-open")
            && job.get("automerge") == Some(&Value::Bool(true))
            && !escalated(job);
        let why = if held {
            autonomy_hold(&data, job)
        } else {
            None
        };
        let number = job
            .get("pr")
            .and_then(Value::as_str)
            .and_then(|pr| GH_PR_PARTS.fullmatch(pr))
            .map(|m| m[3].to_string());
        let (Some(why), Some(number)) = (why, number) else {
            return Ok((None, None));
        };
        let needs = format!("review and merge PR #{number}: {why}");
        patch_job(
            &mut data,
            job_id,
            &json!({"automerge": false, "needs": needs, "note": AUTONOMY_HOLD_NOTE}),
        );
        Ok((Some(dump_board(&data)), Some(needs)))
    })
}

/// Record the head checks passed as the job's checked_head, with no write when the row is missing or holds it.
fn checked_head_write(home: &Path, job_id: &str, sha: &str) -> Res<()> {
    cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        match find_job(&data, job_id) {
            Some(job) if job.get("checked_head").and_then(Value::as_str) != Some(sha) => {}
            _ => return Ok((None, ())),
        }
        patch_job(&mut data, job_id, &json!({"checked_head": sha}));
        Ok((Some(dump_board(&data)), ()))
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
    let nodes = rollup
        .get("contexts")?
        .as_object()?
        .get("nodes")?
        .as_array()?;
    let mut latest: BTreeMap<String, ((bool, Value), &'static str)> = BTreeMap::new();
    for node in nodes {
        let node = node.as_object()?;
        let (check, when, state, bucket) = match node.get("__typename")? {
            Value::String(t) if t == "CheckRun" => {
                let (check, when, state) = (
                    node.get("name")?,
                    node.get("startedAt")?,
                    node.get("status")?,
                );
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
                let (check, when, state) = (
                    node.get("context")?,
                    node.get("createdAt")?,
                    node.get("state")?,
                );
                let bucket = match state.as_str() {
                    Some("SUCCESS") => "pass",
                    Some("PENDING" | "EXPECTED") => "pending",
                    _ => "fail",
                };
                (check, when, state, bucket)
            }
            _ => return None,
        };
        let (Value::String(check), true) = (check, state.is_string()) else {
            return None;
        };
        let rank = (
            bucket == "pending",
            if truthy(when) {
                when.clone()
            } else {
                Value::from("")
            },
        );
        let newer = match latest.get(check) {
            None => true,
            Some(((pending, seen), _)) => match rank.0.cmp(pending) {
                std::cmp::Ordering::Equal => {
                    when_cmp(&rank.1, seen)? == std::cmp::Ordering::Greater
                }
                order => order == std::cmp::Ordering::Greater,
            },
        };
        if newer {
            latest.insert(check.clone(), (rank, bucket));
        }
    }
    Some(
        latest
            .into_iter()
            .map(|(name, (_, bucket))| Check {
                name,
                bucket: bucket.into(),
                link: None,
            })
            .collect(),
    )
}

/// (checks, problem) for commit sha's checks from one gh api graphql call.
fn head_checks(h: &mut dyn Host, pr: &str, sha: &str) -> (Vec<Check>, Option<String>) {
    let repo = GH_PR.fullmatch(pr).unwrap()[1].to_string();
    let (owner, name) = repo.split_once('/').unwrap();
    let (o, r, s) = (
        format!("o={owner}"),
        format!("r={name}"),
        format!("sha={sha}"),
    );
    let args = [
        "api",
        "graphql",
        "-f",
        HEAD_CHECKS_QUERY,
        "-f",
        &o,
        "-f",
        &r,
        "-f",
        &s,
        "--jq",
        ".data.repository.object",
    ];
    let Some(result) = h.gh(&args) else {
        return (vec![], Some("gh is not installed".into()));
    };
    match result.code {
        GhCode::Timeout => return (vec![], Some("timed out after 60 seconds".into())),
        GhCode::Exit(0) => {}
        GhCode::Exit(code) => return (vec![], Some(format!("exited {code} with no check list"))),
    }
    let unusable = || (vec![], Some("printed no check list".to_string()));
    let Ok(Value::Object(object)) = loads(&result.stdout) else {
        return unusable();
    };
    let rollup = match object.get("statusCheckRollup") {
        None => return unusable(),
        Some(Value::Null) => return (vec![], None),
        Some(rollup) => rollup,
    };
    let more = rollup
        .get("contexts")
        .and_then(|c| c.get("pageInfo"))
        .and_then(|p| p.get("hasNextPage"));
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
    let sha = sha
        .as_str()
        .filter(|s| COMMIT.is_full(s))
        .ok_or_else(|| fail("gh pr view printed an unusable headRefOid", 5))?;
    let repo = &GH_PR.fullmatch(pr).unwrap()[1];
    let path = format!("repos/{repo}/compare/{base}...{sha}");
    let behind = gh_answer(
        h.gh(&["api", &path, "--jq", ".behind_by"]),
        "gh api compare",
    )?;
    small_int(strip(&behind)).ok_or_else(|| fail("gh api compare printed no behind_by", 5))
}

/// (behind_by, head sha, base): how many commits of its base the PR's head lacks.
fn pr_behind(h: &mut dyn Host, pr: &str) -> Res<(u64, String, String)> {
    let view = loads(&gh_pr_view(h, pr, &["--json", "headRefOid,baseRefName"])?).ok();
    let field = |k: &str| {
        view.as_ref()
            .and_then(|v| v.get(k))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let (sha, base) = (field("headRefOid"), field("baseRefName"));
    let (Some(sha), Some(base)) = (sha, base) else {
        return Err(fail(
            "gh pr view printed an unusable headRefOid or baseRefName",
            5,
        ));
    };
    if !(COMMIT.is_full(&sha) && BRANCH.is_full(&base)) {
        return Err(fail(
            "gh pr view printed an unusable headRefOid or baseRefName",
            5,
        ));
    }
    Ok((
        behind_by(h, pr, &base, &Value::from(sha.as_str()))?,
        sha,
        base,
    ))
}

/// The `while True` of checks: read the head's checks every 10 seconds until they settle or timeout passes.
fn checks_loop(
    h: &mut dyn Host,
    pr: &str,
    timeout: u64,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Res<String> {
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
                writeln!(
                    err,
                    "factory.py: warning: gh api graphql gave no usable check list ({p})"
                )
                .ok();
            }
            None => usable = true,
        }
        let pending: Vec<&str> = checks
            .iter()
            .filter(|c| c.bucket == "pending")
            .map(|c| c.name.as_str())
            .collect();
        let fresh = updated_from.as_deref() != Some(sha.as_str());
        if !checks.is_empty() && pending.is_empty() {
            let failing: Vec<&str> = checks
                .iter()
                .filter(|c| c.bucket != "pass" && c.bucket != "skipping")
                .map(|c| c.name.as_str())
                .collect();
            if !failing.is_empty() || (behind == 0 && fresh) {
                for c in &checks {
                    writeln!(out, "{} {}", c.bucket, c.name).ok();
                }
                if !failing.is_empty() {
                    return Err(fail(format!("failing checks: {}", failing.join(", ")), 1));
                }
                return Ok(sha);
            }
            if behind != 0 && fresh {
                let n = pr.rsplit('/').next().unwrap();
                let Some(result) = h.gh(&["pr", "update-branch", pr]) else {
                    return Err(fail("gh is not installed", 5));
                };
                match result.code {
                    GhCode::Timeout => {
                        return Err(fail("gh pr update-branch timed out after 60 seconds", 5))
                    }
                    GhCode::Exit(0) => {}
                    GhCode::Exit(_) => {
                        let why = if result.stderr.is_empty() {
                            &result.stdout
                        } else {
                            &result.stderr
                        };
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
                updated_from = Some(sha.clone());
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
            return Ok(sha);
        }
        if elapsed >= timeout_f {
            if let Some(p) = problem {
                return Err(fail(
                    format!("gh api graphql gave no usable check list by the deadline ({p})"),
                    5,
                ));
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

/// Exit 7 when the job is escalated, or 8 under an earned-autonomy hold, before any gh call.
fn merge_hold(home: &Path, slug: &str, job_id: &str, pr: &str) -> Res<()> {
    if let Some(why) = escalation_hold(home, slug, job_id)? {
        return Err(fail(
            format!("{job_id} {why}, so a person reviews and merges {pr}. Nothing merges."),
            7,
        ));
    }
    let data = read_board(home)?;
    let empty = json!({});
    if let Some(why) = autonomy_hold(&data, find_job(&data, job_id).unwrap_or(&empty)) {
        autonomy_hold_write(home, job_id)?;
        return Err(fail(
            format!("{job_id} is held: {why}. A person reviews and merges {pr}. Nothing merges."),
            8,
        ));
    }
    Ok(())
}

pub fn checks_cmd(call: &Call) -> Res<()> {
    let args = &call.args[1..];
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let timeout = match args {
        [] => 300,
        [flag, value] if flag == "--timeout" && small_int(value).is_some_and(|n| n <= 600) => {
            small_int(value).unwrap()
        }
        _ => {
            return Err(fail(
                format!("--timeout takes one value from 0 to 600 seconds. {CHECKS_USAGE}"),
                2,
            ))
        }
    };
    let pr = board_pr(&home, &slug, &job_id, false)?;
    merge_hold(&home, &slug, &job_id, &pr)?;
    let mut host = System::new();
    let state = gh_pr_view(&mut host, &pr, &["--json", "state", "--jq", ".state"])?;
    let state = strip(&state);
    if state != "OPEN" {
        return Err(fail(
            format!(
                "{pr} is {}, not OPEN",
                if state.is_empty() {
                    "in no state"
                } else {
                    state
                }
            ),
            4,
        ));
    }
    let sha = checks_loop(
        &mut host,
        &pr,
        timeout,
        &mut io::stdout(),
        &mut io::stderr(),
    )?;
    checked_head_write(&home, &job_id, &sha)
}

/// The branch a job's PR should target: the project base, or the archive repo's default branch for an archive PR.
pub(crate) fn pr_base(h: &mut dyn Host, meta: &[(String, String)], pr: &str) -> Res<String> {
    let repo = GH_PR.fullmatch(pr).unwrap()[1].to_string();
    let archive = fm_get(meta, "archive").unwrap_or("");
    let lower = repo.to_lowercase();
    if archive.is_empty()
        || lower != archive.to_lowercase()
        || lower == fm_get(meta, "repo").unwrap_or("").to_lowercase()
    {
        return Ok(fm_get(meta, "base")
            .filter(|b| !b.is_empty())
            .unwrap_or("main")
            .to_string());
    }
    let path = format!("repos/{repo}");
    let answer = gh_answer(
        h.gh(&["api", &path, "--jq", ".default_branch"]),
        "gh api repos",
    )?;
    let base = strip(&answer);
    if !BRANCH.is_full(base) {
        return Err(fail("gh printed an unusable default_branch", 5));
    }
    Ok(base.to_string())
}

/// (checks, problem): the PR's checks as gh pr checks lists them and None, or [] and why the reply was
/// unusable. gh's own no-checks reply is usable, as []. With required, only the checks that gate the merge.
fn pr_checks(h: &mut dyn Host, pr: &str, required: bool) -> (Vec<Check>, Option<String>) {
    let mut args = vec!["pr", "checks", pr];
    if required {
        args.push("--required");
    }
    args.extend(["--json", "name,bucket,link"]);
    let Some(result) = h.gh(&args) else {
        return (vec![], Some("gh is not installed".into()));
    };
    let code = match result.code {
        GhCode::Timeout => return (vec![], Some("timed out after 60 seconds".into())),
        GhCode::Exit(code) => code,
    };
    if let Ok(Value::Array(items)) = loads(&result.stdout) {
        let checks: Option<Vec<Check>> = items
            .iter()
            .map(|c| {
                Some(Check {
                    name: c.get("name")?.as_str()?.into(),
                    bucket: c.get("bucket")?.as_str()?.into(),
                    link: c.get("link").and_then(Value::as_str).map(String::from),
                })
            })
            .collect();
        if let Some(checks) = checks {
            return (checks, None);
        }
    }
    if code == 1 && strip(&result.stdout).is_empty() && NO_CHECKS.search(&result.stderr).is_some() {
        return (vec![], None);
    }
    (
        vec![],
        Some(if code != 0 {
            format!("exited {code} with no check list")
        } else {
            "printed no check list".into()
        }),
    )
}

/// The signature babysit records for one PR item: its kind and a hash of what identifies it.
pub(crate) fn handled_sig(kind: &str, parts: &[&str]) -> String {
    format!("{kind}:{}", &sha(parts.join("\n").as_bytes())[..16])
}

/// The well-formed entries of a job's handled field, in board order.
pub(crate) fn valid_handled(v: Option<&Value>) -> Map<String, Value> {
    let Some(Value::Object(entries)) = v else {
        return Map::new();
    };
    entries
        .iter()
        .filter_map(|(sig, e)| {
            let (Value::Number(count), at) = (e.get("count")?, e.get("at")?) else {
                return None;
            };
            let digits = count.to_string();
            let positive_int = !digits.contains(['.', 'e', 'E', '-'])
                && !digits.trim_start_matches('0').is_empty();
            (HANDLED_SIG.is_full(sig) && positive_int && at.is_string())
                .then(|| (sig.clone(), json!({"count": count, "at": at})))
        })
        .collect()
}

/// The job's valid handled entries from the board, else {}.
pub(crate) fn job_handled(home: &Path, job_id: &str) -> Res<Map<String, Value>> {
    let data = read_board(home)?;
    Ok(valid_handled(
        find_job(&data, job_id).and_then(|j| j.get("handled")),
    ))
}

/// The body of one Buildkite GET, at most max + 1 bytes, or the problem line. The token goes only into its header.
fn buildkite_get(url: &str, token: &str, max: usize) -> Result<Vec<u8>, String> {
    let agent = platform_agent("https://api.buildkite.com");
    let unreachable = |text: &str| buildkite_unreachable(text, token);
    let response = agent
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("User-Agent", "factory")
        .call()
        .map_err(|e| unreachable(&urlopen_detail(&e)))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let scopes = if status == 401 || status == 403 {
            " (the token needs read_builds and read_build_logs)"
        } else {
            ""
        };
        return Err(format!("Buildkite answered {status}{scopes}"));
    }
    let mut body = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(max as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| unreachable(&e.to_string()))?;
    Ok(body)
}

/// The Buildkite problem line for an error's full text. The token goes before truncation, so no part of it shows.
fn buildkite_unreachable(text: &str, token: &str) -> String {
    format!(
        "can't reach Buildkite: {}",
        printable_line(&text.replace(token, "[token]"), 200)
    )
}

/// ci_log for a failing check that links to a Buildkite build: the failed job's name and the stripped,
/// redacted tail of its log, or a problem saying why there is none. Reads only the project's buildkite_org.
fn buildkite_log(home: &Path, check: &Check, org: &str, api: &str, max: usize) -> Res<Value> {
    let caps = BUILDKITE_BUILD
        .fullmatch(check.link.as_deref().unwrap_or(""))
        .unwrap();
    let (link_org, pipeline, number) = (&caps[1], &caps[2], &caps[3]);
    let job_id = caps.get(4).map(|m| m.as_str());
    let reading = |job: &str, tail: &str, problem: &str| {
        Ok(json!({"check": check.name, "job": job, "tail": tail, "problem": problem}))
    };
    if org.is_empty() {
        return reading(
            "",
            "",
            "set buildkite_org in the project frontmatter to read Buildkite logs",
        );
    }
    if link_org != org {
        let why = format!(
            "the check links to Buildkite org {link_org}, not the project's buildkite_org {}",
            printable_line(org, 100)
        );
        return reading("", "", &why);
    }
    let env_file = home.join(".env");
    let env = if env_file.exists() {
        read_conf(&env_file).map_err(|e| fail(os_error(&e, Some(&env_file)), 1))?
    } else {
        Default::default()
    };
    let token = env.get("BUILDKITE_API_TOKEN").map_or("", String::as_str);
    if token.is_empty() {
        return reading(
            "",
            "",
            "set BUILDKITE_API_TOKEN in the factory .env to read Buildkite logs",
        );
    }
    let get = |path: &str| -> Result<Map<String, Value>, String> {
        let body = buildkite_get(
            &format!("{api}/organizations/{link_org}/pipelines/{pipeline}/builds/{number}{path}"),
            token,
            max,
        )?;
        let data = if body.len() <= max {
            std::str::from_utf8(&body).ok().and_then(|t| loads(t).ok())
        } else {
            None
        };
        match data {
            Some(Value::Object(data)) => Ok(data),
            _ => Err("Buildkite sent no usable log".into()),
        }
    };
    let build = match get("") {
        Ok(build) => build,
        Err(problem) => return reading("", "", &problem),
    };
    let job = build
        .get("jobs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|j| {
            j.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| BUILDKITE_JOB.is_full(id))
        })
        .find(|j| match job_id {
            Some(id) => j["id"] == id,
            None => {
                j.get("type").and_then(Value::as_str) == Some("script")
                    && matches!(
                        j.get("state").and_then(Value::as_str),
                        Some("failed" | "timed_out")
                    )
            }
        });
    let Some(job) = job else {
        return reading("", "", &format!("no failed job in build {number}"));
    };
    let log = get(&format!("/jobs/{}/log", job["id"].as_str().unwrap()));
    let content = match log.as_ref().map(|l| l.get("content")) {
        Ok(Some(Value::String(content))) => content,
        Ok(_) => return reading("", "", "Buildkite sent no usable log"),
        Err(problem) => return reading("", "", problem),
    };
    let text = BUILDKITE_STRIP
        .replace_all(content, |_| String::new())
        .replace("\r\n", "\n");
    let text = redact_values(
        &text,
        env.values().filter(|v| v.chars().count() >= ENV_SECRET_MIN),
    );
    let lines: Vec<&str> = text
        .trim_end_matches('\n')
        .split('\n')
        .map(|l| l.rsplit('\r').next().unwrap())
        .collect();
    let tail = lines[lines.len().saturating_sub(BUILDKITE_TAIL_LINES)..].join("\n");
    let chars = tail.chars().count();
    let tail: String = tail
        .chars()
        .skip(chars.saturating_sub(BUILDKITE_TAIL_CHARS))
        .collect();
    reading(&str_or_empty(job.get("name")), &tail, "")
}

/// `int(digits)` as a JSON number of any size.
fn py_int(digits: &str) -> Value {
    let digits = digits.trim_start_matches('0');
    loads(if digits.is_empty() { "0" } else { digits }).unwrap()
}

/// `str((item.get("author") or {}).get("login") or "")`.
fn login(item: &Value) -> String {
    str_or_empty(
        item.get("author")
            .filter(|a| truthy(*a))
            .and_then(|a| a.get("login")),
    )
}

/// `(v or {}).get("nodes") or []`, as a list.
fn nodes(v: Option<&Value>) -> &[Value] {
    v.filter(|v| truthy(*v))
        .and_then(|v| v.get("nodes"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Add one item to items or capped. False when it is a review or thread babysit already handled.
fn note(
    handled: &Map<String, Value>,
    items: &mut Vec<Value>,
    capped: &mut Vec<Value>,
    kind: &str,
    label: &str,
    parts: &[&str],
) -> bool {
    let sig = handled_sig(kind, parts);
    let seen = handled.get(&sig);
    if (kind == "review" || kind == "thread") && seen.is_some() {
        return false;
    }
    let count = seen.map_or(Value::from(0), |e| e["count"].clone());
    let at_cap = count.as_f64().is_some_and(|n| n >= HANDLED_CAP as f64);
    let entry = json!({"sig": sig, "label": printable_line(label, 100), "count": count});
    let list = if at_cap { capped } else { items };
    if !list.contains(&entry) {
        list.push(entry);
    }
    true
}

/// One reading of the PR for a babysit session, with the next step it calls for. A handled review or thread
/// is skipped, and a check or conflict worked HANDLED_CAP times reads as capped.
pub(crate) fn pr_reading(
    h: &mut dyn Host,
    home: &Path,
    pr: &str,
    base: &str,
    org: &str,
    handled: &Map<String, Value>,
) -> Res<Value> {
    let view = loads(&gh_pr_view(h, pr, &["--json", PR_STATE_FIELDS])?)
        .ok()
        .filter(Value::is_object);
    let Some(view) = view else {
        return Err(fail("gh pr view printed no JSON object", 5));
    };
    let target = view
        .get("baseRefName")
        .filter(|t| truthy(*t))
        .cloned()
        .unwrap_or_else(|| Value::from(base));
    let Some(target) = target
        .as_str()
        .filter(|t| BRANCH.is_full(t))
        .map(String::from)
    else {
        return Err(fail("gh printed an unusable baseRefName", 5));
    };
    let (listed, problem) = pr_checks(h, pr, true);
    if problem.is_some() && view.get("statusCheckRollup").is_some_and(truthy) {
        return Err(fail(
            "gh pr checks listed none of the PR's checks (check gh auth status)",
            5,
        ));
    }
    let buckets: BTreeSet<&str> = listed.iter().map(|c| c.bucket.as_str()).collect();
    let checks = if buckets.contains("fail") || buckets.contains("cancel") {
        "fail"
    } else if buckets.contains("pending") {
        "pending"
    } else if !buckets.is_empty() {
        "pass"
    } else {
        "none"
    };
    let head = match view.get("commits").filter(|c| truthy(*c)) {
        None | Some(Value::String(_)) => String::new(),
        Some(Value::Array(commits)) => match commits.last() {
            Some(last @ Value::Object(_)) => str_or_empty(last.get("committedDate")),
            _ => String::new(),
        },
        Some(_) => return Err(fail("gh pr view printed commits that are not a list", 1)),
    };
    let new = parse_stamp(&head).is_some_and(|at| h.unix_time() - at < CHECKS_REGISTER as i64);

    let parts: Vec<&str> = pr.split('/').collect();
    let (o, r, n) = (
        format!("o={}", parts[3]),
        format!("r={}", parts[4]),
        format!("n={}", parts[6]),
    );
    let args = [
        "api",
        "graphql",
        "-f",
        PR_GRAPHQL_QUERY,
        "-f",
        &o,
        "-f",
        &r,
        "-F",
        &n,
        "--jq",
        ".data.repository.pullRequest",
    ];
    let pull = loads(&gh_answer(h.gh(&args), "gh api graphql")?)
        .ok()
        .filter(Value::is_object);
    let Some(pull) = pull else {
        return Err(fail("gh api graphql printed no JSON object", 5));
    };
    let bot_logins: BTreeSet<String> = nodes(pull.get("latestReviews"))
        .iter()
        .filter(|r| {
            r.get("author")
                .filter(|a| truthy(*a))
                .and_then(|a| a.get("__typename"))
                .and_then(Value::as_str)
                == Some("Bot")
        })
        .map(login)
        .collect();
    let author = login(&view);
    let reviews: Vec<&Value> = view
        .get("latestReviews")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| r.is_object())
        .collect();
    let replied = reviews
        .iter()
        .filter(|r| login(r) == author)
        .map(|r| str_or_empty(r.get("submittedAt")))
        .chain([head])
        .max()
        .unwrap();
    let state_of = |r: &Value| r.get("state").and_then(Value::as_str).map(String::from);
    let human: Vec<&Value> = reviews
        .iter()
        .copied()
        .filter(|r| {
            r.get("authorAssociation")
                .and_then(Value::as_str)
                .is_some_and(|a| PR_REVIEWERS.contains(&a))
                && login(r) != author
                && !bot_logins.contains(&login(r))
        })
        .collect();
    let (mut items, mut capped) = (Vec::new(), Vec::new());

    let mut unanswered = false;
    for r in &human {
        let at = str_or_empty(r.get("submittedAt"));
        if matches!(
            state_of(r).as_deref(),
            Some("CHANGES_REQUESTED" | "COMMENTED")
        ) && at > replied
        {
            let who = login(r);
            let body = str_or_empty(r.get("body"));
            unanswered = note(
                handled,
                &mut items,
                &mut capped,
                "review",
                &who,
                &[&who, &body, &at],
            ) || unanswered;
        }
    }
    let decision = view.get("reviewDecision").filter(|d| truthy(*d));
    let approved = decision.and_then(Value::as_str) == Some("APPROVED")
        || (decision.is_none()
            && human
                .iter()
                .any(|r| state_of(r).as_deref() == Some("APPROVED"))
            && !human
                .iter()
                .any(|r| state_of(r).as_deref() == Some("CHANGES_REQUESTED")));
    let state = str_or_empty(view.get("state")).to_lowercase();
    let in_queue = pull.get("isInMergeQueue") == Some(&Value::Bool(true))
        || pull.get("mergeQueueEntry").is_some_and(Value::is_object);
    let (mut bots, mut threads) = (0, 0);
    for thread in nodes(pull.get("reviewThreads")) {
        if !thread.is_object() || thread.get("isResolved").is_some_and(truthy) {
            continue;
        }
        let comments: Vec<&Value> = nodes(thread.get("comments"))
            .iter()
            .filter(|c| c.is_object())
            .collect();
        if comments.is_empty() || comments.iter().any(|c| login(c) == author) {
            continue;
        }
        let first = comments[0];
        let bot = first
            .get("author")
            .filter(|a| truthy(*a))
            .and_then(|a| a.get("__typename"))
            .and_then(Value::as_str)
            == Some("Bot");
        if !bot
            && !first
                .get("authorAssociation")
                .and_then(Value::as_str)
                .is_some_and(|a| PR_REVIEWERS.contains(&a))
        {
            continue;
        }
        let path = str_or_empty(thread.get("path"));
        let label = if path.is_empty() {
            "a review thread"
        } else {
            &path
        };
        if !note(
            handled,
            &mut items,
            &mut capped,
            "thread",
            label,
            &[&path, &str_or_empty(first.get("body"))],
        ) {
            continue;
        }
        if bot {
            bots += 1;
        } else {
            threads += 1;
        }
    }
    let queued = in_queue || view.get("autoMergeRequest").is_some_and(truthy);
    let merge_state = view.get("mergeStateStatus").and_then(Value::as_str);
    let conflict = merge_state == Some("DIRTY");
    if conflict {
        note(
            handled,
            &mut items,
            &mut capped,
            "conflict",
            &format!("a merge conflict with {target}"),
            &[&target],
        );
    }
    for c in &listed {
        if c.bucket == "fail" || c.bucket == "cancel" {
            note(
                handled,
                &mut items,
                &mut capped,
                "check",
                &c.name,
                &[&c.name],
            );
        }
    }
    let sig_of = |i: &Value| i["sig"].as_str().unwrap().to_string();
    let listed_sigs: BTreeSet<String> = items.iter().chain(&capped).map(sig_of).collect();
    let mut rearm: Vec<String> = Vec::new();
    for c in listed
        .iter()
        .filter(|c| c.bucket == "pass" || c.bucket == "skipping")
    {
        let sig = handled_sig("check", &[&c.name]);
        if !rearm.contains(&sig) && handled.contains_key(&sig) && !listed_sigs.contains(&sig) {
            rearm.push(sig);
        }
    }
    if !matches!(merge_state, Some("DIRTY" | "UNKNOWN")) {
        rearm.extend(
            handled
                .keys()
                .filter(|s| s.starts_with("conflict:"))
                .cloned(),
        );
    }
    let kinds: BTreeSet<String> = items
        .iter()
        .map(|i| sig_of(i).split(':').next().unwrap().to_string())
        .collect();
    let stacked = target != base;
    let merge_queue = pull.get("isMergeQueueEnabled") == Some(&Value::Bool(true));
    let mut behind = Value::Null;
    let step = if state == "merged" || state == "closed" {
        state.as_str()
    } else if !capped.is_empty() {
        "capped"
    } else if kinds.contains("review") || kinds.contains("thread") {
        "fix-reviews"
    } else if kinds.contains("conflict") {
        "rebase"
    } else if kinds.contains("check") {
        "fix-ci"
    } else if checks == "pending" || (checks == "none" && new) {
        "wait"
    } else if !approved {
        "needs-review"
    } else if queued {
        "wait"
    } else if stacked {
        "wait-base"
    } else if merge_queue {
        "merge"
    } else {
        let n = behind_by(
            h,
            pr,
            &target,
            view.get("headRefOid").unwrap_or(&Value::Null),
        )?;
        behind = Value::from(n);
        if n != 0 {
            "update"
        } else {
            "merge"
        }
    };
    let failed = listed.iter().find(|c| {
        (c.bucket == "fail" || c.bucket == "cancel")
            && c.link
                .as_deref()
                .is_some_and(|l| BUILDKITE_BUILD.is_full(l))
    });
    let ci_log = match failed {
        Some(c) if step == "fix-ci" => {
            buildkite_log(home, c, org, BUILDKITE_API, BUILDKITE_MAX_BYTES)?
        }
        _ => Value::Null,
    };
    let review = if unanswered {
        "changes"
    } else if approved {
        "approved"
    } else {
        "none"
    };
    Ok(json!({
        "pr": py_int(pr.rsplit('/').next().unwrap()), "state": state, "review": review, "bots": bots,
        "checks": checks, "queued": queued, "conflict": conflict, "stacked": stacked, "base": target, "next": step,
        "ci_log": ci_log, "threads": threads, "merge_queue": merge_queue, "items": items, "capped": capped,
        "rearm": rearm, "behind": behind,
    }))
}

/// The `while True` of pr-state: print one reading, re-reading every 30 seconds while next is wait or wait-base.
fn pr_state_loop(
    h: &mut dyn Host,
    home: &Path,
    pr: &str,
    base: &str,
    org: &str,
    handled: &Map<String, Value>,
    wait: u64,
    out: &mut dyn Write,
) -> Res<()> {
    let start = h.monotonic();
    loop {
        let reading = pr_reading(h, home, pr, base, org, handled)?;
        let elapsed = h.monotonic() - start;
        if !matches!(reading["next"].as_str(), Some("wait" | "wait-base")) || elapsed >= wait as f64
        {
            writeln!(out, "{}", dumps(&reading, DEFAULT)).ok();
            return Ok(());
        }
        h.sleep(PR_STATE_INTERVAL.min(wait as f64 - elapsed));
    }
}

pub fn pr_state_cmd(call: &Call) -> Res<()> {
    let args = &call.args[1..];
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let wait = match args {
        [] => 0,
        [flag, value]
            if flag == "--wait" && small_int(value).is_some_and(|n| (1..=600).contains(&n)) =>
        {
            small_int(value).unwrap()
        }
        _ => {
            return Err(fail(
                format!("--wait takes one value from 1 to 600 seconds. {PR_STATE_USAGE}"),
                2,
            ))
        }
    };
    let pr = board_pr(&home, &slug, &job_id, true)?;
    let meta = project_meta(&home, &slug)?;
    let mut host = System::new();
    let base = pr_base(&mut host, &meta, &pr)?;
    let handled = job_handled(&home, &job_id)?;
    let org = fm_get(&meta, "buildkite_org").unwrap_or("");
    pr_state_loop(
        &mut host,
        &home,
        &pr,
        &base,
        org,
        &handled,
        wait,
        &mut io::stdout(),
    )
}

/// Print the one JSON line of pr, status and detail, then exit with code.
fn pr_status(
    out: &mut dyn Write,
    number: &str,
    status: &str,
    detail: String,
    code: i32,
) -> Res<()> {
    writeln!(
        out,
        "{}",
        dumps(
            &json!({"pr": py_int(number), "status": status, "detail": detail}),
            DEFAULT
        )
    )
    .ok();
    if code == 0 {
        Ok(())
    } else {
        Err(exit(code))
    }
}

/// Why pr-merge refuses head: the board records no head that checks passed, or a different one.
fn unchecked_detail(number: &str, head: &str, checked: Option<&str>) -> String {
    match checked {
        None => {
            format!("PR #{number} has no head that checks passed on the board: run checks again")
        }
        Some(c) => format!(
            "PR #{number} head {} is not the head checks passed, {}: run checks again",
            &head[..12],
            &c[..12]
        ),
    }
}

/// Queue the PR with the enqueuePullRequest mutation, else through merge-async, and print one JSON line once
/// GitHub says enqueued or failed. With expect, a head other than expect exits 3 before any mutation.
fn pr_queue_run(
    h: &mut dyn Host,
    pr: &str,
    base: &str,
    number: &str,
    expect: Option<&str>,
    out: &mut dyn Write,
) -> Res<()> {
    let view = loads(&gh_pr_view(
        h,
        pr,
        &["--json", "headRefOid,baseRefName,id"],
    )?)
    .unwrap_or(Value::Null);
    let Some(sha) = view
        .get("headRefOid")
        .and_then(Value::as_str)
        .filter(|s| COMMIT.is_full(s))
        .map(String::from)
    else {
        return Err(fail("gh pr view printed no usable headRefOid", 5));
    };
    if expect.is_some_and(|e| e != sha) {
        let detail = unchecked_detail(number, &sha, expect);
        return pr_status(out, number, "unchecked", detail, 3);
    }
    if view.get("baseRefName").and_then(Value::as_str) != Some(base) {
        let target = printable_line(
            &py_str(view.get("baseRefName").unwrap_or(&Value::Null)),
            255,
        );
        return Err(fail(
            format!("PR #{number} targets {target}, not {base}: it waits for its parent"),
            4,
        ));
    }
    let parts: Vec<&str> = pr.split('/').collect();
    let path = format!("repos/{}/{}/pulls/{number}/merge-async", parts[3], parts[4]);
    let done = |out: &mut dyn Write, status: &str, detail: String, code: i32| {
        pr_status(out, number, status, detail, code)
    };

    let (id, sha_arg) = (
        format!("id={}", py_str(view.get("id").unwrap_or(&Value::Null))),
        format!("sha={sha}"),
    );
    if let Some(GhOut {
        code: GhCode::Exit(0),
        stdout,
        ..
    }) = h.gh(&[
        "api",
        "graphql",
        "-f",
        PR_ENQUEUE_MUTATION,
        "-f",
        &id,
        "-f",
        &sha_arg,
    ]) {
        let entry = loads(&stdout).ok().and_then(|v| {
            v.get("data")?
                .get("enqueuePullRequest")?
                .get("mergeQueueEntry")
                .cloned()
        });
        if let Some(entry) = entry.filter(Value::is_object) {
            let detail = entry
                .get("position")
                .filter(|p| !p.is_null())
                .map_or(String::new(), |p| format!("position {}", py_str(p)));
            return done(out, "enqueued", detail, 0);
        }
    }

    let result = h.gh(&[
        "api",
        "-X",
        "PUT",
        &path,
        "-f",
        &sha_arg,
        "-f",
        "merge_action=merge_queue",
    ]);
    let Some(GhOut {
        code: GhCode::Exit(code),
        stdout,
        stderr,
    }) = result
    else {
        return Err(gh_answer(result, "gh api merge-async").unwrap_err());
    };
    if code != 0 {
        return done(
            out,
            "refused",
            printable_line(if stderr.is_empty() { &stdout } else { &stderr }, 500),
            1,
        );
    }
    let uuid = match loads(&stdout) {
        Ok(Value::Object(reply)) => reply
            .values()
            .filter_map(Value::as_str)
            .find(|v| MERGE_ASYNC_ID.is_full(v))
            .map(String::from),
        _ => None,
    };
    let Some(uuid) = uuid else {
        return Err(fail("gh api merge-async printed no request id", 5));
    };
    let poll = format!("{path}/{uuid}");
    let start = h.monotonic();
    loop {
        let data = loads(&gh_answer(h.gh(&["api", &poll]), "gh api merge-async")?)
            .ok()
            .filter(Value::is_object);
        let Some(data) = data else {
            return Err(fail("gh api merge-async printed no JSON object", 5));
        };
        let status = str_or_empty(data.get("status")).to_lowercase();
        if status == "enqueued" {
            return done(out, &status, String::new(), 0);
        }
        if status == "failed" {
            return done(out, &status, printable_line(&dumps(&data, DEFAULT), 500), 1);
        }
        if h.monotonic() - start >= PR_QUEUE_TIMEOUT {
            return done(out, &status, printable_line(&dumps(&data, DEFAULT), 500), 3);
        }
        h.sleep(PR_QUEUE_INTERVAL);
    }
}

pub fn pr_queue_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let pr = board_pr(&home, &slug, &job_id, false)?;
    let meta = project_meta(&home, &slug)?;
    let base = fm_get(&meta, "base")
        .filter(|b| !b.is_empty())
        .unwrap_or("main");
    let number = pr.split('/').nth(6).unwrap();
    pr_queue_run(
        &mut System::new(),
        &pr,
        base,
        number,
        None,
        &mut io::stdout(),
    )
}

/// Read the PR's mergeability, again while GitHub still computes it, then merge pinned to the head checks
/// passed, hand a merge-queue repo to pr_queue_run, or print why it cannot merge.
fn pr_merge_run(
    h: &mut dyn Host,
    pr: &str,
    base: &str,
    checked: Option<&str>,
    out: &mut dyn Write,
) -> Res<()> {
    let parts: Vec<&str> = pr.split('/').collect();
    let number = parts[6];
    let (o, r, n) = (
        format!("o={}", parts[3]),
        format!("r={}", parts[4]),
        format!("n={number}"),
    );
    let args = [
        "api",
        "graphql",
        "-f",
        PR_MERGE_QUERY,
        "-f",
        &o,
        "-f",
        &r,
        "-F",
        &n,
        "--jq",
        ".data.repository.pullRequest",
    ];
    let start = h.monotonic();
    let (view, unknown) = loop {
        let view = loads(&gh_answer(h.gh(&args), "gh api graphql")?)
            .ok()
            .filter(Value::is_object);
        let Some(view) = view else {
            return Err(fail("gh api graphql printed no JSON object", 5));
        };
        let unknown = ["mergeable", "mergeStateStatus"]
            .iter()
            .any(|k| view.get(*k).and_then(Value::as_str) == Some("UNKNOWN"));
        let open = view.get("state").and_then(Value::as_str) == Some("OPEN");
        let elapsed = h.monotonic() - start;
        if !unknown || !open || elapsed >= PR_MERGE_SETTLE {
            break (view, unknown);
        }
        h.sleep(PR_MERGE_INTERVAL.min(PR_MERGE_SETTLE - elapsed));
    };
    let field = |key: &str| printable_line(&str_or_empty(view.get(key)), 255);
    let (mergeable, status) = (field("mergeable"), field("mergeStateStatus"));
    let state = field("state");
    if state != "OPEN" {
        return Err(fail(format!("{pr} is {state}, not OPEN"), 4));
    }
    let target = field("baseRefName");
    if target != base {
        return Err(fail(
            format!("PR #{number} targets {target}, not {base}: it waits for its parent"),
            4,
        ));
    }
    let done = |out: &mut dyn Write, status: &str, detail: String, code: i32| {
        pr_status(out, number, status, detail, code)
    };
    let sha = field("headRefOid");
    if !COMMIT.is_full(&sha) {
        return Err(fail("gh api graphql printed no usable headRefOid", 5));
    }
    if checked != Some(sha.as_str()) {
        return done(out, "unchecked", unchecked_detail(number, &sha, checked), 3);
    }
    if view.get("isDraft") == Some(&Value::Bool(true)) || status == "DRAFT" {
        return done(out, "draft", format!("PR #{number} is a draft"), 1);
    }
    if mergeable == "CONFLICTING" || status == "DIRTY" {
        let detail =
            format!("PR #{number} conflicts with {base}: merge {base} into the branch and push");
        return done(out, "conflict", detail, 6);
    }
    if view.get("isMergeQueueEnabled") == Some(&Value::Bool(true)) {
        return pr_queue_run(h, pr, base, number, checked, out);
    }
    if unknown {
        let detail =
            format!("GitHub had not computed mergeability after {PR_MERGE_SETTLE} seconds");
        return done(out, "unknown", detail, 3);
    }
    if status == "BEHIND" {
        let detail =
            format!("PR #{number} is behind {base}: run checks again, which updates the branch");
        return done(out, "behind", detail, 3);
    }
    if status == "BLOCKED" {
        let detail = "GitHub blocks the merge: a required review or check is missing".to_string();
        return done(out, "blocked", detail, 1);
    }
    let merge = [
        "pr",
        "merge",
        pr,
        "--merge",
        "--delete-branch",
        "--match-head-commit",
        &sha,
    ];
    match h.gh(&merge) {
        None => Err(fail("gh is not installed", 5)),
        Some(GhOut {
            code: GhCode::Timeout,
            ..
        }) => Err(fail(
            format!("gh pr merge timed out after 60 seconds: read gh pr view {pr} --json state before merging again"),
            5,
        )),
        Some(GhOut {
            code: GhCode::Exit(0),
            ..
        }) => done(out, "merged", format!("head {}", &sha[..12]), 0),
        Some(GhOut {
            code: GhCode::Exit(code),
            stderr,
            ..
        }) => {
            let first = printable_line(stderr.lines().next().unwrap_or(""), 500);
            let detail = if first.is_empty() {
                format!("gh pr merge exited {code}")
            } else {
                first
            };
            done(out, "refused", detail, 1)
        }
    }
}

pub fn pr_merge_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let pr = board_pr(&home, &slug, &job_id, false)?;
    merge_hold(&home, &slug, &job_id, &pr)?;
    let meta = project_meta(&home, &slug)?;
    let base = fm_get(&meta, "base")
        .filter(|b| !b.is_empty())
        .unwrap_or("main");
    let data = read_board(&home)?;
    let checked = find_job(&data, &job_id)
        .and_then(|j| j.get("checked_head"))
        .and_then(Value::as_str)
        .filter(|s| COMMIT.is_full(s));
    pr_merge_run(&mut System::new(), &pr, base, checked, &mut io::stdout())
}

const CI_FAILED: &[&str] = &[
    "FAILURE",
    "TIMED_OUT",
    "STARTUP_FAILURE",
    "ACTION_REQUIRED",
    "CANCELLED",
    "ERROR",
];
const PR_CLOSE_COMMENT: &str = "> \u{1f916} **Agent-authored report** \u{2014} written by factory.py pickup on the owner's behalf, posted automatically. Tests not run.\n\nThe owner closed factory job `{id}` from the dashboard, so this pull request will not merge. The decision page at {link} records the reason.";
const STACKED_CLOSE_COMMENT: &str = "> \u{1f916} **Agent-authored report** \u{2014} written by factory.py on the owner's behalf, posted automatically. Tests not run.\n\nThis pull request was stacked on factory job `{lead}`, which closed without merging, so this pull request will not merge. Factory job `{id}` went back to the ideas column.";

fn stacked_close_note(state: &str) -> &'static str {
    match state {
        "closed" => "closed with a comment",
        "already closed" => "was already closed",
        _ => "was already merged",
    }
}

/// gh's answer on this host, or None without gh.
fn gh_cli(args: &[&str]) -> Option<GhOut> {
    System::new().gh(args)
}

/// data.repository of one gh api graphql answer for repo, or why it is missing.
fn graphql_repository(
    owner: &str,
    name: &str,
    body: &str,
    device: &str,
) -> Result<Map<String, Value>, String> {
    graphql_repository_via(&mut System::new(), owner, name, body, device)
}

fn graphql_repository_via(
    host: &mut dyn Host,
    owner: &str,
    name: &str,
    body: &str,
    device: &str,
) -> Result<Map<String, Value>, String> {
    let q =
        format!("query=query {{ repository(owner: \"{owner}\", name: \"{name}\") {{ {body}}} }}");
    let Some(r) = host.gh(&["api", "graphql", "-f", &q]) else {
        return Err(format!("gh is not installed on {device}"));
    };
    let code = match r.code {
        GhCode::Timeout => {
            return Err(format!(
                "gh api graphql timed out after 60 seconds on {device}"
            ))
        }
        GhCode::Exit(code) => code,
    };
    let found = loads(&r.stdout)
        .ok()
        .and_then(|v| v.get("data")?.get("repository")?.as_object().cloned());
    match found {
        Some(found) => Ok(found),
        None if code != 0 => Err(format!(
            "gh api graphql exited {code} (check gh auth status on {device})"
        )),
        None => Err(format!("gh api graphql returned no repository (check the project file's repo and gh auth status on {device})")),
    }
}

/// The states and head commits of repo's pull requests from one gh api graphql call, or why not. A pull
/// request GitHub returns null for is left out.
pub(crate) fn pr_states(
    repo: &str,
    numbers: &[u64],
    device: &str,
) -> Result<BTreeMap<u64, (Value, Value)>, String> {
    let (owner, name) = repo.split_once('/').unwrap_or((repo, ""));
    let body: String = numbers
        .iter()
        .map(|n| format!("p{n}: pullRequest(number: {n}) {{ state headRefOid }} "))
        .collect();
    let found = graphql_repository(owner, name, &body, device)?;
    Ok(numbers
        .iter()
        .filter_map(|n| {
            let p = found.get(&format!("p{n}")).filter(|p| p.is_object())?;
            let field = |k: &str| p.get(k).cloned().unwrap_or(Value::Null);
            Some((*n, (field("state"), field("headRefOid"))))
        })
        .collect())
}

/// The merge commit of each of repo's merged pull requests, None when GitHub reports none, from one gh api
/// graphql call for each 50 numbers, or why not.
pub(crate) fn pr_merge_commits(
    repo: &str,
    numbers: &[u64],
    device: &str,
) -> Result<BTreeMap<u64, Option<String>>, String> {
    pr_merge_commits_via(&mut System::new(), repo, numbers, device)
}

fn pr_merge_commits_via(
    host: &mut dyn Host,
    repo: &str,
    numbers: &[u64],
    device: &str,
) -> Result<BTreeMap<u64, Option<String>>, String> {
    let (owner, name) = repo.split_once('/').unwrap_or((repo, ""));
    let mut oids = BTreeMap::new();
    for chunk in numbers.chunks(50) {
        let body: String = chunk
            .iter()
            .map(|n| format!("p{n}: pullRequest(number: {n}) {{ state mergeCommit {{ oid }} }} "))
            .collect();
        let found = graphql_repository_via(host, owner, name, &body, device)?;
        for n in chunk {
            let pr = found.get(&format!("p{n}")).filter(|p| p.is_object());
            let oid = pr
                .filter(|p| p.get("state").and_then(Value::as_str) == Some("MERGED"))
                .and_then(|p| p.get("mergeCommit")?.get("oid")?.as_str())
                .filter(|oid| COMMIT.is_full(oid));
            oids.insert(*n, oid.map(String::from));
        }
    }
    Ok(oids)
}

/// The head commit and pr_signal of each of repo's pull requests from one gh api graphql call, or why not.
/// handled maps each number to its job's handled entries.
pub(crate) fn pr_signals(
    repo: &str,
    numbers: &[u64],
    device: &str,
    handled: &BTreeMap<u64, Map<String, Value>>,
) -> Result<BTreeMap<u64, (Value, Option<&'static str>)>, String> {
    let (owner, name) = repo.split_once('/').unwrap_or((repo, ""));
    let parts: Vec<String> = numbers
        .iter()
        .map(|n| format!("p{n}: pullRequest(number: {n}) {{ state mergeable headRefOid baseRefName author {{ login }} reviewThreads(last: 50) {{ nodes {{ isResolved path comments(first: 20) {{ nodes {{ authorAssociation body author {{ __typename login }} }} }} }} }} commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ contexts(first: 50) {{ nodes {{ __typename ... on CheckRun {{ name conclusion isRequired(pullRequestNumber: {n}) }} ... on StatusContext {{ context state isRequired(pullRequestNumber: {n}) }} }} }} }} }} }} }} }}"))
        .collect();
    let found = graphql_repository(owner, name, &format!("{} ", parts.join(" ")), device)?;
    Ok(numbers
        .iter()
        .filter_map(|n| {
            let p = found.get(&format!("p{n}")).filter(|p| p.is_object())?;
            let head = p.get("headRefOid").cloned().unwrap_or(Value::Null);
            Some((*n, (head, pr_signal(p, handled.get(n)))))
        })
        .collect())
}

/// The nodes list under v[key], or none.
fn nodes_under<'a>(v: Option<&'a Value>, key: &str) -> &'a [Value] {
    v.filter(|v| truthy(*v))
        .and_then(|v| v.get(key))
        .and_then(|c| c.get("nodes"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// `str(v or "")`.
fn text_or_empty(v: Option<&Value>) -> String {
    v.filter(|v| truthy(*v)).map(py_str).unwrap_or_default()
}

/// What an open pull request needs from babysit, in pr_reading's order: fix-reviews for an open counted
/// review thread, rebase for a merge conflict, fix-ci for a failed required check, else None. A handled
/// thread is skipped, and a conflict or check worked HANDLED_CAP times is no signal.
pub(crate) fn pr_signal(
    pull: &Value,
    handled: Option<&Map<String, Value>>,
) -> Option<&'static str> {
    if pull.get("state").and_then(Value::as_str) != Some("OPEN") {
        return None;
    }
    let empty = Map::new();
    let handled = handled.unwrap_or(&empty);
    let capped = |kind: &str, part: &str| {
        handled
            .get(&handled_sig(kind, &[part]))
            .and_then(|e| e.get("count"))
            .and_then(Value::as_f64)
            .is_some_and(|c| c >= HANDLED_CAP as f64)
    };
    let login = |item: &Value| text_or_empty(item.get("author").and_then(|a| a.get("login")));
    let author = login(pull);
    for thread in nodes_under(Some(pull), "reviewThreads") {
        if !thread.is_object() || truthy(thread.get("isResolved")) {
            continue;
        }
        let comments: Vec<&Value> = nodes_under(Some(thread), "comments")
            .iter()
            .filter(|c| c.is_object())
            .collect();
        if comments.is_empty() || comments.iter().any(|c| login(c) == author) {
            continue;
        }
        let first = comments[0];
        let bot = first
            .get("author")
            .and_then(|a| a.get("__typename"))
            .and_then(Value::as_str)
            == Some("Bot");
        let member = first
            .get("authorAssociation")
            .and_then(Value::as_str)
            .is_some_and(|a| PR_REVIEWERS.contains(&a));
        if !bot && !member {
            continue;
        }
        let sig = handled_sig(
            "thread",
            &[
                &text_or_empty(thread.get("path")),
                &text_or_empty(first.get("body")),
            ],
        );
        if !handled.contains_key(&sig) {
            return Some("fix-reviews");
        }
    }
    if pull.get("mergeable").and_then(Value::as_str) == Some("CONFLICTING")
        && !capped("conflict", &text_or_empty(pull.get("baseRefName")))
    {
        return Some("rebase");
    }
    for node in nodes_under(Some(pull), "commits") {
        let rollup = node.get("commit").and_then(|c| c.get("statusCheckRollup"));
        for c in nodes_under(rollup, "contexts") {
            let outcome = c
                .get("conclusion")
                .filter(|v| truthy(*v))
                .or_else(|| c.get("state"))
                .and_then(Value::as_str);
            let name = c
                .get("name")
                .filter(|v| truthy(*v))
                .or_else(|| c.get("context"));
            if c.get("isRequired") == Some(&Value::Bool(true))
                && outcome.is_some_and(|o| CI_FAILED.contains(&o))
                && !capped("check", &text_or_empty(name))
            {
                return Some("fix-ci");
            }
        }
    }
    None
}

/// (state, why) after closing the job's open PR with a comment, PR_CLOSE_COMMENT unless one is given.
pub(crate) fn close_pull_request(
    home: &Path,
    j: &Value,
    device: &str,
    comment: Option<&str>,
) -> Res<(&'static str, String)> {
    let pr = py_str(&j["pr"]);
    let project = py_str(&j["project"]);
    let md = project_files(home, &project)?.0;
    if !md.exists() {
        return Ok((
            "open",
            format!("there is no project file for {project} on {device}"),
        ));
    }
    let meta = frontmatter(&md)
        .map_err(|e| fail(os_error(&e, Some(&md)), 1))?
        .0;
    let repo = fm_get(&meta, "repo").unwrap_or("").to_string();
    let m = j["pr"].as_str().and_then(|p| GH_PR.fullmatch(p));
    if repo.is_empty() {
        return Ok(("open", "the project file names no repo".into()));
    }
    if !m.is_some_and(|m| m[1].to_lowercase() == repo.to_lowercase()) {
        return Ok(("open", format!("its URL is not a pull request in {repo}")));
    }
    let view = gh_cli(&[
        "pr",
        "view",
        &pr,
        "--json",
        "state,isCrossRepository,headRefName",
        "--jq",
        ".state + \" \" + (.isCrossRepository | tostring) + \" \" + .headRefName",
    ]);
    let Some(view) = view else {
        return Ok(("open", format!("gh is not installed on {device}")));
    };
    match view.code {
        GhCode::Timeout => {
            return Ok((
                "open",
                format!("gh pr view timed out after 60 seconds on {device}"),
            ))
        }
        GhCode::Exit(0) => {}
        GhCode::Exit(code) => {
            return Ok((
                "open",
                format!("gh pr view exited {code} (check gh auth status on {device})"),
            ))
        }
    }
    let line = strip(&view.stdout);
    let (state, rest) = line.split_once(' ').unwrap_or((line, ""));
    let (cross, head) = rest.split_once(' ').unwrap_or((rest, ""));
    if state == "MERGED" {
        return Ok(("merged", String::new()));
    }
    if state == "CLOSED" {
        return Ok(("already closed", String::new()));
    }
    if head.is_empty() {
        return Ok(("open", "gh pr view reported no head branch".into()));
    }
    if cross != "false" {
        return Ok((
            "open",
            format!("its head branch {head} comes from another repository, not {repo}"),
        ));
    }
    let branch = text_or_empty(j.get("branch"));
    if branch.is_empty() {
        return Ok(("open", "the job has no branch".into()));
    }
    if head != branch {
        return Ok((
            "open",
            format!("its head branch {head} is not the job's branch {branch}"),
        ));
    }
    if state != "OPEN" {
        return Ok(("open", format!("gh pr view reported state {state}")));
    }
    let comment = match comment {
        Some(c) => c.to_string(),
        None => {
            let id = py_str(&j["id"]);
            let link = format!("{}/#{id}/decision", api_url(home)?);
            PR_CLOSE_COMMENT
                .replace("{id}", &id)
                .replace("{link}", &link)
        }
    };
    let Some(close) = gh_cli(&["pr", "close", &pr, "--comment", &comment]) else {
        return Ok(("open", format!("gh is not installed on {device}")));
    };
    Ok(match close.code {
        GhCode::Timeout => (
            "open",
            format!("gh pr close timed out after 60 seconds on {device}"),
        ),
        GhCode::Exit(0) => ("closed", String::new()),
        GhCode::Exit(code) => (
            "open",
            format!("gh pr close exited {code} (check gh auth status on {device})"),
        ),
    })
}

/// Close each stacked rider's open PR after its lead closed, then rewrite the note of every rider whose PR
/// is no longer open in one board write.
pub(crate) fn close_stacked_prs(
    home: &Path,
    lead_id: &str,
    riders: &[Value],
    device: &str,
) -> Res<()> {
    let mut done = Vec::new();
    for r in riders {
        let (job_id, pr) = (py_str(&r["id"]), py_str(&r["pr"]));
        let mut rider = r.clone();
        rider["branch"] = Value::from(format!("factory/{job_id}"));
        let comment = STACKED_CLOSE_COMMENT
            .replace("{lead}", lead_id)
            .replace("{id}", &job_id);
        let (state, why) = close_pull_request(home, &rider, device, Some(&comment))?;
        if state == "open" {
            eprintln!("factory.py: leaves stacked pull request {pr} of {job_id} open: {why}");
            continue;
        }
        eprintln!(
            "factory.py: stacked pull request {pr} of {job_id} {}",
            stacked_close_note(state)
        );
        done.push((
            job_id,
            format!("unbatched: {lead_id} closed; stacked PR {pr} "),
            state,
        ));
    }
    if done.is_empty() {
        return Ok(());
    }
    cas(home, "/api/board", |text| {
        let mut board = load_board(text)?;
        let mut hit = false;
        for (job_id, note, state) in &done {
            let found = find_job(&board, job_id);
            if found.and_then(|f| f.get("note"))
                != Some(&Value::from(format!("{note}is still open")))
            {
                continue;
            }
            patch_job(
                &mut board,
                job_id,
                &json!({"note": format!("{note}{}", stacked_close_note(state))}),
            );
            hit = true;
        }
        Ok((hit.then(|| dump_board(&board)), ()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull(
        state: &str,
        threads: Vec<Vec<Value>>,
        mergeable: &str,
        contexts: Vec<Value>,
        base: &str,
    ) -> Value {
        let threads: Vec<Value> = threads
            .into_iter()
            .map(|t| json!({"isResolved": false, "path": "web/app.js", "comments": {"nodes": t}}))
            .collect();
        json!({"state": state, "mergeable": mergeable, "headRefOid": "abc", "baseRefName": base,
               "author": {"login": "bot-author"}, "reviewThreads": {"nodes": threads},
               "commits": {"nodes": [{"commit": {"statusCheckRollup": {"contexts": {"nodes": contexts}}}}]}})
    }

    fn comment(login: &str, association: &str, kind: &str, body: &str) -> Value {
        json!({"authorAssociation": association, "body": body, "author": {"__typename": kind, "login": login}})
    }

    fn failed() -> Value {
        json!({"__typename": "CheckRun", "name": "lint", "conclusion": "FAILURE", "isRequired": true})
    }

    fn with(mut v: Value, key: &str, value: Value) -> Value {
        v[key] = value;
        v
    }

    const BODY: &str = "Drop the unused import.";

    #[test]
    fn pr_signals_follow_the_reading_order() {
        let reviewer = vec![comment("reviewer-a", "MEMBER", "User", BODY)];
        let bot = vec![comment("review-bot", "NONE", "Bot", BODY)];
        let mut both = reviewer.clone();
        both.push(comment("bot-author", "OWNER", "User", BODY));
        let error = json!({"__typename": "StatusContext", "state": "ERROR", "isRequired": true});
        for (node, reason) in [
            (
                pull(
                    "OPEN",
                    vec![reviewer.clone()],
                    "CONFLICTING",
                    vec![failed()],
                    "main",
                ),
                Some("fix-reviews"),
            ),
            (
                pull("OPEN", vec![bot], "MERGEABLE", vec![], "main"),
                Some("fix-reviews"),
            ),
            (
                pull("OPEN", vec![], "CONFLICTING", vec![failed()], "main"),
                Some("rebase"),
            ),
            (
                pull("OPEN", vec![], "MERGEABLE", vec![failed()], "main"),
                Some("fix-ci"),
            ),
            (
                pull("OPEN", vec![], "MERGEABLE", vec![error], "main"),
                Some("fix-ci"),
            ),
            (pull("OPEN", vec![both], "MERGEABLE", vec![], "main"), None),
            (
                pull(
                    "OPEN",
                    vec![vec![comment("stranger-a", "NONE", "User", BODY)]],
                    "MERGEABLE",
                    vec![],
                    "main",
                ),
                None,
            ),
            (
                pull(
                    "OPEN",
                    vec![],
                    "MERGEABLE",
                    vec![with(failed(), "isRequired", json!(false))],
                    "main",
                ),
                None,
            ),
            (
                pull(
                    "OPEN",
                    vec![],
                    "MERGEABLE",
                    vec![with(failed(), "conclusion", json!("SUCCESS"))],
                    "main",
                ),
                None,
            ),
            (
                pull("MERGED", vec![], "CONFLICTING", vec![failed()], "main"),
                None,
            ),
        ] {
            assert_eq!(pr_signal(&node, None), reason, "{node}");
        }
    }

    /// Answers each merge-commit query for the numbers it names: odd numbers are MERGED, even ones OPEN, and 51
    /// is missing from the answer.
    #[derive(Default)]
    struct MergeCommits {
        calls: Vec<String>,
    }

    impl Host for MergeCommits {
        fn gh(&mut self, args: &[&str]) -> Option<GhOut> {
            let query = args.last().unwrap().to_string();
            let mut repository = Map::new();
            for part in query.split("pullRequest(number: ").skip(1) {
                let n: u64 = part.split(')').next().unwrap().parse().unwrap();
                if n != 51 {
                    let state = if n % 2 == 1 { "MERGED" } else { "OPEN" };
                    repository.insert(
                        format!("p{n}"),
                        json!({"state": state, "mergeCommit": {"oid": format!("{n:040x}")}}),
                    );
                }
            }
            self.calls.push(query);
            ok(&json!({"data": {"repository": repository}}).to_string())
        }

        fn monotonic(&mut self) -> f64 {
            0.0
        }

        fn sleep(&mut self, _: f64) {}

        fn unix_time(&mut self) -> i64 {
            0
        }
    }

    /// Ported from tests/cli/test_outcomes.py OutcomesUnknownTest.test_merge_commits_query: 50 pull requests go
    /// in each gh call, and only a MERGED one with a commit gets an oid.
    #[test]
    fn pr_merge_commits_batches_fifty_per_call() {
        let mut host = MergeCommits::default();
        let numbers: Vec<u64> = (1..=51).collect();
        let found = pr_merge_commits_via(&mut host, "example/alpha", &numbers, "host-a").unwrap();
        assert_eq!(host.calls.len(), 2);
        assert_eq!(found[&1], Some(format!("{:040x}", 1)));
        assert_eq!((&found[&2], &found[&51]), (&None, &None));
        assert_eq!(found.len(), 51);
    }

    /// Ported from tests/cli/test_outcomes.py OutcomesUnknownTest.test_bad_oid and test_gh_failure: an oid that
    /// is not 40 hex digits is dropped, and a missing gh names the host.
    #[test]
    fn pr_merge_commits_rejects_bad_oids_and_missing_gh() {
        let answer = json!({"data": {"repository": {"p9": {"state": "MERGED", "mergeCommit": {"oid": "--output=x"}}}}});
        let mut fake = Fake::default().on(&["api", "graphql"], vec![ok(&answer.to_string())]);
        let found = pr_merge_commits_via(&mut fake, "example/alpha", &[9], "host-a").unwrap();
        assert_eq!(found[&9], None);
        let mut fake = Fake::default().on(&["api", "graphql"], vec![None]);
        assert_eq!(
            pr_merge_commits_via(&mut fake, "example/alpha", &[9], "host-a"),
            Err("gh is not installed on host-a".to_string())
        );
    }

    #[test]
    fn pr_signal_skips_capped_and_handled_items() {
        let capped = json!({"count": 3, "at": "2026-01-02T12:00:00Z"});
        let once = json!({"count": 2, "at": "2026-01-02T12:00:00Z"});
        let handled = |sig: &str, e: &Value| json!({sig: e}).as_object().unwrap().clone();
        let lint = "check:da966368ea663ea5";
        let conflict = handled_sig("conflict", &["main"]);
        let other = with(failed(), "name", json!("test"));
        for (node, h, reason) in [
            (
                pull("OPEN", vec![], "MERGEABLE", vec![failed()], "main"),
                handled(lint, &capped),
                None,
            ),
            (
                pull("OPEN", vec![], "MERGEABLE", vec![failed()], "main"),
                handled(lint, &once),
                Some("fix-ci"),
            ),
            (
                pull(
                    "OPEN",
                    vec![],
                    "MERGEABLE",
                    vec![failed(), other.clone()],
                    "main",
                ),
                handled(lint, &capped),
                Some("fix-ci"),
            ),
            (
                pull("OPEN", vec![], "CONFLICTING", vec![], "main"),
                handled(&conflict, &capped),
                None,
            ),
            (
                pull("OPEN", vec![], "CONFLICTING", vec![other], "main"),
                handled(&conflict, &capped),
                Some("fix-ci"),
            ),
            (
                pull("OPEN", vec![], "CONFLICTING", vec![], "release"),
                handled(&conflict, &capped),
                Some("rebase"),
            ),
            (
                pull("OPEN", vec![], "CONFLICTING", vec![], "main"),
                handled(&conflict, &once),
                Some("rebase"),
            ),
        ] {
            assert_eq!(pr_signal(&node, Some(&h)), reason, "{node}");
        }
        let thread = "thread:892c9835f792e6f5";
        let bot = vec![comment("review-bot", "NONE", "Bot", BODY)];
        let later = vec![comment("review-bot", "NONE", "Bot", "Rename x.")];
        let h = handled(thread, &json!({"count": 1, "at": "2026-01-02T12:00:00Z"}));
        assert_eq!(
            pr_signal(
                &pull("OPEN", vec![bot.clone()], "MERGEABLE", vec![], "main"),
                Some(&h)
            ),
            None
        );
        assert_eq!(
            pr_signal(
                &pull(
                    "OPEN",
                    vec![bot.clone(), later],
                    "MERGEABLE",
                    vec![],
                    "main"
                ),
                Some(&h)
            ),
            Some("fix-reviews")
        );
        assert_eq!(
            pr_signal(
                &pull("OPEN", vec![bot], "MERGEABLE", vec![failed()], "main"),
                Some(&h)
            ),
            Some("fix-ci")
        );
    }

    #[test]
    fn token_redacted_before_truncation() {
        let token = "Z".repeat(24);
        let text = format!("{}{token}{}", "x".repeat(190), "y".repeat(86));
        assert_eq!(text.chars().count(), 300);
        let line = buildkite_unreachable(&text, &token);
        assert!(!line.contains('Z'), "{line}");
    }

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
        Some(GhOut {
            code: GhCode::Exit(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
        })
    }

    fn ok(stdout: &str) -> Option<GhOut> {
        reply(0, stdout, "")
    }

    fn timeout() -> Option<GhOut> {
        Some(GhOut {
            code: GhCode::Timeout,
            stdout: String::new(),
            stderr: String::new(),
        })
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
            self.rules
                .push((prefix.iter().map(|s| s.to_string()).collect(), replies));
            self
        }

        fn count(&self, word: &str) -> usize {
            self.calls
                .iter()
                .filter(|c| c.iter().any(|a| a == word))
                .count()
        }
    }

    impl Host for Fake {
        fn gh(&mut self, args: &[&str]) -> Option<GhOut> {
            self.calls
                .push(args.iter().map(|s| s.to_string()).collect());
            let (_, replies) = self
                .rules
                .iter_mut()
                .find(|(prefix, _)| {
                    prefix.len() <= args.len() && prefix.iter().zip(args).all(|(p, a)| p == a)
                })
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
        run(
            name,
            "COMPLETED",
            conclusion.into(),
            "2026-01-01T00:00:00Z".into(),
        )
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
                "pending" => run(
                    name,
                    "IN_PROGRESS",
                    Value::Null,
                    "2026-01-01T00:00:00Z".into(),
                ),
                "pass" => done(name, "SUCCESS"),
                "fail" => done(name, "FAILURE"),
                "cancel" => done(name, "CANCELLED"),
                _ => done(name, "SKIPPED"),
            })
            .collect();
        rollup(nodes, false)
    }

    fn behind_fake(
        behind: &[(u64, &str)],
        polls: Vec<Option<GhOut>>,
        update: Option<GhOut>,
    ) -> Fake {
        let views = behind
            .iter()
            .map(|(_, sha)| ok(&json!({"headRefOid": sha, "baseRefName": "main"}).to_string()))
            .collect();
        let mut fake = Fake::default().on(
            &["pr", "view", PR, "--json", "headRefOid,baseRefName"],
            views,
        );
        for (n, sha) in behind {
            let path = format!("repos/example/sample/compare/main...{sha}");
            fake = fake.on(&["api", &path], vec![ok(&format!("{n}\n"))]);
        }
        fake.on(&["api", "graphql"], polls)
            .on(&["pr", "update-branch", PR], vec![update])
    }

    /// (exit code, stdout, stderr) of checks_loop.
    fn run_loop(fake: &mut Fake, timeout: u64) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = match checks_loop(fake, PR, timeout, &mut out, &mut err) {
            Ok(_) => 0,
            Err(e) => {
                err.extend(e.stderr.as_bytes());
                e.code
            }
        };
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn checks(
        polls: Vec<Option<GhOut>>,
        timeout: u64,
        behind: &[(u64, &str)],
    ) -> ((i32, String, String), Fake) {
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
        let polls = vec![
            listed(&[("pass", "ci")]),
            listed(&[]),
            listed(&[("pending", "ci")]),
            listed(&[("pass", "ci")]),
        ];
        let (result, fake) = checks(polls, 540, &[(1, &old()), (0, &new())]);
        assert_eq!(result, got(0, "pass ci\n", UPDATED));
        assert_eq!(fake.count("update-branch"), 1);
        assert_eq!(fake.count("headRefOid,baseRefName"), 4);
        assert_eq!(fake.now, 30.0);
    }

    // ChecksTest.test_stale_head_after_update_is_pending
    #[test]
    fn stale_head_after_update_is_pending() {
        let (result, fake) = checks(
            vec![listed(&[("pass", "ci")])],
            540,
            &[(1, &old()), (1, &old()), (0, &new())],
        );
        assert_eq!(result, got(0, "pass ci\n", UPDATED));
        assert_eq!(fake.count("update-branch"), 1);
        assert_eq!(fake.now, 20.0);
    }

    // ChecksTest.test_new_head_passing_exits_0_on_first_reading
    #[test]
    fn new_head_passes_on_first_reading() {
        let (result, fake) = checks(
            vec![listed(&[("pass", "ci")])],
            540,
            &[(1, &old()), (0, &new())],
        );
        assert_eq!(result, got(0, "pass ci\n", UPDATED));
        assert_eq!(fake.count("headRefOid,baseRefName"), 2);
        assert_eq!(fake.now, 10.0);
    }

    // ChecksTest.test_checks_are_read_for_the_head_sha
    #[test]
    fn checks_read_for_head_sha() {
        let polls = vec![
            listed(&[("pass", "ci")]),
            listed(&[]),
            listed(&[("pass", "ci")]),
        ];
        let (_, fake) = checks(polls, 540, &[(1, &old()), (1, &old()), (0, &new())]);
        let shas: Vec<String> = fake
            .calls
            .iter()
            .filter(|c| c[1] == "graphql")
            .map(|c| c[9].clone())
            .collect();
        assert_eq!(
            shas,
            [
                format!("sha={}", old()),
                format!("sha={}", old()),
                format!("sha={}", new())
            ]
        );
        let views = fake
            .calls
            .iter()
            .filter(|c| c[0] == "pr" && c[1] == "view")
            .count();
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
        let polls = vec![
            miss(),
            listed(&[("pending", "test"), ("pass", "lint")]),
            listed(&[("pass", "test"), ("pass", "lint")]),
        ];
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
            (
                rollup(vec![done("ci", "SUCCESS")], true),
                "listed more than 100 checks",
            ),
            (None, "gh is not installed"),
        ] {
            let (result, fake) = checks(vec![poll], 540, &up_to_date());
            let warn = format!(
                "factory.py: warning: gh api graphql gave no usable check list ({problem})\n"
            );
            assert_eq!(
                result,
                (5, String::new(), warn.repeat(13) + NEVER),
                "{problem}"
            );
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
        let (result, fake) = checks(
            vec![listed(&[("pending", "ci")]), miss()],
            540,
            &up_to_date(),
        );
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
        assert_eq!(
            result,
            (0, "no checks reported\n".to_string(), WARN.repeat(12))
        );
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
        for (update, why) in [
            (timeout(), "gh pr update-branch timed out after 60 seconds"),
            (None, "gh is not installed"),
        ] {
            let mut fake = behind_fake(&[(2, &old())], vec![listed(&[("pass", "ci")])], update);
            assert_eq!(
                run_loop(&mut fake, 300),
                (5, String::new(), format!("factory.py: {why}\n"))
            );
        }
    }

    // ChecksTest.test_gh_problem_exits_5, the timeout and missing gh subtests
    #[test]
    fn state_view_timeout_or_missing_gh() {
        for (view, why) in [
            (None, "gh is not installed"),
            (timeout(), "gh pr view timed out after 60 seconds"),
        ] {
            let mut fake = Fake::default().on(&["pr", "view"], vec![view]);
            let e = gh_pr_view(&mut fake, PR, &["--json", "state", "--jq", ".state"])
                .err()
                .unwrap();
            assert_eq!((e.code, e.stderr), (5, format!("factory.py: {why}\n")));
            assert_eq!(
                fake.calls,
                [["pr", "view", PR, "--json", "state", "--jq", ".state"]]
            );
        }
    }

    fn head(reply: Option<GhOut>) -> ((Vec<(String, String)>, Option<String>), Fake) {
        let mut fake = Fake::default().on(&["api", "graphql"], vec![reply]);
        let (checks, problem) = head_checks(&mut fake, PR, &new());
        (
            (
                checks.into_iter().map(|c| (c.name, c.bucket)).collect(),
                problem,
            ),
            fake,
        )
    }

    fn named(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, b)| (n.to_string(), b.to_string()))
            .collect()
    }

    // HeadChecksTest.test_unusable_replies and test_query_names_the_sha
    #[test]
    fn head_checks_unusable() {
        let ((checks, problem), fake) = head(rollup(vec![done("ci", "SUCCESS")], false));
        assert_eq!((checks, problem), (named(&[("ci", "pass")]), None));
        let sha = format!("sha={}", new());
        let expected = [
            "api",
            "graphql",
            "-f",
            HEAD_CHECKS_QUERY,
            "-f",
            "o=example",
            "-f",
            "r=sample",
            "-f",
            &sha,
            "--jq",
            ".data.repository.object",
        ];
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
            (
                rollup(vec![done("ci", "SUCCESS")], true),
                "listed more than 100 checks",
            ),
        ] {
            assert_eq!(
                head(reply).0,
                (vec![], Some(problem.to_string())),
                "{problem}"
            );
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
            assert_eq!(
                head(reply).0,
                (vec![], Some("printed no check list".to_string())),
                "{node}"
            );
        }
    }

    // HeadChecksTest.test_pending_run_wins_over_earlier_pass
    #[test]
    fn head_checks_pending_wins() {
        let nodes = vec![
            run(
                "ci",
                "COMPLETED",
                "SUCCESS".into(),
                "2026-01-01T00:00:05Z".into(),
            ),
            run("ci", "QUEUED", Value::Null, Value::Null),
            run(
                "lint",
                "IN_PROGRESS",
                Value::Null,
                "2026-01-01T00:00:01Z".into(),
            ),
            run(
                "lint",
                "COMPLETED",
                "FAILURE".into(),
                "2026-01-01T00:00:09Z".into(),
            ),
        ];
        assert_eq!(
            head(rollup(nodes, false)).0,
            (named(&[("ci", "pending"), ("lint", "pending")]), None)
        );
        let nodes = vec![
            run(
                "ci",
                "COMPLETED",
                "FAILURE".into(),
                "2026-01-01T00:00:10Z".into(),
            ),
            run(
                "ci",
                "COMPLETED",
                "SUCCESS".into(),
                "2026-01-01T00:00:05Z".into(),
            ),
            status("lint", "FAILURE", "2026-01-01T00:00:01Z"),
            status("lint", "SUCCESS", "2026-01-01T00:00:02Z"),
        ];
        assert_eq!(
            head(rollup(nodes, false)).0,
            (named(&[("ci", "fail"), ("lint", "pass")]), None)
        );
    }

    // PrBehindTest.test_bad_output_exits_5
    #[test]
    fn pr_behind_bad_outputs() {
        let view = |sha: &str, base: &str| {
            ok(&json!({"headRefOid": sha, "baseRefName": base}).to_string())
        };
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
            let mut fake = Fake::default()
                .on(&["pr", "view"], vec![view])
                .on(&["api"], vec![compare]);
            assert_eq!(pr_behind(&mut fake, PR).err().unwrap().code, 5);
        }
        let mut fake = Fake::default()
            .on(&["pr", "view"], vec![good])
            .on(&["api"], vec![ok("1\n")]);
        assert_eq!(
            pr_behind(&mut fake, PR).ok().unwrap(),
            (1, old(), "main".to_string())
        );
    }

    const SPR: &str = "https://github.com/example/sample/pull/44";
    const HEAD: &str = "2026-01-02T10:00:00Z";
    const PARENT: &str = "factory/041-sample-api";

    /// test_pr_state's `view()` with the given fields replaced.
    fn state_view(changes: Value) -> Option<GhOut> {
        let mut view = json!({"state": "OPEN", "baseRefName": "main", "headRefOid": "a".repeat(40),
            "reviewDecision": "APPROVED", "latestReviews": [], "mergeStateStatus": "CLEAN", "autoMergeRequest": null,
            "author": {"login": "pr-owner"}, "statusCheckRollup": [],
            "commits": [{"committedDate": "2026-01-01T09:00:00Z"}, {"committedDate": HEAD}]});
        for (k, v) in changes.as_object().unwrap() {
            view[k] = v.clone();
        }
        ok(&view.to_string())
    }

    fn buckets(buckets: &[&str]) -> Option<GhOut> {
        let list: Vec<Value> = buckets
            .iter()
            .enumerate()
            .map(|(i, b)| json!({"bucket": b, "name": format!("check-{i}")}))
            .collect();
        ok(&Value::from(list).to_string())
    }

    fn state_fake(views: Vec<Option<GhOut>>, checks: Vec<Option<GhOut>>) -> Fake {
        let graph = json!({"isInMergeQueue": false, "isMergeQueueEnabled": false, "mergeQueueEntry": null,
            "reviewThreads": {"nodes": []}, "latestReviews": {"nodes": []}});
        let compare = format!("repos/example/sample/compare/main...{}", "a".repeat(40));
        Fake::default()
            .on(&["pr", "view"], views)
            .on(&["pr", "checks"], checks)
            .on(&["api", "graphql"], vec![ok(&graph.to_string())])
            .on(&["api", &compare], vec![ok("0\n")])
    }

    /// (exit code, the one printed reading) of pr_state_loop.
    fn run_state(fake: &mut Fake, wait: u64) -> (i32, Value) {
        let mut out = Vec::new();
        let code = pr_state_loop(
            fake,
            Path::new("/nonexistent"),
            SPR,
            "main",
            "example-org",
            &Map::new(),
            wait,
            &mut out,
        )
        .map_or_else(|e| e.code, |()| 0);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches('\n').count(), 1);
        (code, loads(&text).unwrap())
    }

    fn views(fake: &Fake) -> usize {
        fake.calls.iter().filter(|c| c[1] == "view").count()
    }

    // PrStateTest.test_no_checks_waits_while_the_head_commit_is_new
    #[test]
    fn new_head_commit_waits() {
        for (age, step) in [(60, "wait"), (120, "merge")] {
            let mut fake = state_fake(vec![state_view(json!({}))], vec![buckets(&[])]);
            fake.unix = parse_stamp(HEAD).unwrap() + age;
            let reading = pr_reading(
                &mut fake,
                Path::new("/nonexistent"),
                SPR,
                "main",
                "",
                &Map::new(),
            )
            .ok()
            .unwrap();
            assert_eq!(reading["next"], step);
        }
    }

    // PrStateTest.test_wait_rereads_every_30_seconds_until_next_changes
    #[test]
    fn wait_rereads_every_30_seconds() {
        let mut fake = state_fake(
            vec![state_view(json!({}))],
            vec![
                buckets(&["pending"]),
                buckets(&["pending"]),
                buckets(&["pass"]),
            ],
        );
        let (code, reading) = run_state(&mut fake, 540);
        assert_eq!((code, reading["next"].as_str()), (0, Some("merge")));
        assert_eq!((fake.now, views(&fake)), (60.0, 3));
    }

    // PrStateTest.test_wait_rereads_a_stacked_pr_until_it_is_retargeted
    #[test]
    fn wait_rereads_stacked_pr() {
        let stacked = state_view(json!({"baseRefName": PARENT}));
        let mut fake = state_fake(
            vec![stacked.clone(), stacked, state_view(json!({}))],
            vec![buckets(&["pass"])],
        );
        let (code, reading) = run_state(&mut fake, 540);
        assert_eq!(
            (code, reading["stacked"].clone(), reading["next"].as_str()),
            (0, Value::Bool(false), Some("merge"))
        );
        assert_eq!((fake.now, views(&fake)), (60.0, 3));
    }

    // PrStateTest.test_wait_stops_at_its_deadline
    #[test]
    fn wait_stops_at_deadline() {
        let mut fake = state_fake(vec![state_view(json!({}))], vec![buckets(&["pending"])]);
        let (code, reading) = run_state(&mut fake, 45);
        assert_eq!(
            (code, reading["next"].as_str(), fake.now),
            (0, Some("wait"), 45.0)
        );
    }

    // PrStateTest.test_without_wait_a_wait_reading_prints_at_once
    #[test]
    fn no_wait_prints_at_once() {
        let mut fake = state_fake(vec![state_view(json!({}))], vec![buckets(&["pending"])]);
        let (code, reading) = run_state(&mut fake, 0);
        assert_eq!(
            (code, reading["next"].as_str(), fake.now, fake.calls.len()),
            (0, Some("wait"), 0.0, 3)
        );
    }

    // LINT in test_pr_state.py
    #[test]
    fn handled_sig_value() {
        assert_eq!(handled_sig("check", &["lint"]), "check:da966368ea663ea5");
    }

    const TOKEN: &str = "bk-synthetic-token-0001";
    const BUILD: &str = "/v2/organizations/example-org/pipelines/sample-web/builds/812";
    const JOB_ID: &str = "0190a1b2-0000-4000-8000-000000000001";
    const LINK: &str = "https://buildkite.com/example-org/sample-web/builds/812";

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    fn build_jobs(jobs: &[(&str, &str, &str)]) -> &'static str {
        let jobs: Vec<Value> = jobs
            .iter()
            .enumerate()
            .map(|(i, (kind, state, name))| json!({"id": format!("0190a1b2-0000-4000-8000-00000000000{}", i + 1), "type": kind, "state": state, "name": name}))
            .collect();
        leak(json!({"jobs": jobs}).to_string())
    }

    fn bk_home(name: &str, env: &str) -> std::path::PathBuf {
        let home = crate::shared::fs::temp_dir(name);
        if !env.is_empty() {
            std::fs::write(home.join(".env"), env).unwrap();
        }
        home
    }

    fn bk_check(link: &str) -> Check {
        Check {
            name: "buildkite/sample-web".into(),
            bucket: "fail".into(),
            link: Some(link.into()),
        }
    }

    // PrStateTest.test_fix_ci_reads_the_failed_buildkite_job_log_tail
    #[test]
    fn buildkite_tail() {
        let home = bk_home(
            "bk-tail",
            &format!("BUILDKITE_API_TOKEN={TOKEN}\nSAMPLE_SECRET=sample-secret-value-0002\n"),
        );
        let mut lines: Vec<String> = (0..100)
            .map(|i| format!("\x1b_bk;t=17000000{i:02}000\x07\x1b[32mstep {i}\x1b[0m"))
            .collect();
        lines.push("progress 10%\rprogress 100%".into());
        lines.push(format!("using {TOKEN} and sample-secret-value-0002"));
        lines.push("error  'unused' is defined but never used".into());
        let log = lines.join("\r\n") + "\r\n";
        let log_body = leak(json!({"url": "", "content": log, "size": log.len()}).to_string());
        let (origin, seen) = crate::shared::http::tests::serve(vec![
            (
                200,
                build_jobs(&[("script", "failed", "Lint frontend")]),
                vec![],
            ),
            (200, log_body, vec![]),
        ]);
        let got = buildkite_log(
            &home,
            &bk_check(&format!("{LINK}#{JOB_ID}")),
            "example-org",
            &format!("{origin}/v2"),
            BUILDKITE_MAX_BYTES,
        )
        .unwrap();
        let mut tail: Vec<String> = (23..100).map(|i| format!("step {i}")).collect();
        tail.extend([
            "progress 100%".into(),
            "using [redacted] and [redacted]".into(),
            "error  'unused' is defined but never used".into(),
        ]);
        assert_eq!(
            got,
            json!({"check": "buildkite/sample-web", "job": "Lint frontend", "tail": tail.join("\n"), "problem": ""})
        );
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
            [BUILD.to_string(), format!("{BUILD}/jobs/{JOB_ID}/log")]
        );
        for s in seen.iter() {
            let bearer = format!("Bearer {TOKEN}");
            assert_eq!(
                (
                    s.header("Authorization"),
                    s.header("Accept"),
                    s.header("User-Agent")
                ),
                (
                    Some(bearer.as_str()),
                    Some("application/json"),
                    Some("factory")
                )
            );
        }
        assert!(!got.to_string().contains(TOKEN));
    }

    // PrStateTest.test_fix_ci_picks_the_first_failed_buildkite_job
    #[test]
    fn buildkite_first_failed_job() {
        let home = bk_home("bk-first", &format!("BUILDKITE_API_TOKEN={TOKEN}\n"));
        let second = "0190a1b2-0000-4000-8000-000000000003";
        let jobs = build_jobs(&[
            ("waiter", "failed", ""),
            ("script", "passed", "Unit tests"),
            ("script", "timed_out", "Lint frontend"),
            ("script", "failed", "Type check"),
        ]);
        let log = leak(json!({"content": "x".repeat(9000) + "\n"}).to_string());
        let (origin, seen) =
            crate::shared::http::tests::serve(vec![(200, jobs, vec![]), (200, log, vec![])]);
        let got = buildkite_log(
            &home,
            &bk_check(LINK),
            "example-org",
            &format!("{origin}/v2"),
            BUILDKITE_MAX_BYTES,
        )
        .unwrap();
        assert_eq!(
            got,
            json!({"check": "buildkite/sample-web", "job": "Lint frontend", "tail": "x".repeat(8000), "problem": ""})
        );
        let paths: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.path.clone())
            .collect();
        assert_eq!(
            paths,
            [BUILD.to_string(), format!("{BUILD}/jobs/{second}/log")]
        );
        let (origin, seen) = crate::shared::http::tests::serve(vec![(
            200,
            build_jobs(&[("script", "passed", "Unit tests")]),
            vec![],
        )]);
        let got = buildkite_log(
            &home,
            &bk_check(LINK),
            "example-org",
            &format!("{origin}/v2"),
            BUILDKITE_MAX_BYTES,
        )
        .unwrap();
        assert_eq!(got["problem"], "no failed job in build 812");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    // PrStateTest.test_buildkite_problems_still_print_the_reading
    #[test]
    fn buildkite_problems() {
        let problem =
            |name: &str,
             env: &str,
             replies: Vec<(u16, &'static str, Vec<(&'static str, &'static str)>)>| {
                let home = bk_home(name, env);
                let origin = if replies.is_empty() {
                    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                    let origin =
                        format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
                    drop(listener);
                    origin
                } else {
                    crate::shared::http::tests::serve(replies).0
                };
                let got = buildkite_log(
                    &home,
                    &bk_check(&format!("{LINK}#{JOB_ID}")),
                    "example-org",
                    &format!("{origin}/v2"),
                    BUILDKITE_MAX_BYTES,
                )
                .unwrap();
                assert!(!got.to_string().contains(TOKEN));
                assert_eq!(
                    (got["job"].as_str(), got["tail"].as_str()),
                    (Some(""), Some(""))
                );
                got["problem"].as_str().unwrap().to_string()
            };
        let env = leak(format!("BUILDKITE_API_TOKEN={TOKEN}\n"));
        assert_eq!(
            problem("bk-none", "", vec![]),
            "set BUILDKITE_API_TOKEN in the factory .env to read Buildkite logs"
        );
        assert_eq!(
            problem("bk-401", env, vec![(401, "{}", vec![])]),
            "Buildkite answered 401 (the token needs read_builds and read_build_logs)"
        );
        assert_eq!(
            problem(
                "bk-302",
                env,
                vec![(
                    302,
                    "{}",
                    vec![("Location", "https://example.invalid/login")]
                )]
            ),
            "Buildkite answered 302"
        );
        assert_eq!(
            problem("bk-text", env, vec![(200, "not json", vec![])]),
            "Buildkite sent no usable log"
        );
        let jobs = build_jobs(&[("script", "failed", "Lint frontend")]);
        assert_eq!(
            problem(
                "bk-size",
                env,
                vec![(200, jobs, vec![]), (200, r#"{"size": 0}"#, vec![])]
            ),
            "Buildkite sent no usable log"
        );
        let down = problem("bk-down", env, vec![]);
        assert!(down.starts_with("can't reach Buildkite: "), "{down}");
        #[cfg(target_os = "linux")]
        assert_eq!(
            down,
            "can't reach Buildkite: <urlopen error [Errno 111] Connection refused>"
        );
    }

    // PrStateTest.test_a_buildkite_log_over_the_read_cap_is_no_usable_log
    #[test]
    fn buildkite_read_cap() {
        let home = bk_home("bk-cap", &format!("BUILDKITE_API_TOKEN={TOKEN}\n"));
        let build = leak(json!({"jobs": [{"id": JOB_ID}]}).to_string());
        let log = leak(json!({"content": "x".repeat(100)}).to_string());
        let (origin, seen) =
            crate::shared::http::tests::serve(vec![(200, build, vec![]), (200, log, vec![])]);
        let got = buildkite_log(
            &home,
            &bk_check(&format!("{LINK}#{JOB_ID}")),
            "example-org",
            &format!("{origin}/v2"),
            64,
        )
        .unwrap();
        assert_eq!(
            (got["problem"].as_str(), got["tail"].as_str()),
            (Some("Buildkite sent no usable log"), Some(""))
        );
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn buildkite_redacts_a_secret_that_extends_another() {
        let home = bk_home(
            "bk-overlap",
            &format!(
                "A_SECRET=abcdefghijkl\nBUILDKITE_API_TOKEN={TOKEN}\nB_SECRET=abcdefghijklMNOPQ\n"
            ),
        );
        let build = build_jobs(&[("script", "failed", "Lint frontend")]);
        let log =
            leak(json!({"content": "long abcdefghijklMNOPQ short abcdefghijkl\n"}).to_string());
        let (origin, _) =
            crate::shared::http::tests::serve(vec![(200, build, vec![]), (200, log, vec![])]);
        let got = buildkite_log(
            &home,
            &bk_check(&format!("{LINK}#{JOB_ID}")),
            "example-org",
            &format!("{origin}/v2"),
            BUILDKITE_MAX_BYTES,
        )
        .unwrap();
        assert_eq!(got["tail"], "long [redacted] short [redacted]");
    }

    #[test]
    fn buildkite_unreadable_env_fails_1() {
        let home = bk_home("bk-bad-env", "");
        std::fs::write(home.join(".env"), b"BUILDKITE_API_TOKEN=\xff\n").unwrap();
        let e = buildkite_log(
            &home,
            &bk_check(LINK),
            "example-org",
            "http://127.0.0.1:9/v2",
            BUILDKITE_MAX_BYTES,
        )
        .unwrap_err();
        assert_eq!(e.code, 1);
        assert!(
            e.stderr.starts_with("factory.py: ") && e.stderr.contains(".env"),
            "{}",
            e.stderr
        );
    }

    #[test]
    fn buildkite_stalled_body_times_out() {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                .unwrap();
            std::thread::sleep(Duration::from_secs(60));
        });
        let started = Instant::now();
        let got = buildkite_get(&format!("{origin}/v2/x"), TOKEN, BUILDKITE_MAX_BYTES);
        let waited = started.elapsed().as_secs();
        let problem = got.unwrap_err();
        assert!(problem.starts_with("can't reach Buildkite: "), "{problem}");
        assert!(!problem.contains(TOKEN));
        assert!((29..40).contains(&waited), "waited {waited} s");
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const UUID: &str = "0190a1b2-0000-4000-8000-0000000000aa";
    const MERGE_ASYNC: &str = "repos/example/sample/pulls/44/merge-async";

    fn queue_fake(graphql: Option<GhOut>, put: Option<GhOut>, polls: Vec<Option<GhOut>>) -> Fake {
        let view = json!({"headRefOid": SHA, "baseRefName": "main", "id": "PR_node44"}).to_string();
        let poll = format!("{MERGE_ASYNC}/{UUID}");
        Fake::default()
            .on(&["pr", "view"], vec![ok(&view)])
            .on(&["api", "graphql"], vec![graphql])
            .on(&["api", "-X", "PUT"], vec![put])
            .on(&["api", &poll], polls)
    }

    fn queue_status(status: &str) -> Option<GhOut> {
        ok(&json!({"status": status}).to_string())
    }

    fn pending_put() -> Option<GhOut> {
        ok(&json!({"id": UUID, "status": "pending"}).to_string())
    }

    /// (exit code, stdout, stderr) of pr_queue_run.
    fn run_queue(fake: &mut Fake) -> (i32, String, String) {
        let mut out = Vec::new();
        let (code, err) = match pr_queue_run(fake, SPR, "main", "44", None, &mut out) {
            Ok(()) => (0, String::new()),
            Err(e) => (e.code, e.stderr),
        };
        (code, String::from_utf8(out).unwrap(), err)
    }

    // PrQueueTest.test_enqueued_after_one_pending_poll
    #[test]
    fn queue_enqueued_after_pending_poll() {
        let mut fake = queue_fake(
            reply(1, "", "mutation refused\n"),
            pending_put(),
            vec![queue_status("pending"), queue_status("ENQUEUED")],
        );
        assert_eq!(
            run_queue(&mut fake),
            got(
                0,
                "{\"pr\": 44, \"status\": \"enqueued\", \"detail\": \"\"}\n",
                ""
            )
        );
        let sha = format!("sha={SHA}");
        assert_eq!(
            fake.calls[0],
            ["pr", "view", SPR, "--json", "headRefOid,baseRefName,id"]
        );
        assert_eq!(
            fake.calls[1],
            [
                "api",
                "graphql",
                "-f",
                PR_ENQUEUE_MUTATION,
                "-f",
                "id=PR_node44",
                "-f",
                &sha
            ]
        );
        assert_eq!(
            fake.calls[2],
            [
                "api",
                "-X",
                "PUT",
                MERGE_ASYNC,
                "-f",
                &sha,
                "-f",
                "merge_action=merge_queue"
            ]
        );
        let poll = format!("{MERGE_ASYNC}/{UUID}");
        assert_eq!(
            fake.calls[3..],
            [["api", poll.as_str()], ["api", poll.as_str()]]
        );
        assert_eq!(fake.now, 10.0);
    }

    // PrQueueTest.test_pending_at_the_deadline_exits_3
    #[test]
    fn queue_pending_at_deadline() {
        let mut fake = queue_fake(miss(), pending_put(), vec![queue_status("pending")]);
        let (code, out, _) = run_queue(&mut fake);
        let reading = loads(&out).unwrap();
        assert_eq!(
            (code, reading["status"].as_str(), fake.now),
            (3, Some("pending"), 120.0)
        );
        assert!(reading["detail"]
            .as_str()
            .unwrap()
            .contains("\"status\": \"pending\""));
        let poll = format!("{MERGE_ASYNC}/{UUID}");
        assert_eq!(
            fake.calls
                .iter()
                .filter(|c| **c == ["api", poll.as_str()])
                .count(),
            13
        );
    }

    // PrQueueTest.test_gh_problem_on_the_put_exits_5
    #[test]
    fn queue_put_timeout_or_missing_gh() {
        for (put, why) in [
            (None, "gh is not installed"),
            (timeout(), "gh api merge-async timed out after 60 seconds"),
        ] {
            let mut fake = queue_fake(miss(), put, vec![queue_status("enqueued")]);
            assert_eq!(
                run_queue(&mut fake),
                got(5, "", &format!("factory.py: {why}\n"))
            );
        }
    }

    // PrQueueTest.test_a_refused_mutation_falls_back_to_merge_async, the missing gh subtest
    #[test]
    fn queue_mutation_missing_gh() {
        let mut fake = queue_fake(None, pending_put(), vec![queue_status("enqueued")]);
        assert_eq!(
            run_queue(&mut fake),
            got(
                0,
                "{\"pr\": 44, \"status\": \"enqueued\", \"detail\": \"\"}\n",
                ""
            )
        );
        assert_eq!(fake.count("graphql"), 1);
        assert_eq!(fake.calls[2][..3], ["api", "-X", "PUT"]);
    }

    const MERGE_HEAD: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    const MOVED_HEAD: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

    #[test]
    fn checks_record_the_passed_head() {
        use crate::shared::http::tests::{home_for, serve};
        let board = r#"{"updated":"","jobs":[{"id":"042-sample-export","project":"alpha","stage":"pr-open"}]}"#;
        let (origin, seen) = serve(vec![
            (200, board, vec![("ETag", "\"b1\"")]),
            (200, "{}", vec![("ETag", "\"b2\"")]),
        ]);
        let home = home_for("checks-record-head", &origin);
        checked_head_write(&home, "042-sample-export", MERGE_HEAD).unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen[1].method, "PUT");
        let put = loads(&String::from_utf8(seen[1].body.clone()).unwrap()).unwrap();
        assert_eq!(put["jobs"][0]["checked_head"], MERGE_HEAD);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn checks_record_skips_an_unchanged_head() {
        use crate::shared::http::tests::{home_for, serve};
        let board = r#"{"updated":"","jobs":[{"id":"042-sample-export","checked_head":"a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1"}]}"#;
        let (origin, seen) = serve(vec![(200, board, vec![("ETag", "\"b1\"")])]);
        let home = home_for("checks-record-same", &origin);
        checked_head_write(&home, "042-sample-export", MERGE_HEAD).unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        std::fs::remove_dir_all(home).unwrap();
    }

    /// PR 42's GraphQL mergeability reading.
    fn mergeability(mergeable: &str, status: &str) -> Value {
        json!({"state": "OPEN", "isDraft": false, "mergeable": mergeable, "mergeStateStatus": status,
               "headRefOid": MERGE_HEAD, "baseRefName": "main", "isMergeQueueEnabled": false})
    }

    fn merge_fake(reads: Vec<Value>, merge: Option<GhOut>) -> Fake {
        let reads = reads.iter().map(|v| ok(&v.to_string())).collect();
        Fake::default()
            .on(&["api", "graphql", "-f", PR_MERGE_QUERY], reads)
            .on(&["pr", "merge"], vec![merge])
    }

    /// (exit code, stdout, stderr) of pr_merge_run with checks passed on MERGE_HEAD.
    fn run_merge(fake: &mut Fake) -> (i32, String, String) {
        run_merge_checked(fake, Some(MERGE_HEAD))
    }

    fn run_merge_checked(fake: &mut Fake, checked: Option<&str>) -> (i32, String, String) {
        let mut out = Vec::new();
        let (code, err) = match pr_merge_run(fake, PR, "main", checked, &mut out) {
            Ok(()) => (0, String::new()),
            Err(e) => (e.code, e.stderr),
        };
        (code, String::from_utf8(out).unwrap(), err)
    }

    fn merges(fake: &Fake) -> usize {
        fake.calls
            .iter()
            .filter(|c| c[..2] == ["pr", "merge"])
            .count()
    }

    fn line(status: &str, detail: &str) -> String {
        format!(
            "{}\n",
            dumps(
                &json!({"pr": 42, "status": status, "detail": detail}),
                DEFAULT
            )
        )
    }

    #[test]
    fn merge_clean_merges() {
        let mut fake = merge_fake(vec![mergeability("MERGEABLE", "CLEAN")], ok(""));
        assert_eq!(
            run_merge(&mut fake),
            got(0, &line("merged", "head a1a1a1a1a1a1"), "")
        );
        assert_eq!(
            fake.calls,
            [
                vec![
                    "api",
                    "graphql",
                    "-f",
                    PR_MERGE_QUERY,
                    "-f",
                    "o=example",
                    "-f",
                    "r=sample",
                    "-F",
                    "n=42",
                    "--jq",
                    ".data.repository.pullRequest"
                ],
                vec![
                    "pr",
                    "merge",
                    PR,
                    "--merge",
                    "--delete-branch",
                    "--match-head-commit",
                    MERGE_HEAD
                ]
            ]
        );
    }

    #[test]
    fn merge_conflict_exits_6() {
        for (mergeable, status) in [
            ("CONFLICTING", "DIRTY"),
            ("CONFLICTING", "BLOCKED"),
            ("MERGEABLE", "DIRTY"),
        ] {
            let mut fake = merge_fake(vec![mergeability(mergeable, status)], ok(""));
            let detail = "PR #42 conflicts with main: merge main into the branch and push";
            assert_eq!(run_merge(&mut fake), got(6, &line("conflict", detail), ""));
            assert_eq!(merges(&fake), 0);
        }
    }

    #[test]
    fn merge_unknown_rereads() {
        let mut fake = merge_fake(
            vec![
                mergeability("UNKNOWN", "UNKNOWN"),
                mergeability("MERGEABLE", "CLEAN"),
            ],
            ok(""),
        );
        assert_eq!(
            run_merge(&mut fake),
            got(0, &line("merged", "head a1a1a1a1a1a1"), "")
        );
        assert_eq!(
            (fake.count("graphql"), fake.now, merges(&fake)),
            (2, 5.0, 1)
        );
    }

    #[test]
    fn merge_unknown_exits_3() {
        let mut fake = merge_fake(vec![mergeability("MERGEABLE", "UNKNOWN")], ok(""));
        let detail = "GitHub had not computed mergeability after 30 seconds";
        assert_eq!(run_merge(&mut fake), got(3, &line("unknown", detail), ""));
        assert_eq!(
            (fake.count("graphql"), fake.now, merges(&fake)),
            (7, 30.0, 0)
        );
    }

    #[test]
    fn merge_behind_exits_3() {
        let mut fake = merge_fake(vec![mergeability("MERGEABLE", "BEHIND")], ok(""));
        let detail = "PR #42 is behind main: run checks again, which updates the branch";
        assert_eq!(run_merge(&mut fake), got(3, &line("behind", detail), ""));
        assert_eq!(merges(&fake), 0);
    }

    #[test]
    fn merge_blocked_exits_1() {
        let mut fake = merge_fake(vec![mergeability("MERGEABLE", "BLOCKED")], ok(""));
        let detail = "GitHub blocks the merge: a required review or check is missing";
        assert_eq!(run_merge(&mut fake), got(1, &line("blocked", detail), ""));
        assert_eq!(merges(&fake), 0);
    }

    #[test]
    fn merge_draft_exits_1() {
        for read in [
            with(mergeability("MERGEABLE", "CLEAN"), "isDraft", true.into()),
            mergeability("MERGEABLE", "DRAFT"),
        ] {
            let mut fake = merge_fake(vec![read], ok(""));
            assert_eq!(
                run_merge(&mut fake),
                got(1, &line("draft", "PR #42 is a draft"), "")
            );
            assert_eq!(merges(&fake), 0);
        }
    }

    #[test]
    fn merge_refused_exits_1() {
        let stderr =
            "GraphQL: Head branch was modified. Review and try the merge again.\nsecond line\n";
        let mut fake = merge_fake(
            vec![mergeability("MERGEABLE", "CLEAN")],
            reply(1, "", stderr),
        );
        let detail = "GraphQL: Head branch was modified. Review and try the merge again.";
        assert_eq!(run_merge(&mut fake), got(1, &line("refused", detail), ""));
    }

    #[test]
    fn merge_refused_without_stderr_names_exit_code() {
        let mut fake = merge_fake(vec![mergeability("MERGEABLE", "CLEAN")], reply(1, "", ""));
        let detail = "gh pr merge exited 1";
        assert_eq!(run_merge(&mut fake), got(1, &line("refused", detail), ""));
    }

    #[test]
    fn merge_queue_delegates() {
        let read = with(
            mergeability("MERGEABLE", "BLOCKED"),
            "isMergeQueueEnabled",
            true.into(),
        );
        let view =
            json!({"headRefOid": MERGE_HEAD, "baseRefName": "main", "id": "PR_node42"}).to_string();
        let entry = json!({"data": {"enqueuePullRequest": {"mergeQueueEntry": {"state": "QUEUED", "position": 1}}}});
        let mut fake = merge_fake(vec![read], ok(""))
            .on(
                &["api", "graphql", "-f", PR_ENQUEUE_MUTATION],
                vec![ok(&entry.to_string())],
            )
            .on(&["pr", "view"], vec![ok(&view)]);
        assert_eq!(
            run_merge(&mut fake),
            got(0, &line("enqueued", "position 1"), "")
        );
        assert_eq!(merges(&fake), 0);
    }

    #[test]
    fn merge_unchecked_missing_exits_3() {
        let mut fake = merge_fake(vec![mergeability("MERGEABLE", "CLEAN")], ok(""));
        let detail = "PR #42 has no head that checks passed on the board: run checks again";
        assert_eq!(
            run_merge_checked(&mut fake, None),
            got(3, &line("unchecked", detail), "")
        );
        assert_eq!(merges(&fake), 0);
    }

    #[test]
    fn merge_unchecked_moved_exits_3() {
        let read = with(
            mergeability("MERGEABLE", "CLEAN"),
            "headRefOid",
            MOVED_HEAD.into(),
        );
        let mut fake = merge_fake(vec![read], ok(""));
        let detail =
            "PR #42 head b2b2b2b2b2b2 is not the head checks passed, a1a1a1a1a1a1: run checks again";
        assert_eq!(run_merge(&mut fake), got(3, &line("unchecked", detail), ""));
        assert_eq!(merges(&fake), 0);
    }

    #[test]
    fn merge_queue_moved_head_exits_3() {
        let read = with(
            mergeability("MERGEABLE", "BLOCKED"),
            "isMergeQueueEnabled",
            true.into(),
        );
        let view =
            json!({"headRefOid": MOVED_HEAD, "baseRefName": "main", "id": "PR_node42"}).to_string();
        let mut fake = merge_fake(vec![read], ok("")).on(&["pr", "view"], vec![ok(&view)]);
        let detail =
            "PR #42 head b2b2b2b2b2b2 is not the head checks passed, a1a1a1a1a1a1: run checks again";
        assert_eq!(run_merge(&mut fake), got(3, &line("unchecked", detail), ""));
        assert_eq!(
            (fake.count("graphql"), merges(&fake), fake.calls.len()),
            (1, 0, 2)
        );
    }

    #[test]
    fn merge_not_open_exits_4() {
        let read = with(mergeability("UNKNOWN", "UNKNOWN"), "state", "MERGED".into());
        let mut fake = merge_fake(vec![read], ok(""));
        let err = format!("factory.py: {PR} is MERGED, not OPEN\n");
        assert_eq!(run_merge(&mut fake), got(4, "", &err));
        assert_eq!(
            (fake.count("graphql"), fake.now, merges(&fake)),
            (1, 0.0, 0)
        );
    }

    #[test]
    fn merge_stacked_exits_4() {
        let read = with(
            mergeability("MERGEABLE", "CLEAN"),
            "baseRefName",
            "factory/041-parent".into(),
        );
        let mut fake = merge_fake(vec![read], ok(""));
        let err =
            "factory.py: PR #42 targets factory/041-parent, not main: it waits for its parent\n";
        assert_eq!(run_merge(&mut fake), got(4, "", err));
        assert_eq!(merges(&fake), 0);
    }

    #[test]
    fn merge_gh_missing_or_timeout_exits_5() {
        let clean = || ok(&mergeability("MERGEABLE", "CLEAN").to_string());
        let late = format!(
            "gh pr merge timed out after 60 seconds: read gh pr view {PR} --json state before merging again"
        );
        for (read, merge, why) in [
            (None, ok(""), "gh is not installed"),
            (
                timeout(),
                ok(""),
                "gh api graphql timed out after 60 seconds",
            ),
            (ok("null"), ok(""), "gh api graphql printed no JSON object"),
            (clean(), None, "gh is not installed"),
            (clean(), timeout(), late.as_str()),
        ] {
            let mut fake = Fake::default()
                .on(&["api", "graphql"], vec![read])
                .on(&["pr", "merge"], vec![merge]);
            assert_eq!(
                run_merge(&mut fake),
                got(5, "", &format!("factory.py: {why}\n"))
            );
        }
    }
}
