//! Not ported yet: worktree, cleanup.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn worktree(_call: &Call) -> Res<()> {
    Err(not_ported("worktree"))
}

pub fn cleanup(_call: &Call) -> Res<()> {
    Err(not_ported("cleanup"))
}
