# Rust quality audit on 12 October 2026

## Result and scope

The baseline is `8c1a58513d4049a99cdf4d56d892c1de0286802b` on `feat/rust-quality`.
The working tree was clean before this run.
The host is Apple silicon macOS with Rust and Cargo 1.96.1.
The workspace has two Rust crates, 83 Rust files, and 29,786 Rust lines at the baseline.
The line count includes tests.

The code has useful foundations.
Nix owns installed profiles and generations.
Discovery caches have explicit provenance and atomic replacement.
The importer validates public input and runs Ruby through the Nix sandbox.
Manual error types preserve native causes in many paths.
The code does not need a new package engine or a generic backend framework.

The main weaknesses are incomplete lint enforcement, inaccurate error contracts,
unsafe process lifetime details, and cancellation lost during best-effort reads.
The selected tests also have an execution gap and an overclaimed source scan.

All Rust targets received lint and build checks.
The deep review focused on recent changes and the runtime, state, app, and importer owners.
The [deep review](rust-quality-deep-review-2026-10-12.md) states which owners were read fully or partly.
The [test audit](rust-quality-test-audit-2026-10-12.md) covers 35 complete declarations.
It recommends 32 retains and three repairs.
It recommends no test deletion.
Other test declarations remain outside that behavioral audit.

## Baseline evidence

Formatting and the existing workspace tests passed before changes.
The broad diagnostic run used `-W clippy::pedantic -W clippy::nursery`.
It produced 553 diagnostic instances: 185 generator and 368 client instances.
There were 293 unique code/location/message combinations.
Targets repeat some diagnostics, so neither count is a count of product defects.
The most common category was missing public `# Errors` documentation.
Other findings included narrowing casts, long functions, implicit clones,
large stack arrays, pointer conversions, and unnecessary collection work.

The final gate uses full `all` and `pedantic` plus selected extra lints.
It does not enable the complete nursery or restriction groups.
Thus the broad baseline count and final gate are different measurements.
The [baseline probes](rust-quality-baseline-probes-2026-10-12.json)
preserve the native signal and source-guard results.

## Confirmed defects and repairs

| Priority | Trigger and effect at the baseline | Owner | Repair and proof |
| --- | --- | --- | --- |
| P1 | A direct child inherits blocked INT, TERM, and HUP. Forwarded signals cannot stop a child with default handlers. | `nix/process.rs` | Save the caller mask in an owned guard. Restore it in the child before exec. Restore the parent on all exits. A native baseline probe hung. The fixed test checks all three real signals, child masks, reaping, and spawn failure. |
| P1 | A canceled optional history read becomes `None`. Rollback or prune can still mutate the profile. | `commands/lifecycle.rs` | Preserve `NixError::Interrupted`. Compiled CLI fixtures check no mutation after preflight cancellation, post-prune status 130, and ordinary history failure. Rollback failed the new test before repair. |
| P2 | Config typos silently select defaults. | `config.rs` | Reject unknown root, source, display, and runtime fields. The new config test failed before repair and passes after it. |
| P2 | An existing package fails with text containing missing/attribute. Bare resolution hides that error as absence. | `catalog/search.rs`, `nix/discovery.rs` | Evaluate exact paths with `hasAttr` and `getAttr`. Missing paths become structured skips. Evaluation failures propagate. Preserve pure evaluation by pinning local store paths with a NAR hash. The failure test failed before repair. A real Nix fixture checks the same case. |
| P2 | `https://` passes prefix validation without a host. | `cask-catalog/src/lib.rs` | Parse with the existing URL library. Require HTTP(S) and a host. Both direct URL and pin-load regressions pass. The direct test failed before repair. |
| P2 | A late importer output failure leaves a partial final flake and blocks retry. | `cask-catalog/src/tap/mod.rs` | Convert in an output-local temporary directory. Publish by rename only after success. A real asset-write failure leaves output empty and permits retry. No deterministic failing-before product run was obtained with a valid empty output. This repair has source evidence and transaction tests. |
| P2 | macOS-only Cargo tests compile under Clippy but never run in CI. | `ci-fast.yml` | Execute workspace Cargo tests on the macOS leg. Local macOS tests pass. Remote CI execution is still required after publication. |
| P2 | The source guard passes an actual shell-spawn alias mutation. | `tests/no_shell.rs` | Keep an honest scan for common literal forms. Include Formula helpers and Python whitespace variants. Add a native argument-handling test at the process owner. The scan is not a complete static proof. |
| P2 | Info loses the cancellation category during its installed-state read. | `commands/info.rs` | Return typed interruption before output. A compiled CLI fixture checks status 130 and no completion output. Ordinary unavailable-state reporting remains. |
| P3 | The timeout test checks an unrelated cleanup path. | `cask-catalog/src/tap/runtime.rs` | Retain the real canary path and check it after cleanup. Retain the timeout proof. |
| P2 | Dependency policy rejects yanked `yoke-derive` 0.8.3. | `Cargo.lock` | Update that transitive package to 0.8.4. Keep the policy unchanged. |

## Lint and code quality changes

