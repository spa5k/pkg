---
title: Troubleshooting and support
---

# Troubleshooting and support

Start with:

```console
pkg doctor
```

To create a support preview:

```console
pkg doctor --support
```

The command prints the complete JSON bundle to standard output and uploads nothing. It remains
available when ordinary health checks fail. Review or redirect those exact bytes yourself; there is
no background sender.

The V1 bundle contains the CLI version, friendly OS and architecture, typed health statuses,
coarse recent operation phase/outcome, and aggregate state size/permissions. Channel, managed-runtime,
and index details stay explicitly `null` or `deferred` until their authenticated observations are
wired. It excludes command arguments, environment values, package names, paths, file contents,
network addresses, raw logs, Nix identities, and secrets.

## Install problems

The terminal installer prints an `Install log:` path before it starts.
Keep that file if setup fails. It contains the failed stage and the installer
result. It is private and is not uploaded. Review it before you share it.

If macOS reports `Operation not permitted`, check the terminal app in
**System Settings > Privacy & Security > Full Disk Access**, then close and
reopen that app. This setting does not repair an interrupted Nix installation.
Do not delete installation records or APFS volumes to force a retry.

Alpha.46 can show a false unmanaged-Nix row in `pkg doctor` after a valid
Determinate install. Alpha.47 contains the correction. Retain the
support report instead of removing Nix based on this row alone.

For more installer stage details with the terminal wrapper, set
`PKG_INSTALL_DEBUG=1` when you run the verified `install-preview.sh` script.
A setup failure returns a nonzero exit code. A successful PackageKit message
from an older background installer does not prove that setup finished.
