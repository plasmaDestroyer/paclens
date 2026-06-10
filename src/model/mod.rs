//! Canonical data model. Every other module imports its types from here.
//!
//! Defined incrementally per the build order (spec §4): the source, package,
//! update, and scan types land in v0.0.2/v0.0.3. Dependency-edge, overlap, and
//! action types arrive with the milestones that consume them (v0.0.6–v0.0.8).

mod package;
mod scan;
mod source;
mod summary;
mod update;

pub use package::{InstallReason, Package};
pub use scan::{CacheSizes, SCHEMA_VERSION, ScanResult};
pub use source::{FlatpakScope, Source, SourceId, SourceKind};
pub use summary::{SourceSummary, summarize};
pub use update::PendingUpdate;
