# Determinate Nix stacked PR implementation plan

Status: active implementation plan for an alpha product.

Current status: **DN-03 evidence complete; product delivery NO-GO; DN-04 is next.**

DN-03 completed the standalone vendor evidence gate. This does not mean that vendor uninstall is clean. It does not mean that crash recovery succeeds. Linux functional behavior checks passed, but strict vendor cleanup failed. In the accepted macOS crash observation, state validation stops after the recovery install exits 0 because `_nixbld1` is missing.

The public evidence is in the [DN-03 parent decision](../spikes/s6-determinate-installer/FINDINGS.md), the [Linux findings](../spikes/s6-determinate-installer/linux-vm/LINUX-FINDINGS.md), and the [macOS lifecycle, residue, and crash findings](../spikes/s6-determinate-installer/macos-vm/FSTAB-CONTRACT-RESEARCH.md).

- Linux R12 proves broad Linux x86_64 behavior. Retained x86_64 R11 and aarch64 R10 prove the two Linux target Asset records.
- macOS R10 completes the standalone lifecycle and residue evidence. Its functional lifecycle and reboots passed, but strict vendor cleanup failed. Crash R1 completes the required negative SIGKILL and reboot observation.
- Clean vendor uninstall remains false on both platforms. DN-13 owns exact fail-closed cleanup.
- Successful crash recovery is unproved. DN-06, DN-07, and DN-16 own the required product controls and later proof. DN-12 may run an optional `repair sequoia` proof.
- DN-04 can now document the proved ownership and executable contract.

This plan replaces the old custom Managed Nix implementation plan. The old plan is preserved in the [dated legacy archive](archive/2026-08-22-custom-managed-nix-v1/README.md). The design reasons and research are in the [architecture report](../architecture-report.html).

This branch contains plans only. It does not integrate Determinate Nix.

This planning branch does not change shipped behavior or current user instructions. DN-15 and DN-16 update the user documents for each platform when that platform changes. DN-20 completes the release documents after the final proof.

## 1. Accepted ownership

The accepted product boundary is:

- The Determinate Nix Installer owns the machine-wide **Base Nix lifecycle**.
- Base Nix lifecycle means Base Nix install, Base Nix repair, the proved Base Nix update policy, and explicit Base Nix uninstall.
- `pkg` owns package selection, package builds, package state, generations, activation, package roots, package garbage collection, package repair, and the product user experience.
- `pkg` can keep its Broker and Root Helper for package work.
- The plan does not assume that Determinate replaces the Broker or Root Helper.
- The plan does not assume that Determinate replaces package roots, package garbage collection, or package repair.
- Raw Nix can exist on the machine. `pkg` keeps raw Nix out of its normal user experience. This is not a security boundary.
- Local administrators can change machine-wide Nix. `pkg doctor` must detect important changes and fail closed where ownership is not clear.
- The old private Managed Nix model has no compatibility bridge. `pkg` detects it, refuses the operation, and tells the user to run the clean old uninstall before a clean new install.

## 2. Upstream observed baseline

The following items are upstream observations. They are **not integrated product facts**.

- Observed package: `nix-installer` 3.22.1.
- Observed full revision: `4132ad07a15ee7d88c096ac7172b7afb2672866b`.
- Observed revision date: 2026-08-19.
- Observed license: LGPL-2.1.
- Observed receipt path: `/nix/receipt.json`.
- Observed installed executable path: `/nix/nix-installer`.
- Observed default: diagnostics are enabled.
- Observed library status: the Rust library interface is experimental.
- Observed install behavior: the installer can install Determinate Nix.
- Observed uninstall behavior: the installed executable reads its receipt and reverses recorded work.

DN-03 recorded the standalone product-facing evidence for the executable. This includes exact command arguments, argument order, exit status, output, diagnostics control, receipt behavior, update ownership, PATH behavior, platform support, and failure observations. DN-04 must map its contract text to that evidence.

The candidate executable call is only a test input. It is not the product contract. No later PR can cite an unproved candidate command as a stable interface.

## 3. Stack rules

We use one linear GitHub PR stack.

### 3.1 Branch names

- The plan branch is `plan/determinate-nix-stacked-prs`.
- Core branches use `dn/NN-short-name`.
- Optional branches use the same format.
- `NN` matches the PR number in this plan.
- A branch contains one reviewable result.

### 3.2 PR bases

- DN-00 targets the repository default branch.
- DN-01 targets the DN-00 branch.
- Each later PR targets the previous published branch.
- There is one published linear stack.
- Do not publish two competing versions of the same stack.
- Do not add Graphite or another stacking dependency.

### 3.3 Required PR description

Each PR description has these fields:

1. **Parent**: the parent PR number and branch.
2. **Goal**: one result that this PR provides.
3. **Ownership change**: what Determinate owns and what `pkg` still owns.
4. **Invariants**: rules that must remain true.
5. **Proof**: tests, VM evidence, logs, digests, and residue checks.
6. **Deletion**: code removed in this PR, or `none`.
7. **Stop rule**: the result that blocks the child PR.
8. **Rollback**: how to remove the change before merge.
9. **Risk**: the highest remaining risk.
10. **Not included**: work that belongs to a later PR.

### 3.4 Parent merge procedure

The stack assumes that GitHub squash-merges each PR.

When a parent PR squash-merges:

1. Fetch the merged default branch.
2. Rebase the next branch on the merged default branch.
3. Resolve only conflicts caused by the parent merge.
4. Run the next PR's local proof again.
5. Force-push the next branch with lease.
6. Retarget the next PR to the default branch.
7. Rebase the next descendant onto the newly rewritten immediate parent.
8. Continue this restack in order for every published descendant.
9. Force-push each rewritten descendant with lease.
10. Verify the merge-base and visible PR diff for every restacked PR.
11. Confirm that each PR contains only its own change.
12. Update every affected parent link in the PR descriptions.

Do not leave a descendant based on an old pre-squash commit. Do not merge a child before its parent. Do not squash several stack entries into one review unless the entries are documentation-only and reviewers agree.

### 3.5 Local proof policy

GitHub Actions are disabled today. This plan does not enable them.

- Each PR records exact local commands and exact results.
- VM evidence includes the image, architecture, date, input asset digest, output logs, and residue report.
- A reviewer must be able to repeat the proof.
- A skipped platform check blocks a production cutover for that platform.
- Local proof is not optional because remote Actions are disabled.

## 4. Dependency diagram and stop gates

```text
DN-00
  |
DN-01 -> DN-02 -> DN-03 [VENDOR CONTRACT STOP GATE]
                         |
                       DN-04 -> DN-05 -> DN-06 -> DN-07 -> DN-08
                                                               |
                                                             DN-09 [PACKAGE PARITY STOP GATE]
                                                               |
                                                             DN-10 -> DN-11 [PATH STOP GATE]
                                                                           |
                                                                         DN-12 [INACTIVE REPAIR/UPDATE]
                                                                           |
                                                                         DN-13 [INACTIVE UNINSTALL]
                                                                           |
                                                                         DN-14 [OLD ALPHA RESET]
                                                                           |
                                                                         DN-15 [LINUX LIFECYCLE]
                                                                           |
                                                                         DN-16 [MACOS LIFECYCLE]
                                                                                           |
                                                                         DN-17 -> DN-18 -> DN-19
                                                                                           |
                                                                         DN-20 [CORE DONE]
                                                                                           |
                                                                         DN-21 -> ... -> DN-27
                                                                                           |
                                                                         DN-28 -> DN-29
                                                                                           |
                                                                         DN-30 -> DN-31
                                                                                           |
                                                                         DN-32
```

Stop gates:

- **DN-03** is complete for standalone evidence. Its negative results still block product delivery until the owning later gates pass.
- **DN-07** blocks integration if the minimal Handoff cannot recover without a second vendor journal.
- **DN-08** blocks integration if one configuration file needs two writers or the vendor has no supported extension point.
- **DN-09** blocks installer cutover until standard-daemon RealNix package behavior matches the current required behavior.
- **DN-11** blocks cutover until PATH behavior is proved for login shells, non-login shells, and GUI launches.
- **DN-12** blocks lifecycle cutover if inactive Base Nix repair or update routing is unsafe.
- **DN-13** blocks lifecycle cutover if inactive uninstall or resumable product cleanup is unsafe.
- **DN-14** blocks lifecycle cutover if the old alpha has no executable authenticated reset path.
- **DN-15** blocks Linux deletion until the complete Linux lifecycle proof passes.
- **DN-16** blocks macOS deletion until the complete Apple Silicon macOS lifecycle proof passes.
- **DN-20** blocks the optional simplification tail until the core cutover is complete.
- **DN-26** blocks local-build admission changes until every build duty has a proved replacement.
- **DN-27** blocks any Root Helper or Broker deletion until every live duty has a proved owner.

## 5. Keep and delete ownership map

Deletion always follows proof. A file name in the candidate-delete column is not permission to remove the complete file.

