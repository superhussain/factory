//! The review and pipeline commands: pipeline, documents, verdict, escalation, repeats, review-plan,
//! review-floor, pre-review, grounding, batch, stack, lesson, agent, trial, experiment and routes.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::cli::{not_ported, Call};
use crate::shared::board::{dump_board, escalated, load_board, read_board};
use crate::shared::conf::{fm_get, frontmatter};
use crate::shared::consts::{
    tag_lookalike, BABYSIT_SENTENCE, JOB_ID, NOTE_INVISIBLE, NOTE_LINK, NOTE_PHRASES, NOTE_SECRETS,
    NOTE_TAG, PIPELINE_GATES, PIPELINE_STAGES, PREFERENCES_SENTENCE, REVIEW_ROUTE_KEYS,
    ROUTE_EFFORTS, ROUTE_MODEL, SLUG,
};
use crate::shared::exit::{fail, Res};
use crate::shared::fs::{is_link, read_text, write_atomic};
use crate::shared::home::factory_home;
use crate::shared::http::{access_message, api, cas};
use crate::shared::project::project_files;
use crate::shared::py::{
    is_space, os_error, printable_line, repr, round_half_even, splitlines, strip,
};
use crate::shared::pyjson::{dumps, loads, DEFAULT, INDENT2};
use crate::shared::pyre::{Flags, Pattern};
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

// ---- not ported yet ----

pub fn verdict_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("verdict"))
}

pub fn lesson_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("lesson"))
}

pub fn escalation_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("escalation"))
}

pub fn repeats_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("repeats"))
}

pub fn review_plan_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("review-plan"))
}

pub fn review_floor_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("review-floor"))
}

pub fn pre_review_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("pre-review"))
}

pub fn grounding_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("grounding"))
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
}
