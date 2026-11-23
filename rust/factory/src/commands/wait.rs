//! wait and watch: block until job artifacts carry a valid first line. round-wait is not ported yet.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use crate::cli::{not_ported, Call};
use crate::shared::exit::{fail, Res};
use crate::shared::fs::is_link;
use crate::shared::py::{os_error, splitlines, strip, universal_newlines};
use crate::shared::pyre::{Flags, Pattern};

static WAIT_ARTIFACT: Pattern = Pattern::new(
    r"spec\.md|spec-evidence\.md|build\.md|decision\.md|round-[1-9][0-9]*/review-(security|ux|ui|code)\.md",
    Flags::NONE,
);
static SKIPPED_REVIEW: Pattern = Pattern::new(r"round-[1-9][0-9]*/review-(ux|ui)\.md", Flags::NONE);
const WAIT_USAGE: &str = "usage: wait <project-slug>/<job-id> <artifact>... [--timeout S] [--hold]";
const WATCH_USAGE: &str = "usage: watch <project-slug>/<job-id> [<artifact>...] [--pr] [--timeout S]";
const WATCH_TIMEOUT: u64 = 540;

/// True when line is a finished first line for artifact.
fn wait_valid(artifact: &str, line: &str) -> bool {
    if ["spec.md", "spec-evidence.md", "build.md"].contains(&artifact) {
        return !strip(line).is_empty();
    }
    if artifact == "decision.md" {
        return ["APPROVE", "ESCALATE", "CHANGES"].contains(&line) || line.starts_with("CLOSED: ");
    }
    if line == "VERDICT: SKIPPED" {
        return SKIPPED_REVIEW.is_full(artifact);
    }
    line == "VERDICT: PASS" || line == "VERDICT: CHANGES"
}

/// `read_text(errors="replace").splitlines()[0]`, or "" for an empty file.
fn first_line(p: &Path) -> io::Result<String> {
    let text = universal_newlines(String::from_utf8_lossy(&fs::read(p)?).into_owned());
    Ok(splitlines(&text, false).first().map_or(String::new(), |l| l.to_string()))
}

/// Each present artifact's first line, refusing any linked file, parent or job folder.
fn job_file_lines(folder: &Path, artifacts: &[&str]) -> Res<BTreeMap<String, String>> {
    let mut lines = BTreeMap::new();
    for artifact in artifacts {
        let path = folder.join(artifact);
        for node in [path.as_path(), path.parent().unwrap(), folder] {
            if is_link(node).map_err(|e| fail(os_error(&e, Some(node)), 1))? {
                return Err(fail(
                    format!("{} is a symlink or junction; nothing was read through it", node.display()),
                    6,
                ));
            }
        }
        if path.is_file() {
            let line = first_line(&path).map_err(|e| fail(os_error(&e, Some(&path)), 1))?;
            lines.insert(artifact.to_string(), line);
        }
    }
    Ok(lines)
}

/// A `--timeout` value of ASCII digits from min to max.
fn timeout_arg(v: Option<&String>, max: u64, min: u64) -> Option<u64> {
    let v = v?;
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    v.parse::<u64>().ok().filter(|n| (min..=max).contains(n))
}

/// The calls the Python tests patch: `hold`, `time.monotonic` and `time.sleep`.
struct Io<'a> {
    hold: &'a dyn Fn(&str) -> Res<()>,
    now: &'a dyn Fn() -> f64,
    sleep: &'a dyn Fn(f64),
}

pub fn wait_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("wait"))
}

pub fn watch_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("watch"))
}

pub fn round_wait_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("round-wait"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_valid_table() {
        let rows: &[(&str, &str, bool)] = &[
            ("spec.md", "# Spec", true),
            ("spec.md", " \t", false),
            ("spec-evidence.md", "", false),
            ("build.md", "Routing", true),
            ("decision.md", "APPROVE", true),
            ("decision.md", "ESCALATE", true),
            ("decision.md", "CHANGES", true),
            ("decision.md", "CLOSED: superseded", true),
            ("decision.md", "CLOSED:", false),
            ("decision.md", "MERGE", false),
            ("round-1/review-ux.md", "VERDICT: SKIPPED", true),
            ("round-12/review-ui.md", "VERDICT: SKIPPED", true),
            ("round-1/review-security.md", "VERDICT: SKIPPED", false),
            ("round-1/review-code.md", "VERDICT: SKIPPED", false),
            ("round-1/review-code.md", "VERDICT: PASS", true),
            ("round-1/review-code.md", "VERDICT: CHANGES", true),
            ("round-1/review-code.md", "VERDICT: pass", false),
        ];
        for (artifact, line, valid) in rows {
            assert_eq!(wait_valid(artifact, line), *valid, "{artifact} {line:?}");
        }
    }
}
