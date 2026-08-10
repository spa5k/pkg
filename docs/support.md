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
