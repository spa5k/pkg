# macOS alpha clean-host proof

This proof runs through the manual `macOS alpha clean-host proof` workflow or an explicitly gated disposable local Tart VM.

The build job creates an ad-hoc-signed technical-preview package.
It also creates two ephemeral signed product publications.

The proof job starts on a separate fresh GitHub-hosted macOS runner.
It receives one tar artifact.
It does not receive source code.
It does not compile `pkg-install`.

The proof uses the shipping package, the shipping `pkg-install` artifact, and the public `pkg` CLI.
It performs real APFS, Keychain, Directory Services, launchd, and `/nix` mutations.
Do not run `prove.sh` on a developer Mac.

For a local Tart proof, copy only the staged proof bundle into a fresh macOS arm64 VM.
Inside that VM, create and remove the disposable marker around the proof:

```sh
sudo install -m 0600 /dev/null /var/tmp/pkg-disposable-macos-proof
trap 'sudo rm -f /var/tmp/pkg-disposable-macos-proof' EXIT
PKG_DISPOSABLE_MACOS_PROOF=local-tart ./prove.sh
```

The local gate requires an arm64 `VirtualMac*` model, kernel hypervisor presence, and a root-owned marker that is not older than five minutes.

macOS cannot remove a live synthetic root object before reboot.
The proof therefore permits one empty and unmounted `/nix` virtual directory after uninstall.
It requires `synthetic.conf`, the APFS volume, the Keychain item, and all product state to be absent.
The virtual directory disappears at the runner's next reboot.

The proof also permits the persistent, zero-byte coordination lock at
`/private/var/db/pkg-install-handoff.lock`. It requires a regular non-symlink
file with owner `root`, group `wheel`, mode `0600`, size zero, and one link.
The lock is not lifecycle state. The safe DB parent avoids native
`/private/var/run`, which is group-writable on macOS.

The workflow is a technical-preview gate.
It does not publish a release.
It does not use Developer ID signing.
It does not notarize the package.
It does not claim Gatekeeper-clean or stable behavior.

Developer ID signing and notarization remain explicit TODO items.

## Deferred hosted rerun

GitHub Actions was disabled at the repository level on 2026-08-15.
Do not count local tests as macOS clean-host proof.

Hosted run `31873974181` used macOS 15.7.7 arm64 image `20260727.0256.1`.
It used artifact digest `12e854d8d05e5e050d8e6e2aedc57726f93e6b39ab43b529b585ff6abf81948c`.
The shipping package hash was `da08f483100b03bf09cea679186a5f26cada7dc5f9e7df075d5a922354b65d34`.
The shipping `pkg-install` hash was `b9dee3f83e9e87b4a74bffea5ddff51b1f3bbe51f4072bbc7b74383b3ee238ed`.
The host had no detected Nix state.
Authentication succeeded, but account preflight failed before the first journal entry.
Commit `05d4509` fixes the signed Directory Services ID parser that caused this failure.

After Kamran explicitly re-enables GitHub Actions, run this command once:

```sh
gh workflow run nightly.yml --ref agent/macos-alpha -f macos_alpha_proof=true
```

Save the run URL, runner image, artifact hashes, command log, assertion count, and retained-state report.
Require every assertion in `prove.sh` to pass before claiming macOS clean-host proof.
