# Install `pkg`

`pkg` is a small client over one native Nix profile. You install two things:
the Nix runtime from its vendor, and the `pkg` client from this repository.

## 1. Install Nix

Install [Determinate Nix](https://docs.determinate.systems/determinate-nix/)
yourself. `pkg` never installs, updates, repairs, or removes Nix. `pkg` does
not add you to `trusted-users` and does not use `sudo`.

Supported systems: x86_64 Linux and Apple silicon macOS. On Linux, the
client needs `glibc` 2.35 or newer. Release archives build against that
baseline, so the binary also runs on older distributions.

## 2. Install the client

Download the client downloader for the exact release and run it:

```sh
version="0.2.0-alpha.12"
curl -fsSL -o pkg-install.sh \
  "https://raw.githubusercontent.com/spa5k/pkg/v${version}/install.sh"
sh pkg-install.sh
```

The downloader is pinned to one tag. It fetches
`pkg-${version}-${system}.tar.gz` and `SHA256SUMS` from the
[v0.2.0-alpha.12 release](https://github.com/spa5k/pkg/releases/tag/v0.2.0-alpha.12),
verifies the archive checksum, and installs:

- `~/.local/bin/pkg` — the whole client;
- `~/.local/share/pkg/completions/{bash,zsh,fish}.txt` — completions.

The client and completions land under `~/.local`. The downloader also
prepares catalogs in the pkg cache. It enables a daily user refresh job.
The first preparation can take several minutes. Progress goes to the terminal.
A failed preparation keeps the installed client and reports how to retry.
Setup still attempts to enable the daily job so it can retry later.
It does not install Nix. It does not use `sudo`. It
does not change Nix trust settings. It does not remove or migrate old
installations.

### Build from a checkout instead

```sh
# on x86_64 Linux; use aarch64-darwin on Apple silicon macOS
tools/release/package_client.sh x86_64-linux

version="0.2.0-alpha.12"
system="x86_64-linux"
cat "dist/SHA256SUMS-${system}"          # compare with your own sha256sum

tar -xzf "dist/pkg-${version}-${system}.tar.gz"
mkdir -p ~/.local/bin
install -m 0755 "pkg-${version}-${system}/bin/pkg" ~/.local/bin/pkg
```

`~/.local/bin/pkg` is the whole client. The archive also ships shell
completions under `completions/` and license notices.

## 3. Shell setup

The downloader sets up `PATH` for new shells. It writes one small managed
block into your shell startup files. The block adds three directories:

- `~/.local/bin` — the `pkg` client;
- the detected Nix `bin` directory — for example `/nix/var/nix/profiles/default/bin`;
- `~/.local/state/nix/profiles/pkg/bin` (or `$XDG_STATE_HOME/nix/profiles/pkg/bin`) —
  the packages you install with `pkg`.

The block sits between two marker lines:

```sh
# >>> pkg paths >>>
...
# <<< pkg paths <<<
```

The markers carry no version. Run a newer downloader and it replaces the old
block. It never adds a second one. Lines outside the block, file modes, and
your own settings stay unchanged. Each directory is guarded, so reruns and
repeated shell starts never duplicate a `PATH` entry.

Startup files per shell:

- Bash: the block goes into `~/.bashrc` and into your existing login file —
  the first of `~/.bash_profile`, `~/.bash_login`, `~/.profile`. When none of
  them exists, the downloader creates `~/.profile` only. It never creates
  `~/.bash_profile` or `~/.bash_login`, because a new login file would hide
  an existing `~/.profile`.
- Zsh: an unset or empty `ZDOTDIR` puts the block into `~/.zshrc`. An
  absolute `ZDOTDIR` puts it into `$ZDOTDIR/.zshrc`. A relative `ZDOTDIR`
  is rejected.
- Other shells, including Fish: no file is written. The downloader prints
  the plain directory list. Add the paths in your shell's own syntax.

A startup file that is a symlink is reported and skipped. The downloader
never writes through a link. Startup-file markers that look wrong are also
skipped. In both cases the downloader prints what to do. It never removes
your lines.

### Nix detection

The downloader looks for a usable `nix` without trusting your current
`PATH`. It checks, in order:

1. `nix` on `PATH`;
2. `/nix/var/nix/profiles/default/bin/nix`;
3. `$XDG_STATE_HOME/nix/profile/bin/nix`, or `~/.local/state/nix/profile/bin/nix`;
4. `~/.nix-profile/bin/nix`.

A candidate counts only when it runs and answers `nix --version`. The
detected Nix `bin` directory is always written into the managed block. This
also happens when Nix was on `PATH` for this run, because that `PATH` entry
can come from a one-time export.

When no usable Nix exists, the client stays installed and usable by its full
path. The downloader prints the vendor link and does not claim a healthy
setup. Install Determinate Nix yourself, then open a new terminal and run
`pkg setup`. Then run `pkg doctor`.

### Opt out

Set `PKG_INSTALL_SHELL_SETUP=0` to skip every startup file change. Use this
for CI or for custom install destinations. The downloader then prints the
directories to add yourself. `PKG_INSTALL_BIN` and `PKG_INSTALL_COMPLETIONS`
move the install targets; both must be absolute paths.

### Manual setup

The downloader cannot change the shell that ran it. For Bash and Zsh it
prints one exact `export PATH=...` command for your current terminal. For
other shells it repeats the directory list. New terminals pick the setup up
automatically after a completed setup. For manual control, `pkg shellenv`
still prints idempotent `PATH` settings and never edits your shell files.

To remove shell setup, delete the managed block between the two marker lines.

## 4. Local search and background refresh

The downloader runs `pkg setup` when Nix is present. For a manual archive
installation, run it yourself:

```sh
pkg setup
```

Normal search reads complete local catalogs. It runs no Nix command and
makes no network request. Missing data produces a setup instruction. Search
marks data older than one day as stale. A failed refresh retains the last
complete catalog. Installed packages remain in the native profile.

The daily job runs only official catalog refresh. It does not run tap Ruby.
It does not install or upgrade packages. It exits after each refresh.

- macOS: `~/Library/LaunchAgents/com.pkg.catalog-refresh.plist`.
  Logs: `$XDG_STATE_HOME/pkg/catalog-refresh.log`, or
  `~/.local/state/pkg/catalog-refresh.log`.
- Linux: `pkg-catalog-refresh.service` and `pkg-catalog-refresh.timer`
  under `$XDG_CONFIG_HOME/systemd/user`, or `~/.config/systemd/user`.
  Logs: `journalctl --user -u pkg-catalog-refresh.service`.

Control the job with these commands:

```sh
pkg background status
pkg background disable
pkg background enable
```

The job needs a logged-in user session. Linux also needs a systemd user
manager. If the manager is unavailable, search preparation still succeeds.
Setup reports that the schedule was not activated. Use `pkg update` to
refresh manually.

Set `PKG_INSTALL_BACKGROUND=0` to prepare without a job. Set
`PKG_INSTALL_PREPARE=0` to skip preparation and scheduling. For manual
preparation without a job, run `pkg setup --no-background`.

## 5. Check the setup

Open a new terminal, then run:

```sh
pkg doctor
```

`doctor` reads the runtime, profile, and launcher status. It repairs nothing
and sends no reports. If `nix` was missing from your shell, `pkg` now also
finds it at the stable installation profile paths listed above, so a
working Nix is no longer reported as missing just because `PATH` is short.

## Update the client

Run the downloader for the new release tag. It replaces `~/.local/bin/pkg`
and the completions, and updates the managed block in place. You can also
build the new archive and replace `~/.local/bin/pkg` with its `bin/pkg`.
Your packages and the native profile stay unchanged.

## Remove the client

```sh
pkg background disable
rm ~/.local/bin/pkg
# optional: local settings and disposable cache
rm -rf ~/.config/pkg ~/.cache/pkg
```

Also delete the managed block from your shell startup files if you no longer
want it. Removing the client leaves Nix and your packages intact. To remove
packages, run `pkg remove` first. To remove Nix, use the vendor's
instructions. `pkg` has no system uninstall and performs no machine teardown.

Known limit: if the external launcher helper leaves a Dock pin behind after
an app is removed, `pkg` does not remove that pin. `pkg` has no Dock engine.

On Apple silicon macOS and x86-64 Linux, `cask:` names resolve through the
default source `github:spa5k/pkg/main?dir=nix/casks`. No local checkout is
needed. See [casks](casks.md).
