//! The start command: promote an idea to spec in one call, from job lookup to batch candidates.

use serde_json::{json, Value};

use crate::cli::Call;
use crate::commands::claims::{board_write, hold_target, WriteOpts};
use crate::commands::pipeline::{batch_cmd, documents_cmd, fetch_pipeline};
use crate::commands::worktree::new_job_worktree;
use crate::commands::{intake, lookup, sync};
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
    let branch = format!("factory/{id}");
    let versions = view["versions"].clone();
    let Value::Object(patch) =
        json!({"stage": "spec", "round": 0, "rounds": [], "branch": branch, "pipeline": versions})
    else {
        unreachable!()
    };
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
    println!(
        "stage spec, branch {branch}, pipeline {}",
        dumps(&versions, DEFAULT)
    );
    header("worktree");
    println!("{}", new_job_worktree(&home, &project, id)?.display());
    header("documents");
    documents_cmd(&call(&project))?;
    header("batch");
    batch_cmd(&call(&target))?;
    header("next");
    println!("pick batch riders, then run prompt {target} spec --out and agent record");
    Ok(())
}
