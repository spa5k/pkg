# Tasks: Deterministic verification suite

## 1. Time injection

- [ ] 1.1 Define `Clock` trait in `pkg-core` with `now()` returning a `Timestamp`; default
      impl delegates to `SystemTime`
- [ ] 1.2 Thread `Clock` through journal writers (`macos_install_journal`, Linux journal),
      receipt publication, and channel timestamp readers behind a constructor, mirroring the
      `CommandExecutor` pattern
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
