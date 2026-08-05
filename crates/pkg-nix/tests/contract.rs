//! Black-box contract tests for the `pkg-nix` crate (`plans/09` §4.1–§4.4).
//!
//! This integration test treats `pkg-nix` purely as an external consumer: it
//! exercises only the crate's public API (re-exports at the crate root plus the
//! public `contract`/`error` modules) and never names serde, `serde_json`,
//! `pkg-core`, or any crate-private item. It is hermetic and deterministic: no
//! network, no Nix, no timing, no `#[ignore]`, no `should_panic`, and no
//! `todo!`/`unimplemented!`.
//!
//! Coverage map (see `plans/09` §4.1–§4.4):
//!
//! - Object safety + `Send + Sync` of `dyn NixAdapter`, and dispatch through
//!   `Arc<dyn NixAdapter>`.
//! - A deterministic stub implementing and invoking all nine trait methods.
//! - Encode/decode round trips for every public request/report type that
//!   exposes serialization.
//! - Explicit top-level `schemaVersion = 1`, stable camelCase keys, and stable
//!   enum spellings on every serialized shape.
//! - Strict unknown-field rejection (top-level and nested).
//! - Unsupported schema-version rejection.
//! - Malformed JSON and wrong-type rejection.
//! - Trailing-bytes rejection.
//! - Exact `JsonCodec` byte-cap behavior (limit boundary + pre-parse check).
//! - Deeply nested input rejected **without panicking**.
//! - Invalid promoted `pkg-core` values rejected.
//! - Empty/duplicate collection rejection.
//! - Collection count-bound failure without huge allocations.
//! - Inconsistent status/payload combinations rejected.
//! - Deterministic canonical ordering where promised.
//! - `RootName` / `RootRef` traversal-safety rejection matrix.
//! - Bounded, redacted `NixAdapterError`.
//! - Substitute trust/signature failure represented only as `NixAdapterError`,
//!   never as a normal outcome.
//! - `BuildApprovalReceipt` shaped as only an opaque, bounded operation id.
//! - No forbidden per-call knobs (`substituters`, `trustedPublicKeys`,
//!   `sandbox`, `builders`, `buildUsers`, `maxJobs`, `expr`, `environment`,
//!   `trustPolicy`) on any serialized shape.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use pkg_nix::error::BoundedSummary;
use pkg_nix::{
    AcceptedFormats, AddRootRequest, AttributePath, BuildApprovalReceipt, BuildReport,
    BuildRequest, BuildStatus, DerivationPath, EvalRealizeRequest, FormatVersion, GcReport,
    GcStatus, JsonCodec, MalformedKind, MethodKind, NarHash, NarIntegrity, NixAdapter,
    NixAdapterError, NixAdapterErrorCode, NixVersion, NixpkgsRevision, OperationId, OutputName,
    OutputSelection, PathInfoReport, PathRepairResult, PathVerifyResult, RealizationReport,
    RepairOutcome, RepairReport, RootName, RootRef, SchemaVersion, Signature, StorePath,
    SubstituteOutcome, SubstituteReport, System, TrustStatus, VerifyMode, VerifyReport,
    VerifyRequest, VersionInfo,
};

// Compile assertion: every contract type appearing in the NixAdapter trait
// signatures is nameable directly from the crate root (plans/09 §4.1),
// including the now re-exported VerifyRequest and PathVerifyResult. Removing
// either reexport breaks this item.
const _: () = {
    let _: std::marker::PhantomData<pkg_nix::VerifyRequest> = std::marker::PhantomData;
    let _: std::marker::PhantomData<pkg_nix::PathVerifyResult> = std::marker::PhantomData;
};

// ===========================================================================
// Fixed, valid fixtures (deterministic).
// ===========================================================================

/// A valid 32-character Nix base32 store-path hash.
const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
/// A valid `sha256` SRI NAR hash.
const NAR: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
/// A valid 40-hex Nixpkgs revision.
const REV: &str = "0123456789abcdef0123456789abcdef01234567";

/// Builds a valid store path with the given name and the fixed digest.
fn store_path(name: &str) -> StorePath {
    StorePath::new(&format!("/nix/store/{STORE_HASH}-{name}")).unwrap()
}

/// Builds a valid derivation path (`.drv` suffix) with the given name.
fn drv(name: &str) -> DerivationPath {
    DerivationPath::from_str(&format!("/nix/store/{STORE_HASH}-{name}.drv")).unwrap()
}

/// A valid signature (`name:standard-base64`).
fn sig(name: &str) -> Signature {
    // 8 standard-base64 characters is a valid, canonical (no padding) blob.
    Signature::new(&format!("{name}:BBBBBBBB")).unwrap()
}

fn nar_hash() -> NarHash {
    NarHash::new(NAR).unwrap()
}

fn nixpkgs_revision() -> NixpkgsRevision {
    NixpkgsRevision::new(REV).unwrap()
}

// --- composite report/request fixtures -------------------------------------

fn version_info_fixture() -> VersionInfo {
    VersionInfo::new(
        NixVersion::new("2.33.5").unwrap(),
        AcceptedFormats::new(FormatVersion::new(1).unwrap()),
    )
}

fn realization_fixture() -> RealizationReport {
    let out = store_path("hello-1.0");
    let mut outputs = BTreeMap::new();
    outputs.insert(OutputName::new("out").unwrap(), out.clone());
    RealizationReport::new(out, drv("hello-1.0"), outputs).unwrap()
}

fn path_info_fixture() -> PathInfoReport {
    PathInfoReport::new(
        store_path("hello-1.0"),
        nar_hash(),
        vec![sig("k"), sig("m")],
        vec![store_path("glibc-2.39")],
        Some(drv("hello-1.0")),
        1024,
        4096,
    )
    .unwrap()
}

fn substitute_fixture(outcome: SubstituteOutcome) -> SubstituteReport {
    SubstituteReport::new(store_path("hello-1.0"), outcome)
}

fn receipt_fixture() -> BuildApprovalReceipt {
    BuildApprovalReceipt::new(OperationId::new("op-0001").unwrap())
}

fn build_request_fixture() -> BuildRequest {
    BuildRequest::new(
        vec![drv("hello-1.0")],
        System::X8664Linux,
        receipt_fixture(),
    )
    .unwrap()
}

fn build_report_built() -> BuildReport {
    BuildReport::new(BuildStatus::Built, vec![store_path("hello-1.0")]).unwrap()
}

fn build_report_acquire_no_binary() -> BuildReport {
    BuildReport::new(BuildStatus::AcquireNoBinary, vec![]).unwrap()
}

fn verify_request_fixture() -> VerifyRequest {
    VerifyRequest::new(vec![store_path("hello-1.0")], VerifyMode::Recursive).unwrap()
}

fn verify_report_fixture() -> VerifyReport {
    VerifyReport::new(vec![PathVerifyResult::new(
        store_path("hello-1.0"),
        NarIntegrity::Intact,
        TrustStatus::Trusted,
    )])
    .unwrap()
}

fn repair_report_fixture() -> RepairReport {
    RepairReport::new(vec![PathRepairResult::new(
        store_path("hello-1.0"),
        RepairOutcome::Restored,
    )])
    .unwrap()
}

fn gc_report_collected() -> GcReport {
    GcReport::new(
        GcStatus::Collected,
        vec![store_path("unreachable-1")],
        12_345,
    )
    .unwrap()
}

fn gc_report_refused() -> GcReport {
    GcReport::new(GcStatus::RefusedUnderLease, vec![], 0).unwrap()
}

fn add_root_fixture() -> AddRootRequest {
    AddRootRequest::new(RootName::new("gen-0007").unwrap(), store_path("hello-1.0"))
}

