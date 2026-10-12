## Purpose

Extend package coverage with compatible Cask metadata while keeping Nix
as the only package execution path and requiring no Homebrew client.

## ADDED Requirements

### Requirement: Cask packages use the same Nix lifecycle

Supported Cask entries SHALL be exposed as ordinary Nix package outputs.
Install, upgrade, removal, and rollback SHALL use the same native user profile.
pkg and its package execution path SHALL NOT install or invoke Homebrew.

#### Scenario: No Brew on the machine

- **WHEN** the user installs a supported Cask-derived package
- **THEN** Nix realizes and installs it without a Brew executable or prefix

### Requirement: Published input revisions

Each published Cask source revision SHALL lock converter and metadata inputs.
Its download inputs SHALL use verified fixed content.
The installed original product source reference SHALL remain moving unless
the user explicitly selected a fixed revision.

#### Scenario: Cask source update

- **WHEN** a reviewed product source revision updates its locked metadata
- **THEN** a normal native upgrade can select its updated supported package outputs

### Requirement: Explicit compatibility limits

The Cask catalog SHALL distinguish supported entries from excluded entries.
Entries that need native installer actions, privileged helpers, drivers,
services, unavailable platform variants, or missing content hashes SHALL be
excluded from initial installation. Exclusions SHALL show a reason.

#### Scenario: Native installer entry

- **WHEN** an entry requires Apple installer actions or a privileged helper
- **THEN** pkg reports it as unsupported without trying Brew or the native installer

#### Scenario: Archive extraction succeeds but launch fails

- **WHEN** a candidate can be extracted but cannot run from the supported Nix layout
- **THEN** it is not advertised as supported

### Requirement: Bounded metadata evaluation

The Cask source SHALL be usable with import-from-derivation disabled.
The client SHALL consume normalized metadata without executing arbitrary tap Ruby.

#### Scenario: Metadata evaluation

- **WHEN** the supported Cask source is evaluated
- **THEN** no evaluation-time derivation build or Ruby tap execution is required
