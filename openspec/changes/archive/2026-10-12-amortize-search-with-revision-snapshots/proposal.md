# Amortize discovery with revision snapshots

Planning change, 29 September 2026. Baseline: current workspace `main`
after the native-client simplification and generated cask catalog changes.

## Why

`pkg search` runs native `nix search <locked-rev> <query>` on every new
query at the same revision. That native query evaluates the whole nixpkgs
tree for the system even when only the pattern differs: measured 6.6 s per
new query with Nix's eval-cache warm, ~30 s cold, on a host where the
complete result set for the system is only 113,596 entries / 19 MB of JSON
and costs about the same one-time fetch (7.9 s warm). pkg's existing cache
is keyed by (source, system, query), so it only accelerates the exact
same query text.

A concurrency rewrite of the runner was reviewed and rejected: it saves
only a few seconds and adds process-signal complexity.

## What Changes

- **BREAKING:** The nixpkgs lane stops passing the user pattern to native
  `nix search`. Discovery evaluates `nix search <locked> ^ --json` once —
  the complete metadata map for (source, system) — and filters locally
  with the same Rust regex engine the cask lane already uses
  (case-insensitive match against the three strings native search
  matches: the full attribute path, the pname — already version-stripped,
  never reparsed — and the description, empty descriptions included;
  verified against native result sets, Nix 2.35.2 `src/nix/search.cc`).
- **BREAKING:** The per-query `search-*.json` cache is removed. A new
  disposable snapshot cache `catalog-*.json` stores the complete
  result map keyed by (kind, source, system) — ONE latest snapshot per
  identity, with the locked revision recorded inside and atomically
  replaced on every fetched revision — under a new schema version and a
  distinct `kind` field, so an old per-query file can
  never be read as a catalog. The cask lane snapshots the whole
  generated `catalogIndex` envelope the same way instead of re-evaluating
  it per query.
- Pattern grammar is now one documented Rust regex grammar for both
  lanes. An invalid pattern is reported as a source failure before any
  expensive evaluation. Patterns relying on engine-specific extensions
  outside Rust regex (for example backreferences) stop matching on the
  nixpkgs lane; common literals, alternation, anchors, and character
  classes behave as before.
- A snapshot is reused only when the freshly resolved locked revision and
  locked reference equal the snapshot's recorded identity. A metadata
  failure is never treated as proof of freshness; it falls back to the
  one latest readable snapshot flagged stale (its own recorded revision
  labels the rows), or fails honestly. A source failure during the
  one-time fetch behaves the same way. A stale cached cask index that
  does not target the current system keeps the live failure detail: only
  a freshly resolved index may report a clean platform skip. An invalid
  or unreadable snapshot file is ignored and refetched, never fatal.
- Snapshots are written atomically (temp file + rename) so concurrent
  invocations cannot leave a torn file. No lock framework.
- `pkg update` invalidates snapshot files together with all other
  discovery cache data.
- Mutable/local path sources without an immutable revision query live and
  write no snapshot, exactly as they write no reusable cache today.
- First query at a new revision remains a full evaluation (~30 s cold,
  ~8 s warm). That cost is stated honestly in documentation; no instant
  cold search is promised.

## Non-goals

- No hosted, central, or shared index, server, or service.
- No change to install, bare-name resolution, runtime discovery, or
  version behavior (install and setup are owned by another change,
  PR #84). Bare install/info resolution stays at baseline behavior.
- No change to nix/process.rs child handling, saved taps (still pure
  local), platform/eligibility rules, row fields, provenance, or result
  ordering.
- No dual-engine regex parity layer; unsupported advanced patterns fail
  or error visibly and are documented as removed.
- No concurrency rewrite.

## Capability

### Modified Capabilities

- `package-discovery`: snapshot cache identity, local filtering grammar,
  invalidation, and stale rules.
