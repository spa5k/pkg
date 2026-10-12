## Purpose

Keep public-tap package identity distinct from its bare token, Nix
attribute, store package name, and source revision, in schema
`pkg-cask-catalog/3` only.

## ADDED Requirements

### Requirement: Qualified source identity

Catalog schema `pkg-cask-catalog/3` SHALL key entries by a qualified
`owner/tap/token` identity. Each entry SHALL contain its source, bare
token, origin path and hash, and per-target status. Each source SHALL have
exact provenance in `inputs`, including source-relative file provenance
when the source is read as Ruby. The official JSON source SHALL be retained
and regenerated in schema 3 with source `homebrew/cask`. No schema-2
compatibility decoder SHALL ship.

#### Scenario: Same token in different taps

- **WHEN** two sources contain the same bare token
- **THEN** both qualified identities exist without an implicit precedence rule

#### Scenario: Duplicate identity

- **WHEN** one input yields two records with the same qualified identity
- **THEN** the refresh fails as a whole-input integrity error and the previous catalog is unchanged

### Requirement: Unambiguous resolution

A qualified request SHALL resolve only within its named source. Bare cask
shorthand SHALL succeed only for one exact bare-token match across all
saved catalogs and official entries, counting excluded entries as matches.
Ambiguous requests SHALL list qualified choices. No request SHALL silently
switch sources, and an unknown qualified source SHALL fail or enter the
explicit first-source add flow rather than being reinterpreted as Nix code.

#### Scenario: One eligible and one excluded match

- **WHEN** two taps share a token and only one is eligible on the current target
- **THEN** the shorthand remains ambiguous rather than selecting the eligible tap automatically

#### Scenario: Unreadable saved catalog

- **WHEN** one saved source catalog is unreadable or malformed while another saved source has the same bare token
- **THEN** bare-token resolution fails and reports the failed source instead of ignoring it and selecting the other source

#### Scenario: Unknown qualified source

- **WHEN** an install names a source that is not saved
- **THEN** the client instructs or offers the explicit `tap add` flow and does not autoimport or reinterpret the request

### Requirement: Native Nix identity and lifecycle

Qualified identities SHALL be exposed in Nix as one validated and quoted
attribute segment holding a deterministic escaped full identity. The
escape SHALL map each original character in one pass: `_` to `_u_`, `.`
to `_d_`, and all other characters unchanged, with inserted underscores
never re-replaced. The mapping SHALL be reversible, and catalog and
public search map keys SHALL keep the original identity. Profile human
labels SHALL be decoded for display. Store names SHALL be safe and
distinct from that attribute identity. Revision changes SHALL NOT change
package identity. Installs SHALL use the native profile for
installation, upgrade, rollback, and removal, SHALL preserve old
generation outputs, and SHALL use stable generated flake references for
explicit updates.

#### Scenario: Reversible escaped attribute

- **WHEN** a full identity contains both a dot and an underscore, for example `example/tap/tool@1.2` and `example/tap/tool_1.2`
- **THEN** each identity maps to a distinct escaped physical attribute (`...tool@1_d_2` and `...tool_u_1_d_2`) that decodes back to the original identity

#### Scenario: Upgrade within one source

- **WHEN** the generated flake advances to new metadata for an installed qualified cask
- **THEN** upgrade selects that same escaped qualified identity and native rollback can return to the earlier profile generation

#### Scenario: Dotted identity survives the native manifest

- **WHEN** an identity contains a dot, for example `tool@1.2`
- **THEN** the escaped physical attribute is used, because a native manifest strips quotes from attributes and a raw dotted full identity would install but break native upgrade as a nested lookup

#### Scenario: Symlinked flake root rejected

- **WHEN** a saved source is published for native use
- **THEN** the client points the stable flake reference at a stable real directory and never at a symlinked flake root, which Nix rejects

#### Scenario: Source removal

- **WHEN** a catalog update removes an installed package's source
- **THEN** the update does not uninstall it or replace it with a same-token package from another source

### Requirement: Generated catalog distribution

The client SHALL consume generated flakes through its existing Cask source
configuration. An independent publisher SHALL be able to generate that
flake from a public tap without a central hosted API. Ordinary search SHALL
read the cheap index without forcing package derivations.

#### Scenario: Independent publisher

- **WHEN** a publisher generates and commits a catalog flake from a public tap
- **THEN** a client configured for that flake can discover and install eligible entries without obtaining the raw Ruby source
