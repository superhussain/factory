//! Not ported yet: checks, pr-state, pr-queue.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn checks_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("checks"))
}

pub fn pr_state_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("pr-state"))
}

pub fn pr_queue_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("pr-queue"))
}
