//! The command table and `factory.main()`: argument parsing, the arity gate, `--home` and `--session`.

use std::ffi::OsString;
use std::path::Path;

use crate::commands::{
    audit, bootstrap, catch_up, checks, claims, daemon, install, intake, lookup, ops, pickup,
    pipeline, prompt, retro, screenshots, setup, sync, tracer, wait, worktree,
};
use crate::shared::consts::{MAX_RIDERS, SLUG};
use crate::shared::exit::{fail, Exit, Res};
use crate::shared::fs::{resolve, same_path};
use crate::shared::home::set_override;

pub struct Call {
    pub args: Vec<String>,
    /// `init --agents` and `--connection`, when given. The setup slice reads them.
    #[allow(dead_code)]
    pub agents: Option<String>,
    #[allow(dead_code)]
    pub connection: Option<String>,
}

pub type Handler = fn(&Call) -> Res<()>;

/// Every command with `factory.py`'s operand bounds, in its `COMMANDS` order.
pub const COMMANDS: &[(&str, Handler, usize, usize)] = &[
    ("where", lookup::where_cmd, 0, 1),
    ("init", setup::init, 0, 1),
    ("claim", claims::claim, 3, 3),
    ("board", claims::board, 2, 2),
    ("next", intake::next_item, 0, 0),
    ("backlog", intake::backlog_cmd, 2, 9),
    ("stop", ops::stop, 1, 1),
    ("push", sync::push, 1, 1),
    ("pull", sync::pull, 1, 1),
    ("sync", sync::sync, 0, 1),
    ("hold", claims::hold, 1, 2),
    ("worktree", worktree::worktree, 1, 1),
    ("job", lookup::job, 1, 1),
    ("handoff", claims::handoff, 1, 1),
    ("builds", lookup::builds, 1, 1),
    ("ideas", intake::ideas, 0, 6),
    ("stuck", intake::stuck, 0, 2),
    ("idea-images", intake::idea_images, 1, 1),
    ("idea-files", intake::idea_files, 1, 1),
    ("adopt-home", setup::adopt_home_cmd, 1, 1),
    ("doctor", setup::doctor_cmd, 0, 0),
    ("autonomy", intake::autonomy_cmd, 1, 3),
    ("halt", intake::halt_cmd, 1, 3),
    ("suggest", intake::suggest_cmd, 2, 5),
    ("control", intake::control_cmd, 4, 6),
    ("routes", pipeline::routes_cmd, 1, 4),
    ("experiment", pipeline::experiment_cmd, 2, 2),
    ("trial", pipeline::trial_cmd, 1, 3),
    ("unclaim", intake::unclaim, 1, 1),
    ("pipeline", pipeline::pipeline_cmd, 1, 2),
    ("documents", pipeline::documents_cmd, 1, 1),
    ("prompt", prompt::prompt_cmd, 2, 8),
    ("wait", wait::wait_cmd, 2, 23),
    ("watch", wait::watch_cmd, 2, 24),
    ("round-wait", wait::round_wait_cmd, 2, 4),
    ("verdict", pipeline::verdict_cmd, 4, 4),
    ("checks", checks::checks_cmd, 1, 3),
    ("pr-state", checks::pr_state_cmd, 1, 3),
    ("pr-handled", ops::pr_handled_cmd, 2, 34),
    ("pr-queue", checks::pr_queue_cmd, 1, 1),
    ("pr-merge", checks::pr_merge_cmd, 1, 1),
    ("pickup", pickup::pickup, 0, 5),
    ("explore", setup::explore, 1, 3),
    ("host", setup::host_cmd, 3, 3),
    ("cleanup", worktree::cleanup, 1, 1),
    ("clean", ops::clean_cmd, 2, 3),
    ("deny-notes", intake::deny_notes_cmd, 1, 2),
    ("lesson", pipeline::lesson_cmd, 3, 5),
    ("tracer", tracer::tracer_cmd, 1, 2),
    ("archive-workspace", ops::archive_workspace, 1, 2),
    ("orphans", ops::orphans_cmd, 0, 0),
    ("escalation", pipeline::escalation_cmd, 2, 2),
    ("catch-up", catch_up::catch_up, 0, 0),
    ("repeats", pipeline::repeats_cmd, 2, 2),
    ("review-plan", pipeline::review_plan_cmd, 2, 2),
    ("review-floor", pipeline::review_floor_cmd, 1, 1),
    ("pre-review", pipeline::pre_review_cmd, 1, 1),
    ("grounding", pipeline::grounding_cmd, 2, 2),
    ("batch", pipeline::batch_cmd, 1, 1 + MAX_RIDERS),
    ("stack", pipeline::stack_cmd, 2, 2),
    ("retro", retro::retro_cmd, 1, 3),
    ("audit", audit::audit_cmd, 1, 2),
    ("usage", audit::usage_cmd, 1, 2),
    ("outcomes", audit::outcomes_cmd, 0, 2),
    ("bench", audit::bench_cmd, 0, usize::MAX),
    ("with-cloudflare", setup::with_cloudflare_cmd, 0, usize::MAX),
    ("agent", pipeline::agent_cmd, 1, 9),
    ("needs", intake::needs_cmd, 6, 6),
    ("inbox", intake::inbox, 0, 0),
    ("triage", intake::triage_cmd, 0, usize::MAX),
];

