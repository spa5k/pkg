---
title: Everyday pkg commands
---

# Everyday commands

```console
pkg search ripgrep
pkg info ripgrep
pkg install ripgrep
pkg list
pkg outdated
pkg upgrade --all
pkg history
pkg rollback
pkg gc
pkg repair
```

Downloads from the signed binary cache are preferred. A cache miss is shown before mutation; a
local sandboxed build runs only after explicit approval and only when platform policy permits it.
`--dry-run` previews an operation, while `--json` and `--jsonl` provide stable machine output.

## Shell setup in the next release

The current source adds `pkg shellenv` for Bash and zsh. It is not in alpha.46.
It prints shell settings and does not edit your files:

```sh
eval "$(pkg shellenv)"
```

Add this line to `~/.zshrc` or your Bash startup file for future sessions.
For the published alpha.46, use the PATH command in the [install guide](install.md).

Nix commands, paths, flags, substituters, and trust keys are not part of the public interface.
