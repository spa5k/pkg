//! Ignored Real-Nix capture, golden diff, and exact FakeNix replay gate.

use std::fs;
use std::path::Path;
use std::str::FromStr;

use pkg_nix::{
    AttributePath, BuildApprovalReceipt, BuildOutputProvenance, BuildRequest, DerivedOutputTarget,
    EvaluateDerivationRequest, NarHash, NarIntegrity, NixAdapter, NixpkgsRevision, OperationId,
    OutputSelection, PolicyVersion, RealNixAdapter, SubstituteOutcome, System, TrustStatus,
    VerifyMode, VerifyRequest,
};
use pkg_testkit::{CapturingNix, ParityTranscript};

const REVISION: &str = "a62e6edd6d5e1fa0329b8653c801147986f8d446";
const NAR_HASH: &str = "sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=";

#[test]
#[ignore = "requires an isolated product-pinned Nix 2.34.8 store"]
fn real_nix_capture_matches_golden_and_replays_in_fake() -> Result<(), Box<dyn std::error::Error>> {
    let binary = std::env::var_os("PKG_REAL_NIX_BIN").ok_or("PKG_REAL_NIX_BIN is required")?;
    let home = std::env::var_os("PKG_REAL_NIX_HOME").ok_or("PKG_REAL_NIX_HOME is required")?;
    let system = System::from_str(&std::env::var("PKG_REAL_NIX_SYSTEM")?)?;
    let capture_path =
        std::env::var_os("PKG_REAL_NIX_CAPTURE").ok_or("PKG_REAL_NIX_CAPTURE is required")?;
    let golden_path =
        std::env::var_os("PKG_REAL_NIX_GOLDEN").ok_or("PKG_REAL_NIX_GOLDEN is required")?;
    let adapter = CapturingNix::new(RealNixAdapter::new(Path::new(&binary), Path::new(&home))?);

    let version = adapter.version()?;
    assert_eq!(version.nix_version().as_str(), "2.34.8");
    let request = EvaluateDerivationRequest::new(
        AttributePath::new("hello")?,
        system,
        NixpkgsRevision::new(REVISION)?,
        NarHash::new(NAR_HASH)?,
        OutputSelection::default_selection(),
    )?;
    let plan = adapter.evaluate_derivation(&request)?;
    let root = plan
        .derivations()
        .iter()
        .find(|item| item.derivation() == plan.root())
        .ok_or("root derivation missing")?;
    let selected = plan.outputs_to_install()[0].clone();
    let expected_path = root
        .outputs()
        .get(&selected)
        .ok_or("selected output missing")?
        .clone();

    assert_eq!(
        adapter.substitute(&expected_path)?.outcome(),
        SubstituteOutcome::Fetched
    );
    let info = adapter.path_info(&expected_path)?;
    assert_eq!(info.store_path(), &expected_path);
    assert!(
        info.signatures()
            .iter()
            .any(|signature| signature.key_name() == "cache.nixos.org-1")
    );
    let verify = adapter.verify(&VerifyRequest::new(
        vec![expected_path.clone()],
        VerifyMode::Recursive,
    )?)?;
    assert_eq!(verify.results().len(), 1);
    assert_eq!(verify.results()[0].nar_integrity(), NarIntegrity::Intact);
    assert_eq!(verify.results()[0].trust(), TrustStatus::Trusted);

    let receipt = BuildApprovalReceipt::new(
        OperationId::new("nightly-real-nix-parity")?,
        plan.closure_digest(),
        PolicyVersion::from_u64(1).ok_or("policy version")?,
    );
    let build = adapter.build(&BuildRequest::new(
        vec![DerivedOutputTarget::new(
            plan.root().clone(),
            vec![selected],
        )?],
        system,
        receipt,
    )?)?;
    assert_eq!(build.outputs().len(), 1);
    assert_eq!(build.outputs()[0].store_path(), &expected_path);
    assert_eq!(
        build.outputs()[0].provenance(),
        BuildOutputProvenance::CacheSigned
    );

    let transcript = adapter.transcript()?;
    // GC has no explicit request value. Its report depends on every unrelated
    // root in the machine-global store, so it cannot be a portable golden.
    // The adjacent Real-Nix smoke still exercises GC behavior directly.
    assert_eq!(transcript.len(), 6);
    let bytes = transcript.to_json_bytes()?;
    fs::write(&capture_path, &bytes)?;
    let decoded = ParityTranscript::from_json_bytes(&bytes)?;
    assert_eq!(decoded, transcript);
    decoded.assert_fake_parity()?;
    let golden = fs::read(&golden_path).map_err(|_| "platform parity golden is missing")?;
    if golden != bytes {
        return Err("Real-Nix capture differs from the reviewed platform golden".into());
    }
    Ok(())
}

#[test]
fn checked_in_goldens_are_canonical_and_replayable() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/real-nix-parity");
    for system in ["x86_64-linux", "aarch64-darwin"] {
        let bytes = fs::read(root.join(format!("{system}.json")))?;
        let transcript = ParityTranscript::from_json_bytes(&bytes)?;
        assert_eq!(transcript.to_json_bytes()?, bytes);
        transcript.assert_fake_parity()?;
    }
    Ok(())
}
