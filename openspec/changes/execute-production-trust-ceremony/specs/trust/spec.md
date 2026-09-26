## ADDED Requirements

### Requirement: Reviewed production signing authority

Production root keys SHALL be generated offline with recorded custody and
recovery owners. Role thresholds and provider identities SHALL be approved and
verified before use. Below-threshold and revoked signers SHALL be rejected.

#### Scenario: Threshold refusal

- **WHEN** staging metadata lacks the required approved signing authority
- **THEN** the product refuses it without changing installed state

### Requirement: Immutable HTTPS channel

The selected production channel SHALL have verified domain and deployment
control. Published target bytes SHALL be immutable. Metadata expiry and refresh
SHALL be tested against the chosen cache policy.

#### Scenario: Rollback

- **WHEN** approved content must be restored
- **THEN** publication uses a higher sequence and does not replay old metadata

### Requirement: Signed and notarized macOS artifacts

Command binaries SHALL use Developer ID Application signatures. The package
SHALL use Developer ID Installer from the same team. Notarization acceptance,
stapling, and clean-Mac Gatekeeper checks SHALL pass before production release.

#### Scenario: Clean-Mac verification

- **WHEN** the final package and commands are downloaded on a clean Mac
- **THEN** default Gatekeeper policy accepts the exact published artifacts

### Requirement: Final artifact proof

Production proof SHALL identify exact signed bytes, native host identity,
installation, upgrade, reboot, removal, and required failure checks. Skipped
required checks SHALL prevent a production-readiness claim.

#### Scenario: Live-channel upgrade

- **WHEN** the channel will advance from N to N+1
- **THEN** the old installation is prepared first and retained for the same-VM upgrade and reboot proof

### Requirement: Operations evidence

The existing runbook SHALL contain reviewed provider-specific custody,
signing, rotation, refresh, compromise, and rollback procedures.

#### Scenario: Missing input

- **WHEN** credentials, key custody, domain control, or native proof are absent
- **THEN** tooling readiness is reported separately from production readiness
