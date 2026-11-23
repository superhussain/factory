//! Not ported yet: routes, experiment, trial, pipeline, documents, verdict, lesson, escalation, repeats, review-plan, review-floor, pre-review, grounding, batch, stack, agent.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn routes_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("routes"))
}

pub fn experiment_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("experiment"))
}

pub fn trial_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("trial"))
}

pub fn pipeline_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("pipeline"))
}

pub fn documents_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("documents"))
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
