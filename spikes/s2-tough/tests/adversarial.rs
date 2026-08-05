// Slice-2 adversarial tests: the security guarantees pkg relies on tough to
// enforce, each pinned to a SPECIFIC tough error variant so a regression in
// tough (or a future bump) cannot silently turn a hard refusal into an accept.
//
// These tests perform NO bespoke ("TUF-lite") cryptography and NO direct
// cryptographic signing. Repositories are built by the existing
// `RepoBuilder` (real `tough::editor::RepositoryEditor`) and loaded by the
// existing `Verifier` (real `tough::RepositoryLoader` over
// `FilesystemTransport`). Every refusal below is produced by tough's own
// client verification, never by publisher-side validation.
//
// Cases covered (PR-5 / S2 slice A):
//   (1) per-role THRESHOLD semantics — differing thresholds across the four
//       roles load when met, and insufficient *valid* signatures are rejected
//       by tough's client (not the editor's publisher-side count), with the
//       failure attributed to the specific role whose threshold was unmet
//       (role-local);
//   (2) ExpirationEnforcement::Safe refuses actually-expired signed metadata
//       against the real clock (jiff::Timestamp::now()), matching
//       `tough::Error::ExpiredMetadata`;
//   (3) conservative `Limits` refuse oversized signed metadata, matching
//       `tough::Error::MaxSizeExceeded` (carried inside a `Transport` error);
//   (4) a one-byte mutation of an advertised target after repository load is
//       refused by `read_target_fully`, which only errors AFTER the stream is
//       fully drained — matching `tough::Error::HashMismatch` — so no tampered
//       bytes are returned or persisted.
//
// All PKCS#8 key material lives only inside each test's `TempDir`.

