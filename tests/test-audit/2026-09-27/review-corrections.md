# Corrections to the GLM reviews

The three linked GLM reports are raw model output. They are evidence of the
review process, not accepted implementation instructions. The final decisions
are in [report.md](report.md) and [reviewed-ledger.json](reviewed-ledger.json).
All runs used `zai` / `glm-5.3-flash` in E2B. Two initial broad reviews timed out
without reports. Three smaller reviews completed. No source edits from a model
review were applied.

## Shared corrections

Each report changed the supplied R/F/C/D meanings. The corrected ledger uses
retain, fix, consolidate, and delete. `U` means unreviewed. No declaration has a
completed deletion decision in this audit.

The working tree included tracked plan-cleanup changes. It was not a clean
checkout of the base commit. The source manifest identifies the executed bytes.
The [final check](evidence/final-source-check.json) confirms that execution did
not change them. Only selected test subsets were run against each mutation.
Do not infer that a mutation would pass the complete repository or native CI.

## Renderer

- The claim that the invalid-tag test catches relaxed validation is false.
  Disabling that guard leaves all five renderer tests green. Missing artifacts
  raise the expected exception later. The complete-fixture control rejects the
  tag in the original source and accepts it in the mutant. The generated
  unsafe script was not executed.
- `v01.0.0` is accepted by the current grammar. The review supplied no product
  requirement to reject it. Do not add that requirement as a test cleanup.
- `PKG_SHA256_*` names are renderer replacement keys. Do not assume that they
  are literal assignments in the output. Prefer distinct real artifact bytes,
  independent digests, and execution of the actual rendered bootstrap.
- The shell execution harness currently fills the template itself. It does
  not cover the renderer that the release command uses. Connect those owners
  before calling this a complete release-to-bootstrap test.
- The proposed `git checkout --` restore can discard local work. Mutation
  probes used a separate copy and restored exact original bytes instead.

## CLI

- The structured-uninstall refusal test is compiled on x86-64 Linux. Its
  condition includes all Linux targets. It ran in the 14-test Linux CLI suite.
- The HOME test observes a relative-root refusal. It does not observe a valid
  production state root under a false HOME value. Retain the refusal contract;
  repair or narrow the wider identity claim. No HOME product defect was proved.
- FakeOperations supplies the operation results and fake Nix calls itself.
  Their order and exhaustion do not establish a production lifecycle sequence.
  The suite's real contribution is dispatch and policy handling. Compare that
  with `core_engine_routes_every_variant_and_preserves_global_policy`; carry
  the distinct GC dry-run assertion before removing the duplicate layer.
- `CoreEngine::into_operations` is a production accessor with one caller in
  `e2e_fake.rs`. The claim that there is no test-only production seam missed
  this accessor. The production CoreEngine itself remains in use by `main`.
- The existing narrow public-result test caught the private-path mutation.
  Keep that contract until a stronger process test demonstrably catches it.
  A blanket removal of unit tests would lose working evidence here.
- The proposal to copy the fake E2E pattern into connector tests is not the
  selected direction. Prefer real broker/process/file-state outcomes, with
  controlled external boundaries that do not supply the asserted result.

## Workflow

- The manual-only production condition is not protected by the current test.
  Replacing that job's condition with unconditional true leaves all eight
  tests green. The guard words occur elsewhere. Check the actual job condition.
- The skipped-harness probe proves that the static suite misses an early exit.
  It does not prove a complete native workflow would pass or fail. Downstream
  artifact checks may detect the missing work. Claims about a more complex
  mutant passing CI were speculation and are not accepted.
- Do not add the proposed `--self-check` mode solely to satisfy tests. The
  existing invalid-argument path can safely prove refusal. Full lifecycle
  execution still needs native proof and valid result artifacts.
- The model called all declarations fully reviewed but admitted that it had
  not read the tail of the 46-name filter list. The parent review completed
  that test file. It did not review every referenced blocker implementation.
  Presence and count of names do not prove those blockers execute.
- README wording and obsolete-name checks are deletion candidates within
  mixed declarations. This does not justify deleting the associated permission,
  trust, ordering, or ownership contracts. Version and action pins need a
  separate contract decision; they are not all disposable wording.

The [seven mutation records](evidence/mutations/results.json) and their logs
support these corrections. All modified source owners were restored exactly.
