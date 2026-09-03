# Proposal: Installation experience

## Why

Installation is the first contact a user has with the product, and today it is the weakest
surface. The macOS path ships a `.pkg` installer with minimal guidance; Linux users must run
the release workflow's staged binaries by hand; there is no single "get the installer" URL;
and failures mid-install leave users reading journal internals. The domain `kelv.dev` is now
owned, so a stable, friendly, verifiable install path is possible for the first time.

Concrete gaps:

- No `curl | sh` style entry script with preflight checks; the alpha required reading the
  release workflow to find inputs.
- No checksum-verified download path for the installer artifacts outside the proof harness.
- Failure recovery guidance is absent: if the Determinate handoff fails, the user is told an
  error class, not what to do next.
- Post-install prints nothing; the user does not know how to start, verify, or uninstall.

## What Changes

1. A single install entry script `install.sh` (served from `kelv.dev`): detects OS and arch,
   refuses unsupported platforms with a clear message, fetches the TUF-verified release
   manifest, verifies target checksums, and invokes the platform installer with the verified
   inputs.
2. Preflight checks in the script: existing Nix or pkg/kelv installation, disk space, macOS
   version, Linux init system, and network reachability to the channel — each with a plain
   failure message and next action.
3. Installer UX (macOS `.pkg` and Linux): phase progress, failure messages that name the next
   action, and a completion panel that prints the three commands that matter (verify, doctor,
   uninstall).
4. Post-install verification step: the script runs the product's own `doctor` (from the
   CLI-UX change) and prints the table, so a broken install is visible immediately.
5. Uninstall parity: `uninstall.sh` with the same preflight and verification discipline.

## Non-goals

- No Homebrew/apt distribution in this change.
- No Windows support.
- No signing/notarization changes (those belong to the production trust ceremony).
- No rename execution; script and paths keep current names, structured for a one-constant
  rename.

## Impact

- New `tools/install/install.sh`, `tools/install/uninstall.sh`, served copies under the
  future channel host.
- `crates/pkg-installer`: installer panels and failure messaging hooks.
- `docs/`: new install guide page.
