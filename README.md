# `pkg`

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`pkg` is a small package command line for Linux and macOS. It wraps one
native Nix profile per user. Nix owns package state, generations, rollback,
and roots. `pkg` owns names, commands, output, and desktop app launchers.

`pkg` requires an external Nix installation. It does not install, update,
repair, or remove Nix. Install Determinate Nix from
[the vendor](https://docs.determinate.systems/determinate-nix/) first.

> [!WARNING]
> `pkg` is a technical preview. Breaking changes can occur before v1.
> [v0.2.0-alpha.3](https://github.com/spa5k/pkg/releases/tag/v0.2.0-alpha.3)
> starts from a fresh state: old alpha installations are not migrated,
> adopted, or erased. See
> [Simplify pkg to native Nix](openspec/changes/simplify-to-native-nix/proposal.md)
> and its [verification record](docs/verification/native-nix-2026-09-28.md).
> The current release gives shorter CLI output and reports partial package
> changes when macOS app launcher setup fails. See the
> [command guide](docs/commands.md).

## Install

1. Install [Determinate Nix](https://docs.determinate.systems/determinate-nix/)
   on your machine. `pkg` never does this step for you.
2. Install the client from the
   [v0.2.0-alpha.9 release](https://github.com/spa5k/pkg/releases/tag/v0.2.0-alpha.9):

   ```sh
   curl -fsSL -o pkg-install.sh \
     https://raw.githubusercontent.com/spa5k/pkg/v0.2.0-alpha.9/install.sh
   sh pkg-install.sh
   ```

The downloader installs `~/.local/bin/pkg` and completions under
`~/.local/share/pkg/completions`. It is client-only: it downloads and
verifies the release archive, never installs Nix, and never uses `sudo`.
It sets up the `pkg` client `PATH` and the `pkg` profile `PATH` for Bash
and Zsh automatically; set `PKG_INSTALL_SHELL_SETUP=0` to opt out.

`pkg` finds the Nix runtime through `PATH`, then at the standard Nix
profile locations (`/nix/var/nix/profiles/default/bin`, the per-user
state profile, `~/.nix-profile`), so a shell without Nix on `PATH` still
works. An explicitly configured `runtime.nix` always wins and never
falls back. You can also build the archive from a checkout of this
repository: `tools/release/package_client.sh x86_64-linux`
(or `aarch64-darwin`).

Details are in the [install guide](docs/install.md).

## Use `pkg`

```sh
pkg search ripgrep         # Search supported sources
pkg info nixpkgs:ripgrep   # Show source, attribute, and revision
pkg install nixpkgs:ripgrep # Install; a qualified ID skips the full-catalog bare-name scan
pkg list                   # List installed entries
pkg update                 # Refresh discovery data only
pkg upgrade --all          # Follow each entry's original reference
pkg history                # Show native profile generations
pkg rollback               # Return to the previous generation
pkg prune --older-than 30d # Remove old generations
pkg remove ripgrep         # Remove an installed entry
pkg doctor                 # Read-only runtime and profile status
pkg shellenv               # Print PATH setup for Bash/Zsh
pkg apps sync              # Rebuild macOS app launchers
```

Packages install into a dedicated native profile at
`~/.local/state/nix/profiles/pkg`. Your default Nix profile stays separate.

On macOS, `pkg` exposes app bundles from supported packages as launchers in
`~/Applications/pkg/`. See [commands](docs/commands.md).

## What `pkg` does not do

- `pkg` does not install, update, repair, or remove the Nix runtime.
- `pkg` does not run a broker, root helper, daemon, or privileged service.
- `pkg` does not sign or host a package channel or central index.
- `pkg` does not install or invoke Homebrew. Casks come from a generated
  catalog of ordinary Nix packages; no package allowlists exist.
- `pkg` does not migrate, adopt, or erase old installations.
- `pkg` does not remove your machine's Nix installation when removed.

Removing the client removes only the client. Your Nix installation and the
`pkg` package profile stay intact.

## Platform status

| Platform | Status |
| --- | --- |
| Linux x86-64 | In development; cask binary and AppImage journeys verified headless only |
| macOS Apple silicon | In development; generated cask catalog, macOS baseline 15.7.7 |
| macOS Intel | Not supported |
| Linux arm64 | Not supported |

The cask source covers a broad generated catalog on both target systems.
Catalog eligibility is a metadata claim, not a verification promise. See
[casks](docs/casks.md).

## Contribute

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the toolchain, checks, and the
OpenSpec workflow. The active design is
[Simplify pkg to native Nix](openspec/changes/simplify-to-native-nix/proposal.md);
the [plan index](plans/README.md) separates historical material.
The current change is
[improve runtime discovery and shell setup](openspec/changes/improve-runtime-discovery-and-shell-setup/proposal.md).
Search cost is amortized by
[revision snapshots](openspec/changes/amortize-search-with-revision-snapshots/proposal.md):
one cached catalog per source answers every later query locally.
The Cask extension is
[import of public taps](openspec/changes/import-public-cask-taps/design.md).
Native lifecycle and archive repairs are specified in
[harden native package lifecycle](openspec/changes/harden-native-package-lifecycle/proposal.md).
The accepted audit repairs are
[implement audit repairs](openspec/changes/implement-audit-repairs/proposal.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
