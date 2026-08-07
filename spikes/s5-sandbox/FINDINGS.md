# S5 findings — managed-daemon sandbox and resource boundary

Status: **partial observed evidence; DR-005 remains Proposed**.

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

## Not established

- Native macOS `_nixbld` sandbox/build evidence.
- Bare-metal Linux or systemd service behavior.
- launchd behavior, Xcode/CLT readiness, or notarization.
- Service-manager defense-in-depth ceilings.
- Production machine-global build admission and cryptographic approval
  receipts; those belong to the broker/build-engine milestones.
- Disk, free-space, and load preflight thresholds.

Therefore this evidence cannot accept DR-005 or unlock PR-26 by itself. It does
validate the Linux Docker harness and the core regular-versus-fixed-output
network model under Nix 2.34.8.
