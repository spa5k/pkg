# Remaining tasks: production trust

Updated 26 September 2026. Scope: TRUST-01 in the
[active plan](../../../plans/determinate-nix-stacked-prs.md).
The current [runbook](../../../docs/ops/production-release.md) owns operator inputs.

## Already delivered

Provider-backed signing interfaces, test-key TUF transaction checks,
`packaging/macos/notarize.py`, exact-release lifecycle helpers, and the initial
production runbook exist. Do not create duplicate helpers or runbooks.
CLI and installer UX work has shipped; the superseded proposals are not blockers.

## Key custody and hosting

- [ ] Verify production domain control, storage immutability, and deployment access.
- [ ] Approve root/role thresholds, independent custody and recovery owners, and the online signing provider.
- [ ] Generate the root offline and retain only public metadata and ceremony evidence in the repository.
- [ ] Configure provider identities and least-privilege signing access.
- [ ] Prove below-threshold refusal, root/online-key rotation, and revoked-signer refusal on staging.
- [ ] Prove metadata expiry refusal and refresh before expiry; review CDN cache policy.
- [ ] Wire the compiled production root and selected channel into reviewed production builds.

## Apple signing

- [ ] Supply Application and Installer identities for the same Apple team and notarization access.
- [ ] Run the existing signing/notarization helper against final command binaries and package assets.
- [ ] Update the installer renderer and asset inventory for production filenames together.
- [ ] Retain submission IDs, hashes, stapling, and clean-Mac Gatekeeper evidence for actual downloads.

## Final proof and operations

- [ ] Accept complete assurance results, including the planned two-week scheduled-check observation period.
- [ ] Prepare and test exact production install/upgrade/reboot/removal inputs on native hosts.
- [ ] Verify published root and targets independently from another host.
- [ ] Complete provider-specific signing, custody, rotation, refresh, compromise, and higher-sequence rollback procedures in the existing runbook.
- [ ] Record owner/reviewer go-live approval and public evidence before announcing production readiness.

No production input is treated as supplied because a local preflight or alpha
release passed. No release is part of the current onboarding audit.
