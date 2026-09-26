# Remaining tasks: deterministic verification

Updated 26 September 2026. Scope: VERIFY-01 in the
[active plan](../../../plans/determinate-nix-stacked-prs.md).

## Already delivered

Clock/FixedClock at channel freshness; hermetic wrapper and static audits;
Linux clean-host release assurance; compiled CLI process tests; value-type
property tests; platform adapter goldens; manual repeat-proof workflow and
exact-release install/upgrade/reboot helpers; bounded macOS broker acquisition-stage
capture in public-release CI ([PR #66](https://github.com/spa5k/pkg/pull/66));
and the hosted macOS capacity wait ([PR #67](https://github.com/spa5k/pkg/pull/67)).
These are not new tasks.

## Repeat and scheduled execution

- [ ] Diagnose the failed prepare phase in [run 33898972544](https://github.com/spa5k/pkg/actions/runs/33898972544); complete both slots and both real reboots.
- [ ] Diagnose nightly cancellations; obtain full fault-harness and real-Nix results on both platforms.
- [ ] Record exact inputs, host identity, phase verdicts, and retained logs; separate historical DN-16 evidence from current repeatability.

## Stable-state and mutation coverage

- [ ] Define stable journal, receipt, and metadata fields with explicit normalization of record-only fields.
- [ ] Add repeated same-platform install/repair fixtures and reviewed golden regeneration.
- [ ] Define a shared cross-platform semantic contract instead of requiring different platform receipts to have identical raw bytes.
- [ ] Inventory live mutation boundaries and map each to a crash/recovery test.
- [ ] Add missing boundary tests and a completeness check that fails on uncovered additions.

## Additional integration evidence

- [ ] Complete the cross-platform concurrent-command matrix for BUG-01. The [local fix proof](../../../tests/macos-clean-host/BUG-01-FIX-2026-09-26.md) covers all eight operation kinds in process, Unix socket closure, and native macOS approval/preparation overlap. Linux runtime and the remaining lifecycle fault boundaries still need evidence.
- [ ] Complete cross-platform BUG-02 lifecycle evidence. The [local fix proof](../../../tests/macos-clean-host/BUG-02-FIX-2026-09-26.md) covers 49 preparation pairs, rooted-install then remove recovery, superseded-attempt cleanup, and native macOS mixed-operation recovery. Keep Linux runtime and the full mutation-boundary matrix open.
- [x] Add local missing-link activation, empty-generation repair, post-activation write failure, and preview-versus-commit regressions from BUG-03 through BUG-06. The [fix evidence](../../../tests/macos-clean-host/BUG-FIXES-2026-09-26.md) records the tests, faults, and macOS results.
- [ ] Run these regressions on native Linux and extend the remaining interruption matrix. The existing local and macOS checks are complete; a zero CLI exit alone is not sufficient evidence.
- [ ] Isolate shell-spawning unit tests from immediate file-lock reacquisition tests on macOS. Keep active-lease refusal checks strict. The alpha.56 release proof retains the transient inherited-lock diagnosis.
- [ ] Extend CLI process transcripts only where the current tests and onboarding audit expose gaps.
- [ ] Add bounded fuzz targets for journal, framing, and metadata parsers with retained reproducers.
- [ ] Add generated install/repair/upgrade/remove sequences with shrinking and lifecycle invariants.
- [ ] Add historical/future journal fixtures for the documented accept-or-refuse policy.
- [ ] Test supported protocol combinations and explicit refusal of unsupported versions; do not invent a compatibility promise.
- [ ] Measure the current Linux proof duration before deciding whether a smaller per-push tier is needed.
- [ ] Complete the planned cargo-nextest runner adoption and nightly cargo-llvm-cov reports after checking platform and hermetic-wrapper compatibility; triage measured coverage gaps.

## Report and rollout

- [ ] Aggregate hermetic, stable-state, and boundary results into a machine-readable report with explicit skipped/failed scope.
- [ ] Obtain two clean weeks of the required scheduled checks before promoting the new gates.
- [ ] Update evidence links and quality baselines only after deliberate reviewed changes on both platforms.
