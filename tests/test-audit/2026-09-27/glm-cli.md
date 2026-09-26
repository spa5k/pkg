# Raw GLM review — verify against the main audit report

This is the model's original response. Some conclusions were corrected by the
parent's source checks and mutation experiments. See `review-corrections.md`.

# pkg-cli integration test audit — base `e96a7d0` (read-only, 10 tool calls)

## Common context (applies to every row)

- **CI/history.** Base is the alpha.55/56 installation merge. Integration tests run in CI as one `cargo test` per file. `cli.rs` spawns the real binary (`CARGO_BIN_EXE_pkg`), so each row is a process contract: exit code, stdout/stderr split, machine JSON shape. `e2e_fake.rs` is in-process over the `CoreEngine` + `CoreOperations` boundary with `pkg_testkit::FakeNix`.
- **Callers.** Outside `execute.rs` and test code, `CoreEngine`/`execute_command` is used only by `src/main.rs` and `src/commands/local.rs`. So the fake harness exercises the same dispatch seam the production connector plugs into.
- **Production seams read.** `CommandResult::new` rejects empty or private strings (`/nix/`, `.drv`, `github:`, platform triples; valid `PublicFlakeRef` allowed), reserved keys (`schemaVersion`, `ok`, `command`, `error`, `type`), private key substrings (`storepath`, `drvpath`, …), depth > 16, and results over 4 MiB. `CoreEngine::execute_with_progress` dispatches 14 product commands; bootstrap commands (doctor, system uninstall, completion, shellenv) refuse with `CONFIG`. `path.rs::resolve_state_location` reads home from the uid passwd entry, never `$HOME`; alternate roots must be absolute and never authorize broker-backed mutations. `DoctorReport::evaluate` builds ordered checks and derives `overall` fail-closed (unmanaged-nix states win over per-check failures).

## The 16 integration tests

