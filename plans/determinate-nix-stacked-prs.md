# Determinate Nix stacked PR implementation plan

Status: active implementation plan for an alpha product.

Current status: **DN-00 through DN-07 are complete. The grouped DN-08–14 PR is active as a partial foundation and evidence PR. Clean-host DN-15 can start after PR 2 lands.**

The current migration delivery has exactly four delivery PRs. PR 1 and PR 2 group several work packages. PR 3 has the single delivery label DN-15. PR 4 has the single delivery label DN-16. DN-17 through DN-32 remain work-package IDs inside PR 4. They do not create more delivery PRs.

| Delivery PR | Scope | Status |
|---|---|---|
| 1 | DN-00 through DN-07: plan, alpha fixes, evidence, contract, and authenticated vendor foundation | Complete |
| 2 | DN-08 through DN-14: inactive integration foundation and decision evidence | Active and partial |
| 3 | DN-15: Linux completion | Ready for clean-host Linux after PR 2 |
| 4 | DN-16: Apple Silicon macOS completion; DN-17 through DN-32 are later cleanup, proof, and optional simplification checkpoints inside this PR | Blocked by Linux completion and macOS proof |

PR 2 does not deliver the complete DN-08 through DN-14 lifecycle. It records safe inactive code, evidence limits, and the accepted ownership policy. After PR 2 lands, DN-15 can start on a clean Linux host. DN-12, DN-13, and DN-14 do not block that clean-host work.

DN-03 completed the standalone vendor evidence gate. This does not mean that vendor uninstall is clean. It does not mean that crash recovery succeeds. Linux functional behavior checks passed, but strict vendor cleanup failed. In the accepted macOS crash observation, state validation stops after the recovery install exits 0 because `_nixbld1` is missing.

The public evidence is in the [DN-03 parent decision](../spikes/s6-determinate-installer/FINDINGS.md), the [Linux findings](../spikes/s6-determinate-installer/linux-vm/LINUX-FINDINGS.md), and the [macOS lifecycle, residue, and crash findings](../spikes/s6-determinate-installer/macos-vm/FSTAB-CONTRACT-RESEARCH.md).

- Linux R12 proves broad Linux x86_64 behavior. Retained x86_64 R11 and aarch64 R10 prove the two Linux target Asset records.
- macOS R10 completes the standalone lifecycle and residue evidence. Its functional lifecycle and reboots passed, but strict vendor cleanup failed. Crash R1 completes the required negative SIGKILL and reboot observation.
- Clean vendor uninstall remains false on both platforms. DN-13 uses the fixed installed vendor executable and receipt paths. Determinate owns any self-copy and vendor residue. Vendor-owned residue is an accepted alpha limit. `pkg` removes only product-owned residue.
- Successful crash recovery is unproved. DN-06 and DN-07 delivered the product controls. DN-16 owns the later crash-recovery proof. The companion DN-12 report concludes that there is no safe general vendor repair route. That report must land in PR 2 before the plan treats this result as accepted evidence.
- Linux alpha proof can use a disposable privileged Docker container with systemd. It does not prove boot, reboot, SELinux, foreign-host behavior, or a full distribution matrix.
- macOS proof needs an Apple Silicon macOS VM or another disposable Mac. Docker cannot prove launchd, APFS, or `diskutil` behavior.
- DN-04 documents the proved ownership and executable contract.

This plan replaces the old custom Managed Nix implementation plan. The old plan is preserved in the [dated legacy archive](archive/2026-08-22-custom-managed-nix-v1/README.md). The design reasons and research are in the [architecture report](../architecture-report.html).

This plan update does not change shipped behavior or current user instructions. DN-15 and DN-16 update the user documents for each platform when that platform changes. DN-20 completes the release documents after the final proof.

## 1. Accepted ownership

The accepted product boundary is:

- Determinate owns the machine-wide **Base Nix lifecycle**.
- Base Nix lifecycle means Base Nix install, supported repair, update, Base Nix service setup and initialization, and uninstall.
- `pkg` owns pinned executable authentication, invocation, bounded progress, process supervision, health and support policy, installed-state validation, Handoff, and redacted error reporting.
- `pkg` does not implement a second Base Nix lifecycle engine or exact cleanup for vendor-owned residue.
- `pkg` owns package selection, package builds, package state, generations, activation, package roots, package garbage collection, package repair, and the product user experience.
- `pkg` can keep its Broker and Root Helper for package work.
- The plan does not assume that Determinate replaces the Broker or Root Helper.
- The plan does not assume that Determinate replaces package roots, package garbage collection, or package repair.
- Raw Nix can exist on the machine. `pkg` keeps raw Nix out of its normal user experience. This is not a security boundary.
- Local administrators can change machine-wide Nix. `pkg doctor` must detect important changes and fail closed where ownership is not clear.
- Old private-alpha installations are a separate migration case. They do not block clean-host work. `pkg` must detect them and refuse unsafe automatic mutation. The product must not show a reset command that it cannot authenticate and run.

Alpha update trust is explicit. `pkg` authenticates the pinned outer Determinate installer and invokes vendor programs through fixed command paths. For `determinate-nixd upgrade`, `pkg` accepts Determinate's inner download and update trust chain. It does not pre-bind or re-authenticate the downloaded daemon or profile payload. After update, `pkg` runs functional installed-state health validation and reports failure. It does not create a second update ledger or extend Handoff only to mirror vendor update state. This deliberate alpha security trade-off trusts Determinate for the inner payload and keeps one update engine.

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
- Delivery branches use the names recorded in the four-PR table and section 6.
- A DN label is a logical work-package and checkpoint ID inside a delivery PR.
- A DN label does not create a separate delivery PR.
- Historical branch names in completed checkpoint descriptions record where the evidence originated. They do not add delivery PRs.

### 3.2 PR bases

- PR 1 uses `dn/05-07-vendor-foundation`. DN-00 through DN-04 branch names are historical checkpoint sources inside PR 1.
- PR 2 uses `dn/08-14-lifecycle-integration` and targets PR 1.
- PR 3 uses `dn/15-linux-lifecycle-cutover` and targets PR 2.
- PR 4 uses `dn/16-macos-lifecycle-cutover` and targets PR 3.
- There is one published linear stack.
- Do not publish two competing versions of the same stack.
- Do not add Graphite or another stacking dependency.

