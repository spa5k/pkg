# Behavioral test audit

Date: 2026-10-12.
Repository: /Users/pacific/Developer/pkg.
Baseline: 8c1a58513d4049a99cdf4d56d892c1de0286802b.
Reviewed declarations: 35. Decisions: 32 retain, 3 repair. Consolidate: 0. Delete: 0.
All other declarations are unreviewed.

## Method and authority

For each row below, I read the complete declaration, its fixture setup, and the relevant production owner. I checked the current command or storage contract and CI execution path. The test filename and test's own label did not establish its value. A real subprocess can still be a protocol fixture. A filesystem test can be the only proof of atomicity or hostile-path refusal.

R means retain. F means retain its useful contract and repair the specified gap. C means consolidate after keeper proof. D means delete after pre-fix or mutation proof. U means unreviewed. No C or D decision is supported by the evidence in this audit.

The ledger gives a credible defect each declaration can detect. That is a reason to retain it, not a claim that a mutation campaign was run for every row. Only the no-shell row received an isolated production mutation. No shared checkout was mutated.

The test-audit skill has stale requirements for deleted engine tools, proof gates, and a stacked-PR plan. Current CONTRIBUTING.md and OpenSpec take precedence. The useful behavioral and keeper rules were applied. The removed tools were not reinstated.

## CI execution map

L = baseline .github/workflows/ci-fast.yml Linux rust job, cargo test --locked.
M = baseline macOS smoke job. All-target Clippy compiles test targets, but Cargo tests do not execute.
N = native-cli-check.py in the Linux/macOS smoke matrix with real Nix.
A = macOS archive/signature Python checks.

The root workspace contains pkg-cli and cask-catalog. It has no default-members restriction. L therefore runs both members' ordinary tests. Unix declarations run on Linux. Linux-only declarations run there. macOS-only declarations have no Cargo execution path in the baseline.

N sends SIGINT at native-cli-check.py:196. It runs normal and verbose install in a PTY, checks status 130 and unchanged profile entries, and retries. The earlier claim that CI has no actual signal coverage was wrong. N does not exercise the generic native helper mask or cancellation during rollback/prune history preflight.

The parent reports that it has added macOS workspace test execution. This audit did not execute that CI change.

## Ledger

All file references and line numbers below refer to the baseline. The prefix for crate sources is /Users/pacific/Developer/pkg/crates/pkg-cli/src/. The full test declarations were read.

### Cache publication and acceptance

Production owner: catalog/cache.rs::key, load, and store. Callers: local native and generated-index discovery. Dependencies: real local files, named temporary file, rename.
Keeper assessment: no inspected CLI journey forces all of these invalid schemas, hostile temporary names, and failed rename conditions. They are distinct cache contracts. Removing them would remove the selected negative proof. Retain the owner and its local tests.

| Declaration and baseline location | Actual contract | Credible defect detected | Decision and CI |
| --- | --- | --- | --- |
| snapshots_are_keyed_by_identity_not_revision_or_query, catalog/cache.rs:194 | Cache identity is stable across revision and query changes. Separate source/target identity stays separate. | Query-dependent keys create multiple incomplete snapshots or prevent later offline queries. | R; L. No stronger inspected keeper. |
| a_new_revision_replaces_the_one_latest_snapshot, catalog/cache.rs:224 | A new source revision replaces the latest snapshot at the same identity. | Old revision remains visible after a successful refresh. | R; L. The prior identity test does not prove replacement. |
| kind_separates_native_and_index_keys, catalog/cache.rs:241 | Native and generated-index lanes cannot overwrite each other. | Same source/target uses one collision-prone key for both schemas. | R; L. Unique lane-boundary proof. |
| invalid_files_are_ignored_not_fatal, catalog/cache.rs:251 | Invalid cache bytes are a miss. They do not stop command discovery. | A disposable corrupt cache becomes a fatal product error. | R; L. Valid fixture decoding is not a keeper. |
| old_schema_snapshots_are_ignored, catalog/cache.rs:263 | A prior cache schema is rejected as a miss. | An incompatible snapshot is accepted after a schema change. | R; L. Distinct from syntax corruption. |
| snapshots_without_full_provenance_are_rejected, catalog/cache.rs:272 | A snapshot missing required source provenance is unusable. | Offline discovery accepts rows whose source/revision is not known. | R; L. No CLI test inspected forces this missing-envelope case. |
| preexisting_temp_path_is_never_removed_or_clobbered, catalog/cache.rs:293 | Store owns only its own temporary file. A preexisting similarly named file remains intact. | Publication removes or overwrites someone else's temporary path. | R; L. Unique ownership proof. |
| preexisting_temp_symlink_is_not_followed, catalog/cache.rs:308 | Publication does not follow or alter a preexisting temporary symlink. | Cache refresh writes through a link into an unrelated file. | R; L. Regular-path test is not a link-proof keeper. |
| rename_failure_cleans_own_temp_only, catalog/cache.rs:337 | Failed publication removes its own temporary file and preserves unrelated paths. | Failed rename leaves debris or deletes a foreign file during cleanup. | R; L. Successful publication cannot prove failure cleanup. |

