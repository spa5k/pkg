# Implement the accepted pkg audit repairs

Follow-up on the completed
[import-public-cask-taps](../import-public-cask-taps/proposal.md),
[harden-native-package-lifecycle](../harden-native-package-lifecycle/proposal.md),
and
[amortize-search-with-revision-snapshots](../amortize-search-with-revision-snapshots/proposal.md)
work, 1 October 2026. Baseline: merge commit `120e9ed` on `main`.

## Why

An internal audit of the shipped code found six accepted findings. None
changes the product direction. All are safety or structure repairs in
code we own: one fail-closed setup gap that can print or widen access to
secret settings, one ownership gap between the tap store and its
callers, one stringly typed catalog decision, one duplicated isolated
Nix environment, possibly one duplicated snapshot-loading lane, and
stale lint/CI comments plus missing panic protection.

## What Changes

1. Fail-closed Nix administrator setup, typed importer errors. The
   client refuses automatic administrator setup when the global
   `/etc/nix/nix.conf` is not world-readable, with manual review
   instructions, before any administrator step runs. The uncertain
   sudo-grep secrets probe and the automatic `chmod 0644` are deleted.
   Setup inspection distinguishes a missing, unreadable, and malformed
   configuration file and preserves the cause when inspection decides
   control flow. A required piped-stdin write failure in a setup step
   surfaces instead of being ignored. The importer seam
   `cask_catalog::tap::import` returns a small typed `ImportError`
   (`SandboxRefused` versus ordinary failure); the client offers its
   existing single setup retry only for the typed refusal, never for
   ordinary text that merely contains the refusal phrase.
2. Tap staging and publication ownership. The store returns one owned
   staged generation tied to its source; consuming `publish` settles the
   staging guard internally and returns a publication that owns the
   retained and scratch state with explicit `finish` and `undo`
   operations. No fallible rollback runs from `Drop`; an unfinished
   publication conservatively retains the old generation. Validation,
   consent, registry, and output stay in the command layer; storage
   cleanup moves into the store. The global registry then source-lock
   ordering, the lock scope through registry commit and undo, the
   two-rename crash gap, and the error and cleanup-warning fidelity are
   unchanged.
3. Typed catalog target decision. The internal per-target decision
   becomes one `TargetDecision` with an eligible variant that owns its
   plan and an excluded variant that owns its reason and detail; the
   summary kind is derived from the plan and the common version and
   homepage sit outside the variants. The emitted JSON contract
   (`pkg-cask-catalog/4`), its deterministic bytes, and the eligibility
   policy are unchanged.
4. One isolated Nix environment constructor. The config gate and the
   raw-export build share one private constructor for the environment
   and process setup only (cleared environment, fixed `PATH` and
   `LC_ALL`, private `HOME`/`XDG_CONFIG_HOME`/`NIX_USER_CONF_FILES`/
   `TMPDIR`, null stdin, piped output). Caller roots, arguments,
   deadlines, log caps, labels, and the early gate versus the fresh
   authoritative build gate stay separate. An environment sentinel test
   proves the whitelist and that ambient `NIX_CONFIG`, proxy, or fake
   credential variables never reach the isolated child.
5. Snapshot lane loading, only where a small helper pays for itself. The
   discovery lanes may share one private payload loader for snapshot
   identity, freshness, persistence, and the stale fallback, accepting
   the two concrete fetch operations. Row, filter, platform, query, and
   regex semantics stay with the lanes. If the cost exceeds the benefit,
   the rejection is recorded and the code stays.
6. Quality and CI repairs. Obsolete `clippy.toml` and
   `rust-toolchain.toml` comments that reference the deleted G-QUALITY
   and G-LINT gates are corrected. Focused production
   `unwrap_used`/`expect_used`/`panic` protection is added as
   individually selected restriction lints with reasoned, narrowly
   scoped test exceptions; the two actual production `expect`/`
   unreachable` sites are replaced on real domain evidence. The macOS
   smoke job lints its `cfg(macos)` code with Clippy, and the installer
   smoke check runs ShellCheck as CONTRIBUTING already requires. No
   advisory fetching is added; the offline bans/licenses/sources check
   stays.

## Non-goals

- No restoration of the deleted package engine, broker, privileged
  service, central index, approval service, old quality ratchets, or
  their tests.
- No blanket pedantic/nursery/restriction lint groups; individual lints
  only, each reasoned.
- No new error taxonomy beyond the one small typed importer error.
- No change to eligibility policy, catalog schema, Nix or client
  decoding, installer production design, or the macOS APFS app storage
  design.
- No macOS behavior proof from the Linux sandbox; macOS review stays
  with the parent.

## Impact

Rust interfaces in `pkg-cli` and `cask-catalog` change
(`setup`, `store`, `classify`/`emit`, `tap::import`). The affected
consumers are this repository's own client and catalog tool; external
usage is unknown.
