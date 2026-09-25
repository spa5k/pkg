# Production release preparation

Status: tooling prepared; production trust is not active.
Alpha.51 still uses the alpha test root and ad-hoc macOS signatures.

## Operator inputs

| Input | Required evidence | Current state |
| --- | --- | --- |
| Production TUF root | Offline generation record, public root digest, custody and recovery owners | Not supplied |
| Online signing provider | KMS/HSM key identities and access policy for each role | Not supplied |
| Production HTTPS channel | Domain control, immutable object storage, deployment access | Not supplied |
| Apple Developer ID Application | Certificate and private key in the signing keychain | No valid local identity |
| Apple Developer ID Installer | Separate installer certificate for the same Apple team | Not supplied |
| Notarization access | Stored `notarytool` keychain profile | Not supplied |

Do not put private keys, exported certificates, or passwords in the repository.
The existing release library accepts provider-backed `KeySource` implementations.
Select and review the actual provider before wiring production authentication.
Do not promote the alpha test root to production.

## Apple signing

[Apple distinguishes Application and Installer certificates](https://developer.apple.com/help/account/certificates/create-developer-id-certificates).
Command binaries need Developer ID Application signatures. The installer package
needs a Developer ID Installer signature. Both must belong to the selected team.

Use [packaging/macos/notarize.py](../../packaging/macos/notarize.py) with the exact
built `pkg`, `pkg-install`, `pkg-nix-broker`, and `pkg-root-helper` executables.
Use `--help` for its required named inputs. `--check-only` checks input presence
and naming; it does not claim key access or successful notarization.

The tool copies inputs into a private staging directory. It signs all command
binaries with hardened runtime and timestamps. It signs the package with the
Installer identity, submits it, requires an Accepted result, staples the ticket,
and requires a passing Gatekeeper assessment. It also submits an archive of the
standalone commands. No release output directory is created on a failure.
It retains public submission IDs and final hashes in `notarization.json`.

Follow [Apple's notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).
A successful local Gatekeeper check does not replace a clean-Mac test of the
actual downloaded package and every installed command. This remains a required
production gate. Build production metadata from the final signed bytes.

The production package has a different filename from the alpha preview. Update
the reviewed installer renderer and release asset inventory together when
production signing is activated. Do not replace a published alpha asset.

## TUF keys and channel

1. Record the required signing thresholds and independent custody owners.
2. Generate the root keys offline. Export only public metadata.
3. Configure the selected KMS/HSM online signing identities with least privilege.
4. Test threshold refusal and key rotation against a staging channel.
5. Test expired metadata refusal and timestamp refresh before expiry.
6. Verify the published root and every target from a separate test host.
7. Build and prove the production release with its compiled production root.

A channel rollback publishes a higher sequence with the previous approved
content. Never replay older signed metadata. Retain the exact publication
manifest, public audit record, artifact hashes, and proof results.

For compromise: stop publication, disable the affected online identity,
rotate with the offline root process, publish new metadata, and verify refusal
of the revoked signer. Record the operator and result. Do not remove user Nix
receipts or package generations as part of channel recovery.

## Release gates

Use the [exact-release checks](../../tests/release-lifecycle/README.md).
Prepare the old release before the live channel changes. Upgrade after the new
channel is published. Resume after a real reboot of the same VM. Keep the
existing interruption/recovery proof as a separate required gate.

Do not announce production readiness until key custody, public channel checks,
Apple notarization, clean-Mac Gatekeeper, and the native lifecycle proof all pass.
