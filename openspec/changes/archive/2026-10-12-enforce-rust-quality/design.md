# Rust quality work

## Scope and completion

This run covers both Rust crates and their checks.
The baseline is commit `8c1a58513d4049a99cdf4d56d892c1de0286802b`.
The workspace has 83 Rust files and 29,786 lines, including tests.
These counts come from `rg --files crates tools/cask-catalog -g '*.rs'` and `wc -l`.

The run is complete when the audit is recorded, the strict lint gate passes,
confirmed priority repairs have regression tests, and the workspace checks pass.
The native CLI check must also pass on this host.
Checks for a different host remain explicit CI requirements.
Future work must have a clear priority and acceptance test.

## Work sequence

1. Capture formatting, lint, and test results from the unchanged baseline.
2. Review runtime, saved state, and importer code in separate read-only slices.
3. Record findings with file paths, triggers, effects, and repair priorities.
4. Enforce strict lints and remove the backlog in each crate.
5. Repair confirmed behavior defects with focused regression tests.
6. Run the full checks and review the final diff and decision trail.

Each repair unit ends with a check before the next unit starts.
Concurrent code writers use separate worktrees.
The parent reviews and integrates each change.
After integration, a worker can make a bounded repair in the main checkout.
The parent assigns exact file ownership before that repair.

## Data and module rules

Existing domain owners stay responsible for their data.
The profile and generation model belongs to Nix.
Catalog snapshots are disposable discovery data.
Public tap input is parsed and validated at the importer boundary.
Typed command errors retain the cause and cancellation state.

Prefer borrowed inputs and explicit conversions.
Use checked integer conversions at operating system and database boundaries.
Keep formatting changes mechanical.
Split long functions at existing domain operations.
Do not add wrappers only to meet a line limit.
Document the actual error conditions of public fallible functions.
Keep protocol case rules explicit when a lint suggests a different comparison.

## Lint policy

Use default `rustfmt` style with the pinned edition and Unix newlines.
Deny Clippy `all` and `pedantic` with lower group priority.
Select useful nursery and restriction lints individually.
Do not enable the complete restriction or nursery groups.
Require a reason for each exception.
Prefer `expect` for specific intentional findings so obsolete exceptions fail checks.
Tests can fail on invalid fixtures through `unwrap`, `expect`, and `panic`.
Tests retain all other production lint rules.

## Confirmed repair units

- Preserve caller and child signal masks through the supervised child lifetime.
- Preserve cancellation through optional lifecycle and info reads.
- Reject unknown configuration fields.
- Distinguish absent exact package paths from evaluation failures without parsing stderr.
- Require HTTP(S) URLs with a host at the cask pin boundary.
- Stage complete imported flakes on the output filesystem before rename publication.
- Execute macOS Cargo tests in CI.
- Repair the source scan claim and the actual canary cleanup assertion.
- Replace the yanked transitive package without relaxing dependency policy.

Exact package probes use pure `builtins.getFlake` evaluation.
Guarded attribute lookup returns a structured skip only for absent paths.
Errors inside present values remain errors.
Local path sources use their Nix store copy and its NAR hash.
Retain the native subdirectory, revision, revision count, and modification time.
Package expressions can depend on these flake metadata fields.
This avoids `--impure` and does not change forge source identity.

Flake staging was chosen over cleanup of a partially written final tree.
Temporary ownership makes failed-build cleanup automatic.
Rename publishes a complete tree on the same filesystem.
The importer retains its existing empty-output requirement.
It does not delete caller data.

## Checks

Run formatting, all-target build, workspace tests, strict all-target Clippy,
private-item documentation, dependency policy, and local documentation links.
Run the native CLI script against the built client.
Record the exact commands and outcomes in the audit.
Use a second reviewer to inspect the diff and the evidence trail.
