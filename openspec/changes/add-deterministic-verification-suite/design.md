# Design: Complete deterministic verification

## Context

The [active plan](../../../plans/determinate-nix-stacked-prs.md) is authoritative.
The original proposal mixed delivered work, future tests, and superseded clock
requirements. This document retains the current test boundaries.

## Decisions

### Time and isolation

Inject time only at the channel freshness decision. Preserve the existing
Clock/FixedClock seam and ambient-clock tripwire. Record-only journal, report,
and log timestamps remain ambient. Monotonic deadlines are not wall-clock
injection targets. Tests use isolated roots and explicit platform gates.

### Repeat proof

Repair and complete the existing `proof-repeat.yml`. It authenticates a sealed
pair and uses a VM-local loopback channel. It requires two independent macOS
slots, with prepare and resume separated by a real operator reboot. Network
access for authenticated inputs and necessary upstream packages is not claimed
to be absent. Retain exact input hashes and all four phase verdicts.

### Stable state

Define stable fields before writing golden files. Compare equivalent inputs on
the same platform. Record-only timestamps, host identities, and other excluded
fields need an explicit reviewed normalization rule. Platform-specific assets
and paths mean raw Linux and macOS receipts are not expected to be identical.
Compare only a documented shared contract across platforms.

### Fault and parser evidence

Inventory live product mutation boundaries and map each to a recovery test.
A text scan alone does not prove completeness. Reconcile the inventory with
actual callers and review new boundaries. Fuzz parser entry points with bounded
nightly jobs and retain reproducer inputs. Use generated operation sequences
for lifecycle state invariants; current value-type property tests are narrower.

### Existing tests and rollout

Extend the current CLI process tests and clean-host harness. Do not introduce a
new test library solely to match an old proposal. Measure CI duration before
adding a reduced smoke tier. Old/new protocol combinations need tests only for
an explicit supported contract; unsupported combinations must refuse safely.
Keep the two-week observation period for new assurance gates. Cancellation or
skipped required jobs do not count as a clean run.
