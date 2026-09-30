## ADDED Requirements

### Requirement: Native cancellation has one result
The client SHALL reap a cancelled child and report interruption consistently in filtered and verbose output. It SHALL retry a mutation only after a new user command.

#### Scenario: Cancel a running build
- **WHEN** the user sends SIGINT during a native build
- **THEN** the client exits with code 130 after the child ends
- **AND** an explicit retry can install and run the package

### Requirement: Revision rules follow source semantics
The client SHALL treat local paths and archive URLs as mutable sources. A hexadecimal directory name SHALL NOT establish a Git revision.

#### Scenario: Upgrade a hexadecimal path
- **WHEN** a local source with a 40-character hexadecimal directory name changes
- **THEN** upgrade resolves that source again
- **AND** the installed executable reports the new version

### Requirement: Qualified resolution isolates saved taps
A qualified request SHALL read only its required saved tap. Bare resolution SHALL refuse an unreadable saved tap. Doctor SHALL report saved tap health without changing it.

#### Scenario: An unrelated saved generation is damaged
- **WHEN** the user requests a qualified Nixpkgs package
- **THEN** that request can complete
- **AND** bare resolution refuses the damaged tap and doctor names it

### Requirement: Namespace choices resolve exactly
Search and ambiguity results SHALL retain full native attributes when short forms collide, including collisions outside the filtered result set.

#### Scenario: Two namespaces contain the same short name
- **WHEN** packages and legacyPackages expose that name
- **THEN** each displayed choice resolves to its own package

### Requirement: macOS bounds survive generation and apply before mutation
The generated plan and client index SHALL retain both declared macOS bounds. Invalid numeric bounds SHALL fail closed. Install and upgrade SHALL compare the actual host version before profile mutation. Upgrade SHALL use the installed entry's original source.

#### Scenario: The installed source adds an incompatible maximum
- **WHEN** the configured cask source changes and the installed source declares a maximum below the host version
- **THEN** upgrade refuses the installed source's package
- **AND** the profile remains unchanged

### Requirement: Extraction preserves vendor resources
Extraction SHALL remove materialized Apple metadata streams at every directory level. It SHALL preserve ordinary vendor files and validated relative links.

#### Scenario: A signed disk image contains nested metadata streams
- **WHEN** 7zz materializes nested extended attributes as regular files
- **THEN** those metadata files are removed
- **AND** the extracted vendor resources and links match the original app

### Requirement: External signatures are not silently discarded
The cask builder SHALL preserve supported external signature attributes as ordinary Nix data. It SHALL refuse malformed, unknown, orphaned, and symlinked signing streams before extraction writes. Normal client failure output SHALL show the specific builder cause when Nix provides it.

#### Scenario: A vendor data file uses a signature in extended attributes
- **WHEN** a supported regular in-bundle file carries external signature attributes
- **THEN** the builder records the exact attribute bytes outside the app payload
- **AND** the incomplete store bundle is not exposed in `Applications`
- **AND** launcher sync and bundled commands restore and verify a private copy before use
- **AND** neither path changes or creates a vendor signature

#### Scenario: A signature stream names no regular file
- **WHEN** a signing stream is orphaned, symlinked, or unsupported
- **THEN** the build refuses it before extraction writes
- **AND** the native package profile remains unchanged

### Requirement: Restored app copies are derived state
Native profiles SHALL remain the package inventory. A restored app cache SHALL be private, versioned by its store output, protected by an ownership marker and a preparation lock, and published only after verification. Cancellation SHALL NOT publish an incomplete app. Public tap assessment SHALL check the restored bundle before launcher exposure. Missing or damaged owned copies SHALL be recoverable from the store payload.

#### Scenario: Remove and roll back an externally signed app
- **WHEN** the user removes the native entry and then rolls back
- **THEN** removal removes its launcher
- **AND** rollback derives a verified launcher from the restored native entry
- **AND** cache presence alone never establishes that a package is installed
