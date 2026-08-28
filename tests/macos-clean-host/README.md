# macOS Apple Silicon lifecycle proof

This is a destructive proof for the DN-16 product lifecycle.
Do not run `prove.sh` on a developer Mac.

The workflow runs only by manual dispatch.
Its destructive jobs require two different self-hosted runner labels:

- `pkg-disposable-macos-proof-1`
- `pkg-disposable-macos-proof-2`

Each label must select one fresh Apple Silicon `VirtualMac`.
The proof is blocked until both disposable runners exist.
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

The runner provisioner must do these steps before it starts the Actions runner.

1. Create a fresh macOS Apple Silicon virtual machine.
2. Confirm that Nix and pkg are absent.
3. Record `sysctl -n kern.bootsessionuuid`.
4. Write this root-owned mode `0600` file:

   `/var/tmp/pkg-disposable-macos-reboot-v1`

   Its one line must have this form:

   `PKG-DN16-REBOOT-V1:<lifecycle-run>:<boot-session-before-reboot>`

5. Reboot the virtual machine.
6. Start the Actions runner after the reboot.
7. When a workflow job is assigned, write this root-owned mode `0600` file:

   `/var/tmp/pkg-disposable-macos-proof`

   Its one line must have this form:

   `PKG-DN16-DISPOSABLE-V1:<github-run-id>:<lifecycle-run>`

The harness compares the saved boot session with the current boot session.
Equal values fail the proof.
This proves that the fresh runner rebooted before the job.
It does not prove product lifecycle recovery across a reboot.
That row needs an external two-phase runner protocol.
An Actions step cannot resume itself after a reboot.

The runner must provide passwordless `sudo` only inside the disposable VM.
The VM must be destroyed after the job.

## Signed input contract

The workflow downloads release N into `candidate/from`.
It downloads release N+1 into `candidate/to`.
It verifies release checksums and Sigstore bundles before `prove.sh` starts.
Both release tags must resolve to reviewed DN-16 commit `8ffd325a4be12a998f3a5684097b57841a11540e`.
Both signed manifests must identify Determinate 3.22.1 and the pinned Apple Silicon installer digest.
The current public release N predates DN-16 and is not a valid baseline.
The proof stays blocked until two compatible signed releases exist.
The harness does not download or authenticate a second release source.
It rechecks the selected checksum files for both local candidate directories.

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
It labels runner, input, compiled, and native evidence separately.
It keeps command output at 64 KiB per row.
It records hashes and fixed state only.
It does not record the environment or secret file contents.

The workflow uploads evidence on success and failure.
It retains evidence for three days.
It does not publish a package or release.
