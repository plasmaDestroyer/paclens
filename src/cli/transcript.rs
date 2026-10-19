//! `paclens transcript`: the last recorded pacman run, as pacman printed it.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::update_view::live::TRANSCRIPT;

/// Copy the last run's transcript to `out`, minus the `Script started …`
/// header line. Bytes go out raw so the terminal renders pacman's colours.
pub fn run(dir: &Path, out: &mut impl Write) -> Result<()> {
    let path = dir.join(TRANSCRIPT);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("no pacman run recorded yet")
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let body = bytes
        .iter()
        .position(|&b| b == b'\n')
        .map_or(&[][..], |i| &bytes[i + 1..]);
    out.write_all(body)?;
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("paclens-transcript-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_the_bytes_after_the_header_line() {
        let dir = temp_dir("body");
        fs::write(
            dir.join(TRANSCRIPT),
            b"Script started on 2026-10-19 [COMMAND=\"pacman -Syu\"]\n\x1b[1m:: Synchronizing\x1b[0m\r\nend",
        )
        .unwrap();
        let mut out = Vec::new();
        run(&dir, &mut out).unwrap();
        assert_eq!(out, b"\x1b[1m:: Synchronizing\x1b[0m\r\nend");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_transcript_says_no_run_recorded() {
        let dir = temp_dir("missing");
        let err = run(&dir, &mut Vec::new()).unwrap_err();
        assert_eq!(err.to_string(), "no pacman run recorded yet");
        let _ = fs::remove_dir_all(&dir);
    }
}
