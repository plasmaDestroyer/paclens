# paclens — Technical Specification

> Version: 0.2 (pre-implementation, revised)
> Last updated: 2026-05
> Covers: v0.0.1 through v0.2.0

---

## 1. Project definition

paclens is a TUI-first system inspection and update tool for Arch Linux. It aggregates pacman and Flatpak into one interface and adds an advisory layer that explains dependency relationships, flags Flatpak/native overlap, and surfaces safe cleanup options.

It is not a package manager. It does not replace pacman or Flatpak. It wraps them, reads from them, and presents their state in a way that helps the user make informed decisions.

### What it is

- a unified update interface for pacman + Flatpak
- a dependency inspector (`why` lookup with removal impact)
- a Flatpak/native overlap detector with tradeoff analysis
- an orphan and cache reporter
- a conservative advisory tool that never acts without confirmation

### What it is not

- an automatic cleaner
- a migration executor (before v0.5)
- a distro-agnostic tool
- a replacement for any package manager
- a tool that guesses or hallucinates package relationships

---

## 2. Design principles

These are not guidelines. They are constraints. Every design decision must satisfy them.

### P1 — Explain before acting

No action executes without showing the user exactly what will happen. This applies to updates, cache cleanup, orphan removal, and eventually migration. The plan is always visible before execution.

### P2 — Safety over aggression

When in doubt, do nothing. When the tool cannot determine whether a removal is safe, it says so and stops. It never removes more than it was asked to remove.

### P3 — Honest confidence

Every inferred relationship, every heuristic match, every advisory verdict carries a confidence label. The tool never presents an inference as a fact. The three labels are `Confirmed`, `Inferred`, and `Unknown` — defined formally in Section 8.

### P4 — Scan → analyze → plan → confirm → execute

This pipeline is the execution model. Every destructive action must pass through all five stages. There is no shortcut. There is no "fix all" button.

### P5 — One source of truth

The scan cache is the single source of truth for the current system state. The TUI reads from the cache. The `why` command reads from the cache. The overlap detector reads from the cache. Nothing re-derives what was already computed during a scan.

### P6 — Source-specific logic

pacman and Flatpak are not the same. Their dependency models are not the same. Their update mechanisms are not the same. Their data paths are not the same. Every provider module must encode the specific behavior of its source. There are no generic shortcuts.

---

## 3. Architecture overview

```
┌─────────────────────────────────────────────────────┐
│                      CLI / TUI                       │
│  clap entry  │  ratatui app  │  event loop           │
└────────────────────────┬────────────────────────────┘
                         │
┌────────────────────────▼────────────────────────────┐
│                   Application core                   │
│  state manager  │  action planner  │  event bus      │
└──────┬──────────────────┬──────────────────┬────────┘
       │                  │                  │
┌──────▼──────┐  ┌────────▼───────┐  ┌──────▼────────┐
│   Scanner   │  │    Analyzer    │  │   Executor    │
│             │  │                │  │               │
│ - pacman    │  │ - dep graph    │  │ - cmd runner  │
│ - flatpak   │  │ - overlap det. │  │ - sudo mgr    │
│             │  │ - orphan det.  │  │ - log writer  │
└──────┬──────┘  └────────┬───────┘  └──────┬────────┘
       │                  │                  │
┌──────▼──────────────────▼──────────────────▼────────┐
│                    Cache layer                        │
│  scan cache  │  dep graph cache  │  overlap cache    │
└──────────────────────────────────────────────────────┘
```

### Module map

