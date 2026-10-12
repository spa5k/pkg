## ADDED Requirements

### Requirement: Repeatable native proof

The existing repeat workflow SHALL authenticate exact sealed inputs and require
two complete independent macOS slots, each with prepare, real reboot, and resume.
Missing, cancelled, or skipped required phases SHALL prevent a passing verdict.

#### Scenario: Complete repeated proof

- **WHEN** both slots use the same authenticated pair
- **THEN** every required phase and lifecycle row passes with retained evidence
- **AND** each slot records the same VM identity and a changed boot identity

### Requirement: Decision-time isolation

Channel freshness tests SHALL use the existing injected clock. Record-only
timestamps and monotonic deadlines SHALL remain outside this clock requirement.
Tests SHALL use isolated roots and explicit platform predicates.

#### Scenario: Freshness boundary

- **WHEN** metadata is expired at the injected decision time
- **THEN** verification refuses without depending on the current wall clock

### Requirement: Stable-state fixtures

Repeated equivalent same-platform operations SHALL preserve the defined stable
state bytes. Exclusions and normalization SHALL be explicit and reviewed.
Different platform receipts SHALL be compared by a defined shared contract,
not assumed to have identical platform assets or paths.

#### Scenario: Stable-state comparison

- **WHEN** equivalent operations run in independent test roots
- **THEN** stable fields match reviewed fixtures and unexplained drift fails

### Requirement: Mutation and parser assurance

Live product mutation boundaries SHALL map to documented recovery tests.
Uncovered new boundaries SHALL fail the completeness check. Parser fuzz cases
and generated lifecycle sequences SHALL retain minimal failure inputs.

#### Scenario: New mutation boundary

- **WHEN** product code adds an uncovered mutation boundary
- **THEN** the inventory check fails until a reviewed recovery test covers it

### Requirement: Truthful assurance report

The report SHALL identify the exact source, inputs, executed checks, skips,
failures, and platform limits. Static test links SHALL NOT be reported as
executed line or branch coverage. Existing CLI and clean-host tests SHALL be
extended before a duplicate framework is introduced.

#### Scenario: Scheduled job cancelled

- **WHEN** cancellation prevents a required fault or real-Nix job
- **THEN** the report marks that evidence incomplete and does not count a clean day
