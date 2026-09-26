# Raw GLM review — verify against the main audit report

This is the model's original response. Some conclusions were corrected by the
parent's source checks and mutation experiments. See `review-corrections.md`.

# Bounded package-test audit — release assurance

Source base: `e96a7d06b01e0205e82f51f112a8951ae9324830` (HEAD, merge of PR #64). Model: GLM 5.3 Flash (zai). No files changed. Seven tool calls used.

## Reviewed / unreviewed scope

**Fully reviewed:** `tools/release/test_workflow.py` (all 8 declarations), `.github/workflows/release.yml` (all 4 jobs, triggers, permissions), `/tmp/pkg-audit/mutations/results.json` (all 5 entries), first 200 lines of `tests/linux-clean-host/run.sh`, and ~95% of `tests/linux-clean-host/test_untraced_vendor_replay.py` (last ~21 lines unread). Git history checked once (8 commits for these paths).

**Not reviewed:** `run.sh` lines ~201–1214, the tail of declaration 7's 46-name filter list (only its start and the `len == 46` assertion), `alpha-release.yml`, `macos-alpha-proof.yml`, `Dockerfile*`, and the Rust/Python files named in the strings. Findings below rest on the string assertions, not those files.

## How the suite behaves today

All 8 declarations are source-text contracts. They read workflow and shell files and assert substrings, counts, and index ordering. None executes `run.sh`, a container, or any proof step. The one real behavioral test in scope is the first half of `test_untraced_vendor_replay.py`: it runs `pkg_bounded_capture.py` as a subprocess and checks truncation, hostile-file refusal, GC exit mapping, and backpressure. That part is a genuine contract test. Everything else is string repetition.

## Declaration table

Class legend: **R** real security contract — detects a true regression; **F** fragile pin — exact version, name, or step wording; **C** string-coupled repetition — catches only deletion of the string; **D** dead-code/doc presence check. Rows often carry two classes.

| # | Declaration | Class | Actual detectable regression | Independent owner / missing stronger test | Risk |
|---|---|---|---|---|---|
| 1 | `test_alpha_build_includes_the_native_catalog_builder` | C, F | Builder line or asset loop deleted from `alpha-release.yml` | None stronger; workflow fails only at runtime, downstream | Low. Pins shell loop wording; rename breaks test, not build |
| 2 | `test_dry_run_has_read_only_permissions_and_pinned_checkout` | **R**, F | `contents: write` added; checkout unpinned; `persist-credentials` restored | None stronger; no native per-workflow enforcement. This is the right keeper | Keep as-is. The two pinned test commands are meta but harmless |
| 3 | `test_workflow_never_loads_a_production_key_or_publishes` | D (+R intent) | `secrets.`, `gh release`, or `aws-actions` reintroduced | `assertNotIn("secrets.")` is a blanket ban — blocks any future legitimate secret. README string check is D | Medium. Real invariant, crude mechanism; gives false comfort |
| 4 | `test_linux_alpha_artifact_is_retained_but_not_published` | F, C, D | Publish step added; retention dropped; `always()`/`success()` gates swapped; evidence step reordered | Upload `if-no-files-found: error` is the runtime backstop | High maintenance. Pins `0.1.0-alpha.56` names, `cargo-about 0.9.1`, `retention-days: 7`, exact step titles, upload SHA. Every version bump edits this test. Also duplicates `run.sh` strings kept by test file `test_untraced_vendor_replay.py` |
| 5 | `test_production_linux_input_is_manual_fixed_and_not_published` | **R** | Job made automatic; TUF root digest changed; runner unpinned | None stronger; manual gate and digest pin are supply-chain pins, not brittle wording | Keep. Digest and manual-input checks are correct security pins |
| 6 | `test_linux_uninstall_uses_plain_terminal_exec_status` | C, D | Exit-78 contract or `cmp` snapshots deleted from `run.sh` | The live proof job itself; CI runs the real uninstall | Low. `assertNotIn("idempotent uninstall")` is a dead-name check. Duplicates the uninstall assertions in `test_untraced_vendor_replay.py` |
| 7 | `test_linux_proof_binary_is_isolated_and_every_blocker_runs_twice` | C (one R part), D | A blocker removed from the 46-filter set; cleanup order flipped; build command duplicated | Filter-set equality (46 unique names) is the strongest keeper here — it proves completeness of the blocker list. The rest is `.index()` ordering over strings | Medium. Strongest and largest declaration, yet fully bypassed by the skipped-proof mutant (see below). Heavily duplicated with `test_untraced_vendor_replay.py` |
| 8 | `test_macos_proof_authenticates_release_checksums_before_use` | **R** intent, C/F mechanism | `cosign verify-blob` moved after asset use | None; macOS workflow never executed by any test | Medium. Correct ordering invariant; `split()` on exact step names is brittle |

**Security contracts to keep:** #2, #5, the ordering core of #8, the upload pin and gate checks in #4. These guard permissions, pinned actions, manual gating, and verify-before-use. No stronger independent owner exists for them.

**Brittle wording to strip:** version strings (`alpha.56`, `0.9.1`, `nix-2.34.8`), artifact names, step titles, `tee` log names, trigger path strings. Extract these as data (one constants block) or parse the YAML and assert on structure, not text.

**Dead-code presence checks to drop or invert:** #3's README assertion, #6's `assertNotIn` name checks, #4's `PKG_NIX_SOURCE_ARCHIVE` / `nix-2.34.8` negatives. They prove only that old strings stay absent.

## The skipped-linux-proof mutation: what it proves

`results.json` entry `skipped-linux-proof`, verified against source:

- **Mutation:** an early successful `exit` inserted before all proof checks in `tests/linux-clean-host/run.sh`.
- **Test command:** `python3 -m unittest discover -s tools/release -p test_workflow.py` → **exit 0 on the mutant**. All 8 declarations passed.
- **Live-mutant evidence:** `original_invalid_input_exit: 2`, `mutant_invalid_input_exit: 0`. The original harness rejects bad input with exit 2; the mutant returns 0. The behavior change is visible from the exit code alone, so the mutant was live, not inert. `restored: true` matches the clean tree at `e96a7d0`.

**What it proves:** the whole suite is blind to proof-execution bypass. `test_workflow.py` checks strings like `docker build` and `package_alpha_candidate.py` with `.index()` — an inserted early exit does not remove those strings, so every ordering assertion still holds. Neither test file ever runs `run.sh`. A contributor can delete the entire Linux proof behavior and keep the contract job green.

**What it does not prove:** full CI would still catch this specific mutant by accident. The `linux-alpha-proof` job runs `run.sh` for real; with an early exit, no `pkg-v0.1.0-alpha.56-linux-x86_64.tar.gz` appears, so `test -f` in "Verify the retained candidate name" fails and the `if-no-files-found: error` upload fails too. That is an implicit, unowned side effect of the workflow — not a test anyone wrote, and only because this mutant forgot to fabricate the file. A smarter mutant (`touch` the expected paths, or skip only evidence subchecks) likely passes CI. The result also says nothing about the `production-linux` job, and it does not show any exit-code contract test for `run.sh` guards exists — it shows the opposite.

## Do these tests run on PRs?

Yes, with path limits. `release.yml` triggers on `pull_request` (paths: `tools/release/**`, `crates/**`, `tests/linux-clean-host/**`, `release.yml`, `Cargo.toml`, `Cargo.lock`, `docs/install.sh`), on `push` to `main`, and `workflow_dispatch`. So:

- `dry-run-sign` — runs `cargo test -p pkg-release` and the `test_workflow.py` suite on matching PRs and main pushes.
- `linux-alpha-proof` — runs the real 60-minute harness on the same PRs.
- `production-linux` — **manual only**: gated on `github.event_name == 'workflow_dispatch' && inputs.production-linux`. Its guards (#5) are exercised on PRs only as string checks.

PRs that touch none of the listed paths skip the whole suite.

## First coherent consolidation batch

Goal: kill the proven blind spot. Convert proof-execution contracts from string checks into one cheap behavioral keeper, and separate security pins from brittle wording. Do not touch behavior of `run.sh` proofs yet.

1. Add a `--self-check` mode to `tests/linux-clean-host/run.sh` that runs only the argument-validation and clean-tree guard path and exits 2 on bad input. Then the skipped-proof mutant (early successful exit) fails it.
2. Add one declaration that executes `run.sh --self-check` (and `sh -n`) and asserts exit 2 on invalid input and 0 on valid input. This is the missing keeper the mutation proved absent.
3. Extract the pinned-action SHAs, permission blocks, and manual-gate strings (declarations 2, 5, gate parts of 4) into one data-driven loop. Leave them as exact pins — they are security contracts.
4. Delete dead-code negatives (#3 README check, #6 `assertNotIn` names) and move all `run.sh` string assertions to one owner file, removing the duplication between `test_workflow.py` and `test_untraced_vendor_replay.py`.

Focused commands:

```sh
# Baseline (must stay green)
python3 -m unittest discover -s tools/release -p 'test_*.py' -v

# Replay the proven gap after step 2 (expect nonzero exit now)
sed 's/^case "${1-}" in/exit 0\ncase "${1-}" in/' tests/linux-clean-host/run.sh > /tmp/mutant.sh
sh /tmp/mutant.sh --self-check; echo "mutant exit: $?"   # expect 2 after fix, 0 today

# Guard contract in isolation
python3 -m unittest tools.release.test_workflow.ReleaseWorkflowTests.test_dry_run_has_read_only_permissions_and_pinned_checkout -v
```

Steps 3–4 are mechanical and safe to follow as a second batch. Step 1–2 closes the only confirmed, mutation-proven hole.

