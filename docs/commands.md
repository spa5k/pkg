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

Nix commands, paths, flags, substituters, and trust keys are not part of the public interface.
