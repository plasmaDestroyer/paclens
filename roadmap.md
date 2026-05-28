# paclens — Project Roadmap

> A beautiful, Arch-first TUI for updating, explaining, and safely inspecting your system.

---

## North star

By v0.2.0, the tool does exactly this:

- scans pacman + Flatpak
- updates both from one place
- explains why anything is installed and what removing it breaks
- flags Flatpak/native overlap with clear tradeoff analysis
- reports orphans and cache safely, without touching anything

Nothing else. No magic. No promises it cannot keep.

---

## Core philosophy

1. Explain before acting.
2. Prefer safety over aggression.
3. Show confidence levels. Never pretend certainty you do not have.
4. Scan, analyze, plan, confirm, execute — in that order, always.
5. One source of truth: the scan cache. Never re-derive what was already computed.

---

## Version phases

```
0.0.x  build the engine       prove each piece works in isolation
0.1.x  build the product      assemble pieces into a coherent tool
0.2.x  stabilize              harden, test, ship
0.3+   extend                 only after 0.2 ships and users confirm direction
```

The 0.0.x phase is not a prototype to throw away. It is the foundation. Every struct, every parser, every cache format defined here carries forward. Build it like it matters, because it does.

---

# Phase 0.0 — Build the engine

---

## v0.0.1 — Skeleton

**Goal:** the app starts cleanly and has a structure.

### Deliverables

- Rust binary compiles and runs
- `clap` CLI with `--help`, `--version`, and subcommand skeleton (`ui`, `status`, `update`, `why`, `overlaps`, `cleanup`)
- empty `ratatui` TUI frame opens, renders, and quits cleanly on `q` / `Ctrl-C`
- config file location resolved via `directories` crate (`~/.config/paclens/config.toml`)
- config loads with defaults if file is absent, errors clearly if malformed
- `tracing` + `tracing-subscriber` wired up, log level controlled by config or `--debug` flag
- log output to `~/.local/share/paclens/paclens.log`
- project module layout established: `cli`, `tui`, `config`, `providers`, `scanner`, `analyzer`, `executor`, `model`, `cache`

### Done when

`paclens` runs, opens an empty TUI frame, logs startup to disk, reads config, and exits cleanly.

---

## v0.0.2 — Source probing

**Goal:** prove the scanners work.

### Deliverables

- detect whether `pacman` is available on the system (check PATH)
- detect whether `flatpak` is available on the system (check PATH)
- run read-only commands per source:
  - `pacman -Q` → installed package names and versions (quick list)
  - `pacman -Qu` → available updates
  - `flatpak list --app --columns=application,name,version,origin,installation` → installed apps
  - `flatpak remote-ls --updates --app --columns=application,version` → available updates
- parse raw stdout into preliminary internal structs — no action taken, no display yet
- graceful handling if a source is missing: log it, skip it, do not crash
- `--debug` flag prints parsed results to stderr

### Preliminary structs (refined in v0.0.3)

These are intentionally minimal. They carry just enough to prove the parsers work.

```
Source      { id, kind, available }
RawPackage  { name, version, source_id }
RawUpdate   { name, current, available, source_id }
```

### Done when

Running `paclens --debug` prints parsed package counts and update counts for each detected source.

---

## v0.0.3 — Cache and model

**Goal:** establish the internal shape of the tool.

### Deliverables

- core data model finalized, replacing the preliminary structs from v0.0.2:

```
Source          { id, kind, available, last_scanned }
Package         { name, version, source, install_reason, size, description,
                  depends_on: Vec<String>, required_by: Vec<String> }
DependencyEdge  { from, to, kind: Real | Inferred, confidence }
ScanResult      { schema_version, sources, packages, updates, scanned_at }
```

- pacman scanner upgraded from `pacman -Q` (quick) to `pacman -Qi` (full metadata) to extract `Install Reason`, `Installed Size`, `Description`, `Depends On`, `Required By`
- scan results serialized to `~/.cache/paclens/scan.toml`
- cache reload on next launch: if cache is fresh (age < TTL), skip re-scan
- `--refresh` flag forces re-scan regardless of cache age
- cache versioning: `schema_version` field; mismatch triggers clean re-scan with a logged warning
- atomic cache writes: write to `.tmp`, then rename (see dev-notes Section 2.7)
- explicit type-level separation between `Real` dependency (from pacman data) and `Inferred` relationship (heuristic)

