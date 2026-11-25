//! Exits as values: a handler returns `Err(Exit)` where `factory.py` calls `fail` or `sys.exit`.

/// Bad usage.
pub const USAGE_ERROR: i32 = 2;
/// Nothing in the backlog (or no idea, or nothing stuck) this device can run.
pub const NOTHING: i32 = 3;
/// Not found.
pub const NOT_FOUND: i32 = 4;
/// Worker or network failure.
pub const WORKER: i32 = 5;
/// Another session holds the job, R2 or origin moved under this device, local job artifacts
/// do not match R2, a job path is a link or cannot be inspected, a pairing was refused, or
/// the checkout holds history origin's base does not descend from.
pub const CONFLICT: i32 = 6;
/// A person reviews and merges the PR, so nothing merges.
pub const HUMAN: i32 = 7;
/// A pattern hit its regex backtrack limit, so nothing it was reading passed on.
pub const LIMIT: i32 = 8;

#[derive(Debug)]
pub struct Exit {
    pub code: i32,
    pub stderr: String,
    /// The Worker status and response text, as Python's `ApiStatusExit` carries them.
    pub status: Option<(u16, String)>,
}

pub type Res<T> = Result<T, Exit>;

/// Python's `fail(msg, code)`.
pub fn fail(msg: impl std::fmt::Display, code: i32) -> Exit {
    Exit {
        code,
        stderr: format!("{msg}\n"),
        status: None,
    }
}

/// Python's `sys.exit(code)`.
pub fn exit(code: i32) -> Exit {
    Exit {
        code,
        stderr: String::new(),
        status: None,
    }
}

impl Exit {
    pub fn raw(code: i32, stderr: impl Into<String>) -> Exit {
        Exit {
            code,
            stderr: stderr.into(),
            status: None,
        }
    }
}
// CI toolchain cache probe; the next commit reverts it.
