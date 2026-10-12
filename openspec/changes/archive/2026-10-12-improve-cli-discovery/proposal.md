# Improve package states and discovery

## Problem

Profile read failures can appear as “not installed.” Package names differ
between discovery and native profile commands. Search treats ordinary text
as a regular expression, returns too many rows, and can spend several minutes
preparing metadata before the first result.

## Decision

Keep the native Nix profile authoritative. Report unknown installed state.
Use the same package identities for search, info, list, remove, and upgrade.
Use literal search by default. Rank and limit human results. Prepare complete
local catalogs during installation. Use a daily user job to refresh official
catalogs. The job exits after each run. It never changes installed packages.

This change extends the discovery design in
[real-use repairs](../2026-10-12-repair-realistic-use-faults/proposal.md).

## Verification

Test profile failures, ambiguous installed names, literal queries, ranking,
limits, unavailable casks, cache-only search, failed refresh retention,
installer preparation, and user job configuration. Run the required checks.

Recorded results: [CLI discovery verification](../../../../docs/verification/2026-10-11-cli-discovery.md).
