//! The review and pipeline commands: pipeline, documents, verdict, escalation, repeats, review-plan,
//! review-floor, pre-review, grounding, batch, stack, lesson, agent, trial, experiment and routes.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::cli::{not_ported, Call};
use crate::shared::board::{dump_board, escalated, lead_of, load_board, patch_job, read_board};
use crate::shared::conf::{fm_get, frontmatter, read_conf};
use crate::shared::consts::{
    tag_lookalike, BABYSIT_SENTENCE, BATCH_REF, JOB_ID, JOB_REL, NOTE_INVISIBLE, NOTE_LINK,
    NOTE_PHRASES, NOTE_SECRETS, NOTE_TAG, PIPELINE_GATES, PIPELINE_STAGES, PREFERENCES_SENTENCE,
    REVIEW_ROUTE_KEYS, ROUTE_EFFORTS, ROUTE_MODEL, SLUG,
};
use crate::shared::exit::{fail, Res};
use crate::shared::fs::{is_link, mkdir_private, read_text, resolve, write_atomic};
use crate::shared::home::factory_home;
use crate::shared::http::{access_message, api, cas};
use crate::shared::proc::capture;
use crate::shared::project::{project_files, r2_target, safe_job_folder};
use crate::shared::py::{
    is_space, os_error, printable_line, repr, round_half_even, round_int, splitlines, strip,
    universal_newlines,
};
use crate::shared::pyjson::{dumps, loads, DEFAULT, INDENT2, NO_ASCII};
use crate::shared::pyre::{translate, Flags, Pattern};
use crate::shared::time::{format_stamp, now, parse_stamp, unix_now};
use crate::shared::unicode::nfkc;

/// What a command prints, held until the handler flushes it.
#[derive(Default)]
struct Out {
    out: String,
    err: String,
}

impl Out {
    fn flush(self) {
        print!("{}", self.out);
        eprint!("{}", self.err);
    }
}

/// Run an inner command into a fresh `Out`, flush it, and return its result.
fn flushed(inner: impl FnOnce(&mut Out) -> Res<()>) -> Res<()> {
    let mut o = Out::default();
    let result = inner(&mut o);
    o.flush();
    result
}

#[cfg(test)]
thread_local! {
    static FETCH_URL: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    static TEMP_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static CHECK_TIMEOUT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// `PROJECT_FETCH_URL.format(repo=repo)`.
#[allow(dead_code)]
fn project_fetch_url(repo: &str) -> String {
    #[cfg(test)]
    if let Some(url) = FETCH_URL.with(|u| u.borrow().clone()) {
        return url.replace("{repo}", repo);
    }
    format!("https://github.com/{repo}.git")
}

/// `tempfile.gettempdir()`.
#[allow(dead_code)]
fn temp_root() -> PathBuf {
    #[cfg(test)]
    if let Some(root) = TEMP_ROOT.with(|r| r.borrow().clone()) {
        return root;
    }
    if cfg!(windows) {
        return std::env::temp_dir();
    }
    ["TMPDIR", "TEMP", "TMP"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `PRE_REVIEW_TIMEOUT`, in seconds.
#[allow(dead_code)]
fn check_timeout() -> u64 {
    #[cfg(test)]
    if let Some(seconds) = CHECK_TIMEOUT.with(std::cell::Cell::get) {
        return seconds;
    }
    1800
}

/// `fnmatch.fnmatchcase`: `*` matches any run, slashes included.
#[allow(dead_code)]
fn fnmatch(name: &str, pat: &str) -> bool {
    let name: Vec<char> = name.chars().collect();
    let pat: Vec<char> = pat.chars().collect();
    // One parsed pattern item: a literal, `?`, `*`, or a set with its negation.
    enum Item {
        Char(char),
        Any,
        Star,
        Set(bool, Vec<(char, char)>),
    }
    let mut items = Vec::new();
    let mut i = 0;
    while i < pat.len() {
        let c = pat[i];
        i += 1;
        match c {
            '*' => {
                if !matches!(items.last(), Some(Item::Star)) {
                    items.push(Item::Star);
                }
            }
            '?' => items.push(Item::Any),
            '[' => {
                let mut j = i;
                if j < pat.len() && pat[j] == '!' {
                    j += 1;
                }
                if j < pat.len() && pat[j] == ']' {
                    j += 1;
                }
                while j < pat.len() && pat[j] != ']' {
                    j += 1;
                }
                if j >= pat.len() {
                    items.push(Item::Char('['));
                    continue;
                }
                let mut body = &pat[i..j];
                i = j + 1;
                let negate = body.first() == Some(&'!');
                if negate {
                    body = &body[1..];
                }
                let mut ranges = Vec::new();
                let mut k = 0;
                while k < body.len() {
                    if k + 2 < body.len() && body[k + 1] == '-' {
                        if body[k] <= body[k + 2] {
                            ranges.push((body[k], body[k + 2]));
                        }
                        k += 3;
                    } else {
                        ranges.push((body[k], body[k]));
                        k += 1;
                    }
                }
                items.push(Item::Set(negate, ranges));
            }
            c => items.push(Item::Char(c)),
        }
    }
    fn matches(items: &[Item], name: &[char]) -> bool {
        match items.first() {
            None => name.is_empty(),
            Some(Item::Star) => (0..=name.len()).any(|k| matches(&items[1..], &name[k..])),
            Some(item) => {
                let Some(&c) = name.first() else {
                    return false;
                };
                let ok = match item {
                    Item::Char(p) => *p == c,
                    Item::Any => true,
                    Item::Set(negate, ranges) => {
                        ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c)) != *negate
                    }
                    Item::Star => unreachable!(),
                };
                ok && matches(&items[1..], &name[1..])
            }
        }
    }
    matches(&items, &name)
}

/// `statistics.median`.
#[allow(dead_code)]
fn median(v: &mut Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Arm "b" when the job id's stable hash bucket falls under percent, else "a".
#[allow(dead_code)]
fn experiment_arm(id: &str, percent: u32) -> &'static str {
    let digest = Sha256::digest(id.as_bytes());
    let bucket = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 100;
    if bucket < percent {
        "b"
    } else {
        "a"
    }
}

/// Python truthiness of a JSON value that may be missing.
#[allow(dead_code)]
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
    }
}

fn json_error(e: serde_json::Error) -> crate::shared::exit::Exit {
    fail(format!("the Worker sent JSON that does not parse: {e}"), 1)
}

/// `type(v) is int`.
fn py_int(v: Option<&Value>) -> Option<i64> {
    v.filter(|v| v.is_i64() || v.is_u64())
        .and_then(Value::as_i64)
}

// ---- admission checks ----

/// Whether text holds an operator-preferences tag as written or once NFKC and the look-alike fold apply.
fn has_envelope_tag(text: &str) -> bool {
    let folded: String = nfkc(text).chars().map(tag_lookalike).collect();
    NOTE_TAG.search(text).is_some() || NOTE_TAG.search(&folded).is_some()
}

/// The reason for the first admission rule (3-7) text breaks, or None.
fn check_untrusted(text: &str) -> Option<String> {
    if NOTE_INVISIBLE.search(text).is_some() {
        return Some("contains control, invisible or bidirectional characters".into());
    }
    if has_envelope_tag(text) {
        return Some("contains the reserved envelope tag".into());
    }
    if NOTE_LINK.search(text).is_some() {
        return Some("links are not allowed".into());
    }
    if NOTE_SECRETS.iter().any(|p| p.search(text).is_some()) {
        return Some("looks like a secret".into());
    }
    for (rule, p) in &NOTE_PHRASES {
        if p.search(text).is_some() {
            return Some(format!(
                "reads like an instruction to override factory rules ({rule})"
            ));
        }
    }
    None
}

/// The first rule `check_text` finds broken in value, or None.
#[allow(dead_code)]
fn check_text(value: Option<&str>, lo: usize, hi: usize, multiline: bool) -> Option<String> {
    let Some(value) = value.filter(|v| (lo..=hi).contains(&v.chars().count())) else {
        return Some(format!("must be a string of {lo}-{hi} characters"));
    };
    if ["```", "~~~", "://"].iter().any(|s| value.contains(s)) {
        return Some("must not hold a code fence or a link".into());
    }
    if value.chars().any(|c| {
        ((c as u32) < 32 || (127..160).contains(&(c as u32)) || c == '\u{2028}' || c == '\u{2029}')
            && !(multiline && c == '\n')
    }) {
        let tail = if multiline { "" } else { " or line breaks" };
        return Some(format!("must not hold control characters{tail}"));
    }
    check_untrusted(value)
}

/// The reason for the first rule (1, 3, 4, 6) a project document breaks, or None.
fn check_document(text: Option<&str>) -> Option<String> {
    let Some(text) = text else {
        return Some("must be text".into());
    };
    if NOTE_INVISIBLE
        .search(&text.replace(['\t', '\r'], ""))
        .is_some()
    {
        return Some("contains control, invisible or bidirectional characters".into());
    }
    if has_envelope_tag(text) {
        return Some("contains the reserved envelope tag".into());
    }
    if NOTE_SECRETS.iter().any(|p| p.search(text).is_some()) {
        return Some("looks like a secret".into());
    }
    None
}

/// The first admission rule (1-5) a stage note breaks, or None.
fn check_note(text: &Value) -> Option<String> {
    let Some(text) = text.as_str() else {
        return Some("must be text".into());
    };
    if text.chars().count() > 2000 {
        return Some("over 2000 characters".into());
    }
    if NOTE_INVISIBLE.search(text).is_some() {
        return Some("contains control, invisible or bidirectional characters".into());
    }
    if has_envelope_tag(text) {
        return Some("contains the reserved envelope tag".into());
    }
    if NOTE_LINK.search(text).is_some() {
        return Some("links are not allowed".into());
    }
    None
}

// ---- pipeline and documents ----

/// The fields agents run under, with each stage's notes wrapped as operator-preferences.
fn pipeline_view(slug: &str, data: &Value) -> Res<Value> {
    let malformed = |field: &str| fail(format!("malformed pipeline view for {slug}: {field}"), 5);
    let Some(map) = data.as_object() else {
        return Err(malformed("body"));
    };
    if map.get("project").and_then(Value::as_str) != Some(slug) {
        return Err(malformed("project"));
    }
    let merge = map.get("merge").and_then(Value::as_str);
    if !matches!(merge, Some("auto" | "human")) {
        return Err(malformed("merge"));
    }
    let rounds = py_int(map.get("review_rounds"));
    if !rounds.is_some_and(|r| (1..=5).contains(&r)) {
        return Err(malformed("review_rounds"));
    }
    if map.get("reviewers") != Some(&json!(["security", "ux", "ui", "code"])) {
        return Err(malformed("reviewers"));
    }
    let gates = map.get("gates").cloned().unwrap_or(Value::Null);
    let gates_ok = gates.as_array().is_some_and(|list| {
        let kept: Vec<Value> = PIPELINE_GATES
            .iter()
            .filter(|g| list.iter().any(|x| x.as_str() == Some(g)))
            .map(|g| Value::from(*g))
            .collect();
        *list == kept
    });
    if !gates_ok {
        return Err(malformed("gates"));
    }
    let versions = map.get("versions").cloned().unwrap_or(Value::Null);
    let versions_ok = versions.as_object().is_some_and(|v| {
        v.len() == 2
            && v.contains_key("global")
            && v.contains_key("project")
            && v.values().all(|x| py_int(Some(x)).is_some_and(|n| n >= 0))
    });
    if !versions_ok {
        return Err(malformed("versions"));
    }
    let notes = map.get("stage_notes");
    let notes_ok = notes.and_then(Value::as_object).is_some_and(|n| {
        n.keys().all(|k| PIPELINE_STAGES.contains(&k.as_str()))
            && n.values().all(|list| {
                list.as_array().is_some_and(|l| {
                    !l.is_empty()
                        && l.iter().all(|x| {
                            x.as_object().is_some_and(|o| {
                                o.len() == 2 && o.contains_key("source") && o.contains_key("text")
                            })
                        })
                })
            })
    });
    if !notes_ok {
        return Err(malformed("stage_notes"));
    }
    let notes = notes.unwrap().as_object().unwrap();
    let mut prompts = Map::new();
    for stage in PIPELINE_STAGES {
        let mut envelopes = Vec::new();
        for note in notes
            .get(*stage)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let source = &note["source"];
            let parsed = source
                .as_str()
                .and_then(|s| s.strip_prefix("pipeline:"))
                .and_then(|s| s.split_once('@'))
                .filter(|(who, n)| {
                    (*who == "global" || *who == slug)
                        && (*n == "0"
                            || (n.starts_with(|c: char| ('1'..='9').contains(&c))
                                && n.bytes().all(|b| b.is_ascii_digit())))
                });
            let valid = parsed.is_some_and(|(who, n)| {
                let key = if who == "global" { "global" } else { "project" };
                n.parse::<i64>().ok() == py_int(versions.get(key))
            });
            if !valid {
                return Err(malformed("source"));
            }
            let source = source.as_str().unwrap();
            if let Some(reason) = check_note(&note["text"]) {
                return Err(fail(
                    format!("pipeline note {source} ({stage}) {reason}; save it again from the dashboard"),
                    5,
                ));
            }
            envelopes.push(format!(
                "<operator-preferences source=\"{source}\" stage=\"{stage}\" trust=\"data\">\n{}\n</operator-preferences>",
                note["text"].as_str().unwrap()
            ));
        }
        if !envelopes.is_empty() {
            let sentence = if *stage == "babysit" {
                BABYSIT_SENTENCE
            } else {
                PREFERENCES_SENTENCE
            };
            prompts.insert(
                stage.to_string(),
                Value::from(format!("{sentence}\n\n{}", envelopes.join("\n\n"))),
            );
        }
    }
    Ok(
        json!({"project": slug, "merge": merge, "review_rounds": rounds, "gates": gates,
              "versions": versions, "prompts": prompts}),
    )
}

/// The project's effective pipeline configuration, as `pipeline_view` returns it.
fn fetch_pipeline(home: &Path, slug: &str) -> Res<Value> {
    let r = api(
        home,
        "GET",
        &format!("/api/pipeline/effective/{slug}"),
        None,
        &[200, 404, 409],
        &[],
    )?;
    if r.status == 404 {
        return Err(fail(
            format!(
                "no pipeline view for {slug}: register the project, or deploy the current Worker"
            ),
            4,
        ));
    }
    if r.status == 409 {
        return Err(fail(
            format!("pipeline configuration for {slug} no longer validates; save it again from the dashboard"),
            5,
        ));
    }
    let data = loads(&r.text()?)
        .map_err(|_| fail(format!("malformed pipeline view for {slug}: body"), 5))?;
    pipeline_view(slug, &data)
}

fn pipeline(home: &Path, slug: &str, flag: Option<&str>, o: &mut Out) -> Res<()> {
    let view = fetch_pipeline(home, slug)?;
    if flag.is_some() {
        writeln!(
            o.out,
            "{}",
            dumps(&json!({"pipeline": view["versions"]}), DEFAULT)
        )
        .unwrap();
    } else {
        writeln!(o.out, "{}", dumps(&view, INDENT2)).unwrap();
    }
    Ok(())
}

/// Print the project's effective pipeline configuration, or with --board only its board patch.
pub fn pipeline_cmd(call: &Call) -> Res<()> {
    let slug = call.args[0].as_str();
    let flag = call.args.get(1).map(String::as_str);
    if !matches!(flag, None | Some("--board")) || !SLUG.is_full(slug) {
        return Err(fail("usage: pipeline <project-slug> [--board]", 2));
    }
    let home = factory_home()?;
    flushed(|o| pipeline(&home, slug, flag, o))
}

const DOCUMENT_NAMES: [&str; 3] = ["notes.md", "decisions.md", "runbook.md"];

fn documents(home: &Path, slug: &str, o: &mut Out) -> Res<()> {
    let path = project_files(home, slug)?
        .0
        .parent()
        .unwrap()
        .join(format!("{slug}.documents.md"));
    if is_link(&path).map_err(|e| fail(os_error(&e, Some(&path)), 1))? {
        return Err(fail("project paths must not be symlinks or junctions", 6));
    }
    let mut envelopes = Vec::new();
    for name in DOCUMENT_NAMES {
        let r = api(
            home,
            "GET",
            &format!("/api/project-documents/{slug}/{name}"),
            None,
            &[200, 404],
            &[],
        )?;
        if r.status == 404 {
            continue;
        }
        let text = String::from_utf8(r.body).ok();
        if let Some(problem) = check_document(text.as_deref()) {
            return Err(fail(
                format!("project document {slug}/{name}: {problem}; fix it in Project settings"),
                5,
            ));
        }
        let text = text.unwrap();
        if !strip(&text).is_empty() {
            let nl = if text.ends_with('\n') { "" } else { "\n" };
            envelopes.push(format!(
                "<operator-preferences source=\"document:{slug}/{name}\" trust=\"data\">\n{text}{nl}</operator-preferences>"
            ));
        }
    }
    let data = if envelopes.is_empty() {
        "This project has no notes.md, decisions.md or runbook.md.\n".to_string()
    } else {
        format!("{PREFERENCES_SENTENCE}\n\n{}\n", envelopes.join("\n\n"))
    };
    write_atomic(&path, data.as_bytes()).map_err(|e| fail(os_error(&e, Some(&path)), 1))?;
    writeln!(o.out, "{}", path.display()).unwrap();
    Ok(())
}

/// Write the project's reference documents, wrapped as data, to projects/<slug>.documents.md.
pub fn documents_cmd(call: &Call) -> Res<()> {
    let slug = call.args[0].as_str();
    if !SLUG.is_full(slug) {
        return Err(fail("usage: documents <project-slug>", 2));
    }
    let home = factory_home()?;
    flushed(|o| documents(&home, slug, o))
}

// ---- routes and experiment ----

/// Python's `str.split()`: runs of whitespace separate, ends are dropped.
fn py_split(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|w| !w.is_empty()).collect()
}

/// A frontmatter block as a dict: each key once, in first place, with its last value.
fn fm_dict(meta: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (k, v) in meta {
        match out.iter_mut().find(|(key, _)| *key == k) {
            Some(slot) => slot.1 = v,
            None => out.push((k, v)),
        }
    }
    out
}

/// routes.md's frontmatter, or nothing when the file is missing.
fn routes_meta(path: &Path) -> Res<Vec<(String, String)>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let meta = frontmatter(path)
        .map_err(|e| fail(os_error(&e, Some(path)), 1))?
        .0;
    Ok(fm_dict(meta))
}

type Route = (String, String, String, Option<String>);

/// (provider, model, effort or None) from "<provider> <model> [<effort>]", or None when it is malformed.
fn parse_route(value: &str) -> Option<(String, String, Option<String>)> {
    let parts = py_split(value);
    if !(2..=3).contains(&parts.len())
        || !SLUG.is_full(parts[0])
        || !ROUTE_MODEL.is_full(parts[1])
        || parts.len() == 3 && !ROUTE_EFFORTS.contains(&parts[2])
    {
        return None;
    }
    Some((
        parts[0].to_string(),
        parts[1].to_string(),
        parts.get(2).map(|e| e.to_string()),
    ))
}

/// [(name, provider, model, effort or None)] from this host's routes.md, in preference order.
fn read_routes(home: &Path) -> Res<Vec<Route>> {
    let path = home.join("routes.md");
    let meta = routes_meta(&path)?;
    if meta.is_empty() {
        return Err(fail(
            format!(
                "no routes in {}: list this host's dispatch routes, primary first, as <name>: <provider> <model> [<effort>]",
                path.display()
            ),
            2,
        ));
    }
    let mut routes = Vec::new();
    for (name, value) in meta {
        if name == "experiment" || REVIEW_ROUTE_KEYS.contains(&name.as_str()) {
            continue;
        }
        let route = parse_route(&value);
        let Some((provider, model, effort)) = route.filter(|_| SLUG.is_full(&name)) else {
            return Err(fail(
                format!(
                    "bad route {} in {}: use <name>: <provider> <model> [<effort>]",
                    repr(&name),
                    path.display()
                ),
                2,
            ));
        };
        routes.push((name, provider, model, effort));
    }
    Ok(routes)
}

/// {"low": route or None, "medium": route or None} from routes.md's optional review lines.
fn read_review_routes(home: &Path) -> Res<Value> {
    let path = home.join("routes.md");
    let meta = routes_meta(&path)?;
    let mut routes = Map::new();
    for key in REVIEW_ROUTE_KEYS {
        let tier = key.split_once('-').unwrap().1;
        let Some(value) = fm_get(&meta, key) else {
            routes.insert(tier.into(), Value::Null);
            continue;
        };
        let Some((provider, model, effort)) = parse_route(value) else {
            return Err(fail(
                format!(
                    "bad review route {key} in {}: use {key}: <provider> <model> [<effort>]",
                    path.display()
                ),
                2,
            ));
        };
        routes.insert(
            tier.into(),
            json!({"name": key, "provider": provider, "model": model, "effort": effort}),
        );
    }
    Ok(Value::Object(routes))
}

struct Experiment {
    name: String,
    provider: String,
    model: String,
    effort: Option<String>,
    percent: u32,
}

/// None, or the experiment from routes.md's optional experiment line.
fn read_experiment(home: &Path, route_names: &[String]) -> Res<Option<Experiment>> {
    let path = home.join("routes.md");
    let meta = routes_meta(&path)?;
    let Some(line) = fm_get(&meta, "experiment") else {
        return Ok(None);
    };
    let bad = || {
        fail(
            format!(
                "bad experiment in {}: use experiment: <name> <provider> <model> [<effort>] <percent>",
                path.display()
            ),
            2,
        )
    };
    let parts = py_split(line);
    if !(4..=5).contains(&parts.len()) {
        return Err(bad());
    }
    let (name, provider, model) = (parts[0], parts[1], parts[2]);
    let effort = (parts.len() == 5).then(|| parts[3]);
    let percent = parts[parts.len() - 1];
    let percent_ok = (1..=3).contains(&percent.len())
        && percent.bytes().all(|b| b.is_ascii_digit())
        && percent.parse::<u32>().unwrap() <= 100;
    if !SLUG.is_full(name)
        || !SLUG.is_full(provider)
        || !ROUTE_MODEL.is_full(model)
        || route_names.iter().any(|r| r == name)
        || effort.is_some_and(|e| !ROUTE_EFFORTS.contains(&e))
        || !percent_ok
    {
        return Err(bad());
    }
    Ok(Some(Experiment {
        name: name.into(),
        provider: provider.into(),
        model: model.into(),
        effort: effort.map(String::from),
        percent: percent.parse().unwrap(),
    }))
}

/// {route name: limited until} from this host's route-limits.json.
fn read_limits(home: &Path) -> Res<Vec<(String, String)>> {
    let path = home.join("route-limits.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let corrupt = || {
        fail(
            format!(
                "cannot read {}: delete it to clear every route limit",
                path.display()
            ),
            2,
        )
    };
    let text = read_text(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::InvalidData => corrupt(),
        _ => fail(os_error(&e, Some(&path)), 1),
    })?;
    let limits = loads(&text).map_err(|_| corrupt())?;
    let Some(map) = limits.as_object() else {
        return Err(corrupt());
    };
    let mut out = Vec::new();
    for (k, v) in map {
        let Some(until) = v.as_str() else {
            return Err(corrupt());
        };
        out.push((k.clone(), until.to_string()));
    }
    if out.iter().any(|(_, until)| parse_stamp(until).is_none()) {
        return Err(corrupt());
    }
    Ok(out)
}

/// The end of provider's extends chain in this host's Paseo config.json, else provider itself.
fn provider_runtime(provider: &str) -> Res<String> {
    let base = match std::env::var_os("PASEO_HOME").filter(|v| !v.is_empty()) {
        Some(home) => PathBuf::from(home),
        None => match std::env::home_dir() {
            Some(home) => home.join(".paseo"),
            None => return Ok(provider.into()),
        },
    };
    let path = base.join("config.json");
    if !path.is_file() {
        return Ok(provider.into());
    }
    let unreadable = || {
        fail(
            format!(
                "cannot read {}: routes pick needs it to find the base runtime of {provider}",
                path.display()
            ),
            2,
        )
    };
    let config = read_text(&path)
        .ok()
        .and_then(|t| loads(&t).ok())
        .ok_or_else(unreadable)?;
    let Some(config) = config.as_object() else {
        return Err(unreadable());
    };
    let agents = config.get("agents").cloned().unwrap_or(json!({}));
    let Some(agents) = agents.as_object() else {
        return Err(unreadable());
    };
    let providers = agents.get("providers").cloned().unwrap_or(json!({}));
    let Some(providers) = providers.as_object() else {
        return Err(unreadable());
    };
    let mut seen = vec![provider.to_string()];
    while let Some(entry) = providers
        .get(seen.last().unwrap())
        .and_then(Value::as_object)
        .filter(|p| p.contains_key("extends"))
    {
        let base = entry["extends"]
            .as_str()
            .filter(|b| SLUG.is_full(b) && !seen.iter().any(|s| s == b));
        let Some(base) = base else {
            return Err(fail(
                format!(
                    "bad extends under provider {} in {}: use a provider name, with no loop",
                    repr(seen.last().unwrap()),
                    path.display()
                ),
                2,
            ));
        };
        seen.push(base.to_string());
    }
    Ok(seen.pop().unwrap())
}

/// True when name is the experiment, False when it is a route, else exit 2 naming what is accepted.
fn check_limit_name(home: &Path, name: &str) -> Res<bool> {
    let routes: Vec<String> = read_routes(home)?.into_iter().map(|r| r.0).collect();
    let experiment = read_experiment(home, &routes)?;
    if experiment.as_ref().is_some_and(|e| e.name == name) {
        return Ok(true);
    }
    if routes.iter().any(|r| r == name) {
        return Ok(false);
    }
    let path = home.join("routes.md");
    let what = if experiment.is_some() {
        "route or experiment"
    } else {
        "route"
    };
    Err(fail(
        format!("unknown {what} {}: not in {}", repr(name), path.display()),
        2,
    ))
}

fn write_limits(path: &Path, limits: &[(String, String)]) -> Res<()> {
    let map: Map<String, Value> = limits
        .iter()
        .map(|(k, v)| (k.clone(), Value::from(v.clone())))
        .collect();
    let text = dumps(&Value::Object(map), INDENT2) + "\n";
    write_atomic(path, text.as_bytes()).map_err(|e| fail(os_error(&e, Some(path)), 1))
}

/// Mark one route or the experiment limited for minutes from now.
fn limit_route(home: &Path, name: &str, minutes: i64) -> Res<(String, bool)> {
    let experiment = check_limit_name(home, name)?;
    let stamp = now();
    let mut limits: Vec<(String, String)> = read_limits(home)?
        .into_iter()
        .filter(|(_, until)| *until > stamp)
        .collect();
    let until = format_stamp(unix_now() + minutes * 60);
    match limits.iter_mut().find(|(k, _)| k == name) {
        Some(slot) => slot.1 = until.clone(),
        None => limits.push((name.to_string(), until.clone())),
    }
    write_limits(&home.join("route-limits.json"), &limits)?;
    Ok((until, experiment))
}

static MINUTES: Pattern = Pattern::new("[1-9][0-9]*", Flags::NONE);

fn routes(home: &Path, sub: &str, args: &[String], o: &mut Out) -> Res<()> {
    let path = home.join("route-limits.json");
    let stamp = now();
    if (sub == "pick" || sub == "list") && args.is_empty() {
        let routes = read_routes(home)?;
        let active: Vec<(String, String)> = read_limits(home)?
            .into_iter()
            .filter(|(_, until)| *until > stamp)
            .collect();
        let active_for = |n: &str| {
            active
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, u)| Value::from(u.clone()))
                .unwrap_or(Value::Null)
        };
        if sub == "list" {
            let mut rows: Vec<Value> = Vec::new();
            for (n, p, m, e) in &routes {
                let mut row = json!({"route": n, "provider": p, "model": m});
                if let Some(e) = e {
                    row["effort"] = Value::from(e.clone());
                }
                row["limited_until"] = active_for(n);
                rows.push(row);
            }
            let names: Vec<String> = routes.iter().map(|r| r.0.clone()).collect();
            if let Some(x) = read_experiment(home, &names)? {
                let mut row =
                    json!({"experiment": x.name, "provider": x.provider, "model": x.model});
                if let Some(e) = &x.effort {
                    row["effort"] = Value::from(e.clone());
                }
                row["percent"] = Value::from(x.percent);
                row["limited_until"] = active_for(&x.name);
                rows.push(row);
            }
            writeln!(o.out, "{}", dumps(&Value::Array(rows), INDENT2)).unwrap();
            return Ok(());
        }
        let Some((name, provider, model, effort)) = routes
            .iter()
            .find(|r| !active.iter().any(|(k, _)| *k == r.0))
            .cloned()
        else {
            let listed: Vec<String> = routes
                .iter()
                .map(|r| format!("{} until {}", r.0, active_for(&r.0).as_str().unwrap()))
                .collect();
            return Err(fail(
                format!("no available route: {}", listed.join(", ")),
                3,
            ));
        };
        let runtime = provider_runtime(&provider)?;
        let mut out =
            json!({"route": name, "provider": provider, "runtime": runtime, "model": model});
        if let Some(e) = effort {
            out["effort"] = Value::from(e);
        }
        writeln!(o.out, "{}", dumps(&out, INDENT2)).unwrap();
    } else if sub == "limit" && (1..=3).contains(&args.len()) {
        if args.len() > 1
            && (args.len() != 3
                || args[1] != "--minutes"
                || !MINUTES.is_full(&args[2])
                || args[2].parse::<i64>().map_or(true, |n| n > 1440))
        {
            return Err(fail(
                "usage: routes limit <name> [--minutes N], with N from 1 to 1440",
                2,
            ));
        }
        let name = &args[0];
        let minutes = if args.len() == 3 {
            args[2].parse().unwrap()
        } else {
            60
        };
        let (until, experiment) = limit_route(home, name, minutes)?;
        let key = if experiment { "experiment" } else { "route" };
        let mut out = Map::new();
        out.insert(key.into(), Value::from(name.clone()));
        out.insert("limited_until".into(), Value::from(until));
        writeln!(o.out, "{}", dumps(&Value::Object(out), INDENT2)).unwrap();
    } else if sub == "clear" && args.len() == 1 {
        let name = &args[0];
        check_limit_name(home, name)?;
        let mut limits = read_limits(home)?;
        if let Some(i) = limits.iter().position(|(k, _)| k == name) {
            limits.remove(i);
            write_limits(&path, &limits)?;
        }
    } else {
        return Err(fail(
            "usage: routes pick|list|limit <name> [--minutes N]|clear <name>",
            2,
        ));
    }
    Ok(())
}