### Child classification and native invocation

Production owner: nix/runtime.rs, nix/process.rs, nix/mod.rs. Callers: runtime discovery, Nix profile commands, direct app helpers. Dependencies: actual child process, argv and pipes.
Keeper assessment: these declarations test execution adapters and output modes. The CLI tests prove command wiring. Neither layer proves every contract of the other. Synthetic exit 130 is useful classification proof but is not proof of actual signal delivery.

| Declaration and baseline location | Actual contract | Credible defect detected | Decision and CI |
| --- | --- | --- | --- |
| discover_captures_the_version_from_a_real_child, nix/runtime.rs:339 | A child supplies the runtime version; a removed executable preserves NotFound; failed probe retains status/stderr; exit 130 is Interrupted. | Runtime discovery hides the IO cause or treats failed/interrupted probes as a version. | R; L. Does not detect inherited signal masks because its fixture is a shell. |
| captured_and_streamed_children_classify_exactly, nix/runtime.rs:375 | Captured history returns stdout. Nonzero capture keeps failure details. Filtered profile execution returns the same terminal class. | One output mode reports success or loses argv/stderr after native failure. | R; L. Retain separately from runtime discovery. |
| direct_commands_share_the_classification, nix/runtime.rs:426 | Direct helper success/failure and captured output use the common classification. | Helpers use a second, inconsistent result convention. | R; L. Add a native signal-mask regression at this seam; this shell fixture is not its keeper. |

New defect-directed proof needed: a native helper must report INT/TERM/HUP mask bits and exit when the supervisor receives each signal. Save and check parent mask restoration and spawn-failure behavior. The parent reports that its direct-helper INT/TERM/HUP regression now passes. This reviewer verified the baseline failure only.

### Native publication and undo

Production owner: tap/store.rs::publish, finish, and undo; tap/atomic.rs. Callers: tap generation/import publication. Dependencies: real directories, exchange/rename, generation identities and held store ownership.
Keeper assessment: no inspected search or CLI journey provides the same filesystem topology and undo assertions. These tests do not duplicate string output tests.

| Declaration and baseline location | Actual contract | Credible defect detected | Decision and CI |
| --- | --- | --- | --- |
| first_publication_renames_and_undo_removes_it, tap/store.rs:736 | The first generated tree becomes current. Provenance/index travel with it. Undo removes only that publication. | Incomplete first generation is published, or undo targets another path. | R; L. Last selected first-publication proof. |
| update_exchanges_and_preserves_the_old_generation, tap/store.rs:785 | Update exchanges current with staged content, retains old content, cleans scratch, and undo restores the old generation. | Rename fallback loses the prior generation or undo discards it. | R; L. First-publication test does not prove exchange. |
| undo_targets_its_locked_store_and_keeps_other_roots_intact, tap/store.rs:861 | Undo remains tied to its original store when another state home has the same source name. | Global config or a source label redirects undo into a second store. | R; L. Unique origin-ownership proof in the inspected sample. |
| exchange_swaps_two_real_directories_atomically, tap/atomic.rs:137 | The native exchange swaps markers. Missing path, file, and symlink inputs are refused. | A non-atomic emulation or pathname bug loses data or exchanges an invalid object. | R; L. Publication tests depend on this adapter and do not replace every negative condition. |
| exchange_swaps_directories_with_non_utf8_bytes, tap/atomic.rs:173 | Linux exchange and directory sync support real non-UTF-8 Unix paths. | UTF-8 conversion corrupts or rejects a valid native pathname. | R; L; cfg Linux. UTF-8 publication tests are not a keeper. |

