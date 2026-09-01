//! Tests for the `broker` module.

use super::*;
use pkg_core::{
    SelectorId, SelectorInput, SourceRevision, VersionPreference, channel::ChannelSequence,
};
use pkg_installer::serve_broker_connection_with_nix;
use pkg_nix::{
    AcceptedFormats, AttributePath, BuildApprovalReceipt, ChannelRefreshErrorCode, DerivationPath,
    DerivedOutputTarget, Digest, EvaluatedDerivation, FormatVersion, GcStatus, GenerationId,
    InProcessBroker, InProcessCallerPeer, NarHash, NarIntegrity, NixAdapter, NixAdapterError,
    NixVersion, NixpkgsRevision, OperationId, OutputName, OutputSelection, PackageVersion,
    PathVerifyResult, PolicyVersion, RootName, RootSetEntry, Signature, SubstituteReceipt, System,
    TrustStatus, VerifyMode, VersionInfo,
};
use pkg_testkit::FakeNix;
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, mpsc},
    thread,
};

const STORE_HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";
const NAR: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
const REV: &str = "0123456789abcdef0123456789abcdef01234567";

fn store_path(name: &str) -> StorePath {
    StorePath::new(&format!("/nix/store/{STORE_HASH}-{name}")).unwrap()
}

fn drv(name: &str) -> DerivationPath {
    DerivationPath::from_str(&format!("/nix/store/{STORE_HASH}-{name}.drv")).unwrap()
}

fn nar_hash() -> NarHash {
    NarHash::new(NAR).unwrap()
}

fn version_info() -> VersionInfo {
    VersionInfo::new(
        NixVersion::new("2.34.8").unwrap(),
        AcceptedFormats::new(FormatVersion::new(1).unwrap()),
    )
}

fn eval_request() -> EvaluateDerivationRequest {
    EvaluateDerivationRequest::new(
        AttributePath::new("hello").unwrap(),
        System::X8664Linux,
        NixpkgsRevision::new(REV).unwrap(),
        nar_hash(),
        OutputSelection::default_selection(),
    )
    .unwrap()
}

fn derivation_plan() -> DerivationPlanReport {
    let root = drv("hello-1.0");
    let mut outputs = BTreeMap::new();
    outputs.insert(OutputName::new("out").unwrap(), store_path("hello-1.0"));
    let evaluated = EvaluatedDerivation::new(
        root.clone(),
        "hello-1.0".into(),
        System::X8664Linux,
        outputs,
        Digest::from_bytes([1; 32]),
        false,
    )
    .unwrap();
    DerivationPlanReport::new(
        4,
        root,
        vec![OutputName::new("out").unwrap()],
        vec![evaluated],
        Digest::from_bytes([2; 32]),
        "hello".into(),
        PackageVersion::new("1.0"),
    )
    .unwrap()
}

fn path_info_report() -> PathInfoReport {
    PathInfoReport::new(
        store_path("hello-1.0"),
        nar_hash(),
        vec![Signature::new("cache:BBBBBBBB").unwrap()],
        vec![],
        Some(drv("hello-1.0")),
        1024,
        4096,
    )
    .unwrap()
}

fn substitute_report() -> SubstituteReport {
    SubstituteReport::fetched(
        store_path("hello-1.0"),
        SubstituteReceipt::new(
            "https://cache.nixos.org",
            nar_hash(),
            vec![Signature::new("cache:BBBBBBBB").unwrap()],
        )
        .unwrap(),
    )
}

fn verify_request() -> VerifyRequest {
    VerifyRequest::new(vec![store_path("hello-1.0")], VerifyMode::Recursive).unwrap()
}

fn verify_report() -> VerifyReport {
    VerifyReport::new(vec![PathVerifyResult::new(
        store_path("hello-1.0"),
        NarIntegrity::Intact,
        TrustStatus::Trusted,
    )])
    .unwrap()
}

fn gc_report() -> GcReport {
    GcReport::new(
        GcStatus::Collected,
        vec![store_path("unreachable-1")],
        12_345,
    )
    .unwrap()
}

