---
Status: Accepted
---

# Broker and Root Helper privilege split

## Context

Repair and GC-root writes need privilege, but the Nix daemon protocol cannot execute `repairPath`. Nix 2.34.8 reports it unsupported over the daemon, even for root. A single privileged service would be too broad.

## Decision

Split steady-state authority into an unprivileged singleton broker and a privileged root helper. The broker is the sole general daemon client and sole spawner of the bundled `nix` CLI; it is a daemon `allowed-user` and never a `trusted-user`. After explicit privileged install or uninstall, the root helper is the sole steady-state root filesystem writer for GC roots, service control, runtime upgrade, and `/nix` ownership. It is also the sole steady-state local-store repair executor, bound by an expiring single-use capability (D-19).

## Consequences

- Root stays the sole `trusted-user`. The broker cannot escalate through the daemon.
- Install and uninstall remain separate, explicit privileged entry points.
- Repair runs in two phases: read-only verify through the broker, then cache-only repair or an approved local rebuild through the helper.
- Machine-global build and GC admission are broker-internal in-memory gates, not filesystem locks.

## Rejected Alternatives

- **One privileged service doing all work.** Too broad a privilege boundary; the broker stays unprivileged (plan 00 D-19).
- **Repair through the daemon protocol.** Impossible: `repairPath` is unsupported over the daemon, so the helper pins `--store local` (plan 00 D-19).
- **Filesystem locks for machine-global build/GC admission.** A single broker cannot represent independent shared holders portably (plan 00 D-19/INV-08).
