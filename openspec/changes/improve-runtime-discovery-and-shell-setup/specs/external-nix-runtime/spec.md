## Purpose

Discovery of the external Nix runtime: where pkg looks, what counts as a
candidate, and how failures report. Adds to the
[external runtime](../../../../changes/simplify-to-native-nix/specs/external-nix-runtime/spec.md)
capability held by the active change.

## ADDED Requirements

### Requirement: Runtime discovery order

Default runtime discovery SHALL search the executable entries of `PATH`
in order, then the stable Nix installation profile paths
`/nix/var/nix/profiles/default/bin/nix`,
`$XDG_STATE_HOME/nix/profile/bin/nix` (default
`~/.local/state/nix/profile/bin/nix`), and `~/.nix-profile/bin/nix`,
in that order.
Only regular executable files SHALL count as candidates. pkg SHALL NOT
scan other disk locations or install, configure, or repair Nix during
discovery.

#### Scenario: Nix is outside PATH at the stable profile path

- **WHEN** `PATH` holds no `nix` and `/nix/var/nix/profiles/default/bin/nix` is an executable regular file
- **THEN** pkg uses that Nix and reports it as the runtime

#### Scenario: Candidate is not an executable file

- **WHEN** a `PATH` entry or stable path holds a non-executable or non-regular `nix`
- **THEN** discovery skips it and continues with the remaining candidates

### Requirement: Configured runtime priority

An explicitly configured `runtime.nix` value SHALL be the only candidate
used. When it is unusable, pkg SHALL fail with an error that names that
configured value and the configuration file, and SHALL NOT silently
select another runtime.

#### Scenario: Configured path is unusable

- **WHEN** the configured `runtime.nix` path does not exist or is not an executable file
- **THEN** pkg fails naming the configured value and does not run default discovery

### Requirement: Missing-runtime failures distinguish the cause

A missing runtime error SHALL distinguish the configured-value failure
from the no-installation-found failure. The no-installation-found error
SHALL name the locations searched and state a useful next action for a
machine where Nix exists but is not reachable through the searched
locations, instead of always advising a reinstall.

#### Scenario: No installation found

- **WHEN** no candidate in `PATH` or the stable profile paths is a usable executable
- **THEN** the error names `PATH` and the stable profile paths and offers adding the Nix `bin` directory to `PATH`, setting `runtime.nix`, or installing Determinate Nix from the vendor
