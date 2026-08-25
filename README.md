# paclens

A TUI-first **pacman + AUR + Flatpak** inspection and update tool for **Arch Linux**.

paclens unifies your two package worlds into one dashboard: update both from
one place, ask *why* anything is installed and what removing it would break,
spot apps you have installed both natively **and** as a Flatpak, and see what
orphans and caches are eating your disk — all without paclens ever touching
your system uninvited.

```
+ paclens · dashboard -------------------------------------------------------+
| + sources ---------------------++ pending updates · pacman (62) ----------+|
| |       SOURCE       INST  UPD || linux    6.9.1  -> 6.9.2                ||
| | > [x] pacman       1832   62 || mesa     24.0   -> 24.1                 ||
| |    -  flatpak-user   12    0 || ...                                     ||
| +------------------------------++-----------------------------------------+|
| + system ----------------------++ keys -----------------------------------+|
| | - plan          62 packages  || ^/v move   ←/→ pane   i packages        ||
| | - pacman cache  7.6 GiB      || space toggle   enter update   r refresh ||
| | - orphans       3            || o overlaps   c cleanup   L log   q quit ||
| +------------------------------++-----------------------------------------+|
+----------------------------------------------------------------------------+
```

paclens is **not** a package manager. It wraps `pacman` and `flatpak`, reads
their state, and presents it. Updates run the real commands in a terminal
emulated inside the TUI — sudo and pacman prompt you exactly as they would in
your shell. Everything else is advisory: paclens never removes anything.

## Features

- **One dashboard** — sources, pending updates, cache/orphan/overlap counts,
  and the update plan, live on one screen.
- **Accurate update detection** — `checkupdates` (pacman-contrib) so counts
  are right even with a stale sync DB; falls back to `pacman -Qu` with a
  staleness warning. Flatpak apps *and* runtimes. AUR packages via `paru`
  (their own source row; `paru -Sua` runs unprivileged in the console), with
  optional `--devel` checks for `-git` packages.
- **Updates in a real terminal** — the update runs on a pty inside the TUI:
  sudo's password prompt, pacman's questions, colors and progress bars all
  work untouched. Ctrl-C interrupts the command, not the TUI.
- **`why` inspector** — install reason, reverse-dependency chain as a tree
  with per-edge confidence labels, what a removal would break or orphan, and
  a cautious verdict. Works for pacman, AUR (with PKGBUILD/VCS caveats),
  Flatpak apps and runtimes.
- **Overlap detector** — finds apps installed both natively and as Flatpaks
  (curated map → reverse-DNS → display name, each with a decreasing
  confidence label) and shows a side-by-side tradeoff: versions, profile
  locations and sizes, sandboxing, integration, which install is likely the
  one you actually use.
- **Migration advisory, then execution** — for each overlap, where both sides
  keep their config/data/cache (curated map at full confidence, XDG-convention
  guesses labeled as such), which direction consolidation would go, and the
  exact steps. `x` runs the copy: existing targets are staged into a
  timestamped backup directory first, the plan contains no `rm` anywhere, and
  the rollback commands are printed when it finishes. The source side is
  removed only after you launch the app and confirm it works — a separate,
  deliberate `R`.
- **Cleanup report** — orphan candidates derived from the dependency graph
  (no `pacman -Qtd`), unused Flatpak runtimes, and cache sizes with an
  *honest* reclaimable figure: an 11 GiB pacman cache that `paccache -rk2`
  would free nothing from says exactly that, rather than implying the total is
  cleanable. The AUR build cache (`~/.cache/paru`) gets its own row. Every
  suggestion is text for *you* to run.
- **Honest confidence** — every inference is labeled `confirmed`, `inferred`,
  or `unknown`. paclens never presents a guess as a fact.
- **Fast** — providers scan in parallel; the TUI opens instantly on the
  cached scan and refreshes in the background.

## Install

From the AUR (once published):

```sh
paru -S paclens
```

From source:

```sh
git clone https://github.com/plasmaDestroyer/paclens
cd paclens
cargo install --path . --locked
```

Optional dependencies: `pacman-contrib` (for `checkupdates` — accurate
update counts without touching your sync DB) and `paru` (AUR update
detection and updates; foreign packages still list without it).

## Usage

`paclens` opens the TUI. Keys:

| Key | Where | Action |
|---|---|---|
| `↑/↓` `j/k` | everywhere | move |
| `←/→` `h/l` | dashboard | switch pane focus |
| `space` | dashboard | toggle a source in/out of the update plan |
| `enter` / `u` | dashboard | run the update (pty console; sudo/pacman prompt as usual) |
| `i` | dashboard | open the selected source's package list |
| `o` / `c` | dashboard | overlap screen / cleanup screen |
| `r` / `L` | dashboard | refresh scan / view the update log |
| `/` | package list | fuzzy filter |
| `s` | package list | cycle sort: updates → reason → name → size |
| `w` | package list | why pane for the selected package |
| `enter` | overlaps | migration report for the selected overlap |
| `d` | overlaps | flip the migration direction |
| `x` | overlaps | run the migration copy — backup first, unprivileged |
| `R` | overlaps | remove the source side — armed only after a clean copy |
| `enter` | cleanup | why report for the selected orphan |
| `esc` | everywhere | back / unwind |
| `q` | everywhere | quit |

Everything is also available headless:

```sh
paclens status            # dashboard summary to stdout
paclens update            # update all sources (asks y/N first)
paclens why firefox       # why is this installed, what breaks without it
paclens overlaps          # Flatpak/native duplicates with tradeoffs
paclens migrate firefox         # where both sides keep data + the steps
paclens migrate firefox --run   # run the copy: plan, y/N, backup, rollback block
```

(The cleanup report is TUI-only for now — press `c` on the dashboard.)

`--refresh` forces a re-scan, `--no-color` gives plain ASCII output,
`--config <path>` uses an alternate config.

## Configuration

`~/.config/paclens/config.toml`, created with defaults on first run. See
[`config.default.toml`](config.default.toml) for the full annotated schema:
cache TTL, source toggles, provider timeout, why depth, overlap ignore list
and extra mappings, orphan ignore list, color theme, log level.

## Design principles

1. **Explain before acting.** Nothing runs without showing exactly what will.
2. **Safety over aggression.** When in doubt, do nothing.
3. **Honest confidence.** Inference is labeled, never promoted.
4. **The pipeline.** scan → analyze → plan → confirm → execute. No "fix all" button.
5. **One source of truth.** Every view reads from the same scan.

[`design.md`](design.md) is the full statement — what paclens will and will not
do to your system, and the reasoning behind every rule. Worth reading before you
let anything near your package manager.

## License

MIT — see [LICENSE](LICENSE).
