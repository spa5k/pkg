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

On Apple silicon with macOS 27, SBCL 2.6.4 cannot start because its static
memory address conflicts with the operating system. The message starts
with `failed to allocate 1048576 bytes at 0x300100000`. This is fixed in
[SBCL 2.6.6](https://www.sbcl.org/all-news.html).

`pkg` pins the app helper dependency to SBCL 2.6.8. If a previously installed
helper has this exact startup failure, `pkg apps sync` builds and checks a
replacement before it switches the helper profile. If that build fails,
the previous helper profile remains in place. The package profile does
not change.

## Supported runtime range

The tested baseline is Determinate 3.22.1 with Nix 2.35.2. Record any other
verified runtime here when it is tested. Report unsupported runtimes with
`pkg doctor` output.

## App limits

macOS app exposure supports a small set of packages with simple app
archives or standalone executables. Packages that need a writable
installation or a native helper are excluded.
