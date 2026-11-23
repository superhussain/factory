//! Not ported yet: tracer.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn tracer_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("tracer"))
}
