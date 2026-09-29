# Import public cask taps locally

Status: accepted 29 September 2026 and implemented. The
[implementation contract](implementation-contract.md) is authoritative.
The [design](design.md) is the technical source of truth. Baseline plan
code: `f6f99a748653861302876dfe127dfc6bf498d323` from
[PR #81](https://github.com/spa5k/pkg/pull/81). Delivered in
[PR #82](https://github.com/spa5k/pkg/pull/82); not merged or released.

## Why

The merged catalog generator reads one pinned JSON snapshot of the
official tap. It cannot ingest any other public tap, and users cannot add
one. The retired answer was a maintainer-only export pipeline plus a
raw-official cutover.

The accepted answer is local import with explicit approval. `pkg` adds a
public Homebrew Cask tap after consent, converts a pinned revision into
native Nix packages on the user's machine, and keeps the existing official
JSON catalog. Nix supplies the pinned Ruby and the upstream Homebrew
reader dependencies. Ruby runs only inside our ordinary sandboxed Nix
derivation. No Brew installation lifecycle, hosted API service, WASM
runtime, per-app code, or formula-to-cask conversion is introduced. Nix
keeps installed state.

## What Changes

- Add client commands `pkg tap add SOURCE [--revision SHA] [--trust]`,
  `pkg tap list`, `pkg tap update [SOURCE] [--revision SHA]`, and
  `pkg tap remove SOURCE`. First consent is shown the canonical repository
  and approval scope, must precede any tap Ruby execution, and is
  remembered per origin. Noninteractive runs require `--trust`.
- Add a backend importer library seam, `cask_catalog::tap`, that fetches
  the pinned public source without credentials, runs the pinned Homebrew
  reader Ruby in a sandboxed Nix derivation, captures metadata, and writes
  one generated schema-3 flake into a caller-owned staging directory. The
  client validates and publishes atomically. Failed refreshes preserve the
  previous source. Ordinary search of saved catalogs never runs Ruby and
  never prompts.
- Support a generic artifact subset: GitHub release archives, raw
  binaries, app bundles, and the existing safe artifact plans. Reject
  installer scripts, payload-dependent metadata, unsupported
  dependencies, and active installer actions explicitly, with recorded
  reasons.
- **BREAKING:** emit `pkg-cask-catalog/3` with `inputs` source provenance
  and qualified `owner/tap/token` identities. Same bare tokens from
  different taps coexist. No schema-2 compatibility layer ships.
- Keep the official JSON source, regenerated in schema 3 with source
  `homebrew/cask`.

## Capabilities

### New Capabilities

- `tap-source-ingestion`: consent-gated local import, credential-free
  public fetch, sandboxed Nix-provided Ruby evaluation, bounded failure
  handling, captured exports, and atomic catalog refresh.
- `namespaced-cask-catalog`: qualified identity, schema 3 only, ambiguous
  bare-token handling including excluded entries, one-quoted-segment
  escaped Nix attributes (reversible `_`/`.` mapping), and client source
  provenance.

### Modified Capabilities

None in the canonical specs directory. On implementation, this change
replaced the single-input and bare-token-key rules left behind by
`generate-cask-catalog-with-rust`. It does not reopen the deleted
package-state engine.

## Impact

Backend work stays in `tools/cask-catalog`, `nix/casks`, and the
raw-reader Nix/Ruby assets. Client work stays in `crates/pkg-cli`. The
documentation worker owns this OpenSpec change, the
`tools/public-tap-check` fixture harness, and the verification evidence.
Delivery is one reviewable implementation PR. No merge or release happens
without a later instruction. Verification is summarized in
[docs/verification/public-taps.md](../../../docs/verification/public-taps.md).
