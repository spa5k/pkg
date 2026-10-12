# Rust design

## Errors

Process supervision returns `io::Result`.
The native adapter stores I/O and JSON errors with their types.
Configuration errors keep TOML and I/O causes.
Catalog errors keep native and cache causes through `Error::source`.
Command reporting selects the exit status from the error variant.
Cask gates and info helpers return `CommandError` without converting cancellation to text.

## Metadata

Each metadata row has one path and a tagged enum.
Package rows require metadata.
Child rows require child names.
Skip and evaluation error rows carry no package data.
Serde rejects unknown tags and missing required fields before cache writes.

## Ownership

Catalog views and saved catalogs share immutable indexes with `Rc`.
Command execution is single threaded.
Tap cache writes require `&mut TapState`.
Map entry APIs load each source once without `RefCell` or repeated map lookups.
Fallible lazy loads use `Option::insert` and enum matching.

## Publication

Snapshot writes use `NamedTempFile` for unique paths and cleanup on drop.
Old process ID temporary paths do not block a new write.
The generator sets catalog permissions and syncs staged data before publication.
Pin writes also sync staged data before publication.

## Validation

Add tests for error causes, invalid metadata, and cancelled cask commands.
Run the workspace checks and the native CLI checks.
Open a separate PR with PR #113 as its base.
