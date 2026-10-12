# Complete the Rust quality plan

Baseline: `df1af1d741774ecdafbb26c517a056c6ae34631b`.
Branch: `feat/rust-quality`.
Source specification: [complete Rust quality plan](../../openspec/changes/archive/2026-10-12-complete-rust-quality-plan/design.md).
This report covers the six items left after the first repair phase.
All six items are implemented.
Final Rust and native Nix verification passed on macOS and Linux at `36ddcba`.
The independent code reviews have no required correction.
The final decision-trail closure check has no unresolved material finding.
All 11 plan tasks have supporting evidence.
The result and its limits are committed on the current branch.

## Implemented outcomes

| Item | Result | Main owner |
| --- | --- | --- |
| Child lifetime | One owner observes exit without reaping, cleans up bounded process groups, and disables signaling before a consuming wait. Signal handlers record each signal kind. Pipe reads have a fixed budget. | `crates/pkg-cli/src/nix/process.rs` |
| App cancellation | Probe, payload, and launcher failures retain typed native interruption and the exact signal status. The helper writes into private staging. Exclusive publication and checked moved-source identity protect existing entries. | `crates/pkg-cli/src/apps/` |
| Package changes | One closed request owner takes the profile lock and orders native reads, mutations, reporting, and app refresh. Pruning has its own policy. App refresh requires the held profile capability. | `crates/pkg-cli/src/commands/change.rs`, `profile_lock.rs` |
| Catalog state | Checked constructors and strict decoding reject invalid IDs and source bindings. Saved collections derive their keys from checked catalogs and reject duplicates. | `crates/pkg-cli/src/catalog/`, `nix/index/`, `tap/store.rs` |
| Test locality | Six cask tests, 23 search tests, and five conflict tests replace three long journeys. Shared builders create real temporary profiles and snapshots. All 182 old assertion sites have an exact keeper declaration. | `crates/pkg-cli/tests/`, [keeper map](rust-quality-test-keepers-2026-10-12.tsv) |
| Discovery | Structured root and system membership distinguishes absent output from evaluation failure. Present malformed or throwing values remain errors. Prepared searches make no Nix call. | `crates/pkg-cli/src/nix/discovery.rs`, `catalog/native.rs` |

## Review corrections

The parent and the independent reviewers found defects during implementation.
Each accepted defect was corrected before closure.

- A continuous output stream could delay cancellation checks. Each pipe read pass is now limited to 64 KiB.
- An output-limit reason could skip stopping a running producer. The stop path now owns that state change.
- Mixed signal kinds could collapse into the last signal. Separate counters now retain each kind for forwarding.
- A child could exit between observation and a Darwin group signal. A fresh non-consuming observation now guards the zombie-only group exception.
- Some test failure paths could leak supervisor children. An owned fixture guard now cleans up the group before it consumes the wait status.
- Full-suite timing included time waiting for another test's process-run lock. The signal checks now run in an isolated owned process. The timeout check starts after child readiness.
- A Darwin test-guard exception could skip a still-running leader. It now requires a fresh non-consuming exit observation.
- Linux Nix metadata omitted the expected store-path field. A structured archive fallback now materializes and hashes the source. It preserves the subdirectory and revision fields without a fixed store-root policy.
- Fresh app homes had no staging parent. App sync now creates and validates that parent.
- Exclusive launcher renames did not validate a replaced source folder. Publication now checks the moved folder's open-directory identity and ownership marker.
- Profile preparation accepted a prune request that it could not handle. Separate request types now make that path unavailable.
- Duplicate fixture builders and two new branch narration comments were removed.

The [review report](rust-quality-completion-review-2026-10-12.md) records the independent results.
The [append-only decision trail](rust-quality-decisions-2026-10-12.tsv) records evidence and corrections.

## Verification

The final macOS and Linux Rust gates passed at `36ddcba`.
They include formatting, workspace Clippy, all-target builds, workspace tests, and private-item documentation.
Clippy uses `--all-targets --all-features -- -D warnings`.
The full `all` and `pedantic` policy remains denied.
No correctness or safety lint suppression was added.
Both gates include the final fixture-guard and Nix compatibility corrections.
Each client library run passed 170 tests and ignored four native fixtures.
Those four fixtures then ran explicitly with real Nix and passed on each host.
Each generator library run passed 123 tests.
The integration target counts differ by host because app tests require macOS.
Helper subprocess test output is not added to these target counts.