fn build_request() -> BuildRequest {
    BuildRequest::new(
        vec![
            DerivedOutputTarget::new(drv("hello-1.0"), vec![OutputName::new("out").unwrap()])
                .unwrap(),
        ],
        System::X8664Linux,
        BuildApprovalReceipt::new(
            OperationId::new("op-0001").unwrap(),
            Digest::from_bytes([0x42; 32]),
            PolicyVersion::from_u64(7).unwrap(),
        ),
    )
    .unwrap()
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "pkg-broker-client-{}-{:?}",
            std::process::id(),
            thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn real_transport_round_trips_all_exposed_typed_calls_and_cleanup() -> Result<(), Box<dyn Error>> {
    let broker = InProcessBroker::new()?;
    let scratch = Scratch::new()?;
    let socket = scratch.0.join("broker.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket)?;
    let server_broker = Arc::clone(&broker);
    let expected_version = version_info();
    let expected_eval_request = eval_request();
    let expected_plan = derivation_plan();
    let expected_path = store_path("hello-1.0");
    let expected_path_info = path_info_report();
    let expected_substitute = substitute_report();
    let expected_verify_request = verify_request();
    let expected_verify = verify_report();
    let expected_gc = gc_report();
    let fake = Arc::new(FakeNix::new());
    fake.expect_version(Ok(expected_version.clone()))
        .expect_evaluate_derivation(expected_eval_request.clone(), Ok(expected_plan.clone()))
        .expect_path_info(expected_path.clone(), Ok(expected_path_info.clone()))
        .expect_substitute(expected_path.clone(), Ok(expected_substitute.clone()))
        .expect_verify(expected_verify_request.clone(), Ok(expected_verify.clone()))
        .expect_gc(Ok(expected_gc.clone()))
        .expect_version(Err(NixAdapterError::TrustFailure))
        .expect_version(Ok(expected_version.clone()));
    let server_adapter: Arc<dyn NixAdapter> = fake.clone();
    let worker = thread::spawn(move || {
        let (server, _) = listener.accept()?;
        serve_broker_connection_with_nix(server, &server_broker, &server_adapter)
            .map_err(|error| io::Error::other(error.to_string()))
    });
    let mut client = BrokerLifecycleClient::connect(&socket)?;

    let resolve_handle = client.begin(BrokerOperationKind::Resolve)?;
    assert_eq!(client.version(resolve_handle.clone())?, expected_version);
    assert_eq!(
        client.poll(resolve_handle.clone())?,
        OperationStatus::Running
    );
    assert_eq!(
        client.evaluate_derivation(resolve_handle.clone(), expected_eval_request)?,
        expected_plan
    );
    assert_eq!(
        client.path_info(resolve_handle.clone(), expected_path.clone())?,
        expected_path_info
    );
    client.cancel(resolve_handle)?;

    let acquire_handle = client.begin(BrokerOperationKind::Acquire)?;
    assert_eq!(
        client.substitute(acquire_handle.clone(), expected_path)?,
        expected_substitute
    );
    assert_eq!(
        client.verify(acquire_handle.clone(), expected_verify_request)?,
        expected_verify
    );
    client.cancel(acquire_handle)?;

    let gc_handle = client.begin(BrokerOperationKind::Gc)?;
    assert_eq!(client.gc(gc_handle.clone())?, expected_gc);
    client.cancel(gc_handle)?;

    let doctor_handle = client.begin(BrokerOperationKind::Doctor)?;
    let failure = client.version(doctor_handle.clone()).unwrap_err();
    assert_eq!(failure.code(), BrokerClientErrorCode::AdapterFailure);
    assert_eq!(
        failure.adapter_code(),
        Some(NixAdapterErrorCode::TrustFailure)
    );
    assert_eq!(client.version(doctor_handle.clone())?, expected_version);
    client.cancel(doctor_handle)?;

    let activate_handle = client.begin(BrokerOperationKind::Activate)?;
    let transition = RootSetTransitionIntent::new(
        pkg_nix::GenerationId::new("gen-0007")?,
        pkg_nix::GenerationId::new("gen-0008")?,
        vec![pkg_nix::RootName::new("hello-out")?],
    )?;
    let failure = client
        .transition_generation_roots(activate_handle.clone(), transition)
        .unwrap_err();
    assert_eq!(
        failure.code(),
        BrokerClientErrorCode::GenerationRootTransitionRefused
    );
    assert_eq!(
        failure.generation_root_transition_code(),
        Some(GenerationRootTransitionErrorCode::AuthorityUnavailable)
    );
    client.complete(activate_handle)?;
    client.stream.shutdown(Shutdown::Write)?;
    worker
        .join()
        .map_err(|_| io::Error::other("broker worker panicked"))??;
    let snapshot = broker.admission_snapshot();
    assert!(!snapshot.build_held());
    assert_eq!(snapshot.gc_inhibitor_count(), 0);
    fake.assert_exhausted()?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn broker_nix_adapter_proxies_safe_calls_and_refuses_build() -> Result<(), Box<dyn Error>> {
    let broker = InProcessBroker::new()?;
    let scratch = Scratch::new()?;
    let socket = scratch.0.join("broker.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket)?;
    let server_broker = Arc::clone(&broker);
    let expected_version = version_info();
    let expected_eval_request = eval_request();
    let expected_plan = derivation_plan();
    let expected_path = store_path("hello-1.0");
    let expected_path_info = path_info_report();
    let expected_substitute = substitute_report();
    let expected_verify_request = verify_request();
    let expected_verify = verify_report();
    let expected_gc = gc_report();
    let fake = Arc::new(FakeNix::new());
    fake.expect_version(Ok(expected_version.clone()))
        .expect_evaluate_derivation(expected_eval_request.clone(), Ok(expected_plan.clone()))
        .expect_path_info(expected_path.clone(), Ok(expected_path_info.clone()))
        .expect_substitute(expected_path.clone(), Ok(expected_substitute.clone()))
        .expect_verify(expected_verify_request.clone(), Ok(expected_verify.clone()))
        .expect_gc(Ok(expected_gc.clone()))
        .expect_version(Err(NixAdapterError::TrustFailure));
    let server_adapter: Arc<dyn NixAdapter> = fake.clone();
    let worker = thread::spawn(move || -> Result<(), io::Error> {
        for _ in 0..7 {
            let (server, _) = listener.accept()?;
            serve_broker_connection_with_nix(server, &server_broker, &server_adapter)
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        Ok(())
    });
    let adapter = BrokerNixAdapter::at(socket);

    assert_eq!(adapter.version()?, expected_version);
    assert_eq!(
        adapter.evaluate_derivation(&expected_eval_request)?,
        expected_plan
    );
    assert_eq!(adapter.path_info(&expected_path)?, expected_path_info);
    assert_eq!(adapter.substitute(&expected_path)?, expected_substitute);
    assert_eq!(adapter.verify(&expected_verify_request)?, expected_verify);
    assert_eq!(adapter.gc()?, expected_gc);
    assert_eq!(
        adapter.build(&build_request()).unwrap_err().code(),
        NixAdapterErrorCode::PermissionDenied
    );
    assert_eq!(
        adapter.version().unwrap_err().code(),
        NixAdapterErrorCode::TrustFailure
    );

    worker
        .join()
        .map_err(|_| io::Error::other("broker worker panicked"))??;
    let snapshot = broker.admission_snapshot();
    assert!(!snapshot.build_held());
    assert_eq!(snapshot.gc_inhibitor_count(), 0);
    fake.assert_exhausted()?;
    Ok(())
}

#[test]
fn build_preparation_refusal_is_typed_and_keeps_the_connection_usable() -> Result<(), Box<dyn Error>>
{
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Build)?;
    let selector = PackageSelector::new(
        SelectorId::new("sel_hello")?,
        SelectorInput::new("hello")?,
        VersionPreference::Any,
        OutputSelection::default_selection(),
        SourceRevision::CurrentChannel,
    );
    server.write_all(&ProductFrameCodec::encode_cli_response(
        1,
        &CliBrokerResponse::BuildPreparationRefused(BuildPreparationErrorCode::PlanningRefused),
    )?)?;
    server.write_all(&ProductFrameCodec::encode_cli_response(
        2,
        &CliBrokerResponse::Status(OperationStatus::Running),
    )?)?;
    let mut client = BrokerLifecycleClient::from_stream(client);

    let error = client
        .prepare_build(handle.clone(), vec![selector])
        .unwrap_err();
    assert_eq!(error.code(), BrokerClientErrorCode::BuildPreparationRefused);
    assert_eq!(
        error.build_preparation_code(),
        Some(BuildPreparationErrorCode::PlanningRefused)
    );
    assert_eq!(client.poll(handle)?, OperationStatus::Running);
    assert!(client.healthy);
    Ok(())
}

#[test]
fn build_refusal_is_typed_and_keeps_the_connection_usable() -> Result<(), Box<dyn Error>> {
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Build)?;
    let digest = Digest::from_bytes([0x33; 32]);
    server.write_all(&ProductFrameCodec::encode_cli_response(
        1,
        &CliBrokerResponse::BuildExecutionRefused(BuildExecutionErrorCode::ResourcePreflightFailed),
    )?)?;
    let mut client = BrokerLifecycleClient::from_stream(client);

    let error = client.execute_build(handle.clone(), digest).unwrap_err();
    assert_eq!(error.code(), BrokerClientErrorCode::BuildRefused);
    assert_eq!(
        error.build_execution_code(),
        Some(BuildExecutionErrorCode::ResourcePreflightFailed)
    );
    assert!(client.healthy);
    let request = read_frame(
        &mut server,
        Instant::now()
            .checked_add(RESPONSE_TIMEOUT)
            .ok_or_else(|| io::Error::other("deadline overflow"))?,
    )?;
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&request)?,
        (1, CliBrokerRequest::ExecuteBuild(handle, digest))
    );
    Ok(())
}

#[test]
fn build_progress_is_streamed_before_a_typed_refusal() -> Result<(), Box<dyn Error>> {
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Build)?;
    let digest = Digest::from_bytes([0x34; 32]);
    for millionths in [250_000, 750_000] {
        server.write_all(&ProductFrameCodec::encode_cli_response(
            1,
            &CliBrokerResponse::BuildExecutionProgress(BuildProgressEstimate::new(millionths)?),
        )?)?;
    }
    server.write_all(&ProductFrameCodec::encode_cli_response(
        1,
        &CliBrokerResponse::BuildExecutionRefused(BuildExecutionErrorCode::ExecutionFailed),
    )?)?;
    let mut client = BrokerLifecycleClient::from_stream(client);
    let mut observed = Vec::new();

    let error = client
        .execute_build_with_progress(handle, digest, &mut |estimate| {
            observed.push(estimate.millionths());
            Ok(())
        })
        .unwrap_err();
    assert_eq!(observed, vec![250_000, 750_000]);
    assert_eq!(error.code(), BrokerClientErrorCode::BuildRefused);
    assert!(client.healthy);
    Ok(())
}

