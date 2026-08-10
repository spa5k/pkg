//! Ignored-by-default parity smoke against the product-pinned Nix 2.34.8 CLI.
//!
//! The nightly lane supplies an isolated store and explicit binary/HOME paths.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::str::FromStr;

use pkg_core::{
    NixpkgsRevision, OutputSelection, PolicyVersion, identity::NarHash, selector::AttributePath,
    state::body_digest,
};
use pkg_nix::{
    BuildApprovalReceipt, BuildOutputProvenance, BuildRequest, DerivedOutputTarget,
    EvaluateDerivationRequest, GcStatus, GenerationId, InProcessHelper, InProcessPeer,
    MaintenanceAdapter, NarIntegrity, NixAdapter, OperationId, RealNixAdapter, RepairMode,
    RepairOutcomeKind, RepairStorePathsRequest, RootNixRepairExecutor, RootSet, RootSetEntry,
    StorePath, SubstituteOutcome, System, TrustStatus, VerifiedRepairScope, VerifyMode,
    VerifyRequest,
};

const REVISION: &str = "a62e6edd6d5e1fa0329b8653c801147986f8d446";
const NAR_HASH: &str = "sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=";

#[test]
#[ignore = "requires an isolated product-pinned Nix 2.34.8 store"]
fn real_nix_matches_the_normalized_adapter_contract() -> Result<(), Box<dyn std::error::Error>> {
    let binary = std::env::var_os("PKG_REAL_NIX_BIN").ok_or("PKG_REAL_NIX_BIN is required")?;
    let home = std::env::var_os("PKG_REAL_NIX_HOME").ok_or("PKG_REAL_NIX_HOME is required")?;
    let system = std::env::var("PKG_REAL_NIX_SYSTEM")?;
    let system = System::from_str(&system)?;
    let adapter = RealNixAdapter::new(Path::new(&binary), Path::new(&home))?;

    let version = adapter
        .version()
        .map_err(|error| format!("version: {error:?}"))?;
    assert_eq!(version.nix_version().as_str(), "2.34.8");

    let request = EvaluateDerivationRequest::new(
        AttributePath::new("hello")?,
        system,
        NixpkgsRevision::new(REVISION)?,
        NarHash::new(NAR_HASH)?,
        OutputSelection::default_selection(),
    )?;
    let plan = adapter
        .evaluate_derivation(&request)
        .map_err(|error| format!("evaluate: {error:?}"))?;
    assert_eq!(plan.pname(), "hello");
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

    let substitute = adapter
        .substitute(&expected_path)
        .map_err(|error| format!("substitute: {error:?}"))?;
    assert_eq!(substitute.outcome(), SubstituteOutcome::Fetched);
    let info = adapter
        .path_info(&expected_path)
        .map_err(|error| format!("path info: {error:?}"))?;
    assert_eq!(info.store_path(), &expected_path);
    assert!(
        info.signatures()
            .iter()
            .any(|signature| signature.key_name() == "cache.nixos.org-1")
    );

    let verify = adapter
        .verify(&VerifyRequest::new(
            vec![expected_path.clone()],
            VerifyMode::Recursive,
        )?)
        .map_err(|error| format!("verify: {error:?}"))?;
    assert_eq!(verify.results().len(), 1);
    assert_eq!(verify.results()[0].nar_integrity(), NarIntegrity::Intact);
    assert_eq!(verify.results()[0].trust(), TrustStatus::Trusted);

    let receipt = BuildApprovalReceipt::new(
        OperationId::new("nightly-real-nix")?,
        plan.closure_digest(),
        PolicyVersion::from_u64(1).ok_or("policy version")?,
    );
    let build = adapter
        .build(&BuildRequest::new(
            vec![DerivedOutputTarget::new(
                plan.root().clone(),
                vec![selected],
            )?],
            system,
            receipt,
        )?)
        .map_err(|error| format!("build: {error:?}"))?;
    assert_eq!(build.outputs().len(), 1);
    assert_eq!(build.outputs()[0].store_path(), &expected_path);
    assert_eq!(
        build.outputs()[0].provenance(),
        BuildOutputProvenance::CacheSigned
    );

    let gc = adapter.gc().map_err(|error| format!("gc: {error:?}"))?;
    assert_eq!(gc.status(), GcStatus::Collected);
    assert!(gc.collected().contains(&expected_path));
    Ok(())
}

