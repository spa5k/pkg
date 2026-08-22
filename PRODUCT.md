# Product

<!-- impeccable:product-schema 1 -->

## Platform

Linux and macOS (terminal CLI).

## Stack

**Current alpha:** Rust. `pkg` is a Rust CLI for Linux and macOS. It drives a
bundled, pinned Nix runtime as a subprocess. Breaking changes can occur before
v1.

**Accepted target:** `pkg` will install Base Nix through a pinned Determinate
Nix Installer. This target is not delivered. The
[active plan](plans/determinate-nix-stacked-prs.md) defines the proof and
cutover work.

## Users

Developers and terminal users who want familiar brew/paru-style package management without learning or configuring Nix.

## Product Purpose

Give terminal users safe, familiar package workflows by hiding a product-managed, pinned Nix runtime behind commands they already know. Success means a user can search, install, upgrade, and recover without ever touching raw Nix.

## Positioning

A product-managed, pinned Nix runtime hidden behind safe familiar commands. In
the accepted target, Determinate will own Base Nix installation. `pkg` will
continue to own package policy, locks, generations, UX, and package recovery.

## Operating Context

Daily terminal tasks: search, info, install, remove, update, upgrade, pin, history, rollback, doctor, gc, repair.

## Current Alpha Capabilities and Constraints

- No raw Nix surface is ever exposed: no flake URLs, overlays, `NIX_PATH`, `--impure`, user substituters, or trust keys.
- The catalog is exactly pinned Nixpkgs revisions; no floating channels or overlays.
- `cache.nixos.org` is the only artifact cache in V1.
- Cache substitution is always attempted first; on a cache miss, native current-system local builds are allowed on both Linux and macOS, only after a deterministic preview and explicit, single-operation, journaled approval bound to the canonical `BuildPlan` digest and policy version (cancel/no approval is the default and leaves the generation unchanged; `--yes` pre-approves that one operation non-interactively but the same preview is still emitted and journaled, and `--yes` never overrides a hard refusal). Evaluation and planning never realize outputs (Nix 2.34.8 evaluates the exact pinned installable with `nix derivation show --recursive` and import-from-derivation disabled); `nix build` begins only at acquire — pure substitution first, then an approved local build if needed. Linux uses the managed `nixbld` build group (`nixbld*` users) and Nix's own Linux namespace/chroot sandbox; macOS uses the managed `nixbld` build group (`_nixbld*` users) and Nix's Darwin sandbox; both run with `sandbox=true` and `sandbox-fallback=false`, and Darwin isolation primitives differ from and are narrower than Linux's. Stock Nix 2.34.8 provides **no** per-build memory/CPU/IO cap: `max-jobs=1` bounds concurrent derivations per client/connection (so `pkg` adds a machine-global local-build admission lease across users on top of it — a second build op waits or cancels, then revalidates approval/readiness once it acquires the lease), the daemon enforces `timeout`/`max-silent-time`/`max-build-log-size` bounds, Nix's Linux per-build cgroup (experimental feature `cgroups`) is for process grouping, lingering-process cleanup, and CPU accounting rather than a resource cap or security isolation, and preflight checks disk/free-space/load; resource exhaustion is a disclosed residual. Service-manager ceilings (systemd on Linux; launchd on macOS) are Pending defense-in-depth, not accepted enforcement. Build only the current native system (`x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, or `aarch64-darwin`): no Rosetta, cross-compilation, emulation, or remote builds. Fail closed if the managed builder pool or sandbox verification is unhealthy. `ACQUIRE_NO_BINARY` is reserved for a closure that cannot be built for the current system or is blocked by policy; a normal cache miss on either OS does not return it.
- **Repair is a privileged, user-initiated, two-phase store operation:** **Phase 0** broker read-only `nix store verify --recursive` over the full closure reachable from the selected output roots; **Phase A** signed-cache-only repair via the root helper at `max-jobs=0`/no builders, automatic and approval-free; **Phase B**, only on a cache miss with a valid deriver, the ordinary deterministic build preview + single-operation approval, where non-interactive runs exit `ACQUIRE_NEEDS_APPROVAL` (68) rather than building (mirrors the install build gate). A clean Phase 0 exits success; any damage is **warned as non-atomic** — affected commands may be temporarily unavailable or observe partial content because Nix repair deletes-then-restores a cache hit and moves-aside-then-replaces a rebuild — and marks the closure unknown/unhealthy, blocking dependent mutations until a fresh read-only verify confirms every target clean (no path is marked repaired before then). The CLI surfaces only sanitized package/per-path outcomes and the sanitized public log reference; raw helper/Nix logs, raw argv, derivers, and unapproved store-path detail stay service-private. `--verify-only` is purely read-only. Activation-forest rematerialization and manifest/lock recovery are separate, subordinate Rust/state paths, not the definition of store repair.
- Exclusive managed ownership of `/nix`, with fail-closed detection of any existing unmanaged Nix (never auto-deletes user state).
- Target platforms: `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin`.

## Brand Commitments

- Name: `pkg` is a working codename, not final.
- Voice: concise, calm, factual.

## Evidence on Hand

- The [plan index](plans/README.md) identifies the active stacked-PR plan. The
  earlier custom private-Nix design is historical and is not normative.
- The Rust product is implemented as an internal alpha. It has not launched. See `README.md` for current platform status.

## Product Principles

1. Familiar surface.
2. Hidden complexity.
3. Atomic, recoverable state.
4. Honest trust boundaries.
5. Broad Nixpkgs discovery without a curated catalog.
