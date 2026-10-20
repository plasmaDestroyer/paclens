//! The run model drawn as text (#81): the live region, the lines printed
//! once above it, the receipt or failure block, and the end line. Pure — no
//! terminal, no clock; the caller passes width, height and `now`.

use std::time::Duration;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::cli::style::{Ghost, Styles};
use crate::config::{Layout, Motif};

use super::medians::Median;
use super::model::{Hook, Note, NoteKind, Outcome, Phase, PhaseKind, PhaseState, Run};
use super::screen_line::{ScreenEvent, classify_screen};

pub struct View {
    pub layout: Layout,
    pub motif: Motif,
    pub candy: bool,
}

/// `above` = lines to print permanently over the region; `region` = the
/// live lines, redrawn each frame.
#[derive(Debug, Default)]
pub struct Frame {
    pub above: Vec<String>,
    pub region: Vec<String>,
}

/// How much of `run.lines` / `run.unknown` / `run.phases` is already in
/// `above`.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delivered {
    pub lines: usize,
    pub unknown: usize,
    pub phases: usize,
}

/// How one segment is painted, once it has been fitted to the width.
#[derive(Clone, Copy)]
enum Paint {
    Plain,
    Dim,
    Bold,
    Ok,
    Bad,
    /// Bold yellow: the running mark, the Pac-Man head.
    Warn,
    Ghost(Ghost),
}

type Seg = (Paint, String);

fn seg(p: Paint, t: impl Into<String>) -> Seg {
    (p, t.into())
}

fn paint(p: Paint, t: &str, s: &Styles) -> String {
    if t.is_empty() {
        return String::new();
    }
    match p {
        Paint::Plain => t.to_string(),
        Paint::Dim => s.dim(t),
        Paint::Bold => s.title(t),
        Paint::Ok => s.success(t),
        Paint::Bad => s.error(t),
        Paint::Warn => s.warn(t),
        Paint::Ghost(g) => s.ghost(g, t),
    }
}

/// Fit plain segments into `width` columns, **then** style each one: a
/// styled string is never cut, so no reset code is ever lost.
fn line(segs: &[Seg], width: usize, s: &Styles) -> String {
    let mut out = String::new();
    let mut left = width;
    for (p, t) in segs {
        let w = t.width();
        if w <= left {
            out += &paint(*p, t, s);
            left -= w;
        } else {
            out += &paint(*p, &cut(t, left, s), s);
            break;
        }
    }
    out
}

/// `t` cut to `w` columns, the last of them an ellipsis.
fn cut(t: &str, w: usize, s: &Styles) -> String {
    if t.width() <= w {
        return t.to_string();
    }
    let Some(room) = w.checked_sub(1) else {
        return String::new();
    };
    let mut out = String::new();
    let mut used = 0;
    for c in t.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw > room {
            break;
        }
        used += cw;
        out.push(c);
    }
    out.push_str(if s.plain() { "~" } else { "…" });
    out
}

/// The Unicode glyph, or its ASCII stand-in on the plain path.
fn g(s: &Styles, uni: &'static str, ascii: &'static str) -> &'static str {
    if s.plain() { ascii } else { uni }
}

fn name(k: PhaseKind) -> &'static str {
    match k {
        PhaseKind::Sync => "sync",
        PhaseKind::Plan => "plan",
        PhaseKind::Download => "download",
        PhaseKind::Checks => "checks",
        PhaseKind::PreHooks => "pre-hooks",
        PhaseKind::Packages => "packages",
        PhaseKind::PostHooks => "post-hooks",
    }
}

fn count(p: &Phase) -> String {
    match (p.state, p.total) {
        (PhaseState::Pending, _) => String::new(),
        (PhaseState::Running, Some(t)) => format!("{}/{t}", p.done),
        _ => p.done.to_string(),
    }
}

fn took(started: Option<Duration>, ended: Option<Duration>, now: Duration) -> Option<Duration> {
    Some(ended.unwrap_or(now).saturating_sub(started?))
}

fn phase(run: &Run, k: PhaseKind) -> Option<&Phase> {
    run.phases.iter().find(|p| p.kind == k)
}

fn all_notes(run: &Run) -> impl Iterator<Item = &Note> {
    let hooks = run.pre_hooks.iter().chain(&run.post_hooks);
    run.notes.iter().chain(hooks.flat_map(|h| &h.notes))
}

fn download_size(run: &Run, s: &Styles) -> String {
    run.totals
        .iter()
        .find(|t| t.0 == "Total Download Size")
        .map(|t| format!(" {} {}", s.bullet(), t.1))
        .unwrap_or_default()
}

/// `pacman -Syu · packages · 2m00s`, plus one ghost per kind of note that
/// wants the user (arcade).
fn header(run: &Run, view: &View, now: Duration, s: &Styles) -> Vec<Seg> {
    let b = s.bullet();
    let at = match run.current() {
        Some(k) => name(k),
        None if run.finished => "done",
        None => "starting",
    };
    let mut segs = vec![
        seg(Paint::Bold, run.command.clone()),
        seg(
            Paint::Dim,
            format!(" {b} {at} {b} {}", duration(run.ended.unwrap_or(now))),
        ),
    ];
    if view.motif == Motif::Arcade {
        let ghosts = [
            (Ghost::Blinky, NoteKind::Error { recovered: false }),
            (Ghost::Pinky, NoteKind::Pacnew),
            (Ghost::Inky, NoteKind::Optdepends),
            (Ghost::Clyde, NoteKind::Warning),
        ];
        for (ghost, kind) in ghosts {
            if all_notes(run).any(|n| n.kind == kind) {
                segs.push(seg(Paint::Ghost(ghost), format!(" {}", g(s, "ᗣ", "G"))));
            }
        }
    }
    segs
}