| Area | Important files or symbols | Core decision | Earliest deletion |
|---|---|---|---|
| Package identity | `GenerationId`, `RootName`, `StorePath` in `pkg-nix` and callers | Keep. These are product domain contracts. `GenerationId` reaches about 529 nodes and 528 edges. | Never as part of the Base Nix cutover. DN-31 can relocate a type only if it removes a dependency edge or enables crate deletion. |
| Package maintenance | `MaintenanceAdapter`, `crates/pkg-store/src/{roots,current,gc}.rs` | Keep. It reaches about 93 nodes and 104 edges. | Only narrow unused methods after DN-27 proof. |
| Broker client | `BrokerLifecycleClient`, `crates/pkg-cli/src/broker.rs` | Keep through core migration. It reaches about 65 nodes. | Base-lifecycle methods can be removed in DN-29 or DN-30. Full deletion needs DN-26 and DN-27 proof. |
| Ownership contracts | `OwnershipExpectation`, `ManagedGroupBindings` | Keep while shared product and Base Nix assets are split. These contracts have high impact. | Remove only fields and uses proved to be Base-Nix-only in DN-17 through DN-19. |
| Package build | `crates/pkg-nix/src/{adapter,real,build,verify,substitute}.rs` | Keep. Add standard-daemon parity before cutover. | No core deletion. |
| Package lifecycle | `crates/pkg-pipeline`, `crates/pkg-core`, `crates/pkg-store` | Keep. Determinate does not provide this product behavior. | Optional simplification only in DN-21 through DN-32. |
| Broker and helper | `crates/pkg-installer/src/{broker,helper,root_client,service}.rs`, binaries | Keep for package work until proof says otherwise. | DN-28 and DN-29 only after DN-26 and DN-27 pass. |
| Product service assets | product broker/helper services, product policy, product state | Keep. They remain owned by `pkg`. | Delete only a proved unused product asset. |
| Linux Base Nix install | `installer.rs`, `linux_*`, Base-Nix parts of `assets.rs` and `bootstrap.rs` | Replace with the vendor executable after Linux cutover proof. | DN-17, and only proved Base-Nix parts. |
| macOS Base Nix install | `macos_*`, `store_apfs.rs`, Base-Nix launchd and filesystem parts | Replace with the vendor executable after macOS cutover proof. | DN-18, and only proved Base-Nix parts. |
| Private runtime provisioning | `pkg-nix/src/managed/{runtime_archive,installer_bundle,provision,daemon,accounts}.rs` | Keep until both platforms use the vendor lifecycle and package parity passes. | DN-19, by proved symbol and caller set. |
| Base Nix ownership and journals | `managed/ownership.rs`, platform install journals, store/repair journals | Keep until handoff and vendor receipt behavior are proved. | DN-17 through DN-19. Keep package journals. |
| Uninstall | `UninstallEngine`, platform uninstall modules | Prove inactive vendor uninstall and full product cleanup before cutover. Full uninstall keeps no product state. | DN-13 starts the inactive path. DN-17 through DN-19 remove obsolete Base-Nix paths. |
| Wire contracts | `pkg-nix/src/{contract,framing}.rs` | Keep during core migration. These files mix live product grammar with candidate obsolete grammar. | DN-30 deletes only dead messages after caller and test proof. |
| Release assets | `tools/release`, channel metadata, runtime manifests | Add the pinned vendor executable first. Keep old assets until both cutovers pass. | DN-19 removes old Base-Nix artifacts. |
| Tests | package, contract, parity, recovery, and platform tests | Keep and adapt. Move fakes only after the production seam changes. | DN-22 deletes broad fakes after replacement tests pass. |

## 6. Core migration PRs

### DN-00 — Archive the old plan and publish this plan

- **Branch:** `plan/determinate-nix-stacked-prs`.
- **Base:** repository default branch.
- **Goal:** preserve the old plan and publish one active, reviewed stack plan.
- **Why now:** implementation needs one source of order, ownership, and stop rules.
- **Likely files and symbols:** `plans/**` plus only the documentation and link-check maintenance required by the archive move.
- **Interface and invariants:** no runtime behavior changes; old history stays readable; the active plan links to the archive and architecture report.
- **Implementation steps:** move old plan files into the dated archive; add an archive notice; add this plan; check every relative link; record that GitHub Actions stay disabled.
- **Tests:** run a local Markdown link check or inspect every relative path; run `git diff --check`.
- **Proof and evidence:** archive file list, active file list, valid links, and clean whitespace check.
- **Deletion:** none. This PR archives files instead of deleting them.
- **Rollback or stop rule:** stop if any old plan file is missing from the archive.
- **Review focus:** history preservation, stack completeness, and no runtime edits.
- **Child-unblock condition:** two plan reviews pass and all blocking comments are resolved.

### DN-01 — Cancel every failed pending-install Broker operation

- **Branch:** `dn/01-cancel-pending-install`.
- **Base:** DN-00 branch and PR.
- **Goal:** close the current alpha cancellation hole in `recover_pending_install`.
- **Why now:** migration work must not build on a known operation leak.
- **Likely files and symbols:** `recover_pending_install`, `BrokerLifecycleClient`, pending-install recovery tests in `pkg-cli` and `pkg-installer`.
- **Interface and invariants:** every path after Broker operation creation either completes the operation or cancels it. A successful local recovery does not return `Ok` while the Broker operation can remain live. The original error remains visible. Cancellation failure does not hide the first failure.
- **Implementation steps:** trace all returns after operation creation. Use the existing cancellation call for local failure. After a completion transport error, reconcile or poll the operation. Return success only if the Broker reports completion. Cancel the operation if it is still live. Route all other failure exits through one cleanup point.
- **Tests:** focused pending-install recovery tests; Broker lifecycle contract tests; completion reply lost after Broker completion; completion transport failure while the operation is live; poll failure; cancel failure; existing CLI recovery tests.
- **Proof and evidence:** injected failures show no live operation. A lost completion reply reconciles to completed or cancels before return. No uncertain state returns `Ok`.
- **Deletion:** remove duplicate local cleanup branches if one shared existing path covers them.
- **Rollback or stop rule:** stop if cancellation can destroy committed package state.
- **Review focus:** exact operation state, error precedence, and no unrelated migration code.
- **Child-unblock condition:** all failed or uncertain paths reconcile or cancel. Successful paths prove Broker completion once.

### DN-02 — Shorten the verify-only repair lease

- **Branch:** `dn/02-short-repair-lease`.
- **Base:** DN-01 branch and PR.
- **Goal:** release the exclusive state lease before long verify-only work.
- **Why now:** this is the second current alpha finding. It can make repair block unrelated work.
- **Likely files and symbols:** repair command flow, `production_repair.rs`, state lease code, `RepairMode`, `MaintenanceAdapter`.
- **Interface and invariants:** the user-state lease protects the stable Generation snapshot. A Broker-held GC inhibitor, retention pin, or equal proved mechanism protects that Generation and its roots for the full verification. Verify-only work does not mutate state. Mutating repair keeps stronger exclusion.
- **Implementation steps:** identify the stable Generation and root set under the user-state lease. Acquire the Broker-held GC inhibitor or equivalent retention pin before lease release. Keep that protection through the full verification. Release it after the result is complete. Keep the current stronger lease and operation admission for mutating repair.
- **Tests:** another safe mutation proceeds during verification; GC cannot remove the verified Generation; history pruning cannot remove its roots; mutating repair retains stronger exclusion; retention acquisition failure keeps the lease and fails safely.
- **Proof and evidence:** synchronization tests prove overlap without sleeps. Root and Generation snapshots prove retention through verification.
- **Deletion:** remove only the now-unneeded wide lease scope.
- **Rollback or stop rule:** stop if any verify-only helper writes state.
- **Review focus:** state consistency, lock boundary, and test determinism.
- **Child-unblock condition:** verification permits safe mutation while GC and history pruning cannot invalidate its Generation.

### DN-03 — Prove the vendor executable contract

- **Status:** complete for standalone evidence. Product delivery remains NO-GO.
- **Branch:** `dn/03-prove-vendor-contract`.
- **Base:** DN-02 branch and PR.
- **Goal:** produce reproducible Linux and Apple Silicon macOS evidence for the exact external executable behavior.
- **Why now:** every integration decision depends on recorded standalone facts. The accepted DN-03 reports now provide those facts for DN-04.
- **Likely files and symbols:** a bounded spike under `spikes/`, evidence manifests, VM scripts, no production module.
- **Interface and invariants:** execute a pinned file by absolute path; verify its digest before privilege; never use `curl|sh`; never use PATH lookup; do not parse plan JSON as a product interface.
- **Implementation steps:** acquire version 3.22.1 for each supported target. Record the full revision and digest. Test exact observed arguments and argument order. Prove diagnostics control. Install the executable on standalone clean hosts. Inspect `/nix/receipt.json` and `/nix/nix-installer`. Test standalone repeat install, SIGKILL, reboot, repair, update, and uninstall behavior. Test foreign and existing Nix only as inputs to the standalone executable. Record Intel macOS asset availability.
- **Tests:** the DN-03 rows in the VM matrix in section 10 were observed. Linux R12 proves broad Linux x86_64 behavior. Retained x86_64 R11 and aarch64 R10 prove the two target Asset records. macOS R10 proves lifecycle and residue behavior. Crash R1 records an accepted negative crash result after `_nixbld1` is missing.
- **Proof and evidence:** the [parent decision](../spikes/s6-determinate-installer/FINDINGS.md) links the accepted public results. The [Linux report](../spikes/s6-determinate-installer/linux-vm/LINUX-FINDINGS.md) owns the Linux evidence. The [macOS report](../spikes/s6-determinate-installer/macos-vm/FSTAB-CONTRACT-RESEARCH.md) owns R10 and Crash R1. DN-03 does not prove `pkg` Handoff, package lifecycle, product repair, product uninstall, product cleanup, or production cutover.
- **Deletion:** none.
- **Rollback or stop rule:** DN-03 evidence is complete, but delivery stays NO-GO. DN-13 must handle exact vendor residue. DN-06, DN-07, and DN-16 must handle the failed crash recovery result. DN-12 may run an optional `repair sequoia` proof. Choose a small full executable fork only after a written blocking gap. Do not copy selected upstream source files.
- **Review focus:** observations versus conclusions, repeatability, licensing, diagnostics, update owner, and crash recovery.
- **Child-unblock condition:** complete. DN-04 can document the proved contract and its limits. This does not unblock product cutover.

