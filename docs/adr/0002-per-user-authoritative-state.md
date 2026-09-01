---
Status: Accepted
---

# Per-user authoritative state over the shared machine store

> **Status note:** This decision remains accepted for per-user Package Lifecycle state. [ADR 0004](./0004-determinate-base-nix-lifecycle.md) partially supersedes only its machine-wide Base Nix ownership claim, including the Base Nix runtime, Base Nix service definitions, Base Nix configuration, Base Nix trust bootstrap, Vendor Receipt, and `/nix` lifecycle.

## Context

The managed Nix store and runtime are root-owned and machine-global. V1 had to choose between one shared package state for all users and per-user state on shared hosts (UD-00.4).

## Decision

Authoritative package state — manifest, lock, generations, activation, and journal — is per-user, keyed by OS uid, and owned by that user. Its production root is `$HOME/.local/share/pkg` on Linux and `~/Library/Application Support/pkg` on macOS, where HOME is the authenticated uid's system/passwd home. Root owns the machine-shared store, immutable runtime, service definitions, configuration, trust bootstrap, ownership receipts, and enclosing service directories. The unprivileged broker owns two distinct private domains: its `broker-home` and mutable authenticated channel/index/source datastore leaves beneath it, and the separate raw-log leaf `/var/lib/pkg/log/broker` on Linux or `/Library/Application Support/pkg/log/broker` on macOS. Users cannot read or write either domain (D-17, INV-10).

## Consequences

- Users are isolated from each other on shared hosts.
- Per-user mutation is serialized by a per-user filesystem lease. Machine-global build and GC admission stay broker-internal.
- GC roots are per-output and keyed by uid.
- Machine-global data does not have one owner: root owns service and trust assets, while the broker owns its private home/datastore and separate private raw-log leaf.
- `XDG_DATA_HOME` is not authoritative in this alpha. The broker, helper, root authorization, and uninstall bind the per-uid namespace to the system/passwd home. There is no fallback root. Explicit alternate roots are read-only inspection origins.

## Rejected Alternatives

- **A single shared profile for all users.** Makes all package state globally shared and weakens isolation; superseded by D-17 (plan 00 UD-00.4).
