# `pkg`

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`pkg` is a small package command line for Linux and macOS. It wraps one
native Nix profile per user. Nix owns package state, generations, rollback,
and roots. `pkg` owns names, commands, output, and desktop app launchers.

`pkg` needs Nix. When Nix is missing, the installer offers to install
[Determinate Nix](https://docs.determinate.systems/determinate-nix/).
`pkg` does not update, repair, or remove Nix.

The completed Rust quality work follows
[Enforce Rust quality](openspec/changes/archive/2026-10-12-enforce-rust-quality/proposal.md) and
[Complete the Rust quality plan](openspec/changes/archive/2026-10-12-complete-rust-quality-plan/proposal.md).
Completed and superseded plans are listed in the
[OpenSpec archive](openspec/changes/archive/README.md).

> [!WARNING]
> `pkg` is a technical preview. Breaking changes can occur before v1.
> [v0.2.0-alpha.3](https://github.com/spa5k/pkg/releases/tag/v0.2.0-alpha.3)
> starts from a fresh state: old alpha installations are not migrated,
> adopted, or erased. See
> [Simplify pkg to native Nix](openspec/changes/archive/2026-10-12-simplify-to-native-nix/proposal.md)
> and its [verification record](docs/verification/native-nix-2026-09-28.md).
> The current release gives shorter CLI output and reports partial package
> changes when macOS app launcher setup fails. See the
> [command guide](docs/commands.md).

## Install

```sh
curl -fsSL https://github.com/spa5k/pkg/releases/latest/download/install.sh | sh
```

Then open a new terminal window.

The installer puts `pkg` in `~/.local/bin` and sets up Bash, Zsh, or Fish.
When Nix is missing, it asks once whether to install Nix. A daily job
updates `pkg` and its search data. Run the same command again to repair
the setup. Run `pkg self uninstall` to remove `pkg`.

Details are in the [install guide](docs/install.md).

## Use `pkg`

```sh
pkg search ripgrep         # Find packages
pkg info ripgrep           # Show details, commands, and other sources
pkg install ripgrep        # Install a package
pkg install rg             # Command names work too
pkg list                   # List installed packages and versions
pkg upgrade                # Upgrade all packages
pkg remove ripgrep         # Remove a package
pkg history                # Show past changes
pkg rollback               # Undo the last change
pkg update                 # Update pkg and its search data
pkg doctor                 # Check the setup
```

`pkg add` also installs packages. `pkg uninstall` also removes packages.

A name that several sources have installs from the first source in this
order: Nixpkgs, Homebrew Casks, Homebrew Formulae. The result names the
other sources. To choose one, use its ID, for example
`pkg install cask:firefox` or `pkg install brew:awscli`.

Packages install into a dedicated native profile at
`~/.local/state/nix/profiles/pkg`. Your default Nix profile stays separate.
Formulae and Casks install through Nix into this profile.
Use `pkg install brew:awscli` for a Formula. The client does not need Brew.
Formulae use verified Nixpkgs equivalents or a restricted bottle importer.
Unsupported recipes are refused with a reason.

On macOS, `pkg` exposes app bundles from supported packages as launchers in
`~/Applications/pkg/`. See [commands](docs/commands.md).

## What `pkg` does not do

- `pkg` does not update, repair, or remove the Nix runtime.
- `pkg` uses a daily user job to update itself and its catalogs. It exits after each run.
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
The native Nix design baseline is
[Simplify pkg to native Nix](openspec/changes/archive/2026-10-12-simplify-to-native-nix/proposal.md);
the [OpenSpec archive](openspec/changes/archive/README.md) records earlier changes.
The [plan index](plans/README.md) records the earlier alpha plans.

## License

Licensed under the [Apache License 2.0](LICENSE).
