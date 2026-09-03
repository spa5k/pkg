# Design: CLI user experience

## Context

The CLI (`crates/pkg-cli`) fronts the installer and channel machinery. It currently optimizes
for proof harnesses: machine-first, silent during long waits, errors designed for log grepping.
This change flips the default audience to humans while keeping the machine mode first-class.

## Decisions

### D1. Registry, not i18n framework

A single `ux::Text` module with `const` entries keyed by stable identifiers. An i18n framework
is overkill for one language and adds a dependency; the registry gives rename-readiness and
golden-testability at zero cost. If translation ever arrives, the registry is the seam.

### D2. Error codes as an enum, not magic numbers

`UxErrorCode` enum in `pkg-core` so installer and channel crates can map their failure classes
without depending on the CLI crate. Codes are documented constants; adding a code is a
documented, breaking-adjacent change requiring a docs PR.

### D3. Plain phase lines, never spinners

Phase lines (`[install] determinate handoff — 3.2s`) are greppable, pipe-safe, and testable
with golden output. Spinners fail all three. Elapsed time comes from the injected clock (see
the deterministic-verification change), so golden tests stay stable.

### D4. Doctor reuses preflight, adds nothing new

`doctor` is a read-only composition of existing checks: privilege/os/arch preflight, channel
probe, TUF root comparison, journal-vs-filesystem consistency. No new check logic is written
in the CLI; wiring only. This keeps doctor honest by construction — it cannot report a
check the installer does not actually enforce.

### D5. Confirmation default: ask interactively, refuse silently

Interactive confirmation reads from the TTY directly (not stdin) so pipes cannot answer it.
Non-interactive without `--yes` refuses — safe default; scripts opt in explicitly.

## Risks / Trade-offs

- **Golden help tests churn during active development.** Accepted; churn is the point —
  surface changes become visible diffs.
- **Exit-code freeze too early.** Mitigated by marking the code table "stable as of alpha.10"
  and gating with the docs reference; codes introduced before that mark may consolidate.
- **Registry indirection cost.** One extra match per printed line is noise; compile-time
  exhaustiveness is the real win (a missing identifier fails the build).

## Open Questions

- Should `--json` also carry phase progress events (one JSON object per line)? Proposal: yes,
  as `{"type":"phase","name":...}` on stderr, so stdout stays a single result object.