/// Commands only the binary has. `factory.py` and `USAGE` do not list them.
const EXTRA: &[(&str, Handler, usize, usize)] = &[
    ("daemon", daemon::daemon, 0, 2),
    ("install", install::install, 4, 5),
    ("stalls", pickup::stalls_cmd, 0, usize::MAX),
    ("pr-screenshots", screenshots::pr_screenshots, 0, usize::MAX),
    ("bootstrap", bootstrap::bootstrap, 0, usize::MAX),
];

pub const USAGE: &str = include_str!("usage.txt");

/// The handler for name with count operands. `ideas` checks its own arguments, so any count passes.
fn handler_for(name: &str, count: usize) -> Option<Handler> {
    let (_, handler, lo, hi) = COMMANDS.iter().chain(EXTRA).find(|(n, ..)| *n == name)?;
    (name == "ideas" || (*lo..=*hi).contains(&count)).then_some(*handler)
}

fn run(args: Vec<OsString>) -> Res<()> {
    let args: Vec<String> = args
        .into_iter()
        .map(OsString::into_string)
        .collect::<Result<_, _>>()
        .map_err(|_| fail("arguments must be UTF-8", 2))?;
    let (mut home, mut session) = (None::<String>, None::<String>);
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" && positional == ["with-cloudflare"] {
            positional.extend(args[i..].iter().cloned());
            break;
        }
        if arg == "--home" || arg == "--session" {
            let slot = if arg == "--home" {
                &mut home
            } else {
                &mut session
            };
            if slot.is_some() || i + 1 == args.len() || args[i + 1].is_empty() {
                return Err(fail(
                    format!("{arg} requires one nonempty value and may occur only once"),
                    2,
                ));
            }
            *slot = Some(args[i + 1].clone());
            i += 2;
        } else {
            positional.push(arg.clone());
            i += 1;
        }
    }
    let (mut agents, mut connection) = (None::<String>, None::<String>);
    if positional.first().map(String::as_str) == Some("init") {
        let mut rest = Vec::new();
        let mut index = 1;
        while index < positional.len() {
            let arg = &positional[index];
            if arg == "--agents" || arg == "--connection" {
                let slot = if arg == "--agents" {
                    &mut agents
                } else {
                    &mut connection
                };
                let value = positional
                    .get(index + 1)
                    .filter(|v| !v.is_empty() && !v.starts_with("--"));
                if slot.is_some() || value.is_none() {
                    return Err(fail(
                        format!("{arg} requires one value and may occur only once"),
                        2,
                    ));
                }
                *slot = value.cloned();
                index += 2;
            } else {
                if arg.starts_with("--") {
                    return Err(fail(format!("unknown init option {arg}"), 2));
                }
                rest.push(arg.clone());
                index += 1;
            }
        }
        if !["none", "claude", "opencode", "both"].contains(&agents.as_deref().unwrap_or("none")) {
            return Err(fail("--agents must be none, claude, opencode or both", 2));
        }
        positional = std::iter::once("init".to_string()).chain(rest).collect();
    }
    let name = positional.first().map(String::as_str).unwrap_or("");
    let handler =
        handler_for(name, positional.len().saturating_sub(1)).ok_or_else(|| Exit::raw(2, USAGE))?;
    if name == "adopt-home" && home.is_some() {
        return Err(fail("adopt-home does not accept --home", 2));
    }
    if let Some(home) = &home {
        if !Path::new(home).is_absolute() {
            return Err(fail("--home must be absolute", 2));
        }
        if (name == "claim" || name == "board")
            && !same_path(Path::new(home), Path::new(&positional[1]))
        {
            return Err(fail(
                "--home disagrees with positional factory directory",
                2,
            ));
        }
    }
    let tag = session
        .clone()
        .unwrap_or_else(crate::shared::home::env_session);
    if !tag.is_empty() && !SLUG.is_full(&tag) {
        return Err(fail("bad session tag: use letters, digits, - and _", 2));
    }
    if let Some(home) = &home {
        set_override(resolve(Path::new(home)).map_err(|_| fail("cannot resolve --home", 2))?);
    }
    if session.is_some() {
        std::env::set_var("FACTORY_SESSION", &tag);
    }
    handler(&Call {
        args: positional[1..].to_vec(),
        agents,
        connection,
    })
}

