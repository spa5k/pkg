## Purpose

The client reads cask catalog status from one generated index envelope,
keeps discovery cheap and per-system, and installs through the ordinary
native profile path. The client contains no second classifier and no
platform constant.

## ADDED Requirements

### Requirement: One generated index envelope

The Cask source SHALL expose exactly one index attribute
(`catalogIndex`), derived by the flake from the single committed
`catalog.json`, whose value is a single envelope: schema identifier,
generator and input provenance, the target system list, the declared
macOS baseline, and per-system entries keyed by token. Each entry SHALL
carry `token`, `name`, `description`, `version`, `homepage`,
`status`, `kind`, `reason`, and `detail`; there is no `depends`
field. Null rules: `name`, `description`, `version`, and `homepage`
are string or null; eligible entries have non-null `kind` and null
`reason` and `detail`; excluded entries have null `kind`, non-null
`reason`, and string-or-null `detail`. The client SHALL
read status only from this envelope, evaluated from the same locked
source reference used for search rows, and SHALL NOT recompute
eligibility from raw cask metadata. The API SHALL live on the flake
only; no hosted service exists.

#### Scenario: Status and rows share one revision

- **WHEN** search rows and a token status are read in one command run
- **THEN** both name the same locked source revision

### Requirement: Missing target detected from the envelope

The client SHALL read the envelope's target list before using any
per-system data, and SHALL report a system outside the target list as
platform-skipped from the envelope alone. The client SHALL NOT probe a
nonexistent per-system attribute, so a genuinely unsupported system is
never confused with a network or evaluation failure.

#### Scenario: Catalog on an untargeted system

- **WHEN** the client runs on a system outside the catalog's targets
- **THEN** the cask source is skipped with the target list shown, and
  no per-system attribute is evaluated

### Requirement: Strict format gate

The client SHALL decode the envelope with an explicit schema
identifier and SHALL refuse an envelope whose schema it does not decode,
naming the schema found and the schema supported.

#### Scenario: Future catalog schema

- **WHEN** the casks source publishes an index schema the client does
  not decode
- **THEN** the command fails with both schema names instead of
  misreading the data

### Requirement: Broad search stays cheap, isolated, and explicit

Catalog search SHALL read the generated index without evaluating or
building package derivations, and SHALL filter locally with a declared
pattern grammar: a Rust regex, documented as such, while the Nixpkgs
lane continues passing patterns to native search unchanged. The
existing discovery cache, source identity, and stale-data rules SHALL
apply to catalog search unchanged. A single bad catalog record SHALL
NOT fail the whole search.

#### Scenario: One unbuildable record

- **WHEN** the catalog contains a record that cannot build and a user
  searches
- **THEN** the search returns rows from the index and no derivation is
  forced

#### Scenario: One bad source record is isolated

- **WHEN** the raw snapshot holds one unsupported record
- **THEN** it is excluded with its reason and search over the index
  still returns every other row, while a structurally corrupt generated
  document is rejected whole with no second client-side classifier

#### Scenario: Pattern grammar is declared

- **WHEN** a user reads the search documentation
- **THEN** the catalog lane's regex grammar and the Nixpkgs lane's
  native pattern behavior are both stated, with no silent semantic
  difference

#### Scenario: Failed catalog fetch with cache

- **WHEN** the live index fetch fails and a revision-matching cache
  exists
- **THEN** cached rows are returned labeled stale with the failure
  recorded

### Requirement: Install gates on generated status

Install and info SHALL resolve cask tokens to the ordinary package
attribute of the casks source. Install SHALL refuse tokens whose
generated status is excluded, with the recorded reason, and SHALL
refuse unknown tokens. Dependency-dependent tokens are excluded at
generation time, so installs stay single-package. Unknown or invalid
tokens SHALL NOT become eligible automatically.

#### Scenario: Excluded token install attempt

- **WHEN** a user tries to install a token the catalog excludes
- **THEN** the install is refused and the recorded reason and version
  are shown

#### Scenario: Unknown token

- **WHEN** a user asks for a token absent from the catalog
- **THEN** it is reported as unknown and never installed

### Requirement: Native lifecycle unchanged by catalog shape

Cask installs SHALL keep the original moving source reference so
native upgrade re-resolves it. Upgrades, rollback, removal, and
launcher sync SHALL use the same native profile path as other
packages. Homebrew cleanup hooks SHALL NOT run.

#### Scenario: Upgrade follows the moving source

- **WHEN** a reviewed source revision updates the generated catalog
- **THEN** a normal native upgrade selects the updated package without
  changing the installed original reference
