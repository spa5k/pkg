# Subsystem test-pruning campaign

Use this for a complete subsystem cutover. A read-only repository audit may
recommend a campaign without claiming these phases are complete.

1. **Baseline.** Pin main and the audited working-tree changes. Inventory each
   owned test, support file, and native scenario. Record each test file's result
   or why its platform cannot run. A blocked native baseline prevents a claim
   of a complete native campaign, not a useful source audit.
2. **Ownership lanes.** Assign each declaration and scenario once, by the
   production owner: CLI/output; broker/admission; pipeline/state/activation;
   channel/crypto/catalog; Nix adapter/build; installer/platform; release and
   assurance tooling. Include shared boundaries. Do not run edits concurrently
   with tests against the same tree.
3. **Read-only ledgers.** Give independent lanes to read-only reviewers when
   useful and authorized. Each reviewer reads complete declarations and owners.
   Record R/F/C/D and all evidence fields from SKILL.md. An inventory row is not
   a reviewed row. Keep unreviewed scope explicit.
4. **Layer plan.** Make a second pass over the ledger. Name a primary keeper for
   each contract, assertions to port, retired layers, CI changes, and test-only
   production seams that can be removed. Evidence must precede deletion.
5. **Cutover.** Apply one owner boundary at a time. One owner edits shared
   support. Port assertions before removing their old owner. Remove unused
   test-only seams with their callers. Preserve existing lint ratchets; change
   a baseline only for measured reductions. Run each keeper and its siblings.
6. **Preservation.** Have independent reviewers compare deleted contracts with
   keepers. For a restored contract, demonstrate a caught production mutation
   and restore the source exactly. Reject unreachable or unrelated negatives.
7. **Product defects.** Record baseline defects separately. If a fix is in
   scope, give it a separate change and demonstrate pre-fix failure and post-fix
   success through the same owner boundary. Do not hide bugs as test cleanup.
8. **Reconcile.** If main advances, port new regressions into the keepers before
   retiring their old files. Rerun the complete subsystem and required native
   proof on the combined source. Report test/support/production LOC separately,
   unresolved gaps, actual commands, and review/commit/PR state. Publish or merge
   only within the user's authorized scope.

Use this repository's Cargo, Python, shell, CI, and native-VM workflows. The
OpenClaw/Vitest/crabbox commands from the source template do not apply here.