/// This host's dispatch routes. Local only: no board, network or subprocess call.
pub fn routes_cmd(call: &Call) -> Res<()> {
    let home = factory_home()?;
    flushed(|o| routes(&home, &call.args[0], &call.args[1..], o))
}

const REVIEW_PAST_STAGES: [&str; 4] = ["approve", "pr-open", "merged", "closed"];
const WINDOW_SECONDS: i64 = 7 * 86400;
const MIN_ARM_FINISHED: usize = 5;
const DECISION_VERDICTS: [&str; 3] = ["APPROVE", "ESCALATE", "CLOSED:"];
const TRACER_REVIEWERS: [&str; 4] = ["security", "ux", "ui", "code"];
const USAGE_TOKENS: [&str; 4] = ["input", "output", "cache_write", "cache_read"];

/// A board timestamp as seconds, or None when it is missing or unparseable.
fn stamp_of(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_str).and_then(parse_stamp)
}

/// A number that remembers whether Python holds it as an int.
#[derive(Clone, Copy)]
struct Num(f64, bool);

impl Num {
    fn value(self) -> Value {
        if self.1 {
            json!(self.0 as i64)
        } else {
            json!(self.0)
        }
    }
}

/// {n, median, mad} of a list of ints; median and mad are None when it is empty.
fn spread(values: &[i64]) -> (usize, Option<(Num, Num)>) {
    if values.is_empty() {
        return (0, None);
    }
    let odd = values.len() % 2 == 1;
    let mut v: Vec<f64> = values.iter().map(|x| *x as f64).collect();
    let m = median(&mut v);
    let mut dev: Vec<f64> = values.iter().map(|x| (*x as f64 - m).abs()).collect();
    let mad = median(&mut dev);
    let round = |x: f64| {
        if odd {
            x
        } else {
            round_half_even(x, 1)
        }
    };
    (
        values.len(),
        Some((Num(round(m), odd), Num(round(mad), odd))),
    )
}

fn spread_value(s: &(usize, Option<(Num, Num)>)) -> Value {
    match s.1 {
        None => json!({"n": 0, "median": null, "mad": null}),
        Some((m, mad)) => json!({"n": s.0, "median": m.value(), "mad": mad.value()}),
    }
}

/// A job's recorded token total, or None when no device entry has a models dict.
fn job_tokens(j: &Value) -> Option<i64> {
    let tokens = j.get("tokens")?.as_object()?;
    let (mut total, mut found) = (0i64, false);
    for entry in tokens.values() {
        let Some(models) = entry.get("models").and_then(Value::as_object) else {
            continue;
        };
        found = true;
        for counts in models.values().filter_map(Value::as_object) {
            total += USAGE_TOKENS
                .iter()
                .filter_map(|k| py_int(counts.get(*k)))
                .sum::<i64>();
        }
    }
    found.then_some(total)
}

fn str_of<'a>(j: &'a Value, key: &str) -> Option<&'a str> {
    j.get(key).and_then(Value::as_str)
}

/// `len(x or [])` for a list value.
fn list_of(j: &Value, key: &str) -> Vec<Value> {
    j.get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// One arm's outcome metrics from the board and this host's local job folders.
fn arm_metrics(arm_jobs: &Map<String, Value>, follow_ups: Value, jobs_root: &Path) -> Value {
    let count_stage = |s: &str| {
        arm_jobs
            .values()
            .filter(|j| str_of(j, "stage") == Some(s))
            .count()
    };
    let means: Vec<i64> = arm_jobs
        .values()
        .filter(|j| str_of(j, "stage").is_some_and(|s| REVIEW_PAST_STAGES.contains(&s)))
        .map(|j| {
            let round = if truthy(j.get("round")) {
                j["round"].as_i64().unwrap_or(0)
            } else {
                0
            };
            round.max(list_of(j, "rounds").len() as i64)
        })
        .collect();
    let (mut missing_round_review, mut missing_decision, mut pr_before_approve) =
        (Vec::new(), Vec::new(), Vec::new());
    let mut coverage_unknown = 0;
    let mut ids: Vec<&String> = arm_jobs.keys().collect();
    ids.sort();
    for job_id in ids {
        let j = &arm_jobs[job_id];
        let project = str_of(j, "project").filter(|p| SLUG.is_full(p));
        let keys_ok = list_of(j, "rounds")
            .iter()
            .filter_map(Value::as_object)
            .all(|entry| {
                entry
                    .keys()
                    .all(|k| TRACER_REVIEWERS.contains(&k.as_str()) || k == "unverified")
            });
        let Some(project) =
            project.filter(|_| JOB_ID.is_full(job_id) && !job_id.contains("..") && keys_ok)
        else {
            coverage_unknown += 1;
            continue;
        };
        let folder = jobs_root.join(project).join(job_id);
        if !folder.is_dir() || !folder.join("spec.md").is_file() {
            coverage_unknown += 1;
            continue;
        }
        let mut missed = false;
        for (i, entry) in list_of(j, "rounds").iter().enumerate() {
            let Some(entry) = entry.as_object() else {
                continue;
            };
            for (reviewer, verdict) in entry {
                if verdict.as_str().is_some_and(|v| v != "" && v != "pending")
                    && !folder
                        .join(format!("round-{}", i + 1))
                        .join(format!("review-{reviewer}.md"))
                        .is_file()
                {
                    missed = true;
                }
            }
        }
        if missed {
            missing_round_review.push(job_id.clone());
        }
        let stage = str_of(j, "stage");
        if matches!(stage, Some("pr-open" | "merged" | "closed"))
            && !folder.join("decision.md").is_file()
        {
            missing_decision.push(job_id.clone());
        }
        if truthy(j.get("pr")) {
            let decision = folder.join("decision.md");
            if decision.is_file() {
                let Ok(text) = read_text(&decision) else {
                    coverage_unknown += 1;
                    continue;
                };
                let first = splitlines(&text, false)
                    .into_iter()
                    .map(strip)
                    .find(|l| !l.is_empty())
                    .unwrap_or("");
                if !DECISION_VERDICTS.iter().any(|v| first.starts_with(v)) {
                    pr_before_approve.push(job_id.clone());
                }
            }
        }
    }
    let counts: i64 = arm_jobs
        .values()
        .filter_map(|j| py_int(j.get("resumes").and_then(|r| r.as_object()?.get("count"))))
        .sum();
    let finished: Vec<&Value> = arm_jobs
        .values()
        .filter(|j| matches!(str_of(j, "stage"), Some("merged" | "closed")))
        .collect();
    let tokens: Vec<i64> = finished.iter().filter_map(|j| job_tokens(j)).collect();
    let mean = if means.is_empty() {
        json!(0)
    } else {
        json!(round_half_even(
            means.iter().sum::<i64>() as f64 / means.len() as f64,
            1
        ))
    };
    let changes: usize = arm_jobs
        .values()
        .flat_map(|j| list_of(j, "rounds"))
        .filter_map(|e| e.as_object().cloned())
        .map(|e| e.values().filter(|v| v.as_str() == Some("CHANGES")).count())
        .sum();
    json!({"jobs": arm_jobs.len(), "merged": count_stage("merged"), "closed": count_stage("closed"),
           "needs_human": count_stage("needs-human"),
           "escalated": arm_jobs.values().filter(|j| escalated(j)).count(),
           "mean_review_rounds": mean, "changes_verdicts": changes, "stuck_resumes": counts,
           "follow_ups_7d": follow_ups,
           "skipped_steps": {"missing_round_review": missing_round_review,
                             "missing_decision": missing_decision, "pr_before_approve": pr_before_approve},
           "coverage_unknown": coverage_unknown, "finished": finished.len(),
           "rounds": spread_value(&spread(&means)), "tokens": spread_value(&spread(&tokens))})
}

/// The `spread` of a metric as its arm_metrics value holds it.
fn spread_of(v: &Value) -> (usize, Option<(Num, Num)>) {
    let num = |x: &Value| Num(x.as_f64().unwrap_or(0.0), x.is_i64() || x.is_u64());
    let n = v["n"].as_u64().unwrap_or(0) as usize;
    if v["median"].is_null() {
        return (n, None);
    }
    (n, Some((num(&v["median"]), num(&v["mad"]))))
}

/// Whether two arm_metrics dicts can be compared, and per metric how b's median reads against a's.
fn compare(a: &Value, b: &Value) -> Value {
    let (fa, fb) = (
        a["finished"].as_u64().unwrap_or(0) as usize,
        b["finished"].as_u64().unwrap_or(0) as usize,
    );
    let ready = fa >= MIN_ARM_FINISHED && fb >= MIN_ARM_FINISHED;
    let mut result = json!({"min_finished": MIN_ARM_FINISHED,
        "verdict": if ready { "compared" } else { "inconclusive" },
        "reason": format!("needs {MIN_ARM_FINISHED} finished jobs per arm: a has {fa}, b has {fb}")});
    for metric in ["rounds", "tokens"] {
        let (sa, sb) = (spread_of(&a[metric]), spread_of(&b[metric]));
        let (Some((ma, da)), Some((mb, db))) = (sa.1, sb.1) else {
            result[metric] = json!({"diff": null, "noise": null, "reads": "inconclusive"});
            continue;
        };
        if !ready || sa.0 < MIN_ARM_FINISHED || sb.0 < MIN_ARM_FINISHED {
            result[metric] = json!({"diff": null, "noise": null, "reads": "inconclusive"});
            continue;
        }
        let both_int = ma.1 && mb.1;
        let diff = Num(
            if both_int {
                mb.0 - ma.0
            } else {
                round_half_even(mb.0 - ma.0, 1)
            },
            both_int,
        );
        let noise = if db.0 > da.0 { db } else { da };
        let reads = if diff.0.abs() <= noise.0 {
            "no difference"
        } else if diff.0 > 0.0 {
            "b higher"
        } else {
            "b lower"
        };
        result[metric] = json!({"diff": diff.value(), "noise": noise.value(), "reads": reads});
    }
    result
}

fn experiment_report(home: &Path, name: &str, o: &mut Out) -> Res<()> {
    let data = read_board(home)?;
    let jobs = data["jobs"].as_array().cloned().unwrap_or_default();
    let mut arm_jobs = [Map::new(), Map::new()];
    let arm_index = |a: &str| match a {
        "a" => Some(0),
        "b" => Some(1),
        _ => None,
    };
    for j in &jobs {
        let Some(id) = str_of(j, "id") else {
            continue;
        };
        let Some(record) = j.get("experiment").and_then(Value::as_object) else {
            continue;
        };
        if record.get("name").and_then(Value::as_str) == Some(name) {
            if let Some(i) = record
                .get("arm")
                .and_then(Value::as_str)
                .and_then(arm_index)
            {
                arm_jobs[i].insert(id.to_string(), j.clone());
            }
        }
    }
    let mut follow = [
        (Vec::<String>::new(), Vec::<String>::new(), 0usize),
        (Vec::new(), Vec::new(), 0),
    ];
    for j in &jobs {
        let (Some(refs), Some(id)) = (j.get("refs").and_then(Value::as_array), str_of(j, "id"))
        else {
            continue;
        };
        for r in refs {
            let (Some(ref_id), Some(kind)) = (str_of(r, "id"), str_of(r, "kind")) else {
                continue;
            };
            if kind != "rework-of" && kind != "follow-up-of" {
                continue;
            }
            let Some(arm) = (0..2).find(|a| arm_jobs[*a].contains_key(ref_id)) else {
                continue;
            };
            let original = &arm_jobs[arm][ref_id];
            let anchor = stamp_of(original.get("done")).or(stamp_of(original.get("started")));
            let age = stamp_of(j.get("started")).or(stamp_of(j.get("updated")));
            match (anchor, age) {
                (Some(anchor), Some(age)) => {
                    if age - anchor <= WINDOW_SECONDS {
                        if kind == "rework-of" {
                            follow[arm].0.push(id.to_string());
                        } else {
                            follow[arm].1.push(id.to_string());
                        }
                    }
                }
                _ => follow[arm].2 += 1,
            }
        }
    }
    let follow_value = |f: &(Vec<String>, Vec<String>, usize)| json!({"rework_of": f.0, "surfaced": f.1, "unknown_window": f.2});
    let root = home.join("jobs");
    let a = arm_metrics(&arm_jobs[0], follow_value(&follow[0]), &root);
    let b = arm_metrics(&arm_jobs[1], follow_value(&follow[1]), &root);
    let comparison = compare(&a, &b);
    let out = json!({"name": name, "a": a, "b": b, "comparison": comparison});
    writeln!(o.out, "{}", dumps(&out, INDENT2)).unwrap();
    Ok(())
}

/// Read-only per-arm outcome report for one owner-model experiment.
pub fn experiment_cmd(call: &Call) -> Res<()> {
    let (sub, name) = (call.args[0].as_str(), call.args[1].as_str());
    if sub != "report" {
        return Err(fail("usage: experiment report <name>", 2));
    }
    if !SLUG.is_full(name) {
        return Err(fail(format!("bad experiment name {}", repr(name)), 2));
    }
    let home = factory_home()?;
    flushed(|o| experiment_report(&home, name, o))
}

// ---- trial ----

const TRIAL_MAX_DAYS: i64 = 28;
const TRIAL_PAUSE_REVERTS: usize = 2;
const TRIAL_KEEP: usize = 20;
const RETRO_EDIT_SCHEMA: &str = "factory.retro-edit/1";
const TRIAL_RESUME_ONLY: &str = "only a signed-in person, or a paired host passing a person's instruction, resumes stage-note trials";

/// A job's arm in one stage-note trial: "a" reads the champion note, "b" the challenger.
fn trial_arm(trial_id: &str, job_id: &str) -> &'static str {
    experiment_arm(&format!("{trial_id}/{job_id}"), 50)
}

/// True for a well-formed trial record.
fn trial_valid(t: &Value) -> bool {
    let Some(map) = t.as_object() else {
        return false;
    };
    let (Some(project), Some(stage)) = (str_of(t, "project"), str_of(t, "stage")) else {
        return false;
    };
    let Some(version) = py_int(map.get("version")) else {
        return false;
    };
    SLUG.is_full(project)
        && PIPELINE_STAGES.contains(&stage)
        && py_int(map.get("champion_version")).is_some_and(|v| v >= 0)
        && str_of(t, "id") == Some(&format!("{project}-{stage}-v{version}"))
        && stamp_of(map.get("opened")).is_some()
}

/// The board's open, well-formed trial for slug's stage note, or None.
#[allow(dead_code)]
fn open_trial_for<'a>(data: &'a Value, slug: &str, stage: &str) -> Option<&'a Value> {
    data.get("trials")?.as_array()?.iter().find(|t| {
        trial_valid(t)
            && str_of(t, "status") == Some("open")
            && str_of(t, "project") == Some(slug)
            && str_of(t, "stage") == Some(stage)
    })
}

/// The trial stage's note in the pipeline version the edit replaced, or "" when absent.
fn trial_champion(home: &Path, trial: &Value) -> Res<String> {
    let version = py_int(trial.get("champion_version")).unwrap_or(0);
    if version == 0 {
        return Ok(String::new());
    }
    let r = api(
        home,
        "GET",
        &format!(
            "/api/pipeline/history/projects/{}/{version}",
            str_of(trial, "project").unwrap_or("")
        ),
        None,
        &[200, 404],
        &[],
    )?;
    if r.status != 200 {
        return Ok(String::new());
    }
    let body = loads(&r.text()?).map_err(json_error)?;
    let stage = str_of(trial, "stage").unwrap_or("");
    Ok(body
        .get("stage_notes")
        .and_then(Value::as_object)
        .and_then(|n| n.get(stage))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string())
}

/// True when slug has TRIAL_PAUSE_REVERTS reverted trials no person resumed.
fn trials_paused(data: &Value, slug: &str) -> bool {
    data.get("trials")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|t| {
            t.is_object()
                && str_of(t, "project") == Some(slug)
                && str_of(t, "status") == Some("reverted")
                && !truthy(t.get("resumed"))
        })
        .count()
        >= TRIAL_PAUSE_REVERTS
}

/// The trial project's jobs started at or after it opened, by arm.
fn trial_jobs(data: &Value, trial: &Value) -> [Map<String, Value>; 2] {
    let opened = stamp_of(trial.get("opened"));
    let mut arms = [Map::new(), Map::new()];
    for j in data["jobs"].as_array().into_iter().flatten() {
        let Some(id) = str_of(j, "id") else {
            continue;
        };
        if j.get("project") != trial.get("project") || j.get("project").is_none() {
            continue;
        }
        let started = stamp_of(j.get("started"));
        if let (Some(opened), Some(started)) = (opened, started) {
            if started >= opened {
                let arm = trial_arm(str_of(trial, "id").unwrap_or(""), id);
                arms[usize::from(arm == "b")].insert(id.to_string(), j.clone());
            }
        }
    }
    arms
}

/// The board's autonomy record with defaults filled in. A malformed record counts as paused.
fn load_autonomy(data: &Value, o: &mut Out) -> Value {
    let default = json!({"paused": true, "projects": [], "self": []});
    let stored = data.get("autonomy").cloned().unwrap_or(json!({}));
    let mut record = default.clone();
    if let Some(stored) = stored.as_object() {
        for (k, v) in stored {
            record[k] = v.clone();
        }
    }
    let lists_ok = ["projects", "self"].iter().all(|k| {
        record[*k]
            .as_array()
            .is_some_and(|l| l.iter().all(Value::is_string))
    });
    if !stored.is_object()
        || !record["paused"].is_boolean()
        || !lists_ok
        || record["projects"].as_array().unwrap().len() > 1
    {
        o.err.push_str("factory.py: the board's autonomy record is malformed, so autonomy counts as paused; run autonomy pause to rewrite it\n");
        return default;
    }
    record
}

fn trial_settle(home: &Path, o: &mut Out) -> Res<()> {
    let data = read_board(home)?;
    if load_autonomy(&data, o)["paused"] != Value::Bool(false) {
        return Err(fail("autonomy is paused; no trial settled", 3));
    }
    let trials: Vec<Value> = data
        .get("trials")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|t| trial_valid(t) && str_of(t, "status") == Some("open"))
        .cloned()
        .collect();
    if trials.is_empty() {
        o.out.push_str("no open trials\n");
        return Ok(());
    }
    let mut updates: Vec<(String, Map<String, Value>)> = Vec::new();
    for t in &trials {
        let (tid, slug, stage) = (
            str_of(t, "id").unwrap(),
            str_of(t, "project").unwrap(),
            str_of(t, "stage").unwrap(),
        );
        let r = api(
            home,
            "GET",
            &format!("/api/pipeline/projects/{slug}"),
            None,
            &[200, 404],
            &[],
        )?;
        let notes = if r.status == 200 {
            loads(&r.text()?).map_err(json_error)?["stage_notes"].clone()
        } else {
            json!({})
        };
        let note = notes.get(stage).cloned().unwrap_or(json!(""));
        if Some(&note) != t.get("challenger") {
            let mut update = Map::new();
            update.insert("status".into(), json!("superseded"));
            update.insert("settled".into(), json!(now()));
            updates.push((tid.to_string(), update));
            writeln!(
                o.out,
                "trial {tid} superseded: the {stage} note changed after the edit"
            )
            .unwrap();
            continue;
        }
        let arms = trial_jobs(&data, t);
        let jobs_root = home.join("jobs");
        let a = arm_metrics(&arms[0], json!({}), &jobs_root);
        let b = arm_metrics(&arms[1], json!({}), &jobs_root);
        let result = compare(&a, &b);
        let reads =
            json!({"rounds": result["rounds"]["reads"], "tokens": result["tokens"]["reads"]});
        let said = format!(
            "rounds {}, tokens {}",
            reads["rounds"].as_str().unwrap(),
            reads["tokens"].as_str().unwrap()
        );
        let expired = unix_now() - stamp_of(t.get("opened")).unwrap() >= TRIAL_MAX_DAYS * 86400;
        let sorted_ids = |m: &Map<String, Value>| {
            let mut ids: Vec<String> = m.keys().cloned().collect();
            ids.sort();
            ids
        };
        let mut update = Map::new();
        update.insert(
            "arms".into(),
            json!({"a": sorted_ids(&arms[0]), "b": sorted_ids(&arms[1])}),
        );
        update.insert("reads".into(), reads.clone());
        let compared = result["verdict"] == "compared";
        if compared && (reads["rounds"] == "b higher" || reads["tokens"] == "b higher") {
            let base: i64 = r
                .header("X-Pipeline-Version")
                .and_then(|v| v.trim().parse().ok())
                .ok_or_else(|| fail("the Worker sent no X-Pipeline-Version", 1))?;
            let body = json!({"schema": RETRO_EDIT_SCHEMA, "run": t.get("run").cloned().unwrap_or(Value::Null),
                              "base_version": base, "stage": stage, "note": trial_champion(home, t)?,
                              "summary": format!("Revert trial {tid}")});
            let put = api(
                home,
                "PUT",
                &format!("/api/pipeline/retro/projects/{slug}"),
                Some(dumps(&body, DEFAULT).as_bytes()),
                &[200, 400, 403, 404, 409, 412, 413, 429],
                &[("Content-Type", "application/json")],
            )?;
            if put.status != 200 {
                let text = String::from_utf8_lossy(&put.body);
                let why =
                    strip(&format!("{} {}", put.status, printable_line(&text, 200))).to_string();
                if expired {
                    update.insert("status".into(), json!("revert-failed"));
                    update.insert("settled".into(), json!(now()));
                    writeln!(
                        o.out,
                        "trial {tid} revert failed after {TRIAL_MAX_DAYS} days: kept the edit: {why}"
                    )
                    .unwrap();
                } else {
                    writeln!(o.out, "trial {tid} revert failed: {why}").unwrap();
                }
                updates.push((tid.to_string(), update));
                continue;
            }
            let version: i64 = put
                .header("X-Pipeline-Version")
                .and_then(|v| v.trim().parse().ok())
                .ok_or_else(|| fail("the Worker sent no X-Pipeline-Version", 1))?;
            update.insert("status".into(), json!("reverted"));
            update.insert("settled".into(), json!(now()));
            update.insert("revert_version".into(), json!(version));
            writeln!(o.out, "trial {tid} reverted to version {version}: {said}").unwrap();
        } else if compared {
            update.insert("status".into(), json!("kept"));
            update.insert("settled".into(), json!(now()));
            writeln!(o.out, "trial {tid} kept: {said}").unwrap();
        } else if expired {
            update.insert("status".into(), json!("inconclusive"));
            update.insert("settled".into(), json!(now()));
            writeln!(
                o.out,
                "trial {tid} inconclusive after {TRIAL_MAX_DAYS} days: kept the edit"
            )
            .unwrap();
        } else {
            writeln!(
                o.out,
                "trial {tid} open: a {} finished, b {} finished",
                a["finished"], b["finished"]
            )
            .unwrap();
        }
        updates.push((tid.to_string(), update));
    }
    let data = cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let mut records: Vec<Value> = data
            .get("trials")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|t| t.is_object())
            .cloned()
            .collect();
        for t in records.iter_mut() {
            if str_of(t, "status") != Some("open") {
                continue;
            }
            let id = str_of(t, "id").map(String::from);
            if let Some((_, update)) = updates.iter().rev().find(|(k, _)| Some(k) == id.as_ref()) {
                for (k, v) in update {
                    t[k] = v.clone();
                }
            }
        }
        let locked =
            |t: &Value| str_of(t, "status") == Some("reverted") && !truthy(t.get("resumed"));
        let settled_key = |t: &Value| -> String {
            match t.get("settled") {
                Some(v) if truthy(Some(v)) => v.as_str().unwrap_or("").to_string(),
                _ => String::new(),
            }
        };
        let mut settled: Vec<usize> = (0..records.len())
            .filter(|&i| str_of(&records[i], "status") != Some("open") && !locked(&records[i]))
            .collect();
        settled.sort_by(|&x, &y| settled_key(&records[y]).cmp(&settled_key(&records[x])));
        settled.truncate(TRIAL_KEEP);
        let kept: Vec<Value> = records
            .iter()
            .enumerate()
            .filter(|(i, t)| {
                str_of(t, "status") == Some("open") || locked(t) || settled.contains(i)
            })
            .map(|(_, t)| t.clone())
            .collect();
        data["trials"] = Value::Array(kept);
        data["updated"] = json!(now());
        Ok((Some(dump_board(&data)), data))
    })?;
    let mut reverted: Vec<&str> = trials
        .iter()
        .filter(|t| {
            let id = str_of(t, "id").unwrap();
            updates
                .iter()
                .rev()
                .find(|(k, _)| k == id)
                .is_some_and(|(_, u)| u.get("status") == Some(&json!("reverted")))
        })
        .map(|t| str_of(t, "project").unwrap())
        .collect();
    reverted.sort();
    reverted.dedup();
    for slug in reverted {
        if trials_paused(&data, slug) {
            writeln!(
                o.out,
                "stage-note trials paused for {slug} after {TRIAL_PAUSE_REVERTS} reverts; a person runs trial resume {slug}"
            )
            .unwrap();
        }
    }
    Ok(())
}

fn trial_resume(home: &Path, slug: &str, reason: &str, o: &mut Out) -> Res<()> {
    let body = dumps(&json!({"instruction": strip(reason)}), DEFAULT);
    let r = api(
        home,
        "POST",
        &format!("/api/trials/resume/{slug}"),
        Some(body.as_bytes()),
        &[200, 403],
        &[("Content-Type", "application/json")],
    )?;
    let text = String::from_utf8_lossy(&r.body).into_owned();
    if r.status == 403 && strip(&text) == TRIAL_RESUME_ONLY {
        return Err(fail(TRIAL_RESUME_ONLY, 5));
    }
    if r.status == 403 {
        return Err(fail(access_message(&home.join(".env")), 5));
    }
    let n = loads(&r.text()?).map_err(json_error)?["resumed"].clone();
    if truthy(Some(&n)) {
        writeln!(
            o.out,
            "stage-note trials resumed for {slug}: {n} reverts cleared"
        )
        .unwrap();
    } else {
        writeln!(o.out, "no unresumed reverts for {slug}; nothing changed").unwrap();
    }
    Ok(())
}

/// Settle the board's open retro stage-note trials, or resume one project's paused trials.
pub fn trial_cmd(call: &Call) -> Res<()> {
    let sub = call.args[0].as_str();
    let slug = call.args.get(1).map(String::as_str);
    let reason = call.args.get(2).map(String::as_str);
    let settle = sub == "settle" && slug.is_none() && reason.is_none();
    let resume = sub == "resume" && slug.is_some_and(|s| SLUG.is_full(s)) && reason.is_some();
    if !settle && !resume {
        return Err(fail(
            "usage: trial settle | trial resume <slug> <reason>",
            2,
        ));
    }
    let home = factory_home()?;
    if settle {
        flushed(|o| trial_settle(&home, o))
    } else {
        flushed(|o| trial_resume(&home, slug.unwrap(), reason.unwrap(), o))
    }
}

// ---- escalation and repeats ----

const ESCALATION_TEXT_LIMIT: usize = 8000;
const REPEATED_MAX: usize = 20;
const CHURN_ROUNDS: usize = 3;
static ROUND: Pattern = Pattern::new("[1-9][0-9]*", Flags::NONE);
static SIGNATURE: Pattern = Pattern::new(
    r"(?:[-*] )?Signature: ([A-Za-z0-9._/-]{1,200}#[a-z0-9]+(?:-[a-z0-9]+){1,5})",
    Flags::NONE,
);
static HEAD: Pattern = Pattern::new("[0-9a-f]{40}", Flags::NONE);

