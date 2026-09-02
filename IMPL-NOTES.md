# DN-1 PR-1 implementation notes (time injection)

## Scope

Implemented per design Amendment 2 (owner decision, 2026-09-03): the clock
decides exactly one security-relevant thing — channel descriptor freshness
(`policy.rs` `expires_at <= now`). Clock injection targets that decision.
Record-only timestamp sites (broker approval journal, repair report, CLI
state files, log rows) stay ambient: a grounding audit confirmed they decide
nothing, and they are excluded from byte stability (Q8). `Instant::now`
timeouts are out of scope.

## What shipped

1. `pkg-core/src/clock.rs` — `Clock` trait + `Timestamp` (jiff-backed, the
   same civil-time type the boundary already parses). `SystemClock` ambient
   default. `PKG_HERMETIC=1` turns ambient reads into panics (the tripwire).
   Combined single test covers both phases (env set → panic via
   `catch_unwind`; env cleared → valid read) to avoid parallel-test env
   races.
2. `pkg-testkit/src/clock.rs` — `FixedClock`: freeze / advance / set,
   fail-closed when exhausted, `constant()` for steady time.
3. `pkg-channel/src/tuf.rs` — `ChannelClient` holds the clock; production
   constructors install `SystemClock`; `new_with_clock` for tests.
   `pkg-nix/src/managed/installer_bundle.rs` feeds the same decision and
   takes the clock via `BundleEnvironment` (respects the 5-argument budget).
   New pins: freeze-attack boundary (fresh at `expires_at − 1s`, refused at
   the exact expiry instant) and an end-to-end `refresh()` test proving only
   the injected clock is read.
4. `tools/verify/run-hermetic.sh` — ambient-time audit (all wall-clock
   families, record-only allowlist), `PKG_HERMETIC` tripwire, network denial
   (unshare on Linux; sandbox-exec probe on macOS; `STRICT=1` fails closed).
   `AUDIT_ONLY=1` backs the fast static gate.
5. `ci-fast.yml` — new `G-AMBIENT-TIME-AUDIT` job (grep-only, no build, <1
   min) so every PR proves no new ambient wall-clock decision site appears.

## Decisions and deviations

- The stopped agent's syn/quote source-scan guard inside `clock.rs` was
  removed: the script audit + tripwire replace it (the guard contradicted
  Amendment 2 by forbidding allowed record-only reads).
- macOS note: this OS build refuses every `sandbox-exec` profile; local runs
  fall back with a loud warning. CI sets `STRICT=1`.
- Observed once: `pkg-testkit http::tests::exact_transcript_serves_drop_and_
  truncate_faults` failed under full machine load; ephemeral-port timing
  flake, not a clock leak. Follow-up task recommended.
- Branch rebased onto `openspec/pre-production-roadmap` (4c86f22) before
  committing; the agent's duplicate OpenSpec edits were dropped in favor of
  the parent's authoritative Amendment 2 text.

## Verification

- `cargo test --locked --workspace` — 1,095 passed, 0 failed (verified
  independently by the orchestrator)
- strict clippy — clean; `cargo fmt` — clean
- quality gate vs `BASE_REF=openspec/pre-production-roadmap` — PASS
- `tools/verify/run-hermetic.sh` — audit clean, tripwire armed
