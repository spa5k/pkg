# Simplify native client internals tasks

Scope: [proposal](proposal.md) and [design](design.md). No OpenSpec
requirements change (`skip_specs: true`); strict validation passes.
All tasks are complete. The completion evidence, the native Linux and
macOS verification, and the final independent standards and spec
reviews are recorded in
[docs/verification/native-client-quality-2026-09-28.md](../../../docs/verification/native-client-quality-2026-09-28.md).

- [x] 1.1 Audit all Rust, installer, release, scripts, and CI for
  structure and duplication opportunities; record them with file,
  function, and benefit, separated into correctness and style.
- [x] 1.2 Split `nix.rs` into `error`, `process`, `manifest`,
  `reference`, and the adapter facade; keep every native operation's
  arguments and diagnostics unchanged.
- [x] 1.3 Unify the process outcome: one reaped record and one
  classifier for captured, streamed, and direct children; delete the
  duplicate outcome types.
- [x] 1.4 Add subprocess regression tests through real discovery,
  capture, streamed, and direct runs of a temporary executable.
- [x] 2.1 Split `catalog.rs` into `id`, `search`, `cask`, `cache`, and
  the shared error module.
- [x] 2.2 Consolidate catalog duplication: source routing, prefix
  strip, search row building, cask revision attachment, skipped-platform
  report, Nix-to-catalog error conversion.
- [x] 3.1 Split `commands.rs` into dispatch/session, query, lifecycle,
  doctor, and utility modules.
- [x] 3.2 Introduce the one command error shape with `?` propagation and
  a single printing/exit conversion point; keep reported mutation,
  interruption, and launcher-partial outcomes pass-through.
- [x] 3.3 Fix the search partial-failure note so healthy rows are not
  labeled stale; fix the config color field doc to its real behavior.
- [x] 3.4 Follow-up review fixes: doctor forwards `--verbose` and
  `--no-color` through the shared runtime setup; one exact full-commit
  extractor serves fixed classification and locked revision; one helper
  verifier checks the pre-existing and freshly installed profile alike;
  the three refactor-only catalog seam tests were removed with their
  baseline coverage kept in the original test.
- [x] 4.1 Verify: build, test, fmt, clippy, docs, and link checks on
  Linux and macOS; strict `openspec validate --strict`; two identical
  CLI comparisons against the baseline (36 manager cases, 32
  independent); native macOS verification of the final candidate.
- [x] 4.2 Close out: final native Linux verification of the final
  binary (clean isolated run through a bounded controlled fixture; SHA
  256 recorded; no full nixpkgs search); final independent standards
  review (approve; four optional judgement items recorded) and spec
  review (no findings on the Linux native axis; two documentation gaps
  fixed); zero unresolved findings for the reviewed scope.
