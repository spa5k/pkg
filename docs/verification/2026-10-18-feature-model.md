# Feature model implementation

Date: 2026-10-18. Source base: `3304f28`.

The [architecture](../architecture.md#the-nouns) defines code ownership for pkg.
The client now groups Profile, Discovery, Taps, and Apps in feature directories.

## Change

| Previous owner | Current owner |
| --- | --- |
| `commands/lifecycle.rs`, `change.rs`, `list.rs`, and `conflict.rs` | `profile/` commands, typed requests, queries, and conflict reports |
| Profile reads and mutation reports in `commands/mod.rs` | `profile/query.rs` and `mod.rs` |
| `installed.rs` and the held profile lease in `profile_lock.rs` | `profile/installed.rs` and `lock.rs` |
| `catalog/`, `commands/search/`, `info.rs`, and `resolution.rs` | `discovery/` catalog data, search, information, and resolution |
| Catalog preparation in `commands/maintenance.rs` | `discovery/refresh.rs` |
| `commands/tap/` | `tap/command/` approval, staging, validation, and publication |
| Tap state in command dispatch and doctor | `tap/query.rs` registry snapshots, lazy catalog reads, and health |
| App commands in `commands/support.rs` and the profile reaction in dispatch | `apps/command.rs` |

Command dispatch calls each feature interface.
Setup and update compose Discovery and Tap commands under the existing refresh lock.
Profile requests one resolved install batch from Discovery.
Taps owns the withdrawn-source approval check.
Apps owns launcher commands and the reaction to a completed profile change.
That reaction still requires the held profile lease.
Apps reads installed entries through Profile, which calls Nix directly.
List and information retain their existing empty-profile handling.
The public `pkg_cli::catalog` import remains available through a re-export.

The isolated reporting test uses its new module path.
Saved catalog tests now exercise the Tap query interface.
A compiled CLI test checks list and history against an existing native generation.

## Checks

Host: Apple silicon macOS. Rust: `1.96.1`.
Runtime: Determinate Nix `3.22.1`, Nix `2.35.2`.

| Check | Result |
| --- | --- |
| `just check` | Passed: formatting, Clippy, workspace build, workspace tests, and private documentation with warnings denied. |
| `cargo deny check` | Passed: advisories, bans, licenses, and sources. It reports the existing duplicate `syn` warning. |
| `cargo test --locked -p pkg-cli --lib discovery::catalog::native::tests::native_flake_discovery -- --ignored` | Passed all four native Discovery tests that the normal suite skips. |
| Compiled CLI and app tests | Passed: read-only profile queries, source approval, mutation ordering, app health, launcher publication, and child cancellation. |
| `python3 tools/native-cli-check.py --pkg target/debug/pkg --evidence-dir /tmp/pkg-feature-model-native-20261018-02` | Passed with real Nix in an isolated home and native profile, including macOS helper startup, launcher synchronization, and removal. |
| `python3 .github/scripts/check_docs_links.py` | Passed. |
| `git diff --check` | Passed. |

The workspace log is `/tmp/pkg-feature-model-check-final.log`.
The native Discovery log is `/tmp/pkg-feature-model-native-discovery.log`.
The native lifecycle log is `/tmp/pkg-feature-model-native-final.log`.
The evidence directory retains command results in `results.json` and the isolated
home, source fixtures, and native profiles.

The native lifecycle check covers failed batch installation, failed upgrade and retry,
selected and previous rollback, concurrent commands, waiter cancellation,
duplicate installation, Formula and Cask source rules, generation retention,
expired generation removal, and macOS launcher creation and removal.

## Navigation and limits

TraceDecay returned fresh, partial impact evidence for the mutation entry point
and the former Tap state type. It identified lifecycle handlers, dispatch,
resolution, and lazy catalog reads.
Its static test map was unavailable because caller expansion exceeded its budget.
Direct reads and literal searches checked imports, CLI dispatch, fixture paths,
and the subprocess test name. The build and executed tests check the moved code.

Preview skipped: the operations were file moves and uniquely anchored extractions.
Reconciliation skipped: all edits completed without interruption.
Native mutation ordering, failure reports, source approval, publication, and
launcher recovery retain their existing behavior behind the new interfaces.

Linux execution was not part of this pass.