### Schema boundaries and retained causes

Production owner: nix/index model/decoder, nix/discovery.rs::MetadataValue, commands/error.rs and typed native/catalog errors. Callers: generated-index and metadata discovery, command dispatch.
Keeper assessment: independent backend fixture and downcast cause assertions detect failures that rendered CLI output alone cannot prove.

| Declaration and baseline location | Actual contract | Credible defect detected | Decision and CI |
| --- | --- | --- | --- |
| invalid_macos_ranges_fail_closed, nix/index/tests.rs:5 | Empty, inverted, invalid and out-of-range macOS bounds are refused. A coherent range is decoded. | Numeric cast or normalization accepts an impossible compatibility range. | R; L. Covers schema range policy; no extra generic numeric test is needed. |
| raw_provenance_accepts_the_backend_envelope_shape, nix/index/tests.rs:48 | Generated backend provenance envelope fields are accepted and retained, with the official lane shape distinct. | A strict decoder rejects intended extensibility or drops the backend identity. | R; L. Do not blindly apply deny_unknown_fields here. |
| decodes_the_actual_catalog_index_slice, nix/index/tests.rs:110 | An independently generated catalog fixture decodes metadata, source status, target kind and absent version. | A hand-authored Rust fixture conceals drift from actual generator output. | R; L. This fixture is a stronger schema keeper than builder-shaped unit data, but no deletion proof for other declarations was performed. |
| metadata_rows_require_the_payload_for_their_kind, nix/discovery.rs:146 | Tagged metadata rejects missing package/children payloads and unknown tags; valid children decode. | Defaulted or optional payloads turn malformed metadata into valid rows. | R; L. It proves the parser boundary, not the whole Nix traversal. |
| command_conversion_keeps_native_and_cache_causes, commands/error.rs:88 | Command/native/catalog/cache error chains retain downcastable IO causes. | Formatting during conversion replaces typed causes with strings. | R; L. A CLI stderr snapshot cannot prove downcasting. |

### App filesystem refusal

Production owner: apps/scan.rs, storage.rs, destination.rs. Callers: apps sync/status and post-mutation launcher refresh. Dependencies: actual files, ownership, modes, links and app markers.
Keeper assessment: the real native smoke journey exercises a normal profile. It does not create these hostile local layouts. Retain each selected local proof.

| Declaration and baseline location | Actual contract | Credible defect detected | Decision and CI |
| --- | --- | --- | --- |
| private_cask_payload_is_discovered_without_executing_its_helper, apps/scan.rs:83 | A private payload is read without running its helper. Invalid escaping source paths and legacy schema are refused. | Discovery executes vendor/helper code, follows an escaping path, or accepts an obsolete layout. | R; L. The deliberately nonexecutable helper makes accidental execution observable. |
| private_storage_rejects_shared_permissions_wrong_owner_and_links, apps/storage.rs:113 | Own private directory passes. Wrong owner, shared mode and symlink fail. | A writable shared or linked restoration root is accepted. | R; L. Distinct owner/mode/link security contract. |
| destination_inspection_separates_missing_owned_and_unowned, apps/destination.rs:195 | Missing, unowned and correctly marked destinations are distinct. The write guard refuses unowned state. | Sync overwrites a user's unrelated app directory. | R; L. Normal successful sync does not prove refusal. |

### Compiled CLI contracts

Production owner: main/cli/commands dispatch; search/source selection; lifecycle; info; app refresh. Fixture owner: crates/pkg-cli/tests/cli.rs. The compiled product binary runs with isolated home and controlled fake Nix. The tests prove argv, command ordering, output and exit-status integration. They do not prove the native Nix store implementation.

