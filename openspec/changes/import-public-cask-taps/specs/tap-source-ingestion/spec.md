## Purpose

Convert approved public Ruby taps into captured metadata and generic Nix
artifact plans locally, with consent, a credential-free public fetch, and
Nix-provided Ruby confined to an ordinary sandboxed derivation.

## ADDED Requirements

### Requirement: Consent-gated local import

The client SHALL provide `tap add SOURCE [--revision SHA] [--trust]`,
`tap list`, `tap update [SOURCE] [--revision SHA]`, and
`tap remove SOURCE`. First consent SHALL show the canonical repository and
approval scope and SHALL precede any tap Ruby execution. Interactive consent
SHALL be remembered per origin. Noninteractive import SHALL require
`--trust`. Ordinary search of saved catalogs SHALL NOT run Ruby and SHALL
NOT prompt. Removing a tap SHALL block future use of that source without
removing installed packages.

#### Scenario: Noninteractive add without trust

- **WHEN** `tap add` runs noninteractively without `--trust`
- **THEN** no Ruby executes and no source is added or changed

#### Scenario: Search stays passive

- **WHEN** the client searches saved catalogs
- **THEN** it reads saved data only, with no prompt and no Ruby execution

#### Scenario: Upgrade after tap removal

- **WHEN** a source is removed with `tap remove` and `pkg upgrade --all` runs afterwards
- **THEN** entries whose original reference is the removed source are filtered out, the remaining entries are upgraded by their explicit native entry IDs (not a native `--all`), and an explicit upgrade of a removed-source entry refuses

### Requirement: Credential-free public fetch

Public repository and source input fetches SHALL NOT inherit credentials.
The fetch SHALL use a real URL parser, verify redirect and destination
policy, refuse local and private-network destinations for untrusted URLs,
and disable hooks, external filters, and submodules. Source identity SHALL
pin whole source content together with runtime, policy, and target, and
SHALL record source-relative file provenance. A helper-only change SHALL
invalidate cached output.

#### Scenario: Redirect to a private address

- **WHEN** a public source URL redirects to a local or private-network destination
- **THEN** the fetch is refused before any Ruby runs

#### Scenario: Helper-only commit

- **WHEN** a helper file changes while a cask file is unchanged
- **THEN** the source tree identity changes and cached export output is invalidated

### Requirement: Sandboxed Nix-provided Ruby evaluation

Tap Ruby SHALL execute only inside an ordinary sandboxed Nix derivation
supplied with pinned native Ruby and pinned upstream Homebrew reader
dependencies. The derivation SHALL NOT be a fixed-output derivation, SHALL
have the sandbox enabled, and the system SHALL refuse sandbox fallback,
relaxed mode, and added host paths. User home, secrets, daemon sockets, and
model credentials SHALL NOT enter builds. Execution and output SHALL be
bounded. A failure SHALL NOT create a successful empty catalog. Captured
metadata SHALL be treated as untrusted input. Tap-supplied Nix files or
configuration SHALL NOT be evaluated. Effective daemon isolation SHALL be
verified with behavior probes on each supported platform before any safety
claim, and the system SHALL fail closed when those probes show insufficient
denial. The product SHALL NOT automatically alter the host daemon
configuration and SHALL NOT add root-equivalent trusted-users entries as a
workaround.

#### Scenario: Denied resource access

- **WHEN** cask Ruby attempts network access, reads the host home, or reads a supplied credential sentinel during evaluation
- **THEN** the derivation denies it, records a bounded failure, and no sentinel content appears in captured output

#### Scenario: Output exhaustion

- **WHEN** cask Ruby produces unbounded output
- **THEN** captured diagnostics and result size stay within the configured bounds and the outcome is never an unbounded capture or a silent success

#### Scenario: Insufficient daemon isolation

- **WHEN** the host daemon's effective sandbox is disabled or the setting is ignored, so behavior probes show reads, writes, or network allowed
- **THEN** the system fails closed: the reader does not run, no import result is produced, and the required daemon setup is reported instead of being applied automatically

### Requirement: Preserve conversion limits

The importer SHALL support only the generic artifact subset: GitHub release
archives, raw release binaries, app bundles, and existing safe artifact
plans, with required payload checksums, archive traversal and symlink
protection, and script rejection. Installer scripts, payload-dependent
metadata, unsupported dependencies, and active installer actions SHALL be
rejected or excluded with recorded reasons. Staging access during DSL
evaluation SHALL produce an excluded `ruby-load-error` entry whose detail
identifies `staged_path` / `staged-path-during-source-eval`, sticky even
when the source rescues the error. A legacy deprecated callback MAY fail
the reader load with `ruby-load-error`; only metadata that loads SHALL
carry callback or structured-step markers. Required install actions that
no generic builder reproduces SHALL NOT become eligible. This conservative
guard SHALL NOT be presented as a complete check of arbitrary Ruby purity,
and the system SHALL NOT promise that every public tap imports
successfully.

#### Scenario: Payload-derived file list

- **WHEN** metadata uses the upstream staging accessor to enumerate a downloaded application
- **THEN** the importer reports an excluded `ruby-load-error` with detail containing `staged_path` / `staged-path-during-source-eval` instead of publishing an incomplete artifact list as eligible

#### Scenario: Required postflight operation

- **WHEN** the upstream definition contains a required postflight action with no equivalent generic builder behavior
- **THEN** the generated target is excluded and its reason identifies the unsupported operation

### Requirement: Captured-byte reproducibility

Export snapshots SHALL identify the source tree, reader runtime, target
context, and exported content hash. Offline Rust generation from the same
captures and compiler version SHALL produce identical catalog bytes. The
system SHALL NOT claim that a raw source revision alone makes arbitrary
Ruby evaluation deterministic.

#### Scenario: Regenerate without Ruby

- **WHEN** captured exports are regenerated offline from the same inputs
- **THEN** the catalog bytes are identical without evaluating tap Ruby

### Requirement: Complete update and atomic publication

Every discovered record and target SHALL have an output or a recorded
exclusion. Missing inventory, corrupt output, or a source-wide failure
SHALL abort the refresh. The importer SHALL NOT publish the client
registry; the client SHALL publish only after validation succeeds, and a
failed refresh SHALL preserve the previous source.

#### Scenario: One source crashes

- **WHEN** one registered source fails before producing its required inventory
- **THEN** the client refuses to publish a partial catalog and retains the prior catalog bytes

#### Scenario: Atomic exchange unsupported

- **WHEN** the filesystem does not support atomic directory exchange for publication
- **THEN** publication fails and the previous source directory and catalog bytes remain unchanged, with no two-rename fallback applied