### 3.3 Required PR description

Each PR description has these fields:

1. **Parent**: the parent PR number and branch.
2. **Goal**: one result that this PR provides.
3. **Ownership change**: what Determinate owns and what `pkg` still owns.
4. **Invariants**: rules that must remain true.
5. **Proof**: tests, platform evidence, logs, digests, and residue checks.
6. **Deletion**: code removed in this PR, or `none`.
7. **Stop rule**: the result that blocks the child PR.
8. **Rollback**: how to remove the change before merge.
9. **Risk**: the highest remaining risk.
10. **Not included**: work that belongs to a later PR.

For a grouped PR, the description reports each stable DN work area as delivered, evidence-only, or blocked. DN labels are not commit boundaries unless that group states an explicit boundary. Work areas can be combined or reordered when the group has no required sequence and dependencies remain safe. The final delivery PR must be green, signed, and reviewed as a whole. A partial PR must not claim that blocked behavior exists.

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

Do not leave a descendant based on an old pre-squash commit. Do not merge a child before its parent. Do not combine published stack entries during merge. PR 1 and PR 2 are grouped delivery PRs from the start. PR 3 and PR 4 keep the single delivery labels DN-15 and DN-16.

### 3.5 Local proof policy

GitHub Actions are disabled today. This plan does not enable them.

- Each PR records exact local commands and exact results.
- Platform evidence includes the image, architecture, date, input asset digest, output logs, and residue report.
- A reviewer must be able to repeat the proof.
- A skipped platform check blocks a production cutover for that platform.
- Local proof is not optional because remote Actions are disabled.

## 4. Dependency diagram and stop gates

```text
PR 1: DN-00 through DN-07 [COMPLETE FOUNDATION]
  |
PR 2: DN-08 through DN-14 [ACTIVE PARTIAL FOUNDATION AND EVIDENCE]
  |
PR 3: DN-15 [LINUX COMPLETION]
  |
PR 4: DN-16 [MACOS COMPLETION]
      DN-17 through DN-20 [POST-CUTOVER CLEANUP AND CORE PROOF]
      DN-21 through DN-32 [OPTIONAL SIMPLIFICATION CHECKPOINTS]
```

Stop gates:

- **DN-03** is complete for standalone evidence. Its negative results define product limits and later platform proof.
- **DN-05–07** blocks integration if asset authentication, safe process execution, or minimal Handoff recovery fails. It must not add a second vendor journal.
- **DN-08–14** is an inactive partial foundation and evidence PR. PR 2 removes the unused ownership partition and its exact-partition tests while preserving the normalized install inventory. It must land its evidence reports and accurate inactive behavior. The accepted policy supersedes the old DN-12, DN-13, and DN-14 clean-host blockers. Determinate owns supported repair, update, and uninstall. Vendor residue is accepted for alpha. Old private-alpha migration is separate.
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
| Unused install ownership partition | `InstallAssetOwner` and exact-partition tests | Delete. Preserve the normalized install inventory instead of adding a second ownership view. | PR 2 / DN-08. |
| Package build | `crates/pkg-nix/src/{adapter,real,build,verify,substitute}.rs` | Keep. Add standard-daemon parity before cutover. | No core deletion. |
| Package lifecycle | `crates/pkg-pipeline`, `crates/pkg-core`, `crates/pkg-store` | Keep. Determinate does not provide this product behavior. | Optional simplification only in DN-21 through DN-32. |
| Broker and helper | `crates/pkg-installer/src/{broker,helper,root_client,service}.rs`, binaries | Keep for package work until proof says otherwise. | DN-28 and DN-29 only after DN-26 and DN-27 pass. |
| Product service assets | product broker/helper services, product policy, product state | Keep. They remain owned by `pkg`. | Delete only a proved unused product asset. |
| Linux Base Nix install | `installer.rs`, `linux_*`, Base-Nix parts of `assets.rs` and `bootstrap.rs` | Replace with the vendor executable after Linux cutover proof. | DN-17, and only proved Base-Nix parts. |
| macOS Base Nix install | `macos_*`, `store_apfs.rs`, Base-Nix launchd and filesystem parts | Replace with the vendor executable after macOS cutover proof. | DN-18, and only proved Base-Nix parts. |
| Private runtime provisioning | `pkg-nix/src/managed/{runtime_archive,installer_bundle,provision,daemon,accounts}.rs` | Keep until both platforms use the vendor lifecycle and package parity passes. | DN-19, by proved symbol and caller set. |
| Base Nix ownership and journals | `managed/ownership.rs`, platform install journals, store/repair journals | Keep until handoff and vendor receipt behavior are proved. | DN-17 through DN-19. Keep package journals. |
| Uninstall | `UninstallEngine`, platform uninstall modules | Authenticate and directly invoke `/nix/nix-installer` with `/nix/receipt.json`. Keep process, cancellation, and Handoff controls. Determinate owns self-copy and residue. Remove product-owned state. | The DN-13 subphase starts the inactive path. DN-17 through DN-19 remove obsolete Base-Nix paths. |
| Wire contracts | `pkg-nix/src/{contract,framing}.rs` | Keep during core migration. These files mix live product grammar with candidate obsolete grammar. | DN-30 deletes only dead messages after caller and test proof. |
| Release assets | `tools/release`, channel metadata, runtime manifests | Add the pinned vendor executable first. Keep old assets until both cutovers pass. | DN-19 removes old Base-Nix artifacts. |
| Tests | package, contract, parity, recovery, and platform tests | Keep and adapt. Move fakes only after the production seam changes. | DN-22 deletes broad fakes after replacement tests pass. |

## 6. Delivery PR work packages

DN-00 through DN-32 are work packages and checkpoints inside the four delivery PRs. They are not separate delivery PRs. Completed checkpoint branch names below are historical evidence only.

### DN-00 work package — Archive the old plan and publish this plan

