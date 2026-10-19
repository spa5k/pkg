# Tasks

- [x] M1. Fetch the latest main and preserve the current work.
- [x] M2. Record the broad macOS and Linux baseline.
- [x] P1. Remove the unconditional version probe from ordinary workflows.
- [x] M3. Measure client installation modes and remaining app and service paths.
- [x] P2. Improve the next measured workflow bottleneck.
- [x] V1. Run behavior checks and repeat command benchmarks on both systems.
- [x] V2. Record the final results and remaining limits.

## Fresh-home installation follow-up

- [x] M4. Compare origin/main with the accepted release in fresh-home real installs.
- [x] V3. Check native commands, profiles, app publication, and removal.
- [x] R2. Record cold-source context, every sample, failures, and limits.

Keep the shared Nix store warm. Do not prewarm source metadata in each new home.
Each binary has an excluded first pair and three measured samples per case.

## Complete shipping payload follow-up

- [x] M5. Run normal client packaging and verify full archive contents.
- [x] M6. Compare all six installer modes with complete notice payloads.
- [x] M7. Measure downloader preparation with complete pinned catalogs.
- [x] V4. Verify client hashes, completions, shell blocks, catalogs, and empty profiles.
- [x] R3. Record every sample, archive identity, original-file restoration, and limits.

Preserve existing dist archives and checksums. Keep local HTTP transport.
Use fresh private homes and pkg caches for full preparation samples.
Keep the external Nix installation and shared store intact.

## More vendor archive types

- [x] M8. Measure pinned tar.xz and tar.bz2 app installs and a standalone vendor PKG.
- [x] V5. Verify app publication, read-only CLI version, removal, and empty profiles.
- [x] R4. Record every attempt, format, exact payload identity, and any refusal.

Use ImageOptim, Jumpcut, and desktoppr from the committed catalog.
Run desktoppr with its read-only `version` word. Do not start app GUIs.
Keep initial shared-store fetches separate from repeated samples.


## Scheduled default installer follow-up

- [x] M9. Measure the complete downloader with a real Linux user refresh timer.
- [x] V6. Check the active timer, a real refresh job, catalogs, and cleanup.
- [x] R5. Record all samples, host prerequisites, and service limits.

Use a disposable Linux user manager with matching HOME and XDG paths.
Use the complete notice payload and real local Nix fixtures.
Keep host preparation outside the measured interval.

M8/V5 record a desktoppr refusal on both clients. Its read-only CLI check is not reached.
The two compressed app formats pass publication, removal, and empty-profile checks.


## Accepted client command matrix

- [x] M10. Compare origin/main with the accepted spool client across all 46 command cases.
- [x] V7. Verify real profiles, changed upgrades, rollback, removal, and local search on both hosts.
- [x] R6. Record all nine-sample medians, observations, identities, and limits.

Use private homes and real local Nix fixtures. Keep the public API and services outside this matrix.
Run each host without another timing or build job.


## Intermittent Linux command delay

- [x] M11. Trace the repeated extra delay in warm local setup commands.
- [x] D7. Identify the delay owner before selecting a change.
- [x] V8. Keep output, process cleanup, cancellation, and resource bounds intact.
- [x] R7. Record diagnostic traces and a measured change or rejection.

Trace only private fixture commands on the disposable Linux host.
Keep untraced samples separate from traced diagnostic durations.
Do not shorten cleanup deadlines to force a speed result.

M11–R7 select no process change. Parent traces locate the extra wait before Nix
child exit. The exact internal Nix cause remains unknown. Cleanup deadlines stay intact.


## Installer checksum process follow-up

- [x] M12. Compare checksum tool selection with the same frozen client and notice payload.
- [x] V9. Keep checksum verification, path escaping, installed bytes, and shell setup intact.
- [x] D8. Select only from repeated end-to-end results on both hosts.
- [x] R8. Record all samples and any rejection.

Prefer the native SHA-256 command only if the measurements justify the change.
Keep the portable checksum fallback. Do not run beside a build on the same host.

