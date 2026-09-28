# Simplify pkg to native Nix

Final implementation plan, 28 September 2026. Implementation is
complete and merged to `main` (PR #77, PR #78). The release is
`v0.2.0-alpha.1`.
Baseline: fetched origin/main at `5256cfa5a052f64ae4219ac1aaf316a7a00d49a2`
(alpha.58). The user accepts breaking changes and removal of old tests.

## Why

pkg still owns a broker, root helper, package state engine, build approval
protocol, and signed catalog despite using Determinate Nix. Remove these
responsibilities and make pkg a small command interface over native Nix.

## What Changes

- **BREAKING:** Require an external Determinate Nix installation. Remove pkg's
  installation, update, repair, and removal of the Nix runtime.
- **BREAKING:** Use one native Nix profile per user for installed state,
  generations, activation, rollback, and roots. Delete the custom equivalents.
- **BREAKING:** Replace the old CLI contracts. Remove exact build approval,
  global dry-run, pin/unpin, outdated, product GC, repair, system uninstall,
  old progress JSONL, per-file conflict policy, and old exit-code guarantees.
- Keep package discovery, source selection, install, list, remove, update,
  upgrade, history, rollback, explicit-age pruning, doctor, shellenv, and
  completion. Use a small concrete Nix adapter.
- Convert compatible Homebrew Casks to ordinary Nix packages through a locked
  product flake and brew-nix. Never install or invoke Brew on the client.
- Expose intact macOS app bundles through user-owned launchers. Start with
  a small supported set.
- **BREAKING:** Start with fresh state. Do not migrate, adopt, or automatically
  erase old installations.
- Delete obsolete code, tests, release jobs, fixtures, and process rules.
  Keep a few direct checks for the new behavior. Do not build another
  verification framework.

## Capabilities

### New Capabilities

- `external-nix-runtime`: Ordinary-user use of an externally managed runtime.
- `native-profile-lifecycle`: Native package state and the reduced command contract.
- `package-discovery`: Source-qualified names and disposable discovery data.
- `cask-packages`: A compatible Cask subset installed through Nix.
- `macos-app-exposure`: Derived app launchers with a limited ownership scope.
- `standalone-client-delivery`: A small release with a clean state start.

### Modified Capabilities

None. The repository has no main specifications under openspec/specs.
The two existing changes describe unfinished work, not accepted main specs.
This change therefore adds the new contract. Removed alpha behavior is explicit
in this proposal and the design; there are no invented REMOVED base requirements.

## Impact

Consolidate application logic in `crates/pkg-cli`. Remove the other product
crates when their live callers disappear. Replace the custom release pipeline.
Keep Linux x86-64 and Apple silicon macOS as the supported systems.

This plan supersedes the future backlog in the
[old plan](../../../plans/determinate-nix-stacked-prs.md),
[verification proposal](../add-deterministic-verification-suite/proposal.md),
and [trust proposal](../execute-production-trust-ceremony/proposal.md).
Retain their history. Do not complete or archive them as delivered.

The [design](design.md) defines decisions and deletion scope.
The [tasks](tasks.md) define ordered implementation work.
The [HTML view](../../../artifacts/pkg-simplification-plan.html) is a reading
copy. OpenSpec is authoritative.