#[test]
fn regressing_build_progress_poisons_the_connection() -> Result<(), Box<dyn Error>> {
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Build)?;
    let digest = Digest::from_bytes([0x35; 32]);
    for millionths in [500_000, 250_000] {
        server.write_all(&ProductFrameCodec::encode_cli_response(
            1,
            &CliBrokerResponse::BuildExecutionProgress(BuildProgressEstimate::new(millionths)?),
        )?)?;
    }
    let mut client = BrokerLifecycleClient::from_stream(client);

    assert_eq!(
        client
            .execute_build(handle.clone(), digest)
            .unwrap_err()
            .code(),
        BrokerClientErrorCode::UnexpectedResponse
    );
    assert!(!client.healthy);
    assert_eq!(
        client.execute_build(handle, digest).unwrap_err().code(),
        BrokerClientErrorCode::ConnectionFailed
    );
    Ok(())
}

#[test]
fn cache_acquisition_refusal_is_typed_and_keeps_the_connection_usable() -> Result<(), Box<dyn Error>>
{
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Acquire)?;
    let selector = PackageSelector::new(
        SelectorId::new("sel_hello")?,
        SelectorInput::new("hello")?,
        VersionPreference::Any,
        OutputSelection::default_selection(),
        SourceRevision::CurrentChannel,
    );
    server.write_all(&ProductFrameCodec::encode_cli_response(
        1,
        &CliBrokerResponse::InstallAcquisitionRefused(CacheInstallErrorCode::AuthorityUnavailable),
    )?)?;
    let mut client = BrokerLifecycleClient::from_stream(client);

    let error = client
        .acquire_install(handle.clone(), vec![selector.clone()])
        .unwrap_err();
    assert_eq!(
        error.code(),
        BrokerClientErrorCode::InstallAcquisitionRefused
    );
    assert_eq!(
        error.cache_install_code(),
        Some(CacheInstallErrorCode::AuthorityUnavailable)
    );
    assert!(client.healthy);
    let request = read_frame(
        &mut server,
        Instant::now()
            .checked_add(RESPONSE_TIMEOUT)
            .ok_or_else(|| io::Error::other("deadline overflow"))?,
    )?;
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&request)?,
        (1, CliBrokerRequest::AcquireInstall(handle, vec![selector]))
    );
    Ok(())
}

