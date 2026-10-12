## Purpose

Help users find package outputs and select their sources without a custom
repository parser, authoritative package index, or second package solver.

## ADDED Requirements

### Requirement: Visible source identity

Discovery SHALL identify each result by source and output.
pkg SHALL accept a bare name only when it selects one supported exact match.
Ambiguous names SHALL require an explicit source-qualified choice.

#### Scenario: Duplicate name

- **WHEN** the same bare name matches supported outputs from two sources
- **THEN** pkg shows both qualified choices and does not silently install either

#### Scenario: Explicit public flake

- **WHEN** the user provides a supported public GitHub flake installable with an attribute
- **THEN** pkg passes the chosen source and attribute through the same native Nix path

### Requirement: Evaluated and disposable discovery data

pkg SHALL obtain package values from Nix evaluation or normalized source data.
It SHALL NOT infer installable package values by parsing Nix or Ruby source.
Cached results SHALL record source revision, system, and cache schema.

#### Scenario: Empty cache

- **WHEN** search runs without cached results
- **THEN** pkg obtains source metadata without requiring a central pkg index service

### Requirement: Refresh does not install packages

Update SHALL refresh or invalidate discovery metadata without changing the profile.
Stale reused data SHALL be identified. A source failure SHALL NOT appear as
an authoritative successful search with zero results.

#### Scenario: Source refresh fails

- **WHEN** the source is unavailable
- **THEN** pkg reports the failure and labels any reused results as stale

#### Scenario: Update succeeds

- **WHEN** discovery metadata refresh completes
- **THEN** installed package versions and original source references are unchanged

### Requirement: Discovery and installed resolution are distinct

Info SHALL distinguish discovered metadata from installed state.
Installation SHALL preserve original moving references for later upgrades
and report the actual resolution after completion.

#### Scenario: Source moves after search

- **WHEN** a moving source changes before installation resolves it
- **THEN** pkg reports the installed resolution and does not claim the old search result fixed it