### DN-04 — Update domain context and add ADR 0004

- **Status:** next. Ready to document the accepted DN-03 evidence.
- **Branch:** `dn/04-domain-and-adr`.
- **Base:** DN-03 branch and PR.
- **Goal:** record the proved Base Nix ownership boundary.
- **Why now:** documentation must follow proof. It must not turn guesses into architecture.
- **Likely files and symbols:** `CONTEXT.md`, new `docs/adr/0004-determinate-base-nix-lifecycle.md`, ADR 0003 status note.
- **Interface and invariants:** `CONTEXT.md` uses product-visible terms only. ADR 0003 remains valid for product package privilege. It is marked partially superseded for Base Nix lifecycle work only.
- **Implementation steps:** add Base Nix, Base Nix Lifecycle, and Package Lifecycle to `CONTEXT.md`. Add Handoff only if users or product behavior expose it. Put the Determinate executable, Vendor Receipt and path, diagnostics, pinning, upstream revision, and rejected alternatives in ADR 0004. Link DN-03 evidence. Mark ADR 0003 partially superseded without rewriting history.
- **Tests:** documentation link check; terminology search for contradictory active definitions.
- **Proof and evidence:** every ADR claim maps to a DN-03 result.
- **Deletion:** remove obsolete current-context statements only. Do not delete old ADRs.
- **Rollback or stop rule:** stop if DN-03 has an unresolved ownership result.
- **Review focus:** domain precision and no claim that package work moved to Determinate.
- **Child-unblock condition:** reviewers agree on one ownership vocabulary.

### DN-05 — Add the pinned vendor asset to release metadata

- **Branch:** `dn/05-vendor-release-asset`.
- **Base:** DN-04 branch and PR.
- **Goal:** authenticate and inventory the external executable as a product release asset.
- **Why now:** production code must never download or trust an unpinned executable.
- **Likely files and symbols:** `tools/release/src/manifest.rs`, `tools/release/src/sign.rs`, channel target fixtures, software inventory, installer release schema.
- **Interface and invariants:** target and digest are exact; asset is selected by supported system; license and matching source location are recorded; diagnostics policy is explicit; downgrade policy is explicit.
- **Implementation steps:** add per-target asset records; add digest verification metadata; add LGPL-2.1 notice and corresponding-source inventory; add diagnostics configuration metadata only if DN-03 proves it is stable; update fixtures and release validation.
- **Tests:** release manifest tests, signature tests, wrong-digest rejection, unsupported-system rejection, inventory completeness test.
- **Proof and evidence:** generated release metadata identifies one exact executable per target.
- **Deletion:** none. Keep old Base Nix assets until both cutovers pass.
- **Rollback or stop rule:** stop if source compliance or a supported target asset is missing.
- **Review focus:** trust chain, target mapping, license obligations, and no ambient download.
- **Child-unblock condition:** the release tool rejects any changed executable and publishes complete inventory metadata.

### DN-06 — Add one concrete vendor executable module

- **Branch:** `dn/06-vendor-executable`.
- **Base:** DN-05 branch and PR.
- **Goal:** provide one thin process boundary for the proved executable contract.
- **Why now:** later orchestration needs a small tested seam.
- **Likely files and symbols:** one new module in `pkg-installer`, `std::process::Command` or current async process support, installer errors, fake-executable tests.
- **Interface and invariants:** use an absolute authenticated path. Pass only proved arguments. Scrub or set the required environment. Keep both stdout and stderr draining while stored output stays bounded. Private data stays out of logs. Do not kill on timeout unless DN-03 proves recovery. Send only signals proved by DN-03. A lost client never silently orphans or kills a privileged child. Add no trait or provider framework.
- **Implementation steps:** define one concrete `DeterminateInstaller` value. Define install, repair, update-if-proved, and uninstall calls only as DN-03 supports them. Construct commands in one place. Reject a missing or changed executable. Drain both pipes concurrently into bounded buffers. Model client loss, timeout, and signal outcomes explicitly. Add a fake executable that records arguments and emits controlled output.
- **Tests:** argument tests; environment tests; exit-code tests; both-pipe back-pressure test; bounded storage test; private-log redaction test; client disconnect test; timeout without unproved kill test; proved signal test; wrong-file test.
- **Proof and evidence:** tests prove exact invocation without needing root or a VM.
- **Deletion:** none.
- **Rollback or stop rule:** remove the module if it needs plan JSON, source embedding, or a broad abstraction to work.
- **Review focus:** process safety, secret-free logs, cancellation, and minimal surface.
- **Child-unblock condition:** fake-process tests cover every accepted external outcome.

### DN-07 — Add Vendor Receipt validation and minimal Handoff state

- **Branch:** `dn/07-receipt-handoff`.
- **Base:** DN-06 branch and PR.
- **Goal:** persist only the minimum Base Nix handoff state needed for safe restart.
- **Why now:** the vendor receipt is opaque, and `pkg` needs a small crash boundary.
- **Likely files and symbols:** `pkg-installer` bootstrap state, new opaque receipt validator, current installer recovery entry points.
- **Interface and invariants:** absence of Handoff state means `NotStarted`. Persist only `Started` and `Accepted`. `Accepted` records the minimum stable identity for the observed executable and receipt. Do not parse or copy the vendor action list. Store no second receipt or action journal.
- **Implementation steps:** durably write `Started` before execution. Validate the vendor result after execution. Atomically write `Accepted` with the minimum identity. On restart, classify and report. Define the same atomic identity update after repair or update changes the observed executable or receipt. Never delete unknown `/nix` content.
- **Tests:** crash before launch, crash during launch, success before state update, missing receipt, damaged receipt, changed executable, and unknown `/nix` tests.
- **Proof and evidence:** each crash point ends in a fail-closed, recoverable state.
- **Deletion:** none. This is not a second receipt or action journal.
- **Rollback or stop rule:** stop if safe recovery requires replaying private vendor action types.
- **Review focus:** durability limits, opaque boundary, and no parallel ownership ledger.
- **Child-unblock condition:** restart classification is deterministic for all injected states.

### DN-08 — Split product assets from Base Nix assets

- **Branch:** `dn/08-split-assets`.
- **Base:** DN-07 branch and PR.
- **Goal:** give every installed asset one owner without changing production behavior.
- **Why now:** standard-daemon parity and platform cutover need a clean asset boundary first.
- **Likely files and symbols:** `assets.rs`, `linux_install_assets`, `macos_install_assets`, `OwnershipExpectation`, `ManagedGroupBindings`, platform asset managers, release manifests.
- **Interface and invariants:** product assets remain exact and authenticated. Base Nix assets remain on the old path until cutover. Classifications have no overlap. One file has one writer. `pkg` does not edit a vendor-owned configuration file.
- **Implementation steps:** classify each current asset. Create explicit product-owned and Base-Nix-owned views. Classify every Nix configuration file or fragment. Identify a supported vendor configuration extension point. Assign Broker daemon admission, `trusted-users`, and `allowed-users` settings to one owner. Prove preservation across vendor repair and update. Retain current production behavior.
- **Tests:** complete partition test; no-overlap and one-writer tests; stable current output test; Linux and macOS asset snapshots; vendor repair/update configuration preservation tests.
- **Proof and evidence:** every existing asset appears exactly once in the partition.
- **Deletion:** no production deletion.
- **Rollback or stop rule:** stop if an asset has mixed ownership, if no supported vendor configuration extension exists, or if `pkg` must edit a vendor-owned file.
- **Review focus:** complete partition and high-impact contract stability.
- **Child-unblock condition:** asset partition tests pass with no behavior change.

### DN-09 — Add standard-daemon RealNix parity

- **Branch:** `dn/09-standard-daemon-parity`.
- **Base:** DN-08 branch and PR.
- **Goal:** prove product package operations against the standard Determinate daemon layout before installer cutover.
- **Why now:** the current adapter assumes a private runtime. Cutover before parity would invert the dependency order.
- **Likely files and symbols:** `RealNixAdapter`, `crates/pkg-nix/src/real.rs`, `adapter.rs`, `build.rs`, `verify.rs`, `substitute.rs`, parity fixtures and `pkg-testkit`.
- **Interface and invariants:** use one fixed standard-daemon mode. Add no provider framework. Package outcomes and trust checks remain equal. Use absolute Nix executable paths or a proved stable discovery rule. Never use PATH lookup. Broker daemon admission and `trusted-users` or `allowed-users` behavior must be explicit.
- **Implementation steps:** add the fixed mode. Bind daemon and store paths proved in DN-03. Use only the vendor-supported configuration extension from DN-08. Prove Broker admission and multi-user access. Run read, resolve, acquire, substitute, local build, root, GC, and repair smoke probes. Run vendor repair and update. Confirm that configuration and package access survive.
- **Tests:** `pkg-nix` real tests, `pkg-testkit` parity tests, package install/remove/update/upgrade/GC smoke tests on both platforms.
- **Proof and evidence:** a standard-daemon parity report for Linux and Apple Silicon macOS.
- **Deletion:** none. Keep the private mode until production cutovers pass.
- **Rollback or stop rule:** stop cutover if package trust, root ownership, multi-user access, build, GC, or configuration ownership differs without an accepted design. Stop if `pkg` must edit a vendor-owned file.
- **Review focus:** package behavior, multi-user safety, and fixed configuration.
- **Child-unblock condition:** required package parity passes on both production platforms.

