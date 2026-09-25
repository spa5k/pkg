---
title: pkg documentation
---

# pkg

Find and install command-line tools with familiar package commands. pkg uses
trusted downloads first. If a local build is needed, it shows the build plan
and asks before it starts.

[Alpha.51](https://github.com/spa5k/pkg/releases/tag/v0.1.0-alpha.51) is available
for Apple silicon macOS and Linux x86-64 with systemd. Setup installs
Determinate Nix for you. The macOS preview is not notarized. The release uses
a test signing root.

Alpha.51 adds one live build line with the current step and elapsed time.
Package lists and history use clear tables. System checks show status marks.
One verified installer command installs or updates pkg on both platforms.
Use `pkg -h` for common commands and examples.

## Start here

- [Install pkg and set up your shell](install.md)
- [Find, install, and manage packages](commands.md)
- [Fix a problem and collect a support report](support.md)
- [Privacy and security](privacy.md)

Package operations belong to pkg. Determinate owns the Nix installation.
No Nix repair or update command is exposed in this alpha.

The [plan index](../plans/README.md) records development work and native proof.
Source changes are available in public downloads only after a new release.