#[test]
fn contradictory_download_progress_poisons_the_connection() -> Result<(), Box<dyn Error>> {
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Acquire)?;
    let selector = PackageSelector::new(
        SelectorId::new("sel_hello")?,
        SelectorInput::new("hello")?,
        VersionPreference::Any,
        OutputSelection::default_selection(),
        SourceRevision::CurrentChannel,
    );
    let started = CliBrokerResponse::InstallDownloadProgress(InstallDownloadProgress::new(
        SelectorInput::new("hello")?,
        0,
        42,
    )?);
    server.write_all(&ProductFrameCodec::encode_cli_response(1, &started)?)?;
    server.write_all(&ProductFrameCodec::encode_cli_response(1, &started)?)?;
    let mut client = BrokerLifecycleClient::from_stream(client);

    let error = client
        .acquire_install(handle.clone(), vec![selector.clone()])
        .unwrap_err();
    assert_eq!(error.code(), BrokerClientErrorCode::UnexpectedResponse);
    assert!(!client.healthy);
    assert_eq!(
        client
            .acquire_install(handle.clone(), vec![selector.clone()])
            .unwrap_err()
            .code(),
        BrokerClientErrorCode::ConnectionFailed
    );
    let request = read_frame(
        &mut server,
        Instant::now()
            .checked_add(RESPONSE_TIMEOUT)
            .ok_or_else(|| io::Error::other("deadline overflow"))?,
    )?;
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&request)?,
        (1, CliBrokerRequest::AcquireInstall(handle, vec![selector]))
    );
    Ok(())
}