The workspace now denies full Clippy `all` and `pedantic`.
Selected extra lints check redundant clones, eager fallback calls, unnecessary collections,
large stack arrays, and use of `Self`.
Rustdoc rejects broken links and invalid HTML.
Unfulfilled lint expectations fail the build.
`just lint`, `just check`, and CI use the same warning policy and target scope.
`just check` builds all targets, runs both crates, and checks private-item documentation.

Cleanup uses borrowed inputs, checked numeric conversions, explicit pointer creation,
smaller importer operations, and accurate public error contracts.
It does not add new error libraries or shared process abstractions.
The separate comment audit caught generic error text that satisfied lint syntax but was false.
Those paragraphs were corrected against the function bodies.

Five new local lint expectations remain.
Two preserve exact `.app` and `.drv` protocol suffixes.
Three retain coherent integration journeys with shared fixtures and ordered state assertions.
Each names its reason.
There are no new correctness or safety lint suppressions.
Existing fixture-only unwrap, expect, and panic exceptions remain.

## Overall plan

The first phase is implemented in [Enforce Rust quality](../../openspec/changes/enforce-rust-quality/proposal.md).
The [tasks](../../openspec/changes/enforce-rust-quality/tasks.md) record completion.
The [decision trail](rust-quality-decisions-2026-10-12.tsv) records choices and evidence.

Further work must use small verifiable changes.
Do not replace the native profile model.

| Order | Work | Acceptance test |
| --- | --- | --- |
| 1 | Deepen child supervision around its full lifetime. Review the rare interval between reaping and clearing the registered PID. | A native process stress test proves no signal reaches a replacement process. Preserve all mask, cancellation, resource, and reap tests. The current PID race is a source risk, not a reproduced fault. |
| 2 | Preserve typed interruption through macOS helper and app-sync outcomes. | Cancel at helper probing, payload copying, and launcher refresh. Verify signal status, truthful partial state, and no unowned destination changes. Keep real filesystem tests. |
| 3 | Concentrate shared package-change ordering in one module. | Compiled CLI journeys prove lock ownership and preflight, mutation, postflight, and launcher failure outcomes. Keep operation-specific policies explicit. Do not add a second transaction journal. |
| 4 | Reduce invalid public state construction. Review raw catalog IDs and saved-catalog map/source consistency. | Invalid values fail at construction. Valid IDs and source bindings round-trip. First prove callers need the narrower interface. |
| 5 | Improve test locality as affected areas change. Separate unrelated assertions in the longest CLI journeys through real fixture owners. | Preserve catalog reuse, output, conflict, and mutation-order contracts. Prove any deletion with an exact keeper. Do not build a fake profile engine. |
| 6 | Improve discovery outcome ownership. Remove remaining diagnostic-text absence checks from snapshot root traversal. | Missing output roots remain normal. Real package errors stay visible. Local searches make no runtime call after preparation. Preserve distinct cache policies. |

The strongest architecture candidate is child lifetime ownership.
The signal mask repair is its first concrete step.
The deep review has four candidate cards and a deletion test for each.
App-sync and discovery modules earn their place through real domain work.
File size alone is not a reason to remove them.

## Standards and skills

The [Rust Style Guide](https://doc.rust-lang.org/style-guide/) defines format policy.
The [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html)
define API review questions.
The [Clippy documentation](https://github.com/rust-lang/rust-clippy/blob/master/book/src/lints.md)
supports lint-group choices.
The deep report records primary-source research and one incorrect generic guide claim that was rejected.

This run applies pstack figure-it-out, principles, no-comments, and decision-trail review.
It applies the project code-review, codebase-design, improve-codebase-architecture,
domain-modeling, implement, research, and test-audit skills.
It applies Rust best practices and the relevant Rust skills rule files.
Project rules take precedence over generic guide advice.
The stale engine and test-framework references in the test-audit skill were not restored.

## Verification

Executed on Apple silicon macOS with Rust 1.96.1:

| Check | Result |
| --- | --- |
| `just check` | Pass. Formatting, strict all-target/all-feature workspace Clippy, build, tests, and private-item rustdoc all pass with the updated lockfile. |
| Workspace tests | Pass. Generator: 121 library, one binary, one integration test. Client: 126 library, 22 CLI, four cancellation, and one source-scan test. Zero doctest failures. |
| `cargo deny check` with cargo-deny 0.20.2 | Pass. Three existing duplicate-version warnings remain permitted by the documented dependency policy. No yanked package or policy exception remains. |
| `sh -n install.sh` and `shellcheck install.sh` | Pass. |
| `python3 -m unittest discover -s docs/tests -v` | Pass. 44 tests ran. Seven Linux isolation tests were skipped on macOS. |
| `python3 .github/scripts/check_docs_links.py` | Pass. |
| Release build and `tools/native-cli-check.py --pkg target/release/pkg` | Pass. Disposable profiles exercised real Nix lifecycle, INT cancellation, discovery, app failure, and the new exact-evaluation fault. |
| `git diff --check` | Pass. |

The broad nursery diagnostic baseline was not repeated as a release gate.
Full Linux execution and remote CI remain unverified on this host.
The macOS CI test step is configured but has not run remotely from these local changes.
No push, PR, merge, or release was performed.

Separate standards, spec, and decision-trail reviews follow the implementation commit.

