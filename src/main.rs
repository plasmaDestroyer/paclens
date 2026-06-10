#![deny(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod analyzer;
mod cli;
mod config;
mod executor;
mod format;
mod glyphs;
mod logging;
mod model;
mod providers;
mod scanner;
mod tui;

use std::process::ExitCode;

fn main() -> ExitCode {
    cli::run()
}
