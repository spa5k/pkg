# DN-03b Linux proof findings — Determinate Nix Installer v3.22.1

| | |
|---|---|
| **Report** | DN-03b. Broad Linux x86_64 QEMU behavior evidence plus a Linux aarch64 container asset-execution result. |
| **Date** | 2026-08-23 (UTC). |
| **Parent** | [S6 research findings](../FINDINGS.md), [Linux VM harness](README.md), [stack plan](../../../plans/determinate-nix-stacked-prs.md). |
| **Status** | Linux x86_64 behavior evidence is accepted. Linux aarch64 asset execution is observed but its DN-03 Asset proof is incomplete. The DN-03 parent gate is **NO-GO**. No clean vendor uninstall is claimed. |

## Terms

- **Vendor** means Determinate Systems, the maker of the installer.
- **Vendor executable** means the `nix-installer` binary from vendor release v3.22.1.
- **Guest** means a disposable Ubuntu virtual machine or container that ran a proof.
- **Lane** means one proof scenario. Each lane used fresh disposable guest state.
- **Sentinel** means a marker file that the harness made before the vendor ran. The harness checked that the vendor did not change it.
- **Residue** means a file or account that stays on the host after uninstall.
- **Sentry endpoint** means the file `/etc/nix/sentry-endpoint`. Vendor execution creates this file with the address of its error-reporting service. The exact internal writer is not public. The address is private. This report does not print it.
- **PASS / FAIL** mean the verdicts of the pinned harness checks. They are not product acceptance.
- **Accepted** means reviewers agreed that the evidence is complete and trustworthy.
- **Observed** means the value is recorded in runtime evidence.
- **Source-derived** means the statement follows from the pinned vendor source.
- **Inference** means an architecture conclusion based on observed or source-derived facts.
- **Proposal** means future product policy. It is not current behavior.

---

## 1. Executive decision

**Observed, Linux x86_64.** The vendor executable installed, repaired, ran a same-version daemon upgrade probe, and uninstalled Determinate Nix. The named residue scan outside `/etc/nix` was empty. `/etc/nix` was not empty. Its first recorded entry was `sentry-endpoint`. The x86_64 harness did not inventory every entry below `/etc/nix`, so at least that file remained.

**Observed, Linux aarch64 container.** The guest reported `aarch64`. The pinned executable installed Determinate Nix 3.22.1 in the vendor's documented root-only container mode. The installed Nix executable ran and reported version 2.35.2. Vendor uninstall returned zero. The full `/etc/nix` inventory contained exactly `sentry-endpoint`.

The aarch64 evidence does not record the host CPU architecture independently. This report therefore does not call the run native.

**Observed result.** The strict clean-uninstall residue contract failed on both runs. The x86_64 result proves at least one `/etc/nix` entry remained. The aarch64 result proves `sentry-endpoint` was the only `/etc/nix` entry.

**Incomplete rows.** Neither Linux harness records a receipt hash. The aarch64 lane also has no diagnostics request capture. These missing facts keep the DN-03 parent gate at NO-GO.

**Inference.** The observed install and recovery behavior supports using the pinned vendor executable as a private helper after the DN-03 gate passes. It does not support a clean vendor-uninstall claim. Section 12 gives the recommendation and the future DN-13 cleanup proposal.

## 2. Scope and exact pins

This report covers the Linux x86_64 runtime lanes and the Linux aarch64 asset-execution lane. It covers DN-03b. It does not cover Apple Silicon macOS or the pkg integration PRs.

DN-03b is the Linux execution child of roadmap PR DN-03. The roadmap keeps the parent number DN-03.

Pins used by all five x86_64 lanes:

| Item | Value |
|---|---|
| Vendor version | 3.22.1 |
| Vendor full revision | `4132ad07a15ee7d88c096ac7172b7afb2672866b` |
| Vendor asset | `nix-installer-x86_64-linux` |
| Vendor asset SHA-256 | `9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c` |
| Product revision | `0d4809e452524f7a135d545da3d26d067dd07d2d` |
| Guest image | Ubuntu 24.04 amd64, release `20260814` |
| Guest image SHA-256 | `6e40c07ae715f744f84af0bec76415cc1987dd115b4b8de437818561f01a3733` |

