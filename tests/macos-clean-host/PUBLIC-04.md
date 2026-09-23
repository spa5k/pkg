# Public alpha lifecycle checks — 23 September 2026

The requested macOS checks passed in disposable Apple Silicon VMs.
This is local regression evidence for PUBLIC-02 and PUBLIC-03.
It does not replace the sealed, two-slot [DN-16 proof](README.md).
It does not certify a new public release.

## Inputs and limits

- macOS 15.7.7 (24G720), `VirtualMac2,1`, 4 CPUs, 8 GiB RAM, 100 GB disk.
- Tart base image:
  `ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c`.
- Installed runtime: published alpha.46, from source `23689a4`.
  The installed broker and helper remained the signed alpha.46 artifacts.
- Automatic-upgrade installer: PUBLIC-02 commit `7fbc8c6`.
- Fresh-install wrapper and installer: PUBLIC-03 commit `f1b0c4d`.
  Both installers used the public alpha.46 trust root and channel.
- Package lifecycle CLI: PUBLIC-03 commit `f1b0c4d`.
  The final remove/rollback message check used `105bfde`.
  Candidate CLIs ran from a separate user path.
- Determinate installer 3.22.1; installed Nix 2.35.2.
- Package source revision: `a62e6edd6d5e1fa0329b8653c801147986f8d446`.

The fresh preview package had local ad-hoc signing and package version
`0.0.0-test`. Its embedded installer reported alpha.46.
It fetched the existing published runtime, not newly released PUBLIC-03 binaries.
This separates installer evidence from CLI evidence.
The old installed CLI still has the known doctor ownership warning and lacks
`shellenv`. The candidate CLI supplies `shellenv`; the doctor correction is in
merged PR #31. A new release must package these changes together.

| Local artifact | SHA-256 |
|---|---|
| Fresh candidate `pkg-install` | `d1fe9751324790ac05de00d90fcabe6dac8480c17e2470fea1c1ebb31f79ca2a` |
| `pkg-candidate-preview.pkg` | `a0b9d4fc89a7a3486fa56eb9e4a8be9a7aac17b13e942ad68478395a527fa781` |

## Results

| Check | Observed result |
|---|---|
| Fresh install | The foreground preview wrapper completed on a clean VM. No Nix receipt, `/opt/pkg`, or public CLI existed before setup. Determinate installation, file checks, service checks, and receipt publication passed. The wrapper returned 0. A cached `fzf` install then ran successfully. |
| Active product upgrade | Restored the retained, real alpha.45 product snapshot and its ownership receipt. With both services running, the candidate installer upgraded to alpha.46 and started both new services. Base Nix receipt/handoff digests and every per-user package-file digest were unchanged. |
| Repeat and partial service retry | An exact repeat succeeded. A retry with one product service stopped also succeeded. Both services answered afterward. |
| Compiled package with cold inputs | Removed the previous package set and pruned four generations and 954 store paths. Forced only the `tmux` output cache lookup to miss. The real CLI fetched build inputs, compiled `tmux`, activated it, and returned 0. `tmux -V` reported `tmux 3.3a`. |
| Remove and rollback | Installed and ran `fzf`, removed it, then restored it with `pkg rollback`. Its filter returned `banana` before removal and after rollback. |
| Package repair | Damaged the resolved `fzf` executable inside the disposable Nix store. Verify-only repair refused with `VERIFY_FAIL`. Approved repair restored the exact original SHA-256 from the signed cache. The executable ran and verify-only repair was clean. |
| Interrupted product upgrade | Sent SIGKILL after the durable journal recorded `broker-binary` as `replaced`, while `mode=offlineUpgrade` and `committed=false`. The same installer recovered the transaction, removed its journal, and started both services. |
| Guest reboot | Rebooted macOS with `shutdown -r now`. Both launchd services ran after boot. `tmux` and `fzf` ran. Every per-user package-file digest matched the pre-reboot inventory. |
| Live uninstall | Ran `sudo pkg --yes uninstall` in a terminal. The vendor reported successful Nix uninstall and returned 0. Product binaries, plists, services, accepted handoff, Nix receipt, Nix mount, and Nix Store APFS volume were absent. |
| Final command messages | Dry-run remove/rollback still reported a preview. Actual operations reported completion. The candidate `shellenv` printed the Bash/zsh PATH setup. |

The cold build started with a 596 MiB store and ended at 1.4 GiB.
The engine and reachable runtime paths remained installed.
This was not an empty-store or fully offline build.
The `tmux` output was absent before the test.
Its retained build log contains compiler detection and 154 compiler-related lines.
This proves a compiled local build, beyond the earlier header-only package test.

The interrupted-upgrade check did not interrupt Determinate or change Base Nix.
It proves one recorded product-file replacement boundary, not every crash point.
The reboot check was a real guest reboot, not a launchd restart.
After uninstall, the empty synthetic `/nix` node remained until reboot.
The test does not claim that all vendor-owned residue was removed.