fn root_ref_fixture() -> RootRef {
    RootRef::new("/nix/var/nix/gcroots/pkg/users/1001/gen-0007").unwrap()
}

fn eval_request_default_outputs() -> EvalRealizeRequest {
    EvalRealizeRequest::new(
        AttributePath::new("python311.pkgs.requests").unwrap(),
        System::X8664Linux,
        nixpkgs_revision(),
        nar_hash(),
        OutputSelection::default_selection(),
    )
    .unwrap()
}

fn eval_request_explicit_outputs() -> EvalRealizeRequest {
    EvalRealizeRequest::new(
        AttributePath::new("ripgrep").unwrap(),
        System::Aarch64Darwin,
        nixpkgs_revision(),
        nar_hash(),
        OutputSelection::explicit(vec![
            OutputName::new("out").unwrap(),
            OutputName::new("man").unwrap(),
        ])
        .unwrap(),
    )
    .unwrap()
}

// --- JSON-shape helpers (raw bytes only; no serde_json dependency) ----------

/// Inserts a top-level `"unknownField":0` into the encoded object bytes by
/// splicing just before the final closing brace.
fn with_unknown_field(bytes: &[u8]) -> Vec<u8> {
    let mut s = String::from_utf8(bytes.to_vec()).expect("encoded json is utf-8");
    let i = s.rfind('}').expect("encoded shape is an object");
    s.insert_str(i, ",\"unknownField\":0");
    s.into_bytes()
}

/// Returns the [`MalformedKind`] if `err` is a `MalformedPayload`, else `None`.
fn malformed_kind(err: &NixAdapterError) -> Option<MalformedKind> {
    match err {
        NixAdapterError::MalformedPayload { kind } => Some(*kind),
        _ => None,
    }
}

/// Reinterprets encoded JSON bytes as `&str` for substring assertions.
fn as_str(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("utf-8")
}

// ===========================================================================
// Deterministic stub adapters
// ===========================================================================

/// A stub that returns a deterministic `Ok` report for every method, so the
/// full nine-method signature set compiles and dispatch can be asserted.
struct OkStub;

impl NixAdapter for OkStub {
    fn version(&self) -> Result<VersionInfo, NixAdapterError> {
        Ok(version_info_fixture())
    }
    fn eval_realize(&self, _: &EvalRealizeRequest) -> Result<RealizationReport, NixAdapterError> {
        Ok(realization_fixture())
    }
    fn path_info(&self, _: &StorePath) -> Result<PathInfoReport, NixAdapterError> {
        Ok(path_info_fixture())
    }
    fn substitute(&self, _: &StorePath) -> Result<SubstituteReport, NixAdapterError> {
        Ok(substitute_fixture(SubstituteOutcome::Fetched))
    }
    fn build(&self, _: &BuildRequest) -> Result<BuildReport, NixAdapterError> {
        Ok(build_report_built())
    }
    fn verify(&self, _: &VerifyRequest) -> Result<VerifyReport, NixAdapterError> {
        Ok(verify_report_fixture())
    }
    fn repair(&self, _: &[StorePath]) -> Result<RepairReport, NixAdapterError> {
        Ok(repair_report_fixture())
    }
    fn gc(&self) -> Result<GcReport, NixAdapterError> {
        Ok(gc_report_collected())
    }
    fn add_root(&self, _: &AddRootRequest) -> Result<RootRef, NixAdapterError> {
        Ok(root_ref_fixture())
    }
}

/// A stub whose every method fails with the closed `Unavailable` error,
/// exercising the failure path (failures are `NixAdapterError`, never raw
/// data).
struct UnavailableStub;

impl NixAdapter for UnavailableStub {
    fn version(&self) -> Result<VersionInfo, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }
    fn eval_realize(&self, _: &EvalRealizeRequest) -> Result<RealizationReport, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }
    fn path_info(&self, _: &StorePath) -> Result<PathInfoReport, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }
    fn substitute(&self, _: &StorePath) -> Result<SubstituteReport, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }
    fn build(&self, _: &BuildRequest) -> Result<BuildReport, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }
    fn verify(&self, _: &VerifyRequest) -> Result<VerifyReport, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }
    fn repair(&self, _: &[StorePath]) -> Result<RepairReport, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }
    fn gc(&self) -> Result<GcReport, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }
    fn add_root(&self, _: &AddRootRequest) -> Result<RootRef, NixAdapterError> {
        Err(NixAdapterError::Unavailable)
    }
}

// ===========================================================================
// Macro: decode of given bytes for a type must error.
// ===========================================================================

/// Runs `<$ty>::decode` and asserts it errors, returning the error.
macro_rules! decode_err {
    ($codec:expr, $ty:ty, $bytes:expr) => {{
        let b: &[u8] = $bytes;
        match <$ty>::decode(&$codec, b) {
            Ok(_) => panic!("expected decode to reject"),
            Err(e) => e,
        }
    }};
}

/// Encodes the given value, splices in an unknown top-level field, and asserts
/// the resulting decode is rejected as `MalformedPayload` (json kind).
macro_rules! assert_unknown_field_rejected {
    ($codec:expr, $ty:ty, $value:expr) => {{
        let valid = $value.encode().expect("encode");
        let bad = with_unknown_field(&valid);
        let err = decode_err!($codec, $ty, &bad);
        assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);
        assert_eq!(malformed_kind(&err), Some(MalformedKind::Json));
    }};
}

// ===========================================================================
// Tests: object safety, Send + Sync, nine-method dispatch
// ===========================================================================

#[test]
fn trait_is_object_safe_send_sync_and_dyn_compatible() {
    // `dyn NixAdapter` is itself Send + Sync (the trait requires it).
    fn _assert_send_sync<T: Send + Sync + ?Sized>() {}
    _assert_send_sync::<dyn NixAdapter>();

    // Object-safe: the trait can live behind `Box`/`Arc` and be taken by `&dyn`.
    let _: Box<dyn NixAdapter> = Box::new(OkStub);
    let _: Arc<dyn NixAdapter> = Arc::new(OkStub);
    fn _takes_dyn(_: &dyn NixAdapter) {}
    _takes_dyn(&OkStub);
}

#[test]
fn stub_dispatches_all_nine_methods_through_dyn() {
    let a: Arc<dyn NixAdapter> = Arc::new(OkStub);

    let v = a.version().expect("version");
    assert_eq!(v.nix_version().as_str(), "2.33.5");
    assert_eq!(v.accepted_formats().path_info().get(), 1);

    let r = a
        .eval_realize(&eval_request_default_outputs())
        .expect("eval_realize");
    assert_eq!(r.store_path(), &store_path("hello-1.0"));
    assert_eq!(r.deriver(), &drv("hello-1.0"));
    assert_eq!(r.outputs().len(), 1);

    let pi = a.path_info(&store_path("hello-1.0")).expect("path_info");
    assert_eq!(pi.nar_hash(), &nar_hash());
    assert_eq!(pi.signatures().len(), 2);
    assert_eq!(pi.references(), &[store_path("glibc-2.39")]);
    assert_eq!(pi.deriver(), Some(&drv("hello-1.0")));
    assert_eq!(pi.nar_size(), 1024);
    assert_eq!(pi.closure_size(), 4096);

    let s = a.substitute(&store_path("hello-1.0")).expect("substitute");
    assert_eq!(s.store_path(), &store_path("hello-1.0"));
    assert_eq!(s.outcome(), SubstituteOutcome::Fetched);

    let b = a.build(&build_request_fixture()).expect("build");
    assert_eq!(b.status(), BuildStatus::Built);
    assert_eq!(b.outputs(), &[store_path("hello-1.0")]);

    let vr = a.verify(&verify_request_fixture()).expect("verify");
    assert_eq!(vr.results().len(), 1);
    assert_eq!(vr.results()[0].nar_integrity(), NarIntegrity::Intact);
    assert_eq!(vr.results()[0].trust(), TrustStatus::Trusted);

    let rep = a.repair(&[store_path("hello-1.0")]).expect("repair");
    assert_eq!(rep.results().len(), 1);
    assert_eq!(rep.results()[0].outcome(), RepairOutcome::Restored);

    let g = a.gc().expect("gc");
    assert_eq!(g.status(), GcStatus::Collected);
    assert_eq!(g.collected(), &[store_path("unreachable-1")]);
    assert_eq!(g.freed_bytes(), 12_345);

    let root = a.add_root(&add_root_fixture()).expect("add_root");
    assert_eq!(
        root.as_str(),
        "/nix/var/nix/gcroots/pkg/users/1001/gen-0007"
    );
}

