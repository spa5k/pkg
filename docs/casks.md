# Cask catalog source (`nix/casks`)

This directory is the official `cask:` catalog source for `pkg`. It turns a
generated subset of Homebrew Cask metadata into ordinary Nix packages.
Daily use — `search`, `info`, `install`, `upgrade`, `remove` — runs no
Ruby and installs no Brew. The only Ruby path is `pkg tap add` and
`pkg tap update`: they convert a pinned public tap revision locally, with
Nix-supplied pinned Ruby, inside an ordinary sandboxed Nix derivation (see
[Saved public tap catalogs](#saved-public-tap-catalogs)). There is no
import-from-derivation, no hosted API, no WASM runtime, no formula solver,
no per-app allowlists, and no second installer.

The base design is the OpenSpec change
[Generate the Cask catalog with a Rust tool](../openspec/changes/generate-cask-catalog-with-rust/proposal.md).
Public tap ingestion is specified in
[Import public cask taps locally](../openspec/changes/import-public-cask-taps/proposal.md).

## What this source is

A maintainer-run Rust generator (`tools/cask-catalog`) reads a pinned
offline Homebrew Cask JSON snapshot and emits one deterministic catalog file
(`nix/casks/catalog/catalog.json`, schema `pkg-cask-catalog/3`). The Nix
flake reads that committed data with plain `builtins.fromJSON` and builds
generic packages from it. The client converts nothing per install.

- The input is pinned exactly: source URL, revision, content SHA-256, and the
  upstream data license are recorded in the catalog's provenance.
- Generation is deterministic and atomic: same pin plus same generator
  version always produce byte-identical output, and a failed run replaces
  nothing.
- The only flake input is a pinned `nixpkgs` revision. The generated catalog
  is committed data, not a live service.

Schema 3 is source-qualified:

- `inputs` maps each source to its provenance. The official JSON source is
  `homebrew/cask` and is retained; official metadata is not re-evaluated
  from raw Ruby.
- Entries are keyed by the qualified identity `owner/tap/token`. Each entry
  records its `source`, its bare `token`, file provenance, and per-target
  status. The same bare token from different sources coexists with no
  precedence rule.
- Nix attributes use one quoted source-qualified segment holding the
  escaped full identity:
  `packages.<system>."owner/tap/token"`. Escape in one pass: `_`
  becomes `_u_`, `.` becomes `_d_`, other characters stay unchanged.
  Example: `example/tap/tool@1.2` becomes
  `packages.<system>."example/tap/tool@1_d_2"`. A normal token needs no
  escaping. The mapping is reversible; catalog and search keys stay
  original.

## Targets and eligibility

The catalog names exactly two target systems: `aarch64-darwin` and
`x86_64-linux`. The declared macOS baseline is `15.7.7`. A raw saved tap
catalog names `targets = [<native system>]` only — the system it was
imported for; the ordinary official JSON catalog keeps both targets.

Each token carries one status per target system:

- `eligible` — the generator found a plan a generic builder can express.
  This is a metadata claim, not a verification promise. The first build
  proves the payload; a raw `.pkg` plan must still pass the strict payload
  script and layout checks at build time.
- `excluded` — with a machine-readable reason (for example
  `installer-script`, `formula-dependency`, `minimum-os`,
  `unsupported-platform`) and optional detail.

Public tap sources add their own recorded rejections, for example
`ruby-load-error` with a `staged_path` / `staged-path-during-source-eval`
detail (payload-dependent metadata; sticky even when the source rescues
the error), `active-install-operation` (installer scripts and required
callbacks; a legacy deprecated callback may instead fail the reader load
as `ruby-load-error`), and `private-network-destination` (fetch policy).
Callbacks and structured steps are preserved in metadata when the record
loads, and are never executed.

There are no package allowlists and no per-token flags. The full catalog
output and the index state eligibility only; they never state compatibility.
Verification results live in `docs/verification/` as observational records.

Known scope limits, by design:

- All records with Homebrew formula dependencies are excluded as
  `formula-dependency`. No nixpkgs names are guessed.
- All records with cask dependencies are excluded as
  `cask-dependency-integration` for this version. There is no dependency
  closure engine and no second dependency manager; the native Nix lifecycle
  stays single-package.
- Fonts, services, drivers, and privileged installers are unsupported.
- Linux targets include only casks that declare compatible binaries for
  them. This is not a promise about arbitrary brew Formula dependencies
  and not a promise that every tap supports Linux.
- Linux AppImage and binary support was verified headless only (for example
  `1password-cli` 2.39.0 and KOReader `v2026.07.1 --help`); no GUI launch is
  claimed.
- A direct `.AppImage` download declared as one Linux `binary` uses the
  same Nix wrapper as `app_image`. Its declared binary name is preserved.
  AppImage binaries inside archives, or mixed with other artifacts, are
  excluded. The wrapper supplies the runtime without host FUSE.
- History note: the alpha Raycast exclusion came from a hidden bundle
  `Info.plist` found in on-device verification, not from visible cask
  metadata. That is why the generic build-time minimum check exists, and why
  eligibility is not a launch promise.

## Outputs

- `packages.<system>."<escaped identity>"` — one lazy derivation per eligible
  qualified identity and target system, exposed as one quoted attribute
  segment holding the deterministic escaped full identity (one pass:
  `_` -> `_u_`, `.` -> `_d_`, other characters unchanged). In this
  directory the source is `homebrew/cask`, so identities
  look like `homebrew/cask/<token>`; identities without
  `_` or `.` stay unchanged by the escape. Install, list, upgrade, and
  remove go through the same native profile path as Nixpkgs packages.
- `catalogIndex` — the client status index, one cheap eval output with no
  per-system attribute:

  ```sh
  nix eval --json <casks-source>#catalogIndex
  ```

  The envelope carries the per-source provenance from `inputs` plus
  `systems.<system>.entries.<owner/tap/token>` records of `source`, bare
  `token`, `name`, `description`, `version`, `homepage`, `status`, `kind`,
  `reason`, and `detail`. Null rules: `name`, `description`, `version`, and
  `homepage` are string or null; eligible entries have a non-null `kind` and
  null `reason`/`detail`; excluded entries have a null `kind` and a non-null
  `reason`. Build plans are not exposed to the client. The client checks
  `targets` before reading any system data.
- `catalogStatus` — provenance plus per-system counts (total, eligible,
  excluded, by reason) for coverage reporting.

## How the client uses the index

- Search reads `catalogIndex` once per query and filters locally with a Rust
  regex, matched case-insensitively against the token, the name, and the
  description. The Nixpkgs lane keeps passing patterns to `nix search`
  unchanged. See [commands](commands.md) for both grammars.
- Search lists eligible entries only. Excluded tokens stay discoverable
  through `pkg info`, which shows the recorded reason and detail.
- A bare name resolves to a cask only when it equals one bare token
  exactly and that token is unique across the official catalog and every
  saved tap catalog. Excluded entries count as matches. An ambiguous token
  lists the qualified choices; no source is picked by ordering. A display
  name is never a bare-name resolution.
- A saved source catalog that is unreadable or malformed fails bare-token
  resolution for the whole pass. The failed source is reported. It is
  never silently ignored in favor of another source with the same token.
- A qualified `cask:owner/tap/token` ID resolves only inside its named
  source. An unknown qualified source fails with an explicit
  `pkg tap add owner/tap` instruction; it is never reinterpreted as Nix
  code.
- `info` and `install` resolve qualified identities to the ordinary
  `packages.<system>."<escaped identity>"` attribute — one quoted segment
  holding the escaped full identity. Profile human labels are decoded
  for display. A native manifest strips quotes from attributes, so a raw
  dotted full identity installs but breaks native upgrade as a nested
  lookup; the escaped attribute avoids that. Parent Nix-only probes
  (2026-09-29) passed native install, upgrade, and rollback with the
  escaped attribute on Linux and macOS. Product encoding tests pass on
  both hosts (encoded-segment round trips, single quoted-segment
  installs, and references that survive encoded paths).
- Normal `search`, `info`, and `install` of saved catalogs never execute
  source Ruby and never prompt.
- Install refuses excluded tokens with the recorded reason and refuses
  unknown tokens. Nothing invalid becomes eligible.
- Installs keep the original moving source reference, so a normal native
  upgrade re-resolves to the current generated catalog. Rollback, removal,
  and launcher sync use the ordinary native profile path. Homebrew cleanup
  hooks (`zap`, `uninstall`) never run.

## Saved public tap catalogs

Users add public Homebrew Cask taps locally. The command grammar is in
[commands](commands.md):

```sh
pkg tap add goreleaser/tap
pkg info cask:goreleaser/tap/mcp
pkg install cask:goreleaser/tap/mcp   # only if info reports it eligible
```

This public tap ships binaries, manual pages, and shell completions.
Native imports and the package lifecycle were tested on both supported
systems. Further Linux tests downloaded packages from three public taps
and ran real commands. See the
[public-tap verification summary](verification/public-taps.md) and the
[real installation report](https://github.com/spa5k/pkg/blob/913dedb3131a3f1db4c3ab385e3dc4ca495c21f3/docs/verification/public-taps-real-installs-2026-09-29/README.md)
for the exact versions and limits.

### Consent and commands

- `SOURCE` is a public GitHub `owner/tap` repository. The canonical
  repository is `owner/homebrew-<tap>`; the common short source
  `owner/tap` maps to `owner/homebrew-tap`. Arbitrary URLs are not
  accepted in this version.
- The first `tap add` for an origin shows the canonical repository and the
  approval scope, then asks for consent. Consent is remembered per origin.
  No fetch, import, or Ruby runs before it.
- A noninteractive `tap add` without prior consent must pass `--trust`.
  Without it, nothing runs and nothing changes.
- Only `tap add` and `tap update` execute tap Ruby, under the same
  restrictions. `pkg update` refreshes discovery data only; it does not
  refresh saved tap catalogs.
- `tap remove` blocks future use of a source. It does not uninstall
  packages. After a removal, `pkg upgrade --all` filters removed-source
  references and forwards the remaining entries by explicit native entry
  IDs (not native `--all`); an explicit upgrade of a removed-source entry
  refuses.

### Ruby runs in Nix, not on the host

Tap Ruby and the upstream Homebrew reader are pinned and supplied by the
Nix runtime. They are evaluated inside an ordinary sandboxed Nix
derivation that is not a fixed-output derivation. The conversion emits
ordinary Nix packages. It is not a Homebrew installation and adds no Brew
lifecycle. Tap-supplied Nix files are never evaluated. Saved flake
evaluation and package builds use no import-from-derivation.

### Local state and publication

Each saved tap lives in a per-user state directory. Each source publishes
into a stable real directory; Nix rejects a symlinked flake root. A new
generation is swapped in by one atomic directory exchange. A failed
refresh preserves the previous catalog bytes. Old generation outputs are
retained, so native rollback keeps working. A catalog path with special
characters is referenced as a percent-encoded path URI.

Updating a tap's metadata does not upgrade installed packages. Run
`pkg tap update`, then `pkg upgrade ENTRY` or `--all`.

### Sandbox prerequisite

The product checks actual daemon behavior with fresh probes before every
raw import and fails closed when isolation is insufficient. It never
edits the host daemon and never adds `trusted-users` entries. The daemon
settings below are a setup prerequisite applied by the host owner.

Parent Nix-only evidence (2026-09-29):

- Linux: a strict build denied a host-file read, an outside-directory
  write, and a live host-loopback connect.
- macOS as shipped: the daemon ran with `sandbox = false`. An ordinary
  user's `--option sandbox true` was ignored, and all three probes were
  allowed.
- macOS disposable VM: all three probes were denied only after the daemon
  ran with `sandbox = true`, `sandbox-fallback = false`, a minimal
  `sandbox-paths` set, the derivation set
  `__darwinAllowLocalNetworking = false`, and the LaunchDaemon gave builds
  a private `TMPDIR` (`/nix/var/nix/builds`, `root:wheel 0755`). The
  default Darwin global tmp grant can expose files outside the build.

These are Nix-only proofs for the daemon settings themselves. The
product reader runs, the Linux product refusal of
`sandbox-fallback = true`, and the final Linux whole import passed. See
the
[OpenSpec design](../openspec/changes/import-public-cask-taps/design.md)
and the
[verification summary](verification/public-taps.md).

### Trust limits

- Source approval records consent for running tap Ruby during import. It
  does not certify the installed application.
- The Nix build sandbox ends at build time. It does not surround the app
  after launch.
- Payload checksums prove bytes, not publisher trust.
- Vendor signatures and Gatekeeper assessment are preserved. `pkg` never
  removes quarantine flags and never re-signs apps.
- The public app assessment gate covers public-tap vendor `.app` bundles
  at the app exposure path. It requires Gatekeeper assessments to be
  enabled (`spctl --status` reports `assessments enabled`) before any
  codesign/spctl assessment; the exact-enabled status gate is
  unit-proven on both hosts, the actual native status was recorded as
  `assessments enabled` with codesign and spctl assessment of the
  cached signed app accepted (Notarized Developer ID), and the final
  native check proved
  signed-app exposure, unsigned rejection, and launcher preservation.
  A native globally-disabled-Gatekeeper retest is not claimed (the
  reject-disabled clause is unit-proven); it stays
  an explicit limit. CLI binaries and ordinary Nixpkgs apps are
  outside this gate, and no GUI launch or application runtime isolation
  is claimed; full GUI/runtime/app malware analysis is out of scope, not
  pending.

## Catalog update procedure

This procedure stays the maintainer path for the official catalog and its
pinned JSON snapshot input. Updates are maintainer-run and reviewed; no
scheduler exists. Run these commands from the repository root. This
example reproduces the current pin. For an update, supply the new exact
revision and its expected SHA-256:

    cargo run --locked -p cask-catalog -- fetch \
      --revision 245947c0b920cbe83f4003bb7c6be737352c8314 \
      --sha256 1f19ecee6bad49e35d3f96cf295fd728db2ce8f7cdd95efdbcf250e5f4529251
    cargo run --locked -p cask-catalog -- generate
    # review the diff, then commit input.json and catalog.json together

`fetch` refuses to replace the pin when the downloaded bytes do not match
the expected hash. `generate` is offline and deterministic: the committed
pin plus the generator version always produce the same catalog bytes. CI
regenerates from the committed pin and fails on any diff.

Verify evaluation and one build before merging (import-from-derivation
disabled):

    nix eval --no-allow-import-from-derivation --json ./nix/casks#catalogIndex
    nix build ./nix/casks#packages.aarch64-darwin."homebrew/cask/cursor"

Installed entries keep their original references. Selecting newer metadata is
done by native profile upgrade, not by rewriting installed references.

## Historical snapshot: PR #81 verification (2026-09-28)

The numbers below are the PR #81 verification snapshot of the schema-2
official catalog. They are kept as a dated record of that verified state.
They are not new public-tap results, and no new counts are claimed for
schema 3 until it is regenerated and verified. The committed catalog was
generated from input revision
`245947c0b920cbe83f4003bb7c6be737352c8314` (7709 records). Metadata-eligible
counts — eligibility only, not build verification:

| Target | Eligible | Total | Largest exclusion reasons |
| --- | --- | --- | --- |
| `aarch64-darwin` | 3144 | 7709 | `missing-checksum` 2124, `deprecated` 689, `disabled` 332 |
| `x86_64-linux` | 160 | 7709 | `unsupported-platform` 3941, `missing-checksum` 1606, `deprecated` 690 |

Evidence on hand (see the
[catalog verification record](verification/2026-09-28-rust-cask-catalog.md),
the [client quality record](verification/native-client-quality-2026-09-28.md),
and the [detailed plan](plans/rust-cask-catalog.md)):

- All 81 Rust tests passed. Formatting, Clippy, rustdoc, dependency policy,
  documentation links, and strict OpenSpec validation passed.
- The Nix builder suite passed 13 checks. Real Linux runs verified
  `op` 2.39.0 and KOReader `--help` (`v2026.07.1`). GUI interaction was not
  checked.
- On the macOS VM (15.7.7, Apple silicon), Cursor 3.17.19 and Raycast
  1.104.25 built with valid deep/strict signatures. Cursor's CLI and op ran.
  Cursor started an app process. GUI interaction was not checked.
- Both native client journeys passed search, info, install, update,
  upgrade, rollback, and removal. The final profiles were empty. Controlled
  version-label changes verified source re-resolution on both systems.

## Limits

- Two target systems only. Other systems need new verification, not a list
  edit.
- Some applications reject Nix store locations or self-update against
  immutable files. Such apps surface as build or runtime failures, not as
  catalog changes; no per-token workaround exists.
- Rollback restores the packaged app version, not application data such as
  preferences, accounts, or databases.
- Public taps import only the generic artifact subset: GitHub release
  archives, raw release binaries, app bundles, and the existing safe plans.
  Installer scripts, active callbacks, unsupported dependencies, and
  payload-dependent metadata are rejected with recorded reasons. No tap is
  promised to import successfully.
