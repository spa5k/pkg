# DN-03b Linux proof findings — Determinate Nix Installer v3.22.1

| | |
|---|---|
| **Report** | DN-03b. Broad Linux x86_64 QEMU behavior evidence plus Linux x86_64 and aarch64 container Asset proofs. |
| **Date** | 2026-08-24 (report date; R12 ran on 2026-08-23 UTC). |
| **Parent** | [S6 research findings](../FINDINGS.md), [Linux VM harness](README.md), [stack plan](../../../plans/determinate-nix-stacked-prs.md). |
| **Status** | Fresh R12 proves the required Linux x86_64 behavior and both Linux target Asset proofs. R10 proves the Apple Silicon lifecycle and residue rows. Crash R1 completes the standalone SIGKILL and reboot observation with a negative result. Parent status: COMPLETE — EVIDENCE COMPLETE; PRODUCT DELIVERY NO-GO. DN-04 may document the proved contract. Linux clean vendor uninstall remains `FAIL`. DN-13 owns exact residue cleanup only. |

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

**Observed, fresh Linux x86_64 R12.** Five fresh QEMU lanes ran from product revision `33b386dd473e66c7772c4392d7f56953e1398595`. Install, repeat install, clean reboot, repair, the pinned same-version daemon upgrade, uninstall, repeat-uninstall refusal, crash recovery, diagnostics-disabled behavior, foreign-input preservation, and upstream-input refusal all produced reviewable evidence. The lifecycle guest returned 1 only because the strict residue contract recorded two related FAIL lines. `/etc/nix/sentry-endpoint` remained after successful uninstall. All other lifecycle functional checks passed.

**Observed, final Linux x86_64 container.** The guest reported `x86_64`. The pinned executable installed Determinate Nix 3.22.1. The installed Nix executable ran and reported version 2.35.2. The loopback canary recorded zero requests while telemetry was disabled. The receipt was a safe regular file. Its metadata and a private SHA-256 were recorded without copying its contents. The installed helper matched the pinned asset. Vendor uninstall returned zero. The full `/etc/nix` inventory contained exactly `sentry-endpoint`. Docker removed the exact container.

**Observed, Linux aarch64 container.** The guest reported `aarch64`. Docker reported an ARM64 Linux server, and the host reported Darwin arm64. The pinned executable installed Determinate Nix 3.22.1 in the vendor's documented root-only container mode. The installed Nix executable ran and reported version 2.35.2. The loopback canary recorded zero diagnostics requests while telemetry was disabled. The receipt metadata and a private SHA-256 were recorded before uninstall. Vendor uninstall returned zero. The full `/etc/nix` inventory contained exactly `sentry-endpoint`.

This is an ARM64 Linux container on an ARM64 Docker server. It is not a bare-metal Linux proof. The evidence does not independently prove the absence of every virtualization or translation layer, so this report does not call the run native.

**Observed result.** The strict clean-uninstall residue contract failed in the fresh broad x86_64 R12 run and both retained container Asset proofs. R12 proves that at least `sentry-endpoint` remained. R12 did not count every `/etc/nix` entry. The retained x86_64 R11 and aarch64 R10 container inventories prove that `sentry-endpoint` was the only `/etc/nix` entry in those guests.

**Observed, Linux.** The strict sentry residue remains a real clean-uninstall `FAIL`. Fresh guest state, creation during vendor execution, and unchanged private bytes and digest values after uninstall are recorded in R12.

**Inference.** These runtime facts support attribution of the sentry residue to vendor execution.

**Decision.** The required Linux behavior and the Linux aarch64 and x86_64 Asset evidence are accepted. R10 proves the Apple Silicon lifecycle and residue rows. Crash R1 completes the standalone SIGKILL and reboot observation with a negative result. The parent status is **COMPLETE — EVIDENCE COMPLETE; PRODUCT DELIVERY NO-GO.** DN-04 may document the proved contract. DN-13 owns exact residue cleanup only; DN-03 does not apply it.

**Inference.** The observed install and recovery behavior supports using the pinned vendor executable as a private helper after the DN-03 gate passes. It does not support a clean vendor-uninstall claim. Section 12 gives the recommendation and the future DN-13 cleanup proposal.

## 2. Scope and exact pins

This report covers the Linux x86_64 runtime lanes and the Linux aarch64 Asset proof lane. It covers DN-03b. It does not cover Apple Silicon macOS or the pkg integration PRs.

DN-03b is the Linux execution child of roadmap PR DN-03. The roadmap keeps the parent number DN-03.

Pins used by all five x86_64 lanes:

| Item | Value |
|---|---|
| Vendor version | 3.22.1 |
| Vendor full revision | `4132ad07a15ee7d88c096ac7172b7afb2672866b` |
| Vendor asset | `nix-installer-x86_64-linux` |
| Vendor asset SHA-256 | `9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c` |
| Product revision | `33b386dd473e66c7772c4392d7f56953e1398595` |
| Guest image | Ubuntu 24.04 amd64, release `20260814` |
| Guest image SHA-256 | `6e40c07ae715f744f84af0bec76415cc1987dd115b4b8de437818561f01a3733` |