```
src/
├── main.rs              entry point, arg parsing, mode dispatch
├── cli/
│   ├── mod.rs           clap command definitions
│   ├── why.rs           `paclens why <pkg>` handler
│   ├── update.rs        `paclens update` handler
│   ├── overlaps.rs      `paclens overlaps` handler
│   ├── status.rs        `paclens status` handler
│   └── cleanup.rs       `paclens cleanup` handler
├── tui/
│   ├── mod.rs           ratatui app setup, event loop
│   ├── app.rs           application state struct
│   ├── event.rs         event types (terminal input + internal channels)
│   ├── screens/
│   │   ├── dashboard.rs
│   │   ├── updates.rs
│   │   ├── packages.rs
│   │   ├── why.rs
│   │   ├── overlaps.rs
│   │   ├── cleanup.rs
│   │   └── help.rs
│   ├── widgets/
│   │   ├── source_bar.rs
│   │   ├── detail_pane.rs
│   │   ├── progress.rs
│   │   ├── search_bar.rs
│   │   └── footer.rs
│   └── theme.rs         color palette, style constants
├── model/
│   ├── mod.rs           re-exports all model types
│   ├── source.rs        Source, SourceId, SourceKind
│   ├── package.rs       Package, InstallReason
│   ├── update.rs        PendingUpdate
│   ├── dependency.rs    DependencyEdge, EdgeKind, Confidence
│   ├── overlap.rs       OverlapCandidate, MatchMethod, Tradeoff
│   ├── scan.rs          ScanResult
│   └── action.rs        ActionPlan, ActionStep, ActionKind
├── providers/
│   ├── mod.rs           Provider trait, CommandRunner trait
│   ├── pacman.rs        pacman provider
│   └── flatpak.rs       Flatpak provider
├── scanner/
│   ├── mod.rs           Scanner, orchestrates providers
│   └── cache.rs         ScanCache, read/write/invalidate
├── analyzer/
│   ├── mod.rs
│   ├── dep_graph.rs     petgraph wrapper, graph construction from Package data
│   ├── why.rs           why query logic, verdict generation
│   ├── overlap.rs       overlap detection algorithm
│   └── cleanup.rs       orphan detection (from graph), cache sizing
├── executor/
│   ├── mod.rs
│   ├── runner.rs        command spawning, output capture
│   ├── sudo.rs          privilege escalation model
│   └── log.rs           update log writer
└── config/
    ├── mod.rs
    └── schema.rs        Config struct, defaults, TOML deserialization
```

---

## 4. Data model

All types are defined in `src/model/`. These are the canonical definitions. Every other module imports from here.

### 4.1 SourceId

```rust
/// Newtype wrapper for source identifiers.
/// Values: "pacman", "flatpak-user", "flatpak-system"
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub String);
```

### 4.2 Source

```rust
pub struct Source {
    pub id: SourceId,
    pub kind: SourceKind,
    pub available: bool,       // binary found on PATH?
    pub last_scanned: Option<DateTime<Utc>>,
}

pub enum SourceKind {
    Pacman,
    Flatpak { scope: FlatpakScope },
}

pub enum FlatpakScope {
    User,
    System,
}
```

### 4.3 Package

```rust
pub struct Package {
    pub name: String,
    pub version: String,
    pub source_id: SourceId,
    pub install_reason: InstallReason,
    pub size_bytes: Option<u64>,
    pub description: Option<String>,
    /// Direct dependencies (package names this requires).
    /// Populated from `pacman -Qi` "Depends On" field.
    /// Empty for Flatpak (deps are bundled, not cross-referenced).
    pub depends_on: Vec<String>,
    /// Direct reverse dependencies (package names that require this).
    /// Populated from `pacman -Qi` "Required By" field.
    /// Empty for Flatpak.
    pub required_by: Vec<String>,
    /// Optional dependencies (informational only, not used for graph edges).
    pub optional_deps: Vec<String>,
    /// Packages this provides (virtual package names).
    /// Populated from `pacman -Qi` "Provides" field.
    pub provides: Vec<String>,
}

pub enum InstallReason {
    Explicit,       // user installed it directly
    Dependency,     // installed as dep of something else
    Unknown,        // flatpak or source does not distinguish
}
```

### 4.4 PendingUpdate

```rust
pub struct PendingUpdate {
    pub package_name: String,
    pub current_version: String,
    pub available_version: String,
    pub source_id: SourceId,
}
```

### 4.5 DependencyEdge

```rust
/// Edge weight in the dependency graph.
pub struct DependencyEdge {
    pub kind: EdgeKind,
    pub confidence: Confidence,
}

pub enum EdgeKind {
    /// From pacman dep data. Ground truth.
    Real,
    /// Heuristic. Cross-source or appstream-derived.
    Inferred,
}

pub enum Confidence {
    /// Derived from authoritative source data with no inference.
    Confirmed,
    /// Heuristic derivation, likely correct, basis is stated.
    Inferred,
    /// Tool cannot determine from available data.
    Unknown,
}
```

Note: `DependencyEdge` no longer stores `from`/`to` — those are the graph node endpoints. The edge itself is a weight carrying only metadata.

### 4.6 OverlapCandidate