#[test]
fn root_publication_refusal_is_typed_and_keeps_the_connection_usable() -> Result<(), Box<dyn Error>>
{
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Build)?;
    server.write_all(&ProductFrameCodec::encode_cli_response(
        1,
        &CliBrokerResponse::BuildRootPublicationRefused(BuildRootPublicationErrorCode::Cancelled),
    )?)?;
    server.write_all(&ProductFrameCodec::encode_cli_response(
        2,
        &CliBrokerResponse::Status(OperationStatus::Running),
    )?)?;
    let intent = RootSetIntent::new(
        GenerationId::new("gen-0007")?,
        vec![RootSetEntry::new(
            RootName::new("hello-out")?,
            store_path("hello-1.0"),
        )],
    )?;
    let mut client = BrokerLifecycleClient::from_stream(client);

    let error = client
        .publish_build_roots(handle.clone(), intent.clone())
        .unwrap_err();
    assert_eq!(error.code(), BrokerClientErrorCode::BuildRootRefused);
    assert_eq!(
        error.build_root_code(),
        Some(BuildRootPublicationErrorCode::Cancelled)
    );
    assert!(client.healthy);
    assert_eq!(client.poll(handle.clone())?, OperationStatus::Running);

    let deadline = Instant::now()
        .checked_add(RESPONSE_TIMEOUT)
        .ok_or_else(|| io::Error::other("deadline overflow"))?;
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&read_frame(&mut server, deadline)?)?,
        (
            1,
            CliBrokerRequest::PublishBuildRoots(handle.clone(), intent)
        )
    );
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&read_frame(&mut server, deadline)?)?,
        (2, CliBrokerRequest::Poll(handle))
    );
    Ok(())
}