The harness checked the guest image digest before it started QEMU. It checked the vendor asset digest again after it copied the asset inside the guest. It executed only that verified copy, by absolute path.

The final aarch64 r10 lane used these additional pins:

| Item | Value |
|---|---|
| Vendor asset | `nix-installer-aarch64-linux` |
| Vendor asset SHA-256 | `9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179` |
| Product revision | `16f0bbef1fa8d329b55da498828e0c2ba616a43c` |
| Container image | `ubuntu@sha256:33ceb71981b602c1a7443a53469e4dba065f7503eab3078a2d7a57a2ab987517` |
| Container architecture | `linux/arm64`; guest `uname -m` returned `aarch64` |

The aarch64 lane mounted the pinned executable and probe read-only. It used the root-only container form documented by the vendor: `install linux --init none --extra-conf 'sandbox = false'`.

The exact diagnostics environment override was `DETSYS_IDS_TELEMETRY=disabled`. The exact state-changing command used an explicit loopback endpoint:

```text
/input/nix-installer-aarch64-linux --diagnostic-endpoint http://127.0.0.1:18080 install linux --determinate --no-confirm --no-modify-profile --init none --extra-conf 'sandbox = false'
```

The uninstall used the same environment and loopback endpoint. Docker used `--network none`. The in-container canary listened only on `127.0.0.1:18080` and counted requests. It recorded zero requests after install and zero total requests after uninstall.

**Observed runtime result.** The installed Nix executable printed `nix (Determinate Nix 3.22.1) 2.35.2`.

**Source-derived payload explanation.** The pinned v3.22.1 source embeds the Determinate Nix payload and `determinate-nixd` in the installer build. This is the source-defined local payload for the installed Nix files.

The exact Docker argv is in private evidence. It used `--rm`, the exact name `pkg-s6-dn03b-arm64-probe-16f0bbe-r10`, `--platform linux/arm64`, `--network none`, and the pinned image digest. Immediately after Docker returned, the host runner compared `docker ps -a` names by exact equality. The saved match list was empty and the saved count was zero. The cleanup record and complete private evidence bundle have checksum manifests.

The diagnostics check is not a packet capture and does not prove that all possible traffic was absent. It proves that neither state-changing command reached the controlled loopback endpoint while the supported telemetry-disable policy was active.

The final x86_64 r11 lane used these additional pins:

| Item | Value |
|---|---|
| Vendor asset | `nix-installer-x86_64-linux` |
| Vendor asset SHA-256 | `9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c` |
| Product revision | `7ff31c5c86e6a6d11d9594a0657062d76767c271` |
| Authenticated image index | `ubuntu@sha256:33ceb71981b602c1a7443a53469e4dba065f7503eab3078a2d7a57a2ab987517` |
| Exact Linux AMD64 child | `ubuntu@sha256:1e0a86e57d247923571b75e0aaf48a1449cf8c543d51fb3e07a4a7d7bfa79316` |
| Container architecture | Requested `linux/amd64`; image inspection reported `linux/amd64`; guest reported `x86_64` |

**Observed.** Docker fetched the raw authenticated index. The SHA-256 of those raw bytes was exactly the pinned index digest. The index contained one Linux AMD64 child. Docker pulled that exact child and inspected it as `linux/amd64`.

**Observed.** The host and Docker server were ARM64. The requested and inspected guest platform was AMD64. The guest reported `x86_64`. The first fresh r10 attempt failed when Nix could not load its syscall filter. The r10 container was removed exactly, and its evidence remains unchanged. The corrected r11 container kept `sandbox = false` and also used `filter-syscalls = false`.

**Inference.** The x86_64 guest ran under Docker's architecture emulation. The two Nix settings are disposable container-proof inputs. They are not product settings.

**Observed.** The r11 install and uninstall used `DETSYS_IDS_TELEMETRY=disabled`, the explicit loopback endpoint, and Docker `--network none`. The live canary recorded zero requests after install and zero total requests after uninstall. This is not a packet capture. It proves only that the state-changing commands did not reach the controlled loopback endpoint while the supported telemetry-disable policy was active.

**Retained container evidence.** R11 remains the final x86_64 container Asset proof. Its exact runtime evidence and probe hash did not change. Later static corrections added exact destructive approval, rejected pre-existing Nix state before the first evidence write, kept `filter-syscalls = false` specific to x86_64 emulation, and removed QEMU receipt content capture. The fresh R12 QEMU lanes ran the corrected QEMU receipt path at source `33b386d`. R12 does not replace the narrow R11 container Asset proof.

### Intel macOS asset availability

**Observed.** An authenticated GitHub API response for official release v3.22.1 listed exactly four assets: `nix-installer-aarch64-darwin`, `nix-installer-aarch64-linux`, `nix-installer-x86_64-linux`, and `nix-installer.sh`. It listed no `nix-installer-x86_64-darwin` asset. The exact-name count was zero. No Intel asset was downloaded.

