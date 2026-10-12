# Design

## Discovery data

Resolve the source once and evaluate its locked reference. Enumerate the
native package roots and recursive legacy package sets. Evaluate only
derivation names and descriptions in batches of 256 paths. Start a new
Nix process for each batch. Disable import from derivation. Preserve the
native traversal and name parsing rules. Bisect failed batches to isolate
ignored legacy evaluation errors. A fatal package-root error fails the
source. A batch above 512 MiB RSS is stopped and split; a single oversized
path is retried with more frequent garbage collection, then fails the source
if it still exceeds the same limit. Use a small initial GC heap and immediate
allocator purging for these metadata processes. Set the attribute-set update
threshold to 65536 to reduce allocations without changing values. Sample RSS
every 5 ms and stop at 480 MiB to leave 32 MiB for growth between samples.
Cancellation stops the scan without publishing it.

Write paths, metadata, and namespace claims into a staging SQLite database.
Use an 8 MiB page cache and disk-backed temporary data. Publish one complete
snapshot per source and system by atomic replacement. Record schema,
source, system, locked reference, revision, and completion. Keep the old
complete snapshot until replacement succeeds. Corrupt or mismatched files
cannot answer as current. Mutable path sources use disposable snapshots.
Keep the smaller generated cask envelopes in their existing JSON format.

Stream SQLite rows through the existing case-insensitive Rust regex.
Preserve full-namespace ID claims, source order, row order, provenance, and
the query JSON schema. Spool matched rows on disk for human table width and
JSON output, instead of collecting an unbounded result vector. Do not add
FTS or a second regex grammar.

`pkg update` refreshes source identities and prepares changed snapshots in
the foreground. It shows progress on stderr. First search does the same
when no current snapshot exists. Cache generation has its own process
lock. Bare resolution checks exact attributes first, across both native
roots and every applicable cask source. Broader matching requires a complete
current snapshot; stale discovery never authorizes an install.

## Native profile operations

Lock the logical profile path, not the generation it points to. Hold the
lock before inventory checks and through the native mutation, verification,
reporting, and app synchronization. Standalone app sync takes the same
lock. Lock ordering is profile, then app sync. OS locks release on exit;
there are no PID-file stale-lock rules. External raw Nix commands do not
participate in this lock.

Check all active entries for identical source, attribute, and requested
outputs before adding an entry. Preserve different pins and outputs. Print
`Already installed` for an unchanged request. Generate profile-scoped
conflict recovery commands, including verbose native output. Keep native
rollback selection and report the actual destination generation.

## Desktop packages and setup

Namespace private cask files under `libexec/pkg-casks/<store-output-name>`.
Update the manifest, materializer, bundled command wrappers, and Rust
discovery together. Read old schema-1 outputs for existing profile history.
Keep signatures and user application data intact.

Provide DejaVu fonts and a runtime Fontconfig configuration for AppImages.
Keep user fonts available. Do not require modifications to the user's home.

Preserve existing daemon environment keys when setting TMPDIR. Reload the
changed launchd definition with bootout/bootstrap and inspect live TMPDIR.
Retry bootstrap's transient status 5 for up to ten seconds while launchd
finishes removing the old service. Other failures remain failures.
Reuse the strict effective-config gate. Remove only the known mandatory,
self-mapped `/private/tmp` and `/private/var/tmp` defaults. Custom unsafe
grants need manual review. The fresh in-build host
read/write/network probe remains authoritative. Do not report setup as
ready from the two boolean settings alone. Plan boolean repairs from the
effective values, including settings overridden later in the config file.