#[test]
fn adapter_failure_path_is_closed_nix_adapter_error() {
    // Every method's failure surfaces as the closed NixAdapterError — never raw
    // wire data, stdout/stderr, or credentials.
    let a: Arc<dyn NixAdapter> = Arc::new(UnavailableStub);
    assert_eq!(
        a.version().unwrap_err().code(),
        NixAdapterErrorCode::Unavailable
    );
    assert_eq!(
        a.eval_realize(&eval_request_default_outputs())
            .unwrap_err()
            .code(),
        NixAdapterErrorCode::Unavailable
    );
    assert_eq!(
        a.path_info(&store_path("x")).unwrap_err().code(),
        NixAdapterErrorCode::Unavailable
    );
    assert_eq!(
        a.substitute(&store_path("x")).unwrap_err().code(),
        NixAdapterErrorCode::Unavailable
    );
    assert_eq!(
        a.build(&build_request_fixture()).unwrap_err().code(),
        NixAdapterErrorCode::Unavailable
    );
    assert_eq!(
        a.verify(&verify_request_fixture()).unwrap_err().code(),
        NixAdapterErrorCode::Unavailable
    );
    assert_eq!(
        a.repair(&[store_path("x")]).unwrap_err().code(),
        NixAdapterErrorCode::Unavailable
    );
    assert_eq!(a.gc().unwrap_err().code(), NixAdapterErrorCode::Unavailable);
    assert_eq!(
        a.add_root(&add_root_fixture()).unwrap_err().code(),
        NixAdapterErrorCode::Unavailable
    );
}

#[test]
fn method_kind_has_exactly_nine_variants() {
    assert_eq!(MethodKind::ALL.len(), 9);
    // Stable camelCase names round-trip.
    for m in MethodKind::ALL {
        let s = m.as_str();
        assert_eq!(MethodKind::from_str(s), Ok(m));
        assert_eq!(m.to_string(), s);
    }
    // Unknown method-name strings are rejected.
    assert!(MethodKind::from_str("nope").is_err());
}

// ===========================================================================
// Tests: schema version vocabulary
// ===========================================================================

#[test]
fn schema_version_only_accepts_current() {
    assert_eq!(SchemaVersion::current().get(), 1);
    assert_eq!(SchemaVersion::CURRENT.get(), 1);
    assert!(SchemaVersion::new(1).is_ok());
    // Any other value — including zero and future versions — is rejected.
    assert_eq!(
        SchemaVersion::new(0).unwrap_err().code(),
        NixAdapterErrorCode::UnsupportedSchemaVersion
    );
    assert_eq!(
        SchemaVersion::new(2).unwrap_err().code(),
        NixAdapterErrorCode::UnsupportedSchemaVersion
    );
}

// ===========================================================================
// Tests: encode/decode round trips for every serialized type
// ===========================================================================

#[test]
fn version_info_round_trip() {
    let c = JsonCodec::production();
    let v = version_info_fixture();
    let enc = v.encode().expect("encode");
    let back = VersionInfo::decode(&c, &enc).expect("decode");
    // Wire-stable: re-encoding reproduces the exact bytes.
    assert_eq!(back.encode().expect("re-encode"), enc);
    assert_eq!(back, v);
}

#[test]
fn eval_realize_request_round_trips_default_and_explicit_outputs() {
    let c = JsonCodec::production();

    // Default (meta) outputs encode as JSON null and round-trip.
    let def = eval_request_default_outputs();
    let enc = def.encode().expect("encode");
    assert!(
        std::str::from_utf8(&enc)
            .unwrap()
            .contains("\"outputs\":null")
    );
    let back = EvalRealizeRequest::decode(&c, &enc).expect("decode");
    assert_eq!(back.encode().expect("re-encode"), enc);
    assert_eq!(back, def);
    assert!(back.outputs().is_default());

    // Explicit outputs preserve caller order.
    let exp = eval_request_explicit_outputs();
    let enc = exp.encode().expect("encode");
    assert!(
        std::str::from_utf8(&enc)
            .unwrap()
            .contains("\"outputs\":[\"out\",\"man\"]")
    );
    let back = EvalRealizeRequest::decode(&c, &enc).expect("decode");
    assert_eq!(back.encode().expect("re-encode"), enc);
    assert_eq!(back, exp);
    assert_eq!(
        back.outputs()
            .explicit_outputs()
            .unwrap()
            .iter()
            .map(OutputName::as_str)
            .collect::<Vec<_>>(),
        ["out", "man"]
    );
}

#[test]
fn realization_report_round_trip() {
    let c = JsonCodec::production();
    let r = realization_fixture();
    let enc = r.encode().expect("encode");
    let back = RealizationReport::decode(&c, &enc).expect("decode");
    // Wire-stable.
    assert_eq!(back.encode().expect("re-encode"), enc);
    // Compare every accessor (pkg-core Realization equality is identity-only;
    // RealizationReport is compared field-by-field here).
    assert_eq!(back.store_path(), r.store_path());
    assert_eq!(back.deriver(), r.deriver());
    assert_eq!(back.outputs(), r.outputs());
}

#[test]
fn path_info_report_round_trip() {
    let c = JsonCodec::production();
    let p = path_info_fixture();
    let enc = p.encode().expect("encode");
    let back = PathInfoReport::decode(&c, &enc).expect("decode");
    assert_eq!(back.encode().expect("re-encode"), enc);
    assert_eq!(back, p);
}

#[test]
fn substitute_report_round_trips_all_outcomes() {
    let c = JsonCodec::production();
    for outcome in [
        SubstituteOutcome::Fetched,
        SubstituteOutcome::AbsentFromSubstituters,
        SubstituteOutcome::NoBinaryAvailable,
    ] {
        let r = substitute_fixture(outcome);
        let enc = r.encode().expect("encode");
        let back = SubstituteReport::decode(&c, &enc).expect("decode");
        assert_eq!(back.encode().expect("re-encode"), enc);
        assert_eq!(back, r);
        assert_eq!(back.outcome(), outcome);
    }
}

#[test]
fn build_request_round_trip() {
    let c = JsonCodec::production();
    let r = build_request_fixture();
    let enc = r.encode().expect("encode");
    let back = BuildRequest::decode(&c, &enc).expect("decode");
    assert_eq!(back.encode().expect("re-encode"), enc);
    assert_eq!(back, r);
    assert_eq!(back.receipt().operation_id().as_str(), "op-0001");
}

#[test]
fn build_report_round_trips_built_and_acquire_no_binary() {
    let c = JsonCodec::production();
    for rep in [build_report_built(), build_report_acquire_no_binary()] {
        let enc = rep.encode().expect("encode");
        let back = BuildReport::decode(&c, &enc).expect("decode");
        assert_eq!(back.encode().expect("re-encode"), enc);
        assert_eq!(back, rep);
    }
}