fn phase_row(p: &Phase, now: Duration, s: &Styles) -> Vec<Seg> {
    let (mark_paint, mark) = match p.state {
        PhaseState::Done => (Paint::Ok, s.check()),
        PhaseState::Running => (Paint::Warn, g(s, "▸", ">")),
        PhaseState::Pending => (Paint::Dim, g(s, "·", ".")),
    };
    let time = took(p.started, p.ended, now)
        .map(duration)
        .unwrap_or_default();
    let text = format!(" {:<10} {:>7}  {time}", name(p.kind), count(p));
    let text_paint = if p.state == PhaseState::Pending {
        Paint::Dim
    } else {
        Paint::Plain
    };
    vec![
        seg(mark_paint, mark),
        seg(text_paint, text.trim_end().to_string()),
    ]
}

/// Done phases folded into one row: `✓ sync 5 · plan 38 · download 38  1m17s`.
fn done_summary(run: &Run, now: Duration, s: &Styles) -> Option<Vec<Seg>> {
    let done: Vec<&Phase> = run
        .phases
        .iter()
        .filter(|p| p.state == PhaseState::Done)
        .collect();
    if done.is_empty() {
        return None;
    }
    let list: Vec<String> = done
        .iter()
        .map(|p| format!("{} {}", name(p.kind), p.done))
        .collect();
    let total: Duration = done
        .iter()
        .filter_map(|p| took(p.started, p.ended, now))
        .sum();
    let sep = format!(" {} ", s.bullet());
    Some(vec![
        seg(Paint::Ok, s.check()),
        seg(
            Paint::Dim,
            format!(" {}  {}", list.join(&sep), duration(total)),
        ),
    ])
}

/// `⏱ 12s · ∅ 23s (51 runs)` — the median is an estimate from past runs.
fn hook_time(h: &Hook, now: Duration, s: &Styles) -> String {
    let t = took(h.started, h.ended, now)
        .map(duration)
        .unwrap_or_default();
    let typical = |m: Median| duration(Duration::from_secs(m.secs));
    match (h.median, s.plain()) {
        (Some(m), false) => format!("⏱ {t} · ∅ {} ({} runs)", typical(m), m.runs),
        (Some(m), true) => format!("{t} - ~{} ({} runs)", typical(m), m.runs),
        (None, false) => format!("⏱ {t}"),
        (None, true) => t,
    }
}

/// What the running phase is on now: a package, a hook, the download.
fn item(run: &Run, now: Duration, s: &Styles) -> Option<Vec<Seg>> {
    let k = run.current()?;
    let ph = phase(run, k)?;
    let total = ph.total.map_or_else(|| "?".to_string(), |t| t.to_string());
    match k {
        PhaseKind::Packages => {
            // The model keeps packages in plan order; the one on screen now
            // is the last `(i/n) verb name` line pacman settled.
            let (verb, pname) =
                run.lines
                    .iter()
                    .rev()
                    .find_map(|l| match classify_screen(l, false) {
                        ScreenEvent::Package { verb, name, .. } => Some((verb, name)),
                        _ => None,
                    })?;
            let p = run.packages.iter().find(|p| p.name == pname)?;
            let mut segs = vec![seg(
                Paint::Plain,
                format!("({}/{total}) {verb} {pname}", ph.done),
            )];
            if let (Some(old), Some(new)) = (&p.old, &p.new) {
                segs.push(seg(Paint::Dim, format!("  {old} {} {new}", s.arrow())));
            }
            Some(segs)
        }
        PhaseKind::PreHooks | PhaseKind::PostHooks => {
            let list = if k == PhaseKind::PreHooks {
                &run.pre_hooks
            } else {
                &run.post_hooks
            };
            let (i, h) = list
                .iter()
                .enumerate()
                .rfind(|(_, h)| h.started.is_some())?;
            let desc = h.desc.as_deref().or(h.file.as_deref()).unwrap_or_default();
            Some(vec![
                seg(Paint::Plain, format!("({}/{total}) {desc}", i + 1)),
                seg(Paint::Dim, format!("  {}", hook_time(h, now, s))),
            ])
        }
        PhaseKind::Download => Some(vec![seg(
            Paint::Plain,
            format!("{}/{total}{}", ph.done, download_size(run, s)),
        )]),
        PhaseKind::Sync => Some(vec![seg(Paint::Plain, run.dbs.last()?.clone())]),
        _ => None,
    }
}

/// The measured bar of the running phase (`[-----C o  ] 50%`), or — arcade,
/// once packages are known — the package lane.
fn meter(run: &Run, view: &View, w: usize, s: &Styles) -> Option<String> {
    if view.motif == Motif::Arcade && !run.packages.is_empty() {
        return Some(lane(run, w, s));
    }
    let ph = phase(run, run.current()?)?;
    let total = ph.total.filter(|&t| t > 0)?;
    let pct = u8::try_from(ph.done.min(total) * 100 / total).unwrap_or(100);
    // `[` + cells + `] ` + up to `100%`: the row is `cells + 7` wide.
    let cells = w.saturating_sub(7).min(40);
    if cells == 0 {
        return Some(line(&[seg(Paint::Plain, format!("{pct}%"))], w, s));
    }
    Some(format!("[{}] {pct}%", bar(pct, cells, view.candy, s)))
}

