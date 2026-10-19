# Tasks

- [x] M1. Profile real DMG and ZIP install stages with temporary state.
- [x] D1. Select a measured change or record why none is justified.
- [x] T1. Add a focused regression for the selected behavior, if needed.
- [x] P1. Implement the selected change or record its rejection.
- [x] V1. Verify behavior and repeat relevant end-to-end measurements.
- [x] R1. Record results, failures, and remaining limits.

## First helper setup follow-up

- [x] M2. Profile native calls for helper setup with a fresh private HOME.
- [x] D2. Select a measured startup change or record its rejection.
- [x] V2. Verify any selected change and record the remaining limits.

The first-helper content hash prototype is rejected. No reference change is made.

## Shared Cask source follow-up

- [x] M3. Compare the helper input with the exact pinned Cask Nixpkgs source.
- [x] D3. Select a shared input only if compatibility and first-install data pass.
- [x] V3. Verify native helper startup, repair, and real app behavior.

The Cask Nixpkgs revision reports SBCL 2.6.8, as does the current helper input.
The prototype must keep the exact helper source and known-fault repair.

## Existing source transport follow-up

- [x] M4. Compare native tarball transport with the current GitHub input.
- [x] D4. Select transport only from complete native and first-app results.
- [x] V4. Verify exact source contents, helper behavior, and app lifecycle.

Use the existing b6c8664 revision and its independently computed NAR hash.
The temporary native prototype starts only after the active Mac app measurements.

The shared-revision follow-up is rejected. Full first-app medians are
42,416.208 ms current and 52,955.963 ms shared. Both native lifecycles pass.
The source is restored to the exact pre-trial helper input and comment.
Warm trial data is complete. It shows no clear gain.
The tarball transport follow-up is also complete.

Tarball transport is rejected after one native pair. Exact source contents,
helper output, and startup pass. No full product transport trial is selected.

## App count follow-up

- [x] M5. Measure native launcher sync for 1, 10, and 30 active fixture apps.
- [x] V5. Verify app bundles, healthy launchers, unchanged profiles, and cleanup.
- [x] R5. Record every sample, the helper version, and fixture limits.

Use the unchanged default compiler profile for this separate comparison.
Start only after the current Mac compiler and installer measurements end.
Do not launch app GUIs. Use unique fixture app names in temporary homes.