#[test]
#[ignore = "requires an isolated root-owned product-pinned Nix 2.34.8 store"]
fn real_root_repair_stops_on_cache_miss_then_builds_after_typed_approval()
-> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("PKG_REAL_NIX_ISOLATED").as_deref() != Some(OsStr::new("1")) {
        return Err("PKG_REAL_NIX_ISOLATED=1 is required".into());
    }
    let binary = std::env::var_os("PKG_REAL_NIX_BIN").ok_or("PKG_REAL_NIX_BIN is required")?;
    let home = std::env::var_os("PKG_REAL_NIX_HOME").ok_or("PKG_REAL_NIX_HOME is required")?;
    let built = Command::new(&binary)
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "build",
            "--impure",
            "--expr",
            r#"derivation { name = "pkg-uncached-repair"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "echo original > $out" ]; __noChroot = true; }"#,
            "--no-link",
            "--print-out-paths",
        ])
        .env_clear()
        .env("HOME", &home)
        .env("TMPDIR", Path::new(&home).join("tmp"))
        .env("NIX_CONFIG", "include /opt/pkg/etc/pkg/nix.conf")
        .env(
            "NIX_DAEMON_SOCKET_PATH",
            "/nix/var/nix/daemon-socket/socket",
        )
        .env("NIX_REMOTE", "daemon")
        .env("NIX_STATE_DIR", "/nix/var/nix")
        .env("NIX_USER_CONF_FILES", "")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()?;
    if !built.status.success() {
        return Err("fixture derivation failed".into());
    }
    let output = std::str::from_utf8(&built.stdout)?.trim();
    let store_path = StorePath::new(output)?;
    let mut permissions = fs::metadata(store_path.as_str())?.permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(store_path.as_str(), permissions.clone())?;
    fs::write(store_path.as_str(), b"corrupt\n")?;
    permissions.set_mode(0o444);
    fs::set_permissions(store_path.as_str(), permissions)?;

    let executor = std::sync::Arc::new(RootNixRepairExecutor::new(
        Path::new(&binary),
        Path::new(&home),
    )?);
    let helper = InProcessHelper::with_repair_executor(991, executor)?;
    let maintenance = helper
        .connect(InProcessPeer::authenticated_uid(991))?
        .for_caller(1001);
    let generation = GenerationId::new("gen-0007")?;
    maintenance.publish_root_set(&RootSet::new(
        1001,
        generation.clone(),
        vec![RootSetEntry::new(
            pkg_nix::RootName::new("hello-out")?,
            store_path.clone(),
        )],
    )?)?;

    let cache_scope = VerifiedRepairScope::new(
        1001,
        generation.clone(),
        [store_path.clone()],
        None,
        PolicyVersion::from_u64(1).ok_or("policy version")?,
        RepairMode::CacheOnly,
    )?;
    let cache_capability = maintenance.issue_repair_capability(&cache_scope)?;
    let cache_report = maintenance
        .repair_store_paths(&RepairStorePathsRequest::new(cache_capability))
        .map_err(|error| format!("cache repair: {error:?}"))?;
    assert_eq!(
        cache_report.outcomes()[0].kind(),
        RepairOutcomeKind::CacheMiss
    );
    assert_eq!(fs::read(store_path.as_str())?, b"corrupt\n");

    let build_scope = VerifiedRepairScope::new(
        1001,
        generation,
        [store_path.clone()],
        Some(body_digest(b"approved real repair plan")),
        PolicyVersion::from_u64(1).ok_or("policy version")?,
        RepairMode::Build,
    )?;
    let build_capability = maintenance.issue_repair_capability(&build_scope)?;
    let build_report = maintenance
        .repair_store_paths(&RepairStorePathsRequest::new(build_capability))
        .map_err(|error| format!("build repair: {error:?}"))?;
    assert_eq!(
        build_report.outcomes()[0].kind(),
        RepairOutcomeKind::Restored
    );
    assert_eq!(fs::read(store_path.as_str())?, b"original\n");
    Ok(())
}
