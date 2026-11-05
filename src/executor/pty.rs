//! One pty spawn, shared by the TUI console and the pacman view (#81).
//!
//! A pty means sudo, doas, pkexec and pacman all see a real terminal: their
//! own prompts, echo control, colors and progress bars work untouched.

use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;

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
    /// pacman's terminal follows ours when the pane is resized.
    pub fn resize(&self, rows: u16, cols: u16) {
        let _ = self.master.resize(portable_pty::PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    pub fn close(self) -> Box<dyn Child + Send + Sync> {
        drop(self.writer);
        drop(self.master);
        self.child
    }
}

/// Spawn `argv` on a fresh `rows`×`cols` pty (clamped to at least 2×20).
/// The child leads a new session with the pty as its terminal.
pub fn spawn(argv: &[String], rows: u16, cols: u16) -> anyhow::Result<Pty> {
    let (program, args) = argv.split_first().ok_or_else(|| anyhow!("empty command"))?;
    let pair = open(rows, cols)?;
    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|err| anyhow!("failed to launch: {err}"))?;
    // The child holds the slave; dropping ours lets the reader see EOF.
    drop(pair.slave);
    finish(pair.master, child)
}

/// Like [`spawn`], but the child stays in our session: its stdio is the pty,
/// its controlling terminal is still ours (#82). sudo keys its timestamp on
/// the controlling terminal and the session, so a `sudo -v` done here covers
/// it, while pacman still writes to and reads from the pty.
///
/// The pty is nobody's terminal, so ^C typed into it signals no one: the
/// caller sends it as a signal ([`signal`]). With sudo's default
/// `use_pty`, sudo also gives the command a hidden terminal of its own,
/// relayed to ours: a program under it that opens `/dev/tty` (pinentry, a
/// scriptlet's `stty`) reaches the user's terminal, not this pty. And
/// sudoers I/O logging turns the logged streams into pipes.
pub fn spawn_shared(argv: &[String], rows: u16, cols: u16) -> anyhow::Result<Pty> {
    let (program, args) = argv.split_first().ok_or_else(|| anyhow!("empty command"))?;
    let pair = open(rows, cols)?;
    let path = pair
        .master
        .tty_name()
        .ok_or_else(|| anyhow!("the pty has no name"))?;
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&path)?;
    let child = std::process::Command::new(program)
        .args(args)
        .stdin(slave.try_clone()?)
        .stdout(slave.try_clone()?)
        .stderr(slave)
        .spawn()
        .map_err(|err| anyhow!("failed to launch `{program}`: {err}"))?;
    drop(pair.slave);
    finish(pair.master, Box::new(child))
}

fn open(rows: u16, cols: u16) -> anyhow::Result<portable_pty::PtyPair> {
    native_pty_system()
        .openpty(PtySize {
            rows: rows.max(2),
            cols: cols.max(20),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| anyhow!("failed to open a pty: {err}"))
}

fn finish(
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
) -> anyhow::Result<Pty> {
    let reader = master
        .try_clone_reader()
        .map_err(|err| anyhow!("failed to read the pty: {err}"))?;
    let writer = master.take_writer().ok();
    Ok(Pty {
        child,
        reader,
        writer,
        master,
    })
}

/// Send `sig` to `pid`, a child not yet reaped. For a [`spawn_shared`] child
/// that is sudo, which passes it on to the command it runs.
pub fn signal(pid: u32, sig: i32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: kill(2) takes no pointers; the caller vouches for the pid.
        unsafe { libc::kill(pid, sig) };
    }
}

/// The exit code, or `None` when a signal ended the process.
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
        if let Err(err) = reader.read_to_end(&mut out) {
            assert_eq!(err.raw_os_error(), Some(5), "{err}");
        }
        assert!(String::from_utf8_lossy(&out).contains("hi"));
        assert_eq!(exit_code(&status), Some(3));
        Ok(())
    }

    /// What the child printed until it exited.
    fn output(mut pty: Pty) -> anyhow::Result<(ExitStatus, String)> {
        let status = pty.child.wait()?;
        let mut reader = std::mem::replace(&mut pty.reader, Box::new(std::io::empty()));
        let _child = pty.close();
        let mut out = Vec::new();
        let _ = reader.read_to_end(&mut out);
        Ok((status, String::from_utf8_lossy(&out).into_owned()))
    }

    /// Session id and controlling terminal (/proc/self/stat fields 6 and 7):
    /// the two sudo keys its timestamp on.
    const SID_CTTY: &str = "cut -d' ' -f6,7 /proc/self/stat";

    /// The child's `<sid> <tty_nr>`, then `tty` if its stdout is a terminal.
    fn probe() -> Vec<String> {
        let script = format!("echo $({SID_CTTY}); test -t 1 && echo tty || echo notty");
        ["sh", "-c", &script]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn ours() -> anyhow::Result<String> {
        let out = std::process::Command::new("sh")
            .args(["-c", SID_CTTY])
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// #82: sudo finds a `sudo -v` done here only from our own session and
    /// terminal; the pty is still the child's stdout.
    #[test]
    fn a_shared_child_keeps_our_session_and_writes_to_the_pty() -> anyhow::Result<()> {
        let (_, out) = output(spawn_shared(&probe(), 5, 40)?)?;
        assert_eq!(
            out.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>(),
            [ours()?.as_str(), "tty"]
        );
        Ok(())
    }

    #[test]
    fn a_plain_child_leads_its_own_session() -> anyhow::Result<()> {
        let (_, out) = output(spawn(&probe(), 5, 40)?)?;
        let lines: Vec<&str> = out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert_ne!(lines.first().copied(), Some(ours()?.as_str()), "{out}");
        assert_eq!(lines.get(1).copied(), Some("tty"), "{out}");
        Ok(())
    }

    /// ^C typed into a shared pty raises nothing, so it is sent instead.
    #[test]
    fn a_signal_reaches_a_shared_child() -> anyhow::Result<()> {
        let argv: Vec<String> = ["sleep", "10"].iter().map(|s| s.to_string()).collect();
        let pty = spawn_shared(&argv, 5, 40)?;
        signal(pty.child.process_id().unwrap_or(0), libc::SIGINT);
        let (status, _) = output(pty)?;
        assert_eq!(exit_code(&status), None, "ended by the signal");
        Ok(())
    }
}
