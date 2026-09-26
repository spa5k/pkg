# Design: Production trust activation

## Context

The [production runbook](../../../docs/ops/production-release.md) and
[active plan](../../../plans/determinate-nix-stacked-prs.md) supersede the older
unverified domain, custody, threshold, and credential assumptions.

## Decisions

1. Record operator-approved root thresholds, custody and recovery owners,
   online provider identities, and access policy before generating production
   metadata. Generate root material offline. Never store private keys here.
2. Use the existing provider-backed KeySource boundary. Verify independent
   signing authority and threshold refusal. Do not infer security from a count
   of secrets in one vault.
3. Verify control of the selected HTTPS domain, immutable storage, and
   deployment account. Review cache lifetime against metadata expiry.
4. Sign executables with Developer ID Application and the package with
   Developer ID Installer from the same team. Use the existing notarization
   helper, require acceptance and stapling, and test the downloaded artifacts
   on a clean Mac with default Gatekeeper policy.
5. Build metadata from final signed bytes. Prove production install, repeat,
   upgrade, reboot, removal, and failure behavior with exact hashes. Prepare
   the old installation before advancing a live channel.
6. Publish rollback as a higher sequence with previous approved content.
   Never replay old signed metadata. Rehearse rotation, expiry refresh, and
   compromise response with retained public audit records.

## Open operator decisions

The actual key provider, role thresholds, custody/recovery holders, production
domain, refresh automation, and credentials require verified inputs.
Earlier proposed values are not completed decisions or available resources.
