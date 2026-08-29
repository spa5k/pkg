# macOS Apple Silicon lifecycle proof

This workflow is the destructive DN-16 lifecycle proof.
The manual GitHub workflow is the only supported entry point.
It uses one dispatch.
It does not publish either private draft release.

## Fixed execution order

The workflow uses these explicit job dependencies:

1. Validate the immutable dispatch.
2. Build the proof-only harness.
3. Acquire and authenticate the sealed proof inputs.
4. Prepare slot 1.
5. Resume slot 1 after an operator reboot.
6. Prepare slot 2.
7. Resume slot 2 after an operator reboot.
8. Aggregate all four phase artifacts.

There is no matrix-order assumption.
Slot 2 cannot start before slot 1 resumes successfully.
Each prepare job and its resume job use the same runner label and runner name.
The two slots use different labels, names, VMs, and instance nonces.

The labels and runner names are:

- Slot 1: `pkg-disposable-macos-proof-1` and `pkg-dn16-proof-runner-1`.
- Slot 2: `pkg-disposable-macos-proof-2` and `pkg-dn16-proof-runner-2`.

Each runner must be an ephemeral self-hosted runner.
Each VM must be an Apple Silicon `VirtualMac`.
Use this immutable Tart image:

`ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c`

The VM root filesystem needs at least 75,161,927,680 free bytes (70 GiB).
Use a virtual disk of at least 100 GiB so the base image and runner files leave enough free space.
Confirm this free-space value before you register the runner.
The runner must provide passwordless `sudo`.
Do not use a production machine.

## Immutable dispatch and input trust

The dispatch must run from the verified signed
`dn16-macos-proof-workflow-13` annotated tag.
The supplied commit SHA must equal the tag target, checkout SHA, and workflow SHA.
The protected `release` environment gates this check.
The workflow pins the exact SHA-256 of `proof-pair.json`.

A hosted macOS job acquires the proof inputs once from the sealed channel.
It does not use the GitHub release API or download from a draft release.
Each channel inventory contains a `proof-inputs/` copy of the nine release files used by the proof.
It authenticates each `SHA256SUMS` with its Sigstore bundle.
It authenticates both preview packages and both Apple Silicon CLIs.
Each authenticated `SHA256SUMS` binds the selected package, CLI, and release manifest.
The pinned pair binds both releases to reviewed DN-16 product commit
`337ba704bc2d01d006b671be7fbdd25583ddfc89`.

The channel download rejects redirects.
It uses HTTPS only.
It rejects credentials, query text, fragments, traversal, unsafe paths, missing entries, and extra entries.
It downloads every inventory-listed metadata and target file before a destructive job starts.
It verifies every listed length and SHA-256.
It uploads only a bounded receipt for the 68 verified files.
It does not upload the full channel tree to GitHub artifact storage.
Each VM phase then makes 21 direct logical fetches for the fixed pair, two inventories,
and the exact 18 proof-input files.
Before this fetch, it verifies the provisioned `/usr/local/bin/cosign` version and SHA-256.
The workflow pins the final exact byte total for the 18 proof-input files.
It also pins the final exact byte total for all 21 response bodies.
Each VM rejects redirects, partial files, symlinks, missing files, extra files,
wrong lengths, wrong digests, wrong releases, and wrong Sigstore identities.
The successor sealed pair contains the original 50 files and 18 sealed proof inputs.
The two channel manifests must match the authenticated proof-input manifests.
The Apple Silicon CLI and Sigstore bundle must match the channel manifest.
The two channels must use the same trusted root.

The authenticated N package must embed the selected `/n/` routes.
The authenticated N+1 package must embed the selected `/n-plus-1/` routes.
The proof checks both routes before product mutation.

## VM identity files

The provisioner creates a different 64-character lowercase hexadecimal nonce for each VM.
It writes this root-owned mode `0600` file:

`/var/tmp/pkg-disposable-macos-instance`

The exact content is:

`PKG-DN16-INSTANCE-V1:<instance-nonce>`

Before the first prepare job, record the current boot UUID.
Write this root-owned mode `0600` file:

`/var/tmp/pkg-disposable-macos-reboot-v2`

The exact newline-terminated content is:

`PKG-DN16-REBOOT-V2:<github-run-id>:<slot>:<runner-name>:<instance-nonce>:<boot-uuid-before-reboot>:<unix-time>`

Reboot the fresh VM.
Register the ephemeral runner after the reboot.

Before each prepare or resume job is assigned, write this root-owned mode `0600` file:

`/var/tmp/pkg-disposable-macos-proof`

The exact content is:

`PKG-DN16-DISPOSABLE-V1:<github-run-id>:<slot>`

This job marker must be no older than five minutes.
The initial reboot marker and instance marker must also be fresh for prepare.
The resume phase uses the persisted instance nonce.
It does not require a new instance nonce.
The early host gate records the exact marker digests and current boot UUID.
The same runner-owned record binds the run, slot, phase, runner name, and VM nonce.
The later proof rechecks those bytes and that boot UUID after the input download.
It does not apply the original five-minute age limit a second time.

