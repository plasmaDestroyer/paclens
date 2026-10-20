//! Play a `script` recording of a pacman run through the live view (#81):
//!
//! cargo run --example replay -- <typescript> --timing <file> [--log <file>]
//!     [--layout inline|inline-grow|nom|receipt] [--motif subtle|arcade]
//!     [--speed F] [--exit N] [--width W]

use std::io::IsTerminal;

use anyhow::{Context, Result, bail};
use paclens::cli::style::Styles;
use paclens::config::{ColorTheme, Layout, Motif};
use paclens::update_view::render::{View, candy_enabled};
use paclens::update_view::replay::{Play, log_schedule, parse_recording, play, script_start};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut typescript, mut timing, mut log) = (None, None, None);
    let (mut layout, mut motif) = (Layout::Inline, Motif::Subtle);
    let (mut speed, mut exit, mut width) = (1.0, Some(0), None);
    while let Some(a) = args.next() {
        let mut value = || args.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--timing" => timing = Some(value()?),
            "--log" => log = Some(value()?),
            "--layout" => {
                let v = value()?;
                layout = Layout::parse(&v).with_context(|| format!("unknown layout {v}"))?;
            }
            "--motif" => {
                let v = value()?;
                motif = Motif::parse(&v).with_context(|| format!("unknown motif {v}"))?;
            }
            "--speed" => {
                speed = value()?.parse()?;
                // 0 = no sleeping; a tiny speed would overflow the sleep.
                if !(speed == 0.0 || (0.01..=1000.0).contains(&speed)) {
                    bail!("--speed is 0 or between 0.01 and 1000");
                }
            }
            "--exit" => exit = Some(value()?.parse()?),
            "--width" => width = Some(value()?.parse()?),
            _ if a.starts_with("--") => bail!("unknown option {a}"),
            _ => typescript = Some(a),
        }
    }
    let (Some(typescript), Some(timing)) = (typescript, timing) else {
        bail!(
            "usage: replay <typescript> --timing <file> [--log <file>] [--layout L] [--motif M] [--speed F] [--exit N] [--width W]"
        );
    };
    let out = std::fs::read(&typescript).with_context(|| typescript.clone())?;
    let chunks = parse_recording(&out, &std::fs::read_to_string(&timing)?)?;
    let log = match (log, script_start(&out)) {
        (Some(path), Some(start)) => log_schedule(&std::fs::read_to_string(path)?, start),
        (Some(_), None) => {
            bail!("{typescript}: no `Script started on` header to line the log up with")
        }
        _ => Vec::new(),
    };
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let stdout = std::io::stdout();
    let p = Play {
        typescript: &out,
        chunks: &chunks,
        log: &log,
        view: View {
            layout,
            motif,
            candy: std::fs::read_to_string("/etc/pacman.conf").is_ok_and(|c| candy_enabled(&c)),
        },
        speed,
        exit,
        width: width.unwrap_or(cols),
        height: rows,
        live: stdout.is_terminal(),
        transcript: &typescript,
        styles: Styles::resolve(false, ColorTheme::Dark, stdout.is_terminal()),
    };
    play(&mut stdout.lock(), &p)?;
    Ok(())
}
