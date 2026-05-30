# Handoff: v0.0.4 — Basic dashboard (your milestone)

The core engine is done through v0.0.3 and tagged. v0.0.4 is the first TUI
milestone and it's yours. This note points you at the data, the seams, and the
rules — it does **not** write any TUI code.

## What's ready for you to consume

One call gives you everything the dashboard needs:

```rust
// src/scanner/mod.rs
pub fn load_or_scan(
    runner: &dyn CommandRunner,
    config: &Config,
    refresh: bool,
    config_path: Option<&Path>,
) -> anyhow::Result<ScanResult>
```

- Use the real runner: `crate::providers::SystemCommandRunner`.
- It warm-starts from `~/.cache/paclens/scan.toml`, re-scans when stale, and
  writes the cache back. You don't manage caching — just call it.

The data model you'll render (all in `crate::model`, see spec §4):

- `ScanResult { schema_version, scanned_at, sources, packages, updates, cache_sizes }`
- `Source { id: SourceId, kind: SourceKind, available, last_scanned }`
- `Package { name, version, source_id, install_reason, size_bytes, description, depends_on, required_by, optional_deps, provides }`
- `PendingUpdate { package_name, current_version, available_version, source_id }`
- `SourceId` is `"pacman"`, `"flatpak-user"`, `"flatpak-system"`. Flatpak counts
  = sources/packages whose id starts with `"flatpak"`.

**Best reference:** `src/cli/status.rs` already calls `load_or_scan` and computes
per-source installed/update counts, cache size, and a relative last-scan time.
The dashboard is the TUI version of exactly that data — read it first.

## Where the TUI starts

`src/tui/mod.rs` is the current empty frame: `run()` sets up the terminal, loops
on `draw` + key events, quits on `q`/`Ctrl-C`. That quit handling and the panic-safe
`init`/`restore` are done — build on them.

`run()` currently takes no arguments. You'll want to thread data in. Suggested
shape (your call):

```rust
// in src/cli/mod.rs dispatch — you'll change this line:
Command::Ui => report(tui::run(&config, cli.refresh, config_path.as_deref())),
```

Then inside `tui::run`, call `load_or_scan(...)`, build your `App`, and enter the
loop. (Async scanning with a loading spinner via tokio + mpsc is described in
dev-notes §2.6 / spec §10.1 — you can start synchronous and add that later.)

## Suggested App state (full version in dev-notes §5)

For v0.0.4, you only need a slice of it:

```rust
pub struct App {
    pub scan: ScanResult,        // or Option<ScanResult> if you scan async
    pub scan_state: ScanState,   // Idle | Scanning | Done | Error(String)
    pub cursor: usize,           // selected source row
    // grows in later milestones: screen, detail_pane_open, search, ...
}
```

Rules from dev-notes §5 (hard constraints):
- render fns take `&App` (never mutate); event handlers take `&mut App`.
- no global mutable state; no interior mutability except async channels.

## v0.0.4 deliverables (roadmap)

- Dashboard screen: per-source row (source, installed count, update count, last
  scan time) + a summary row on top.
- Keyboard nav (arrows, `Tab` between sections) + a footer bar of keybindings.
- Data from cache if fresh, scan if stale (just call `load_or_scan`).
- Loading state shown during a scan (if you go async).
- Errors shown **inline** — never a crash or blank screen.

**Done when:** open the TUI, see installed + update counts per source, navigate
with the keyboard, see errors inline.

## Read before you start

- spec §10 — TUI spec (layout model, screens, global keybindings, color palette).
- dev-notes §5 — TUI state management; §2.6 — async output/event loop (for later).
- roadmap "v0.0.4 — Basic dashboard" — deliverables + done-when.
- Keep colors only in `src/tui/theme.rs` (create it) and honor `--no-color`
  (it's parsed in `cli::Cli.no_color`, currently unused — wire it when you add color).

## Conventions that still apply

- No `unwrap()`/`expect()` in production paths (`#![deny(clippy::unwrap_used)]`);
  tests may use them.
- `cargo fmt`, and `cargo clippy -- -D warnings -D clippy::unwrap_used` must stay green.
- Note: Rust 2024 `if let ... && ...` let-chains are available (clippy will ask
  you to collapse nested `if let`s).

When your dashboard is working, commit and tag `v0.0.4`, and I can pick up the
next core milestone (v0.0.5 update dry-run logic + `paclens update --dry-run`).
You can delete this file once v0.0.4 lands.