The harness checked the guest image digest before it started QEMU. It checked the vendor asset digest again after it copied the asset inside the guest. It executed only that verified copy, by absolute path.

The aarch64 lane used these additional pins:

| Item | Value |
|---|---|
| Vendor asset | `nix-installer-aarch64-linux` |
| Vendor asset SHA-256 | `9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179` |
| Product revision | `05891b67d78bd9b4fa9a43ada6fb9b0f802d7de2` |
| Container image | `ubuntu@sha256:33ceb71981b602c1a7443a53469e4dba065f7503eab3078a2d7a57a2ab987517` |
| Container architecture | `linux/arm64`; guest `uname -m` returned `aarch64` |

The aarch64 lane mounted the pinned executable read-only. It used the root-only container form documented by the vendor: `install linux --init none --extra-conf 'sandbox = false'`.

**Observed runtime result.** The installed Nix executable printed `nix (Determinate Nix 3.22.1) 2.35.2`.

**Source-derived payload explanation.** The pinned v3.22.1 source embeds the Determinate Nix payload and `determinate-nixd` in the installer build. This is the source-defined local payload for the installed Nix files.

The operator launched the container with Docker `--network none`. The private evidence bundle does not independently record the Docker network setting. This report therefore does not claim a proved no-network run.

Not in scope: pkg Handoff state, package lifecycle, product repair, product uninstall, and final product residue. Those belong to DN-07 and later PRs.

## 3. Test environment and limitations

### Linux x86_64 QEMU environment

- QEMU `q35` machine, TCG acceleration, 2 vCPU, 4096 MiB RAM.
- Guest kernel `6.8.0-137-generic`, x86_64.
- Fresh 30 GiB sparse overlay per lane. The base image stayed read-only.
- User-mode network. Only SSH was forwarded, to `127.0.0.1` on the host.
- systemd was running. cloud-init completed before each lane.
- Each x86_64 guest phase had a two-hour timeout and a 60-second forced-stop grace period.
- The harness refused to run unless the host worktree was clean.

### Linux aarch64 container environment

- Pinned Ubuntu container image `ubuntu@sha256:33ceb71981b602c1a7443a53469e4dba065f7503eab3078a2d7a57a2ab987517`.
- The operator command requested Docker platform `linux/arm64`.
- The guest recorded `uname -m` as `aarch64`.
- The guest recorded kernel `Linux 6.12.76-linuxkit`.
- The evidence does not independently record the host CPU architecture.
- The vendor ran as root with `--init none` and `sandbox = false`.
- No systemd service, reboot, repair, crash recovery, repeat install, or repeat uninstall ran in this lane.

### Environment limitations

- TCG emulation is slow. Timing statements in this report are not performance claims.
- One x86_64 VM image and kernel and one aarch64 container image and kernel were tested. Other Linux distributions and kernels were not tested.
- Only the `linux` planner ran. The `steam-deck` and `ostree` planners were not tested.
- No multi-user package operations ran. Package parity belongs to DN-09.
- The x86_64 diagnostics checks used a guest-local capture service. They are not a full network capture. See section 10.
- Each accepted final lane has one recorded result. This is not repeatability proof. The stack plan asks for two clean runs at cutover time in DN-15.

## 4. Final lane table

The first upstream run is **not** in this table. It is historical. See section 9.

