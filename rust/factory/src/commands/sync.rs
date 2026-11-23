//! Not ported yet: push, pull, sync.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn push(_call: &Call) -> Res<()> {
    Err(not_ported("push"))
}

pub fn pull(_call: &Call) -> Res<()> {
    Err(not_ported("pull"))
}

pub fn sync(_call: &Call) -> Res<()> {
    Err(not_ported("sync"))
}