/// One `·` per real package, `ᗧ` at the done count, a pellet `●` at the end
/// when a reboot is due. Scaled down only when it would not fit.
fn lane(run: &Run, w: usize, s: &Styles) -> String {
    let n = run.packages.len();
    let done = run.packages.iter().filter(|p| p.done).count().min(n);
    let room = w.saturating_sub(3).max(1);
    let (cells, eaten) = if n > room {
        (room, done * room / n)
    } else {
        (n, done)
    };
    let mut segs = vec![
        seg(Paint::Plain, " ".repeat(eaten)),
        seg(Paint::Warn, g(s, "ᗧ", "C")),
        seg(Paint::Dim, g(s, "·", ".").repeat(cells - eaten)),
    ];
    if all_notes(run).any(|n| n.kind == NoteKind::Reboot) {
        segs.push(seg(Paint::Bold, format!(" {}", g(s, "●", "o"))));
    }
    line(&segs, w, s)
}

/// Phase rows for the inline layouts, roomiest first: every phase; done
/// ones folded; pending ones dropped too; the running one alone.
fn phase_variants(run: &Run, now: Duration, s: &Styles) -> [Vec<Vec<Seg>>; 4] {
    let rows = |keep: fn(&Phase) -> bool| -> Vec<Vec<Seg>> {
        run.phases
            .iter()
            .filter(|p| keep(p))
            .map(|p| phase_row(p, now, s))
            .collect()
    };
    let folded = |keep: fn(&Phase) -> bool| -> Vec<Vec<Seg>> {
        let mut v: Vec<Vec<Seg>> = done_summary(run, now, s).into_iter().collect();
        v.extend(rows(keep));
        v
    };
    [
        rows(|_| true),
        folded(|p| p.state != PhaseState::Done),
        folded(|p| p.state == PhaseState::Running),
        rows(|p| p.state == PhaseState::Running),
    ]
}

fn live(run: &Run, view: &View, w: usize, height: usize, now: Duration, s: &Styles) -> Vec<String> {
    let fit = |segs: &Vec<Seg>| line(segs, w, s);
    let mut tail: Vec<String> = item(run, now, s).iter().map(fit).collect();
    tail.extend(meter(run, view, w, s));
    let mut rows = vec![fit(&header(run, view, now, s))];
    match view.layout {
        Layout::Inline => {
            // Fixed height, so the region never jumps.
            let h = (height / 3).clamp(1, 10);
            let variants = phase_variants(run, now, s);
            let pick = variants
                .iter()
                .position(|v| 1 + v.len() + tail.len() <= h)
                .unwrap_or(variants.len() - 1);
            rows.extend(variants[pick].iter().map(fit));
            rows.extend(tail);
            rows.truncate(h);
            rows.resize(h, String::new());
            return rows;
        }
        Layout::InlineGrow => {
            rows.extend(phase_variants(run, now, s)[1].iter().map(fit));
            rows.extend(tail);
        }
        Layout::Nom => {
            let (_, last, _, _) = s.tree_glyphs();
            for p in &run.phases {
                rows.push(fit(&phase_row(p, now, s)));
                if p.state == PhaseState::Running
                    && let Some(mut it) = item(run, now, s)
                {
                    it.insert(0, seg(Paint::Dim, format!("  {last}")));
                    rows.push(fit(&it));
                }
            }
        }
        Layout::Receipt => {
            let one = match run.current().and_then(|k| phase(run, k)) {
                Some(p) => {
                    let mut segs = phase_row(p, now, s);
                    if let Some(it) = item(run, now, s) {
                        segs.push(seg(Paint::Plain, "  "));
                        segs.extend(it);
                    }
                    segs
                }
                None => header(run, view, now, s),
            };
            return vec![fit(&one)];
        }
    }
    rows.truncate(height.saturating_sub(1).max(1));
    rows
}

/// The plan as pacman printed it: from `Package (N)` to the last total
/// (`start..end` in `run.lines`), and where its prompt line settled, if it has.
fn plan(run: &Run) -> Option<(usize, usize, Option<usize>)> {
    let start = run
        .lines
        .iter()
        .rposition(|l| l.starts_with("Package (") || l.starts_with("Packages ("))?;
    let prompt = run.lines[start..]
        .iter()
        .position(|l| is_prompt(l))
        .map(|i| start + i);
    let mut end = prompt.unwrap_or(run.lines.len());
    while end > start && run.lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    Some((start, end, prompt))
}

fn is_prompt(l: &str) -> bool {
    let t = l.trim_end();
    t.ends_with("[Y/n]") || t.ends_with("[y/N]")
}