#[test]
fn verify_request_round_trip() {
    let c = JsonCodec::production();
    let r = verify_request_fixture();
    let enc = r.encode().expect("encode");
    let back = VerifyRequest::decode(&c, &enc).expect("decode");
    assert_eq!(back.encode().expect("re-encode"), enc);
    assert_eq!(back, r);
    assert_eq!(back.mode(), VerifyMode::Recursive);
}

#[test]
fn verify_report_round_trip() {
    let c = JsonCodec::production();
    let r = verify_report_fixture();
    let enc = r.encode().expect("encode");
    let back = VerifyReport::decode(&c, &enc).expect("decode");
    assert_eq!(back.encode().expect("re-encode"), enc);
    assert_eq!(back, r);
}

#[test]
fn repair_report_round_trip() {
    let c = JsonCodec::production();
    let r = repair_report_fixture();
    let enc = r.encode().expect("encode");
    let back = RepairReport::decode(&c, &enc).expect("decode");
    assert_eq!(back.encode().expect("re-encode"), enc);
    assert_eq!(back, r);
}

#[test]
fn gc_report_round_trips_collected_and_refused() {
    let c = JsonCodec::production();
    for rep in [gc_report_collected(), gc_report_refused()] {
        let enc = rep.encode().expect("encode");
        let back = GcReport::decode(&c, &enc).expect("decode");
        assert_eq!(back.encode().expect("re-encode"), enc);
        assert_eq!(back, rep);
    }
}

#[test]
fn add_root_request_round_trip() {
    let c = JsonCodec::production();
    let r = add_root_fixture();
    let enc = r.encode().expect("encode");
    let back = AddRootRequest::decode(&c, &enc).expect("decode");
    assert_eq!(back.encode().expect("re-encode"), enc);
    assert_eq!(back, r);
}

#[test]
fn root_ref_round_trip() {
    let c = JsonCodec::production();
    let r = root_ref_fixture();
    let enc = r.encode().expect("encode");
    let back = RootRef::decode(&c, &enc).expect("decode");
    assert_eq!(back.encode().expect("re-encode"), enc);
    assert_eq!(back, r);
}

// ===========================================================================
// Tests: explicit schemaVersion=1, camelCase keys, stable enum spellings
// ===========================================================================

/// Collects every public serialized shape in one place.
fn all_shapes() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("versionInfo", version_info_fixture().encode().unwrap()),
        (
            "evalRealizeRequest(default)",
            eval_request_default_outputs().encode().unwrap(),
        ),
        (
            "evalRealizeRequest(explicit)",
            eval_request_explicit_outputs().encode().unwrap(),
        ),
        ("realizationReport", realization_fixture().encode().unwrap()),
        ("pathInfoReport", path_info_fixture().encode().unwrap()),
        (
            "substituteReport(fetched)",
            substitute_fixture(SubstituteOutcome::Fetched)
                .encode()
                .unwrap(),
        ),
        (
            "substituteReport(absent)",
            substitute_fixture(SubstituteOutcome::AbsentFromSubstituters)
                .encode()
                .unwrap(),
        ),
        (
            "substituteReport(noBinary)",
            substitute_fixture(SubstituteOutcome::NoBinaryAvailable)
                .encode()
                .unwrap(),
        ),
        ("buildRequest", build_request_fixture().encode().unwrap()),
        ("buildReport(built)", build_report_built().encode().unwrap()),
        (
            "buildReport(acquireNoBinary)",
            build_report_acquire_no_binary().encode().unwrap(),
        ),
        ("verifyRequest", verify_request_fixture().encode().unwrap()),
        ("verifyReport", verify_report_fixture().encode().unwrap()),
        ("repairReport", repair_report_fixture().encode().unwrap()),
        (
            "gcReport(collected)",
            gc_report_collected().encode().unwrap(),
        ),
        ("gcReport(refused)", gc_report_refused().encode().unwrap()),
        ("addRootRequest", add_root_fixture().encode().unwrap()),
        ("rootRef", root_ref_fixture().encode().unwrap()),
    ]
}

#[test]
fn every_shape_carries_explicit_schema_version_1() {
    for (name, bytes) in all_shapes() {
        let s = std::str::from_utf8(&bytes).expect("utf-8");
        assert!(
            s.contains("\"schemaVersion\":1"),
            "{name}: missing explicit top-level schemaVersion=1 in {s}"
        );
    }
}

#[test]
fn wire_names_are_stable_camelcase() {
    // Struct keys are camelCase.
    assert!(as_str(&version_info_fixture().encode().unwrap()).contains("\"nixVersion\""));
    assert!(as_str(&version_info_fixture().encode().unwrap()).contains("\"acceptedFormats\""));
    assert!(as_str(&version_info_fixture().encode().unwrap()).contains("\"pathInfo\":1"));
    assert!(as_str(&path_info_fixture().encode().unwrap()).contains("\"narHash\""));
    assert!(as_str(&path_info_fixture().encode().unwrap()).contains("\"narSize\""));
    assert!(as_str(&path_info_fixture().encode().unwrap()).contains("\"closureSize\""));
    assert!(
        as_str(&eval_request_default_outputs().encode().unwrap()).contains("\"nixpkgsRevision\"")
    );
    assert!(
        as_str(&eval_request_default_outputs().encode().unwrap()).contains("\"nixpkgsNarHash\"")
    );
    assert!(as_str(&build_request_fixture().encode().unwrap()).contains("\"operationId\""));
    assert!(as_str(&gc_report_collected().encode().unwrap()).contains("\"freedBytes\""));
    assert!(as_str(&verify_report_fixture().encode().unwrap()).contains("\"narIntegrity\""));

    // Enum variant spellings are stable camelCase.
    assert!(
        as_str(
            &substitute_fixture(SubstituteOutcome::Fetched)
                .encode()
                .unwrap()
        )
        .contains("\"outcome\":\"fetched\"")
    );
    assert!(
        as_str(
            &substitute_fixture(SubstituteOutcome::AbsentFromSubstituters)
                .encode()
                .unwrap()
        )
        .contains("\"outcome\":\"absentFromSubstituters\"")
    );
    assert!(
        as_str(
            &substitute_fixture(SubstituteOutcome::NoBinaryAvailable)
                .encode()
                .unwrap()
        )
        .contains("\"outcome\":\"noBinaryAvailable\"")
    );
    assert!(as_str(&build_report_built().encode().unwrap()).contains("\"status\":\"built\""));
    assert!(
        as_str(&build_report_acquire_no_binary().encode().unwrap())
            .contains("\"status\":\"acquireNoBinary\"")
    );
    assert!(as_str(&verify_request_fixture().encode().unwrap()).contains("\"mode\":\"recursive\""));
    assert!(as_str(&verify_report_fixture().encode().unwrap()).contains("\"trust\":\"trusted\""));
    assert!(
        as_str(&verify_report_fixture().encode().unwrap()).contains("\"narIntegrity\":\"intact\"")
    );
    assert!(
        as_str(&repair_report_fixture().encode().unwrap()).contains("\"outcome\":\"restored\"")
    );
    assert!(as_str(&gc_report_collected().encode().unwrap()).contains("\"status\":\"collected\""));
    assert!(
        as_str(&gc_report_refused().encode().unwrap()).contains("\"status\":\"refusedUnderLease\"")
    );
}

#[test]
fn no_shape_carries_a_forbidden_per_call_knob() {
    // No request or report may surface any per-call trust/flag/expression knob
    // as a JSON **key token** (plans/01 §11.1; T-DAEMON-1/T-CACHE-1/T-BUILD-1).
    // Checking for the quoted key token (e.g. `"sandbox"`) avoids matching an
    // unrelated value substring such as a store path that happens to contain
    // the same letters.
    let forbidden = [
        "substituters",
        "trustedPublicKeys",
        "sandbox",
        "builders",
        "buildUsers",
        "maxJobs",
        "expr",
        "environment",
        "trustPolicy",
    ];
    for (name, bytes) in all_shapes() {
        let s = std::str::from_utf8(&bytes).expect("utf-8");
        for knob in forbidden {
            let quoted = format!("\"{knob}\"");
            assert!(
                !s.contains(&quoted),
                "{name}: serialized shape contains forbidden knob key {quoted}: {s}"
            );
        }
    }
}

