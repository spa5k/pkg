# Design: revision snapshots for discovery

## Context

Verified native `nix search` semantics (Nix 2.35.2, read from
`src/nix/search.cc` and confirmed with live queries at a locked nixpkgs
revision; see the review note `^python312Packages` returning zero proves
nothing — full attribute paths start with
`legacyPackages.<system>.`, so an anchor at `python312Packages` can never
match them):

- The pattern is a POSIX ERE, matched case-insensitively and unanchored.
- Matched fields, per derivation: the FULL attribute path (for example
  `legacyPackages.x86_64-linux.python312Packages.requests`), the
  derivation name with the version stripped (exactly what the JSON
  already reports as `pname` — never reparsed), and the description
  (an absent or empty description is the empty string; `^$` matches it).
- `^` selects every package; with `--json` there is no highlighting cost.

Full result map for x86_64-linux at one nixpkgs revision: 113,596
entries, 19.1 MB JSON, 7.9 s with warm eval-cache — the same order as any
single new-query native search (6.6 s), because whole-tree evaluation
dominates and is per-pattern today only because pkg asks per-pattern.

## Decision

Evaluate once per (source, system, locked revision), cache the complete
map, and filter locally per query.

1. Snapshot record (`catalog-*.json`, schema 4): `kind`
   (`nixpkgs-snapshot` or `cask-index`), moving `source`,
   `locked_reference`, `revision`, `system`, `saved_unix`, and the full
   result map (nixpkgs) or full index envelope (cask) in one `payload`
   field. The filename key hashes (kind, source, system) — never the
   query or the revision: exactly ONE latest snapshot is kept per
   identity, atomically replaced whenever a new revision is fetched, so
   the cache cannot grow per revision. Read re-validates schema, kind,
   source, system, and (for fresh reuse) revision and locked reference,
   so a stale, misplaced, corrupt, or legacy file cannot answer as a
   catalog.
2. Lane flow: resolve source identity; if the resolved revision matches a
   readable snapshot, filter locally (status fresh, no evaluation).
   Otherwise run the one-time full fetch against the locked reference,
   write the snapshot atomically, then filter. An invalid regex fails the
   lane before any child runs (compile check first).
3. Local filter: one shared Rust regex, case-insensitive, matched
   against the same three strings native search matches for the nixpkgs
   lane — the full attribute path, the pname (already version-stripped;
   never reparsed from the version), and the description (empty included)
   — and token/name/description for the cask lane as today. This unifies
   the documented grammar on the engine pkg already ships. Native search
   uses POSIX ERE (not ECMAScript); the Rust regex engine agrees with it
   for ordinary patterns (verified against native result sets) but
   differs on advanced syntax (backreferences, lookaround are errors in
   Rust regex): accepted breaking change, documented.
4. Failure ladder: metadata fetch failure or live fetch failure reuses
   the one latest readable snapshot for the same source+system flagged
   `stale` (rows keep the snapshot's own reference/revision), else the
   lane fails honestly. A metadata failure is never treated as proof of
   freshness. A stale cached cask index that does not target the current
   system keeps the live failure detail; only a freshly resolved index
   may report a clean platform skip. An unreadable/invalid snapshot is
   ignored and refetched.
   Sources without an immutable revision (local paths) never read or
   write snapshots; they query live each time.
5. Bare-name resolution (install/info) is out of scope here and stays at
   baseline behavior: no snapshot reuse, no stale fallback. Targeted
   `info` lookups (`nix search <ref>#attr .`) stay native and targeted;
   they never evaluate the whole tree.
6. `pkg update` removes `catalog-*.json` together with all other
   discovery cache files.
7. Resource shape: ONE ~19 MB file per (source, system, kind), replaced
   atomically on every fetched revision, one resident map per command
   run (~60 MB peak while filtering 113k entries), parsed once per lane
   — no per-query copies, no full-catalog clone to write the cache (the
   owned map is serialized by reference, then consumed for filtering),
   and no unbounded growth or all-file scans on the stale fallback.

## Risks

- Regex engine change on the nixpkgs lane can alter results for advanced
  patterns. Mitigation: document the grammar; common patterns (literals,
  `|`, anchors, classes) verified identical to native behavior.
- Bigger cache files (19 MB vs KB). Accepted: disposable derived data,
  one per identity, machine-local.
- First query at a new revision still pays full evaluation. Stated in
  docs; it is inherent to `nix search` and unchanged by this design.
