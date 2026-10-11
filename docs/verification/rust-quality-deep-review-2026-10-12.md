# Deep Rust quality review

Date: 2026-10-12.
Repository: /Users/pacific/Developer/pkg.
Baseline: 8c1a58513d4049a99cdf4d56d892c1de0286802b.
Reviewer: deep_quality_review.
Scope: read-only review of the repository. Native probes and one mutation ran in isolated /tmp copies.

## Result

Two high-priority faults remain in the baseline. A native child inherits blocked SIGINT, SIGTERM, and SIGHUP. Optional history reads discard cancellation before rollback or prune. The child-mask fault has a native reproduction. The history fault has direct source evidence.

The baseline also has a macOS test execution gap and a source-policy test that can pass after a real shell-spawn defect. The policy test has a passing mutation result. These need repair. No test deletion is recommended.

These are baseline findings. The parent agent owns the product repairs. This report does not verify the parent's later edits.

## Method and authority

I read AGENTS.md, CONTRIBUTING.md, CONTEXT.md, docs/architecture.md, and the project codebase-design, improve-codebase-architecture, test-audit, and research skills. I read codebase-design/DEEPENING.md. I used current OpenSpec designs for simplify-client, improve-rust-quality, and implement-audit-repairs. I checked the ADR status headers. ADRs 0001–0004 describe the superseded engine. ADR 0005's earlier quality ratchet is also superseded by the ordinary-check model.

I reviewed recent commit history and then read the production owner and complete declarations for 35 selected tests. I traced errors from child execution to command status. I inspected unsafe signal, atomic directory exchange, owner/permission checks, and publication ownership. I compared test compilation with actual CI execution. I used current primary Rust documentation through Context7, then official documentation and the exact Rust 1.96.1 standard-library source.

The repository was not changed by this reviewer. The parent authorized a source copy and probe fixtures under /tmp. The only production mutation was made in that copy. It was restored byte for byte.

TraceDecay structure scores are leads only. Its graph uses the main registry scope and counts non-Rust files. This report makes no quantitative quality claim from graph scores or file counts.

### Skill drift

The test-audit skill still refers to tools/verify/TEST-CONTRACTS.md, spikes/s2-tough, old native-engine evidence gates, and a stacked-PR implementation plan. Those subjects were deleted in the native-client design. CONTRIBUTING.md and the current OpenSpec changes explicitly replace them. I did not restore those requirements or treat their absence as a fault. The skill's behavioral audit, keeper-proof, and mutation rules remain useful.

The Rust skills were read from /Users/pacific/.codex/worktrees/strict-client-quality/pkg/.agents/skills because they were not present at the main checkout skill paths. Their generic advice is subordinate to project rules.

## Priority findings

### P1: Children inherit a blocked termination mask

Owner: crates/pkg-cli/src/nix/process.rs at baseline lines 166–178 and 342–380.
Callers: Nix captured/filtered/inherited execution and direct app-helper execution.

The parent blocks SIGINT, SIGTERM, and SIGHUP before Command::spawn. It registers the process group and then unblocks only the parent thread. The child keeps the blocked mask across exec. Forwarding a signal to that process group therefore does not stop a native child that relies on default signal handling.

