# paclens

A TUI-first system inspection and update tool for **Arch Linux**. It unifies pacman, the AUR and Flatpak into one interface and layers on advisory features: a `why` dependency inspector, a Flatpak/native overlap detector with migration advisory and execution, and an orphan/cache reporter.

paclens is **not** a package manager. It wraps pacman, whichever AUR helper you already use, and Flatpak; it reads their state and presents it. It never acts without confirmation and never guesses package relationships.

## Status

**0.3.4 — working tool, daily-driveable.** ~16k lines of Rust across `src/`. 462 tests green; `cargo fmt --check` and `clippy -D warnings -D clippy::unwrap_used` clean.

Shipped: the full TUI (dashboard, package list, overlap screen, cleanup screen, pty exec console, log viewer), pacman + AUR + Flatpak providers, the scan cache, the dependency graph, `why`, overlap detection, migration advisory **and** execution, and the cleanup report. Headless equivalents exist for everything except `cleanup`.

Shipped milestones are **history, not a plan** — see "What shipped" below. All open work lives in GitHub issues; there is no roadmap file.

## Source-of-truth documents

Read these before doing non-trivial work.

- **`design.md` — the whole design document, and the only one.** What paclens will and will not do to a user's system (§1–5), how it is built (§6–12), the dated decisions log (§13) that explains why any of it is the way it is, and how the project is run (§14–15). Read §1–5 before proposing anything; read the rest before writing non-trivial code.
- `config.default.toml` — default config schema.
- `overlap_map.toml` — bundled Flatpak-ID → pacman-name map (`include_str!()` into the binary).

**`design.md` outranks this file, the README, and every comment in the code.** It absorbed `spec.md` and `dev-notes.md` on 2026-08-25 — those files no longer exist, and the parts of them that had drifted past the code were dropped rather than carried over. Source comments cite it as `design §N`.

## Design principles

Summary only — **`design.md` is authoritative**, and carries the concrete rules
each principle produces (no `--noconfirm`, no partial upgrades, no removal
without an explanation, no misleading numbers, and the rest).

1. **Explain before acting.** No action runs without showing exactly what will happen.
2. **Safety over aggression.** When in doubt, do nothing. Never remove more than asked.
3. **Honest confidence.** Every inference carries a `Confirmed`, `Inferred`, or `Unknown` label. Never present inference as fact. Never promote a label — an `Unknown` edge in a path caps the verdict at `Unknown`.
4. **Pipeline:** scan → analyze → plan → confirm → execute. No shortcuts, no "fix all" button.
5. **One source of truth:** the scan cache. The TUI, `why`, and the overlap detector all read from it. Nothing re-derives what a scan already computed.
6. **Source-specific logic.** pacman and Flatpak differ in every respect. No generic cross-source shortcuts. *(Under active review — see "What's next".)*

`design.md` also carries **the test** that decides whether something becomes a
rule at all: *can you state the harm?* If yes, it is a rule and it holds. If the
only objection is taste, it is a **default** — off, warned, available — not a
prohibition.

## Architecture

```
CLI/TUI  →  Application core (state, planner, event bus)
         →  Scanner | Analyzer | Executor
         →  Cache layer (scan cache; graph & overlaps recomputed on load)
```

Modules: `main.rs`, `cli/`, `tui/`, `model/`, `providers/`, `scanner/`, `analyzer/`, `executor/`, `config/`, plus `planner.rs`, `format.rs`, `fuzzy.rs`, `glyphs.rs`, `logging.rs`.

Module contracts (design §6):
- **Provider** — accepts an injectable `CommandRunner` (the testing seam); returns `Ok(vec![])` when nothing is installed; `Err` only when the binary exists but the command failed; never calls sudo; never knows about other providers.
- **Scanner** — detects providers, runs them concurrently on scoped threads (`std::thread::scope`), assembles `ScanResult`, writes cache. Never analyzes. One exception, made explicit in the 2026-07-14 decision: it asks the pure analyzer *which* paths to measure, then measures them.
- **Analyzer** — pure: same `ScanResult` → same output. Never calls subprocesses, never writes disk. Builds dep graph, overlaps, orphan list from `ScanResult`.
- **Executor** — only runs pre-built `ActionPlan`s. Never decides what to do. Logs every command. Reports exit codes without interpretation.

## Key technical decisions

Orientation only — **design §3 carries the rules and §13 the dated reasoning.**

