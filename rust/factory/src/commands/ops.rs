//! Not ported yet: stop, pr-handled, clean, archive-workspace, orphans.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn stop(_call: &Call) -> Res<()> {
    Err(not_ported("stop"))
}

pub fn pr_handled_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("pr-handled"))
}

pub fn clean_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("clean"))
}

pub fn archive_workspace(_call: &Call) -> Res<()> {
    Err(not_ported("archive-workspace"))
}

pub fn orphans_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("orphans"))
}
