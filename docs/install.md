---
title: Install pkg
---

# Install pkg

No public release exists yet. `docs/install.sh` is a release template. It exits
before network access until the release process replaces every fixed token.

The first preview targets Linux x86-64 and macOS arm64. Linux arm64 is deferred.

## Public release process

For Linux:

1. Download `install.sh` and `SHA256SUMS` from the exact signed release.
2. Verify `SHA256SUMS`.
3. Read `install.sh`.
4. Run `sh install.sh`.
5. Run `pkg doctor`.

The script accepts `--verify-only`. It does not accept a caller URL, checksum,
target, install path, or Nix setting.

For macOS:

1. Download the exact signed package and `SHA256SUMS`.
2. Verify `SHA256SUMS`.
3. Run `sudo installer -pkg v0.1.0-alpha.1/pkg-0.1.0-alpha.1-preview.pkg -target /`.
4. Run `pkg doctor`.

The embedded `pkg-install` uses an ad-hoc signature. The package is not notarized.
Do not publish it. Developer ID signing and notarization remain TODO items.

## Local candidate proof

Local candidate archives contain test-key installers and fixed loopback URLs.
They are not public releases. Each archive includes:

- the prepared platform installer;
- checksums for every other archive file;
- the Apache-2.0 license;
- Rust dependency licenses;
- the Nix 2.34.8 LGPL-2.1 text and exact source information;
- release notes with the test-only limits.

Use `tests/linux-clean-host/run.sh` in a disposable Linux Docker host. Use
`tests/macos-clean-host/prove.sh` only in a disposable Tart virtual machine.
Both proofs stop on the first failed check.

The Linux proof covers foreign-state refusal, interrupted install recovery,
ownership drift, install, retry, cached installs, one approved local build,
update, upgrade, rollback, repair, isolation, and uninstall.

The macOS proof covers the same product flow with the macOS service, APFS, and
account boundaries. A local Tart result does not prove Developer ID signing,
notarization, or Gatekeeper acceptance.

## Uninstall

Run `pkg uninstall --dry-run` to preview product-owned assets. Run
`pkg uninstall` to remove them. The command removes only authenticated `pkg`
state. It refuses changed, unrecorded, or foreign state and keeps it for manual
review.