### Done when

Run twice. First run scans with full metadata and writes cache. Second run loads from cache and skips scan. `--refresh` forces re-scan. Schema mismatch triggers clean re-scan.

---

## v0.0.4 — Basic dashboard

**Goal:** make the data visible.

### Deliverables

- TUI dashboard screen with:
  - source list: each source, installed count, update count, last scan time
  - total summary row at top
  - basic keyboard navigation (arrow keys, `Tab` between sections)
  - footer bar showing available keybindings
- data sourced from cache if fresh, scan triggered if stale
- loading spinner shown during scan
- error state shown inline if a source fails — not a crash, not a blank screen

### Done when

Open the TUI, see installed and update counts per source, navigate with keyboard, see errors inline.

---

## v0.0.5 — Update dry run

**Goal:** prove the update plan is correct before any action.

### Deliverables

- update screen accessible from dashboard (`U` keybind)
- pending updates listed and grouped by source
- per-source toggle: include or exclude each source from the planned run
- update summary: "N packages will be updated across M sources"
- `paclens update --dry-run` CLI flag: prints same summary, no TUI needed
- no execution of any kind in this milestone
- error handling: if update list fetch fails for one source, show error inline, continue with others

### Done when

Open update screen, see what would update, toggle sources on/off, see summary change. Nothing executes.

---

## v0.0.6 — First action path

**Goal:** one real end-to-end action.

### Deliverables

- execute updates for Flatpak first (no sudo required, lower risk to start)
- confirmation prompt before execution: "Update N Flatpak apps? [y/N]"
- TUI suspends (LeaveAlternateScreen), command runs in raw terminal, TUI restores (EnterAlternateScreen) after completion
- success / failure status shown after TUI restores
- update log written to `~/.local/share/paclens/logs/YYYY-MM-DD.log`
- partial failure handled: show what succeeded, what failed, never hide it

### Behavior rules

- no silent execution
- no auto-retry
- failure of one source does not block others

### Done when

Confirm a Flatpak update in the TUI, see output in terminal, see pass/fail result after TUI restores, find the log on disk.

---

## v0.0.7 — `why` prototype

**Goal:** start the core differentiator.

### Deliverables

- `paclens why <package>` CLI subcommand
- dependency graph built in-memory from `pacman -Qi` output (using `Depends On` and `Required By` fields — no pactree calls)
- graph constructed with `petgraph`, node index map (`HashMap<String, NodeIndex>`) built alongside
- for pacman packages:
  - install reason: explicit or dependency
  - reverse dependency list from graph (what requires this package)
  - depth indicator: how many hops from a top-level explicit install
- removal impact text:
  - "removing this would also remove: X, Y, Z" (orphaned deps)
  - "removing this would break (but not auto-remove): A, B" (packages that still need it)
- verdict label: `likely safe` / `is a dependency` / `unclear — check manually`
- conservative by default: when data is incomplete, verdict is `unclear`

### Confidence labels introduced here

| Label | Meaning |
|---|---|
| `confirmed` | derived directly from pacman dep data |
| `inferred` | heuristic match, likely correct |
| `unknown` | tool cannot determine — user must check |

### Done when

`paclens why firefox` outputs: install reason, reverse dep list, removal impact, and a cautious verdict.

---

## v0.0.8 — Overlap prototype

**Goal:** validate the second big differentiator.

### Deliverables

- scan cross-references pacman packages and Flatpak apps to detect same-app installs
- matching strategy in priority order:
  1. known name map — hardcoded `overlap_map.toml` bundled in binary via `include_str!()` (`firefox` ↔ `org.mozilla.firefox`)
  2. reverse DNS suffix match (`org.mozilla.firefox` → `firefox`)
  3. display name match from Flatpak appstream metadata (`flatpak info --show-metadata`)
- `paclens overlaps` CLI subcommand:
  - list detected overlaps with source, version, confidence label
  - tradeoff summary per overlap
- advisory only — no suggested actions, no remove prompts
- false positive suppression: skip matches against a hardcoded blocklist of system package names that are too generic (`base`, `linux`, `files`, `core`, etc.)

