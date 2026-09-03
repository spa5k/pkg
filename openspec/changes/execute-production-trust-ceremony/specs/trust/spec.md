## ADDED Requirements

### Requirement: Production TUF root with ceremony

The production TUF root key SHALL be generated offline on an air-gapped machine with recorded
ceremony steps, SHALL never exist on a network-connected machine, and online keys SHALL use a
3-of-5 threshold. Root v1 SHALL be published before any production target.

#### Scenario: Offline root generation

- **WHEN** the root key is generated
- **THEN** the ceremony record lists participants, machine, date, and key fingerprint
- **AND** the private key material never transits a network-connected host

#### Scenario: Threshold enforcement

- **WHEN** channel metadata is signed
- **THEN** at least 3 of 5 online keys are required, and the client rejects metadata signed
  below threshold

### Requirement: Stable channel hosting

The release channel SHALL be served at `channel.kelv.dev` over TLS, targets SHALL be immutable
once published, and the product's channel constant SHALL reference this host in production
builds.

#### Scenario: TLS and immutability

- **WHEN** a client fetches metadata or targets
- **THEN** the connection is TLS-verified and a published target's bytes never change

#### Scenario: Proof URLs retired

- **WHEN** production builds are produced
- **THEN** no trycloudflare or localhost URL appears in any shipped binary or script

### Requirement: Signed and notarized macOS package

The macOS `.pkg` SHALL be signed with a Developer ID Application certificate, notarized with
Apple's notary service, and stapled. A clean Mac SHALL install it with a default Gatekeeper
policy without warnings.

#### Scenario: Gatekeeper clean pass

- **WHEN** a user installs the stapled package on a clean macOS machine
- **THEN** Gatekeeper accepts it and `spctl -a -vv` reports accepted with the Developer ID

### Requirement: Production release re-proof

Before public availability, one full slot lifecycle proof SHALL run against the production
channel and the signed package, with evidence archived to the same standard as the DN-16
proof.

#### Scenario: Production proof gate

- **WHEN** the production release candidate exists
- **THEN** the proof workflow runs against `channel.kelv.dev` and the notarized package
- **AND** the release is published only after every slot verdict passes

### Requirement: Key custody runbook

An operations runbook SHALL document key custody locations, signing procedures, rotation
schedule, compromise response, and channel rollback, and SHALL be reviewed after any key event.

#### Scenario: Rotation procedure exists

- **WHEN** an online key needs rotation
- **THEN** the runbook specifies the exact steps and the client's threshold behavior during
  rotation