| Lane | Guest exit | Observed result | Exact meaning |
|---|---:|---|---|
| `lifecycle` | 1 | The results file recorded 14 PASS lines and 2 FAIL lines. | The named residue scan outside `/etc/nix` was empty. `/etc/nix` was nonempty, and its first recorded entry was `sentry-endpoint`. The harness did not inventory all `/etc/nix` entries. The vendor contract is FAIL. |
| `crash-recovery` | 0 | Installer killed mid-install, then reboot, then reinstall succeeded. | The vendor survived a hard kill and a reboot. A second install repaired the state. |
| `foreign-nix` | 0 | Vendor install returned 0. Sentinel preserved. | The vendor **accepted** a foreign `/nix` and installed into it. `pkg` must own the refusal policy. |
| `upstream-input` (r2) | 0 | Upstream Nix installed. Later Determinate install returned 1. | The vendor can build an upstream Nix input. The observed Determinate attempt refused it. The lane did not require that status. |
| `diagnostics-disabled` | 0 | Install returned 0 with zero captured requests. | The empty diagnostic endpoint produced no requests to the guest-local capture service. |
| `aarch64-container` (r4) | 1 | Ten asset, install, execution, identity, inventory, and uninstall checks passed. The strict residue check failed. | Linux aarch64 asset execution is observed. The full `/etc/nix` inventory contained only `sentry-endpoint`. Diagnostics capture and a receipt hash were not recorded, so the DN-03 Asset proof remains incomplete. |

The `lifecycle` and `aarch64-container` exit statuses of 1 are correct harness behavior. Each proof found a real strict-residue failure. The x86_64 evidence proves at least the sentry file remained. The aarch64 evidence proves it was the only `/etc/nix` entry.

## 5. Lifecycle observations

These steps are in execution order. All commands used absolute paths.

1. `nix-installer --version` printed `nix-installer 3.22.1`. Exit status 0.
2. The harness recorded the identity of `/etc/nix/sentry-endpoint`. The file was **absent** before install.
3. `nix-installer --help` returned status 0.
4. Install ran with these arguments, in this order: `<staged binary> --diagnostic-endpoint http://127.0.0.1:18080 install --determinate --no-confirm --no-modify-profile`. Exit status 0.
5. The guest-local capture service counted 2 diagnostic requests during that install.
6. The install printed one warning. The vendor self-test ran a build probe through `sh -lc` and `bash -lc`. Both shells could not find `nix` on PATH. This is expected with `--no-modify-profile`. So the install was **not** warning-free.
7. `/nix/receipt.json` existed and was non-empty. `/nix/nix-installer` existed and was executable. The installed copy had the same SHA-256 as the pinned vendor asset. The harness did not hash the receipt.
8. `/etc/nix/sentry-endpoint` now existed. It was a regular file. It was owned by `root:root`. Its mode was `0600`. Its size was 95 bytes.
9. The guest performed a clean reboot. The kernel boot ID changed. The boot ID value is private and is not printed here.
10. Repeat install used the same arguments. Exit status 0. The capture service counted 0 diagnostic requests. The receipt and the installed copy stayed intact.
11. Default repair ran as `/nix/nix-installer --diagnostic-endpoint '' repair --no-confirm`. Exit status 0.
12. Sequoia repair ran as `/nix/nix-installer --diagnostic-endpoint '' repair sequoia --no-confirm`. Exit status 1. The vendor refused it on Linux. The refusal message said the command is macOS only.
13. The harness ran only these help probes from the staged installer: `update --help`, `upgrade --help`, and `self-update --help`. Each returned status 2 and an unrecognized-subcommand error. It did not run the bare forms.
14. `/usr/local/bin/determinate-nixd` existed. It was a regular file, owned by `root:root`, mode `555`. `determinate-nixd version` returned status 0. It reported daemon and client version 3.22.1.
15. `/usr/local/bin/determinate-nixd upgrade --version v3.22.1` returned status 0. This is the pinned same-version upgrade probe.
16. `/etc/nix/sentry-endpoint` after the daemon upgrade was identical to the file after install.
17. Uninstall ran as `/nix/nix-installer --diagnostic-endpoint '' uninstall --no-confirm /nix/receipt.json`. Exit status 0.
18. `/etc/nix/sentry-endpoint` after uninstall was still present and unchanged.
19. `/nix/receipt.json` was absent. A repeat uninstall from the staged copy returned status 1. Its output matched the pinned missing-receipt refusal. It reported reading the receipt and a missing file.
20. The named residue scan outside `/etc/nix` found no residue. `/nix/receipt.json`, `/nix`, and `/usr/local/bin/determinate-nixd` were absent. No nix or determinate systemd unit files remained. No nix or determinate entries remained in `/usr/local/bin`. No `nixbld<N>` users remained. The separate `residue.txt` file was empty.
21. `/etc/nix` remained. It was a directory, owned by `root:root`, mode `755`, and not a symlink. It was **not empty**. The first recorded entry was `sentry-endpoint`. The harness did not count all entries.
22. The pinned residue contract therefore failed. The results file recorded two FAIL lines: `/etc/nix is unsafe or nonempty`, and `uninstall observations violate the pinned residue contract`. The harness did not prove that `sentry-endpoint` was the only x86_64 `/etc/nix` entry.

