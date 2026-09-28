# Release tooling

`package_client.sh` builds and packages the standalone `pkg` client
(NN-08, `standalone-client-delivery`).

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

The recipe has not yet been run against the final one-crate workspace.
Run it and inspect `dist/` before the first release tag.

The old channel, TUF, installer, and proof-pair tooling was deleted with its
subjects. Historical release evidence stays under `tests/`.
