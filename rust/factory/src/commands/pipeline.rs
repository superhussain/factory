//! The review and pipeline commands: pipeline, documents, verdict, escalation, repeats, review-plan,
//! review-floor, pre-review, grounding, batch, stack, lesson, agent, trial, experiment and routes.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::cli::{not_ported, Call};
use crate::shared::consts::{
    tag_lookalike, BABYSIT_SENTENCE, NOTE_INVISIBLE, NOTE_LINK, NOTE_PHRASES, NOTE_SECRETS,
    NOTE_TAG, PIPELINE_GATES, PIPELINE_STAGES, PREFERENCES_SENTENCE, SLUG,
};
use crate::shared::exit::{fail, Res};
use crate::shared::fs::{is_link, write_atomic};
use crate::shared::home::factory_home;
use crate::shared::http::api;
use crate::shared::project::project_files;
use crate::shared::py::{os_error, strip};
use crate::shared::pyjson::{dumps, loads, DEFAULT, INDENT2};
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

// ---- not ported yet ----

pub fn routes_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("routes"))
}

pub fn experiment_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("experiment"))
}

pub fn trial_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("trial"))
}

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
}
