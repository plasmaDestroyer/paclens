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

`flatpak remote-ls --updates` can be slow if remotes are unreachable. Always run with a 10-second timeout (`tokio::time::timeout`). If it times out, log a warning and fall back to the cached update list. Never block the UI waiting for a remote.

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
- runs providers concurrently via `tokio::join!`
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