### Tradeoff model shown for every overlap

| Factor | Native | Flatpak |
|---|---|---|
| sandboxing | no | yes |
| system integration | full | partial |
| update source | pacman | Flatpak remote |
| profile location | `~/.config/`, `~/.local/share/` | `~/.var/app/<id>/` |
| portal-gated access | no | yes (files, camera, etc.) |

### Done when

If `firefox` and `org.mozilla.firefox` are both installed, `paclens overlaps` detects it, labels it with confidence, and explains the tradeoff without suggesting any action.

---

## v0.0.9 — Usability pass

**Goal:** make it pleasant enough to keep iterating on.

### Deliverables

- keyboard navigation audit: every screen reachable, no dead-end states
- consistent footer bar on every screen showing available keys
- startup: TUI opens in under 200ms when cache is warm
- error messages: human-readable, inline, no raw panics, no raw stderr dumps
- color palette established and used consistently (via `src/tui/theme.rs`): background, surface, text, muted, accent, success, warning, error
- layout cleanup: consistent spacing, alignment, column widths
- `--no-color` flag for piped or non-interactive use

### Done when

The tool feels stable. Navigation is predictable. Errors are informative. Colors are consistent. Nothing looks accidental.

---

## Promotion to v0.1.0

The 0.0.x phase ends when all of the following are true:

- scanning is reliable for pacman and Flatpak
- Flatpak updates execute cleanly with terminal passthrough
- `why` exists in at least basic CLI form with confidence labels
- overlap detection works for common cases
- dep graph built from `pacman -Qi` in-memory, not per-package pactree calls
- TUI is stable and navigable
- cache works: warm start, forced refresh, version invalidation, atomic writes

At that point the engine exists. 0.1.x builds the product on top of it.

---

# Phase 0.1 — Build the product

---

## v0.1.0 — Foundation

**Goal:** promote the engine to a product shell.

### Deliverables

- pacman update execution added with privilege escalation (see spec Section 13 and dev-notes Section 2.5)
- TUI suspends for sudo prompt, restores after command completes
- unified update screen: pacman + Flatpak together, grouped by source
- dashboard wired to full scan: installed counts, update counts, last scan time
- CLI and TUI fully consistent: same data, same logic, different presentation
- `anyhow` error handling hardened throughout — no `unwrap()` in any production path
- log rotation: keep last N logs (configurable), delete older on startup

### Done when

Open the TUI, see the full dashboard, navigate to update screen, run both sources, see output in terminal passthrough, exit cleanly.

---

## v0.1.1 — Update view

**Goal:** make updating the core interaction.

### Deliverables

- per-source toggle persisted for session
- update summary preview: "N packages from M sources — proceed?"
- update execution isolated per source: failure in one does not abort others
- post-update summary: per-source pass/fail and packages-updated count
- update log linked from TUI: `L` keybind opens log path in $PAGER or inline viewer
- dep graph and cache invalidated after successful update (data has changed)

### Done when

One screen to update everything. Clear before and after. Failure is visible and isolated. Cache refreshes post-update.

---

## v0.1.2 — `why` in TUI

**Goal:** bring the core differentiator into the product.

### Deliverables

- `W` keybind on any package in any list opens `why` panel (right pane or modal)
- panel shows: install reason, reverse dep chain (as indented text tree), removal impact, verdict
- confidence label shown on every dep edge
- verdict colors: green for `likely safe`, yellow for `is a dependency`, red for `unclear`
- panel navigable with arrow keys, closeable with `Esc`
- `paclens why <pkg>` CLI output matches panel content exactly

### Done when

Press `W` on any package, get full explanation inline. No context switching to a terminal.

---

## v0.1.3 — Relationship model

**Goal:** make the data smarter without overpromising.

### Deliverables

- dep graph cached as part of `ScanResult` (serialized `petgraph` via serde)
- graph queries: forward deps, reverse deps, transitive depth lookup — all in-memory from cache
- Flatpak app grouping by app ID prefix — heuristic only, clearly labeled as such
- UI shows explicit distinction between `real dependency` (pacman data) and `inferred relationship` (heuristic) on every edge
- dep graph rendered in `why` panel as indented text tree with edge labels