#[test]
fn root_removal_refusal_is_typed_and_keeps_the_connection_usable() -> Result<(), Box<dyn Error>> {
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Gc)?;
    server.write_all(&ProductFrameCodec::encode_cli_response(
        1,
        &CliBrokerResponse::GenerationRootRemovalRefused(
            GenerationRootRemovalErrorCode::RemovalFailed,
        ),
    )?)?;
    server.write_all(&ProductFrameCodec::encode_cli_response(
        2,
        &CliBrokerResponse::Status(OperationStatus::Running),
    )?)?;
    let generation = GenerationId::new("gen-0007")?;
    let mut client = BrokerLifecycleClient::from_stream(client);

    let error = client
        .remove_generation_roots(handle.clone(), generation.clone())
        .unwrap_err();
    assert_eq!(
        error.code(),
        BrokerClientErrorCode::GenerationRootRemovalRefused
    );
    assert_eq!(
        error.generation_root_removal_code(),
        Some(GenerationRootRemovalErrorCode::RemovalFailed)
    );
    assert!(client.healthy);
    assert_eq!(client.poll(handle.clone())?, OperationStatus::Running);

    let deadline = Instant::now()
        .checked_add(RESPONSE_TIMEOUT)
        .ok_or_else(|| io::Error::other("deadline overflow"))?;
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&read_frame(&mut server, deadline)?)?,
        (
            1,
            CliBrokerRequest::RemoveGenerationRoots(handle.clone(), generation)
        )
    );
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&read_frame(&mut server, deadline)?)?,
        (2, CliBrokerRequest::Poll(handle))
    );
    Ok(())
}

#[test]
fn root_attestation_refusal_is_typed_and_keeps_the_connection_usable() -> Result<(), Box<dyn Error>>
{
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Activate)?;
    server.write_all(&ProductFrameCodec::encode_cli_response(
        1,
        &CliBrokerResponse::GenerationRootAttestationRefused(
            GenerationRootAttestationErrorCode::AttestationFailed,
        ),
    )?)?;
    server.write_all(&ProductFrameCodec::encode_cli_response(
        2,
        &CliBrokerResponse::Status(OperationStatus::Running),
    )?)?;
    let generation = GenerationId::new("gen-0007")?;
    let mut client = BrokerLifecycleClient::from_stream(client);

    let error = client
        .attest_generation_roots(handle.clone(), generation.clone())
        .unwrap_err();
    assert_eq!(
        error.code(),
        BrokerClientErrorCode::GenerationRootAttestationRefused
    );
    assert_eq!(
        error.generation_root_attestation_code(),
        Some(GenerationRootAttestationErrorCode::AttestationFailed)
    );
    assert!(client.healthy);
    assert_eq!(client.poll(handle.clone())?, OperationStatus::Running);

    let deadline = Instant::now()
        .checked_add(RESPONSE_TIMEOUT)
        .ok_or_else(|| io::Error::other("deadline overflow"))?;
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&read_frame(&mut server, deadline)?)?,
        (
            1,
            CliBrokerRequest::AttestGenerationRoots(handle.clone(), generation)
        )
    );
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&read_frame(&mut server, deadline)?)?,
        (2, CliBrokerRequest::Poll(handle))
    );
    Ok(())
}

#[test]
fn explicit_gc_admission_round_trips() -> Result<(), Box<dyn Error>> {
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Gc)?;
    server.write_all(&ProductFrameCodec::encode_cli_response(
        1,
        &CliBrokerResponse::GcAdmissionAcquired,
    )?)?;
    let mut client = BrokerLifecycleClient::from_stream(client);

    client.acquire_gc(handle.clone())?;
    let deadline = Instant::now()
        .checked_add(RESPONSE_TIMEOUT)
        .ok_or_else(|| io::Error::other("deadline overflow"))?;
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&read_frame(&mut server, deadline)?)?,
        (1, CliBrokerRequest::AcquireGc(handle))
    );
    Ok(())
}

#[test]
fn repair_generation_round_trips_only_path_free_intent_and_count() -> Result<(), Box<dyn Error>> {
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Repair)?;
    let request = RepairGenerationRequest::new(GenerationId::new("gen-0042")?, true);
    let report = RepairGenerationReport::new(pkg_nix::RepairGenerationStatus::DamageDetected, 2)?;
    server.write_all(&ProductFrameCodec::encode_cli_response(
        1,
        &CliBrokerResponse::RepairGeneration(report.clone()),
    )?)?;
    let mut client = BrokerLifecycleClient::from_stream(client);

    assert_eq!(
        client.repair_generation(handle.clone(), request.clone())?,
        report
    );
    let deadline = Instant::now()
        .checked_add(RESPONSE_TIMEOUT)
        .ok_or_else(|| io::Error::other("deadline overflow"))?;
    let frame = read_frame(&mut server, deadline)?;
    assert!(!String::from_utf8_lossy(&frame).contains("/nix/"));
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&frame)?,
        (1, CliBrokerRequest::RepairGeneration(handle, request))
    );
    Ok(())
}

