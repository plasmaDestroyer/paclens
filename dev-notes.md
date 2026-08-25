# paclens — Developer Notes

> Implementation guidance, known hard problems, and build-order advice.
> Read this before writing any non-trivial code.

---

## 1. Build order

This matches the roadmap milestones. Do not reorder.

```
v0.0.1  skeleton        CLI + empty TUI + config + logging
v0.0.2  providers       pacman -Q, flatpak list → parse into structs
v0.0.3  cache + model   pacman -Qi (full metadata), ScanResult, write/read cache
v0.0.4  dashboard       wire cached data into TUI screens
v0.0.5  update dry run  show what would update, no execution
v0.0.6  first action    execute Flatpak update, TUI suspend/restore
v0.0.7  dep graph + why build graph from pacman -Qi, reverse dep lookup, verdicts
v0.0.8  overlap detect  cross-reference pacman + Flatpak, matching pipeline
v0.0.9  usability       keyboard, colors, speed, error messages
```

The temptation will be to build the TUI first because it is visible and satisfying. Resist this until v0.0.4. You need data flowing through the model before the TUI has anything to show. An empty TUI frame at v0.0.1 is fine — it proves ratatui works. But do not invest in layout, theming, or widgets until v0.0.4+.

The build order front-loads the hard parts (parsing, caching, graph construction) so that the later milestones (TUI polish, overlap detection) have a solid foundation to build on.

---

## 2. Hard problems

### 2.1 Parsing `pacman -Qi` output

This is the single most important parser. Get it right early.

The format is multi-record, one record per package, separated by blank lines. Each line is `Key  : Value` (two-space-colon pattern). Edge cases:

- **Multiline values:** `Description`, `Optional Deps`, and sometimes `Licenses` can span multiple lines. Continuation lines start with whitespace. Your parser must handle this: if a line starts with whitespace and the previous line had a key, append it to the previous value.

- **"None" sentinel:** `Depends On : None`, `Required By : None`, `Optional Deps : None` — these mean "empty," not a package named "None". Check for this explicitly.

- **Version constraints:** `Depends On` and `Provides` may include version constraints like `glibc>=2.38` or `sh=5.2`. Strip the operator and version, keep only the package name.

- **Optional deps format:** each optional dep is on its own line, indented, format: `package-name: description [installed]`. Parse the package name only. The `[installed]` suffix is informational.

- **Virtual packages:** a package's `Provides` field lists virtual names it satisfies. When building the dep graph, if package A depends on virtual name V, and package B provides V, the edge should be A → B. Build an alias map: `HashMap<String, String>` mapping virtual names to real package names.

- **Package groups:** the `Groups` field lists groups the package belongs to (e.g., `base-devel`). Informational only in v0.x — do not confuse with dependencies.

- **Installed Size parsing:** format is human-readable: "12.34 MiB", "956.00 KiB", "1.23 GiB". Parse into bytes. Handle all three units.

Write a fixture file for each edge case and test the parser against it.

### 2.2 Flatpak column parsing

Always request explicit columns with `--columns=`. Never parse positional output — Flatpak's default column order changes between versions.

The output is tab-separated when `--columns` is used. Handle:
- missing fields (tab-tab with nothing between)
- apps with no version set (version field is empty)
- apps installed in both user and system scope (appear as separate rows)

`flatpak info --show-metadata <app-id>` returns GLib keyfile format (INI-like). It is NOT TOML or JSON. The `[Application]` section has `name=` for the display name. Parse with a simple line-by-line approach: find `[Application]` header, then scan for `name=` key. Do not use a TOML parser on this — it will fail.

`flatpak remote-ls --updates` can be slow if remotes are unreachable. Always time it out, log a warning, and fall back to the cached update list. Never block the UI waiting for a remote.

> **Superseded (2026-07-12).** The timeout is not `tokio::time::timeout` — `SystemCommandRunner` is sync. It spawns with piped stdio, drains on threads, and polls `try_wait`, killing past `scan.provider_timeout_secs`. It applies to *every* provider command, not just `remote-ls`. See §7.

### 2.3 Dep graph from `pacman -Qi` (not pactree)

Do NOT call `pactree -r <pkg>` for every installed package. On a typical Arch system (~1500-2500 packages), that would mean ~2000 subprocess calls — unusable.

Instead, build the entire graph from one `pacman -Qi` call:

```
pacman -Qi    →    parse all packages
                   for each package:
                     Depends On  →  forward edges (this requires that)
                     Required By →  reverse edges (that requires this)
```

This gives you the complete dep graph from a single command. All graph queries (forward, reverse, transitive) then run in-memory on the `petgraph` structure. No further subprocess calls needed.

`pactree` is not a dependency of paclens.

### 2.4 petgraph practical notes

petgraph uses `NodeIndex` (integer) internally. You need a lookup map:

```rust
struct DepGraph {
    graph: DiGraph<String, DependencyEdge>,
    index: HashMap<String, NodeIndex>,
}

impl DepGraph {
    fn get_or_insert(&mut self, name: &str) -> NodeIndex {
        if let Some(&idx) = self.index.get(name) {
            idx
        } else {
            let idx = self.graph.add_node(name.to_string());
            self.index.insert(name.to_string(), idx);
            idx
        }
    }
}
```

The graph is not serialized to cache. It is rebuilt from `Package.depends_on` / `required_by` data on every load. This is fast (<100ms for 2000+ packages) and avoids versioning headaches with petgraph's internal representation.

### 2.5 Sudo in TUI

`sudo` writes its password prompt directly to `/dev/tty`, bypassing stdout/stderr. You cannot capture or redirect it. There are three approaches:

**Option A (v0.0.6, recommended):** Suspend the TUI, run the command in the raw terminal, restore the TUI.

```rust
// Before execution:
crossterm::execute!(stdout, LeaveAlternateScreen)?;
crossterm::terminal::disable_raw_mode()?;

// Run the command (user sees sudo prompt, pacman output, etc.):
let status = Command::new("sudo")
    .args(["pacman", "-Syu"])
    .status()?;

// After:
crossterm::terminal::enable_raw_mode()?;
crossterm::execute!(stdout, EnterAlternateScreen)?;
// Re-render TUI with result
```

Slightly jarring (screen switches) but completely reliable.

**Option B (v0.1+, smoother):** Warm the sudo credential cache before running:

```rust
// Before entering the update flow:
Command::new("sudo").arg("-v").status()?;
// Now sudo won't prompt again for the configured timeout
// Run pacman with sudo, piping output to TUI
```

This only works if `sudo` is configured with a credential timeout (default: 15 minutes). If `timestamp_timeout = 0`, it always prompts.

**Option C (advanced):** Check for `SUDO_ASKPASS` environment variable or a graphical askpass agent. If available, use it. Otherwise fall back to Option A.

Recommendation: start with Option A. It is simple and always works. Move to B or C only if users report the screen-switching as a pain point.

### 2.6 Streaming command output

> **Superseded (2026-07-08).** Execution runs on a real pty (`portable-pty` + `vt100`), not piped stdio, and there is no async runtime. Both the code sketch below and the `tokio::select!` loop in §5 are spec-era design, kept for context only. See §7.

