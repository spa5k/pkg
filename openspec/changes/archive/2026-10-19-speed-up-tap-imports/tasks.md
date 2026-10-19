# Tasks

- [x] M1. Profile the complete current-branch tap update stages.
- [x] M2. Profile phases inside the sandbox export build.
- [x] D1. Select a measured change or record its rejection.
- [x] T1. Add a focused regression for any selected behavior.
- [x] P1. Implement the change or record its rejection.
- [x] V1. Verify metadata, failures, and repeated end-to-end times.
- [x] R1. Record results and limits.

No production change is selected. T1 and P1 need no code.
V1 records successful metadata checks and the retained HTTP 403 trial.

## Existing standalone boot follow-up

- [x] M3. Measure the existing standalone boot path in a temporary Nix build.
- [x] D2. Select a startup change or record its rejection.
- [x] V2. Compare complete captures and the unchanged live sandbox probe.
- [x] R2. Record results and limits.

The standalone follow-up fails in its first excluded warmup.
V2 records normal capture success and the failed standalone export.
Both unchanged live probes pass. No complete metadata match or speed claim exists.
The normal upstream startup stays in production.

## Complete standalone startup follow-up

- [x] M4. Supply the pinned upstream Ruby startup state in a temporary build.
- [x] V3. Compare complete native captures and live sandbox controls.
- [x] D3. Retain normal startup unless complete results justify a change.
- [x] R3. Record failures, exact prototype source, and all timing samples.

Keep the production entry point unchanged during measurement.
Use a separate disposable host while shipping installer runs are active.


The complete standalone path still fails at runtime setup.
The first guarded entry matches all requested Linux captures but omits
upstream Ruby helpers, shared PATH state, and exporter program name.
It is rejected after source review. The queued Mac trial is cancelled
before its first build. No production source changes.

## Complete Ruby runtime follow-up

- [x] M5. Measure a direct export entry with full upstream Ruby runtime state.
- [x] V4. Compare native captures, runtime helpers, child commands, and live controls.
- [x] M6. Repeat public tap CLI add/update/remove with exact saved metadata.
- [x] D4. Select a measured implementation or record its rejection.
- [x] T2. Retain a focused runtime-state regression for a selected change.
- [x] P2. Apply any selected startup change.
- [x] V5. Run full Rust and native lifecycle checks for the final source.
- [x] R4. Record all samples, failures, source identities, and limits.


## Public tap package lifecycle

- [x] M7. Measure a real pinned public tap package on both native targets.
- [x] V6. Verify the executable, compressed manpage, three completions, and profile state.
- [x] V7. Verify pinned updates, fixed upgrades, source removal, refusal, and package removal.
- [x] R5. Record samples, first fetches, metadata identities, and cleanup.

Use goreleaser/tap at77e442b6000587bf57e543081e95fd908b918bd7.
Use its GoReleaser2.18.2 CLI archive. Run only --version.
Do not publish a release or start a vendor GUI or server.

M5–R4 select no production change. No new runtime regression or implementation is retained.
M6 records the completed manual-entry current-branch matrix and the later HTTP 403
in the exact-metadata supplement. Full prototype Rust/native suites do not run.
V5 uses the unchanged accepted spool source and its earlier complete checks.
The third raw trial matches all Linux captures but has no clear speed gain.
The original Mac baseline fails its mandatory host-read probe before tap Ruby.

V6/V7 pass for the complete Linux public package lifecycle. R5 retains all samples.
M7 remains open for the Mac payload measurement. Its original mandatory
host-read control refuses before tap Ruby. The Mac daemon is preserved.
The extra Linux current-branch supplement has one measured pair and no speed claim.
See the [consolidated speed record](../../../../docs/verification/2026-10-19-speedup-summary.md).
