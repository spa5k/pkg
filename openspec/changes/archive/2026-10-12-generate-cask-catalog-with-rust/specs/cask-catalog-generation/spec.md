## Purpose

An offline maintainer tool converts one pinned Homebrew Cask JSON
snapshot into the deterministic normalized catalog that Nix builds from
and the client reads status from. Generation is repeatable, provenanced,
and safe against vendor-controlled content.

## ADDED Requirements

### Requirement: Deterministic offline generation

The generator SHALL read only the pinned input snapshot and the pin
record. The same pin and generator version SHALL produce byte-identical
catalog output. The output SHALL contain no timestamps and no
order-dependent data. Continuous integration SHALL fail when
regenerating from the committed pin changes the committed catalog.

#### Scenario: Regeneration is byte-identical

- **WHEN** the generator runs twice on the same committed pin and input
- **THEN** both catalog outputs are byte-identical

#### Scenario: Tampered or stale generated data

- **WHEN** the committed catalog does not match what the committed pin
  regenerates
- **THEN** the check fails before publication instead of shipping
  drifted data

### Requirement: Exact input identification and provenance

Each generated catalog SHALL embed the input source URL, the exact
input revision, the input content hash, the generator name and version,
the target systems, and the declared macOS baseline. Snapshot field
coverage is not part of provenance; it is printed by the generator and
recorded in the dated verification note. The pin record and the
generated catalog SHALL be committed
together. The metadata input SHALL be pinned exactly once; no consumer
SHALL re-pin the raw snapshot.

#### Scenario: Provenance audit

- **WHEN** a reviewer inspects a published catalog
- **THEN** it names the exact input revision, content hash, and
  generator version it was produced from

### Requirement: Required platform scope fields, fail closed

The accepted input schema SHALL require the `supported_platforms` field
family. A snapshot where no record carries it SHALL fail generation
before any existing output is replaced. One record missing the field
SHALL be excluded as malformed in isolation. The aarch64-darwin target
SHALL require the exact `arm64_sequoia` tag; the x86_64-linux target
SHALL require the exact `x86_64_linux` tag. OS support
SHALL be decided only from explicit complete constraints
(`supported_platforms` membership and `depends_on` OS and arch
entries). Variation keys and artifact kinds SHALL NOT be used as
evidence of OS support. No default OS scope SHALL be guessed.

#### Scenario: Snapshot loses the support field

- **WHEN** an updated snapshot no longer carries `supported_platforms`
- **THEN** generation fails, the previous catalog stays in place, and
  the missing field is named

#### Scenario: Record without the support field

- **WHEN** one record lacks `supported_platforms`
- **THEN** that record alone is excluded as malformed and no OS scope
  is assumed for it

### Requirement: Per-target variation merge

The generator SHALL compute one effective record per token and target
by replacing base fields with the target variation's fields, including
wholesale replacement of the artifacts array. A missing target
variation SHALL NOT by itself decide support either way.

#### Scenario: sha-only variation serves a second system

- **WHEN** a record's base payload targets a system whose variation
  overrides only the checksum
- **THEN** that system classifies against the merged effective record

### Requirement: Bounded eligibility and exclusion reasons

Eligibility SHALL be computed per token and target from the effective
record: OS and arch intent, allowed artifact kinds for that OS, present
verified checksum, present URL, absent executable installer stanzas, no formula or cask dependencies
(excluded, not resolved), and declared OS minimums compatible with the
declared baseline. Unsupported tokens SHALL be published as excluded
entries with a machine-readable reason from a fixed vocabulary. Unknown
artifact kinds and malformed records SHALL exclude that token only,
with a recorded reason. Disabled or deprecated records SHALL be
rejected.

#### Scenario: Installer stanza excludes one token

- **WHEN** one token's effective record carries an `installer` script
- **THEN** that token alone is excluded with an installer reason and
  the rest of the catalog is unaffected

### Requirement: Whole-catalog integrity and atomic publication

The generator SHALL fail without replacing any published output when
the input contains duplicate tokens, a token is not
a usable catalog key, the input content hash does not match the pin,
the snapshot root is not a nonempty JSON array, or the snapshot lacks
a required field family. Output SHALL be staged
temporarily and moved into place only after every check passes.

#### Scenario: Duplicate token aborts generation

- **WHEN** the pinned snapshot contains two records with one token
- **THEN** generation fails with the duplicate named and the previous
  catalog file is untouched

### Requirement: Typed artifact plans with safe paths

Plans SHALL preserve every app, binary, rename target, completion, and
manpage the effective record declares. Plan path fields SHALL be
normalized against recognized anchors, and the generator SHALL reject
absolute paths, traversal, and unresolved expansions as
`malformed-record`. Vendor strings SHALL appear only as JSON data;
the generator SHALL NOT emit Nix or shell syntax.

#### Scenario: App with vendored CLI

- **WHEN** a record declares an app plus a renamed binary under an app
  anchor
- **THEN** the plan carries both artifacts and the rename, and no
  per-token special case exists in any builder

#### Scenario: Traversal in a link target

- **WHEN** a binary source resolves outside the extraction anchor
- **THEN** the token is excluded as malformed and the reason names the
  field

### Requirement: Dependencies exclude instead of resolving

Records with any Homebrew formula dependency SHALL be excluded with a
formula-dependency reason; the generator SHALL NOT guess nixpkgs
attributes for Brew tokens and SHALL NOT commit a per-formula map.
Records with any cask dependency SHALL be excluded with a
cask-dependency-integration reason. Plans SHALL carry no dependency
output and the client SHALL NOT expand any closure; the native Nix
lifecycle stays single-package.

#### Scenario: Formula dependency

- **WHEN** a token depends on a Homebrew formula
- **THEN** the token is excluded with a formula-dependency reason
  instead of guessing a nixpkgs attribute

#### Scenario: Cask dependency

- **WHEN** a token depends on another cask
- **THEN** the token is excluded with a cask-dependency-integration
  reason and no closure is computed

### Requirement: Known and unknown tokens stay distinct

The catalog SHALL distinguish tokens present in the input from tokens
absent from it. A token absent from the input SHALL carry no status and
SHALL never be presented as eligible.

#### Scenario: Typo token

- **WHEN** a user asks about a token the input does not contain
- **THEN** it is reported as unknown, not eligible and not excluded
