//! Not ported yet: retro.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn retro_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("retro"))
}
