//! Not ported yet: wait, watch, round-wait.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn wait_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("wait"))
}

pub fn watch_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("watch"))
}

pub fn round_wait_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("round-wait"))
}
