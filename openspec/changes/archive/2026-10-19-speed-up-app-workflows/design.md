# Design

Use a temporary source copy for stage-timing instrumentation.
Keep the production checkout and frozen comparison binaries unchanged.
Measure real store-warm DMG and ZIP installs with private app caches.
Record helper and payload child durations inside the existing call path.
Accept no production change until the measured cost supports it.
Require a focused behavioral regression for any selected change.

## Selected change

Three warm DMG and ZIP samples show 637 and 657 ms median helper help
probes. App refresh takes 1,931 and 1,874 ms. The normal helper sync takes
1,200 and 1,152 ms. Other app work is small in these two packages.

Use the actual staged launcher command to check helper startup. Verify the
pinned helper profile before running it. Repair only the exact known SBCL
static-address failure. Build a fresh pinned helper profile, verify its
entry, and retry the staged command once. Promote that helper profile only
after the retry succeeds. A failed build or retry keeps the old profile.
Do not cache helper health. Preserve signal handling and Dock side-effect
reporting. Publish launchers only after helper success and ownership checks.

The TraceDecay graph is unavailable for these symbols after an index
refresh. Exact source references show two production callers: app sync
calls helper selection and staged publication; staged publication calls
the helper command. Run helper repair tests, app publication tests, the
full Rust checks, and the real native macOS lifecycle.

## First helper setup

A separate instrumented app install records about 32 seconds in initial
helper selection. Keep that first setup observation separate from warm
paired install times. Profile the real native calls with a temporary runtime
wrapper. Check source metadata, Nix evaluation, and profile creation in order.
The wrapper runs the real Nix executable with unchanged arguments.
Select no reference or cache change before this measurement.