The pinned contract allows `/etc/nix` to remain only when it is empty. One file is enough to fail it.

## 6. Sentry residue finding

Definition. The sentry endpoint file holds the address that the vendor uses for anonymous error reports.

### Linux x86_64 observations

| Stage | Result |
|---|---|
| Before install | Absent |
| After install | Regular file, `root:root`, mode `0600`, 95 bytes |
| After `determinate-nixd upgrade --version v3.22.1` | Same file, unchanged |
| After uninstall | Same file, unchanged |

A private host-side check compared each capture with the default endpoint string extracted from the pinned vendor binary. The check found that all three captures were byte-identical and matched that embedded default. This report records only that redacted result. The address itself, the raw bytes, and the file hash are private and are not published.

Consequences:

- The file appeared during the install phase. This proof does not identify which internal vendor component wrote it.
- The daemon upgrade does not change it.
- Uninstall does not remove it.
- This recorded file is sufficient to fail the strict lifecycle residue contract.
- The harness did not inventory every x86_64 `/etc/nix` entry, so it does not prove this was the only residue inside that directory.

### Linux aarch64 container observations

The private r4 evidence records this non-secret metadata:

| Stage | Type | Numeric owner | Mode | Size | Link count |
|---|---|---|---|---:|---|
| After install | Regular file, not a symlink | uid 0, gid 0 | `0600` | 95 bytes | Not recorded |
| After uninstall | Regular file, not a symlink | uid 0, gid 0 | `0600` | 95 bytes | Not recorded |

The full post-uninstall `/etc/nix` inventory contained exactly `/etc/nix/sentry-endpoint`. The private identity hashes before and after uninstall were equal. The digest value, endpoint address, and file bytes remain private. Metadata and the private identity hash are separate evidence.

**Source-derived conclusion.** The pinned uninstaller reverses receipt actions. Its visible Determinate daemon revert removes `nix.conf`, then removes `/etc/nix` only when that directory is empty. The pinned public source contains no direct `sentry-endpoint` removal action. This explains why the visible revert path leaves a nonempty `/etc/nix`. It does not identify the private internal component that created the file.

**Inference.** The file is vendor-owned residue because it appeared during vendor execution and survived successful vendor uninstall. The evidence does not prove intentional retention or a vendor defect.

This report adds no product cleanup for this file. Section 12 routes the final residue policy to the later uninstall-policy PR.

## 7. Crash recovery result

Method:

1. Install started with diagnostics disabled.
2. The harness waited for observable progress. Progress meant both of these: `/usr/local/bin/determinate-nixd` existed, and `/nix/store` had at least one entry.
3. The harness sent SIGKILL to the installer.
4. The guest rebooted.
5. Install ran again with the same arguments.

Result: exit status 0. `/nix/receipt.json` was present and non-empty. `/nix/nix-installer` was present and executable.

Honest limit: this proves receipt and helper presence after recovery. It does not run a functional Nix command. A functional check belongs to a later PR.

## 8. Foreign Nix input result

Method: the harness created `/nix` with mode `0755` and wrote a sentinel file into it. Then the vendor install ran.

Result:

- Vendor exit status **0**. The vendor accepted the foreign `/nix` and installed into it.
- The sentinel file was still present after the install.
- The harness compared the sentinel hash before and after the install, inside the guest. The hash did not change.
- After the run, `/nix/store` had many entries, `/nix/receipt.json` existed, and `/nix/nix-installer` existed.

