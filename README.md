# `pkg`

[![Release](https://img.shields.io/github/v/release/spa5k/pkg?include_prereleases&sort=semver)](https://github.com/spa5k/pkg/releases/tag/v0.1.0-alpha.4)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`pkg` is a package manager for Linux and macOS. It has a simple command
interface like Homebrew and paru.

Nix is the private package and build engine. You do not need to install,
configure, or operate Nix.

> [!WARNING]
> `pkg` is a technical preview. Breaking changes can occur before v1.

## Install

### Linux x86-64

Download and read the fixed release installer. Then run it.

```sh
curl -fsSLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.4/install.sh
less install.sh
sh install.sh
pkg doctor
```

The script downloads the pinned installer, checks its SHA-256 digest, and then
requests administrator access.

### macOS Apple silicon

Download the package and its checksums. Then install it.

```sh
curl -fsSLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.4/pkg-0.1.0-alpha.4-preview.pkg
curl -fsSLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.4/SHA256SUMS
grep '  pkg-0.1.0-alpha.4-preview.pkg$' SHA256SUMS | shasum -a 256 --check
sudo installer -pkg ./pkg-0.1.0-alpha.4-preview.pkg -target /
pkg doctor
```

The macOS preview is not Developer ID signed or notarized. See the
[install guide](docs/install.md) and the
[latest release](https://github.com/spa5k/pkg/releases/tag/v0.1.0-alpha.4)
for verification details.

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

Preview the files that `pkg` will remove. Then uninstall it.

```sh
pkg uninstall --dry-run
pkg uninstall
```

The uninstaller removes only authenticated `pkg` state. It keeps changed,
unrecorded, or foreign state for manual review.

## Security

`pkg` uses this fixed local boundary:

```text
pkg CLI -> local broker -> non-root broker -> privileged helper -> managed runtime
```

- Package metadata and release inputs are authenticated.
- The installer refuses foreign or changed Nix state.
- Privileged operations use a narrow helper interface.
- Public commands do not accept Nix commands, expressions, installables, store
  paths, trust roots, or arbitrary Nix options.
- A normal user cannot access the private runtime, daemon, helper, or trust
  controls.

Read the full [security model](plans/08-security-model.md).

## Platform status

| Platform | Preview status |
| --- | --- |
| Linux x86-64 | Supported and tested with the public installer |
| macOS Apple silicon | Available; Developer ID signing and notarization are TODO items |
| Linux arm64 | Not available in this preview |

The clean-host proofs cover install, retry, cached package installation, one
approved local build, upgrade, rollback, repair, ownership drift, isolation,
and uninstall.

## Contribute

Read [CONTRIBUTING.md](CONTRIBUTING.md) for the toolchain, local checks, and
review rules.

The design documents are in [`plans/`](plans/README.md):

- [Architecture decisions](plans/00-overview-and-decisions.md)
- [Security model](plans/08-security-model.md)
- [Release and operations](plans/10-release-and-operations.md)
- [PR roadmap](plans/11-pr-roadmap.md)
- [Open decisions and risks](plans/12-open-decisions-and-risks.md)

## License

Licensed under the [Apache License 2.0](LICENSE).
