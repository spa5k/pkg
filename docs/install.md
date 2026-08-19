---
title: Install pkg
---

# Install pkg

The current public release is
[`v0.1.0-alpha.3`](https://github.com/spa5k/pkg/releases/tag/v0.1.0-alpha.3).
`docs/install.sh` is the source template for its fixed Linux installer. An
unrendered template exits before network access.

The first preview targets Linux x86-64 and macOS arm64. Linux arm64 is deferred.

## Linux x86-64

Download and read the release installer. Then run it.

```sh
curl -fsSLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.3/install.sh
less install.sh
sh install.sh
pkg doctor
```

The script accepts `--verify-only`. It does not accept a caller URL, checksum,
target, install path, or Nix setting.

## macOS Apple silicon

Download the package and its checksums. Then install it.

```sh
curl -fsSLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.3/pkg-0.1.0-alpha.3-preview.pkg
curl -fsSLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.3/SHA256SUMS
grep '  pkg-0.1.0-alpha.3-preview.pkg$' SHA256SUMS | shasum -a 256 --check
sudo installer -pkg ./pkg-0.1.0-alpha.3-preview.pkg -target /
pkg doctor
```

The embedded `pkg-install` uses an ad-hoc signature. The package is not
Developer ID signed or notarized. These items remain TODO items.

## Local candidate proof

Local candidate archives are separate test artifacts. They contain test-key
installers and fixed loopback URLs. Each archive includes:

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
