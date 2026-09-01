# DN-08 vendor configuration extension proof

| | |
|---|---|
| **Question** | Can the pinned Determinate Nix Installer give `pkg` one vendor-owned configuration path for Broker admission, `trusted-users`, and `allowed-users`? |
| **Pinned vendor** | `nix-installer` v3.22.1, commit `4132ad07a15ee7d88c096ac7172b7afb2672866b` |
| **Result** | **NO-GO for DN-08.** The installer has a usable configuration path. It is not yet proved for Broker admission or for preservation through the required Linux aarch64 and Apple Silicon macOS update paths. |

## Scope and method

This report uses primary sources only. It does not change product code. It does
not change the host Nix installation. It does not copy a vendor receipt.

1. Context7 resolved the official Determinate documentation. It says that
   Determinate Nix owns `/etc/nix/nix.conf` and custom settings belong in
   `/etc/nix/nix.custom.conf`. [Determinate documentation](https://docs.determinate.systems/determinate-nix)
2. I cloned the official source at the exact pinned commit into a temporary
   directory. The checked-out commit was
   `4132ad07a15ee7d88c096ac7172b7afb2672866b`.
3. I ran the pinned source test `extra_trusted_users`. It passed. The build
   required empty temporary payload files because upstream compile-time build
   variables are normally supplied by its release build.
4. I ran one disposable, automatically removed `linux/arm64` Docker container.
   It used the exact pinned release binary as a read-only bind mount. The local
   SHA-256 was `9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179` and the
   size was `69625424`, matching the checked-in release inventory.

The container command was:

```text
nix-installer --diagnostic-endpoint http://127.0.0.1:18080 install linux \
  --determinate --no-confirm --no-modify-profile --init none \
  --extra-conf 'trusted-users = root pkg-nix-broker' \
  --extra-conf 'allowed-users = root pkg-nix-broker' \
  --extra-conf 'sandbox = false'
```

It then compared `/etc/nix/nix.custom.conf` before and after default repair.
The container had no network. It was removed by `docker run --rm`.

## Source-proven facts

- `--extra-conf` accepts one or more Nix configuration inputs. The v3.22.1
  README documents it as extra configuration and exposes
  `NIX_INSTALLER_EXTRA_CONF`. [README](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/README.md#installer-settings)
- The installer parses all supplied lines as Nix configuration. It only changes
  `build-users-group`, `ssl-cert-file`, and `experimental-features`; it leaves
  `allowed-users` unchanged. [place_nix_configuration.rs](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/place_nix_configuration.rs#L194-L310)
- For Determinate Nix, the installer asks `determinate-nixd` to create the
  vendor main configuration, then writes the extra configuration to
  `/etc/nix/nix.custom.conf`. `pkg` would supply settings; the vendor remains
  the writer. [place_nix_configuration.rs](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/place_nix_configuration.rs#L38-L112), [execution path](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/place_nix_configuration.rs#L347-L385)
- `trusted-users` has an upstream unit test. It remains in the custom file.
  The test also verifies the normal upstream `nix.conf` case. The local pinned
  source test passed. [test](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/place_nix_configuration.rs#L555-L638)
- Default `repair` only repairs shell hooks and, on macOS, remote-building
  hooks. It does not plan Nix configuration changes. [repair.rs](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/subcommand/repair.rs#L189-L211)
- The pinned source does not contain `allowed-users`. Its generic parser accepts
  the line, but it has no setting-specific test. Nix documents `allowed-users`
  as the daemon connection allowlist. [Nix reference](https://nix.dev/manual/nix/2.28/command-ref/conf-file)

## Runtime-proven facts

| Target and operation | Result | Meaning |
|---|---|---|
| Linux aarch64 install | PASS | All three extra lines were present in `/etc/nix/nix.custom.conf`. |
| Linux aarch64 default repair | PASS | A byte-for-byte comparison showed that `nix.custom.conf` did not change. |
| Linux aarch64 same-version update | NOT PROVED | `determinate-nixd upgrade --version v3.22.1` failed to connect because this safe container used `--init none`; it has no daemon socket. |
| Apple Silicon macOS install, repair, update | NOT PROVED | The accepted macOS R10 lifecycle did not pass these extra lines or record this file. |

The existing Linux aarch64 evidence only proves `sandbox = false` during an
`--init none` install. It did not run repair or update. The existing broad
Linux lifecycle proves default repair and one same-version update on x86_64,
but it does not record `nix.custom.conf`. [Linux evidence](linux-vm/LINUX-FINDINGS.md#linux-aarch64-asset-proof-mapping), [x86_64 lifecycle](linux-vm/LINUX-FINDINGS.md#5-lifecycle-observations).

## What remains unproved

- That the closed `determinate-nixd` includes `nix.custom.conf` at runtime for
  this pinned release on both required platforms. The installer source invokes
  the closed binary to initialize Determinate Nix.
- That `pkg-nix-broker` can connect through the standard daemon when
  `allowed-users` and `trusted-users` are set this way. The narrow container
  did not create the product account or run a normal daemon.
- That the same-version update preserves the file and its effective settings on
  Linux aarch64 and Apple Silicon macOS. The daemon implementation is closed.
- That a real N-to-N+1 update preserves the file. The prior evidence proves
  only a same-version x86_64 probe.

## Required next proof

Use two clean disposable guests: native Linux aarch64 with systemd and Apple
Silicon macOS with launchd. For each guest, install with the exact three lines
above. Then prove all of the following before DN-08 code changes:

1. The vendor main configuration includes the custom file.
2. The Broker account can connect and receives the intended trusted policy.
3. An ordinary unlisted account is refused.
4. Default repair preserves the file and both observed access outcomes.
5. The exact same-version daemon update preserves the file and both outcomes.
6. No `pkg` code writes `/etc/nix/nix.conf` or `/etc/nix/nix.custom.conf` after
   the vendor process has run.

Until that evidence exists, retain the active legacy Nix configuration path.
Do not add a target ownership receipt for vendor files. Do not enable the
standard-daemon route.

## Checks

```text
git -C <temporary official source> rev-parse HEAD
# 4132ad07a15ee7d88c096ac7172b7afb2672866b

env NIX_TARBALL_URL=https://example.invalid/nix.tar.xz \
  DETERMINATE_NIX_TARBALL_PATH=<empty temporary file> \
  DETERMINATE_NIXD_BINARY_PATH=<empty temporary file> \
  cargo test --locked extra_trusted_users --lib
# 1 passed

docker version --format 'client={{.Client.Version}} server={{.Server.Version}} {{.Server.Os}}/{{.Server.Arch}}'
# server=29.4.1 linux/arm64
```

The checked-out source commit carries its upstream release signature, but this
host lacks `gpg`, so local `git verify-commit` could not run. The repository's
existing S6 findings record GitHub verification of the same commit.
