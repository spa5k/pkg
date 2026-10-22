# Install `pkg`

`pkg` is a small client over one native Nix profile.

## Install

Run this command in a terminal:

```sh
curl -fsSL https://github.com/spa5k/pkg/releases/latest/download/install.sh | sh
```

Then open a new terminal window.

The installer takes no options. It does these steps:

1. It looks for a working Nix. When no Nix exists, it asks
   `pkg needs Nix. Install Nix now? [Y/n]`. On yes, it runs the
   [Determinate Nix](https://docs.determinate.systems/determinate-nix/)
   installer, which asks for your password.
2. It downloads the client for your system and verifies its checksum.
3. It starts the new client once. Only a working client replaces the old
   client.
4. It sets up your shell.
5. It turns on a daily job that updates `pkg` and its search data. The
   first run starts at once in the background.

Supported systems: x86_64 Linux and Apple silicon macOS. On Linux, the
client needs `glibc` 2.35 or newer.

The installer never uses `sudo` itself. It does not change Nix trust
settings. It does not remove or migrate old installations.

### Files

- `~/.local/bin/pkg`: the client.
- `~/.local/share/pkg/completions/`: shell completions.
- `~/.local/state/nix/profiles/pkg`: the packages that you install. When
  `XDG_STATE_HOME` is set, the profile is under that directory.

`pkg` refuses to start when `HOME` names a folder that does not exist, or
when an XDG base directory is a file. The message names the path.

### Shell setup

The installer adds three directories to `PATH`, in this order: the
client, Nix, and your packages. Your shell also gets `pkg` completion.

- Bash: a managed block in `~/.bashrc` and in your login file. The login
  file is the first of `~/.bash_profile`, `~/.bash_login`, and
  `~/.profile`. When none exists, the installer creates `~/.profile`.
- Zsh: a managed block in `${ZDOTDIR:-$HOME}/.zshrc`.
- Fish: the file `~/.config/fish/conf.d/pkg.fish`. When
  `XDG_CONFIG_HOME` is set, the file is under that directory.

The `SHELL` variable selects the shell. Existing `pkg` setup for other
shells is updated too. For other shells, the installer prints the
directories to add.

The managed block sits between these marker lines:

```sh
# >>> pkg paths >>>
...
# <<< pkg paths <<<
```

A new installer run replaces the block. It never adds a second block.
Lines outside the block and file modes stay. The installer skips a
startup file that is a link, or a file with damaged markers, and tells
you what to do.

### Nix detection

The installer and `pkg` look for `nix` in this order:

1. `nix` on `PATH`;
2. `/nix/var/nix/profiles/default/bin/nix`;
3. `$XDG_STATE_HOME/nix/profile/bin/nix`, or
   `~/.local/state/nix/profile/bin/nix`;
4. `~/.nix-profile/bin/nix`.

A candidate counts only when it answers `nix --version`. When a Nix
exists but does not work, the installer does not offer a new Nix. Repair
Nix, then run the installer again.

When you decline the Nix install, the client is still installed. Install
Nix later with this command, then open a new terminal window:

```sh
curl -fsSL https://install.determinate.systems/nix | sh -s -- install
```

## Search data

The first search prepares missing search data. For the default Nixpkgs
source, pkg downloads the package list that the NixOS channel publishes
(about 10 MB). This takes a few seconds. The daily job usually prepares
the data before your first search.

A source that has no published list, such as a fork or a pinned commit, is
evaluated on this computer. This takes a few minutes one time. To stop it,
press Ctrl-C. The next search continues the work.

Normal search reads complete local catalogs. It runs no Nix command and
makes no network request. Search marks data older than three days as
stale. A failed refresh keeps the last complete catalog.

## Update

`pkg update` updates the client to the newest release, then updates the
search data. The daily job does the same. Installed packages do not
change. To upgrade packages, run `pkg upgrade --all`.

An update downloads the release checksum list from GitHub. It downloads
an archive only when a newer release exists. It starts the new client
once before it replaces the old client. A development build never
replaces the installed client.

To repair the setup, run the install command again.

## Daily job

- macOS: `~/Library/LaunchAgents/com.pkg.catalog-refresh.plist`.
  Log: `~/.local/state/pkg/catalog-refresh.log`.
- Linux: `pkg-catalog-refresh.service` and `pkg-catalog-refresh.timer`
  in `~/.config/systemd/user`. Log:
  `journalctl --user -u pkg-catalog-refresh.service`.

The job runs `pkg update --scheduled`. It does not refresh saved taps,
and it does not change installed packages. It needs a logged-in user
session. Linux also needs a systemd user manager.

These commands control the job:

```sh
pkg background status
pkg background disable
pkg background enable
```

## Check the setup

Open a new terminal window, then run:

```sh
pkg doctor
```

`doctor` reports Nix, the profile, the search data, the daily job, and
`PATH`. It repairs nothing and sends no reports.

## Remove

```sh
pkg self uninstall
```

This command removes the client, completions, the managed shell setup,
the daily job, settings, and the search data. Nix and your installed
packages stay. To remove packages first, run `pkg remove`. To remove
Nix, use the vendor's instructions.

Known limit: if the external launcher helper leaves a Dock pin after an
app is removed, `pkg` does not remove that pin.

## Build from a checkout

```sh
# on x86_64 Linux; use aarch64-darwin on Apple silicon macOS
tools/release/package_client.sh x86_64-linux
```

The archive in `dist/` contains `bin/pkg`, completions, and license
notices. A client that you build yourself does not update itself. Run
the installer to go back to the released client.

On Apple silicon macOS and x86-64 Linux, `cask:` names resolve through
the default source `github:spa5k/pkg/main?dir=nix/casks`. No local
checkout is needed. See [casks](casks.md).
