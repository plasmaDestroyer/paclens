# paclens — design

What this tool will and will not do to your system, and why.

This is the project's only design document. `README.md` is for people deciding
whether to install paclens; `CLAUDE.md` is working instructions for agents. Both
defer to this file, and so does every comment in the code — where any of them
disagrees with what follows, this wins and the other one is wrong.

It absorbed `spec.md` and `dev-notes.md` on 2026-08-25. Sections 1–5 are what the
tool promises. Sections 6–12 are how it is built, and the things that were
painful to learn. Section 13 is the dated record of every decision, which is why
the rest says what it says.

**Contents**

| | |
|---|---|
| 1–5 | [The test](#1-the-test) · [Principles](#2-principles) · [Rules](#3-rules) · [Defaults](#4-defaults-not-rules) · [Not in scope](#5-not-in-scope) |
| 6–9 | [Architecture](#6-architecture-and-module-contracts) · [Data and cache](#7-data-cache-and-schema) · [Graph and confidence](#8-dependency-graph-and-the-confidence-model) · [Overlaps](#9-overlap-detection) |
| 10–12 | [Providers and parsing](#10-providers--commands-parsing-and-gotchas) · [Privilege, logging, errors](#11-privilege-logging-and-errors) · [Testing](#12-testing) |
| 13–14 | [Decisions log](#13-decisions-log) · [Amending this](#14-amending-this) |

---

## 1. The test

Every rule here had to pass one question: **can you state the harm?**

If a thing has a nameable way of hurting someone — a broken upgrade, a deleted
file, a number that lies — it becomes a rule, and it holds even when it is
inconvenient and even when someone asks for it to be relaxed.

If the only objection is that it feels wrong, it is not a rule. It is a
**default**. Defaults are where taste belongs: the people who need the thing can
turn it on, having read what it costs, and everyone else never meets it. A
prohibition costs you every user who genuinely needed it; a default costs
nothing.

This distinction is the point of the document. Without it, "principles" quietly
become a list of the author's preferences, and the real constraints get lost
among them.

---

## 2. Principles

**1. Explain before acting.** Nothing runs until you have seen exactly what will
run. Not a summary of it — the commands.

**2. Safety over aggression.** When in doubt, do nothing. When paclens cannot
tell whether something is safe, it says so and stops. It never removes more than
it was asked to.

**3. Honest confidence.** Every inference carries a label: `Confirmed`,
`Inferred`, or `Unknown`. A guess is never presented as a fact, and a label is
never promoted — one `Unknown` edge anywhere in a chain caps the whole verdict
at `Unknown`.

**4. The pipeline.** scan → analyze → plan → confirm → execute. Every
destructive action passes through all five. There is no shortcut and there is no
"fix all" button.

**5. One source of truth.** Every view reads the same scan. Two screens can be
stale together, but they can never disagree with each other.

**6. Source-specific logic.** pacman, the AUR and Flatpak differ in every
respect that matters, and paclens does not paper over that with a generic
abstraction. *Under active review as more sources arrive — see the open issues
labelled `sources`.*

---

## 3. Rules

Each of these is a principle applied to a specific decision that came up.

### Updates

**No `--noconfirm` for pacman. Ever.**
It does not just skip "are you sure" — it silently answers conflict resolution
and package-replacement prompts, so a transaction can remove something you
wanted and you never see the question. Hands-free updating is solved instead by
answering prompts in the terminal, selectively, where every question and answer
stays visible and anything unrecognised hands control back.

**Never produce a partial upgrade.**
No `-Sy` without `-Su`, and no skipping the sync to save a few seconds — not
even when paclens just checked for updates and "knows" the answer. A half-synced
system breaks later, somewhere unrelated, in a way that looks like a different
bug entirely.

**paru is never run under sudo.**
It builds as you and elevates only for the install step. Running the build as
root is the failure that PKGBUILD review exists to protect you from.

**`pacman -Sc` is never suggested.**
Since pacman 7 it trips over partial downloads owned by the sandbox user.
`paccache` and `paru -Sc --aur` are suggested instead.

### Removing things

**No removal without an explanation shown first.**
Every removal path passes through a report of what depends on the thing and
what breaks without it. There is no bare delete key anywhere, and there will
not be one.

**Nothing is removed automatically. There is no "clean everything".**
Cleanup is per item, and every item is a separate judgement. Suggestions are
text you can read before you run them.

**Migration never deletes.**
A migration plan contains no `rm` anywhere — existing files are copied aside
into a timestamped backup first, then the copy happens. Removing the source is
a separate plan that only arms after a clean copy that *you* verified.

**Leftover download partials are reported, never removed.**
paclens tells you they exist and what would clear them. Deleting files it does
not fully understand is not its job.

*Intended, not yet built:* removal is **unavailable** when the verdict is
`Unknown` — not discouraged, not behind an extra prompt. If the tool cannot say
what breaks, it has no business offering to break it.

### Numbers

**A number that would mislead is not shown.**
An 11 GiB package cache that `paccache` would free nothing from says exactly
that. Reclaimable sits next to the total, never in place of it.

**No total silently double-counts.**
Package sizes overlap, because shared libraries belong to everything that needs
them. Any total says which unit it is measuring.

**Progress is measured or labelled.**
A count shown is a real count. A bar is either a real ratio or an estimate
explicitly marked as one. No indicator sits at its maximum while work continues.

**Never show a zero you are about to contradict.**
While a scan is incomplete, derived figures read `computing…`. Showing `0`
and correcting it two seconds later is worse than showing nothing.

### Inference

**A false negative beats a false positive.**
Overlap matching would rather miss a duplicate than invent one. Every match
carries the confidence of the weakest step that produced it.

**Unknown is a real answer.**
"I cannot tell" is a legitimate, common, and useful thing for this tool to say.
It is never rounded up to a guess to make a screen look more complete.

---

## 4. Defaults, not rules

These exist, they are off, and turning them on is your call. Each one asks
first and says what it costs.

- **Auto-approving routine prompts.** Off. Enabling it requires acknowledging a
  warning, and while it is on, that stays visible — not just at the moment you
  enabled it.
- **Keeping sudo alive through a long run.** Off. Convenient, and a small
  widening of what can use your credentials while the run lasts.
- **Checking VCS packages against upstream.** Off. Accurate and slow.
- **Background checks and notifications.** Off. Enable them if you want them.

If you find yourself wanting to *forbid* one of these, re-read the test above.

---

## 5. Not in scope

**paclens is not a package manager.** It wraps pacman, paru and Flatpak, reads
their state, and explains it. Where it acts, it acts by running the real tools
in a real terminal, visibly. It does not reimplement them and does not want to
replace them.

**Arch only.** Not a portability decision that might change — a scope decision.
Everything above depends on knowing exactly how one package manager behaves.

**No daemon.** A user timer running a short-lived check is fine. A resident
process is not.

**No plugin system, for now.** Not until the provider contract has survived
several real sources and someone has a concrete thing they cannot do without it.

---

## 6. Architecture and module contracts

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

#### Module map

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

### Module contracts

#### Provider

Every provider must:
- accept a `CommandRunner` for dependency injection (testing seam)
- return `Ok(vec![])` if nothing is installed (not an error)
- return `Err` only if the source binary exists but the command failed
- respect the configured timeout (`config.scan.provider_timeout_secs`)
- never call anything that requires sudo (scanning is always unprivileged)
- never know about other providers (the scanner orchestrates them)

#### Scanner

The scanner:
- detects available providers (checks PATH)
- runs providers concurrently on scoped threads (`std::thread::scope`)
- assembles the combined `ScanResult`
- writes the result to cache
- never analyzes data — that is the analyzer's job

#### Analyzer

The analyzer:
- is pure: given the same `ScanResult`, always produces the same output
- never calls providers or subprocess commands
- never writes to disk
- constructs the dep graph, overlap candidates, and orphan list from `ScanResult`

#### Executor

The executor:
- only executes pre-built `ActionPlan` values
- never decides what to do — all decisions come from the user via TUI
- logs every command before and after execution
- reports exit codes without interpretation (the TUI interprets)

---

### Initialization

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

## 7. Data, cache, and schema

The authoritative data model is `src/model/` — it has moved past what was
originally specified (the `aur` source, flatpak runtimes as packages,
profile-size maps) and a prose copy would only drift. What follows is the
part that is *not* obvious from the types.

#### Location

```
~/.cache/paclens/scan.toml
```

Resolved via `directories::ProjectDirs`. Create directory with `0700` permissions if absent.

#### Format

TOML. Contains the serialized `ScanResult` struct.

#### Invalidation rules (checked in order)

1. `--refresh` flag → always invalidate, re-scan
2. `schema_version` mismatch → delete cache, re-scan, log warning
3. pacman db modified since last scan: compare `scanned_at` against mtime of `/var/lib/pacman/local/` → invalidate if db is newer
4. `scanned_at` older than `config.general.cache_ttl` → invalidate
5. `config.toml` modified more recently than `scan.toml` → invalidate (source enable/disable may have changed)
6. Otherwise → load from cache

#### Write behavior

Atomic writes: write to `scan.toml.tmp`, then `rename()` to `scan.toml`. `rename` is atomic on Linux (same filesystem). Clean up stale `.tmp` files at startup.

#### Schema versioning

`schema_version` is a constant in `src/scanner/cache.rs`. Increment on any breaking change to `ScanResult`. No migration logic — mismatch triggers full re-scan.

#### What is not cached

The dependency graph and overlap results are computed from `ScanResult` on every load. The graph is built from `Package.depends_on` / `required_by` fields in-memory. Overlaps are detected from the package list. Both operations are fast enough (<200ms total) to not require separate caching.

---

## 8. Dependency graph and the confidence model

#### Construction

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

#### Queries

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

#### Safe-to-remove heuristic

A package is labeled `likely safe` only when:
- `InstallReason::Dependency`
- reverse dep count == 0
- no `Confirmed` incoming edges

A package is `is a dependency` when:
- reverse dep count > 0

Otherwise: `unclear — check manually`.

#### Confidence propagation

- edges from pacman `Depends On` / `Required By` → `EdgeKind::Real`, `Confidence::Confirmed`
- edges from Flatpak app ID grouping → `EdgeKind::Inferred`, `Confidence::Inferred`
- any cross-source edge → `EdgeKind::Inferred`, `Confidence::Unknown`

The `why` verdict uses the lowest-confidence edge in the relevant path. If any edge is `Unknown`, the aggregate verdict cannot be better than `Unknown`.

---

Formal definition. Every piece of advisory output carries one of these labels.

| Label | Definition | Examples |
|---|---|---|
| `Confirmed` | Derived from authoritative source data with no inference | pacman dep edges, install reason from `pacman -Qi` |
| `Inferred` | Heuristic derivation, likely correct, basis is stated | reverse DNS overlap match, app ID prefix grouping |
| `Unknown` | Tool cannot determine from available data | cross-source relationships, display name matches |

#### Rules

1. Never promote a label: if an `Unknown` edge is in the path, the verdict is at best `Unknown`
2. Always show the label inline with the fact, not separately
3. `Confirmed` does not mean "safe to remove" — it means "this relationship is certain"
4. Verdicts combine confidence levels from multiple sources — label each component separately in the UI

---

## 9. Overlap detection

#### Input

- all `Package` entries where `source_id` is pacman
- all `Package` entries where `source_id` is flatpak-user or flatpak-system

#### Matching pipeline

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

#### False positive suppression

Do not match if:
- the Flatpak entry is a runtime (filtered out in Step 9.1)
- the package name appears in `config.overlap.ignore`
- the pacman name is in the generic blocklist: `base`, `linux`, `linux-headers`, `glibc`, `gcc`, `files`, `core`, `extra`, `man`, `lib`, `utils`

#### Primary install heuristic

1. If native has `InstallReason::Explicit` and Flatpak has `InstallReason::Unknown` → native is likely primary
2. If Flatpak profile path exists and is > 10MB → Flatpak is likely primary (user has data there)
3. Otherwise → `PrimaryHeuristic::Unknown`

Advisory only. Never act on heuristic without user confirmation.

#### Tradeoff model

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

## 10. Providers — commands, parsing, and gotchas

#### Provider trait

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

#### Pacman provider

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

#### Flatpak provider

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

### Hard-won parsing notes

#### Parsing `pacman -Qi` output

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

#### Flatpak column parsing

Always request explicit columns with `--columns=`. Never parse positional output — Flatpak's default column order changes between versions.

The output is tab-separated when `--columns` is used. Handle:
- missing fields (tab-tab with nothing between)
- apps with no version set (version field is empty)
- apps installed in both user and system scope (appear as separate rows)

`flatpak info --show-metadata <app-id>` returns GLib keyfile format (INI-like). It is NOT TOML or JSON. The `[Application]` section has `name=` for the display name. Parse with a simple line-by-line approach: find `[Application]` header, then scan for `name=` key. Do not use a TOML parser on this — it will fail.

`flatpak remote-ls --updates` can be slow if remotes are unreachable. Always time it out, log a warning, and fall back to the cached update list. Never block the UI waiting for a remote.

> **Superseded (2026-07-12).** The timeout is not `tokio::time::timeout` — `SystemCommandRunner` is sync. It spawns with piped stdio, drains on threads, and polls `try_wait`, killing past `scan.provider_timeout_secs`. It applies to *every* provider command, not just `remote-ls`. See §7.

#### Dep graph from `pacman -Qi` (not pactree)

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

#### petgraph practical notes

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

#### Atomic cache writes

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

#### Overlap false positives

The reverse DNS heuristic will produce false positives. Known patterns:

**Generic names that match unrelated pacman packages:**
- `io.elementary.files` → `files` — could match anything named `files`
- `org.gnome.Shell.Extensions.GSConnect` → `gsconnect` — only if a pacman package with that exact name exists

**Mitigation:** maintain a blocklist of generic pacman names that should never appear as overlap targets: `base`, `linux`, `files`, `core`, `extra`, `lib`, `utils`, `man`, `docs`. Ship this in the binary, not in config.

**General rule:** a missed overlap (false negative) is better than a wrong match (false positive). Start conservative. Expand the known map over time based on user reports.

#### Flatpak profile paths

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

#### pacman db lock detection

Before any pacman operation, check for `/var/lib/pacman/db.lck`:

```rust
if Path::new("/var/lib/pacman/db.lck").exists() {
    return Err(anyhow!("pacman database is locked. Another instance may be running. \
        If no other pacman process is active, remove /var/lib/pacman/db.lck"));
}
```

Show this as a specific, actionable error — not a generic "pacman failed."

---

### AUR packages

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

### Known fragile points

**pacman -Qi format.** Stable for years but no API contract. Test against fixtures. Run CI. When pacman updates, re-capture fixtures from a real system and verify.

**flatpak --columns output.** Supported but column names can change. Pin to the exact column names used in the spec. If Flatpak adds/removes a column name, the parser must handle it gracefully (skip unknown columns, warn on missing expected columns).

**sudo credential timeout.** Varies by system. `timestamp_timeout = 0` means every sudo call prompts. `NOPASSWD` means no prompt at all. Document both in README.

**TOML cache size.** For a system with ~2000 packages, the TOML cache will be 2-5MB. This is fine for now. If it becomes a bottleneck (measure first), switch to MessagePack.

**Flatpak remote availability.** Campus networks, VPNs, and firewalls can block Flatpak remotes. Always timeout remote calls. Always fall back to cached data. Never block the UI on network.

---

## 11. Privilege, logging, and errors

#### Principle

paclens runs as the user. It only escalates privileges when executing a pacman update. It never stores credentials. It never runs a background privileged process.

#### Escalation mechanism

For TUI mode (v0.0.6 through v0.1+):
1. paclens suspends the TUI (`LeaveAlternateScreen`)
2. shows the user the exact command that will run
3. spawns the command (which may include `sudo`) in the raw terminal
4. user interacts with sudo prompt and pacman directly
5. command completes, paclens restores the TUI (`EnterAlternateScreen`)
6. result (exit code) shown in TUI

See open question Q6 for discussion on `--noconfirm`.

#### Flatpak

User-scope Flatpak updates: no sudo needed. System-scope: needs sudo (same escalation model). Scope detected from `installation` column.

#### Detecting privilege tool

Check in order: `sudo`, `doas`, `pkexec`. Use the first one found. If none available: show error, do not proceed with privileged operations.

#### What paclens never does

- caches sudo credentials between sessions
- stores passwords
- runs as a daemon with elevated privileges
- uses setuid or capabilities

---

#### Location

```
~/.local/share/paclens/logs/paclens-YYYY-MM-DD-HHMMSS.log
```

Timestamp in filename prevents collisions if run multiple times per day.

#### Log levels

| Level | When used |
|---|---|
| ERROR | unrecoverable errors, provider failures |
| WARN | recoverable issues, unexpected output, heuristic fallbacks |
| INFO | scan start/end, update execution, key events |
| DEBUG | raw command output, parse steps, cache operations |

Controlled by `config.general.log_level` or `--debug` flag (`--debug` sets DEBUG).

#### Rotation

Keep `config.general.log_keep_count` most recent files (default: 10). Delete older on startup.

#### Update log format

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

#### Rules

- no `unwrap()` or `expect()` in production paths — `#![deny(clippy::unwrap_used)]`
- `anyhow::Result` for fallible functions, `thiserror` for provider-specific error types
- every user-visible error has a human-readable message and a "what happens next" line
- every error logged at appropriate level
- provider errors are isolated: one failure does not abort others

#### User-visible error format

```
error: pacman scan failed
  pacman exited with code 1
  stderr: error: could not open database
  → paclens will continue without pacman data
```

#### Recovery table

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

## 12. Testing

#### Unit tests

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

#### Integration tests

Mock `CommandRunner` returns fixture data. Test the full scan → cache → analyzer pipeline end-to-end.

#### Not tested in CI

- TUI rendering (manual)
- actual pacman/Flatpak execution (requires real system)
- sudo behavior

#### CI pipeline

GitHub Actions. Stable Rust only.

```
cargo fmt --check
cargo clippy -- -D warnings -D clippy::unwrap_used
cargo test
cargo build --release
```

---

#### Fixture structure

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

#### Injectable CommandRunner

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

## 13. Decisions log

Append-only, dated, newest last. Nothing here is edited after the fact — a
decision that was later reversed gets a new entry saying so, not a rewrite.

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

2026-08-24 | roadmap.md retired; GitHub issues are the only plan
           | every forward-looking item in it now has an issue (#1-#45),
           | including the ones that had quietly never shipped: the ? help
           | overlay from the v0.1.6 polish pass, interactive cleanup, the
           | app-grouping database, the plugin-system question, and the AUR
           | and crates.io publishing steps outstanding since v0.2.0. What
           | the file uniquely held was history — per-milestone deliverables
           | and done-when criteria — and that is summarized in CLAUDE.md's
           | shipped table and preserved in git. Two planning surfaces is
           | how this one went stale: it still described v0.3-v0.5 as
           | uncommitted months after they shipped. Source comments that
           | cite "roadmap vX.Y" stay as provenance — they resolve against
           | the shipped table. Do not recreate the file.

2026-08-24 | milestones renumbered; 0.3.0 is the first tagged release
           | the old scheme mixed granularities: 0.1.0-0.1.5 took a patch
           | bump per feature, then 0.2.0-0.5.0 took a whole minor each for
           | the same size of work, so the number said more about when a
           | milestone happened than how much it carried. The original
           | v0.0.1-v0.4.0 tags existed on the remote but were never
           | published to the AUR or crates.io, so nothing outside this
           | repo consumed them; they were deleted rather than left to
           | contradict the new numbering (user decision, since moving
           | published refs is otherwise off the table).
           | Regrouped into three capability blocks — 0.1.0 see,
           | 0.2.0 understand, 0.3.0 extend — which is chronological and
           | thematic at once. Going forward a minor is a capability you
           | can point at and a patch is fixes; unshipped work gets no
           | number at all, since pre-assigning them is what drifted last
           | time. Old labels in commits, decision entries above and source
           | comments are left alone as contemporaneous record; CLAUDE.md
           | carries the mapping forward.
```

---

## 14. Amending this

Rules move to defaults, and defaults become rules, when the evidence changes.
That is fine — it is what the decisions log in §13 is for. What is not fine is a rule
quietly eroding because it was inconvenient one afternoon.

So: changing anything here is a deliberate act. Record it, date it, and say what
changed your mind.
