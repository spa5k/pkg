## Purpose

Make supported Nix-installed macOS apps accessible through desktop launchers
without changing their stored bundles or managing native installer behavior.

## ADDED Requirements

### Requirement: Intact bundles and limited launcher ownership

pkg SHALL expose supported intact store app bundles through a dedicated
pkg-owned directory under the user's Applications directory.
It SHALL NOT rebuild each app as a per-file link forest or replace unrelated apps.

#### Scenario: Other apps exist

- **WHEN** pkg refreshes its launchers
- **THEN** unrelated contents of the user's Applications directory remain unchanged

#### Scenario: Destination is not owned by pkg

- **WHEN** the selected destination already contains unowned files
- **THEN** pkg refuses to replace the directory and reports the conflict

### Requirement: Launchers derive from active package state

Launcher targets SHALL derive from active native package outputs.
Successful install, remove, upgrade, and rollback SHALL refresh them.
The apps sync command SHALL support an explicit repeat of this derived work.

#### Scenario: App rollback

- **WHEN** an older native generation becomes active
- **THEN** its supported app version becomes the launcher target

#### Scenario: Explicit sync

- **WHEN** the user runs apps sync
- **THEN** pkg rebuilds its launchers from the current profile without changing package selection

### Requirement: Launcher failure does not obscure package state

A launcher failure after a profile change SHALL produce a nonzero result
that states the package change completed and provides the sync retry.
It SHALL NOT roll back packages automatically or label native state corrupt.

#### Scenario: Sync fails after upgrade

- **WHEN** package upgrade succeeds but launcher generation fails
- **THEN** the result distinguishes the completed upgrade from the failed launcher work

### Requirement: Advertised desktop entry points work

An advertised app SHALL work through its documented Finder, Spotlight,
Dock, and CLI entry points, as applicable.
pkg SHALL NOT claim that app rollback restores application-created data.

#### Scenario: App needs unsupported mutable installation

- **WHEN** an app cannot operate from an immutable store bundle
- **THEN** it is excluded or given a working bounded package override before support is advertised
