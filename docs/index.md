---
title: pkg documentation
---

# pkg

`pkg` is a familiar package manager backed by a private, product-managed Nix runtime. You use
package names and ordinary commands; raw Nix expressions, store paths, daemon access, and trust
configuration stay internal.

The project is in technical-preview development. There is no published installer yet. The checked-in
installer template deliberately refuses to download anything until a release replaces every pinned
version, URL, and SHA-256 placeholder.

## Start here

- [Install safely](install.md)
- [Everyday commands](commands.md)
- [Troubleshooting and support](support.md)
- [Privacy and security](privacy.md)

The [plan index](../plans/README.md) identifies the active implementation plan
and separates it from historical design material. The plan describes a target.
It does not describe delivered alpha behavior.
