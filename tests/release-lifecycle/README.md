# Exact public release checks

`check.py` runs only as a normal test user on a GitHub-hosted disposable runner
or a VirtualMac guest. Every run also requires `--confirm TEST-DISPOSABLE-HOST`.
It refuses a fresh proof on a host that already has pkg or a Nix receipt.
It never deletes existing installations to make a test pass.

The harness downloads exact alpha release files. It verifies each Sigstore
bundle against the repository, release workflow, and tag before execution.
It renders the same public installer template used for normal setup.

## Current release smoke test

The manual [release-check workflow](../../.github/workflows/release-check.yml)
runs fresh installation, repeat installation, package execution, removal,
rollback, verification, and doctor on Linux and macOS. macOS also installs and
runs the public `just` flake. Linux public flakes remain unsupported.
Select the release currently served by the live alpha channel.

Hosted CI tool images can give the runner ownership of `/usr/local` or
`/usr/local/bin`, or make `/opt` writable by all users. The workflow records those directory permissions, then sets
only those three directories to root:0 mode 0755 before installation. It does not
change child files. This fixture step refuses local hosts, symlinks, and existing
pkg or Nix installations. Product ownership checks remain enabled.

Before a hosted macOS public build, the fixture waits up to ten minutes for
three CPU-load samples, 15 seconds apart, at or below the logical CPU count.
It records every sample and fails if capacity does not become available.
This avoids treating a busy newly provisioned runner as build-ready. The
product still applies its own disk and load checks; their limits are unchanged.

The workflow retains a result table, individual logs, exact-byte checks, and a
checkpoint. It does not reboot a hosted runner and does not claim reboot proof.

## Native upgrade and reboot

Use one disposable VM for all three phases. The public installers use a live
channel, so downloading two historical installers after publication cannot
prove an old-to-new upgrade. Keep the old installation before advancing the
channel.

Run the following phases with the same absolute `--work` directory and fixed
`--from-release` and `--to-release` alpha tags:

1. `prepare`: run while the old release is active. Install it, compare installed
   binaries with signed release bytes, install fzf, and record the Nix identity.
2. `upgrade`: run after the new channel is active. Check the original installed
   bytes, install the new release, verify exact new bytes and unchanged Nix
   records, and run the package/build/removal/rollback checks.
3. Reboot the same VM.
4. `resume`: require a different boot UUID on the same host, recheck exact
   installed bytes, and run doctor and the installed commands.

The final checkpoint reports `passed` only after resume succeeds.
The three phases support automated VM orchestration. They do not reboot or
replace the VM themselves.

Product interruption and recovery remain covered by the
[existing native lifecycle proof](../macos-clean-host/PUBLIC-04.md) and
[interruption harness](../macos-clean-host/interrupt_product_upgrade.py).
Do not interrupt a fresh vendor Nix installation. That is a separate ownership
boundary, not a safe product-upgrade cancellation test.