use pkg_spike_s2_tough::DelegationSpec;
use pkg_spike_s2_tough::repo::{build_root, hours_from_now, root_json_bytes};
use pkg_spike_s2_tough::{
    CONSERVATIVE_LIMITS, RepoBuilder, RoleSpec, RootSpec, SignKey, Verifier, generate_keys,
    read_target_fully, sign_role,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tough::error::Error as ToughError;
use tough::schema::Error as SchemaError;
use tough::schema::RoleType;
use tough::{ExpirationEnforcement, TargetName};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A freshly-created persistent datastore directory (required for rollback
/// protection) plus the `TempDir` that owns the whole ephemeral repo.
struct OwnedRepo {
    _tmp: TempDir,
    repo: pkg_spike_s2_tough::RepoPaths,
    datastore: PathBuf,
}

impl OwnedRepo {
    fn verifier(&self) -> Verifier {
        Verifier::new(
            self.repo.root_bytes.clone(),
            self.repo.metadata_url(),
            self.repo.targets_url(),
        )
    }
}

/// Build and sign a minimal single-target repository with the given root spec.
async fn build(spec: RootSpec) -> OwnedRepo {
    let tmp = TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    let datastore = tmp.path().join("datastore");
    std::fs::create_dir_all(&datastore).unwrap();
    let repo = RepoBuilder::new(repo_dir, spec)
        .target("hello.txt", b"hello world\n".to_vec())
        .write()
        .await;
    OwnedRepo {
        _tmp: tmp,
        repo,
        datastore,
    }
}

/// A 1-of-1 root spec valid for ~30 days (the slice-1 default shape).
fn single_key_spec() -> RootSpec {
    let key = SignKey::generate();
    RootSpec::single(key, 1, hours_from_now(24 * 30))
}

/// Corrupt exactly ONE signature in a signed metadata file on disk by flipping
/// a single hex character of its `sig` field to a DIFFERENT valid hex
/// character. The file remains valid JSON and the `signed` payload is
/// untouched, so the signature's key id still names a role key — the signature
/// simply stops verifying cryptographically. This is the cleanest way to make
/// the CLIENT threshold check fire (the publisher produced a full signature
/// set that passed the editor's publisher-side count; only tough's per-signature
/// `verify_role` catches that one signature is no longer authentic).
fn corrupt_one_signature(path: &std::path::Path) {
    let bytes = std::fs::read(path).unwrap();
    let mut v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let sigs = v
        .get_mut("signatures")
        .expect("metadata has a signatures array")
        .as_array_mut()
        .expect("signatures is an array");
    assert!(
        !sigs.is_empty(),
        "metadata must carry at least one signature to corrupt"
    );
    let sig_str = sigs[0]
        .get("sig")
        .expect("signature object has a sig field")
        .as_str()
        .expect("sig is a string")
        .to_string();
    let mut chars: Vec<char> = sig_str.chars().collect();
    let orig = chars[0];
    // Flip the first hex char to a different VALID hex char so the value still
    // deserializes as `Decoded<Hex>`, but the decoded signature bytes differ.
    chars[0] = if orig == '0' { '1' } else { '0' };
    sigs[0]["sig"] = serde_json::Value::String(chars.into_iter().collect());
    std::fs::write(path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
}

/// Snapshot every regular file under `root` as a sorted `(relative path,
/// bytes)` vector. Test-only: lets two snapshots of the datastore be compared
/// with `assert_eq!` to prove an operation changed NOTHING on disk. Only
/// regular files are captured (directories and symlinks are skipped); the
/// walk descends into subdirectories iteratively.
fn datastore_snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let ft = entry.file_type().unwrap();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

// ===========================================================================
// (1) Per-role THRESHOLD semantics
// ===========================================================================
//
// Root spec configures DIFFERING, per-role thresholds across all four roles:
//   root     = 1-of-1  (threshold 1)
//   targets  = 2-of-2  (threshold 2)
//   snapshot = 1-of-1  (threshold 1)
//   timestamp= 2-of-2  (threshold 2)
// This is non-uniform (the slice-1 default is 1-of-1 everywhere), so a
// successful load proves tough honors an independent threshold per role.
fn differing_threshold_spec() -> RootSpec {
    let root_key = SignKey::generate();
    let targets_keys = generate_keys(2);
    let snapshot_key = SignKey::generate();
    let timestamp_keys = generate_keys(2);
    RootSpec {
        root: RoleSpec::single(root_key),
        targets: RoleSpec::threshold_of(targets_keys, 2),
        snapshot: RoleSpec::single(snapshot_key),
        timestamp: RoleSpec::threshold_of(timestamp_keys, 2),
        consistent_snapshot: true,
        version: 1,
        expires: hours_from_now(24 * 30),
    }
}

/// A repository whose root declares DIFFERING per-role thresholds (root=1,
/// targets=2, snapshot=1, timestamp=2), correctly signed so every role MEETS
/// its own threshold, loads through tough's full client verification without
/// error, and its target reads back byte-for-byte.
#[tokio::test]
async fn differing_per_role_thresholds_load_when_met() {
    let owned = build(differing_threshold_spec()).await;
    let verifier = owned.verifier();

    let repo = verifier
        .load(&owned.datastore)
        .await
        .expect("a correctly signed repo with differing per-role thresholds must load");

    // Full verification (root -> timestamp -> snapshot -> targets) succeeded;
    // prove the target is readable too.
    let got = read_target_fully(&repo, &TargetName::new("hello.txt").unwrap())
        .await
        .expect("read hello.txt")
        .expect("hello.txt present");
    assert_eq!(got, b"hello world\n");
}

/// Insufficient VALID signatures are rejected by tough's CLIENT — not merely by
/// the editor's publisher-side count. tough's `timestamp.json` is fetched with
/// `fetch_max_size` (it has NO parent role recording its hash, unlike
/// targets/snapshot), so corrupting one of its two signatures lets the bytes
/// reach `Root::verify_role`, which cryptographically verifies each signature
/// and counts only the valid ones. The publisher produced two valid signatures
/// (passing the editor's publisher-side `threshold <= signatures.len()` check
/// in `SignedRole::new`); after delivery one signature no longer verifies, so
/// tough independently rejects the metadata.
///
/// The refusal is `tough::Error::VerifyMetadata { role: Timestamp }` wrapping
/// `tough::schema::Error::SignatureThreshold { role: Timestamp, threshold: 2,
/// valid: 1 }`. The `role: Timestamp` + `threshold: 2` fields are the concrete
/// role-local assertion: tough looks the threshold up PER ROLE
/// (`root.roles[RoleType::Timestamp].threshold == 2`), so the enforced value is
/// timestamp's own and is distinct from root/snapshot (1).
#[tokio::test]
async fn insufficient_valid_signatures_rejected_by_tough_client_role_local() {
    let owned = build(differing_threshold_spec()).await;
    let verifier = owned.verifier();

    // Sanity: the unmodified repo loads (all thresholds met).
    verifier
        .load(&owned.datastore)
        .await
        .expect("unmodified repo loads");

    // Use a SECOND datastore so this refusal is not confounded by the
    // successful load's persisted timestamp (rollback protection).
    let ds2 = owned._tmp.path().join("datastore2");
    std::fs::create_dir_all(&ds2).unwrap();

    // Corrupt ONE of timestamp.json's two signatures on disk. timestamp.json
    // has no version prefix (it is the top of the chain), so the path is fixed.
    let timestamp_path = owned.repo.metadata_dir.join("timestamp.json");
    assert!(timestamp_path.is_file(), "timestamp.json exists on disk");
    corrupt_one_signature(&timestamp_path);

    let err = verifier
        .load(&ds2)
        .await
        .expect_err("tough must reject metadata below the role threshold");

    // The outer error is the per-role verification failure for Timestamp.
    match &err {
        ToughError::VerifyMetadata { role, source, .. } => {
            assert_eq!(
                *role,
                RoleType::Timestamp,
                "threshold failure must be attributed to the Timestamp role"
            );
            // The inner cause is the unmet threshold, carrying the role-local
            // threshold value (2) and the count of valid signatures (1).
            match source {
                SchemaError::SignatureThreshold {
                    role,
                    threshold,
                    valid,
                    ..
                } => {
                    assert_eq!(*role, RoleType::Timestamp);
                    assert_eq!(*threshold, 2, "enforced threshold is timestamp's own (2)");
                    assert_eq!(*valid, 1, "exactly one of two signatures still verifies");
                }
                other => panic!(
                    "expected SignatureThreshold inside VerifyMetadata, got {:?}",
                    other
                ),
            }
        }
        other => panic!("expected VerifyMetadata(Timestamp), got {:?}", other),
    }
}

// ===========================================================================
// (2) ExpirationEnforcement::Safe refuses actually-expired signed metadata
// ===========================================================================

/// `ExpirationEnforcement::Safe` refuses signed metadata that is already
/// expired, evaluated by tough against the REAL clock
/// (`datastore.system_time()` -> `jiff::Timestamp::now()`). The targets role
/// is expired one hour in the past (`targets_expires(hours_from_now(-1))`);
/// root/timestamp/snapshot remain valid, so the load reaches the targets
/// expiration check (TUF 5.6.5) and is refused there with
/// `tough::Error::ExpiredMetadata { role: Targets }`.
///
/// The contrast with `ExpirationEnforcement::Unsafe` on the SAME expired repo
/// isolates that it is specifically `Safe` (and the real clock) doing the
/// refusing: `Unsafe` loads the identical expired metadata without error. We
/// do NOT claim tough checks `descriptor.expiresAt` or any build-time value;
/// only the signed TUF metadata expiration, against the wall clock.
#[tokio::test]
async fn expiration_safe_refuses_expired_targets_against_real_clock() {
    let owned = expired_targets_repo().await;
    let verifier = owned.verifier();

    // (a) SAFE mode refuses the expired metadata against the real clock.
    let err = verifier
        .load(&owned.datastore)
        .await
        .expect_err("Safe mode must refuse expired targets metadata");
    match &err {
        ToughError::ExpiredMetadata { role, .. } => {
            assert_eq!(
                *role,
                RoleType::Targets,
                "expiry refusal must name the Targets role whose expiration lapsed"
            );
        }
        other => panic!("expected ExpiredMetadata(Targets), got {:?}", other),
    }

    // (b) Contrast: UNSAFE mode loads the identical expired metadata, proving
    // the refusal above is caused by Safe + the real clock, not by any other
    // defect in the repo.
    let ds_unsafe = owned._tmp.path().join("datastore_unsafe");
    std::fs::create_dir_all(&ds_unsafe).unwrap();
    verifier
        .load_with(
            &ds_unsafe,
            CONSERVATIVE_LIMITS,
            ExpirationEnforcement::Unsafe,
        )
        .await
        .expect("Unsafe mode does not enforce expiration");
}

/// Build a repo whose TARGETS role expired one hour ago (root/snapshot ~30d
/// out, timestamp ~1d out) — so only targets is expired.
async fn expired_targets_repo() -> OwnedRepo {
    let tmp = TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    let datastore = tmp.path().join("datastore");
    std::fs::create_dir_all(&datastore).unwrap();
    let spec = single_key_spec();
    let repo = RepoBuilder::new(repo_dir, spec)
        .targets_expires(hours_from_now(-1))
        .target("hello.txt", b"hello world\n".to_vec())
        .write()
        .await;
    OwnedRepo {
        _tmp: tmp,
        repo,
        datastore,
    }
}

// ===========================================================================
// (3) Conservative Limits refuse oversized signed metadata
// ===========================================================================

/// Empirically chosen count of timestamp keys. Each authorized timestamp key
/// contributes one signature to `timestamp.json` (~240 B/sig in tough's pretty
/// serialization), so with `TIMESTAMP_KEY_COUNT` keys `timestamp.json` lands at
/// ~38 KiB — comfortably OVER `max_timestamp_size` (32 KiB). The same keys
/// also appear in the pinned `root.json` (~340 B/key), which lands at ~55 KiB —
/// comfortably UNDER `max_root_size` (64 KiB). Both margins exceed 5 KiB.
const TIMESTAMP_KEY_COUNT: usize = 160;

/// A repository whose timestamp role authorizes `TIMESTAMP_KEY_COUNT` keys
/// (threshold 1). `RepositoryEditor` signs the timestamp role with EVERY
/// authorized key that is supplied, so `timestamp.json` carries
/// `TIMESTAMP_KEY_COUNT` signatures and exceeds `max_timestamp_size`. Root,
/// targets, and snapshot stay 1-of-1 (one shared key) so the pinned
/// `root.json` stays well under `max_root_size` — meaning the load reaches the
/// timestamp fetch and the limit that fires is specifically `max_timestamp_size`.
fn oversized_timestamp_spec() -> RootSpec {
    let root_key = SignKey::generate();
    let timestamp_keys = generate_keys(TIMESTAMP_KEY_COUNT);
    RootSpec {
        root: RoleSpec::single(root_key.clone()),
        targets: RoleSpec::single(root_key.clone()),
        snapshot: RoleSpec::single(root_key.clone()),
        timestamp: RoleSpec::threshold_of(timestamp_keys, 1),
        consistent_snapshot: true,
        version: 1,
        expires: hours_from_now(24 * 30),
    }
}

/// The conservative `Limits` mechanism refuses an *actually* oversized signed
/// `timestamp.json` — using `CONSERVATIVE_LIMITS` UNCHANGED, not a copied limit
/// reduced below the real file. We build a validly signed repository whose
/// `timestamp.json` genuinely exceeds `CONSERVATIVE_LIMITS.max_timestamp_size`
/// by authorizing `TIMESTAMP_KEY_COUNT` ephemeral timestamp keys (each key adds
/// a signature to timestamp.json through `RepositoryEditor`), while the pinned
/// `root.json` still fits under `CONSERVATIVE_LIMITS.max_root_size`. We assert
/// BOTH file sizes on disk BEFORE load.
///
/// `timestamp.json` is fetched with a hard cap of `max_timestamp_size` (it is
/// the top of the chain; unlike targets/snapshot there is no parent role
/// recording its length, so `max_timestamp_size` is enforced directly). tough
/// refuses the oversized file with `tough::Error::MaxSizeExceeded`, carried
/// inside a `tough::Error::Transport`. We assert the exact error chain and that
/// the reported `max_size` equals the real
/// `CONSERVATIVE_LIMITS.max_timestamp_size` constant (not a reduced copy), with
/// specifier `"max_timestamp_size argument"`. This exercises the
/// `max_timestamp_size` limit only; it does NOT prove endless-data protection
/// for the other limit fields.
#[tokio::test]
async fn conservative_limits_refuse_oversized_timestamp_metadata() {
    let owned = build(oversized_timestamp_spec()).await;
    let verifier = owned.verifier();

    // Assert BOTH file sizes on disk BEFORE load: timestamp.json is genuinely
    // over max_timestamp_size, and root.json still fits under max_root_size
    // (so the limit that fires is specifically the timestamp one).
    let timestamp_path = owned.repo.metadata_dir.join("timestamp.json");
    let root_path = owned.repo.metadata_dir.join("1.root.json");
    let timestamp_bytes = std::fs::read(&timestamp_path).unwrap();
    let root_bytes = std::fs::read(&root_path).unwrap();
    assert!(
        (timestamp_bytes.len() as u64) > CONSERVATIVE_LIMITS.max_timestamp_size,
        "timestamp.json ({} B) must exceed max_timestamp_size ({} B)",
        timestamp_bytes.len(),
        CONSERVATIVE_LIMITS.max_timestamp_size
    );
    assert!(
        (root_bytes.len() as u64) < CONSERVATIVE_LIMITS.max_root_size,
        "root.json ({} B) must fit under max_root_size ({} B)",
        root_bytes.len(),
        CONSERVATIVE_LIMITS.max_root_size
    );

    // Load with the conservative limits pkg ships — UNCHANGED, not a reduced
    // copy. tough refuses the oversized timestamp.json during fetch.
    let err = verifier
        .load_with(
            &owned.datastore,
            CONSERVATIVE_LIMITS,
            ExpirationEnforcement::Safe,
        )
        .await
        .expect_err("oversized timestamp metadata must be refused");

    // tough surfaces a transport-layer error wrapping MaxSizeExceeded.
    let transport = match &err {
        ToughError::Transport { source, .. } => source,
        other => panic!("expected Transport error for oversize, got {:?}", other),
    };
    assert_eq!(
        transport.kind(),
        tough::TransportErrorKind::Other,
        "max-size refusal is a generic transport failure"
    );
    // The concrete cause is MaxSizeExceeded with the REAL configured cap +
    // specifier (the actual conservative constant, not a reduced copy).
    let cause = std::error::Error::source(transport)
        .expect("transport error has a cause")
        .downcast_ref::<ToughError>()
        .expect("cause is a tough::Error");
    match cause {
        ToughError::MaxSizeExceeded {
            max_size,
            specifier,
            ..
        } => {
            assert_eq!(
                *max_size, CONSERVATIVE_LIMITS.max_timestamp_size,
                "reported cap is the real conservative max_timestamp_size constant"
            );
            assert_eq!(
                *specifier, "max_timestamp_size argument",
                "specifier names the timestamp size limit"
            );
        }
        other => panic!("expected MaxSizeExceeded cause, got {:?}", other),
    }
}

// ===========================================================================
// (4) One-byte target tamper is refused after drain; no bytes returned
// ===========================================================================

/// After the repository loads (so the in-memory targets metadata pins the
/// authentic sha256), mutate ONE byte of the advertised target file on disk
/// (same length — only the content changes) and prove `read_target_fully`
/// returns an error. tough's `DigestAdapter` only emits the hash-mismatch error
/// when the stream reaches end-of-input, i.e. AFTER every byte has been
/// produced; `read_target_fully` drains the whole stream into a `Vec` via
/// `IntoVec`, so the error surfaces exactly once draining is complete and the
/// collected (tampered) bytes are dropped — never returned, never written to
/// disk by this path.
///
/// The error is `tough::Error::Transport` wrapping a `TransportError` whose
/// cause is `tough::Error::HashMismatch` (`expected` == the signed sha256,
/// `calculated` == the tampered sha256). We prefer this hash variant over the
/// length variant because the one-byte mutation leaves the signed length
/// unchanged.
#[tokio::test]
async fn one_byte_target_tamper_refused_after_drain_no_bytes_returned() {
    use pkg_spike_s2_tough::repo::sha256_hex;

    let owned = build(single_key_spec()).await;
    let verifier = owned.verifier();
    let repo = verifier
        .load(&owned.datastore)
        .await
        .expect("unmodified repo loads");

    let original: Vec<u8> = b"hello world\n".to_vec();
    let original_sha = sha256_hex(&original);

    // The consistent-snapshot target file is named `{sha256}.hello.txt`.
    let target_file = owned
        .repo
        .targets_dir
        .join(format!("{original_sha}.hello.txt"));
    assert!(
        target_file.is_file(),
        "consistent-snapshot target file exists"
    );

    // Sanity: the authentic target reads back before tampering.
    let clean = read_target_fully(&repo, &TargetName::new("hello.txt").unwrap())
        .await
        .expect("read clean target")
        .expect("target present");
    assert_eq!(clean, original);

    // Mutate exactly ONE byte (flip the trailing '\n' to 'x'); length is
    // unchanged so the signed length bound is not what trips.
    let mut tampered = original.clone();
    let last = tampered.len() - 1;
    tampered[last] = if original[last] == b'\n' { b'x' } else { b'\n' };
    assert_eq!(
        tampered.len(),
        original.len(),
        "one-byte mutation keeps length"
    );
    std::fs::write(&target_file, &tampered).unwrap();
    let tampered_sha = sha256_hex(&tampered);
    assert_ne!(tampered_sha, original_sha, "mutation changed the digest");

    // Snapshot the ENTIRE datastore (every regular file's relative path +
    // bytes) right before the tampered read; we assert it is unchanged
    // afterward (below, after the error-chain assertions), with one documented
    // exception.
    let datastore_before = datastore_snapshot(&owned.datastore);

    // read_target_fully must REFUSE — and only after fully draining (the
    // DigestAdapter emits the mismatch at end-of-stream).
    let res = read_target_fully(&repo, &TargetName::new("hello.txt").unwrap()).await;
    assert!(
        res.is_err(),
        "tampered target must error, never return bytes"
    );
    let err = res.unwrap_err();

    // Unwrap the transport error and downcast its cause to HashMismatch.
    let transport = match &err {
        ToughError::Transport { source, .. } => source,
        other => panic!("expected Transport error for tamper, got {:?}", other),
    };
    assert_eq!(
        transport.kind(),
        tough::TransportErrorKind::Other,
        "hash-mismatch refusal is a generic transport failure"
    );
    let cause = std::error::Error::source(transport)
        .expect("transport error has a cause")
        .downcast_ref::<ToughError>()
        .expect("cause is a tough::Error");
    match cause {
        ToughError::HashMismatch {
            calculated,
            expected,
            ..
        } => {
            assert_eq!(
                *expected, original_sha,
                "expected hash is the signed (authentic) sha256"
            );
            assert_eq!(
                *calculated, tampered_sha,
                "calculated hash is the tampered sha256"
            );
        }
        other => panic!("expected HashMismatch cause, got {:?}", other),
    }

    // The tampered read must persist nothing of its own: tough caches only
    // METADATA (written during `load`, before this read), never target
    // content, and the read path writes no target bytes. We snapshot every
    // datastore regular file before/after and assert (a) the exact SET of
    // files is unchanged — so no tampered target (`hello.txt` or otherwise)
    // was cached, strictly stronger than the old single-file absence check —
    // and (b) every file's bytes are identical, with a single documented
    // exception.
    let datastore_after = datastore_snapshot(&owned.datastore);
    let before_paths: Vec<&PathBuf> = datastore_before.iter().map(|(p, _)| p).collect();
    let after_paths: Vec<&PathBuf> = datastore_after.iter().map(|(p, _)| p).collect();
    assert_eq!(
        before_paths, after_paths,
        "tampered read must not add or remove any datastore file"
    );
    // `latest_known_time.json` is tough's monotonic-clock bookkeeping: in Safe
    // mode `read_target` re-samples the wall clock for its expiry check
    // (`Datastore::system_time` rewrites the file), so its value legitimately
    // advances on every read. It is neither signed metadata nor target
    // content, so advancing it is the documented, benign side effect of the
    // Safe expiry check. Every OTHER file must be byte-for-byte unchanged.
    for ((path, before_bytes), (_, after_bytes)) in datastore_before.iter().zip(&datastore_after) {
        if path.as_path() == Path::new("latest_known_time.json") {
            continue;
        }
        assert_eq!(
            before_bytes, after_bytes,
            "tampered read must leave {:?} byte-for-byte unchanged",
            path
        );
    }
    // And the on-disk target still holds the tampered bytes (we did not mutate
    // it back), proving nothing "healed" the file.
    assert_eq!(
        std::fs::read(&target_file).unwrap(),
        tampered,
        "on-disk target remains the tampered bytes we wrote"
    );
}

// ===========================================================================
// (5) Cross-run ROLLBACK protection depends on persisted datastore state
// ===========================================================================

/// Read `signed.version` out of any on-disk signed metadata role JSON (pretty
/// or compact). Test-only sanity helper: it parses NO signatures and performs NO
/// verification — it just reports the version field the publisher wrote, so a
/// test can state precisely which version is served vs. persisted.
fn role_version_on_disk(path: &Path) -> u64 {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let v: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    v["signed"]["version"]
        .as_u64()
        .unwrap_or_else(|| panic!("missing signed.version in {}", path.display()))
}

/// Cross-run ROLLBACK protection (TUF 5.4.3.1) is enforced by tough's
/// `load_timestamp` against the *persisted* datastore copy of `timestamp.json`,
/// NOT against any in-memory or publisher-side state. We prove this end-to-end
/// with two loads of the SAME metadata URL through the SAME datastore:
///
/// 1. Publish a validly signed repository whose targets/snapshot/timestamp are
///    all version 2 (root stays version 1 — the trusted root) and LOAD it with a
///    pre-created persistent datastore. `load_timestamp` writes `timestamp.json`
///    (v2) into that datastore (`datastore.create("timestamp.json", ..)`).
/// 2. At the SAME metadata URLs, republish a validly signed version-1 set of
///    metadata using the SAME role keys and the SAME trusted root (targets/
///    snapshot/timestamp all version 1). The `timestamp.json` served at the
///    fixed (unversioned) `timestamp.json` URL is now version 1.
/// 3. A second `load` with the SAME datastore path must be REFUSED: tough reads
///    the persisted `timestamp.json` (v2) from the datastore, verifies it
///    against the trusted root (same keys, so it verifies), and runs
///    `ensure!(old_timestamp.version <= timestamp.version)`. With persisted=2
///    and fetched=1 that is `2 <= 1` = false, so tough returns
///    `tough::Error::OlderMetadata { role: Timestamp, current_version: 2,
///    new_version: 1 }`. The timestamp guard fires BEFORE the snapshot is even
///    fetched, so the refusal is attributed to Timestamp. We assert the exact
///    variant + role + both version numbers.
/// 4. A GENUINELY FRESH datastore (no persisted timestamp) accepts the identical
///    old valid v1 repo, because with no persisted `timestamp.json` the
///    rollback guard's `if let Some(..)` is not entered — proving cross-run
///    rollback protection depends entirely on persisted state, not on the
///    metadata itself.
///
/// No bespoke cryptography and no direct signing: both versions are built by
/// `RepoBuilder` (real `RepositoryEditor` + `LocalKeySource`) and loaded by
/// `Verifier` (real `RepositoryLoader` over `FilesystemTransport`).
#[tokio::test]
async fn cross_run_rollback_refuses_older_timestamp_via_persisted_datastore() {
    let tmp = TempDir::new().unwrap();
    // ONE shared served repository directory -> the metadata URLs never change
    // between the two publishes.
    let repo_dir = tmp.path().join("repo");
    // ONE pre-created persistent datastore, reused across both loads (this is
    // the state that makes rollback protection cross-run).
    let datastore = tmp.path().join("datastore");
    std::fs::create_dir_all(&datastore).unwrap();

    // A single Ed25519 key authorized for all four roles in BOTH versions, so
    // v1 and v2 share the same trusted root (root version 1) and the same role
    // keys — exactly the "same role keys / trusted root" the brief requires.
    let key = SignKey::generate();
    let spec = RootSpec::single(key.clone(), 1, hours_from_now(24 * 30));

    // 1. Publish + load version 2 (targets/snapshot/timestamp = 2; root = 1).
    let repo_v2 = RepoBuilder::new(repo_dir.clone(), spec.clone())
        .targets_version(2)
        .snapshot_version(2)
        .timestamp_version(2)
        .target("hello.txt", b"hello world\n".to_vec())
        .write()
        .await;
    // The verifier is pinned to the trusted root (root v1, same keys). It is
    // reused for every load below because the trusted root never changes.
    let verifier = Verifier::new(
        repo_v2.root_bytes.clone(),
        repo_v2.metadata_url(),
        repo_v2.targets_url(),
    );
    verifier
        .load(&datastore)
        .await
        .expect("version-2 repo loads on the first run");

    // The served AND persisted timestamp are both version 2 — this is what the
    // rollback guard will compare against on the next load.
    let served_timestamp = repo_dir.join("metadata").join("timestamp.json");
    let persisted_timestamp = datastore.join("timestamp.json");
    assert_eq!(
        role_version_on_disk(&served_timestamp),
        2,
        "served timestamp.json is version 2"
    );
    assert!(
        persisted_timestamp.is_file(),
        "timestamp.json was persisted to the datastore during the first load"
    );
    assert_eq!(
        role_version_on_disk(&persisted_timestamp),
        2,
        "persisted datastore timestamp is version 2"
    );

    // 2. Republish a validly signed version-1 set of metadata at the SAME URLs,
    //    using the SAME role keys and the SAME trusted root. Building into the
    //    same `repo_dir` overwrites the (unversioned) `timestamp.json` with
    //    version 1 and writes the version-1 `1.snapshot.json` / `1.targets.json`
    //    alongside the leftover version-2 files (which the v1 timestamp never
    //    references). Root stays version 1.
    let _repo_v1 = RepoBuilder::new(repo_dir.clone(), spec.clone())
        .targets_version(1)
        .snapshot_version(1)
        .timestamp_version(1)
        .target("hello.txt", b"hello world\n".to_vec())
        .write()
        .await;
    assert_eq!(
        role_version_on_disk(&served_timestamp),
        1,
        "served timestamp.json is now version 1 (republished at the same URL)"
    );
    // The persisted datastore copy is STILL version 2 (republishing does not
    // touch the datastore) — this version skew is the rollback signal.
    assert_eq!(
        role_version_on_disk(&persisted_timestamp),
        2,
        "persisted datastore timestamp is still version 2"
    );

    // 3. Second load with the SAME datastore path -> REFUSED as a rollback.
    let err = verifier
        .load(&datastore)
        .await
        .expect_err("older timestamp must be refused as a rollback");
    match &err {
        ToughError::OlderMetadata {
            role,
            current_version,
            new_version,
            ..
        } => {
            assert_eq!(
                *role,
                RoleType::Timestamp,
                "rollback refusal names the Timestamp role (its guard fires first)"
            );
            assert_eq!(
                *current_version, 2,
                "current_version is the persisted datastore timestamp (2)"
            );
            assert_eq!(
                *new_version, 1,
                "new_version is the freshly fetched timestamp (1)"
            );
        }
        other => panic!("expected OlderMetadata(Timestamp), got {:?}", other),
    }

    // 4. A GENUINELY FRESH datastore (no persisted timestamp) accepts the
    //    identical old valid v1 repo: with no persisted `timestamp.json` the
    //    rollback guard is not entered, so nothing compares versions and the
    //    valid v1 metadata loads cleanly. This proves cross-run rollback
    //    protection depends on persisted state, not on the metadata alone.
    let fresh_datastore = tmp.path().join("fresh_datastore");
    std::fs::create_dir_all(&fresh_datastore).unwrap();
    verifier
        .load(&fresh_datastore)
        .await
        .expect("a fresh datastore accepts the old valid v1 repo");
    // And that fresh load accepted and persisted version 1 (proving it really
    // loaded the old repo, not some cached v2).
    assert_eq!(
        role_version_on_disk(&fresh_datastore.join("timestamp.json")),
        1,
        "fresh load accepted and persisted version 1"
    );
}

// ===========================================================================
// (6) MIX-AND-MATCH: spliced older snapshot refused for a hash mismatch
// ===========================================================================

/// MIX-AND-MATCH defense (TUF step 3.1): the snapshot a client fetches MUST be
/// the exact bytes whose sha256 + length the trusted `timestamp.json` pinned —
/// even if those bytes are themselves a validly signed snapshot for a DIFFERENT
/// (older) repository. We prove this with a cryptographically valid signed
/// mismatch: NOT corrupted JSON, NOT a bad signature, NOT a missing file.
///
/// 1. Build two INDEPENDENTLY valid signed repositories sharing the same trusted
///    role keys (same root version 1, same key authorized for every role):
///    Repo A (old): targets/snapshot/timestamp version 1, a single top-level
///      target, NO delegated role — so A's `snapshot.json` carries exactly one
///      meta entry (`targets.json`).
///    Repo B (new): targets/snapshot/timestamp version 2, a top-level target AND
///      one delegated role — so B's `snapshot.json` carries TWO meta entries
///      (`targets.json` + `extra.json`) and is strictly LARGER than A's.
///    Both load successfully through `Verifier` (fresh datastores) — proving
///    each chain is independently valid and accepted by tough (in particular,
///    A's snapshot is a validly signed snapshot that tough accepts on its own).
/// 2. SPLICE: overwrite B's EXISTING `2.snapshot.json` — the exact path B's v2
///    timestamp requests (consistent-snapshot prefixes the snapshot version) —
///    with the bytes of A's validly signed v1 `snapshot.json`. The served path
///    remains present (no request can 404); the bytes are valid JSON validly
///    signed by the shared snapshot key.
/// 3. Loading the spliced B repo with a FRESH datastore is REFUSED. tough's
///    `load_snapshot` fetches `2.snapshot.json` through `fetch_sha256`, which
///    wraps the stream in `max_size_adapter` (cap = the length B's timestamp
///    declares) then `DigestAdapter` (hash = the sha256 B's timestamp declares).
///    Because A's snapshot is no larger than B's declared length, the size cap
///    never trips; at end-of-stream the `DigestAdapter` finds A's sha256 !=
///    B's expected sha256 and emits a `HashMismatch`. The downstream
///    `VersionMismatch` check (which runs only AFTER a successful hash check) is
///    therefore never reached. The error surfaces as `tough::Error::Transport`
///    wrapping a `TransportError` (kind `Other`) whose cause is
///    `tough::Error::HashMismatch { calculated: <A's snapshot sha256>,
///    expected: <B's snapshot sha256> }`. We assert the exact nesting, the kind,
///    and that `calculated`/`expected` equal the independently computed sha256
///    of A's and B's snapshots respectively.
///
/// This pins the cryptographic binding between timestamp and snapshot; a future
/// tough regression that fetched the snapshot by path alone (ignoring the pinned
/// hash/length) would turn this refusal into an accept.
#[tokio::test]
async fn mix_and_match_spliced_older_snapshot_refused_for_hash_mismatch() {
    use pkg_spike_s2_tough::repo::sha256_hex;

    // Shared trusted role keys: the SAME Ed25519 key is authorized for every
    // role in BOTH repositories, so the two chains share a compatible trusted
    // root (root version 1, identical key set).
    let key = SignKey::generate();
    let spec = RootSpec::single(key.clone(), 1, hours_from_now(24 * 30));

    // --- Repo A (old): single top-level target, NO delegation. -------------
    // Its snapshot.json carries exactly ONE meta entry (targets.json).
    let tmp_a = TempDir::new().unwrap();
    let dir_a = tmp_a.path().join("repo");
    let repo_a = RepoBuilder::new(dir_a.clone(), spec.clone())
        .targets_version(1)
        .snapshot_version(1)
        .timestamp_version(1)
        .target("old.txt", b"old signed content\n".to_vec())
        .write()
        .await;

    // --- Repo B (new): top-level target AND one delegated role. -----------
    // Its snapshot.json carries TWO meta entries (targets.json + extra.json),
    // so B's snapshot.json is strictly LARGER than A's. This size margin is what
    // makes the HASH mismatch (not the size cap) fire on the splice below.
    let tmp_b = TempDir::new().unwrap();
    let dir_b = tmp_b.path().join("repo");
    let repo_b = RepoBuilder::new(dir_b.clone(), spec.clone())
        .targets_version(2)
        .snapshot_version(2)
        .timestamp_version(2)
        .target("new.txt", b"new signed content here\n".to_vec())
        .delegated_role(DelegationSpec {
            role_name: "extra".to_string(),
            key: key.clone(),
            paths: vec!["extra/*".to_string()],
            targets: vec![(
                "extra/x.txt".to_string(),
                b"extra delegated bytes\n".to_vec(),
            )],
        })
        .write()
        .await;

    // (a) BOTH repos are INDEPENDENTLY VALID and load through tough's full
    //     client verification — proving each signed chain is accepted on its own
    //     (in particular, A's snapshot is a validly signed snapshot that tough
    //     accepts, and so is B's).
    {
        let ds = tmp_a.path().join("ds_a");
        std::fs::create_dir_all(&ds).unwrap();
        Verifier::new(
            repo_a.root_bytes.clone(),
            repo_a.metadata_url(),
            repo_a.targets_url(),
        )
        .load(&ds)
        .await
        .expect("Repo A (old) loads independently");
    }
    {
        let ds = tmp_b.path().join("ds_b");
        std::fs::create_dir_all(&ds).unwrap();
        Verifier::new(
            repo_b.root_bytes.clone(),
            repo_b.metadata_url(),
            repo_b.targets_url(),
        )
        .load(&ds)
        .await
        .expect("Repo B (new) loads independently");
    }

    // Capture the two snapshots and their sha256 BEFORE splicing.
    let a_snapshot_path = dir_a.join("metadata").join("1.snapshot.json");
    let b_snapshot_path = dir_b.join("metadata").join("2.snapshot.json");
    assert!(
        a_snapshot_path.is_file(),
        "A's 1.snapshot.json exists (the source snapshot)"
    );
    assert!(
        b_snapshot_path.is_file(),
        "B's 2.snapshot.json exists — the EXACT path B's timestamp requests"
    );
    let a_snapshot = std::fs::read(&a_snapshot_path).unwrap();
    let b_snapshot = std::fs::read(&b_snapshot_path).unwrap();
    let a_hash = sha256_hex(&a_snapshot);
    let b_hash = sha256_hex(&b_snapshot);
    assert_ne!(
        a_hash, b_hash,
        "A's and B's snapshots are genuinely different signed content"
    );
    // The size condition that makes the HASH (not the size cap) fire: A's
    // served snapshot length must not EXCEED the length B's timestamp declares
    // (which equals B's snapshot length). `max_size_adapter` trips only on a
    // STRICT `size > max_size`, so `<=` is the precise correctness condition.
    // A has one meta entry; B has two, so the margin is ~one meta entry.
    assert!(
        (a_snapshot.len() as u64) <= (b_snapshot.len() as u64),
        "A's snapshot ({} B) must not exceed B's declared snapshot length ({} B) \
         so the size cap does not trip and the hash mismatch fires",
        a_snapshot.len(),
        b_snapshot.len()
    );

    // (b) SPLICE: overwrite B's EXISTING 2.snapshot.json with A's validly
    //     signed v1 snapshot bytes. The served path stays present (no 404); the
    //     bytes are valid JSON validly signed by the shared snapshot key.
    std::fs::write(&b_snapshot_path, &a_snapshot).unwrap();
    assert!(
        b_snapshot_path.is_file(),
        "the spliced served path 2.snapshot.json exists (no request can 404)"
    );
    assert_eq!(
        sha256_hex(&std::fs::read(&b_snapshot_path).unwrap()),
        a_hash,
        "2.snapshot.json now holds A's snapshot bytes"
    );

    // (c) Loading the spliced B repo with a FRESH datastore is REFUSED for a
    //     concrete snapshot hash mismatch.
    let ds_spliced = tmp_b.path().join("ds_spliced");
    std::fs::create_dir_all(&ds_spliced).unwrap();
    let err = Verifier::new(
        repo_b.root_bytes.clone(),
        repo_b.metadata_url(),
        repo_b.targets_url(),
    )
    .load(&ds_spliced)
    .await
    .expect_err("the spliced snapshot must be refused");

    // tough surfaces the snapshot fetch failure as a Transport error.
    let transport = match &err {
        ToughError::Transport { source, .. } => source,
        other => panic!(
            "expected Transport error for spliced snapshot, got {:?}",
            other
        ),
    };
    assert_eq!(
        transport.kind(),
        tough::TransportErrorKind::Other,
        "hash-mismatch refusal is a generic transport failure"
    );
    // The concrete cause is HashMismatch: the served bytes hash to A's snapshot
    // sha256 (`calculated`), but B's timestamp pinned B's snapshot sha256
    // (`expected`).
    let cause = std::error::Error::source(transport)
        .expect("transport error has a cause")
        .downcast_ref::<ToughError>()
        .expect("cause is a tough::Error");
    match cause {
        ToughError::HashMismatch {
            calculated,
            expected,
            ..
        } => {
            assert_eq!(
                *calculated, a_hash,
                "calculated hash is A's (spliced) snapshot sha256"
            );
            assert_eq!(
                *expected, b_hash,
                "expected hash is B's original snapshot sha256, pinned by B's timestamp"
            );
        }
        other => panic!("expected HashMismatch cause, got {:?}", other),
    }
}

// ===========================================================================
// (7) ROOT ROTATION N -> N+1: dual authorization threshold checks
// ===========================================================================
//
// Root-version rotation (TUF 5.3.4) requires DUAL authorization: version N+1
// of `root.json` MUST be signed by (1) a threshold of keys authorized in the
// TRUSTED root (version N), AND (2) a threshold of keys authorized in the NEW
// root (version N+1) itself. tough's `load_root` runs BOTH checks against the
// fetched `new_root`, IN THAT ORDER, each via `Root::verify_role`, wrapping any
// unmet threshold in `tough::Error::VerifyMetadata { role: Root }`. The exact
// call sites (tough 0.24.0 src/lib.rs::load_root, step 5.3.4) are:
//
//     root.signed.verify_role(&new_root)        // (1) old/trusted root role
//         .context(VerifyMetadataSnafu { role: RoleType::Root })?;
//     new_root.signed.verify_role(&new_root)    // (2) new root self role
//         .context(VerifyMetadataSnafu { role: RoleType::Root })?;
//
// so check (1) (the OLD root authorizing the new one) runs BEFORE check (2)
// (the NEW root authorizing itself).

/// The on-wire signature/key membership of a `root.json`, parsed test-only
/// (NO signature verification, NO canonicalization): the root role's authorized
/// keyids, the root `keys`-map keyids, and the keyids of the attached
/// signatures. Used to state the key-membership facts that DISAMBIGUATE which
/// root-rotation threshold failed — the two failure modes below produce
/// IDENTICAL `SignatureThreshold` numbers (threshold 1, valid 0), so the
/// disambiguation is structural (which key is in which role), not numeric.
struct RootMembership {
    version: u64,
    root_role_keyids: HashSet<String>,
    keys_map_keyids: HashSet<String>,
    sig_keyids: HashSet<String>,
}

/// Parse `root.json` bytes into a `RootMembership`. Reads only the JSON the
/// publisher wrote; it parses NO signatures and performs NO verification.
fn root_membership(bytes: &[u8]) -> RootMembership {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).unwrap_or_else(|e| panic!("parse root.json: {e}"));
    let version = v["signed"]["version"]
        .as_u64()
        .expect("root.json carries signed.version");
    let root_role_keyids = v["signed"]["roles"]["root"]["keyids"]
        .as_array()
        .expect("root role has a keyids array")
        .iter()
        .map(|x| {
            x.as_str()
                .expect("root role keyid is a hex string")
                .to_ascii_lowercase()
        })
        .collect();
    let keys_map_keyids = v["signed"]["keys"]
        .as_object()
        .expect("root has a keys object")
        .keys()
        .map(|k| k.to_ascii_lowercase())
        .collect();
    let sig_keyids = v["signatures"]
        .as_array()
        .expect("root.json has a signatures array")
        .iter()
        .map(|s| {
            s["keyid"]
                .as_str()
                .expect("signature carries a hex keyid")
                .to_ascii_lowercase()
        })
        .collect();
    RootMembership {
        version,
        root_role_keyids,
        keys_map_keyids,
        sig_keyids,
    }
}

/// Match `tough::Error::VerifyMetadata { role: Root }` wrapping
/// `tough::schema::Error::SignatureThreshold { role: Root, threshold, valid }`
/// with the EXACT threshold + valid counts. This is the precise shape tough's
/// `load_root` emits when EITHER the old-root or the new-root-self signature
/// threshold is unmet during a root rotation (both call sites wrap with
/// `role: RoleType::Root`).
fn assert_root_verify_threshold(err: &ToughError, threshold: u64, valid: u64) {
    match err {
        ToughError::VerifyMetadata { role, source, .. } => {
            assert_eq!(
                *role,
                RoleType::Root,
                "root-rotation refusal names the Root role"
            );
            match source {
                SchemaError::SignatureThreshold {
                    role,
                    threshold: t,
                    valid: v,
                    ..
                } => {
                    assert_eq!(*role, RoleType::Root);
                    assert_eq!(*t, threshold, "exact enforced threshold");
                    assert_eq!(*v, valid, "exact count of valid signatures");
                }
                other => panic!(
                    "expected SignatureThreshold inside VerifyMetadata, got {:?}",
                    other
                ),
            }
        }
        other => panic!("expected VerifyMetadata(Root), got {:?}", other),
    }
}

/// Create a fresh persistent datastore directory under `base/name` (each
/// root-rotation scenario below gets its OWN datastore so a refusal is never
/// confounded by a prior load's persisted metadata).
fn fresh_datastore(base: &Path, name: &str) -> PathBuf {
    let p = base.join(name);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// True root version N -> N+1 dual authorization. We construct root v1 with a
/// DISTINCT old root key (1-of-1) and a SEPARATE operational key for targets /
/// snapshot / timestamp, and root v2 that REMOVES the old root key entirely and
/// authorizes a DISTINCT new root key (1-of-1) while RETAINING the same
/// operational keys (so v2's published targets/snapshot/timestamp still verify
/// under v2). We serve current v2 targets/snapshot/timestamp (signed by
/// `RepositoryEditor` over the operational key) and exercise THREE fresh
/// datastores against the SAME pinned v1 root and a served `2.root.json` whose
/// signature set we control through the existing publisher boundary
/// (`build_root` assembles the roles + key map; `sign_role` re-signs the SAME
/// root payload through `tough::sign::Sign`; `root_json_bytes` serializes):
///
///   A. v2 signed ONLY by the new key -> fails check (1), the OLD-root
///      threshold: v1's root role {old} does not contain the new key, so
///      `Root::verify_role` counts 0 valid signatures against threshold 1.
///   B. v2 signed ONLY by the old key -> PASSES check (1) (old key is in v1's
///      root role, valid 1 >= 1) but fails check (2), the NEW-root SELF
///      threshold: v2's root role {new} does not contain the old key, so valid
///      0 < 1.
///   C. v2 signed by BOTH old and new -> passes both checks and LOADS, with the
///      target reading back byte-for-byte.
///
/// Checks A and B yield IDENTICAL `SignatureThreshold` numbers (threshold 1,
/// valid 0), so we additionally assert the key-membership facts that
/// disambiguate which one failed: v1's root role authorizes ONLY the old key,
/// v2's root role authorizes ONLY the new key, the old key is ABSENT from v2's
/// key map, and the served `2.root.json`'s attached signature keyid(s) are
/// exactly {new} for A and {old} for B (so A's failure is the old-root check —
/// the only signature present is not in v1's role — and B's is the new-root
/// self check — the only signature present is in v1's role but not v2's).
///
/// No bespoke cryptography and no direct signing: every `2.root.json` variant
/// is assembled by `build_root` and re-signed through `tough::sign::Sign` via
/// `sign_role`; the operational metadata is signed by `RepositoryEditor` +
/// `LocalKeySource`; all verification is `tough::RepositoryLoader`. All PKCS#8
/// key material lives only inside the test's `TempDir`.
#[tokio::test]
async fn root_rotation_dual_authorization_threshold_checks() {
    let old_root_key = SignKey::generate();
    let new_root_key = SignKey::generate();
    let op_key = SignKey::generate();
    let old_hex = old_root_key.key_id_hex();
    let new_hex = new_root_key.key_id_hex();

    // v1: root role = {old_root_key} (1-of-1); operational roles = {op_key}.
    let v1_spec = RootSpec {
        root: RoleSpec::single(old_root_key.clone()),
        targets: RoleSpec::single(op_key.clone()),
        snapshot: RoleSpec::single(op_key.clone()),
        timestamp: RoleSpec::single(op_key.clone()),
        consistent_snapshot: true,
        version: 1,
        expires: hours_from_now(24 * 30),
    };
    let pinned_v1 = root_json_bytes(&build_root(&v1_spec).await);

    // v2: root role = {new_root_key} (1-of-1) — the OLD root key is REMOVED;
    // operational roles = {op_key} RETAINED (so v2's published metadata still
    // verifies under v2).
    let v2_spec = RootSpec {
        root: RoleSpec::single(new_root_key.clone()),
        targets: RoleSpec::single(op_key.clone()),
        snapshot: RoleSpec::single(op_key.clone()),
        timestamp: RoleSpec::single(op_key.clone()),
        consistent_snapshot: true,
        version: 2,
        expires: hours_from_now(24 * 30),
    };

    // Build the served repository from v2: writes `2.root.json` (signed by the
    // new key via `build_root`) plus current targets/snapshot/timestamp signed
    // by the operational key through `RepositoryEditor`. We OVERWRITE
    // 2.root.json per case below with the exact signature set under test.
    let tmp = TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    let repo = RepoBuilder::new(repo_dir.clone(), v2_spec.clone())
        .target("hello.txt", b"hello world\n".to_vec())
        .write()
        .await;
    let metadata_dir = repo.metadata_dir.clone();
    let verifier = Verifier::new(pinned_v1.clone(), repo.metadata_url(), repo.targets_url());

    // The v2 Root VALUE (assembled roles + key map, carrying NO old key). It is
    // re-signed through `sign_role` (tough::sign::Sign) to produce each on-wire
    // signature set. This is the narrow test-publisher boundary; there is no
    // direct cryptographic call and no TUF-lite verification anywhere.
    let v2_root_value = build_root(&v2_spec).await.signed;
    let root2_path = metadata_dir.join("2.root.json");

    // --- key-membership facts (disambiguate the two failure modes) -----------
    // v1's root role authorizes ONLY the old key; v2's root role authorizes
    // ONLY the new key; the old key is present in v1's key map and ABSENT from
    // v2's key map (v2 removed it).
    let m_v1 = root_membership(&pinned_v1);
    assert_eq!(m_v1.version, 1, "pinned trusted root is version 1");
    assert_eq!(
        m_v1.root_role_keyids,
        std::iter::once(old_hex.clone()).collect::<HashSet<_>>(),
        "v1 root role authorizes ONLY the old key"
    );
    assert!(m_v1.keys_map_keyids.contains(&old_hex));
    assert!(!m_v1.keys_map_keyids.contains(&new_hex));

    let m_v2 = root_membership(&std::fs::read(&root2_path).unwrap());
    assert_eq!(m_v2.version, 2, "served root is version 2");
    assert_eq!(
        m_v2.root_role_keyids,
        std::iter::once(new_hex.clone()).collect::<HashSet<_>>(),
        "v2 root role authorizes ONLY the new key (old key removed)"
    );
    assert!(m_v2.keys_map_keyids.contains(&new_hex));
    assert!(
        !m_v2.keys_map_keyids.contains(&old_hex),
        "v2 key map no longer contains the old root key"
    );

    // (A) v2 signed ONLY by the new key -> fails the OLD-root threshold (1):
    //     the only signature present (new key) is NOT in v1's root role {old}.
    {
        let new_only = sign_role(v2_root_value.clone(), &[&new_root_key]).await;
        let bytes = root_json_bytes(&new_only);
        std::fs::write(&root2_path, &bytes).unwrap();
        let m = root_membership(&bytes);
        assert_eq!(
            m.sig_keyids,
            std::iter::once(new_hex.clone()).collect::<HashSet<_>>(),
            "served 2.root.json carries only the new-key signature"
        );
        assert!(
            !m_v1.root_role_keyids.contains(&new_hex),
            "disambiguating fact: the new key is NOT in v1's root role, so the \
             OLD-root check (1) is the one that failed with valid 0"
        );
        let ds = fresh_datastore(tmp.path(), "ds_new_only");
        let err = verifier
            .load(&ds)
            .await
            .expect_err("v2 signed only by new must fail the old-root threshold");
        assert_root_verify_threshold(&err, 1, 0);
    }

    // (B) v2 signed ONLY by the old key -> passes old-root threshold (the old
    //     key IS in v1's role), fails the NEW-root SELF threshold (the old key
    //     is NOT in v2's role {new}).
    {
        let old_only = sign_role(v2_root_value.clone(), &[&old_root_key]).await;
        let bytes = root_json_bytes(&old_only);
        std::fs::write(&root2_path, &bytes).unwrap();
        let m = root_membership(&bytes);
        assert_eq!(
            m.sig_keyids,
            std::iter::once(old_hex.clone()).collect::<HashSet<_>>(),
            "served 2.root.json carries only the old-key signature"
        );
        assert!(
            m_v1.root_role_keyids.contains(&old_hex),
            "disambiguating fact: the old key IS in v1's root role, so the \
             OLD-root check (1) PASSED (valid 1)"
        );
        assert!(
            !m_v2.root_role_keyids.contains(&old_hex),
            "disambiguating fact: the old key is NOT in v2's root role {{new}}, so \
             the NEW-root SELF check (2) is the one that failed with valid 0"
        );
        let ds = fresh_datastore(tmp.path(), "ds_old_only");
        let err = verifier
            .load(&ds)
            .await
            .expect_err("v2 signed only by old must fail the new-root self threshold");
        assert_root_verify_threshold(&err, 1, 0);
    }

    // (C) v2 signed by BOTH old and new -> dual-authorized, loads, target reads.
    {
        let both = sign_role(v2_root_value.clone(), &[&old_root_key, &new_root_key]).await;
        let bytes = root_json_bytes(&both);
        std::fs::write(&root2_path, &bytes).unwrap();
        let m = root_membership(&bytes);
        assert_eq!(
            m.sig_keyids,
            [old_hex.clone(), new_hex.clone()]
                .into_iter()
                .collect::<HashSet<_>>(),
            "served 2.root.json carries both the old- and new-key signatures"
        );
        let ds = fresh_datastore(tmp.path(), "ds_both");
        let loaded = verifier
            .load(&ds)
            .await
            .expect("v2 dual-signed by old+new loads through tough's full verification");
        let got = read_target_fully(&loaded, &TargetName::new("hello.txt").unwrap())
            .await
            .expect("read hello.txt")
            .expect("hello.txt present");
        assert_eq!(got, b"hello world\n");
    }
}

// ===========================================================================
// (8) ACTUAL REVOCATION: revoked old root key can no longer sign a new root
// ===========================================================================
//
// After a VALID dual-signed v1 -> v2 rotation, the old root key is fully
// REVOKED: root v3's root role still authorizes ONLY the new key, and v3's key
// map does NOT contain the old key. A correctly new-key-signed v3 LOADS through
// the v1 -> v2 -> v3 chain. But the SAME v3 payload signed ONLY by the revoked
// old key is REJECTED: at the v2 -> v3 step tough runs `v2.verify_role(v3)`
// (check (1) for that hop), v2's root role {new} does not authorize the old
// key, so `Root::verify_role` counts 0 valid signatures against threshold 1 ->
// `tough::Error::VerifyMetadata { role: Root }` wrapping
// `tough::schema::Error::SignatureThreshold { role: Root, threshold: 1, valid: 0 }`.
//
// This is a REAL revocation test, NOT a missing-signature test: the rejected
// `3.root.json` CARRIES exactly one signature whose keyid NAMES the old
// (revoked) key, and the old key is provably ABSENT from v2's and v3's key maps
// and root role keyids. We assert both. The root update files are the real
// existing `2.root.json` and `3.root.json` served on disk; the current targets /
// snapshot / timestamp remain validly published by `RepositoryEditor` over the
// retained operational key (unchanged across v1/v2/v3, so the same published
// metadata verifies under every root in the chain).
#[tokio::test]
async fn root_rotation_revocation_rejects_signed_by_revoked_key() {
    let old_root_key = SignKey::generate();
    let new_root_key = SignKey::generate();
    let op_key = SignKey::generate();
    let old_hex = old_root_key.key_id_hex();
    let new_hex = new_root_key.key_id_hex();

    // v1: root role = {old_root_key}; operational roles = {op_key}.
    let v1_spec = RootSpec {
        root: RoleSpec::single(old_root_key.clone()),
        targets: RoleSpec::single(op_key.clone()),
        snapshot: RoleSpec::single(op_key.clone()),
        timestamp: RoleSpec::single(op_key.clone()),
        consistent_snapshot: true,
        version: 1,
        expires: hours_from_now(24 * 30),
    };
    let pinned_v1 = root_json_bytes(&build_root(&v1_spec).await);

    // v2 and v3: root role = {new_root_key}; the OLD root key is REVOKED
    // (absent from both the root role and the key map). Operational roles =
    // {op_key} retained so the SAME published metadata verifies under each.
    let v2_spec = RootSpec {
        root: RoleSpec::single(new_root_key.clone()),
        targets: RoleSpec::single(op_key.clone()),
        snapshot: RoleSpec::single(op_key.clone()),
        timestamp: RoleSpec::single(op_key.clone()),
        consistent_snapshot: true,
        version: 2,
        expires: hours_from_now(24 * 30),
    };
    let v3_spec = RootSpec {
        root: RoleSpec::single(new_root_key.clone()),
        targets: RoleSpec::single(op_key.clone()),
        snapshot: RoleSpec::single(op_key.clone()),
        timestamp: RoleSpec::single(op_key.clone()),
        consistent_snapshot: true,
        version: 3,
        expires: hours_from_now(24 * 30),
    };

    // Served repo built from v2: `2.root.json` (new-signed) + current
    // operational metadata. We then OVERWRITE 2.root.json with the DUAL-signed
    // form so the v1 -> v2 hop passes, and ADD a real `3.root.json`.
    let tmp = TempDir::new().unwrap();
    let repo_dir = tmp.path().join("repo");
    let repo = RepoBuilder::new(repo_dir.clone(), v2_spec.clone())
        .target("hello.txt", b"hello world\n".to_vec())
        .write()
        .await;
    let metadata_dir = repo.metadata_dir.clone();
    let verifier = Verifier::new(pinned_v1.clone(), repo.metadata_url(), repo.targets_url());

    // Dual-signed `2.root.json` (old + new) so the v1 -> v2 rotation succeeds:
    // check (1) passes (old key in v1's role), check (2) passes (new key in
    // v2's role).
    let v2_root_value = build_root(&v2_spec).await.signed;
    let v2_dual = sign_role(v2_root_value, &[&old_root_key, &new_root_key]).await;
    let root2_path = metadata_dir.join("2.root.json");
    std::fs::write(&root2_path, root_json_bytes(&v2_dual)).unwrap();
    assert!(root2_path.is_file(), "real existing 2.root.json is served");

    // The v3 Root VALUE: root role {new}, key map excludes the old key. Built
    // once, then signed two ways (good: new key; bad: revoked old key).
    let v3_root_value = build_root(&v3_spec).await.signed;
    let v3_good = sign_role(v3_root_value.clone(), &[&new_root_key]).await;
    let v3_good_bytes = root_json_bytes(&v3_good);
    let root3_path = metadata_dir.join("3.root.json");

    // --- revocation facts: the old key is ABSENT from v2 AND v3 key maps +
    //     root role keyids, while the new key remains authorized everywhere. --
    let m_v2 = root_membership(&std::fs::read(&root2_path).unwrap());
    assert_eq!(m_v2.version, 2);
    let m_v3 = root_membership(&v3_good_bytes);
    assert_eq!(m_v3.version, 3);
    for (label, m) in [("v2", &m_v2), ("v3", &m_v3)] {
        assert!(
            !m.keys_map_keyids.contains(&old_hex),
            "{label} key map must NOT contain the old (revoked) key"
        );
        assert!(
            !m.root_role_keyids.contains(&old_hex),
            "{label} root role must NOT authorize the old (revoked) key"
        );
        assert!(
            m.root_role_keyids.contains(&new_hex),
            "{label} root role still authorizes the new key"
        );
    }

    // Phase A: a correctly new-key-signed v3 LOADS through v1 -> v2 -> v3.
    std::fs::write(&root3_path, &v3_good_bytes).unwrap();
    assert!(root3_path.is_file(), "real existing 3.root.json is served");
    assert_eq!(
        root_membership(&v3_good_bytes).sig_keyids,
        std::iter::once(new_hex.clone()).collect::<HashSet<_>>(),
        "good 3.root.json is signed by the new key"
    );
    {
        let ds = fresh_datastore(tmp.path(), "ds_good_v3");
        let loaded = verifier
            .load(&ds)
            .await
            .expect("a correctly new-key-signed v3 loads after the dual-signed v2");
        let got = read_target_fully(&loaded, &TargetName::new("hello.txt").unwrap())
            .await
            .expect("read hello.txt")
            .expect("hello.txt present");
        assert_eq!(got, b"hello world\n");
    }

    // Phase B: the SAME v3 payload signed ONLY by the revoked old key is
    // REJECTED at the v2 -> v3 hop with a Root SignatureThreshold of valid 0.
    // This is a real revocation, not a missing signature: the rejected
    // 3.root.json CARRIES exactly one signature whose keyid NAMES the old key.
    let v3_bad = sign_role(v3_root_value.clone(), &[&old_root_key]).await;
    let v3_bad_bytes = root_json_bytes(&v3_bad);
    std::fs::write(&root3_path, &v3_bad_bytes).unwrap();
    assert!(root3_path.is_file(), "real existing 3.root.json is served");
    assert_eq!(
        root_membership(&v3_bad_bytes).sig_keyids,
        std::iter::once(old_hex.clone()).collect::<HashSet<_>>(),
        "bad 3.root.json carries the OLD (revoked) key's signature — real revocation"
    );
    {
        let ds = fresh_datastore(tmp.path(), "ds_bad_v3");
        let err = verifier
            .load(&ds)
            .await
            .expect_err("v3 signed only by the revoked old key must be rejected");
        assert_root_verify_threshold(&err, 1, 0);
    }
}