### What this is not

- no cross-ecosystem dep graph — pacman and Flatpak dependency trees are separate and cannot be merged
- no claim that Flatpak runtime versions relate to pacman package versions

### Done when

Select any package, open `why`, see full dep chain with confirmed/inferred labels on every edge.

---

## v0.1.4 — Overlap detector in TUI

**Goal:** surface Flatpak/native overlap as a first-class screen.

### Deliverables

- overlap screen accessible from dashboard (`O` keybind)
- lists all detected overlaps: display name, native version, Flatpak version, confidence label
- primary install heuristic displayed: which install is likely the one in active use
- tradeoff table shown in detail pane for selected overlap
- Flatpak profile path and size shown when `~/.var/app/<id>/` exists
- confidence label on every match
- advisory only — no action buttons in this milestone

### Done when

Navigate to overlap screen, see all detected Flatpak/native duplicates, read the tradeoff for each, make your own decision.

---

## v0.1.5 — Cleanup summary

**Goal:** add low-risk maintenance value.

### Deliverables

- cleanup screen with two sections: orphans and cache
- orphan list: pacman packages with no reverse deps and install reason = dependency (derived from dep graph, not a separate `pacman -Qtd` call)
- cache summary: pacman cache size at `/var/cache/pacman/pkg/`, Flatpak unused runtime count and size
- suggestions shown as advisory text only — no action buttons
- pressing `Enter` on any orphan opens its `why` panel before surfacing any suggestion
- commands shown explicitly: "to clean orphans: `pacman -Rns $(pacman -Qtdq)`"

### What this is not

- not an automatic cleaner
- not a replacement for `paccache`
- never one-click delete

### Done when

Open cleanup screen, see orphan count and cache sizes, see what to run manually, feel informed rather than railroaded.

---

## v0.1.6 — Polish pass

**Goal:** make it feel like a real TUI tool.

### Deliverables

- search / filter in package list (`/` to activate, `Esc` to clear)
- detail pane toggle: `D` shows/hides right-side detail for selected item
- color theme: readable in dark terminals, tested in light terminals, respects `--no-color`
- startup: TUI visible in under 200ms on warm cache; background refresh indicator if stale
- progress: spinner on scan, per-source status line on update
- screen transitions: no flicker, no blank frames between pane swaps
- help screen: `?` opens full keybinding reference overlay
- footer bar consistent and complete on every screen

### Done when

The tool is polished enough to recommend. Navigation is smooth. Information density is high without feeling cluttered. No rough edges.

---

# Phase 0.2 — Stabilize

---

## v0.2.0 — Stable core

**Goal:** lock the first shippable version.

### Deliverables

- all 0.1.x parsers reviewed and hardened against unusual real-world output
- cache invalidation tightened: detect pacman db changes (`/var/lib/pacman/local/` mtime), not just age
- output format normalized: same data shape regardless of provider, no source-specific leakage into UI
- `why` edge cases handled: virtual packages (Provides field), package groups, split packages
- overlap detector false positive reduction: confidence scoring reviewed against real installs
- configuration file at `~/.config/paclens/config.toml` (full schema in config.default.toml)
- test coverage on:
  - pacman `-Qi` parser (including multiline descriptions, optional deps, virtual packages)
  - Flatpak `list` parser (app and runtime)
  - dep graph construction and traversal
  - overlap matching (all three strategies + false positive rejection)
  - scan cache read/write/invalidation/schema mismatch
- README with install instructions and usage docs
- AUR PKGBUILD for `paru -S paclens`

### Promotion criteria

v0.2.0 ships when:

- no known parser crashes on real-world output
- all unit tests pass in CI
- the tool has been used on the developer's daily driver for at least 2 weeks without data loss or corruption
- config file works end-to-end
- `--help` output is complete and accurate for all subcommands

### Done when

You would trust this tool on your daily driver. Config works. Edge cases do not crash. Tests catch regressions.

---

## What stays out until after v0.2.0

Explicitly deferred. Not forgotten — just not yet.