When NOT using the TUI-suspend approach (e.g., for Flatpak which doesn't need sudo), you can stream output into the TUI:

```rust
let mut child = tokio::process::Command::new("flatpak")
    .args(["update", "--noninteractive"])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()?;

let stdout = child.stdout.take().unwrap();
let reader = tokio::io::BufReader::new(stdout);
let mut lines = reader.lines();

while let Some(line) = lines.next_line().await? {
    tx.send(AppEvent::OutputLine(line)).await?;
}
```

The TUI event loop handles two event types: crossterm terminal events and internal `AppEvent` messages. Use `tokio::select!` to poll both:

```rust
loop {
    tokio::select! {
        Some(event) = rx.recv() => handle_app_event(event, &mut app),
        Ok(true) = crossterm_event_available() => handle_input(&mut app),
    }
    terminal.draw(|f| render(&app, f))?;
}
```

Keep the output buffer bounded: store only the last N lines (e.g., 500). pacman full-upgrade output can be thousands of lines.

### 2.7 Atomic cache writes

```rust
let tmp = cache_path.with_extension("toml.tmp");
std::fs::write(&tmp, &serialized)?;
std::fs::rename(&tmp, &cache_path)?;
```

`rename` is atomic on Linux when source and target are on the same filesystem (guaranteed since both are in `~/.cache/paclens/`). If the process dies during `write`, only the `.tmp` file is corrupted — the real cache is untouched.

At startup, clean up any stale `.tmp` files:
```rust
if tmp.exists() {
    let _ = std::fs::remove_file(&tmp);
}
```

### 2.8 Overlap false positives

The reverse DNS heuristic will produce false positives. Known patterns:

**Generic names that match unrelated pacman packages:**
- `io.elementary.files` → `files` — could match anything named `files`
- `org.gnome.Shell.Extensions.GSConnect` → `gsconnect` — only if a pacman package with that exact name exists

**Mitigation:** maintain a blocklist of generic pacman names that should never appear as overlap targets: `base`, `linux`, `files`, `core`, `extra`, `lib`, `utils`, `man`, `docs`. Ship this in the binary, not in config.

**General rule:** a missed overlap (false negative) is better than a wrong match (false positive). Start conservative. Expand the known map over time based on user reports.

### 2.9 Flatpak profile paths

Flatpak apps use `~/.var/app/<application-id>/` but the internal structure varies:

```
~/.var/app/org.mozilla.firefox/
├── .mozilla/        # Firefox profile
├── cache/           # XDG cache
├── config/          # XDG config
└── data/            # XDG data
```

Some apps put everything in `data/`, some split across all three, some use custom paths inside the app dir.

For the overlap report, just compute total size:
```
du -sb ~/.var/app/<id>/
```

Do not try to parse the internal layout in v0.x. That complexity belongs in the migration engine (v0.5+).

### 2.10 pacman db lock detection

Before any pacman operation, check for `/var/lib/pacman/db.lck`:

```rust
if Path::new("/var/lib/pacman/db.lck").exists() {
    return Err(anyhow!("pacman database is locked. Another instance may be running. \
        If no other pacman process is active, remove /var/lib/pacman/db.lck"));
}
```

Show this as a specific, actionable error — not a generic "pacman failed."

---

## 3. Module contracts

### Provider

Every provider must:
- accept a `CommandRunner` for dependency injection (testing seam)
- return `Ok(vec![])` if nothing is installed (not an error)
- return `Err` only if the source binary exists but the command failed
- respect the configured timeout (`config.scan.provider_timeout_secs`)
- never call anything that requires sudo (scanning is always unprivileged)
- never know about other providers (the scanner orchestrates them)

### Scanner

The scanner:
- detects available providers (checks PATH)
- runs providers concurrently on scoped threads (`std::thread::scope`)
- assembles the combined `ScanResult`
- writes the result to cache
- never analyzes data — that is the analyzer's job

### Analyzer

The analyzer:
- is pure: given the same `ScanResult`, always produces the same output
- never calls providers or subprocess commands
- never writes to disk
- constructs the dep graph, overlap candidates, and orphan list from `ScanResult`

### Executor

The executor:
- only executes pre-built `ActionPlan` values
- never decides what to do — all decisions come from the user via TUI
- logs every command before and after execution
- reports exit codes without interpretation (the TUI interprets)

---

## 4. Initialization sequence

```
1. parse CLI args (clap)
2. load config (create default if absent)
3. init tracing (file + optional stderr)
4. detect available providers (check PATH for pacman, flatpak)
5. load scan cache
   5a. cache valid → proceed with cached ScanResult
   5b. cache invalid/absent/stale → spawn scan (async)
6. build dep graph from ScanResult
7. detect overlaps from ScanResult
8. compute orphan list from dep graph
9. dispatch:
   - no subcommand or `ui` → open TUI with data
   - `status` / `why` / `overlaps` / `cleanup` → print and exit
   - `update` → either print dry-run or execute
```

If dispatching to TUI: open the frame immediately (step 9). Show a loading state. Steps 5-8 can run async, with results pushed to the TUI via channel. The user sees the dashboard populate as data arrives.

---

## 5. TUI state management

Single `App` struct owns all mutable state:

```rust
pub struct App {
    pub screen: Screen,
    pub scan_result: Option<ScanResult>,
    pub dep_graph: Option<DepGraph>,
    pub overlaps: Vec<OverlapCandidate>,
    pub orphans: Vec<String>,
    pub scan_state: ScanState,       // Idle | Scanning | Done | Error(String)
    pub update_state: UpdateState,   // Idle | Confirming | Running | Done(Result)
    pub cursors: HashMap<Screen, usize>,
    pub detail_pane_open: bool,
    pub search: Option<SearchState>,
    pub flash_message: Option<(String, Instant)>,  // temporary status message
}

pub enum Screen {
    Dashboard,
    Updates,
    Packages,
    Overlaps,
    Cleanup,
    Help,
}
```

Rules:
- rendering functions take `&App` (immutable) — they never mutate state
- event handlers take `&mut App` — they are the only thing that mutates state
- no global mutable state anywhere
- no interior mutability unless strictly needed for async channels

---

## 6. Known fragile points

**pacman -Qi format.** Stable for years but no API contract. Test against fixtures. Run CI. When pacman updates, re-capture fixtures from a real system and verify.

**flatpak --columns output.** Supported but column names can change. Pin to the exact column names used in the spec. If Flatpak adds/removes a column name, the parser must handle it gracefully (skip unknown columns, warn on missing expected columns).

**sudo credential timeout.** Varies by system. `timestamp_timeout = 0` means every sudo call prompts. `NOPASSWD` means no prompt at all. Document both in README.

**TOML cache size.** For a system with ~2000 packages, the TOML cache will be 2-5MB. This is fine for now. If it becomes a bottleneck (measure first), switch to MessagePack.

**Flatpak remote availability.** Campus networks, VPNs, and firewalls can block Flatpak remotes. Always timeout remote calls. Always fall back to cached data. Never block the UI on network.

---

## 7. Decisions log

Fill this in as decisions are made. Format: date, decision, reasoning.

```
YYYY-MM-DD | decision
           | reasoning

YYYY-MM-DD | dep graph built from pacman -Qi, not pactree
           | pactree per-package is O(n) subprocess calls, unusable on large systems.
           | pacman -Qi gives Depends On and Required By in one call.

YYYY-MM-DD | TOML for cache format
           | human-readable, debuggable, good enough performance for <5MB.
           | bincode/msgpack if profiling shows >500ms read/write.

YYYY-MM-DD | overlap_map.toml bundled, not fetched remotely
           | no network dependency at startup, simpler, user-extensible via config.

YYYY-MM-DD | TUI suspend for sudo, not output piping
           | sudo writes to /dev/tty directly, cannot be captured.
           | LeaveAlternateScreen/EnterAlternateScreen is reliable.

YYYY-MM-DD | no --noconfirm for pacman
           | suppresses conflict resolution prompts, too dangerous.
           | let user interact with pacman directly in raw terminal.

2026-06-13 | why verdict: zero reverse deps ⇒ likely safe, any install reason
           | spec §7.3 (dependency-only) conflicts with §11.4's canonical
           | example (explicit leaf = likely safe). §11.4 wins; unknown
           | install reason stays `unclear [unknown]`, never guessed.

2026-06-13 | dashboard enrichment deferred (user request 2026-06-12)
           | homescreen should eventually carry much more: storage breakdown,
           | orphan/overlap counts (spec §10.3). Design pass before v0.1.0.
           | → delivered 2026-07-05 as the v0.1.1 quadrant dashboard.

2026-07-05 | pacman updates via checkupdates, -Qu only as fallback
           | pacman -Qu compares against the local sync DB, which is stale
           | unless the user recently ran pacman -Sy — it silently reports
           | zero pending updates. checkupdates (pacman-contrib) syncs a
           | temp DB copy and is accurate. Source.accurate_updates records
           | which path ran (spec §4.2 deviation); the UI hints when stale.

2026-07-05 | flatpak runtimes are first-class packages
           | flatpak update updates runtimes (platforms, GL drivers, themes),
           | so they count as installed and as update targets. Package.runtime
           | flags them (spec §4.3 deviation); overlap matching excludes them
           | (spec §9.3). SCHEMA_VERSION bumped to 2 for both new fields.

2026-07-05 | scan lanes on scoped threads (spec Q5 resolved)
           | pacman, flatpak, and du run concurrently; wall time is the
           | slowest lane. std::thread::scope, no tokio needed in the scanner.

2026-07-06 | inline execution console replaces TUI suspend (user request)
           | update output streams into the update window via piped stdio;
           | typed keys forward to the child's stdin (sudo -S password,
           | pacman prompts). Deviates from spec §13.2 suspend flow for the
           | sudo/no-tool cases; doas/pkexec keep the suspend path (they
           | cannot read a piped stdin). Password is forwarded, never echoed,
           | never stored (§13.5 holds). L opens the update log inline too.
           | Ceiling: no cancel mid-run, no backspace in blind input; a pty
           | (portable-pty) is the upgrade path if prompts misbehave.

2026-07-06 | why pane follows the cursor instead of arrow-scrolling
           | roadmap v0.1.2 says "panel navigable with arrow keys"; we keep
           | arrows on the package list (the pane live-follows the cursor)
           | and cap the chain tree to fit with "… n more" — better UX than
           | a focus switch. W stays on the package list; the update screen
           | has no per-package cursor to hang it on.

2026-07-07 | dep graph stays rebuilt-on-load, NOT serialized (v0.1.3)
           | roadmap v0.1.3 says "graph cache (serialized petgraph)"; the
           | standing v0.0.3 decision wins: serializing petgraph couples the
           | cache to petgraph's internals, and a rebuild from the cached
           | ScanResult is <100ms on a full system. Deviation, not drift.

2026-07-07 | flatpak enters the graph as app → runtime Inferred edges
           | flatpak list's runtime column is authoritative for "app uses
           | runtime", but the grouping relation (spec §7.4) is our
           | derivation — every such edge is EdgeKind::Inferred with
           | Confidence::Inferred, and the why verdict wears the worst edge
           | label. Runtimes stay out of orphans() (pacman-only concept).

2026-07-07 | WhyReport unified across sources (v0.1.3)
           | the Flatpak variant died; every found package gets the full
           | WhyDetail with source_id + runtime flag. Verdicts: flatpak app
           | leaf = likely safe [confirmed] (self-contained), used runtime =
           | is a dependency [inferred], unused runtime = likely safe
           | [inferred] (absence of inferred edges is itself inferred). The
           | Unknown-install-reason ⇒ Unclear rule is pacman-only — flatpaks
           | always scan with an Unknown reason and the graph decides.

2026-07-08 | exec console is a real pty (exact passthrough; user request)
           | portable-pty + vt100: the child sees a genuine terminal, so
           | sudo/doas/pkexec/pacman prompt, color and redraw natively; every
           | key (Ctrl-C included) passes through to the child. Replaces the
           | piped-stdio console AND the doas/pkexec suspend path. Logs keep
           | commands + exit codes only — the byte stream is terminal noise.
           | Ceiling: pty size fixed at spawn (no mid-run resize).

2026-07-08 | in-TUI confirm modal removed (user decision)
           | the update screen already shows the plan and exact commands, and
           | pacman/sudo ask their own questions in the pty — a second modal
           | was double-confirmation. Enter on the plan view runs. P1/P4
           | still hold: explain-then-confirm happens on the plan screen plus
           | the tool's own prompt. The CLI keeps its y/N prompt.

2026-07-08 | console dismissal returns to the dashboard (user decision)
           | the result view died with the modal: any key after "done" lands
           | on a refreshing dashboard with a one-line summary flash riding
           | the bottom border (L there opens the full log inline).

2026-07-08 | update screen lists only sources that have updates
           | a clean source has nothing to toggle or run — showing it was
           | noise (user decision; dashboard sources pane still shows all).

2026-07-11 | update screen deleted — the dashboard IS the plan view
           | after the pty console the screen only owned toggles + run, and
           | the dashboard already showed everything else. Space toggles the
           | selected source ([✓]/[ ]/dim dash column), u runs the plan in
           | the console, the system pane shows "plan · N packages / M
           | sources" (P1 stays visible), and the console/log viewer became
           | screen-independent overlays. Enter still opens the package
           | list. Spec §10.2's separate update screen is superseded.

2026-07-11 | package-list sort modes ride a header-row model
           | s cycles updates-first → reason → name → size. pkg_rows()
           | interleaves dim non-selectable group headers; the cursor stays
           | in package space and pkg_row_index() maps it to table rows, so
           | the scrolloff margin math runs in row space. Updates-first
           | drops its headers when nothing is pending; flatpak
           | Unknown-reason packages group as "apps"; fuzzy filter overrides
           | any sort, headerless. Sort persists for the session.

2026-07-11 | flatpak profile sizes measured by the scanner (v0.1.4)
           | ScanResult.flatpak_profile_sizes (SCHEMA_VERSION 3): the
           | flatpak lane runs du -sb on ~/.var/app/<id> per app; missing
           | dirs are absent. The analyzer stays pure — it only reads the
           | map and string-builds the display path. Spec §9.4 heuristic 2
           | (>10 MiB profile ⇒ flatpak likely primary) went live with it;
           | rule 1 (explicit native) still outranks it per spec order.

2026-07-11 | overlap screen layout: list on top, tradeoff pane below
           | the §9.5 table is wide (factor/native/flatpak columns), so a
           | bottom pane fits it better than a why-style side pane. o (not
           | just spec's O) opens it from the dashboard. Advisory only —
           | no action keys in v0.1.4 by design.

2026-07-11 | cleanup data comes from the graph, not new subprocess calls
           | (v0.1.5) orphans = DepGraph::orphans (no pacman -Qtd); unused
           | runtimes = runtime packages with no reverse deps over the
           | v0.1.3 app→runtime edges — name-level, conservative. Flatpak
           | sizes ride the list's size column (decimal g_format_size
           | units; SCHEMA_VERSION 4). CacheSizes.flatpak_unused_* stays
           | None — the analyzer/App derives those live instead of the
           | scanner caching a graph-derived number (one source of truth).

2026-07-11 | cleanup screen: why-before-suggestion, commands as text
           | Enter swaps the cache pane for the selected orphan's why
           | report (roadmap rule). Suggestions are copiable text
           | (paccache -rk2, flatpak uninstall --unused, pacman -Rns with
           | real names) — no action keys until the v0.5 trust ladder.

2026-07-12 | provider timeout implemented in SystemCommandRunner (v0.2.0)
           | spec §2.2 wanted a tokio timeout on remote-ls; the runner is
           | sync, so it spawns with piped stdio, drains on threads, polls
           | try_wait and kills past config scan.provider_timeout_secs
           | (0 = off). Applies to every provider command, not just
           | remote-ls — any hung binary dies, not just flatpak's.

2026-07-12 | v0.2.0 audit outcomes
           | cache invalidation already watched /var/lib/pacman/local mtime
           | (shipped v0.0.3, roadmap item was pre-satisfied). Config knobs
           | min_confidence (TUI), orphan_ignore and provider_timeout_secs
           | were schema-only — now consumed. Cargo version finally bumped
           | 0.0.1 → 0.2.0. PKGBUILD lives in packaging/ — AUR publish and
           | crates.io name registration are human steps. The two-week
           | daily-driver promotion criterion is the user's clock, not ours.

2026-07-12 | AUR = the libalpm split, not a second pacman scan (v0.3)
           | foreign packages already carry full pacman -Qi metadata; the
           | scanner just relabels them (pacman -Qm names → source_id aur,
           | works without paru; SCHEMA_VERSION 5). paru only does what
           | pacman can't: update detection (-Qua, exit 1 = none, same line
           | format as -Qu → shared parse_updates_as) and the update step
           | (paru -Sua). The graph/why/overlap treat aur as alpm (is_alpm):
           | Real/Confirmed edges, real install reasons, native side of
           | overlaps, orphan candidates.

2026-07-12 | paru is never run under sudo
           | it builds as the user and self-elevates for the install step —
           | needs_privilege() excludes aur; the pty console handles paru's
           | own sudo prompt. The aur source shows "not found" without paru
           | even though its packages still list (update path unavailable).

2026-07-12 | -git detection = paru --devel, not our own ls-remote
           | roadmap wanted upstream-HEAD comparison for VCS packages; paru
           | already implements it. config scan.aur_devel (default false —
           | slow) forwards the flag; the why caveat points at the knob.

2026-07-14 | cleanup screen must show reclaimable, not just total (v0.5 req)
           | field finding: 11 GB pacman cache, but paccache -dk2 reclaims
           | 0 bytes — the bulk is the *current* version of every installed
           | package (the downgrade safety net), which paccache never touches.
           | Showing "11 GB · run paccache -rk2" implies the total is
           | cleanable. Fix when cleanup grows: parse paccache -d dry-run
           | output for the honest reclaimable number, show both.
           | → delivered 2026-07-17.

2026-07-14 | AUR build cache belongs on the cleanup screen (v0.5 req)
           | ~/.cache/paru was 9 GB on the reference system and paclens
           | doesn't surface it. Suggest `paru -Sc --aur` (clone dir only).
           | → delivered 2026-07-17.

2026-07-14 | cleanup execution: paccache + paru -Sc --aur, never pacman -Sc
           | pacman >=7's sandboxed downloader (DownloadUser=alpm) leaves
           | download-* partials owned by the alpm user; pacman -Sc (and so
           | paru -Sc) errors trying to remove them. paccache only globs
           | *.pkg.tar* — immune. paru -Sc --aur skips the pacman cache
           | entirely. Leftover download-* partials: detect and report with
           | a suggested sudo rm, never auto-remove.

2026-07-14 | migration probe: scanner asks the pure analyzer what to measure
           | (v0.4) the profile-dir list depends on overlap matching, so the
           | scanner runs detect_overlaps + migrate::probe_paths on the
           | just-assembled scan and du -sb's the results into
           | ScanResult.profile_dir_sizes (schema v6). The purity contract
           | holds: the analyzer never touches disk; the scanner never
           | decides which paths matter. Overlaps still recompute on load.

2026-07-14 | flatpak counterpart paths derive from the sandbox HOME convention
           | (v0.4) Flatpak sets the XDG vars inside ~/.var/app/<id>, so
           | ~/.config/X ↔ <sandbox>/config/X, ~/.local/share/X ↔ data/X,
           | ~/.cache/X ↔ cache/X, and any other dotdir (~/.mozilla) keeps
           | its name under the sandbox HOME. Apps that ignore XDG inside
           | the sandbox exist; the probe's existence data plus the
           | "advisory — verify" warning carry that risk honestly.

2026-07-14 | migration report defaults toward the likely-primary side
           | (v0.4, user choice) consolidate into where the data already
           | is; d/--to flips. Unknown primary defaults to native → flatpak
           | with an explicit "primary side unclear" warning — never a
           | silent guess. Rows appear only when at least one side exists;
           | cache rows always advise "skip — regenerates".

2026-07-17 | the migration copy plan never contains an rm (v0.5)
           | plan_migration only ever mkdirs and cp -aT: existing targets
           | are staged into a timestamped backup dir first, then each
           | actionable pair is copied. Source data is never deleted, so
           | rollback stays possible — rollback_lines renders the restore
           | commands from the same pair indices. Roadmap v0.5's "never
           | delete source data automatically" is therefore structural, not
           | a rule the executor has to remember. Cache pairs and empty
           | sources are skipped outright.

2026-07-17 | ActionKind::Migrate is never privileged; Remove follows the source
           | profile data under ~ is user-owned even when the app is a
           | system-scope flatpak, so the copy always runs unprivileged.
           | Only the removal plan escalates: sudo pacman -Rns for the
           | native side (pacman removes AUR packages too), flatpak
           | uninstall --user/--system for the flatpak side. cp -aT, not
           | cp -a — plain cp -a nests into an existing target directory.

2026-07-17 | removal is armed by a clean copy, never offered alongside it
           | (v0.5, user decision) x runs the copy plan in the console; only
           | a zero-exit run arms R, and the pane then reads "launch <app>
           | and verify, then R removes the source". esc or moving the
           | cursor disarms and names the backup dir. A failed copy never
           | arms removal. The CLI has the same shape: --run prints the
           | exact commands, asks y/N, copies, prints the rollback block,
           | then asks a second y/N for the removal after telling the user
           | to verify. This is roadmap v0.5's "install target first, verify
           | it launches, then offer to remove source" — the install step is
           | moot because an overlap means both sides are already installed.

2026-07-17 | cleanup shows reclaimable next to the total (v0.5; req 2026-07-14)
           | the scanner runs the matching paccache -dk2 dry run and parses
           | its figure into CacheSizes.pacman_cache_reclaimable_bytes
           | (SCHEMA_VERSION 7), so the pane reads "11 GiB (0 B
           | reclaimable)" instead of implying the total is cleanable. The
           | paccache suggestion hides itself when it would free nothing.
           | ~/.cache/paru gets its own row with paru -Sc --aur (clone dir
           | only); pacman -Sc is still never suggested (2026-07-14
           | sandboxed-download partials decision).

2026-08-23 | tokio dropped — it was never used (v0.5.0)
           | the dependency survived from the spec-era design (tokio::join!
           | lanes, tokio::select! event loop, tokio::process streaming) but
           | every one of those was superseded: scan lanes went to
           | std::thread::scope (2026-07-05), the provider timeout to a sync
           | runner (2026-07-12), and execution to a pty (2026-07-08). Zero
           | async fn, zero .await, zero tokio references in src/ or tests/;
           | nothing else pulled it in. Removing it drops tokio, tokio-macros
           | and bytes from the lock. Do not reintroduce without a concrete
           | need — the codebase is deliberately sync.

2026-08-24 | enter runs the update; the package list moves to i (user decision)
           | updating is what you open paclens to do, so it belongs on the
           | most default key. enter was drilling into the selected source's
           | package list — an inspection detour — while the primary action
           | sat on u. Swapped: enter executes the plan (u kept as an alias
           | so muscle memory and the docs' older wording still work), and
           | the package list moved to i for "info". d was the other
           | candidate and was rejected: in a package tool d reads as
           | "delete", and paclens should never put a destructive-sounding
           | key on a purely inspective action. P1/P4 are untouched — the
           | dashboard still shows the whole plan before enter runs it, and
           | an empty plan still just flashes "you're up to date".

2026-08-24 | key hints wrap to the pane and drop by rank, never clip
           | the keys pane rendered three hardcoded rows, so anything wider
           | than the pane was truncated mid-word — "r refr", "q qui". The
           | rows are now computed: hints carry a display order and a drop
           | rank, wrap_hints packs them into the width the pane actually
           | has, and fit_hints drops the lowest-ranked hint and re-wraps
           | until they fit the available rows. Display order is independent
           | of rank, so a narrow pane reads like a wide one with fewer
           | entries rather than a reshuffled one. enter update is rank 0 and
           | never drops; q quit is rank 1 (a user who cannot find the exit
           | is stuck); L log drops first. Both dashboard layouts share the
           | list — the flat one just passes max_rows = 1. Also added
           | Glyphs::left/right: the pane-focus hint hardcoded ←/→ even in
           | the ASCII path, which is exactly the tofu the no-color glyph
           | set exists to prevent.
```

---

## 8. Testing without a real system

### Fixture structure

```
tests/
  fixtures/
    pacman/
      qi_firefox.txt          single package, explicit install
      qi_dep_package.txt      single package, installed as dependency
      qi_virtual_provider.txt package with Provides field
      qi_multiline_desc.txt   package with multiline Description
      qi_optional_deps.txt    package with Optional Deps (multi-line)
      qi_none_fields.txt      package with "None" in Depends On / Required By
      qi_small_system.txt     full pacman -Qi output for 20-package test system
      qu_sample.txt           pacman -Qu output with 5 updates
      qu_empty.txt            pacman -Qu with no updates (empty output)
    flatpak/
      list_apps.txt           flatpak list --app output, 10 apps
      list_runtimes.txt       flatpak list --runtime output
      remote_ls_updates.txt   flatpak remote-ls --updates output
      info_metadata.txt       flatpak info --show-metadata output (keyfile format)
    overlap/
      overlap_map.toml        the bundled map (same as production)
```

Capture these from your own Zephyrus G14. They are the ground truth.

### Injectable CommandRunner

Providers accept a `CommandRunner` trait. In tests, inject a mock that returns fixture content:

```rust
struct MockRunner {
    responses: HashMap<String, CommandOutput>,
}

impl CommandRunner for MockRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput> {
        let key = format!("{} {}", program, args.join(" "));
        self.responses.get(&key)
            .cloned()
            .ok_or_else(|| anyhow!("no mock for: {}", key))
    }
}
```

This is the primary testing seam. Every provider test uses this pattern.

---

## 9. AUR package notes

Do not write the PKGBUILD until v0.2.0 is stable.

When you do:
- `cargo build --release --locked` (requires `Cargo.lock` committed)
- the binary is `target/release/paclens` — install to `/usr/bin/paclens`
- `overlap_map.toml` is compiled into the binary via `include_str!()` — no runtime file needed
- `optdepends=('pacman-contrib: optional, not currently used')`
- license: pick before publishing (MIT or Apache-2.0 are standard for Rust tools)
- source: `git+https://github.com/plasmaDestroyer/paclens.git`

Register the name on crates.io before publishing to AUR — even with a `0.0.1` stub.

---

## 10. Name decision

Must be locked before v0.0.1. The name affects:
- binary name (`/usr/bin/<name>`)
- config path (`~/.config/<name>/`)
- cache path (`~/.cache/<name>/`)
- log path (`~/.local/share/<name>/logs/`)
- AUR package name
- crates.io crate name
- GitHub repo name

Before committing: check AUR (`paru -Ss <name>`), crates.io, and GitHub for conflicts.
