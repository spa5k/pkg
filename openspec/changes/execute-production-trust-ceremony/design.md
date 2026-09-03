# Design: Production trust ceremony

## Context

DN-16 proved the full lifecycle on proof-grade trust: test TUF root, ephemeral tunnel,
unsigned package. This change replaces each pillar with production-grade equivalents. It is an
operations change with code touch-points limited to constants, release tooling, and docs.

## Decisions

### D1. Order: keys → hosting → signing → release → re-proof

The domain already exists (`kelv.dev`). Keys come first because root v1 must exist before any
production metadata is signed; hosting second because the channel must serve the new root
before clients can pin it; signing third because notarized packages reference the final
distribution story; the release and re-proof close the loop. Each stage has a go/no-go the
runbook records.

### D2. Root custody: encrypted offline medium, 2-of-2 physical split

The root private key lives on an encrypted USB medium stored offline; the passphrase is split
between two custodians (Kamran + one trusted holder) using Shamir slices. Recovery requires
both. This matches single-maintainer reality while avoiding a single point of failure.

### D3. Online keys: 3-of-5, all in a password manager + one hardware token

Five online keys: two in the primary vault, two in a backup vault, one on a hardware token.
Signing uses the release workflow's existing isolated signer job (the one the TUF-transaction
CI job already exercises) with keys injected as secrets; threshold prevents single-secret
compromise from forging metadata.

### D4. Hosting: static object storage behind a CDN, no compute

`channel.kelv.dev` is objects + TLS + cache rules, no server process. The proof-server
(`serve_proof_channel.py`) pattern retires entirely for production. Publish = upload new
versioned files + update timestamp; immutability enforced by bucket policy, rollback by
re-publishing prior versioned metadata.

### D5. Notarization in CI, credentials in vault

`productsign` + `notarytool` run in the release workflow with Developer ID credentials from a
secret store (App Store Connect API key). Stapling is mandatory before the artifact enters the
channel. A failed notarization fails the release; there is no unsigned escape hatch.

### D6. Re-proof is the DN-16 workflow, retargeted

The repeat-run proof (from the deterministic-verification change) is pointed at production
inputs — same matrix, same evidence standard, one slot. No new harness code.

## Risks / Trade-offs

- **Ceremony discipline vs single maintainer**: two-person ideals degrade to one person plus
  recorded steps. Mitigated by physical split custody (D2) and the recorded runbook.
- **CDN cache staleness vs TUF timestamp freshness**: timestamp expiry windows must exceed CDN
  max-age. Set timestamp validity to 7 days, CDN TTL to 1 hour.
- **Key loss**: root loss is survivable via pre-published root rotation path (root v2 signed
  by v1 held in escrow). Runbook documents the escrow location.

## Open Questions

- Registrar/DNSSEC for kelv.dev: enable DNSSEC now or after hosting lands? Proposal: now, it
  is one toggle and removes a later migration.
