# S5 findings — managed-daemon sandbox and resource boundary

Status: **Linux Docker and native macOS evidence observed; DR-005 remains
Proposed pending required signoff and the limitations below**.

## Observed Linux Docker lane

- Date: 2026-08-07.
- Docker Engine: 29.4.1, Docker Desktop Linux VM.
- Kernel: LinuxKit 6.12.76, `aarch64-linux`, cgroup v2.
- Image: `nixos/nix:2.34.8`, observed digest
  `sha256:1a711b619c8a713eff32c3f8d8781b3b4d0130cb91c0a57f67e87abfeeb90b01`.
- Nix daemon ran as root with group `nixbld`; a build process with UID 30001
  was observed in its per-build cgroup.
- Effective settings were verified as `sandbox=true`,
  `sandbox-fallback=false`, `build-users-group=nixbld`, `max-jobs=1`, and
  `use-cgroups=true`.
- A regular derivation could not reach `cache.nixos.org`.
- A fixed-output derivation reached the same endpoint and completed only after
  Nix verified the declared SHA-256 output hash.
- The build cgroup was created and removed. In Docker's cgroup subtree,
  `memory.max`, `cpu.max`, and `pids.max` were absent because those controllers
  were not delegated into the child. No resource cap is claimed.
- Without privileged container capabilities, daemon readiness failed and the
  harness emitted `complete:false` with exit 69 instead of falling back.
- Without the explicit `--approve-build` flag, readiness is reported as an
  incomplete run and all builds remain `pending_approval`. The flag proves a
  per-invocation UX gate only; it is not a production approval receipt.

## Observed native macOS lane

- Date: 2026-08-08.
- Host: macOS 26.6 (`arm64`), full Xcode 26.6 selected.
- Nix: pinned and checksum-verified Nix 2.34.8, multi-user daemon managed by
  launchd, with an encrypted APFS `/nix` volume.
- The daemon used `sandbox=true`, `sandbox-fallback=false`, group `nixbld`,
  and `_nixbld1..32`; a build executed as `_nixbld1` (UID 351).
- The raw socket parent was observed as `root:pkg-nix-broker` mode `0750`.
  The ordinary console user was denied at socket traversal, while the
  unprivileged `pkg-nix-broker` account reached the daemon and was untrusted.
- A regular derivation could not reach `cache.nixos.org`.
- A fixed-output derivation reached the endpoint and completed only after Nix
  verified the declared SHA-256 output hash.
- A run without `--approve-build` performs readiness only. The approved flag
  is consumed for one invocation and is evidence of the spike UX gate, not a
  production approval receipt.
- macOS has no cgroups and no per-build memory/CPU/IO cap is claimed. The
  configured timeout, silent timeout, log-size bound, and one-job policy remain
  the honest stock-Nix boundary.

## Not established

- Bare-metal Linux or systemd service behavior.
- The final product launchd labels/bundle, installer signing, or notarization.
- Reboot persistence of the encrypted managed store and broker-only socket
  permissions; the observed launchd services were exercised in the live boot.
- Service-manager defense-in-depth ceilings.
- Production machine-global build admission and cryptographic approval
  receipts; those belong to the broker/build-engine milestones.
- Disk, free-space, and load preflight thresholds.

Therefore this evidence does not accept DR-005 or unlock PR-26 by itself. It
does validate the Linux Docker and native macOS sandbox/build-user lanes plus
the core regular-versus-fixed-output network model under Nix 2.34.8. Required
architecture/security signoff remains separate.