// ===========================================================================
// Tests: strict decode (unknown fields, schema, malformed, trailing)
// ===========================================================================

#[test]
fn decode_rejects_unknown_top_level_fields() {
    let c = JsonCodec::production();
    assert_unknown_field_rejected!(c, VersionInfo, version_info_fixture());
    assert_unknown_field_rejected!(c, EvalRealizeRequest, eval_request_default_outputs());
    assert_unknown_field_rejected!(c, RealizationReport, realization_fixture());
    assert_unknown_field_rejected!(c, PathInfoReport, path_info_fixture());
    assert_unknown_field_rejected!(
        c,
        SubstituteReport,
        substitute_fixture(SubstituteOutcome::Fetched)
    );
    assert_unknown_field_rejected!(c, BuildRequest, build_request_fixture());
    assert_unknown_field_rejected!(c, BuildReport, build_report_built());
    assert_unknown_field_rejected!(c, VerifyRequest, verify_request_fixture());
    assert_unknown_field_rejected!(c, VerifyReport, verify_report_fixture());
    assert_unknown_field_rejected!(c, RepairReport, repair_report_fixture());
    assert_unknown_field_rejected!(c, GcReport, gc_report_collected());
    assert_unknown_field_rejected!(c, AddRootRequest, add_root_fixture());
    assert_unknown_field_rejected!(c, RootRef, root_ref_fixture());
}

#[test]
fn decode_rejects_unknown_nested_fields() {
    let c = JsonCodec::production();

    // Unknown field inside acceptedFormats.
    let bad =
        br#"{"schemaVersion":1,"nixVersion":"2.33.5","acceptedFormats":{"pathInfo":1,"extra":9}}"#;
    let err = decode_err!(c, VersionInfo, bad);
    assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);

    // Unknown field inside the opaque build receipt.
    let bad = br#"{"schemaVersion":1,"targets":["/nix/store/0123456789abcdfghijklmnpqrsvwxyz-hello-1.0.drv"],"system":"x86_64-linux","receipt":{"operationId":"op-0001","secret":"x"}}"#;
    let err = decode_err!(c, BuildRequest, bad);
    assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);

    // Unknown field inside a per-path verify result.
    let bad = br#"{"schemaVersion":1,"results":[{"path":"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-hello-1.0","narIntegrity":"intact","trust":"trusted","extra":1}]}"#;
    let err = decode_err!(c, VerifyReport, bad);
    assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);

    // Unknown field inside a per-path repair result.
    let bad = br#"{"schemaVersion":1,"results":[{"path":"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-hello-1.0","outcome":"restored","extra":1}]}"#;
    let err = decode_err!(c, RepairReport, bad);
    assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);
}

#[test]
fn decode_rejects_unsupported_schema_version() {
    let c = JsonCodec::production();
    let mut bad = version_info_fixture().encode().unwrap();
    // The codec always emits exactly "schemaVersion":1; rewrite it to 2.
    let as_string = String::from_utf8(bad.clone()).unwrap();
    bad = as_string
        .replace("\"schemaVersion\":1", "\"schemaVersion\":2")
        .into_bytes();

    let err = decode_err!(c, VersionInfo, &bad);
    assert_eq!(err.code(), NixAdapterErrorCode::UnsupportedSchemaVersion);
    match err {
        NixAdapterError::UnsupportedSchemaVersion { observed } => assert_eq!(observed, 2),
        other => panic!("expected UnsupportedSchemaVersion, got {other}"),
    }
}

#[test]
fn decode_rejects_malformed_json_and_wrong_types() {
    let c = JsonCodec::production();
    // Truly malformed JSON.
    for bad in [b"{not json".as_slice(), b"", b"{\"", b"\"unterminated"] {
        let err = decode_err!(c, VersionInfo, bad);
        assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);
        assert_eq!(malformed_kind(&err), Some(MalformedKind::Json));
    }
    // Valid JSON of the wrong top-level type is also rejected.
    for bad in [b"null".as_slice(), b"123", b"[]", b"\"a string\"", b"true"] {
        let err = decode_err!(c, SubstituteReport, bad);
        assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);
        assert_eq!(malformed_kind(&err), Some(MalformedKind::Json));
    }
}

#[test]
fn decode_rejects_trailing_bytes() {
    let c = JsonCodec::production();
    let valid = substitute_fixture(SubstituteOutcome::Fetched)
        .encode()
        .unwrap();
    let mut trailing = valid.clone();
    // Trailing non-whitespace must be rejected (trailing whitespace is allowed).
    trailing.extend_from_slice(b"x");
    let err = decode_err!(c, SubstituteReport, &trailing);
    assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);
    assert_eq!(malformed_kind(&err), Some(MalformedKind::Json));
}

#[test]
fn codec_size_limit_is_exact_and_checked_before_parsing() {
    assert_eq!(JsonCodec::production().limit(), JsonCodec::PRODUCTION_LIMIT);
    assert_eq!(JsonCodec::PRODUCTION_LIMIT, 64 * 1024 * 1024);

    let rep = substitute_fixture(SubstituteOutcome::Fetched);
    let enc = rep.encode().unwrap();
    let len = enc.len();

    // At the exact limit, decode succeeds (the check is `> limit`, not `>=`).
    let at = JsonCodec::with_limit(len);
    assert_eq!(at.limit(), len);
    assert!(SubstituteReport::decode(&at, &enc).is_ok());

    // One byte under the limit is rejected before parsing, carrying the limit.
    let under = JsonCodec::with_limit(len - 1);
    let err = SubstituteReport::decode(&under, &enc).unwrap_err();
    assert_eq!(err.code(), NixAdapterErrorCode::OversizedInput);
    match err {
        NixAdapterError::OversizedInput { limit_bytes } => assert_eq!(limit_bytes, len - 1),
        other => panic!("expected OversizedInput, got {other}"),
    }
}

#[test]
fn decode_rejects_deeply_nested_input_as_malformed() {
    // Deeply nested input must be rejected as MalformedPayload and must not
    // overflow the stack. These DTOs are non-recursive, so serde_json reports a
    // JSON type-mismatch (also `MalformedPayload` / `MalformedKind::Json`)
    // before reaching its own recursion limit; `ExcessiveNesting` is reserved
    // for future recursive DTOs and is not asserted here.
    let c = JsonCodec::production();
    let deeply_nested = format!("{}{}", "[".repeat(512), "]".repeat(512));
    let err = decode_err!(c, VersionInfo, deeply_nested.as_bytes());
    assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);
    assert_eq!(malformed_kind(&err), Some(MalformedKind::Json));
    let deeply_nested_obj = format!("{}0{}", "{".repeat(512), "}".repeat(512));
    let err = decode_err!(c, GcReport, deeply_nested_obj.as_bytes());
    assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);
    assert_eq!(malformed_kind(&err), Some(MalformedKind::Json));
}

// ===========================================================================
// Tests: invalid promoted values, empty/duplicate collections, bounds
// ===========================================================================