```rust
pub struct OverlapCandidate {
    pub display_name: String,
    pub native_package: Option<PackageRef>,
    pub flatpak_app: Option<PackageRef>,
    pub match_method: MatchMethod,
    pub confidence: Confidence,
    pub tradeoff: Tradeoff,
}

/// Lightweight reference to a package without cloning all data.
pub struct PackageRef {
    pub name: String,
    pub version: String,
    pub source_id: SourceId,
}

pub enum MatchMethod {
    /// Entry in overlap_map.toml.
    KnownMap,
    /// Reversed last component of Flatpak app ID matches pacman name.
    ReverseDnsSuffix,
    /// Flatpak appstream display name matches pacman name.
    DisplayNameMatch,
}

pub struct Tradeoff {
    pub native_profile_path: Option<PathBuf>,
    pub flatpak_profile_path: Option<PathBuf>,
    pub native_version: Option<String>,
    pub flatpak_version: Option<String>,
    pub native_is_newer: Option<bool>,
    pub flatpak_profile_size_bytes: Option<u64>,
    pub likely_primary: PrimaryHeuristic,
}

pub enum PrimaryHeuristic {
    Native,
    Flatpak,
    Unknown,
}
```

### 4.7 ScanResult

```rust
pub struct ScanResult {
    /// Increment when struct changes in a breaking way. Current: 1.
    pub schema_version: u32,
    pub scanned_at: DateTime<Utc>,
    pub sources: Vec<Source>,
    pub packages: Vec<Package>,
    pub updates: Vec<PendingUpdate>,
    pub cache_sizes: CacheSizes,
}

pub struct CacheSizes {
    pub pacman_cache_bytes: Option<u64>,
    pub flatpak_unused_runtime_count: Option<u32>,
    pub flatpak_unused_runtime_bytes: Option<u64>,
}
```

Note: the dep graph and overlap results are not cached inside `ScanResult`. They are computed by the analyzer from the cached `ScanResult` on load. This avoids serializing the `petgraph` structure (complex, version-fragile). Graph construction from the `Package.depends_on` / `required_by` fields is fast (<100ms for ~2000 packages) and does not need caching.

### 4.8 ActionPlan

```rust
pub struct ActionPlan {
    pub created_at: DateTime<Utc>,
    pub steps: Vec<ActionStep>,
    pub requires_sudo: bool,
}

pub struct ActionStep {
    pub source_id: SourceId,
    pub kind: ActionKind,
    pub targets: Vec<String>,
    pub command: Vec<String>,   // exact argv to execute
}

pub enum ActionKind {
    Update,
    // Remove is not generated in v0.x. Listed for future use.
}
```

---

## 5. Provider specifications

### 5.1 Provider trait

```rust
/// Trait for command execution, injectable for testing.
pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput>;
}

pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Trait for package source providers.
pub trait Provider: Send + Sync {
    fn source_id(&self) -> SourceId;
    fn is_available(&self) -> bool;
    fn scan_installed(&self) -> Result<Vec<Package>>;
    fn scan_updates(&self) -> Result<Vec<PendingUpdate>>;
    fn build_update_command(&self, targets: &[String]) -> Vec<String>;
    fn requires_sudo_for_update(&self) -> bool;
}
```

`CommandRunner` is injected into providers. In production: calls the real binary. In tests: returns fixture data. This is the primary testing seam.

### 5.2 Pacman provider

**Binary:** `pacman`

**Installed packages (full metadata):**
```
pacman -Qi
```
Parse: multi-record output. Records separated by blank lines. Each record is `Key  : Value` pairs (note: two spaces before colon).

Fields extracted per record:

| Field | Maps to |
|---|---|
| `Name` | `Package.name` |
| `Version` | `Package.version` |
| `Description` | `Package.description` |
| `Installed Size` | `Package.size_bytes` (parse human-readable: "12.34 MiB") |
| `Install Reason` | `Package.install_reason` |
| `Depends On` | `Package.depends_on` (space-separated, strip version constraints) |
| `Required By` | `Package.required_by` (space-separated) |
| `Optional Deps` | `Package.optional_deps` |
| `Provides` | `Package.provides` (space-separated, strip version constraints) |
| `Groups` | informational, not stored in v0.x |

**Parsing edge cases:**
- `Depends On : None` → empty vec, not a package named "None"
- `Required By : None` → empty vec
- multiline `Description` — value continues on next line if next line starts with whitespace
- multiline `Optional Deps` — each optdep on its own line, indented
- version constraints in `Depends On` and `Provides` (e.g., `glibc>=2.38`) — strip the constraint, keep only the package name
- `Provides` can include virtual names (e.g., `sh` provided by `bash`)

