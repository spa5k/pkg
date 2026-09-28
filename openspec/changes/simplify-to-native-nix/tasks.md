# Native Nix implementation tasks

These tasks implement the final [design](design.md).
All 44 tasks are complete; the completion evidence is linked at the end.
Each numbered group was a reviewable work unit.
Keep groups in this order. The Cask source can be prepared after group 3, but
group 6 needs both the native lifecycle and the Cask outputs.

Old tests and contracts are not merge requirements for removed behavior.
Delete them with their production code. Verification below means a direct
check or delivered artifact; it does not require a new automated test for each task.
Use only a few focused tests where they catch a real defect in code we still own.

## 1. NN-01: Confirm native use and define the small application

- [x] 1.1 Record the implementation base and inspect later main changes before coding; verify that the deletion map still matches live code and note any changed responsibilities.
- [x] 1.2 Run native profile add/list/remove/history/rollback in disposable Linux x86-64 and Apple silicon macOS environments; record Nix versions, chosen profile paths, and results as one short note.
- [x] 1.3 Check that the chosen native profile and retained generations are registered GC roots; verify that a controlled collection preserves them and leaves the default profile unchanged.
- [x] 1.4 Define the internal command, output, nix, catalog, apps, and config modules in pkg-cli; verify that there is one application target and no new generic backend interface or pkg-next engine.
- [x] 1.5 Define the reduced parser and new query JSON schema; verify that help lists the D4 commands and rejects the removed alpha commands and global options.

## 2. NN-02: Replace runtime and basic package execution

- [x] 2.1 Implement external Nix discovery, absolute executable resolution, version/capability checks, and read-only doctor; verify clear output for a missing executable and unavailable daemon without starting an installer or service.
- [x] 2.2 Implement direct argument-vector process execution, source-config refusal, diagnostics, cancellation, and child reaping; check argument separation and one interrupted native command without adding a broker protocol.
- [x] 2.3 Resolve the fixed native profile/config/cache paths and reject relative XDG bases; verify isolation from default Nix and old pkg state on both systems.
- [x] 2.4 Implement explicit-installable install, list, and exact-entry remove using native profile commands; verify one complete package journey as an ordinary user.
- [x] 2.5 Decode required native profile identity fields and retain useful errors; check recorded supported output and one malformed output case without rebuilding the old adapter test framework.
- [x] 2.6 Remove the replaced command paths and their broker-specific tests as callers disappear; verify that the live basic journey no longer invokes pkg services or product build approval.

## 3. NN-03: Add discovery and source selection

- [x] 3.1 Add explicit Nixpkgs and Cask source configuration and qualified name rules; verify that an ambiguous bare name is refused and an exact qualified name selects the intended output.
- [x] 3.2 Implement native search and info metadata queries with source revision and system identity; verify representative top-level and nested attributes without a repository parser.
- [x] 3.3 Add the small disposable discovery cache and update command; verify that deleting the cache does not affect list and refreshing it does not mutate the profile.
- [x] 3.4 Preserve original moving references for installs and report the actual resolved state; verify a source change between discovery and installation using controlled revisions.
- [x] 3.5 Measure one cold and one warm search on each system and record the result; ship the native path without adding a hosted index or evaluator campaign.

## 4. NN-04: Complete the native lifecycle

- [x] 4.1 Implement upgrade for exact installed entry names and all entries; verify that moving references advance and exact revisions remain fixed.
- [x] 4.2 Implement native history and rollback with native generation identifiers; verify an upgrade and return to the retained earlier package version.
- [x] 4.3 Implement explicit-age prune through profile history removal; verify preservation of the current generation and refusal when the age is absent.
- [x] 4.4 Implement shellenv and completion for the new grammar; verify idempotent Bash/Zsh path output and completion generation without editing startup files.
- [x] 4.5 Check one output conflict, one source failure, and one interrupted mutation; verify useful errors and a native state re-read without custom recovery or silent mutation retry.
- [x] 4.6 Remove remaining custom package state and lifecycle callers; verify that installed inventory and rollback no longer read the old manifests, pin state, journals, or root ledger.

## 5. NN-05: Add the Cask source as Nix packages

