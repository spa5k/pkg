# `pkg`

`pkg` is a pre-launch Rust package manager for Linux and macOS.

The command interface is similar to Homebrew and paru. Nix is the private build
and package engine. Users do not run Nix commands or manage Nix settings.

## Current state

The main product flows are implemented. They include search, package details,
install, remove, list, update, upgrade, rollback, repair, doctor, garbage
collection, and safe uninstall.

The product has these local clean-host proofs:

- Linux x86-64 uses a privileged Docker host.
- macOS arm64 uses a disposable Tart virtual machine.

The proofs cover cached installs, one approved local build, upgrade, rollback,
repair, safe retry, ownership drift, and uninstall. They also verify that a
normal user cannot use the private runtime, helper, daemon, or trust controls.

No public release exists yet. Current release candidates use test signing keys
and a loopback service. Do not publish them.

Linux arm64 is not in the first preview. The macOS package is not Developer ID
signed or notarized. These items remain TODO items.

## Security boundary

The product uses this fixed boundary:

```text
pkg CLI -> local broker -> non-root runtime broker -> privileged helper -> managed runtime
```

The installer refuses foreign or changed Nix state. The uninstaller removes
only authenticated `pkg` state. Product commands do not accept raw Nix
commands, expressions, installables, store paths, trust roots, or arbitrary
Nix options.

## Install

See [the install guide](docs/install.md). A public install command will be
added after production TUF signing and fixed HTTPS hosting are ready.

## Project documents

- [Plan index](plans/README.md)
- [Architecture decisions](plans/00-overview-and-decisions.md)
- [Security model](plans/08-security-model.md)
- [PR roadmap](plans/11-pr-roadmap.md)
- [Open decisions and risks](plans/12-open-decisions-and-risks.md)
- [Contributor guide](CONTRIBUTING.md)

## License

Licensed under the [Apache License 2.0](LICENSE).