**Update list:**
```
pacman -Qu
```
Parse: one line per update, format `<name> <current> -> <available>`.

**Cache size:**
```
du -sb /var/cache/pacman/pkg/
```
Parse: first whitespace-separated field is bytes.

**Update execution:**
See open question Q6 (Section 18) regarding `--noconfirm`.

**Error handling:**
- `pacman` not on PATH → `Source.available = false`, skip
- non-zero exit from scan commands → capture stderr, surface as provider error
- db lock (`/var/lib/pacman/db.lck` exists) → detect and show specific error: "pacman database is locked. Another pacman process may be running."

### 5.3 Flatpak provider

**Binary:** `flatpak`

**Installed apps:**
```
flatpak list --app --columns=application,name,version,origin,installation
```
Parse: tab-separated. `installation` is `user` or `system`.

**Installed runtimes (for unused runtime detection):**
```
flatpak list --runtime --columns=application,version,installation
```

**Update list:**
```
flatpak remote-ls --updates --app --columns=application,version
```
Timeout: 10 seconds. If timeout: warn, use last cached update list if available.

**Unused runtimes:**
```
flatpak uninstall --unused --dry-run
```
Parse: list of runtimes that would be removed.

**Update execution:**
```
flatpak update --noninteractive
```
Flatpak's `--noninteractive` suppresses its own prompts. paclens handles confirmation before calling this.

**Profile path:**
```
~/.var/app/<application-id>/
```
Check for existence. If present, compute size with `du -sb`.

**Appstream metadata (for display name matching in overlap detection):**
```
flatpak info --show-metadata <application-id>
```
Output is GLib keyfile format. Parse the `[Application]` section for `name=` field.

**Error handling:**
- `flatpak` not on PATH → skip
- remote unreachable → show warning, continue with other data
- no apps installed → return empty vec, not an error

---

## 6. Cache specification

### 6.1 Location

```
~/.cache/paclens/scan.toml
```

Resolved via `directories::ProjectDirs`. Create directory with `0700` permissions if absent.

### 6.2 Format

TOML. Contains the serialized `ScanResult` struct.

### 6.3 Invalidation rules (checked in order)

1. `--refresh` flag → always invalidate, re-scan
2. `schema_version` mismatch → delete cache, re-scan, log warning
3. pacman db modified since last scan: compare `scanned_at` against mtime of `/var/lib/pacman/local/` → invalidate if db is newer
4. `scanned_at` older than `config.general.cache_ttl` → invalidate
5. `config.toml` modified more recently than `scan.toml` → invalidate (source enable/disable may have changed)
6. Otherwise → load from cache

### 6.4 Write behavior

Atomic writes: write to `scan.toml.tmp`, then `rename()` to `scan.toml`. `rename` is atomic on Linux (same filesystem). Clean up stale `.tmp` files at startup.

### 6.5 Schema versioning

`schema_version` is a constant in `src/scanner/cache.rs`. Increment on any breaking change to `ScanResult`. No migration logic — mismatch triggers full re-scan.

### 6.6 What is not cached

The dependency graph and overlap results are computed from `ScanResult` on every load. The graph is built from `Package.depends_on` / `required_by` fields in-memory. Overlaps are detected from the package list. Both operations are fast enough (<200ms total) to not require separate caching.

---

## 7. Dependency graph specification

### 7.1 Construction

Built by the analyzer from the `packages` field of `ScanResult`. Not built during the scan itself.

Uses `petgraph::DiGraph<String, DependencyEdge>`. Nodes are package names. Edges carry `DependencyEdge` (kind + confidence).

**Construction algorithm:**

```
for each Package in ScanResult.packages:
    add node(package.name) if not present
    for each dep_name in package.depends_on:
        add node(dep_name) if not present
        add edge(package.name → dep_name, Real, Confirmed)
    for each provided_name in package.provides:
        record alias: provided_name → package.name
```

Virtual packages (from `Provides`) are resolved via the alias map. When a package depends on a virtual name, the edge points to the real provider.

A `HashMap<String, NodeIndex>` maps package names to graph indices. This is rebuilt every time the graph is constructed (not serialized).

### 7.2 Queries

**Forward deps (what does X require):**
```
graph.neighbors_directed(node, Outgoing)
```

**Reverse deps (what requires X):**
```
graph.neighbors_directed(node, Incoming)
```

