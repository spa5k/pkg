# Spec delta: package-discovery (revision snapshots)

## MODIFIED Requirements

### Requirement: Evaluated and disposable discovery data

pkg SHALL obtain package values from Nix evaluation or normalized source data.
It SHALL NOT infer installable package values by parsing Nix or Ruby source.
pkg SHALL cache complete discovery metadata per source, system, and kind as
disposable derived data — one latest snapshot per identity, atomically
replaced when a new locked revision is fetched — and SHALL filter that
cached metadata locally per query. pkg SHALL NOT treat a per-query cache
file as complete metadata. Sources without an immutable revision SHALL be
queried live and SHALL NOT populate the revision snapshot cache.

The nixpkgs lane SHALL match the query with the Rust regex engine,
case-insensitively, against the same three strings native `nix search`
matches: the full attribute path, the package name (already
version-stripped), and the description — including an empty description.
An invalid pattern SHALL be reported as a source failure before package
evaluation runs.

#### Scenario: New query at a cached revision

- **WHEN** a query differs from earlier queries but the source's locked
  revision is unchanged and a snapshot exists
- **THEN** pkg filters the cached snapshot locally and reports the source
  as fresh without running a new native evaluation

#### Scenario: Revision moves

- **WHEN** the source's locked revision differs from the snapshot's
  recorded revision
- **THEN** pkg refetches complete metadata from the locked reference and
  atomically replaces the latest snapshot before filtering, keeping no
  per-revision history

#### Scenario: Invalid snapshot file

- **WHEN** a snapshot file is unreadable or fails identity re-validation
- **THEN** pkg ignores it and refetches rather than failing the search

#### Scenario: Empty cache

- **WHEN** search runs without cached results
- **THEN** pkg obtains source metadata without requiring a central pkg
  index service

#### Scenario: First query cost

- **WHEN** search runs at a revision with no snapshot
- **THEN** pkg performs one full native evaluation and does not claim
  cold search is instant

### Requirement: Refresh does not install packages

Update SHALL refresh or invalidate discovery metadata without changing the
profile. Stale reused data SHALL be identified. A source failure SHALL NOT
appear as an authoritative successful search with zero results. Update
SHALL invalidate revision snapshots together with all other discovery
cache data.

#### Scenario: Source refresh fails

- **WHEN** the source is unavailable
- **THEN** pkg reports the failure and labels any snapshot-reused results
  as stale, carrying the snapshot's own locked reference and revision

#### Scenario: Stale index on an untargeted system

- **WHEN** a live source check fails and the latest stale cached index
  does not target the current system
- **THEN** pkg keeps the failure detail and the stale status; a stale
  untargeted index SHALL NOT produce a clean platform skip. Only a
  freshly resolved index may report a platform skip

#### Scenario: Update succeeds

- **WHEN** discovery metadata refresh completes
- **THEN** installed package versions and original source references are
  unchanged and snapshot files are dropped