/// `factory.main()`: run one command and return its exit code.
pub fn main(args: Vec<OsString>) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprint!("{}", e.stderr);
            e.code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn usage_has_python_prefix() {
        assert!(USAGE.starts_with("factory.py: usage:\n"));
    }

    #[test]
    fn daemon_is_binary_only() {
        assert!(handler_for("daemon", 0).is_some());
        assert!(handler_for("daemon", 2).is_some());
        assert!(handler_for("daemon", 3).is_none());
        assert!(!USAGE.contains("daemon"));
    }

    #[test]
    fn binary_only_commands_print_their_own_usage() {
        let stderr = |list: &[&str]| run(args(list)).unwrap_err().stderr;
        for list in [
            &["stalls"][..],
            &["stalls", "archive", "a1b2c3d4e5", "--force", "x"],
        ] {
            assert_eq!(
                stderr(list),
                "usage: factory stalls list | archive <id> [--force]\n"
            );
        }
        for list in [
            &["bootstrap"][..],
            &["bootstrap", "plan", "--answers", "a", "b"],
        ] {
            assert_eq!(
                stderr(list),
                "usage: factory bootstrap <up|down|plan --answers <file>>\n"
            );
        }
    }

    #[test]
    fn arity_and_options_fail_like_python() {
        let usage = |list: &[&str]| run(args(list)).unwrap_err().stderr == USAGE;
        assert!(usage(&[]) && usage(&["nope"]) && usage(&["claim", "a"]) && usage(&["job"]));
        let msg = |list: &[&str]| run(args(list)).unwrap_err().stderr;
        assert_eq!(
            msg(&["--home", "/x", "--home", "/x", "where"]),
            "factory.py: --home requires one nonempty value and may occur only once\n"
        );
        assert_eq!(
            msg(&["--home", "relative", "where"]),
            "factory.py: --home must be absolute\n"
        );
        assert_eq!(
            msg(&["--session", "bad tag", "where"]),
            "factory.py: bad session tag: use letters, digits, - and _\n"
        );
        assert_eq!(
            msg(&["init", "--agents", "emacs"]),
            "factory.py: --agents must be none, claude, opencode or both\n"
        );
        assert_eq!(
            msg(&["init", "--agents", "--x"]),
            "factory.py: --agents requires one value and may occur only once\n"
        );
        assert_eq!(
            msg(&["init", "--bogus"]),
            "factory.py: unknown init option --bogus\n"
        );
        assert_eq!(
            msg(&["--home", "/x", "adopt-home", "/y"]),
            "factory.py: adopt-home does not accept --home\n"
        );
        assert_eq!(
            msg(&["--home", "/x", "board", "/y", "1-a"]),
            "factory.py: --home disagrees with positional factory directory\n"
        );
        assert!(handler_for("ideas", 7).is_some() && handler_for("claim", 3).is_some());
        assert!(handler_for("claim", 2).is_none() && handler_for("", 0).is_none());
    }
}
