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
version="0.2.0-alpha.2"
curl -fsSL -o pkg-install.sh \
  "https://raw.githubusercontent.com/spa5k/pkg/v${version}/install.sh"
sh pkg-install.sh
```

The downloader is pinned to one tag. It fetches
`pkg-${version}-${system}.tar.gz` and `SHA256SUMS` from the
[v0.2.0-alpha.2 release](https://github.com/spa5k/pkg/releases/tag/v0.2.0-alpha.2),
verifies the archive checksum, and installs only:

- `~/.local/bin/pkg` — the whole client;
- `~/.local/share/pkg/completions/{bash,zsh,fish}.txt` — completions.

Everything lands under `~/.local`. Until `~/.local/bin` is on your `PATH`,
invoke the client by its full path, for example `~/.local/bin/pkg doctor`.

The downloader fetches and verifies the client archive only. It does not
install Nix. It does not use `sudo`. It does not change Nix trust settings.
It does not remove or migrate old installations.

### Build from a checkout instead

```sh
# on x86_64 Linux; use aarch64-darwin on Apple silicon macOS
tools/release/package_client.sh x86_64-linux

version="0.2.0-alpha.2"
system="x86_64-linux"
cat "dist/SHA256SUMS-${system}"          # compare with your own sha256sum

tar -xzf "dist/pkg-${version}-${system}.tar.gz"
mkdir -p ~/.local/bin
install -m 0755 "pkg-${version}-${system}/bin/pkg" ~/.local/bin/pkg
```

`~/.local/bin/pkg` is the whole client. The archive also ships shell
completions under `completions/` and license notices.

## 3. Set up your shell

Until `~/.local/bin` is on your `PATH`, call the client by its full path:

```sh
~/.local/bin/pkg doctor
```

To use the packages you install, add the pkg profile to your `PATH`:

```sh
eval "$(~/.local/bin/pkg shellenv)"   # try it first
```

To use the plain `pkg` command everywhere, also add `~/.local/bin` to your
`PATH` in `~/.bashrc` or `~/.zshrc`:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Then put the same `eval "$(pkg shellenv)"` line in `~/.bashrc` or
`~/.zshrc`. `pkg shellenv` prints idempotent settings. It never edits your
shell files.

On Apple silicon macOS, `cask:` names resolve through the default source
`github:spa5k/pkg/main?dir=nix/casks`. No local checkout is needed. See
[casks](casks.md).

The downloader installs completions under
`~/.local/share/pkg/completions/`; the archive also carries them under
`completions/`.

## 4. Check the setup

```sh
~/.local/bin/pkg doctor
```

`doctor` reads the runtime, profile, and launcher status. It repairs
nothing and sends no reports.

## Update the client

Run the downloader for the new release tag. It replaces `~/.local/bin/pkg`
and the completions. You can also build the new archive and replace
`~/.local/bin/pkg` with its `bin/pkg`. Your packages and the native
profile stay unchanged.

## Remove the client

```sh
rm ~/.local/bin/pkg
# optional: local settings and disposable cache
rm -rf ~/.config/pkg ~/.cache/pkg
```

Removing the client leaves Nix and your packages intact. To remove packages,
run `pkg remove` first. To remove Nix, use the vendor's instructions. `pkg`
has no system uninstall and performs no machine teardown.

Known limit: if the external launcher helper leaves a Dock pin behind after
an app is removed, `pkg` does not remove that pin. `pkg` has no Dock engine.