Meaning: the vendor does **not** refuse a foreign `/nix`. It installs into it. So `pkg` product preflight must own the refusal policy. The stack plan already requires this in DN-10.

Honest limits:

- The lane stored no independent before/after hashes in the evidence. The comparison ran, but its values were not recorded.
- The input was a plain non-empty `/nix`, not a complete foreign Nix install. No foreign daemon, users, or receipt existed.
- This lane does not prove full filesystem preservation and does not set final product policy.

## 9. Upstream Nix input result

Final run (revision r2):

1. Install ran as `<staged binary> --diagnostic-endpoint '' install --prefer-upstream-nix --no-confirm --no-modify-profile`. Exit status 0.
2. `/nix/var/nix/profiles/default/bin/nix --version` printed `nix (Nix) 2.35.2`. Exit status 0.
3. `/nix/receipt.json` was present and non-empty.
4. A second install ran as `<staged binary> --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile`. Exit status **1**. The vendor refused to install Determinate over the upstream install.

Meaning: the pinned vendor executable can create a real upstream Nix input. In this run, the later Determinate attempt returned status 1 and refused the different planner settings.

This report does not claim conversion of an upstream install into a Determinate install. That path was not proved.

Historical note, kept as evidence only: the first upstream run, in a different guest, returned status 1 during `provision_nix`. The guest DNS could not resolve `releases.nixos.org`. The vendor tried to fetch the upstream Nix tarball and failed on DNS. That attempt left a partial plan, and the later Determinate command also returned status 1. The run is not part of the accepted matrix and is not counted in section 4 or section 14.

Honest limits:

- The refusal check confirmed that a receipt still existed after the refused run. It did not compare before and after receipt hashes.
- The harness recorded the Determinate exit status. It did not require status 1 for the lane to pass. DN-10 must enforce the product refusal policy.

## 10. Diagnostics-disabled result

Method: the guest-local capture service listened on `127.0.0.1:18081`. The install ran with an empty `--diagnostic-endpoint` value. The environment also set `DETSYS_IDS_TRANSPORT` to the capture address, so that any other report would go to the capture service.

Result: install exit status 0. The capture counter read **0**. Zero requests reached the guest-local capture endpoint.

Honest limits:

- This is not a full packet capture.
- It does not prove that all possible network traffic was absent.
- It proves only that the disabled configuration made no request to the controlled transport endpoint.

The parent S6 findings record the vendor disable contract from vendor help text and vendor source. This lane supports that contract on Linux x86_64.

## 11. Summary of pinned vendor behaviors

### Receipt evidence status

**Observed.** The checked-in x86_64 harness verifies that `/nix/receipt.json` is nonempty, but it does not compute or save a receipt hash. The aarch64 evidence file `/var/tmp/pkg-s6-dn03b-aarch64-evidence/probe-05891b6-r4/receipt.stat` records a non-symlink regular file owned by uid 0 and gid 0, mode `0600`, and size 30,503 bytes. The aarch64 probe also does not compute a receipt hash.

Receipt contents remain private and were not archived. No receipt-digest evidence file exists for these Linux runs. A later receipt digest must stay in private evidence because it fingerprints receipt state. The roadmap receipt-hash evidence requirement is therefore **Unproved**. The standalone receipt row currently proves presence and installed-copy identity only.

| Behavior | Command form | Result |
|---|---|---|
| Install | `install --determinate --no-confirm --no-modify-profile` | Status 0. One warning. See section 5, step 6. |
| Repeat install | Same | Status 0. Install intact. No diagnostic requests. |
| Clean reboot | `systemctl reboot` | Boot ID changed. Install intact. |
| Default repair | `repair --no-confirm` | Status 0. |
| Sequoia repair on Linux | `repair sequoia --no-confirm` | Status 1. Refused. macOS-only command. |
| Installer `update` | `update --help` | Status 2. No such subcommand. |
| Installer `upgrade` | `upgrade --help` | Status 2. No such subcommand. |
| Installer `self-update` | `self-update --help` | Status 2. No such subcommand. |
| Daemon upgrade, same version | `determinate-nixd upgrade --version v3.22.1` | Status 0. |
| Uninstall | `/nix/nix-installer ... uninstall --no-confirm /nix/receipt.json` | Status 0. At least `sentry-endpoint` remained on x86_64; it was the only `/etc/nix` entry on aarch64. |
| Repeat uninstall | Same, against a missing receipt | Status 1. Pinned missing-receipt refusal. |

