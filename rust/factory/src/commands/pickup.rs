//! Not ported yet: pickup.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn pickup(_call: &Call) -> Res<()> {
    Err(not_ported("pickup"))
}
