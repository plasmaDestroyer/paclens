#![deny(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod analyzer;
pub mod cli;
pub mod config;
mod executor;
mod format;
mod fuzzy;
mod glyphs;
mod logging;
mod model;
mod planner;
mod providers;
mod scanner;
mod tui;
pub mod update_view;
