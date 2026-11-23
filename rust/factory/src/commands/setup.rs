//! Not ported yet: init, adopt-home, doctor, explore, host, with-cloudflare.

use crate::cli::{not_ported, Call};
use crate::shared::exit::Res;

pub fn init(_call: &Call) -> Res<()> {
    Err(not_ported("init"))
}

pub fn adopt_home_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("adopt-home"))
}

pub fn doctor_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("doctor"))
}

pub fn explore(_call: &Call) -> Res<()> {
    Err(not_ported("explore"))
}

pub fn host_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("host"))
}

pub fn with_cloudflare_cmd(_call: &Call) -> Res<()> {
    Err(not_ported("with-cloudflare"))
}
