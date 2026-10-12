# Tasks: revision snapshots for discovery

## 1. Snapshot cache

- [x] 01-01 Add one latest `catalog-*.json` snapshot per (kind, source,
      system) (schema 4, generic `Snapshot<P>` payload) with the locked
      revision as an identity field inside, read re-validation of
      schema/kind/source/system/revision, atomic tmp+rename replacement
      on every new revision, and invalidation of both cache prefixes
      (legacy per-query files included).
- [x] 01-02 Unit tests: identity re-validation, revision checking, kind
      separation, one-file replacement across revisions, invalid and
      old-schema files ignored, invalidation count.

## 2. Lanes

- [x] 02-01 Rework `search_source` (nixpkgs) onto the snapshot + local
      Rust regex filter over the three native matching fields — full
      attribute path, pname (already version-stripped), description
      (empty included) — case-insensitive; invalid pattern fails before
      any child runs; no full-map clone for rows or cache writes.
- [x] 02-02 Rework `search_catalog` (cask) to snapshot the whole index
      envelope once; keep platform-skip-from-envelope, eligible-only
      rows; a stale cached index that does not target this system keeps
      the live failure detail — only a freshly resolved index may report
      a clean platform skip.
- [x] 02-03 Behavior tests: fresh reuse at same revision (plus locked
      reference match), different query from the same snapshot,
      one-file replace at new revision, stale reuse on live failure
      (single latest-file read, no scans), invalid snapshot refetch,
      invalid pattern before any child, path-only and `^$` matching
      regressions, stale untargeted index after metadata failure;
      behavioral comparison against native result sets (fzf, ripgrep,
      path-only, `^$` on a small flake) with exactly equal result
      fields and source reports.

## 3. Commands and docs

- [x] 03-01 `pkg update` invalidates snapshots and legacy cache files;
      lifecycle wording checked. Bare-name resolution
      (`resolve_inner`, install/info) stays at baseline behavior: no
      snapshot reuse, no stale fallback.
- [x] 03-02 `docs/commands.md` grammar section rewritten: shared Rust
      regex grammar, three matching fields, POSIX-ERE divergence note,
      empty-description behavior.
- [x] 03-03 Spec delta updated for latest-snapshot identity and matching
      fields (strict OpenSpec validation runs with the parent locally;
      not claimed here).

## 4. Verification

- [x] 04-01 fmt, clippy strict, targeted and full tests, rustdoc.
      Verified on v0.2.0-alpha.3 base: `cargo fmt --all`,
      `cargo clippy --locked --all-targets -- -D warnings`, strict rustdoc
      (`-D warnings`), and `git diff --check` all clean; full workspace
      suite 205 tests passed, 0 failed.
- [x] 04-02 Live measurements (release build, warm timings):
      new query 2.806 s baseline → 0.329 s candidate, repeat query
      ~0.3 s. First-cache-build times (39.9 s candidate vs 60.6 s
      baseline) are NOT comparable because shared Nix caches were warm
      for different phases. Result fields and source reports are
      exactly equal for fzf and ripgrep.
