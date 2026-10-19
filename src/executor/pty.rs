//! One pty spawn, shared by the TUI console and the pacman view (#81).
//!
//! A pty means sudo, doas, pkexec and pacman all see a real terminal: their
//! own prompts, echo control, colors and progress bars work untouched.

use std::io::{Read, Write};

use anyhow::anyhow;
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

/// A command running on its own pty. Read its output from `reader`, type into
/// `writer`; [`Pty::close`] once the child exits so `reader` sees EOF.
pub struct Pty {
    pub child: Box<dyn Child + Send + Sync>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Option<Box<dyn Write + Send>>,
    master: Box<dyn MasterPty + Send>,
}

impl Pty {
    /// Drop the writer and the master — closes the pty, which unblocks a
    /// reader on EOF/EIO. Hands back the child for whoever still needs it.
    pub fn close(self) -> Box<dyn Child + Send + Sync> {
        drop(self.writer);
        drop(self.master);
        self.child
    }
}

/// Spawn `argv` on a fresh `rows`×`cols` pty (clamped to at least 2×20).
pub fn spawn(argv: &[String], rows: u16, cols: u16) -> anyhow::Result<Pty> {
    let (program, args) = argv.split_first().ok_or_else(|| anyhow!("empty command"))?;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: rows.max(2),
            cols: cols.max(20),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| anyhow!("failed to open a pty: {err}"))?;

    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|err| anyhow!("failed to launch: {err}"))?;
    // The child holds the slave; dropping ours lets the reader see EOF.
    drop(pair.slave);

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|err| anyhow!("failed to read the pty: {err}"))?;
    let writer = pair.master.take_writer().ok();
    Ok(Pty {
        child,
        reader,
        writer,
        master: pair.master,
    })
}

/// The exit code, or `None` when a signal ended the process.
// ponytail: first production caller is the pacman view (#81); drop the allow then.
#[allow(dead_code)]
pub fn exit_code(status: &ExitStatus) -> Option<i32> {
    match status.signal() {
        Some(_) => None,
        None => Some(status.exit_code() as i32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_streams_output_and_reports_exit_code() -> anyhow::Result<()> {
        let argv: Vec<String> = ["sh", "-c", "printf hi; exit 3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut pty = spawn(&argv, 5, 40)?;
        let status = pty.child.wait()?;
        let mut reader = std::mem::replace(&mut pty.reader, Box::new(std::io::empty()));
        let _child = pty.close();
        let mut out = Vec::new();
        // EIO at EOF on Linux ptys is fine: everything was read before it.
        let _ = reader.read_to_end(&mut out);
        assert!(String::from_utf8_lossy(&out).contains("hi"));
        assert_eq!(exit_code(&status), Some(3));
        Ok(())
    }
}
