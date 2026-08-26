---
title: Install pkg
---

# Install pkg

The current public release is
[`v0.1.0-alpha.7`](https://github.com/spa5k/pkg/releases/tag/v0.1.0-alpha.7).
`docs/install.sh` is the source template for its fixed Linux installer. An
unrendered template exits before network access.

The first preview targets Linux x86-64 and macOS arm64. Linux arm64 is deferred.

## Linux x86-64

Download and read the release installer. Then run it.

```sh
curl -fsSLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.7/install.sh
less install.sh
sh install.sh
pkg doctor
```

The script accepts `--verify-only`. It does not accept a caller URL, checksum,
target, install path, or Nix setting.

The current Linux candidate authenticates the pinned Determinate Nix Installer
3.22.1 executable. It starts that vendor installer once. One supervisor drains
bounded output, waits for the process, and reaps it.

After vendor start, there is no safe product cancellation, signal, hard
timeout, or parent-death guarantee. A stored `Started` state means an Unknown
Base Nix Outcome. `pkg` fails closed and does not retry it. Only vendor exit
status `0` followed by installed-state validation becomes `Accepted`.

## macOS Apple silicon

Download the package and its checksums. Then install it.

```sh
curl -fsSLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.7/pkg-0.1.0-alpha.7-preview.pkg
curl -fsSLO https://github.com/spa5k/pkg/releases/download/v0.1.0-alpha.7/SHA256SUMS
grep '  pkg-0.1.0-alpha.7-preview.pkg$' SHA256SUMS | shasum -a 256 --check
sudo installer -pkg ./pkg-0.1.0-alpha.7-preview.pkg -target /
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

Use `tests/linux-clean-host/run.sh` on a native x86-64 Docker server. The server
can be local or on a disposable GitHub-hosted runner. A GitHub-hosted result is
accepted only for its exact signed commit. Complete logs, its results matrix,
and retained artifacts need independent review.

Use `tests/macos-clean-host/prove.sh` only in a disposable Tart virtual machine
or on another disposable Apple Silicon Mac. Linux and Docker results do not
satisfy macOS proof. Both proofs stop on the first failed check.

The Linux proof covers foreign-state refusal, ownership drift, one-start vendor
install, repeat product install, cached installs, one approved local build,
package update, product upgrade, rollback, Package Repair, isolation, and
terminal vendor uninstall.

The macOS proof covers the same product flow with the macOS service, APFS, and
account boundaries. A local Tart result does not prove Developer ID signing,
notarization, or Gatekeeper acceptance.

## Uninstall

Run `pkg uninstall --dry-run` to preview product-owned assets. Dry-run can use a
structured format. In the current Linux source, run live `pkg uninstall` with
plain terminal output. Linux refuses live JSON or JSONL output before mutation.
This restriction remains Linux-only until PR 4 proves and adopts the terminal
boundary on macOS.

On Linux, `pkg` first removes and verifies all product-owned state. It then
revalidates the installed Determinate executable and its opaque receipt. The
final action replaces `pkg` with the vendor uninstaller. The vendor owns its
signals, status, temporary files, self-copy, native cleanup, and residue.

The command refuses changed, unrecorded, or foreign state. It keeps that state
for manual review. Determinate can leave vendor-owned residue. `pkg` does not
delete that residue or infer uninstall success from its absence.