### DN-10 — Classify existing and foreign Nix

- **Branch:** `dn/10-classify-existing-nix`.
- **Base:** DN-09 branch and PR.
- **Goal:** fail closed when `/nix` exists without stable `pkg` handoff identity.
- **Why now:** production install must not adopt or destroy an unknown installation.
- **Likely files and symbols:** managed detection, Doctor command, bootstrap preflight, receipt and executable validation.
- **Interface and invariants:** initial alpha accepts only a clean host or a stable `pkg` handoff created by the new flow; foreign Nix, upstream Nix, unmarked Determinate, and old private alpha are distinct reports but all block automatic install.
- **Implementation steps:** define classifications from observable facts; add Doctor messages and user actions; add installer refusal; avoid automatic deletion or repair; keep automatic Determinate adoption as optional future work after stable identity proof.
- **Tests:** table tests for clean, accepted, foreign, upstream, unmarked Determinate, damaged accepted state, and old alpha.
- **Proof and evidence:** each fixture maps to one stable classification and one safe action.
- **Deletion:** remove only ambiguous old detection branches that become unreachable.
- **Rollback or stop rule:** stop if two unsafe states can produce the accepted identity.
- **Review focus:** false acceptance, user instructions, and no auto-adoption.
- **Child-unblock condition:** all unknown states fail before privilege or filesystem mutation.

### DN-11 — Prove and enforce PATH behavior

- **Branch:** `dn/11-path-gate`.
- **Base:** DN-10 branch and PR.
- **Goal:** make PATH behavior an explicit user experience compatibility gate.
- **Why now:** raw Nix visibility is not a security boundary, but unexpected profile edits can break the product experience.
- **Likely files and symbols:** installer invocation options, `crates/pkg-cli/src/path.rs`, shell tests, Doctor output, VM evidence.
- **Interface and invariants:** `pkg` never finds the vendor executable or Nix through PATH; normal `pkg` use works in login, non-login, and GUI contexts; no unproved shell profile edit is accepted.
- **Implementation steps:** test the DN-03 profile-control result; enforce the proved option; inspect all supported shells and GUI launch state; add Doctor reporting for unexpected raw Nix PATH exposure; document that absolute invocation remains possible.
- **Tests:** login shell, non-login shell, clean environment, GUI app launch, existing profile content, repeated install, and uninstall profile residue.
- **Proof and evidence:** before-and-after environment snapshots for each supported launch context.
- **Deletion:** remove custom PATH manipulation only after equivalent product launch behavior is proved.
- **Rollback or stop rule:** stop cutover if `pkg` needs fragile shell mutation or if the vendor silently changes profiles.
- **Review focus:** UX statement versus security statement and absolute path use.
- **Child-unblock condition:** every PATH matrix row has a stable expected result.

### DN-12 — Add inactive Base Nix repair and update routing

- **Branch:** `dn/12-inactive-repair-update`.
- **Base:** DN-11 branch and PR.
- **Goal:** implement vendor Base Nix repair and the proved update policy behind an inactive production gate.
- **Why now:** full lifecycle cutover needs repair and update ready before install changes.
- **Likely files and symbols:** repair routing, `production_repair.rs`, Doctor, vendor executable module, Handoff identity update, package repair paths.
- **Interface and invariants:** Base Nix repair and package repair remain separate. The route is inactive in shipped behavior. No statement names `determinate-nixd` as update owner unless DN-03 proved it. A changed receipt or executable updates `Accepted` atomically.
- **Implementation steps:** classify repair requests. Route inactive Base Nix repair to the proved command. Retain package repair. Implement only the update owner and trigger proved in DN-03. Revalidate and atomically update accepted identity after vendor change. Report unsupported update policy.
- **Tests:** inactive-gate test; Base-only damage; package-only damage; combined damage; failed and interrupted repair; identity update; N to N+1; proved downgrade policy.
- **Proof and evidence:** state and ownership snapshots show one owner for each repair class. Shipped behavior remains on the current-alpha route.
- **Deletion:** none.
- **Rollback or stop rule:** stop if vendor repair changes package-owned state, identity update is not atomic, or update ownership is unclear.
- **Review focus:** inactive gating, operation classification, and package repair separation.
- **Child-unblock condition:** every repair class has one safe owner and the inactive route passes on both platforms.

### DN-13 — Add inactive vendor uninstall and full product cleanup

- **Branch:** `dn/13-inactive-uninstall`.
- **Base:** DN-12 branch and PR.
- **Goal:** prove a resumable full uninstall before either platform cuts over.
- **Why now:** install cannot cut over until its complete reverse operation exists.
- **Likely files and symbols:** `UninstallEngine`, platform uninstall modules, vendor executable module, lifecycle state, generations, activation forests, package roots, product asset managers.
- **Interface and invariants:** the route is inactive in shipped behavior. Full uninstall has no keep-state mode. Retained generations are invalid after Base Nix removes the store. Therefore full uninstall removes Lifecycle State, Generations, Activation Forests, registered package roots, product assets, and Base Nix.
- **Implementation steps:** verify every product and vendor identity before mutation. Write durable uninstall progress. Stop product services. Run the vendor uninstall. Record vendor completion. Remove exact product assets and all product state. Remove registered package roots and activation forests. Prove final residue. Resume safely from every recorded step.
- **Tests:** inactive-gate test; clean and repeated uninstall; interruption at every durable step; missing or damaged receipt; changed executable; foreign Nix refusal; product state and root removal; exact residue.
- **Proof and evidence:** pre/post ownership reports and step-by-step restart evidence show full removal and no unknown deletion.
- **Deletion:** none. Old uninstall remains active until platform cutover.
- **Rollback or stop rule:** stop if identity cannot be verified before mutation, vendor completion cannot be resumed, or exact product cleanup cannot be proved.
- **Review focus:** destructive order, durable progress, no keep-state path, and why generations cannot survive store removal.
- **Child-unblock condition:** both platforms resume every uninstall step and end with the exact empty owned-state result.

### DN-14 — Add an executable old-alpha reset and refusal path

- **Branch:** `dn/14-old-alpha-reset`.
- **Base:** DN-13 branch and PR.
- **Goal:** refuse old private alpha state and provide a reset command that is known to exist.
- **Why now:** the new lifecycle is ready, but users need a real route out of the last alpha.
- **Likely files and symbols:** existing-install detection, Doctor, installer preflight, last-alpha release assets, signed uninstaller metadata.
- **Interface and invariants:** old alpha is detected before mutation. New code does not import old receipts or journals. Instructions never print a command or binary that is absent.
- **Implementation steps:** identify a stable old-alpha marker. Prove that the last private-alpha uninstall command and binary still exist and work. If the new binary lacks that command, publish the signed last-alpha uninstaller with an exact digest and source inventory. Refuse new install until the old uninstaller completes and clean state is proved.
- **Tests:** old alpha complete; old alpha damaged; available old command; missing old command with signed asset; wrong uninstaller digest; partial uninstall; clean post-uninstall state.
- **Proof and evidence:** the displayed reset action executes from every supported old-alpha fixture. Foreign Nix cannot match the marker.
- **Deletion:** no compatibility bridge.
- **Rollback or stop rule:** stop if no executable and authenticated reset path exists or the old-alpha marker can match foreign Nix.
- **Review focus:** executable instructions, signed fallback asset, and zero silent adoption.
- **Child-unblock condition:** every old-alpha fixture either resets with a proved binary or refuses with a real recovery path.

### DN-15 — Cut over the complete Linux Base Nix lifecycle

- **Branch:** `dn/15-linux-lifecycle-cutover`.
- **Base:** DN-14 branch and PR.
- **Goal:** switch Linux install, repair, proved update, and uninstall together to the vendor lifecycle.
- **Why now:** all lifecycle operations, package parity, configuration, detection, PATH, and old-alpha reset are proved.
- **Likely files and symbols:** Linux bootstrap, inactive lifecycle routes, Handoff, product asset install, Doctor, release asset selection, and Linux user documents.
- **Interface and invariants:** no runtime fallback. Product assets remain owned by `pkg`. Vendor owns all Base Nix lifecycle operations. Full uninstall removes all product state as defined in DN-13. Unsupported hosts fail before mutation. Linux user documents describe the new Linux behavior in this PR.
- **Implementation steps:** enable the vendor lifecycle gate for Linux. Verify the authenticated asset. Run install through Handoff. Enable vendor repair and proved update. Enable resumable full uninstall. Run package and product-service smoke tests. Update Linux install, repair, update, and uninstall documents. Keep the old implementation present but unreachable for deletion proof.
- **Tests:** fake process integration and complete Linux VM matrix, including full lifecycle and product package operations.
- **Proof and evidence:** Linux x86_64 and released Linux aarch64 lifecycle reports pass twice from clean snapshots.
- **Deletion:** none. Old Linux Base Nix code remains until DN-17.
- **Rollback or stop rule:** revert before release if any lifecycle row fails. Do not add a runtime fallback.
- **Review focus:** one complete lifecycle owner, configuration preservation, Handoff, and full uninstall.
- **Child-unblock condition:** all blocking Linux lifecycle rows pass twice with no old runtime path used.

