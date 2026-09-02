## ADDED Requirements

### Requirement: Repeat-run lifecycle proof

The system SHALL provide a manually dispatched workflow that executes the full two-slot
lifecycle proof (fresh install, repeat no-op, offline repair, package state, offline N+1
upgrade, reboot, resume, package lifecycle, terminal uninstall) from pinned inputs in a clean
environment, and SHALL require zero live infrastructure beyond the pinned inputs and the
runners themselves.

#### Scenario: On-demand proof with pinned inputs

- **WHEN** an engineer dispatches the repeat-run proof workflow with a sealed pair SHA
- **THEN** the workflow fetches the pinned inputs by digest, runs both slots, and reports PASS
- **AND** no Quick Tunnel, local proof server, or developer-machine process is required

#### Scenario: Repeat verdict equality

- **WHEN** the same pinned inputs are proven twice on equivalent runners
- **THEN** both runs report the same slot-level verdicts with no manual intervention

### Requirement: Hermetic unit and integration tests

Every test in the workspace SHALL pass with no network access, an injected frozen clock, and
per-test isolated temporary roots. Tests whose verdict legitimately depends on the platform
SHALL be gated by an explicit platform predicate and reported as skipped, not failed, elsewhere.

#### Scenario: No network during tests

- **WHEN** the workspace test suite runs with networking disabled
- **THEN** every non-gated test still passes

#### Scenario: Frozen clock

- **WHEN** a test executes code that records timestamps
- **THEN** the observed time comes from the injected clock, never from the ambient system clock

#### Scenario: Platform-dependent verdict is explicit

- **WHEN** a test can only pass on specific platforms (for example
  `live_uninstall_accepts_only_plain_output` on x86_64 Linux)
- **THEN** it carries an explicit gate, and on other platforms reports as skipped with a reason

### Requirement: Byte-stable journals and receipts

Installing the same release pair N times, and repairing from equivalent prior states, SHALL
produce bit-identical journal files, ownership receipts, and channel metadata bytes. Golden
files SHALL pin the exact bytes for one representative release pair per platform.

#### Scenario: Repeated install byte equality

- **WHEN** the same release pair is installed twice into equivalent clean environments
- **THEN** journals, receipts, and channel metadata compare bit-identical

#### Scenario: Golden byte pinning

- **WHEN** the byte-stability suite runs in CI
- **THEN** produced bytes compare against checked-in golden files and any drift fails the build

### Requirement: Fault-injection coverage at every mutation boundary

Every filesystem mutation boundary in install, repair, upgrade, and uninstall paths SHALL have
at least one crash-injection test that proves the documented recovery behavior for that
boundary. The mapping of boundaries to tests SHALL be generated from the code and checked for
completeness, so new mutation boundaries cannot ship untested.

#### Scenario: Boundary inventory completeness

- **WHEN** the fault-injection matrix generator runs
- **THEN** it lists every mutation boundary in the product paths and the test covering each
- **AND** the check fails if any boundary lacks a covering test

#### Scenario: Crash at a boundary recovers as documented

- **WHEN** a crash is injected at any listed mutation boundary
- **THEN** the next run performs exactly the documented recovery (preserve started state, roll
  forward, or refuse retry) and no other state change

### Requirement: Determinism report

The system SHALL provide `tools/verify/determinism-report.sh` that runs the hermetic audit,
the byte-stability suite, and the fault-injection completeness check, and emits a single
verdict: DETERMINISTIC, or NON-DETERMINISTIC with a cause list.

#### Scenario: Single verdict

- **WHEN** the determinism report completes
- **THEN** it prints exactly one verdict line and a machine-readable summary to a results file
