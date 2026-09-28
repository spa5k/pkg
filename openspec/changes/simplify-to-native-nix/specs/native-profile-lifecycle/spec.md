## Purpose

Manage user packages through one dedicated native Nix profile and accept
native generation, conflict, rollback, and retention behavior.

## ADDED Requirements

### Requirement: Dedicated authoritative profile

pkg SHALL use a named per-user Nix profile separate from the default profile.
Native profile metadata SHALL be the sole authority for installed packages.
No discovery cache or product manifest SHALL override it.

#### Scenario: First installation

- **WHEN** a package is installed through pkg
- **THEN** it appears in pkg's native profile while the user's default profile is unchanged

#### Scenario: Cache is removed

- **WHEN** all pkg discovery cache files are deleted
- **THEN** list still reports installed entries from Nix

### Requirement: Native package mutations

Install and remove SHALL delegate package realization and profile changes to Nix.
pkg SHALL allow normal upstream substitution and build behavior without a
separate build-approval record. Removal SHALL address selected exact entries.

#### Scenario: Build is needed

- **WHEN** Nix cannot substitute an installable and can build it
- **THEN** pkg permits normal Nix realization without requiring old product approval

#### Scenario: Conflicting output files

- **WHEN** Nix rejects a profile change because entries conflict
- **THEN** pkg reports that failure without silently applying a keep-first or keep-last policy

### Requirement: Native upgrades preserve source meaning

Upgrade SHALL follow each entry's original moving source reference.
An entry fixed to an exact revision SHALL remain fixed and be identified as such.
Discovery refresh SHALL NOT redefine installed entries' original references.

#### Scenario: Moving source advances

- **WHEN** an installed moving source has a newer revision and the user upgrades it
- **THEN** Nix resolves the original reference and pkg reports the resulting installed state

#### Scenario: Fixed source

- **WHEN** the user requests upgrade for an exact-revision entry
- **THEN** pkg reports that the reference is fixed and does not replace it with a moving reference

### Requirement: Native history and rollback

History and rollback SHALL use Nix generations.
The retained native generation links SHALL protect their outputs from collection.
Rollback SHALL change package versions, not application-created user data.

#### Scenario: Previous generation exists

- **WHEN** the user rolls back to a retained generation
- **THEN** its packages become active through the dedicated profile

#### Scenario: App has mutable data

- **WHEN** a packaged app version is rolled back
- **THEN** pkg leaves that app's preferences and user databases unchanged

### Requirement: Explicit profile retention

Prune SHALL require an explicit positive age and remove only eligible
non-current generations from pkg's profile. It SHALL NOT invoke store-wide GC.

#### Scenario: Prune old history

- **WHEN** the user runs prune with an age
- **THEN** eligible old generations are removed and the current generation remains

#### Scenario: No retention argument

- **WHEN** prune is called without an age
- **THEN** pkg returns a usage error without deleting history

### Requirement: Interrupted operations report native state

pkg SHALL forward cancellation to its child and reap it.
It SHALL re-read the profile after an uncertain result where possible.
It SHALL NOT automatically repeat an uncertain mutation or require a product journal.

#### Scenario: Cancellation during installation

- **WHEN** the user interrupts a native operation
- **THEN** pkg ends the child operation and reports observed state or explicit uncertainty

### Requirement: Reduced command and output contract

pkg SHALL expose the commands defined in design decision D4.
Removed alpha commands and options SHALL fail as usage errors.
JSON output SHALL be limited to versioned query results for search, info,
list, and doctor. The old progress event and exit-code contracts SHALL NOT apply.

#### Scenario: Removed command

- **WHEN** the user calls pin, repair, or system uninstall
- **THEN** the new CLI rejects it instead of invoking legacy machinery

#### Scenario: Structured installed query

- **WHEN** the user requests list as JSON
- **THEN** pkg returns the documented query schema with native entry identity

### Requirement: Read-only diagnostics and shell setup

Doctor SHALL inspect without repair or transmission.
Shellenv SHALL print repeatable shell settings without editing user startup files.

#### Scenario: Launcher drift

- **WHEN** doctor detects an outdated launcher
- **THEN** it reports the condition and the sync command without changing files

#### Scenario: Shell setup requested twice

- **WHEN** the emitted shell setup is evaluated twice
- **THEN** the dedicated profile is available without duplicate path entries