### DN-16 — Cut over the complete Apple Silicon macOS Base Nix lifecycle

- **Branch:** `dn/16-macos-lifecycle-cutover`.
- **Base:** DN-15 branch and PR.
- **Goal:** switch Apple Silicon macOS install, repair, proved update, and uninstall together to the vendor lifecycle.
- **Why now:** the shared lifecycle is proved on Linux and macOS-specific proof is complete.
- **Likely files and symbols:** macOS bootstrap, APFS detection, inactive lifecycle routes, Handoff, product launchd assets, release target selection, and macOS user documents.
- **Interface and invariants:** no runtime fallback. Vendor owns Base Nix APFS, daemon, repair, update policy, and uninstall work. `pkg` owns product services and package work. Intel macOS is not claimed without full proof. Apple Silicon user documents describe the new behavior in this PR.
- **Implementation steps:** enable the vendor lifecycle gate on Apple Silicon. Verify the authenticated asset. Run install through Handoff. Enable repair and proved update. Enable resumable full uninstall. Prove product launchd ownership and package behavior. Update macOS install, repair, update, and uninstall documents.
- **Tests:** fake process integration and complete Apple Silicon macOS VM matrix.
- **Proof and evidence:** complete clean, repeat, crash, reboot, repair, update, package, uninstall, and residue reports pass twice.
- **Deletion:** none. Old macOS Base Nix code remains until DN-18.
- **Rollback or stop rule:** revert before release if APFS, launchd, configuration, receipt, package, or lifecycle proof fails.
- **Review focus:** complete lifecycle ownership, APFS, launchd, target support, and no Intel claim.
- **Child-unblock condition:** all blocking Apple Silicon lifecycle rows pass twice with no old runtime path used.

### DN-17 — Delete proved Linux Base Nix implementation

- **Branch:** `dn/17-delete-linux-base`.
- **Base:** DN-16 branch and PR.
- **Goal:** remove only Linux Base Nix installer code made unreachable by the cutover.
- **Why now:** Linux production and uninstall proof have passed without fallback.
- **Likely files and symbols:** Base-Nix-only parts of `installer.rs`, `linux_accounts.rs`, `linux_filesystem.rs`, `linux_install_journal*.rs`, `linux_systemd.rs`, `linux_backend.rs`, `linux_platform_assets.rs`, `linux_uninstall.rs`, and `assets.rs`.
- **Interface and invariants:** keep Broker, Root Helper, product services, product assets, package roots, GC, repair, and shared high-fan-in contracts still used by package work.
- **Implementation steps:** compute callers for every candidate symbol; delete leaf code first; compile after each bounded group; remove Base-Nix-only tests after replacement proof exists; retain mixed files and surviving symbols.
- **Tests:** Linux cutover matrix, package lifecycle suite, product service tests, contract tests, and dependency tree check.
- **Proof and evidence:** deleted-symbol caller sets are empty and replacement tests are named.
- **Deletion:** only proved Linux Base Nix code and assets.
- **Rollback or stop rule:** stop deletion when a symbol still serves package or product ownership.
- **Review focus:** proof-before-delete and no whole-file assumptions.
- **Child-unblock condition:** Linux package and uninstall matrices still pass after deletion.

### DN-18 — Delete proved macOS Base Nix implementation

- **Branch:** `dn/18-delete-macos-base`.
- **Base:** DN-17 branch and PR.
- **Goal:** remove only macOS Base Nix, APFS, daemon, launchd, asset, and journal code made unreachable by cutover.
- **Why now:** macOS production and uninstall proof have passed without fallback.
- **Likely files and symbols:** Base-Nix-only parts of `macos_accounts.rs`, `macos_filesystem.rs`, `macos_install_journal*.rs`, `macos_launchd.rs`, `macos_backend.rs`, `macos_platform_assets.rs`, `macos_uninstall.rs`, `store_apfs.rs`, `store_mount.rs`, and `store_provision_macos.rs`.
- **Interface and invariants:** keep product launchd assets, package privilege paths, package roots, GC, package repair, and any macOS security code still used by product work.
- **Implementation steps:** trace each candidate symbol; separate mixed product and Base Nix behavior; delete only zero-caller Base Nix leaves; rerun Apple Silicon proof; do not claim or delete Intel behavior without proof.
- **Tests:** Apple Silicon cutover matrix, package lifecycle, product service tests, security tests, and dependency tree check.
- **Proof and evidence:** empty caller sets and named replacement evidence.
- **Deletion:** only proved macOS Base Nix and APFS lifecycle code.
- **Rollback or stop rule:** stop if an APFS, launchd, ACL, or security function still protects product state.
- **Review focus:** mixed ownership and platform residue.
- **Child-unblock condition:** Apple Silicon package and uninstall matrices pass after deletion.

### DN-19 — Delete shared private Base Nix provisioning and old release assets

- **Branch:** `dn/19-delete-private-runtime`.
- **Base:** DN-18 branch and PR.
- **Goal:** remove shared private runtime provisioning, obsolete Base Nix ownership records, obsolete Base Nix journals, installer bundles, and release artifacts.
- **Why now:** both platforms use the vendor owner, and platform-specific old paths are gone.
- **Likely files and symbols:** `pkg-nix/src/managed/{runtime_archive,installer_bundle,provision,daemon,accounts,ownership}.rs`, mixed journal code, release manifests, old runtime fixtures, Cargo manifests.
- **Interface and invariants:** keep package state, generation state, package recovery, package roots, GC, package repair, Broker and Root Helper package paths, `GenerationId`, and live `MaintenanceAdapter` behavior.
- **Implementation steps:** trace callers and feature use; remove old release targets; remove old authenticated runtime archives; delete Base-Nix-only receipt and journal fields; update fixtures; run `cargo tree` before dependency removal.
- **Tests:** workspace tests selected by impact, release-tool tests, package parity, clean install/uninstall proof, and cargo tree checks.
- **Proof and evidence:** no release references to old runtime artifacts and no live callers of deleted symbols.
- **Deletion:** proved private Base Nix provisioning, bundles, receipts, journals, assets, and now-unused dependencies.
- **Rollback or stop rule:** stop if a candidate dependency still supports package work or product service security.
- **Review focus:** transitive use, release reproducibility, and package-state preservation.
- **Child-unblock condition:** release metadata contains only the new Base Nix asset model and all core package tests pass.

### DN-20 — Complete core clean-host and release proof

- **Branch:** `dn/20-core-proof-docs`.
- **Base:** DN-19 branch and PR.
- **Goal:** prove the complete core system and update current user documents.
- **Why now:** code deletion is complete, so the final proof measures the new design before the release documents are finalized.
- **Likely files and symbols:** VM evidence, `docs/install.md`, `docs/commands.md`, `docs/support.md`, `docs/privacy.md`, release checklist.
- **Interface and invariants:** docs state the actual owner, diagnostics policy, PATH behavior, supported targets, repair, update, uninstall, and old-alpha refusal.
- **Implementation steps:** run the complete matrix from clean snapshots; publish reproducible evidence; build release metadata; install the built product; run package lifecycle; repair; update; uninstall; inspect residue; complete cross-platform, privacy, support, and release documents after the new results pass.
- **Tests:** full section 10 matrix and local release-tool tests.
- **Proof and evidence:** signed or hashed evidence bundle with exact build input and product version.
- **Deletion:** stale current user documentation and obsolete examples only.
- **Rollback or stop rule:** do not release if any blocking matrix row fails.
- **Review focus:** end-to-end ownership, docs accuracy, and no hidden old fallback.
- **Child-unblock condition:** core definition of done in section 13 is complete.

## 7. Optional simplification tail

Do not publish DN-21 until DN-20 is complete. These PRs simplify product code. They are not required for the Determinate cutover.

### DN-21 — Inventory final privilege and process seams

- **Branch:** `dn/21-seam-inventory`.
- **Base:** DN-20 branch and PR.
- **Goal:** record every live process, privilege seam, caller, and duty before simplification.
- **Why now:** core behavior is stable. Deletion needs a complete baseline.
- **Likely files and symbols:** Broker, Root Helper, `NixAdapter`, `MaintenanceAdapter`, vendor process, command engines, services, protocols.
- **Interface and invariants:** this PR observes only. It does not assume that a seam should be removed.
- **Implementation steps:** trace every caller. Record authentication, privilege, state, recovery, service, and log duties. Record the high-fan-in graph for each shared contract.
- **Tests:** inventory consistency checks and existing contract tests.
- **Proof and evidence:** every live duty has one current owner and named callers.
- **Deletion:** none.
- **Rollback or stop rule:** stop if any live process duty has no understood owner.
- **Review focus:** completeness and no desired-answer bias.
- **Child-unblock condition:** reviewers accept the full duty inventory.

### DN-22 — Move broad fakes to existing seams

- **Branch:** `dn/22-test-real-seams`.
- **Base:** DN-21 branch and PR.
- **Goal:** replace broad command fakes with the existing `NixAdapter`, `MaintenanceAdapter`, and vendor-process seams.
- **Why now:** DN-21 names the stable seams and their duties.
- **Likely files and symbols:** `pkg-testkit`, `e2e_fake.rs`, fake Nix, fake maintenance, fake executable, CLI tests.
- **Interface and invariants:** tests exercise real orchestration. Fakes implement existing seams. No new universal fake is added.
- **Implementation steps:** map each broad fake use. Replace it with the smallest existing seam. Keep real-process parity tests. Delete a broad fake only after all its cases move.
- **Tests:** migrated CLI, package, recovery, and installer tests.
- **Proof and evidence:** deliberate orchestration faults still fail tests.
- **Deletion:** broad command fake paths and unused fixtures.
- **Rollback or stop rule:** stop if a replacement fake copies internal implementation detail.
- **Review focus:** test fidelity and no new test-only architecture.
- **Child-unblock condition:** no production behavior depends only on the broad command fake.

