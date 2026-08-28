# macOS Apple Silicon lifecycle proof

This is a destructive proof for the DN-16 product lifecycle.
The manual GitHub workflow is the only supported entry point.

The workflow runs only by manual dispatch.
Its destructive matrix uses two self-hosted runner labels in this order:

- `pkg-disposable-macos-proof-1`
- `pkg-disposable-macos-proof-2`

Each label must select one fresh Apple Silicon `VirtualMac`.
The matrix permits only one destructive job at a time.
Both lifecycle slots use the same GitHub workflow run and GitHub run ID.
Only the runner for the current slot must exist.
GitHub-hosted runners are not permitted.

The trusted provisioner must map the labels to these exact runner names:

- `pkg-disposable-macos-proof-1` maps to `pkg-dn16-proof-runner-1`.
- `pkg-disposable-macos-proof-2` maps to `pkg-dn16-proof-runner-2`.

The two names must identify different virtual machines.
Each machine must have a different 64-character lowercase hexadecimal instance nonce.
The provisioner writes the nonce to `/var/tmp/pkg-disposable-macos-instance`.
The file must be owned by `root:wheel` and have mode `0600`.
Its one line must have the form `PKG-DN16-INSTANCE-V1:<nonce>`.

## External runner contract

The runner provisioner must do these steps before it starts each Actions runner.

The trusted scheduler must receive the GitHub run ID and lifecycle slot out of band.
It must select the exact runner name before it creates the VM markers.
GitHub Actions does not create or update these root-owned markers.

Start with slot 1 only.
Do not register slot 2 before slot 1 completes.
Register each Actions runner with `--ephemeral`.

1. Create a fresh macOS Apple Silicon virtual machine.
2. Confirm that Nix and pkg are absent.
3. Record `sysctl -n kern.bootsessionuuid`.
4. Write a root-owned mode `0600` reboot marker.

   `/var/tmp/pkg-disposable-macos-reboot-v2`

   It must have one newline-terminated line with this form:

   `PKG-DN16-REBOOT-V2:<github-run-id>:<lifecycle-run>:<runner-name>:<instance-nonce>:<boot-session-before-reboot>:<unix-time>`

5. Reboot the virtual machine.
6. Start the Actions runner after the reboot.
7. When a workflow job is assigned, write this root-owned mode `0600` file:

   `/var/tmp/pkg-disposable-macos-proof`

   Its one line must have this form:

   `PKG-DN16-DISPOSABLE-V1:<github-run-id>:<lifecycle-run>`

The harness compares the saved boot session with the current boot session.
Equal values fail the proof.
The run ID, lifecycle slot, runner name, instance nonce, and marker age must also match.
The instance nonce must match `/var/tmp/pkg-disposable-macos-instance` and the bounded preflight evidence.
The marker and its timestamp must not be older than five minutes.
This proves that the fresh runner rebooted before the job.
It does not prove product lifecycle recovery across a reboot.
That row needs an external two-phase runner protocol.
An Actions step cannot resume itself after a reboot.

The runner must provide passwordless `sudo` only inside the disposable VM.

Wait until the slot 1 matrix job has a terminal result.
Confirm that all required slot 1 artifact uploads for that result completed.
Confirm that the ephemeral runner registration is absent.
Only then stop and destroy the slot 1 VM.

Create and register slot 2 after the slot 1 cleanup gate passes.
Use the same GitHub workflow run and GitHub run ID.
Do not enable or dispatch the workflow again.
Use the slot 2 runner name, label, lifecycle slot, and a new instance nonce.
Repeat the VM, marker, reboot, and runner steps above.

Wait until the slot 2 matrix job has a terminal result.
Confirm that all required slot 2 artifact uploads for that result completed.
Confirm that the ephemeral runner registration is absent.
Only then stop and destroy the slot 2 VM.

The hosted aggregate job starts after both matrix jobs are terminal.
It downloads both bounded evidence artifacts.
It fails if either exact runner name or instance nonce is not distinct.

## Signed input contract

The workflow downloads two release candidates into `candidate/from` and `candidate/to`.
The required workflow contract authenticates each `SHA256SUMS`, preview package, and CLI with Sigstore before the harness starts.
The proof must not run if any of these Sigstore checks is absent.
Both release tags must resolve to reviewed DN-16 commit `8ffd325a4be12a998f3a5684097b57841a11540e`.
The two authenticated preview package digests and release IDs must differ.
The release manifests must identify Determinate 3.22.1 and the pinned Apple Silicon installer digest.
Each authenticated `SHA256SUMS` binds its release manifest and package digests.
The manifest checks do not replace the Sigstore identity of each package.
The current public release N predates DN-16 and is not a valid baseline.
The proof stays blocked until two compatible signed releases exist.
The harness does not download or authenticate a second release source.
It rechecks the selected checksum files for both local candidate directories.

Both public packages resolve the same live product channel.
After N+1 is published, running package N can install the N+1 product bundle.
Package names and package digests do not prove a native N to N+1 transition.
That row needs two pinned staged channel states.
The current harness records this row as externally blocked.

The proof-only harness artifact contains exactly these files:

- `README.md`
- `pkg-installer-tests`
- `prove.sh`
- `INVENTORY`
- `SHA256SUMS`

The destructive host receives no source code.
It does not run Cargo.
It does not receive repository secrets.

The release does not publish a separate macOS `pkg-install` file.
The harness uses native `pkgutil --expand-full` to read `pkg-install` from the already authenticated preview package.
It verifies the embedded code signature before use.

## Proof result

The harness writes a small TSV result matrix.
It labels runner, input, compiled, native, and external evidence separately.
It keeps command output at 64 KiB per row.
It records hashes and fixed state only.
It does not record the environment or secret file contents.

The workflow uploads evidence on success and failure.
It retains evidence for three days.
It does not publish a package or release.
It stays failed while staged-channel upgrade and lifecycle reboot recovery are blocked.
