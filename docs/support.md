---
title: Troubleshooting and support
---

# Troubleshooting and support

Start with:

```console
pkg doctor
```

`doctor` reads the runtime, profile, and launcher status. It repairs
nothing. It sends no reports. If the Nix executable is missing or the
daemon is unavailable, `doctor` states that directly.

## Runtime problems

`pkg` requires an external Nix installation. If `pkg doctor` reports a
missing runtime, install or repair Nix through
[Determinate](https://docs.determinate.systems/determinate-nix/). `pkg`
never installs or repairs the runtime itself.

If `pkg doctor` reports an unavailable daemon, start or enable the Nix
daemon with your administrator tools. Do not add yourself to
`trusted-users` for `pkg`.

## Launcher problems on macOS

If a package operation finishes but the launcher sync fails, the command
reports partial completion. Retry with:

```console
pkg apps sync
```

The retry rebuilds launchers only. It does not change package selection.

## Supported runtime range

The tested baseline is Determinate 3.22.1 with Nix 2.35.2. Record any other
verified runtime here when it is tested. Report unsupported runtimes with
`pkg doctor` output.

## App limits

macOS app exposure supports a small set of packages with simple app
archives or standalone executables. Packages that need a writable
installation or a native helper are excluded.