- [x] 5.1 Create nix/casks/flake.nix with pinned brew-nix, Nixpkgs, and explicitly selected metadata inputs; commit the complete lock and verify evaluation with import-from-derivation disabled.
- [x] 5.2 Export the supported package set and same-revision support metadata; verify that native installers, missing hashes, and unsupported variants have explicit exclusion reasons.
- [x] 5.3 Select and realize three representative outputs: app archive, executable, and app with CLI; record the exact tokens/revisions and verify package contents without Brew.
- [x] 5.4 Connect cask-qualified names to the same native install path; verify list, source identity, upgrade, and removal with no Cask-specific installer.
- [x] 5.5 Document the lock-update procedure and initial support list; verify that a newer outer source revision can be selected by native profile upgrade without rewriting installed original references.

## 6. NN-06: Add macOS app exposure

- [x] 6.1 Select and pin the mac-app-util helper package in a separate native helper profile; verify its commands and distribution notices and confirm that user package rollback does not remove the helper.
- [x] 6.2 Implement active-output discovery and the dedicated pkg-owned launcher folder; verify intact bundle targets and refusal to replace an unowned destination.
- [x] 6.3 Add launcher refresh after install, remove, upgrade, and rollback plus apps sync; verify that each operation targets the app from the active native profile.
- [x] 6.4 Report launcher failure as partial completion with a nonzero result and a sync retry; verify that retry only rebuilds launchers and leaves package selection unchanged.
- [x] 6.5 Check the accepted apps through their applicable Finder, Spotlight, Dock, CLI, and new-login paths; remove unsupported candidates from the support list rather than adding installer or Brew fallbacks.

## 7. NN-07: Delete old code, tests, and documentation contracts

- [x] 7.1 Delete obsolete pkg-store, pkg-pipeline, and pkg-core code after their callers are replaced; verify the workspace graph has no product generation, approval, journal, activation, or root-publication engine.
- [x] 7.2 Delete pkg-installer, old pkg-nix runtime/privilege code, pkg-macos-security helper code, and service assets; verify no build target emits pkg-install, pkg-nix-broker, or pkg-root-helper.
- [x] 7.3 Delete pkg-channel, old pkg-index/pkg-resolver code, and the fake pkg-testkit engine; verify only native realization and small catalog routing remain.
- [x] 7.4 Delete obsolete tests, fixtures, mutation cases, proof jobs, and quality baselines with their subjects; verify surviving CI checks the new code without requiring the removed alpha contracts.
- [x] 7.5 Remove unused workspace members and dependencies and refresh Cargo.lock; verify build, formatting, lints, and dependency checks for the remaining targets using the existing pinned toolchain.
- [x] 7.6 Rewrite README, command/install docs, and relevant ADRs for the new behavior; verify no active document promises old state migration, build approval, product repair, privileged setup, or universal Cask coverage.
- [x] 7.7 Replace stale contribution rules and plan-link invariants with this OpenSpec workflow; verify docs-linkcheck targets the current design and old proposals stay marked superseded rather than delivered.

## 8. NN-08: Deliver the standalone client

- [x] 8.1 Replace custom release/channel assembly with client archives, completions, notices, and checksums; inspect archive contents to confirm there are no privileged helpers, private Nix bundles, or product catalog assets.
- [x] 8.2 Replace the all-in-one install entry with client-only setup and external vendor instructions; verify it cannot install Brew, change Nix trust, or perform machine teardown.
- [x] 8.3 Install the actual release archive on clean Linux x86-64 and Apple silicon macOS hosts with external Nix; verify the native package journey and macOS app subset without development files or old services.
- [x] 8.4 Replace and remove only the client on those hosts; verify the dedicated package profile and external runtime remain intact.
- [x] 8.5 Record supported runtime versions, source revisions, app limits, and the short completion evidence; verify the final docs match the artifact and report any unsupported cases without claiming old proof coverage.

## Completion evidence

Evidence: [docs/verification/native-nix-2026-09-28.md](../../../docs/verification/native-nix-2026-09-28.md).
All 44 tasks are complete, including the final archive pass (8.3, 8.5).
Known limits are recorded there, not as open tasks.
