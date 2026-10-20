# Simplify setup

## Why

A first install took 268 seconds and printed 208 lines on a Mac.
The user had to read a pinned versioned URL, choose from five installer
options, and run `pkg setup` after the install. The installer did not
help when Nix was missing. Fish users got no `PATH` setup. Zsh users got
no working completion. The client never updated itself. `pkg doctor`
reported a healthy system while the background job was off or the
client was not on `PATH`.

## What Changes

- One stable install URL: `releases/latest/download/install.sh`. The
  installer takes no arguments and no options.
- **BREAKING** Remove `PKG_INSTALL_BIN`, `PKG_INSTALL_COMPLETIONS`,
  `PKG_INSTALL_SHELL_SETUP`, `PKG_INSTALL_PREPARE`, and
  `PKG_INSTALL_BACKGROUND`. The client always goes to `~/.local/bin`.
- When no Nix exists and a terminal is available, the installer asks one
  question and runs the Determinate Nix installer on approval.
- The installer runs the new client before it replaces the old client.
- The installer prints about five lines. It does not prepare search data.
  It enables the daily job, which starts at once in the background.
- Shell setup covers Bash, Zsh, and Fish. Zsh and Bash completion work
  in new shells without user steps.
- **BREAKING** Remove `pkg setup`. The first search prepares missing
  search data itself.
- `pkg update` and the daily job also update the installed client from
  the newest published release.
- **BREAKING** Hide `background`, `shellenv`, and `completion` from help.
- `pkg doctor` reports the background job and `PATH` state.
- Add `pkg self uninstall`.
- Long operations show one transient status line on a terminal.
- A package install tells the user to open a new terminal window when
  the profile is not on `PATH` yet.

## Capabilities

### New Capabilities

- `client-setup`: install, update, and removal of the pkg client, and
  its shell setup.

### Modified Capabilities

- None.

## Impact

- New `crates/pkg-cli/src/client.rs` and
  `crates/pkg-cli/src/commands/uninstall.rs`. New dependency `semver`.
- `install.sh` is rewritten. Its tests in `docs/tests/` are rewritten.
- The release workflow publishes `install.sh` as a release asset. A
  release must be marked as the latest release so that the stable URL
  serves it.
- `README.md`, `docs/install.md`, `docs/commands.md`, and
  `docs/privacy.md` change.

## Non-goals

- No Nix repair, update, or removal by pkg.
- No `sudo` call by pkg itself. The Nix installer asks for its own
  permissions.
- No installer options for other directories.