## 12. Architecture recommendation

Recommendation: use the pinned standalone vendor executable as a **private bundled implementation detail**.

Rules:

1. `pkg` installs Nix. The user does not install Nix first. No user-visible install step depends on a user-run vendor command.
2. Do not integrate the experimental Rust crate. The [pinned vendor documentation](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/docs/rust-library.md) says that library use is experimental and can be removed.
3. Do not copy or fork the vendor source. Do not copy selected upstream files.
4. Do not publish or link the helper in a public PATH. Keep the executable private to `pkg`. Invoke it by absolute authenticated path only.
5. `pkg` owns verification, orchestration, progress, cancellation, reboot, rollback, and product state.
6. `pkg` owns the foreign-input refusal policy. The vendor accepts a foreign `/nix`. Only `pkg` preflight can refuse it.
7. **Current decision:** do not add product cleanup for the sentry endpoint in DN-03.
8. **Ordered ownership:** DN-03 records vendor behavior and the ownership boundary. DN-13 owns product cleanup after the earlier integration PRs exist. This is an ordered dependency, not a DN-03-to-DN-13 cycle.
9. **Future DN-13 proposal:** after successful vendor uninstall, inspect only `/etc/nix/sentry-endpoint` without following links. Remove it only when every property is present and exact: non-symlink regular file, uid 0, gid 0, mode `0600`, link count 1, and the size and digest approved for the pinned vendor version and target. Any missing or mismatched property leaves the file in place and keeps the strict residue verdict FAIL. Never recursively delete `/etc/nix`. Remove `/etc/nix` only after the exact file checks passed and the directory is empty, not a symlink, owned by uid 0 and gid 0, and mode `0755`. Current evidence does not record link count, so it cannot yet authorize this future rule. DN-03 does not apply this proposal.

Reasons:

- The x86_64 evidence proves the tested install, repair, same-version upgrade, uninstall, and crash-recovery behavior.
- Receipt-hash evidence and the aarch64 diagnostics Asset proof remain unresolved in DN-03.
- A private path does not remove LGPL-2.1 obligations. DN-05 must keep the license notice and source inventory, and it must complete a distribution compliance review.

## 13. Stop-gate decision

### Linux x86_64 decision

The x86_64 evidence is sufficient to describe the tested vendor install, repeat install, reboot, repair, help-probe, same-version daemon upgrade, crash recovery, diagnostics control, foreign input, upstream input, and uninstall behavior. It proves at least `sentry-endpoint` remained inside `/etc/nix`. It does not prove a clean vendor uninstall. It also does not provide the receipt hash required by the roadmap evidence list.

### Linux aarch64 Asset proof mapping

The roadmap uses `Asset proof` for three DN-03 matrix rows: standalone invocation and arguments, diagnostics control, and standalone receipt and installed-copy behavior.

The aarch64 r4 lane proves these facts:

- the pinned asset digest matched;
- the guest reported `aarch64`;
- the installer version and exact install arguments were recorded;
- install returned 0;
- a nonempty receipt was present and its metadata was recorded;
- the installed vendor copy matched the pinned asset digest;
- the installed Nix executable ran;
- uninstall returned 0;
- the post-uninstall `/etc/nix` inventory contained only `sentry-endpoint`.

The lane does not prove diagnostics request suppression. It records no receipt hash. It does not run the aarch64 Sample rows: repeat install, SIGKILL and reboot, repair and update, or repeat uninstall. Therefore it proves aarch64 asset execution, but it does not yet complete every DN-03 `Asset proof` row or the child-unblock asset-support condition.