#[test]
fn decode_rejects_invalid_promoted_values() {
    let c = JsonCodec::production();

    // Invalid Nix version (whitespace and punctuation).
    let bad =
        br#"{"schemaVersion":1,"nixVersion":"bad version!","acceptedFormats":{"pathInfo":1}}"#;
    assert_eq!(
        decode_err!(c, VersionInfo, bad).code(),
        NixAdapterErrorCode::ValidationFailure
    );

    // Format version zero.
    let bad = br#"{"schemaVersion":1,"nixVersion":"2.33.5","acceptedFormats":{"pathInfo":0}}"#;
    assert_eq!(
        decode_err!(c, VersionInfo, bad).code(),
        NixAdapterErrorCode::ValidationFailure
    );

    // Invalid store path (digest too short).
    let bad = br#"{"schemaVersion":1,"storePath":"/nix/store/abc-x","outcome":"fetched"}"#;
    assert_eq!(
        decode_err!(c, SubstituteReport, bad).code(),
        NixAdapterErrorCode::ValidationFailure
    );

    // Invalid NAR hash.
    let bad = br#"{"schemaVersion":1,"storePath":"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-x","narHash":"not-a-hash","narSize":1,"closureSize":1,"references":[],"signatures":[]}"#;
    assert_eq!(
        decode_err!(c, PathInfoReport, bad).code(),
        NixAdapterErrorCode::ValidationFailure
    );

    // Unknown system.
    let bad = br#"{"schemaVersion":1,"targets":["/nix/store/0123456789abcdfghijklmnpqrsvwxyz-x.drv"],"system":"windows-foo","receipt":{"operationId":"op-0001"}}"#;
    assert_eq!(
        decode_err!(c, BuildRequest, bad).code(),
        NixAdapterErrorCode::ValidationFailure
    );

    // Invalid operation id (whitespace).
    let bad = br#"{"schemaVersion":1,"targets":["/nix/store/0123456789abcdfghijklmnpqrsvwxyz-x.drv"],"system":"x86_64-linux","receipt":{"operationId":"bad id"}}"#;
    assert_eq!(
        decode_err!(c, BuildRequest, bad).code(),
        NixAdapterErrorCode::ValidationFailure
    );

    // Inverted sizes (closure smaller than nar).
    let bad = br#"{"schemaVersion":1,"storePath":"/nix/store/0123456789abcdfghijklmnpqrsvwxyz-x","narHash":"sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=","narSize":100,"closureSize":50,"references":[],"signatures":[]}"#;
    assert_eq!(
        decode_err!(c, PathInfoReport, bad).code(),
        NixAdapterErrorCode::ValidationFailure
    );
}

#[test]
fn constructors_reject_empty_and_duplicate_collections() {
    // RealizationReport: empty outputs.
    assert!(RealizationReport::new(store_path("x"), drv("x"), BTreeMap::new()).is_err());

    // PathInfoReport: duplicate signatures, duplicate references.
    let s = sig("k");
    assert!(
        PathInfoReport::new(
            store_path("x"),
            nar_hash(),
            vec![s.clone(), s],
            vec![],
            None,
            1,
            1
        )
        .is_err()
    );
    let r = store_path("other");
    assert!(
        PathInfoReport::new(
            store_path("x"),
            nar_hash(),
            vec![],
            vec![r.clone(), r],
            None,
            1,
            1
        )
        .is_err()
    );

    // BuildRequest: empty and duplicate targets.
    assert!(BuildRequest::new(vec![], System::X8664Linux, receipt_fixture()).is_err());
    let d = drv("x");
    assert!(BuildRequest::new(vec![d.clone(), d], System::X8664Linux, receipt_fixture()).is_err());

    // BuildReport: built-with-no-outputs and duplicate outputs.
    assert!(BuildReport::new(BuildStatus::Built, vec![]).is_err());
    let p = store_path("x");
    assert!(BuildReport::new(BuildStatus::Built, vec![p.clone(), p]).is_err());

    // VerifyRequest: empty and duplicate paths.
    assert!(VerifyRequest::new(vec![], VerifyMode::Shallow).is_err());
    let p = store_path("x");
    assert!(VerifyRequest::new(vec![p.clone(), p], VerifyMode::Shallow).is_err());

    // VerifyReport / RepairReport: empty.
    assert!(VerifyReport::new(vec![]).is_err());
    assert!(RepairReport::new(vec![]).is_err());

    // GcReport: duplicate collected.
    let p = store_path("x");
    assert!(GcReport::new(GcStatus::Collected, vec![p.clone(), p], 1).is_err());
}

#[test]
fn build_request_enforces_target_count_bound_without_huge_allocations() {
    let receipt = receipt_fixture();

    // Exactly at the cap (1024) is accepted.
    let targets: Vec<DerivationPath> = (0..1024).map(|i| drv(&format!("t{i}"))).collect();
    assert!(BuildRequest::new(targets, System::X8664Linux, receipt.clone()).is_ok());

    // One over the cap is rejected with a bounded validation failure.
    let targets: Vec<DerivationPath> = (0..1025).map(|i| drv(&format!("t{i}"))).collect();
    let err = BuildRequest::new(targets, System::X8664Linux, receipt).unwrap_err();
    assert_eq!(err.code(), NixAdapterErrorCode::ValidationFailure);
}

#[test]
fn inconsistent_status_payload_combinations_rejected() {
    // Built requires nonempty outputs; AcquireNoBinary requires empty outputs.
    assert_eq!(
        BuildReport::new(BuildStatus::Built, vec![])
            .unwrap_err()
            .code(),
        NixAdapterErrorCode::ValidationFailure
    );
    assert_eq!(
        BuildReport::new(BuildStatus::AcquireNoBinary, vec![store_path("x")])
            .unwrap_err()
            .code(),
        NixAdapterErrorCode::ValidationFailure
    );

    // RefusedUnderLease requires empty collected and zero freed bytes.
    assert_eq!(
        GcReport::new(GcStatus::RefusedUnderLease, vec![store_path("x")], 0)
            .unwrap_err()
            .code(),
        NixAdapterErrorCode::ValidationFailure
    );
    assert_eq!(
        GcReport::new(GcStatus::RefusedUnderLease, vec![], 1)
            .unwrap_err()
            .code(),
        NixAdapterErrorCode::ValidationFailure
    );

    // PathInfo: closure smaller than nar; self reference; primary not an output.
    assert_eq!(
        PathInfoReport::new(store_path("x"), nar_hash(), vec![], vec![], None, 100, 50)
            .unwrap_err()
            .code(),
        NixAdapterErrorCode::ValidationFailure
    );
    let self_path = store_path("x");
    assert_eq!(
        PathInfoReport::new(
            self_path.clone(),
            nar_hash(),
            vec![],
            vec![self_path],
            None,
            1,
            1
        )
        .unwrap_err()
        .code(),
        NixAdapterErrorCode::ValidationFailure
    );
    let mut outputs = BTreeMap::new();
    outputs.insert(OutputName::new("out").unwrap(), store_path("other"));
    assert_eq!(
        RealizationReport::new(store_path("primary"), drv("d"), outputs)
            .unwrap_err()
            .code(),
        NixAdapterErrorCode::ValidationFailure
    );
}

// ===========================================================================
// Tests: collection amplification + duplicate-map-key hardening at the decode
// boundary. Each collection-bearing wire field enforces its count + checked
// total-byte cap DURING deserialization (before unbounded allocation or
// promotion), and the realization outputs map rejects duplicate JSON keys
// rather than last-wins. The public constructors remain defense-in-depth.
// ===========================================================================

/// Builds a comma-joined JSON string array: `mk(0)`,`mk(1)`,...,`mk(n-1)`.
fn json_string_array(n: usize, mk: impl Fn(usize) -> String) -> String {
    (0..n)
        .map(|i| format!("\"{}\"", mk(i)))
        .collect::<Vec<_>>()
        .join(",")
}

/// Builds a full PathInfoReport JSON object with the given signatures body
/// (a comma-joined list of quoted strings) and no references.
fn path_info_with_signatures(signatures: &str) -> Vec<u8> {
    format!(
        r#"{{"schemaVersion":1,"storePath":"/nix/store/{STORE_HASH}-hello-1.0","narHash":"{NAR}","narSize":1,"closureSize":1,"references":[],"signatures":[{signatures}]}}"#
    )
    .into_bytes()
}

