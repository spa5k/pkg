## Purpose

Client downloader behavior for shell path setup and Nix detection. Adds
to the [standalone client
delivery](../../../../changes/simplify-to-native-nix/specs/standalone-client-delivery/spec.md)
capability held by the active change.

## ADDED Requirements

### Requirement: Installer shell path setup

The downloader SHALL set up the `pkg` client `PATH` and the `pkg`
package profile `PATH` for subsequent Bash and Zsh shells automatically,
through one small managed block per startup file with explicit markers.
For Bash, the block SHALL cover both the interactive startup file and
the login startup file. It SHALL keep unrelated startup content, file
modes, and user settings unchanged, and reruns SHALL NOT duplicate the
block. Updates SHALL write new content through an unpredictable
sibling temporary file in the same directory and an atomic rename.
`PKG_INSTALL_SHELL_SETUP=0`
SHALL skip all startup file changes. The downloader SHALL NOT write
Bash syntax into a non-Bash shell configuration and SHALL NOT change the
shell that ran it; it SHALL print one exact `export PATH` command for
the current Bash or Zsh shell and the plain directory list for other
shells, and SHALL tell the user to open a new terminal for automatic
setup only when the setup completed.

#### Scenario: First install on Bash

- **WHEN** the downloader finishes on a host whose login shell is Bash
- **THEN** a new interactive Bash shell resolves `pkg` and the `pkg` profile `bin` directory on `PATH` without manual edits

#### Scenario: Rerun

- **WHEN** the downloader runs again after a successful setup
- **THEN** the managed block appears once and unrelated startup lines are unchanged

#### Scenario: Opt-out

- **WHEN** `PKG_INSTALL_SHELL_SETUP=0` is set
- **THEN** no startup file is changed and the manual lines are printed instead

#### Scenario: Unsupported shell

- **WHEN** the login shell is Fish or another unsupported shell
- **THEN** the downloader writes no shell configuration file and prints the plain directory list to add in that shell's own syntax

#### Scenario: Verification failure leaves startup files untouched

- **WHEN** archive verification fails
- **THEN** no startup file is changed and no client is installed

### Requirement: Installer Nix detection

The downloader SHALL detect a usable Nix without requiring a correct
inherited `PATH`, SHALL report the detected executable, and SHALL add
the detected Nix `bin` directory to the managed block whenever Nix was
found, also when it was found through `PATH`. When no Nix is found, the
downloader SHALL leave a working installed client, print the vendor
install link, and NOT report the setup as healthy; the final report
SHALL claim new terminals get `nix` only when Nix was detected and the
setup completed. It SHALL NOT install, remove, update, or
reconfigure Nix, use `sudo`, change trusted users, or change daemon or
sandbox configuration.

#### Scenario: Nix present outside PATH

- **WHEN** Nix is usable at a stable profile path outside `PATH`
- **THEN** the downloader reports the detected path and a new shell resolves both `pkg` and `nix`

#### Scenario: Nix absent

- **WHEN** no usable Nix exists on the host
- **THEN** the client stays installed and usable by full path, the vendor link is printed, and the output does not claim a healthy setup
