//! Firmware through fwupd/LVFS (#15) — report-only, by design.
//!
//! A failed firmware flash can brick hardware, many need a reboot into a
//! special mode, and some cannot be undone. That is not an update to fold
//! into "update everything", so the plan never carries a firmware step; the
//! user runs `fwupdmgr update` themselves.

use crate::model::{InstallReason, Package, PendingUpdate, SourceId};
use crate::providers::{CommandRunner, ProviderError};
use serde::Deserialize;

pub const FWUPD_BIN: &str = "fwupdmgr";

#[derive(Deserialize, Default)]
struct Devices {
    #[serde(rename = "Devices", default)]
    devices: Vec<Device>,
}

#[derive(Deserialize)]
struct Device {
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Version")]
    version: Option<String>,
    #[serde(rename = "Flags", default)]
    flags: Vec<String>,
    #[serde(rename = "Releases", default)]
    releases: Vec<Release>,
}

#[derive(Deserialize)]
struct Release {
    #[serde(rename = "Version")]
    version: String,
}

fn parse(stdout: &str) -> Devices {
    serde_json::from_str(stdout).unwrap_or_default()
}

/// `get-devices --json`: the devices fwupd can update, with their version.
pub fn parse_devices(stdout: &str) -> Vec<Package> {
    parse(stdout)
        .devices
        .into_iter()
        .filter(|d| d.flags.iter().any(|f| f == "updatable"))
        .filter_map(|d| {
            Some(Package {
                version: d.version?.trim().to_string(),
                name: d.name,
                source_id: SourceId::fwupd(),
                install_reason: InstallReason::Unknown,
                size_bytes: None,
                description: None,
                depends_on: Vec::new(),
                required_by: Vec::new(),
                optional_deps: Vec::new(),
                provides: Vec::new(),
                runtime: false,
                scope: None,
                foreign: false,
                signed: false,
                packager: None,
                repo_version: None,
            })
        })
        .collect()
}

/// `get-updates --json`: each device lists its releases newest first.
pub fn parse_updates(stdout: &str) -> Vec<PendingUpdate> {
    parse(stdout)
        .devices
        .into_iter()
        .filter_map(|d| {
            Some(PendingUpdate {
                available_version: d.releases.first()?.version.clone(),
                current_version: d.version.unwrap_or_default().trim().to_string(),
                package_name: d.name,
                source_id: SourceId::fwupd(),
            })
        })
        .collect()
}

pub fn scan(
    runner: &dyn CommandRunner,
) -> Result<(Vec<Package>, Vec<PendingUpdate>), ProviderError> {
    let out = runner
        .run(FWUPD_BIN, &["get-devices", "--json"])
        .map_err(|source| ProviderError::Exec {
            program: FWUPD_BIN.to_string(),
            source,
        })?;
    if out.exit_code != 0 {
        return Err(ProviderError::CommandFailed {
            program: "fwupdmgr get-devices".to_string(),
            exit_code: out.exit_code,
            stderr: out.stderr,
        });
    }
    // `get-updates` exits non-zero when nothing is pending: an answer, and
    // its stdout then holds no devices anyway.
    let updates = runner
        .run(FWUPD_BIN, &["get-updates", "--json"])
        .map(|o| parse_updates(&o.stdout))
        .unwrap_or_default();
    Ok((parse_devices(&out.stdout), updates))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICES: &str = r#"{"Devices":[
      {"Name":"12th Gen Intel Core","Version":"0x0000043b","Flags":["internal"]},
      {"Name":"2450 MTFDKBA1T0TFK","Version":"V5MA010 ","Flags":["internal","updatable"]},
      {"DeviceId":"a6c8","Plugin":"linux_display"},
      {"Name":"System Firmware","Version":"785","Flags":["updatable"]}]}"#;

    const UPDATES: &str = r#"{"Devices":[
      {"Name":"System Firmware","Version":"785","Flags":["updatable"],
       "Releases":[{"Version":"790","RemoteId":"lvfs"},{"Version":"788","RemoteId":"lvfs"}]}]}"#;

    #[test]
    fn only_updatable_devices_are_listed() {
        let p = parse_devices(DEVICES);
        let got: Vec<_> = p
            .iter()
            .map(|p| (p.name.as_str(), p.version.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("2450 MTFDKBA1T0TFK", "V5MA010"),
                ("System Firmware", "785")
            ]
        );
    }

    #[test]
    fn the_newest_release_is_the_update() {
        let u = parse_updates(UPDATES);
        assert_eq!(u.len(), 1);
        assert_eq!(
            (
                u[0].current_version.as_str(),
                u[0].available_version.as_str()
            ),
            ("785", "790")
        );
    }

    #[test]
    fn nothing_pending_is_empty_not_an_error() {
        let runner = crate::providers::test_support::MockRunner::new()
            .with("fwupdmgr get-devices --json", DEVICES, 0)
            .with("fwupdmgr get-updates --json", "{\"Devices\":[]}", 2);
        let (p, u) = scan(&runner).expect("scans");
        assert_eq!((p.len(), u.len()), (2, 0));
    }
}
