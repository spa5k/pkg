## Purpose

Keep public-tap package identity distinct from its bare token, Nix attribute,
store package name, and source revision.

## ADDED Requirements

### Requirement: Qualified source identity

Catalog schema `pkg-cask-catalog/3` SHALL key entries by a qualified ID.
Each entry SHALL contain its source ID, bare token, origin path/hash, and
per-target status. Each source SHALL have exact provenance in `inputs`.
The generator, Nix index, and client decoder SHALL use this schema together.
No schema-2 compatibility decoder is required.

#### Scenario: Same token in different taps

- **WHEN** two sources contain the same bare token
- **THEN** both qualified IDs exist without an implicit precedence rule

### Requirement: Unambiguous resolution

A qualified cask request SHALL resolve only within its named source.
Unqualified cask shorthand SHALL succeed only for one exact bare-token
match, counting excluded entries as matches. Ambiguous requests SHALL list
qualified choices. No request SHALL silently switch sources.

#### Scenario: One eligible and one excluded match

- **WHEN** two taps share a token and only one is eligible on the current target
- **THEN** the shorthand remains ambiguous rather than selecting the eligible tap automatically

### Requirement: Native Nix identity and lifecycle

Qualified IDs SHALL be encoded as one validated and quoted Nix attribute
segment. Store names SHALL be safe and distinct from that attribute identity.
Revision changes SHALL NOT change package identity. Installs SHALL retain
the moving generated-flake reference and use the existing native profile.

#### Scenario: Upgrade within one source

- **WHEN** the generated flake advances to new metadata for an installed qualified cask
- **THEN** upgrade selects that same qualified ID and native rollback can return to the earlier profile generation

#### Scenario: Source removal

- **WHEN** a catalog update removes an installed package's source
- **THEN** the update does not uninstall it or replace it with a same-token package from another source

### Requirement: Generated catalog distribution

The client SHALL consume a generated flake through its existing Cask source
configuration. An independent publisher SHALL be able to generate that
flake from its own source registry without a central hosted API. Ordinary
search SHALL read the cheap index without forcing package derivations.

#### Scenario: Independent publisher

- **WHEN** a publisher generates and commits a catalog flake from a public tap
- **THEN** a client configured for that flake can discover and install eligible entries without obtaining the raw Ruby source
