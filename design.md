# paclens — design

What this tool will and will not do to your system, and why.

This document outranks the others. Where `spec.md`, `CLAUDE.md`, or a comment in
the code disagrees with it, this wins and the other one is wrong. `dev-notes.md`
§7 records *when* each rule below was decided and what it cost; this records
*what* the rules are.

---

## The test

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

## Principles

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

## Rules

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

## Defaults, not rules

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

## Not in scope

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

## Amending this

Rules move to defaults, and defaults become rules, when the evidence changes.
That is fine — it is what `dev-notes.md` §7 is for. What is not fine is a rule
quietly eroding because it was inconvenient one afternoon.

So: changing anything here is a deliberate act. Record it, date it, and say what
changed your mind.
