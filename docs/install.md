# Install `pkg`

`pkg` is a small client over one native Nix profile. You install two things:
the Nix runtime from its vendor, and the `pkg` client from this repository.

## 1. Install Nix

Install [Determinate Nix](https://docs.determinate.systems/determinate-nix/)
yourself. `pkg` never installs, updates, repairs, or removes Nix. `pkg` does
not add you to `trusted-users` and does not use `sudo`.

Supported systems: x86_64 Linux and Apple silicon macOS.

## 2. Install the client

`v0.2.0-alpha.1` is an **unreleased candidate**. No download is published
yet. Build the archive from a checkout of this repository and install it by
hand:

```sh
# on x86_64 Linux; use aarch64-darwin on Apple silicon macOS
tools/release/package_client.sh x86_64-linux

version="0.2.0-alpha.1"
system="x86_64-linux"
cat "dist/SHA256SUMS-${system}"          # compare with your own sha256sum

tar -xzf "dist/pkg-${version}-${system}.tar.gz"
mkdir -p ~/.local/bin
install -m 0755 "pkg-${version}-${system}/bin/pkg" ~/.local/bin/pkg
```

`~/.local/bin/pkg` is the whole client. The archive also ships shell
completions under `completions/` and license notices. Until your `PATH`
includes `~/.local/bin`, invoke the client by its full path, for example
`~/.local/bin/pkg doctor`.

The release downloader, once published, will only fetch and verify the
client archive. It will not install Nix. It will not use `sudo`. It will
not change Nix trust settings. It will not remove or migrate old
installations. Until a release exists, build the archive from a checkout;
the `install.sh` in this checkout is the client-only downloader.

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

On Apple silicon macOS, before the casks source merges upstream, point pkg
at your local checkout of this repository so `cask:` names resolve. Use an
absolute path with the `nix/casks` subdirectory:

```toml
# ~/.config/pkg/config.toml
[sources]
casks = "path:/ABSOLUTE/PATH/TO/pkg-checkout/nix/casks"
```

The default GitHub reference works only after `nix/casks` is merged and
published on `main`.

Completions for Bash, Zsh, and Fish ship inside the archive under
`completions/`.

## 4. Check the setup

```sh
~/.local/bin/pkg doctor
```

`doctor` reads the runtime, profile, and launcher status. It repairs
nothing and sends no reports.

## Update the client

Build the new archive and replace `~/.local/bin/pkg` with its `bin/pkg`.
Your packages and the native profile stay unchanged.

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
