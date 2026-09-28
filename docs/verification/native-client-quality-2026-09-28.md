# Native client quality — 2026-09-28

Status: implemented and verified. This is a structural quality follow-up
on the native client recorded in
[native-nix-2026-09-28](native-nix-2026-09-28.md). Client
`0.2.0-alpha.1` stays an unreleased candidate. Toolchain checks passed
in the Linux sandbox and on the parent's macOS host. Native
verification passed on both systems, and both independent final
reviews are recorded below. The code was authored by E2B GLM 5.3 and
reviewed and verified by the parent manager.

- Baseline: local commit `a1dc324` (parent branch
  `feat/native-nix-openspec-plan`), the implemented
  [simplify-to-native-nix](../../openspec/changes/simplify-to-native-nix/proposal.md)
  client.
- Scope: [simplify-native-client-internals](../../openspec/changes/simplify-native-client-internals/proposal.md).

## Final structure

`crates/pkg-cli/src` after the change:

- `nix/` — `mod` (adapter facade: one method per native operation),
  `error` (failure modes), `process` (one signal forward/reap boundary
  and one shared outcome for every external child), `manifest` (decoded
  native output shapes), `reference` (pure reference rules with one
  exact full-commit extractor).
- `catalog/` — `mod` (shared catalog error), `id` (parse, validate,
  normalize IDs), `search` (discovery with provenance, exact lookups),
  `cask` (support classification, platform rule), `cache` (disposable
  derived data).
- `commands/` — `mod` (dispatch, session, the shared runtime setup with
  `--verbose`/`--no-color`, the one error-to-exit conversion), `query`
  (search, info, list), `lifecycle` (install, remove, update, upgrade,
  history, rollback, prune), `doctor`, `support` (shellenv, completion,
  apps sync).
- `apps.rs` (one helper-profile verifier), `config.rs`, `output.rs`,
  `cli.rs`, `lib.rs`, `main.rs`.

Duplication removed: one catalog routing point
(`CatalogId::source_and_attribute`), one prefix-strip core, one search
row builder for all three cache outcomes, cask revision attached at
classification, one skipped-platform report, one doctor row literal,
one completion call, one full-commit extractor, one process outcome and
classifier, one command error conversion, one helper-profile verifier.
No generic backend, no new crates, no new dependencies.

## Counts, honestly

Production code: non-blank lines not starting `//`, before the first
`#[cfg(test)]`, per file (counting rule fixed before measuring; not a
parser-derived executable-LOC metric).

| Area | Baseline | Final |
| --- | --- | --- |
| nix | 743 | 714 |
| catalog | 748 | 756 |
| commands | 1024 | 971 |
| apps, config, output, cli, lib, main | 818 | 815 |
| **Total** | **3333** | **3256** |

Production code is 77 lines smaller (2.3 percent). This is a modest
reduction from real duplication removal; there was no count target.

Physical source size grew: 5204 → 5500 lines (+296). The growth is the
added regression tests and per-module documentation. Both numbers are
stated so neither hides the other. The largest file shrank from 1352
lines (the old `nix.rs`) to 652 (`apps.rs`); the next largest are
`catalog/search.rs` (628), `nix/mod.rs` (537), `commands/query.rs`
(484), and `nix/manifest.rs` (459).

## Tests

Before: 32 tests (25 unit, 7 CLI). Final: 36 tests (28 unit, 8 CLI) —
the original 32, plus three subprocess regressions (discovery capture,
captured/streamed classification, direct-command classification) and
one doctor flag regression (`--verbose` trace, `--no-color` NO_COLOR
forwarding; environment-isolated, valid empty profile manifest for the
macOS read-only apps path).

## Checks passed

Linux sandbox (Rust 1.96.1, x86_64, no Nix runtime):

- `cargo build --locked --all-targets`, `cargo test --locked` (36/36),
  `cargo fmt --all --check`,
  `cargo clippy --locked --all-targets -- -D warnings`,
  `RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps`,
  and the repository link checker.
- Independent 32-case differential run of the baseline and final
  binaries: status, stdout, and stderr identical in all 32 cases.

macOS (parent, Rust 1.96.1): fmt, clippy `-D warnings`, all 36 tests,
release build, and rustdoc with private items under `-D warnings` — all
pass. Strict `openspec validate simplify-native-client-internals
--strict` passes. Link checker: 105 files, 347 links. The parent's
36-case CLI comparison against the `a1dc324` baseline is identical in
status, stdout, and stderr (argv0 normalized to `pkg`); the 36-case and
32-case runs are separate comparisons of different case sets, both
identical where they overlap.

## Native macOS verification (parent)

macOS 15.7.7, Apple silicon VM, ordinary user uid 501, Determinate
3.22.1 / Nix 2.35.2, Homebrew moved aside. Final candidate archive:

- Fresh state: doctor and list report an empty client honestly.
- Journey: hello install and exec; info and search with provenance;
  update refreshes metadata and preserves the manifest; upgrade,
  history, exact entry remove, rollback, and prune 1d all behave as
  contracted; invalid entry IDs and `--json` on mutations are refused.