- **Status:** complete.
- **Historical source branch:** `plan/determinate-nix-stacked-prs`.
- **Delivery PR:** PR 1 checkpoint.
- **Goal:** preserve the old plan and publish one active, reviewed stack plan.
- **Why now:** implementation needs one source of order, ownership, and stop rules.
- **Likely files and symbols:** `plans/**` plus only the documentation and link-check maintenance required by the archive move.
- **Interface and invariants:** no runtime behavior changes; old history stays readable; the active plan links to the archive and architecture report.
- **Implementation steps:** move old plan files into the dated archive; add an archive notice; add this plan; check every relative link; record that GitHub Actions stay disabled.
- **Tests:** run a local Markdown link check or inspect every relative path; run `git diff --check`.
- **Proof and evidence:** archive file list, active file list, valid links, and clean whitespace check.
- **Deletion:** none. This work package archives files instead of deleting them.
- **Rollback or stop rule:** stop if any old plan file is missing from the archive.
- **Review focus:** history preservation, stack completeness, and no runtime edits.
- **Child-unblock condition:** two plan reviews pass and all blocking comments are resolved.

### DN-01 work package — Cancel every failed pending-install Broker operation

- **Status:** complete.
- **Historical source branch:** `dn/01-cancel-pending-install`.
- **Delivery PR:** PR 1 checkpoint after DN-00.
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

### DN-02 work package — Shorten the verify-only repair lease

- **Status:** complete.
- **Historical source branch:** `dn/02-short-repair-lease`.
- **Delivery PR:** PR 1 checkpoint after DN-01.
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

### DN-03 work package — Prove the vendor executable contract

- **Status:** complete for standalone evidence. This checkpoint did not approve product delivery by itself.
- **Historical source branch:** `dn/03-determinate-nix-spike`.
- **Delivery PR:** PR 1 checkpoint after DN-02.
- **Goal:** produce reproducible Linux and Apple Silicon macOS evidence for the exact external executable behavior.
- **Why now:** every integration decision depends on recorded standalone facts. The accepted DN-03 reports now provide those facts for DN-04.
- **Likely files and symbols:** a bounded spike under `spikes/`, evidence manifests, platform proof scripts, no production module.
- **Interface and invariants:** execute a pinned file by absolute path; verify its digest before privilege; never use `curl|sh`; never use PATH lookup; do not parse plan JSON as a product interface.
- **Implementation steps:** acquire version 3.22.1 for each supported target. Record the full revision and digest. Test exact observed arguments and argument order. Prove diagnostics control. Install the executable on standalone clean hosts. Inspect `/nix/receipt.json` and `/nix/nix-installer`. Test standalone repeat install, SIGKILL, reboot, repair, update, and uninstall behavior. Test foreign and existing Nix only as inputs to the standalone executable. Record Intel macOS asset availability.
- **Tests:** the DN-03 rows in the platform matrix in section 10 were observed. Linux R12 proves broad Linux x86_64 behavior. Retained x86_64 R11 and aarch64 R10 prove the two target Asset records. macOS R10 proves lifecycle and residue behavior. Crash R1 records an accepted negative crash result after `_nixbld1` is missing.
- **Proof and evidence:** the [parent decision](../spikes/s6-determinate-installer/FINDINGS.md) links the accepted public results. The [Linux report](../spikes/s6-determinate-installer/linux-vm/LINUX-FINDINGS.md) owns the Linux evidence. The [macOS report](../spikes/s6-determinate-installer/macos-vm/FSTAB-CONTRACT-RESEARCH.md) owns R10 and Crash R1. DN-03 does not prove `pkg` Handoff, package lifecycle, product repair, product uninstall, product cleanup, or production cutover.
- **Deletion:** none.
- **Rollback or stop rule:** DN-03 evidence is complete. DN-06 and DN-07 own fail-closed product controls. DN-16 owns later macOS crash-recovery proof. DN-13 records accepted vendor residue instead of creating a cleanup engine. The companion DN-12 report must land in PR 2 before its capability limits are accepted. Do not copy or fork vendor source.
- **Review focus:** observations versus conclusions, repeatability, licensing, diagnostics, update owner, and crash recovery.
- **Child-unblock condition:** complete. DN-04 can document the proved contract and its limits. This does not unblock product cutover.

### DN-04 work package — Update domain context and add ADR 0004

- **Status:** complete.
- **Historical source branch:** `dn/04-determinate-base-nix-contract`.
- **Delivery PR:** PR 1 checkpoint after DN-03.
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

### DN-05–07 work-package group — Ship the authenticated vendor foundation

- **Status:** complete.
- **Branch:** `dn/05-07-vendor-foundation`.
- **Delivery PR:** PR 1, after the DN-04 checkpoint.
- **Goal:** ship pinned vendor assets, one private process Adapter, and minimal Vendor Receipt and Handoff validation in one review.
- **Order:** DN-05, DN-06, and DN-07 are ordered work-package and checkpoint IDs. Implement them in this order. End each checkpoint with one signed, green, independently reviewable commit.
- **Deletion:** none. Keep old Base Nix assets until both platform cutovers pass. This PR adds no second receipt or vendor action journal.
- **Combined stop rule:** do not merge if source compliance, a supported target asset, exact safe invocation, or deterministic Handoff recovery is missing.

**DN-05 subphase — Pinned vendor assets**

- **Goal and files:** authenticate and inventory the executable in `tools/release/src/manifest.rs`, `tools/release/src/sign.rs`, channel target fixtures, software inventory, and the installer release schema.
- **Interface:** use one exact target and digest for each supported system. Record LGPL-2.1 and the matching source location. State diagnostics and downgrade policy explicitly. Never use an ambient download.
- **Work:** add per-target records and digest metadata. Add the license notice and corresponding-source inventory. Add diagnostics metadata only when DN-03 proves it stable. Update fixtures and release validation.
- **Tests and proof:** run release manifest and signature tests. Test wrong digests, unsupported systems, and incomplete inventory. Generated metadata must identify one exact executable per target.
- **Gate and review:** stop if source compliance or a supported target asset is missing. Review the trust chain, target map, license duties, and rejection of any changed executable.

**DN-06 subphase — Private process Adapter**