/// The first line of text, as `text.splitlines()[:1]` holds it.
fn first_line(text: &str) -> Option<&str> {
    splitlines(text, false).into_iter().next()
}

/// The Worker's review escalation reply, or None when it is not one.
fn escalation_verdict(text: &str) -> Option<Value> {
    let reply = loads(text).ok()?;
    let map = reply.as_object()?;
    let ok = matches!(
        map.get("decision").and_then(Value::as_str),
        Some("escalate" | "continue")
    ) && py_int(map.get("confidence")).is_some()
        && py_int(map.get("threshold")).is_some()
        && map.get("reason").is_some_and(Value::is_string)
        && map.get("model").is_some_and(Value::is_string)
        && map.get("decider").unwrap_or(&json!("model")) == "model";
    if !ok {
        return None;
    }
    let mut verdict = reply.clone();
    verdict["decider"] = json!("model");
    Some(verdict)
}

fn escalation(home: &Path, slug: &str, job_id: &str, round_text: &str, o: &mut Out) -> Res<()> {
    let folder = safe_job_folder(home, slug, job_id)?.join(format!("round-{round_text}"));
    let mut reviews = Map::new();
    for name in TRACER_REVIEWERS {
        let path = folder.join(format!("review-{name}.md"));
        if !path.is_file() {
            return Err(fail(
                format!("{} is missing: the round is not complete", path.display()),
                4,
            ));
        }
        let text = read_text(&path).map_err(|e| fail(os_error(&e, Some(&path)), 1))?;
        if first_line(&text) == Some("VERDICT: CHANGES") {
            let cut: String = text.chars().take(ESCALATION_TEXT_LIMIT).collect();
            reviews.insert(name.into(), Value::from(cut));
        }
    }
    let unchanged = json!({"decision": "continue", "confidence": null, "threshold": null,
                           "reason": "no reviewer asked for changes", "model": null, "decider": "rule"});
    if reviews.is_empty() {
        writeln!(o.out, "{}", dumps(&unchanged, DEFAULT)).unwrap();
        return Ok(());
    }
    let body = dumps(&json!({"reviews": reviews}), NO_ASCII);
    let (status, text) = match api(
        home,
        "POST",
        "/api/review/escalation",
        Some(body.as_bytes()),
        &[200, 400, 404, 502, 503],
        &[("Content-Type", "application/json")],
    ) {
        Ok(r) => (
            Some(r.status),
            String::from_utf8_lossy(&r.body).into_owned(),
        ),
        Err(e) => {
            let captured = printable_line(&e.stderr.replacen("factory.py: ", "", 1), 300);
            let why = if captured.is_empty() {
                "unknown error".to_string()
            } else {
                captured
            };
            writeln!(
                o.err,
                "factory.py: review escalation check failed ({why}); the review loop continues"
            )
            .unwrap();
            let failed = json!({"decision": "continue", "confidence": null, "threshold": null,
                                "reason": "the escalation check failed", "model": null, "decider": "rule"});
            writeln!(o.out, "{}", dumps(&failed, DEFAULT)).unwrap();
            return Ok(());
        }
    };
    let verdict = if status == Some(200) {
        escalation_verdict(&text)
    } else {
        None
    };
    let verdict = match verdict {
        Some(v) => v,
        None => {
            let why = if status == Some(200) {
                "malformed reply".to_string()
            } else {
                let detail = printable_line(&text, 200);
                let status = status.unwrap();
                if detail.is_empty() {
                    status.to_string()
                } else {
                    format!("{status}: {detail}")
                }
            };
            writeln!(
                o.err,
                "factory.py: review escalation check failed ({why}); the review loop continues"
            )
            .unwrap();
            let mut failed = unchanged;
            failed["reason"] = json!("the escalation check failed");
            failed
        }
    };
    writeln!(o.out, "{}", dumps(&verdict, DEFAULT)).unwrap();
    Ok(())
}

/// Parse `<slug>/<job-id>` and a positive round number, as escalation and repeats do.
fn job_round(call: &Call) -> Res<(PathBuf, String, String, String)> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    if !ROUND.is_full(&call.args[1]) {
        return Err(fail("round must be a positive integer", 2));
    }
    Ok((home, slug, job_id, call.args[1].clone()))
}

/// Print one JSON verdict on whether round <round>'s CHANGES findings need a person.
pub fn escalation_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id, round_text) = job_round(call)?;
    flushed(|o| escalation(&home, &slug, &job_id, &round_text, o))
}

/// The Signature: lines of a CHANGES review's text, or an empty set for any other review.
fn changes_signatures(text: &str) -> BTreeSet<String> {
    let lines = splitlines(text, false);
    if lines.first() != Some(&"VERDICT: CHANGES") {
        return BTreeSet::new();
    }
    lines
        .iter()
        .filter_map(|l| SIGNATURE.fullmatch(strip(l)))
        .map(|m| m.get(1).unwrap().as_str().to_string())
        .collect()
}

/// A file's text with undecodable bytes replaced, as `read_text(errors="replace")` gives it.
fn read_lossy(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(universal_newlines(
        String::from_utf8_lossy(&bytes).into_owned(),
    ))
}

/// The Signature: lines of a CHANGES review, or an empty set for any other or missing review.
fn review_signatures(path: &Path) -> BTreeSet<String> {
    if !path.is_file() {
        return BTreeSet::new();
    }
    changes_signatures(&read_lossy(path).unwrap_or_default())
}

/// Save a round's repeats on the board job's repeated list, in one board write.
fn record_repeats(
    home: &Path,
    slug: &str,
    job_id: &str,
    round_n: u64,
    repeats: &[Value],
    o: &mut Out,
) {
    let at = now();
    let new: Vec<Value> = repeats
        .iter()
        .map(|r| {
            json!({"round": round_n, "first": r["first"], "reviewer": r["reviewer"],
                   "signature": r["signature"], "at": at})
        })
        .collect();
    let key = |r: &Value| {
        (
            r.get("round").cloned(),
            r.get("reviewer").cloned(),
            r.get("signature").cloned(),
        )
    };
    let keys: Vec<_> = new.iter().map(key).collect();
    let result = cas(home, "/api/board", |text| {
        let mut data = load_board(text)?;
        let Some(job) = data["jobs"]
            .as_array()
            .and_then(|jobs| jobs.iter().find(|j| str_of(j, "id") == Some(job_id)))
        else {
            return Ok((None, ()));
        };
        if str_of(job, "project") != Some(slug) {
            return Ok((None, ()));
        }
        let old = job
            .get("repeated")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut rows: Vec<Value> = old
            .iter()
            .filter(|r| !r.is_object() || !keys.contains(&key(r)))
            .cloned()
            .chain(new.iter().cloned())
            .collect();
        if rows.len() > REPEATED_MAX {
            rows.drain(..rows.len() - REPEATED_MAX);
        }
        if rows == old {
            return Ok((None, ()));
        }
        patch_job(&mut data, job_id, &json!({"repeated": rows}));
        Ok((Some(dump_board(&data)), ()))
    });
    if let Err(e) = result {
        o.err.push_str(&e.stderr);
        writeln!(
            o.err,
            "factory.py: {job_id} repeats were not recorded on the board; the review loop goes on"
        )
        .unwrap();
    }
}

/// The paths the job's own commits change from old to new, or None when old is not an ancestor of new.
fn round_own_files(wt: &Path, old: &str, new: &str) -> Option<Vec<String>> {
    let wt = wt.to_string_lossy();
    let ancestor = capture(
        &["git", "-C", &wt, "merge-base", "--is-ancestor", old, new],
        None,
        &[],
        None,
    )
    .ok()?;
    if ancestor.code != 0 {
        return None;
    }
    let range = format!("{old}..{new}");
    let log = capture(
        &[
            "git",
            "-C",
            &wt,
            "log",
            "--first-parent",
            "--no-merges",
            "--no-renames",
            "--name-only",
            "-z",
            "--format=",
            &range,
        ],
        None,
        &[],
        None,
    )
    .ok()?;
    if log.code != 0 {
        return None;
    }
    Some(
        log.stdout
            .split('\0')
            .map(|p| p.trim_matches('\n'))
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect(),
    )
}

/// The head a review-plan.json records, or None for a missing, linked or malformed file.
fn plan_head(path: &Path) -> Option<String> {
    if !path.is_file() || is_link(path).unwrap_or(true) || is_link(path.parent()?).unwrap_or(true) {
        return None;
    }
    let plan = loads(&read_text(path).ok()?).ok()?;
    let head = plan.as_object()?.get("head")?.as_str()?;
    HEAD.is_full(head).then(|| head.to_string())
}

/// The head round-k/review-plan.json records, or None for a missing, linked or malformed file.
fn round_head(folder: &Path, k: usize) -> Option<String> {
    plan_head(&folder.join(format!("round-{k}")).join("review-plan.json"))
}

/// The paths changed in each of the last CHURN_ROUNDS rounds up to n, sorted, or [] on any doubt.
fn churn_files(folder: &Path, wt: &Path, n: usize) -> Vec<Value> {
    let heads: Option<Vec<String>> = (n - CHURN_ROUNDS..=n)
        .map(|k| round_head(folder, k))
        .collect();
    let Some(heads) = heads.filter(|_| wt.is_dir()) else {
        return Vec::new();
    };
    let sets: Option<Vec<BTreeSet<String>>> = heads
        .windows(2)
        .map(|w| round_own_files(wt, &w[0], &w[1]).map(|v| v.into_iter().collect()))
        .collect();
    let Some(sets) = sets else {
        return Vec::new();
    };
    let common = sets[1..].iter().fold(sets[0].clone(), |acc, s| {
        acc.intersection(s).cloned().collect()
    });
    let rounds: Vec<usize> = (n - CHURN_ROUNDS + 1..=n).collect();
    common
        .into_iter()
        .map(|path| json!({"path": path, "rounds": rounds}))
        .collect()
}

fn repeats(home: &Path, slug: &str, job_id: &str, round_text: &str, o: &mut Out) -> Res<()> {
    let n: usize = round_text
        .parse()
        .map_err(|_| fail("round must be a positive integer", 2))?;
    let job = safe_job_folder(home, slug, job_id)?;
    let folder = job.join(format!("round-{n}"));
    for name in TRACER_REVIEWERS {
        let path = folder.join(format!("review-{name}.md"));
        if !path.is_file() {
            return Err(fail(
                format!("{} is missing: the round is not complete", path.display()),
                4,
            ));
        }
    }
    let mut found: Vec<Value> = Vec::new();
    for name in TRACER_REVIEWERS {
        let now_signatures = review_signatures(&folder.join(format!("review-{name}.md")));
        let mut first: std::collections::BTreeMap<String, usize> = Default::default();
        for k in (1..n).rev() {
            let earlier = review_signatures(
                &job.join(format!("round-{k}"))
                    .join(format!("review-{name}.md")),
            );
            for s in now_signatures.intersection(&earlier) {
                first.insert(s.clone(), k);
            }
        }
        found.extend(
            first
                .into_iter()
                .map(|(s, k)| json!({"reviewer": name, "signature": s, "first": k})),
        );
    }
    let wt = home.join("worktrees").join(slug).join(job_id);
    let churn = if n >= 4 {
        churn_files(&job, &wt, n)
    } else {
        Vec::new()
    };
    let decision = if !found.is_empty() {
        "repeat"
    } else if !churn.is_empty() {
        "churn"
    } else {
        "continue"
    };
    if !found.is_empty() {
        record_repeats(home, slug, job_id, n as u64, &found, o);
    }
    let previous = if n > 1 { json!(n - 1) } else { Value::Null };
    let out = json!({"decision": decision, "round": n, "previous": previous, "repeats": found, "churn": churn});
    writeln!(o.out, "{}", dumps(&out, DEFAULT)).unwrap();
    Ok(())
}

/// Print one JSON verdict on whether a reviewer repeats a CHANGES signature from any earlier round.
pub fn repeats_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id, round_text) = job_round(call)?;
    flushed(|o| repeats(&home, &slug, &job_id, &round_text, o))
}

// ---- review-floor and review-plan ----

const VISIBLE_GLOBS: &[&str] = &[
    "*.html",
    "*.htm",
    "*.css",
    "*.scss",
    "*.sass",
    "*.less",
    "*.svg",
    "*.png",
    "*.jpg",
    "*.jpeg",
    "*.gif",
    "*.webp",
    "*.ico",
    "*.vue",
    "*.svelte",
    "*.jsx",
    "*.tsx",
    "*.astro",
    "*.hbs",
    "*.njk",
    "*.ejs",
    "*.erb",
    "*.j2",
    "*.jinja",
    "*templates/*",
    "*components/*",
    "web/*",
    "public/*",
    "static/*",
    "assets/*",
];
const PROTECTED_PATHS: &[&str] = &[
    ".github/*",
    "*/.github/*",
    "*.gitlab-ci*",
    "*.circleci/*",
    "*buildkite*",
    "*jenkinsfile*",
    ".factory/*",
    ".husky/*",
    "*.pre-commit-config.yaml",
    "*lefthook*",
    "*conftest.py",
    "*pytest.ini",
    "*tox.ini",
    "*jest.config*",
    "*vitest.config*",
    "*playwright.config*",
    "package.json",
    "*/package.json",
    "*.gitleaks.toml",
    "mise.toml",
    "*/mise.toml",
    "*.mise.toml",
    "*.oxfmtrc*",
    "nx.json",
    "tools/verify.py",
    "tests/cli/support.py",
    "tests/support/*",
    "tools/secret-scan.py",
    "pnpm-workspace.yaml",
    "*/pnpm-workspace.yaml",
    "*.npmrc",
];
static VISUAL_CRITERION: Pattern = Pattern::new(
    "screenshot|captur|before and after|before/after|visual evidence",
    Flags::I,
);
static VISIBLE_CHANGE: Pattern = Pattern::new(r"^## Visible change[ \t]*$", Flags::M);
static FULL_REVIEW: Pattern = Pattern::new(r"^## Full review[ \t]*$", Flags::M);
static TIER_LINE: Pattern = Pattern::new(
    r"^Tier:\s*(low|medium|high)\s*$",
    Flags {
        multiline: true,
        ignore_case: true,
        ..Flags::NONE
    },
);
static BACKTICK: Pattern = Pattern::new(r"`([^`\n]+)`", Flags::NONE);
static SECTION_END: Pattern = Pattern::new(r"^##\s", Flags::NONE);
static PROJECT_REPO: Pattern = Pattern::new(r"\A[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z", Flags::ASCII);
static BASE_NAME: Pattern = Pattern::new(r"[A-Za-z0-9][A-Za-z0-9._/-]{0,199}", Flags::NONE);
const SKIPPABLE_REVIEWERS: [&str; 2] = ["ux", "ui"];
const REVIEW_PLAN_USAGE: &str = "usage: review-plan <project-slug>/<job-id> <round>";
const REVIEW_FLOOR_USAGE: &str = "usage: review-floor <project-slug>/<job-id>";
const REVIEW_FLOOR_FILE: &str = "review-floor.json";

fn tier_rank(tier: &str) -> u8 {
    match tier {
        "low" => 0,
        "medium" => 1,
        _ => 2,
    }
}

/// The lines under the first ## heading named name, up to the next ## heading, or None.
fn markdown_section(text: &str, name: &str) -> Option<String> {
    let lines = splitlines(text, false);
    let src = format!(r"^##\s+(?:\d+\.\s*)?{}\b", fancy_regex::escape(name));
    let heading = fancy_regex::Regex::new(&translate(&src, Flags::I)).unwrap();
    let start = lines
        .iter()
        .position(|l| heading.is_match(l).unwrap_or(false))?;
    let end = (start + 1..lines.len())
        .find(|&i| SECTION_END.match_start(lines[i]).is_some())
        .unwrap_or(lines.len());
    Some(lines[start + 1..end].join("\n"))
}

/// A file's strict UTF-8 text, or a failure naming it.
fn read_file(path: &Path) -> Res<String> {
    read_text(path).map_err(|e| fail(os_error(&e, Some(path)), 1))
}

/// VISIBLE_GLOBS plus every backtick span in the project file's ## UI section.
fn visible_globs(project_file: &Path) -> Res<Vec<String>> {
    let section = markdown_section(&read_file(project_file)?, "UI").unwrap_or_default();
    let mut globs: Vec<String> = VISIBLE_GLOBS.iter().map(|g| g.to_string()).collect();
    globs.extend(
        BACKTICK
            .find_iter(&section)
            .map(|m| m.as_str().trim_matches('`').to_string()),
    );
    Ok(globs)
}

/// The sorted paths that match a PROTECTED_PATHS glob, compared lowercased.
fn touched_protected(paths: &[String]) -> Vec<String> {
    let mut hit: Vec<String> = paths
        .iter()
        .filter(|p| {
            let lower = p.to_lowercase();
            PROTECTED_PATHS.iter().any(|g| fnmatch(&lower, g))
        })
        .cloned()
        .collect();
    hit.sort();
    hit
}

/// (visible, reason): whether a round changes something a person sees, and the first rule that decided it.
fn plan_reason(
    spec_text: &str,
    decision_text: &str,
    changed: &[String],
    globs: &[String],
) -> (bool, String) {
    let Some(criteria) = markdown_section(spec_text, "Acceptance criteria") else {
        return (true, "spec.md has no Acceptance criteria section".into());
    };
    if VISUAL_CRITERION.search(&criteria).is_some() {
        return (
            true,
            "spec.md has a visual-evidence acceptance criterion".into(),
        );
    }
    if VISIBLE_CHANGE.search(decision_text).is_some() {
        return (true, "decision.md names a visible change".into());
    }
    for path in changed {
        if let Some(glob) = globs.iter().find(|g| fnmatch(path, g)) {
            return (true, format!("{path} matches {glob}"));
        }
    }
    (
        false,
        format!(
            "spec.md has no visual-evidence criterion and none of the {} changed files matches a visible-file glob",
            changed.len()
        ),
    )
}

/// The spec's Review tier line value, lowercased, or None.
fn spec_tier(spec_text: &str) -> Option<String> {
    let section = markdown_section(spec_text, "Review tier").unwrap_or_default();
    TIER_LINE
        .search(&section)
        .map(|m| m.get(1).unwrap().as_str().to_lowercase())
}

#[derive(Clone, Debug, PartialEq)]
struct Floor {
    tier: Option<String>,
    full_review: bool,
    unreadable: bool,
}

/// The recorded floor, None for a missing file, or a high floor marked unreadable for a bad one.
fn read_review_floor(path: &Path) -> Option<Floor> {
    if !path.is_file() {
        return None;
    }
    let floor = read_text(path).ok().and_then(|t| loads(&t).ok());
    let map = floor.as_ref().and_then(Value::as_object);
    let tier = map.and_then(|m| m.get("tier")).unwrap_or(&Value::Null);
    let tier_ok = tier.is_null()
        || tier
            .as_str()
            .is_some_and(|t| ["low", "medium", "high"].contains(&t));
    let full = map
        .and_then(|m| m.get("full_review"))
        .and_then(Value::as_bool);
    match (map, full) {
        (Some(_), Some(full)) if tier_ok => Some(Floor {
            tier: tier.as_str().map(String::from),
            full_review: full,
            unreadable: false,
        }),
        _ => Some(Floor {
            tier: Some("high".into()),
            full_review: false,
            unreadable: true,
        }),
    }
}

/// The floor raised by the current spec tier and decision.md's Full review heading. It never lowers.
fn merge_review_floor(floor: Option<Floor>, spec_text: &str, decision_text: &str) -> Floor {
    let floor = floor.unwrap_or(Floor {
        tier: None,
        full_review: false,
        unreadable: false,
    });
    let tiers: Vec<String> = [floor.tier.clone(), spec_tier(spec_text)]
        .into_iter()
        .flatten()
        .collect();
    let mut tier: Option<String> = None;
    for t in tiers {
        if tier
            .as_ref()
            .is_none_or(|best| tier_rank(&t) > tier_rank(best))
        {
            tier = Some(t);
        }
    }
    Floor {
        tier,
        full_review: floor.full_review || FULL_REVIEW.search(decision_text).is_some(),
        unreadable: floor.unreadable,
    }
}

/// (tier, reason): the spec's Review tier line, raised to high by the first floor that applies.
fn review_tier(
    spec_text: &str,
    decision_text: &str,
    protected: Option<&[String]>,
    floor: &Floor,
) -> (String, String) {
    let high = |r: &str| ("high".to_string(), r.to_string());
    if FULL_REVIEW.search(decision_text).is_some() {
        return high("decision.md asks for a full review");
    }
    if floor.full_review {
        return high("review-floor.json records a full review");
    }
    let Some(protected) = protected else {
        return high("the protected paths could not be read");
    };
    if !protected.is_empty() {
        return high("the branch touches protected paths");
    }
    let Some(tier) = spec_tier(spec_text) else {
        return high("spec.md has no Review tier line");
    };
    if let Some(recorded) = floor
        .tier
        .as_ref()
        .filter(|f| tier_rank(f) > tier_rank(&tier))
    {
        if floor.unreadable {
            return (
                recorded.clone(),
                "review-floor.json could not be read".into(),
            );
        }
        return (
            recorded.clone(),
            format!("review-floor.json records {recorded}, above spec.md's {tier}"),
        );
    }
    let reason = format!("spec.md rates the job {tier}");
    (tier, reason)
}

/// git in wt, as a captured result, or None when git cannot run.
fn git_run(
    wt: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    timeout: Option<u64>,
) -> Option<crate::shared::proc::Out> {
    let wt = wt.to_string_lossy();
    let argv: Vec<&str> = ["git", "-C", &wt]
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    capture(
        &argv,
        None,
        env,
        timeout.map(std::time::Duration::from_secs),
    )
    .ok()
}

/// None once origin/<base> in wt matches <base> at the project file's repo, or why it cannot be trusted.
fn fetch_base(wt: &Path, base: &str, repo: &str) -> Option<String> {
    if base.starts_with("factory/") {
        return Some(format!("base {base} is a factory branch"));
    }
    if !BASE_NAME.is_full(base) || base.contains("..") {
        return Some(format!("base {base} is not a safe branch name"));
    }
    if !PROJECT_REPO.is_full(repo) || repo.contains("..") {
        return Some(format!(
            "project file repo {} is not owner/name",
            repr(repo)
        ));
    }
    let mut scopes = vec!["--local"];
    let ext = git_run(
        wt,
        &["config", "--bool", "extensions.worktreeConfig"],
        &[],
        None,
    );
    if ext.is_some_and(|o| strip(&o.stdout) == "true") {
        scopes.push("--worktree");
    }
    for scope in scopes {
        let cfg = git_run(
            wt,
            &[
                "config",
                scope,
                "--includes",
                "--get-regexp",
                r"^(url|http)\.",
            ],
            &[],
            None,
        );
        match cfg.map(|o| o.code) {
            Some(0) => return Some("the worktree's git config sets url or http keys".into()),
            Some(1) => {}
            _ => return Some("could not read the worktree's git config".into()),
        }
    }
    let refspec = format!("+refs/heads/{base}:refs/remotes/origin/{base}");
    let url = project_fetch_url(repo);
    let r = git_run(
        wt,
        &["fetch", "-q", &url, &refspec],
        &[("GIT_TERMINAL_PROMPT", "0")],
        Some(120),
    );
    match r {
        Some(o) if o.code == 0 => None,
        _ => Some(format!("could not fetch origin {base}")),
    }
}

/// Run git in wt with replace refs and grafts off, so the worktree cannot swap the base's objects.
fn base_read(wt: &Path, args: &[&str]) -> Option<crate::shared::proc::Out> {
    let graft = if cfg!(windows) { "nul" } else { "/dev/null" };
    let argv: Vec<&str> = ["-c", "core.useReplaceRefs=false"]
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    git_run(
        wt,
        &argv,
        &[("GIT_NO_REPLACE_OBJECTS", "1"), ("GIT_GRAFT_FILE", graft)],
        None,
    )
}

fn nul_paths(out: &str) -> Vec<String> {
    out.split('\0')
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect()
}

/// The paths the worktree's branch changes since it left origin/<base>, or None on any git failure.
fn changed_files(wt: &Path, base: &str) -> Option<Vec<String>> {
    let mb = base_read(
        wt,
        &["merge-base", &format!("refs/remotes/origin/{base}"), "HEAD"],
    )
    .filter(|o| o.code == 0)?;
    let diff = base_read(
        wt,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-only",
            "--no-renames",
            "-z",
            strip(&mb.stdout),
            "HEAD",
        ],
    )
    .filter(|o| o.code == 0)?;
    Some(nul_paths(&diff.stdout))
}

/// The paths changed from an earlier round's head to HEAD, or None when that head is not an ancestor.
fn round_changed_files(wt: &Path, head: &str) -> Option<Vec<String>> {
    if git_run(
        wt,
        &["merge-base", "--is-ancestor", head, "HEAD"],
        &[],
        None,
    )?
    .code
        != 0
    {
        return None;
    }
    let diff = git_run(
        wt,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--name-only",
            "--no-renames",
            "-z",
            head,
            "HEAD",
        ],
        &[],
        None,
    )
    .filter(|o| o.code == 0)?;
    Some(nul_paths(&diff.stdout))
}

/// (K, head) from the newest round-K/review-plan.json with K below n, or None.
fn previous_head(folder: &Path, n: usize) -> Option<(usize, String)> {
    for k in (1..n).rev() {
        let path = folder.join(format!("round-{k}")).join("review-plan.json");
        if path.is_file()
            && !is_link(&path).unwrap_or(true)
            && !is_link(path.parent()?).unwrap_or(true)
        {
            return plan_head(&path).map(|head| (k, head));
        }
    }
    None
}

fn floor_record(floor: &Floor) -> String {
    dumps(
        &json!({"tier": floor.tier, "full_review": floor.full_review}),
        DEFAULT,
    )
}

fn write_file(path: &Path, text: &str) -> Res<()> {
    write_atomic(path, text.as_bytes()).map_err(|e| fail(os_error(&e, Some(path)), 1))
}

fn refuse_links(nodes: &[&Path]) -> Res<()> {
    for node in nodes {
        if is_link(node).map_err(|e| fail(os_error(&e, Some(node)), 1))? {
            return Err(fail(
                format!(
                    "{} is a symlink or junction; nothing was written",
                    node.display()
                ),
                6,
            ));
        }
    }
    Ok(())
}

fn on_board(data: &Value, job_id: &str) -> bool {
    data["jobs"]
        .as_array()
        .is_some_and(|jobs| jobs.iter().any(|j| str_of(j, "id") == Some(job_id)))
}

