# Release service boundary

This crate implements the provider-neutral, security-sensitive part of a V1
release:

- parse a closed release manifest and ask an external authority to authenticate
  distinct release/security attestations while reserving the next sequence;
- require exactly the descriptor, four managed-Nix archives, four privileged
  asset manifests, and four per-system indexes as TUF targets;
- keep the three CLI binaries and their Sigstore bundles outside TUF while
  checking every committed checksum and length;
- accept an already-signed offline root and sign only targets, snapshot, and
  timestamp through `tough::key_source::KeySource`; the exact trusted-root
  SHA-256 is part of the approved manifest, and timestamp refresh authority
  independently checks the same root digest;
- derive signed target hashes from the approved manifest, rehash immediately
  before signing, and let `tough` recheck those exact bytes while creating
  consistent-snapshot target copies;
- TUF-verify every target, then copy every publication object into an anonymous
  file and expose only read-only cursors so publishers never reopen mutable paths;
- preflight, idempotently ensure, and verify immutable objects at both GitHub
  Releases and the CDN mirror, then activate GitHub source-of-truth before the
  CDN mirror;
- refresh short-lived `timestamp.json` independently from the product channel
  sequence, with a separate monotonic authority lease and atomic stable-route
  update at both destinations;
- write a mandatory create-only, allowlisted signing audit event whose actor
  comes from the authorization lease rather than a caller-supplied string.

The CI workflow uses fresh in-memory Ed25519 test keys. It proves a 2-of-3
offline root, separate online-role keys, a real signed repository, and a real
`tough` client verification. Test keys never leave process memory.

Production deployment must provide KMS/HSM-backed `KeySource`, `ReleaseAuthority`,
and `Publisher` adapters. The authority must verify approval attestations,
exclusively reserve the authoritative sequence, expose the authenticated
workload identity, keep uncommitted leases reacquirable by id, and idempotently
commit a published lease for crash recovery. Authorized operational cleanup owns
abandoned-lease cancellation. No local-key production adapter exists, so selecting a
cloud does not silently move root or online private keys into GitHub secrets.
Root signing is an offline custody ceremony; `sign_channel` accepts only signed
root metadata. The provider choice, destination credentials, protected release
environments, and custodian roster are operational configuration, not repository
defaults.

`Publisher::ensure_object` must treat an already-present exact digest and length
as success and reject conflicts. Both release discovery and timestamp routing
must be idempotent for the same version/digest. A signed transaction cannot be
published directly: `persist` atomically writes a closed transaction record,
exact object blobs, hashes, and the opaque authority lease id to a private
directory first. `DurableRelease::resume` and `DurableTimestampRefresh::resume`
rehash every blob, recompute the closed transaction digest, and reacquire the
same externally durable lease only when its authority-stored digest matches.
This prevents local transaction-record substitution after approval. Publication
APIs borrow only these durable transactions; if a remote or
authority commit fails—or the process crashes—the exact transaction is retried.
Successful reconciliation writes an idempotent `COMMITTED` marker.
Before an unactivated transaction uploads or becomes authoritative, publication
parses the sealed TUF roles and requires at least one hour of validity remaining.
A durable transaction that aged out is refused and must be re-signed under a new
authorized transaction if GitHub has not activated it. Once GitHub reports the
exact digest active, even an aged recovery may only finish that same mirror and
authority lease; it cannot activate a different digest.

The trusted offline root must outlive every metadata expiration it authorizes.
Timestamp refresh refuses an expired root or snapshot, and refuses either one
that expires before the requested timestamp. V1 accepts only the exact
authority-approved current root; a later root rotation must publish and verify
the complete sequential root-update chain rather than substituting a self-signed
trust anchor.
