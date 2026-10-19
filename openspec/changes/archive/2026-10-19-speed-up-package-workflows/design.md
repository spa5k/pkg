# Design

Measure each proposed change with the same release compiler and fixture.
Exclude one warmup and alternate the base and candidate order.
Check real output and installed behavior before accepting a speed gain.
Separate network observations from repeated local measurements.

## Runtime discovery

Resolve and pin the configured Nix executable without starting it.
Run the requested Nix command as the first process.
Doctor requests the version explicitly and reports probe failures.
Native mutations still check the decoded profile schema before they run.
There is no persistent runtime cache or cached failure.

## Further candidates

Measure repeated profile checks, unchanged tap imports, client download steps,
and cold Formula metadata generation before selecting the next change.
Record each accepted design here.

## Client archive reads

Verify the compressed archive checksum first.
Decompress it once into the private temporary directory.
Use those tar bytes for the member listing and all four member reads.
Keep exact regular-file checks and read each member through stdout.
Archive paths are never extracted to disk.
The temporary tar uses extra disk space until the installer exits.
The measured 4.6 MB archive took 170 ms with repeated decompression and
102 ms with one decompression in the initial nine-sample check.

## Launcher refresh after a profile change

Use the postflight profile entries for launcher refresh.
The caller retains the profile lock through both operations.
Explicit `apps sync` still reads the profile after it takes the lock.
Keep the app lock, signature checks, helper checks, and atomic publication.
The existing CLI order test now requires exactly one postflight profile read.

## Cold Formula metadata

Evaluate at most 4,096 candidate names per Nix process instead of 128.
Limit each encoded name list to 64 KiB. Shrink batches with long names.
Reject an oversized single name before the native process starts.
Keep the exact candidate set, per-name evaluation, pinned source, timeout,
garbage collector settings, cache keys, and output format.
The first Linux experiment checked 21,532 names across both target systems.
The 1,024-name step reduced elapsed time from 117.0 seconds to 26.9 seconds.
A 4,096-name pilot took 10.9 seconds and about 419 MiB of peak memory.
A whole-input batch exceeded the native argument limit on Linux.
The bounded batch test failed before the fix and passes after it.
The metadata and the generated catalog hashes match exactly.
Repeat the final bounded implementation before accepting its speed estimate.


## Balanced alternating comparisons

Use an even number of measured pairs by default.
Alternate candidate and baseline order after the excluded warmup.
An even count gives each binary the same number of first and second positions.
The Linux checksum control exposes a large first-position delay.
An unchanged Linux command choice can appear slower with an odd count.
Keep complete raw samples and preserve explicit odd counts for reproduction.
Use balanced16-pair command and installer rechecks for final interpretation.
Do not infer that balanced order removes all host noise or bimodal timing.
