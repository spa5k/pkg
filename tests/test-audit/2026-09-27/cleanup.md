# Test cleanup and regression guard

Base: `e96a7d06b01e0205e82f51f112a8951ae9324830`.
Plan: [VERIFY-01](../../../plans/determinate-nix-stacked-prs.md#verify-01-repeatable-assurance).

This batch removes the 192-line fake E2E suite and two public CoreEngine
accessors used only by tests. Real CLI, renderer, shell, and filesystem checks
replace weak assertions. The [regression guard](../../../tools/verify/TEST-CONTRACTS.md)
repeats eleven known defects in required Fast CI. Each defect must fail its
tests between passing baseline and restored controls.

## Preserved contracts

| Removed or repaired check | Keeper |
| --- | --- |
| Fake E2E and duplicated dispatch checks | The routing test owns exactly-once dispatch, install approval, and GC dry-run policy. The real public-result privacy test remains. |
| Duplicate top-level help smoke | The discoverability test checks the real `--help` entry point, home guide, and command help. |
| HOME test that stopped at an invalid path | Compiled `pkg history` opens real temporary state under the OS account home with spoofed or absent HOME. The false home stays empty. |
| Doctor check that passed with no matching rows | The CLI must report `runtime.managed` and `channel.signed` exactly once. |
| Renderer fixtures and digest-presence assertions | The renderer CLI refuses missing, symlinked, empty, and directory artifacts and invalid tags. Its generated shell verifies actual bytes with real SHA-256. Distinct artifacts catch swapped digests; tampering must fail before execution. |
| Global workflow guard strings | The workflow test checks the actual production job condition and manual boolean inputs, including missing or duplicate fields. |
| Static Linux harness entry checks | The real harness refuses invalid arguments before external commands. A valid control reaches the first Docker boundary. |
| README wording and obsolete-name negatives | Removed: no distinct runtime or security contract depended on those words. |

## Validation

- E2B Linux workspace: 1,232 passed, zero failed, eight ignored.
- E2B Python: 147 passed, three macOS-only cases skipped. The bounded capture
  and vendor replay script passed separately.
- macOS CLI: 207 passed. Changed renderer, bootstrap, and workflow suites:
  19 passed, with Bash and zsh available.
- All eleven mutations were caught; baseline and restored controls passed.
  The six Python-driven cases were repeated after the final classifier fix.
- Eight real subprocess tests validate the guard. They reject weak assertions,
  missing/skipped tests, direct and child import errors, child syntax errors,
  abnormal exit after a failing summary, and stale mutation targets.
- Rust 1.96.1 G-LINT passed: formatting, Clippy, rustdoc, build, deny, and audit.
  Quality passed with 922 existing debt sites; its baseline is unchanged.
  Workflow validation, documentation links, static audits, and diff checks passed.
- Independent A and E preservation reviews have no open findings. Review found
  lost exactly-once dispatch coverage and two false-green guard paths. All were
  fixed and covered by controlled defects or real subprocess regressions.

Raw logs, ledgers, and model transcripts are retained locally. New CI runs
retain command logs and machine-readable results as workflow artifacts. Raw
execution output is not part of the source diff.

## Limits

The E2B model was `zai` / `glm-5.3-flash`. Parent review and combined validation
refined its edits. Both sandboxes were stopped after evidence download.
External downloads, host identity, elevation, and installed CLI paths remain
controlled boundaries in the bootstrap harness. Native installation, real Nix
lifecycle, reboot, and network isolation were not rerun in this batch.

This is a bounded cleanup, not a complete repository pruning campaign or
completion of VERIFY-01. Review unexamined contracts before deleting more tests.
The seven cleanup files remove ten production lines and add a net 24 test
lines. Confidence comes from defect detection, not file or line counts.
