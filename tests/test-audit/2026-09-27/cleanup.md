# First test cleanup and replacement batch

Date: 27 September 2026 (Asia/Kolkata).
Base: `e96a7d06b01e0205e82f51f112a8951ae9324830`, with the prior uncommitted
plan cleanup and audit documents preserved.

This batch implements the first repair and consolidation work from the
[audit](report.md). It removes the 192-line fake E2E suite and two public
CoreEngine accessors used only by tests. The remaining routing contract has one
owner. New process tests exercise the real renderer, generated bootstrap, CLI
state boundary, doctor output, and Linux harness argument handling.

## Removed checks and their replacements

| Removed or repaired layer | Contract owner after cleanup |
| --- | --- |
| `crates/pkg-cli/tests/e2e_fake.rs` and `CoreEngine::{operations,into_operations}` | `core_engine_routes_every_variant_and_preserves_global_policy` owns dispatch, exactly-once execution, install approval, and GC dry-run policy. Its child test module reads private state directly. The real public-result privacy test remains. Fake Nix expectations produced by the fake operations are not a product lifecycle contract. |
| Separate top-level help smoke test | `every_command_has_examples_and_remains_discoverable` now checks the actual `--help` entry point as well as the home guide and command help. |
| HOME test that stopped at an invalid relative path | The compiled `pkg history` command opens real temporary state beneath the OS account home. Both spoofed and absent HOME values must succeed, return empty history, and write the log at the selected state location. The false home stays empty. This proves the trusted home boundary for an explicit root; it does not prove every default-root path. |
| Doctor assertion that passed with no matching rows | The real CLI result must contain `runtime.managed` and `channel.signed` exactly once, before either success or failure status is checked. |
| In-process renderer checks with incomplete fixtures | The real renderer CLI receives all three distinct artifacts. Each artifact is tested as missing, a symlink, empty, or a directory. Invalid tags must produce the tag error and no script. Safe tag-derived fixture filenames prevent a later file error from masking a disabled tag guard. |
| Renderer digest-presence checks and hand-filled bootstrap placeholders | The shell harness executes output from the real renderer. Real `shasum` verification checks the generated digest against actual bytes. Distinct Linux, macOS package, and wrapper bytes detect a swapped digest. Tampered downloads must fail before the execution marker. |
| Global presence of manual-job guard words | The test checks the condition inside the actual production job and the boolean input inside the manual trigger. Missing or duplicate required fields fail. The fixed-format extractor is deliberately limited to this workflow; it is not a general YAML validator. |
| Static checks used as evidence that the Linux harness runs | The real harness must reject malformed arguments with exit 2 before Docker or Git entry. Sentinel executables contain those external boundaries. A valid-argument control reaches the first Docker attempt and stops there. This is entry-path proof, not a native lifecycle run. |
| README wording and two obsolete-name negatives | Deleted without replacements. No distinct runtime or security contract depended on those words. Other security pins and the blocker inventory remain. |

The shell PATH and removed-binary startup tests remain. Narrow tests are retained
where they detect a distinct real defect. No new mock lifecycle suite, production
test mode, dependency, or CI workflow was added.

## Validation

| Final check | Result |
| --- | --- |
| E2B Linux workspace, all targets and features, locked dependencies | 1,232 passed; 0 failed; 8 ignored |
| E2B Python unittest cases | 147 passed; 3 macOS-only cases skipped; 150 total |
| Bounded capture and vendor replay script | Passed |
| macOS pkg-cli, all targets and features | 207 passed; 0 failed; 0 ignored |
| macOS renderer, bootstrap, and workflow suites | 19 passed; Bash and zsh were available |
| Formatting, Clippy with warnings denied, rustdoc, build, cargo-deny, cargo-audit | All passed with Rust 1.96.1 |
| Quality ratchet, ambient-time/temp-root static audits, documentation links, whitespace | All passed; 922 existing lint-debt sites remain |
| Independent preservation review | One finding fixed; no open finding in the reviewed scope |
| Controlled source mutations and restored controls | All 11 mutations failed by assertion; all 11 restored controls passed |

The mutation set contains the seven audit cases plus four preservation checks:
spoofed HOME as the trusted state boundary, swapped Linux/macOS artifact digests,
lost GC dry-run policy, and duplicate remove dispatch. Six audit gaps gained
effective checks. The seventh privacy mutation is still caught by its existing
narrow keeper after the fake layer is removed. Mutation failures were test
assertions, not compiler failures or zero-test runs.

See the [Linux summary](cleanup-evidence/e2b/summary.json),
[exact commands](cleanup-evidence/e2b/validation.json),
[mutation results](cleanup-evidence/e2b/final-mutations/results.json),
[macOS runs](cleanup-evidence/local/macos-tests.json),
[local gates](cleanup-evidence/local/local-gates.json), and
[preservation review](preservation-review.md). The original eight ignored Rust
cases remain. The three skipped Python cases require macOS packaging tools;
the changed suites were run separately on macOS. A missing zsh branch in E2B
is not treated as a pass for that shell.

The [quality output](cleanup-evidence/local/quality-gate.log) and
[static audit output](cleanup-evidence/local/static-audits.log) are retained.
The static audit used `AUDIT_ONLY=1`; it did not run network isolation or the
hermetic tripwire. Existing lint debt was not hidden by a baseline change.

Existing CI already runs the renderer and bootstrap suites on Linux and macOS
for every pull request. The release assurance job runs the workflow suite when
its path filters match. CLI tests remain in workspace test jobs. No CI route
was removed or weakened.

Production code: 10 lines removed, all test-only accessors. Test code: 345 lines
added and 321 removed, net 24 added. Separate test-support code: no change.
Counts cover only these seven code/test files; audit evidence and prior cleanup
are excluded. Confidence comes from the mutation results, not the net line count.

E2B sandbox `iqhwh2z27ui6el6r2o9bw` was stopped after the evidence was downloaded
and its hashes were checked. The [candidate hashes](cleanup-evidence/e2b/candidate-hashes.json)
match the local files. All mutation owners were restored byte for byte.

## Review and limits

The E2B model was `zai` / `glm-5.3-flash`. It edited three disjoint groups.
The installer and workflow tasks completed. The CLI task saved its edits but
timed out during validation. The parent reviewed and refined all seven files,
then ran the combined checks. Parent corrections removed repeated digest
assertions, replaced the weak HOME draft, retained the real `--help` entry point,
fixed scoped permission and duplicate-field checks, and removed both accessors.

An independent preservation review found one lost contract: the initial retained
routing test inspected only the last operation, so duplicate dispatch could pass.
The final test asserts one new call per command. A duplicate-remove mutation is
included in the final validation. The review found no other actionable issue in
these seven files. Its source review did not replace runtime validation.

Downloads, host identity commands, elevation, systemd presence, and the installed
CLI path remain controlled boundaries in the bootstrap harness. The renderer,
shell script, checksum command, files, exit codes, and log permissions are real.
No native install, real Nix lifecycle, reboot, or full GitHub workflow ran in this
batch. Existing ignored and platform-specific tests remain explicit limits.

This is the first reviewed batch. It does not complete the whole-repository
pruning campaign. The historical audit ledger remains unchanged. Further work
must review the unexamined subsystem contracts before deleting their tests.
At this cleanup checkpoint, no commit or pull request had been created.
The [regression guard follow-up](regression-guard.md) records the publication
scope and repeatable CI checks added next.
