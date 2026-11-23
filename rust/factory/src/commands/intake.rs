//! Not ported yet: next, backlog, ideas, stuck, idea-images, idea-files, autonomy, halt, suggest, control, unclaim, deny-notes, needs, inbox, triage.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn next_item(_call: &Call) -> Res<()> {
    Err(not_ported("next"))
}

pub fn backlog_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("backlog"))
}

pub fn ideas(_call: &Call) -> Res<()> {
    Err(not_ported("ideas"))
}

pub fn stuck(_call: &Call) -> Res<()> {
    Err(not_ported("stuck"))
}

pub fn idea_images(_call: &Call) -> Res<()> {
    Err(not_ported("idea-images"))
}

pub fn idea_files(_call: &Call) -> Res<()> {
    Err(not_ported("idea-files"))
}

pub fn autonomy_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("autonomy"))
}

pub fn halt_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("halt"))
}

pub fn suggest_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("suggest"))
}

pub fn control_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("control"))
}

pub fn unclaim(_call: &Call) -> Res<()> {
    Err(not_ported("unclaim"))
}

pub fn deny_notes_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("deny-notes"))
}

pub fn needs_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("needs"))
}

pub fn inbox(_call: &Call) -> Res<()> {
    Err(not_ported("inbox"))
}

pub fn triage_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("triage"))
}
