# Test audit: real behavior and false confidence

Audit date: 27 September 2026 (Asia/Kolkata).

Follow-up: the [first cleanup and replacement batch](cleanup.md) implements
the repairs and consolidation below. This report retains the original audit
findings and pre-change evidence.

The selected tests can pass with broken production behavior. Seven isolated
mutations demonstrated this. The next work should repair these gaps and remove
redundant test layers after their remaining contracts have a clear owner.
This audit changed no production code or tests. It did not delete tests or
weaken a CI gate.

## Confirmed findings

| Finding | Evidence | Required next step |
| --- | --- | --- |
| The installer renderer test does not check the artifact digests it claims to protect. | Replacing every SHA-256 with 64 zeroes left all five renderer tests green. None of three independently calculated fixture digests appeared in the result. The original renderer included all three. | Exercise the real renderer command with distinct artifact bytes. Check the generated digests independently, then feed its actual output into the bootstrap execution test. |
| The symlink rejection test can pass at the wrong guard. | Removing `path.is_symlink()` left all five renderer tests green. The test supplies a Linux symlink but omits the macOS artifacts. A complete fixture is rejected by the original owner and accepted by the mutant. | Supply every other required artifact. Require refusal of the symlink before rendering or execution. |
| The invalid-tag test can pass at the wrong guard. | Disabling tag validation left all five renderer tests green. The test uses a directory without the required artifacts, so a later file check raises the expected exception. With complete artifacts, the mutant accepts a tag with shell metacharacters and the original refuses it. | Run the real renderer with complete artifacts for each invalid tag. Assert refusal and no output script. The audit did not execute the unsafe generated script. |
| The doctor subprocess test has a vacuous failure-path assertion. | Removing both `runtime.managed` and `channel.signed` rows left the named compiled CLI test green. Its filtered `.all(...)` accepts an empty set. | Assert both required IDs exist, then check their status and the process exit. Keep platform-specific ownership refusals explicit. |
| The fake CLI suite is not end-to-end privacy proof. | Removing `/nix/` rejection from real `CommandResult` validation left `e2e_fake.rs` green. The existing `public_result_rejects_private_runtime_material_and_reserved_fields` test failed on the leaked path and passed after restoration. | Retire the duplicate fake-routing layer only after its dispatch/policy assertions are assigned to a keeper. Remove its unused production accessor with it. Keep the independent validation contract until a stronger process test owns it. |
| Workflow text checks do not prove that the Linux lifecycle harness executes. | Inserting an early successful exit into the real harness left all eight `tools/release/test_workflow.py` tests green. Executing the mutant with invalid arguments returned 0; the original returned 2. | Retain native lifecycle proof. Replace behavioral claims based on string presence with executable checks or validated proof results. Retain distinct workflow permission and trust guards until replacement is shown. |
| The production job's manual-only condition is not protected by its test. | Changing the `production-linux` job condition to unconditional true left all eight workflow tests green. The required words still occur elsewhere in the workflow. | Check the effective condition on the production job, plus its input and trigger restrictions. Prove that the same mutation fails. No workflow was dispatched during this probe. |

These are test defects or overstated coverage claims. The injected behavior is
not present in the original product. A passing mutated subset does not mean
that every repository test or the complete native workflow would pass.

Exact source owners and test declarations:

- `tools/install/render.py:13`; `tools/install/test_render.py:12`, `:23`, and `:28`.
- `crates/pkg-cli/src/commands/doctor.rs:370`;
  `crates/pkg-cli/tests/cli.rs:105`.
- `crates/pkg-cli/src/commands/execute.rs:761`;
  `crates/pkg-cli/tests/e2e_fake.rs:151`;
  `crates/pkg-cli/src/commands/execute/tests.rs:182`.
- `tests/linux-clean-host/run.sh:1`; `tools/release/test_workflow.py`.
- `.github/workflows/release.yml:41`; `tools/release/test_workflow.py:87`.

The [mutation results](evidence/mutations/results.json),
[restored controls](evidence/mutations/restored-controls.json), and retained
logs record the exact commands and exits. Mutations ran in a separate copy.
Every changed owner was restored byte for byte. All 1,164 tracked source files
in the execution snapshot also matched the local tree before the final audit
links were added to the plans. The [final source check](evidence/final-source-check.json)
records the sandbox checks.

## Baseline and platform limits

| Executed in E2B Linux | Result |
| --- | --- |
| Rust workspace, all targets and features, locked dependencies, Rust 1.96.1 | 1,234 passed; 0 failed; 8 ignored |
| Python unittest files | 149 methods across 20 files: 146 passed; 3 skipped |
| Standalone TUF harness | 20 passed; 0 failed |
| Bounded subprocess capture and vendor replay script | Passed |
| Restored CLI privacy and doctor controls | Both passed |