### Parent gate

The DN-03 parent remains **NO-GO** until all remaining DN-03 rows pass. This includes the missing Linux receipt-hash evidence, the missing aarch64 diagnostics Asset proof, the blocking Apple Silicon macOS rows, and the Intel macOS asset-availability result. The macOS DN-03 evidence lane owns the Apple Silicon rows and the Intel asset-availability probe.

DN-04 cannot start before the DN-03 parent passes. DN-13 later owns exact product cleanup. This ordering is not a dependency cycle because DN-03 proves vendor behavior and ownership; DN-13 implements and proves product-owned cleanup after integration support exists.

Do not call either Linux residue lane a clean-uninstall PASS. Do not claim clean vendor uninstall.

## 14. Evidence manifest

All final evidence directories live outside the repository. The x86_64 evidence is under `/var/tmp/pkg-s6-dn03b-evidence/`. The aarch64 evidence is under `/var/tmp/pkg-s6-dn03b-aarch64-evidence/`. Nothing from either root is committed.

Common values for all five lanes:

- Product revision: `0d4809e452524f7a135d545da3d26d067dd07d2d`
- Vendor revision: `4132ad07a15ee7d88c096ac7172b7afb2672866b`
- Vendor asset SHA-256: `9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c`
- Guest image SHA-256: `6e40c07ae715f744f84af0bec76415cc1987dd115b4b8de437818561f01a3733`

| Directory | Lane | UTC run date | Guest exit | Results SHA-256 |
|---|---|---|---:|---|
| `lifecycle-0d4809e` | lifecycle | 2026-08-22T14:12:29Z | 1 | `2fd082265c038e91207ab9988e17544022b0a4e846c12f0af81f2705d59b6090` |
| `crash-recovery-0d4809e` | crash-recovery | 2026-08-22T14:19:10Z | 0 | `a427be0f40bf67de85aa202d6962e8feeadee1a26cfbd15c3a45c6491b519c24` |
| `foreign-nix-0d4809e` | foreign-nix | 2026-08-22T14:21:48Z | 0 | `b6fc7ae351738e8413e27fb2bd9cb80693f6870ae4029a6ffc4422d1d71aa165` |
| `upstream-input-0d4809e-r2` | upstream-input | 2026-08-22T14:25:44Z | 0 | `adc33230ee4e5001e3070e49591d87be5697a9fa6362ede258cfe08ac8a79208` |
| `diagnostics-disabled-0d4809e` | diagnostics-disabled | 2026-08-22T14:27:45Z | 0 | `a1d4db35bd2af8a9ad02b33bd90c16bef65a693062c711823e534eb8cddc661c` |

About the results hashes:

- Each hash is the SHA-256 of the lane's `guest-evidence/results` file.
- The `results` file holds only the public lane summary lines. It holds no receipt, no sentry bytes, and no endpoint address.
- So the hashes fingerprint only the public lane summaries. They do not expose private receipts.
- The first upstream run, `upstream-input-0d4809e`, is historical and is not part of this manifest. It is retained only to explain the transient DNS failure.

The private aarch64 evidence is under `/var/tmp/pkg-s6-dn03b-aarch64-evidence/probe-05891b6-r4/`.

| Directory | Lane | UTC run date | Container exit | Results SHA-256 | Probe SHA-256 |
|---|---|---|---:|---|---|
| `probe-05891b6-r4` | aarch64-container | 2026-08-23T19:53:03Z | 1 | `c8b3a3511e3e00901f5751ce1361afa47ce524332ef306a8b53aa056ee8e292f` | `ead1f75ac0e7a2e578b12196e4ec96312bf946bcabdc3e00877dd3e10fd9b32b` |

The aarch64 output records ten PASS lines and one strict-residue FAIL line. The only `/etc/nix` entry was `sentry-endpoint`. It was a non-symlink regular file with uid 0, gid 0, mode `0600`, and size 95 bytes. Link count was not recorded. The receipt contents and sentry bytes were not archived. Receipt metadata and a private sentry identity hash were recorded. The private digest value is not published.

