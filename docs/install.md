---
title: Install pkg
---

# Install pkg

The current public release is
[`v0.1.0-alpha.54`](https://github.com/spa5k/pkg/releases/tag/v0.1.0-alpha.54).
It supports **Apple silicon macOS** and **Linux x86-64 with systemd**.
Intel macOS and Linux arm64 are not release targets yet.

Use the same installer for a new installation or a compatible pkg upgrade.
It installs Determinate Nix when needed.
You do not need to install Nix first. An existing Nix installation must be
checked before setup; do not delete it to bypass an error.

This is an alpha. The macOS package is ad-hoc signed and is not notarized.
The release uses a test signing root. Use a disposable system for evaluation.

## One command for installation and updates

Run this as your normal user on either supported platform:

```sh
curl -fsSL --proto '=https' --proto-redir '=https' https://raw.githubusercontent.com/spa5k/pkg/main/install.sh -o "$HOME/pkg-install.sh" && /bin/sh "$HOME/pkg-install.sh"
```

The public script pins one release and verifies every downloaded installer file.
It requests administrator access only after verification. It checks the installed
version and runs doctor after setup. Use the same command for a compatible pkg
update. Package updates still use `pkg update` and `pkg upgrade --all`.

To check the downloads without installing, run the saved script with
`/bin/sh "$HOME/pkg-install.sh" --verify-only`.
Use `--no-color` for plain output. The saved script stays pinned; run the download
command again to get the current published version.

The bootstrap is distributed over HTTPS. Its pinned checksums protect the
subsequent downloads. This shell step does not claim to implement TUF. The
installed product retains its existing signed-channel verification.

## macOS Apple silicon

Use the terminal installer. It keeps the macOS permission prompt in your
terminal session, shows the real result, and saves a private log.

```sh
curl -fLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.54/pkg-0.1.0-alpha.54-preview.pkg && \
curl -fLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.54/install-preview.sh && \
printf '%s  %s\n' '64ff6f29a287ce4b4759096b3a862b58a8bca681b05dcf3ea46d8a94793de4b4' 'install-preview.sh' | shasum -a 256 --check && \
/bin/bash ./install-preview.sh ./pkg-0.1.0-alpha.54-preview.pkg 8c601088e68deb63da9010673292dbf0dbfc496fee4e8ff892cc040b96a370ba
```

Allow the macOS system configuration prompt if it appears. Keep the terminal
open until setup finishes. The log path appears before setup starts.

Add installed commands to your current shell:

```sh
[ ! -x /usr/local/bin/pkg ] || eval "$(/usr/local/bin/pkg shellenv)"
pkg doctor
pkg install fzf
fzf --version
```

Add the same `[ ! -x /usr/local/bin/pkg ] || eval "$(/usr/local/bin/pkg shellenv)"` line to `~/.zshrc` for future zsh sessions.
Use `~/.bashrc` if your interactive shell is Bash. `cxx-prettyprint` is a
header library; it does not add a command to PATH.

### macOS product upgrade and repair

Run the same verified installer for a compatible product upgrade.
It checks the installed service files, stops the pkg services, updates the
product files, and starts the services again. It keeps Nix and installed packages.
You do not need manual `launchctl` commands for a normal upgrade.

Product file repair remains an offline support operation. See the repair
section below. Do not use cleanup commands from older alpha troubleshooting
sessions as an upgrade method.

## Linux x86-64

Download and verify the installer before you run it:

```sh
curl -fLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.54/pkg-install-x86_64-linux && \
printf '%s  %s\n' 'd78aa382373fb591a58e280f6385eff345b2ac99f4c34e3df847bdaa102fb0a5' 'pkg-install-x86_64-linux' | sha256sum --check && \
chmod 700 ./pkg-install-x86_64-linux && \
sudo ./pkg-install-x86_64-linux
```

Open a new login shell, or load the installed shell settings:

```sh
. /etc/profile.d/pkg.sh
pkg doctor
pkg install fzf
fzf --version
```

The repository's [install.sh](../install.sh) wraps the same pinned download
with verification and a private log. [docs/install.sh](install.sh) is the
release template. It refuses network access until release tooling fills in
its version, URL, and checksum.

### Three Linux install modes

For a normal product upgrade, run the verified installer without options.
It checks, stops, updates, and restarts the pkg services automatically.
It keeps Nix and installed packages.

#### Fresh install

Run the verified installer without options. It installs the package engine,
installs pkg, starts services, and saves the installation record.

#### Offline product upgrade

For operator-controlled maintenance, use `--leave-services-offline`:

```sh
sudo systemctl stop pkg-nix-broker.service pkg-root-helper.service pkg-nix-broker.socket pkg-root-helper.socket
sudo systemctl disable pkg-nix-broker.service pkg-root-helper.service pkg-nix-broker.socket pkg-root-helper.socket
sudo ./pkg-install-x86_64-linux --leave-services-offline
sudo systemctl daemon-reload
sudo systemctl enable --now pkg-root-helper.socket pkg-nix-broker.socket pkg-root-helper.service pkg-nix-broker.service
```

Run the last two commands only after the installer reports success. Product
unit files must be unchanged and must have no drop-ins.

Omit `--leave-services-offline` for the normal automatic upgrade.

#### Offline product asset repair

Package repair and product repair have different purposes:

- `pkg repair` checks and repairs installed packages.
- `pkg-install --repair-product-assets` restores the pkg program and service
  files from the same authenticated release. Services must be stopped first.

Product repair remains an offline support operation on both platforms.
It does not repair or update Nix. Use the matching release and retain its log.

## Recovery after an interrupted install

If setup says that Nix is ready but pkg is incomplete, run the same installer
again. It can finish the saved pkg transaction without reinstalling Nix.

If setup reports an unknown Nix outcome, retain the log and contact support.
Do not delete the receipt or handoff files. A completed macOS Nix installation
can be resumed with `--resume` only when its receipt, daemon, and matching
pending pkg transaction all pass validation.

The installer waits for Determinate after it starts. Do not close the terminal
to force a retry. A partial Nix installation needs diagnosis first.

Alpha.47 and later fix the false unmanaged-Nix diagnostic from alpha.46.
If the diagnostic remains after an upgrade, keep the support report and
installation log. See [support](support.md).

## Local candidate proof

Candidate archives and private test channels are for disposable test systems.
They are not public release assets. The [active plan](../plans/determinate-nix-stacked-prs.md)
records the platform proof gates and their scope. The public release notes
record what was tested for each release.

## Remove pkg itself

Preview the operation first:

```sh
sudo pkg system uninstall --dry-run
sudo pkg system uninstall
```

Use plain terminal output for live uninstall. It removes verified pkg-owned
files, then runs the authenticated Determinate uninstaller. Changed or foreign
files are retained for review. Determinate can leave vendor-owned residue.
