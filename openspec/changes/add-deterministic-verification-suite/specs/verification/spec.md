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

### Requirement: Fast lifecycle smoke tier

The system SHALL provide a per-push integration test that executes the real product
binaries through install, doctor, update, one package operation, and terminal uninstall
against a local TUF channel inside a container, completing within 15 minutes on
standard CI runners.

#### Scenario: Per-push lifecycle smoke

- **WHEN** a pull request changes product code
- **THEN** the smoke tier runs the reduced case list (~8 cases) against real binaries
- **AND** the tier completes within 15 minutes and fails the pull request on any case failure

#### Scenario: Reduced case list is a subset of the proof matrix

- **WHEN** the smoke case list is defined
- **THEN** every smoke case also exists in the staged-proof blocking matrix, so smoke failures
  predict proof failures and smoke coverage can never exceed proven behavior

### Requirement: Black-box CLI process tests

The system SHALL test the compiled CLI binary as a process: spawning the binary, asserting
exit codes per the documented mapping, stdout/stderr transcripts, JSON output shape, and
filesystem effects. Transcript snapshots SHALL pin help, error, and doctor output.

#### Scenario: Process-level transcript

- **WHEN** a CLI scenario runs
- **THEN** the test spawns the compiled binary and compares stdout, stderr, and exit code
  against a checked-in snapshot
- **AND** snapshot changes require deliberate review

### Requirement: Fuzzing of untrusted input surfaces

The journal file parser, the broker framing protocol decoder, and the TUF metadata JSON
parser SHALL have coverage-guided fuzz targets. Fuzzing SHALL run in short nightly bursts
with committed corpora, and any crash, hang, or sanitizer finding SHALL fail the nightly
lane.

#### Scenario: Nightly fuzz burst

- **WHEN** the nightly lane runs
- **THEN** each fuzz target executes for a bounded duration against its committed corpus
- **AND** a finding fails the lane and files the reproducer input as an artifact

### Requirement: Property-based operation sequences

The journal state machine SHALL be exercised by property-based tests that generate random
operation sequences (install, repair, upgrade, uninstall interleavings) and assert the
documented invariants after every step: idempotency, monotonic generation numbers, exact
receipt reuse, and fail-closed recovery from injected interruption.

#### Scenario: Generated sequence invariant check

- **WHEN** a generated operation sequence executes
- **THEN** the invariants hold after every step
- **AND** a failing sequence shrinks to a minimal reproducer recorded in the test output

### Requirement: Protocol compatibility matrix

The broker framing protocol SHALL carry a compatibility test matrix: an older client
binary against a newer broker, and a newer client against an older broker, built from
pinned protocol versions. Any combination the protocol claims to support SHALL pass the
existing framing contract tests; unsupported combinations SHALL fail closed with the
documented version error.

#### Scenario: Rolling upgrade safety

- **WHEN** the matrix runs for a protocol change
- **THEN** each supported old-new combination passes and each unsupported combination
  refuses with the version handshake error

### Requirement: Cross-platform receipt equality

Ownership receipts produced for the same release pair on different platforms SHALL be
byte-identical, and this equality SHALL be pinned by a test comparing receipts produced
in the Linux container and macOS environments.

#### Scenario: Same pair, same receipt bytes

- **WHEN** the same release pair installs on two platforms
- **THEN** the produced ownership receipts compare bit-identical

### Requirement: Journal schema migration compatibility

Golden journal files for each historical schema version SHALL be checked in, and the
product SHALL read, upgrade per the documented policy, or refuse each with a fail-closed
error. Refusal cases SHALL include journals from a newer schema version.

#### Scenario: Old journal upgrade

- **WHEN** the product opens a golden v-older journal
- **THEN** it upgrades per policy or reports the documented error

#### Scenario: Future journal refusal

- **WHEN** the product opens a journal whose schema version exceeds its own
- **THEN** it refuses fail-closed without mutation
