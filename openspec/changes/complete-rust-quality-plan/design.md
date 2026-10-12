# Design for the remaining Rust quality work

## Frame

Baseline: `df1af1d` on `feat/rust-quality`.
The working tree is clean at the start.
There are six work units across the client crate and its verification code.
Process ownership and cancellation have the highest risk.
The current child lifetime race is a source risk. It has not been reproduced as PID reuse.

Done means all six units have implemented outcomes and passing acceptance checks.
It also means strict workspace checks and real Nix verification pass on this host.
Separate Standards and Spec reviews must have no required corrections.
A different model must review the decision trail.
Host-specific execution limits must be stated.
Do not end the run after one unit.

## Workflow

1. Trace process, app/command, and catalog/discovery/test owners in parallel.
2. Compare complete design candidates before choosing interfaces.
3. Capture focused pre-change failures where the defect can be reproduced.
4. Implement process lifetime and app cancellation in separate worktrees.
5. Integrate each unit after focused tests and strict lint checks pass.
6. Implement shared command ordering after typed app outcomes are available.
7. Implement validated public state and structured discovery outcomes.
8. Improve CLI test locality against a recorded coverage map.
9. Run the full workspace checks and native CLI journeys.
10. Run comment, Standards, Spec, and cross-model decision-trail reviews.
11. Correct findings, rerun affected checks, commit on the current branch, and record completion.

The throughput limit is three worker slots.
Each concurrent writer has its own worktree and exact file ownership.
The main agent owns integration, shared policy, reports, and final verification.
Local unit checks run before integration. The full test suite runs at the end.
The decision trail is append-only.

## Acceptance checks

| Unit | Required observable result |
| --- | --- |
| Child lifetime | Native tests send real INT, TERM, and HUP across spawn, running, exit, and cleanup. The supervisor never signals a child after it relinquishes ownership. Unrelated sentinel processes survive the stress test. Existing mask, spawn failure, bounded output, timeout, and reaping contracts stay covered. |
| App cancellation | Compiled CLI tests cancel helper probing, payload preparation, and launcher refresh. Each keeps the signal exit status, gives truthful partial-state output, and leaves unowned destinations unchanged. Real filesystem tests retain marker and source-map recovery. |
| Command ordering | Compiled CLI tests prove one lock owner and ordered preflight, native mutation, postflight, and app refresh. A failed or canceled stage stops later mutation and app refresh. Read-only reconciliation may report partial native work. Each command keeps its own mutation and reporting policy. |
| Public state | Invalid catalog IDs fail construction or decoding. Saved catalog keys must match their source identity. Valid IDs and source bindings round-trip. Callers use the checked interface. |
| Test locality | Record old assertions and their new owners. Separate unrelated journeys with reusable real fixtures. Preserve cache reuse, output, conflict, cancellation, lock, and mutation-order contracts. A deleted test needs an exact keeper proof. |
| Discovery | Missing output roots yield normal empty results. Existing values that throw evaluation errors return those errors. Searches after preparation make no runtime call. Snapshot and metadata cache policies remain distinct. |

## Principles used

Model the Domain changes loose outcomes into owned process, app error, and catalog state.
Boundary Discipline puts source and ID validation at construction and decoding.
Separate Before Serializing Shared State gives each writer an isolated worktree.
Sequence Work into Verifiable Units requires a passing check before integration.
Prove It Works requires native process and Nix execution after local tests.
Laziness Protocol keeps the selected interface small and removes duplicate ordering.

## Design selection

Four designs were read before implementation. Three were independent.
The fourth used the parent model because the configured model was unavailable.
It also received a late comparison note, so it is auxiliary evidence.
An independent Luna judge selected candidate 1 with 22 points.
Candidate 2 received 21 points. Candidate 3 received 13 points.
The parent agrees with the selected ownership shape.

Use one synchronous child owner. Signal handlers record cancellation only.
Observe exit without reaping. Finish bounded-group cleanup before consuming wait.
Only bounded children get a dedicated process group. Other children retain terminal access.
Keep the existing typed interruption outcome and internal I/O error boundary.
Do not add a general process error taxonomy or a supervisor executable.

Use a small typed app error at the existing owners.
The exact pinned helper deletes and recreates its output folder.
It also requires a directory symlink for its source view.
Run it against private staged output and an explicit source-view symlink.
Check staged output and destination ownership before publication.
Keep the prior launcher folder and source map if helper work fails or is canceled.
Report failed publication or recovery. Do not claim rollback of Dock changes.

Use a closed package-change request with one profile-lock owner.
Keep command policy explicit. Pass the held lock to app refresh.
Do not add callback stages, a backend framework, or a package-state journal.

Make catalog IDs and saved bindings checked at construction and decode.
Derive saved map keys from checked values. Reject duplicates and mismatched keys.
Keep the existing index model and strict decoder. Close unchecked routes without a broad redesign.

Use structured root and system membership for discovery.
Keep throwing and malformed present values as errors.
If path-flake metadata omits its store path, materialize the source with the native flake archive command.
Hash the archived path and preserve native subdirectory and revision fields.
Do not substitute the original mutable source path or add a fixed store-root policy.
Move test setup to real fixture owners and record every assertion keeper before deletion.

The native macOS WNOWAIT probe passed. It proves non-consuming exit observation only.
Native product tests cover real signals, group descendants, output limits, and unrelated sentinels.
They do not force PID reuse.
Darwin reports EPERM for a group that contains only the exited, unreaped leader.
A private member check accepts that case only when it finds no live members.
Truncated or unreadable member data returns a cleanup error.
On normal exit, cleanup uncertainty is returned as the run error after reaping.
After an earlier run error, Drop attempts cleanup and keeps the original error.
The owner disables all further signaling before any consuming wait attempt.
It reaps the direct child even when group cleanup fails.
A descendant with changed permissions can survive a denied group signal.
Its process group keeps its own identity reserved until it disappears.
The published XNU source used for the boundary check is older than the host kernel.