**Transitive reverse deps (full removal impact):**
DFS from node following Incoming edges. Collect all reachable nodes. Stop at `config.why.max_depth`.

**Orphan detection (from graph):**
A pacman package is an orphan candidate when:
- `install_reason == Dependency`
- incoming edge count == 0 (nothing requires it)

This replaces calling `pacman -Qtd` — the information is already in the graph.

### 7.3 Safe-to-remove heuristic

A package is labeled `likely safe` only when:
- `InstallReason::Dependency`
- reverse dep count == 0
- no `Confirmed` incoming edges

A package is `is a dependency` when:
- reverse dep count > 0

Otherwise: `unclear — check manually`.

### 7.4 Confidence propagation

- edges from pacman `Depends On` / `Required By` → `EdgeKind::Real`, `Confidence::Confirmed`
- edges from Flatpak app ID grouping → `EdgeKind::Inferred`, `Confidence::Inferred`
- any cross-source edge → `EdgeKind::Inferred`, `Confidence::Unknown`

The `why` verdict uses the lowest-confidence edge in the relevant path. If any edge is `Unknown`, the aggregate verdict cannot be better than `Unknown`.

---

## 8. Confidence model

Formal definition. Every piece of advisory output carries one of these labels.

| Label | Definition | Examples |
|---|---|---|
| `Confirmed` | Derived from authoritative source data with no inference | pacman dep edges, install reason from `pacman -Qi` |
| `Inferred` | Heuristic derivation, likely correct, basis is stated | reverse DNS overlap match, app ID prefix grouping |
| `Unknown` | Tool cannot determine from available data | cross-source relationships, display name matches |

### Rules

1. Never promote a label: if an `Unknown` edge is in the path, the verdict is at best `Unknown`
2. Always show the label inline with the fact, not separately
3. `Confirmed` does not mean "safe to remove" — it means "this relationship is certain"
4. Verdicts combine confidence levels from multiple sources — label each component separately in the UI

---

## 9. Overlap detection algorithm

### 9.1 Input

- all `Package` entries where `source_id` is pacman
- all `Package` entries where `source_id` is flatpak-user or flatpak-system

### 9.2 Matching pipeline

For each Flatpak app, attempt matches in order. Use the first match found.

**Step 1: Known name map**

Load `overlap_map.toml` (bundled via `include_str!()`).

If the Flatpak app ID appears in this map and the corresponding pacman package is installed → match with `Confidence::Confirmed`, `MatchMethod::KnownMap`.

**Step 2: Reverse DNS suffix**

Extract the last component of the Flatpak app ID, lowercased:
- `org.mozilla.firefox` → `firefox`
- `com.visualstudio.code` → `code`
- `io.github.celluloid_player.Celluloid` → `celluloid`

If a pacman package with exactly that name exists → match with `Confidence::Inferred`, `MatchMethod::ReverseDnsSuffix`.

**Step 3: Display name match**

Fetch Flatpak appstream metadata. Extract display name. Lowercase, strip whitespace. Compare against pacman package names.

If match → `Confidence::Unknown`, `MatchMethod::DisplayNameMatch`.

### 9.3 False positive suppression

Do not match if:
- the Flatpak entry is a runtime (filtered out in Step 9.1)
- the package name appears in `config.overlap.ignore`
- the pacman name is in the generic blocklist: `base`, `linux`, `linux-headers`, `glibc`, `gcc`, `files`, `core`, `extra`, `man`, `lib`, `utils`

### 9.4 Primary install heuristic

1. If native has `InstallReason::Explicit` and Flatpak has `InstallReason::Unknown` → native is likely primary
2. If Flatpak profile path exists and is > 10MB → Flatpak is likely primary (user has data there)
3. Otherwise → `PrimaryHeuristic::Unknown`

Advisory only. Never act on heuristic without user confirmation.

### 9.5 Tradeoff model

For each overlap, show:

| Factor | Native | Flatpak |
|---|---|---|
| sandboxing | no | yes (portals) |
| system integration | full (dbus, theming, etc.) | partial (portal-gated) |
| update source | pacman (rolling) | Flatpak remote |
| profile location | `~/.config/`, `~/.local/share/` | `~/.var/app/<id>/` |
| file access | unrestricted | portal-gated |
| theming | system theme | may not follow system theme |

---

## 10. TUI specification

### 10.1 Framework

`ratatui` with `crossterm` backend. Event loop runs on the main thread. Async operations (scan, execution) spawn on tokio and send results via `mpsc` channel. The event loop `select!`s between crossterm events and channel messages.