| # | Test (location) | R – requirement | F – mechanism | C – actual assertion | D – disposition | Limitations |
|---|---|---|---|---|---|---|
| 1 | `help_exits_success_and_lists_the_product_commands` (cli.rs:10) | Top-level help works and names product verbs | Spawned `pkg --help` | Exit 0; stdout contains 6 verbs | Keeper (smoke) | Substring only; no layout or description check |
| 2 | `help_guides_common_tasks_and_short_flags_match_long_flags` (cli.rs:27) | Help guides tasks; short flags equal long flags | `-h`, `install --help`, `Cli::try_parse` | `-h` hides `--state`; install help shows flake example; `-vy` parses equal to long form | Keeper | Equality only for one flag pair |
| 3 | `clap_usage_failures_exit_two` (cli.rs:43) | Usage failures exit 2 with clean stdout | Three bad argv shapes | Code 2; stdout empty | Keeper | No stderr content check |
| 4 | `configuration_failure_obeys_json_and_jsonl_terminal_contracts` (cli.rs:56) | Config refusal honors machine terminal contract | `--state relative install` → CONFIG | Exit 78; exactly 1 stdout line; schemaVersion/ok/command/error.symbol; `type` absent for `--json`, `result` for `--jsonl` | Keeper (strong) | None material |
| 5 | `home_environment_is_not_a_state_identity_input` (cli.rs:82) | State identity ignores `$HOME` | Spoofed/removed HOME + relative `--state` | Exit 78 with exact message, both HOME variants | Keeper (security) | Only observes the relative-root refusal; production-root derivation under spoofed HOME is not seen here |
| 6 | `completion_is_real_static_source_and_doctor_reports_verified_host_state` (cli.rs:105) | Completion is real; doctor reports verified host state honestly | `completion bash`; doctor on temp state | Completion has `_pkg`. Doctor: healthy path asserts `runtime.managed`/`channel.signed` pass; unhealthy path accepts 74/78 and asserts both checks fail | **Required stronger boundary**: failure branch uses `.filter(id).all(fail)` — vacuously true when rows are missing | Mutation `doctor-missing-checks` survived this test (exit 0, 30.03 s); needs exact check-id set/count on the failure branch; 30 s runtime shows real broker timeouts |
| 7 | `doctor_support_is_preview_only_and_available_on_an_unhealthy_host` (cli.rs:163) | Support bundle works on unhealthy host; preview only, no secrets | `doctor --support` with `PKG_TEST_SECRET` | Exit 0; privacy flags; secret and state path absent from output | Keeper (privacy, output-level) | Only one planted secret; redaction of other host material rests on unit scope |
| 8 | `doctor_support_refuses_competing_machine_output_modes` (cli.rs:188) | `--support` refuses JSON/JSONL in any flag order | Four argv orders | Exit 2; USAGE symbol; command `doctor` | Keeper | None |
| 9 | `command_logging_records_only_the_command_not_package_arguments` (cli.rs:205) | Logs record the command, never package args/env | Real install to caller-owned temp state | Exit 78 at CONFIG first; `pkg.log` has `command_finished` + `install`, no package name, no `argv`, no `environment` | Keeper (privacy) | The 78 refusal is the claimed boundary itself, not masking; log-file path depends on tempdir under user home |
| 10 | `semantic_usage_failure_uses_the_selected_machine_format` (cli.rs:232) | Semantic usage failure follows selected format | `--json upgrade` without default | Exit 2; USAGE symbol in JSON | Keeper | Single case |
| 11 | `live_structured_uninstall_refuses_before_privilege_or_mutation` (cli.rs:246, **cfg linux / macOS-aarch64**) | Structured modes refuse live uninstall before privilege | `system uninstall --yes` with `--json`/`--jsonl` | Exit 78; exact message "live uninstall requires plain output" | Keeper (cfg-gated pair member) | Proves refusal ordering by message only; no direct proof that no broker call started. Not compiled on x86 Linux CI runners |
| 12 | `uninstall_refuses_an_unsupported_host_before_privilege` (cli.rs:263, cfg **not** linux/macOS) | Unsupported host refuses uninstall early | `--json --dry-run system uninstall` | Exit 78; exact unsupported-host message | Keeper (complement of #11) | Never runs on Linux/macOS CI; pair coverage depends on runner matrix |
| 13 | `shellenv_is_usable_before_state_exists_and_exports_the_package_path` (cli.rs:278) | `shellenv` works with no state; guards against double prepend | Eval snippet twice in `/bin/sh` | Success; exactly one `/current/bin` in PATH; suffix intact | Keeper (real idempotency) | Only `/bin/sh` |
| 14 | `every_command_has_examples_and_remains_discoverable` (cli.rs:298) | Home help hides advanced flags; every command has examples at any width | Clap grammar walk + 3 widths | Home lists all commands, no `--state`; each help has Examples, usage, no ANSI; typo suggests `install` | Keeper | Partly a change-detector, but it pins a user-facing contract |
| 15 | `package_uninstall_is_a_remove_alias_and_never_reaches_product_uninstall` (cli.rs:336) | `remove` aliases `uninstall`; bare `uninstall`/`system` fail; `system uninstall` help stays distinct | `try_parse` equality + spawns | Equal parse across 4 flag sets; exit 2 for bare forms; help shows both sudo and package forms | Keeper | Parse-level equality, not full dispatch equality |
| 16 | `every_command_routes_through_typed_fake_core_and_engine_calls_are_exact` (e2e_fake.rs:151) | Every command routes through the typed core boundary in exact order with correct policy | `FakeOperations` + `FakeNix`, real `CoreEngine` | Install asserts `policy.yes()`; gc asserts `dry_run`; engine-unavailable path via exhausted expectations; exact 14-call order; serialized fields contain no `/nix/store/`; `assert_exhausted` | Keeper for routing/order/exhaustion. **Privacy claim must stay with the lib keeper test** | Fake routing duplication: dispatch coverage exists three times (this plus two lib dispatch tests). The `/nix/store/` window scan is weak; mutation evidence below proves it |

## Fake seams, duplication, vacuous checks, masking

- **Fake routing duplication.** The 14-way dispatch table is asserted in `e2e_fake.rs`, `dispatches_every_engine_command_without_reparsing`, and `core_engine_routes_every_variant_and_preserves_global_policy`. Only #16 adds exact order, in-operation policy asserts, and harness exhaustion. Acceptable, but do not add a fourth.
- **Test-only seams.** Small and clean: `OperationPolicy::for_test` is `cfg(test)`; `resolve_state_location_from` is a pure, env-free core — a good seam; `write_success` is `cfg(test)`. `FakeNix` lives in `pkg_testkit`, not behind production conditionals. No mock theater found.
- **Vacuous assertion.** cli.rs:105 failure branch: `.filter(...).all(...)` over a possibly empty check array. This is the one real gap and the mutation run proved it.
- **Early refusals.** Tests 4, 5, 9, 11, 12 assert the refusal as the claimed behavior, with exact codes, symbols, and messages. None masks a weaker check.

## Independent assessment of the mutation evidence

- **`private-result-leak` (execute.rs).** Credible. With the `/nix/` rejection removed, the fake suite stayed green (exit 0) because `FakeOperations` results never carry store paths, so the window scan cannot see the loss. The lib keeper failed with exit 101 and a matching `FAILED` line. Restoration is confirmed green in `restored-controls.json`. Conclusion: the privacy boundary belongs to `public_result_rejects_private_runtime_material_and_reserved_fields`; #16 must not be cited as privacy proof.
- **`doctor-missing-checks` (doctor.rs).** Credible and consistent with my source read. The recorded 30.03 s run took the unhealthy path, where the vacuous `all()` passes with zero matching rows. On the healthy path, `.find(...).unwrap()` would have caught it, so the mutant survives only where the suite cannot control broker health. Restoration confirmed (exit 0, 30.04 s).
- **`renderer-*` and `skipped-linux-proof`.** Outside pkg-cli scope. Records show all three mutants survived their suites, with independent controls (expected digests, symlink probe, exit-code delta) present. Evidence is coherent; I did not rerun it.

## Remaining unreviewed unit scope

I read `execute/tests.rs` and the `doctor.rs::evaluate` and `path.rs` resolution sections only. Not reviewed: `path/tests.rs` beyond resolution, `broker/tests.rs`, the large `local.rs` module tests (real broker connector), `query.rs`, `human.rs` + `changes.rs`, `state.rs`, `uninstall.rs`, `doctor/probe` tests, `completion/tests.rs`, and `presentation`/`progress`/`ux` units.

## Recommended next batch (one coherent batch)

1. Fix cli.rs:105 first: on the unhealthy path assert the exact check-id set and count (`runtime.managed` and `channel.signed` both present, both fail), not a filtered `all()`. Re-verify against the `doctor-missing-checks` mutant.
2. Add boundary tests for the real connector in `commands/local.rs` — the only production `CoreOperations` caller — covering generation commit and rollback failure paths through `FakeNix`, in the style of `e2e_fake.rs`. This turns the strongest existing pattern (exact policy, exact order, exhaustion) into coverage of the code users actually run.

