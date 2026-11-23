//! Not ported yet: prompt.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn prompt_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("prompt"))
}