**Source-derived.** The pinned official source also excludes `x86_64-darwin` from `supportedSystems` and has no Intel macOS build workflow. See the parent S6 findings, sections 4 and 5.

**Conclusion.** Release v3.22.1 has no standalone Intel macOS executable. Intel macOS remains unsupported. This is an asset-availability result only. It is not a lifecycle result.

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
- Docker reported a Linux ARM64 server. The host recorded Darwin arm64.
- The vendor ran as root with `--init none` and `sandbox = false`.
- Docker used `--network none`. A loopback canary remained active during install and uninstall.
- No systemd service, reboot, repair, crash recovery, repeat install, or repeat uninstall ran in this lane.

### Final Linux x86_64 container environment

- The authenticated index resolved to the exact Linux AMD64 child digest recorded in section 2.
- Docker image inspection reported `linux/amd64`. The guest reported `x86_64`.
- The host and Docker server reported ARM64. This was an emulated container proof. It was not a native or bare-metal proof.
- Docker used `--network none`. A live loopback canary stayed active during install and uninstall.
- The vendor ran as root with `--init none`, `sandbox = false`, and `filter-syscalls = false`.
- No systemd service, reboot, repair, crash recovery, repeat install, or repeat uninstall ran in this lane.
- Docker `--rm` removed the exact recorded CID and exact container name. Both later inspections failed, and the exact-ID container list was empty.

### Environment limitations

- TCG emulation is slow. Timing statements in this report are not performance claims.
- One x86_64 VM image and kernel and one aarch64 container image and kernel were tested. Other Linux distributions and kernels were not tested.
- Only the `linux` planner ran. The `steam-deck` and `ostree` planners were not tested.
- No multi-user package operations ran. Package parity belongs to DN-09.
- The x86_64 diagnostics checks used a guest-local capture service. They are not a full network capture. See section 10.
- Each R12 lane has one recorded result. This is not repeatability proof. The stack plan asks for two clean runs at cutover time in DN-15.

## 4. Final lane table

The first five rows are the fresh R12 QEMU evidence at source `33b386d`. R12 replaces the unavailable older broad QEMU directories as gate evidence. R10 and R11 remain the narrow container Asset proofs.

| Lane | Guest exit | Observed result | Exact meaning |
|---|---:|---|---|
| `lifecycle` (R12) | 1 | Fourteen functional checks passed. Two related strict-residue checks failed. | Install, clean reboot with changed boot ID, repeat install, repair, pinned daemon upgrade, uninstall, and repeat-uninstall refusal worked. The guest returned 1 only because `/etc/nix` was nonempty after uninstall. Its first recorded entry was `sentry-endpoint`. |
| `crash-recovery` (R12) | 0 | The active installer was killed after observable progress. Reboot and reinstall succeeded. | The vendor recovered receipt and helper state after SIGKILL and reboot. |
| `foreign-nix` (R12) | 0 | Vendor install returned 0. The sentinel was preserved. | The vendor **accepted** a foreign `/nix` and installed into it. `pkg` must own the refusal policy. |
| `upstream-input` (R12) | 0 | Upstream Nix 2.35.2 installed. Later Determinate install returned 1. The upstream receipt remained present. | The observed Determinate attempt refused the upstream input without removing the existing receipt. The lane did not compare receipt hashes. |
| `diagnostics-disabled` (R12) | 0 | Install returned 0 with zero captured requests. | The empty diagnostic endpoint produced no requests to the guest-local capture service. |
| `aarch64-container` (r10) | 1 | Eleven in-container checks passed. The immediate host cleanup count was zero. The strict residue check failed. | The three Linux aarch64 Asset proof rows are complete. The full `/etc/nix` inventory contained only `sentry-endpoint`. This is not a Sample-row proof. |
| `x86_64-container` (r11) | 1 | Eleven in-container checks passed. The strict residue check failed. The separate exact-container cleanup passed. | The three Linux x86_64 Asset proof rows are complete. The full `/etc/nix` inventory contained only `sentry-endpoint`. This is not a Sample-row proof. |

The R12 `lifecycle`, R10 `aarch64-container`, and R11 `x86_64-container` exit statuses of 1 are correct harness behavior. Each proof found a real strict-residue failure. R12 proves at least the sentry file remained. Both retained container inventories prove it was the only `/etc/nix` entry in those guests.

## 5. Lifecycle observations

These steps are in execution order. All commands used absolute paths.

