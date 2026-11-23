mod cli;
mod commands;
// The core serves every port slice; most items wait for the slice that calls them.
#[allow(dead_code)]
mod shared;

use std::io::Write;

fn main() {
    let code = cli::main(std::env::args_os().skip(1).collect());
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}
