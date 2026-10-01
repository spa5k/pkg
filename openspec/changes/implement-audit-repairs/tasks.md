# Tasks: implement the accepted pkg audit repairs

Scope: [proposal](proposal.md) and [design](design.md). One stacked
branch series delivers the six findings in the order below. A task is
checked only when its branch is complete and its checks ran.

## 1. Fail-closed setup, typed importer errors

- [x] 1.1 Refuse automatic administrator setup when the global
      `/etc/nix/nix.conf` exists but is not world-readable: manual
      review instructions before ALL administrator steps; delete the
      sudo-grep secrets probe and the automatic `chmod 0644`; doctor
      stops suggesting a blind chmod.
- [x] 1.2 Setup inspection distinguishes missing, unreadable, and
      malformed configuration files and preserves the cause when the
      inspection decides control flow; a required piped-stdin write
      failure in `run_step` surfaces.
- [x] 1.3 Typed public `ImportError` (`SandboxRefused` vs ordinary
      failure) at the `cask_catalog::tap` seam, produced by
      construction at the config gate; ordinary text containing the
      refusal phrase never triggers setup; the existing single retry
      runs only after verified opt-in setup; formatting happens only at
      the CLI boundary.
- [x] 1.4 Tests: failure cannot reach chmod; unreadable vs missing vs
      malformed config; typed category versus coincidental text;
      required stdin write failure.

## 2. Tap staging and publication ownership

- [x] 2.1 `new_staging` returns one owned `StagedGeneration`; consuming
      `publish` settles the guard internally and returns a `Publication`
      owning the retained/scratch state with explicit `finish` and
      `undo`; no fallible rollback from `Drop`; a staged generation from
      another source is refused; registry-then-source lock ordering,
      lock scope through commit/undo, the two-rename crash gap, error
      fidelity, and cleanup warnings are preserved; validation, consent,
      registry, and output stay in the command layer.

## 3. Typed catalog target decision

- [ ] 3.1 Internal `TargetDecision` (eligible with plan vs excluded
      with reason and detail, common version/homepage outside the
      variants, kind derived from the plan); classify/capture/emit and
      all consumers/tests adapted; emitted JSON contract, deterministic
      bytes, and the committed catalog unchanged.

## 4. One isolated Nix environment constructor

- [ ] 4.1 One private constructor for the shared environment/process
      setup of the config gate and the raw-export build; caller roots,
      args, deadlines, log caps, labels, and gates stay separate.
- [ ] 4.2 Environment sentinel test: exact whitelist in the isolated
      child; no ambient `NIX_CONFIG`, proxy, or fake credential
      sentinel leakage.

## 5. Snapshot lane loading (only if a small helper pays)

- [ ] 5.1 Share one private payload loader across the discovery lanes
      (identity, freshness, persistence, stale fallback) accepting the
      two concrete fetch operations, with row/filter/platform/query
      semantics kept in the lanes — or record the rejection here and
      leave the code.

## 6. Quality and CI

- [ ] 6.1 Correct only the obsolete `clippy.toml` and
      `rust-toolchain.toml` comments referencing deleted gates; ADR 0005
      stays superseded.
- [ ] 6.2 Add individually selected `unwrap_used`/`expect_used`/`panic`
      deny lints for production with reasoned, narrowly scoped test
      exceptions; replace the two actual production sites on domain
      evidence.
- [ ] 6.3 macOS smoke job installs and runs Clippy on all targets; the
      installer smoke check runs ShellCheck with runner-provided
      binaries; no advisory fetching added.

## 7. Delivery

- [ ] 7.1 Stacked branch series on baseline `120e9ed`, one purpose per
      branch; stack record, bundle, and honest report prepared outside
      git; final checks run on the pinned Rust 1.96.1.