1. `nix-installer --version` printed `nix-installer 3.22.1`. Exit status 0.
2. The harness recorded the identity of `/etc/nix/sentry-endpoint`. The file was **absent** before install.
3. `nix-installer --help` returned status 0.
4. Install ran with these arguments, in this order: `<staged binary> --diagnostic-endpoint http://127.0.0.1:18080 install --determinate --no-confirm --no-modify-profile`. Exit status 0.
5. The guest-local capture service counted 3 diagnostic requests during that install.
6. The install printed one warning. The vendor self-test ran a build probe through `sh -lc` and `bash -lc`. Both shells could not find `nix` on PATH. This is expected with `--no-modify-profile`. So the install was **not** warning-free.
7. `/nix/receipt.json` was a non-symlink regular file. It had uid 0, gid 0, mode `0600`, size 30,946 bytes, and link count 1. R12 recorded a private SHA-256 only. It did not copy, print, or archive receipt contents. `/nix/nix-installer` existed and was executable. Its SHA-256 matched the pinned vendor asset.
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

**Observed, fresh R12 harness.** The checked-in harness rejected a receipt symlink. It required a nonempty regular file. It recorded only no-follow type, numeric owner, mode, size, link count, path, and a private SHA-256. It recorded the installed helper hash separately. It did not copy, print, or archive receipt contents.

## 6. Sentry residue finding

Definition. The sentry endpoint file holds the address that the vendor uses for anonymous error reports.

### Fresh Linux x86_64 R12 observations

| Stage | Result |
|---|---|
| Before install | Absent |
| After install | Non-symlink regular file, uid 0, gid 0, `root:root`, mode `0600`, 95 bytes |
| After `determinate-nixd upgrade --version v3.22.1` | Same metadata; private bytes and digest value unchanged |
| After uninstall | Same metadata; private bytes and digest value unchanged |

R12 recorded private byte captures and private digest records at all three present stages. The byte captures were identical. The digest values were equal. The digest files also name their stage-specific private capture paths, so the complete digest-file lines are not byte-identical. The endpoint address, raw bytes, and digest value remain private.

R12 did **not** record sentry link count. The sentry stat format has no link-count field. This report makes no R12 sentry link-count claim. The retained R10 and R11 container evidence separately recorded link count 1.

Consequences:

- The file appeared during the install phase. This proof does not identify which internal vendor component wrote it.
- The daemon upgrade does not change it.
- Uninstall does not remove it.
- This recorded file is sufficient to fail the strict lifecycle residue contract.
- The R12 harness did not inventory every x86_64 `/etc/nix` entry, so it does not prove this was the only residue inside that directory.

### Linux aarch64 container observations

The private r10 evidence records this non-secret metadata:

| Stage | Type | Numeric owner | Mode | Size | Link count |
|---|---|---|---|---:|---|
| After install | Regular file, not a symlink | uid 0, gid 0 | `0600` | 95 bytes | 1 |
| After uninstall | Regular file, not a symlink | uid 0, gid 0 | `0600` | 95 bytes | 1 |

The full post-uninstall `/etc/nix` inventory contained exactly `/etc/nix/sentry-endpoint`. The private identity hashes before and after uninstall were equal. The digest value, endpoint address, and file bytes remain private. Metadata and the private identity hash are separate evidence.

### Final Linux x86_64 container observations

The private r11 evidence records the same sentry contract after install and after uninstall: non-symlink regular file, uid 0, gid 0, mode `0600`, size 95 bytes, and link count 1. The full post-uninstall inventory contained exactly `/etc/nix/sentry-endpoint`. The private hashes before and after uninstall were equal. The digest value, endpoint address, and file bytes remain private.

**Source-derived conclusion.** The pinned uninstaller reverses receipt actions. Its visible Determinate daemon revert removes `nix.conf`, then removes `/etc/nix` only when that directory is empty. The pinned public source contains no direct `sentry-endpoint` removal action. This explains why the visible revert path leaves a nonempty `/etc/nix`. It does not identify the private internal component that created the file.

**Inference.** The file is vendor-owned residue because it appeared during vendor execution and survived successful vendor uninstall. The evidence does not prove intentional retention or a vendor defect.

This report adds no product cleanup for this file. Section 12 routes the final residue policy to the later uninstall-policy PR.

## 7. Fresh R12 crash recovery result

Method:

1. Install started with diagnostics disabled.
2. The harness waited for observable progress. Progress meant both of these: `/usr/local/bin/determinate-nixd` existed, and `/nix/store` had at least one entry.
3. The harness sent SIGKILL to the installer.
4. The guest rebooted.
5. Install ran again with the same arguments.

Result: exit status 0. `/nix/receipt.json` was present and non-empty. `/nix/nix-installer` was present and executable.

Honest limit: this proves receipt and helper presence after recovery. It does not run a functional Nix command. A functional check belongs to a later PR.

## 8. Fresh R12 foreign Nix input result

Method: the harness created `/nix` with mode `0755` and wrote a sentinel file into it. Then the vendor install ran.

Result:

- Vendor exit status **0**. The vendor accepted the foreign `/nix` and installed into it.
- The sentinel file was still present after the install.
- The harness compared the sentinel hash before and after the install, inside the guest. The hash did not change.
- After the run, `/nix/store` had many entries, `/nix/receipt.json` existed, and `/nix/nix-installer` existed.

Meaning: the vendor does **not** refuse a foreign `/nix`. It installs into it. So `pkg` product preflight must own the refusal policy. The stack plan already requires this in DN-10.

