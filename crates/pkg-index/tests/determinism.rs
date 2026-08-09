use pkg_core::{ChannelSequence, NixpkgsRevision, System};
use pkg_index::{BuildMetadata, IndexCandidate, build_index, build_index_from_json};

const FIXTURE: &[u8] = include_bytes!("../../../fixtures/nixpkgs-slice-tiny/index-input.json");
const EXPECTED_DIGEST: &str = "a5e939cbaec822eddfc68119bef256ba3936f708dfb404894cf70fa10631a031";

fn build_fixture() -> pkg_index::BuiltIndex {
    let metadata = BuildMetadata::new(
        ChannelSequence::from_u64(42).unwrap(),
        System::Aarch64Darwin,
        NixpkgsRevision::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
        "2025-01-01T00:00:00Z",
    )
    .unwrap();
    build_index_from_json(metadata, FIXTURE).unwrap()
}

#[test]
fn tiny_slice_has_cross_host_stable_bytes() {
    let built = build_fixture();
    assert_eq!(built.sha256_hex(), EXPECTED_DIGEST);

    let parsed: serde_json::Value = serde_json::from_slice(built.bytes()).unwrap();
    assert_eq!(parsed["schemaVersion"], 1);
    assert_eq!(parsed["source"], "self-built");
    assert_eq!(parsed["records"].as_array().unwrap().len(), 3);
}

#[test]
fn reversing_projection_order_produces_identical_bytes() {
    let expected = build_fixture();
    let mut candidates: Vec<IndexCandidate> = serde_json::from_slice(FIXTURE).unwrap();
    candidates.reverse();
    let metadata = BuildMetadata::new(
        ChannelSequence::from_u64(42).unwrap(),
        System::Aarch64Darwin,
        NixpkgsRevision::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
        "2024-12-31T16:00:00-08:00",
    )
    .unwrap();
    let actual = build_index(metadata, candidates).unwrap();
    assert_eq!(actual.bytes(), expected.bytes());
}
