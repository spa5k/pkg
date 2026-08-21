---
Status: Accepted
---

# Activation forest lives outside the Nix store

## Context

`pkg` must expose a working package environment without depending on Nix profile state. Nix's `nix profile` has no stable machine contract, so Rust owns activation. A natural choice was a Nix `buildEnv` store object.

## Decision

Activation is a deterministic symlink forest that Rust materializes outside `/nix/store`, under the per-user state. Activation invokes no Nix. `current` is a relative symlink into the forest. A `treeDigest` binds the sorted path-to-target records (D-18, INV-11).

## Consequences

- No Nix build runs at activation time.
- The forest is rebuildable from the generation record and its verified outputs.
- Collisions are resolved per file in Rust. Only abort, keep-first, and keep-last exist.
- Rollback re-materializes a fresh forest instead of reusing a retained one.

## Rejected Alternatives

- **Nix `buildEnv` store object.** Adds a per-generation Nix build and couples activation to the store (plan 00 D-18).
- **`nix profile` as authoritative.** No stable in-band machine contract (plan 00 §6.3).
- **Reuse the retained forest on rollback.** Breaks per-generation isolation that pruning and GC rely on (plan 05 §8.1).
