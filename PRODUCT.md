# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Stack

Delegated: the user explicitly requested a self-contained static HTML/CSS/JavaScript walkthrough artifact, so this stack applies only to that artifact. The actual product being demonstrated is a planned Rust CLI for Linux and macOS; no implementation exists yet.

## Users

Developers and terminal users who want familiar brew/paru-style package management without learning or configuring Nix.

## Product Purpose

Give terminal users safe, familiar package workflows by hiding a product-managed, pinned Nix runtime behind commands they already know. Success means a user can search, install, upgrade, and recover without ever touching raw Nix.

## Positioning

A product-managed, pinned Nix runtime hidden behind safe familiar commands — a division of labor no neighboring tool can each truthfully claim: Nixpkgs is the catalog, Nix handles resolution, verified substitutes, and builds, and Rust owns trust policy, locks, generations, UX, and recovery.

## Operating Context

Daily terminal tasks: search, info, install, remove, update, upgrade, pin, history, rollback, doctor, gc, repair.

## Capabilities and Constraints

- V1 is planning only; no implemented product exists.
- No raw Nix surface is ever exposed: no flake URLs, overlays, `NIX_PATH`, `--impure`, user substituters, or trust keys.
- The catalog is exactly pinned Nixpkgs revisions; no floating channels or overlays.
- `cache.nixos.org` is the only artifact cache in V1.
- Cache substitution is always attempted first; on a cache miss, native current-system local builds are allowed on both Linux and macOS, only after a deterministic preview and explicit, per-operation, journaled approval (cancel/no approval is the default and leaves the generation unchanged). Linux uses managed `nixbld` users, the Nix Linux sandbox, `sandbox=true`, `sandbox-fallback=false`, and cgroups/RLIMIT-backed caps where available. macOS uses managed `_nixbld` users, Nix's Darwin sandbox, `sandbox=true`, `sandbox-fallback=false`, and RLIMIT/disk/free-space/load safeguards; Darwin isolation primitives differ from and are narrower than Linux's (no cgroups). Build only the selected platform's native system (`x86_64-linux` / `aarch64-darwin`): no Rosetta, cross-compilation, emulation, or remote builds. Fail closed if the managed builder pool or sandbox verification is unhealthy. `ACQUIRE_NO_BINARY` is reserved for a closure that cannot be built for the current system or is blocked by policy; a normal cache miss on either OS does not return it.
- Exclusive managed ownership of `/nix`, with fail-closed detection of any existing unmanaged Nix (never auto-deletes user state).
- Target platforms: `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`, `aarch64-darwin`.

## Brand Commitments

- Name: `pkg` is a working codename, not final.
- Voice: concise, calm, factual.

## Evidence on Hand

- `plans/` is the only product-truth source; see `plans/00-overview-and-decisions.md` and `plans/README.md`.
- No implementation code exists. All versions, sizes, timings, and terminal outcomes shown in the artifact must be labeled illustrative, not measured.

## Product Principles

1. Familiar surface.
2. Hidden complexity.
3. Atomic, recoverable state.
4. Honest trust boundaries.
5. Broad Nixpkgs discovery without a curated catalog.