### 10.2 Layout model

```
┌─────────────────────────────────────────────────────────┐
│  header: tool name │ current screen name │ scan status  │
├─────────────────────────────────────────────────────────┤
│                                                         │
│                    main content area                    │
│                                                         │
│                                        ┌───────────────┐│
│                                        │  detail pane  ││
│                                        │  (toggleable) ││
│                                        └───────────────┘│
├─────────────────────────────────────────────────────────┤
│  footer: keybindings for current screen                 │
└─────────────────────────────────────────────────────────┘
```

Detail pane: toggled with `D`. Right side, ~35% width. Shows context for selected item.

### 10.3 Screens

**Dashboard:** source status bars, total pending updates (prominent), last scan time, orphan count (if >0), overlap count (if >0). Navigation hints in footer.

**Update screen:** source toggles (space bar), grouped update list, per-row: name, current → new version. Summary bar at bottom. Enter to confirm. During execution: TUI suspends, command runs in raw terminal, TUI restores with result.

**Package list:** searchable (`/`), filterable, sorted by name. Columns: name, version, source, install reason, size. `W` opens why panel.

**Why panel:** right pane or full-width modal. Shows: install reason, reverse dep tree (indented, with confidence labels on each edge), removal impact, verdict (colored). Arrow keys scroll, Esc closes.

**Overlap screen:** list of overlap candidates. Columns: display name, native version, Flatpak version, confidence. Detail pane shows tradeoff table.

**Cleanup screen:** orphan list and cache summary. Enter on orphan → why panel. Commands shown as copiable text.

**Help overlay:** full-screen. All keybindings grouped by screen. Toggle with `?`.

### 10.4 Global keybindings

| Key | Action |
|---|---|
| `Q` | quit |
| `?` | toggle help overlay |
| `R` | force refresh scan |
| `D` | toggle detail pane |
| `Esc` | close panel / go back |
| `Tab` | move between focusable areas |
| `/` | activate search (list screens) |
| `Enter` | confirm / select |

### 10.5 Screen-specific keybindings

| Screen | Key | Action |
|---|---|---|
| Dashboard | `U` | go to update screen |
| Dashboard | `P` | go to package list |
| Dashboard | `O` | go to overlap screen |
| Dashboard | `C` | go to cleanup screen |
| Any list | `W` | open why panel for selected |
| Update | `Space` | toggle source on/off |
| Update | `L` | open log viewer |

### 10.6 Color palette

Defined in `src/tui/theme.rs`. All colors reference this module — no hardcoded values anywhere else.

| Role | Dark terminal | Purpose |
|---|---|---|
| bg | terminal default | do not override terminal bg |
| surface | `#313244` | card/pane backgrounds |
| text | `#cdd6f4` | primary text |
| muted | `#6c7086` | secondary text, borders |
| accent | `#89b4fa` | highlights, selected items |
| success / confirmed | `#a6e3a1` | safe verdicts, confirmed labels |
| warning / inferred | `#f9e2af` | caution verdicts, inferred labels |
| error / unknown | `#f38ba8` | errors, unclear verdicts |

`--no-color` disables all ANSI colors and switches to ASCII-only box drawing.

---

## 11. CLI specification

### 11.1 Entry point

```
paclens [FLAGS] [SUBCOMMAND]
```

No subcommand: open TUI (equivalent to `paclens ui`).

### 11.2 Global flags

| Flag | Description |
|---|---|
| `--refresh` | force re-scan, ignore cache |
| `--no-color` | disable color output |
| `--debug` | enable debug-level logging to stderr and log file |
| `--config <path>` | use alternate config file |
| `-v, --version` | print version and exit |
| `-h, --help` | print help and exit |

### 11.3 Subcommands

**`paclens ui`** — open TUI (default when no subcommand given)

**`paclens status`** — print dashboard summary to stdout
```
pacman    1247 installed   3 updates
flatpak     42 installed   1 update
orphans      8
overlaps     2 detected
cache      4.2 GB
last scan  12 minutes ago
```

**`paclens update [--dry-run] [--source <id>]`** — update all or specific source. `--dry-run` prints plan, executes nothing.

**`paclens why <package>`** — print dependency explanation
```
firefox
  source:   pacman
  reason:   explicitly installed
  required by: nothing
  would also remove: nothing
  verdict:  likely safe [confirmed]
```