/// One frame of the live view. `sent` says what earlier frames already put
/// in `above`; the returned `Delivered` covers this frame too, so the same
/// run drawn again with it gives an empty `above`.
///
/// `run.lines` is counted in `Delivered::lines` by `nom` (every settled
/// line) and by the others for the plan only, which they print once —
/// at the prompt when it does not fit, otherwise once it is answered.
#[allow(clippy::too_many_arguments)]
pub fn frame(
    run: &Run,
    view: &View,
    sent: Delivered,
    width: u16,
    height: u16,
    now: Duration,
    idle_prompt: Option<&[String]>,
    s: &Styles,
) -> (Frame, Delivered) {
    let w = usize::from(width);
    let height = usize::from(height);
    let mut above = Vec::new();
    let mut d = sent;
    let from = |v: &[String], i: usize| v.get(i..).unwrap_or_default().to_vec();
    let plan = plan(run);
    // Unknown lines inside an open plan wait: the plan goes up whole, in
    // place, so sending them now would print them twice.
    // ponytail: an unknown line in the same frame as the plan header waits
    // too and is never sent; per-line indices in the model if that shows up.
    let plan_open = plan.is_some_and(|(_, _, p)| p.is_none_or(|p| sent.lines <= p));
    if view.layout == Layout::Nom {
        // Every settled line goes up as printed, unrecognised ones included.
        above.extend(from(&run.lines, sent.lines));
        d.lines = run.lines.len();
        d.unknown = run.unknown.len();
    } else if !plan_open {
        above.extend(from(&run.unknown, sent.unknown));
        d.unknown = run.unknown.len();
    }

    // Once answered, the plan stays in scrollback, prompt line and all.
    if idle_prompt.is_none()
        && let Some((a, b, Some(p))) = plan
        && d.lines <= p
    {
        if d.lines < b {
            above.extend(run.lines[a.max(d.lines)..b].iter().cloned());
        }
        above.push(run.lines[p].clone());
        d.lines = p + 1;
        d.unknown = run.unknown.len();
    }

    if view.layout == Layout::Receipt {
        // One permanent line per phase as it completes; a phase that never
        // ran is passed over once a later one has started.
        while let Some(p) = run.phases.get(d.phases) {
            let later = run.phases[d.phases + 1..]
                .iter()
                .any(|q| q.state != PhaseState::Pending);
            match p.state {
                PhaseState::Done => above.push(phase_done(run, p, w, s)),
                PhaseState::Pending if later => {}
                _ => break,
            }
            d.phases += 1;
        }
    }

    let region = match idle_prompt {
        Some(rows) => {
            let prompt = rows.last().cloned().unwrap_or_default();
            match plan {
                // The whole plan as pacman printed it, if it fits; else it
                // goes to scrollback once and the prompt stands alone.
                Some((a, b, None)) if is_prompt(&prompt) => {
                    let lines = &run.lines[a..b];
                    if d.lines >= b {
                        vec![prompt]
                    } else if lines.len() < height.saturating_sub(1)
                        && lines.iter().chain([&prompt]).all(|l| l.width() <= w)
                    {
                        let mut r = lines.to_vec();
                        r.push(prompt);
                        r
                    } else {
                        above.extend(lines.iter().cloned());
                        d.lines = b;
                        d.unknown = run.unknown.len();
                        vec![prompt]
                    }
                }
                // Verbatim: a cut prompt can lose its answer choices.
                _ => rows.to_vec(),
            }
        }
        None => live(run, view, w, height, now, s),
    };
    (Frame { above, region }, d)
}

/// `✓ download 38 · 702.91 MiB · 1m17s`
fn phase_done(run: &Run, p: &Phase, w: usize, s: &Styles) -> String {
    let size = match p.kind {
        PhaseKind::Download => download_size(run, s),
        _ => String::new(),
    };
    let time = took(p.started, p.ended, Duration::ZERO)
        .map(duration)
        .unwrap_or_default();
    let text = format!(" {} {}{size} {} {time}", name(p.kind), p.done, s.bullet());
    line(&[seg(Paint::Ok, s.check()), seg(Paint::Plain, text)], w, s)
}

/// The receipt on exit 0; the failure block otherwise.
pub fn closing(run: &Run, view: &View, width: u16, transcript: &str, s: &Styles) -> Vec<String> {
    let w = usize::from(width);
    let mut out = vec![line(&end_segs(run, view.motif, s), w, s)];
    if let Outcome::Failed(_) = run.outcome() {
        // pacman's errors verbatim, a little context, where the rest is.
        let errors = all_notes(run).filter(|n| matches!(n.kind, NoteKind::Error { .. }));
        out.extend(errors.map(|n| format!("error: {}", n.text)));
        let context: Vec<&String> = run
            .lines
            .iter()
            .rev()
            .filter(|l| !l.trim().is_empty() && !l.starts_with("error: "))
            .take(5)
            .collect();
        out.extend(context.into_iter().rev().map(|l| s.dim(l)));
        out.push(format!("transcript: {transcript}"));
        return out;
    }
    let arcade = view.motif == Motif::Arcade;
    let bullet = (Paint::Dim, s.bullet());
    let mark = |ghost: Ghost| {
        if arcade {
            (Paint::Ghost(ghost), g(s, "ᗣ", "G"))
        } else {
            bullet
        }
    };
    let texts = |want: &dyn Fn(&NoteKind) -> bool| -> Vec<String> {
        run.needs_you()
            .into_iter()
            .filter(|n| want(&n.kind))
            .map(|n| n.text.clone())
            .collect()
    };
    let mut groups: Vec<((Paint, &str), String)> = Vec::new();
    if !texts(&|k| *k == NoteKind::Reboot).is_empty() {
        let pellet = if arcade {
            (Paint::Bold, g(s, "●", "o"))
        } else {
            bullet
        };
        groups.push((pellet, "reboot recommended".into()));
    }
    let rebuild: Vec<String> = texts(&|k| *k == NoteKind::Rebuild)
        .iter()
        .filter_map(|t| t.split_whitespace().last().map(str::to_string))
        .collect();
    if !rebuild.is_empty() {
        groups.push((bullet, format!("rebuild: {}", rebuild.join(", "))));
    }
    let pacnew = texts(&|k| *k == NoteKind::Pacnew);
    if !pacnew.is_empty() {
        groups.push((
            mark(Ghost::Pinky),
            format!(".pacnew: {}", pacnew.join(", ")),
        ));
    }
    let opt = texts(&|k| *k == NoteKind::Optdepends);
    if !opt.is_empty() {
        groups.push((mark(Ghost::Inky), format!("optional: {}", opt.join("; "))));
    }
    let retried = texts(&|k| *k == NoteKind::Error { recovered: true });
    if !retried.is_empty() {
        groups.push((
            mark(Ghost::Blinky),
            format!("retried: {}", retried.join("; ")),
        ));
    }
    // Snapshots by config, first → last: `snap-pac root 1001 → 1002`.
    let mut snaps: Vec<(&str, u64, u64)> = Vec::new();
    for n in run.needs_you() {
        if let NoteKind::Snapshot { config, number } = &n.kind {
            match snaps.iter_mut().find(|x| x.0 == config) {
                Some(x) => x.2 = *number,
                None => snaps.push((config, *number, *number)),
            }
        }
    }
    if !snaps.is_empty() {
        let list: Vec<String> = snaps
            .iter()
            .map(|(c, a, b)| {
                if a == b {
                    format!("{c} {a}")
                } else {
                    format!("{c} {a} {} {b}", s.arrow())
                }
            })
            .collect();
        groups.push((bullet, format!("snap-pac {}", list.join(", "))));
    }
    let n = run.unknown.len();
    if n > 0 {
        let lines = if n == 1 { "line" } else { "lines" };
        let b = s.bullet();
        groups.push((
            bullet,
            format!("{n} {lines} not recognised {b} paclens transcript"),
        ));
    }
    for ((p, m), text) in groups {
        out.push(line(
            &[seg(p, m), seg(Paint::Plain, format!(" {text}"))],
            w,
            s,
        ));
    }
    out
}

