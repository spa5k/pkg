# Design: simplify the native client internals

Context: [proposal](proposal.md). The active product design stays
[simplify-to-native-nix](../simplify-to-native-nix/design.md). This
design changes structure only; it re-decides nothing about product
behavior.

## D1 — Module seams follow ownership, not size

A module is split only when it holds two jobs with different owners or
different change reasons. The splits chosen:

- `nix/`: `error` (failure modes), `process` (signal forward/reap
  boundary for every external child), `manifest` (decoded native output
  shapes), `reference` (pure reference rules), and the facade `mod`
  (process construction and one operation per native command).
- `catalog/`: `id` (parse, validate, normalize user IDs), `search`
  (discovery queries with provenance and exact lookups), `cask` (support
  classification and the platform rule), `cache` (disposable derived
  data), and `mod` (the error type shared by the four).
- `commands/`: `mod` (dispatch, session, failure-reporting rules),
  `query` (search, info, list), `lifecycle` (install, remove, update,
  upgrade, history, rollback, prune), `doctor`, `support` (shellenv,
  completion, apps sync).

`apps.rs` is not split. It is one ownership story (destination,
helper, scan) at an acceptable size; a split would only move code.

## D2 — One process outcome, one classifier

Every external child — native Nix commands and the macOS app helper —
runs through one boundary: signals are forwarded, the child is reaped,
and the run is classified exactly once. `Reaped` holds the child's
complete output record plus the forwarded-signal record; inherited
streams leave the buffers empty, so captured and streamed children share
one shape. `Reaped::classify` produces the one shared `Outcome`
(success, interrupted, failed). Native commands wrap it into `NixError`
with their argument vector; direct helper commands return it as is.
There is no process framework and no second representation.

## D3 — One command error shape

Handlers return `Result<(), CommandError>`. `Message` carries a not yet
printed diagnostic; the runner prints it once with the `pkg:` prefix and
exits 1. `Reported` carries an exit status for outcomes the handler has
already fully printed — mutation failures with re-read native state,
interruptions (exit 130), the partial launcher failure, and source
failures in search and update. Small `From` conversions for Nix,
catalog, config, and string errors make `?` read as one line. This
replaces the per-handler session/let-else/return-exit and
match-result/fail patterns; parse-before-runtime ordering for info,
install, and prune stays explicit in the handlers.

## D4 — Duplication removed only where one place is provably better

- Catalog routing lives once in `CatalogId::source_and_attribute`;
  `installable` and `info` both use it.
- The output-set prefix strip has one core; the exposed attribute and
  the any-system strip are two calls of it.
- Search rows are built in one place; the fresh, live, and stale cache
  outcomes converge before row building.
- Cask classification takes the source revision as given; the resolved
  decision no longer rebuilds each variant to attach it.
- The skipped-platform report, the configured-source pair, and the
  doctor row literal each have one constructor next to their data.

## D5 — Behavior is pinned by differential checks

Two CLI comparisons against the `a1dc324` baseline — 36 cases run by
the manager and 32 cases run independently — are identical in status,
stdout, and stderr where they overlap (argv0 normalized). The intended
changes beyond them: the search partial-failure note, the config color
doc, doctor's `--verbose`/`--no-color` forwarding, the exact `rev=`
full-commit rule, and the unified helper verification. Native
verification of the final candidate passed on macOS (core journey,
casks, launcher ownership, interruption) and Linux (doctor flags,
profile isolation, list JSON, missing-entry refusals, update and
upgrade semantics, rollback, source failure, prune gating, cask
refusal, SIGINT interruption); see the verification record. The final
independent standards and spec reviews found zero unresolved findings
for the reviewed scope.
