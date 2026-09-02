# Tasks: Production trust ceremony

> Blocked by: `add-deterministic-verification-suite`, `improve-cli-ux`, `improve-install-ux`
> all landed and green for two consecutive weeks.

## 1. Key ceremony

- [ ] 1.1 Acquire two encrypted USB media and a hardware token; record custody plan
- [ ] 1.2 Air-gapped ceremony: generate root key, record steps/participants/fingerprint in the
      ceremony record; Shamir-split the passphrase between two custodians
- [ ] 1.3 Generate 5 online keys; distribute per design D3; add threshold metadata (3-of-5)
- [ ] 1.4 Sign and publish root v1 to a staging location; verify client threshold enforcement
      with the existing TUF tests
- [ ] 1.5 Pre-sign root v2 (rotation escrow) and store per runbook

## 2. Channel hosting

- [ ] 2.1 DNSSEC on kelv.dev; create `channel.kelv.dev` pointing at object storage
- [ ] 2.2 TLS cert, bucket immutability policy, CDN with 1h TTL; 7-day timestamp validity
- [ ] 2.3 Publish root v1 + initial metadata; smoke-verify with the product client against
      production for the first time
- [ ] 2.4 Update product channel constant in a feature flag (`--channel` default flips to
      production); retire every proof-grade URL from shipped artifacts

## 3. Apple Developer ID

- [ ] 3.1 Enroll Apple Developer Program (long pole — start immediately)
- [ ] 3.2 Create Developer ID Application cert; store in vault
- [ ] 3.3 Release workflow: `productsign`, `notarytool` submit/wait/staple; fail release on
      notarization failure
- [ ] 3.4 Clean-Mac Gatekeeper verification; `spctl -a -vv` transcript archived as evidence

## 4. Production release

- [ ] 4.1 Build release pair under production root; SHA256SUMS + sigstore bundles
- [ ] 4.2 Publish to channel; verify remote from a network-clean VM
- [ ] 4.3 Add install/uninstall scripts as channel targets; wire `kelv.dev/install.sh`
      redirect

## 5. Re-proof and go-live

- [ ] 5.1 Dispatch repeat-run proof against production channel + notarized package
- [ ] 5.2 Archive evidence to DN-16 standard (run ID, transcripts, verdicts)
- [ ] 5.3 Runbook review; go/no-go recorded; tag the release on `main`

## 6. Ops runbook

- [ ] 6.1 `docs/ops/trust-runbook.md`: custody, signing steps, rotation schedule, compromise
      response, channel rollback
- [ ] 6.2 Calendar: annual root review, quarterly online-key rotation drill