Honest limits:

- The lane stored no independent before/after sentinel hashes in the evidence. The in-guest comparison passed, but its values cannot be rechecked from the bundle.
- The input was a plain non-empty `/nix`, not a complete foreign Nix install. No foreign daemon, users, or receipt existed.
- This lane does not prove full filesystem preservation and does not set final product policy.

## 9. Fresh R12 upstream Nix input result

Fresh R12 run:

1. Install ran as `<staged binary> --diagnostic-endpoint '' install --prefer-upstream-nix --no-confirm --no-modify-profile`. Exit status 0.
2. `/nix/var/nix/profiles/default/bin/nix --version` printed `nix (Nix) 2.35.2`. Exit status 0.
3. `/nix/receipt.json` was present and non-empty.
4. A second install ran as `<staged binary> --diagnostic-endpoint '' install --determinate --no-confirm --no-modify-profile`. Exit status **1**. The vendor refused to install Determinate over the upstream install.
5. The post-refusal snapshot still listed `/nix/receipt.json`. The lane did not read, copy, or archive its contents.

Meaning: the pinned vendor executable can create a real upstream Nix input. In this run, the later Determinate attempt returned status 1 and refused the different planner settings.

This report does not claim conversion of an upstream install into a Determinate install. That path was not proved.

Historical note from the earlier report: a prior upstream run returned status 1 during `provision_nix` after guest DNS failed. Its old evidence directory is unavailable. It is not R12 evidence and is not gate evidence.

Honest limits:

- The refusal check confirmed that a receipt still existed after the refused run. It did not compare before and after receipt hashes.
- The harness recorded the Determinate exit status. It did not require status 1 for the lane to pass. DN-10 must enforce the product refusal policy.

## 10. Fresh R12 diagnostics result and limit

Method: the guest-local capture service listened on `127.0.0.1:18081`. The install ran with an empty `--diagnostic-endpoint` value. The environment also set `DETSYS_IDS_TRANSPORT` to the capture address, so that any other report would go to the capture service.

Result: install exit status 0. The capture counter read **0**. Zero requests reached the guest-local capture endpoint.

The R12 lifecycle lane used the enabled loopback endpoint and recorded 3 requests during initial install and 0 during repeat install. This proves local endpoint behavior for the exact commands. Vendor source configures a Sentry-based diagnostics handler, and the installed lifecycle left the private sentry endpoint file. R12 did not capture outbound packets. It does not prove that a real Internet endpoint received data. External network transmission remains unproved.

Honest limits:

- This is not an outbound packet capture.
- It does not prove that all possible traffic was absent or that any external diagnostics service received data.
- It proves only that the disabled configuration made no request to the controlled transport endpoint.

The parent S6 findings record the vendor disable contract from vendor help text and vendor source. This lane supports that contract on Linux x86_64.

## 11. Summary of pinned vendor behaviors

### Receipt evidence status

**Observed, fresh Linux x86_64 R12.** The lifecycle receipt was a non-symlink regular file owned by uid 0 and gid 0, mode `0600`, size 30,946 bytes, and link count 1. R12 recorded a private SHA-256 before uninstall. It did not print, copy, or archive receipt contents. The installed helper matched the pinned asset.

**Observed, retained Linux x86_64 R11 container.** The R11 receipt was a non-symlink regular file owned by uid 0 and gid 0, mode `0600`, size 30,618 bytes, and link count 1. The probe recorded a private SHA-256 before uninstall. It did not print, copy, or archive receipt contents. The installed helper matched the pinned asset. R11 remains the narrow x86_64 container Asset proof.

**Observed, Linux aarch64.** The r10 receipt was a non-symlink regular file owned by uid 0 and gid 0, mode `0600`, size 30,503 bytes, and link count 1. The probe computed its SHA-256 before uninstall and kept the digest only in private evidence. It did not print, copy, or archive receipt contents. The installed helper was a non-symlink executable with the pinned asset digest. Therefore the aarch64 standalone receipt and installed-copy Asset proof is complete.

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
| Uninstall | `/nix/nix-installer ... uninstall --no-confirm /nix/receipt.json` | Status 0. At least `sentry-endpoint` remained in the broad x86_64 run. It was the only `/etc/nix` entry in both final container proofs. |
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
9. **Future DN-13 proposal:** after successful vendor uninstall, inspect only `/etc/nix/sentry-endpoint` without following links. Remove it only when every property is present and exact: non-symlink regular file, uid 0, gid 0, mode `0600`, link count 1, and the size and digest approved for the pinned vendor version and target. Any missing or mismatched property leaves the file in place and keeps the strict residue verdict FAIL. Never recursively delete `/etc/nix`. Remove `/etc/nix` only after the exact file checks passed and the directory is empty, not a symlink, owned by uid 0 and gid 0, and mode `0755`. The final aarch64 r10 and x86_64 r11 evidence records link count 1 and keeps each target digest private. DN-03 does not approve or apply this proposal.

Reasons:

