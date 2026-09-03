# Tasks: Deterministic verification suite

## 1. Time injection

- [ ] 1.1 Define `Clock` trait in `pkg-core` with `now()` returning a `Timestamp`; default
      impl delegates to `SystemTime`
- [ ] 1.2 Inject `Clock` at the single wall-clock decision: the channel freshness check
      (`pkg-channel/src/policy.rs` `validate_descriptor` `now` parameter, fed from
      `jiff::Timestamp::now()` at `tuf.rs` call sites). Record-only timestamp sites
      (broker approval journal, repair report, CLI state files, logs) stay ambient:
      a grounding audit confirmed they decide nothing and are excluded from byte
      stability (owner decision, 2026-09-03). `Instant::now` timeouts are out of scope.
- [ ] 1.3 Add `FixedClock` test double in `pkg-testkit`
- [ ] 1.4 Convert timestamp-dependent tests to `FixedClock`; verify zero ambient-time reads in
      `#[cfg(test)]` builds via a debug assertion
- [ ] 1.5 Run workspace tests with networking disabled (`tools/verify/run-hermetic.sh` wrapper
      using `unshare -n` on Linux and a network-deny wrapper on macOS CI); fix every leak

## 2. Repeat-run proof workflow

- [ ] 2.1 New workflow `proof-repeat.yml` with `workflow_dispatch` input `pair_sha`
- [ ] 2.2 Job fetches pinned inputs by digest into an ephemeral in-job channel using
      `serve_proof_channel.py _serve` bound to localhost
- [ ] 2.3 Both macOS slots and the Linux staged host execute the full lifecycle matrix
- [ ] 2.4 Verdict equality check: slot verdicts compared against a checked-in expected matrix
- [ ] 2.5 Document dispatch procedure in `tools/release/README.md`

## 3. Byte-stability suite

- [ ] 3.1 Golden directory `crates/pkg-testkit/golden/{x86_64-linux,aarch64-darwin}/` with
      journal, receipt, and channel metadata bytes for the representative pair
- [ ] 3.2 Test `byte_stability` installs the pair twice into two clean fake roots and asserts
      bit-equality of all three artifact classes
- [ ] 3.3 `UPDATE_GOLDEN=1` regeneration path with a mandatory README note on review policy
- [ ] 3.4 CI job runs the suite on both platforms

## 4. Fault-injection matrix

- [ ] 4.1 Generator `tools/verify/gen-boundary-inventory.rs` scanning mutation call sites
- [ ] 4.2 Inventory file `tools/verify/boundaries.json` mapping boundary → test name
- [ ] 4.3 Completeness check wired into CI; fails on uncovered boundary
- [ ] 4.4 Add injection tests for every currently uncovered boundary (expected: the unlink and
      rename windows; audit against `cbd3494` receipt-reuse path)
- [ ] 4.5 Platform-gate `live_uninstall_accepts_only_plain_output`; convert any other
      platform-dependent verdicts found by the hermetic audit

## 5. Determinism report and rollout

- [ ] 5.1 `tools/verify/determinism-report.sh` aggregating hermetic audit, byte-stability, and
      boundary completeness into one verdict + JSON summary
- [ ] 5.2 Nightly schedule for the report; two clean weeks before making it a merge gate
- [ ] 5.3 Rebase `tools/quality/baseline.json` if lint sites moved
- [ ] 5.4 Update `plans/` index and ADR 0005 quality-gate docs to reference the new gate

## 7. Integration tier (amended after plan review)

- [ ] 7.1 Fast lifecycle smoke tier: reduced ~8-case list as a subset of the staged-proof
      blocking matrix, containerized, real binaries + local TUF channel, per-push, <15 min
- [ ] 7.2 Black-box CLI process tests with trycmd: spawn compiled binary, snapshot
      stdout/stderr/exit codes/filesystem effects; golden transcripts for help, errors,
      doctor
- [ ] 7.3 cargo-fuzz targets: journal file parser, framing protocol decoder, TUF metadata
      JSON parser; committed corpora; nightly 5-min bursts; reproducer artifact on finding
- [ ] 7.4 Property-based operation sequences: proptest-generated install/repair/upgrade/
      uninstall interleavings asserting idempotency, monotonic generations, exact receipt
      reuse, fail-closed recovery; shrinkers recorded
- [ ] 7.5 Protocol compatibility matrix: pinned old/new broker + client builds run the
      framing contract tests; unsupported combos fail closed with the version error
- [ ] 7.6 Cross-platform receipt equality test pinning byte-identical receipts for the
      same pair across Linux container and macOS
- [ ] 7.7 Journal schema migration goldens: one golden journal per historical schema
      version; upgrade-or-refuse assertions; future-version refusal fail-closed
- [ ] 7.8 Tooling adoption: cargo-nextest as the workspace runner; cargo-llvm-cov report
      published nightly; coverage gaps triaged into follow-up tasks
