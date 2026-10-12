# Comment cleanup and plan archive

This follow-up changes the Rust quality branch after `d8c2f1c`.
It follows the pstack `no-comments` workflow and the Rust guides.

## Comment review

All 69 accepted spans, containing 152 comment lines, were removed.
The final review also found and removed one orphan line.
The Rust and Cargo diff removes 183 comment lines and adds 13.
The net reduction is 170 lines across 26 files.
The documentation checker also loses one redundant comment.

Required API contracts and proven external constraints remain.
These include the Dock side effect, Darwin wait behavior, daemon sandbox
restrictions, post-fork safety, and process identity ownership.
The Nix configuration note now distinguishes restricted daemon overrides
from all client options. Local configuration cannot prove daemon enforcement.

Seven integration test crates use a local `missing_docs` expectation.
Its reason is that those crates have no public API.
Workspace lint levels remain unchanged. Unfulfilled expectations are denied.
The existing eight scoped Clippy allowances remain intentional test or
case-sensitive matching policy. No safety suppression was added.

No comments were restored. No rejected review was rerun.
There were no constraint encoding offers, approved encodings, or unenforced
constraints. Both accepted structural findings are fixed.

## Selected design

Four independent sketches were compared. A separate Luna judge selected
typed app health and one private upgrade policy.
The implementation uses that shape. It adds no general stage framework.
A separate Luna review found no concrete semantic fault in the final change.

`apps::status` returns typed health and a diagnostic message.
Doctor reads health without parsing text. One constructor retains the
existing diagnostic prefix for JSON and verbose output.
Operational errors still become unreadable observations. Signals still stop
doctor and retain their exit status.

`UpgradePolicy` owns selection, registry loading, removed-source handling,
fixed-reference filtering, and eligibility order.
Formula eligibility runs before Cask eligibility against installed sources.
The existing profile lock stays in the command owner.
`Prepared::Upgrade` holds checked nonempty targets. An empty selection cannot
reach native Nix, where it would mean an upgrade of all entries.

## Signal fixture repair

The first PR CI run exposed a timed signal race on macOS.
The old fixture failed locally on repetition 37 of 100.
Its signal could arrive after supervision returned.

The fixture now raises the signal from the existing stderr completion
callback. On this path, the direct child has been reaped, and bounded output
processing has ended. The signal guard is still active.
An inherited pipe holder remains alive during classification.
The outer test owns its process group until group cleanup and leader reap.
The repaired fixture passed 100 repetitions on macOS for INT, TERM, and HUP.
A separate Luna review found no process ownership flaw.
The callback contract also describes error paths, where cleanup can follow.

## OpenSpec archive

Seventeen old changes moved to the [archive](../../openspec/changes/archive/README.md).
Fifteen were complete. Two were superseded and were not delivered.
Their 38 unchecked tasks remain unchecked.
All 78 archived files are present. All 17 task files retain their completion
marks. Non-Markdown metadata is unchanged.

Archival used OpenSpec 1.11.0 with `--skip-specs --yes`.
Validation was enabled. Obsolete delta specs were not applied.
The two plans in [PR #115](https://github.com/spa5k/pkg/pull/115) remain active
until merge. Incoming links and links within moved plans now resolve to the
archive. Both HTML plan files also have valid OpenSpec links.

## Verification

On macOS arm64 with Rust 1.96.1, `just check` passed:

- Formatting and all-target, all-feature Clippy with warnings denied.
- Locked all-target workspace build and all workspace tests.
- Documentation for private items with warnings denied.

The library suites passed 170 client tests and 123 generator tests.
Four native discovery tests remain explicitly ignored in the ordinary suite;
their prior native-host proof is recorded in the
[completion report](rust-quality-completion-2026-10-12.md).

Four new behavioral tests verify stable healthy and drifted doctor output,
changed-source precedence, fixed-only upgrades without mutation, malformed
registry failure order, and installed-source eligibility after a configuration
change. Existing lock, cancellation, removed-tap, and publication checks pass.

All 15 documentation-checker tests pass. The Markdown link checker passes.
All 21 OpenSpec links in the two HTML plans resolve.
The release client builds successfully.
Current remote checks are attached to PR #115.

No accepted cleanup finding or required implementation remains open.
