# Proposal: Deterministic verification suite

## Why

The macOS lifecycle proof (run `33329100389`) and the staged Linux proof each passed once, on
specific hardware, on a specific day. A single green run is evidence, not a guarantee. Before the
production trust ceremony puts real keys and a public channel behind this product, verification
must become deterministic: the same inputs must produce the same verdict every time, on any
machine, with no dependence on clock, network ordering, or leftover state.

Known gaps observed during the DN-15/DN-16 proof campaigns:

- Proof runs depended on a live Quick Tunnel URL and a local `serve_proof_channel.py` process.
  Hostname changes forced full binary rebuilds (see the tunnel-replacement incident).
- One pre-existing test fails on arm64 macOS but passes in CI (`live_uninstall_accepts_only_plain_output`)
  — a platform-dependent verdict, not a deterministic one.
- Journal and receipt bytes were never proven byte-stable across repeated installs of the same
  release pair; timestamp or ordering drift would silently break exact-receipt reuse
  (`cbd3494` added receipt reuse; a determinism regression would corrupt it).
- Fault injection exists for selected boundaries (crash-after-exit-zero, sigkill, unlink windows)
  but is not systematically mapped to every filesystem mutation boundary.

## What Changes

1. A repeat-run verification workflow: any engineer can trigger the full two-slot lifecycle proof
   with pinned inputs from a clean environment, on demand, and get the same verdict.
2. A hermetic test audit: every workspace test must pass with no network, frozen clock
   (`#[cfg(test)]` time injection), and isolated temp roots. Platform-dependent verdicts are
   either fixed or explicitly gated and reported.
3. Byte-stability proofs: installing the same release pair N times must produce bit-identical
   journals, receipts, and channel metadata. Golden files pin the exact bytes.
4. A fault-injection matrix: every filesystem mutation boundary in install, repair, upgrade, and
   uninstall paths gets at least one crash-injection test that proves the documented recovery
   behavior (preserve started / roll forward / refuse retry).
5. A determinism report: `tools/verify/determinism-report.sh` emits a single verdict
   (DETERMINISTIC / NON-DETERMINISTIC with causes) covering all of the above.

## Non-goals

- No new product features. This change adds verification only.
- No renaming (`pkg` → `kelv` is a separate later change).
- No change to the TUF channel protocol itself.
- No new proof environments beyond the existing Linux staged host and macOS Apple Silicon slots.

## Impact

- `crates/pkg-installer`, `crates/pkg-nix`, `crates/pkg-testkit`: time injection, test isolation.
- `.github/workflows/`: new repeat-run proof workflow (manual dispatch).
- `tools/verify/`: new determinism report script.
- `tools/quality/baseline.json`: debt ratchet rebased if test restructuring moves lint sites.
