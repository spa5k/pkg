# Cask source (`nix/casks`)

This directory is the `cask:` catalog source for `pkg`. It turns a supported
subset of Homebrew Casks into ordinary Nix packages. There is no Brew, no
Ruby evaluation, no import-from-derivation, and no second installer.

## What this flake is

`nix/casks/flake.nix` wraps the pinned
[`brew-nix`](https://github.com/BatteredBunny/brew-nix) converter. All four
inputs are locked in `flake.lock`:

| Input | Role |
| --- | --- |
| `brew-nix` | Cask-to-derivation converter |
| `brew-api` | Cask metadata (`cask.json`) |
| `nixpkgs` | Package set the converter builds against |
| `nix-darwin` | Only used by brew-nix's own checks output; pinned for completeness |

The revisions published in `caskSupport` are read from the locked inputs at
evaluation time (`input.rev`). They are never copied into the flake by hand,
so they cannot drift from `flake.lock` when the lock is updated.

The `brew-api` metadata input is pinned explicitly in this flake. Upstream
warns that its default metadata input can be stale; `pkg` never relies on the
default. The converter reads `cask.json` with plain JSON import, so the whole
source evaluates with import-from-derivation disabled.

## Outputs

- `packages.aarch64-darwin.<token>` — one derivation per supported Cask.
  Install, list, upgrade, and remove go through the same native profile path
  as Nixpkgs packages. No Cask-specific installer exists.
- `caskSupport.aarch64-darwin` — the only published form of the support
  list, a JSON-valued eval output for the CLI:
  `nix eval --json github:spa5k/pkg/main?dir=nix/casks#caskSupport.aarch64-darwin`

Support metadata has schema `pkg-cask-support/1` and contains `sources`
(pinned revisions), `supported` (token, kind, version, homepage, artifact
kinds, whether a bounded override is applied), and `excluded` (token, version,
reason, detail). Reasons are computed from the pinned metadata at evaluation
time; they are never hand-copied strings.

## Initial support list (aarch64-darwin)

| Token | Kind | Notes |
| --- | --- | --- |
| `iterm2` | app | App archive (`iTerm.app`), checksum taken from the cask metadata; built and verified on macOS 15.7.7 (`codesign --verify --deep --strict`, `open`). |
| `chromedriver` | binary | Standalone executable from a zip archive; `--version` verified on macOS 15.7.7. |
| `cursor` | app+cli | App archive plus a bounded override that links the vendored CLI the cask declares (`Cursor.app/Contents/Resources/app/bin/code`) as `bin/cursor`, the link name the cask metadata declares. The build fails if that file is missing. |

### Why overrides append to `installPhase`

The converter defines `installPhase` as a plain string. In Nixpkgs stdenv, a
plain-string phase replaces the phase function, so `postInstall` hooks never
run for these derivations. Cask overrides therefore append to `installPhase`
itself. This was verified against the locked brew-nix revision and the
stdenv `runPhase` implementation.

The `cursor` override also replaces the converter's generated `bin/cursor`
GUI wrapper, because the cask metadata declares `cursor` as the name of the
CLI link. The GUI stays exposed as the `.app` bundle, not as a PATH wrapper.

Explicitly inspected exclusions, with computed reasons:

| Token | Reason |
| --- | --- |
| `zoom` | `native-installer` (pkg payload, privileged helper and launchctl services) |
| `4peaks` | `missing-checksum` (`no_check`) |
| `1password@nightly` | `missing-checksum` (`no_check`, `latest` download) |
| `font-0xproto` | `unsupported-artifact` (font, not an app or binary) |
| `raycast` | `minimum-os` (metadata ships `LSMinimumSystemVersion` 26.0; `open` fails with -10825 on supported macOS 15.7.7, verified on-device) |

Policy: support immutable vendor downloads with verified hashes, simple app
archives, standalone executables, and curated app-plus-CLI cases. Exclude
native installer actions, privileged helpers, services, unsupported artifact
variants, and missing checksums. Extracting a `.pkg` payload is not proof
that its installer behavior works, so pkg payloads stay excluded.

`zap` and `uninstall` Cask stanzas are cleanup lifecycle data. `pkg` does not
execute them. Removing a package removes the Nix package only.

## Lock-update procedure

Updates are ordinary reviewed changes on `main`. The outer reference
`github:spa5k/pkg/main?dir=nix/casks` moves; the inputs in each published
revision stay locked. Input URLs in `flake.nix` pin exact commits, so a
plain `nix flake update` cannot advance them. To update an input, edit
its revision in the `flake.nix` input URL and re-lock it:

    cd nix/casks
    $EDITOR flake.nix          # advance one input URL to the new commit
    nix flake lock --update-input brew-api   # repeat per edited input

Commit `flake.nix` and `flake.lock` together in the same change. The
`caskSupport` source revisions follow the lock automatically because they
are derived from `input.rev` at evaluation time.

Then verify before merging:

    nix eval --no-allow-import-from-derivation --json .#caskSupport.aarch64-darwin
    nix build .#packages.aarch64-darwin.iterm2 .#packages.aarch64-darwin.chromedriver .#packages.aarch64-darwin.cursor

The evaluation throws if a curated token stops classifying as supported in
the new metadata, so a metadata update that breaks support cannot merge
silently. Update the tables above in the same change when tokens change.
Installed entries keep their original references; selecting newer metadata
is done by native profile upgrade, not by rewriting installed references.

## Limits

- Apple silicon macOS only for now.
- Some applications reject Nix store locations or self-update against
  immutable files; such apps are removed from the support list rather than
  worked around.
- Rollback restores the packaged app version, not application data such as
  preferences, accounts, or databases.
