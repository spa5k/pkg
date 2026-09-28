# Simplify the native client internals

A structural quality follow-up on the implemented
[simplify-to-native-nix](../simplify-to-native-nix/proposal.md) client,
28 September 2026. Baseline: local commit `a1dc324` on
`feat/native-nix-openspec-plan`.

## Why

The native client works, but its logic sits in three files above one
thousand lines each (`nix.rs`, `catalog.rs`, `commands.rs`). Each file
holds several unrelated jobs, and the same knowledge is spelled out more
than once. This makes review harder than the behavior warrants.

## What Changes

- Split the three files into cohesive modules at their natural seams.
  `nix` becomes the adapter facade, the process forward/reap boundary,
  the decoded output shapes, and the pure reference rules. `catalog`
  becomes ID parsing, discovery search with provenance, cask support
  classification, and the disposable cache. `commands` becomes the
  shared dispatch/session/failure rules plus query, lifecycle, doctor,
  and utility command modules.
- One command error shape: handlers return `Result<(), CommandError>` and
  use `?`; one place converts it to printing and exit codes. Already
  reported outcomes (mutation failures, interruptions, partial launcher
  failures) pass through without duplicated diagnostics.
- One process outcome: native children and the direct helper command are
  spawned, reaped, and classified through one shared representation.
- Remove real duplication: cask revision attachment, the output-set
  prefix strip, per-source search row building, the skipped-platform
  report, catalog source routing, the doctor row literal, and the
  per-shell completion call.
- Fix diagnosed defects:
  - The search partial-failure note no longer labels healthy rows as
    stale, and the config color field documents only its real `never`
    behavior.
  - `pkg doctor --verbose` and `--no-color` were ignored by doctor's own
    runtime setup; doctor now shares the session's runtime setup while
    staying read-only.
  - Fixed-reference detection and locked-revision extraction disagreed:
    a `rev=` value of full-commit length plus a suffix classified as
    fixed while the locked revision was rejected, and a `prev=`
    parameter counted as `rev=`. One exact extractor now serves both.
  - The macOS helper profile was verified by two drifted code paths; one
    verifier now checks the pre-existing and the freshly installed
    profile alike, entry count included.
- Add regression tests that run a real child process through discovery,
  capture, streaming, the direct path, and doctor's flag forwarding.
- Keep the change modest in size: production code shrank slightly while
  tests and module documentation grew.

## Non-goals

- No requirement changes. The contracts in
  [simplify-to-native-nix specs](../simplify-to-native-nix/proposal.md)
  are preserved unchanged; this change is a pure refactor plus the
  listed defect fixes, so it carries no spec deltas
  (`skip_specs: true`).
- No generic backend traits, no new crates, no new dependencies, no
  line-count target.
- `config.rs`, `output.rs`, and `cli.rs` keep their shape; `apps.rs`
  keeps its module but its helper-profile check became one verifier.
- The old compatibility constraints do not apply; nothing here is a
  feature.

## Impact

Only `crates/pkg-cli` internals plus the listed defect fixes. CLI exit
codes, JSON envelopes, human output, cancellation behavior, and the
macOS launcher behavior are unchanged; two CLI comparisons against the
`a1dc324` baseline (36 cases by the manager, 32 cases independently)
are identical in status, stdout, and stderr where they overlap.
Production code shrank modestly (3333 → 3256 counted lines) while the
physical source grew with tests and module documentation; the evidence
is recorded in
[docs/verification/native-client-quality-2026-09-28.md](../../../docs/verification/native-client-quality-2026-09-28.md).
Native verification passed on macOS and Linux. The final independent
standards and spec reviews recorded there found zero unresolved
findings for the reviewed scope.
