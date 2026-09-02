# macOS Apple Silicon repeat-run loopback proof

This harness runs the destructive DN-1 repeat-run proof.
The manual GitHub workflow `.github/workflows/proof-repeat.yml` is the only
supported entry point. It uses one dispatch. It publishes nothing.

The proof repeats the DN-16 lifecycle on a sealed pair whose channel URLs
are baked at compile time to `https://127.0.0.1:8443`. The proof therefore
does not depend on any live external channel. It proves the proof machine
itself can run again.

## Before you can dispatch

1. Mint the loopback pair with
   `tools/release/mint_dn1_proof_pair.sh`. Sign both draft releases
   through `publish-release.yml` from the `dn16-proof-workflow-1` tag
   checkout. Seal the pair and upload it to the `dn1-proof-pair-1` tag.
2. Replace every pin marked `PENDING-DN1-MINT` in
   `.github/workflows/proof-repeat.yml` with the sealed values.
3. Cut the signed annotated tag `dn1-proof-workflow-1` on that exact
   commit. The dispatch must run from that tag.
4. Register the two disposable Apple Silicon runners again. The
   post-proof cleanup removed them. Each runner must be an ephemeral
   self-hosted runner inside a disposable Tart VM of

   `ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c`

   The labels and runner names stay:

   - Slot 1: `pkg-disposable-macos-proof-1` and `pkg-dn16-proof-runner-1`.
   - Slot 2: `pkg-disposable-macos-proof-2` and `pkg-dn16-proof-runner-2`.

   The VM root filesystem needs at least 75,161,927,680 free bytes
   (70 GiB). Use a virtual disk of at least 100 GiB. The runner must
   provide passwordless `sudo`. Do not use a production machine.

## Fixed execution order

1. Validate the immutable repeat dispatch.
2. Build the proof-only harness.
3. Acquire and authenticate the sealed pair from `dn1-proof-pair-1`.
4. Prepare slot 1 through offline N+1.
5. Resume slot 1 after an operator reboot.
6. Prepare slot 2.
7. Resume slot 2 after an operator reboot.
8. Aggregate the digest-level evidence of all four phases.

There is no matrix-order assumption. Slot 2 cannot start before slot 1
resumes successfully.

## The loopback channel

Each phase downloads the pair bundle from the tag through the GitHub API.
It verifies the tarball digest, the pair digest, and both sealed
inventories against the pins in the workflow. Then the harness tool
`serve_pair_loopback.py` moves the verified tree to `channel/` with one
rename, generates a disposable CA and server certificate, and serves
`n/` and `n-plus-1/` on `127.0.0.1:8443` over TLS only. The workflow
installs the CA into the VM System keychain with
`security add-trusted-cert -d -r trustRoot` before the product runs.

Teardown always runs. It stops the server, removes the served tree, and
reverses the trust with `security remove-trusted-cert -d`. A failed
cleanup fails the phase. There is no plaintext fallback. The product
enforces HTTPS channel URLs by policy.

## Operator pause

After each prepare job finishes, the phase evidence ends with
`status=awaiting-reboot`. You must then:

1. Do not destroy or replace the VM.
2. Reboot the same VM.
3. Do not change its instance nonce.
4. Register the same runner name and label before the resume job starts.
5. Only then create slot 2. Do it after slot 1 has resumed.
6. Do not dispatch the workflow again.

## Evidence

The aggregate compares only digest-level keys against the workflow pins:
the pair tarball digest, the pair digest, both inventory digests and
lengths, both channel totals, both canonical rows digests, the trusted
root digest, and the product commit. The four phase artifacts must each
report a passed result, keep one runner identity and one VM nonce within
the slot, and change the boot UUID across the reboot. The aggregate
ignores runner-embedded dates and identifiers.
