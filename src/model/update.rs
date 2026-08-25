//! Pending update representation (design §7).

use serde::{Deserialize, Serialize};

use super::SourceId;

/// One available update for an installed package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingUpdate {
    pub package_name: String,
    pub current_version: String,
    pub available_version: String,
    pub source_id: SourceId,
}
