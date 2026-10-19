# Speed up local search

## Why

Local search copied catalog text repeatedly and created output rows before knowing whether a row matched. Large catalogs made this cost visible.

## What Changes

- Borrow SQLite text and the native source ID buffer while matching.
- Create result rows only after a match.
- Create only output-specific spool indexes and reuse the fixed prepared INSERT.
- Decode catalog entries directly, use fixed-row ASCII typo distance, and check duplicate fuzzy inputs once.
- Use a proven unique exposed-name scan for newly published catalogs.
- Preserve Unicode, ranking, limits, provenance, catalog integrity, and legacy catalog behavior.

## Capabilities

### New Capabilities

- None.

### Modified Capabilities

- None.

## Impact

Search latency and CPU use fall on real and synthetic catalogs. Output bytes and result ordering remain unchanged.