fn end_segs(run: &Run, motif: Motif, s: &Styles) -> Vec<Seg> {
    let arcade = motif == Motif::Arcade;
    let ok = if arcade {
        (Paint::Warn, g(s, "ᗧ··", "C.."))
    } else {
        (Paint::Ok, s.check())
    };
    let fail = if arcade {
        (Paint::Ghost(Ghost::Blinky), g(s, "ᗣ", "G"))
    } else {
        (Paint::Bad, s.cross())
    };
    let took = duration(run.ended.unwrap_or_default());
    let eaten = run.packages.iter().filter(|p| p.done).count();
    let b = s.bullet();
    let ((p, mark), text) = match run.outcome() {
        Outcome::Succeeded => (ok, format!(" {eaten} eaten in {took}")),
        Outcome::NothingChanged => {
            let mark = if arcade {
                (Paint::Warn, g(s, "ᗧ", "C"))
            } else {
                ok
            };
            (mark, format!(" nothing to do {b} clean board"))
        }
        Outcome::Unknown => (
            (Paint::Warn, "?"),
            format!(" pacman exit 0 {b} result unknown (no pacman.log entry)"),
        ),
        Outcome::Failed(Some(n)) => (fail, format!(" pacman exit {n}")),
        Outcome::Failed(None) => (fail, " pacman stopped by a signal".to_string()),
        Outcome::Running => (
            (Paint::Dim, g(s, "…", "...")),
            " pacman running".to_string(),
        ),
    };
    vec![seg(p, mark), seg(Paint::Plain, text)]
}

/// `ᗧ·· 38 eaten in 2m04s` and its clean-board, unknown and failure variants.
pub fn end_line(run: &Run, motif: Motif, s: &Styles) -> String {
    line(&end_segs(run, motif, s), usize::MAX, s)
}

/// pacman's own progress bar, `cells` wide — pacman 7 `callback.c`
/// `fill_progress`. `eaten = pct * cells / 100` (integer division).
///
/// No candy: `#` for each eaten cell, `-` for the rest.
/// Candy (`ILoveCandy`): cell `j` (0 = left) is `-` while `j < eaten`; the
/// head sits at `j == eaten` (none at 100%); past it, counting `i = cells - j`
/// down from the right as pacman does, `o` where `i % 3 == 0`, else a space.
/// pacman flips the mouth each time the head moves; that needs state across
/// frames, so here the mouth follows the percent: `C` even, `c` odd. The
/// head is bold yellow, as in pacman.
pub fn bar(pct: u8, cells: usize, candy: bool, s: &Styles) -> String {
    let pct = usize::from(pct.min(100));
    let eaten = pct * cells / 100;
    if !candy {
        return "#".repeat(eaten) + &"-".repeat(cells - eaten);
    }
    let mut out = "-".repeat(eaten);
    for j in eaten..cells {
        if j == eaten {
            out += &s.warn(if pct.is_multiple_of(2) { "C" } else { "c" });
        } else if (cells - j).is_multiple_of(3) {
            out.push('o');
        } else {
            out.push(' ');
        }
    }
    out
}

/// An uncommented `ILoveCandy` line in pacman.conf.
pub fn candy_enabled(pacman_conf: &str) -> bool {
    pacman_conf
        .lines()
        .any(|l| l.split('#').next().unwrap_or_default().trim() == "ILoveCandy")
}