| Feature | Reason deferred |
|---|---|
| paru / AUR integration | adds parser complexity before core is proven |
| cargo / npm / brew / pipx support | maintenance cost before value is confirmed |
| migration engine | needs profile mapping DB that does not exist yet |
| automatic config/data copying | too risky without real-world edge case data |
| app-level grouping database | requires community contribution or external source |
| destructive cleanup automation | only after advisory layer is trusted by real users |
| system doctor (lock issues, broken deps) | useful but scope-creep risk before core is stable |
| daemon mode | not needed until background sync is a confirmed need |
| plugin / provider system | premature abstraction |
| distro support beyond Arch family | out of scope entirely |

---

# Phase 0.3+ — Extend

These become real only after v0.2 ships and user feedback confirms direction. Not committed. Listed so the shape is visible.

---

**v0.3 — paru + AUR**
- AUR package scanning via paru
- AUR update detection
- paru as optional provider, gracefully absent if not installed
- AUR-specific caveats surfaced in `why` output (VCS packages, manual PKGBUILD review needed)
- `-git` package update detection (compare upstream HEAD, not version string)

**v0.4 — Migration advisory**
- structured migration report per detected overlap
- profile location mapping: show where each source stores data, read-only
- config/data/cache split shown per app when paths are known
- "here is what you would need to do manually" — no file ops yet

**v0.5 — Migration execution**
- controlled profile copy with automatic backup before touching anything
- install target first, verify it launches, then offer to remove source
- full audit log of every file operation
- rollback instructions shown if something goes wrong
- never delete source data automatically — user confirms after verifying target works

**v0.6 — Broader sources**
- cargo installs (`~/.cargo/bin`)
- npm global packages (`npm list -g`)
- pipx apps
- optional: brew/linuxbrew
- each added only after its parser is solid and tested, not as a batch

**v0.7+ — Advanced features (not designed yet)**
- system doctor (broken deps, lock issues, stale config paths)
- interactive cleanup with per-item confirmation
- app-level grouping database (community-maintained or crowdsourced)
- optional plugin/provider system for user-contributed sources

---

## Technical stack

| Concern | Crate |
|---|---|
| TUI | `ratatui` + `crossterm` |
| async runtime | `tokio` |
| CLI parsing | `clap` (derive mode) |
| dep graph | `petgraph` |
| config + cache | `serde` + `toml` |
| error handling | `anyhow` + `thiserror` |
| logging | `tracing` + `tracing-subscriber` |
| config/data paths | `directories` |

Single binary. No daemon. No root service. Sudo only when executing pacman updates.

---

## Timeline

| Milestone | Focus | Estimate |
|---|---|---|
| v0.0.1 | skeleton | 3–5 days |
| v0.0.2 | source probing | 4–6 days |
| v0.0.3 | cache + model | 4–6 days |
| v0.0.4 | basic dashboard | 3–5 days |
| v0.0.5 | update dry run | 2–4 days |
| v0.0.6 | first action | 4–6 days |
| v0.0.7 | `why` prototype | 5–7 days |
| v0.0.8 | overlap prototype | 5–7 days |
| v0.0.9 | usability pass | 3–5 days |
| **0.0.x total** | **engine** | **~5–7 weeks** |
| v0.1.0 | foundation | 1–2 weeks |
| v0.1.1 | update view | 1–2 weeks |
| v0.1.2 | `why` in TUI | 1–2 weeks |
| v0.1.3 | relationship model | 1–2 weeks |
| v0.1.4 | overlap in TUI | 1–2 weeks |
| v0.1.5 | cleanup summary | 1 week |
| v0.1.6 | polish pass | 1–2 weeks |
| **0.1.x total** | **product** | **~7–10 weeks** |
| v0.2.0 | stable core | 2–3 weeks |
| **Grand total** | | **~4–5 months solo** |

Estimates assume part-time work alongside coursework. Each milestone produces something independently useful — the tool gains real value at every step, not just at the end.

---

## Summary

```
0.0.x  engine exists
0.1.x  product exists
0.2.0  shippable product
```

At v0.2.0:

> a beautiful, Arch-first TUI that scans pacman and Flatpak, runs updates from one place,
> explains dependency impact with honest confidence labels, and flags Flatpak/native overlap
> with clear tradeoff analysis — without touching anything it cannot justify.

That is a complete, honest, shippable product.