fn review_plan(home: &Path, slug: &str, job_id: &str, round_text: &str, o: &mut Out) -> Res<()> {
    let data = read_board(home)?;
    if !on_board(&data, job_id) {
        return Err(fail(
            format!("no job {job_id} on the board. Nothing was written."),
            4,
        ));
    }
    let project_file = project_files(home, slug)?.0;
    let meta = frontmatter(&project_file)
        .map_err(|e| fail(os_error(&e, Some(&project_file)), 1))?
        .0;
    let base = fm_get(&meta, "base")
        .filter(|b| !b.is_empty())
        .unwrap_or("main")
        .to_string();
    let repo = fm_get(&meta, "repo").unwrap_or("").to_string();
    let stacked = data["jobs"].as_array().into_iter().flatten().find(|j| {
        lead_of(j).as_deref() == Some(job_id) && j.get("stacked") == Some(&Value::Bool(true))
    });
    let wt_id = stacked.and_then(|j| str_of(j, "id")).unwrap_or(job_id);
    let wt = home.join("worktrees").join(slug).join(wt_id);
    let folder = home.join("jobs").join(slug).join(job_id);
    let round_folder = folder.join(format!("round-{round_text}"));
    let targets: Vec<PathBuf> = SKIPPABLE_REVIEWERS
        .iter()
        .map(|n| round_folder.join(format!("review-{n}.md")))
        .collect();
    let plan_file = round_folder.join("review-plan.json");
    let floor_file = folder.join(REVIEW_FLOOR_FILE);
    refuse_links(&[
        &folder,
        &round_folder,
        &targets[0],
        &targets[1],
        &plan_file,
        &floor_file,
    ])?;
    if !folder.join("spec.md").is_file() {
        return Err(fail(
            format!("{} is missing", folder.join("spec.md").display()),
            4,
        ));
    }
    let review_routes = read_review_routes(home)?;
    let spec_text = read_file(&folder.join("spec.md"))?;
    let decision = folder.join("decision.md");
    let decision_text = if decision.is_file() {
        read_file(&decision)?
    } else {
        String::new()
    };
    let n: usize = round_text.parse().unwrap();
    let head = if wt.is_dir() {
        git_run(&wt, &["rev-parse", "HEAD"], &[], None)
            .filter(|o| o.code == 0)
            .map(|o| strip(&o.stdout).to_string())
            .filter(|h| !h.is_empty())
    } else {
        None
    };
    let prev = if head.is_some() {
        previous_head(&folder, n)
    } else {
        None
    };
    let last = folder.join(format!("round-{}", n - 1));
    let passed = prev.is_some()
        && SKIPPABLE_REVIEWERS.iter().all(|name| {
            let path = last.join(format!("review-{name}.md"));
            path.is_file()
                && read_text(&path).is_ok_and(|t| {
                    t.starts_with("VERDICT: PASS") || t.starts_with("VERDICT: SKIPPED")
                })
        });
    let fixed = if passed {
        round_changed_files(&wt, &prev.as_ref().unwrap().1)
    } else {
        None
    };
    let globs = visible_globs(&project_file)?;
    let branch = if wt.is_dir() && fetch_base(&wt, &base, &repo).is_none() {
        changed_files(&wt, &base)
    } else {
        None
    };
    let protected = branch.as_deref().map(touched_protected);
    let (visible, reason) = if let Some(fixed) = &fixed {
        let hit = fixed
            .iter()
            .find_map(|p| globs.iter().find(|g| fnmatch(p, g)).map(|g| (p, g)));
        if VISIBLE_CHANGE.search(&decision_text).is_some() {
            (true, "decision.md names a visible change".to_string())
        } else if let Some((p, g)) = hit {
            (true, format!("{p} matches {g}"))
        } else {
            (
                false,
                format!(
                    "none of the {} files changed since round {} matches a visible-file glob, and round {} ux and ui did not ask for changes",
                    fixed.len(),
                    prev.as_ref().unwrap().0,
                    n - 1
                ),
            )
        }
    } else if let Some(branch) = &branch {
        plan_reason(&spec_text, &decision_text, branch, &globs)
    } else {
        (
            true,
            format!("the diff against origin/{base} could not be read"),
        )
    };
    if !visible {
        for path in &targets {
            if path.exists() {
                return Err(fail(
                    format!("{} already exists; nothing was written", path.display()),
                    6,
                ));
            }
        }
    }
    let floor = merge_review_floor(read_review_floor(&floor_file), &spec_text, &decision_text);
    let (tier, tier_reason) = review_tier(&spec_text, &decision_text, protected.as_deref(), &floor);
    let run: Vec<&str> = if visible {
        TRACER_REVIEWERS.to_vec()
    } else {
        vec!["security", "code"]
    };
    let mut routes = Map::new();
    for r in &run {
        let route = if tier == "high" {
            Value::Null
        } else {
            let key = if tier == "medium" || *r == "security" {
                "medium"
            } else {
                "low"
            };
            review_routes[key].clone()
        };
        routes.insert(r.to_string(), route);
    }
    if let Some(head) = &head {
        if !round_folder.is_dir() {
            std::fs::create_dir(&round_folder)
                .map_err(|e| fail(os_error(&e, Some(&round_folder)), 1))?;
        }
        let plan = json!({"round": n, "head": head, "protected": protected, "tier": tier, "routes": routes});
        write_file(&plan_file, &(dumps(&plan, DEFAULT) + "\n"))?;
        write_file(&floor_file, &(floor_record(&floor) + "\n"))?;
    }
    if !visible {
        for path in &targets {
            write_file(
                path,
                &format!(
                    "VERDICT: SKIPPED\n\nfactory.py review-plan skipped this review: {reason}.\n"
                ),
            )?;
        }
    }
    let skipped: Vec<&str> = if visible {
        vec![]
    } else {
        SKIPPABLE_REVIEWERS.to_vec()
    };
    let out = json!({"round": n, "run": run, "skipped": skipped, "reason": reason, "protected": protected,
                     "tier": tier, "tier_reason": tier_reason, "routes": routes});
    writeln!(o.out, "{}", dumps(&out, DEFAULT)).unwrap();
    Ok(())
}

/// review-plan's check of its round operand.
fn plan_round(round_text: &str) -> Res<()> {
    if !ROUND.is_full(round_text) {
        return Err(fail(
            format!("round must be a positive integer. {REVIEW_PLAN_USAGE}"),
            2,
        ));
    }
    Ok(())
}

/// Write SKIPPED ux and ui reviews for a round that changes nothing a person sees, and print one JSON plan.
pub fn review_plan_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    plan_round(&call.args[1])?;
    flushed(|o| review_plan(&home, &slug, &job_id, &call.args[1], o))
}

fn review_floor(home: &Path, slug: &str, job_id: &str, o: &mut Out) -> Res<()> {
    if !on_board(&read_board(home)?, job_id) {
        return Err(fail(
            format!("no job {job_id} on the board. Nothing was written. {REVIEW_FLOOR_USAGE}"),
            4,
        ));
    }
    let folder = home.join("jobs").join(slug).join(job_id);
    let floor_file = folder.join(REVIEW_FLOOR_FILE);
    refuse_links(&[&folder, &floor_file])?;
    if !folder.join("spec.md").is_file() {
        return Err(fail(
            format!("{} is missing", folder.join("spec.md").display()),
            4,
        ));
    }
    let decision = folder.join("decision.md");
    let decision_text = if decision.is_file() {
        read_file(&decision)?
    } else {
        String::new()
    };
    let spec_text = read_file(&folder.join("spec.md"))?;
    let floor = merge_review_floor(read_review_floor(&floor_file), &spec_text, &decision_text);
    let record = floor_record(&floor);
    write_file(&floor_file, &(record.clone() + "\n"))?;
    writeln!(o.out, "{record}").unwrap();
    Ok(())
}

/// Raise review-floor.json to the spec's current tier and decision.md's Full review heading, and print it.
pub fn review_floor_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    flushed(|o| review_floor(&home, &slug, &job_id, o))
}

// ---- pre-review ----

const CHECKS_FILE: &str = ".factory/checks.json";
const DIFF_BUDGET_LINES_PER_FILE: usize = 150;
const DIFF_BUDGET_FILE_FACTOR: usize = 2;
const DIFF_BUDGET_LINE_FACTOR: usize = 3;
const DIFF_BUDGET_UNLISTED_MAX: usize = 20;
const PRE_REVIEW_USAGE: &str = "usage: pre-review <project-slug>/<job-id>";
const CHECK_NAME_SRC: &str = "[A-Za-z0-9][A-Za-z0-9_.-]{0,39}";
static CHECK_NAME: Pattern = Pattern::new(CHECK_NAME_SRC, Flags::NONE);
static SPEC_TASKS: Pattern = Pattern::new(
    r"^## (?:\d+\.\s*)?Tasks\b[^\n]*\n(.*?)(?=^## |\Z)",
    Flags {
        multiline: true,
        dotall: true,
        ..Flags::NONE
    },
);
static REPO_PATH: Pattern = Pattern::new(r"[A-Za-z0-9_./-]{1,200}", Flags::ASCII);
static TRANSIENT_LOG: Pattern = Pattern::new(
    r"ECONNRESET|ETIMEDOUT|EAI_AGAIN|Connection reset by peer|Temporary failure in name resolution|Could not resolve host|429 Too Many Requests|502 Bad Gateway|503 Service Unavailable|504 Gateway Timeout",
    Flags::NONE,
);
static BACKTICK_SPAN: Pattern = Pattern::new("`([^`]*)`", Flags::NONE);
static BACKTICK_RUN: Pattern = Pattern::new("`+", Flags::NONE);

/// The distinct safe path tokens from the Tasks section's - Files: lines, in first-seen order.
fn spec_budget_paths(text: &str) -> Vec<String> {
    let text = format!("{text}\n");
    let Some(section) = SPEC_TASKS.search(&text) else {
        return Vec::new();
    };
    let mut paths: Vec<String> = Vec::new();
    for line in splitlines(section.get(1).unwrap().as_str(), false) {
        if !strip(line).starts_with("- Files:") {
            continue;
        }
        for m in BACKTICK_SPAN.find_iter(line) {
            let t = &m.as_str()[1..m.as_str().len() - 1];
            if REPO_PATH.is_full(&t.replace('*', ""))
                && !t.starts_with('/')
                && !t.contains("..")
                && !paths.iter().any(|p| p == t)
            {
                paths.push(t.to_string());
            }
        }
    }
    paths
}

/// {path: added plus deleted lines} the branch changes since the merge base with ref, or None on a git failure.
fn diff_numstat(wt: &Path, reference: &str) -> Option<Vec<(String, u64)>> {
    let mb = base_read(wt, &["merge-base", reference, "HEAD"]).filter(|o| o.code == 0)?;
    let diff = base_read(
        wt,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--numstat",
            "--no-renames",
            "-z",
            strip(&mb.stdout),
            "HEAD",
        ],
    )
    .filter(|o| o.code == 0)?;
    let mut counts: Vec<(String, u64)> = Vec::new();
    for record in diff.stdout.split('\0').filter(|r| !r.is_empty()) {
        let mut parts = record.splitn(3, '\t');
        let (added, deleted, path) = (parts.next()?, parts.next()?, parts.next()?);
        let n: u64 = [added, deleted]
            .iter()
            .filter(|n| **n != "-")
            .map(|n| n.parse::<u64>().unwrap_or(0))
            .sum();
        match counts.iter_mut().find(|(p, _)| p == path) {
            Some(slot) => slot.1 = n,
            None => counts.push((path.to_string(), n)),
        }
    }
    Some(counts)
}

/// Whether a spec Files token covers a changed path: a * glob, a directory ending in /, or the exact path.
fn budget_covers(token: &str, path: &str) -> bool {
    if token.contains('*') {
        return fnmatch(path, token);
    }
    if token.ends_with('/') {
        path.starts_with(token)
    } else {
        path == token
    }
}

/// The pre-review budget object comparing the spec's Files paths with the branch's numstat.
fn diff_budget(paths: &[String], numstat: &[(String, u64)]) -> Value {
    let expected_lines = paths.len() * DIFF_BUDGET_LINES_PER_FILE;
    let files = numstat.len();
    let lines: u64 = numstat.iter().map(|(_, n)| n).sum();
    let file_limit = paths.len() * DIFF_BUDGET_FILE_FACTOR;
    let line_limit = expected_lines * DIFF_BUDGET_LINE_FACTOR;
    let mut unlisted: Vec<&String> = numstat
        .iter()
        .map(|(p, _)| p)
        .filter(|p| !paths.iter().any(|t| budget_covers(t, p)))
        .collect();
    unlisted.sort();
    let over = files > file_limit || lines > line_limit as u64;
    json!({"result": if over { "over" } else { "within" }, "files": files, "lines": lines,
           "expected_files": paths.len(), "expected_lines": expected_lines, "file_limit": file_limit,
           "line_limit": line_limit, "unlisted": unlisted, "reason": null})
}

/// The Budget: line for pre-review.md.
fn budget_line(budget: &Value) -> String {
    if budget["result"] == "none" {
        return format!("Budget: none ({})", budget["reason"].as_str().unwrap_or(""));
    }
    format!(
        "Budget: {} (files {}, limit {}; lines {}, limit {})",
        budget["result"].as_str().unwrap(),
        budget["files"],
        budget["file_limit"],
        budget["lines"],
        budget["line_limit"]
    )
}

/// (checks, reason): the checks file's list of {name, run}, or [] and why the file is unusable.
fn load_checks(text: &str) -> (Vec<(String, Vec<String>)>, Option<String>) {
    let data = match loads(text) {
        Ok(data) => data,
        Err(e) => {
            return (
                Vec::new(),
                Some(format!("{CHECKS_FILE} is not valid JSON: {e}")),
            )
        }
    };
    let checks = data
        .get("checks")
        .and_then(Value::as_array)
        .filter(|c| (1..=20).contains(&c.len()));
    let Some(checks) = checks.filter(|_| data.is_object()) else {
        return (
            Vec::new(),
            Some(format!(
                "{CHECKS_FILE} needs a checks list of 1 to 20 entries"
            )),
        );
    };
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for (i, check) in checks.iter().enumerate() {
        let name = check
            .as_object()
            .and_then(|c| c.get("name"))
            .and_then(Value::as_str);
        let Some(name) = name.filter(|n| CHECK_NAME.is_full(n) && !out.iter().any(|(k, _)| k == n))
        else {
            return (
                Vec::new(),
                Some(format!(
                    "check {} needs a unique name matching {CHECK_NAME_SRC}",
                    i + 1
                )),
            );
        };
        let run = check.get("run").and_then(Value::as_array).filter(|r| {
            (1..=64).contains(&r.len())
                && r.iter().all(|a| {
                    a.as_str()
                        .is_some_and(|a| !a.is_empty() && !a.contains('\0'))
                })
        });
        let Some(run) = run else {
            return (
                Vec::new(),
                Some(format!(
                    "check {name} needs a run list of 1 to 64 nonempty strings"
                )),
            );
        };
        out.push((
            name.to_string(),
            run.iter()
                .map(|a| a.as_str().unwrap().to_string())
                .collect(),
        ));
    }
    (out, None)
}

/// A check's exit: a code, a negative signal, or "timeout" or "missing" when it never finished.
#[derive(Clone, Debug, PartialEq)]
enum CheckExit {
    Code(i32),
    Timeout,
    Missing,
}

impl CheckExit {
    fn value(&self) -> Value {
        match self {
            CheckExit::Code(c) => json!(c),
            CheckExit::Timeout => json!("timeout"),
            CheckExit::Missing => json!("missing"),
        }
    }

    fn text(&self) -> String {
        match self {
            CheckExit::Code(c) => c.to_string(),
            CheckExit::Timeout => "timeout".into(),
            CheckExit::Missing => "missing".into(),
        }
    }
}

/// (exit, seconds): run argv in cwd with output to log.
fn run_check(argv: &[String], cwd: &Path, log: &Path, timeout: u64) -> Res<(CheckExit, i64)> {
    use std::process::{Command, Stdio};
    let start = std::time::Instant::now();
    let seconds = |s: std::time::Instant| round_int(s.elapsed().as_secs_f64());
    let mut out = std::fs::File::create(log).map_err(|e| fail(os_error(&e, Some(log)), 1))?;
    let err = out
        .try_clone()
        .map_err(|e| fail(os_error(&e, Some(log)), 1))?;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            out.try_clone()
                .map_err(|e| fail(os_error(&e, Some(log)), 1))?,
        ))
        .stderr(Stdio::from(err));
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            use std::io::Write;
            let _ = write!(out, "missing program: {}\n", argv[0]);
            return Ok((CheckExit::Missing, seconds(start)));
        }
    };
    let deadline = start + std::time::Duration::from_secs(timeout);
    loop {
        if let Some(status) = child.try_wait().map_err(|e| fail(os_error(&e, None), 1))? {
            #[cfg(unix)]
            if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
                return Ok((CheckExit::Code(-signal), seconds(start)));
            }
            return Ok((CheckExit::Code(status.code().unwrap_or(1)), seconds(start)));
        }
        if std::time::Instant::now() >= deadline {
            #[cfg(unix)]
            {
                extern "C" {
                    fn kill(pid: i32, sig: i32) -> i32;
                }
                // SAFETY: kill takes two plain integers and touches no Rust memory; -pid names the check's
                // own process group, which process_group(0) created for it.
                unsafe {
                    kill(-(child.id() as i32), 9);
                }
            }
            #[cfg(not(unix))]
            let _ = child.kill();
            let _ = child.wait();
            return Ok((CheckExit::Timeout, seconds(start)));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A check result's class: None on a pass, else budget, structural, canceled, transient or deterministic.
fn failure_class(code: &CheckExit, log: &Path) -> Option<&'static str> {
    let code = match code {
        CheckExit::Code(0) => return None,
        CheckExit::Timeout => return Some("budget"),
        CheckExit::Missing | CheckExit::Code(126 | 127) => return Some("structural"),
        CheckExit::Code(c) if *c < 0 || *c == 130 || *c == 143 => return Some("canceled"),
        CheckExit::Code(c) => *c,
    };
    let _ = code;
    let bytes = std::fs::read(log).unwrap_or_default();
    let tail = &bytes[bytes.len().saturating_sub(65536)..];
    if TRANSIENT_LOG
        .search(&String::from_utf8_lossy(tail))
        .is_some()
    {
        Some("transient")
    } else {
        Some("deterministic")
    }
}

/// The last lines of a failing check's log for pre-review.md, with secrets redacted.
fn pre_review_tail(_log_text: &str, _env: &crate::shared::conf::Conf) -> Res<Vec<String>> {
    // Deferred to integration: 2202-port-retro
    Err(not_ported("pre-review"))
}

/// The user id that owns this process.
#[cfg(unix)]
fn own_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid takes no arguments, cannot fail and touches no Rust memory.
    unsafe { getuid() }
}

/// Create the job's temp folder private to this account, refusing a name someone else holds.
fn make_job_temp(slug: &str, job_id: &str) -> Res<PathBuf> {
    let path = temp_root().join(format!("factory-{slug}-{job_id}"));
    let refused = || {
        fail(
            format!(
                "{} is a link or a file; remove it, then run prompt again",
                path.display()
            ),
            6,
        )
    };
    let link = is_link(&path).map_err(|e| fail(os_error(&e, Some(&path)), 1))?;
    if link || std::fs::symlink_metadata(&path).is_ok() && !path.is_dir() {
        return Err(refused());
    }
    mkdir_private(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => refused(),
        _ => fail(os_error(&e, Some(&path)), 1),
    })?;
    let meta = std::fs::symlink_metadata(&path).map_err(|e| fail(os_error(&e, Some(&path)), 1))?;
    if is_link(&path).unwrap_or(true) || !meta.is_dir() {
        return Err(refused());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.uid() != own_uid() {
            return Err(fail(
                format!(
                    "{} belongs to another account; remove it, then run prompt again",
                    path.display()
                ),
                6,
            ));
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| refused())?;
    }
    Ok(path)
}

/// The raw id of the job's batched-into ref, as Python's `lead_of` returns it.
fn lead_value(j: &Value) -> Option<Value> {
    j.get("refs")?
        .as_array()?
        .iter()
        .find(|r| r.get("kind").and_then(Value::as_str) == Some(BATCH_REF))
        .map(|r| r.get("id").cloned().unwrap_or(Value::Null))
}

fn pre_review(home: &Path, slug: &str, job_id: &str, o: &mut Out) -> Res<()> {
    let data = read_board(home)?;
    let Some(job) = data["jobs"]
        .as_array()
        .and_then(|jobs| jobs.iter().find(|j| str_of(j, "id") == Some(job_id)))
    else {
        return Err(fail(
            format!("no job {job_id} on the board. Nothing was written. {PRE_REVIEW_USAGE}"),
            4,
        ));
    };
    let round_n = py_int(job.get("round")).filter(|r| *r >= 0).unwrap_or(0) + 1;
    let project_file = project_files(home, slug)?.0;
    let meta = frontmatter(&project_file)
        .map_err(|e| fail(os_error(&e, Some(&project_file)), 1))?
        .0;
    let base = fm_get(&meta, "base")
        .filter(|b| !b.is_empty())
        .unwrap_or("main")
        .to_string();
    let repo = fm_get(&meta, "repo").unwrap_or("").to_string();
    let lead = if job.get("stacked") == Some(&Value::Bool(true)) {
        lead_value(job).filter(|v| !v.is_null())
    } else {
        None
    };
    let budget_ref = match &lead {
        None => Some(format!("refs/remotes/origin/{base}")),
        Some(Value::String(l)) if JOB_ID.is_full(l) && !l.contains("..") => {
            Some(format!("refs/heads/factory/{l}"))
        }
        Some(_) => None,
    };
    let stacked = data["jobs"].as_array().into_iter().flatten().find(|j| {
        lead_of(j).as_deref() == Some(job_id) && j.get("stacked") == Some(&Value::Bool(true))
    });
    let wt = home
        .join("worktrees")
        .join(slug)
        .join(stacked.and_then(|j| str_of(j, "id")).unwrap_or(job_id));
    let folder = home.join("jobs").join(slug).join(job_id);
    let out_file = folder.join("pre-review.md");
    refuse_links(&[&folder, &out_file])?;
    if !wt.is_dir() {
        return Err(fail(
            format!("no worktree at {}; nothing was written", wt.display()),
            4,
        ));
    }
    let git = |args: &[&str]| {
        base_read(&wt, args)
            .filter(|o| o.code == 0)
            .map(|o| o.stdout)
    };
    let head = git(&["rev-parse", "HEAD"])
        .map(|h| strip(&h).to_string())
        .filter(|h| !h.is_empty());
    let mut reason = fetch_base(&wt, &base, &repo);
    let mut checks = Vec::new();
    let spec = folder.join("spec.md");
    let mut paths = Vec::new();
    let mut numstat = None;
    let why;
    if is_link(&spec).unwrap_or(false) {
        why = "spec.md is a symlink or junction";
    } else if !spec.is_file() {
        why = "the job folder has no spec.md";
    } else if {
        paths = spec_budget_paths(&read_lossy(&spec).unwrap_or_default());
        paths.is_empty()
    } {
        why = "spec.md has no task Files paths";
    } else if reason.is_some() {
        why = "the base could not be fetched";
    } else if {
        numstat = budget_ref.as_deref().and_then(|r| diff_numstat(&wt, r));
        numstat.is_none()
    } {
        why = "the branch diff could not be read";
    } else {
        why = "";
    }
    let budget = match &numstat {
        Some(numstat) => diff_budget(&paths, numstat),
        None => {
            json!({"result": "none", "files": 0, "lines": 0, "expected_files": 0, "expected_lines": 0,
                       "file_limit": 0, "line_limit": 0, "unlisted": [], "reason": why})
        }
    };
    if reason.is_none() {
        match git(&["show", &format!("refs/remotes/origin/{base}:{CHECKS_FILE}")]) {
            Some(text) => (checks, reason) = load_checks(&text),
            None => reason = Some(format!("origin/{base} has no {CHECKS_FILE}")),
        }
    }
    let head_text = head.clone().unwrap_or_else(|| "None".into());
    if let Some(reason) = reason {
        write_file(
            &out_file,
            &format!(
                "PRE-REVIEW: NONE\n\nHead: {head_text}\n{}\nReason: {}\n",
                budget_line(&budget),
                printable_line(&reason, 300)
            ),
        )?;
        let out = json!({"result": "none", "head": head, "checks": [], "reason": reason, "round": round_n,
                         "repeats": [], "budget": budget});
        writeln!(o.out, "{}", dumps(&out, DEFAULT)).unwrap();
        return Ok(());
    }
    let logs = make_job_temp(slug, job_id)?.join("pre-review");
    if is_link(&logs).unwrap_or(false) {
        return Err(fail(
            format!(
                "{} is a symlink or junction; nothing was written",
                logs.display()
            ),
            6,
        ));
    }
    mkdir_private(&logs).map_err(|e| fail(os_error(&e, Some(&logs)), 1))?;
    let checks_sha = git(&["rev-parse", &format!("refs/remotes/origin/{base}")])
        .map(|s| strip(&s).to_string())
        .unwrap_or_default();
    let mut results: Vec<(String, CheckExit, i64, Option<&str>, bool)> = Vec::new();
    for (name, run) in &checks {
        let log = logs.join(format!("{name}.log"));
        let (mut code, mut seconds) = run_check(run, &wt, &log, check_timeout())?;
        let (mut kind, mut retried) = (failure_class(&code, &log), false);
        if kind == Some("transient") {
            let attempt = logs.join(format!("{name}.attempt-1.log"));
            std::fs::rename(&log, &attempt).map_err(|e| fail(os_error(&e, Some(&log)), 1))?;
            (code, seconds) = run_check(run, &wt, &log, check_timeout())?;
            (kind, retried) = (failure_class(&code, &log), true);
        }
        results.push((name.clone(), code, seconds, kind, retried));
    }
    let failed: Vec<&(String, CheckExit, i64, Option<&str>, bool)> = results
        .iter()
        .filter(|r| r.1 != CheckExit::Code(0))
        .collect();
    let result = if failed.is_empty() { "pass" } else { "fail" };
    let mut lines = vec![
        format!("PRE-REVIEW: {}", result.to_uppercase()),
        String::new(),
        format!("Head: {head_text}"),
        budget_line(&budget),
        format!("Checks: origin/{base}:{CHECKS_FILE} at {checks_sha}"),
        format!("Logs: {}", logs.display()),
        String::new(),
        "| Check | Exit | Seconds | Class |".into(),
        "| --- | --- | --- | --- |".into(),
    ];
    for (name, code, seconds, kind, retried) in &results {
        let tail = if *retried { ", retried" } else { "" };
        lines.push(format!(
            "| {name} | {} | {seconds} | {}{tail} |",
            code.text(),
            kind.unwrap_or("-")
        ));
    }
    lines.push(String::new());
    lines.push("Classes are explained under \"Failure classes\" in docs/autonomy.md of the factory repository.".into());
    if budget["result"] == "over" {
        lines.extend([
            String::new(),
            "## Diff budget".into(),
            String::new(),
            "| Measure | Spec estimate | Limit | Branch |".into(),
            "| --- | --- | --- | --- |".into(),
            format!(
                "| Files | {} | {} | {} |",
                budget["expected_files"], budget["file_limit"], budget["files"]
            ),
            format!(
                "| Lines | {} | {} | {} |",
                budget["expected_lines"], budget["line_limit"], budget["lines"]
            ),
        ]);
        let unlisted: Vec<&str> = budget["unlisted"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        if !unlisted.is_empty() {
            lines.extend([
                String::new(),
                "Files outside the spec's task list:".into(),
                String::new(),
            ]);
            for p in unlisted.iter().take(DIFF_BUDGET_UNLISTED_MAX) {
                let p = printable_line(p, 200);
                let longest = BACKTICK_RUN
                    .find_iter(&p)
                    .map(|m| m.as_str().len())
                    .max()
                    .unwrap_or(0);
                let fence = "`".repeat(longest + 1);
                lines.push(if p.contains('`') {
                    format!("- {fence} {p} {fence}")
                } else {
                    format!("- `{p}`")
                });
            }
            if unlisted.len() > DIFF_BUDGET_UNLISTED_MAX {
                lines.push(format!(
                    "- and {} more",
                    unlisted.len() - DIFF_BUDGET_UNLISTED_MAX
                ));
            }
        }
    }
    let env_file = home.join(".env");
    let env = if env_file.exists() {
        read_conf(&env_file).map_err(|e| fail(os_error(&e, Some(&env_file)), 1))?
    } else {
        Default::default()
    };
    for r in &failed {
        let log_text = read_lossy(&logs.join(format!("{}.log", r.0))).unwrap_or_default();
        let tail = pre_review_tail(&log_text, &env)?;
        lines.extend([
            String::new(),
            format!("## {}", r.0),
            String::new(),
            "~~~text".into(),
        ]);
        lines.extend(tail);
        lines.push("~~~".into());
    }
    write_file(&out_file, &(lines.join("\n") + "\n"))?;
    let mut repeats: Vec<String> = Vec::new();
    let names: BTreeSet<String> = failed
        .iter()
        .filter(|r| r.3 == Some("deterministic"))
        .map(|r| r.0.clone())
        .collect();
    if !names.is_empty() {
        let record_file = make_job_temp(slug, job_id)?.join("pre-review-failures.json");
        let record = read_text(&record_file).ok().and_then(|t| loads(&t).ok());
        let valid = record.as_ref().and_then(Value::as_object).filter(|m| {
            m.values()
                .all(|v| v.as_array().is_some_and(|l| l.iter().all(Value::is_string)))
        });
        let mut record = valid.cloned().unwrap_or_default();
        let key = round_n.to_string();
        let mut merged: BTreeSet<String> = names.clone();
        if let Some(old) = record.get(&key).and_then(Value::as_array) {
            merged.extend(old.iter().filter_map(Value::as_str).map(String::from));
        }
        record.insert(key, json!(merged));
        write_file(
            &record_file,
            &(dumps(&Value::Object(record.clone()), DEFAULT) + "\n"),
        )?;
        let earlier: BTreeSet<String> = record
            .iter()
            .filter(|(k, _)| {
                !k.is_empty()
                    && k.chars().all(|c| c.is_ascii_digit())
                    && k.parse::<i64>().is_ok_and(|k| k < round_n)
            })
            .flat_map(|(_, v)| {
                v.as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
            })
            .collect();
        repeats = names.intersection(&earlier).cloned().collect();
    }
    let checks_out: Vec<Value> = results
        .iter()
        .map(|(name, code, seconds, kind, retried)| {
            json!({"name": name, "exit": code.value(), "seconds": seconds, "class": kind, "retried": retried})
        })
        .collect();
    let out = json!({"result": result, "head": head, "checks": checks_out, "reason": null, "round": round_n,
                     "repeats": repeats, "budget": budget});
    writeln!(o.out, "{}", dumps(&out, DEFAULT)).unwrap();
    if !failed.is_empty() {
        return Err(crate::shared::exit::exit(1));
    }
    Ok(())
}

/// Run the base branch's checks file in the job worktree, write pre-review.md and print one JSON line.
pub fn pre_review_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    flushed(|o| pre_review(&home, &slug, &job_id, o))
}

// ---- grounding and verdict ----

const GROUNDING_USAGE: &str = "usage: grounding <project-slug>/<job-id> <round>";
const GROUNDING_NEAR: i64 = 10;
#[cfg_attr(not(test), allow(dead_code))]
const GROUNDING_ROWS_MAX: usize = 100;
static CITATION: Pattern = Pattern::new(
    r"(?<![\w./:-])((?:[\w.-]+/)*[\w-][\w.-]*\.[A-Za-z][A-Za-z0-9]{0,9}):([0-9]+)(?:-([0-9]+))?((?:,[0-9]+)*)(?![\w])",
    Flags::NONE,
);
/// CITATION without its lookarounds, to split one match into groups.
static CITATION_PARTS: Pattern = Pattern::new(
    r"((?:[\w.-]+/)*[\w-][\w.-]*\.[A-Za-z][A-Za-z0-9]{0,9}):([0-9]+)(?:-([0-9]+))?((?:,[0-9]+)*)",
    Flags::NONE,
);
static HUNK: Pattern = Pattern::new(r"@@ -\S+ \+([0-9]+)(?:,([0-9]+))? @@", Flags::NONE);