### DN-23 — Replace command engines with one concrete Application

- **Branch:** `dn/23-concrete-application`.
- **Base:** DN-22 branch and PR.
- **Goal:** replace `CommandEngine`, `CoreOperations`, and `CoreEngine` with `Application::execute(request, progress)`.
- **Why now:** tests now use real seams and can protect the orchestration change.
- **Likely files and symbols:** the three engines, CLI dispatch, pipeline entry points, progress handling.
- **Interface and invariants:** use one concrete `Application`. Do not wrap old engines. Do not add an `Application` trait or `pkg-app` crate.
- **Implementation steps:** move behavior directly into the concrete application. Convert one command family at a time. Preserve errors and progress. Delete each old engine after its last caller moves.
- **Tests:** CLI contract and command tests.
- **Proof and evidence:** all commands reach the concrete method and no old engine remains.
- **Deletion:** `CommandEngine`, `CoreOperations`, `CoreEngine`, and duplicate dispatch.
- **Rollback or stop rule:** stop if `Application` becomes a service locator or wrapper around old engines.
- **Review focus:** direct replacement and smaller surface.
- **Child-unblock condition:** all package mutation commands use the concrete entry point.

### DN-24 — Centralize package-mutation recovery

- **Branch:** `dn/24-total-recovery`.
- **Base:** DN-23 branch and PR.
- **Goal:** run all required recovery once before package mutation and delete bypass paths.
- **Why now:** one application entry point exists.
- **Likely files and symbols:** lifecycle recovery, package journals, activation, GC, pending operations.
- **Interface and invariants:** recovery is idempotent. Read-only commands avoid mutation locks. Mutation begins only after recovery succeeds.
- **Implementation steps:** list current recovery entries. Order them by ownership and lock rules. Call existing recovery from `Application`. Delete direct mutation bypasses and duplicate recovery calls.
- **Tests:** injected crash-state table for every mutation command.
- **Proof and evidence:** every mutation sees the same recovered state.
- **Deletion:** bypass paths, duplicate recovery calls, and unreachable partial paths.
- **Rollback or stop rule:** stop if recovery order creates a cross-domain cycle.
- **Review focus:** total coverage, idempotence, and lock order.
- **Child-unblock condition:** no package mutation bypasses total recovery.

### DN-25 — Remove proved direct-Nix duplicates

- **Branch:** `dn/25-direct-nix-duplicates`.
- **Base:** DN-24 branch and PR.
- **Goal:** remove only direct Nix process code duplicated by existing adapters.
- **Why now:** application and recovery paths are stable.
- **Likely files and symbols:** direct process callers, `NixAdapter`, `RealNixAdapter`, pipeline acquire paths.
- **Interface and invariants:** resolution stays in `pkg-resolver`. The adapter exposes product operations, not raw command flags. Skip this PR if no duplicate exists.
- **Implementation steps:** inventory direct Nix calls. Compare each with existing adapter behavior. Move only exact duplicates. Leave resolver policy in `pkg-resolver`.
- **Tests:** resolver, acquire, parity, and package lifecycle tests.
- **Proof and evidence:** each deletion has an equal existing adapter path.
- **Deletion:** proved duplicate process and parsing code only.
- **Rollback or stop rule:** skip the PR if the inventory finds no duplicate. Stop if moving code changes ownership.
- **Review focus:** evidence-gated deletion and resolver boundary.
- **Child-unblock condition:** inventory is complete and all duplicates are removed or explicitly retained.

### DN-26 — Prove local-build admission and build-duty replacements

- **Branch:** `dn/26-build-admission-proof`.
- **Base:** DN-25 branch and PR.
- **Goal:** prove a replacement for multi-user local-build admission and every related build duty.
- **Why now:** Broker deletion cannot be considered without this proof.
- **Likely files and symbols:** build approval receipts, `build_authority*`, Broker build messages, acquisition, logs, cancellation, local command.
- **Interface and invariants:** untrusted users cannot approve privileged builds. Approval stays explicit and auditable. Every current build duty gets a replacement owner.
- **Implementation steps:** inventory admission, plan binding, replay protection, scheduling, execution, logs, progress, cancellation, completion, and audit. Implement a bounded proof replacement. Compare it with current safety.
- **Tests:** unauthorized user, replay, changed plan, concurrent approval, cancellation, lost client, approved build, logs, and audit.
- **Proof and evidence:** a multi-user adversarial result for every build duty.
- **Deletion:** none.
- **Rollback or stop rule:** retain Broker build ownership if one duty lacks equal proof.
- **Review focus:** complete build duties and privilege safety.
- **Child-unblock condition:** every build duty has a proved replacement or explicit Broker retain decision.

### DN-27 — Prove replacement owners for every privileged duty

- **Branch:** `dn/27-privilege-duty-proof`.
- **Base:** DN-26 branch and PR.
- **Goal:** prove a replacement or retain decision for every Broker and Root Helper duty.
- **Why now:** helper or Broker deletion needs complete evidence, not a partial package smoke test.
- **Likely files and symbols:** Broker, Root Helper, `MaintenanceAdapter`, services, datastore, logs, all protocol messages.
- **Interface and invariants:** no removal assumption. Multi-user safety, durability, cancellation, and recovery stay equal or stronger.
- **Implementation steps:** prove owners for operation begin, poll, cancel, and complete; channel refresh; Catalog search and info; resolve and acquire; builds; build and GC admission; root publication and attestation; repair; private datastore and logs; product service install, repair, update, and uninstall.
- **Tests:** capability-specific security, crash, concurrency, recovery, ownership, and service lifecycle tests.
- **Proof and evidence:** a complete duty matrix with current owner, candidate owner, proof, and final decision.
- **Deletion:** none.
- **Rollback or stop rule:** any missing or weaker replacement means the current process keeps that duty.
- **Review focus:** matrix completeness and equal security.
- **Child-unblock condition:** every live duty has a proved owner or retain decision.

### DN-28 — Delete or deepen the Root Helper

- **Branch:** `dn/28-root-helper-decision`.
- **Base:** DN-27 branch and PR.
- **Goal:** delete the Root Helper only if every helper duty has equal replacement proof.
- **Why now:** DN-27 completes the duty proof.
- **Likely files and symbols:** `pkg-root-helper`, helper protocol, service assets, `MaintenanceAdapter` implementation.
- **Interface and invariants:** the result follows evidence. A retained helper has a deep package-operation interface and no Base Nix lifecycle grammar.
- **Implementation steps:** apply the duty matrix. If deletion passes, remove service, client, protocol, assets, and dependencies in caller order. Otherwise retain it and delete only obsolete Base Nix duties.
- **Tests:** all DN-27 helper cases, package lifecycle, product service, and adversarial tests.
- **Proof and evidence:** every removed helper duty links to equal replacement proof.
- **Deletion:** conditional full helper deletion or narrow Base Nix grammar deletion.
- **Rollback or stop rule:** keep the helper if any duty lacks equal proof.
- **Review focus:** strict use of the stop gate.
- **Child-unblock condition:** the final helper ownership model is tested and documented.

### DN-29 — Delete or deepen the Broker

- **Branch:** `dn/29-broker-decision`.
- **Base:** DN-28 branch and PR.
- **Goal:** delete the Broker only if every Broker duty has equal replacement proof.
- **Why now:** build and full duty proofs are complete.
- **Likely files and symbols:** `BrokerLifecycleClient`, Broker binary, service, protocol, datastore, logs, package operations.
- **Interface and invariants:** Base Nix replacement alone never justifies full deletion. A retained Broker keeps a deep package interface and loses only obsolete Base Nix grammar.
- **Implementation steps:** apply DN-26 and DN-27. Trace the 65-node reach. Delete in leaf order only if every duty moved. Otherwise retain the Broker and prune only proved Base-lifecycle behavior.
- **Tests:** all Broker duties, authentication, cancellation, crash recovery, package lifecycle, service lifecycle, and concurrent users.
- **Proof and evidence:** every removed Broker duty has a tested owner.
- **Deletion:** conditional full Broker deletion or narrow Base-lifecycle grammar deletion.
- **Rollback or stop rule:** keep the Broker if one duty lacks equal proof.
- **Review focus:** high fan-in, duty completeness, and no forced deletion.
- **Child-unblock condition:** all package and product service duties work through the final process model.

### DN-30 — Prune dead transport grammar

- **Branch:** `dn/30-prune-transport`.
- **Base:** DN-29 branch and PR.
- **Goal:** delete only transport messages that have no live producer or consumer.
- **Why now:** final Broker and Helper topology is known.
- **Likely files and symbols:** `contract.rs`, `framing.rs`, request, response, and report variants.
- **Interface and invariants:** do not delete either file as a block. Keep strict decoding and all live process messages.
- **Implementation steps:** compute producers and consumers. Delete zero-use variants and conversions. Update message inventories. Do not relocate domain types here.
- **Tests:** contract round trips, framing rejection, package lifecycle, and process tests.
- **Proof and evidence:** before/after message inventory shows every survivor has a producer and consumer.
- **Deletion:** dead grammar, conversions, and dead grammar tests.
- **Rollback or stop rule:** retain a message if dynamic or platform use is not resolved.
- **Review focus:** transport only and proof by live endpoints.
- **Child-unblock condition:** every surviving message has a producer, consumer, and test.

