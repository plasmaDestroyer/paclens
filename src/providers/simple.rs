//! Sources with one shape: a binary on `PATH`, one scan call that returns
//! packages and updates, and one unprivileged update command that asks
//! nothing. Each is a row here, and the scanner and planner read the row —
//! adding such a source is a provider module plus one entry.
//!
//! A source that needs more (privilege, a second tool for update detection,
//! relabelling another source's packages) keeps its own plumbing.

use crate::config::Config;
use crate::model::{Package, PendingUpdate, SourceId, SourceKind};
use crate::providers::{CommandRunner, ProviderError};

pub type Scan = fn(&dyn CommandRunner) -> Result<(Vec<Package>, Vec<PendingUpdate>), ProviderError>;

pub struct SimpleSource {
    pub id: &'static str,
    pub kind: SourceKind,
    /// The binary whose presence on `PATH` makes the source available.
    pub bin: &'static str,
    pub enabled: fn(&Config) -> bool,
    pub scan: Scan,
    pub update: fn() -> Vec<String>,
}

impl SimpleSource {
    pub fn source_id(&self) -> SourceId {
        SourceId(self.id.to_string())
    }
}

pub const SIMPLE: &[SimpleSource] = &[
    SimpleSource {
        id: "rustup",
        kind: SourceKind::Rustup,
        bin: super::rustup::RUSTUP_BIN,
        enabled: |c| c.sources.rustup,
        scan: super::rustup::scan,
        update: super::rustup::update_command,
    },
    SimpleSource {
        id: "brew",
        kind: SourceKind::Brew,
        bin: super::brew::BREW_BIN,
        enabled: |c| c.sources.brew,
        scan: super::brew::scan,
        update: super::brew::update_command,
    },
    SimpleSource {
        id: "uv",
        kind: SourceKind::Uv,
        bin: super::uv::UV_BIN,
        enabled: |c| c.sources.uv,
        scan: super::uv::scan,
        update: super::uv::update_command,
    },
];

/// The row for a source kind, if it is one of these.
pub fn for_kind(kind: &SourceKind) -> Option<&'static SimpleSource> {
    SIMPLE.iter().find(|s| &s.kind == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_is_its_own_source() {
        for (i, a) in SIMPLE.iter().enumerate() {
            assert_eq!(for_kind(&a.kind).map(|s| s.id), Some(a.id));
            assert!(
                SIMPLE[i + 1..].iter().all(|b| b.id != a.id),
                "{} twice",
                a.id
            );
        }
    }
}
