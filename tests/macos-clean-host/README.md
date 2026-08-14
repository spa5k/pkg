# macOS alpha clean-host proof

This proof runs only through the manual `macOS alpha clean-host proof` workflow.

The build job creates an ad-hoc-signed technical-preview package.
It also creates two ephemeral signed product publications.

The proof job starts on a separate fresh GitHub-hosted macOS runner.
It receives one tar artifact.
It does not receive source code.
It does not compile `pkg-install`.

The proof uses the shipping package, the shipping `pkg-install` artifact, and the public `pkg` CLI.
It performs real APFS, Keychain, Directory Services, launchd, and `/nix` mutations.
Do not run `prove.sh` on a developer Mac.

macOS cannot remove a live synthetic root object before reboot.
The proof therefore permits one empty and unmounted `/nix` virtual directory after uninstall.
It requires `synthetic.conf`, the APFS volume, the Keychain item, and all product state to be absent.
The virtual directory disappears at the runner's next reboot.

The workflow is a technical-preview gate.
It does not publish a release.
It does not use Developer ID signing.
It does not notarize the package.
It does not claim Gatekeeper-clean or stable behavior.

Developer ID signing and notarization remain explicit TODO items.
