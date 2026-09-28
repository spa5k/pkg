# Homebrew Casks as Nix packages through pkg

Checked on 27 September 2026. Research only. No packages were installed.

## Proposed direction

Yes, `pkg` can offer a useful subset of Homebrew Casks as Nix packages without installing or invoking Brew. Start with `BatteredBunny/brew-nix`. Keep Nix as the only package engine. This replaces the earlier Brew-adapter proposal, which does not meet the user's requirement.

The conversion normally checks and extracts a vendor's existing application archive into a Nix output. It repackages a binary; it does not compile the application's source. The implementation uses Nix, archive tools, and generated shell instructions. It needs no Homebrew or Ruby client at runtime. [Converter source](https://github.com/BatteredBunny/brew-nix/blob/16131ae4126c54b1502aa7eaf6573d7fbf16b656/casks.nix)

## Reuse the existing import path

`brew-nix` directly exports `packages.aarch64-darwin.<token>` and `packages.x86_64-darwin.<token>`. It also provides an overlay. Home Manager and nix-darwin are not required to select these outputs. [Flake exports](https://github.com/BatteredBunny/brew-nix/blob/16131ae4126c54b1502aa7eaf6573d7fbf16b656/flake.nix)

The existing macOS public-flake route locks GitHub sources and derives packages. This is a starting integration point, not proof of graphical application support. Search and info remain Nixpkgs-only; public-flake outdated checks are not implemented. Cask discovery, source update policy, and application exposure need work. [Public-flake interface](../commands.md#public-flake-packages-macos), [current source adapter](../../crates/pkg-nix/src/real/flake.rs)

Proposed design:

1. Add a small product-controlled flake that reuses `brew-nix`.
2. Pin the converter, Nixpkgs, and Cask metadata independently.
3. Expose packages that pass the supported-artifact policy.
4. Add those outputs to existing discovery and Nix installation paths.
5. Add macOS application launchers for the resulting bundles.

This should remain a source/catalog adapter. It should not add another installer or receipt database. Give Cask-derived results a source identity when Nixpkgs has the same name. These are recommendations, not implemented features.

## Metadata and evaluation

`brew-nix` reads `cask.json` from a separate `brew-api` source input. Its lock file pins that input by revision and `narHash`. This metadata path has no import-from-derivation. The later application download uses `fetchurl` during normal package realization. [Lock file](https://github.com/BatteredBunny/brew-nix/blob/16131ae4126c54b1502aa7eaf6573d7fbf16b656/flake.lock), [metadata import](https://github.com/BatteredBunny/brew-nix/blob/16131ae4126c54b1502aa7eaf6573d7fbf16b656/casks.nix)

Upstream warns that its default metadata input can be stale. The wrapper should maintain a separately pinned input and test updates before publication. A content hash fixes input bytes; it does not establish publisher trust or application compatibility. [Update guidance](https://github.com/BatteredBunny/brew-nix)

## Supported subset

| Cask form | Initial policy |
| --- | --- |
| One ordinary app in a supported archive | Candidate after launch and update checks. |
| Standalone binary | Candidate after executable and dependency checks. |
| Multiple apps or extra CLI artifacts | Inspect explicitly; first app/binary entries receive special treatment. |
| Native installer, privileged helper, service, or extension | Exclude unless a separate Nix package implements required behavior. |
| Missing checksum or unstable download | Require a pinned, verified source override. |

`brew-nix` extracts some `.pkg` payloads, but does not execute Apple's installer or Cask lifecycle hooks. File extraction is not complete installer support. Its app path takes precedence over the binary path; extra Cask CLI links need verification. [Implementation](https://github.com/BatteredBunny/brew-nix/blob/16131ae4126c54b1502aa7eaf6573d7fbf16b656/casks.nix)

Some apps reject Nix store locations. Intel variant selection can need a macOS-version override. Missing hashes need manual overrides. Start with Apple silicon macOS, matching current `pkg` support. [Upstream limitations](https://github.com/BatteredBunny/brew-nix)

Official examples show useful test categories: [Raycast](https://formulae.brew.sh/api/cask/raycast.json) includes app and login-item behavior; [Cursor](https://formulae.brew.sh/api/cask/cursor.json) adds a CLI link; [Zoom](https://formulae.brew.sh/api/cask/zoom.json) needs a native installer and helper/service cleanup. These are test candidates, not claims of importer compatibility.

## Application exposure and rollback

A bundle in `/nix/store` still needs suitable macOS application exposure. `mac-app-util` offers standalone `mktrampoline` and `sync-trampolines` commands without requiring nix-darwin. Evaluate it before writing another launcher system. Its sync operation deletes and recreates the destination, so use only a dedicated pkg-owned directory. This is an unproved candidate for `pkg`. [mac-app-util](https://github.com/hraban/mac-app-util)

Current pkg activation reconstructs directories with links to individual files.
For the GUI proof, launch the intact app bundle from its Nix output. Do not
assume the reconstructed tree preserves signed-app behavior. This is a required
compatibility check, not a demonstrated failure.
[Activation implementation](../../crates/pkg-store/src/activate.rs)

The current activation code recursively creates file links inside directories, including `.app` bundles. For the first proof, point launchers at the intact `$out/Applications/App.app` in the Nix store. Signed-app behavior through the reconstructed activation tree is untested. [Activation code](../../crates/pkg-store/src/activate.rs)

Nix generations can retain app versions. They do not restore mutable app data, preferences, accounts, or external services. Test self-updaters against immutable files. Describe rollback as restoring the packaged app version, not all application state.

## Alternatives and first proof

`atahanyorganci/nix-casks` rejects native installers, `latest`, and `no_check`; it ignores several lifecycle actions. Its exporter reads a `fetchurl` derivation during evaluation, adding import-from-derivation. These differences favor `brew-nix` for the first proof. [Converter](https://github.com/atahanyorganci/nix-casks/blob/b7fecb4dd490bdcd7be02bd913ac685a1bda305e/src/lib/homebrew.ts), [exporter](https://github.com/atahanyorganci/nix-casks/blob/b7fecb4dd490bdcd7be02bd913ac685a1bda305e/nix/casks/default.nix)

`nix-homebrew` provisions Brew. nix-darwin's Homebrew module invokes it. Neither meets the no-Brew requirement. [nix-homebrew](https://github.com/zhaofengli/nix-homebrew), [nix-darwin module](https://github.com/nix-darwin/nix-darwin/blob/master/modules/homebrew.nix)

A disposable-host proof should cover a simple app, an app with a CLI, a rejected installer, upgrade, rollback, and application discovery. Disable import-from-derivation. Verify Brew and Ruby are absent. Record failures before expanding coverage. No runtime checks were performed for this note.
