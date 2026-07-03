//! Analysis layer: dep graph, why queries, overlap detection, orphan detection.
//!
//! Pure: given the same `ScanResult`, always produces the same output. Never
//! calls providers or subprocesses, never writes to disk.
//!
//! Built in v0.0.7 (dep graph + why); overlap detection lands in v0.0.8.

mod graph;
mod why;

pub use graph::DepGraph;
pub use why::{PacmanWhy, Verdict, WhyReport, why};
