# Release tooling

`package_client.sh` builds and packages the standalone `pkg` client
(NN-08, `standalone-client-delivery`). `sign.sh` signs a draft release.

Usage:

```sh
tools/release/package_client.sh x86_64-linux    # on x86_64 Linux
tools/release/package_client.sh aarch64-darwin  # on Apple silicon macOS
```

It writes to `dist/`:

- `pkg-<version>-<system>.tar.gz`
- `SHA256SUMS-<system>`

The version comes from Cargo metadata for `pkg-cli`. The archive contains
`bin/pkg`, `completions/`, `LICENSE`, `NOTICES.md`, and `licenses/` with
the third-party Rust dependency license texts and notices plus
`licenses/THIRDPARTY.md`. It contains no privileged helper, private Nix
bundle, or product catalog asset.

The `client-release` workflow attaches both archives, one merged
`SHA256SUMS`, and the tag's `install.sh` to a draft release. Then sign the
draft on the maintainer computer:

```sh
tools/release/sign.sh v<version>
```

`sign.sh` downloads the draft archives and `SHA256SUMS`, checks the
checksums, signs `SHA256SUMS` with `ssh-keygen -Y sign -n pkg-release`,
verifies the signature, and attaches `SHA256SUMS.sig`. The key must be
`RELEASE_KEY` in `crates/pkg-cli/src/client.rs`. `pkg update` refuses a
release without a valid signature, so do not publish an unsigned draft.
To change the key, ship a client with the new `RELEASE_KEY`, signed by the
old key, before you sign with the new key.

Publish the draft as the latest release. Do not mark it as a pre-release.
The stable install URL `releases/latest/download/install.sh` and
`pkg update` read only the latest release.

`install.sh` checks the archive checksum but not the signature. A new
install trusts GitHub and TLS. The signature protects installed clients
from a changed release.

The recipe was run for the `v0.2.0-alpha.1` candidate; the produced
archive is covered by the
[verification records](../../docs/verification/native-client-quality-2026-09-28.md).
Inspect `dist/` after any local run.

The old channel, TUF, installer, and proof-pair tooling was deleted with its
subjects. Historical release evidence stays under `tests/`.
