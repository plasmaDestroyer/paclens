//! The update action plan (design §7).
//!
//! An `ActionPlan` is the output of the *plan* step (P4: scan → analyze → plan →
//! confirm → execute). It is built by `crate::planner` from a `ScanResult` and a
//! per-source selection, shown to the user (CLI dry-run and the TUI
//! dashboard), and handed to the executor. It is ephemeral: never cached, so
//! no serde derive.

use super::SourceId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionPlan {
    pub steps: Vec<ActionStep>,
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
    /// What to call this step on screen and in the log.
    ///
    /// Usually the source id. It is not always: one source can produce more
    /// than one step — flatpak's two installations are two commands with
    /// different privilege — and two rows both reading `flatpak`, one green
    /// and one red, would be ambiguous about which half failed.
    pub label: String,
    /// Does this step need a privilege tool in front of its command?
    ///
    /// **Declared by whoever builds the step, never inferred from the source
    /// id** (design §13, 2026-09-07). It used to be read back out of the id —
    /// "privileged unless you are flatpak-user or aur" — which made root the
    /// default for every source nobody had thought about yet. Building it here
    /// costs the planner nothing: it already knows, because it just chose the
    /// command.
    pub privileged: bool,
    /// Does this step need the terminal to itself?
    ///
    /// Declared by whoever builds the step, for the same reason `privileged`
    /// is (design §13, 2026-09-07): `pacman -Syu` and an AUR helper ask
    /// questions and expect answers, while `flatpak update --noninteractive`
    /// and `cargo install-update -a` never do. Nothing reads it out of the
    /// source id, and forgetting it means a step keeps the terminal — the
    /// harmless direction, since that is what every step did before
    /// `--parallel` existed.
    pub interactive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Update,
    /// Migration copy step: backup or `cp -aT` of user-owned profile
    /// dirs. Never privileged, regardless of source.
    Migrate,
    /// Post-migration source removal, after the user verified the
    /// target side works. Privilege follows the source as usual.
    Remove,
}

impl ActionPlan {
    /// Does any step need privilege escalation? Derived from the steps, so it
    /// cannot disagree with them.
    pub fn requires_sudo(&self) -> bool {
        self.steps.iter().any(|s| s.privileged)
    }

    /// Number of distinct sources in the plan.
    ///
    /// Not the step count: flatpak is one source whose two installations are
    /// two steps with different privilege (design §13, 2026-09-07), and a
    /// plan that said "2 sources" for one tool would be counting commands and
    /// calling them sources.
    pub fn source_count(&self) -> usize {
        let mut ids: Vec<&SourceId> = self.steps.iter().map(|s| &s.source_id).collect();
        ids.sort_by_key(|id| id.as_str());
        ids.dedup();
        ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}
