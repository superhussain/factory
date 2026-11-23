#[allow(dead_code)]
mod shared;
use std::ffi::OsString;
use std::io::Write;
use std::{env, io, process};

const USAGE: &str = include_str!("usage.txt");

fn run(_args: &[OsString], err: &mut dyn Write) -> i32 {
    let _ = err.write_all(USAGE.as_bytes());
    2
}

fn main() {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    process::exit(run(&args, &mut io::stderr()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_prints_usage_and_exits_2() {
        let mut err = Vec::new();
        let args = ["claim", "a", "b", "c", "d"].map(OsString::from);
        assert_eq!(run(&args, &mut err), 2);
        assert_eq!(err, USAGE.as_bytes());
    }

    #[test]
    fn usage_has_python_prefix() {
        assert!(USAGE.starts_with("factory.py: usage:\n"));
    }
}
