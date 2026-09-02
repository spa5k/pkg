## ADDED Requirements

### Requirement: User-facing text registry

All user-visible CLI strings SHALL live in a single registry module with stable identifiers.
Inline format strings in command implementations SHALL NOT reach the terminal.

#### Scenario: Centralized strings

- **WHEN** a command prints help, an error, progress, or a confirmation
- **THEN** the text originates from the registry, and the registry is the only module whose
  literals appear in `--help` or runtime output

#### Scenario: Rename readiness

- **WHEN** the product binary name changes
- **THEN** all user-visible strings update from one registry constant without touching command
  implementations

### Requirement: Consistent help with examples

Every command SHALL provide usage, at least one example, and a docs link in `--help`, and the
full `--help` output SHALL be pinned by golden tests.

#### Scenario: Golden help

- **WHEN** `--help` output for any command changes
- **THEN** the golden test fails and requires a deliberate update

### Requirement: Actionable errors with stable codes

Every failure printed for humans SHALL state what happened and the next action. Machine mode
(`--json`) SHALL include a stable error code per failure class, and codes SHALL remain stable
across releases once published.

#### Scenario: Channel failure tells the next action

- **WHEN** channel metadata verification fails
- **THEN** the human output names the failing role (root, targets, snapshot, timestamp), and
  the next action (refresh, check pinned root, or contact channel operator)
- **AND** `--json` output carries the documented error code for that class

### Requirement: Progress reporting that respects context

Long operations SHALL print named phase lines with elapsed time. Output SHALL be quiet when
non-interactive (no TTY or `CI` set) or when `--quiet` is passed, except errors.

#### Scenario: Interactive progress

- **WHEN** an install runs on an interactive terminal
- **THEN** phase transitions print one line each with elapsed seconds since operation start

#### Scenario: Non-interactive quiet

- **WHEN** the same install runs with no TTY
- **THEN** no progress lines print and the exit code alone reports the outcome

### Requirement: Destructive operation confirmation

Uninstall and repair SHALL require explicit confirmation interactively, SHALL support `--yes`
for scripts, and SHALL refuse without either in non-interactive mode.

#### Scenario: Non-interactive refusal

- **WHEN** uninstall runs without a TTY and without `--yes`
- **THEN** the command refuses with the documented usage error and exits 1

### Requirement: Documented exit codes

Exit codes SHALL be: 0 success; 1 usage; 2 state conflict; 3 network or channel; 4 verification
failure; 5 internal. The mapping SHALL be documented in help output and docs, and SHALL be
golden-tested.

#### Scenario: State conflict distinguishes from failure

- **WHEN** install runs and the product is already installed
- **THEN** the command exits 2 with the no-op explanation, not 5

### Requirement: Doctor command

A `doctor` command SHALL check prerequisites, channel reachability, TUF root health, and local
install-state consistency, and SHALL print a pass/fail table using the same exit-code mapping.

#### Scenario: Healthy install

- **WHEN** `doctor` runs on a healthy installation
- **THEN** every row passes and the exit code is 0

#### Scenario: Broken root pin

- **WHEN** the pinned TUF root does not match the channel root
- **THEN** the corresponding row fails with the next action and the exit code is 4
