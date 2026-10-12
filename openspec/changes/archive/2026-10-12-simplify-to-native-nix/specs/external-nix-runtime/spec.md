## Purpose

Use an externally managed Nix runtime without making pkg responsible for
machine setup, privileged services, or runtime maintenance.

## ADDED Requirements

### Requirement: External runtime ownership

pkg SHALL use an existing supported Determinate Nix installation.
It SHALL NOT install, update, repair, or remove that runtime.

#### Scenario: Nix is missing

- **WHEN** a command needs Nix and the executable is unavailable
- **THEN** pkg fails with vendor setup guidance without starting an installer

#### Scenario: Client update

- **WHEN** the pkg executable is updated
- **THEN** the external Nix runtime and daemon configuration remain unchanged

### Requirement: Ordinary-user execution

Package operations SHALL run through the normal Nix daemon as the invoking user.
pkg SHALL NOT require its own privileged service, root helper, or trusted-user grant.

#### Scenario: Allowed ordinary user

- **WHEN** a normal allowed user installs a package
- **THEN** pkg uses Nix without sudo, a pkg broker, or a pkg root helper

### Requirement: Runtime compatibility and useful failures

pkg SHALL check required runtime capabilities before mutation.
It SHALL preserve useful upstream diagnostics and SHALL NOT report runtime,
source, or integrity failures as a successful empty result or a missing package.
Unknown failure categories SHALL remain generic failures with context.

#### Scenario: Required interface is unavailable

- **WHEN** Nix lacks a required command or returns an unsupported required data shape
- **THEN** pkg reports the incompatibility without rewriting profile state itself

#### Scenario: Source download fails

- **WHEN** a selected source cannot be downloaded
- **THEN** pkg reports the source operation failure instead of package-not-found

### Requirement: Source settings do not alter daemon trust

pkg SHALL disable automatic acceptance of source-provided Nix configuration.
It SHALL NOT add substituters, trust keys, or daemon permissions from package metadata.

#### Scenario: Flake requests a new cache

- **WHEN** a selected flake supplies a substituter and a signing key
- **THEN** pkg does not accept those settings or change daemon configuration
