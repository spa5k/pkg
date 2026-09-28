# `pkg`

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`pkg` is a small package command line for Linux and macOS. It wraps one
native Nix profile per user. Nix owns package state, generations, rollback,
and roots. `pkg` owns names, commands, output, and desktop app launchers.

`pkg` requires an external Nix installation. It does not install, update,
repair, or remove Nix. Install Determinate Nix from
[the vendor](https://docs.determinate.systems/determinate-nix/) first.

> [!WARNING]
> `pkg` is a technical preview. Breaking changes can occur before v1. The
> native Nix redesign is implemented as an unreleased candidate. See
> [Simplify pkg to native Nix](openspec/changes/simplify-to-native-nix/proposal.md)
> and its [verification record](docs/verification/native-nix-2026-09-28.md).

## Install

1. Install [Determinate Nix](https://docs.determinate.systems/determinate-nix/)
   on your machine. `pkg` never does this step for you.
2. Build the client candidate from a checkout of this repository:
   `tools/release/package_client.sh x86_64-linux` (or `aarch64-darwin`).

The candidate is unpublished. Build it from a checkout until a release
exists. The `install.sh` in this checkout is client-only: it downloads and
verifies the client archive and never installs Nix.

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
- `pkg` does not install or invoke Homebrew. Supported Casks are converted
  to ordinary Nix packages.
- `pkg` does not migrate, adopt, or erase old installations.
- `pkg` does not remove your machine's Nix installation when removed.

Removing the client removes only the client. Your Nix installation and the
`pkg` package profile stay intact.

## Platform status

| Platform | Status |
| --- | --- |
| Linux x86-64 | In development |
| macOS Apple silicon | In development; supported app subset only |
| macOS Intel | Not supported |
| Linux arm64 | Not supported |

## Contribute

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the toolchain, checks, and the
OpenSpec workflow. The active design is
[Simplify pkg to native Nix](openspec/changes/simplify-to-native-nix/proposal.md);
the [plan index](plans/README.md) separates historical material.

## License

Licensed under the [Apache License 2.0](LICENSE).
