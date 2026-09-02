# Proposal: Production trust ceremony

## Why

Everything shipped so far runs on proof-grade trust: a test TUF root (`1c5ceff8…` with three
throwaway online keys), an ephemeral Quick Tunnel URL that no longer exists, and an unsigned
macOS package. Real users require real trust: keys with ceremony and custody, a stable domain
(`kelv.dev`, now owned), and a Developer ID-signed, notarized macOS package.

This change is deliberately sequenced LAST. It executes only after the deterministic
verification suite, the CLI UX change, and the installation experience change have landed and
held green — putting production keys behind an unproven surface would be ceremony without
safety.

## What Changes

1. **Key ceremony**: generate a production TUF root key offline (air-gapped ceremony, recorded
   steps, two-person acknowledgment where possible) and a threshold set of online keys
   (3-of-5). Publish root v1. Define rotation and compromise procedures in an ops runbook.
2. **Channel hosting**: stand up `channel.kelv.dev` with TLS, immutable target storage, and
   CDN caching; point the product's channel constant at it; retire all proof-grade URLs.
3. **macOS signing**: Developer ID Application certificate; sign the `.pkg` with
   `productsign`; notarize with `notarytool`; staple the ticket; verify Gatekeeper pass on a
   clean Mac.
4. **Production release pair**: build, sign, seal, and publish the first production release
   under the new root; update `SHA256SUMS` + sigstore bundles accordingly.
5. **Re-proof**: one full slot lifecycle proof against the production channel and the signed
   package before any public announcement. Evidence archived like DN-16.
6. **Ops runbook**: key custody locations, signing procedures, rotation schedule, compromise
   response, channel rollback procedure.

## Non-goals

- No product behavior changes; trust infrastructure only.
- No rename execution (separate change, may land before or after; the ceremony is
  name-agnostic by pinning URLs in constants).
- No beta/stable channel split; one production channel.

## Impact

- `tools/release/`: signing workflow steps, key handling, notarization integration.
- Product channel constants move to `channel.kelv.dev`.
- New `docs/ops/` runbooks (not public).
- One new release tag on `main` after re-proof passes.