| Declaration and absolute baseline location | Actual contract | Credible defect detected | Decision and CI |
| --- | --- | --- | --- |
| invalid_search_pattern_is_a_usage_error_before_runtime_discovery, /Users/pacific/Developer/pkg/crates/pkg-cli/tests/cli.rs:163 | Invalid regex returns usage status 2 with no stdout even when no runtime can be found. | Runtime discovery occurs before argument validation and masks the usage error. | R; L. This ordering proof is distinct from parsing a Regex in isolation. |
| cold_search_gives_setup_guidance_without_a_runtime, /Users/pacific/Developer/pkg/crates/pkg-cli/tests/cli.rs:175 | Cold local search returns setup guidance without probing Nix. | Local discovery wrongly requires a runtime, or hides actionable cold-cache guidance. | R; L. Real Nix smoke cannot prove runtime independence. |
| cask_flows_read_the_generated_index, /Users/pacific/Developer/pkg/crates/pkg-cli/tests/cli.rs:558 | Commands use generated cask data; reject absent/excluded entries before add; retain local filtered rows after partial update; report unreadable installed state; preserve pre-install exit 130 without profile mutation. | Wiring queries the wrong lane, installs an excluded cask, or converts a failed gate into a mutation. | R; L. Large but coherent cross-command regression. No keeper was proven for all phases, so no consolidation or deletion. |
| install_filters_native_chatter_and_keeps_warnings_and_prompts, /Users/pacific/Developer/pkg/crates/pkg-cli/tests/cli.rs:1402 | Normal mode suppresses specified chatter and retains warnings/prompt text; verbose retains chatter. | Filtering removes meaningful diagnostics or verbose loses native details. | R; L. Final captured output does not prove prompt text is visible before a child waits for input. Do not claim that stronger contract. |
| removal_filters_ignored_errors_but_preserves_failure_causes, /Users/pacific/Developer/pkg/crates/pkg-cli/tests/cli.rs:1491 | Successful remove reports removed state and selected diagnostics. Failed remove keeps native failure and avoids a false Removed result. Verbose keeps ignored lines. | Filter policy swallows the decisive failure or prints success after native failure. | R; L. Distinct operation/failure proof; the install filtering test is not a keeper. |
| install_reports_partial_state_when_app_setup_fails, /Users/pacific/Developer/pkg/crates/pkg-cli/tests/cli.rs:1849 | A successful profile add followed by failing app setup reports partial state and status 1. It does not report plain Installed success. | Post-mutation app failure is presented as a successful install or hidden retry noise. | F; cfg macOS. M compiles it but baseline CI never runs it. Retain the declaration and add macOS execution. |

### Builder timeout and log bounds

Production owner: /Users/pacific/Developer/pkg/tools/cask-catalog/src/tap/runtime.rs::run_bounded_child and run_nix_export_with_limits. Callers: untrusted tap raw export and config gate. Dependencies: actual process groups and pipes, distinct log/wall-time policy.
Keeper assessment: these are real subprocess tests at the runner seam. Client runtime tests have different limits and do not replace them. Successful real Nix publication does not exercise hostile stdout volume or hung descendants.

| Declaration and absolute baseline location | Actual contract | Credible defect detected | Decision and CI |
| --- | --- | --- | --- |
| nix_timeout_kills_the_process_group_and_returns_promptly, /Users/pacific/Developer/pkg/tools/cask-catalog/src/tap/runtime.rs:769 | A 2-second deadline returns a wall-limit error within 30 seconds even when descendants hold pipes. | Killing only the leader leaves drain joins blocked. | F; L. Retain timeout proof. Repair the last canary assertion: it checks tempdir/x, not the actual canary path. No keeper proved for canary cleanup. |
| nix_log_flood_aborts_instead_of_truncating_into_success, /Users/pacific/Developer/pkg/tools/cask-catalog/src/tap/runtime.rs:801 | A child writing 4 MiB then exiting 0 still returns log-cap failure. | A short successful leader exits before polling sees overflow; truncated output is accepted. | R; L. Distinct from timeout and live-child overflow. |
| nix_log_flood_with_hung_descendant_aborts_within_seconds, /Users/pacific/Developer/pkg/tools/cask-catalog/src/tap/runtime.rs:827 | Overflow with a pipe-holding hung descendant fails with log-cap error within 5 seconds despite 120-second wall limit. | Overflow is checked only after EOF or only after the deadline. | R; L. Distinct from already-exited flood case. |

### Product source policy guard

Production owner: direct Command construction across product sources. Test owner: /Users/pacific/Developer/pkg/crates/pkg-cli/tests/no_shell.rs:84.

| Declaration | Actual contract and credible defect | Keeper and decision |
| --- | --- | --- |
| product_sources_never_spawn_a_shell | Detects only listed literal source spellings in selected roots. It can flag direct Command::new("sh") spelling. It does not establish the full claim in its name. | F; L. The isolated alias-to-/bin/sh mutation compiled, ran, and passed the unchanged guard. No stronger complete policy keeper is proven. Retain a narrow honest scan and add runtime argv-handling proof. |

