# Local search allocation design

The native snapshot visitor lends package text for the current SQLite row.
The callback cannot retain those references after the row advances.
Text reads still validate column types and UTF-8.
Database integrity and snapshot identity checks remain in place.

The matcher accepts a private borrowed field view.
The public owned result interface delegates to the same matcher.
Native search builds output rows after matching.
Exact package resolution uses the same borrowed snapshot visitor.

ASCII equality does not need lowercase copies.
Already lowercase ASCII fields can be borrowed.
Other text uses Rust's existing Unicode lowercase conversion.
Exact identity matches still bypass visibility filters.
Name filters run before description normalization.

The benchmark runs each release binary in an isolated temporary home.
It alternates binary order and excludes one warmup per binary and case.
It checks expected result counts and exact output equality.
The fixture does not measure Nix evaluation, downloads, or profile mutations.

The wider CLI benchmark uses temporary Git flakes and real Nix profiles.
It measures 1, 10, and 100 installed entries.
Its source fixtures contain 128 native packages, 1,000 casks, and 2,000 formulae.
The search benchmark also covers 1,002 and 10,002 native records.
Optional resource samples measure child CPU use and peak RSS in a separate pass.
The catalog benchmark uses the pinned upstream input and native Nix metadata.
The native lifecycle test records command wall time beside its existing output.
Host service or administrator paths that cannot be isolated are reported as limits.


## Search spool prepared insert follow-up

The accepted spool still prepares the same INSERT for every retained row.
Test connection-local prepared statement reuse inside push.
Keep the exact SQL and parameter values. Do not change limits, schemas,
result ordering, fuzzy reset behavior, temp-file storage, or integrity checks.
Compare the accepted spool release with a separate temporary candidate.
Use dense full output plus ordinary queries and exact multi-source checks.
Reject the trial if repeated timings do not justify the change on both hosts.


## Borrowed output rows

The output visitor currently copies each SQLite body into a String.
Test a row-local text reference that the output callback consumes immediately.
The reference must not survive the SQLite row. Preserve type and UTF-8 errors,
query SQL, output order, and JSON/text parsing. Compare against the accepted
prepared-insert client with dense output, ordinary queries, large catalogs,
exact multi-source output checks, and separate resource samples.
Use a separate temporary source and frozen binary. Restore accepted source
before timings. Select only after complete measurements and required checks.


## Unlimited JSON result sorting

The baseline maintains an order index during each spool insert.
An unlimited JSON query does not prune rows during insertion.
The trial omits that single index only when output is ungrouped and unlimited.
The existing ORDER BY rank, id performs the final SQLite sort.
Grouped text and limited queries keep the exact accepted indexes.
SQLite file-backed temporary storage and cache settings stay unchanged.
Measure dense and broad output plus separate CPU and peak RSS passes.
Keep output equivalence checks and full verification before selection.


## Native source ID buffer

The current native loop allocates a source-qualified ID before every match.
A literal search usually returns few rows from a large catalog.
The trial keeps a String buffer for IDs that do not match.
A matched row takes ownership of that buffer, as the accepted code does.
The next row reserves enough capacity before it writes the same ID bytes.
This avoids a second ID copy for dense output.
Keep the complete source scan and all UTF-8 and integrity checks.
Measure literal, typo, regex, dense output, and separate CPU/RSS samples.

## Complete catalog follow-up trials

The full Cask and Formula profile locates repeated allocation of JSON value maps.
The typed-system trial decodes the known entries map directly.
It ignores unknown system metadata without retaining another value map.
The outer schema decoder and all source, target, status, and version checks remain.
System sections still require maps. Public conversion from a value map remains available.

The unique-name trial records a publication fact in the same SQLite transaction.
New catalogs with no repeated exposed names can scan the packages table directly.
Old metadata defaults to the original claims join.
Catalogs with collisions keep the original join and full attribute identities.
The schema, claims table, provenance checks, and SQLite integrity checks remain.
An old snapshot gains no new scan path until a new snapshot is prepared.

The fixed-row fuzzy trial keeps the OSA recurrence and Unicode reference algorithm.
It uses three fixed ASCII rows and avoids repeated normalized candidate names.
A separate bounded-distance variant is measured directly before an algorithm decision.

## Combined validation

Individual measurements identify candidate changes. They do not establish a combined gain.
Freeze one combined default-profile release per host after full required checks.
Compare it directly with origin for broad commands, normal release installation,
complete cached sources, fresh catalog preparation, and larger synthetic native catalogs.
Keep the exact source and binary hashes with every comparison.
Run builds, archive work, resource passes, and timed workloads in separate phases per host.
