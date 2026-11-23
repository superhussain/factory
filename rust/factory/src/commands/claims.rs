//! Not ported yet: claim, board, hold, handoff.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn claim(_call: &Call) -> Res<()> {
    Err(not_ported("claim"))
}

pub fn board(_call: &Call) -> Res<()> {
    Err(not_ported("board"))
}

pub fn hold(_call: &Call) -> Res<()> {
    Err(not_ported("hold"))
}

pub fn handoff(_call: &Call) -> Res<()> {
    Err(not_ported("handoff"))
}