Bounded repair recommendation:

- State the checked source surface and exact limitations. Include nix/formulae/lib if it belongs to the policy. Do not claim a source scan detects all shell execution.
- Add a direct execution regression that passes shell metacharacters and spaces as arguments to a native helper. Assert the helper receives each exact argument and that no shell side effect occurs. This protects the intended argv behavior at the production runner seam.
- Use the known alias mutation as a repair acceptance case if a stronger source-policy detector is introduced. A normal argument-preservation regression and a text scan together still cannot prove every arbitrary Command construction is safe.
- Do not add an AST dependency or a generic static-analysis framework solely to make the test's existing broad name true.

## Mutation and native probe evidence

### Native inherited-mask probe

Directory: /tmp/pkg-deep-quality-probe.
Source: exact baseline process.rs; no mutation.
Compiler: rustc 1.96.1 (31fca3adb 2026-06-26).
Platform: Darwin arm64.
Result: direct control INT=0 TERM=0 HUP=0 and SIGTERM exit within one second. Supervised child INT=1 TERM=1 HUP=1 and no exit after one second. SIGKILL to the recorded helper PID was required to reap the supervisor, which then returned Interrupted signal 15.
Artifacts: results.json, compile.log, helper.c, probe.rs, direct-control.ready, baseline-supervised.ready.
Terminal state: all probe processes reaped. No live terminal session handle remains.

This proves the baseline bug. It does not validate the parent's subsequent repair.

### Shell guard passing mutation

Directory: /tmp/pkg-deep-quality-shell-guard.
Copy roots: crates/pkg-cli/src, tools/cask-catalog/src, nix/casks/lib, and the unchanged no_shell.rs declaration.
Baseline test: 1 passed.
Mutation: run_direct_captured uses Path alias /bin/sh with arguments -c and printf injected.
Mutation test: 1 passed.
Mutated runner: compiled; actual output Ok((Success, "injected")).
Restoration: byte-for-byte original owner SHA-256 c2b8b882267b51ec0a0049af67b00f93992596a6572e911c636cfb66d1558b62.
Artifacts: results.json, compile.log, baseline.log, mutation.log, mutation-compile.log, mutation-execution.log.
Terminal state: no probe process remains.

The exact passing mutation supports F, not D. It demonstrates that the guard needs a narrower truthful claim or a stronger checked contract. It does not prove that the entire test has no value.

## Missing defect-directed contracts

The selected baseline tests do not prove:

- The actual INT/TERM/HUP mask and prompt termination of a native direct helper.
- Cancellation during rollback/prune history preflight prevents the mutation command.
- Cancellation during a post-prune history read preserves signal status.
- The macOS partial-state app failure declaration executes in CI.
- Arbitrary aliases to shell programs are caught by the no-shell source scan.
- An install prompt without a newline is visible before a helper waits for input.

The first four are immediate repair proofs. The parent reports the signal regression now passes and plans the history and macOS CI repairs. Prompt timing is a stated limit of the selected output test, not a reproduced product failure.

## Test seams and production design

No reviewed production branch was shown to exist only for tests. Command overrides and explicit runtime selection are useful product seams. The native APIs and filesystem owners are the right places for OS proofs. Keep fake Nix for command argv/order/output checks and real Nix smoke for profile semantics. Keep native helpers for signal-mask and argv proof.

Do not build a parallel fake profile engine. Do not unify cache lanes through the generic snapshot loader that implement-audit-repairs/design.md section 5 rejected. Do not replace useful real-filesystem tests with mocks.

## Unreviewed scope and validation limits

All other test declarations are U. This includes most CLI tests, unselected tap/store tests, app helper/assessment tests, the remaining nix/index tests, catalog search/local tests, and most cask catalog classifier/export/archive declarations. Workflow reads establish the execution path for the selected tests; they do not establish prior CI success.

This reviewer ran only the native signal probe, the no-shell baseline, and its isolated mutation/runner check. Full workspace tests, Clippy, native Nix smoke, macOS app sync, archive checks, and Miri were not run by this reviewer.

No test deletion is justified by this audit. No numeric test quality score is used.