## Representative output

Fresh setup:

```text
pkg setup — 0.1.0-alpha.46
Downloading and verifying the release. This can take a few minutes...
Checking this Mac...
Preparing pkg files and folders...
Preparing the package and build engine...
Installing Determinate Nix. This can take a few minutes. Please keep this window open.
Determinate Nix installation completed.
Installing pkg commands and services...
Verifying the installed files...
Checking the package engine...
Saving the installation record...
pkg is installed.
```

Actual compiled build, using the installed alpha.46 CLI:

```text
tmux cache response: 404
Checking for a trusted download...
Package source ready.
Preparing a local build...
Building tmux 3.3a...
Building: 0%
Building: 50%
Building: 100%
Local build complete.
```

Final PUBLIC-03 message check:

```text
1 package(s) ready to remove
Packages removed. Your package environment is ready.
rollback to gen-0001 ready
Restored the package environment from gen-0001.
```

## Reusable checks

Run these only inside a disposable macOS VM.
The helpers reject a host that is not `VirtualMac`.
They use the real package service and native tools.
They do not mock the installer, broker, helper, Nix store, or package build.

### Package lifecycle

With a working installation and a current candidate CLI:

```sh
/bin/bash tests/macos-clean-host/public-package-lifecycle.sh /path/to/candidate/pkg
```

Run as the ordinary VM test user. The damage step requests sudo.
The script checks the resolved package path before damage.
It stops on a failed assertion and retains the VM for inspection.
This script ran end to end for the recorded package lifecycle result.

### One output cache miss

Use [cache_miss_proxy.py](cache_miss_proxy.py) for the cold compiled-build check.
Resolve a real `cache.nixos.org` upstream IP before changing guest DNS.
Create a temporary test certificate for `cache.nixos.org` inside the VM.
Keep its private key in a root-owned mode-0700 directory.

Back up `/etc/hosts`, `/etc/ssl/cert.pem`, and `/etc/nix/macos-keychain.crt`
with ownership and modes preserved.
Append the test certificate to the two guest trust files.
Map `cache.nixos.org` to `127.0.0.1` in the guest hosts file and flush DNS.
Then run the proxy as root:

```sh
sudo /usr/bin/python3 tests/macos-clean-host/cache_miss_proxy.py \
  --upstream-address "$upstream_ip" \
  --missing-hash 4gzrlwrp0xwycby9f57qhvdprhfn77gs \
  --certificate /var/root/pkg-cache-miss-proof/cert.pem \
  --private-key /var/root/pkg-cache-miss-proof/key.pem
```

The fixed hash is for this pinned `tmux 3.3a` output only.
The proxy returns 404 for that one `.narinfo` request.
All other response bodies pass through unchanged from the real cache.
The upstream connection retains TLS hostname verification.
Nix cache signatures, trusted keys, package source, and channel metadata stay unchanged.

Confirm that the output path is absent and that only its cache lookup returns 404.
Run `pkg --yes --verbose install tmux` as the VM test user.
Keep the CLI output and the Nix build log.
Run the resulting `tmux -V`.
Stop the proxy, restore all three saved files, compare their bytes, and flush DNS.
Restore the files even if the build fails.

The completed build used a one-off form of this helper with the same fixed inputs.
The extracted helper was then checked with a real forwarded `nix-cache-info`
request and the forced 404. It did not run a second full build.
All three modified guest files matched their backups after the build.

### Interrupted product upgrade

Start with an older authenticated product installation and accepted Base Nix.
Use a candidate installer that authenticates the intended newer release.
Run:

```sh
sudo /usr/bin/python3 tests/macos-clean-host/interrupt_product_upgrade.py \
  /path/to/candidate/pkg-install /var/root/pkg-upgrade-interruption
```

The helper requires an accepted handoff before starting.
It waits for an uncommitted offline-upgrade replacement, kills that product
installer process group, saves the journal, and retries the same installer.
It requires journal removal and both running product services after recovery.
It keeps the interrupted and retry logs.

The recorded run used a one-off driver that also restored the retained alpha.45
product snapshot. The checked-in helper extracts the interruption/retry procedure;
its syntax and argument handling were checked. It has not independently repeated
the full upgrade. It intentionally leaves old-release provisioning to the caller.

## Retained evidence and remaining release work

The local evidence directory is `~/pkg-public-validation-2026-09-23`.
It retains complete captured logs, the compiler log, local drivers, installer,
preview package, and SHA-256 inventory. It excludes temporary TLS private keys.
The product upgrade also retained the killed transaction journal and both root
installer logs in the disposable VM.
This evidence has not received independent assurance review.

The test VMs were stopped after collection.
The host Mac's installed product binaries and services were not changed.

Before public distribution, build and test one release that contains all merged
changes. Production TUF keys, Developer ID signing, and notarization remain
separate release requirements. Linux CI covers the native systemd container
lane; this macOS report does not extend platform support or prove Linux reboot.
