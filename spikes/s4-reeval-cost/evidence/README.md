# S4 Complete Real evidence — 2026-08-09

These artifacts are the reviewed inputs to DR-004. Both reports validate as
`mode=real`, `completeness=complete`, `harnessOnly=false`, detected Nix exactly
`2.34.8`, all five canonical scenarios complete, and zero failures.

## Native macOS arm64

- Host: macOS Darwin 26.6, `aarch64-darwin`, 10 cores.
- Nix binary: `/nix/var/nix/profiles/default/bin/nix`.
- Store: fresh user-owned local chroot store selected by
  `--store-root /private/tmp/pkg-s4-macos-store`; the service-only managed daemon
  socket was not contacted.
- Cache state: one verified online prefetch, then source-warm/process-cold
  `--offline` evaluation samples.
- Artifacts: [`macos-aarch64-2026-08-09/report.json`](macos-aarch64-2026-08-09/report.json)
  and [`summary.md`](macos-aarch64-2026-08-09/summary.md).

## Native Linux arm64

- Host: Docker Desktop Linux VM, `aarch64-linux`, 10 cores; container image
  `nixos/nix:2.34.8`.
- Harness build: `rust:1.96-alpine`, producing a native musl arm64 runner from
  the locked spike workspace.
- Nix binary: `/root/.nix-profile/bin/nix` inside the disposable container.
- Store: the container's disposable local `/nix` store.
- Cache state: one verified online prefetch, then source-warm/process-cold
  `--offline` evaluation samples.
- Artifacts: [`linux-aarch64-2026-08-09/report.json`](linux-aarch64-2026-08-09/report.json)
  and [`summary.md`](linux-aarch64-2026-08-09/summary.md).

## Scope

No package outputs were built or realized. The original native x86_64 reference
lane remains a PR-32 baseline-expansion task before GA. Emulated QEMU timings
are deliberately excluded from the accepted performance evidence.