- **Goal and files:** add one concrete vendor process Adapter in `pkg-installer`. Reuse `std::process::Command` or the current async process support. Add installer errors and fake-executable tests.
- **Interface:** use an absolute authenticated path and only proved arguments. Set or scrub the required environment. Drain stdout and stderr concurrently into bounded storage. Keep private data out of logs. Do not kill on timeout unless DN-03 proves recovery. Send only proved signals. A lost client must not silently orphan or kill a privileged child. Add no trait or provider framework.
- **Work:** define one concrete `DeterminateInstaller`. Add only the install, repair, proved-update, and uninstall calls supported by DN-03. Build commands in one place. Reject a missing or changed executable. Model client loss, timeout, and signal results explicitly. Use a fake executable that records arguments and controls output.
- **Tests and proof:** test arguments, environment, exit codes, both-pipe back-pressure, bounded storage, log redaction, client disconnect, timeout without an unproved kill, proved signals, and a wrong file. The tests must prove exact invocation without root or a VM.
- **Gate and review:** remove the Adapter if it requires plan JSON, source embedding, or a broad abstraction. Review process safety, cancellation, minimal surface, and every accepted external outcome.

**DN-07 subphase — Vendor Receipt and Handoff**

- **Goal and files:** persist only the minimum restart state in `pkg-installer` bootstrap state, an opaque receipt validator, and current installer recovery entry points.
- **Interface:** no Handoff state means `NotStarted`. Persist only `Started` and `Accepted`. `Accepted` stores the minimum stable executable and receipt identity. Do not parse or copy the vendor action list. Never delete unknown `/nix` content.
- **Work:** durably write `Started` before execution. Validate the vendor result. Atomically write `Accepted`. Classify and report state on restart. Atomically update identity after a proved repair or update changes the executable or receipt.
- **Tests and proof:** inject a crash before launch, during launch, and after vendor success but before state update. Test missing or damaged receipts, changed executables, and unknown `/nix`. Every state must fail closed and produce deterministic restart classification.
- **Gate and review:** stop if safe recovery needs private vendor action replay. Review durability, the opaque seam, and the absence of a parallel ownership ledger.

### DN-08–14 work-package group — Build the inactive lifecycle integration

- **Status:** active and partial. This PR contains inactive foundation code and decision evidence. It does not complete DN-08 through DN-14.
- **Branch:** `dn/08-14-lifecycle-integration`.
- **Delivery PR:** PR 2, based on PR 1.
- **Goal:** keep the safe inactive integration foundation and record the accepted lifecycle ownership policy and its evidence limits.
- **Execution:** DN-08 through DN-14 are logical work areas inside one delivery PR. They are not required commit boundaries or a required chronological commit order. Implementation can combine or reorder them when dependencies remain safe. Keep every route inactive in shipped behavior. The final PR must be green, signed, and reviewed as a whole.
- **Combined deletion:** delete the unused ownership partition and its exact-partition tests. Preserve the normalized install inventory. Keep old detection and PATH code unchanged. DN-17 through DN-19 own any later deletion of obsolete Base Nix detection or PATH code after platform cutover proof. Add no compatibility bridge.
- **Combined stop rule:** do not call this PR a complete lifecycle delivery. PR 2 must preserve the normalized inventory and include its evidence reports. After it lands, clean-host DN-15 can start. Platform behavior remains inactive until DN-15 or DN-16 proves and enables it.

Current work-area result:

- **DN-08:** simplified. PR 2 deletes the unused ownership partition and its exact-partition tests. It preserves the normalized install inventory. A supported vendor configuration extension is NO-GO. The inactive typed Root Helper proxy replaces that rejected design.
- **DN-09:** partial. The standard Determinate adapter mode and typed Root Helper proxy are inactive. Live Linux and macOS parity still need proof.
- **DN-10:** partial. Inactive classification and Doctor behavior exist. The privileged producer and platform proof remain gated.
- **DN-11:** evidence is partial. Production PATH behavior stays unchanged. Platform launch-context proof remains gated.
- **DN-12:** the companion PR-2 evidence report records the vendor capability limits. The report must land before PR 2 can claim this proof. Determinate owns supported Base Nix repair and update. Package Repair stays product-owned. No speculative product repair or update engine is added.
- **DN-13:** partial. The inactive path authenticates and directly invokes fixed `/nix/nix-installer` with fixed `/nix/receipt.json`. Existing process, cancellation, and Handoff controls remain. Determinate owns self-copy and vendor residue. `pkg` removes product-owned state.
- **DN-14:** NO-GO for an old-alpha reset route. There is no authenticated fallback executable. Old private-alpha migration is separate and does not block clean hosts. No dead refusal or reset code is added.

**DN-08 subphase — Remove the unused ownership partition**

- **Files and interface:** update `assets.rs`, platform asset lists and managers, `InstallAssetOwner`, and exact-partition tests. Preserve the normalized install inventory and stable output. `pkg` does not edit vendor-owned configuration.
- **Work:** delete the unused ownership partition and its exact-partition tests. Keep the normalized install inventory. The vendor configuration extension is NO-GO. Use the inactive typed Root Helper proxy for package Nix operations. Do not add a second writer for vendor-owned configuration.
- **Tests and gate:** prove that the normalized install inventory and stable output are unchanged. Linux and macOS live preservation proof remains a cutover gate. Review inventory preservation and the one-writer configuration rule. Do not add a replacement owner field.

**DN-09 subphase — Standard-daemon RealNix parity**

- **Files and interface:** update `RealNixAdapter`, `crates/pkg-nix/src/{real,adapter,build,verify,substitute}.rs`, parity fixtures, and `pkg-testkit`. Use one fixed standard-daemon mode. Add no provider framework. Preserve package trust and outcomes. Use absolute Nix paths or one proved stable rule, never PATH.
- **Work:** bind proved daemon and store paths. Route package Nix operations through the inactive typed Root Helper proxy. Prove Broker admission and multi-user access. Probe read, resolve, acquire, substitute, local build, roots, GC, and package repair. Do not depend on the rejected vendor configuration extension.
- **Tests and gate:** run real adapter tests, parity tests, and install, remove, update, upgrade, and GC smoke tests on Linux and Apple Silicon macOS. Attach a standard-daemon parity report for both platforms. Stop for unexplained differences in trust, roots, access, build, GC, or configuration ownership. Keep private mode until both cutovers pass. Review package behavior, multi-user safety, and fixed configuration.

