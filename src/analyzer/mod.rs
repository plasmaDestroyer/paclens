//! Analysis layer: dep graph, why queries, overlap detection, orphan detection.
//!
//! Pure: given the same `ScanResult`, always produces the same output. Never
//! calls providers or subprocesses, never writes to disk.
//!
//! Built in v0.0.7 (dep graph + why) and v0.0.8 (overlap detection).

mod graph;
pub mod kernel;
pub mod migrate;
mod overlap;
pub mod pacfiles;
pub mod services;
mod why;

pub use graph::DepGraph;
pub use kernel::{RebootStatus, reboot_status};
pub use overlap::detect_overlaps;
pub use pacfiles::PacFile;
pub use services::{StaleUnit, stale_units};
pub use why::{Verdict, WhyDetail, WhyReport, tree_lines, why};
// Named in bin code only through WhyDetail.tree's type; tests construct it.
#[allow(unused_imports)]
pub use why::TreeNode;
