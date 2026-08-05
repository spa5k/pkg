// Test-only Ed25519 signing keys for the S2 spike publisher.
//
// CRYPTOGRAPHIC BOUNDARY (per the slice-1 brief):
//
//   * aws-lc-rs is used ONLY to *generate* ephemeral Ed25519 PKCS#8 test
//     material. It performs no parsing and no signing here.
//   * Parsing the PKCS#8, signing bytes, and deriving the TUF public key all go
//     through `tough`'s PUBLIC signing abstraction:
//         tough::sign::parse_keypair  -> impl tough::sign::Sign
//         tough::sign::Sign::sign     -> signature bytes
//         tough::sign::Sign::tuf_key  -> tough::schema::key::Key
//   * There is NO bespoke ("TUF-lite") signature/verification code and NO
//     hand-built `tough::schema::key::Key` in this spike. The TUF `Key` and its
//     key id are always obtained from `Sign::tuf_key()` / `Key::key_id()`.
//
// Every key here is a freshly generated Ed25519 keypair held in memory for the
// duration of a test. The PKCS#8 bytes are written to a file ONLY inside the
// repo's ephemeral temp directory (so `tough::key_source::LocalKeySource` —
// tough's public key source — can read them for `RepositoryEditor`); those
// files are deleted with the temp directory. No private key material is ever
// written outside a test's `TempDir`, and there are no reusable secrets in this
// repository.

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::Ed25519KeyPair;
use olpc_cjson::CanonicalFormatter;
use serde::Serialize;
use std::sync::Arc;
use tough::schema::decoded::{Decoded, Hex};
use tough::schema::key::Key;
use tough::schema::{Signature, Signed};
use tough::sign::{Sign, parse_keypair};

/// An in-memory, test-only Ed25519 signing key.
///
/// Stores only the PKCS#8 bytes plus the TUF public `Key` and key id, both of
/// which are derived through `tough::sign::parse_keypair` + `Sign::tuf_key`
/// (never constructed by hand). Cheap to clone (PKCS#8 lives behind an `Arc`).
#[derive(Clone)]
pub struct SignKey {
    pkcs8: Arc<Vec<u8>>,
    tuf_key: Key,
    key_id: Decoded<Hex>,
}

impl std::fmt::Debug for SignKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Do NOT format the private keypair. Only show the (public) key id.
        f.debug_struct("SignKey")
            .field("key_id", &hex::encode(self.key_id.as_ref()))
            .finish_non_exhaustive()
    }
}

impl SignKey {
    /// Generate a fresh, random Ed25519 keypair.
    ///
    /// aws-lc-rs is used ONLY to produce the PKCS#8; parsing it into a `Sign`
    /// happens in `from_pkcs8` via `tough::sign::parse_keypair`.
    pub fn generate() -> Self {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("generate ed25519 pkcs8");
        Self::from_pkcs8(pkcs8.as_ref())
    }

    /// Build a `SignKey` from raw PKCS#8 DER bytes.
    ///
    /// The TUF public key and key id are derived through tough's public signing
    /// API: `parse_keypair(..).tuf_key()` then `Key::key_id()`.
    pub fn from_pkcs8(pkcs8: &[u8]) -> Self {
        let signer =
            parse_keypair(pkcs8).expect("tough::sign::parse_keypair accepts ed25519 pkcs8");
        let tuf_key = signer.tuf_key();
        let key_id = tuf_key.key_id().expect("compute tuf key id");
        Self {
            pkcs8: Arc::new(pkcs8.to_vec()),
            tuf_key,
            key_id,
        }
    }

    /// The TUF public key, derived via `Sign::tuf_key()`.
    pub fn tuf_key(&self) -> &Key {
        &self.tuf_key
    }

    /// The TUF key id (sha256 of the canonical-JSON key object), via
    /// `Key::key_id()`.
    pub fn key_id(&self) -> &Decoded<Hex> {
        &self.key_id
    }

    /// Hex-encoded key id, for assertions/logs (public, non-secret).
    pub fn key_id_hex(&self) -> String {
        hex::encode(self.key_id.as_ref())
    }

    /// The raw PKCS#8 bytes (test-only). Used only to seed an ephemeral
    /// `LocalKeySource` file inside the repo's temp directory.
    pub fn pkcs8(&self) -> &[u8] {
        &self.pkcs8
    }

    /// Parse the stored PKCS#8 via `tough::sign::parse_keypair` and return a
    /// boxed `Sign` for the root-bootstrap signing path.
    pub fn signer(&self) -> Box<dyn Sign> {
        Box::new(parse_keypair(&self.pkcs8).expect("parse ed25519 pkcs8 via tough::sign"))
    }
}

/// Generate `n` fresh independent Ed25519 keys.
pub fn generate_keys(n: usize) -> Vec<SignKey> {
    (0..n).map(|_| SignKey::generate()).collect()
}

// ---------------------------------------------------------------------------
// NARROW TEST-PUBLISHER BOUNDARY: signing the bootstrap `root.json`.
// ---------------------------------------------------------------------------
//
// `tough::editor::RepositoryEditor` reads an ALREADY-SIGNED `root.json` from
// disk; unlike targets/snapshot/timestamp (which the editor signs through
// `KeySource`s), the *initial* root must be assembled from `tough::schema`
// types and signed once by hand. This is the ONLY manual signing in the spike.
//
// Even here there are NO direct cryptographic calls: the canonical-JSON bytes
// are produced with `olpc_cjson` (the same crate tough uses), and each
// signature is produced through `tough::sign::Sign::sign` (via
// `SignKey::signer`). This exactly mirrors
// `tough::editor::signed::SignedRole::new`. Everything downstream
// (targets/snapshot/timestamp/delegated targets) is signed by
// `RepositoryEditor` + `LocalKeySource`.

/// Sign the `signed` payload of a TUF role with `keys`, returning a `Signed<T>`.
///
/// `role` is canonical-JSON serialized (OLPC), then each key signs it through
/// `tough::sign::Sign::sign`. Used only for the bootstrap `root.json`.
pub async fn sign_role<T: Serialize>(role: T, keys: &[&SignKey]) -> Signed<T> {
    let mut data = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut data, CanonicalFormatter::new());
    role.serialize(&mut ser).expect("canonical-serialize role");

    let rng = SystemRandom::new();
    let mut signatures = Vec::with_capacity(keys.len());
    for k in keys {
        let sig = k
            .signer()
            .sign(&data, &rng)
            .await
            .expect("tough::sign::Sign::sign for root bootstrap");
        signatures.push(Signature {
            keyid: k.key_id().clone(),
            sig: sig.into(),
        });
    }
    Signed {
        signed: role,
        signatures,
    }
}