- Fresh R12 proves the tested x86_64 install, repeat install, reboot, repair, same-version upgrade, uninstall, repeat-uninstall refusal, crash-recovery, foreign-input, upstream-input, receipt, and diagnostics behavior.
- Linux aarch64 and x86_64 completed the three Asset proof rows. R12 now records the broad x86_64 receipt hash. R11 remains the narrow x86_64 container Asset proof.
- A private path does not remove LGPL-2.1 obligations. DN-05 must keep the license notice and source inventory, and it must complete a distribution compliance review.

## 13. Stop-gate decision

### Linux x86_64 decision

Fresh R12 is the current reviewable broad x86_64 evidence. It is sufficient to describe the tested vendor install, repeat install, reboot and changed boot ID, repair, help probes, same-version daemon upgrade, crash recovery, diagnostics control, foreign input, upstream input, receipt identity, installed-copy identity, and uninstall behavior. R12 records the private receipt hash. Retained R11 separately proves Nix execution and the exact post-uninstall inventory in the emulated x86_64 container. Neither run proves a clean vendor uninstall. R11 does not prove a native x86_64 run or any Sample row in the container.

### Linux aarch64 Asset proof mapping

The roadmap uses `Asset proof` for three DN-03 matrix rows: standalone invocation and arguments, diagnostics control, and standalone receipt and installed-copy behavior.

The aarch64 r10 lane proves these facts:

- the pinned asset digest matched;
- the guest reported `aarch64`;
- the installer version and exact install arguments were recorded;
- install returned 0;
- an opaque nonempty receipt was present; its metadata and private SHA-256 were recorded before uninstall;
- the installed vendor copy matched the pinned asset digest;
- the installed Nix executable ran;
- the exact install and uninstall argv used a loopback diagnostics endpoint;
- the exact environment set `DETSYS_IDS_TELEMETRY=disabled`;
- the active loopback canary counted zero requests after install and zero total requests after uninstall;
- uninstall returned 0;
- the sentry file link count was 1 after install and after uninstall;
- the post-uninstall `/etc/nix` inventory contained only `sentry-endpoint`.
- the exact Docker argv included `--rm`, the pinned image digest, the exact container name, `--platform linux/arm64`, and `--network none`;
- the immediate exact-name post-cleanup list was empty and its count was zero;
- the cleanup record and complete private evidence bundle passed their checksum manifests.

The lane completes all three Linux aarch64 `Asset proof` rows. It does not run the aarch64 Sample rows: repeat install, SIGKILL and reboot, repair and update, or repeat uninstall. It does not prove systemd, reboot, repair, update, crash recovery, clean vendor uninstall, product cleanup, or the complete product lifecycle.

### Parent gate

**Observed.** The strict sentry residue is still a real `FAIL`. Release v3.22.1 has no standalone Intel macOS executable.

**Inference.** The tested Linux lanes support attribution of the sentry residue to vendor execution.

**Unproved.** Successful crash recovery and functional Nix recovery are unproved.

**Decision.** The required Linux behavior and both Linux target Asset proofs are accepted. R10 proves the Apple Silicon lifecycle and residue rows. Crash R1 completes the standalone SIGKILL and reboot observation with a negative result. The parent status is **COMPLETE — EVIDENCE COMPLETE; PRODUCT DELIVERY NO-GO.** DN-04 may document the proved contract. DN-13 later owns exact residue cleanup only. It does not own crash recovery. This report does not apply cleanup. Intel macOS remains unsupported.

**Inference.** This ordering is not a dependency cycle. DN-03 records vendor behavior and the ownership boundary. DN-13 implements and proves product-owned residue cleanup after integration support exists.

Do not call any Linux residue lane a clean-uninstall PASS. Do not claim clean vendor uninstall.

## 14. Evidence manifest

All evidence directories live outside the repository. Fresh broad x86_64 R12 evidence is under `/private/var/tmp/pkg-s6-dn03b-evidence/`. Retained aarch64 evidence is under `/private/var/tmp/pkg-s6-dn03b-aarch64-evidence/`. Nothing from either root is committed.

Common values for all five fresh R12 lanes:

- Product revision: `33b386dd473e66c7772c4392d7f56953e1398595`
- Vendor revision: `4132ad07a15ee7d88c096ac7172b7afb2672866b`
- Vendor asset SHA-256: `9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c`
- Guest image SHA-256: `6e40c07ae715f744f84af0bec76415cc1987dd115b4b8de437818561f01a3733`

