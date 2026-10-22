# Security policy

## Supported versions

Only the latest release gets security fixes. `pkg update` and the daily job
install it.

## Report a vulnerability

Do not open a public issue, discussion, or pull request for a
vulnerability.

1. Open a [private security advisory](https://github.com/spa5k/pkg/security/advisories/new).
2. Name the affected version. `pkg --version` prints it.
3. Describe the problem, its effect, and the steps to reproduce it.

The maintainer replies in the advisory. Please keep the details private
until a fix is released.

## Scope

In scope:

- the `pkg` client in `crates/`;
- `install.sh`;
- release archives, `SHA256SUMS`, and `SHA256SUMS.sig`;
- the catalog generators in `tools/` and their workflows;
- the Nix flakes in `nix/`.

Out of scope:

- problems in Nix, Nixpkgs packages, Homebrew, or vendor apps. Report them
  to those projects.
- the trust limits that the [README](README.md#limits) names.

## Release key

The release key signs `SHA256SUMS` and the release tags:

```text
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOaFZma99iXoZWruu1raDx1IAPNLJPOF/TYfXbAvyehh
SHA256:bnH/S3tr+6oBsV5Yrfufi2IJSVS7X1YcKeLw6zcvX4M
```

`RELEASE_KEY` in [`crates/pkg-cli/src/client.rs`](crates/pkg-cli/src/client.rs)
holds the same key. The README shows how to
[verify a release](README.md#verify-a-release-yourself).