/// Builds a full EvalRealizeRequest JSON object with the given explicit
/// `outputs` body (a comma-joined list of quoted strings).
fn eval_with_outputs(outputs: &str) -> Vec<u8> {
    format!(
        r#"{{"schemaVersion":1,"attribute":"ripgrep","system":"x86_64-linux","nixpkgsRevision":"{REV}","nixpkgsNarHash":"{NAR}","outputs":[{outputs}]}}"#
    )
    .into_bytes()
}

#[test]
fn decode_rejects_signatures_over_count_cap_during_visiting() {
    let c = JsonCodec::production();
    // 1024 minimal valid signatures (the exact cap) decode successfully.
    let at = json_string_array(1024, |i| format!("k{i}:BBBBBBBB"));
    assert!(PathInfoReport::decode(&c, &path_info_with_signatures(&at)).is_ok());

    // 1025 (cap+1) minimal valid signatures are rejected DURING decode, before
    // promotion, as a redacted MalformedPayload — the wire visitor stops at
    // max+1, so no unbounded allocation reaches the constructor.
    let over = json_string_array(1025, |i| format!("k{i}:BBBBBBBB"));
    let err = decode_err!(c, PathInfoReport, &path_info_with_signatures(&over));
    assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);
}

#[test]
fn decode_rejects_eval_outputs_over_count_cap_during_visiting() {
    let c = JsonCodec::production();
    // 1024 explicit output names (the exact cap) decode successfully.
    let at = json_string_array(1024, |i| format!("k{i}"));
    let back = EvalRealizeRequest::decode(&c, &eval_with_outputs(&at)).expect("at-cap decode");
    assert!(!back.outputs().is_default());

    // 1025 (cap+1) explicit output names are rejected DURING decode.
    let over = json_string_array(1025, |i| format!("k{i}"));
    let err = decode_err!(c, EvalRealizeRequest, &eval_with_outputs(&over));
    assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);
}

#[test]
fn eval_realize_constructor_enforces_output_count_cap() {
    // Regression: the public EvalRealizeRequest::new constructor enforces the
    // same MAX_EVAL_OUTPUTS (1024) count cap as the decode wire visitor —
    // closing the in-memory bypass where an oversized pkg-core
    // OutputSelection could be handed to the (formerly infallible)
    // constructor and then reach encode. At the exact cap the constructor
    // accepts; one over the cap it rejects with a bounded ValidationFailure
    // (and this rejection is by the constructor itself, not merely by decode).
    let mk = |n: usize| {
        let names: Vec<OutputName> = (0..n)
            .map(|i| OutputName::new(&format!("k{i}")).unwrap())
            .collect();
        EvalRealizeRequest::new(
            AttributePath::new("ripgrep").unwrap(),
            System::X8664Linux,
            nixpkgs_revision(),
            nar_hash(),
            OutputSelection::explicit(names).unwrap(),
        )
    };

    // Exactly at the cap is accepted by the public constructor.
    assert!(mk(1024).is_ok());

    // One over the cap is rejected by the public constructor.
    let err = mk(1025).unwrap_err();
    assert_eq!(err.code(), NixAdapterErrorCode::ValidationFailure);
}

#[test]
fn decode_rejects_duplicate_realization_output_keys() {
    let c = JsonCodec::production();
    // Two identical values for the same key "out"; the store path equals the
    // value, so last-wins semantics would ACCEPT this (a single "out" entry
    // naming the primary). Rejecting it proves duplicate-key detection rather
    // than silent last-wins.
    let p = format!("/nix/store/{STORE_HASH}-hello-1.0");
    let d = format!("/nix/store/{STORE_HASH}-hello-1.0.drv");
    let bad = format!(
        r#"{{"schemaVersion":1,"storePath":"{p}","deriver":"{d}","outputs":{{"out":"{p}","out":"{p}"}}}}"#
    );
    let err = decode_err!(c, RealizationReport, bad.as_bytes());
    assert_eq!(err.code(), NixAdapterErrorCode::MalformedPayload);
}

#[test]
fn bounded_collection_encodings_remain_byte_identical() {
    // The wrappers serialize in the existing wire shapes, so decode then
    // re-encode is byte-identical for every collection-bearing report (string
    // sequences, the unique string map, and the complex result sequences).
    let c = JsonCodec::production();

    let enc = path_info_fixture().encode().unwrap();
    assert_eq!(
        PathInfoReport::decode(&c, &enc).unwrap().encode().unwrap(),
        enc
    );

    let enc = realization_fixture().encode().unwrap();
    assert_eq!(
        RealizationReport::decode(&c, &enc)
            .unwrap()
            .encode()
            .unwrap(),
        enc
    );

    let enc = verify_report_fixture().encode().unwrap();
    assert_eq!(
        VerifyReport::decode(&c, &enc).unwrap().encode().unwrap(),
        enc
    );

    let enc = gc_report_collected().encode().unwrap();
    assert_eq!(GcReport::decode(&c, &enc).unwrap().encode().unwrap(), enc);
}

// ===========================================================================
// Tests: deterministic canonical ordering
// ===========================================================================

#[test]
fn path_info_canonicalizes_signatures_and_references() {
    let pi = PathInfoReport::new(
        store_path("x"),
        nar_hash(),
        vec![sig("m"), sig("k")],                       // unsorted
        vec![store_path("z-ref"), store_path("a-ref")], // unsorted
        None,
        10,
        100,
    )
    .unwrap();
    assert_eq!(pi.signatures()[0].as_str(), "k:BBBBBBBB");
    assert_eq!(pi.signatures()[1].as_str(), "m:BBBBBBBB");
    assert_eq!(pi.references()[0].name(), "a-ref");
    assert_eq!(pi.references()[1].name(), "z-ref");
}

#[test]
fn gc_report_canonicalizes_collected() {
    let g = GcReport::new(
        GcStatus::Collected,
        vec![store_path("z"), store_path("a")],
        10,
    )
    .unwrap();
    assert_eq!(g.collected()[0].name(), "a");
    assert_eq!(g.collected()[1].name(), "z");
}

#[test]
fn encode_is_deterministic_across_calls() {
    // Repeated encoding of the same value is byte-identical (sorted maps, sorted
    // and de-duplicated canonical collections).
    for _ in 0..3 {
        assert_eq!(
            path_info_fixture().encode().unwrap(),
            path_info_fixture().encode().unwrap()
        );
        assert_eq!(
            realization_fixture().encode().unwrap(),
            realization_fixture().encode().unwrap()
        );
        assert_eq!(
            gc_report_collected().encode().unwrap(),
            gc_report_collected().encode().unwrap()
        );
    }
}

// ===========================================================================
// Tests: RootName / RootRef traversal-safety matrix
// ===========================================================================

#[test]
fn root_name_rejects_unsafe_components() {
    for ok in ["gen-0007", "pkg", "a.b.c", "x_y-z.1"] {
        assert!(RootName::new(ok).is_ok(), "should accept {ok:?}");
    }
    // leading-dot, separators, control, traversal, overlength, non-ascii.
    for bad in [
        "",
        ".",
        "..",
        "...",
        ".hidden",
        ".x",
        "../etc",
        "a/b",
        "a\\b",
        "a b",
        "a;b",
        "a\0b",
        "café",
        &"a".repeat(129),
    ] {
        assert!(RootName::new(bad).is_err(), "should reject {bad:?}");
    }
}

