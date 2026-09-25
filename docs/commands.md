---
title: Everyday pkg commands
---

# Everyday commands

These commands are available in alpha.53.

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

Run `pkg` or `pkg -h` for a short command guide. Use `pkg install --help` for install
examples. `-y` accepts confirmation prompts. `-v` shows more detail. `-q`
hides progress. Use the long options in scripts when that improves clarity.

`pkg list` shows package names and versions in name order. It shows pin status
when a package is pinned. A different source selector appears below the table.
`pkg list --name-only` prints one package name per line for scripts.
`pkg list --size --with-outputs` also shows selected outputs and the recorded size of
the primary output closure. This includes shared dependencies. It is not the
space that removal will free. Other selected output closures can use more space.
`pkg info` shows the package description, homepage, license, and availability.
`pkg history` shows saved generation IDs, package changes, versions, and the
active generation. `pkg history <ID>` shows all packages in that saved environment.
If its parent was deleted, history shows the saved inventory and states that the
previous generation is not retained. It does not invent a change list.

There is no separate build command. `pkg install` first checks trusted cached
downloads. If a local build is needed, it shows the plan and asks for approval.
Dependency and cache checks can take a minute on a fresh system.
The minimum free disk value is a starting requirement. It is not a peak usage
estimate. A build can require more disk space. Build percentages appear only
after the build service reports progress.

## Public flake packages (macOS)

Alpha.48 supports public flake packages on macOS. Linux refuses public flake
references until source evaluation has the same per-process local-file boundary.

```sh
pkg install 'github:Mic92/nix-update#default'
pkg list
pkg upgrade 'github:Mic92/nix-update#default'
pkg pin 'github:Mic92/nix-update#default'
pkg unpin 'github:Mic92/nix-update#default'
pkg remove 'github:Mic92/nix-update#default'
pkg rollback
```

Quote the reference so the shell keeps it as one argument. A reference can
select a branch, tag, or commit: `github:owner/repository/ref#package`.
Without a package fragment, pkg selects the flake's default package.
The package must support your system.

Pkg saves the exact source commit, content hash, and dependency locks. An
upgrade checks the original source again. A commit reference stays at that
commit. Pinning keeps the installed output. Removal and rollback use the same
commands as Nixpkgs packages.

The source must be public and have complete committed dependency locks. Local
path inputs and private credentials are not supported. Pkg uses its existing
trusted caches and asks for build approval when needed. The source cannot add
caches, keys, or Nix settings. Builds that require evaluation-time builds are
not supported.

Search, info, and the catalog update use Nixpkgs. `pkg outdated` reports public
flake sources as unchecked. Use `pkg upgrade` to check and install their updates.

## Remove and upgrade packages

`pkg remove fzf` removes a package from your active environment.
`pkg uninstall fzf` is the same command as `pkg remove fzf`.
`sudo pkg system uninstall` removes **pkg and its managed Nix installation**.
Use `sudo pkg system uninstall --dry-run` to inspect that operation first.

Remove, uninstall, pin, unpin, and upgrade accept a unique installed package name.
For example, `pkg remove just` finds `github:casey/just#default` when its installed
name is `just`. Exact selectors take priority. If a name matches several
packages, the command gives valid installed IDs. Use `pkg list` to choose a full selector. No approval is requested for
an invalid removal. Removal approval shows the resolved names, versions, and sources.

`pkg update` refreshes the signed catalog. It does not change installed packages.
`pkg outdated` shows available package updates.
`pkg upgrade fzf` upgrades one installed package.
`pkg upgrade --all` upgrades all unpinned packages.
The available versions come from the Nixpkgs revision in the signed channel.
An upgrade can report no change when that revision has not changed.

`pkg pin fzf` keeps the installed version. `pkg unpin fzf` permits updates again.
`pkg history` lists saved generations. `pkg rollback` shows changes and asks
for approval before it restores the previous environment. Use `pkg rollback <ID>`
for another saved environment. Use `--dry-run` for a preview or `--yes` in scripts.
Removed files can remain in the store while a saved generation needs them.
Use `pkg gc --dry-run` to preview cleanup. Use `pkg gc` to perform it.

## Package coverage

The signed alpha.53 channel contains 64,412 package records for Apple silicon
macOS and 69,517 records for x86-64 Linux. It uses metadata generated from
Nixpkgs, including nested package sets such as `python311Packages.requests`.

Use `pkg search QUERY` to find packages. Use the exact package ID from the
results with `pkg info ID` and `pkg install ID`.
Coverage depends on the channel's Nixpkgs revision, your platform, evaluation
success, and upstream package policy. Some packages have no executable command.
A missing binary cache entry can require a local build.
Alpha.48 also accepts valid name-only derivations such as `unixtools.watch`.
An unknown version appears as `null` in JSON output. Internal Nix build metadata
stays in the package store and does not cause conflicts in the active environment.

## Shell setup

Alpha.48 provides `pkg shellenv` for Bash and zsh.
It prints shell settings and does not edit your files:

```sh
eval "$(pkg shellenv)"
```

Add this line to `~/.zshrc` or your Bash startup file for future sessions.
See the [install guide](install.md) for download and upgrade commands.

Nix commands, paths, flags, substituters, and trust keys are not part of the public interface.

## Terminal output and scripts

Terminal output uses compact tables, status marks, and one live build line.
The line shows reported progress and known download byte counts. It has no elapsed timer. A percentage does not
mean the package is active. Setup still saves and activates the new environment
after the build finishes. Long pauses show that no new update has arrived.
The progress line clears before an approval prompt or error.

Use `--no-color` or `NO_COLOR=1` for static output. `CI` and `TERM=dumb` also
select a plain transcript. `--quiet` removes progress and keeps the final result.
`--json` and `--jsonl` keep their existing machine formats. `pkg list --name-only`
prints names only, including on a terminal.

The same layout applies across the command tree. Wide terminals show tables;
small terminals show labeled cards. Long values wrap within the terminal width.
Previews show the planned targets and state that no changes were applied.
Install and rollback results identify the saved environment. History includes
pin and output changes even when package versions match.

The public installer selects alpha.53.