/// A cited path and its lines, as `find_citations` lists them.
#[derive(Debug, PartialEq)]
struct Citation {
    citation: String,
    path: String,
    lines: Vec<i64>,
    span: Option<(i64, i64)>,
}

/// Each distinct path:line a review cites, in order of first appearance.
#[cfg_attr(not(test), allow(dead_code))]
fn find_citations(text: &str) -> Vec<Citation> {
    let mut found = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for whole in CITATION.find_iter(text) {
        let all = whole.as_str().to_string();
        let m = CITATION_PARTS.fullmatch(whole.as_str()).unwrap();
        let mut path = m.get(1).unwrap().as_str();
        while let Some(rest) = path.strip_prefix("./") {
            path = rest;
        }
        if seen.contains(&all) || path.split('/').any(|p| p == "..") {
            continue;
        }
        seen.insert(all.clone());
        let num = |s: &str| s.parse::<i64>().unwrap_or(i64::MAX);
        let mut lines = vec![num(m.get(2).unwrap().as_str())];
        let end = m.get(3).map(|e| num(e.as_str()));
        lines.extend(end);
        lines.extend(
            m.get(4)
                .unwrap()
                .as_str()
                .split(',')
                .filter(|x| !x.is_empty())
                .map(num),
        );
        let span = end.map(|_| (lines[0], lines[1]));
        found.push(Citation {
            citation: all,
            path: path.to_string(),
            lines,
            span,
        });
    }
    found
}

/// The lines the branch changes since origin/<base>, by path, or why they cannot be read.
type Ranges = std::collections::BTreeMap<String, Vec<(i64, i64)>>;

#[cfg_attr(not(test), allow(dead_code))]
fn diff_ranges(wt: &Path, base: &str, repo: &str) -> Result<Ranges, String> {
    if let Some(reason) = fetch_base(wt, base, repo) {
        return Err(reason);
    }
    let unreadable = format!("the diff against origin/{base} could not be read");
    let Some(mb) = base_read(
        wt,
        &["merge-base", &format!("refs/remotes/origin/{base}"), "HEAD"],
    )
    .filter(|o| o.code == 0) else {
        return Err(unreadable);
    };
    let Some(diff) = base_read(
        wt,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "-U0",
            "--no-renames",
            strip(&mb.stdout),
            "HEAD",
        ],
    )
    .filter(|o| o.code == 0) else {
        return Err(unreadable);
    };
    let mut ranges = Ranges::new();
    let (mut path, mut header): (Option<String>, bool) = (None, false);
    for line in splitlines(&diff.stdout, false) {
        if line.starts_with("diff --git ") {
            (path, header) = (None, true);
        } else if header && line.starts_with("+++ ") {
            path = line.strip_prefix("+++ b/").map(String::from);
        } else if line.starts_with("@@ ") {
            header = false;
            if let (Some(p), Some(m)) = (&path, HUNK.match_start(line)) {
                let start: i64 = m.get(1).unwrap().as_str().parse().unwrap_or(0);
                let count: i64 = m.get(2).map_or(1, |c| c.as_str().parse().unwrap_or(0));
                ranges
                    .entry(p.clone())
                    .or_default()
                    .push((start, start + count.max(1) - 1));
            }
        }
    }
    Ok(ranges)
}

/// (status, resolved, reason) for one cited path and its lines: ok, no-file, bad-line or off-diff.
#[cfg_attr(not(test), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
fn ground_citation(
    path: &str,
    lines: &[i64],
    wt: &Path,
    folder: &Path,
    tracked: &[String],
    ranges: Option<&Ranges>,
    near: bool,
    span: Option<(i64, i64)>,
) -> (String, String, String) {
    let regular = |root: &Path, rel: &str| {
        let file = root.join(rel);
        let (Ok(root), Ok(real)) = (resolve(root), resolve(&file)) else {
            return false;
        };
        file.is_file()
            && !is_link(&file).unwrap_or(true)
            && real.ancestors().skip(1).any(|a| a == root)
    };
    let files: Option<Vec<String>> = if regular(wt, path) {
        Some(vec![path.to_string()])
    } else if !path.contains('/')
        && tracked
            .iter()
            .any(|t| t.rsplit('/').next() == Some(path) && regular(wt, t))
    {
        Some(
            tracked
                .iter()
                .filter(|t| t.rsplit('/').next() == Some(path) && regular(wt, t))
                .cloned()
                .collect(),
        )
    } else if JOB_REL.is_full(path) && regular(folder, path) {
        None
    } else {
        return (
            "no-file".into(),
            "no-file".into(),
            format!("no file at {path} in the worktree or job folder"),
        );
    };
    let count = |p: PathBuf| {
        let bytes = std::fs::read(p).unwrap_or_default();
        bytes_lines(&bytes)
    };
    let (resolved, counts) = match &files {
        None => (format!("job:{path}"), vec![count(folder.join(path))]),
        Some(files) => (
            files.join(", "),
            files
                .iter()
                .map(|f| count(wt.join(f)))
                .collect::<Vec<i64>>(),
        ),
    };
    if let Some(missing) = lines
        .iter()
        .find(|line| !counts.iter().any(|n| 1 <= **line && **line <= *n))
    {
        let reason = if counts.len() == 1 {
            format!("{resolved} has {} lines", counts[0])
        } else {
            format!(
                "{} files named {path}, none has line {missing}",
                counts.len()
            )
        };
        return ("bad-line".into(), resolved, reason);
    }
    if let (true, Some(ranges), Some(files)) = (near, ranges, &files) {
        let changed: Vec<(i64, i64)> = files
            .iter()
            .flat_map(|f| ranges.get(f).cloned().unwrap_or_default())
            .collect();
        if changed.is_empty() {
            return (
                "off-diff".into(),
                resolved.clone(),
                format!("{resolved} is not changed on this branch"),
            );
        }
        let spans: Vec<(i64, i64)> = lines.iter().map(|l| (*l, *l)).chain(span).collect();
        if !spans.iter().any(|(lo, hi)| {
            changed
                .iter()
                .any(|(start, end)| start - GROUNDING_NEAR <= *hi && *lo <= end + GROUNDING_NEAR)
        }) {
            return (
                "off-diff".into(),
                resolved.clone(),
                format!("more than {GROUNDING_NEAR} lines from every change to {resolved}"),
            );
        }
    }
    ("ok".into(), resolved, String::new())
}

/// `len(bytes.splitlines())`.
fn bytes_lines(bytes: &[u8]) -> i64 {
    let mut n = 0;
    let mut i = 0;
    let mut start = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                n += 1;
                start = i + 1;
            }
            b'\n' => {
                n += 1;
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    n + i64::from(start < bytes.len())
}

/// One unverified citation row for grounding.md.
#[cfg_attr(not(test), allow(dead_code))]
struct GroundRow {
    review: String,
    citation: String,
    status: String,
    reason: String,
}

/// grounding.md's text for a round's unverified rows.
#[cfg_attr(not(test), allow(dead_code))]
fn grounding_report(
    rows: &[GroundRow],
    head: Option<&str>,
    base: &str,
    diff_reason: Option<&str>,
    checked: usize,
    files: usize,
) -> String {
    let cell = |t: &str| printable_line(t, 200).replace('|', "/");
    let mut out = vec![
        if rows.is_empty() {
            "GROUNDING: CLEAN".to_string()
        } else {
            format!("GROUNDING: UNVERIFIED {}", rows.len())
        },
        String::new(),
        format!("Head: {}", head.unwrap_or("None")),
        match diff_reason {
            None => format!("Diff: origin/{base}"),
            Some(reason) => format!("Diff: unreadable: {}", cell(reason)),
        },
        format!("Checked: {checked} citations in {files} review files"),
    ];
    if !rows.is_empty() {
        out.extend([
            String::new(),
            "| Review | Citation | Status | Reason |".into(),
            "| --- | --- | --- | --- |".into(),
        ]);
        for r in rows.iter().take(GROUNDING_ROWS_MAX) {
            out.push(format!(
                "| {} | `{}` | {} | {} |",
                cell(&r.review),
                cell(&r.citation),
                r.status,
                cell(&r.reason)
            ));
        }
        if rows.len() > GROUNDING_ROWS_MAX {
            out.extend([
                String::new(),
                format!("And {} more.", rows.len() - GROUNDING_ROWS_MAX),
            ]);
        }
    }
    out.extend([
        String::new(),
        "Unverified means the cited path or line did not check out. The finding may still be right. No verdict changed.".into(),
    ]);
    out.join("\n") + "\n"
}

fn grounding(_home: &Path, _slug: &str, _job_id: &str, round_text: &str, _o: &mut Out) -> Res<()> {
    if !ROUND.is_full(round_text) {
        return Err(fail(
            format!("bad round {}. {GROUNDING_USAGE}", repr(round_text)),
            2,
        ));
    }
    // Deferred to integration: 2194-port-claims and its hold sub-job
    Err(not_ported("grounding"))
}

/// Check each path:line a round's reviews cite against the job worktree, write grounding.md and the board.
pub fn grounding_cmd(call: &Call) -> Res<()> {
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    flushed(|o| grounding(&home, &slug, &job_id, &call.args[1], o))
}

const VERDICT_USAGE: &str =
    "usage: verdict <project-slug>/<job-id> <round> <reviewer> <PASS|CHANGES|SKIPPED|pending>";

fn verdict(
    _home: &Path,
    _slug: &str,
    _job_id: &str,
    _n: u64,
    _reviewer: &str,
    _value: &str,
    _o: &mut Out,
) -> Res<()> {
    // Deferred to integration: 2194-port-claims
    Err(not_ported("verdict"))
}

/// Set one reviewer's verdict in the board's rounds entry N in one CAS write, keeping the rest.
pub fn verdict_cmd(call: &Call) -> Res<()> {
    let (round_text, reviewer, value) =
        (&call.args[1], call.args[2].as_str(), call.args[3].as_str());
    if !ROUND.is_full(round_text)
        || !TRACER_REVIEWERS.contains(&reviewer)
        || !["PASS", "CHANGES", "pending", "SKIPPED"].contains(&value)
        || value == "SKIPPED" && !["ux", "ui"].contains(&reviewer)
    {
        return Err(fail(VERDICT_USAGE, 2));
    }
    let (home, slug, job_id) = r2_target(&call.args[0], true)?;
    let n: u64 = round_text.parse().unwrap_or(u64::MAX);
    flushed(|o| verdict(&home, &slug, &job_id, n, reviewer, value, o))
}

// ---- not ported yet ----

pub fn lesson_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("lesson"))
}

pub fn batch_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("batch"))
}

pub fn stack_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("stack"))
}

