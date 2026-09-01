//! Tests for the `nixpkgs` module.

use std::num::NonZeroU64;
use std::sync::Mutex;

use super::*;

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const NAR_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const STORE_PATH: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-source";

fn spec() -> NixpkgsFetchSpec {
    NixpkgsFetchSpec::from_parts(
        ChannelSequence::new(NonZeroU64::new(7).unwrap()),
        PolicyVersion::new(NonZeroU64::new(3).unwrap()),
        [0x42; 32],
        NIXPKGS_OWNER,
        NIXPKGS_REPO,
        REVISION,
        NAR_HASH,
    )
    .unwrap()
}

fn metadata(revision: &str, nar_hash: &str) -> Vec<u8> {
    format!(
            r#"{{"locked":{{"type":"github","owner":"NixOS","repo":"nixpkgs","rev":"{revision}","narHash":"{nar_hash}","lastModified":1}},"path":"{STORE_PATH}","revision":"{revision}","locks":{{"version":7,"root":"root","nodes":{{"root":{{"locked":{{"rev":"ignored"}}}}}}}}}}"#
        )
        .into_bytes()
}

fn replace_ascii(input: Vec<u8>, from: &str, to: &str) -> Vec<u8> {
    String::from_utf8(input)
        .unwrap()
        .replace(from, to)
        .into_bytes()
}

struct ExactRunner {
    expected: NixpkgsPin,
    response: Mutex<Option<Result<Vec<u8>, NixpkgsSourceError>>>,
}

impl ExactRunner {
    fn new(expected: NixpkgsPin, response: Result<Vec<u8>, NixpkgsSourceError>) -> Self {
        Self {
            expected,
            response: Mutex::new(Some(response)),
        }
    }
}

impl NixpkgsMetadataRunner for ExactRunner {
    fn run_metadata(&self, pin: &NixpkgsPin) -> Result<Vec<u8>, NixpkgsSourceError> {
        if pin != &self.expected {
            return Err(NixpkgsSourceError::runner_failure());
        }
        self.response
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap_or_else(|| Err(NixpkgsSourceError::runner_failure()))
    }
}

#[test]
fn fetch_spec_exposes_only_the_authenticated_pin() {
    let spec = spec();
    assert_eq!(spec.pin().revision().as_str(), REVISION);
    assert_eq!(spec.pin().nar_hash().as_str(), NAR_HASH);
}

#[test]
fn top_level_locked_identity_promotes_a_private_source() {
    let spec = spec();
    let runner = ExactRunner::new(spec.pin().clone(), Ok(metadata(REVISION, NAR_HASH)));
    let source = fetch_verified_nixpkgs(&spec, &runner).unwrap();
    assert_eq!(source.revision().as_str(), REVISION);
    assert_eq!(source.nar_hash().as_str(), NAR_HASH);
    assert_eq!(source.private_store_path().as_str(), STORE_PATH);
    assert_eq!(source.marker_key(), REVISION);
    let debug = format!("{source:?}");
    assert!(!debug.contains(STORE_PATH));
    assert!(!debug.contains(NAR_HASH));
}

#[test]
fn release_pin_promotes_the_same_verified_private_source() {
    let pin = NixpkgsPin::new(REVISION, NAR_HASH).unwrap();
    let runner = ExactRunner::new(pin.clone(), Ok(metadata(REVISION, NAR_HASH)));

    let source = fetch_pinned_nixpkgs(&pin, &runner).unwrap();

    assert_eq!(source.revision().as_str(), REVISION);
    assert_eq!(source.nar_hash().as_str(), NAR_HASH);
    let debug = format!("{source:?}");
    assert!(!debug.contains(STORE_PATH));
    assert!(!debug.contains(NAR_HASH));
}

#[test]
fn revision_nar_hash_and_top_level_revision_mismatches_fail_closed() {
    let spec = spec();
    let other_revision = "1123456789abcdef0123456789abcdef01234567";
    let top_level_only = replace_ascii(
        metadata(REVISION, NAR_HASH),
        &format!(r#""revision":"{REVISION}""#),
        &format!(r#""revision":"{other_revision}""#),
    );
    for response in [
        metadata(other_revision, NAR_HASH),
        metadata(
            REVISION,
            "sha256-BAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        ),
        top_level_only,
    ] {
        let runner = ExactRunner::new(spec.pin().clone(), Ok(response));
        assert_eq!(
            fetch_verified_nixpkgs(&spec, &runner).unwrap_err().code(),
            NixpkgsSourceErrorCode::IdentityMismatch
        );
    }
}

#[test]
fn source_kind_owner_and_repo_mismatches_fail_closed() {
    let spec = spec();
    for response in [
        replace_ascii(metadata(REVISION, NAR_HASH), "github", "gitlab"),
        replace_ascii(metadata(REVISION, NAR_HASH), "NixOS", "attacker"),
        replace_ascii(metadata(REVISION, NAR_HASH), "nixpkgs", "other"),
    ] {
        let runner = ExactRunner::new(spec.pin().clone(), Ok(response));
        assert_eq!(
            fetch_verified_nixpkgs(&spec, &runner).unwrap_err().code(),
            NixpkgsSourceErrorCode::IdentityMismatch
        );
    }
}

#[test]
fn optional_top_level_revision_may_be_absent() {
    let spec = spec();
    let without_revision = replace_ascii(
        metadata(REVISION, NAR_HASH),
        &format!(r#","revision":"{REVISION}""#),
        "",
    );
    let runner = ExactRunner::new(spec.pin().clone(), Ok(without_revision));
    assert!(fetch_verified_nixpkgs(&spec, &runner).is_ok());
}

#[test]
fn malformed_oversized_duplicate_and_non_store_outputs_are_refused() {
    let spec = spec();
    let cases = [
        (Vec::new(), NixpkgsSourceErrorCode::MalformedMetadata),
        (
            vec![b' '; MAX_METADATA_BYTES + 1],
            NixpkgsSourceErrorCode::MetadataTooLarge,
        ),
        (
            br#"{"locked":{},"locked":{},"path":"x"}"#.to_vec(),
            NixpkgsSourceErrorCode::MalformedMetadata,
        ),
        (
            replace_ascii(
                metadata(REVISION, NAR_HASH),
                STORE_PATH,
                "/tmp/attacker-source",
            ),
            NixpkgsSourceErrorCode::InvalidSourcePath,
        ),
    ];
    for (response, expected) in cases {
        let runner = ExactRunner::new(spec.pin().clone(), Ok(response));
        assert_eq!(
            fetch_verified_nixpkgs(&spec, &runner).unwrap_err().code(),
            expected
        );
    }
}

#[test]
fn runner_failure_stays_closed_and_redacted() {
    let spec = spec();
    let runner = ExactRunner::new(
        spec.pin().clone(),
        Err(NixpkgsSourceError::runner_failure()),
    );
    let error = fetch_verified_nixpkgs(&spec, &runner).unwrap_err();
    assert_eq!(error.code(), NixpkgsSourceErrorCode::RunnerFailure);
    assert!(!error.to_string().contains(REVISION));
    assert!(!format!("{:?}", spec.pin()).contains(REVISION));
}
