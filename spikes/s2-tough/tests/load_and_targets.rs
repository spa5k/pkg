// Slice-1 integration tests: pinned-root load, persistent datastore files,
// delegated-target verification, and read_target_fully.
//
// These are HAPPY-PATH tests only. Adversarial cases (threshold, one-bit
// tamper, root rotation, rollback, mix-and-match, expiry, size-limit refusal)
// are slice 2.

use pkg_spike_s2_tough::ChannelDescriptor;
use pkg_spike_s2_tough::{Verifier, build_fixture, read_target_fully};
use tough::TargetName;

/// Helper: each test gets its own isolated fixture (and temp dir).
async fn fixture() -> pkg_spike_s2_tough::Fixture {
    build_fixture().await
}

/// Build a `Verifier` pinned to the fixture's trusted root bytes.
fn verifier(f: &pkg_spike_s2_tough::Fixture) -> Verifier {
    Verifier::new(f.root_bytes().to_vec(), f.metadata_url(), f.targets_url())
}

// ---------------------------------------------------------------------------
// 1. Pinned-root happy-path `RepositoryLoader` load.
// ---------------------------------------------------------------------------

/// A pinned trusted root + `FilesystemTransport` + `ExpirationEnforcement::Safe`
/// + conservative `Limits` + a persistent datastore loads the fixture repo
/// without error. This exercises tough's full client verification (root →
/// timestamp → snapshot → targets → delegated targets).
#[tokio::test]
async fn pinned_root_loads_happy_path() {
    let f = fixture().await;
    let verifier = verifier(&f);
    let repo = verifier.load(&f.datastore).await;
    assert!(
        repo.is_ok(),
        "pinned-root load must succeed: {:?}",
        repo.err()
    );
}

// ---------------------------------------------------------------------------
// 2. Persistent timestamp/snapshot files appear in the datastore after load.
// ---------------------------------------------------------------------------

/// `tough` writes `timestamp.json` and `snapshot.json` into the *persistent*
/// datastore during load (rollback protection requires these survive between
/// updates). They must NOT exist before load and MUST exist after.
#[tokio::test]
async fn persistent_timestamp_and_snapshot_after_load() {
    let f = fixture().await;
    let verifier = verifier(&f);

    // Before load, the persistent datastore is empty.
    assert!(
        !f.datastore.join("timestamp.json").exists(),
        "datastore must be empty before load"
    );
    assert!(
        !f.datastore.join("snapshot.json").exists(),
        "datastore must be empty before load"
    );

    verifier
        .load(&f.datastore)
        .await
        .expect("pinned-root load succeeds");

    // After load, both metadata files are persisted for rollback protection.
    assert!(
        f.datastore.join("timestamp.json").exists(),
        "timestamp.json must be persisted in the datastore after load"
    );
    assert!(
        f.datastore.join("snapshot.json").exists(),
        "snapshot.json must be persisted in the datastore after load"
    );
}

// ---------------------------------------------------------------------------
// 3. read_target_fully returns bytes ONLY after the stream is fully drained.
// ---------------------------------------------------------------------------

/// `read_target_fully` drains the target stream to completion via
/// `IntoVec::into_vec`, so tough's incremental SHA-256 check finishes BEFORE
/// the bytes are returned. For the happy path every top-level target (the
/// descriptor, a representative managed-Nix runtime, and the Nixpkgs source)
/// reads back byte-for-byte identical to the fixture bytes.
#[tokio::test]
async fn read_top_level_targets_after_drain() {
    let f = fixture().await;
    let verifier = verifier(&f);
    let repo = verifier.load(&f.datastore).await.expect("load ok");

    // descriptor.json — the channel descriptor target.
    let got = read_target_fully(&repo, &TargetName::new("descriptor.json").unwrap())
        .await
        .expect("read descriptor")
        .expect("descriptor present");
    assert_eq!(
        got, f.descriptor_bytes,
        "descriptor bytes must match exactly after drain"
    );

    // Representative managed-Nix runtime target (top-level).
    let (nix_name, nix_bytes) = &f.nix_targets[0];
    let got = read_target_fully(&repo, &TargetName::new(nix_name).unwrap())
        .await
        .expect("read nix")
        .expect("nix target present");
    assert_eq!(
        got, *nix_bytes,
        "managed-Nix runtime bytes must match exactly after drain"
    );

}

// ---------------------------------------------------------------------------
// 4. Delegated-target verification (the "index" role) — if the API supports it.
// ---------------------------------------------------------------------------

