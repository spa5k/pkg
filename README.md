# `pkg`

[![Release](https://img.shields.io/github/v/release/spa5k/pkg?include_prereleases&sort=semver)](https://github.com/spa5k/pkg/releases/tag/v0.1.0-alpha.49)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`pkg` is a package manager for Linux and macOS. It has a simple command
interface like Homebrew and paru.

Base Nix is the machine-wide package and build engine. You do not need to
install or configure Nix first. pkg authenticates and starts the pinned Determinate Nix Installer 3.22.1 executable on supported
systems.

> [!WARNING]
> `pkg` is a technical preview. Breaking changes can occur before v1.

## Install

Use one command for **Apple silicon macOS** or **Linux x86-64 with systemd**:

```sh
curl -fsSL --proto '=https' --proto-redir '=https' https://raw.githubusercontent.com/spa5k/pkg/main/install.sh -o "$HOME/pkg-install.sh" && /bin/sh "$HOME/pkg-install.sh"
```

The [install guide](docs/install.md) includes fixed release downloads,
checksums, shell setup, and recovery steps. Run the same command to update pkg.

The terminal installer installs Determinate Nix for you. It saves a private
log and waits for setup to finish. The macOS package is ad-hoc signed and is
not notarized. This alpha uses a test signing root.

## Use `pkg`

```sh
pkg search ripgrep       # Find a package
pkg info ripgrep         # Show package details
pkg install ripgrep      # Install a package
pkg list                 # List installed packages
pkg outdated             # Find available upgrades
pkg update               # Refresh package metadata
pkg upgrade --all        # Upgrade all packages
pkg history              # Show saved generations
pkg rollback             # Restore the previous package environment
pkg repair               # Check and repair installed packages
pkg remove ripgrep       # Remove a package
```

`pkg` uses cached packages first. If a local build is required, `pkg` asks for
one exact, one-time approval.

Alpha.49 on macOS also supports
[public flake packages](docs/commands.md#public-flake-packages-macos).

## Uninstall

Preview the files that `pkg` will remove. Then uninstall it. Live uninstall on
Linux and macOS requires plain terminal output. Live JSON and JSONL output are
refused before any change.

```sh
pkg uninstall --dry-run
sudo pkg uninstall
```

`pkg` first removes and verifies authenticated product-owned state. It then
replaces itself with the authenticated installed Determinate uninstaller. This
vendor uninstall is the last action. The vendor command returns its status
directly to the shell.

`pkg` keeps changed, unrecorded, or foreign state for manual review. Determinate
can leave vendor-owned residue. `pkg` does not delete that residue or infer
success from its absence.

## Security

`pkg` exposes package commands through this product interface:

```text
pkg package commands -> Broker -> Root Helper -> Package Lifecycle
pkg installer -> authenticated Determinate executable -> Base Nix Lifecycle
```

- Catalog metadata and release inputs are authenticated. Public flake sources
  on macOS are selected by the user and locked by content; they are not endorsed
  by pkg. Linux refuses them until it has the same source read boundary.
- The installer refuses foreign or changed Nix state.
- Privileged operations use a narrow helper interface.
- Public commands accept package names and validated public flake references.
  They do not accept Nix commands, expressions, store paths, trust roots, or
  arbitrary Nix options.
- The Root Helper accepts only closed product operations.

Raw Nix availability is not a security boundary. Base Nix daemon access is not
a product security boundary. Local administrators can access or change Base
Nix. `pkg doctor` checks important changes and fails closed when ownership is
not clear.

Linux and Apple silicon macOS use pinned Determinate Nix Installer 3.22.1 for
Base Nix install and terminal uninstall. `pkg` does not own Base Nix update or
repair. See the
[active implementation plan](plans/determinate-nix-stacked-prs.md).

## Platform status

| Platform | Preview status |
| --- | --- |
| Linux x86-64 | Public alpha.49; requires systemd |
| macOS Apple silicon | Public alpha.49; ad-hoc signed, not notarized |
| macOS Intel | Not supported |
| Linux arm64 | Not available in this preview |

The native platform cutover proofs passed. The release includes automatic
product service updates, the doctor ownership correction, and `pkg shellenv`.
The [native lifecycle report](tests/macos-clean-host/PUBLIC-04.md) records the
compiled-package, repair, reboot, and uninstall checks that support these changes.
See the [release notes](https://github.com/spa5k/pkg/releases/tag/v0.1.0-alpha.49)
for checks on the exact release files and known limits.

## Contribute

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the toolchain, local checks, and
review rules.

The [plan index](plans/README.md) identifies the one active
[stacked-PR implementation plan](plans/determinate-nix-stacked-prs.md). The
earlier custom Base Nix design is archived and is not normative.

## License

Licensed under the [Apache License 2.0](LICENSE).
