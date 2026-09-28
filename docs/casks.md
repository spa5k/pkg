# Cask catalog source (`nix/casks`)

This directory is the `cask:` catalog source for `pkg`. It turns a broad,
generated subset of Homebrew Cask metadata into ordinary Nix packages. There
is no Brew, no Ruby evaluation, no import-from-derivation, no hosted API, and
no second installer.

The active design is the OpenSpec change
[Generate the Cask catalog with a Rust tool](../openspec/changes/generate-cask-catalog-with-rust/proposal.md).

## What this source is

A maintainer-only Rust generator (`tools/cask-catalog`) reads a pinned
offline Homebrew Cask JSON snapshot and emits one deterministic catalog file
(`nix/casks/catalog/catalog.json`, schema `pkg-cask-catalog/2`). The Nix
flake reads that committed data with plain `builtins.fromJSON` and builds
generic packages from it. The client converts nothing per install.

- The input is pinned exactly: source URL, revision, content SHA-256, and the
  upstream data license are recorded in the catalog's provenance.
- Generation is deterministic and atomic: same pin plus same generator
  version always produce byte-identical output, and a failed run replaces
  nothing.
- The only flake input is a pinned `nixpkgs` revision. The generated catalog
  is committed data, not a live service.

## Targets and eligibility

The catalog names exactly two target systems: `aarch64-darwin` and
`x86_64-linux`. The declared macOS baseline is `15.7.7`.

Each token carries one status per target system:

- `eligible` — the generator found a plan a generic builder can express.
  This is a metadata claim, not a verification promise. The first build
  proves the payload; a raw `.pkg` plan must still pass the strict payload
  script and layout checks at build time.
- `excluded` — with a machine-readable reason (for example
  `installer-script`, `formula-dependency`, `minimum-os`,
  `unsupported-platform`) and optional detail.

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
- Linux AppImage and binary support was verified headless only (for example
  `1password-cli` 2.39.0 and KOReader `v2026.07.1 --help`); no GUI launch is
  claimed.
- History note: the alpha Raycast exclusion came from a hidden bundle
  `Info.plist` found in on-device verification, not from visible cask
  metadata. That is why the generic build-time minimum check exists, and why
  eligibility is not a launch promise.

## Outputs

- `packages.<system>.<token>` — one lazy derivation per eligible token and
  target system. Install, list, upgrade, and remove go through the same
  native profile path as Nixpkgs packages.
- `catalogIndex` — the client status index, one cheap eval output with no
  per-system attribute:

  ```sh
  nix eval --json <casks-source>#catalogIndex
  ```

  The envelope carries the same provenance fields as the catalog plus
  `systems.<system>.entries.<token>` records of `token`, `name`,
  `description`, `version`, `homepage`, `status`, `kind`, `reason`, and
  `detail`. Null rules: `name`, `description`, `version`, and `homepage`
  are string or null; eligible entries have a non-null `kind` and null
  `reason`/`detail`; excluded entries have a null `kind` and a non-null
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
- A bare name resolves to a cask only when it equals one token exactly. A
  display name is never a bare-name resolution.
- `info` and `install` resolve tokens to the ordinary
  `packages.<system>.<token>` attribute. Tokens with `@`, `+`, or `.` in the
  name stay one quoted attribute segment.
- Install refuses excluded tokens with the recorded reason and refuses
  unknown tokens. Nothing invalid becomes eligible.
- Installs keep the original moving source reference, so a normal native
  upgrade re-resolves to the current generated catalog. Rollback, removal,
  and launcher sync use the ordinary native profile path. Homebrew cleanup
  hooks (`zap`, `uninstall`) never run.

## Catalog update procedure

Updates are maintainer-run and reviewed; no scheduler exists. Run these
commands from the repository root. This example reproduces the current pin.
For an update, supply the new exact revision and its expected SHA-256:

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
    nix build ./nix/casks#packages.aarch64-darwin.cursor

Installed entries keep their original references. Selecting newer metadata is
done by native profile upgrade, not by rewriting installed references.

## Current catalog state and evidence

The committed catalog was generated from input revision
`245947c0b920cbe83f4003bb7c6be737352c8314` (7709 records). Metadata-eligible
counts — eligibility only, not build verification:

| Target | Eligible | Total | Largest exclusion reasons |
| --- | --- | --- | --- |
| `aarch64-darwin` | 3144 | 7709 | `missing-checksum` 2124, `deprecated` 689, `disabled` 332 |
| `x86_64-linux` | 160 | 7709 | `unsupported-platform` 3941, `missing-checksum` 1606, `deprecated` 690 |

Evidence on hand (see [verification records](verification/) and the
[detailed plan](plans/rust-cask-catalog.md)):

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
