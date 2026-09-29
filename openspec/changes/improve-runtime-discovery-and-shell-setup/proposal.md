# Improve runtime discovery and installer shell setup

Proposed 29 September 2026 for `v0.2.0-alpha.4`. Baseline: `main` after
the `v0.2.0-alpha.3` release.

## Why

`pkg` alpha.3 discovers Nix through `PATH` only. On a normal Determinate
Nix installation whose `nix` lives at
`/nix/var/nix/profiles/default/bin/nix`, a shell without that directory
on `PATH` makes `pkg doctor` report Nix as missing and tell the user to
install Nix again. The user in this state has a working daemon and a
working `nix`.

The client downloader installs only the client and completions. It tells
the user to edit shell startup files by hand. When that manual step is
not completed, the client, the Nix runtime, and installed packages stay
unreachable in new shells.

## What Changes

- Default runtime discovery searches the executable entries of `PATH`,
  then the stable Nix installation profile paths
  (`/nix/var/nix/profiles/default/bin/nix`,
  `$XDG_STATE_HOME/nix/profile/bin/nix` or
  `~/.local/state/nix/profile/bin/nix`, and `~/.nix-profile/bin/nix`).
- An explicitly configured `runtime.nix` keeps priority. When it is not
  usable, pkg fails with a clear error about that configured value. pkg
  never silently selects another runtime.
- Candidates that are not executable files are ignored during discovery.
- Discovery failures distinguish a failed configured value from no
  installation found, name the locations checked, and state one useful
  next action.
- The downloader sets up the `pkg` client `PATH` and the `pkg` profile
  `PATH` for Bash and Zsh automatically, through one small managed block
  per startup file. `PKG_INSTALL_SHELL_SETUP=0` opts out.
- The downloader detects a usable Nix without requiring a correct
  inherited `PATH`, reports what it found, and always adds the detected
  Nix `bin` directory to the managed `PATH` block, also when Nix was
  found through `PATH`, because that entry can be a one-time export.
- When Nix is absent, the downloader still leaves a working installed
  client, prints the vendor install link, and does not claim a healthy
  setup.
- The downloader keeps its existing release pinning and safe archive
  extraction unchanged.

This change is authorized new behavior. Earlier documents said no pkg
component edits shell startup files. That promise is withdrawn for the
downloader's managed block; `pkg shellenv` itself still only prints.

## Capabilities

### Modified Capabilities

- `external-nix-runtime`: add the runtime discovery requirement below.
- `standalone-client-delivery`: add the installer shell path setup
  requirement below.

Both capabilities are held as deltas by the active change
[simplify to native Nix](../simplify-to-native-nix/proposal.md); this
change adds requirements to them.

## Impact

- `crates/pkg-cli/src/nix/mod.rs` gains the discovery order and the split
  failure modes. No new dependency.
- `install.sh` gains managed shell setup and Nix detection. It stays
  POSIX shell and needs no tool beyond those it already requires.
- `docs/install.md`, `README.md`, and `CONTRIBUTING.md` update their
  install and workflow text.
- Version moves to `0.2.0-alpha.4` in the workspace, the downloader, the
  installer test fixture, and the release links.

## Non-goals

- No pkg installation, update, repair, or removal of Nix.
- No `sudo`, trusted-user changes, daemon settings, or service changes.
- No disk-wide search for Nix.
- No generic shell framework or shell plugin manager.
- No new subcommand.
