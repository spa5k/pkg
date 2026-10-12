## Purpose

Deliver a standalone pkg client with fresh native profile state and remove
the old product's machine lifecycle and compatibility obligations.

## ADDED Requirements

### Requirement: Standalone client release

The release SHALL supply a client for Linux x86-64 and Apple silicon macOS,
with notices, completion support, and artifact integrity information.
It SHALL NOT require a pkg broker, root helper, private runtime bundle,
central product catalog service, or Homebrew.

#### Scenario: Fresh supported host

- **WHEN** a user with externally installed Determinate Nix installs the client archive
- **THEN** package operations work without the development checkout or old pkg services

### Requirement: Fresh state without automatic migration

The new client SHALL use its new profile and configuration conventions.
It SHALL NOT import or automatically delete old pkg package state, runtime
receipts, accounts, or services. Old command and test contracts SHALL NOT
constrain the replacement.

#### Scenario: Old state exists

- **WHEN** the new client is run on a machine with old pkg files
- **THEN** it uses the new native profile and does not adopt or erase those files

### Requirement: Client lifecycle does not tear down Nix

Client update SHALL preserve the native package profile.
Client removal SHALL leave the externally managed Nix runtime and package
state intact unless the user separately performs explicit native cleanup.

#### Scenario: Remove only the client

- **WHEN** the pkg executable is removed
- **THEN** Nix remains usable and the dedicated package profile remains inspectable with Nix
