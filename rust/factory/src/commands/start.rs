//! The start command: promote an idea to spec in one call, from job lookup to batch candidates.

use serde_json::{Map, Value};

use crate::cli::Call;
use crate::commands::claims::{board_write, hold_target, WriteOpts};
use crate::commands::pipeline::{batch_cmd, documents_cmd, fetch_pipeline};
use crate::commands::worktree::new_job_worktree;
use crate::commands::{intake, lookup, sync};
use crate::shared::board::{draft_pr, read_board};
use crate::shared::exit::{fail, Res};
use crate::shared::home::factory_home;
use crate::shared::pyjson::{dumps, DEFAULT, INDENT2};
use crate::shared::pyval::truthy;

fn call(arg: &str) -> Call {
    Call {
        args: vec![arg.to_string()],
        agents: None,
        connection: None,
    }
}

fn header(step: &str) {
    println!("== {step}");
}

pub fn start(call_in: &Call) -> Res<()> {
    let id = call_in.args[0].as_str();
    let home = factory_home()?;
    let row = lookup::job_row(&home, id)?;
    let project = match row.get("project").and_then(Value::as_str) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            return Err(fail(
                "Assign a registered project in the idea drawer before starting this idea",
                2,
            ))
        }
    };
    let stage = row.get("stage").and_then(Value::as_str).unwrap_or("");
    if stage != "idea" {
        return Err(fail(
            format!("{id} is at stage {stage}, not idea. Nothing was written."),
            2,
        ));
    }
    let target = format!("{project}/{id}");
    header("where");
    lookup::where_cmd(&call(&project))?;
    header("hold");
    hold_target(&target, None)?;
    header("job");
    let row = lookup::job_row(&home, id)?;
    println!("{}", dumps(&row, INDENT2));
    if row["backlog"]["decision"].as_str() == Some("rework") {
        header("next");
        println!("rework: {id} has backlog decision rework; run backlog rework as idea.md step 2 says. Nothing was promoted.");
        return Ok(());
    }
    if truthy(row.get("idea_images")) {
        header("idea-images");
        intake::idea_images(&call(&target))?;
    }
    if truthy(row.get("idea_files")) {
        header("idea-files");
        intake::idea_files(&call(&target))?;
    }
    header("sync");
    sync::sync(&call(&project))?;
    header("pipeline");
    let mut view = fetch_pipeline(&home, &project)?;
    view.as_object_mut().unwrap().remove("prompts");
    println!("{}", dumps(&view, DEFAULT));
    header("board");
    // Only an idea with a depends-on ref reads the whole board to find a draft parent.
    let depends = row["refs"]
        .as_array()
        .is_some_and(|refs| refs.iter().any(|r| r["kind"] == "depends-on"));
    let stack_base = if depends {
        let board = read_board(&home)?;
        let jobs = board["jobs"].as_array().map_or(&[][..], Vec::as_slice);
        stack_parent(&row, jobs, &project).map(|p| format!("factory/{p}"))
    } else {
        None
    };
    let branch = format!("factory/{id}");
    let versions = view["versions"].clone();
    let mut patch = Map::from_iter([
        ("stage".to_string(), Value::from("spec")),
        ("round".to_string(), Value::from(0)),
        ("rounds".to_string(), Value::Array(Vec::new())),
        ("branch".to_string(), Value::from(branch.as_str())),
        ("pipeline".to_string(), versions.clone()),
    ]);
    if let Some(base) = &stack_base {
        patch.insert("stack_base".to_string(), Value::from(base.as_str()));
    }
    board_write(
        &home,
        id,
        patch,
        &WriteOpts {
            show: false,
            verdict: None,
            rewrite_round: None,
        },
    )?;
    let stacked = stack_base
        .as_ref()
        .map(|b| format!(", stacked on {b}"))
        .unwrap_or_default();
    println!(
        "stage spec, branch {branch}{stacked}, pipeline {}",
        dumps(&versions, DEFAULT)
    );
    header("worktree");
    let wt = new_job_worktree(&home, &project, id, stack_base.as_deref())?;
    println!("{}", wt.display());
    header("documents");
    documents_cmd(&call(&project))?;
    header("batch");
    batch_cmd(&call(&target))?;
    header("next");
    let out = home
        .join("prompts")
        .join(&project)
        .join(id)
        .join("spec.txt");
    println!(
        "1. Pick batch riders as idea.md step 5 says, or none: batch {target} <candidate-id> ..."
    );
    println!("2. agent record {target} spec <harness> <via> <model> [<effort>]");
    println!("3. prompt {target} spec --out {}", out.display());
    println!("4. Spawn factory-spec-writer with: Your full prompt is in <printed path>. Read it first and follow it exactly.");
    println!("5. After spec.md lands, read references/commands/feature.md and continue at step 3.");
    if let Some(base) = &stack_base {
        println!("stacked: ship as a draft PR against {base} with automerge false and draft true");
    }
    Ok(())
}

/// The first depends-on id of row whose job is a draft PR in the same project, which the new job stacks on.
fn stack_parent(row: &Value, jobs: &[Value], project: &str) -> Option<String> {
    row.get("refs")
        .and_then(Value::as_array)?
        .iter()
        .filter(|r| r.get("kind").and_then(Value::as_str) == Some("depends-on"))
        .filter_map(|r| r.get("id").and_then(Value::as_str))
        .find(|id| {
            jobs.iter().any(|j| {
                j.get("id").and_then(Value::as_str) == Some(id)
                    && draft_pr(j)
                    && j.get("project").and_then(Value::as_str) == Some(project)
            })
        })
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stack_parent_picks_draft_dependency() {
        let row = json!({"id": "301-proto-pacing-chart", "stage": "idea", "refs": [
            {"id": "299-other", "kind": "rework-of"},
            {"id": "250-ready", "kind": "depends-on"},
            {"id": "400-shared-chart", "kind": "depends-on"},
            {"id": "300-proto-pacing", "kind": "depends-on"},
        ]});
        let jobs = [
            json!({"id": "299-other", "project": "factory", "stage": "pr-open", "draft": true}),
            json!({"id": "250-ready", "project": "factory", "stage": "pr-open", "draft": false}),
            json!({"id": "300-proto-pacing", "project": "factory", "stage": "pr-open", "draft": true}),
            json!({"id": "400-shared-chart", "project": "deets", "stage": "pr-open", "draft": true}),
        ];
        assert_eq!(
            stack_parent(&row, &jobs, "factory"),
            Some("300-proto-pacing".into())
        );
        let other_project = [jobs[0].clone(), jobs[1].clone(), jobs[3].clone()];
        assert_eq!(stack_parent(&row, &other_project, "factory"), None);
        assert_eq!(
            stack_parent(&json!({"id": "302-x"}), &jobs, "factory"),
            None
        );
    }
}
