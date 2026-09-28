# Import raw Ruby casks from public taps

Status: proposed, 29 September 2026. This is a plan, not an implementation.
Baseline: `f6f99a748653861302876dfe127dfc6bf498d323`, which merged
[PR #81](https://github.com/spa5k/pkg/pull/81).

## Why

The merged catalog generator reads a pinned JSON snapshot. It cannot read
a vendor's Ruby casks or combine taps with the same token. Adding public
taps must not create a Ruby interpreter, package allowlist, hosted catalog
service, or second package manager inside the client.

Homebrew already owns Ruby Cask semantics. Use its pinned reader during
catalog updates. Rust keeps validation and catalog generation. Nix keeps
builds and the package lifecycle. The
[source research](../../../docs/research/2026-09-29-raw-ruby-public-taps.md)
records upstream interfaces and their limits.

## What Changes

- Add a declarative source registry and lock for public Git taps and pinned
  JSON snapshots. A source entry is configuration, not package-specific code.
- Add a small exporter around pinned Homebrew Ruby code. Run it only in
  isolated maintainer environments. Use native ARM64 macOS and x86-64 Linux
  runners where target behavior matters. No Ruby runs during client commands
  or Nix evaluation.
- Preserve unsupported operations in the export. Reject required install
  steps, payload-dependent metadata, and missing target evidence. Do not
  silently turn incomplete metadata into an eligible package.
- **BREAKING:** Emit `pkg-cask-catalog/3` with source provenance and qualified
  package identities. Same-token entries from different taps coexist.
- Keep one generated flake and one client Cask source. Ordinary Nix install,
  upgrade, rollback, and removal remain the lifecycle. Publishers can generate
  their own catalog flake without a hosted API.
- Replace the JSON-mirror-only fetch contract. Keep the existing JSON reader
  as a real input adapter while the official tap moves to raw-source export.

## Capabilities

### New Capabilities

- `tap-source-ingestion`: pinned sources, isolated Ruby export, bounded error
  handling, export snapshots, and atomic catalog publication.
- `namespaced-cask-catalog`: source-qualified identity, schema version 3,
  ambiguous-name handling, Nix attributes, and client source provenance.

### Modified Capabilities

None in the canonical specs directory. Earlier changes remain unarchived.
On implementation, this proposal replaces the single-input and token-key
rules in `generate-cask-catalog-with-rust`. Archive both changes in order.
It does not reopen the deleted package-state engine.

## Impact

The changes are confined to `tools/cask-catalog`, a small maintainer Ruby
exporter and its runner recipe, `nix/casks`, and the client's catalog identity
and decoding code. Package builders keep their current artifact contract.
There is no runtime Brew installation, tap server, dependency solver, or
background updater. No schema-2 compatibility layer is planned.

The [design](design.md) is authoritative. The [tasks](tasks.md) are all open.
The [readable plan](../../../docs/plans/public-cask-taps.md) and
[HTML plan](../../../artifacts/public-cask-taps-plan.html) explain the rollout.