## 15. Primary sources

These sources are official and primary. No blog or secondary source is cited.

- [Exact release v3.22.1](https://github.com/DeterminateSystems/nix-installer/releases/tag/v3.22.1)
- [Release asset used here](https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-x86_64-linux)
- [Linux aarch64 release asset used here](https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-aarch64-linux)
- [Pinned official Linux and container support documentation](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/README.md#in-a-container)
- [Pinned source commit](https://github.com/DeterminateSystems/nix-installer/commit/4132ad07a15ee7d88c096ac7172b7afb2672866b)
- [Pinned embedded payload source](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/distribution.rs)
- [Pinned daemon revert source](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/action/common/provision_determinate_nixd.rs)
- [Repository LGPL-2.1 license at the pinned commit](https://raw.githubusercontent.com/DeterminateSystems/nix-installer/4132ad07a15ee7d88c096ac7172b7afb2672866b/LICENSE)
- [Pinned Rust library warning](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/docs/rust-library.md)
- [Official diagnostics documentation](https://docs.determinate.systems/guides/telemetry/)
- Pinned diagnostics source: [`src/diagnostics.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/diagnostics.rs), [`src/cli/mod.rs`](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/src/cli/mod.rs), and [`detsys-ids-client` 0.7.0](https://static.crates.io/crates/detsys-ids-client/detsys-ids-client-0.7.0.crate)
- [Official daemon documentation, including `determinate-nixd upgrade`](https://docs.determinate.systems/determinate-nix/determinate-nixd/)
- [Official uninstall guidance](https://docs.determinate.systems/troubleshooting/installation-failed-macos/)
- [Ubuntu release image](https://cloud-images.ubuntu.com/releases/noble/release-20260814/ubuntu-24.04-server-cloudimg-amd64.img)

## 16. Honest limitations and later proof work

Limitations of this spike:

1. Linux x86_64 received the broad behavior run. Linux aarch64 received only a root-only container asset-execution run. The macOS DN-03 evidence lane is separate.
2. One x86_64 VM image and one aarch64 container image were tested.
3. One run per lane. Cutover PRs require two clean runs.
4. Crash recovery proves receipt and helper presence only. It does not run a functional Nix command.
5. Foreign input stored no independent before/after sentinel hashes in the evidence. The in-guest comparison passed, but it cannot be rechecked from the files.
6. Foreign input was a plain non-empty `/nix`, not a complete foreign Nix install.
7. Upstream refusal checked receipt presence only. It did not compare before and after receipt hashes.
8. The harness recorded the upstream Determinate exit status. It did not require refusal for the lane to pass.
9. Diagnostics-disabled proves only the controlled transport counter. It is not a packet capture. It does not prove that all network traffic was absent.
10. The lifecycle lane did not prove pkg Handoff, package lifecycle, product repair, or product uninstall. Those belong to later PRs.
11. The vendor executable ran as root inside a disposable guest. Behavior under a restricted or hardened host was not tested.
12. `determinate-nixd upgrade --version v3.22.1` was a same-version probe. A real N to N+1 upgrade was not run.
13. The aarch64 container run did not run systemd, reboot, repair, crash recovery, diagnostics capture, repeat install, or repeat uninstall.
14. Neither Linux run recorded the roadmap-required receipt hash.
15. The macOS DN-03 evidence lane still owns the blocking Apple Silicon rows and Intel macOS asset availability.

Later proof work:

- DN-07: prove Handoff crash boundaries and receipt validation.
- DN-09: prove standard-daemon package parity, including functional Nix commands.
- DN-10: prove the foreign-input and upstream-input refusal policy in `pkg` preflight.
- DN-11: prove PATH behavior for login shells, non-login shells, and GUI launches.
- DN-13: decide and prove the final uninstall residue policy, with exact type, owner, mode, and hash checks.
- DN-15: prove the complete Linux lifecycle twice from clean snapshots, including a real version upgrade.