**`paclens overlaps`** — print detected Flatpak/native overlaps
```
firefox
  native:  firefox 128.0-1  (pacman, explicit)
  flatpak: org.mozilla.firefox 128.0  (flathub, user)
  match:   known map [confirmed]
  primary: native (explicit install)
  tradeoff: native has full integration; flatpak has sandboxing
```

**`paclens cleanup`** — print orphan and cache summary (advisory, no actions)

---

## 12. Configuration specification

### 12.1 Location

```
~/.config/paclens/config.toml
```

Created with defaults on first run if absent. Full schema with comments in `config.default.toml`.

### 12.2 Validation

- unknown keys: log warning, continue (forward compatibility)
- invalid values: log error, use default for that field
- malformed TOML: abort with clear error message pointing to the file and line

---

## 13. Privilege model

### 13.1 Principle

paclens runs as the user. It only escalates privileges when executing a pacman update. It never stores credentials. It never runs a background privileged process.

### 13.2 Escalation mechanism

For TUI mode (v0.0.6 through v0.1+):
1. paclens suspends the TUI (`LeaveAlternateScreen`)
2. shows the user the exact command that will run
3. spawns the command (which may include `sudo`) in the raw terminal
4. user interacts with sudo prompt and pacman directly
5. command completes, paclens restores the TUI (`EnterAlternateScreen`)
6. result (exit code) shown in TUI

See open question Q6 for discussion on `--noconfirm`.

### 13.3 Flatpak

User-scope Flatpak updates: no sudo needed. System-scope: needs sudo (same escalation model). Scope detected from `installation` column.

### 13.4 Detecting privilege tool

Check in order: `sudo`, `doas`, `pkexec`. Use the first one found. If none available: show error, do not proceed with privileged operations.

### 13.5 What paclens never does

- caches sudo credentials between sessions
- stores passwords
- runs as a daemon with elevated privileges
- uses setuid or capabilities

---

## 14. Logging specification

### 14.1 Location

```
~/.local/share/paclens/logs/paclens-YYYY-MM-DD-HHMMSS.log
```

Timestamp in filename prevents collisions if run multiple times per day.

### 14.2 Log levels

| Level | When used |
|---|---|
| ERROR | unrecoverable errors, provider failures |
| WARN | recoverable issues, unexpected output, heuristic fallbacks |
| INFO | scan start/end, update execution, key events |
| DEBUG | raw command output, parse steps, cache operations |

Controlled by `config.general.log_level` or `--debug` flag (`--debug` sets DEBUG).

### 14.3 Rotation

Keep `config.general.log_keep_count` most recent files (default: 10). Delete older on startup.

### 14.4 Update log format

```
[2026-05-20T14:32:11Z INFO] update session started
[2026-05-20T14:32:11Z INFO] sources: [pacman, flatpak-user]
[2026-05-20T14:32:11Z INFO] pacman: running update (19 packages)
[2026-05-20T14:33:02Z INFO] pacman: completed, exit 0
[2026-05-20T14:33:02Z INFO] flatpak-user: running update (3 apps)
[2026-05-20T14:33:15Z INFO] flatpak-user: completed, exit 0
[2026-05-20T14:33:15Z INFO] update session complete: all sources succeeded
```

---

## 15. Error handling

### 15.1 Rules

- no `unwrap()` or `expect()` in production paths — `#![deny(clippy::unwrap_used)]`
- `anyhow::Result` for fallible functions, `thiserror` for provider-specific error types
- every user-visible error has a human-readable message and a "what happens next" line
- every error logged at appropriate level
- provider errors are isolated: one failure does not abort others

### 15.2 User-visible error format

```
error: pacman scan failed
  pacman exited with code 1
  stderr: error: could not open database
  → paclens will continue without pacman data
```

### 15.3 Recovery table

| Error | Recovery |
|---|---|
| provider binary not found | skip source, show "not available" |
| provider exits non-zero | show error inline, continue with others |
| pacman db locked | show specific message, do not proceed with pacman |
| cache write fails | log error, use in-memory data for session |
| cache schema mismatch | delete cache, re-scan, log warning |
| config parse error | abort with error pointing to file |
| sudo/doas not available | show error, skip privileged operations |
| Flatpak remote unreachable | show warning, use cached data if available |
| provider timeout (>10s) | kill child process, show timeout error, continue |

---

## 16. Testing strategy

### 16.1 Unit tests

Every parser must have unit tests against real command output fixtures stored in `tests/fixtures/`.

