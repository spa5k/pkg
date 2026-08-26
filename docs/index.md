---
title: pkg documentation
---

# pkg

`pkg` is a familiar package manager backed by machine-wide Base Nix. You use
package names and ordinary commands. Raw Nix expressions, store paths, daemon
access, and trust configuration stay outside the normal product interface.

The current Linux source authenticates pinned Determinate Nix Installer 3.22.1
for Base Nix install and terminal uninstall. `pkg` still owns Package Lifecycle,
Package Repair, package policy, and product state. macOS keeps its existing Base
Nix path until separate real-host proof passes.

The project is in technical-preview development. A published alpha installer is
available. The checked-in installer template deliberately refuses to download
anything until a release replaces every pinned version, URL, and SHA-256
placeholder.

## Start here

- [Install safely](install.md)
- [Everyday commands](commands.md)
- [Troubleshooting and support](support.md)
- [Privacy and security](privacy.md)

The [plan index](../plans/README.md) identifies the active implementation plan
and separates it from historical design material. It also records proof that
still blocks the Linux candidate and the macOS cutover.
