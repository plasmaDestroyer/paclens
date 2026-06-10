//! The update action plan (spec §4.8).
//!
//! An `ActionPlan` is the output of the *plan* step (P4: scan → analyze → plan →
//! confirm → execute). It is built by `crate::planner` from a `ScanResult` and a
//! per-source selection, shown to the user (CLI dry-run and the TUI update
//! screen), and — from v0.0.6 — handed to the executor. It is ephemeral: never
//! cached, so no serde derive.

use chrono::{DateTime, Utc};

use super::SourceId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionPlan {
    pub created_at: DateTime<Utc>,
    pub steps: Vec<ActionStep>,
    /// True if any step needs privilege escalation (pacman, or system-scope
    /// Flatpak). The exact escalation tool is chosen by the executor (v0.0.6).
    pub requires_sudo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionStep {
    pub source_id: SourceId,
    pub kind: ActionKind,
    /// The packages this step affects (for display); a full pacman `-Syu` updates
    /// everything, so for pacman this is informational.
    pub targets: Vec<String>,
    /// The exact argv to run (without any privilege prefix).
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Update,
    // Remove is not generated in v0.x. Listed in the spec for future use.
}

impl ActionPlan {
    /// Total number of target packages across all steps.
    pub fn total_targets(&self) -> usize {
        self.steps.iter().map(|s| s.targets.len()).sum()
    }

    /// Number of sources (steps) in the plan.
    pub fn source_count(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}
