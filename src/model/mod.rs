//! Canonical data model. Every other module imports its types from here.

mod action;
mod dependency;
mod migrate;
mod overlap;
mod package;
mod scan;
mod source;
mod summary;
mod update;

pub use action::{ActionKind, ActionPlan, ActionStep};
pub use dependency::{Confidence, DependencyEdge, EdgeKind};
pub use migrate::{Direction, MigrationReport, PathKind, PathMapping};
pub use overlap::{MatchMethod, OverlapCandidate, PackageRef, PrimaryHeuristic, Tradeoff};
pub use package::{InstallReason, Package};
pub use scan::{CacheSizes, SCHEMA_VERSION, ScanResult};
pub use source::{FlatpakScope, Source, SourceCapabilities, SourceId, SourceKind};
pub use summary::{SourceSummary, summarize};
pub use update::PendingUpdate;