**DN-10 subphase — Existing and foreign Nix classification**

- **Files and interface:** update managed detection, Doctor, bootstrap preflight, and receipt and executable validation. Accept only a clean host or stable Handoff from the new flow. Report foreign Nix, upstream Nix, unmarked Determinate, damaged accepted state, and old alpha separately. All unsafe states block automatic install.
- **Work and tests:** classify only observable facts. Add clear Doctor actions and installer refusal. Test clean, accepted, foreign, upstream, unmarked Determinate, damaged accepted, and old-alpha fixtures. Each fixture must map to one stable classification and one safe action. Keep automatic adoption as future work. Keep all current production detection code in place.
- **Gate:** every unknown state must fail before privilege or mutation. Stop if two unsafe states can produce accepted identity. Never repair, adopt, or delete unknown state automatically. Review false acceptance, user instructions, and the lack of auto-adoption.

**DN-11 subphase — PATH behavior**

- **Files and interface:** update proved installer options, `crates/pkg-cli/src/path.rs`, shell tests, Doctor output, and platform evidence. `pkg` never locates the vendor executable or Nix through PATH. Normal use must work in login, non-login, clean non-login, and GUI environments. Raw Nix visibility is not a security boundary.
- **Work and tests:** enforce the DN-03 profile-control result. Inspect supported shells and GUI launch state. Report unexpected raw Nix exposure. Test existing profile content, repeat install, uninstall residue, and every launch context. Record before-and-after environments.
- **Gate:** stop if `pkg` needs fragile shell mutation or the vendor silently changes profiles. Keep current production PATH code in place. Record any proved-obsolete PATH code for deletion by DN-17 through DN-19 after platform cutover. Review the user-experience statement separately from the security statement and confirm absolute path use.

**DN-12 subphase — Base Nix repair and update policy**

- **Decision:** Determinate owns supported Base Nix repair and update. The companion PR-2 report shows that there is no general repair command and that update interruption is not a proved product contract. These are support limits, not reasons to build a second engine. The report must land before PR 2 claims this evidence. Package Repair stays product-owned.
- **Work:** use only vendor operations that the vendor supports. Authenticate the pinned outer installer and use fixed command paths. For `determinate-nixd upgrade`, accept the vendor's inner download and update trust chain for alpha. Do not pre-bind or re-authenticate the downloaded daemon or profile payload. If no supported operation exists, Doctor and product health report it as unsupported. Add no product Base Nix repair provider, update engine, or speculative route.
- **Tests and gate:** retain the research evidence. Check that Package Repair remains separate. Check that unsupported Base Nix operations do not mutate the machine. After update, run functional installed-state health validation and report failure. Do not create a second update ledger or extend Handoff only to mirror vendor update state. This evidence does not block clean-host DN-15 after PR 2 lands.

**DN-13 subphase — Direct vendor uninstall**

- **Invocation:** `pkg` authenticates and directly invokes fixed `/nix/nix-installer` with fixed `/nix/receipt.json`. Determinate owns any self-copy needed while uninstall removes `/nix`.
- **Policy:** Determinate owns Base Nix uninstall and vendor residue. `pkg` keeps its process, cancellation, and Handoff validation controls. It removes product-owned files, services, packages, roots, and state. Vendor-owned residue is accepted for alpha. `pkg` does not implement exact cleanup of that residue.
- **Tests and gate:** test exact executable and receipt paths, authentication, process control, cancellation, Handoff validation, and product-owned cleanup. DN-15 proves this in disposable Linux. DN-16 needs separate macOS APFS and launchd proof. Exact vendor-residue cleanup is not a cutover gate.

**DN-14 subphase — Old-alpha reset and refusal**

- **Decision:** NO-GO. There is no authenticated old-alpha fallback executable with an exact digest and source inventory.
- **Work:** add no reset route and no dead refusal module. Do not print a command that the product cannot execute. Keep the existing alpha handling unchanged until a real authenticated reset artifact exists.
- **Tests and gate:** keep old private-alpha classification fail-closed. Treat migration as separate future work. It does not block a clean-host DN-15 run.

### DN-15 delivery label — Cut over the complete Linux Base Nix lifecycle

- **Status:** ready for clean-host implementation after PR 2 lands.
- **Branch:** `dn/15-linux-lifecycle-cutover`.
- **Delivery PR:** PR 3, based on PR 2.
- **Goal:** switch clean-host Linux Base Nix install and supported lifecycle operations to Determinate.
- **Why later:** start after PR 2 lands its inactive foundation and evidence. Old private-alpha migration is outside this clean-host cutover.
- **Likely files and symbols:** Linux bootstrap, inactive lifecycle routes, Handoff, product asset install, Doctor, release asset selection, and Linux user documents.
- **Interface and invariants:** no runtime fallback. Product assets remain owned by `pkg`. Determinate owns supported Base Nix lifecycle operations. `pkg` owns authentication, invocation, progress, health, validation, errors, and product-owned cleanup. Vendor-owned residue is accepted for alpha. Unsupported hosts fail before mutation. Linux user documents describe the new behavior in this PR.
- **Implementation steps:** enable the vendor lifecycle gate for clean Linux hosts. Verify the authenticated asset. Run install through Handoff. Invoke only supported vendor repair, update, and uninstall operations. Report unsupported capability without custom mutation. Run package and product-service smoke tests. Update Linux documents. Keep the old implementation present but unreachable for deletion proof.
- **Tests:** run fake process integration and the Linux alpha matrix in a disposable privileged Docker container with systemd.
- **Proof and evidence:** repeat the clean-host Linux lifecycle proof. State that container proof does not prove host boot, reboot, SELinux, foreign-host coexistence, or a complete distribution matrix.
- **Deletion:** none. Old Linux Base Nix code remains until DN-17.
- **Rollback or stop rule:** revert before release if any lifecycle row fails. Do not add a runtime fallback.
- **Review focus:** one complete lifecycle owner, configuration preservation, Handoff, and full uninstall.
- **Child-unblock condition:** all blocking Linux lifecycle rows pass twice with no old runtime path used.