See [Rust summary](evidence/rust-baseline-summary.json),
[Python file results](evidence/python-baseline.json),
[TUF summary](evidence/tuf-baseline-summary.json), and
[bounded-capture output](evidence/bounded-capture-baseline.log).

E2B supplied x86-64 Linux with two CPUs and about 2 GiB RAM. The Rust build used
one build job and disabled debug information to fit the sandbox. This run did
not establish network isolation, real Nix behavior, a clean-host installation,
privileged mount behavior, a reboot, or macOS behavior.

The Python total above is corrected from the initial report: three macOS
packaging cases were skipped, not passed. The original logs are unchanged.

The eight ignored Rust cases require privileged mount or process fixtures,
an official runtime archive, real Nix stores, or public flakes that are refused
on Linux. The Python shell test also skips zsh when it is absent. Those limits
are not passing platform evidence. The existing
[alpha.56 native proof](../../release-lifecycle/ALPHA-56-2026-09-26.md) is
historical evidence and was not rerun for this audit.

## Source and audit scope

Base: `e96a7d06b01e0205e82f51f112a8951ae9324830`.
The E2B sync included the tracked, uncommitted plan-cleanup changes. The source
manifest is an exact hash inventory of that tree, not a claim of a clean commit.
The new audit skill and retained-spike index were copied separately because
E2B sync excludes untracked files. The user's unrelated HTML artifact was not
copied. The temporary sandbox ID was `iqdo9wxiw9hz4zmryrrsm`. Its evidence was
downloaded and hash-checked. The sandbox was then stopped.

The [structural inventory](evidence/inventory.json) found 1,436 declarations:
1,287 Rust test attributes and 149 Python test methods. Rust macros, property
expansion, documentation tests, and platform configuration mean this is not a
runtime test count. Script-style assertions and native scenarios are additional
surfaces. An inventory entry is not a reviewed test; its initial `U` mark means
unreviewed. Baseline execution is also not a semantic review.

The [reviewed ledger](reviewed-ledger.json) covers 29 declarations in four test
files: 15 CLI process tests, one fake CLI routing test, five renderer tests, and
eight workflow tests. These files contain 1,060 lines, including their helpers.
One of the CLI declarations is excluded on Linux. Related source owners,
keepers, history, and CI routing were inspected; the ledger states the limits.
Most installer, Nix adapter, store, channel, and pipeline declarations remain
outside this detailed source review. This is a broad baseline with a focused
semantic audit, not a completed repository pruning campaign.

The [source manifest](evidence/source-manifest.json) and
[evidence index](evidence/evidence-index.json) preserve file hashes and exact
execution logs. The initial wide GLM tasks exceeded the E2B command limit before
saving reports. The audit used smaller bounded GLM tasks instead. The configured
provider/model is `zai` / `glm-5.3-flash`; the E2B bridge has no model override.

The raw GLM reports cover [CLI](glm-cli.md), [renderer](glm-renderer.md), and
[workflow](glm-workflow.md). Their conclusions were checked against source and
execution evidence. The [review corrections](review-corrections.md) reject
several unsupported claims. In particular, the model said the invalid-tag and
manual-job guards were protected; two further mutations disproved those claims.
Use the corrected ledger and this report for decisions.

## First cutover

1. Repair the three renderer assertions at the real rendering boundary. Extend
   the existing shell execution harness to consume the generated script, rather
   than manually reconstructing the renderer's output. Network and privilege
   doubles may control those external boundaries; they must not supply digest
   validation or the result being asserted.
2. Repair the doctor's missing-ID assertion and keep the demonstrated mutation
   as a control during the change.
3. Consolidate CLI routing. Compare `core_engine_routes_every_variant_and_preserves_global_policy`
   with the fake integration suite. Keep any distinct argument or policy check
   once. `CoreEngine::into_operations` has only the fake integration test as a
   caller in this repository; remove that test-only accessor with the retired
   layer if no remaining keeper requires it. Carry the distinct GC dry-run
   policy assertion before retiring that layer.
4. Separate static workflow security policy from executable lifecycle evidence.
   Repair the production job condition check. Use the existing invalid-argument
   path for a safe harness refusal check; do not add a test-only execution mode.
   Do not mass-delete the macOS or Linux guards. Several privileged, crash,
   ownership, and trust contracts still need a replacement before consolidation.

The [pkg test-audit skill](../../../.agents/skills/test-audit/SKILL.md) and its
[campaign procedure](../../../.agents/skills/test-audit/CAMPAIGN.md) replace the
source template's OpenClaw, Vitest, crabbox, and PR-helper assumptions. They
require real boundary evidence, explicit keepers, and caught mutations. They do
not make unit tests a default requirement or make deletion count a goal.

Production LOC change: 0. Test LOC change: 0. Test-support LOC change: 0.
Only audit guidance and evidence were added. The earlier plan cleanup is a
separate, still-uncommitted change. This audit has no commit or pull request.
