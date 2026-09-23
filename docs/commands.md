---
title: Everyday pkg commands
---

# Everyday commands

```console
pkg search ripgrep
pkg info ripgrep
pkg install ripgrep
pkg list
pkg remove ripgrep
pkg update
pkg outdated
pkg upgrade ripgrep
pkg upgrade --all
pkg history
pkg rollback
pkg gc
pkg repair
```

Downloads from the signed binary cache are preferred. A cache miss is shown before mutation; a
local sandboxed build runs only after explicit approval and only when platform policy permits it.
`--dry-run` previews an operation, while `--json` and `--jsonl` provide stable machine output.

## Remove and upgrade packages

`pkg remove fzf` removes a package from your active environment.
`pkg uninstall` removes **pkg and its managed Nix installation**.
Use `remove` for individual packages.

`pkg update` refreshes the signed catalog. It does not change installed packages.
`pkg outdated` shows available package updates.
`pkg upgrade fzf` upgrades one installed package.
`pkg upgrade --all` upgrades all unpinned packages.
The available versions come from the Nixpkgs revision in the signed channel.
An upgrade can report no change when that revision has not changed.

`pkg pin fzf` keeps the installed version. `pkg unpin fzf` permits updates again.
`pkg history` lists saved generations. `pkg rollback` restores the previous one.
Removed files can remain in the store while a saved generation needs them.
Use `pkg gc --dry-run` to preview cleanup. Use `pkg gc` to perform it.

## Package coverage

The published alpha.47 channel contains the original 11-package test catalog.
Installation already accepts exact Nixpkgs attribute IDs outside that catalog.
The small catalog mainly limits search, package details, and update reports.
The next catalog uses metadata generated from Nixpkgs, including nested package
sets such as `python311Packages.requests`. This change requires a new signed
channel before installed clients can use it.

Use `pkg search QUERY` to find packages. Use the exact package ID from the
results with `pkg info ID` and `pkg install ID`.
Coverage depends on the channel's Nixpkgs revision, your platform, evaluation
success, and upstream package policy. Some packages have no executable command.
A missing binary cache entry can require a local build.
The next runtime also accepts valid name-only derivations such as `unixtools.watch`.
An unknown version appears as `null` in JSON output. Internal Nix build metadata
stays in the package store and does not cause conflicts in the active environment.

## Shell setup

Alpha.47 provides `pkg shellenv` for Bash and zsh.
It prints shell settings and does not edit your files:

```sh
eval "$(pkg shellenv)"
```

Add this line to `~/.zshrc` or your Bash startup file for future sessions.
See the [install guide](install.md) for download and upgrade commands.

Nix commands, paths, flags, substituters, and trust keys are not part of the public interface.
