# Determinate Nix stacked PR implementation plan

Status: active implementation plan for an alpha product.

Current status: **Delivery PR 1 and PR 2 are complete. DN-15 Linux proof passed. DN-16 production code is reviewed and backed up, but it is not merged or proved.**

The current migration delivery has exactly four delivery PRs. PR 1 and PR 2 group several work packages. PR 3 has the single delivery label DN-15. PR 4 has the single delivery label DN-16. DN-17 through DN-32 remain work-package IDs inside PR 4. They do not create more delivery PRs.

| Delivery PR | Scope | Status |
|---|---|---|
| 1 | DN-00 through DN-07: plan, alpha fixes, evidence, contract, and authenticated vendor foundation | Complete |
| 2 | DN-08 through DN-14: inactive integration foundation and decision evidence | Complete |
| 3 | DN-15: Linux completion | Native proof passed at `9bd17b`; not published |
| 4 | DN-16: Apple Silicon macOS completion; DN-17 through DN-32 are later cleanup, proof, and optional simplification checkpoints inside this PR | Production code reviewed at `8ffd325a`; not merged; native proof blocked |

PR 2 does not deliver a platform cutover. It records safe inactive code,
evidence limits, and the accepted ownership policy. PR 3 activates that work on
Linux. The signed Linux commit
`9bd17b716503d7be3bcf5bd310ceddd9aecede50` passed native x86-64 workflow run
[`33198985687`](https://github.com/spa5k/pkg/actions/runs/33198985687). The
evidence received independent review. The run did not publish a release.

The macOS production code at
`8ffd325a4be12a998f3a5684097b57841a11540e` is reviewed and backed up. It is not
merged. The separate proof branch contains work-in-progress workflow and
harness structure. It is not proof evidence.

DN-03 completed the standalone vendor evidence gate. This does not mean that vendor uninstall is clean. It does not mean that crash recovery succeeds. Linux functional behavior checks passed, but strict vendor cleanup failed. In the accepted macOS crash observation, state validation stops after the recovery install exits 0 because `_nixbld1` is missing.

The public evidence is in the [DN-03 parent decision](../spikes/s6-determinate-installer/FINDINGS.md), the [Linux findings](../spikes/s6-determinate-installer/linux-vm/LINUX-FINDINGS.md), and the [macOS lifecycle, residue, and crash findings](../spikes/s6-determinate-installer/macos-vm/FSTAB-CONTRACT-RESEARCH.md).

- Linux R12 proves broad Linux x86_64 behavior. Retained x86_64 R11 and aarch64 R10 prove the two Linux target Asset records.
- macOS R10 completes the standalone lifecycle and residue evidence. Its functional lifecycle and reboots passed, but strict vendor cleanup failed. Crash R1 completes the required negative SIGKILL and reboot observation.
- Clean vendor uninstall remains false on both platforms. DN-13 uses the fixed installed vendor executable and receipt paths. Determinate owns any self-copy and vendor residue. Vendor-owned residue is an accepted alpha limit. `pkg` removes only product-owned residue.
- Successful crash recovery is unproved. DN-06 and DN-07 delivered the product controls. DN-16 owns the later macOS crash proof. The landed DN-12 report concludes that there is no safe general vendor repair route.
- Linux alpha proof passed on a disposable native x86-64 GitHub-hosted runner at exact commit `9bd17b716503d7be3bcf5bd310ceddd9aecede50` in run `33198985687`. The checked-in harness used privileged Docker with systemd. The result proves only that runner and container environment. It does not prove boot, reboot, SELinux, foreign-host behavior, or a full distribution matrix. Nothing was published.
- macOS proof needs two exact disposable Apple Silicon runners. Docker cannot prove launchd or native reboot behavior. No required runner is registered. No two authenticated, signed DN-16 release inputs exist. Those inputs establish shipping identity only. Both tags use one live channel, so they do not prove native N-to-N+1. No staged channel or snapshot protocol installs N before the channel advances to N+1. No two-phase product lifecycle reboot proof exists.
- DN-04 documents the proved ownership and executable contract.

This plan replaces the old custom Managed Nix implementation plan. The old plan is preserved in the [dated legacy archive](archive/2026-08-22-custom-managed-nix-v1/README.md). The design reasons and research are in the [architecture report](../architecture-report.html).

DN-15 changes the current Linux source and candidate behavior. Its user
documents describe that Linux behavior now. The published alpha.7 release
remains separate. DN-16 changes current macOS source and candidate behavior.
It is not in alpha.7. Its production code is not merged, and its native proof
has not run. DN-20 completes the release documents after final proof.

## 1. Accepted ownership

The accepted product boundary is:

- Determinate owns the machine-wide **Base Nix lifecycle**.
- Base Nix lifecycle means Base Nix install, supported repair, update, Base Nix service setup and initialization, and uninstall.
- For Base Nix install, `pkg` authenticates the pinned Determinate Nix Installer 3.22.1 executable and starts it once through an absolute path and fixed environment.
- One supervisor drains bounded vendor diagnostic output, waits for the vendor process, and reaps it.
- Vendor stdout and stderr are not a stable progress or completion protocol.
- Before vendor start, `pkg` can refuse or stop. After vendor start, there is no proved safe cancellation, signal, hard timeout, or parent-death guarantee.
- A persisted `Started` Base Nix Handoff means an Unknown Base Nix Outcome. It fails closed and does not authorize retry, resume, adoption, or reconstruction.
- Only vendor exit status `0` plus successful installed-state validation can become Accepted Base Nix Handoff.
- For live Base Nix uninstall, `pkg` owns only pre-`exec` format rejection, complete verified product-owned cleanup, exact installed executable and opaque receipt revalidation, Accepted Base Nix Handoff and product-state consumption immediately before `exec`, and terminal vendor uninstall invocation.
- Determinate owns vendor uninstall signals, exit status, self-copy, native cleanup, temporary files, and residue.
- `pkg` does not supervise, cancel, resume, retry, or reconstruct the vendor uninstall phase.
- `pkg` does not implement a second Base Nix lifecycle engine or exact cleanup for vendor-owned residue.
- `pkg` owns package selection, package builds, package state, generations, activation, package roots, package garbage collection, package repair, and the product user experience.
- Linux product installation has three modes. Fresh Install alone activates the fixed product service set.
- An ordinary N-to-N+1 product upgrade is an Offline Upgrade. A same-release Product Asset Repair is an Offline Repair.
- Offline Upgrade and Offline Repair require all fixed product units to be inactive and disabled, to use the exact product unit fragments, and to have no drop-ins.
- Both offline modes only query systemd state. They change product files only. They never stop, disable, reload, start, or restart a unit. They leave product services offline.
- Product Asset Repair moves forward to authenticated same-release bytes. It does not restore unknown damaged bytes.
- The operator activates or reboots into the authenticated result after an offline operation.
- The active-upgrade service-state recovery design is deleted. No replacement protocol is needed because existing-install work stays offline.
- `pkg` can keep its Broker and Root Helper for package work.
- The plan does not assume that Determinate replaces the Broker or Root Helper.
- The plan does not assume that Determinate replaces package roots, package garbage collection, or package repair.
- Raw Nix can exist on the machine. `pkg` keeps raw Nix out of its normal user experience. This is not a security boundary.
- Local administrators can change machine-wide Nix. `pkg doctor` must detect important changes and fail closed where ownership is not clear.
- Old private-alpha installations are a separate migration case. They do not block clean-host work. `pkg` must detect them and refuse unsafe automatic mutation. The product must not show a reset command that it cannot authenticate and run.

Live Base Nix uninstall requires plain terminal output. Structured JSON or JSONL
output is rejected before mutation. Dry-run uninstall can remain structured. A
synchronous `exec` return means the vendor did not start. `pkg` restores exact
Accepted Base Nix Handoff under the same stable lock and revalidates identities.
Restore failure fails closed. `SIGKILL` or crash after state consumption but
before `exec` leaves Base Nix unmarked and Base Nix Handoff absent. `pkg` refuses
this state and does not infer success, retry, adopt, resume, repair, or
reconstruct it. Alpha recovery is unsupported. After `exec` starts the vendor
program, Determinate owns signals and exit status. Later loss of the vendor result is an
Unknown Base Nix Outcome.

Linux uses root-owned mode-`0600` `/run/pkg-install-handoff.lock` as its one
volatile coordination exception. macOS uses the persistent, zero-byte,
root-owned mode-`0600` `/private/var/db/pkg-install-handoff.lock`. Both are
coordination, not lifecycle state. The macOS path avoids the native
group-writable `/private/var/run` directory.

The trust rule for any future post-alpha update route is explicit. `pkg` authenticates the pinned outer Determinate installer and invokes vendor programs through fixed command paths. If that route uses `determinate-nixd upgrade`, `pkg` accepts Determinate's inner download and update trust chain. It does not pre-bind or re-authenticate the downloaded daemon or profile payload. After update, `pkg` runs functional installed-state health validation and reports failure. It does not create a second update ledger or extend Base Nix Handoff only to mirror vendor update state. This security trade-off trusts Determinate for the inner payload and keeps one update engine. This rule does not approve or expose an update action by itself.

## 2. Pinned upstream baseline

The product release metadata pins the following version, revision, license, and
asset paths. Behavior statements remain limited to the recorded observations.

- Pinned package: `nix-installer` 3.22.1.
- Pinned full revision: `4132ad07a15ee7d88c096ac7172b7afb2672866b`.
- Revision date: 2026-08-19.
- Pinned license: LGPL-2.1.
- Fixed receipt path: `/nix/receipt.json`.
- Fixed installed executable path: `/nix/nix-installer`.
- Observed default: diagnostics are enabled.
- Observed library status: the Rust library interface is experimental.
- Observed install behavior: the installer can install Determinate Nix.
- Observed uninstall behavior: the installed executable reads its receipt and reverses recorded work.

DN-03 recorded the standalone product-facing evidence for the executable. This includes exact command arguments, argument order, exit status, output, diagnostics control, receipt behavior, update ownership, PATH behavior, platform support, and failure observations. DN-04 maps its contract text to that evidence.

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

### 3.5 Platform proof policy

Each PR records the exact command and result for each proof. A platform result
includes the image, architecture, date, input asset digest, output logs, results
matrix, and residue report. A reviewer must be able to repeat it.

A disposable native x86-64 GitHub-hosted runner can satisfy the Linux proof. It
must run the checked-in privileged Docker and systemd harness against one exact
signed commit. The complete logs, results matrix, and retained artifacts must be
available after the run. At least one reviewer who did not run the proof must
review that evidence before acceptance. A temporary proof branch does not
become a release branch.

A local native x86-64 host can also satisfy Linux proof when it produces the
same evidence. An emulated x86-64 Docker server cannot satisfy the proof.

The macOS harness runs only through the manual
[macOS Apple Silicon lifecycle proof workflow](../.github/workflows/macos-alpha-proof.yml).
Do not run `tests/macos-clean-host/prove.sh` directly. The workflow needs two
exact disposable Apple Silicon runners and two authenticated, signed DN-16
release inputs. It also needs a staged channel or snapshot protocol that
installs N before the channel advances to N+1. The signed inputs establish
shipping identity only. They do not prove N-to-N+1 because both tags use one
live channel. A GitHub-hosted Linux result and Docker do not satisfy any macOS
row. A skipped platform check blocks production cutover for that platform.

The current external blockers are exact:

- no required disposable DN-16 runner is registered;
- no two authenticated, signed DN-16 release inputs exist;
- no staged channel or snapshot protocol installs N before the channel advances
  to N+1;
- no two-phase product lifecycle reboot proof exists.

## 4. Dependency diagram and stop gates

```text
PR 1: DN-00 through DN-07 [COMPLETE FOUNDATION]
  |
PR 2: DN-08 through DN-14 [COMPLETE FOUNDATION AND EVIDENCE]
  |
PR 3: DN-15 [LINUX COMPLETION]
  |
PR 4: DN-16 [MACOS COMPLETION]
      DN-17 through DN-20 [POST-CUTOVER CLEANUP AND CORE PROOF]
      DN-21 through DN-32 [OPTIONAL SIMPLIFICATION CHECKPOINTS]
```

Stop gates:

- **DN-03** is complete for standalone evidence. Its negative results define product limits and later platform proof.
- **DN-05–07** blocks integration if asset authentication, safe process execution, or minimal Base Nix Handoff handling fails. It must not add a second vendor journal.
- **DN-08–14** is a complete inactive foundation and evidence PR. PR 2 removes the unused ownership partition and its exact-partition tests while preserving the normalized install inventory. Determinate owns supported repair, update, and uninstall. Vendor residue is accepted for alpha. Old private-alpha migration is separate.
- **DN-15** passed its native x86-64 proof and independent evidence review at exact commit `9bd17b716503d7be3bcf5bd310ceddd9aecede50` in run `33198985687`. The proof did not publish a release.
- **DN-16** blocks macOS deletion until Apple Silicon proves Base Nix install, terminal vendor uninstall, package operations, interruption and crash behavior, real reboot behavior, and the documented failure cases. Run `33256299782` stopped at the hosted input gate because the channel and proof-input release manifests were not byte-identical. No self-hosted runner or product mutation started.
  Run `33258481980` passed the hosted gates but failed before Handoff during the slot 1 Alpha.10 native install. Product `service-root` classification rejected the standard `root:admin` (`0:80`) `/Library/Application Support` ancestor because the shared filesystem check required root GID `0`.
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
| Base Nix ownership and journals | `managed/ownership.rs`, platform install journals, store/repair journals | Keep until Base Nix Handoff and vendor receipt behavior are proved. | DN-17 through DN-19. Keep package journals. |
| Uninstall | `UninstallEngine`, platform uninstall modules | Require plain output. Finish and verify product cleanup. Under the stable lock, revalidate identity, consume Accepted Base Nix Handoff immediately before `exec`, and start terminal vendor uninstall. Restore Accepted state on synchronous `exec` return. Refuse the unmarked crash window. | The DN-13 subphase starts the inactive path. DN-17 through DN-19 remove obsolete Base-Nix paths. |
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
- **Proof and evidence:** the [parent decision](../spikes/s6-determinate-installer/FINDINGS.md) links the accepted public results. The [Linux report](../spikes/s6-determinate-installer/linux-vm/LINUX-FINDINGS.md) owns the Linux evidence. The [macOS report](../spikes/s6-determinate-installer/macos-vm/FSTAB-CONTRACT-RESEARCH.md) owns R10 and Crash R1. DN-03 does not prove `pkg` Base Nix Handoff, package lifecycle, product repair, product uninstall, product cleanup, or production cutover.
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
- **Implementation steps:** add Base Nix, Base Nix Lifecycle, Package Lifecycle, Base Nix Handoff, and Unknown Base Nix Outcome to `CONTEXT.md`. Put the Determinate executable, Vendor Receipt and path, diagnostics, pinning, upstream revision, and rejected alternatives in ADR 0004. Link DN-03 evidence. Mark ADR 0003 partially superseded without rewriting history.
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
- **Goal:** ship pinned vendor assets, one private install process Adapter, and minimal Vendor Receipt and Base Nix Handoff validation in one review.
- **Order:** DN-05, DN-06, and DN-07 are ordered work-package and checkpoint IDs. Implement them in this order. End each checkpoint with one signed, green, independently reviewable commit.
- **Deletion:** none. Keep old Base Nix assets until both platform cutovers pass. This PR adds no second receipt or vendor action journal.
- **Combined stop rule:** do not merge if source compliance, a supported target asset, exact safe invocation, or fail-closed Base Nix Handoff handling is missing.

**DN-05 subphase — Pinned vendor assets**

- **Goal and files:** authenticate and inventory the executable in `tools/release/src/manifest.rs`, `tools/release/src/sign.rs`, channel target fixtures, software inventory, and the installer release schema.
- **Interface:** use one exact target and digest for each supported system. Record LGPL-2.1 and the matching source location. State diagnostics and downgrade policy explicitly. Never use an ambient download.
- **Work:** add per-target records and digest metadata. Add the license notice and corresponding-source inventory. Add diagnostics metadata only when DN-03 proves it stable. Update fixtures and release validation.
- **Tests and proof:** run release manifest and signature tests. Test wrong digests, unsupported systems, and incomplete inventory. Generated metadata must identify one exact executable per target.
- **Gate and review:** stop if source compliance or a supported target asset is missing. Review the trust chain, target map, license duties, and rejection of any changed executable.

**DN-06 subphase — Private install process Adapter**

- **Goal and files:** add one concrete vendor install process Adapter in `pkg-installer`. Reuse `std::process::Command` or the current async process support. Add installer errors and fake-executable tests.
- **Interface:** for install, use an absolute authenticated path and only proved arguments. Set or scrub the required environment. Start one vendor process. Drain stdout and stderr concurrently into bounded diagnostic storage. Keep private data out of logs. Do not use vendor output as a stable progress or completion protocol. One supervisor waits for and reaps the process. Before start, the product can refuse or stop. After start, it has no proved safe cancellation, signal, hard timeout, or parent-death guarantee. Terminal uninstall does not use this child-process interface. Add no trait or provider framework.
- **Work:** define one concrete `DeterminateInstaller` for install. Build the install command in one place. Reject a missing or changed executable. Keep one supervisor alive until wait and output drain finish. Report the observed terminal result without converting it to installed-state success. Use a fake executable that records arguments and controls output. Add no alpha Base Nix repair or update route. DN-13 owns terminal uninstall.
- **Tests and proof:** test install arguments, environment, exit codes, signaled exit, both-pipe back-pressure, bounded storage, log redaction, caller loss, supervisor wait and reap, spawn failure, wait failure, and a wrong file. Prove that caller loss does not cause a second start. Record that no post-start cancellation, signal, hard timeout, or parent-death promise exists. The tests must prove exact install invocation without root or a VM.
- **Gate and review:** remove the Adapter if it requires plan JSON, source embedding, or a broad abstraction. Review process safety, one-start behavior, wait and reap behavior, minimal surface, and every observed external outcome.

**DN-07 subphase — Vendor Receipt and Base Nix Handoff**

- **Goal and files:** persist only the minimum restart state in `pkg-installer` bootstrap state, an opaque receipt validator, and current installer recovery entry points.
- **Interface:** no Base Nix Handoff state means `NotStarted`. Persist only `Started` and `Accepted`. A persisted `Started` state means an Unknown Base Nix Outcome. It fails closed and does not authorize retry. Only exit status `0` plus installed-state validation can become `Accepted`. `Accepted` stores the minimum stable executable and receipt identity. Do not parse or copy the vendor action list. Never delete unknown `/nix` content.
- **Work:** durably write `Started` before vendor start. Start the vendor once. Keep `Started` after any nonzero exit, signal, lost supervisor, wait failure, or failed installed-state validation. Atomically write `Accepted` only after exit status `0` and successful validation. Classify and report state on restart. Atomically update identity after a proved repair or update changes the executable or receipt.
- **Tests and proof:** inject a crash before vendor start, while it runs, and after exit status `0` but before acceptance. Test nonzero exit, signal, a persisted `Started` state after simulated supervisor loss, failed validation, missing or damaged receipts, changed executables, and unknown `/nix`. Each persisted `Started` case must be an Unknown Base Nix Outcome, fail closed, and never start the vendor again.
- **Gate and review:** stop if safe recovery needs private vendor action replay. Review durability, the opaque seam, and the absence of a parallel ownership ledger.

### DN-08–14 work-package group — Build the inactive lifecycle integration

- **Status:** complete as an inactive foundation and evidence PR. It does not claim a platform cutover.
- **Branch:** `dn/08-14-lifecycle-integration`.
- **Delivery PR:** PR 2, based on PR 1.
- **Goal:** keep the safe inactive integration foundation and record the accepted lifecycle ownership policy and its evidence limits.
- **Execution:** DN-08 through DN-14 are logical work areas inside one delivery PR. They are not required commit boundaries or a required chronological commit order. Implementation can combine or reorder them when dependencies remain safe. Keep every route inactive in shipped behavior. The final PR must be green, signed, and reviewed as a whole.
- **Combined deletion:** delete the unused ownership partition and its exact-partition tests. Preserve the normalized install inventory. Keep old detection and PATH code unchanged. DN-17 through DN-19 own any later deletion of obsolete Base Nix detection or PATH code after platform cutover proof. Add no compatibility bridge.
- **Combined stop rule:** do not call this PR a platform cutover. PR 2 preserves the normalized inventory and includes its evidence reports. Platform behavior remains inactive until DN-15 or DN-16 proves and enables it.

Current work-area result:

- **DN-08:** simplified. PR 2 deletes the unused ownership partition and its exact-partition tests. It preserves the normalized install inventory. A supported vendor configuration extension is NO-GO. The inactive typed Root Helper proxy replaces that rejected design.
- **DN-09:** partial. The standard Determinate adapter mode and typed Root Helper proxy are inactive. Live Linux and macOS parity still need proof.
- **DN-10:** partial. Inactive classification and Doctor behavior exist. The privileged producer and platform proof remain gated.
- **DN-11:** evidence is partial. Production PATH behavior stays unchanged. Platform launch-context proof remains gated.
- **DN-12:** the companion PR-2 evidence report records the vendor capability limits. The report must land before PR 2 can claim this proof. Determinate owns supported Base Nix repair and update. Package Repair stays product-owned. No speculative product repair or update engine is added.
- **DN-13:** partial. The inactive path finishes and verifies product cleanup, revalidates fixed `/nix/nix-installer` and opaque `/nix/receipt.json`, consumes Accepted Base Nix Handoff and product state, then uses terminal `exec`. Determinate owns the vendor phase and residue.
- **DN-14:** NO-GO for an old-alpha reset route. There is no authenticated fallback executable. Old private-alpha migration is separate and does not block clean hosts. No dead refusal or reset code is added.

**DN-08 subphase — Remove the unused ownership partition**

- **Files and interface:** update `assets.rs`, platform asset lists and managers, `InstallAssetOwner`, and exact-partition tests. Preserve the normalized install inventory and stable output. `pkg` does not edit vendor-owned configuration.
- **Work:** delete the unused ownership partition and its exact-partition tests. Keep the normalized install inventory. The vendor configuration extension is NO-GO. Use the inactive typed Root Helper proxy for package Nix operations. Do not add a second writer for vendor-owned configuration.
- **Tests and gate:** prove that the normalized install inventory and stable output are unchanged. Linux and macOS live preservation proof remains a cutover gate. Review inventory preservation and the one-writer configuration rule. Do not add a replacement owner field.

**DN-09 subphase — Standard-daemon RealNix parity**

- **Files and interface:** update `RealNixAdapter`, `crates/pkg-nix/src/{real,adapter,build,verify,substitute}.rs`, parity fixtures, and `pkg-testkit`. Use one fixed standard-daemon mode. Add no provider framework. Preserve package trust and outcomes. Use absolute Nix paths or one proved stable rule, never PATH.
- **Work:** bind proved daemon and store paths. Route package Nix operations through the inactive typed Root Helper proxy. Prove Broker admission and multi-user access. Probe read, resolve, acquire, substitute, local build, roots, GC, and package repair. Do not depend on the rejected vendor configuration extension.
- **Tests and gate:** run real adapter tests, parity tests, package install, package remove, package update, product upgrade, and GC smoke tests on Linux and Apple Silicon macOS. Attach a standard-daemon parity report for both platforms. Stop for unexplained differences in trust, roots, access, build, GC, or configuration ownership. Keep private mode until both cutovers pass. Review package behavior, multi-user safety, and fixed configuration.

**DN-10 subphase — Existing and foreign Nix classification**

- **Files and interface:** update managed detection, Doctor, bootstrap preflight, and receipt and executable validation. Accept only a clean host or Accepted Base Nix Handoff from the new flow. Report foreign Nix, upstream Nix, unmarked Determinate, damaged accepted state, and old alpha separately. All unsafe states block automatic install.
- **Work and tests:** classify only observable facts. Add clear Doctor actions and installer refusal. Test clean, accepted, foreign, upstream, unmarked Determinate, damaged accepted, and old-alpha fixtures. Each fixture must map to one stable classification and one safe action. Keep automatic adoption as future work. Keep all current production detection code in place.
- **Gate:** every unknown state must fail before privilege or mutation. Stop if two unsafe states can produce accepted identity. Never repair, adopt, or delete unknown state automatically. Review false acceptance, user instructions, and the lack of auto-adoption.

**DN-11 subphase — PATH behavior**

- **Files and interface:** update proved installer options, `crates/pkg-cli/src/path.rs`, shell tests, Doctor output, and platform evidence. `pkg` never locates the vendor executable or Nix through PATH. Normal use must work in login, non-login, clean non-login, and GUI environments. Raw Nix visibility is not a security boundary.
- **Work and tests:** enforce the DN-03 profile-control result. Inspect supported shells and GUI launch state. Report unexpected raw Nix exposure. Test existing profile content, repeat install, uninstall residue, and every launch context. Record before-and-after environments.
- **Gate:** stop if `pkg` needs fragile shell mutation or the vendor silently changes profiles. Keep current production PATH code in place. Record any proved-obsolete PATH code for deletion by DN-17 through DN-19 after platform cutover. Review the user-experience statement separately from the security statement and confirm absolute path use.

**DN-12 subphase — Base Nix repair and update policy**

- **Decision:** Determinate owns supported Base Nix repair and update. The companion PR-2 report shows that there is no general repair command and that update interruption is not a proved product contract. These are support limits, not reasons to build a second engine. The report must land before PR 2 claims this evidence. Package Repair stays product-owned.
- **Work:** expose no Base Nix repair or update action on any alpha platform. A future post-alpha product route needs separate approval. If that route uses `determinate-nixd upgrade`, it must use the fixed command path and the accepted vendor inner trust chain. It must run functional installed-state health validation. Add no product Base Nix repair provider, update engine, or speculative route.
- **Tests and gate:** retain the research evidence. Check that Package Repair remains separate. Check that PR 3 exposes no Base Nix repair or update action. Do not create a second update ledger or extend Base Nix Handoff only to mirror vendor update state. This evidence does not block clean-host DN-15 after PR 2 lands.

**DN-13 subphase — Terminal vendor uninstall**

- **Invocation:** require plain output and reject live structured JSON or JSONL before mutation. Dry-run can remain structured. Finish and verify all product-owned cleanup. Revalidate exact `/nix/nix-installer` and opaque `/nix/receipt.json`. Consume Accepted Base Nix Handoff and product state. Use `exec` to replace the `pkg` process with the fixed vendor uninstall invocation.
- **Ordering and lock:** all product cleanup finishes before the vendor action. Then hold the stable platform handoff lock, revalidate identities, consume exact Accepted Base Nix Handoff immediately before `exec`, and start terminal vendor uninstall. Linux uses volatile root-owned mode-`0600` `/run/pkg-install-handoff.lock`. macOS uses persistent, zero-byte, root-owned mode-`0600` `/private/var/db/pkg-install-handoff.lock` because native `/private/var/run` is group-writable. The locks are coordination, not lifecycle state, and are the deliberate product-residue exception.
- **Policy:** Determinate owns signals and exit status after `exec`, plus self-copy, native cleanup, temporary files, and residue. `pkg` does not supervise, cancel, resume, retry, or reconstruct that phase. Product cleanup always finishes before vendor cleanup. The vendor action is last.
- **Synchronous return:** if `exec` returns, the vendor did not start. Under the same stable lock, restore exact Accepted Base Nix Handoff and revalidate executable and receipt identities. Restore or identity-validation failure fails closed.
- **Crash window:** `SIGKILL` or crash between Accepted-state consumption and `exec` leaves Base Nix unmarked and Base Nix Handoff absent. The vendor did not start. Refuse the state. Do not infer success, retry, adopt, resume, repair, or reconstruct it. Alpha recovery is unsupported.
- **Later loss:** after `exec` starts the vendor program, Determinate owns signals and status. Crash or loss of the vendor result is an Unknown Base Nix Outcome. Recovery requires reinstall or vendor support.
- **No success inference:** never infer vendor uninstall success from later absence of `/nix`, the installed helper, the opaque receipt, a vendor temporary file, a service, or another vendor-owned path. Absence can be observed and reported. After crash or loss of `exec` outcome, the result remains an Unknown Base Nix Outcome.
- **Tests and gate:** test structured live-output rejection before mutation, dry-run output, all product actions before vendor action, exact identity revalidation, immediate Base Nix Handoff consumption, and terminal vendor uninstall. Test synchronous `exec` return restores exact Accepted Base Nix Handoff under the same lock. Test restore failure fails closed. Test `SIGKILL` after consumption leaves Base Nix Handoff absent, Base Nix unmarked, refusal on restart, and no vendor start. Prove that no code converts vendor-path absence into uninstall success. DN-15 proves this in disposable Linux. DN-16 needs separate macOS APFS, launchd, crash, and real reboot proof. Exact vendor-residue cleanup is not a cutover gate.

**DN-14 subphase — Old-alpha reset and refusal**

- **Decision:** NO-GO. There is no authenticated old-alpha fallback executable with an exact digest and source inventory.
- **Work:** add no reset route and no dead refusal module. Do not print a command that the product cannot execute. Keep the existing alpha handling unchanged until a real authenticated reset artifact exists.
- **Tests and gate:** keep old private-alpha classification fail-closed. Treat migration as separate future work. It does not block a clean-host DN-15 run.

### DN-15 delivery label — Cut over Linux Base Nix install and uninstall

- **Status:** native x86-64 lifecycle proof and independent evidence review are complete at signed commit `9bd17b716503d7be3bcf5bd310ceddd9aecede50`. GitHub Actions run `33198985687` passed. It did not publish a release.
- **Branch:** `dn/15-linux-lifecycle-cutover`.
- **Delivery PR:** PR 3, based on PR 2.
- **Goal:** activate and prove clean-host Linux Base Nix install and uninstall through Determinate.
- **Why later:** start after PR 2 lands its inactive foundation and evidence. Old private-alpha migration is outside this clean-host cutover.
- **Likely files and symbols:** Linux bootstrap, inactive lifecycle routes, Base Nix Handoff, product asset install, Doctor, release asset selection, and Linux user documents.
- **Interface and invariants:** no runtime fallback. Product assets remain owned by `pkg`. Determinate owns any supported native Base Nix repair and update behavior. `pkg` exposes no Base Nix repair or update action on any alpha platform. A post-alpha product route needs separate approval. Package Repair remains product-owned. Fresh Install alone activates product services. Ordinary N-to-N+1 product upgrade and same-release Product Asset Repair are offline, systemd-query-only, and product-file-only. They require the fixed product units to be inactive and disabled, with exact fragments and no drop-ins. They leave product services offline for operator activation or reboot. Product Asset Repair rolls forward to authenticated same-release bytes. Install authenticates pinned Determinate Nix Installer 3.22.1 and starts it once. One supervisor waits and reaps. After start, there is no product cancellation, signal, hard timeout, or parent-death guarantee. A persisted `Started` Base Nix Handoff is an Unknown Base Nix Outcome and cannot retry. Only exit status `0` plus installed-state validation can become `Accepted`. Live uninstall uses the terminal boundary from DN-13 and requires plain output. Vendor-owned residue is accepted for alpha. Unsupported hosts fail before mutation. Linux user documents describe the new behavior in this PR.
- **Implementation steps:** enable Base Nix install and terminal vendor uninstall for clean Linux hosts. Authenticate the vendor executable. Persist `Started`, start it once, drain bounded output, and keep one supervisor until wait and reap complete. Keep `Started` for any uncertain result. Write `Accepted` only after exit status `0` and installed-state validation. Keep Fresh Install recovery after Accepted Handoff so the same installer can finish product files without a second vendor start. For existing installs, classify the operation as Offline Upgrade or explicit Offline Repair. Query all fixed systemd states and definitions before mutation and again during recovery. Mutate only authenticated product files. Publish the product receipt last. Delete active-upgrade service recovery because offline modes never own service state. For live uninstall, reject structured JSON or JSONL, finish and verify every product action, hold the stable lock, revalidate exact `/nix/nix-installer` and opaque `/nix/receipt.json`, consume Accepted Base Nix Handoff immediately before `exec`, then start terminal vendor uninstall as the last action. Restore Accepted state on synchronous `exec` return. Refuse the unmarked crash window. Run package and product-service smoke tests. Update Linux install and uninstall documents. Keep the old Base Nix implementation present but unreachable for deletion proof. Add no Base Nix repair or update product action.
- **Tests:** run fake install-process tests for exact authentication and arguments, one start, bounded output drain, wait and reap, spawn failure, nonzero exit, signal, lost caller, persisted `Started` after simulated supervisor loss, and failed installed-state validation. Each persisted `Started` case must remain an Unknown Base Nix Outcome and must not retry. Prove that only exit status `0` plus validation becomes `Accepted`. Run the Linux install, terminal plain-output uninstall, synchronous-restore, restore-failure, unmarked-`SIGKILL`, Unknown Base Nix Outcome, Package Repair, and package-operation matrix on a disposable native x86-64 host. The checked-in privileged Docker and systemd harness can run locally or on a GitHub-hosted runner.
- **Proof and evidence:** repeat clean-host Linux install and terminal vendor uninstall proof. Prove live structured-output refusal before mutation. Prove vendor action is last. Prove synchronous `exec` return restores exact Accepted state. Prove restore failure is fail-closed. Prove `SIGKILL` after consumption leaves Base Nix Handoff absent, Base Nix unmarked, restart refusal, and no vendor start. Prove later loss of vendor outcome remains an Unknown Base Nix Outcome. A GitHub-hosted result must name the exact signed commit and retain complete logs, the results matrix, and artifacts for independent review. State that this proof does not prove host boot, reboot, SELinux, foreign-host coexistence, or a complete distribution matrix. Do not claim Base Nix repair or update proof.
- **Deletion:** none. Old Linux Base Nix code remains until DN-17.
- **Rollback or stop rule:** revert before release if any lifecycle row fails. Do not add a runtime fallback.
- **Review focus:** authenticated one-start install, supervisor wait and reap, honest post-start limits, Base Nix Handoff acceptance, terminal vendor uninstall, vendor-action-last ordering, synchronous restore, unmarked crash refusal, Unknown Base Nix Outcome handling, accepted vendor residue, and no Base Nix repair or update product route.
- **Child-unblock condition:** complete. All blocking Linux install, terminal vendor uninstall, synchronous-restore, unmarked-crash, Unknown Base Nix Outcome, and package rows passed twice with no old runtime path used. The native x86-64 evidence received independent review.

The completed native Linux proof included these separate rows:

1. **N-to-N+1 Offline Upgrade:** install N, stop and disable the fixed units,
   run N+1, prove product identity changed, prove Base Nix Handoff and package
   state did not change, and prove services stayed offline.
2. **Same-release Offline Repair:** install a release, stop and disable the
   fixed units while their files are still authenticated, change one product
   service asset, run `pkg-install --repair-product-assets`, prove the exact
   same-release bytes returned, and prove services stayed offline.
3. **Vendor cgroup:** prove the one Determinate process and its descendants use
   the expected containment and no second vendor process starts.
4. **Real systemd:** prove active, enabled, mixed, drop-in, changed-fragment,
   and unqueryable states refuse before product-file mutation. Prove a fully
   inactive and disabled exact unit set can upgrade and repair without a
   systemd mutation command.

All four rows passed in run `33198985687` at exact signed commit
`9bd17b716503d7be3bcf5bd310ceddd9aecede50`. The result did not publish a
release.

### DN-16 delivery label — Cut over Apple Silicon macOS Base Nix install and uninstall

- **Status:** production code is reviewed and backed up at `8ffd325a4be12a998f3a5684097b57841a11540e`. It is not merged. The hosted proof gate validates all 68 sealed files. Each destructive phase directly fetches and validates the exact compact 21-object input set. The workflow does not transport the 445 MB full channel tree through GitHub artifacts. Native proof has not passed.
- **Branch:** `dn/16-macos-lifecycle-cutover`.
- **Delivery PR:** PR 4, based on PR 3.
- **Goal:** activate and prove Apple Silicon macOS Base Nix install and uninstall through Determinate.
- **Why later:** Linux completion is done. Native proof still needs the manual workflow, two exact disposable Apple Silicon runners, two authenticated signed DN-16 release inputs, a staged channel or snapshot protocol that installs N before the channel advances to N+1, and two-phase product lifecycle reboot proof. The signed inputs establish shipping identity only. Docker cannot prove launchd or native reboot behavior.
- **Likely files and symbols:** macOS bootstrap, APFS detection, inactive lifecycle routes, Base Nix Handoff, product launchd assets, release target selection, and macOS user documents.
- **Interface and invariants:** no runtime fallback. Determinate owns Base Nix APFS setup, daemon setup, any supported native repair or update behavior, and uninstall. `pkg` exposes no Base Nix repair or update action on any alpha platform. Package Repair remains product-owned. Install uses the same one-start, one-supervisor, fail-closed Base Nix Handoff contract as Linux. The macOS store-preserving uninstall action remains distinct until PR 4 proves and adopts the terminal vendor uninstall boundary. Keep the shared runtime schema and artifacts until the later PR 4 cleanup work. Intel macOS is not claimed without full proof. Apple Silicon user documents describe the new behavior in this PR.
- **Implementation steps:** enable Base Nix install and terminal vendor uninstall on Apple Silicon after real proof. Authenticate the vendor executable. Persist `Started`, start it once, wait and reap, and accept only exit status `0` plus installed-state validation. For live uninstall, reject structured JSON or JSONL, finish and verify every product action, hold the stable lock, revalidate the exact installed executable and opaque receipt, consume Accepted Base Nix Handoff immediately before `exec`, then start terminal vendor uninstall as the last action. Prove synchronous restore, restore failure, unmarked crash refusal, product launchd ownership, Package Repair, package behavior, and real reboot behavior. Update macOS install and uninstall documents. Add no Base Nix repair or update product action.
- **Tests:** the manual workflow must run fake install-process integration and the Apple Silicon macOS one-start install, supervisor wait and reap, persisted-`Started` refusal, terminal plain-output vendor uninstall, synchronous-restore, restore-failure, unmarked-`SIGKILL`, Unknown Base Nix Outcome, Package Repair, package-operation, crash, and real reboot matrix. Do not run `tests/macos-clean-host/prove.sh` directly.
- **Proof and evidence:** no DN-16 native result exists. The manual workflow must use two exact disposable runners and authenticated signed N and N+1 DN-16 inputs. The hosted gate must first fetch and verify all 68 sealed files and emit only a bounded acquisition receipt. Each prepare and resume phase must then fetch the fixed pair, two fixed inventories, and 18 exact proof-input files directly from the pinned HTTPS channel. This is 21 logical fetches and the workflow-pinned exact response-byte total per phase. Those inputs establish shipping identity only. The staged pair installs N before the authenticated N+1 transition. Clean install, repeat install, one-start supervision, persisted-`Started` refusal, crash, real reboot, Package Repair, package update, product upgrade, terminal vendor uninstall, synchronous restore, restore failure, unmarked refusal, Unknown Base Nix Outcome, and residue reports must pass twice. Vendor action is last. Do not claim Base Nix repair or update proof. Linux or Docker evidence cannot satisfy this gate.
- **Deletion:** none. Old macOS Base Nix code remains until DN-18.
- **Rollback or stop rule:** revert before release if APFS, launchd, installed-executable or receipt identity, package, terminal-uninstall, interruption, crash, or real-reboot proof fails.
- **Review focus:** authenticated one-start install, supervisor wait and reap, honest post-start limits, distinct pre-PR4 store-preserving action, terminal vendor uninstall, vendor-action-last ordering, synchronous restore, unmarked crash refusal, Unknown Base Nix Outcome handling, APFS, launchd, real reboot, accepted vendor residue, target support, and no Base Nix repair or update product route.
- **Child-unblock condition:** blocked. The sealed N and N+1 input pair and two-phase workflow exist, but two complete native proof slots have not passed. Both exact disposable Apple Silicon runners must complete prepare, operator reboot, and resume. All blocking install, terminal vendor uninstall, synchronous-restore, unmarked-crash, Unknown Base Nix Outcome, package, crash, and real reboot rows must pass twice with no old runtime path used.

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
- **Interface and invariants:** docs state the actual owner, diagnostics policy, PATH behavior, supported targets, Base Nix install and uninstall, Package Repair, package update, product upgrade, and old-alpha refusal. Docs state that no Base Nix repair or update action is exposed in alpha.
- **Implementation steps:** run the complete matrix from clean snapshots; publish reproducible evidence; build release metadata; install the built product; run package lifecycle, Package Repair, package update, and product upgrade; terminally `exec` the vendor uninstaller; inspect residue without inferring success from absence; complete cross-platform, privacy, support, and release documents after the new results pass.
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
| DN-05–07 | Complete; assets, one-start invocation, wait and reap, and fail-closed state classification pass | release trust, fake process, Base Nix Handoff, and receipt fault injection |
| DN-08–14 | Unused ownership partition and exact-partition tests are removed; normalized inventory and evidence reports remain accurate | normalized inventory, typed proxy, detection, PATH evidence, terminal vendor uninstall, and decision reports |
| DN-15 | Native x86-64 clean-host Linux install and terminal vendor uninstall pass; complete evidence receives independent review | one-start install; wait and reap; persisted-`Started` refusal; exit-0-plus-validation acceptance; synchronous restore; restore failure; unmarked `SIGKILL`; Unknown Base Nix Outcome; Package Repair and package operations |
| DN-16 | Apple Silicon install and terminal vendor uninstall pass twice in a macOS VM or on another disposable Mac | one-start install; wait and reap; persisted-`Started` refusal; exit-0-plus-validation acceptance; synchronous restore; restore failure; unmarked `SIGKILL`; Unknown Base Nix Outcome; Package Repair, package operations, crash, and real reboot |
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

Evidence for each row records the platform image, architecture, exact signed
product commit, vendor version, vendor full revision, asset digest, exact
invocation, exit status, complete logs, results matrix, file ownership,
services, receipt state, package state, residue, and retained artifacts. Linux
alpha rows can run through the privileged Docker and systemd harness on a
disposable native x86-64 GitHub-hosted runner. An independent reviewer must
accept the evidence. The result proves only that runner and container
environment. macOS rows require an Apple Silicon macOS VM or another disposable
Mac.

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
| Authenticated install start | DN-05–07 / DN-06 | Blocking | Sample | Blocking | Start one pinned 3.22.1 executable through its absolute path and fixed environment |
| Install supervisor | DN-05–07 / DN-06 | Blocking | Sample | Blocking | One supervisor drains bounded diagnostic output, waits, and reaps; stdout and stderr are not a stable progress or completion protocol; no post-start cancellation, signal, hard timeout, parent-death, or second-start promise |
| Started Base Nix Handoff | DN-05–07 / DN-07 | Blocking | Sample | Blocking | Persisted `Started` is an Unknown Base Nix Outcome; restart fails closed and never retries |
| Accepted Base Nix Handoff | DN-05–07 / DN-07 | Blocking | Sample | Blocking | Only exit status `0` plus installed-state validation becomes `Accepted` atomically |
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
| Base Nix repair policy | DN-12 policy; a post-alpha product route needs separate approval | No alpha action | No alpha action | No alpha action | Determinate owns supported native behavior; `pkg` exposes no alpha action; Package Repair stays product-owned |
| Base Nix update policy | DN-12 policy; a post-alpha product route needs separate approval | No alpha action | No alpha action | No alpha action | Determinate owns supported native behavior; `pkg` exposes no alpha action or product ledger |
| Terminal vendor uninstall | DN-13 foundation; DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Plain output only; product actions first; lock; revalidate; consume immediately; `exec` last; restore sync return; refuse unmarked crash; never infer success |
| Old private alpha migration | DN-14 decision | Separate | Separate | Separate | Fail closed; do not block clean-host work or invent an unauthenticated reset route |
| Complete clean install | DN-15 Linux; DN-16 macOS | Blocking | Blocking before target release | Blocking | Accepted Base Nix Handoff and working product |
| Complete repeat install | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Stable install, package, and uninstall result |
| Package Repair after Base Nix cutover | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Product-owned Package Repair works; no Base Nix repair or update action is exposed on any alpha platform |
| N to N+1 product upgrade | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | State, packages, Base Nix Handoff, and identity remain valid |
| Product downgrade | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Follow the explicit proved product policy; do not invoke Base Nix update |
| Explicit terminal uninstall | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Remove and verify product-owned state first, then terminally `exec` vendor uninstall; do not promise vendor completion |
| Install, remove, and update package | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Generation and state transitions work |
| Local build | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Current approval and multi-user safety remain |
| Package roots and GC | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Active Generation and per-user isolation remain |
| Package repair | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Product-owned repair only |
| N-to-N+1 offline product upgrade | DN-15 Linux; DN-16 macOS | Blocking | Blocking | Blocking | Product identity changes; Base Nix Handoff and package state stay valid; product services stay offline |
| Same-release Product Asset Repair | DN-15 Linux | Blocking | Sample | Not yet implemented | Exact authenticated same-release product-file bytes are restored; unknown bytes are never restored; services stay offline |
| Vendor process cgroup | DN-15 Linux | Blocking | Sample | Separate macOS process proof | One vendor process start; expected descendants stay contained; no second vendor process starts |
| Real systemd offline contract | DN-15 Linux | Blocking | Sample | Not applicable | Unsafe service states refuse before file mutation; exact inactive and disabled state uses query commands only |
| Modified product service asset | DN-15 Linux; DN-16 macOS | Blocking | Sample | Blocking | Product repair detects and handles it without changing Base Nix or package state |
| Final release ownership | DN-20 | Blocking | Blocking | Blocking | Each remaining asset has one owner |
| Final release residue | DN-20 | Blocking | Blocking | Blocking | No product lifecycle residue except Linux volatile `/run/pkg-install-handoff.lock` and macOS persistent zero-byte `/private/var/db/pkg-install-handoff.lock`; record accepted vendor-owned alpha residue |
| Optional Root Helper removal | DN-28 | Blocking | Sample | Blocking | Every helper duty has equal replacement proof |
| Optional Broker removal | DN-29 | Blocking | Sample | Blocking | Every Broker duty has equal replacement proof |
| Optional transport and dependency pruning | DN-30 through DN-32 | Blocking | Target check | Blocking | Only dead grammar, edges, and dependencies are removed |
| Intel macOS | DN-03 asset probe; full proof not scheduled | Not applicable | Not applicable | Unsupported-target probe | Do not claim support without a full asset and lifecycle matrix |

`Blocking` records a required gate. It is not the current result. The DN-15
Linux gates passed at `9bd17b716503d7be3bcf5bd310ceddd9aecede50` in run
[`33198985687`](https://github.com/spa5k/pkg/actions/runs/33198985687). The Apple
Silicon macOS gates remain blocked. `Sample` means the architecture result must
match the blocking platforms, but the release owner can define the exact
repeated sample count. `Separate` means the old private-alpha migration is
outside the clean-host cutover. Linux aarch64 becomes fully blocking for all
rows before a Linux aarch64 release. Linux runner and container results do not
prove host boot, reboot, SELinux, foreign-host behavior, or a complete
distribution matrix. Docker and Linux-hosted proof do not satisfy any macOS
row.

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
- Advice to adopt pre-existing Determinate installations became fail-closed classification. Any future adoption needs separate stable identity proof. It never applies to the unmarked terminal-uninstall crash state.
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
- [ ] Are exact commands and results recorded?
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
- Install uses one authenticated vendor start and one supervisor that waits and reaps. There is no safe post-start cancellation, signal, hard timeout, or parent-death promise.
- A persisted `Started` Base Nix Handoff is an Unknown Base Nix Outcome and fails closed without retry. Only exit status `0` plus installed-state validation becomes `Accepted`.
- Pre-uninstall executable, receipt, and Accepted Base Nix Handoff validation fail closed. There is no vendor-phase recovery promise after terminal `exec`.
- Standard-daemon package parity passes before and after cutover.
- Existing foreign, upstream, and unmarked Determinate Nix refuse safely. Old private-alpha state is a separate fail-closed migration case.
- PATH behavior matches the documented user experience.
- Determinate owns supported native Base Nix repair and update behavior. `pkg` exposes no repair or update action on any alpha platform. A post-alpha product route needs separate approval.
- Determinate owns Base Nix uninstall. `pkg` requires plain output, finishes product cleanup, consumes Accepted state, and terminally `exec`s the authenticated installed uninstaller. Vendor completion is not promised. Vendor residue is accepted.
- Vendor-owned residue is accepted for alpha. `pkg` removes all product-owned lifecycle residue. Linux root-owned mode-`0600` `/run/pkg-install-handoff.lock` is volatile. macOS persistent, zero-byte, root-owned mode-`0600` `/private/var/db/pkg-install-handoff.lock` uses a safe parent. Both are coordination exceptions, not lifecycle state.
- Package repair, builds, generations, roots, GC, and state remain correct.
- Old Linux, macOS, and shared private Base Nix code is deleted only where replacement proof exists.
- Broker, Root Helper, and high-fan-in contracts remain where package work still needs them.
- Current user documents match the observed product.
- Every accepted proof result has complete logs, a results matrix, and retained artifacts. GitHub-hosted Linux proof also identifies the exact signed commit and receives independent review.

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

The first code change is DN-01. The first vendor proof is DN-03. The first production Base Nix install and uninstall cutover is DN-15. The first old Base Nix code deletion is DN-17.