### DN-16 delivery label — Cut over the complete Apple Silicon macOS Base Nix lifecycle

- **Status:** blocked by Linux completion and disposable macOS proof.
- **Branch:** `dn/16-macos-lifecycle-cutover`.
- **Delivery PR:** PR 4, based on PR 3.
- **Goal:** switch Apple Silicon macOS install, repair, proved update, and uninstall together to the vendor lifecycle.
- **Why later:** start after Linux completion. Use an Apple Silicon macOS VM or another disposable Mac. Docker cannot prove launchd, APFS, or `diskutil` behavior.
- **Likely files and symbols:** macOS bootstrap, APFS detection, inactive lifecycle routes, Handoff, product launchd assets, release target selection, and macOS user documents.
- **Interface and invariants:** no runtime fallback. Vendor owns Base Nix APFS, daemon, repair, update policy, and uninstall work. `pkg` owns product services and package work. Intel macOS is not claimed without full proof. Apple Silicon user documents describe the new behavior in this PR.
- **Implementation steps:** enable the vendor lifecycle gate on Apple Silicon. Verify the authenticated asset. Run install through Handoff. Invoke supported vendor repair, update, and uninstall operations. Report unsupported capability without custom mutation. Prove product launchd ownership and package behavior. Update macOS documents.
- **Tests:** fake process integration and complete Apple Silicon macOS VM matrix.
- **Proof and evidence:** complete clean, repeat, crash, reboot, repair, update, package, uninstall, and residue reports pass twice.
- **Deletion:** none. Old macOS Base Nix code remains until DN-18.
- **Rollback or stop rule:** revert before release if APFS, launchd, configuration, receipt, package, or lifecycle proof fails.
- **Review focus:** complete lifecycle ownership, APFS, launchd, target support, and no Intel claim.
- **Child-unblock condition:** all blocking Apple Silicon lifecycle rows pass twice with no old runtime path used.

### DN-17 work package — Delete proved Linux Base Nix implementation

- **Delivery PR:** PR 4, after DN-16 passes.
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

### DN-18 work package — Delete proved macOS Base Nix implementation

- **Delivery PR:** PR 4, after DN-17 passes.
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

### DN-19 work package — Delete shared private Base Nix provisioning and old release assets

- **Delivery PR:** PR 4, after DN-18 passes.
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

### DN-20 work package — Complete core clean-host and release proof

- **Delivery PR:** PR 4, after DN-19 passes.
- **Goal:** prove the complete core system and update current user documents.
- **Why now:** code deletion is complete, so the final proof measures the new design before the release documents are finalized.
- **Likely files and symbols:** platform evidence, `docs/install.md`, `docs/commands.md`, `docs/support.md`, `docs/privacy.md`, release checklist.
- **Interface and invariants:** docs state the actual owner, diagnostics policy, PATH behavior, supported targets, repair, update, uninstall, and old-alpha refusal.
- **Implementation steps:** run the complete matrix from clean snapshots; publish reproducible evidence; build release metadata; install the built product; run package lifecycle; repair; update; uninstall; inspect residue; complete cross-platform, privacy, support, and release documents after the new results pass.
- **Tests:** full section 10 matrix and local release-tool tests.
- **Proof and evidence:** signed or hashed evidence bundle with exact build input and product version.
- **Deletion:** stale current user documentation and obsolete examples only.
- **Rollback or stop rule:** do not release if any blocking matrix row fails.
- **Review focus:** end-to-end ownership, docs accuracy, and no hidden old fallback.
- **Child-unblock condition:** core definition of done in section 13 is complete.

## 7. Optional simplification work packages in PR 4

Do not start DN-21 until DN-20 is complete. DN-21 through DN-32 are optional checkpoints inside delivery PR 4. They are not separate PRs. They are not required for the Determinate cutover.

### DN-21 work package — Inventory final privilege and process seams

- **Delivery PR:** PR 4, after DN-20 passes.
- **Goal:** record every live process, privilege seam, caller, and duty before simplification.
- **Why now:** core behavior is stable. Deletion needs a complete baseline.
- **Likely files and symbols:** Broker, Root Helper, `NixAdapter`, `MaintenanceAdapter`, vendor process, command engines, services, protocols.
- **Interface and invariants:** this work package observes only. It does not assume that a seam should be removed.
- **Implementation steps:** trace every caller. Record authentication, privilege, state, recovery, service, and log duties. Record the high-fan-in graph for each shared contract.
- **Tests:** inventory consistency checks and existing contract tests.
- **Proof and evidence:** every live duty has one current owner and named callers.
- **Deletion:** none.
- **Rollback or stop rule:** stop if any live process duty has no understood owner.
- **Review focus:** completeness and no desired-answer bias.
- **Child-unblock condition:** reviewers accept the full duty inventory.

### DN-22 work package — Move broad fakes to existing seams

- **Delivery PR:** PR 4, after DN-21 passes.
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

### DN-23 work package — Replace command engines with one concrete Application

- **Delivery PR:** PR 4, after DN-22 passes.
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

### DN-24 work package — Centralize package-mutation recovery

- **Delivery PR:** PR 4, after DN-23 passes.
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

### DN-25 work package — Remove proved direct-Nix duplicates

- **Delivery PR:** PR 4, after DN-24 passes.
- **Goal:** remove only direct Nix process code duplicated by existing adapters.
- **Why now:** application and recovery paths are stable.
- **Likely files and symbols:** direct process callers, `NixAdapter`, `RealNixAdapter`, pipeline acquire paths.
- **Interface and invariants:** resolution stays in `pkg-resolver`. The adapter exposes product operations, not raw command flags. Skip this work package if no duplicate exists.
- **Implementation steps:** inventory direct Nix calls. Compare each with existing adapter behavior. Move only exact duplicates. Leave resolver policy in `pkg-resolver`.
- **Tests:** resolver, acquire, parity, and package lifecycle tests.
- **Proof and evidence:** each deletion has an equal existing adapter path.
- **Deletion:** proved duplicate process and parsing code only.
- **Rollback or stop rule:** skip the work package if the inventory finds no duplicate. Stop if moving code changes ownership.
- **Review focus:** evidence-gated deletion and resolver boundary.
- **Child-unblock condition:** inventory is complete and all duplicates are removed or explicitly retained.

