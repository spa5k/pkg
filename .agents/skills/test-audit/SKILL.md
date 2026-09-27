---
name: test-audit
description: Audit or prune pkg tests for real behavioral coverage, redundant test layers, weak assertions, and test-only production seams. Use for a requested test audit or when designing a replacement for low-value tests.
---

# Test audit for pkg

Optimize for confidence in the shipped package manager. Test count and coverage
percentage are not goals. Prefer compiled CLI processes, real files and sockets,
real signed metadata, and native lifecycle journeys over mocked collaborators.
A unit test or a passing mock does not justify its own retention.

## Scope and authority

Read the root and scoped `AGENTS.md`, [CONTRIBUTING.md](../../../CONTRIBUTING.md),
and the [active plan](../../../plans/determinate-nix-stacked-prs.md).
Use the current source, CI routing, regression history, and retained native
proof. Pin the source SHA and record uncommitted changes separately.

Start with a read-only audit. A broad audit maps coverage and selects coherent
batches; it does not claim to have reviewed every test declaration. Mark unread
or unexecuted scope explicitly. For a complete subsystem pruning campaign,
read [CAMPAIGN.md](CAMPAIGN.md). Editing tests requires evidence for the
specific cutover. A request to audit does not by itself require a deletion PR.

## Value and retention

For each retained or new test, name the observable contract, a credible defect
that makes it fail, and why a stronger existing test does not already catch it.
Use one primary owner per contract. Another layer needs a distinct failure mode.

Prefer, in order of relevance to the contract:

- Native install, package use, upgrade, reboot, repair, rollback, and removal.
- A compiled CLI or service process with real filesystem, socket, and protocol
  behavior. Control external network responses without replacing the owner.
- Real cryptographic verification and parser, journal, or storage boundary tests
  with independent data, including adversarial and property-based cases.
- A narrow in-process test only when it catches a distinct defect that the
  stronger boundary cannot exercise reliably or at reasonable cost.

Do not discard the last proof of a security, crash-recovery, ownership, expiry,
protocol, or platform contract because it is called a unit test. Move its
assertion to a stronger owner first when practical. Deterministic clock and
fault inputs can be necessary; they must not implement the expected result.

Suspect tests include source-string greps, copied manifests, exact call lists,
mock-return echoes, assertions against untouched stores, self-comparisons,
expected values produced by the same implementation, repeated wrapper tests,
and test-only exports or reset hooks. Names do not establish value. Read the
whole test, parameter rows, production owner, non-test callers, overlaps,
relevant history, and CI routing. A source check may remain only if it enforces
an independent contract with no stronger practical guard. Static or slow alone
is not a reason to delete a test.

## Evidence before a cutover

Record one row per reviewed declaration (split parameter rows when decisions
differ): exact name and location; actual assertion and defect detected;
production owner and non-test callers; CI job; history; stronger keeper or
reason no proof is needed; test/support/production deletion unlocked; risk;
validation command; and evidence limits.

Use `R` to retain, `F` to repair the assertion, `C` to consolidate into a named
keeper, and `D` to delete. `U` means unreviewed, not safe to delete. `C` and `F`
are not deletion approvals until the keeper or repair is demonstrated.
Baseline failures are defects or environment failures to investigate. Never
remove a failing test just to obtain a green run.

For a false-green claim, make the smallest credible mutation in an isolated
copy, run the unchanged test, and record the result. For a keeper or repaired
regression, show a failing mutation or pre-fix control and a passing candidate.
Restore mutated source byte for byte. Separate executable evidence from source
inspection and from older platform reports.

## pkg validation and execution

Use Rust 1.96.1 from `rust-toolchain.toml` and the locked dependency graph.
Run the smallest owning Cargo test or Python unittest first. Use real binary
integration tests such as `crates/pkg-cli/tests/cli.rs` when they own the
contract. The `e2e_fake.rs` name does not make a mocked routing test end-to-end.

For source changes, apply the relevant gates in CONTRIBUTING: workspace
formatting, Clippy, rustdoc, build, deny, audit, and required platform tests.
For documentation, run `python3 .github/scripts/check_docs_links.py` and
`git diff --check`. Preserve CI routing and security gates during consolidation.
No replacement test is needed for a deletion that removes no distinct contract.

Run the [behavior regression guard](../../../tools/verify/TEST-CONTRACTS.md)
when its covered owners or tests change. Keep source mutations and their keeper
tests together. New false-green findings need a case in that guard or equivalent
native mutation evidence. Never accept a compile error, skipped test, or empty
test selection as proof that a regression is caught.

Keep the retained TUF harness under `spikes/s2-tough` in scope when auditing
channel security; security CI and fixture generation still use it. Current
behavior and release evidence live under `tests/`, not the removed plan archive.

When the user selects E2B, create a sandbox, sync the tracked working tree, and
record its exact source manifest. Copy new audit inputs separately because
sync excludes untracked files and Git history. Supply history from the pinned
public revision or a bounded local history extract. Inspect the configured Pi
model and report the exact model used; do not silently substitute models.
Use `e2b_pi` for model credentials. Do not read or print credential files.
Download reports and logs before stopping the sandbox. Always stop it when done.

E2B Linux execution proves only the boundaries actually run there. It does not
prove macOS launchd, APFS, Gatekeeper, reboot, or a full product install. Do not
run destructive clean-host harnesses on the developer host or relax their host
guards to run them in E2B. Native proof must use the documented disposable hosts.

## Report

Lead with concrete defects and the first safe pruning batch. Include retained
false positives, keeper contracts, baseline failures, mutations, CI gaps,
executed proof, and unreviewed scope. Count production, tests, and support
separately. State whether source was changed, and the commit/PR state.