#[test]
fn root_ref_rejects_non_canonical_managed_paths() {
    for ok in [
        "/nix/var/nix/gcroots/pkg/users/1001/gen-0007",
        "/nix/var/nix/gcroots/pkg/users/0/gen-0007",
        "/nix/var/nix/gcroots/pkg/users/4294967295/gen-0007",
        "/nix/var/nix/gcroots/pkg/users/1001/sub/gen-0007",
    ] {
        assert!(RootRef::new(ok).is_ok(), "should accept {ok:?}");
    }
    let huge = format!("/nix/var/nix/gcroots/pkg/users/1001/{}", "a".repeat(4100));
    for bad in [
        "",
        "relative/path",
        "/tmp/foo",
        // Wrong managed prefix.
        "/nix/var/nix/gcroots/other/users/1001/gen-0007",
        // Missing uid component.
        "/nix/var/nix/gcroots/pkg/users/gen-0007",
        // Non-numeric uid.
        "/nix/var/nix/gcroots/pkg/users/abc/gen-0007",
        // Leading-zero (non-canonical) uid.
        "/nix/var/nix/gcroots/pkg/users/01001/gen-0007",
        // Dot-dot traversal.
        "/nix/var/nix/gcroots/pkg/users/1001/../etc",
        // Bare dot component.
        "/nix/var/nix/gcroots/pkg/users/1001/.",
        // Repeated slash (empty component).
        "/nix/var/nix/gcroots/pkg/users/1001//gen-0007",
        // Trailing slash.
        "/nix/var/nix/gcroots/pkg/users/1001/gen-0007/",
        // Leading-dot rest component.
        "/nix/var/nix/gcroots/pkg/users/1001/.hidden",
        // Control byte.
        "/nix/var/nix/gcroots/pkg/users/1001/a\0b",
        // Overlength.
        huge.as_str(),
    ] {
        assert!(RootRef::new(bad).is_err(), "should reject {bad:?}");
    }
}

// ===========================================================================
// Tests: bounded/redacted errors
// ===========================================================================

#[test]
fn adapter_errors_are_bounded_and_redacted() {
    // The closed code set is stable and non-empty.
    for code in [
        NixAdapterErrorCode::UnexpectedCall,
        NixAdapterErrorCode::OversizedInput,
        NixAdapterErrorCode::MalformedPayload,
        NixAdapterErrorCode::UnsupportedSchemaVersion,
        NixAdapterErrorCode::UnsupportedUpstreamFormat,
        NixAdapterErrorCode::ValidationFailure,
        NixAdapterErrorCode::Timeout,
        NixAdapterErrorCode::Unavailable,
        NixAdapterErrorCode::TrustFailure,
        NixAdapterErrorCode::IntegrityFailure,
        NixAdapterErrorCode::PermissionDenied,
        NixAdapterErrorCode::OperationFailed,
    ] {
        assert!(!code.as_str().is_empty());
        assert_eq!(code.to_string(), code.as_str());
    }

    // unexpected_call takes only the expected/actual MethodKind — no free text.
    // The summary is a crate-owned static string selected by the constructor:
    // "method mismatch" when the kinds differ.
    let e = NixAdapterError::unexpected_call(MethodKind::Substitute, MethodKind::Build);
    assert_eq!(e.expected_method(), Some(MethodKind::Substitute));
    assert_eq!(e.actual_method(), Some(MethodKind::Build));
    assert_eq!(e.mismatch_summary(), Some("method mismatch"));
    assert!(e.mismatch_summary().unwrap().len() <= BoundedSummary::MAX);
    // Display carries the method names and the static summary only.
    assert!(e.to_string().contains("substitute"));
    assert!(e.to_string().contains("build"));

    // "request mismatch" when the kinds are equal.
    let e = NixAdapterError::unexpected_call(MethodKind::Verify, MethodKind::Verify);
    assert_eq!(e.mismatch_summary(), Some("request mismatch"));

    // The four new coarse operation-failure categories are data-free: they
    // carry no payload and map to their own stable codes.
    for e in [
        NixAdapterError::TrustFailure,
        NixAdapterError::IntegrityFailure,
        NixAdapterError::PermissionDenied,
        NixAdapterError::OperationFailed,
    ] {
        assert!(!e.to_string().is_empty());
    }
    assert_eq!(
        NixAdapterError::TrustFailure.code(),
        NixAdapterErrorCode::TrustFailure
    );
    assert_eq!(
        NixAdapterError::IntegrityFailure.code(),
        NixAdapterErrorCode::IntegrityFailure
    );
    assert_eq!(
        NixAdapterError::PermissionDenied.code(),
        NixAdapterErrorCode::PermissionDenied
    );
    assert_eq!(
        NixAdapterError::OperationFailed.code(),
        NixAdapterErrorCode::OperationFailed
    );

    // OversizedInput carries only the limit, never the payload.
    let e = NixAdapterError::OversizedInput { limit_bytes: 777 };
    match e {
        NixAdapterError::OversizedInput { limit_bytes } => assert_eq!(limit_bytes, 777),
        other => panic!("expected OversizedInput, got {other}"),
    }
    assert_eq!(e.code(), NixAdapterErrorCode::OversizedInput);

    // MalformedPayload carries only a redacted kind, never bytes.
    let e = NixAdapterError::MalformedPayload {
        kind: MalformedKind::ExcessiveNesting,
    };
    assert_eq!(e.code(), NixAdapterErrorCode::MalformedPayload);
    assert_eq!(malformed_kind(&e), Some(MalformedKind::ExcessiveNesting));
}

// ===========================================================================
// Tests: substitute outcome is normal-only; BuildApprovalReceipt is opaque
// ===========================================================================

#[test]
fn substitute_outcomes_are_normal_only_failures_are_errors() {
    // The closed SubstituteOutcome enum contains ONLY normal cache outcomes;
    // there is no trust/signature-failure variant. A trust or signature failure
    // is represented solely as Err(NixAdapterError). This exhaustive match
    // fails to compile if a failure variant is ever added.
    fn is_normal(o: SubstituteOutcome) -> bool {
        match o {
            SubstituteOutcome::Fetched
            | SubstituteOutcome::AbsentFromSubstituters
            | SubstituteOutcome::NoBinaryAvailable => true,
        }
    }
    for o in [
        SubstituteOutcome::Fetched,
        SubstituteOutcome::AbsentFromSubstituters,
        SubstituteOutcome::NoBinaryAvailable,
    ] {
        assert!(is_normal(o));
    }

    // And the only failure path is the trait's Result error.
    let a: Arc<dyn NixAdapter> = Arc::new(UnavailableStub);
    let err = a.substitute(&store_path("x")).unwrap_err();
    assert_eq!(err.code(), NixAdapterErrorCode::Unavailable);
}

#[test]
fn build_approval_receipt_is_opaque_bounded_operation_id() {
    // OperationId is bounded and validated: nonempty, ≤64 bytes, [A-Za-z0-9_-].
    assert!(OperationId::new("op-0001").is_ok());
    assert!(OperationId::new("").is_err());
    assert!(OperationId::new(&"a".repeat(65)).is_err());
    assert!(OperationId::new("bad id").is_err());
    assert!(OperationId::new("dotted.id").is_err());

    // The receipt exposes only the operation id.
    let r = BuildApprovalReceipt::new(OperationId::new("op-0001").unwrap());
    assert_eq!(r.operation_id().as_str(), "op-0001");

    // Its wire shape is exactly {"operationId":"..."} and carries no knobs.
    let encoded = build_request_fixture().encode().unwrap();
    let s = as_str(&encoded);
    assert!(s.contains("\"receipt\":{\"operationId\":\"op-0001\"}"));
    for knob in [
        "sandbox",
        "substituters",
        "trustedPublicKeys",
        "maxJobs",
        "builders",
        "expr",
    ] {
        assert!(
            !s.contains(knob),
            "build request exposes forbidden knob {knob}: {s}"
        );
    }
}
