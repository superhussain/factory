//! The build-landed command: hold the job, read the latest build.md run line, push the branch
//! and the job folder.

use crate::cli::Call;
use crate::commands::claims::hold_target;
use crate::commands::sync::push_target;
use crate::shared::exit::{fail, Res};
use crate::shared::fs::{is_link, read_text};
use crate::shared::proc::{git, origin_git};
use crate::shared::project::{r2_target, safe_job_folder};

pub fn build_landed(call: &Call) -> Res<()> {
    let target = call.args[0].as_str();
    let (home, slug, id) = r2_target(target, true)?;
    println!("== hold");
    hold_target(target, None)?;
    println!("== build.md");
    let path = safe_job_folder(&home, &slug, &id)?.join("build.md");
    let no_run = || {
        fail(
            format!(
                "no run section with a first line in {}; nothing was pushed",
                path.display()
            ),
            4,
        )
    };
    if is_link(&path).unwrap_or(true) {
        return Err(no_run());
    }
    let text = read_text(&path).map_err(|_| no_run())?;
    let (run, line) = latest_run_line(&text).ok_or_else(no_run)?;
    println!("{run}: {line}");
    println!("== git push");
    let wt = home.join("worktrees").join(&slug).join(&id);
    if !wt.is_dir() {
        return Err(fail(
            format!("no worktree at {}; nothing was pushed", wt.display()),
            4,
        ));
    }
    let branch = format!("factory/{id}");
    let head = git(
        &wt,
        &["rev-parse", "--short", &format!("refs/heads/{branch}")],
    );
    if head.is_empty() {
        return Err(fail(
            format!("no branch {branch} in {}; nothing was pushed", wt.display()),
            4,
        ));
    }
    origin_git(
        &wt,
        &[
            "push",
            "-q",
            "origin",
            &format!("refs/heads/{branch}:refs/heads/{branch}"),
        ],
    )?;
    println!("pushed {branch} {head}");
    println!("== push");
    push_target(target)?;
    println!("== next");
    if line.starts_with("BLOCKED:") {
        println!("blocked: act on the {run} line as feature.md step 3 and execution.md say; start no pre-review");
    } else if line.starts_with("BLOCKED-ENV:") {
        println!("blocked: act on the {run} line as feature.md step 3 says; start no pre-review");
    } else {
        println!("run pre-review {target} in the background");
    }
    Ok(())
}

/// ("Run N", the first non-empty line after the last `## Run N` heading).
pub(crate) fn latest_run_line(text: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let (at, run) = lines.iter().enumerate().rev().find_map(|(i, line)| {
        let n = line.strip_prefix("## Run ")?.trim_end();
        (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then(|| (i, format!("Run {n}")))
    })?;
    let first = lines[at + 1..]
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())?;
    Some((run, first.to_string()))
}

#[cfg(test)]
mod tests {
    use super::latest_run_line;

    #[test]
    fn build_landed_picks_the_last_run() {
        let text = "# Build\n\n## Run 1\n\nRouting: one.\n\n## Run 2\n\nRouting: two.\nmore\n";
        assert_eq!(
            latest_run_line(text),
            Some(("Run 2".to_string(), "Routing: two.".to_string()))
        );
    }

    #[test]
    fn build_landed_needs_a_run_heading() {
        assert_eq!(latest_run_line("# Build\n\nRouting: one.\n"), None);
    }

    #[test]
    fn build_landed_needs_a_line_after_the_heading() {
        assert_eq!(
            latest_run_line("## Run 1\n\nfirst\n\n## Run 2\n\n  \n"),
            None
        );
    }
}
