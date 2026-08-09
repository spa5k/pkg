use pkg_core::{ChannelSequence, NixpkgsRevision, System};
use pkg_index::{
    BuildMetadata, CatalogListOptions, IndexCandidate, IndexQuery, InfoLookup, SearchOptions,
    build_index, build_index_from_json,
};
use serde::Serialize;

const FIXTURE: &[u8] = include_bytes!("../../../fixtures/nixpkgs-slice-tiny/index-input.json");

fn built_fixture() -> pkg_index::BuiltIndex {
    build_index_from_json(metadata(), FIXTURE).unwrap()
}

fn metadata() -> BuildMetadata {
    BuildMetadata::new(
        ChannelSequence::from_u64(42).unwrap(),
        System::Aarch64Darwin,
        NixpkgsRevision::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
        "2025-01-01T00:00:00Z",
    )
    .unwrap()
}

fn pretty(value: &impl Serialize) -> String {
    format!("{}\n", serde_json::to_string_pretty(value).unwrap())
}

#[test]
fn search_output_matches_golden_and_hides_nix_identity_fields() {
    let built = built_fixture();
    let response = IndexQuery::new(built.document(), true)
        .search(&SearchOptions::new("rg", 25, false, None).unwrap())
        .unwrap();
    let json = pretty(&response);
    assert_eq!(
        json,
        include_str!("../../../fixtures/nixpkgs-slice-tiny/query/search-rg.json")
    );
    assert!(!json.contains("attrPath"));
    assert!(!json.contains("/nix/store/"));
}

#[test]
fn catalog_page_output_matches_golden() {
    let built = built_fixture();
    let response = IndexQuery::new(built.document(), false)
        .catalog_list(&CatalogListOptions::new(1, 2, false).unwrap())
        .unwrap();
    assert_eq!(
        pretty(&response),
        include_str!("../../../fixtures/nixpkgs-slice-tiny/query/catalog-page.json")
    );
}

#[test]
fn info_output_matches_golden_and_is_honest_about_unavailable_data() {
    let built = built_fixture();
    let response = IndexQuery::new(built.document(), false)
        .info("python3Packages.requests")
        .unwrap();
    assert!(matches!(response.lookup(), InfoLookup::Found { .. }));
    let json = pretty(&response);
    assert_eq!(
        json,
        include_str!("../../../fixtures/nixpkgs-slice-tiny/query/info-requests.json")
    );
    assert!(json.contains(r#""advisoryStatus": "unavailable""#));
    assert!(json.contains(r#""installedSizeEstimateBytes": null"#));
    assert!(!json.contains("aarch64-darwin"));
}

#[test]
fn exact_search_license_filter_and_not_found_are_stable() {
    let built = built_fixture();
    let query = IndexQuery::new(built.document(), false);
    let exact = query
        .search(&SearchOptions::new("rip", 25, true, None).unwrap())
        .unwrap();
    assert!(exact.results().is_empty());
    let licensed = query
        .search(&SearchOptions::new("ripgrep", 25, false, Some("MIT")).unwrap())
        .unwrap();
    assert_eq!(licensed.results()[0].package(), "ripgrep");
    let missing = query.info("does-not-exist").unwrap();
    assert!(matches!(missing.lookup(), InfoLookup::NotFound { .. }));
}

#[test]
fn display_name_collision_is_ambiguous_not_guessed() {
    let mut candidates: Vec<IndexCandidate> = serde_json::from_slice(FIXTURE).unwrap();
    let requests = candidates
        .iter()
        .find(|candidate| candidate.attr_path == "python3Packages.requests")
        .unwrap()
        .clone();
    candidates.push(IndexCandidate {
        attr_path: "pythonPackages.requests".into(),
        ..requests
    });
    let built = build_index(metadata(), candidates).unwrap();
    let response = IndexQuery::new(built.document(), false)
        .info("requests")
        .unwrap();
    let InfoLookup::Ambiguous { candidates } = response.lookup() else {
        panic!("display-name collision must remain ambiguous");
    };
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].package(), "python3Packages.requests");
    assert_eq!(candidates[1].package(), "pythonPackages.requests");
}