### DN-31 — Relocate domain types only for a measured dependency gain

- **Branch:** `dn/31-domain-edge-reduction`.
- **Base:** DN-30 branch and PR.
- **Goal:** move high-impact domain types only when the move removes a dependency edge or enables crate deletion.
- **Why now:** transport is stable and the final dependency graph is measurable.
- **Likely files and symbols:** `GenerationId`, `MaintenanceAdapter`, `OwnershipExpectation`, `ManagedGroupBindings`, Cargo manifests.
- **Interface and invariants:** do not move a type for neatness. Preserve validation and semantics. Skip the PR if no measured gain exists.
- **Implementation steps:** model candidate moves. Count dependency edges before and after. Select at most one coherent move. Update imports mechanically. Delete an empty dependency only after target tests pass.
- **Tests:** affected contract, package, platform, and compile checks.
- **Proof and evidence:** graph evidence shows a removed edge or crate dependency.
- **Deletion:** old type location and dependency edge only if the move proves a gain.
- **Rollback or stop rule:** skip if there is no gain. Stop if fan-in makes the change risky without deletion value.
- **Review focus:** measured value and mechanical semantics.
- **Child-unblock condition:** the move passes or the recorded skip decision is reviewed.

### DN-32 — Complete simplification proof

- **Branch:** `dn/32-simplification-proof`.
- **Base:** DN-31 branch and PR, or DN-30 if DN-31 is skipped.
- **Goal:** measure the final design, prune proved-unused dependencies, and update architecture documents.
- **Why now:** all optional process and topology decisions are complete.
- **Likely files and symbols:** Cargo manifests, lockfile, code-health evidence, `CONTEXT.md`, ADR status notes, current docs.
- **Interface and invariants:** dependency removal follows `cargo tree` and target builds. No safety check is removed to improve a metric.
- **Implementation steps:** run dependency trees by target. Remove only unused crates. Run code-health and compare with baseline. Run affected tests and final VM smoke. Update docs for the retained Broker and Helper model.
- **Tests:** target builds, affected workspace tests, release tests, security tests, and final VM smoke.
- **Proof and evidence:** line, dependency, graph, test, and platform deltas with exact commands.
- **Deletion:** proved-unused dependencies and stale docs.
- **Rollback or stop rule:** restore a dependency if target-specific code still needs it.
- **Review focus:** evidence, not deletion count.
- **Child-unblock condition:** optional definition of done is complete.

## 8. PR dependency and test table

| PR | Merge blocker | Likely affected tests |
|---|---|---|
| DN-00 | Archive and links complete; plan reviews pass | link check, `git diff --check` |
| DN-01 | No failed pending operation remains live | CLI recovery, Broker lifecycle |
| DN-02 | Verify-only path has no writes | repair, leases, concurrency |
| DN-03 | Complete for standalone evidence; negative cleanup and crash results route to later owners | Linux/macOS VM spike matrix and accepted child reports |
| DN-04 | Next; every domain claim maps to DN-03 | documentation links and terminology |
| DN-05 | Exact assets and LGPL inventory complete | release manifest, signing, wrong digest |
| DN-06 | Exact safe process invocation | fake executable process tests |
| DN-07 | Crash states classify safely | handoff and receipt fault injection |
| DN-08 | Complete no-overlap asset partition | installer asset snapshots |
| DN-09 | Standard-daemon package parity | `pkg-nix`, `pkg-testkit`, package smoke |
| DN-10 | Unknown Nix always fails closed | detection table, Doctor, preflight |
| DN-11 | PATH matrix passes | shell, clean environment, GUI launch |
| DN-12 | Inactive repair/update routes are safe | repair classes, identity update, N to N+1 |
| DN-13 | Inactive full uninstall resumes exactly | uninstall steps, state/root removal, residue |
| DN-14 | Old-alpha reset action exists and authenticates | old-alpha refusal and signed uninstaller |
| DN-15 | Complete Linux lifecycle passes twice | Linux install, repair, update, uninstall, packages |
| DN-16 | Complete Apple Silicon lifecycle passes twice | macOS install, repair, update, uninstall, packages |
| DN-17 | Deleted Linux symbols have no live callers | Linux matrix and package lifecycle |
| DN-18 | Deleted macOS symbols have no live callers | macOS matrix and security tests |
| DN-19 | Old runtime absent from release graph | release, parity, dependency tree |
| DN-20 | Full core definition of done | complete VM and release matrix |
| DN-21 | Every live process and duty is inventoried | contract and inventory checks |
| DN-22 | Existing seams replace broad fakes | CLI, package, installer recovery |
| DN-23 | Old engines are replaced, not wrapped | CLI command and lifecycle tests |
| DN-24 | No package mutation bypasses recovery | crash-state table |
| DN-25 | Direct-Nix duplicate proof is complete | resolver, acquire, parity |
| DN-26 | Every build duty has a proved owner | adversarial build and cancellation |
| DN-27 | Every privileged duty has a proved owner | full duty capability matrix |
| DN-28 | Root Helper decision follows DN-27 | helper security, package and service lifecycle |
| DN-29 | Broker decision follows DN-26 and DN-27 | Broker auth, cancellation, package and service lifecycle |
| DN-30 | Every transport message has live endpoints | contract and framing round trips |
| DN-31 | A move removes an edge, or PR is skipped | affected contract and package checks |
| DN-32 | Target dependency proof and final checks pass | builds, affected tests, security, VM smoke |

## 9. Dependency decisions

### 9.1 Use

- Use the pinned external executable only.
- Execute it through one thin concrete module.
- Use absolute paths.
- Verify the asset digest before privilege.
- Keep the installed `nix` crate for user, process, signal, socket, and filesystem work that remains.
- Keep `tough` for authenticated channel and release metadata.
- Keep `hex` where strict digest encoding is required.
- Keep `futures-util` where current async control is smaller and safer than manual polling.
- Keep small strict encoders at trust and wire boundaries.
- Keep `jiff` where it already reduces strict time parsing. Evaluate new uses only when total code becomes smaller.

### 9.2 Reject

- Do not add the Determinate Rust crate.
- Do not copy upstream source files.
- Do not use upstream plan JSON as a product interface.
- Do not use `curl|sh`.
- Do not look up the vendor executable or Nix through PATH.
- Do not add a generic installer-provider framework.
- Do not add an `Application` trait.
- Do not add a `pkg-app` crate.
- Do not make a broad `thiserror` conversion.
- Do not add `strum` for trusted wire names.
- Do not add a generic Serde protocol envelope.
- Do not replace `futures-util` with manual polling only to reduce dependency count.

### 9.3 Prune only after proof

Use `cargo tree` for each supported target before removal.

- `tar` and `lzma-rust2` can go only after private runtime archive code is gone.
- `listenfd` can go only if no retained service uses socket activation.
- `socket2` can go only if retained Broker and CLI transport do not use it.
- `exacl`, `plist`, and `pkg-macos-security` can go only after target-specific product asset and privilege code proves them unused.
- A crate is not unused because Linux does not use it. Check Apple Silicon macOS too.

## 10. VM proof matrix

Evidence for each row records platform image, architecture, product revision, vendor version, vendor full revision, asset digest, exact invocation, exit status, logs, file ownership, services, receipt state, package state, and residue.

