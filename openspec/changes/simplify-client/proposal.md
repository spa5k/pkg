# Simplify the current client

## Problem

The checkout still contains executable experiments for the deleted managed Nix
design. Current app handling mixes storage, bundle discovery, assessment,
helper management, and launcher updates in one file. Query commands mix source
selection, result storage, and info output. Catalog validation repeats identity
and version rules that other modules already own.

## Decision

Remove the superseded experiment code. Keep its findings and evidence. Link
deleted files to the exact Git revision that contains them.

Split app handling, catalog decoding, and query commands by responsibility.
Use one implementation for repeated identity, version, locking, and source
selection rules. Remove the old private app payload layout and obsolete cache
cleanup. Keep the current native Nix profile and current generated catalogs.

Use the request for a 30 percent reduction as direction. Report the measured
reduction for all source files and for the current product separately. Do not
count file moves as deleted code. Keep behavioral tests for current contracts.

## Verification

Run the workspace checks, installer tests, documentation link checks, and
native lifecycle checks. Check current private app payload discovery and
rejection of malformed payloads. Record the source counts and check results.
See the [verification record](../../../docs/verification/simplify-client-2026-10-12.md).