- A nonexistent source fails with the native diagnostic, the profile is
  re-read, and the manifest is unchanged.
- Casks: zoom is excluded with its recorded reason; iterm2 installs,
  the helper profile is created and verified, and owned launchers
  appear; `pkg apps sync` works.
- Launcher ownership: an unowned launcher folder makes upgrade report
  partial completion — nonzero exit, the package change completed, and
  a named `pkg apps sync` retry — and the user file inside is
  preserved; repair plus retry leaves the profile unchanged; remove
  clears launchers and rollback restores them.
- Interruption: SIGINT sent only to the pkg process during a real slow
  build exits 130, the child pid is gone, and the re-read profile is
  unchanged.
- Native `pkg doctor --verbose --no-color` passes with no inherited
  NO_COLOR.

## Native Linux verification (parent)

External runtime only: Determinate 3.22.1 / Nix 2.35.2 on x86_64
Linux, ordinary user uid 1001, daemon `trusted:false`. Final candidate
binary SHA256:
`d76cc1a8af795bdb3f932174f6f431b0201c4b7194a3836996e42b41ba3d44e2`.

One clean isolated run drove the final binary through a bounded,
controlled fixture source. No full nixpkgs search ran (the known limit
in [native-nix-2026-09-28](native-nix-2026-09-28.md) stands). The run
checked, by case:

- T1 doctor: all four native probes traced under `--verbose`, each
  carrying `--option accept-flake-config false` and `--option
  allow-import-from-derivation false`. NO_COLOR forwarding is proven by
  the CLI regression test, not by this run.
- T2 install: package lands in the dedicated pkg profile; the default
  profile stays absent.
- T3 list: `--json` reports schema 1 with native entry ids.
- T4 missing entries: remove and upgrade exit 1.
- T5 update: after a source advance, `list` output is byte-identical.
- T6 moving reference: upgrade advances the locked revision.
- T7 fixed reference: an explicit full-commit entry is reported and
  retained during `upgrade --all`.
- T8 rollback exits 0.
- T9 source failure: the native diagnostic prints, the profile is
  re-read, exit 1.
- T10 prune gating: no age exits 2, `0d` exits 1, `1d` exits 0.
- T11 cask install on Linux: refused, exit 1.
- Usage rules: `--json` on mutations exits 2; a relative XDG base exits
  1; `apps sync` on Linux exits 1.
- T12 interruption: SIGINT sent only to the pkg process during a real
  native build exits 130. The Nix child pid is gone. The profile
  re-read prints. The fresh profile is unchanged.

## Intended behavior changes

1. Search with a partially failed source no longer labels healthy rows
   stale (diagnostic text only).
2. The `display.color` config doc states that only `never` has an
   effect (documentation only).
3. Doctor forwards `--verbose` and `--no-color` to its native probes
   through the shared runtime setup (flag behavior; read-only output
   otherwise unchanged).
4. Fixed-reference classification and locked-revision extraction share
   one exact rule: `rev=<40 hex><suffix>` and `prev=<40 hex>` no longer
   count as fixed (classification behavior).
5. The macOS helper profile is verified by one verifier whether
   pre-existing or freshly installed, and the count check now also
   applies after install (verification behavior).

## Standards

Independent standards review of the final candidate against the saved
`a1dc324` baseline snapshot: **approve**. The concrete baseline
findings are addressed in code: doctor now shares the session's runtime
setup (CLI regression test included), the dead `Captured.stderr` field
is deleted, and one exact full-commit extractor serves fixed-reference
classification and locked revision. The planned restructuring was
delivered as planned: the module split, one command error boundary, one
process outcome, and the catalog dedup. No unresolved documented-
standard violation and no semantic regression remain. Four judgement
items stay open; they are style only, not defects: doctor row statuses
are strings, the apps status text prefix is parsed back by doctor, the
prune age makes a string, number, string round-trip, and `resolve_bare`
spells both sources itself.

## Spec

Independent spec review, final verdict: **no findings** on the Linux
native axis. All eight original invariants were checked on the final
binary. They are: profile isolation; fixed native flags; moving and
fixed references; exact entry prevalidation; interruption handling;
platform launcher boundaries; prune gating; and query JSON.
Source-failure handling was checked too. The scope is Linux; real macOS
checks are the parent's native record above. The initial audit found
two documentation gaps: the query JSON schema was not documented, and
historical root docs were stale. Both were fixed in this change. No
high or medium spec issues remain. Coverage is claimed only for the
checks listed here and above, not for untested surfaces.

Summary: zero unresolved findings for the reviewed scope. The
judgement items in Standards remain optional follow-ups.

## Release status

All local and native evidence for this change is complete. The quality
PR stacks on parent #77 (base `feat/native-nix-openspec-plan`; the
parent PR's CI passed) and carries this record. Its own GitHub CI
result is reported on the PR, not claimed here. The client stays an
unreleased candidate.
