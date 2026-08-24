# Release service boundary

This crate implements the provider-neutral, security-sensitive part of a V1
release:

- parse a closed release manifest and ask an external authority to authenticate
  distinct release/security attestations while reserving the next sequence;
- require exactly the descriptor, two managed-Nix archives, two privileged
  asset manifests, two per-system indexes, and the closed Determinate inventory
  as TUF targets;
- keep the two `pkg` binaries and one Linux `pkg-install` binary with their
  Sigstore bundles outside TUF while
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

The Determinate inventory is fixed to version 3.22.1 and revision
`4132ad07a15ee7d88c096ac7172b7afb2672866b`. It contains these three installer
binaries:

| System | Length | SHA-256 |
| --- | ---: | --- |
| `aarch64-darwin` | 58427232 | `90cb96f597530553eef1311b37124d1e895fdb3a19877e65a4572dda7753f50b` |
| `aarch64-linux` | 69625424 | `9cf29b616f7a2ea430e054b163f507a9157511c6951dfa9e55dd9e3a270d9179` |
| `x86_64-linux` | 74918096 | `9e7a42aaf618a42231dfe400f36fe7438b9d916ccd13b29c2ff4de90ecc95c5c` |

The Linux ARM entry is a release asset. It does not enable Linux ARM product
support. There is no Intel macOS entry.

The same TUF inventory contains the LGPL-2.1 `LICENSE` from the pinned revision.
It is 26434 bytes with SHA-256
`36b6d3fa47916943fd5fec313c584784946047ec1337a78b440e5992cb595f89`.
It also contains the v3.22.1 source archive from the official GitHub codeload
tag URL. The archive is 214322 bytes with SHA-256
`e946ce0920e1ac0a76281d1d0d24b5ddb0fa1807f5317d1545130fe8a04ff084`.
The release tool accepts only the fixed upstream URLs recorded in the closed
manifest schema. It does not download these files. Put the five verified files
in the release input `determinate/` directory. The signer checks regular-file
identity, length, and SHA-256 again before it signs and seals the targets.

The Linux `pkg-install` build embeds the approved root and the immutable HTTPS
metadata and target directory URLs. The release build sets
`PKG_RELEASE_TUF_ROOT_JSON`, `PKG_RELEASE_CHANNEL_METADATA_URL`, and
`PKG_RELEASE_CHANNEL_TARGETS_URL`. The public installer accepts no replacement
values.

`stage_linux_alpha.py` accepts one already-built x86-64 Linux `pkg-install`,
places it under `v0.1.0-alpha.7/`, computes its SHA-256, and renders the small
bootstrap template with one fixed HTTPS release path. It does not build, sign,
or publish. The retained CI artifact uses an ephemeral test root. Production
staging waits for the external key ceremony and hosting activation.

`package_alpha_candidate.py` builds deterministic Linux x86-64 and macOS arm64
archives from prepared proof files. Each archive contains checksums, the
Apache-2.0 license, Rust dependency licenses, the Nix 2.34.8 LGPL-2.1 text,
exact Nix source information, and fixed release notes. It rejects the wrong
binary format, symlinks, a changed Nix source archive, and existing output.

The candidate archives stay outside TUF. They contain no TUF metadata, proof
keys, proof certificates, or proof service files. Their release notes say
`TEST KEYS. LOOPBACK SERVICE. NOT FOR PUBLICATION.` Production signing and
fixed hosting remain required.

Build one native index on a release host that has the managed pkg runtime:

```sh
cargo run -p pkg-release --bin pkg-release-index -- \
  1 aarch64-darwin <nixpkgs-revision> <nixpkgs-nar-hash> \
  2026-08-18T00:00:00Z index.aarch64-darwin.json.br
```

The command uses the fixed managed Nix binary, home, daemon, and projection.
It accepts no Nix command, expression, installable, store path, option, URL,
or trust root. The output file must not exist.
The output is the deterministic Brotli target used by the signed channel.

Install cargo-about 0.9.1. The candidate packager runs it with the fixed
configuration and the locked workspace:

```sh
cargo install --locked cargo-about --version 0.9.1 --features cli \
  --root target/release-tools
```

Download the exact Nix source archive before candidate packaging:

```sh
curl --fail --location --proto '=https' --proto-redir '=https' \
  --output nix-2.34.8.tar.gz \
  https://github.com/NixOS/nix/archive/refs/tags/2.34.8.tar.gz
printf '%s  %s\n' \
  ecc2f226a1ba27ad56eb85f42af8f078067fe5a219fceb82cb3fda9ba24387a5 \
  nix-2.34.8.tar.gz | shasum -a 256 --check
```

Build, package, and prove the exact Linux candidate with this command:

```sh
PKG_CARGO_ABOUT=target/release-tools/bin/cargo-about \
PKG_NIX_SOURCE_ARCHIVE=$PWD/nix-2.34.8.tar.gz \
tests/linux-clean-host/run.sh --keep-artifacts \
  target/release-candidates/linux
```

The proof extracts the new archive. It runs only with those extracted payload
files. The two environment variables are required with `--keep-artifacts`.

Use `macos-aarch64` with a prepared macOS payload. The macOS payload must
contain `v0.1.0-alpha.7/pkg-install` and
`v0.1.0-alpha.7/pkg-0.1.0-alpha.7-preview.pkg`.

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