## One slot, prepare phase

Create only the VM for the current slot.
Do not create slot 2 while slot 1 is active.

The prepare job does these operations:

1. It proves the fresh VM state and initial reboot.
2. It revalidates the compact authenticated pair, two inventories, and 18 proof-input files.
3. It runs the compiled handoff and ordering tests.
4. It installs release N with native Package Installer.
5. It verifies the N CLI, ownership receipt, accepted Determinate handoff, and product services.
6. It proves the exact repeat-install no-op.
7. It proves offline Product Asset Repair.
8. It installs representative `hello` and `ripgrep` package state under release N.
9. It stops and disables both product services.
10. It snapshots the exact accepted Handoff state.
11. It snapshots selected exact Base Nix state.
12. It snapshots the complete user package-state tree.
13. It snapshots the explicit offline state of both product services.
14. It runs the authenticated N+1 package installer while services are offline.
15. It verifies the N+1 CLI and ownership receipt.
16. It verifies that both product services stayed offline.
17. It byte-compares every saved state snapshot after the N+1 transition.
18. It writes the protected continuation record.
19. It uploads bounded prepare evidence.

The accepted N+1 installer message must state that product services remain offline.
The proof does not claim that this installer changes Base Nix.
It proves that the selected Base Nix state did not change.

The fresh native install uses Package Installer `-dumplog` output.
The existing evidence capture keeps only its final 65,536 bytes.
If this install fails, the proof also records one file with these exact fields:

```text
installer_status=<integer>
handoff_state=absent|started|accepted|invalid
journal_present=true|false
```

This summary is at most 1 KiB.
It validates the protected Handoff and journal file boundaries before it reads them.
It does not copy either protected file into evidence.

The continuation record is:

`/private/var/db/pkg-dn16-proof-continuation-v1`

It is a regular root-owned `root:wheel` file with mode `0600`.
It is outside `RUNNER_TEMP`.
It binds the run ID, run attempt, slot, runner name, VM nonce, prepare boot UUID, workflow SHA,
release tags, proof-pair SHA, N+1 CLI SHA, ownership digest, and all snapshot digests.

The snapshot directory is:

`/private/var/db/pkg-dn16-proof-continuation-state-v1`

It is root-owned with mode `0700`.
Each exact snapshot is root-owned with mode `0600`.

## Operator reboot and resume

Wait for the prepare job and its evidence upload to finish.
Wait for the ephemeral prepare runner to deregister.
Do not destroy or replace the VM.

Reboot the same VM.
Do not change its instance nonce.
Write a fresh disposable job marker for the same run and slot.
Create a new registration token.
Register the same runner name and label with `--ephemeral`.

The resume job does these operations:

1. It verifies the root-owned continuation record and every bound snapshot.
2. It verifies the same runner name and VM nonce.
3. It verifies that the boot UUID changed after prepare.
4. It verifies the exact N+1 CLI, ownership receipt, and accepted handoff.
5. It explicitly verifies that both product services are still offline.
6. It byte-compares the Handoff, Base Nix, package, and service snapshots again.
7. It starts both product services.
8. It runs update, upgrade, rollback, repair, and garbage collection.
9. It removes `ripgrep` through the native `pkg remove` command.
10. It verifies that remove activated a new generation.
11. It verifies that `hello` remains and `ripgrep` is absent.
12. It verifies that the prior generation record remains.
13. It proves structured uninstall refusal without mutation.
14. It performs terminal uninstall.
15. It verifies final product and Base Nix absence.
16. It removes the proof continuation files.
17. It uploads bounded resume evidence.

The proof permits the persistent, zero-byte coordination lock at
`/private/var/db/pkg-install-handoff.lock`. It requires a regular non-symlink
file with owner `root`, group `wheel`, mode `0600`, size zero, and one link.
The lock is not lifecycle state. The safe DB parent avoids native
`/private/var/run`, which is group-writable on macOS.

After resume finishes, wait for its artifact upload.
Wait for the ephemeral runner to deregister.
Remove a stale offline registration if one remains.
Confirm that the exact registration is absent.
Then destroy the VM.

Only then create slot 2.
Repeat the complete prepare, reboot, and resume protocol with a new VM nonce.
Use the same GitHub workflow run and run ID.
Do not dispatch the workflow again.

## Aggregate result

The hosted aggregate job requires exactly four evidence artifacts:

- Slot 1 prepare
- Slot 1 resume
- Slot 2 prepare
- Slot 2 resume

It requires every job result to be successful.
It requires prepare and resume to use the same runner and VM nonce within a slot.
It requires a changed boot UUID within each slot.
It requires different VM nonces between slots.
It requires the staged N-to-N+1 row and the continuation-reboot row.
A partial, skipped, or synthetic row cannot pass.

Evidence is retained for three days.
The destructive hosts receive no source checkout.
They do not run Cargo.
They do not receive repository secrets.
The workflow does not create, publish, or modify a release.
