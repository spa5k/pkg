## Purpose

Convert pinned public Ruby taps and JSON snapshots to captured metadata and
the existing generic Nix artifact plans during maintainer updates.

## ADDED Requirements

### Requirement: Pinned source registry

The maintainer tool SHALL read source identities from declarative data.
Each source SHALL have a stable identity, origin URL, exact input revision,
content hash, and license record. Cask discovery SHALL NOT use a package
allowlist. Duplicate source IDs and duplicate tokens within one source SHALL
fail the update.

#### Scenario: Add a public tap

- **WHEN** a maintainer registers and locks a supported public tap
- **THEN** all regular cask files under its configured directory are discovered without editing package-specific code

#### Scenario: Helper-only update

- **WHEN** a helper file changes while the cask file is unchanged
- **THEN** the source tree identity changes and cached export output is invalidated

### Requirement: Isolated maintainer-only Ruby evaluation

Raw Ruby SHALL execute only in an isolated export runner. The client,
generated package builds, and Nix evaluation SHALL NOT require Ruby or Brew.
The runner SHALL deny network access, contain resource use, and exclude
model, developer, and publication credentials. Native target context SHALL
be used when evaluating host-dependent Ruby. Homebrew simulation SHALL NOT
be presented as full operating-system emulation.

#### Scenario: Export attempts external access

- **WHEN** a cask attempts an unprovided network or host resource during export
- **THEN** the runner denies access and the export does not become an apparently successful empty record

#### Scenario: Install a generated cask

- **WHEN** the client installs an eligible generated entry
- **THEN** it uses the ordinary Nix package lifecycle and does not load the tap's Ruby

### Requirement: Preserve conversion limits

The exporter SHALL preserve target evidence, constraints, artifact options,
and required active install operations. Access to upstream staging during
DSL evaluation SHALL force an explicit exclusion, even if the source rescues
the resulting error. This conservative guard SHALL NOT be presented as a
complete check of arbitrary Ruby purity. Unknown required operations SHALL
NOT be silently discarded to create eligibility.

#### Scenario: Payload-derived file list

- **WHEN** metadata uses the upstream staging accessor to enumerate the downloaded application
- **THEN** the exporter reports `staged-path-during-metadata` instead of publishing an incomplete artifact list as eligible

#### Scenario: Required postflight operation

- **WHEN** the upstream definition contains a required postflight action with no equivalent generic builder behavior
- **THEN** the generated target is excluded and its reason identifies the unsupported operation

### Requirement: Captured-byte reproducibility

Export snapshots SHALL identify the source tree, exporter runtime, adapter,
target context, and exported content hash. Offline Rust generation from the
same captures and compiler version SHALL produce identical catalog bytes.
The system SHALL NOT claim that a raw Git revision alone makes arbitrary
Ruby evaluation deterministic.

#### Scenario: Regenerate without Ruby

- **WHEN** committed export snapshots are verified against their lock and regenerated offline
- **THEN** the catalog matches the committed bytes without evaluating a tap

### Requirement: Complete update and atomic runtime publication

Every discovered record/target SHALL have an output or recorded exclusion.
Missing inventory, unavailable targets, corrupt output, or source-wide
failure SHALL abort the update. The previous runtime catalog SHALL remain
unchanged on failure. Export runners SHALL NOT have publication authority.

#### Scenario: One source crashes

- **WHEN** one registered source fails before producing its required inventory
- **THEN** the tool refuses to publish a partial catalog and retains the prior runtime catalog bytes