#[test]
fn channel_refresh_round_trips_a_sanitized_report() -> Result<(), Box<dyn Error>> {
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Refresh)?;
    let sequence = ChannelSequence::from_u64(43)
        .ok_or_else(|| io::Error::other("invalid test channel sequence"))?;
    let report = ChannelRefreshReport::new(true, sequence);
    server.write_all(&ProductFrameCodec::encode_cli_response(
        1,
        &CliBrokerResponse::ChannelRefreshed(report),
    )?)?;
    let mut client = BrokerLifecycleClient::from_stream(client);

    assert_eq!(
        client.refresh_channel(handle.clone(), pkg_nix::ChannelRefreshMode::Check)?,
        report
    );
    let deadline = Instant::now()
        .checked_add(RESPONSE_TIMEOUT)
        .ok_or_else(|| io::Error::other("deadline overflow"))?;
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&read_frame(&mut server, deadline)?)?,
        (
            1,
            CliBrokerRequest::RefreshChannel(handle, pkg_nix::ChannelRefreshMode::Check)
        )
    );
    Ok(())
}

#[test]
fn channel_refresh_refusal_is_typed_and_keeps_the_connection_usable() -> Result<(), Box<dyn Error>>
{
    let (mut server, client) = UnixStream::pair()?;
    let handle = InProcessBroker::new()?
        .connect(InProcessCallerPeer::authenticated(1001))?
        .begin(BrokerOperationKind::Refresh)?;
    server.write_all(&ProductFrameCodec::encode_cli_response(
        1,
        &CliBrokerResponse::ChannelRefreshRefused(ChannelRefreshErrorCode::Verification),
    )?)?;
    let mut client = BrokerLifecycleClient::from_stream(client);

    let error = client
        .refresh_channel(handle.clone(), pkg_nix::ChannelRefreshMode::Force)
        .unwrap_err();
    assert_eq!(
        error.code(),
        BrokerClientErrorCode::ChannelRefreshVerification
    );
    assert!(client.healthy);
    let deadline = Instant::now()
        .checked_add(RESPONSE_TIMEOUT)
        .ok_or_else(|| io::Error::other("deadline overflow"))?;
    assert_eq!(
        ProductFrameCodec::decode_cli_request(&read_frame(&mut server, deadline)?)?,
        (
            1,
            CliBrokerRequest::RefreshChannel(handle, pkg_nix::ChannelRefreshMode::Force)
        )
    );
    Ok(())
}

#[test]
fn mismatched_response_poisoning_prevents_stream_reuse() -> Result<(), Box<dyn Error>> {
    let (mut server, client) = UnixStream::pair()?;
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let worker = thread::spawn(move || -> Result<(), io::Error> {
        let deadline = Instant::now()
            .checked_add(RESPONSE_TIMEOUT)
            .ok_or_else(|| io::Error::other("deadline overflow"))?;
        let _ = read_frame(&mut server, deadline);
        server.write_all(
            &ProductFrameCodec::encode_cli_response(999, &CliBrokerResponse::Cancelled)
                .map_err(io::Error::other)?,
        )?;
        release_rx
            .recv()
            .map_err(|_| io::Error::other("client dropped before release"))
    });
    let mut client = BrokerLifecycleClient::from_stream(client);
    assert_eq!(
        client
            .begin(BrokerOperationKind::Resolve)
            .map_err(super::BrokerClientError::code),
        Err(BrokerClientErrorCode::UnexpectedResponse)
    );
    assert_eq!(
        client
            .begin(BrokerOperationKind::Resolve)
            .map_err(super::BrokerClientError::code),
        Err(BrokerClientErrorCode::ConnectionFailed)
    );
    release_tx.send(())?;
    worker
        .join()
        .map_err(|_| io::Error::other("fake broker panicked"))??;
    Ok(())
}
