//! Not ported yet: audit, usage, outcomes, bench.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn audit_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("audit"))
}

pub fn usage_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("usage"))
}

pub fn outcomes_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("outcomes"))
}

pub fn bench_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("bench"))
}
