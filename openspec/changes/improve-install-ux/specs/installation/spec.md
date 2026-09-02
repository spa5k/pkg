## ADDED Requirements

### Requirement: Single verified entry script

An `install.sh` script SHALL be the documented install path. It SHALL detect OS and
architecture, refuse unsupported combinations with a clear message, fetch release inputs only
through the TUF-verified channel, verify every downloaded artifact checksum before use, and
never execute an artifact whose verification failed.

#### Scenario: Unsupported platform refusal

- **WHEN** the script runs on an unsupported OS or architecture
- **THEN** it prints the supported matrix and exits non-zero without downloading anything

#### Scenario: Checksum gate before execution

- **WHEN** a downloaded artifact fails checksum verification
- **THEN** the script deletes the artifact, prints the failing digest, and exits non-zero
- **AND** no installer binary is executed

### Requirement: Preflight before mutation

The script SHALL run preflight checks before any system mutation: existing Nix installation,
existing product installation, free disk space, macOS version or Linux init system, and
channel reachability. Each failed check SHALL print a plain message with the next action.

#### Scenario: Existing product install

- **WHEN** the product is already installed
- **THEN** the script reports the installed version, points to update and doctor, and exits
  without mutation

#### Scenario: Existing foreign Nix

- **WHEN** a non-Determinate or unknown Nix is present
- **THEN** the script refuses with the documented guidance and does not proceed to the
  Determinate handoff

### Requirement: Installer phase and failure messaging

The platform installers SHALL report phase progress to the entry script and, on failure, SHALL
produce one message that names the failing phase, what state remains on disk, and the next
action (retry, doctor, or uninstall).

#### Scenario: Handoff failure message

- **WHEN** the Determinate handoff fails mid-install
- **THEN** the final message states the handoff phase, that the journal preserves the started
  state, and the exact retry command

### Requirement: Post-install verification

After a successful install, the script SHALL run the product `doctor` command and print its
table. The script's exit code SHALL reflect the doctor verdict.

#### Scenario: Immediately visible breakage

- **WHEN** the install completes but the channel probe fails
- **THEN** the doctor table shows the failing row and the script exits with the channel error
  code

### Requirement: Uninstall parity

An `uninstall.sh` script SHALL exist with the same preflight discipline (detect install state,
confirm destructive action, refuse non-interactive without consent) and SHALL verify it is
removing files the product owns via the existing ownership receipts.

#### Scenario: Owned-files-only removal

- **WHEN** uninstall removes files
- **THEN** removal is limited to paths with valid ownership receipts, per the existing
  terminal-uninstall contract