### DN-26 work package — Prove local-build admission and build-duty replacements

- **Delivery PR:** PR 4, after DN-25 passes or is skipped.
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

### DN-27 work package — Prove replacement owners for every privileged duty

- **Delivery PR:** PR 4, after DN-26 passes.
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

### DN-28 work package — Delete or deepen the Root Helper

- **Delivery PR:** PR 4, after DN-27 passes.
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

### DN-29 work package — Delete or deepen the Broker

- **Delivery PR:** PR 4, after DN-28 passes.
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

### DN-30 work package — Prune dead transport grammar

- **Delivery PR:** PR 4, after DN-29 passes.
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

### DN-31 work package — Relocate domain types only for a measured dependency gain

- **Delivery PR:** PR 4, after DN-30 passes.
- **Goal:** move high-impact domain types only when the move removes a dependency edge or enables crate deletion.
- **Why now:** transport is stable and the final dependency graph is measurable.
- **Likely files and symbols:** `GenerationId`, `MaintenanceAdapter`, `OwnershipExpectation`, `ManagedGroupBindings`, Cargo manifests.
- **Interface and invariants:** do not move a type for neatness. Preserve validation and semantics. Skip this work package if no measured gain exists.
- **Implementation steps:** model candidate moves. Count dependency edges before and after. Select at most one coherent move. Update imports mechanically. Delete an empty dependency only after target tests pass.
- **Tests:** affected contract, package, platform, and compile checks.
- **Proof and evidence:** graph evidence shows a removed edge or crate dependency.
- **Deletion:** old type location and dependency edge only if the move proves a gain.
- **Rollback or stop rule:** skip if there is no gain. Stop if fan-in makes the change risky without deletion value.
- **Review focus:** measured value and mechanical semantics.
- **Child-unblock condition:** the move passes or the recorded skip decision is reviewed.

### DN-32 work package — Complete simplification proof

- **Delivery PR:** PR 4, after DN-31 passes or is skipped.
- **Goal:** measure the final design, prune proved-unused dependencies, and update architecture documents.
- **Why now:** all optional process and topology decisions are complete.
- **Likely files and symbols:** Cargo manifests, lockfile, code-health evidence, `CONTEXT.md`, ADR status notes, current docs.
- **Interface and invariants:** dependency removal follows `cargo tree` and target builds. No safety check is removed to improve a metric.
- **Implementation steps:** run dependency trees by target. Remove only unused crates. Run code-health and compare with baseline. Run affected tests and final platform smoke. Update docs for the retained Broker and Helper model.
- **Tests:** target builds, affected workspace tests, release tests, security tests, and final platform smoke.
- **Proof and evidence:** line, dependency, graph, test, and platform deltas with exact commands.
- **Deletion:** proved-unused dependencies and stale docs.
- **Rollback or stop rule:** restore a dependency if target-specific code still needs it.
- **Review focus:** evidence, not deletion count.
- **Child-unblock condition:** optional definition of done is complete.

## 8. Work-package dependency and test table

| Work package | Gate before the next work package | Likely affected tests |
|---|---|---|
| DN-00 | Archive and links complete; plan reviews pass | link check, `git diff --check` |
| DN-01 | No failed pending operation remains live | CLI recovery, Broker lifecycle |
| DN-02 | Verify-only path has no writes | repair, leases, concurrency |
| DN-03 | Complete for standalone evidence; negative cleanup and crash results route to later owners | Linux/macOS platform spike matrix and accepted child reports |
| DN-04 | Complete; every domain claim maps to DN-03 | documentation links and terminology |
| DN-05–07 | Complete; assets, invocation, and crash-state classification pass | release trust, fake process, Handoff, and receipt fault injection |
| DN-08–14 | Unused ownership partition and exact-partition tests are removed; normalized inventory and evidence reports remain accurate | normalized inventory, typed proxy, detection, PATH evidence, direct vendor uninstall, and decision reports |
| DN-15 | PR 2 lands; then the clean-host Linux alpha lifecycle passes in disposable privileged/systemd Docker | Linux install, supported vendor operations, uninstall, packages |
| DN-16 | Apple Silicon lifecycle passes twice in a macOS VM or on another disposable Mac | macOS install, supported vendor operations, uninstall, packages |
| DN-17 | Deleted Linux symbols have no live callers | Linux matrix and package lifecycle |
| DN-18 | Deleted macOS symbols have no live callers | macOS matrix and security tests |
| DN-19 | Old runtime absent from release graph | release, parity, dependency tree |
| DN-20 | Full core definition of done | complete platform and release matrix |
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
| DN-31 | A move removes an edge, or the work package is skipped | affected contract and package checks |
| DN-32 | Target dependency proof and final checks pass | builds, affected tests, security, platform smoke |

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

## 10. Platform proof matrix

Evidence for each row records platform image, architecture, product revision, vendor version, vendor full revision, asset digest, exact invocation, exit status, logs, file ownership, services, receipt state, package state, and residue. Linux alpha rows can run in disposable privileged Docker with systemd. They prove only that container environment. macOS rows require an Apple Silicon macOS VM or another disposable Mac.