| Directory | Lane | UTC run date | Guest exit | Results SHA-256 | Complete bundle SHA-256 |
|---|---|---|---:|---|---|
| `lifecycle-33b386d-r12` | lifecycle | 2026-08-23T18:37:26Z | 1 | `07f6702e19cfe24067a4718af8c4401345b5c76416b82fa4b0a414efa5ef845a` | `1b0128ba5f4a3e9c913c3778471734dcfe8031dd7539b965e2d1307ca4a6828a` |
| `diagnostics-disabled-33b386d-r12` | diagnostics-disabled | 2026-08-23T18:41:51Z | 0 | `a1d4db35bd2af8a9ad02b33bd90c16bef65a693062c711823e534eb8cddc661c` | `b3a5cebc7975be8e04f409592228443afa52fa4428f9be34d5c3bf1b27ad8af0` |
| `crash-recovery-33b386d-r12` | crash-recovery | 2026-08-23T18:45:28Z | 0 | `a427be0f40bf67de85aa202d6962e8feeadee1a26cfbd15c3a45c6491b519c24` | `c2ead304a217c214805dee43f1cadbdb857225d4ad31ab434e96d5be4385a682` |
| `foreign-nix-33b386d-r12` | foreign-nix | 2026-08-23T18:48:48Z | 0 | `b6fc7ae351738e8413e27fb2bd9cb80693f6870ae4029a6ffc4422d1d71aa165` | `c5deb44b9175110986ebb026ae7ce082b03f02fd043a1bca6234c3a16e8ab966` |
| `upstream-input-33b386d-r12` | upstream-input | 2026-08-23T18:52:54Z | 0 | `adc33230ee4e5001e3070e49591d87be5697a9fa6362ede258cfe08ac8a79208` | `a2c92c9b4bab26f0be88b83ba83eba1ae73bf7af3d4083b905e9d8cfdeed9d42` |

Each results hash fingerprints only the public `guest-evidence/results` summary. Each complete bundle hash is the SHA-256 of the sorted absolute-path per-file SHA-256 lines for every regular file in that lane directory. Review recomputed all five hashes. Every bundle file had mode `0600`. Every bundle directory had mode `0700`. No bundle contained a symlink, `receipt.json`, a copied receipt, or another receipt-content artifact. The receipt digest stayed private.

The older broad directories named `lifecycle-0d4809e`, `diagnostics-disabled-0d4809e`, `crash-recovery-0d4809e`, `foreign-nix-0d4809e`, `upstream-input-0d4809e`, and `upstream-input-0d4809e-r2` are unavailable. They are not gate evidence. Their old summary hashes are not current evidence. R12 replaces them. The retained ARM R10 and x86_64 container R11 evidence below remains available and keeps its narrower Asset-proof role.

The retained private aarch64 evidence is under `/private/var/tmp/pkg-s6-dn03b-aarch64-evidence/probe-16f0bbe-r10/`.

| Directory | Lane | UTC run date | Container exit | Results SHA-256 | Probe SHA-256 |
|---|---|---|---:|---|---|
| `probe-16f0bbe-r10` | aarch64-container | 2026-08-23T15:28:52Z | 1 | `67a7900f81fc39c48f0ec06af02f2b04469cdf0270652a8271d14e0397fb8344` | `e522ac56186e77a803ca39667e2cab3be972fe06deb2aa90940bce107eda4d9c` |

The aarch64 output records eleven PASS lines and one strict-residue FAIL line. The only `/etc/nix` entry was `sentry-endpoint`. It was a non-symlink regular file with uid 0, gid 0, mode `0600`, size 95 bytes, and link count 1 after install and uninstall. Receipt contents and sentry bytes were not printed, copied, or archived. Receipt metadata, a private receipt SHA-256, and private sentry identity hashes were recorded. The private digest values are not published.

The immediate cleanup check ran at `2026-08-23T15:28:53Z`. The exact-name match list was empty and the count was zero. The cleanup checksum manifest has SHA-256 `5aa2080210dd9e1a49be083d201262a2b3e61e6387465f5a1dc1e3eecfd968a3`. The complete private evidence checksum manifest has SHA-256 `2f889c53569cc7e724db86be19fdeae09d690b952c51aa068b876677659f444f`. Both manifests passed verification.

The authenticated official release manifest is stored in the prior private r9 directory. Its SHA-256 is `69f5e70f86310df06d4d3c83698e21e4e9803a7f7319fb31778b71c07f99f8a1`. The derived four-row asset list has SHA-256 `5293dfa5a1b6e660612f35bbf0de6456ecef55481453701792ee9ec2131b3b42`. The exact `nix-installer-x86_64-darwin` asset count is zero.

**Later cleanup check, not r9 acceptance evidence.** At `2026-08-23T15:21:40Z`, a host-side `docker ps -a` query compared names by exact equality with `pkg-s6-dn03b-arm64-probe-bff2cb4-r9`. It recorded an empty match list and exact count 0. The cleanup-record checksum manifest has SHA-256 `9ee5e24199a85c4eadce75c2bbe4765bbae482f2f619da5e250245593c123af9`. Its provenance states that this was a later live query. It does not prove immediate cleanup and does not upgrade r9 from Unproved.

The final private x86_64 container evidence is under `/private/var/tmp/pkg-s6-dn03b-evidence/probe-7ff31c5-r11/`. The failed preflight run remains separately under `/private/var/tmp/pkg-s6-dn03b-evidence/probe-4beeb2b-r10/`. Neither path was reused or overwritten.

