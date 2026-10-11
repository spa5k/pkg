# `pkg`

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`pkg` is a small package command line for Linux and macOS. It wraps one
native Nix profile per user. Nix owns package state, generations, rollback,
and roots. `pkg` owns names, commands, output, and desktop app launchers.

`pkg` requires an external Nix installation. It does not install, update,
repair, or remove Nix. Install Determinate Nix from
[the vendor](https://docs.determinate.systems/determinate-nix/) first.

The current reliability work follows
[Repair faults found through real package use](openspec/changes/repair-realistic-use-faults/proposal.md).
The CLI discovery change is
[Improve package states and discovery](openspec/changes/improve-cli-discovery/proposal.md).
Homebrew formula discovery and source sections follow
[Show Homebrew formulae and source sections](openspec/changes/improve-homebrew-discovery/proposal.md).
Package command output follows
[Improve package command output](openspec/changes/improve-package-command-experience/proposal.md).
Code cleanup follows
[Simplify the native client](openspec/changes/simplify-client/proposal.md).
Rust error and ownership changes follow
[Improve Rust error handling and ownership](openspec/changes/improve-rust-quality/proposal.md).
The strict lint and code audit work follows
[Enforce Rust quality](openspec/changes/enforce-rust-quality/proposal.md).

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
   [v0.2.0-alpha.15 release](https://github.com/spa5k/pkg/releases/tag/v0.2.0-alpha.15):

   ```sh
   curl -fsSL -o pkg-install.sh \
     https://raw.githubusercontent.com/spa5k/pkg/v0.2.0-alpha.15/install.sh
   sh pkg-install.sh
   ```

The downloader installs `~/.local/bin/pkg` and completions under
`~/.local/share/pkg/completions`. It is client-only: it downloads and
verifies the release archive, never installs Nix, and never uses `sudo`.
It prepares local search when Nix is present. It also enables a daily user
job to refresh official catalogs. Set `PKG_INSTALL_BACKGROUND=0` to prepare
without this job. Set `PKG_INSTALL_PREPARE=0` to skip both steps.
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
pkg setup                  # Prepare search and enable daily refresh
pkg search ripgrep         # Search local catalogs
pkg search aws             # Rank tools before helpers; hide library noise
pkg search aws --source homebrew # Show Homebrew Formulae and Casks
pkg search brew:aws        # Search official Homebrew Formulae
pkg search cask:aws         # Search Homebrew Casks and registered cask taps
pkg search ripgrpe          # Show similar names when no literal result exists
pkg info nixpkgs:ripgrep   # Show source, attribute, and revision
pkg install nixpkgs:ripgrep # Install one exact package ID
pkg list                   # List installed entries
pkg update                 # Refresh official catalogs and saved taps
pkg upgrade --all          # Follow each entry's original reference
pkg history                # Show native profile generations
pkg rollback               # Return to the previous generation
pkg prune --older-than 30d # Remove old generations
pkg remove ripgrep         # Remove an installed entry
pkg doctor                 # Read-only runtime and profile status
pkg shellenv               # Print PATH setup for Bash/Zsh
pkg apps sync              # Rebuild macOS app launchers
```

`pkg add` also installs packages. `pkg uninstall` also removes packages.
Install and remove show the full package ID. Install also shows the commands
that the package exposes in the profile.

Packages install into a dedicated native profile at
`~/.local/state/nix/profiles/pkg`. Your default Nix profile stays separate.
Formulae and Casks install through Nix into this profile.
Use `pkg install brew:awscli` for a Formula. The client does not need Brew.
Formulae use verified Nixpkgs equivalents or a restricted bottle importer.
Unsupported recipes are refused with a reason.

On macOS, `pkg` exposes app bundles from supported packages as launchers in
`~/Applications/pkg/`. See [commands](docs/commands.md).

## What `pkg` does not do

- `pkg` does not install, update, repair, or remove the Nix runtime.
- `pkg` uses a daily user job for catalog refresh. It exits after each run.
  It has no broker, root helper, or privileged service.
- `pkg` does not sign or host a package channel or central index.
- `pkg` does not install or invoke Homebrew. Casks come from a generated
  catalog of ordinary Nix packages. Formulae use the same native profile.
  Both importers support a restricted subset.
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
OpenSpec workflow. Read the [module map](docs/architecture.md) to find code owners.
The active design is
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
Package conflict reporting follows
[explain package file conflicts](openspec/changes/improve-package-conflicts/proposal.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