| Case | First proving delivery PR / subphase | Linux x86_64 | Linux aarch64 | Apple Silicon macOS | Required result |
|---|---|---|---|---|---|
| Standalone vendor invocation and arguments | DN-03 | Blocking | Asset proof | Blocking | Exact observed arguments and exit behavior |
| Standalone diagnostics control | DN-03 | Blocking | Asset proof | Blocking | Proved endpoint or build policy |
| Standalone receipt and installed copy | DN-03 | Blocking | Asset proof | Blocking | Observed receipt and executable behavior |
| Standalone repeat install | DN-03 | Blocking | Sample | Blocking | Stable vendor outcome |
| Standalone SIGKILL and reboot | DN-03 | Blocking | Sample | Blocking | Observed vendor recovery behavior |
| Standalone repair and update | DN-03 | Blocking | Sample | Blocking | Exact vendor behavior and update owner |
| Standalone uninstall | DN-03 | Blocking | Sample | Blocking | Observed vendor-owned cleanup only |
| Wrong vendor digest | DN-05–07 / DN-05 | Blocking | Blocking | Blocking | Refuse before privilege and execution |
| Handoff crash before launch | DN-05–07 / DN-07 | Blocking | Sample | Blocking | `Started` persists and restart fails closed |
| Handoff crash after vendor success | DN-05–07 / DN-07 | Blocking | Sample | Blocking | Receipt identity can become `Accepted` atomically |
| Missing or damaged receipt | DN-05–07 / DN-07 | Blocking | Sample | Blocking | Refuse destructive work; no action-list parsing |
| Modified installed vendor executable | DN-05–07 / DN-07 | Blocking | Sample | Blocking | Detect identity change and refuse or atomically reaccept after proved lifecycle work |
| Standard-daemon package behavior | DN-08–14 / DN-09 | Blocking | Blocking before target release | Blocking | Required package parity passes |
| Broker daemon admission | DN-08–14 / DN-09 | Blocking | Sample | Blocking | Multi-user access follows one owned configuration path |
| Vendor config one-writer | DN-08 foundation; DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | No vendor-owned file has a second writer |
| Foreign Nix | DN-08–14 / DN-10 | Blocking | Sample | Blocking | Refuse and preserve all files |
| Upstream Nix | DN-08–14 / DN-10 | Blocking | Sample | Blocking | Refuse and preserve all files |
| Unmarked Determinate | DN-08–14 / DN-10 | Blocking | Sample | Blocking | Refuse initial-alpha adoption |
| Login shell PATH | DN-08–14 / DN-11 | Blocking | Sample | Blocking | `pkg` works and observed raw Nix exposure matches policy |
| Non-login shell PATH | DN-08–14 / DN-11 | Blocking | Sample | Blocking | `pkg` works without profile assumptions |
| GUI launch PATH | DN-08–14 / DN-11 | Blocking | Sample | Blocking | `pkg` works from a clean GUI environment |
| Base Nix repair owner | DN-12 policy; DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Use a supported vendor operation or report unsupported; add no product engine |
| Base Nix update owner | DN-12 policy; DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Accept the vendor inner trust chain for alpha; run functional health validation; add no product ledger or engine |
| Full uninstall and resume | DN-13 foundation; DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Authenticate and directly invoke fixed `/nix/nix-installer` with fixed receipt path; keep controls; accept vendor residue |
| Old private alpha migration | DN-14 decision | Separate | Separate | Separate | Fail closed; do not block clean-host work or invent an unauthenticated reset route |
| Complete clean install | DN-15 Linux; DN-16 macOS | Blocking | Blocking before target release | Blocking | Accepted Handoff and working product |
| Complete repeat install | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Stable complete lifecycle result |
| Complete repair and update | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Base and package repair stay separate |
| N to N+1 product upgrade | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | State, packages, Handoff, and identity remain valid |
| Downgrade | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Follow explicit proved policy |
| Complete explicit uninstall | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Invoke vendor Base Nix uninstall and remove all product-owned state, assets, roots, and services |
| Install, remove, and update package | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Generation and state transitions work |
| Local build | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Current approval and multi-user safety remain |
| Package roots and GC | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Active Generation and per-user isolation remain |
| Package repair | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Product-owned repair only |
| Modified product service asset | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Product repair detects and handles it |
| Final release ownership | DN-20 | Blocking | Blocking | Blocking | Each remaining asset has one owner |
| Final release residue | DN-20 | Blocking | Blocking | Blocking | No product-owned residue; record accepted vendor-owned alpha residue |
| Optional Root Helper removal | DN-28 | Blocking | Sample | Blocking | Every helper duty has equal replacement proof |
| Optional Broker removal | DN-29 | Blocking | Sample | Blocking | Every Broker duty has equal replacement proof |
| Optional transport and dependency pruning | DN-30 through DN-32 | Blocking | Target check | Blocking | Only dead grammar, edges, and dependencies are removed |
| Intel macOS | DN-03 asset probe; full proof not scheduled | Not applicable | Not applicable | Unsupported-target probe | Do not claim support without a full asset and lifecycle matrix |

`Blocking` means the PR cannot merge without the result. `Sample` means the architecture result must match the blocking platforms, but the release owner can define the exact repeated sample count. `Separate` means the old private-alpha migration is outside the clean-host cutover. Linux aarch64 becomes fully blocking for all rows before a Linux aarch64 release. Linux container results do not prove host boot, reboot, SELinux, foreign-host behavior, or a complete distribution matrix. Docker does not satisfy any macOS row.

## 11. Agent review synthesis

The report and this plan use work from GLM 5.3, DeepSeek Pro, and Qwen 3.8 Max. Advice is not accepted because of the model name. It is accepted only when repository and upstream evidence support it.

### 11.1 Accepted advice

- Use the external executable. Do not create a source fork.
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
- Advice about update delegation became DN-12 evidence. Determinate owns supported update operations. `pkg` does not name or invoke an unproved product command.

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
- [ ] Does the PR avoid claiming that Determinate replaces package builds, generations, roots, GC, Package Repair, Broker, or Root Helper without proof?
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
- [ ] Does old private-alpha state fail closed without an invented reset command?
- [ ] Is old private-alpha migration kept separate from clean-host cutover?
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
- Existing foreign, upstream, and unmarked Determinate Nix refuse safely. Old private-alpha state is a separate fail-closed migration case.
- PATH behavior matches the documented user experience.
- Determinate owns supported Base Nix repair, update, and uninstall. Unsupported capability is reported without a second product engine.
- Vendor-owned residue is accepted for alpha. `pkg` removes all product-owned residue.
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
- implement a product-owned Base Nix repair, update, uninstall, or vendor-residue cleanup engine;
- delete any runtime code or dependency;
- change package state, generations, roots, GC, builds, or repair.

The first code change is DN-01. The first vendor proof is DN-03. The first complete production cutover is DN-15. The first old Base Nix code deletion is DN-17.
