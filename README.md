# `pkg`

[![Release](https://img.shields.io/github/v/release/spa5k/pkg?include_prereleases&sort=semver)](https://github.com/spa5k/pkg/releases/tag/v0.1.0-alpha.7)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`pkg` is a package manager for Linux and macOS. It has a simple command
interface like Homebrew and paru.

Base Nix is the machine-wide package and build engine. You do not need to
install or configure Nix first. The current DN-16 source authenticates and
starts the pinned Determinate Nix Installer 3.22.1 executable on supported
systems.

> [!WARNING]
> `pkg` is a technical preview. Breaking changes can occur before v1.

## Install

### Linux x86-64

Download and read the fixed release installer. Then run it.

```sh
curl -fsSLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.7/install.sh
less install.sh
sh install.sh
pkg doctor
```

The script downloads the pinned `pkg` installer, checks its SHA-256 digest, and
then requests administrator access. The current Linux candidate authenticates
the pinned Determinate Nix Installer 3.22.1 executable before it starts Base Nix
installation.

After the vendor installer starts, `pkg` waits for it. The current vendor
contract has no safe cancellation, signal, hard timeout, or parent-death
guarantee. If the result is unknown, `pkg` fails closed and does not retry.

### macOS Apple silicon

Download the package and its checksums. Then install it.

```sh
curl -fsSLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.7/pkg-0.1.0-alpha.7-preview.pkg
curl -fsSLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.7/SHA256SUMS
grep '  pkg-0.1.0-alpha.7-preview.pkg$' SHA256SUMS | shasum -a 256 --check
sudo installer -pkg ./pkg-0.1.0-alpha.7-preview.pkg -target /
pkg doctor
```

The macOS preview is not Developer ID signed or notarized. See the
[install guide](docs/install.md) and the
[latest release](https://github.com/spa5k/pkg/releases/tag/v0.1.0-alpha.7)
for verification details.

The package command above installs public alpha.7. Alpha.7 does not contain the
DN-16 Determinate cutover described below. The DN-16 candidate still needs its
disposable native Apple silicon proof.

The DN-16 macOS source supports Apple silicon only. It refuses Intel macOS.
`pkg-install` obtains and authenticates the pinned executable through the
authenticated installer repository. It then uses that executable to install
Base Nix. You do not need to install Nix before you install `pkg`.

After the vendor installer starts, `pkg` waits for it. A stored `Started` state
means that the Base Nix result is unknown. `pkg` fails closed. It does not start
the vendor installer again.

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
pkg rollback 2           # Restore generation 2
pkg repair               # Check and repair installed packages
pkg remove ripgrep       # Remove a package
```

`pkg` uses cached packages first. If a local build is required, `pkg` asks for
one exact, one-time approval.

## Uninstall

Preview the files that `pkg` will remove. Then uninstall it. Live uninstall on
Linux and macOS requires plain terminal output. Live JSON and JSONL output are
refused before any change.

```sh
pkg uninstall --dry-run
pkg uninstall
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

- Package metadata and release inputs are authenticated.
- The installer refuses foreign or changed Nix state.
- Privileged operations use a narrow helper interface.
- Public commands do not accept Nix commands, expressions, installables, store
  paths, trust roots, or arbitrary Nix options.
- The Root Helper accepts only closed product operations.

Raw Nix availability is not a security boundary. Base Nix daemon access is not
a product security boundary. Local administrators can access or change Base
Nix. `pkg doctor` checks important changes and fails closed when ownership is
not clear.

This section describes the current DN-16 source. It is not in public alpha.7.
Linux and Apple silicon macOS use pinned Determinate Nix Installer 3.22.1 for
Base Nix install and terminal uninstall. `pkg` does not own Base Nix update or
repair. See the
[active implementation plan](plans/determinate-nix-stacked-prs.md).

## Platform status

| Platform | Preview status |
| --- | --- |
| Linux x86-64 | Alpha.7 is public; newer Determinate source has passed its native proof |
| macOS Apple silicon | Alpha.7 does not contain DN-16; the DN-16 candidate still needs disposable native proof and Apple signing |
| macOS Intel | Not supported; the installer refuses this system |
| Linux arm64 | Not available in this preview |

The checked-in clean-host matrix covers install, cached package installation,
one approved local build, upgrade, rollback, Package Repair, ownership drift,
isolation, and uninstall. The Linux proof passed. The macOS Determinate cutover
still needs its disposable Apple silicon proof.

## Contribute

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the toolchain, local checks, and
review rules.

The [plan index](plans/README.md) identifies the one active
[stacked-PR implementation plan](plans/determinate-nix-stacked-prs.md). The
earlier custom Base Nix design is archived and is not normative.

## License

Licensed under the [Apache License 2.0](LICENSE).