| Case | First proving PR | Linux x86_64 | Linux aarch64 | Apple Silicon macOS | Required result |
|---|---|---|---|---|---|
| Standalone vendor invocation and arguments | DN-03 | Blocking | Asset proof | Blocking | Exact observed arguments and exit behavior |
| Standalone diagnostics control | DN-03 | Blocking | Asset proof | Blocking | Proved endpoint or build policy |
| Standalone receipt and installed copy | DN-03 | Blocking | Asset proof | Blocking | Observed receipt and executable behavior |
| Standalone repeat install | DN-03 | Blocking | Sample | Blocking | Stable vendor outcome |
| Standalone SIGKILL and reboot | DN-03 | Blocking | Sample | Blocking | Observed vendor recovery behavior |
| Standalone repair and update | DN-03 | Blocking | Sample | Blocking | Exact vendor behavior and update owner |
| Standalone uninstall | DN-03 | Blocking | Sample | Blocking | Observed vendor-owned cleanup only |
| Wrong vendor digest | DN-05 | Blocking | Blocking | Blocking | Refuse before privilege and execution |
| Handoff crash before launch | DN-07 | Blocking | Sample | Blocking | `Started` persists and restart fails closed |
| Handoff crash after vendor success | DN-07 | Blocking | Sample | Blocking | Receipt identity can become `Accepted` atomically |
| Missing or damaged receipt | DN-07 | Blocking | Sample | Blocking | Refuse destructive work; no action-list parsing |
| Modified installed vendor executable | DN-07 | Blocking | Sample | Blocking | Detect identity change and refuse or atomically reaccept after proved lifecycle work |
| Standard-daemon package behavior | DN-09 | Blocking | Blocking before target release | Blocking | Required package parity passes |
| Broker daemon admission | DN-09 | Blocking | Sample | Blocking | Multi-user access follows one owned configuration path |
| Vendor config repair/update preservation | DN-09 | Blocking | Sample | Blocking | No vendor-owned file has a second writer |
| Foreign Nix | DN-10 | Blocking | Sample | Blocking | Refuse and preserve all files |
| Upstream Nix | DN-10 | Blocking | Sample | Blocking | Refuse and preserve all files |
| Unmarked Determinate | DN-10 | Blocking | Sample | Blocking | Refuse initial-alpha adoption |
| Login shell PATH | DN-11 | Blocking | Sample | Blocking | `pkg` works and observed raw Nix exposure matches policy |
| Non-login shell PATH | DN-11 | Blocking | Sample | Blocking | `pkg` works without profile assumptions |
| GUI launch PATH | DN-11 | Blocking | Sample | Blocking | `pkg` works from a clean GUI environment |
| Inactive Base Nix repair | DN-12 | Blocking | Sample | Blocking | Vendor-owned repair only |
| Inactive Base Nix update | DN-12 | Blocking | Sample | Blocking | Exact proved owner; no assumed `determinate-nixd` owner |
| Inactive full uninstall and resume | DN-13 | Blocking | Sample | Blocking | Durable full removal steps resume safely |
| Old private alpha reset | DN-14 | Blocking | Sample | Blocking | Refuse and provide an executable authenticated uninstaller |
| Complete clean install | DN-15 Linux; DN-16 macOS | Blocking | Blocking before target release | Blocking | Accepted Handoff and working product |
| Complete repeat install | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Stable complete lifecycle result |
| Complete repair and update | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Base and package repair stay separate |
| N to N+1 product upgrade | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | State, packages, Handoff, and identity remain valid |
| Downgrade | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Follow explicit proved policy |
| Complete explicit uninstall | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Remove all product state, product assets, roots, and Base Nix |
| Install, remove, and update package | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Generation and state transitions work |
| Local build | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Current approval and multi-user safety remain |
| Package roots and GC | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Active Generation and per-user isolation remain |
| Package repair | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Product-owned repair only |
| Modified product service asset | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Product repair detects and handles it |
| Final release ownership | DN-20 | Blocking | Blocking | Blocking | Each remaining asset has one owner |
| Final release residue | DN-20 | Blocking | Blocking | Blocking | No owned service, file, account, mount, root, state, or profile residue |
| Optional Root Helper removal | DN-28 | Blocking | Sample | Blocking | Every helper duty has equal replacement proof |
| Optional Broker removal | DN-29 | Blocking | Sample | Blocking | Every Broker duty has equal replacement proof |
| Optional transport and dependency pruning | DN-30 through DN-32 | Blocking | Target check | Blocking | Only dead grammar, edges, and dependencies are removed |
| Intel macOS | DN-03 asset probe; full proof not scheduled | Not applicable | Not applicable | Unsupported-target probe | Do not claim support without a full asset and lifecycle matrix |

`Blocking` means the PR cannot merge without the result. `Sample` means the architecture result must match the blocking platforms, but the release owner can define the exact repeated sample count. Linux aarch64 becomes fully blocking for all rows before a Linux aarch64 release.

## 11. Agent review synthesis

The report and this plan use work from GLM 5.3, DeepSeek Pro, and Qwen 3.8 Max. Advice is not accepted because of the model name. It is accepted only when repository and upstream evidence support it.

### 11.1 Accepted advice

- Use the external executable before considering a source fork.
- Keep the vendor process boundary thin.
- Split machine-wide Base Nix ownership from product package ownership.
- Delete old Base Nix code only after clean-host proof.
- Fix the failed pending-operation cancellation path.
- Shorten the verify-only repair lease.
- Move standard-daemon RealNix parity before production installer cutover.
- Move broad test fakes to the real vendor-process and Nix-adapter seams after those seams exist.
- Preserve exact trust-boundary encoders.
- Consider dependencies only when they remove more code than they add.

### 11.2 Adapted advice

- Advice to remove Broker and Root Helper was changed to proof gates. Determinate does not own package privilege work.
- Advice to replace the custom installer was narrowed to Base Nix lifecycle. Package roots, GC, repair, generations, and builds remain product work.
- Advice to simplify `Application` became one concrete `Application::execute` method. No trait or new crate is planned.
- Advice to adopt existing Determinate installations became fail-closed classification. Automatic adoption is optional after stable identity proof.
- Advice about update delegation became a DN-03 research question. The plan does not name `determinate-nixd` as owner without proof.

### 11.3 Rejected advice

- Do not use the experimental Rust library.
- Do not copy selected upstream Rust modules.
- Do not parse vendor plan JSON as a product interface.
- Do not add a provider framework for one vendor.
- Do not replace `futures-util` with manual polling.
- Do not add broad `thiserror`, `strum`, or generic Serde changes.
- Do not delete high-fan-in contracts as one block.
- Do not preserve backward compatibility for the old alpha.

### 11.4 Current alpha findings carried into the stack

1. `recover_pending_install` can return after a failed path without cancelling its Broker operation. A completion transport error can also leave the result uncertain. DN-01 reconciles completion and cancels any operation that remains live.
2. Verify-only repair can hold the exclusive state lease for too long. DN-02 releases it only after a Broker GC inhibitor or equal retention pin protects the verified Generation and roots.

## 12. Self-review checklist

Run this checklist on the plan and on every implementation PR.

### Ownership

- [ ] Does the PR name the Base Nix owner?
- [ ] Does the PR name the package-work owner?
- [ ] Does the PR avoid claiming that Determinate replaces package builds, generations, roots, GC, repair, Broker, or Root Helper without proof?
- [ ] Does every changed asset have one owner?

### Proof before deletion

- [ ] Is replacement behavior merged and proved before old code deletion?
- [ ] Are Linux and macOS cutovers complete before shared deletion?
- [ ] Does every deleted symbol have no live caller?
- [ ] Does every deleted test have named replacement evidence?
- [ ] Are mixed files edited by symbol instead of deleted blindly?

### Vendor boundary

- [ ] Is the executable pinned and digest-checked?
- [ ] Is it executed by absolute path?
- [ ] Are exact arguments supported by DN-03 evidence?
- [ ] Are diagnostics handled by proved policy?
- [ ] Is the receipt treated as opaque?
- [ ] Is plan JSON absent from product interfaces?
- [ ] Is there no `curl|sh`, PATH lookup, Rust crate, copied source, or provider framework?

### Safety

- [ ] Does unknown Nix fail closed?
- [ ] Does the code avoid deleting unknown `/nix` content?
- [ ] Does the old alpha refuse with clean reset instructions?
- [ ] Are crash, reboot, repeat, wrong-digest, and residue cases covered?
- [ ] Are package and Base Nix repair separate?
- [ ] Are privilege changes supported by multi-user proof?

### Stack quality

- [ ] Does the PR target the immediate parent?
- [ ] After a squash merge, were all descendants restacked in order and were all merge-bases and PR diffs checked?
- [ ] Is the result reviewable without the child?
- [ ] Is the stop rule explicit?
- [ ] Is rollback possible before merge?
- [ ] Are local commands and exact results recorded?
- [ ] Does the PR avoid unrelated cleanup?

### Simple language and document checks

- [ ] Does each sentence state one main idea?
- [ ] Are observations labeled as observations?
- [ ] Are future decisions labeled as proof work?
- [ ] Do relative links work?
- [ ] Are branch names, PR numbers, and dependency order consistent?

## 13. Definition of done

### Core migration is done after DN-20 when:

- The pinned vendor executable is the only production Base Nix install path on supported targets.
- Linux x86_64, released Linux aarch64, and Apple Silicon macOS pass the required matrix.
- The release system authenticates the exact executable and records LGPL-2.1 and source inventory.
- Diagnostics behavior is explicit and proved.
- Vendor Receipt and Handoff recovery fail closed.
- Standard-daemon package parity passes before and after cutover.
- Existing foreign, upstream, unmarked Determinate, and old-alpha Nix refuse safely.
- PATH behavior matches the documented user experience.
- Base Nix repair, update policy, and uninstall have one proved owner.
- Package repair, builds, generations, roots, GC, and state remain correct.
- Old Linux, macOS, and shared private Base Nix code is deleted only where replacement proof exists.
- Broker, Root Helper, and high-fan-in contracts remain where package work still needs them.
- Current user documents match the observed product.
- All local proof results are attached because GitHub Actions remain disabled.

### Optional simplification is done after DN-32 when:

- Package mutations use one concrete application entry point.
- Recovery runs once before mutation.
- Direct Nix duplicate cleanup changed no `pkg-resolver` ownership, or DN-25 was skipped because no duplicate existed.
- Root Helper and Broker decisions follow evidence, not the desired deletion count.
- Live transport grammar has a producer, consumer, and test.
- Domain types move only for a measured dependency-edge or crate-deletion gain.
- Dependencies are removed only after per-target use proof.
- The final code-health report shows the before and after state.

## 14. Not implemented by this plan branch

DN-00 changes `plans/**` plus only required documentation and link-check maintenance. It does not:

- download, vendor, execute, or integrate the Determinate executable;
- change install, repair, update, uninstall, Broker, or Root Helper behavior;
- add a release asset or digest;
- enable GitHub Actions;
- add Graphite or another stack tool;
- add a compatibility bridge for the old alpha;
- adopt an existing Determinate or foreign Nix installation;
- claim Intel macOS support;
- decide that `determinate-nixd` owns updates;
- delete any runtime code or dependency;
- change package state, generations, roots, GC, builds, or repair.

The first code change is DN-01. The first vendor proof is DN-03. The first complete production cutover is DN-15. The first old Base Nix code deletion is DN-17.