Required coverage:
- `providers::pacman` — `pacman -Qi` parse (normal, multiline description, optional deps, virtual packages, "None" fields)
- `providers::pacman` — `pacman -Qu` parse
- `providers::flatpak` — `flatpak list` parse (app and runtime)
- `providers::flatpak` — `flatpak remote-ls --updates` parse
- `scanner::cache` — write, read, version mismatch, age invalidation, pacman-db-mtime invalidation
- `analyzer::dep_graph` — construction, forward deps, reverse deps, transitive lookup, empty graph, virtual package resolution
- `analyzer::overlap` — known map match, reverse DNS, display name, false positive rejection, blocklist
- `analyzer::why` — safe verdict, dependency verdict, unclear verdict, virtual package handling

### 16.2 Integration tests

Mock `CommandRunner` returns fixture data. Test the full scan → cache → analyzer pipeline end-to-end.

### 16.3 Not tested in CI

- TUI rendering (manual)
- actual pacman/Flatpak execution (requires real system)
- sudo behavior

### 16.4 CI pipeline

GitHub Actions. Stable Rust only.

```
cargo fmt --check
cargo clippy -- -D warnings -D clippy::unwrap_used
cargo test
cargo build --release
```

---

## 17. Deferred features

Out of scope for v0.0.x through v0.2.0. Do not design for them. Do not add extension points unless they emerge naturally.

| Feature | Earliest | Rationale |
|---|---|---|
| paru / AUR provider | v0.3 | needs its own parser, VCS package handling |
| migration advisory | v0.4 | needs profile mapping database |
| migration execution | v0.5 | high-risk file operations |
| cargo / npm / brew / pipx | v0.6 | maintenance burden before core is stable |
| app-level grouping DB | v0.4 | needs community contribution |
| destructive cleanup | v0.5 | only after advisory layer is trusted |
| system doctor | v0.7 | scope creep risk |
| daemon / background sync | post-1.0 | no confirmed need |
| plugin system | post-1.0 | premature abstraction |
| non-Arch distro support | never | by design |

---

## 18. Open questions

Decisions needed before or during implementation. Recorded to avoid re-litigating mid-build.

**Q1: Binary name**
`paclens` is the current placeholder. Affects config path, log path, AUR package name, and crates.io registration. Must be locked before any path-related code is written.

**Q2: pactree as optional enhancement**
The dep graph is built from `pacman -Qi` output (Depends On / Required By fields) — no pactree dependency. However, pactree provides more detailed transitive analysis. Options:
- never use pactree (the in-memory graph covers all needs)
- detect pactree and use it for `--deep` transitive queries as an optional enhancement

Recommendation: do not depend on pactree. The in-memory graph from `pacman -Qi` data is sufficient and faster.

**Q3: overlap_map.toml maintenance**
Options:
- ship a minimal map (~50 entries, bundled), allow user extension in config
- pull from a remote URL periodically
- crowdsource contributions via GitHub

Recommendation: ship bundled + user config extension. No remote fetch in v0.x. Accept GitHub PRs for new entries.

**Q4: Flatpak system vs user scope**
If both user and system Flatpak installs exist for the same app, show as two separate packages with scope labeled. Do not merge.

**Q5: Scan parallelism**
pacman and Flatpak scans run concurrently via `tokio::join!`. Dep graph construction runs after both complete (needs both datasets). Overlap detection runs after graph is built.

**Q6: `--noconfirm` for pacman**
`pacman -Syu --noconfirm` suppresses pacman's own prompts — including conflict resolution. This is dangerous: if pacman encounters a file conflict or package replacement, `--noconfirm` may cause it to skip or fail silently.

Options:
- use `--noconfirm` and accept the risk (simpler UX, rare failures)
- do not use `--noconfirm`, let pacman prompt directly in the raw terminal (user handles conflicts themselves)
- use `--noconfirm` but detect common failure patterns in exit code/stderr and warn

Recommendation: do NOT use `--noconfirm`. Since the TUI suspends and pacman runs in the raw terminal, let the user interact with pacman directly. This is safer and consistent with principle P2.

**Q7: Cache format**
TOML is human-readable but verbose for ~2000 packages. Options:
- TOML (current choice — debuggable, ~2-5MB for typical system)
- MessagePack or bincode (faster, smaller, but not human-readable)
- JSON (middle ground)

Recommendation: start with TOML. If cache read/write becomes a measurable bottleneck (>500ms), switch to MessagePack. Profile before optimizing.