pub fn agent_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("agent"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnmatch_ports_fnmatchcase() {
        assert!(fnmatch("dashboard/app.js", "dashboard/*"));
        assert!(fnmatch("a/b/c.css", "*.css"));
        assert!(fnmatch("x1", "x?"));
        assert!(!fnmatch("x12", "x?"));
        assert!(fnmatch("b", "[a-c]"));
        assert!(!fnmatch("b", "[!a-c]"));
        assert!(fnmatch("[", "["));
        assert!(fnmatch("]", "[]]"));
        assert!(!fnmatch("A", "a"));
    }

    #[test]
    fn median_matches_statistics() {
        assert_eq!(median(&mut vec![3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut vec![4.0, 1.0, 2.0, 3.0]), 2.5);
    }

    /// The exit code and stderr of a failed result.
    fn failed<T>(r: Res<T>) -> (i32, String) {
        match r {
            Ok(_) => panic!("expected a failure"),
            Err(e) => (e.code, e.stderr),
        }
    }

    mod routes {
        use super::super::*;
        use super::failed;
        use crate::shared::fs::temp_dir;
        use std::fs;

        const ROUTES: &str = "---\nalpha: provider-a model-a\nbeta: provider-b vendor/model-b[1m]\n---\n\nThis host's routes.\n";

        fn home(name: &str, routes: &str) -> PathBuf {
            let home = temp_dir(name);
            fs::write(home.join("routes.md"), routes).unwrap();
            home
        }

        fn run(home: &Path, args: &[&str]) -> (Res<()>, Out) {
            let mut o = Out::default();
            let rest: Vec<String> = args[1..].iter().map(|a| a.to_string()).collect();
            let r = routes(home, args[0], &rest, &mut o);
            (r, o)
        }

        fn limits(home: &Path) -> Value {
            loads(&fs::read_to_string(home.join("route-limits.json")).unwrap()).unwrap()
        }

        #[test]
        fn parse_route() {
            let owned = |p: &str, m: &str, e: Option<&str>| {
                Some((p.to_string(), m.to_string(), e.map(String::from)))
            };
            assert_eq!(
                super::super::parse_route("provider-a model-a low"),
                owned("provider-a", "model-a", Some("low"))
            );
            assert_eq!(
                super::super::parse_route("provider-a model-a"),
                owned("provider-a", "model-a", None)
            );
            for value in [
                "provider-a",
                "provider-a model-a extra",
                "provider/a model-a",
                "provider-a model;rm",
                "a b low c",
            ] {
                assert_eq!(super::super::parse_route(value), None, "{value}");
            }
        }

        #[cfg(unix)]
        #[test]
        fn pick_runtime() {
            let home = home("routes-runtime", ROUTES);
            let paseo = home.join("paseo");
            fs::create_dir(&paseo).unwrap();
            let config = paseo.join("config.json");
            std::env::set_var("PASEO_HOME", &paseo);
            let providers = json!({"claude-factory": {"extends": "claude"}, "claude-x": {"extends": "opencode"},
                                   "two-hop": {"extends": "claude-factory"}, "plain": {"label": "no extends"}});
            fs::write(
                &config,
                json!({"agents": {"providers": providers}}).to_string(),
            )
            .unwrap();
            for (provider, runtime) in [
                ("claude-factory", "claude"),
                ("claude-x", "opencode"),
                ("two-hop", "claude"),
                ("plain", "plain"),
                ("missing", "missing"),
            ] {
                assert_eq!(provider_runtime(provider).unwrap(), runtime);
            }
            fs::remove_file(&config).unwrap();
            assert_eq!(provider_runtime("missing").unwrap(), "missing");
            for (text, message) in [
                ("{", "cannot read"),
                (r#"{"agents": {"providers": []}}"#, "cannot read"),
                (
                    r#"{"agents": {"providers": {"missing": {"extends": 5}}}}"#,
                    "bad extends under provider 'missing'",
                ),
                (
                    r#"{"agents": {"providers": {"missing": {"extends": "loop"}, "loop": {"extends": "missing"}}}}"#,
                    "bad extends under provider",
                ),
            ] {
                fs::write(&config, text).unwrap();
                let (code, err) = failed(provider_runtime("missing"));
                assert_eq!(code, 2);
                assert!(err.contains(message), "{text}: {err}");
            }
            fs::write(&config, "{}").unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&config, fs::Permissions::from_mode(0o000)).unwrap();
            if fs::read(&config).is_err() {
                let (_, err) = failed(provider_runtime("missing"));
                assert!(err.contains(&format!(
                    "cannot read {}: routes pick needs it",
                    config.display()
                )));
            }
            std::env::remove_var("PASEO_HOME");
        }

        #[test]
        fn limit_reads_routes_once() {
            let home = home("routes-limit-once", ROUTES);
            let (r, o) = run(&home, &["limit", "alpha"]);
            r.unwrap();
            let printed = loads(&o.out).unwrap();
            assert_eq!(printed["route"], "alpha");
            assert_eq!(printed["limited_until"], limits(&home)["alpha"]);
        }

        #[test]
        fn limit_writes_and_prunes() {
            let home = home("routes-limit-prunes", ROUTES);
            fs::write(
                home.join("route-limits.json"),
                r#"{"beta": "2001-01-01T00:00:00Z"}"#,
            )
            .unwrap();
            let before = unix_now();
            let (r, o) = run(&home, &["limit", "alpha"]);
            r.unwrap();
            let after = unix_now();
            let written = limits(&home);
            assert_eq!(
                written.as_object().unwrap().keys().collect::<Vec<_>>(),
                ["alpha"]
            );
            assert_eq!(
                loads(&o.out).unwrap(),
                json!({"route": "alpha", "limited_until": written["alpha"]})
            );
            let until = parse_stamp(written["alpha"].as_str().unwrap()).unwrap();
            assert!(before + 3600 <= until && until <= after + 3600);
            let (r, _) = run(&home, &["limit", "beta", "--minutes", "1440"]);
            r.unwrap();
            let written = limits(&home);
            let mut keys: Vec<&String> = written.as_object().unwrap().keys().collect();
            keys.sort();
            assert_eq!(keys, ["alpha", "beta"]);
            let until = parse_stamp(written["beta"].as_str().unwrap()).unwrap();
            assert!(until > after + 1439 * 60);
        }

        #[test]
        fn limit_and_clear_experiment() {
            let home = home(
                "routes-limit-experiment",
                &ROUTES.replace(
                    "---\n\n",
                    "experiment: pilot provider-c model-c 50\n---\n\n",
                ),
            );
            let (r, o) = run(&home, &["limit", "pilot"]);
            r.unwrap();
            assert_eq!(
                loads(&o.out).unwrap(),
                json!({"experiment": "pilot", "limited_until": limits(&home)["pilot"]})
            );
            let (r, o) = run(&home, &["clear", "pilot"]);
            r.unwrap();
            assert_eq!(o.out, "");
            assert_eq!(limits(&home), json!({}));
        }
    }

    mod experiment {
        use super::super::*;
        use super::failed;
        use crate::shared::fs::temp_dir;
        use std::fs;

        const ROUTES: &str =
            "---\nprimary: provider-a model-a high\nfallback: provider-b model-b\n---\n";
        const UNITS: [&str; 5] = [
            "014-inline-images",
            "015-report-cache",
            "016-doc-lint",
            "017-another",
            "018-more",
        ];

        fn read(name: &str, line: Option<&str>) -> Res<Option<Experiment>> {
            let home = temp_dir(name);
            let text = match line {
                Some(line) => format!("{}{line}{}", &ROUTES[..4], &ROUTES[4..]),
                None => ROUTES.to_string(),
            };
            fs::write(home.join("routes.md"), text).unwrap();
            read_experiment(&home, &["primary".to_string(), "fallback".to_string()])
        }

        fn exits(name: &str, line: &str) {
            let home = temp_dir(name);
            let (code, err) = failed(read(name, Some(line)));
            assert_eq!(code, 2, "{line}");
            assert_eq!(
                err,
                format!(
                    "factory.py: bad experiment in {}: use experiment: <name> <provider> <model> [<effort>] <percent>\n",
                    home.join("routes.md").display()
                )
            );
        }

        #[test]
        fn no_line_reads_none() {
            assert!(read("exp-none", None).unwrap().is_none());
        }

        #[test]
        fn accepts_the_documented_line() {
            let x = read(
                "exp-doc",
                Some("experiment: pilot provider-b model-b high 50\n"),
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                (
                    x.name.as_str(),
                    x.provider.as_str(),
                    x.model.as_str(),
                    x.effort.as_deref(),
                    x.percent
                ),
                ("pilot", "provider-b", "model-b", Some("high"), 50)
            );
            let effort = |line| read("exp-doc", Some(line)).unwrap().unwrap();
            assert_eq!(
                effort("experiment: pilot provider-b model-b 50\n").effort,
                None
            );
            assert_eq!(
                effort("experiment: pilot provider-b model-b max 0\n").percent,
                0
            );
            assert_eq!(
                effort("experiment: pilot provider-b model-b low 100\n").percent,
                100
            );
        }

        #[test]
        fn malformed_lines_exit_2() {
            for line in [
                "experiment: pilot provider-b model-b\n",
                "experiment: pilot provider-b model-b high\n",
                "experiment: pilot provider-b model-b high extra 50\n",
                "experiment: pilot provider-b model;rm 50\n",
                "experiment: pilot pro vider model-b 50\n",
                "experiment: pri mary provider-b model-b 50\n",
                "experiment: pilot provider-b model-b sideways 50\n",
                "experiment: pilot provider-b model-b high 101\n",
                "experiment: pilot provider-b model-b high -1\n",
                "experiment: pilot provider-b model-b high fifty\n",
            ] {
                exits("exp-malformed", line);
            }
        }

        #[test]
        fn the_name_must_not_collide_with_a_route() {
            exits("exp-collide", "experiment: primary provider-b model-b 50\n");
        }

        #[test]
        fn percent_zero_sends_every_id_to_arm_a() {
            for id in UNITS {
                assert_eq!(experiment_arm(id, 0), "a");
            }
        }

        #[test]
        fn percent_100_sends_every_id_to_arm_b() {
            for id in UNITS {
                assert_eq!(experiment_arm(id, 100), "b");
            }
        }

        #[test]
        fn the_same_id_always_lands_in_the_same_arm() {
            for id in UNITS {
                let first = experiment_arm(id, 50);
                assert!((0..5).all(|_| experiment_arm(id, 50) == first), "{id}");
            }
        }

        #[test]
        fn the_documented_buckets() {
            assert_eq!(experiment_arm("014-inline-images", 50), "b");
            assert_eq!(experiment_arm("015-report-cache", 50), "b");
            assert_eq!(experiment_arm("016-doc-lint", 50), "a");
        }

        #[test]
        fn traversal_keys_count_as_coverage_unknown_without_a_probe() {
            let home = temp_dir("exp-traversal");
            let planted = home.join("outside/030-up");
            fs::create_dir_all(&planted).unwrap();
            fs::write(planted.join("spec.md"), "# spec\n").unwrap();
            for id in ["031-reviewer", "032-ok"] {
                let folder = home.join("jobs/alpha").join(id);
                fs::create_dir_all(&folder).unwrap();
                fs::write(folder.join("spec.md"), "# spec\n").unwrap();
            }
            let mut arm = Map::new();
            arm.insert(
                "030-up".into(),
                json!({"id": "030-up", "project": "../outside", "stage": "review"}),
            );
            arm.insert(
                "031-reviewer".into(),
                json!({"id": "031-reviewer", "project": "alpha", "stage": "review", "rounds": [{"../../x": "PASS"}]}),
            );
            arm.insert(
                "032-ok".into(),
                json!({"id": "032-ok", "project": "alpha", "stage": "review"}),
            );
            let follow = json!({"rework_of": [], "surfaced": [], "unknown_window": 0});
            let metrics = arm_metrics(&arm, follow, &home.join("jobs"));
            assert_eq!(metrics["coverage_unknown"], 2);
        }
    }

    mod trial {
        use super::super::*;

        const TRIAL: &str = "factory-spec-v8";

        fn trial(over: Value) -> Value {
            let mut t = json!({"project": "factory", "stage": "spec", "version": 8, "run": "20261111T090000Z-a1b2c3",
                               "summary": "Spec note", "champion_version": 7, "champion": "old", "challenger": "new",
                               "opened": "2026-11-12T09:00:00Z", "status": "open"});
            for (k, v) in over.as_object().unwrap() {
                t[k] = v.clone();
            }
            if t.get("id").is_none() {
                let id = format!(
                    "{}-{}-v{}",
                    t["project"].as_str().unwrap(),
                    t["stage"].as_str().unwrap(),
                    t["version"]
                );
                t["id"] = json!(id);
            }
            t
        }

        fn running(trials: Vec<Value>) -> Value {
            json!({"updated": "2026-11-12T09:00:00Z", "jobs": [], "trials": trials,
                   "autonomy": {"paused": false, "projects": ["factory"], "self": []}})
        }

        fn arm_ids(trial_id: &str, arm: &str, count: usize) -> Vec<String> {
            (2100..)
                .map(|n| format!("{n}-job"))
                .filter(|id| trial_arm(trial_id, id) == arm)
                .take(count)
                .collect()
        }

        #[test]
        fn arm_is_a_stable_half_split_per_trial() {
            assert_eq!(
                trial_arm(TRIAL, "2100-x"),
                experiment_arm(&format!("{TRIAL}/2100-x"), 50)
            );
            let arms: Vec<&str> = (2100..2300)
                .map(|n| trial_arm(TRIAL, &format!("{n}-x")))
                .collect();
            let a = arms.iter().filter(|x| **x == "a").count();
            assert!(70 < a && a < 130);
            assert!((2100..2300)
                .zip(&arms)
                .all(|(n, arm)| trial_arm(TRIAL, &format!("{n}-x")) == *arm));
        }

        #[test]
        fn open_trial_for_and_pause() {
            let data = running(vec![
                trial(json!({"status": "kept"})),
                trial(json!({"stage": "build"})),
                trial(json!({"project": "sample", "version": 2})),
            ]);
            assert!(open_trial_for(&data, "factory", "spec").is_none());
            assert_eq!(
                open_trial_for(&data, "factory", "build").unwrap()["id"],
                "factory-build-v8"
            );
            assert!(open_trial_for(&json!({"jobs": []}), "factory", "spec").is_none());
            let mut reverted = vec![
                trial(json!({"version": 5, "status": "reverted"})),
                trial(json!({"version": 6, "status": "reverted"})),
            ];
            assert!(trials_paused(&running(reverted.clone()), "factory"));
            assert!(!trials_paused(&running(reverted.clone()), "sample"));
            assert!(!trials_paused(&running(reverted[..1].to_vec()), "factory"));
            reverted[0]["resumed"] = json!(now());
            assert!(!trials_paused(&running(reverted), "factory"));
        }

        #[test]
        fn revert_failed_neither_blocks_nor_pauses() {
            let failed = running(vec![
                trial(json!({"version": 5, "status": "revert-failed"})),
                trial(json!({"version": 6, "status": "revert-failed"})),
            ]);
            assert!(open_trial_for(&failed, "factory", "spec").is_none());
            assert!(!trials_paused(&failed, "factory"));
        }

        #[test]
        fn trial_jobs_need_the_project_and_a_start_at_or_after_opened() {
            let t = trial(json!({"opened": "2026-11-12T09:00:00Z"}));
            let (a, b) = (
                arm_ids(TRIAL, "a", 1).remove(0),
                arm_ids(TRIAL, "b", 1).remove(0),
            );
            let jobs = json!({"jobs": [
                {"id": a, "project": "factory", "started": "2026-11-12T09:00:00Z"},
                {"id": b, "project": "factory", "started": "2026-11-13T09:00:00Z"},
                {"id": "2001-early", "project": "factory", "started": "2026-11-12T08:59:59Z"},
                {"id": "2002-none", "project": "factory"},
                {"id": "2003-other", "project": "sample", "started": "2026-11-13T09:00:00Z"}]});
            let arms = trial_jobs(&jobs, &t);
            assert_eq!(arms[0].keys().collect::<Vec<_>>(), [&a]);
            assert_eq!(arms[1].keys().collect::<Vec<_>>(), [&b]);
        }

        #[test]
        fn malformed_trial_record_is_ignored() {
            let data = json!({"trials": [
                trial(json!({"project": "sample", "id": "sample-spec-v9"})),
                trial(json!({"project": "sample", "stage": "../x"})),
                trial(json!({"project": "../sample"})),
                trial(json!({"project": "sample", "champion_version": "7"})),
            ]});
            assert!(open_trial_for(&data, "sample", "spec").is_none());
            let good = json!({"trials": [trial(json!({"project": "sample"}))]});
            assert!(open_trial_for(&good, "sample", "spec").is_some());
        }
    }

    /// A git command in dir with a clean config and a fixed author, as the Python tests run it.
    #[cfg(unix)]
    pub(super) fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    mod repeats {
        use super::super::*;
        use crate::shared::fs::temp_dir;
        use crate::shared::http::tests::{home_for, serve};
        use std::fs;

        const PASS: &str = "VERDICT: PASS\n\nNothing to change.\n";
        const CODE: &str = "VERDICT: CHANGES\n\nThe export route skips the person check.\nSignature: worker/export.js#missing-person-check\n\nNit: rename `rowsToCsv`.\n";
        const SIGNATURES: [&str; 4] = [
            "missing-csv-route",
            "missing-person-check",
            "csv-quote-escape",
            "missing-header-row",
        ];

        fn write(home: &Path, n: usize, texts: &[(&str, &str)]) {
            let folder = home.join(format!("jobs/sample/042-csv-export/round-{n}"));
            fs::create_dir_all(&folder).unwrap();
            for name in TRACER_REVIEWERS {
                let text = texts
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map_or(PASS, |(_, v)| v);
                fs::write(folder.join(format!("review-{name}.md")), text).unwrap();
            }
        }

        fn check(home: &Path, round: &str) -> Value {
            let mut o = Out::default();
            repeats(home, "sample", "042-csv-export", round, &mut o).unwrap();
            loads(&o.out).unwrap()
        }

        fn changes(slug: &str) -> String {
            format!(
                "VERDICT: CHANGES\n\nThe export still fails.\nSignature: worker/export.js#{slug}\n"
            )
        }

        fn churned(decision: &str, churn: Value, repeats: Value) -> Value {
            json!({"decision": decision, "round": 4, "previous": 3, "repeats": repeats, "churn": churn})
        }

        fn churn() -> Value {
            json!([{"path": "worker/export.js", "rounds": [2, 3, 4]}])
        }

        /// Commit once per round, changing worker/export.js in every round but skip, and record each head.
        #[cfg(unix)]
        fn build(name: &str, skip: Option<usize>, merge_base: bool) -> (PathBuf, PathBuf) {
            use super::git_in;
            let home = temp_dir(name);
            let wt = home.join("worktrees/sample/042-csv-export");
            fs::create_dir_all(wt.join("worker")).unwrap();
            fs::create_dir_all(wt.join("docs")).unwrap();
            git_in(&wt, &["init", "-q"]);
            for n in 1..=4 {
                write(&home, n, &[("code", &changes(SIGNATURES[n - 1]))]);
                if merge_base && n > 1 {
                    git_in(&wt, &["checkout", "-q", "base"]);
                    fs::write(wt.join("docs/base.md"), format!("base {n}\n")).unwrap();
                    git_in(&wt, &["add", "-A"]);
                    git_in(&wt, &["commit", "-q", "-m", &format!("base {n}")]);
                    git_in(&wt, &["checkout", "-q", "-"]);
                    git_in(&wt, &["merge", "-q", "--no-edit", "base"]);
                }
                if Some(n) != skip {
                    fs::write(wt.join("worker/export.js"), format!("// round {n}\n")).unwrap();
                }
                fs::write(wt.join(format!("round-{n}.md")), format!("round {n}\n")).unwrap();
                git_in(&wt, &["add", "-A"]);
                git_in(&wt, &["commit", "-q", "-m", &format!("round {n}")]);
                let head = git_in(&wt, &["rev-parse", "HEAD"]);
                plan(&home, n, &head);
                if merge_base && n == 1 {
                    git_in(&wt, &["branch", "base"]);
                }
            }
            (home, wt)
        }

        fn plan(home: &Path, n: usize, head: &str) {
            fs::write(
                home.join(format!(
                    "jobs/sample/042-csv-export/round-{n}/review-plan.json"
                )),
                json!({"round": n, "head": head}).to_string(),
            )
            .unwrap();
        }

        #[test]
        fn a_repeat_is_recorded_on_the_board() {
            let board = r#"{"updated": "", "jobs": [{"id": "042-csv-export", "project": "sample", "stage": "review"}]}"#;
            let (origin, seen) = serve(vec![
                (200, board, vec![("ETag", "\"e1\"")]),
                (200, "", vec![("ETag", "\"e2\"")]),
            ]);
            let home = home_for("repeats-record", &origin);
            write(&home, 1, &[("code", CODE)]);
            write(&home, 2, &[("code", CODE)]);
            assert_eq!(check(&home, "2")["decision"], "repeat");
            let put = loads(std::str::from_utf8(&seen.lock().unwrap()[1].body).unwrap()).unwrap();
            let row = &put["jobs"][0]["repeated"];
            assert_eq!(row.as_array().unwrap().len(), 1);
            assert_eq!(
                (
                    &row[0]["round"],
                    &row[0]["first"],
                    &row[0]["reviewer"],
                    &row[0]["signature"]
                ),
                (
                    &json!(2),
                    &json!(1),
                    &json!("code"),
                    &json!("worker/export.js#missing-person-check")
                )
            );
            assert!(parse_stamp(row[0]["at"].as_str().unwrap()).is_some());
            // The same round again keeps one row for the repeat, whatever second it lands in.
            let again = put.to_string();
            let again: &'static str = Box::leak(again.into_boxed_str());
            let (origin, seen) = serve(vec![
                (200, again, vec![("ETag", "\"e2\"")]),
                (200, "", vec![("ETag", "\"e3\"")]),
            ]);
            let home2 = home_for("repeats-record-2", &origin);
            write(&home2, 1, &[("code", CODE)]);
            write(&home2, 2, &[("code", CODE)]);
            assert_eq!(check(&home2, "2")["decision"], "repeat");
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Some(put) = seen.lock().unwrap().get(1) {
                let put = loads(std::str::from_utf8(&put.body).unwrap()).unwrap();
                assert_eq!(put["jobs"][0]["repeated"].as_array().unwrap().len(), 1);
            }
            // A pass in round 1 ends the repeat and writes nothing.
            let home3 = temp_dir("repeats-record-3");
            write(&home3, 1, &[]);
            write(&home3, 2, &[("code", CODE)]);
            assert_eq!(check(&home3, "2")["decision"], "continue");
        }

        #[cfg(unix)]
        #[test]
        fn a_file_changed_in_three_consecutive_rounds_is_churn() {
            let (home, _) = build("churn-three", None, false);
            assert_eq!(check(&home, "4"), churned("churn", churn(), json!([])));
        }

        #[cfg(unix)]
        #[test]
        fn round_3_never_churns() {
            let (home, _) = build("churn-round-3", None, false);
            assert_eq!(
                check(&home, "3"),
                json!({"decision": "continue", "round": 3, "previous": 2, "repeats": [], "churn": []})
            );
        }

        #[cfg(unix)]
        #[test]
        fn a_file_skipped_in_one_round_is_not_churn() {
            let (home, _) = build("churn-skipped", Some(3), false);
            assert_eq!(check(&home, "4"), churned("continue", json!([]), json!([])));
        }

        #[cfg(unix)]
        #[test]
        fn a_missing_or_malformed_round_head_is_not_churn() {
            let (home, _) = build("churn-malformed", None, false);
            let path = home.join("jobs/sample/042-csv-export/round-3/review-plan.json");
            let head = loads(&fs::read_to_string(&path).unwrap()).unwrap()["head"]
                .as_str()
                .unwrap()
                .to_string();
            for text in [
                None,
                Some("{".to_string()),
                Some(json!({"head": &head[..39]}).to_string()),
                Some(json!(["x"]).to_string()),
            ] {
                let _ = fs::remove_file(&path);
                if let Some(text) = &text {
                    fs::write(&path, text).unwrap();
                }
                assert_eq!(
                    check(&home, "4"),
                    churned("continue", json!([]), json!([])),
                    "{text:?}"
                );
            }
        }

        #[cfg(unix)]
        #[test]
        fn a_non_ancestor_head_is_not_churn() {
            use super::git_in;
            let (home, wt) = build("churn-ancestor", None, false);
            let tree = git_in(&wt, &["rev-parse", "HEAD^{tree}"]);
            plan(
                &home,
                2,
                &git_in(&wt, &["commit-tree", &tree, "-m", "rewritten"]),
            );
            assert_eq!(check(&home, "4"), churned("continue", json!([]), json!([])));
        }

        #[cfg(unix)]
        #[test]
        fn no_worktree_is_not_churn() {
            let (home, wt) = build("churn-no-worktree", None, false);
            fs::remove_dir_all(&wt).unwrap();
            assert_eq!(check(&home, "4"), churned("continue", json!([]), json!([])));
        }

        #[cfg(unix)]
        #[test]
        fn a_repeat_wins_over_churn() {
            let (home, _) = build("churn-repeat", None, false);
            write(&home, 4, &[("code", &changes(SIGNATURES[2]))]);
            let repeats = json!([{"reviewer": "code", "signature": "worker/export.js#csv-quote-escape", "first": 3}]);
            assert_eq!(check(&home, "4"), churned("repeat", churn(), repeats));
        }

        #[cfg(unix)]
        #[test]
        fn a_base_merge_is_not_churn() {
            let (home, _) = build("churn-base-merge", Some(3), true);
            assert_eq!(check(&home, "4"), churned("continue", json!([]), json!([])));
        }

        #[cfg(unix)]
        #[test]
        fn own_churn_survives_base_merges() {
            let (home, _) = build("churn-own", None, true);
            assert_eq!(check(&home, "4"), churned("churn", churn(), json!([])));
        }
    }

    mod review_plan {
        use super::super::*;
        use crate::shared::fs::temp_dir;
        use crate::shared::http::tests::serve;
        use std::fs;

        const JOB: &str = "001-sample-job";
        const SPEC: &str = "# Spec: sample\n\n## Acceptance criteria\n\n| # | Criterion |\n| 1 | The docs read well |\n";
        const FULL: &str = "CHANGES\n\n## Full review\n\nAuth changed.\n";
        const ROUTES: &str = "---\nprimary: provider-a model-big medium\nreview-low: provider-a model-small low\nreview-medium: provider-a model-mid medium\n---\n";
        const BOARD: &str = r#"{"updated": "", "jobs": [{"id": "001-sample-job", "project": "sample", "title": "Sample job", "stage": "review"}]}"#;

        fn low() -> String {
            format!("{SPEC}\n## Review tier\n\nTier: low\n\n- Docs only.\n")
        }

        fn medium() -> String {
            low().replace("Tier: low", "Tier: medium")
        }

        fn small() -> Value {
            json!({"name": "review-low", "provider": "provider-a", "model": "model-small", "effort": "low"})
        }

        fn mid() -> Value {
            json!({"name": "review-medium", "provider": "provider-a", "model": "model-mid", "effort": "medium"})
        }

        fn nulls() -> Value {
            json!({"security": null, "code": null})
        }

        fn all() -> Value {
            json!(["security", "ux", "ui", "code"])
        }

        pub(in super::super) struct Fix {
            pub home: PathBuf,
            pub folder: PathBuf,
            pub wt: PathBuf,
            pub project: PathBuf,
        }

        impl Fix {
            pub fn new(name: &str) -> Fix {
                let home = temp_dir(name);
                fs::create_dir(home.join("projects")).unwrap();
                let project = home.join("projects/sample.md");
                fs::write(
                    &project,
                    "---\nrepo: example/sample\nbase: main\n---\n# Sample\n",
                )
                .unwrap();
                let folder = home.join("jobs/sample").join(JOB);
                fs::create_dir_all(&folder).unwrap();
                fs::write(folder.join("spec.md"), SPEC).unwrap();
                let wt = home.join("worktrees/sample").join(JOB);
                FETCH_URL.with(|u| {
                    *u.borrow_mut() = Some(home.join("origin.git").display().to_string())
                });
                Fix {
                    home,
                    folder,
                    wt,
                    project,
                }
            }

            pub fn git(&self, args: &[&str]) -> String {
                super::git_in(&self.wt, args)
            }

            pub fn worktree(&self, files: &[&str]) {
                fs::create_dir_all(&self.wt).unwrap();
                self.git(&["init", "-q"]);
                fs::write(self.wt.join("README.md"), "base\n").unwrap();
                self.git(&["add", "-A"]);
                self.git(&["commit", "-q", "-m", "base"]);
                let origin = self.home.join("origin.git");
                super::git_in(
                    &self.home,
                    &["init", "-q", "--bare", &origin.display().to_string()],
                );
                self.git(&["remote", "add", "origin", &origin.display().to_string()]);
                self.git(&["push", "-q", "origin", "HEAD:refs/heads/main"]);
                self.commit(files);
            }

            pub fn commit(&self, files: &[&str]) {
                for rel in files {
                    let path = self.wt.join(rel);
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(path, "change\n").unwrap();
                }
                self.git(&["add", "-A"]);
                self.git(&["commit", "-q", "-m", "change"]);
            }

            pub fn head(&self) -> String {
                self.git(&["rev-parse", "HEAD"])
            }

            fn serve(&self) {
                let (origin, _) = serve(vec![(200, BOARD, vec![])]);
                fs::write(
                    self.home.join("config.md"),
                    format!("---\napi_url: {origin}\n---\n"),
                )
                .unwrap();
            }

            fn plan(&self, round: &str) -> (Res<()>, Out) {
                self.serve();
                let mut o = Out::default();
                let r = review_plan(&self.home, "sample", JOB, round, &mut o);
                (r, o)
            }

            fn floor_cmd(&self) -> (Res<()>, Out) {
                self.serve();
                let mut o = Out::default();
                let r = review_floor(&self.home, "sample", JOB, &mut o);
                (r, o)
            }

            fn review(&self, round: &str, name: &str) -> PathBuf {
                self.folder.join(format!("round-{round}/review-{name}.md"))
            }

            fn skipped(&self, round: &str) -> Value {
                let (r, o) = self.plan(round);
                r.unwrap();
                assert_eq!(o.err, "");
                let plan = loads(&o.out).unwrap();
                assert_eq!(plan["round"], json!(round.parse::<u64>().unwrap()));
                assert_eq!(
                    (&plan["run"], &plan["skipped"]),
                    (&json!(["security", "code"]), &json!(["ux", "ui"]))
                );
                for name in ["ux", "ui"] {
                    let text = fs::read_to_string(self.review(round, name)).unwrap();
                    assert!(text.starts_with(
                        "VERDICT: SKIPPED\n\nfactory.py review-plan skipped this review: "
                    ));
                }
                plan
            }

            fn runs_all(&self, round: &str) -> Value {
                let (r, o) = self.plan(round);
                r.unwrap();
                assert_eq!(o.err, "");
                let plan = loads(&o.out).unwrap();
                assert_eq!((&plan["run"], &plan["skipped"]), (&all(), &json!([])));
                for name in ["ux", "ui"] {
                    assert!(!self.review(round, name).exists());
                }
                plan
            }

            fn recorded(&self, round: &str) -> Value {
                loads(
                    &fs::read_to_string(
                        self.folder.join(format!("round-{round}/review-plan.json")),
                    )
                    .unwrap(),
                )
                .unwrap()
            }

            fn nothing_written(&self, round: &str, r: Res<()>, o: &Out, code: i32) -> String {
                assert_eq!(o.out, "");
                let (got, err) = super::failed(r);
                assert_eq!(got, code, "{err}");
                for name in ["ux", "ui"] {
                    assert!(!self.review(round, name).exists());
                }
                err
            }

            fn fix_round(&self, ux: &str) {
                self.worktree(&["web/dashboard.html"]);
                self.runs_all("1");
                assert_eq!(
                    self.recorded("1"),
                    json!({"round": 1, "head": self.head(), "protected": [], "tier": "high",
                           "routes": {"security": null, "ux": null, "ui": null, "code": null}})
                );
                fs::write(self.review("1", "ux"), ux).unwrap();
                fs::write(self.review("1", "ui"), "VERDICT: PASS\n").unwrap();
            }
        }

        #[cfg(unix)]
        #[test]
        fn docs_only_skips() {
            let f = Fix::new("plan-docs");
            f.worktree(&["docs/trust.md"]);
            assert!(f.skipped("1")["reason"]
                .as_str()
                .unwrap()
                .contains("none of the 1 changed files"));
        }

        #[cfg(unix)]
        #[test]
        fn runtime_only_skips() {
            let f = Fix::new("plan-runtime");
            f.worktree(&["skills/factory/runtime/x.py"]);
            f.skipped("1");
        }

        #[cfg(unix)]
        #[test]
        fn dashboard_runs_all() {
            let f = Fix::new("plan-dashboard");
            f.worktree(&["web/dashboard.html"]);
            assert_eq!(
                f.runs_all("1")["reason"],
                "web/dashboard.html matches *.html"
            );
        }

        #[cfg(unix)]
        #[test]
        fn visual_criterion_runs_all() {
            let f = Fix::new("plan-visual");
            fs::write(
                f.folder.join("spec.md"),
                format!("{SPEC}| 2 | Capture before and after screenshots |\n"),
            )
            .unwrap();
            f.worktree(&["docs/trust.md"]);
            assert_eq!(
                f.runs_all("1")["reason"],
                "spec.md has a visual-evidence acceptance criterion"
            );
        }

        #[cfg(unix)]
        #[test]
        fn project_ui_glob_runs_all() {
            let f = Fix::new("plan-ui-glob");
            let text = fs::read_to_string(&f.project).unwrap();
            fs::write(
                &f.project,
                text + "\n## UI\n\nScreens under `docs/screens/*` count.\n",
            )
            .unwrap();
            f.worktree(&["docs/screens/a.md"]);
            assert_eq!(
                f.runs_all("1")["reason"],
                "docs/screens/a.md matches docs/screens/*"
            );
        }

        #[cfg(unix)]
        #[test]
        fn visible_change_decision_runs_all() {
            let f = Fix::new("plan-visible-decision");
            fs::write(
                f.folder.join("decision.md"),
                "CHANGES\n\n## Visible change\n\nThe CLI prints new copy.\n",
            )
            .unwrap();
            f.worktree(&["docs/trust.md"]);
            assert_eq!(
                f.runs_all("1")["reason"],
                "decision.md names a visible change"
            );
        }

        #[test]
        fn missing_worktree_runs_all() {
            let f = Fix::new("plan-no-worktree");
            let plan = f.runs_all("1");
            assert!(plan["reason"]
                .as_str()
                .unwrap()
                .contains("could not be read"));
            assert!(plan["protected"].is_null());
        }

        #[cfg(unix)]
        #[test]
        fn protected_paths_reported() {
            let f = Fix::new("plan-protected");
            f.worktree(&[
                "skills/factory/runtime/factory.py",
                ".github/workflows/ci.yml",
                "docs/trust.md",
            ]);
            assert_eq!(
                f.skipped("1")["protected"],
                json!([".github/workflows/ci.yml"])
            );
            assert_eq!(
                f.recorded("1"),
                json!({"round": 1, "head": f.head(), "protected": [".github/workflows/ci.yml"], "tier": "high", "routes": nulls()})
            );
        }

        #[test]
        fn repo_check_config_is_protected() {
            let paths: Vec<String> = [
                "tools/assets.mjs",
                "package.json",
                "paseo-plugin/package.json",
                ".gitleaks.toml",
                "mise.toml",
                ".oxfmtrc.json",
                "nx.json",
                "tools/verify.py",
                "tests/cli/support.py",
                "tests/support/browser-server.mjs",
                "docs/trust.md",
                "pnpm-lock.yaml",
                "tools/secret-scan.py",
                "pnpm-workspace.yaml",
                "paseo-plugin/pnpm-workspace.yaml",
                ".npmrc",
                "paseo-plugin/.npmrc",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect();
            assert_eq!(
                touched_protected(&paths),
                [
                    ".gitleaks.toml",
                    ".npmrc",
                    ".oxfmtrc.json",
                    "mise.toml",
                    "nx.json",
                    "package.json",
                    "paseo-plugin/.npmrc",
                    "paseo-plugin/package.json",
                    "paseo-plugin/pnpm-workspace.yaml",
                    "pnpm-workspace.yaml",
                    "tests/cli/support.py",
                    "tests/support/browser-server.mjs",
                    "tools/secret-scan.py",
                    "tools/verify.py",
                ]
            );
        }

        #[cfg(unix)]
        #[test]
        fn later_round_adds_visible_file() {
            let f = Fix::new("plan-later");
            f.worktree(&["docs/trust.md"]);
            f.skipped("1");
            f.commit(&["web/a.css"]);
            assert_eq!(f.runs_all("2")["reason"], "web/a.css matches *.css");
        }

        #[cfg(unix)]
        #[test]
        fn existing_file_exits_6() {
            let f = Fix::new("plan-existing");
            f.worktree(&["docs/trust.md"]);
            fs::create_dir(f.folder.join("round-1")).unwrap();
            fs::write(f.review("1", "ui"), "VERDICT: PASS\n").unwrap();
            let (r, o) = f.plan("1");
            assert_eq!(o.out, "");
            let (code, err) = super::failed(r);
            assert_eq!(code, 6);
            assert!(err.contains("already exists; nothing was written"));
            assert!(!f.review("1", "ux").exists());
        }

        #[cfg(unix)]
        #[test]
        fn bad_round_exits_2() {
            let f = Fix::new("plan-bad-round");
            f.worktree(&["docs/trust.md"]);
            for round in ["0", "x", "-1"] {
                let (code, err) = super::failed(plan_round(round));
                assert_eq!(code, 2);
                assert_eq!(
                    err,
                    format!("factory.py: round must be a positive integer. {REVIEW_PLAN_USAGE}\n")
                );
                for name in ["ux", "ui"] {
                    assert!(!f.review(round, name).exists());
                }
            }
        }

        #[cfg(unix)]
        #[test]
        fn linked_round_folder_exits_6() {
            let f = Fix::new("plan-linked-round");
            f.worktree(&["docs/trust.md"]);
            let real = f.home.join("elsewhere");
            fs::create_dir(&real).unwrap();
            std::os::unix::fs::symlink(&real, f.folder.join("round-1")).unwrap();
            let (r, o) = f.plan("1");
            let err = f.nothing_written("1", r, &o, 6);
            assert!(err.contains("symlink or junction"));
            assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
        }

        #[cfg(unix)]
        #[test]
        fn missing_spec_exits_4() {
            let f = Fix::new("plan-missing-spec");
            f.worktree(&["docs/trust.md"]);
            fs::remove_file(f.folder.join("spec.md")).unwrap();
            let (r, o) = f.plan("1");
            assert!(f
                .nothing_written("1", r, &o, 4)
                .contains("spec.md is missing"));
        }

        #[cfg(unix)]
        #[test]
        fn fix_round_reads_its_own_diff() {
            let f = Fix::new("plan-fix-own");
            f.fix_round("VERDICT: PASS\n");
            f.commit(&["tests/cli/test_export.py"]);
            assert_eq!(
                f.skipped("2")["reason"],
                "none of the 1 files changed since round 1 matches a visible-file glob, and round 1 ux and ui did not ask for changes"
            );
            assert_eq!(
                f.recorded("2"),
                json!({"round": 2, "head": f.head(), "protected": [], "tier": "high", "routes": nulls()})
            );
        }

        #[cfg(unix)]
        #[test]
        fn fix_round_after_ux_changes_runs_all() {
            let f = Fix::new("plan-fix-ux");
            f.fix_round("VERDICT: CHANGES\n");
            f.commit(&["tests/cli/test_export.py"]);
            assert_eq!(
                f.runs_all("2")["reason"],
                "web/dashboard.html matches *.html"
            );
        }

        #[cfg(unix)]
        #[test]
        fn fix_round_without_plan_file_runs_all() {
            let f = Fix::new("plan-fix-no-plan");
            f.fix_round("VERDICT: PASS\n");
            fs::remove_file(f.folder.join("round-1/review-plan.json")).unwrap();
            f.commit(&["tests/cli/test_export.py"]);
            assert_eq!(
                f.runs_all("2")["reason"],
                "web/dashboard.html matches *.html"
            );
        }

        #[cfg(unix)]
        #[test]
        fn fix_round_after_rewrite_runs_all() {
            let f = Fix::new("plan-fix-rewrite");
            f.fix_round("VERDICT: PASS\n");
            f.git(&["commit", "-q", "--amend", "-m", "rewritten"]);
            f.commit(&["tests/cli/test_export.py"]);
            assert_eq!(
                f.runs_all("2")["reason"],
                "web/dashboard.html matches *.html"
            );
        }

        #[cfg(unix)]
        #[test]
        fn fix_round_visible_change_decision_runs_all() {
            let f = Fix::new("plan-fix-visible");
            f.fix_round("VERDICT: PASS\n");
            fs::write(
                f.folder.join("decision.md"),
                "CHANGES\n\n## Visible change\n\nThe CLI prints new copy.\n",
            )
            .unwrap();
            f.commit(&["tests/cli/test_export.py"]);
            assert_eq!(
                f.runs_all("2")["reason"],
                "decision.md names a visible change"
            );
        }

        #[cfg(unix)]
        #[test]
        fn low_tier_routes_reviewers() {
            let f = Fix::new("plan-low-tier");
            fs::write(f.folder.join("spec.md"), low()).unwrap();
            fs::write(f.home.join("routes.md"), ROUTES).unwrap();
            f.worktree(&["docs/trust.md"]);
            let plan = f.skipped("1");
            assert_eq!(
                (&plan["tier"], &plan["tier_reason"]),
                (&json!("low"), &json!("spec.md rates the job low"))
            );
            assert_eq!(plan["routes"], json!({"security": mid(), "code": small()}));
            assert_eq!(
                f.recorded("1"),
                json!({"round": 1, "head": f.head(), "protected": [], "tier": "low",
                       "routes": {"security": mid(), "code": small()}})
            );
            fs::write(
                f.folder.join("spec.md"),
                low().replace("Tier: low", "tier: MEDIUM"),
            )
            .unwrap();
            let plan = f.skipped("2");
            assert_eq!(
                (&plan["tier"], &plan["routes"]),
                (&json!("medium"), &json!({"security": mid(), "code": mid()}))
            );
        }

        #[cfg(unix)]
        #[test]
        fn floors_force_high() {
            let cases: [(&str, String, Option<&str>, &[&str]); 4] = [
                (
                    "decision.md asks for a full review",
                    low(),
                    Some(FULL),
                    &["docs/trust.md"],
                ),
                (
                    "the protected paths could not be read",
                    low(),
                    None,
                    &["docs/trust.md"],
                ),
                (
                    "the branch touches protected paths",
                    low(),
                    None,
                    &[".github/workflows/ci.yml"],
                ),
                (
                    "spec.md has no Review tier line",
                    format!("{SPEC}\nTier: low\n"),
                    None,
                    &["docs/trust.md"],
                ),
            ];
            for (i, (reason, spec, decision, files)) in cases.into_iter().enumerate() {
                let f = Fix::new(&format!("plan-floors-{i}"));
                fs::write(f.home.join("routes.md"), ROUTES).unwrap();
                fs::write(f.folder.join("spec.md"), spec).unwrap();
                if let Some(decision) = decision {
                    fs::write(f.folder.join("decision.md"), decision).unwrap();
                }
                f.worktree(files);
                if reason == "the protected paths could not be read" {
                    FETCH_URL.with(|u| {
                        *u.borrow_mut() = Some(f.home.join("gone.git").display().to_string())
                    });
                }
                let (r, o) = f.plan("1");
                r.unwrap();
                assert_eq!(o.err, "");
                let plan = loads(&o.out).unwrap();
                assert_eq!(
                    (&plan["tier"], plan["tier_reason"].as_str().unwrap()),
                    (&json!("high"), reason)
                );
                let nulls: Map<String, Value> = plan["run"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| (r.as_str().unwrap().to_string(), Value::Null))
                    .collect();
                assert_eq!(plan["routes"], Value::Object(nulls.clone()));
                let recorded = f.recorded("1");
                assert_eq!(
                    (&recorded["tier"], &recorded["routes"]),
                    (&json!("high"), &Value::Object(nulls))
                );
            }
        }

        #[test]
        fn review_floor_never_lowers() {
            let f = Fix::new("plan-floor-never-lowers");
            fs::write(f.folder.join("decision.md"), FULL).unwrap();
            let recorded = json!({"tier": "medium", "full_review": true});
            for spec in [medium(), low()] {
                fs::write(f.folder.join("spec.md"), &spec).unwrap();
                if spec == low() {
                    fs::remove_file(f.folder.join("decision.md")).unwrap();
                }
                let (r, o) = f.floor_cmd();
                r.unwrap();
                assert_eq!(loads(&o.out).unwrap(), recorded);
                assert_eq!(
                    loads(&fs::read_to_string(f.folder.join("review-floor.json")).unwrap())
                        .unwrap(),
                    recorded
                );
            }
            fs::write(
                f.folder.join("spec.md"),
                low().replace("Tier: low", "Tier: high"),
            )
            .unwrap();
            let (r, o) = f.floor_cmd();
            r.unwrap();
            assert_eq!(
                loads(&o.out).unwrap(),
                json!({"tier": "high", "full_review": true})
            );
        }

        #[cfg(unix)]
        #[test]
        fn floor_keeps_recorded_tier() {
            let f = Fix::new("plan-floor-tier");
            fs::write(f.folder.join("spec.md"), medium()).unwrap();
            let (r, o) = f.floor_cmd();
            r.unwrap();
            assert_eq!(o.out, "{\"tier\": \"medium\", \"full_review\": false}\n");
            f.worktree(&["docs/trust.md"]);
            assert_eq!(f.skipped("1")["tier"], "medium");
            fs::write(f.folder.join("spec.md"), low()).unwrap();
            f.commit(&["docs/a.md"]);
            let plan = f.skipped("2");
            assert_eq!(
                (&plan["tier"], &plan["tier_reason"]),
                (
                    &json!("medium"),
                    &json!("review-floor.json records medium, above spec.md's low")
                )
            );
        }

        #[cfg(unix)]
        #[test]
        fn floor_keeps_full_review() {
            let f = Fix::new("plan-floor-full");
            fs::write(f.folder.join("spec.md"), medium()).unwrap();
            fs::write(f.folder.join("decision.md"), FULL).unwrap();
            f.worktree(&["docs/trust.md"]);
            assert_eq!(
                f.skipped("1")["tier_reason"],
                "decision.md asks for a full review"
            );
            assert_eq!(
                loads(&fs::read_to_string(f.folder.join("review-floor.json")).unwrap()).unwrap(),
                json!({"tier": "medium", "full_review": true})
            );
            fs::remove_file(f.folder.join("decision.md")).unwrap();
            f.commit(&["docs/a.md"]);
            let plan = f.skipped("2");
            assert_eq!(
                (&plan["tier"], &plan["tier_reason"]),
                (
                    &json!("high"),
                    &json!("review-floor.json records a full review")
                )
            );
        }

        #[cfg(unix)]
        #[test]
        fn floor_bad_or_linked() {
            let f = Fix::new("plan-floor-bad");
            fs::write(f.folder.join("spec.md"), low()).unwrap();
            f.worktree(&["docs/trust.md"]);
            let floor = f.folder.join("review-floor.json");
            let bads = [
                "not json",
                r#"{"tier": "huge", "full_review": false}"#,
                r#"{"tier": [], "full_review": false}"#,
                r#"{"tier": {}, "full_review": false}"#,
                r#"{"tier": "low"}"#,
                "[]",
            ];
            let high = json!({"tier": "high", "full_review": false});
            for (n, bad) in bads.iter().enumerate() {
                fs::write(&floor, bad).unwrap();
                let (r, o) = f.floor_cmd();
                r.unwrap();
                assert_eq!(loads(&o.out).unwrap(), high);
                assert_eq!(loads(&fs::read_to_string(&floor).unwrap()).unwrap(), high);
                fs::write(&floor, bad).unwrap();
                let (r, o) = f.plan(&(n + 1).to_string());
                r.unwrap();
                assert_eq!(o.err, "");
                let plan = loads(&o.out).unwrap();
                assert_eq!(
                    (&plan["tier"], &plan["tier_reason"]),
                    (
                        &json!("high"),
                        &json!("review-floor.json could not be read")
                    ),
                    "{bad}"
                );
                assert_eq!(loads(&fs::read_to_string(&floor).unwrap()).unwrap(), high);
            }
            fs::remove_file(&floor).unwrap();
            let real = f.home.join("elsewhere.json");
            fs::write(&real, "{\"tier\": \"low\", \"full_review\": false}\n").unwrap();
            std::os::unix::fs::symlink(&real, &floor).unwrap();
            for (r, o) in [f.floor_cmd(), f.plan(&(bads.len() + 1).to_string())] {
                assert_eq!(o.out, "");
                let (code, err) = super::failed(r);
                assert_eq!(code, 6);
                assert!(err.contains("symlink or junction; nothing was written"));
            }
            assert_eq!(
                fs::read_to_string(&real).unwrap(),
                "{\"tier\": \"low\", \"full_review\": false}\n"
            );
        }

        #[cfg(unix)]
        #[test]
        fn no_routes_file_gives_null_routes() {
            let f = Fix::new("plan-no-routes");
            fs::write(f.folder.join("spec.md"), low()).unwrap();
            f.worktree(&["docs/trust.md"]);
            let plan = f.skipped("1");
            assert_eq!((&plan["tier"], &plan["routes"]), (&json!("low"), &nulls()));
        }

        #[cfg(unix)]
        #[test]
        fn bad_review_route_exits_2() {
            let f = Fix::new("plan-bad-route");
            fs::write(f.folder.join("spec.md"), low()).unwrap();
            fs::write(
                f.home.join("routes.md"),
                ROUTES.replace("model-small low", "model-small fast"),
            )
            .unwrap();
            f.worktree(&["docs/trust.md"]);
            let (r, o) = f.plan("1");
            assert!(f
                .nothing_written("1", r, &o, 2)
                .contains("bad review route review-low"));
            assert!(!f.folder.join("round-1").exists());
        }
    }

    mod pre_review {
        use super::super::*;
        use super::git_in;
        use crate::shared::fs::temp_dir;
        use crate::shared::http::tests::serve;
        use std::fs;

        const JOB: &str = "001-sample-job";
        const DEFER: &str = "deferred to integration: 2202-port-retro";
        const SPEC: &str = "# Sample\n\n## Design\n\n- Files: `outside/design.py`\n\n## Tasks\n\n1. Tool\n   - Files: `tools/a.py`\n2. Test\n   - Files: `tests/b.py`\n\n## Risks\n\n- None.\n";

        fn check(name: &str, code: &str) -> Value {
            json!({"name": name, "run": ["python3", "-c", code]})
        }

        struct Fix {
            home: PathBuf,
            folder: PathBuf,
            out: PathBuf,
            tmp: PathBuf,
            logs: PathBuf,
            wt: PathBuf,
            origin: PathBuf,
        }

        impl Fix {
            fn new(name: &str) -> Fix {
                let home = temp_dir(name);
                fs::create_dir(home.join("projects")).unwrap();
                fs::write(
                    home.join("projects/sample.md"),
                    "---\nrepo: example/sample\nbase: main\n---\n# Sample\n",
                )
                .unwrap();
                let folder = home.join("jobs/sample").join(JOB);
                fs::create_dir_all(&folder).unwrap();
                let tmp = home.join("tmp");
                fs::create_dir(&tmp).unwrap();
                TEMP_ROOT.with(|r| *r.borrow_mut() = Some(tmp.clone()));
                let logs = tmp.join(format!("factory-sample-{JOB}/pre-review"));
                let wt = home.join("worktrees/sample").join(JOB);
                fs::create_dir_all(&wt).unwrap();
                git_in(&wt, &["init", "-q"]);
                let origin = home.join("origin.git");
                git_in(
                    &home,
                    &["init", "-q", "--bare", &origin.display().to_string()],
                );
                git_in(
                    &wt,
                    &["remote", "add", "origin", &origin.display().to_string()],
                );
                FETCH_URL.with(|u| *u.borrow_mut() = Some(origin.display().to_string()));
                fs::write(wt.join("README.md"), "base\n").unwrap();
                let f = Fix {
                    out: folder.join("pre-review.md"),
                    home,
                    folder,
                    tmp,
                    logs,
                    wt,
                    origin,
                };
                f.commit(None, None);
                f
            }

            fn git(&self, args: &[&str]) -> String {
                git_in(&self.wt, args)
            }

            fn commit(&self, checks: Option<Value>, text: Option<&str>) {
                if checks.is_some() || text.is_some() {
                    fs::create_dir_all(self.wt.join(".factory")).unwrap();
                    let body = match text {
                        Some(t) => t.to_string(),
                        None => json!({"checks": checks.unwrap()}).to_string(),
                    };
                    fs::write(self.wt.join(".factory/checks.json"), body).unwrap();
                }
                self.git(&["add", "-A"]);
                self.git(&["commit", "-q", "-m", "change"]);
            }

            fn base(&self, checks: Option<Value>, text: Option<&str>) {
                self.commit(checks, text);
                self.git(&["push", "-q", "-f", "origin", "HEAD:refs/heads/main"]);
            }

            fn run_gate(&self, round: Option<i64>, extra: Value) -> (i32, Value, String) {
                let mut job = json!({"id": JOB, "project": "sample", "title": "Sample job", "stage": "review"});
                for (k, v) in extra.as_object().unwrap() {
                    job[k] = v.clone();
                }
                if let Some(round) = round {
                    job["round"] = json!(round);
                }
                let board: &'static str = Box::leak(
                    json!({"updated": "", "jobs": [job]})
                        .to_string()
                        .into_boxed_str(),
                );
                let (origin, _) = serve(vec![(200, board, vec![])]);
                fs::write(
                    self.home.join("config.md"),
                    format!("---\napi_url: {origin}\n---\n"),
                )
                .unwrap();
                let mut o = Out::default();
                let r = pre_review(&self.home, "sample", JOB, &mut o);
                let (code, err) = match r {
                    Ok(()) => (0, o.err.clone()),
                    Err(e) => (e.code, o.err.clone() + &e.stderr),
                };
                let out = if o.out.is_empty() {
                    Value::Null
                } else {
                    loads(&o.out).unwrap()
                };
                (code, out, err)
            }

            fn gate(&self) -> (i32, Value, String) {
                self.run_gate(None, json!({}))
            }

            fn text(&self) -> String {
                fs::read_to_string(&self.out).unwrap()
            }

            fn names(out: &Value) -> Vec<String> {
                out["checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| c["name"].as_str().unwrap().to_string())
                    .collect()
            }

            fn assert_none(&self, reason: &str) {
                let (code, out, err) = self.gate();
                assert_eq!((code, err.as_str()), (0, ""));
                assert_eq!(
                    (&out["result"], &out["checks"], &out["reason"]),
                    (&json!("none"), &json!([]), &json!(reason))
                );
                assert!(self.text().contains(&format!("Reason: {reason}\n")));
                assert!(!self.logs.exists());
            }

            fn forged(&self) -> PathBuf {
                let forged = self.home.join("forged.git");
                git_in(
                    &self.home,
                    &["init", "-q", "--bare", &forged.display().to_string()],
                );
                self.commit(Some(json!([check("forged", "raise SystemExit(3)")])), None);
                self.git(&[
                    "push",
                    "-q",
                    &forged.display().to_string(),
                    "HEAD:refs/heads/main",
                ]);
                forged
            }

            fn linked(&self) {
                let checkout = self.home.join("checkout");
                fs::rename(&self.wt, &checkout).unwrap();
                git_in(
                    &checkout,
                    &[
                        "worktree",
                        "add",
                        "-q",
                        "-b",
                        "job",
                        &self.wt.display().to_string(),
                    ],
                );
            }

            fn spec(&self) {
                fs::write(self.folder.join("spec.md"), SPEC).unwrap();
            }

            fn write(&self, sizes: &[(&str, usize)]) {
                for (path, n) in sizes {
                    let file = self.wt.join(path);
                    fs::create_dir_all(file.parent().unwrap()).unwrap();
                    fs::write(file, "x\n".repeat(*n)).unwrap();
                }
                self.commit(None, None);
            }

            fn stacked_rider(&self) -> Value {
                self.spec();
                self.base(Some(json!([check("lint", "pass")])), None);
                let sizes: Vec<(String, usize)> =
                    (0..6).map(|i| (format!("src/lead{i}.py"), 120)).collect();
                let sizes: Vec<(&str, usize)> =
                    sizes.iter().map(|(p, n)| (p.as_str(), *n)).collect();
                self.write(&sizes);
                self.git(&["branch", "factory/000-sample-lead"]);
                self.write(&[("tools/a.py", 10)]);
                json!({"stacked": true, "refs": [{"kind": "batched-into", "id": "000-sample-lead"}]})
            }

            fn budget_none(&self, reason: &str, extra: Value) {
                let (_, out, err) = self.run_gate(None, extra);
                assert_eq!(err, "");
                assert_eq!(
                    out["budget"],
                    json!({"result": "none", "files": 0, "lines": 0, "expected_files": 0, "expected_lines": 0,
                           "file_limit": 0, "line_limit": 0, "unlisted": [], "reason": reason})
                );
                assert!(self
                    .text()
                    .contains(&format!("\nBudget: none ({reason})\n")));
            }
        }

        #[cfg(unix)]
        #[test]
        fn no_checks_file_is_none() {
            let f = Fix::new("pre-no-checks");
            f.git(&["push", "-q", "origin", "HEAD:refs/heads/main"]);
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(
                (
                    &out["result"],
                    &out["checks"],
                    &out["round"],
                    &out["repeats"]
                ),
                (&json!("none"), &json!([]), &json!(1), &json!([]))
            );
            let text = f.text();
            assert!(text.starts_with("PRE-REVIEW: NONE\n\n"));
            assert!(text.contains("Reason: origin/main has no .factory/checks.json"));
        }

        #[cfg(unix)]
        #[test]
        fn branch_copy_is_never_read() {
            let f = Fix::new("pre-branch-copy");
            f.base(Some(json!([check("base", "print('base ran')")])), None);
            f.commit(Some(json!([check("branch", "raise SystemExit(3)")])), None);
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(Fix::names(&out), ["base"]);
            assert!(fs::read_to_string(f.logs.join("base.log"))
                .unwrap()
                .contains("base ran"));
            assert!(!f.logs.join("branch.log").exists());
        }

        #[cfg(unix)]
        #[test]
        fn all_pass() {
            use std::os::unix::fs::PermissionsExt;
            let f = Fix::new("pre-all-pass");
            f.base(
                Some(json!([check("lint", "pass"), check("cli", "print('ok')")])),
                None,
            );
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            let head = f.git(&["rev-parse", "HEAD"]);
            assert_eq!(
                (&out["result"], &out["head"], &out["reason"]),
                (&json!("pass"), &json!(head), &Value::Null)
            );
            assert_eq!((&out["round"], &out["repeats"]), (&json!(1), &json!([])));
            let checks = out["checks"].as_array().unwrap();
            assert_eq!(
                checks
                    .iter()
                    .map(|c| (c["name"].clone(), c["exit"].clone()))
                    .collect::<Vec<_>>(),
                [(json!("lint"), json!(0)), (json!("cli"), json!(0))]
            );
            assert!(checks.iter().all(|c| c["seconds"].is_i64()));
            assert_eq!(
                fs::metadata(&f.logs).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let text = f.text();
            assert!(text.starts_with("PRE-REVIEW: PASS\n\n"));
            assert!(text.contains(&format!("Head: {head}\n")));
            assert!(text.contains(&format!(
                "Checks: origin/main:.factory/checks.json at {head}\n"
            )));
            assert!(text.contains("| lint | 0 |") && text.contains("| cli | 0 |"));
            assert!(!text.contains("## "));
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2202-port-retro"]
        fn failure_writes_tail_and_log() {
            let _ = DEFER;
            let f = Fix::new("pre-tail");
            let noisy = "import sys\nfor i in range(60): print(f'line {i} ~x~')\nsys.exit(1)";
            f.base(
                Some(json!([check("format", "pass"), check("lint", noisy)])),
                None,
            );
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (1, ""));
            assert_eq!(out["result"], "fail");
            let text = f.text();
            assert!(text.starts_with("PRE-REVIEW: FAIL\n\n"));
            assert!(text.contains("| lint | 1 |") && !text.contains("## format"));
            let block: Vec<&str> = text
                .split("## lint\n\n~~~text\n")
                .nth(1)
                .unwrap()
                .split("~~~\n")
                .next()
                .unwrap()
                .lines()
                .collect();
            assert_eq!(block.len(), 40);
            assert_eq!((block[0], block[39]), ("line 20 -x-", "line 59 -x-"));
            assert_eq!(
                fs::read_to_string(f.logs.join("lint.log"))
                    .unwrap()
                    .lines()
                    .count(),
                60
            );
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2202-port-retro"]
        fn a_check_failing_in_two_consecutive_rounds_is_a_repeat() {
            let f = Fix::new("pre-repeat");
            f.base(
                Some(json!([
                    check("lint", "pass"),
                    check("unit", "raise SystemExit(1)")
                ])),
                None,
            );
            let (code, out, _) = f.gate();
            assert_eq!(
                (code, &out["round"], &out["repeats"]),
                (1, &json!(1), &json!([]))
            );
            let (code, out, _) = f.run_gate(Some(1), json!({}));
            assert_eq!(
                (code, &out["round"], &out["repeats"]),
                (1, &json!(2), &json!(["unit"]))
            );
            let record =
                fs::read_to_string(f.logs.parent().unwrap().join("pre-review-failures.json"))
                    .unwrap();
            assert_eq!(
                loads(&record).unwrap(),
                json!({"1": ["unit"], "2": ["unit"]})
            );
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2202-port-retro"]
        fn failure_classes() {
            let f = Fix::new("pre-classes");
            let record = f.logs.parent().unwrap().join("pre-review-failures.json");
            let rows = [
                (
                    "budget",
                    check("unit", "import time; time.sleep(30)"),
                    json!("timeout"),
                    "budget",
                ),
                (
                    "missing",
                    json!({"name": "unit", "run": ["factory-test-no-such-program"]}),
                    json!("missing"),
                    "structural",
                ),
                (
                    "127",
                    check("unit", "raise SystemExit(127)"),
                    json!(127),
                    "structural",
                ),
                (
                    "signal",
                    check(
                        "unit",
                        "import os, signal; os.kill(os.getpid(), signal.SIGTERM)",
                    ),
                    json!(-15),
                    "canceled",
                ),
                (
                    "transient",
                    check("unit", "print('ECONNRESET'); raise SystemExit(1)"),
                    json!(1),
                    "transient, retried",
                ),
                (
                    "deterministic",
                    check("unit", "raise SystemExit(1)"),
                    json!(1),
                    "deterministic",
                ),
            ];
            CHECK_TIMEOUT.with(|t| t.set(Some(1)));
            for (row, unit, exit, cell) in rows {
                let _ = fs::remove_file(&record);
                f.base(Some(json!([check("lint", "pass"), unit])), None);
                let (code, out, err) = f.gate();
                assert_eq!((code, err.as_str()), (1, ""), "{row}");
                let (lint, result) = (&out["checks"][0], &out["checks"][1]);
                assert_eq!(
                    (&lint["class"], &lint["retried"]),
                    (&Value::Null, &json!(false))
                );
                assert_eq!(
                    (&result["exit"], &result["class"], &result["retried"]),
                    (
                        &exit,
                        &json!(cell.split(',').next().unwrap()),
                        &json!(cell.ends_with("retried"))
                    )
                );
                let text = f.text();
                assert!(text
                    .contains("| Check | Exit | Seconds | Class |\n| --- | --- | --- | --- |\n"));
                assert!(text.contains(&format!(" | {cell} |\n\nClasses are explained under ")));
                assert_eq!(record.exists(), row == "deterministic");
            }
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2202-port-retro"]
        fn a_transient_failure_is_retried_once() {
            let f = Fix::new("pre-transient");
            let marker = f.tmp.join("deps-ran");
            let flaky = format!(
                "import pathlib, sys\nmarker = pathlib.Path({:?})\nif not marker.exists():\n    marker.touch()\n    print('ECONNRESET')\n    sys.exit(1)\nprint('installed')",
                marker.display().to_string()
            );
            f.base(
                Some(json!([
                    check("deps", &flaky),
                    check("unit", "raise SystemExit(1)")
                ])),
                None,
            );
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (1, ""));
            let (deps, unit) = (&out["checks"][0], &out["checks"][1]);
            assert_eq!(
                (&deps["exit"], &deps["class"], &deps["retried"]),
                (&json!(0), &Value::Null, &json!(true))
            );
            assert_eq!(
                (&unit["class"], &unit["retried"]),
                (&json!("deterministic"), &json!(false))
            );
            assert_eq!(
                fs::read_to_string(f.logs.join("deps.attempt-1.log")).unwrap(),
                "ECONNRESET\n"
            );
            assert_eq!(
                fs::read_to_string(f.logs.join("deps.log")).unwrap(),
                "installed\n"
            );
            assert!(!f.logs.join("unit.attempt-1.log").exists());
            assert!(f.text().contains(" | -, retried |\n"));
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2202-port-retro"]
        fn a_pass_between_does_not_reset_the_repeat() {
            let f = Fix::new("pre-pass-between");
            let record = f.logs.parent().unwrap().join("pre-review-failures.json");
            f.base(Some(json!([check("unit", "raise SystemExit(1)")])), None);
            let (code, out, _) = f.gate();
            assert_eq!((code, &out["repeats"]), (1, &json!([])));
            f.base(
                Some(json!([
                    check("unit", "pass"),
                    check("deps", "print('ECONNRESET'); raise SystemExit(1)")
                ])),
                None,
            );
            let (code, out, _) = f.run_gate(Some(1), json!({}));
            assert_eq!(
                (code, &out["round"], &out["repeats"]),
                (1, &json!(2), &json!([]))
            );
            assert_eq!(
                loads(&fs::read_to_string(&record).unwrap()).unwrap(),
                json!({"1": ["unit"]})
            );
            f.base(Some(json!([check("unit", "raise SystemExit(1)")])), None);
            let (code, out, _) = f.run_gate(Some(2), json!({}));
            assert_eq!(
                (code, &out["round"], &out["repeats"]),
                (1, &json!(3), &json!(["unit"]))
            );
            assert_eq!(
                loads(&fs::read_to_string(&record).unwrap()).unwrap(),
                json!({"1": ["unit"], "3": ["unit"]})
            );
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2202-port-retro"]
        fn two_failures_in_one_round_are_not_a_repeat() {
            let f = Fix::new("pre-two-failures");
            let record = f.logs.parent().unwrap().join("pre-review-failures.json");
            f.base(Some(json!([check("unit", "raise SystemExit(1)")])), None);
            f.run_gate(Some(2), json!({}));
            f.base(
                Some(json!([
                    check("lint", "raise SystemExit(1)"),
                    check("unit", "pass")
                ])),
                None,
            );
            let (code, out, _) = f.run_gate(Some(2), json!({}));
            assert_eq!(
                (code, &out["round"], &out["repeats"]),
                (1, &json!(3), &json!([]))
            );
            assert_eq!(
                loads(&fs::read_to_string(&record).unwrap()).unwrap(),
                json!({"3": ["lint", "unit"]})
            );
            fs::write(&record, "[not an object]").unwrap();
            f.base(Some(json!([check("lint", "raise SystemExit(1)")])), None);
            let (_, out, _) = f.run_gate(Some(3), json!({}));
            assert_eq!(out["repeats"], json!([]));
            assert_eq!(
                loads(&fs::read_to_string(&record).unwrap()).unwrap(),
                json!({"4": ["lint"]})
            );
            for bad in [json!({"4": 5}), json!({"4": "lint"}), json!({"4": [5]})] {
                fs::write(&record, bad.to_string()).unwrap();
                let (code, out, _) = f.run_gate(Some(4), json!({}));
                assert_eq!((code, &out["repeats"]), (1, &json!([])));
                assert_eq!(
                    loads(&fs::read_to_string(&record).unwrap()).unwrap(),
                    json!({"5": ["lint"]})
                );
            }
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2202-port-retro"]
        fn failure_tail_is_redacted() {
            let f = Fix::new("pre-redacted");
            fs::write(
                f.home.join(".env"),
                "FACTORY_TEST_SECRET=synthetic-secret-0123456789\n", // gitleaks:allow synthetic secret
            )
            .unwrap();
            let leak = "import sys\nprint('token=synthetic-secret-0123456789')\nprint('Authorization: Bearer abcdefghijklmnop123456')\nsys.exit(1)"; // gitleaks:allow synthetic secret
            f.base(Some(json!([check("leak", leak)])), None);
            let (code, _, _) = f.gate();
            assert_eq!(code, 1);
            let text = f.text();
            assert!(
                !text.contains("synthetic-secret-0123456789")
                    && !text.contains("abcdefghijklmnop123456")
            );
            assert!(
                text.contains("token=[redacted]") && text.contains("Authorization: [redacted]")
            );
            assert!(fs::read_to_string(f.logs.join("leak.log"))
                .unwrap()
                .contains("synthetic-secret-0123456789"));
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2202-port-retro"]
        fn nested_env_values_are_redacted_longest_first() {
            let f = Fix::new("pre-nested");
            fs::write(
                f.home.join(".env"),
                "FACTORY_TEST_SHORT=abcdefghijkl\nFACTORY_TEST_LONG=abcdefghijklMNOP\n",
            )
            .unwrap();
            f.base(
                Some(json!([check(
                    "nested",
                    "import sys\nprint('value abcdefghijklMNOP')\nsys.exit(1)"
                )])),
                None,
            );
            assert_eq!(f.gate().0, 1);
            let text = f.text();
            assert!(!text.contains("MNOP") && text.contains("value [redacted]"));
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2202-port-retro"]
        fn end_overlapping_env_values_are_fully_redacted() {
            let f = Fix::new("pre-overlap");
            fs::write(
                f.home.join(".env"),
                "FACTORY_TEST_LEFT=alpha-secret-overlap1234\nFACTORY_TEST_RIGHT=overlap1234-beta-secret\n",
            )
            .unwrap();
            let printed =
                "import sys\nprint('value alpha-secret-overlap1234-beta-secret')\nsys.exit(1)";
            f.base(Some(json!([check("overlap", printed)])), None);
            assert_eq!(f.gate().0, 1);
            let text = f.text();
            assert!(!text.contains("secret") && text.contains("value [redacted]\n"));
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2202-port-retro"]
        fn timeout_and_missing() {
            let f = Fix::new("pre-timeout");
            f.base(
                Some(json!([check("slow", "import time; time.sleep(30)"),
                            {"name": "gone", "run": ["factory-test-no-such-program"]},
                            {"name": "noexec", "run": ["./README.md"]}])),
                None,
            );
            CHECK_TIMEOUT.with(|t| t.set(Some(1)));
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (1, ""));
            let got: Vec<Value> = out["checks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| json!([c["name"], c["exit"], c["class"], c["retried"]]))
                .collect();
            assert_eq!(
                got,
                [
                    json!(["slow", "timeout", "budget", false]),
                    json!(["gone", "missing", "structural", false]),
                    json!(["noexec", "missing", "structural", false])
                ]
            );
            assert!(!f
                .logs
                .parent()
                .unwrap()
                .join("pre-review-failures.json")
                .exists());
            assert_eq!(
                fs::read_to_string(f.logs.join("gone.log")).unwrap(),
                "missing program: factory-test-no-such-program\n"
            );
            assert_eq!(
                fs::read_to_string(f.logs.join("noexec.log")).unwrap(),
                "missing program: ./README.md\n"
            );
        }

        #[cfg(unix)]
        #[test]
        fn invalid_checks_file_is_none() {
            let f = Fix::new("pre-invalid");
            for (text, reason) in [
                ("{not json".to_string(), "is not valid JSON"),
                (
                    json!({"checks": [{"name": "-bad", "run": ["x"]}]}).to_string(),
                    "unique name",
                ),
                (
                    json!({"checks": [{"name": "a", "run": []}]}).to_string(),
                    "run list",
                ),
            ] {
                f.base(None, Some(&text));
                let (code, out, err) = f.gate();
                assert_eq!((code, err.as_str()), (0, ""));
                assert_eq!(out["result"], "none");
                assert!(out["reason"].as_str().unwrap().contains(reason));
                assert!(f.text().contains("Reason: "));
            }
        }

        #[cfg(unix)]
        #[test]
        fn link_refused() {
            let f = Fix::new("pre-link");
            f.base(Some(json!([check("lint", "pass")])), None);
            let elsewhere = f.home.join("elsewhere.md");
            std::os::unix::fs::symlink(&elsewhere, &f.out).unwrap();
            let (code, out, err) = f.gate();
            assert_eq!((code, out), (6, Value::Null));
            assert!(err.contains("symlink or junction"));
            assert!(!elsewhere.exists() && !f.logs.exists());
            fs::remove_file(&f.out).unwrap();
            let real = f.home.join("jobs/sample/real");
            fs::rename(&f.folder, &real).unwrap();
            std::os::unix::fs::symlink(&real, &f.folder).unwrap();
            let (code, out, _) = f.gate();
            assert_eq!((code, out), (6, Value::Null));
            assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
        }

        #[cfg(unix)]
        #[test]
        fn forged_local_base_is_refetched() {
            let f = Fix::new("pre-forged-base");
            f.base(Some(json!([check("base", "print('base ran')")])), None);
            f.commit(Some(json!([check("forged", "raise SystemExit(3)")])), None);
            f.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(Fix::names(&out), ["base"]);
            assert!(!f.logs.join("forged.log").exists());
        }

        #[cfg(unix)]
        #[test]
        fn factory_base_is_none() {
            let f = Fix::new("pre-factory-base");
            f.base(Some(json!([check("lint", "pass")])), None);
            fs::write(
                f.home.join("projects/sample.md"),
                "---\nrepo: example/sample\nbase: factory/x\n---\n",
            )
            .unwrap();
            f.assert_none("base factory/x is a factory branch");
        }

        #[cfg(unix)]
        #[test]
        fn fetch_failure_is_none() {
            let f = Fix::new("pre-fetch-failure");
            f.commit(Some(json!([check("lint", "pass")])), None);
            f.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
            f.assert_none("could not fetch origin main");
        }

        #[cfg(unix)]
        #[test]
        fn repointed_origin_is_ignored() {
            let f = Fix::new("pre-repointed");
            f.base(Some(json!([check("base", "print('base ran')")])), None);
            let forged = f.forged();
            f.git(&["remote", "set-url", "origin", &forged.display().to_string()]);
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(Fix::names(&out), ["base"]);
            assert!(!f.logs.join("forged.log").exists());
        }

        #[cfg(unix)]
        #[test]
        fn url_rewrite_is_none() {
            let f = Fix::new("pre-url-rewrite");
            f.base(Some(json!([check("base", "pass")])), None);
            let forged = f.forged();
            f.git(&[
                "config",
                &format!("url.{}.insteadOf", forged.display()),
                &f.origin.display().to_string(),
            ]);
            f.assert_none("the worktree's git config sets url or http keys");
        }

        #[cfg(unix)]
        #[test]
        fn unreadable_git_config_is_none() {
            let f = Fix::new("pre-bad-config");
            f.base(Some(json!([check("base", "pass")])), None);
            fs::write(f.wt.join(".git/config"), "[core\n").unwrap();
            f.assert_none("could not read the worktree's git config");
        }

        #[cfg(unix)]
        #[test]
        fn bad_repo_is_none() {
            let f = Fix::new("pre-bad-repo");
            f.base(Some(json!([check("base", "pass")])), None);
            fs::write(
                f.home.join("projects/sample.md"),
                "---\nrepo: ../x\nbase: main\n---\n",
            )
            .unwrap();
            f.assert_none("project file repo '../x' is not owner/name");
        }

        #[cfg(unix)]
        #[test]
        fn linked_worktree_fetches() {
            let f = Fix::new("pre-linked");
            f.base(Some(json!([check("base", "print('base ran')")])), None);
            f.linked();
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(Fix::names(&out), ["base"]);
        }

        #[cfg(unix)]
        #[test]
        fn worktree_config_rewrite_is_none() {
            let f = Fix::new("pre-worktree-config");
            f.base(Some(json!([check("base", "pass")])), None);
            f.linked();
            let forged = f.forged();
            f.git(&["config", "extensions.worktreeConfig", "true"]);
            f.git(&[
                "config",
                "--worktree",
                &format!("url.{}.insteadOf", forged.display()),
                &f.origin.display().to_string(),
            ]);
            f.assert_none("the worktree's git config sets url or http keys");
        }

        #[cfg(unix)]
        #[test]
        fn replaced_checks_blob_is_ignored() {
            let f = Fix::new("pre-replaced-blob");
            f.base(Some(json!([check("base", "print('base ran')")])), None);
            let real = f.git(&["rev-parse", "HEAD:.factory/checks.json"]);
            let forged_file = f.home.join("forged.json");
            fs::write(
                &forged_file,
                json!({"checks": [check("forged", "raise SystemExit(3)")]}).to_string(),
            )
            .unwrap();
            let forged = f.git(&["hash-object", "-w", &forged_file.display().to_string()]);
            f.git(&["replace", &real, &forged]);
            f.git(&["config", "core.useReplaceRefs", "true"]);
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(Fix::names(&out), ["base"]);
            assert!(!f.logs.join("forged.log").exists());
        }

        #[cfg(unix)]
        #[test]
        fn forged_tag_is_ignored() {
            let f = Fix::new("pre-forged-tag");
            f.base(Some(json!([check("base", "print('base ran')")])), None);
            let pushed = f.git(&["rev-parse", "HEAD"]);
            f.commit(Some(json!([check("forged", "raise SystemExit(3)")])), None);
            f.git(&["tag", "origin/main", "HEAD"]);
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(Fix::names(&out), ["base"]);
            assert!(f.text().contains(&format!(
                "Checks: origin/main:.factory/checks.json at {pushed}\n"
            )));
            assert!(changed_files(&f.wt, "main")
                .unwrap()
                .contains(&".factory/checks.json".to_string()));
        }

        #[cfg(unix)]
        #[test]
        fn base_diff_ignores_replace_and_grafts() {
            let f = Fix::new("pre-replace-grafts");
            f.git(&["push", "-q", "origin", "HEAD:refs/heads/main"]);
            fs::write(f.wt.join("README.md"), "branch\n").unwrap();
            f.commit(None, None);
            let new = f.git(&["rev-parse", "HEAD:README.md"]);
            let old = f.git(&["rev-parse", "HEAD~1:README.md"]);
            f.git(&["replace", &new, &old]);
            assert_eq!(fetch_base(&f.wt, "main", "example/sample"), None);
            let readme = "README.md".to_string();
            assert!(changed_files(&f.wt, "main").unwrap().contains(&readme));
            assert!(diff_numstat(&f.wt, "refs/remotes/origin/main")
                .unwrap()
                .iter()
                .any(|(p, _)| *p == readme));
            assert!(diff_ranges(&f.wt, "main", "example/sample")
                .unwrap()
                .contains_key(&readme));
            let head = f.git(&["rev-parse", "HEAD"]);
            fs::write(f.wt.join(".git/info/grafts"), head + "\n").unwrap();
            assert!(changed_files(&f.wt, "main").unwrap().contains(&readme));
        }

        #[cfg(unix)]
        fn forging_driver(f: &Fix) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let marker = f.home.join("driver-ran");
            let driver = f.home.join("driver.sh");
            fs::write(
                &driver,
                format!(
                    "#!/bin/sh\necho ran >> '{}'\necho FORGED\n",
                    marker.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&driver, fs::Permissions::from_mode(0o755)).unwrap();
            f.git(&["config", "diff.external", &driver.display().to_string()]);
            f.git(&[
                "config",
                "diff.forge.textconv",
                &driver.display().to_string(),
            ]);
            fs::create_dir_all(f.wt.join(".git/info")).unwrap();
            fs::write(f.wt.join(".git/info/attributes"), "* diff=forge\n").unwrap();
            marker
        }

        #[cfg(unix)]
        #[test]
        fn base_diff_ignores_external_diff_and_textconv() {
            let f = Fix::new("pre-ext-diff");
            f.git(&["push", "-q", "origin", "HEAD:refs/heads/main"]);
            fs::write(f.wt.join("README.md"), "branch\n").unwrap();
            f.commit(None, None);
            let marker = forging_driver(&f);
            assert_eq!(fetch_base(&f.wt, "main", "example/sample"), None);
            assert!(changed_files(&f.wt, "main")
                .unwrap()
                .contains(&"README.md".to_string()));
            let numstat = diff_numstat(&f.wt, "refs/remotes/origin/main").unwrap();
            assert_eq!(numstat.iter().find(|(p, _)| p == "README.md").unwrap().1, 2);
            assert_eq!(
                diff_ranges(&f.wt, "main", "example/sample").unwrap()["README.md"],
                [(1, 1)]
            );
            assert!(!marker.exists());
        }

        #[cfg(unix)]
        #[test]
        fn round_diff_ignores_external_diff_and_textconv() {
            let f = Fix::new("pre-round-ext-diff");
            let head = f.git(&["rev-parse", "HEAD"]);
            fs::write(f.wt.join("README.md"), "round\n").unwrap();
            f.commit(None, None);
            let marker = forging_driver(&f);
            assert_eq!(round_changed_files(&f.wt, &head).unwrap(), ["README.md"]);
            assert!(!marker.exists());
        }

        #[cfg(unix)]
        #[test]
        fn budget_over_warns() {
            let f = Fix::new("pre-budget-over");
            f.spec();
            f.base(Some(json!([check("lint", "pass")])), None);
            f.write(&[
                ("tools/a.py", 300),
                ("tests/b.py", 300),
                ("docs/c.md", 120),
                ("docs/d.md", 120),
                ("src/e.py", 120),
                ("src/f.py", 120),
                ("src/g.py", 120),
            ]);
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(out["result"], "pass");
            let unlisted = ["docs/c.md", "docs/d.md", "src/e.py", "src/f.py", "src/g.py"];
            assert_eq!(
                out["budget"],
                json!({"result": "over", "files": 7, "lines": 1200, "expected_files": 2, "expected_lines": 300,
                       "file_limit": 4, "line_limit": 900, "unlisted": unlisted, "reason": null})
            );
            let text = f.text();
            let head = f.git(&["rev-parse", "HEAD"]);
            assert!(text.contains(&format!(
                "Head: {head}\nBudget: over (files 7, limit 4; lines 1200, limit 900)\nChecks: "
            )));
            let section = text.split("## Diff budget\n").nth(1).unwrap();
            assert!(section.contains("| Measure | Spec estimate | Limit | Branch |\n"));
            assert!(
                section.contains("| Files | 2 | 4 | 7 |\n")
                    && section.contains("| Lines | 300 | 900 | 1200 |\n")
            );
            assert!(section.contains("Files outside the spec's task list:\n"));
            let bullets: Vec<&str> = section.lines().filter(|l| l.starts_with("- ")).collect();
            let expected: Vec<String> = unlisted.iter().map(|p| format!("- `{p}`")).collect();
            assert_eq!(bullets, expected);
        }

        #[cfg(unix)]
        #[test]
        fn budget_cuts_unlisted_and_fences_backticks() {
            let f = Fix::new("pre-budget-cut");
            f.spec();
            f.base(Some(json!([check("lint", "pass")])), None);
            let mut names: Vec<String> = (0..21).map(|i| format!("src/m{i:02}.py")).collect();
            names.push("src/a`odd.py".into());
            let sizes: Vec<(&str, usize)> = names.iter().map(|n| (n.as_str(), 1)).collect();
            f.write(&sizes);
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(out["budget"]["unlisted"].as_array().unwrap().len(), 22);
            let text = f.text();
            let bullets: Vec<&str> = text.lines().filter(|l| l.starts_with("- ")).collect();
            assert_eq!(bullets.len(), 21);
            assert_eq!(bullets[..2], ["- `` src/a`odd.py ``", "- `src/m00.py`"]);
            assert_eq!(bullets[19..], ["- `src/m18.py`", "- and 2 more"]);
        }

        #[cfg(unix)]
        #[test]
        fn budget_fences_backtick_runs() {
            let f = Fix::new("pre-budget-fence");
            f.spec();
            f.base(Some(json!([check("lint", "pass")])), None);
            f.write(&[
                ("src/m0.py", 1),
                ("src/m1.py", 1),
                ("src/m2.py", 1),
                ("src/m3.py", 1),
                ("src/b``two.py", 1),
            ]);
            let (code, _, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            let text = f.text();
            assert_eq!(
                text.lines().find(|l| l.starts_with("- ")).unwrap(),
                "- ``` src/b``two.py ```"
            );
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn budget_survives_non_utf8() {
            use std::os::unix::ffi::OsStrExt;
            let f = Fix::new("pre-budget-bytes");
            let mut spec = SPEC.as_bytes().to_vec();
            spec.extend_from_slice(b"\xff\xfe bad\n");
            fs::write(f.folder.join("spec.md"), spec).unwrap();
            f.base(Some(json!([check("lint", "pass")])), None);
            fs::write(f.wt.join(std::ffi::OsStr::from_bytes(b"bad\xff.py")), "x\n").unwrap();
            f.write(&[("tools/a.py", 10)]);
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            let budget = &out["budget"];
            assert_eq!(
                (
                    &budget["result"],
                    &budget["files"],
                    &budget["expected_files"]
                ),
                (&json!("within"), &json!(2), &json!(2))
            );
            assert!(f
                .text()
                .contains("Budget: within (files 2, limit 4; lines 11, limit 900)\n"));
        }

        #[cfg(unix)]
        #[test]
        fn budget_within() {
            let f = Fix::new("pre-budget-within");
            f.spec();
            f.base(Some(json!([check("lint", "pass")])), None);
            f.write(&[("tools/a.py", 10)]);
            let (code, out, err) = f.gate();
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(out["budget"]["result"], "within");
            let text = f.text();
            assert!(text.contains("Budget: within (files 1, limit 4; lines 10, limit 900)\n"));
            assert!(!text.contains("## "));
        }

        #[cfg(unix)]
        #[test]
        fn budget_stacked_rider_counts_own_commits() {
            let f = Fix::new("pre-rider-own");
            let extra = f.stacked_rider();
            let (code, out, err) = f.run_gate(None, extra);
            assert_eq!((code, err.as_str()), (0, ""));
            assert_eq!(out["budget"]["result"], "within");
            assert!(f
                .text()
                .contains("Budget: within (files 1, limit 4; lines 10, limit 900)\n"));
        }

        #[cfg(unix)]
        #[test]
        fn budget_stacked_rider_without_lead_branch() {
            let f = Fix::new("pre-rider-no-lead");
            let extra = f.stacked_rider();
            f.git(&["branch", "-D", "factory/000-sample-lead"]);
            f.budget_none("the branch diff could not be read", extra);
        }

        #[cfg(unix)]
        #[test]
        fn budget_stacked_rider_with_non_string_lead() {
            let f = Fix::new("pre-rider-int-lead");
            f.stacked_rider();
            f.budget_none(
                "the branch diff could not be read",
                json!({"stacked": true, "refs": [{"kind": "batched-into", "id": 7}]}),
            );
        }

        #[cfg(unix)]
        #[test]
        fn budget_none_reasons() {
            let f = Fix::new("pre-budget-none");
            f.base(Some(json!([check("lint", "pass")])), None);
            f.write(&[("tools/a.py", 10)]);
            f.budget_none("the job folder has no spec.md", json!({}));
            fs::write(f.home.join("elsewhere.md"), SPEC).unwrap();
            std::os::unix::fs::symlink(f.home.join("elsewhere.md"), f.folder.join("spec.md"))
                .unwrap();
            f.budget_none("spec.md is a symlink or junction", json!({}));
            fs::remove_file(f.folder.join("spec.md")).unwrap();
            fs::write(
                f.folder.join("spec.md"),
                "## Tasks\n\n1. Step\n   - Files: `/etc/x` and `../y`\n",
            )
            .unwrap();
            f.budget_none("spec.md has no task Files paths", json!({}));
            f.spec();
            let branch = f.git(&["rev-parse", "--abbrev-ref", "HEAD"]);
            f.git(&["checkout", "-q", "--orphan", "unrelated"]);
            f.commit(None, None);
            f.git(&["push", "-q", "-f", "origin", "HEAD:refs/heads/main"]);
            f.git(&["checkout", "-q", "-f", &branch]);
            f.budget_none("the branch diff could not be read", json!({}));
            fs::write(
                f.home.join("projects/sample.md"),
                "---\nrepo: example/sample\nbase: factory/x\n---\n",
            )
            .unwrap();
            f.budget_none("the base could not be fetched", json!({}));
        }

        #[test]
        fn paths() {
            let text = "## Design\n\n- Files: `outside.py`\n\n## Tasks\n\n1. One\n   - Files: `tools/*.py`, `docs/`\n   - Change: `not/a/files/line.py`\n2. Two\n   - Files: `tools/*.py`, `/etc/passwd`, `../up.py`, `a/../b.py`, `tests/t.py`\n\n## Risks\n\n- Files: `after.py`\n";
            assert_eq!(
                spec_budget_paths(text),
                ["tools/*.py", "docs/", "tests/t.py"]
            );
            assert!(spec_budget_paths("## Design\n\n- Files: `a.py`\n").is_empty());
        }

        #[test]
        fn covers() {
            assert!(budget_covers("tools/*.py", "tools/a.py"));
            assert!(!budget_covers("tools/*.py", "tools/a.md"));
            assert!(budget_covers("docs/", "docs/deep/c.md"));
            assert!(!budget_covers("docs/", "docsx/c.md"));
            assert!(budget_covers("tests/t.py", "tests/t.py"));
            assert!(!budget_covers("tests/t.py", "tests/t.pyc"));
        }
    }

    mod grounding {
        use super::super::*;
        use super::git_in;
        use crate::shared::fs::temp_dir;
        use crate::shared::http::tests::serve;
        use std::fs;

        const JOB: &str = "042-sample-export";
        const CODE_REVIEW: &str = "VERDICT: CHANGES\n\n1. The export loop at `src/export.py:52` skips the last row.\n2. `export.py:412` never closes the file.\n3. Move the view into src/export_view.py:12.\n4. The query in src/db.py:300 is unbounded.\n5. Same loop again at src/export.py:52.\n";
        const SECURITY_REVIEW: &str =
            "VERDICT: PASS\n\nThe escaping at src/export.py:60-64 is fine.\n";
        const SKIPPED_REVIEW: &str =
            "VERDICT: SKIPPED\n\nfactory.py review-plan skipped this review: src/nope.py:1.\n";
        const UNVERIFIED: [&str; 3] = [
            "review-code.md export.py:412 bad-line",
            "review-code.md src/export_view.py:12 no-file",
            "review-code.md src/db.py:300 off-diff",
        ];

        fn round_1() -> Value {
            json!({"security": "PASS", "ux": "SKIPPED", "ui": "SKIPPED", "code": "CHANGES"})
        }

        fn numbered(count: usize, tag: &str) -> String {
            (1..=count).map(|i| format!("line {i}{tag}\n")).collect()
        }

        struct Fix {
            home: PathBuf,
            folder: PathBuf,
            round: PathBuf,
            out: PathBuf,
            wt: PathBuf,
        }

        impl Fix {
            fn new(name: &str) -> Fix {
                let home = temp_dir(name);
                fs::create_dir(home.join("projects")).unwrap();
                fs::write(
                    home.join("projects/sample.md"),
                    "---\nrepo: example/sample\nbase: main\n---\n# Sample\n",
                )
                .unwrap();
                let folder = home.join("jobs/sample").join(JOB);
                let round = folder.join("round-1");
                fs::create_dir_all(&round).unwrap();
                fs::write(folder.join("spec.md"), "# Spec\n\nSample.\n\nMore.\n").unwrap();
                let wt = home.join("worktrees/sample").join(JOB);
                fs::create_dir_all(wt.join("src")).unwrap();
                git_in(&wt, &["init", "-q"]);
                let origin = home.join("origin.git");
                git_in(
                    &home,
                    &["init", "-q", "--bare", &origin.display().to_string()],
                );
                git_in(
                    &wt,
                    &["remote", "add", "origin", &origin.display().to_string()],
                );
                FETCH_URL.with(|u| *u.borrow_mut() = Some(origin.display().to_string()));
                fs::write(wt.join("src/export.py"), numbered(180, "")).unwrap();
                fs::write(wt.join("src/db.py"), numbered(400, "")).unwrap();
                let f = Fix {
                    out: round.join("grounding.md"),
                    home,
                    folder,
                    round,
                    wt,
                };
                f.commit();
                f.git(&["push", "-q", "origin", "HEAD:refs/heads/main"]);
                let mut lines: Vec<String> = numbered(180, "")
                    .lines()
                    .map(|l| format!("{l}\n"))
                    .collect();
                let changed: Vec<String> = numbered(75, " changed")
                    .lines()
                    .map(|l| format!("{l}\n"))
                    .collect();
                lines.splice(39..75, changed[39..75].iter().cloned());
                fs::write(f.wt.join("src/export.py"), lines.concat()).unwrap();
                f.commit();
                f.review("code", CODE_REVIEW);
                f.review("security", SECURITY_REVIEW);
                f.review("ux", SKIPPED_REVIEW);
                f.review("ui", SKIPPED_REVIEW);
                f
            }

            fn git(&self, args: &[&str]) -> String {
                git_in(&self.wt, args)
            }

            fn commit(&self) {
                self.git(&["add", "-A"]);
                self.git(&["commit", "-q", "-m", "change"]);
            }

            fn review(&self, reviewer: &str, text: &str) {
                fs::write(self.round.join(format!("review-{reviewer}.md")), text).unwrap();
            }

            /// Run grounding against a fixed board; returns the exit code, stdout and the PUT bodies.
            fn grounding(
                &self,
                round: &str,
                rounds: Value,
                jobs: Option<Value>,
            ) -> (i32, String, Vec<Value>) {
                let job = json!({"id": JOB, "project": "sample", "title": "Sample export", "stage": "review",
                                 "round": 1, "rounds": rounds});
                let board = json!({"updated": "", "jobs": jobs.unwrap_or(json!([job]))});
                let board: &'static str = Box::leak(board.to_string().into_boxed_str());
                let (origin, seen) = serve(vec![
                    (200, board, vec![("ETag", "\"e1\"")]),
                    (200, board, vec![("ETag", "\"e1\"")]),
                    (200, "", vec![]),
                ]);
                fs::write(
                    self.home.join("config.md"),
                    format!("---\napi_url: {origin}\ndevice: sample-device\n---\n"),
                )
                .unwrap();
                let mut o = Out::default();
                let code = match grounding(&self.home, "sample", JOB, round, &mut o) {
                    Ok(()) => 0,
                    Err(e) => e.code,
                };
                std::thread::sleep(std::time::Duration::from_millis(50));
                let puts = seen
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|r| r.method == "PUT")
                    .map(|r| loads(std::str::from_utf8(&r.body).unwrap()).unwrap())
                    .collect();
                (code, o.out, puts)
            }

            fn result(&self, rounds: Value) -> (Value, Vec<Value>) {
                let (code, out, puts) = self.grounding("1", rounds, None);
                assert_eq!(code, 0);
                assert_eq!(out.matches('\n').count(), 1);
                (loads(&out).unwrap(), puts)
            }
        }

        const DEFER: &str = "deferred to integration: 2194-port-claims";

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2194-port-claims"]
        fn running_example() {
            let _ = DEFER;
            let f = Fix::new("ground-example");
            let (result, _) = f.result(json!([round_1()]));
            assert_eq!(
                result,
                json!({"round": 1, "result": "unverified", "checked": 5, "diff": null, "unverified": [
                    {"review": "review-code.md", "citation": "export.py:412", "status": "bad-line",
                     "reason": "src/export.py has 180 lines"},
                    {"review": "review-code.md", "citation": "src/export_view.py:12", "status": "no-file",
                     "reason": "no file at src/export_view.py in the worktree or job folder"},
                    {"review": "review-code.md", "citation": "src/db.py:300", "status": "off-diff",
                     "reason": "src/db.py is not changed on this branch"}]})
            );
            let head = f.git(&["rev-parse", "HEAD"]);
            assert_eq!(
                fs::read_to_string(&f.out).unwrap(),
                format!("GROUNDING: UNVERIFIED 3\n\nHead: {head}\nDiff: origin/main\nChecked: 5 citations in 2 review files\n\n| Review | Citation | Status | Reason |\n| --- | --- | --- | --- |\n| review-code.md | `export.py:412` | bad-line | src/export.py has 180 lines |\n| review-code.md | `src/export_view.py:12` | no-file | no file at src/export_view.py in the worktree or job folder |\n| review-code.md | `src/db.py:300` | off-diff | src/db.py is not changed on this branch |\n\nUnverified means the cited path or line did not check out. The finding may still be right. No verdict changed.\n")
            );
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2194-port-claims"]
        fn board_entry() {
            let f = Fix::new("ground-board");
            let (_, puts) = f.result(json!([round_1()]));
            let mut entry = round_1();
            entry["unverified"] = json!(UNVERIFIED);
            assert_eq!(
                puts.iter()
                    .map(|b| b["jobs"][0]["rounds"].clone())
                    .collect::<Vec<_>>(),
                [json!([entry])]
            );
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2194-port-claims"]
        fn clean_rerun() {
            let f = Fix::new("ground-clean");
            f.review(
                "code",
                "VERDICT: PASS\n\nThe loop at src/export.py:52 is fine now.\n",
            );
            let mut entry = round_1();
            entry["unverified"] = json!(UNVERIFIED);
            let (result, puts) = f.result(json!([entry]));
            assert_eq!(
                (&result["result"], &result["checked"], &result["unverified"]),
                (&json!("clean"), &json!(2), &json!([]))
            );
            assert_eq!(
                puts.iter()
                    .map(|b| b["jobs"][0]["rounds"].clone())
                    .collect::<Vec<_>>(),
                [json!([round_1()])]
            );
            let text = fs::read_to_string(&f.out).unwrap();
            assert!(text.starts_with("GROUNDING: CLEAN\n\nHead: "));
            assert!(
                text.contains("Checked: 2 citations in 2 review files\n\nUnverified means")
                    && !text.contains('|')
            );
            let (_, puts) = f.result(json!([round_1()]));
            assert!(puts.is_empty());
        }

        #[test]
        fn citation_forms() {
            let text = "See ./src/a.py:3, src/b.py:4-9 and src/c.md:1,5,7 and ./src/a.py:3 again. Not https://example.com:443/a.py:3, localhost:8220, /abs/path.py:3, ../up.py:2 or a.py:x.";
            let cite = |c: &str, p: &str, lines: Vec<i64>, span| Citation {
                citation: c.into(),
                path: p.into(),
                lines,
                span,
            };
            assert_eq!(
                find_citations(text),
                [
                    cite("./src/a.py:3", "src/a.py", vec![3], None),
                    cite("src/b.py:4-9", "src/b.py", vec![4, 9], Some((4, 9))),
                    cite("src/c.md:1,5,7", "src/c.md", vec![1, 5, 7], None),
                ]
            );
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2194-port-claims"]
        fn skipped_ignored() {
            let f = Fix::new("ground-skipped");
            f.review("code", "VERDICT: SKIPPED\n\nsrc/nope.py:1\n");
            f.review("security", SKIPPED_REVIEW);
            let (result, _) = f.result(json!([round_1()]));
            assert_eq!(
                (&result["result"], &result["checked"]),
                (&json!("clean"), &json!(0))
            );
            assert!(fs::read_to_string(&f.out)
                .unwrap()
                .contains("Checked: 0 citations in 0 review files\n"));
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2194-port-claims"]
        fn resolution() {
            let f = Fix::new("ground-resolution");
            fs::create_dir(f.wt.join("lib")).unwrap();
            fs::write(f.wt.join("lib/export.py"), numbered(500, "")).unwrap();
            f.git(&["add", "-A"]);
            let tracked: Vec<String> = f.git(&["ls-files"]).lines().map(String::from).collect();
            let near: Ranges = [("src/export.py".to_string(), vec![(40, 75)])]
                .into_iter()
                .collect();
            let empty = Ranges::new();
            let ground = |path: &str, lines: &[i64], ranges: Option<&Ranges>, near: bool, span| {
                ground_citation(path, lines, &f.wt, &f.folder, &tracked, ranges, near, span)
            };
            let t = |a: &str, b: &str, c: &str| (a.to_string(), b.to_string(), c.to_string());
            assert_eq!(ground("export.py", &[412], None, false, None).0, "ok");
            assert_eq!(
                ground("export.py", &[600], None, false, None),
                t(
                    "bad-line",
                    "lib/export.py, src/export.py",
                    "2 files named export.py, none has line 600"
                )
            );
            assert_eq!(
                ground("spec.md", &[3], Some(&empty), true, None),
                t("ok", "job:spec.md", "")
            );
            assert_eq!(
                ground("src/export.py", &[80], Some(&near), true, None),
                t("ok", "src/export.py", "")
            );
            assert_eq!(
                ground("src/export.py", &[90], Some(&near), true, None).2,
                "more than 10 lines from every change to src/export.py"
            );
            assert_eq!(
                ground(
                    "src/export.py",
                    &[20, 100],
                    Some(&near),
                    true,
                    Some((20, 100))
                ),
                t("ok", "src/export.py", "")
            );
            assert_eq!(
                ground("src/export.py", &[20, 100], Some(&near), true, None).0,
                "off-diff"
            );
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2194-port-claims"]
        fn unreadable_diff() {
            let f = Fix::new("ground-unreadable");
            FETCH_URL
                .with(|u| *u.borrow_mut() = Some(f.home.join("gone.git").display().to_string()));
            let (result, _) = f.result(json!([round_1()]));
            assert_eq!(result["diff"], "could not fetch origin main");
            let statuses: Vec<&Value> = result["unverified"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| &r["status"])
                .collect();
            assert_eq!(statuses, [&json!("bad-line"), &json!("no-file")]);
            let text = fs::read_to_string(&f.out).unwrap();
            assert!(
                text.contains("\nDiff: unreadable: could not fetch origin main\n")
                    && !text.contains("off-diff")
            );
        }

        #[cfg(unix)]
        #[test]
        #[ignore = "deferred to integration: 2194-port-claims"]
        fn refusals() {
            let f = Fix::new("ground-refusals");
            for round in ["0", "x", "01"] {
                let (code, out, puts) = f.grounding(round, json!([round_1()]), None);
                assert_eq!((code, out.as_str(), puts.len()), (2, "", 0));
            }
            for (round, rounds, jobs) in [
                ("1", json!([round_1()]), Some(json!([]))),
                ("1", json!([]), None),
                ("2", json!([round_1()]), None),
            ] {
                let (code, out, puts) = f.grounding(round, rounds, jobs);
                assert_eq!((code, out.as_str(), puts.len()), (4, "", 0));
            }
            fs::rename(&f.wt, f.home.join("moved")).unwrap();
            assert_eq!(f.grounding("1", json!([round_1()]), None).0, 4);
            fs::rename(f.home.join("moved"), &f.wt).unwrap();
            fs::write(f.home.join("elsewhere.md"), "x\n").unwrap();
            std::os::unix::fs::symlink(f.home.join("elsewhere.md"), &f.out).unwrap();
            assert_eq!(f.grounding("1", json!([round_1()]), None).0, 6);
            assert_eq!(
                fs::read_to_string(f.home.join("elsewhere.md")).unwrap(),
                "x\n"
            );
        }

        #[cfg(unix)]
        #[test]
        fn arm_metrics_accepts_unverified() {
            let f = Fix::new("ground-arm-metrics");
            let mut entry = round_1();
            entry["unverified"] = json!(UNVERIFIED);
            let mut jobs = Map::new();
            jobs.insert(
                JOB.into(),
                json!({"project": "sample", "stage": "review", "rounds": [entry]}),
            );
            for name in TRACER_REVIEWERS {
                fs::write(f.round.join(format!("review-{name}.md")), "").unwrap();
            }
            assert_eq!(
                arm_metrics(&jobs, json!([]), &f.home.join("jobs"))["coverage_unknown"],
                0
            );
        }

        #[test]
        fn report_lists_rows() {
            let rows = [GroundRow {
                review: "review-code.md".into(),
                citation: "a|b.py:1".into(),
                status: "no-file".into(),
                reason: "no file".into(),
            }];
            let text = grounding_report(
                &rows,
                None,
                "main",
                Some("could not fetch origin main"),
                1,
                1,
            );
            assert!(text.starts_with("GROUNDING: UNVERIFIED 1\n\nHead: None\nDiff: unreadable: could not fetch origin main\n"));
            assert!(text.contains("| review-code.md | `a/b.py:1` | no-file | no file |\n"));
        }
    }
}