/// `12s`, `2m04s`, `1h02m`.
pub fn duration(d: Duration) -> String {
    let t = d.as_secs();
    match t {
        0..60 => format!("{t}s"),
        60..3600 => format!("{}m{:02}s", t / 60, t % 60),
        _ => format!("{}h{:02}m", t / 3600, t % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ColorTheme;
    use crate::update_view::model::fixture_run;
    use crate::update_view::screen::fixture_lines;
    use std::collections::HashMap;
    use unicode_width::UnicodeWidthStr;

    const Z: Duration = Duration::ZERO;
    const ESC: char = '\u{1b}';

    fn plain() -> Styles {
        Styles::resolve(true, ColorTheme::Dark, false)
    }
    /// Unicode glyphs, no colour.
    fn uni() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, false)
    }
    fn colour() -> Styles {
        Styles::resolve(false, ColorTheme::Dark, true)
    }

    fn empty() -> Run {
        Run::new("pacman -Syu", HashMap::new(), HashMap::new())
    }

    /// The fixture's first `n` settled lines, not finished.
    fn partial(n: usize) -> Run {
        let mut r = empty();
        for l in fixture_lines().iter().take(n) {
            r.apply_line(l, Z);
        }
        r
    }

    fn view(layout: Layout, motif: Motif) -> View {
        View {
            layout,
            motif,
            candy: true,
        }
    }

    fn draw(r: &Run, v: &View, h: u16, s: &Styles) -> Frame {
        frame(r, v, Delivered::default(), 120, h, secs(120), None, s).0
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            match (esc, c) {
                (false, ESC) => esc = true,
                (true, c) if c.is_ascii_alphabetic() => esc = false,
                (true, _) => {}
                (false, c) => out.push(c),
            }
        }
        out
    }

    fn all_views() -> Vec<View> {
        Layout::ALL
            .iter()
            .flat_map(|&l| Motif::ALL.iter().map(move |&m| view(l, m)))
            .collect()
    }

    #[test]
    fn every_layout_and_motif_renders_the_fixture() {
        let r = fixture_run();
        for v in all_views() {
            let f = draw(&r, &v, 40, &plain());
            assert!(!f.region.is_empty(), "{:?}/{:?}", v.layout, v.motif);
            let c = closing(&r, &v, 120, "/t", &plain());
            assert!(c[0].contains("38 eaten"), "{c:?}");
            assert!(c.len() <= 6, "{c:?}");
        }
    }

    #[test]
    fn receipt_names_reboot_rebuild_snapshot_and_mirror() {
        let c = closing(
            &fixture_run(),
            &view(Layout::Inline, Motif::Subtle),
            120,
            "/t",
            &uni(),
        )
        .join("\n");
        for want in ["reboot", "wps-office", "root 1001 → 1002", "luajit"] {
            assert!(c.contains(want), "{want} missing in\n{c}");
        }
        assert!(!c.contains("not recognised"), "{c}");
    }

    #[test]
    fn receipt_counts_unrecognised_lines() {
        let mut r = fixture_run();
        r.unknown = vec!["odd".into(), "odder".into()];
        let c = closing(&r, &view(Layout::Inline, Motif::Subtle), 120, "/t", &uni());
        assert!(
            c.iter()
                .any(|l| l.ends_with(" 2 lines not recognised · paclens transcript")),
            "{c:?}"
        );
    }

    #[test]
    fn failure_block_pins_errors_and_transcript() {
        let mut r = partial(151);
        r.apply_line("error: failed to commit transaction (conflicting files)", Z);
        r.finish(Some(1), secs(64));
        let c = closing(&r, &view(Layout::Inline, Motif::Subtle), 120, "/t", &uni());
        assert_eq!(c[0], "✗ pacman exit 1");
        assert!(
            c.iter()
                .any(|l| l == "error: failed to commit transaction (conflicting files)")
        );
        // The mirror error never recovered on a failed run: pinned too.
        assert!(c.iter().any(|l| l.contains("failed retrieving file")));
        assert!(c.iter().any(|l| l.contains("( 3/38) upgrading zstd")));
        assert_eq!(c.last().map(String::as_str), Some("transcript: /t"));
    }

    #[test]
    fn signal_failure_says_so() {
        let mut r = empty();
        r.finish(None, Z);
        let c = closing(&r, &view(Layout::Nom, Motif::Arcade), 80, "/t", &uni());
        assert!(c[0].contains("pacman stopped by a signal"), "{c:?}");
    }

    #[test]
    fn inline_height_is_fixed_and_grow_is_not() {
        let mid = partial(161);
        for h in [40u16, 15, 6] {
            let f = draw(&mid, &view(Layout::Inline, Motif::Subtle), h, &plain());
            assert_eq!(f.region.len(), (h as usize / 3).min(10), "h={h}");
        }
        let done = fixture_run();
        let inline = draw(&done, &view(Layout::Inline, Motif::Subtle), 40, &plain());
        let grow = draw(
            &done,
            &view(Layout::InlineGrow, Motif::Subtle),
            40,
            &plain(),
        );
        assert_eq!(inline.region.len(), 10);
        assert!(grow.region.len() < 10, "{:?}", grow.region);
    }

    #[test]
    fn inline_shows_header_phases_item_and_bar() {
        let f = draw(
            &partial(161),
            &view(Layout::Inline, Motif::Subtle),
            40,
            &uni(),
        );
        let t = f.region.join("\n");
        assert!(
            f.region[0].starts_with("pacman -Syu · packages · 2m00s"),
            "{t}"
        );
        assert!(t.contains("download"), "{t}");
        assert!(t.contains("13/38"), "{t}");
        assert!(t.contains("upgrading openssh"), "{t}");
        assert!(t.contains("] 34%"), "{t}");
    }

    #[test]
    fn hook_row_shows_elapsed_and_median() {
        use crate::update_view::medians::Median;
        let medians = HashMap::from([("60-x.hook".to_string(), Median { secs: 23, runs: 51 })]);
        let mut r = Run::new("pacman -Syu", medians, HashMap::new());
        r.apply_log(
            crate::update_view::log_line::classify_log(
                "[2026-10-16T11:37:21+0530] [ALPM] transaction completed",
            )
            .map(|x| x.1)
            .unwrap_or(crate::update_view::log_line::LogEvent::Other),
            secs(1),
        );
        r.apply_log(
            crate::update_view::log_line::classify_log(
                "[2026-10-16T11:37:21+0530] [ALPM] running '60-x.hook'...",
            )
            .map(|x| x.1)
            .unwrap_or(crate::update_view::log_line::LogEvent::Other),
            secs(1),
        );
        r.apply_line(":: Running post-transaction hooks...", secs(1));
        r.apply_line("(1/2) Doing x...", secs(3));
        let f = frame(
            &r,
            &view(Layout::Inline, Motif::Subtle),
            Delivered::default(),
            120,
            40,
            secs(15),
            None,
            &uni(),
        )
        .0;
        assert!(
            f.region
                .iter()
                .any(|l| l.contains("(1/2) Doing x...") && l.ends_with("⏱ 12s · ∅ 23s (51 runs)")),
            "{:?}",
            f.region
        );
    }

    #[test]
    fn frame_lines_never_exceed_width() {
        let runs = [partial(30), partial(120), partial(161), fixture_run()];
        for s in [plain(), colour()] {
            for r in &runs {
                for v in all_views() {
                    let f = frame(r, &v, Delivered::default(), 40, 30, secs(99), None, &s).0;
                    for l in &f.region {
                        let bare = strip(l);
                        assert!(bare.width() <= 40, "{:?}: {bare:?}", v.layout);
                        // The last escape in a line is a reset (all, or colour).
                        if let Some(i) = l.rfind(ESC) {
                            let tail = &l[i..];
                            assert!(
                                tail.starts_with("\u{1b}[0m") || tail.starts_with("\u{1b}[39m"),
                                "{l:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn colour_reaches_the_region() {
        let f = draw(
            &partial(161),
            &view(Layout::Inline, Motif::Subtle),
            40,
            &colour(),
        );
        assert!(f.region.iter().any(|l| l.contains(ESC)));
    }

    #[test]
    fn permanent_lines_are_delivered_once() {
        let mut r = partial(120);
        for v in all_views() {
            let (f1, d1) = frame(&r, &v, Delivered::default(), 120, 40, Z, None, &plain());
            let (f2, d2) = frame(&r, &v, d1, 120, 40, Z, None, &plain());
            assert!(f2.above.is_empty(), "{:?}: {:?}", v.layout, f2.above);
            assert_eq!(d1, d2);
            let _ = f1;
        }
        let v = view(Layout::Inline, Motif::Subtle);
        let (_, d) = frame(&r, &v, Delivered::default(), 120, 40, Z, None, &plain());
        for l in ["odd one", "odd two", "odd three"] {
            r.unknown.push(l.into());
        }
        let (f, _) = frame(&r, &v, d, 120, 40, Z, None, &plain());
        assert_eq!(f.above, ["odd one", "odd two", "odd three"]);
    }

    #[test]
    fn nom_prints_settled_lines_once() {
        let r = partial(12);
        let v = view(Layout::Nom, Motif::Subtle);
        let (f, d) = frame(&r, &v, Delivered::default(), 120, 40, Z, None, &plain());
        assert_eq!(f.above, fixture_lines()[..12]);
        assert_eq!(d.lines, 12);
    }

    #[test]
    fn receipt_layout_prints_each_done_phase_once() {
        let r = fixture_run();
        let v = view(Layout::Receipt, Motif::Subtle);
        let (f, d) = frame(&r, &v, Delivered::default(), 120, 40, Z, None, &uni());
        assert_eq!(f.region.len(), 1);
        assert!(
            f.above
                .contains(&"✓ download 38 · 702.91 MiB · 0s".to_string()),
            "{:?}",
            f.above
        );
        assert_eq!(d.phases, 7);
        let mid = partial(120);
        let (f, d) = frame(&mid, &v, Delivered::default(), 120, 40, Z, None, &uni());
        assert!(
            f.above.iter().any(|l| l.starts_with("✓ sync 5")),
            "{:?}",
            f.above
        );
        assert_eq!(d.phases, 2);
    }

    #[test]
    fn whole_plan_shown_at_prompt() {
        let lines = fixture_lines();
        let r = partial(58);
        let prompt = vec![lines[58].clone()];
        let v = view(Layout::Inline, Motif::Subtle);
        let (f, _) = frame(
            &r,
            &v,
            Delivered::default(),
            120,
            49,
            Z,
            Some(&prompt),
            &plain(),
        );
        for row in &lines[13..57] {
            assert!(f.region.contains(row), "missing {row:?}");
        }
        assert_eq!(f.region.last(), Some(&lines[58]));
        assert!(f.region.len() <= 48);

        let (f, d) = frame(
            &r,
            &v,
            Delivered::default(),
            120,
            20,
            Z,
            Some(&prompt),
            &plain(),
        );
        assert_eq!(f.region, prompt);
        assert!(f.above.contains(&lines[13]) && f.above.contains(&lines[56]));
        let (f, _) = frame(&r, &v, d, 120, 20, Z, Some(&prompt), &plain());
        assert!(f.above.is_empty());
        assert_eq!(f.region, prompt);
    }

    #[test]
    fn plan_stays_in_scrollback_after_the_answer() {
        let lines = fixture_lines();
        let r = partial(60);
        let (f, _) = frame(
            &r,
            &view(Layout::Inline, Motif::Subtle),
            Delivered::default(),
            120,
            49,
            Z,
            None,
            &plain(),
        );
        assert!(f.above.contains(&lines[15]) && f.above.contains(&lines[58]));
    }

    #[test]
    fn prompt_shows_pacman_screen_tail() {
        let r = partial(1);
        let rows = vec![
            "…".to_string(),
            ":: Proceed with installation? [Y/n]".to_string(),
        ];
        let (f, _) = frame(
            &r,
            &view(Layout::Receipt, Motif::Arcade),
            Delivered::default(),
            120,
            40,
            Z,
            Some(&rows),
            &plain(),
        );
        assert_eq!(f.region, rows);
    }

    #[test]
    fn long_idle_prompt_is_kept_verbatim() {
        let r = partial(1);
        let rows = vec![":: Replace foo with extra/bar? [Y/n]".to_string()];
        let (f, _) = frame(
            &r,
            &view(Layout::Inline, Motif::Subtle),
            Delivered::default(),
            12,
            40,
            Z,
            Some(&rows),
            &plain(),
        );
        assert_eq!(f.region, rows);
    }

    #[test]
    fn unknown_line_in_plan_goes_up_once() {
        let lines = fixture_lines();
        for height in [49, 20] {
            let mut r = partial(14);
            r.apply_line("odd notice", Z);
            let v = view(Layout::Inline, Motif::Subtle);
            let (f, d) = frame(&r, &v, Delivered::default(), 120, height, Z, None, &plain());
            let mut above = f.above;
            for l in &lines[14..59] {
                r.apply_line(l, Z);
            }
            let prompt = vec![lines[58].clone()];
            let (f, d) = frame(&r, &v, d, 120, height, Z, Some(&prompt), &plain());
            above.extend(f.above);
            r.apply_line(&lines[59], Z);
            let (f, _) = frame(&r, &v, d, 120, height, Z, None, &plain());
            above.extend(f.above);
            let n = above.iter().filter(|l| *l == "odd notice").count();
            assert_eq!(n, 1, "height {height}: {above:?}");
            assert!(above.contains(&lines[58]), "height {height}");
        }
    }

    #[test]
    fn candy_bar_follows_pacman_rule() {
        let p = plain();
        assert_eq!(bar(50, 10, true, &p), "-----C o  ");
        assert_eq!(bar(51, 10, true, &p), "-----c o  ");
        assert_eq!(bar(0, 10, true, &p), "Co  o  o  ");
        assert_eq!(bar(100, 10, true, &p), "----------");
    }

    #[test]
    fn candy_bar_matches_pacman_on_screen() {
        // Fixture lines 101 and 105: 3% and 4% of pacman's 64-cell bar.
        let lines = fixture_lines();
        for (i, pct) in [(101, 3u8), (105, 4)] {
            let on_screen = lines[i]
                .split_once('[')
                .and_then(|(_, r)| r.split_once(']'))
                .map(|(b, _)| b.to_string());
            assert_eq!(Some(bar(pct, 64, true, &plain())), on_screen);
        }
    }

    #[test]
    fn hash_bar_without_candy() {
        assert_eq!(bar(50, 10, false, &plain()), "#####-----");
    }

    #[test]
    fn candy_head_is_bold_yellow() {
        let b = bar(50, 10, true, &colour());
        assert!(b.contains(ESC) && strip(&b) == "-----C o  ", "{b:?}");
    }

    #[test]
    fn candy_read_from_pacman_conf() {
        assert!(candy_enabled("[options]\nILoveCandy\n"));
        assert!(candy_enabled("[options]\n  ILoveCandy   # yes\n"));
        assert!(!candy_enabled("#ILoveCandy\n"));
        assert!(!candy_enabled("[options]\nColor\n"));
    }

    #[test]
    fn durations_read_like_a_stopwatch() {
        assert_eq!(duration(secs(12)), "12s");
        assert_eq!(duration(secs(124)), "2m04s");
        assert_eq!(duration(secs(3725)), "1h02m");
    }

    #[test]
    fn end_line_per_outcome_and_motif() {
        let mut r = fixture_run();
        r.finish(Some(0), secs(124));
        assert_eq!(end_line(&r, Motif::Arcade, &uni()), "ᗧ·· 38 eaten in 2m04s");
        assert_eq!(end_line(&r, Motif::Subtle, &uni()), "✓ 38 eaten in 2m04s");
        assert_eq!(
            end_line(&r, Motif::Arcade, &plain()),
            "C.. 38 eaten in 2m04s"
        );
        let mut f = empty();
        f.finish(Some(1), Z);
        assert_eq!(end_line(&f, Motif::Subtle, &uni()), "✗ pacman exit 1");
        assert_eq!(end_line(&f, Motif::Arcade, &uni()), "ᗣ pacman exit 1");
    }

    #[test]
    fn clean_board_when_nothing_changed() {
        let mut r = empty();
        r.apply_log(
            crate::update_view::log_line::classify_log(
                "[2026-10-16T11:35:36+0530] [PACMAN] Running 'pacman -Syu'",
            )
            .map(|x| x.1)
            .unwrap_or(crate::update_view::log_line::LogEvent::Other),
            Z,
        );
        r.apply_line(" there is nothing to do", Z);
        r.finish(Some(0), secs(3));
        let l = end_line(&r, Motif::Subtle, &uni());
        assert!(l.contains("nothing to do") && !l.contains("eaten"), "{l}");
    }

    #[test]
    fn unknown_outcome_says_so() {
        let mut r = partial(12);
        r.finish(Some(0), Z);
        let l = end_line(&r, Motif::Subtle, &uni());
        assert_eq!(l, "? pacman exit 0 · result unknown (no pacman.log entry)");
    }

    #[test]
    fn arcade_lane_has_one_dot_per_package_left() {
        let f = draw(
            &partial(161),
            &view(Layout::Inline, Motif::Arcade),
            40,
            &uni(),
        );
        let lane = f.region.iter().find(|l| l.contains('ᗧ'));
        let dots = lane.map(|l| l.matches('·').count());
        assert_eq!(dots, Some(38 - 13), "{:?}", f.region);
    }

    #[test]
    fn arcade_receipt_marks_reboot_with_a_pellet() {
        let c = closing(
            &fixture_run(),
            &view(Layout::Inline, Motif::Arcade),
            120,
            "/t",
            &uni(),
        );
        assert!(
            c.iter().any(|l| l.starts_with('●') && l.contains("reboot")),
            "{c:?}"
        );
    }
}