| Check | Evidence | Result |
| --- | --- | --- |
| macOS strict Rust gate | `/tmp/pkg-rq3-final-macos-complete-rust.log` | `just check` passed with all final source corrections. |
| Linux strict Rust gate | `/tmp/pkg-rq3-linux-complete.log`, `/tmp/pkg-rq3-linux-complete.exit` | All equivalent Cargo commands passed. The recorded pipeline exit status is zero. |
| Real Nix discovery | `/tmp/pkg-rq3-final-macos-complete-discovery.log`, `/tmp/pkg-rq3-linux-complete.log` | All four runtime fixtures ran and passed on each host. |
| Release builds and native CLI | `/tmp/pkg-rq3-final-macos-complete-release.log`, `/tmp/pkg-rq3-final-macos-complete-cli.log`, `/tmp/pkg-rq3-linux-complete.log` | Both release builds passed. All four macOS groups and both Linux groups passed. |
| Installer | `/tmp/pkg-rq3-install-tests.log`, `/tmp/pkg-rq3-linux-install.log` | macOS: 37 passed and seven host-specific tests skipped. Linux: all 44 passed with zero skips. Shell syntax and ShellCheck passed. |
| Documentation links and checker tests | `/tmp/pkg-rq3-final-doc-links.log`, `/tmp/pkg-rq3-link-tests.log` | All 15 tests passed. The link scan passed after the report edits. |
| Dependency policy | `/tmp/pkg-rq3-dependency-policy.log` | Bans, licenses, and sources passed. Existing duplicate-dependency warnings remain. This check does not include advisories. |
| Native archive and signature regression checks | `/tmp/pkg-rq3-native-archives.log` | Passed with real 7zz, libarchive, signature streams, and APFS file clones. |

The macOS host runs macOS 27.0.1, build 26A434, on arm64.
Its kernel is Darwin 27.0.0, XNU `13432.1.9~1`.
Rust is 1.96.1. Nix is 2.35.2, Determinate 3.22.1.
The temporary Linux host runs x86_64 Linux 6.1.158+, Rust 1.96.1, and Nix 2.35.2, Determinate 3.22.5.
No remote CI run is claimed.
The Linux script and exit marker were downloaded with the log.
Cargo manifests and the three changed process/discovery source files have matching SHA-256 values on both hosts.
The temporary E2B host was stopped after the downloads.
The earlier macOS timer failure and Linux metadata failure logs remain available.
The completed implementation worktrees have recoverable archived attachments.
Archive requests for the two older first-phase worktrees were queued.
An intermediate check showed one archived attachment.
The latest check shows active attachments for both older worktrees.
Their final removal is not confirmed.

The archive fallback follows the native [Nix flake archive contract](https://github.com/nixos/nix/blob/master/src/nix/flake-archive.md).
It requests the archive only when metadata omits `path`.
It rejects malformed archive path data and preserves native interruption.
The alternate store-root behavior is covered by a fake-runtime fixture.
The two native hosts use the default store root.

## Evidence limits

Native tests cover real INT, TERM, and HUP, direct-child reaping, bounded descendants, continuous output, timeout, and unrelated sentinel processes.
They do not force PID reuse.
The native WNOWAIT probe proves non-consuming exit observation.
The Darwin process-group check used published XNU `12377.1.9` source.
That source tag is older than the host kernel.

The Darwin member query is conservative.
Unreadable or truncated member data produces a cleanup error.
It does not provide an atomic snapshot of every descendant.
A foreign-UID descendant can reject a group signal.
The tests use descendants that the owner can signal.
If the OS denies the direct-child stop, waiting for that child can still block.
After an earlier operation error, Drop attempts cleanup and retains the original error.

The app helper can change the Dock before it fails or receives a signal.
The command reports that possible effect.
Filesystem recovery does not undo Dock changes.
The moved-source fixture tests the publication boundary directly.
It does not force an external replacement race through the full product.

The test keeper map identifies function declaration lines.
It does not claim that each line is the new assertion line.
One snapshot-reader mutation failed during fixture preparation.
It does not prove every possible cache-reuse regression.
See the [test locality report](rust-quality-test-locality-2026-10-12.md).

## Principles applied

Model the Domain produced owned process states, typed app interruption, checked catalog IDs, and distinct profile-change and prune requests.
Boundary Discipline placed validation at decode, construction, helper output, and filesystem publication boundaries.
Separate Before Serializing Shared State gave each writer an isolated worktree.
Sequence Work into Verifiable Units required focused checks before each integration.
Prove It Works added native process, archive, Nix discovery, and compiled CLI checks.
Laziness Protocol kept one change owner and shared fixture builders without a new backend framework.