| Directory | Lane | Results file UTC mtime | Container exit | Results SHA-256 | Probe SHA-256 |
|---|---|---|---:|---|---|
| `probe-7ff31c5-r11` | x86_64-container | 2026-08-23T15:22:24Z | 1 | `321868681c137e32a77be96a4c3b814c67031b456e9ea4f7173f554f8db43f06` | `0f0f2d412893c570cf11b89b1f0711282dd880a8a49501db807b5e3366ba546a` |

The r11 output records eleven PASS lines and one strict-residue FAIL line. Install, Nix execution, and uninstall returned zero. The live canary recorded zero requests after install and zero total requests after uninstall. The receipt and sentry metadata were safe. Private digests were present and well formed. Receipt contents and sentry bytes were not printed, copied, or archived. The installed helper matched the public pinned asset digest. The named residue scan outside `/etc/nix` was empty. The exact `/etc/nix` inventory contained only `sentry-endpoint`. The exact container was removed.

The r10 run stopped before receipt creation. It recorded zero canary requests and a safe exact-container cleanup. Its install failed when Nix tried to load a syscall filter under AMD64-on-ARM64 emulation. It is failure history, not gate evidence.

## 15. Primary sources

These sources are official and primary. No blog or secondary source is cited.

- [Exact release v3.22.1](https://github.com/DeterminateSystems/nix-installer/releases/tag/v3.22.1)
- [Official GitHub release API manifest for v3.22.1](https://api.github.com/repos/DeterminateSystems/nix-installer/releases/tags/v3.22.1)
- [Release asset used here](https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-x86_64-linux)
- [Linux aarch64 release asset used here](https://github.com/DeterminateSystems/nix-installer/releases/download/v3.22.1/nix-installer-aarch64-linux)
- [Pinned official Linux and container support documentation](https://github.com/DeterminateSystems/nix-installer/blob/4132ad07a15ee7d88c096ac7172b7afb2672866b/README.md#in-a-container)
- [Docker image pull by digest and platform](https://docs.docker.com/reference/cli/docker/image/pull/)
- [Docker registry manifest inspection](https://docs.docker.com/reference/cli/docker/buildx/imagetools/inspect/)
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

1. Linux x86_64 received the broad behavior run and one final container Asset proof. Linux aarch64 received one root-only container Asset proof. The macOS DN-03 evidence lane is separate.
2. One x86_64 VM image and one image index with x86_64 and aarch64 container children were tested.
3. One run per lane. Cutover PRs require two clean runs.
4. Crash recovery proves receipt and helper presence only. It does not run a functional Nix command.
5. Foreign input stored no independent before/after sentinel hashes in the evidence. The in-guest comparison passed, but it cannot be rechecked from the files.
6. Foreign input was a plain non-empty `/nix`, not a complete foreign Nix install.
7. Upstream refusal checked receipt presence only. It did not compare before and after receipt hashes.
8. The harness recorded the upstream Determinate exit status. It did not require refusal for the lane to pass.
9. Diagnostics-disabled proves only the controlled transport counter. It is not an outbound packet capture. It proves neither the absence of all traffic nor delivery to a real external service.
10. The lifecycle lane did not prove pkg Handoff, package lifecycle, product repair, or product uninstall. Those belong to later PRs.
11. The vendor executable ran as root inside a disposable guest. Behavior under a restricted or hardened host was not tested.
12. `determinate-nixd upgrade --version v3.22.1` was a same-version probe. A real N to N+1 upgrade was not run.
13. The container Asset proofs did not run systemd, reboot, repair, update, crash recovery, repeat install, or repeat uninstall.
14. Both Linux target Asset proofs recorded the roadmap-required private receipt hash. The x86_64 proof used architecture emulation and disabled the Nix syscall filter for that disposable unsandboxed container.
15. macOS R10 proves the lifecycle and residue rows. Crash R1 completes the standalone SIGKILL and reboot observation with a negative result. Successful and functional recovery remain unproved, and product delivery remains `NO-GO`. Intel macOS asset availability is observed, but no Intel lifecycle ran because v3.22.1 has no standalone Intel macOS executable.
16. The prior aarch64 r9 run did not preserve immediate cleanup evidence. The final r10 run replaces it for Asset-row acceptance and preserves the exact cleanup record.
17. The container entry and target-selection corrections were not rerun in R10 or R11. Static checks prove those guards. Fresh R12 did run the corrected QEMU receipt path. R12 adds no new container runtime fact.

Later proof work:

- DN-07: prove Handoff crash boundaries and receipt validation.
- DN-09: prove standard-daemon package parity, including functional Nix commands.
- DN-10: prove the foreign-input and upstream-input refusal policy in `pkg` preflight.
- DN-11: prove PATH behavior for login shells, non-login shells, and GUI launches.
- DN-13: decide and prove the final uninstall residue policy, with exact type, owner, mode, and hash checks.
- DN-15: prove the complete Linux lifecycle twice from clean snapshots, including a real version upgrade.