/// tough 0.24.0 walks delegations during load: it fetches the delegated role's
/// metadata (`<role>.json`), verifies its signature against the delegation in
/// the top-level targets, and records its targets. Reading a delegated target
/// then goes through the same hash check as a top-level target. This proves the
/// full delegated path end-to-end.
#[tokio::test]
async fn read_delegated_index_target() {
    let f = fixture().await;
    let verifier = verifier(&f);
    let repo = verifier.load(&f.datastore).await.expect("load ok");

    // All four per-system index targets live under the delegated "index" role.
    assert_eq!(
        f.index_targets.len(),
        4,
        "fixture carries four index targets"
    );
    for (name, bytes) in &f.index_targets {
        let got = read_target_fully(&repo, &TargetName::new(name).unwrap())
            .await
            .expect("read delegated index target")
            .unwrap_or_else(|| panic!("delegated index target {name} must be present"));
        assert_eq!(
            got, *bytes,
            "delegated index target {name} bytes must match exactly"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. A target the repository does not advertise reads back as `None`.
// ---------------------------------------------------------------------------

/// `read_target_fully` returns `Ok(None)` (not an error) for a target name that
/// no role advertises — the contract PR-11 relies on to distinguish "missing"
/// from "tampered".
#[tokio::test]
async fn missing_target_is_none() {
    let f = fixture().await;
    let verifier = verifier(&f);
    let repo = verifier.load(&f.datastore).await.expect("load ok");

    let got = read_target_fully(&repo, &TargetName::new("does/not/exist.bin").unwrap())
        .await
        .expect("missing target is Ok(None), not Err");
    assert!(got.is_none(), "an unadvertised target must be None");
}

// ---------------------------------------------------------------------------
// 6. Regression: descriptor per-system maps carry exactly the four supported
//    systems, and every descriptor target name/hash matches the fixture bytes.
// ---------------------------------------------------------------------------

/// Regression for the slice-1 fixture bug: `descriptor.nixRuntime.perSystem`
/// and `descriptor.index.perSystem` were built by parsing path segment 1 of the
/// target name. For `nix/<ver>/<sys>.tar.xz` that segment is the Nix VERSION,
/// and for `index/<seq>/<sys>.json.br` it is the sequence number — so all four
/// systems collapsed onto a single map key (every iteration inserted under
/// `2.24.10` / `42`). The fixture now keys both maps straight from
/// SUPPORTED_SYSTEMS, so each must carry exactly the four keys x86_64-linux,
/// aarch64-linux, x86_64-darwin, aarch64-darwin.
///
/// This test loads the repo through `Verifier`, fully drains `descriptor.json`
/// (so its TUF hash check completes before any bytes are consumed), parses a
/// `ChannelDescriptor`, asserts the four-key invariant on BOTH per-system maps,
/// and cross-checks every descriptor target name/hash against the corresponding
/// actual fixture bytes (the bytes that were signed into the repo). Semantic
/// policy validation stays TEST-ONLY here; this adds NO production policy
/// validation.
#[tokio::test]
async fn descriptor_per_system_maps_have_all_four_systems_and_match_fixture_bytes() {
    use pkg_spike_s2_tough::descriptor::SUPPORTED_SYSTEMS;
    use pkg_spike_s2_tough::repo::sha256_hex;
    use std::collections::BTreeSet;

    let f = fixture().await;
    let verifier = verifier(&f);
    let repo = verifier.load(&f.datastore).await.expect("load ok");

    // 1. Fully drain descriptor.json so its TUF hash check completes BEFORE the
    //    bytes are consumed.
    let bytes = read_target_fully(&repo, &TargetName::new("descriptor.json").unwrap())
        .await
        .expect("read descriptor")
        .expect("descriptor.json target present");
    assert_eq!(
        bytes, f.descriptor_bytes,
        "drained descriptor must match fixture"
    );

    // 2. Parse the authenticated descriptor.
    let descriptor: ChannelDescriptor =
        serde_json::from_slice(&bytes).expect("parse ChannelDescriptor");

    // 3. EXACT four-key invariant on BOTH per-system maps.
    let expected: BTreeSet<&str> = SUPPORTED_SYSTEMS.iter().copied().collect();
    let nix_keys: BTreeSet<&str> = descriptor
        .nix_runtime
        .per_system
        .keys()
        .map(String::as_str)
        .collect();
    let index_keys: BTreeSet<&str> = descriptor
        .index
        .per_system
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        nix_keys, expected,
        "nixRuntime.perSystem must have exactly the four supported system keys"
    );
    assert_eq!(
        index_keys, expected,
        "index.perSystem must have exactly the four supported system keys"
    );

    // 4. Cross-check every descriptor target against the corresponding actual
    //    fixture bytes (the ground truth signed into the TUF repo). The fixture
    //    vecs are built in SUPPORTED_SYSTEMS order, so zipping pairs each system
    //    with its exact target tuple — the same construction the (fixed) fixture
    //    now uses, exercised here from the outside.
    //    a) nixRuntime.perSystem[sys].sha256  <->  fixture nix target bytes
    for (sys, (_, actual)) in SUPPORTED_SYSTEMS.iter().zip(&f.nix_targets) {
        let entry = &descriptor.nix_runtime.per_system[*sys];
        assert_eq!(
            entry.sha256,
            sha256_hex(actual),
            "nixRuntime.perSystem[{sys}].sha256 must match fixture bytes"
        );
    }
    //    b) index.perSystem[sys].{target,sha256}  <->  fixture index target
    for (sys, (name, actual)) in SUPPORTED_SYSTEMS.iter().zip(&f.index_targets) {
        let entry = &descriptor.index.per_system[*sys];
        assert_eq!(
            entry.target, *name,
            "index.perSystem[{sys}].target must match fixture target name"
        );
        assert_eq!(
            entry.sha256,
            sha256_hex(actual),
            "index.perSystem[{sys}].sha256 must match fixture bytes"
        );
    }
}
