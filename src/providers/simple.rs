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
    /// The update command; `None` for a report-only source, which the plan
    /// skips.
    pub update: Option<fn() -> Vec<String>>,
    /// The command that refreshes the index the scan compares against, when
    /// the scan reads a cached one instead of asking upstream. Its counts are
    /// then a floor, not an answer: a stale index can only miss newer
    /// versions (#17). `None` = the scan checks upstream itself.
    pub refresh: Option<&'static str>,
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
        update: Some(super::rustup::update_command),
        refresh: None,
    },
    SimpleSource {
        id: "brew",
        kind: SourceKind::Brew,
        bin: super::brew::BREW_BIN,
        enabled: |c| c.sources.brew,
        scan: super::brew::scan,
        update: Some(super::brew::update_command),
        refresh: Some("brew update"),
    },
    SimpleSource {
        id: "uv",
        kind: SourceKind::Uv,
        bin: super::uv::UV_BIN,
        enabled: |c| c.sources.uv,
        scan: super::uv::scan,
        update: Some(super::uv::update_command),
        refresh: None,
    },
    SimpleSource {
        id: "gobin",
        kind: SourceKind::GoBin,
        bin: super::gobin::GO_BIN,
        enabled: |c| c.sources.gobin,
        scan: super::gobin::scan,
        update: Some(super::gobin::update_command),
        refresh: None,
    },
    SimpleSource {
        id: "npm",
        kind: SourceKind::Npm,
        bin: super::npm::NPM_BIN,
        enabled: |c| c.sources.npm,
        scan: super::npm::scan,
        update: None,
        refresh: None,
    },
    SimpleSource {
        id: "fwupd",
        kind: SourceKind::Fwupd,
        bin: super::fwupd::FWUPD_BIN,
        enabled: |c| c.sources.fwupd,
        scan: super::fwupd::scan,
        update: None,
        refresh: Some("fwupdmgr refresh"),
    },
];

/// The row for a source kind, if it is one of these.
pub fn for_kind(kind: &SourceKind) -> Option<&'static SimpleSource> {
    SIMPLE.iter().find(|s| &s.kind == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// brew runs with `HOMEBREW_NO_AUTO_UPDATE=1` and fwupd reads its cached
    /// LVFS metadata: neither scan asks upstream. The rest do (#17).
    #[test]
    fn only_cached_index_sources_name_a_refresh() {
        let cached: Vec<_> = SIMPLE
            .iter()
            .filter(|s| s.refresh.is_some())
            .map(|s| s.id)
            .collect();
        assert_eq!(cached, ["brew", "fwupd"]);
    }

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
