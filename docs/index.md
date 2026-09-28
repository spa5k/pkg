---
title: pkg documentation
---

# pkg

`pkg` is a small package command line over one native Nix profile per user.
Nix owns package state, generations, rollback, and roots. `pkg` owns names,
commands, output, and desktop app launchers.

The active design is
[Simplify pkg to native Nix](../openspec/changes/simplify-to-native-nix/proposal.md).
The client is in development for x86_64 Linux and Apple silicon macOS.

## Start here

- [Install pkg and set up your shell](install.md)
- [Commands and package IDs](commands.md)
- [Supported runtimes and app limits](support.md)
- [Privacy](privacy.md)

## Ownership

- You install and maintain the Nix runtime, or your administrator does.
- `pkg` installs, lists, upgrades, and removes packages in its own profile.
- `pkg` never installs or invokes Homebrew. Supported Casks are ordinary Nix
  packages.
- `pkg` has no broker, root helper, daemon, or package channel.