The pinned Rust 1.96.1 implementation explicitly inherits the mask in both fork/exec and posix_spawn. It does not reset it. [Pinned Rust process implementation](https://raw.githubusercontent.com/rust-lang/rust/1.96.1/library/std/src/sys/process/unix/unix.rs).

The native probe ran on Darwin arm64 with rustc 1.96.1 (31fca3adb 2026-06-26). It used the exact git-show copy of process.rs. The helper was a small C program. It read its mask, wrote its PID and mask bits to a file, and waited in pause().

| Run | Child mask | SIGTERM result |
| --- | --- | --- |
| Direct native control | INT=0, TERM=0, HUP=0 | Exited within one second; status -15 |
| Baseline run_direct | INT=1, TERM=1, HUP=1 | Supervisor remained blocked after one second |
| Probe cleanup | Same blocked helper | SIGKILL to the helper PID released the supervisor; it then returned Interrupted with signal 15 |

The cleanup step is not a repair. It proves that the forwarding path could record interruption but could not terminate this child.

Evidence: /tmp/pkg-deep-quality-probe/results.json, compile.log, helper.c, probe.rs, process.rs, direct-control.ready, and baseline-supervised.ready. The logs preserve the helper PIDs. No running terminal session remains.

Minimal repeat procedure:

1. Copy process.rs from baseline with git show into a fresh /tmp directory.
2. Compile helper.c with the native C compiler.
3. Compile probe.rs with Rust 1.96.1 and the existing libc rlib. The probe calls process::run_direct with the helper path and its ready-file argument.
4. Start a direct helper control. Read the ready file. Send SIGTERM. Confirm prompt exit.
5. Start the Rust supervisor. Read the helper ready file. Send SIGTERM to the supervisor. Check its mask and wait result.
6. If it remains blocked, send SIGKILL to the recorded helper PID. Reap the supervisor.

Repair choices at concept level:

- Keep the spawn/registration race protection. Save the caller's prior mask in an owned guard. Restore the intended child mask in an async-signal-safe pre_exec hook. Restore the parent mask on every exit path.
- Or change registration so it does not carry a blocked mask across spawn. This still needs proof that an arriving signal cannot fall into the spawn/registration gap.

A pre_exec hook must not allocate, lock a mutex, or call unsafe post-fork library work. [CommandExt safety contract](https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html).

Required proof: native child mask, real INT/TERM/HUP delivery, spawn failure, parent-mask restoration, existing blocked caller-mask policy, captured and direct paths, and prompt reaping. Shell fixtures alone are not sufficient. Shells can alter masks during their own startup.

### P1: Optional history reads discard cancellation before mutation

Owner: crates/pkg-cli/src/commands/lifecycle.rs.
Baseline rollback starts at line 653. Its preflight profile_history().ok() is at line 663.
Baseline prune starts at line 729. Its preflight and postflight .ok() calls are at lines 736 and 748.

A profile-history command can return NixError::Interrupted. Converting that result to Option drops the distinction between an unavailable history and a canceled operation. Rollback can then call profile_rollback. Prune can then call profile_wipe_history. A canceled post-prune history read can also be reported as normal success.

The intended contract permits best-effort history when that read has an ordinary failure. It must preserve cancellation. The defect is independent of the child mask: a synthetic exit 130 or a cooperative child can still reach this path.

This is direct source evidence. I did not execute a rollback/prune mutation or alter a real profile. The parent will repair it. Required regression: fake history exits 130, mutation argv is never observed, and command status is 130. Also retain the ordinary history-unavailable behavior.

### P2: macOS-only Cargo tests compile but do not execute in baseline CI

Owner: .github/workflows/ci-fast.yml.
The Linux rust job runs cargo test --locked at line 50. The macOS smoke job runs all-target Clippy at line 115 and a release build at line 118. It does not run Cargo tests.

For example, crates/pkg-cli/tests/cli.rs:1849, install_reports_partial_state_when_app_setup_fails, is cfg(target_os = "macos"). It protects post-mutation partial state when app setup fails. Linux never builds that declaration. macOS Clippy compiles it but does not execute it.

Repair: run the workspace Cargo tests on the macOS smoke leg. Both workspace members are at the root with no default-members restriction. The parent has accepted this change.

Correction: tools/native-cli-check.py DOES send SIGINT at baseline line 196. It exercises normal and verbose install in a PTY, checks status 130 and unchanged profile entries, and retries with real Nix. An earlier claim that CI had no signal coverage was wrong. That real check does not prove a generic native helper's signal mask. It also does not cancel rollback/prune history preflight.

### P2: The no-shell policy guard passes after a real shell-spawn defect

Owner: crates/pkg-cli/tests/no_shell.rs:84.
Production mutation owner: crates/pkg-cli/src/nix/process.rs::run_direct_captured.

I compiled the unchanged baseline no_shell test in /tmp. It passed. I then replaced the direct helper command target in the isolated production copy with a Path alias to /bin/sh and argv "-c", "printf injected". The unchanged guard still passed. The mutated native probe compiled and returned Success with output "injected".

The guard matches a small list of literal source forms. It misses aliases. Its Python spelling checks also miss equivalent spacing, such as shell = True. Its scan roots omit nix/formulae/lib.

Evidence: /tmp/pkg-deep-quality-shell-guard/results.json, baseline.log, mutation.log, mutation-compile.log, mutation-execution.log, probe.rs. The original owner SHA-256 is c2b8b882267b51ec0a0049af67b00f93992596a6572e911c636cfb66d1558b62. The mutated owner was restored to that hash.

Decision: repair. Do not delete it without a stronger policy proof. It is useful as a narrow tripwire, but its name currently suggests complete protection that it does not provide. A replacement needs an explicit checked surface and the known alias mutation as a failing acceptance case. A source-text check must not claim to prove arbitrary Rust behavior.

### P2: Some info and app-helper paths still erase the cancellation category

Owners: commands/info.rs:152–188; apps/helper.rs::helper_runtime_compatible and run_sync_trampolines; app payload/storage/assessment helpers; commands/mod.rs::apps_refresh_after_mutation.

Info stringifies installed_entries errors and converts the result to Option. An interrupted installed-state query can print installed_state "unknown" and finish with a generic message error, rather than signal-derived status.

App-helper execution returns Outcome::Interrupted at the process seam, but several app helpers convert it to String. The command layer receives a generic error. After a native package mutation, this may correctly report partial state, but it loses the cause category and signal status.

This is a scoped typed-error gap. It is not a request to replace every String. Preserve typed cancellation until the command policy decides how to report partial state. Keep ordinary corrupt/unreadable installed-state diagnostics. I did not execute a whole app sync or info cancellation probe, so this finding has source evidence only.

### P3: One timeout test has an unrelated canary cleanup assertion

Owner: tools/cask-catalog/src/tap/runtime.rs::nix_timeout_kills_the_process_group_and_returns_promptly, line 769.

The test creates a canary with setup_canary(), drops it, then asserts that tempdir/x does not exist. That is not the canary path. The main error and elapsed-time assertions do protect the timeout behavior. The last assertion does not prove the cleanup stated by its comment.

Decision: retain the timeout proof and repair the cleanup assertion against the saved actual canary path. No deletion or mutation proof was needed for this source-confirmed mismatch.

## Rust rule choices and source checks

I read all nine Apollo chapters in rust-best-practices/references/chapter_01.md through chapter_09.md. I also read the detailed rust-skills rules listed below. I used them as review questions, not as an automatic rewrite list.

| Area | Read rules | Applied choice |
| --- | --- | --- |
| Ownership | own-borrow-over-clone, own-arc-shared, own-clone-explicit, own-copy-small, own-cow-conditional, own-rc-single-thread, own-refcell-interior | Immutable Rc-backed catalog state is correct for single-thread command ownership. Keep ordinary mutable borrows for TapState. Do not add Arc or RefCell without a real sharing need. |
| Errors | err-source-chain, err-context-chain, err-custom-type, err-expect-bugs-only | Preserve native IO causes and interruption until command policy. Manual Error implementations already serve this contract. Do not add anyhow or thiserror only to match generic guide style. |
| Unsafe | unsafe-safety-comment, unsafe-minimize-scope, unsafe-maybeuninit, unsafe-miri-ci | Review ABI, lifetime, signal-handler safety, mask restoration, process-group ownership, and pathname validity. Missing commentary alone is not proof of UB. Miri cannot replace native signal and filesystem evidence. |
| Numeric | num-cast-try-from, num-float-compare, num-nonzero, num-overflow-explicit, num-saturating-clamp | Check actual ranges and meanings. Keep safe clamping and explicit conversion at schema boundaries. I found no additional demonstrated overflow defect in the selected modules. |
| Serde | serde-deny-unknown-fields, serde-enum-representation, serde-try-from-validate | Reject config misspellings where the schema requires strict fields. Preserve deliberate extensibility in generated provenance envelopes. Metadata tagged payloads must match their kind. Do not add deny_unknown_fields to flatten-backed data. |
| Lints | lint-workspace-lints, lint-clippy-nursery-selected, lint-pedantic-selective | The user's strict workspace lint policy overrides the generic selective-pedantic advice. Exemptions need a precise reason. Lint success does not establish cancellation, atomicity, or CI execution. |
| Tests | test-fixture-raii, test-integration-dir, test-proptest-properties | Retain real filesystem/process contracts and source-independent schema fixtures. A coherent regression can have several assertions. Do not import the guide's test naming or one-assertion style as project policy. |

I did not claim every rule file in rust-skills was read. The selected rule set above was read in full. Chapter 09's Send/Sync table incorrectly labels mutable references as not Send. Official Rust documentation says mutable references are Send when the referred type is Send. I rejected that guide statement. [Official reference traits](https://doc.rust-lang.org/std/primitive.reference.html).

Research record:

| Source | Retrieval and use |
| --- | --- |
| Context7 /rust-lang/rust | Resolved from "Rust". High-reputation primary source and relevant standard-library snippets. Queries covered process execution/pre_exec/error causes and ownership/lints. The results did not resolve every platform-specific detail, so I used the pinned primary source next. |
| [Rust 1.96.1 Unix process source](https://raw.githubusercontent.com/rust-lang/rust/1.96.1/library/std/src/sys/process/unix/unix.rs) | Pinned behavior for fork and posix_spawn mask inheritance. This source supports the native reproduction, rather than substituting latest docs for the pinned compiler. |
| [CommandExt](https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html) | Current primary safety contract for post-fork pre_exec hooks and process groups. |
| [Rc](https://doc.rust-lang.org/std/rc/struct.Rc.html) and [Arc](https://doc.rust-lang.org/std/sync/struct.Arc.html) | Confirmed ownership and thread-sharing differences. No extra sharing abstraction is justified by this command architecture. |
| [References](https://doc.rust-lang.org/std/primitive.reference.html) | Corrected the guide's mutable-reference Send statement. |
| [Error](https://doc.rust-lang.org/std/error/trait.Error.html) | Checked the source-chain contract. Native error causes should stay available below the presentation layer. |
| [Rust operator expressions](https://doc.rust-lang.org/reference/expressions/operator-expr.html) | Checked narrowing and overflow semantics before evaluating range-handling code. Debug/release arithmetic differs unless policy is explicit. |
| [Serde container attributes](https://serde.rs/container-attrs.html) | Checked deny_unknown_fields and flatten limitations. Strict config and extensible generated provenance are different contracts. |
| [rustc lint levels](https://doc.rust-lang.org/rustc/lints/levels.html) and [Clippy usage](https://doc.rust-lang.org/nightly/clippy/usage.html) | Checked lint-group policy and execution. Compiling all test targets does not run those tests. |

Current documentation was checked on 2026-10-12. The execution probe used the repository's pinned 1.96.1 compiler. The report does not depend on an unpinned API addition.

## Architecture deepening candidates

These are conceptual cards for the parent to render. They do not propose an interface or implementation. The strongest card is child supervision. Repair the concrete faults before a broader redesign.

### 1. Own the full child lifetime and signal contract

Recommendation: strong.
Files: crates/pkg-cli/src/nix/process.rs; tools/cask-catalog/src/tap/runtime.rs; apps/helper.rs and its callers.
Dependency class: local external process and Unix signal API.

Problem: process execution has a small callable surface, but mask state, handler registration, process-group ownership, cancellation, drains, and reaping form one lifetime contract. The mask bug crosses those internal pieces. App callers also reinterpret interruption. The catalog builder has a separate runner with strict log and wall-time bounds.

Deletion test: keep the existing process module. Removing it would repeat substantial supervision work in Nix and helper callers. The builder runner also earns its place through sandbox/export limits. This is a deepening opportunity inside the owners, not evidence that either module should disappear.

Conceptual solution: one owner must hold the child, signal state, cleanup obligations, and terminal outcome through all success and failure exits. Keep the builder's bounded export policy and the client's metadata/direct-command policy distinct. Do not force these into a generic runner merely because both use Command.

Test seam benefit: native helper fixtures can prove the observable signal contract, timeout, overflow, and reaping without mocking Unix behavior. Unsafe reviews become local to the owner.

Domain/ADR constraints: preserve external Nix as the only runtime and Nix as profile authority. No old pkg engine, new service, or backend abstraction. No active ADR conflict.

### 2. Own package-change ordering and cancellation

Recommendation: strong, after the targeted rollback/prune repair.
Files: commands/lifecycle.rs; commands/mod.rs; profile_lock.rs; apps/mod.rs.
Dependency class: true external Nix plus real local profile and launcher state.

Problem: install, remove, upgrade, rollback, and prune coordinate similar obligations. They lock the profile, inspect state, run a native operation, inspect again, report partial results, and refresh launchers. The history .ok() calls show how a best-effort read can break cancellation ordering. Commands still need knowledge of several owners to report a truthful result.

Deletion test: the lifecycle module and native adapter each earn their place. Removing them would push the protocol into dispatch or duplicate it. Repeated operation-specific predicates are not automatically interchangeable.

Conceptual solution: concentrate the shared change protocol and its cancellation/partial-state rules in one owner. Keep per-operation selection and reporting policies explicit. This should reduce the amount of ordering knowledge a command needs.

Test seam benefit: compiled CLI fixtures can inject failures at preflight, native mutation, postflight, and launcher refresh. Real Nix smoke checks can continue to prove actual profile semantics.

Domain/ADR constraints: profile generation and rollback remain Nix concepts. Do not add a second transaction journal or recovery engine. No active ADR conflict.

### 3. Own the verified app-sync protocol and typed outcomes

Recommendation: worth exploring.
Files: apps/mod.rs, scan.rs, assessment.rs, payload.rs, storage.rs, destination.rs, helper.rs; commands/mod.rs.
Dependency class: real macOS filesystem and direct helper execution.

Problem: the current file split gives useful owners for source validation, private storage, destination ownership, helper compatibility, and payload copying. The coordination still depends on ordered checks and String-based failures. A command caller must understand which stage can create partial state and how interruption travels through it.

Deletion test: keep the focused filesystem adapters. Their rules are substantive and tested against real files. Consolidating all files into a large function would lose locality. Deepen the coordinator's contract instead.

Conceptual solution: keep verification and action under a single app-sync owner until it can return a truthful outcome. Preserve cancellation and original causes until command reporting. Keep marker ownership, symlink refusal, and clone/copy boundaries local to their adapters.

Test seam benefit: retain the small native filesystem proofs. Add or strengthen a whole-sync negative journey that proves refusal before writes and correct partial state after a failing helper. Do not delete the lower proofs without mutation evidence that the journey detects their defects.

Domain/ADR constraints: launchers and restored app copies remain derived, disposable state. Nix profile authority stays intact. No privileged app service. No active ADR conflict.

### 4. Own local discovery query outcomes without merging cache policies

Recommendation: worth exploring.
Files: commands/search/{mod,source,results}.rs; catalog/{local,search,cache}.rs; commands/info.rs.
Dependency class: in-process query plus real local SQLite and generated index files.

Problem: local discovery must select sources, collect rows, keep provenance, and distinguish a failed source from a failed output sink. The baseline suppression defect, already assigned to the parent, shows that these failure classes can leak across collection and presentation. Info independently handles installed-state errors.

Deletion test: source selection and storage adapters earn their place. Removing source.rs would move meaningful selection knowledge back to the caller. A small file is not proof of shallowness.

Conceptual solution: make query collection and outcome policy one clear owner. Keep source-specific parsing and cache acceptance separate. Preserve errors that prevent truthful output. This card needs the parent's source/sink repair as a first step.

Test seam benefit: query fixtures can prove selected-source behavior, damaged-source diagnostics, sink failure, provenance, and stable local-only operation. Keep real SQLite and filesystem paths in those tests.

Domain/ADR constraints: implement-audit-repairs/design.md section 5 explicitly rejects a generic snapshot-lane loader. Do not revive it. Native/formula/index policies differ. This candidate concerns query outcome policy only. A generic cache unification would conflict with the accepted design.

## Reviewed scope and limits

Fully read or read across complete functional owners: nix/process.rs; nix/runtime.rs execution owner and selected declarations; nix/discovery.rs; commands/error.rs; search/source.rs and search/mod.rs; catalog/cache.rs; nix/index/mod.rs; profile_lock.rs; apps scan, payload, storage, destination, and coordinator; tap/atomic.rs; the tap/store publication/undo owner; the catalog builder's bounded runner/export owner; baseline ci-fast.yml; tools/native-cli-check.py.

Partially read: commands/lifecycle.rs outside the selected install/upgrade/history owners; commands/mod.rs outside error/session/mutation-reporting paths; commands/info.rs; catalog/local.rs and catalog/search.rs; apps/assessment.rs and helper.rs tests; tap/store constructor/lock and other declarations; nix/index/model.rs; remaining catalog builder runtime tests; cask/formula workflow details.

The full declarations for the 35 ledger rows were read. Other tests are unreviewed. In particular, most CLI declarations, other tap publication tests, app-helper tests, and catalog classification/export/archive tests were not evaluated for deletion. No blanket test-suite quality claim follows from this sample.

Recent hotspots checked: typed errors and immutable catalog state in 5028885; native-client split in 2c78754; install/remove reporting in 8fc06b3 and 4bb554f; formula discovery and local search in ddf6f66 and 5bbfa34; bounded lifecycle/search fixes in 65674d9; tap publication/undo in d90e04f and 5a6ff0a; isolated Nix environment in 70ecffa and 87f340d. These informed risk selection, not line-count scores.

Execution performed by this reviewer: native mask control/probe; no_shell baseline; isolated no_shell mutation; compilation and execution of the mutated direct-command probe. This reviewer did not run full cargo test, Clippy, Miri, native-cli-check, a Nix build, or the macOS app journey. The parent is responsible for final repair validation.

At the final status check, the parent had uncommitted changes in CI, contributing/build/lint files, config, catalog search, process supervision, a new process submodule, verification reports, and the enforce-rust-quality OpenSpec change. This report remains anchored to the fixed baseline. It must not be read as an audit of those edits.

## Parent repair status, separate from this baseline review

The parent reports that the inherited-mask fault is repaired and its native direct-helper INT/TERM/HUP regression passes. This reviewer did not execute that final regression. The parent also owns history cancellation and macOS Cargo execution repairs. For the shell guard, use a bounded repair: direct native argv-handling regression, plus an explicit narrow source-scan contract. Do not claim complete shell-spawn prevention from literal source scans.
