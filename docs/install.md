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

Determinate alone owns Base Nix. Product upgrade and Product Asset Repair do
not change Base Nix.

### Three Linux install modes

In the commands below, `./pkg-install` is the authenticated installer artifact
from the product release that you want to run. The current `install.sh` removes
its temporary artifact. Obtain and authenticate the release artifact again for
an upgrade or repair. Same-release repair must not use an artifact from another
release.

#### Fresh Install

Run the installer without an option on a clean host.

```sh
sudo ./pkg-install
```

Fresh Install is the only mode that enables and starts product services. A
successful run prints:

```text
pkg is installed.
```

If Base Nix becomes Accepted but product installation does not finish, the
installer keeps its Fresh Install recovery. It prints:

```text
Base Nix is ready, but pkg product installation is incomplete. Run pkg-install again.
```

Run the same authenticated artifact again without an option. Do not use the
repair option for this case.

```sh
sudo ./pkg-install
```

The retry continues the retained Fresh Install. It does not start the
Determinate installer a second time.

#### Offline product upgrade

An ordinary run against an Accepted installation is a product upgrade. It is
not a Base Nix update. First stop and disable all four product units.

```sh
sudo systemctl stop pkg-nix-broker.service pkg-root-helper.service pkg-nix-broker.socket pkg-root-helper.socket
sudo systemctl disable pkg-nix-broker.service pkg-root-helper.service pkg-nix-broker.socket pkg-root-helper.socket
sudo ./pkg-install
```

All four units must use their exact files under `/usr/lib/systemd/system`.
They must have no drop-ins. Remove all product unit drop-ins before the run.
The installer only queries systemd. It does not stop, disable, reload, start,
or restart a unit.

The upgrade replaces only authenticated product files. It keeps Base Nix, Base
Nix Handoff, package state, roots, and user Generations. A successful run prints:

```text
pkg product files are upgraded. Product services remain offline.
```

Activate the authenticated result after the command succeeds. Use one of these
two choices.

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now pkg-root-helper.socket pkg-nix-broker.socket pkg-root-helper.service pkg-nix-broker.service
```

Or enable the units and reboot.

```sh
sudo systemctl daemon-reload
sudo systemctl enable pkg-root-helper.socket pkg-nix-broker.socket pkg-root-helper.service pkg-nix-broker.service
sudo reboot
```

#### Offline Product Asset Repair

Product Asset Repair is Linux-only. It repairs product files from the same
authenticated release. It is not Package Repair. It is not Base Nix repair.

Stop and disable all four product units while their files are still
authenticated. Remove all product unit drop-ins. Then run the same-release
installer with the exact option.

```sh
sudo systemctl stop pkg-nix-broker.service pkg-root-helper.service pkg-nix-broker.socket pkg-root-helper.socket
sudo systemctl disable pkg-nix-broker.service pkg-root-helper.service pkg-nix-broker.socket pkg-root-helper.socket
sudo ./pkg-install --repair-product-assets
```

Repair requires Accepted Base Nix and the same-release product receipt. The
receipt must show that the product created each repairable file. Repair moves
forward to the authenticated bytes in that same release. It never restores
unknown damaged bytes.

Repair only queries systemd state. It does not execute a product service hook.
It leaves all product services inactive and disabled. A successful run prints:

```text
pkg product files are repaired. Product services remain offline.
```

Use the activation or reboot steps from the upgrade section after you inspect
the result.

Do not run `systemctl stop` or `systemctl disable` after a product binary or unit
file has become untrusted. Those commands can interpret or execute the changed
unit. Product Asset Repair requires the service set to be safely offline first.
Use a trusted rescue procedure if the damage happened before that boundary.
This alpha command does not make an active, changed service safe.

### Refusal and recovery rules

The installer refuses upgrade or repair if one product unit is active, enabled,
unqueryable, uses a different unit fragment, or has a drop-in. It prints:

```text
Stop and disable all pkg product services. Remove all product unit drop-ins. Then run pkg-install again.
```

The installer also refuses a missing, damaged, foreign, or wrong-release
receipt. It does not guess ownership. Product Asset Repair refuses a directory,
account, or other non-file change.

A pending recovery must use the same operation that created it. A mode mismatch
prints:

```text
Use the same pkg-install operation that created the pending recovery.
```

An installer that does not understand the stored recovery schema leaves the
file unchanged. It prints:

```text
Use the pkg-install version that created the pending recovery. The recovery file was not changed.
```

Do not delete or edit a refused recovery file. Use the matching installer and
the matching operation.

Upgrade and repair also require the current product receipt schema. An older,
newer, or malformed product receipt is not migrated by this command. The
installer refuses it and keeps it for review.

Upgrade rollback restores authenticated bytes from the prior receipt. Repair
recovery moves forward to same-release authenticated bytes. Both modes recheck
the offline service state before product-file mutation. They leave services
offline after success, failure, or retry.

A root administrator can still change systemd state between a query and a file
write. The repeated checks reduce this time-of-check/time-of-use risk. They
cannot remove it against another concurrent root process. Do not change product
units or services while upgrade or repair runs.

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
installers and fixed loopback URLs. Both archives include:

- the prepared platform installer;
- checksums for every other archive file;
- the Apache-2.0 license;
- Rust dependency licenses;
- release notes with the test-only limits.

The macOS archive also includes the Nix 2.34.8 LGPL-2.1 text and exact source
information. The Linux archive does not bundle the Determinate installer or a
Nix runtime.

Use `tests/linux-clean-host/run.sh` on a native x86-64 Docker server. The server
can be local or on a disposable GitHub-hosted runner. A GitHub-hosted result is
accepted only for its exact signed commit. Complete logs, its results matrix,
and retained artifacts need independent review.

Use `tests/macos-clean-host/prove.sh` only in a disposable Tart virtual machine
or on another disposable Apple Silicon Mac. Linux and Docker results do not
satisfy macOS proof. Both proofs stop on the first failed check.

The current Linux harness covers foreign-state refusal, ownership drift,
one-start vendor install, repeat product install, cached installs, one approved
local build, package update, package upgrade, package rollback, Package Repair,
isolation, package roots and garbage collection, absence of the old
`/opt/pkg/nix` runtime, and terminal vendor uninstall.

The final native x86-64 run is still pending for the current signed product
commit. Separate blocking native rows also remain for a real N-to-N+1 product
upgrade, same-release Product Asset Repair of a modified service file, vendor
process cgroup behavior, and real systemd offline-state behavior. This document
does not claim a public release or completed native proof for those rows.

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