Keep the Mac-only checksum preference after balanced samples and fallback checks.
The Linux checksum choice stays unchanged. All initial failed and overlapping runs remain.


## Balanced benchmark sampling

- [x] M13. Use even default pair counts in alternating comparison tools.
- [x] V10. Check command help, Python format/lint, and complete smoke execution.
- [x] M14. Repeat the final broad command matrix with balanced order on both hosts.
- [x] R9. Record all samples, order limits, final source identities, and the sampling correction.

The checksum follow-up shows strong Linux first-position timing differences.
Odd pair counts can give one binary more first positions and move its median.
Keep explicit odd --runs values for historical reproduction.
Use even defaults for new comparisons and16 pairs for the final command matrix.
Do not edit scripts while their active timing jobs still use them.

All final balanced cases pass. Keep the slower Linux warm-install p50 and
its faster paired/mean values. No general gain is claimed for that case.

## Client installation without Nix

- [x] M15. Measure all client installer modes in a Linux root without Nix.
- [x] V11. Check exact client/completion bytes, shell setup, guidance, and refusal behavior.
- [x] R10. Record runtime isolation, all attempts, payloads, and host limits.

Use a disposable Linux host and a private chroot with real host tools.
Keep the sandbox Nix installation intact. The private root must contain no Nix.
Use complete release notices, real clients, local HTTP, and fresh homes.
Run as a normal user inside the root. Disable background scheduling.
The prepare mode must give the documented external-runtime guidance.
This downloader does not install Nix. Do not add runtime installation behavior.


## Existing-profile mutation observation follow-up

- [x] M16. Compare reuse of the first decoded profile before install and rollback.
- [x] V12. Keep fresh-profile validation, unknown-manifest refusal, locking, cancellation, and postflight reads.
- [x] D9. Select only after full checks and balanced command results on both systems.
- [x] R11. Keep exact source, native lifecycle evidence, all timings, failures, and limits.

A private checked-profile value binds a decoded map to its runtime and profile path.
Use that value only while the existing profile lock is held.
Existing public mutation methods keep their own preflight checks.
A missing profile keeps the local empty-map path and the original fresh-mutation check.
Postflight and failure reads remain fresh. No observed state is persisted between commands.
Change only the production files nix/mod.rs, profile/query.rs, and profile/change.rs.
Update the existing command-order behavior test for the removed read.
Keep its lock, signal, changed-state, failure, and postflight assertions.
Keep the selected ID-buffer source as the independent baseline for this trial.
Do not apply this private source until all preceding jobs on that host finish.
Run full Rust checks and native lifecycle checks before the 16-pair command matrix.

## Thousand-package profile check

- [x] M17. Compare the final client with origin using 1,000 real installed fixture packages.
- [x] V13. Check exact lists, repeated messages, stable rows and generations, and empty cleanup.
- [x] R12. Keep all balanced samples, separate read-only resource passes, source hashes, and limits.

Run after all preceding timing queues on each host. Use private homes and profiles.
Measure listing, history, recent pruning, and repeating all 1,000 packages.
Use four balanced pairs after one excluded pair. Keep the shared native store warm.
Do not add a memory claim for the long repeated-install command.

## Small native search control

- [x] M18. Repeat the small native search comparison on one physical catalog and HOME.
- [x] V14. Check exact output and cache bytes in direct, reversed, and same-binary groups.
- [x] R13. Retain all gaps and distinguish variation from a confirmed code cause.

The final Mac matrix has slower small native query medians. Use both hosts.
Run origin/final, origin A/A, final A/A, and final/origin with 16 balanced pairs.
Each group has one excluded first pair. Use the actual final publication with 128 rows.
Keep the PATH empty during queries. Run only after the thousand-package profile check.

## 2026-10-19 status

The checked-profile change is selected after full checks and the balanced command matrix on both hosts.
The 1,000-package and small-search controls are complete on Mac only; Linux raw files were lost when the host expired.
