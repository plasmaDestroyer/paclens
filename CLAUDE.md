# paclens

A TUI-first system inspection and update tool for **Arch Linux**. It unifies pacman and Flatpak into one interface and layers on advisory features: a `why` dependency inspector, a Flatpak/native overlap detector, and an orphan/cache reporter.

paclens is **not** a package manager. It wraps pacman and Flatpak, reads their state, and presents it. It never acts without confirmation and never guesses package relationships.

## Status

Pre-implementation. The design docs are complete; no Rust code is written yet. Build proceeds in the locked order below — do not reorder.

## Source-of-truth documents

Read these before doing non-trivial work. They are authoritative for design intent.

- `spec.md` — canonical technical spec: architecture, data model (`src/model/`), provider/cache/graph/overlap specs, confidence model, CLI/TUI specs, error handling, open questions.
- `roadmap.md` — milestone deliverables and "done when" criteria, v0.0.1 → v0.2.0.
- `dev-notes.md` — implementation guidance, hard parsing problems, module contracts, fixtures, decisions log. **Read before writing non-trivial code.**
- `config.default.toml` — default config schema.
- `overlap_map.toml` — bundled Flatpak-ID → pacman-name map (`include_str!()` into the binary).

The data model in spec.md §4 is canonical; `src/model/` must match it. When an implementation choice arises, check spec.md §18 (open questions) and dev-notes.md §7 (decisions log) first.

## Design principles (hard constraints, not guidelines)

1. **Explain before acting.** No action runs without showing exactly what will happen.
2. **Safety over aggression.** When in doubt, do nothing. Never remove more than asked.
3. **Honest confidence.** Every inference carries a `Confirmed`, `Inferred`, or `Unknown` label. Never present inference as fact. Never promote a label — an `Unknown` edge in a path caps the verdict at `Unknown`.
4. **Pipeline:** scan → analyze → plan → confirm → execute. No shortcuts, no "fix all" button.
5. **One source of truth:** the scan cache. The TUI, `why`, and overlap detector all read from it. Nothing re-derives what a scan already computed.
6. **Source-specific logic.** pacman and Flatpak differ in every respect. No generic cross-source shortcuts.

## Architecture

```
CLI/TUI  →  Application core (state, planner, event bus)
         →  Scanner | Analyzer | Executor
         →  Cache layer (scan cache; graph & overlaps recomputed on load)
```

Module layout (see spec.md §3 for the full tree): `main.rs`, `cli/`, `tui/`, `model/`, `providers/`, `scanner/`, `analyzer/`, `executor/`, `config/`.

Module contracts (dev-notes.md §3):
- **Provider** — accepts an injectable `CommandRunner` (the testing seam); returns `Ok(vec![])` when nothing is installed; `Err` only when the binary exists but the command failed; never calls sudo; never knows about other providers.
- **Scanner** — detects providers, runs them concurrently (`tokio::join!`), assembles `ScanResult`, writes cache. Never analyzes.
- **Analyzer** — pure: same `ScanResult` → same output. Never calls subprocesses, never writes disk. Builds dep graph, overlaps, orphan list from `ScanResult`.
- **Executor** — only runs pre-built `ActionPlan`s. Never decides what to do. Logs every command. Reports exit codes without interpretation.

## Key technical decisions

- **Dep graph from one `pacman -Qi` call**, not per-package `pactree`. Parse `Depends On` / `Required By`; all graph queries run in-memory on `petgraph`. pactree is not a dependency.
- **Cache = `ScanResult` serialized to TOML** at `~/.cache/paclens/scan.toml`. The dep graph and overlaps are recomputed on load, never serialized (avoids petgraph version fragility). Atomic writes: write `.tmp`, then `rename()`.
- **Sudo in TUI:** suspend the TUI (`LeaveAlternateScreen` + disable raw mode), run the command in the raw terminal so the user handles the sudo/pacman prompts directly, then restore. No `--noconfirm` for pacman (it suppresses conflict resolution).
- **Overlap matching** in priority order: known map → reverse-DNS suffix → display-name match, each with a decreasing confidence label. A generic blocklist suppresses false positives. A false negative is better than a false positive.

## Conventions

- **No `unwrap()` / `expect()` in production paths** — `#![deny(clippy::unwrap_used)]`. Use `anyhow::Result` for app code, `thiserror` for provider error types.
- Provider errors are isolated: one source failing must not abort others.
- TUI: rendering fns take `&App` (never mutate); event handlers take `&mut App` (the only mutators). No global mutable state.
- Colors live only in `src/tui/theme.rs`. `--no-color` switches to ASCII box drawing.
- Every parser has unit tests against real-output fixtures in `tests/fixtures/`, driven by a mock `CommandRunner`. Capture fixtures from a real Arch system.
- **Testing is a hard requirement, not an afterthought.** Every module carries unit tests; every feature ships with tests. Keep them small, granular, and specific — test pure helpers directly, not just via their callers. Make logic hermetically testable by injecting the `CommandRunner` seam and passing environment-derived inputs (availability flags, mtimes) into pure cores rather than reading PATH/filesystem inside the logic (see `scan`→`assemble`, `staleness`→`staleness_with`). Integration tests in `tests/` drive the built binary (`CARGO_BIN_EXE_paclens`) sandboxed with temp `XDG_*` dirs; grow them as the surface stabilizes. `cargo test`, `clippy -- -D warnings -D clippy::unwrap_used`, and `fmt --check` stay green on every commit.

## Build order (locked — see roadmap.md / dev-notes.md §1)

```
v0.0.1 skeleton       CLI + empty TUI + config + logging
v0.0.2 providers      pacman -Q, flatpak list → parse
v0.0.3 cache + model  pacman -Qi (full metadata), ScanResult, cache r/w
v0.0.4 dashboard      wire cached data into TUI
v0.0.5 update dry run  show plan, no execution
v0.0.6 first action    execute Flatpak update (no sudo), TUI suspend/restore
v0.0.7 dep graph + why build graph, reverse-dep lookup, verdicts
v0.0.8 overlap detect  cross-reference pacman + Flatpak
v0.0.9 usability       keyboard, colors, speed, errors
```

Do not build out TUI layout/theming/widgets before v0.0.4 — get data flowing through the model first.

## Tech stack

ratatui + crossterm (TUI), tokio (async), clap derive (CLI), petgraph (graph), serde + toml (config/cache), anyhow + thiserror (errors), tracing + tracing-subscriber (logging), directories (paths). Single binary, no daemon, sudo only for pacman updates.

## Out of scope (do not design for these)

AUR/paru (v0.3+), migration advisory/execution (v0.4–0.5), cargo/npm/brew/pipx (v0.6+), destructive cleanup (v0.5+), daemon/plugin system (post-1.0), any non-Arch distro (never). Do not add extension points for deferred features.

## Common commands

```
cargo build
cargo test
cargo fmt --check
cargo clippy -- -D warnings -D clippy::unwrap_used
cargo build --release
```