- **Dep graph from one `pacman -Qi` call**, not per-package `pactree`. All graph queries run in-memory on `petgraph`.
- **Cache = `ScanResult` serialized to TOML** at `~/.cache/paclens/scan.toml`, currently `SCHEMA_VERSION = 12`. The dep graph and overlaps are recomputed on load, never serialized. Atomic writes: write `.tmp`, then `rename()`.
- **Execution runs on a real pty** (`portable-pty` + `vt100`) inside the TUI. The child sees a genuine terminal, so sudo/doas/pkexec/pacman/paru prompt, colour and redraw natively; every key including Ctrl-C passes through. This replaced both the piped-stdio console and the original suspend/restore flow.
- **No `--noconfirm` for pacman**, ever. It suppresses conflict resolution.
- **The dashboard *is* the plan view.** There is no separate update screen. `space` toggles a source, **`enter` runs the plan** (`u` is an alias), `i` opens the selected source's package list, and the console and log viewer are screen-independent overlays. There is no in-TUI confirm modal — the plan is visible and the tools ask their own questions.
- **AUR is the libalpm split, not a second scan.** Foreign packages already carry full `pacman -Qi` metadata; the scanner relabels them via `pacman -Qm`. the helper only does what pacman can't: update detection (`-Qua`) and the update step (`-Sua`). paru, yay and pikaur are autodetected in that order. **An AUR helper is never run under sudo** — they self-elevate.
- **Overlap matching** in priority order: known map → reverse-DNS suffix → display-name match, each with a decreasing confidence label. A generic blocklist suppresses false positives. A false negative is better than a false positive.
- **Migration copy plans contain no `rm` anywhere.** Backups are staged into a timestamped dir, then targets are copied with `cp -aT`. Source removal is a separate plan, armed only by a clean copy and a user's explicit verification. `ActionKind::Migrate` is never privileged; `ActionKind::Remove` follows the source.
- **Cleanup figures are honest.** The reclaimable number comes from the matching `paccache -dk2` dry run, shown next to the total — an 11 GiB cache that reclaims nothing says so. `pacman -Sc` is never suggested (it trips over pacman ≥7's sandboxed-download partials); `paccache` and `paru -Sc --aur` are.

## Conventions

- **Hold the line on `design.md`.** If a request conflicts with a rule there — whoever it comes from, including the repo owner — say so *before* doing the work, name the rule, and ask. A rule changing is a fine outcome; a rule eroding quietly because it was inconvenient one afternoon is not. Apply the test first: if the request touches a **default** rather than a rule, just do it — defaults exist to be flipped, and pushing back on one is noise.
- **No `unwrap()` / `expect()` in production paths** — `#![deny(clippy::unwrap_used)]`. Use `anyhow::Result` for app code, `thiserror` for provider error types.
- Provider errors are isolated: one source failing must not abort others.
- TUI: rendering fns take `&App` (never mutate); event handlers take `&mut App` (the only mutators). No global mutable state.
- Colors are centralized per render target: TUI styles in `src/tui/theme.rs`, CLI text styling in `src/cli/style.rs`; both follow one semantic palette (green = available, yellow = pending updates, dim = secondary, bold = emphasis) and share glyphs from `src/glyphs.rs`. Color is suppressed by `--no-color`, by `color_theme = "none"`, and (for the CLI) when the stream is not a TTY; the no-color path also switches to ASCII box drawing and ASCII glyphs.
- Every parser has unit tests against real-output fixtures in `tests/fixtures/`, driven by a mock `CommandRunner`. Capture fixtures from a real Arch system.
- **Every config knob must be consumed.** A knob that exists in the schema but changes no behaviour is a bug (v0.2.0 audit).
- **Versioning:** a minor is a capability you can point at; a patch is fixes and polish. Every release gets a `vX.Y.Z` git tag — the PKGBUILD builds from `#tag=v$pkgver` and cannot build without one. **design §14 says when each one ships** — a minor is cut per describable capability, several to a milestone, never on a milestone boundary.
- **Git:** work on `main` directly — this is a solo repo, no feature branches. Commits are authored by the repo owner alone: **no `Co-Authored-By` or `Claude-Session` trailers.** Keep the existing message style — a `type(scope):` subject line, then prose explaining *why*, not a bullet list of what changed.
- **Git hooks do the checking.** `.githooks/` is tracked and wired up with `git config core.hooksPath .githooks` (local config, so a fresh clone must set it once). `pre-commit` runs `cargo fmt` (fixing and re-staging), then clippy and the tests, aborting the commit if either fails; it skips entirely when no Rust or manifest file is staged. `pre-push` runs `cargo install --path .` so `paclens` on PATH is what last landed — on push rather than on commit, because the fat-LTO relink costs ~80s (#76) and commits here are deliberately granular. It never blocks the push, and skips when the pushed range changes no build file. `pre-push` also publishes to the AUR, but only what it can verify: pushing a `v*` tag schedules the **source** package, which waits in the background for the tag to reach origin (the PKGBUILD builds from that tag, and pre-push runs before it lands) and gives up rather than publishing if the push never happens; **`paclens-bin` is never published by a tag push** — its checksum belongs to a release asset the workflow builds minutes later, so it goes out on the push that changes `packaging/`, and only when the sum in the tree matches the asset actually published. `.SRCINFO` is regenerated with `makepkg --printsrcinfo`, and every check is read from it rather than from the PKGBUILD text. Escape hatches: `--no-verify`, `PACLENS_SKIP_HOOKS=1`, `PACLENS_NO_INSTALL=1` for the install alone, `PACLENS_NO_AUR=1` for the AUR alone, and `PACLENS_AUR=1` to force the AUR check on an ordinary push. Log: `~/.cache/paclens/aur-push.log`.
- **Testing is a hard requirement, not an afterthought.** Every module carries unit tests; every feature ships with tests. Keep them small, granular, and specific — test pure helpers directly, not just via their callers. Make logic hermetically testable by injecting the `CommandRunner` seam and passing environment-derived inputs (availability flags, mtimes) into pure cores rather than reading PATH/filesystem inside the logic (see `scan`→`assemble`, `staleness`→`staleness_with`). Integration tests in `tests/` drive the built binary (`CARGO_BIN_EXE_paclens`) sandboxed with temp `XDG_*` dirs. `cargo test`, `clippy -- -D warnings -D clippy::unwrap_used`, and `fmt --check` stay green on every commit.

## What shipped

Three capability blocks, one minor each:

```
0.1.0  see        CLI + TUI, config, logging, pacman/flatpak providers, scan
                  cache, data model, dep graph, quadrant dashboard, accurate
                  update detection, updates executed on a real pty
0.2.0  understand why with labeled reverse-dep chains, app→runtime edges,
                  overlap detection and the tradeoff screen, cleanup report,
                  every config knob consumed, provider timeouts
0.3.0  extend     AUR as its own source via paru, migration advisory and
                  execution behind backups, honest reclaimable cleanup figures
```

**Renumbered 2026-08-24.** The old scheme mixed granularities — 0.1.x took a
patch bump per feature, then 0.2.0–0.5.0 took a whole minor each for the same
size of work. The original `v0.0.1`–`v0.4.0` tags were deleted in the same
change: they were never published to the AUR or crates.io and nothing outside
this repo consumed them. Older labels in commit messages, decision-log
entries and source comments map forward like this:

```
v0.0.1 – v0.1.1  →  0.1.0
v0.1.2 – v0.2.0  →  0.2.0
v0.3.0 – v0.5.0  →  0.3.0
```

## What's next

**GitHub issues are the only plan.** `roadmap.md` was retired on 2026-08-24 once every forward-looking item in it had an issue; the version history it recorded is summarized above and preserved in git. Do not recreate it — a second planning surface is how the last one went stale.

**Issue #75 is the roadmap — start there.** It is the only surface that sequences work across themes: what is next, what is after that, and what is deliberately unscheduled. Everything else groups rather than orders:

| surface | answers |
|---|---|
| **#75** | what do I do next, and then what |
| milestones | which theme this belongs to, how far along it is |
| `Tracking:` issues (#69–#74) | order *within* one theme, and what blocks what |
| labels | everything touching one area — theme (`sources`, `system-health`, `update-flow`, `integration`, `cleanup`, `aur`), area (`area:*`), `priority:*`, `needs-design` |

Where #75 and a tracking issue disagree, #75 wins on order and the tracking issue wins on detail. #75 carries no version numbers, and must not start doing so — see design §14.

**Broader sources.** This is now a stated goal, not a maybe: paclens should be the one tool for everything that updates on this machine. cargo, rustup, npm globals, pipx, fwupd, optionally go/brew. Each lands only after its parser is solid and tested — never as a batch.

Before any of them: **the provider contract needs generalizing** for sources with no install reason, no dependency metadata and no orphan concept. This presses directly on principle 6 and on the "no extension points for deferred features" rule below — both were written when there were two sources. Settle it deliberately and record it in design §13 rather than drifting into it one provider at a time.

The payoff that keeps this from becoming "topgrade with a TUI": overlap detection extended across sources. The same tool installed via pacman *and* cargo is the same duplicate problem as native-vs-Flatpak, and `PATH` precedence decides which one you actually run.

**System health.** Arch news before an update, `.pacnew`/`.pacsave`, reboot-required, services needing restart, db lock detection, stale mirrors, `pacman.log` history, downgrade-from-cache.

These are the two large themes, in no fixed order — whichever lands first takes the next minor. **Do not assign version numbers to unshipped work**; pre-assigning them is exactly what made the old scheme drift, and issues carry the plan now.

**1.0 means the stated goal is real:** paclens is the one tool for everything that updates on this machine, and the advisory layer — `why`, overlaps, migration — covers every source it knows about. Nothing short of that gets a 1.

## Tech stack

ratatui + crossterm (TUI), portable-pty + vt100 (exec console), clap derive (CLI), petgraph (graph), serde + toml + serde_ignored (config/cache), anyhow + thiserror (errors), tracing + tracing-subscriber + tracing-appender (logging), chrono (timestamps), directories (paths).

**No async runtime.** The scanner uses `std::thread::scope`; there is no `async fn` in the codebase. tokio was a spec-era dependency and was removed in 0.3.0 — do not reintroduce it without a concrete need.

Single binary, no daemon, sudo only for pacman updates.

## Out of scope

- **Never:** any non-Arch distro.
- **Not without a decision first:** a daemon or resident process (a systemd *user timer* invoking a short-lived run is fine; a daemon is not), a plugin system, remote/multi-host operation.
- **Do not add extension points for deferred features.** Build for what exists. *(The v0.6 source work is the one sanctioned exception — see above.)*
- Destructive cleanup automation stays behind the trust ladder: the cleanup screen deliberately has **no action keys**. Suggestions are copiable text until that changes deliberately.
