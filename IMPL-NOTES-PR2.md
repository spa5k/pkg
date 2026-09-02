# DN-1 PR-2 implementation notes (hermetic runner hardening)

## Verdicts from the CI spike (three iterations, runs 33674556472 → 33679126631)

1. **Userns spike (review M7/R17)**: GitHub ubuntu runners BLOCK unprivileged
   user namespaces (`unshare -rn` fails with `uid_map: Operation not
   permitted`). Fallback chain added: unprivileged userns first, then
   `sudo unshare -n` (root netns) with `setpriv` dropping back to the
   invoking uid so cargo execs normally and owns its target files. Final
   verdict: **G-HERMETIC green** — full workspace suite, network-denied,
   `PKG_HERMETIC` tripwire armed, `STRICT=1`.
2. **macOS sandbox-exec**: GitHub macos-15 runners ALSO refuse every
   sandbox-exec profile (probe log evidence). The probe lane runs unwrapped
   by design; **Linux is the sole authoritative network-denial lane**.
3. **Pre-existing Linux-only lint**: `redundant_pub_crate` on four
   `production_native_system` variants in pkg-pipeline (invisible on macOS,
   hidden since ci-fast was disabled during the DN-16 cleanup). Fixed to
   match the fifth variant.

## Fixes the spike forced (each verified by a failing run, then green)

- `CARGO_NET_OFFLINE=1` → `true` (cargo rejects `1` as a bool)
- `cargo fetch --locked` before going offline (fresh runners have empty caches)
- `ip link set lo up` inside every namespace (loopback binds)
- sudo path re-exports cargo's env conditionally (empty `CARGO_TARGET_DIR`
  is a hard cargo error)
- short TMPDIR (`/tmp/hm.XXXXXXXX`): macOS `SUN_LEN` is 104 bytes and
  broker socket paths extend TMPDIR
- macOS TMPDIR under `$HOME`: the journal safety checks reject
  world-writable ancestors (`/tmp` is 1777 — 27 test failures)
- best-effort cleanup trap: ownership tests leave restricted fixtures on
  purpose; a failing `rm` must never fail a green run

## Temp-root audit design

Production code must not use the ambient temp dir. Exemptions by
construction: inline `#[cfg(test)]` regions (awk per-file reset — a
state-leak bug was caught by probe-testing), sibling `tests.rs`/`tests/`
paths, pkg-testkit. Documented production baselines found by the audit
itself: the installer-bundle anonymous spool tempfile and provision
`TempPath` — await an explicit-root spool design decision.

## Workflows

- ci-fast re-enabled (was disabled during DN-16 environment cleanup — PRs
  #8/#9 had been running without it)
- `G-STATIC-AUDITS` (renamed): both audits, < 1 min
- `G-HERMETIC`: full run, STRICT, Linux — authoritative
- `G-HERMETIC-MACOS`: probe lane, log-evidence
- nightly: full hermetic run on schedule
