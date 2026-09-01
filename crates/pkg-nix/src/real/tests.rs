//! Tests for the real Nix adapter.

use super::*;
use super::{process::*, root::*, substitute::*};

#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::fs;
use std::sync::{Arc, Mutex};

use pkg_core::{
    NixpkgsRevision, OutputSelection, PolicyVersion, System, identity::NarHash,
    selector::AttributePath,
};

#[derive(Debug)]
struct Scripted {
    calls: Arc<Mutex<Vec<Vec<OsString>>>>,
    outcomes: Mutex<Vec<Result<CommandOutcome, NixAdapterError>>>,
}

impl Scripted {
    fn new(outcomes: Vec<CommandOutcome>) -> Self {
        Self::with_results(outcomes.into_iter().map(Ok).collect())
    }

    fn with_results(outcomes: Vec<Result<CommandOutcome, NixAdapterError>>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            outcomes: Mutex::new(outcomes.into_iter().rev().collect()),
        }
    }
}

impl CommandExecutor for Scripted {
    fn execute(&self, spec: CommandSpec) -> Result<CommandOutcome, NixAdapterError> {
        self.calls
            .lock()
            .map_err(|_| NixAdapterError::OperationFailed)?
            .push(spec.args);
        self.outcomes
            .lock()
            .map_err(|_| NixAdapterError::OperationFailed)?
            .pop()
            .ok_or(NixAdapterError::UnexpectedExtraCall {
                actual: MethodKind::Version,
                summary: crate::error::BoundedSummary::new("extra call"),
            })?
    }
}

fn success(stdout: impl Into<Vec<u8>>) -> CommandOutcome {
    CommandOutcome {
        code: Some(0),
        stdout: stdout.into(),
        stderr: Vec::new(),
        stdout_oversized: false,
        stderr_oversized: false,
        timed_out: false,
    }
}

fn success_with_stderr(stderr: impl Into<Vec<u8>>) -> CommandOutcome {
    let mut outcome = success(Vec::new());
    outcome.stderr = stderr.into();
    outcome
}

fn failure(code: i32) -> CommandOutcome {
    let mut outcome = success(Vec::new());
    outcome.code = Some(code);
    outcome
}

#[test]
fn absolute_root_operation_deadline_clamps_all_executor_timeouts() {
    let deadline = Instant::now().checked_add(Duration::from_mins(1)).unwrap();
    for local_limit in [SHORT_TIMEOUT, BUILD_TIMEOUT, GC_TIMEOUT] {
        let timeout = bounded_timeout(Some(deadline), local_limit).unwrap();
        assert!(timeout <= Duration::from_mins(1));
        assert!(!timeout.is_zero());
    }
}

#[cfg(unix)]
fn captured_environment(
    executor: &ProcessExecutor,
) -> Result<BTreeMap<String, String>, Box<dyn std::error::Error>> {
    let outcome = executor.execute(CommandSpec {
        program: NixProgram::Modern,
        args: Vec::new(),
        timeout: SHORT_TIMEOUT,
    })?;
    Ok(String::from_utf8(outcome.stdout)?
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect())
}

#[cfg(unix)]
#[test]
fn repair_executors_keep_standard_determinate_and_managed_environments_distinct()
-> Result<(), Box<dyn std::error::Error>> {
    const CHILD: &str = "PKG_NIX_STANDARD_EXECUTOR_ENV_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let current_thread = std::thread::current();
        let test_name = current_thread.name().ok_or("missing test name")?;
        let status = std::process::Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg(test_name)
            .env(CHILD, "1")
            .env("NIX_CONFIG", "ambient-config")
            .env("NIX_DAEMON_SOCKET_PATH", "/ambient/socket")
            .env("NIX_REMOTE", "ambient-remote")
            .env("NIX_STATE_DIR", "/ambient/state")
            .status()?;
        assert!(status.success());
        return Ok(());
    }

    assert_eq!(
        Path::new(STANDARD_DETERMINATE_NIX_BINARY),
        Path::new("/nix/var/nix/profiles/default/bin/nix")
    );
    let _fixed_constructor: fn(&Path) -> Result<RootNixRepairExecutor, NixAdapterError> =
        RootNixRepairExecutor::new_standard_determinate;
    let _managed_constructor: fn(&Path, &Path) -> Result<RootNixRepairExecutor, NixAdapterError> =
        RootNixRepairExecutor::new;

    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    fs::create_dir(&home)?;
    fs::create_dir(home.join("tmp"))?;
    let binary = temporary.path().join("nix");
    fs::write(&binary, "#!/bin/sh\nexec /usr/bin/env\n")?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;

    let standard = ProcessExecutor {
        nix_binary: binary.clone(),
        nix_store_binary: binary.clone(),
        private_home: home.clone(),
        daemon_socket: None,
    };
    let legacy = ProcessExecutor {
        nix_binary: binary.clone(),
        nix_store_binary: binary,
        private_home: home.clone(),
        daemon_socket: Some(PathBuf::from(MANAGED_DAEMON_SOCKET)),
    };

    let standard_environment = captured_environment(&standard)?;
    assert_eq!(
        standard_environment.get("HOME"),
        Some(&home.display().to_string())
    );
    assert_eq!(
        standard_environment.get("TMPDIR"),
        Some(&home.join("tmp").display().to_string())
    );
    assert_eq!(
        standard_environment.get("NIX_USER_CONF_FILES"),
        Some(&String::new())
    );
    assert_eq!(
        standard_environment.get("PATH"),
        Some(&MANAGED_PATH.to_owned())
    );
    for key in [
        "NIX_CONFIG",
        "NIX_DAEMON_SOCKET_PATH",
        "NIX_REMOTE",
        "NIX_STATE_DIR",
    ] {
        assert!(!standard_environment.contains_key(key));
    }

    let legacy_environment = captured_environment(&legacy)?;
    assert_eq!(
        legacy_environment.get("NIX_CONFIG"),
        Some(&MANAGED_NIX_CONFIG.to_owned())
    );
    assert_eq!(
        legacy_environment.get("NIX_DAEMON_SOCKET_PATH"),
        Some(&MANAGED_DAEMON_SOCKET.to_owned())
    );
    assert_eq!(
        legacy_environment.get("NIX_REMOTE"),
        Some(&"daemon".to_owned())
    );
    assert_eq!(
        legacy_environment.get("NIX_STATE_DIR"),
        Some(&MANAGED_NIX_STATE.to_owned())
    );
    Ok(())
}

fn repair_scope(mode: RepairMode) -> Result<VerifiedRepairScope, Box<dyn std::error::Error>> {
    Ok(VerifiedRepairScope::new(
        1001,
        crate::GenerationId::new("gen-0007")?,
        [StorePath::new(
            "/nix/store/22222222222222222222222222222222-demo",
        )?],
        (mode == RepairMode::Build).then(|| body_digest(b"approved repair plan")),
        PolicyVersion::from_u64(1).ok_or("policy version")?,
        mode,
    )?)
}

#[test]
fn root_repair_cache_miss_requires_successful_repair_and_live_local_store()
-> Result<(), Box<dyn std::error::Error>> {
    let scripted = Scripted::new(vec![success(Vec::new()), failure(1), success(Vec::new())]);
    let calls = Arc::clone(&scripted.calls);
    let executor = RootNixRepairExecutor::scripted(scripted);
    let outcomes = executor.execute(&repair_scope(RepairMode::CacheOnly)?)?;
    assert_eq!(outcomes, vec![RepairOutcomeKind::CacheMiss]);
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 3);
    assert!(calls.iter().all(|call| {
        call.windows(2)
            .any(|arguments| arguments == [OsString::from("--store"), OsString::from("local")])
    }));
    assert!(calls[0].windows(3).any(|arguments| {
        arguments
            == [
                OsString::from("--option"),
                OsString::from("max-jobs"),
                OsString::from("0"),
            ]
    }));
    assert!(calls[0].windows(3).any(|arguments| {
        arguments
            == [
                OsString::from("--option"),
                OsString::from("builders"),
                OsString::new(),
            ]
    }));
    assert_eq!(
        &calls[0][calls[0].len() - 3..],
        [
            OsString::from("store"),
            OsString::from("repair"),
            OsString::from("/nix/store/22222222222222222222222222222222-demo"),
        ]
    );
    assert_eq!(
        &calls[1][calls[1].len() - 4..],
        [
            OsString::from("store"),
            OsString::from("verify"),
            OsString::from("--no-trust"),
            OsString::from("/nix/store/22222222222222222222222222222222-demo"),
        ]
    );
    assert_eq!(
        &calls[2][calls[2].len() - 2..],
        [OsString::from("store"), OsString::from("info")]
    );
    Ok(())
}

#[test]
fn root_repair_build_is_bounded_and_must_verify_clean() -> Result<(), Box<dyn std::error::Error>> {
    let scripted = Scripted::new(vec![success(Vec::new()), success(Vec::new())]);
    let calls = Arc::clone(&scripted.calls);
    let executor = RootNixRepairExecutor::scripted(scripted);
    assert_eq!(
        executor.execute(&repair_scope(RepairMode::Build)?)?,
        vec![RepairOutcomeKind::Restored]
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 2);
    assert!(calls[0].windows(3).any(|arguments| {
        arguments
            == [
                OsString::from("--option"),
                OsString::from("max-jobs"),
                OsString::from("1"),
            ]
    }));

    let executor =
        RootNixRepairExecutor::scripted(Scripted::new(vec![success(Vec::new()), failure(1)]));
    assert_eq!(
        executor
            .execute(&repair_scope(RepairMode::Build)?)
            .unwrap_err()
            .code(),
        crate::MaintenanceErrorCode::BackendFailure
    );
    Ok(())
}

#[test]
fn root_repair_command_failure_is_not_downgraded_to_cache_miss()
-> Result<(), Box<dyn std::error::Error>> {
    let executor = RootNixRepairExecutor::scripted(Scripted::new(vec![failure(1)]));
    assert_eq!(
        executor
            .execute(&repair_scope(RepairMode::CacheOnly)?)
            .unwrap_err()
            .code(),
        crate::MaintenanceErrorCode::BackendFailure
    );
    Ok(())
}

#[test]
fn root_gc_uses_only_the_fixed_local_store() -> Result<(), Box<dyn std::error::Error>> {
    let path = "/nix/store/22222222222222222222222222222222-dead";
    let scripted = Scripted::new(vec![
        success(format!("{path}\n")),
        success_with_stderr(format!("deleting '{path}'\n")),
    ]);
    let calls = Arc::clone(&scripted.calls);
    let report = RootNixGcExecutor::scripted(scripted).collect()?;

    assert_eq!(report.status(), GcStatus::Collected);
    assert_eq!(
        report
            .collected()
            .iter()
            .map(StorePath::as_str)
            .collect::<Vec<_>>(),
        [path]
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(
        calls.as_slice(),
        [
            vec![
                OsString::from("--store"),
                OsString::from("local"),
                OsString::from("--gc"),
                OsString::from("--print-dead"),
            ],
            vec![
                OsString::from("--store"),
                OsString::from("local"),
                OsString::from("--gc"),
            ],
        ]
    );
    Ok(())
}

#[test]
fn root_gc_refuses_malformed_preflight_before_deletion() {
    let scripted = Scripted::new(vec![success("/tmp/not-a-store-path\n")]);
    let calls = Arc::clone(&scripted.calls);
    let error = RootNixGcExecutor::scripted(scripted)
        .collect()
        .expect_err("malformed dead-path report must refuse");

    assert_eq!(error.code(), crate::NixAdapterErrorCode::MalformedPayload);
    assert_eq!(calls.lock().expect("call log").len(), 1);
}

#[test]
fn root_gc_does_not_downgrade_command_failure() {
    let error = RootNixGcExecutor::scripted(Scripted::new(vec![failure(1)]))
        .collect()
        .expect_err("failed local-store preflight must refuse");

    assert_eq!(error.code(), crate::NixAdapterErrorCode::OperationFailed);
}

#[test]
fn root_gc_resolves_only_the_fixed_local_product_closure() -> Result<(), Box<dyn std::error::Error>>
{
    let root = StorePath::new("/nix/store/22222222222222222222222222222222-product")?;
    let dependency = "/nix/store/33333333333333333333333333333333-dependency";
    let raw = br#"{"info":{"22222222222222222222222222222222-product":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":5,"references":["33333333333333333333333333333333-dependency"],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":false,"url":null,"version":2},"33333333333333333333333333333333-dependency":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":7,"references":[],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":false,"url":null,"version":2}},"storeDir":"/nix/store","version":2}"#;
    let scripted = Scripted::new(vec![success(raw.as_slice())]);
    let calls = Arc::clone(&scripted.calls);
    let closure =
        RootNixGcExecutor::scripted(scripted).closure_for_roots(std::slice::from_ref(&root))?;

    assert_eq!(
        closure.iter().map(StorePath::as_str).collect::<Vec<_>>(),
        [root.as_str(), dependency]
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(
        calls.as_slice(),
        [vec![
            OsString::from("--extra-experimental-features"),
            OsString::from("nix-command flakes"),
            OsString::from("--option"),
            OsString::from("allow-import-from-derivation"),
            OsString::from("false"),
            OsString::from("path-info"),
            OsString::from("--json"),
            OsString::from("--json-format"),
            OsString::from("2"),
            OsString::from("--recursive"),
            OsString::from("--store"),
            OsString::from("local"),
            OsString::from(root.as_str()),
        ]]
    );
    Ok(())
}

#[test]
fn root_gc_lists_only_valid_registered_local_paths() -> Result<(), Box<dyn std::error::Error>> {
    let first = "/nix/store/22222222222222222222222222222222-product";
    let second = "/nix/store/33333333333333333333333333333333-source";
    let raw = br#"{"info":{"22222222222222222222222222222222-product":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":5,"references":[],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":false,"url":null,"version":2},"33333333333333333333333333333333-source":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":7,"references":[],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":false,"url":null,"version":2}},"storeDir":"/nix/store","version":2}"#;
    let scripted = Scripted::new(vec![success(raw.as_slice())]);
    let calls = Arc::clone(&scripted.calls);
    let paths = RootNixGcExecutor::scripted(scripted).registered_paths()?;

    assert_eq!(
        paths.iter().map(StorePath::as_str).collect::<Vec<_>>(),
        [first, second]
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(
        calls.as_slice(),
        [vec![
            OsString::from("--extra-experimental-features"),
            OsString::from("nix-command flakes"),
            OsString::from("--option"),
            OsString::from("allow-import-from-derivation"),
            OsString::from("false"),
            OsString::from("path-info"),
            OsString::from("--all"),
            OsString::from("--json"),
            OsString::from("--json-format"),
            OsString::from("2"),
            OsString::from("--store"),
            OsString::from("local"),
        ]]
    );
    Ok(())
}

#[test]
fn broker_repair_closure_uses_only_fixed_recursive_daemon_queries()
-> Result<(), Box<dyn std::error::Error>> {
    let root = StorePath::new("/nix/store/22222222222222222222222222222222-product")?;
    let dependency = "/nix/store/33333333333333333333333333333333-dependency";
    let raw = br#"{"info":{"22222222222222222222222222222222-product":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":5,"references":["33333333333333333333333333333333-dependency"],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":false,"url":null,"version":2},"33333333333333333333333333333333-dependency":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":7,"references":[],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":false,"url":null,"version":2}},"storeDir":"/nix/store","version":2}"#;
    let scripted = Scripted::new(vec![success(raw.as_slice())]);
    let calls = Arc::clone(&scripted.calls);
    let closure =
        RealNixAdapter::scripted(scripted).closure_for_roots(std::slice::from_ref(&root))?;

    assert_eq!(
        closure.iter().map(StorePath::as_str).collect::<Vec<_>>(),
        [root.as_str(), dependency]
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(
        calls.as_slice(),
        [vec![
            OsString::from("--extra-experimental-features"),
            OsString::from("nix-command flakes"),
            OsString::from("--option"),
            OsString::from("allow-import-from-derivation"),
            OsString::from("false"),
            OsString::from("path-info"),
            OsString::from("--json"),
            OsString::from("--json-format"),
            OsString::from("2"),
            OsString::from("--recursive"),
            OsString::from(root.as_str()),
        ]]
    );
    Ok(())
}

#[test]
fn broker_repair_closure_counts_shared_dependencies_once() -> Result<(), Box<dyn std::error::Error>>
{
    let first = StorePath::new("/nix/store/11111111111111111111111111111111-first")?;
    let second = StorePath::new("/nix/store/22222222222222222222222222222222-second")?;
    let shared = "/nix/store/33333333333333333333333333333333-shared";
    let first_raw = br#"{"info":{"11111111111111111111111111111111-first":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":5,"references":["33333333333333333333333333333333-shared"],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":false,"url":null,"version":2},"33333333333333333333333333333333-shared":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":7,"references":[],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":false,"url":null,"version":2}},"storeDir":"/nix/store","version":2}"#;
    let second_raw = br#"{"info":{"22222222222222222222222222222222-second":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":5,"references":["33333333333333333333333333333333-shared"],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":false,"url":null,"version":2},"33333333333333333333333333333333-shared":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":7,"references":[],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":false,"url":null,"version":2}},"storeDir":"/nix/store","version":2}"#;
    let adapter = RealNixAdapter::scripted(Scripted::new(vec![
        success(first_raw.as_slice()),
        success(second_raw.as_slice()),
    ]));

    let closure = adapter.closure_for_roots(&[first.clone(), second.clone()])?;

    assert_eq!(
        closure.iter().map(StorePath::as_str).collect::<Vec<_>>(),
        [first.as_str(), second.as_str(), shared]
    );
    Ok(())
}

#[test]
fn fixed_mode_versions_are_exact_and_environment_runner_is_not_bypassed()
-> Result<(), Box<dyn std::error::Error>> {
    let legacy = RealNixAdapter::scripted(Scripted::new(vec![
        success("nix (Nix) 2.34.8\n"),
        success("nix-store (Nix) 2.34.8\n"),
    ]));
    let legacy_version = legacy.version()?;
    assert_eq!(legacy_version.nix_version().as_str(), PINNED_NIX_VERSION);
    assert_eq!(legacy_version.accepted_formats().path_info().get(), 2);

    let standard = RealNixAdapter::scripted_standard_determinate(Scripted::new(vec![
        success("nix (Determinate Nix 3.22.1) 2.35.2\n"),
        success("nix-store (Determinate Nix 3.22.1) 2.35.2\n"),
    ]));
    assert_eq!(
        standard.version()?.nix_version().as_str(),
        STANDARD_DETERMINATE_NIX_VERSION
    );
    Ok(())
}

#[test]
fn root_operation_clone_preserves_standard_determinate_version_banner()
-> Result<(), Box<dyn std::error::Error>> {
    let adapter = RealNixAdapter::scripted_standard_determinate(Scripted::new(vec![
        success("nix (Determinate Nix 3.22.1) 2.35.2\n"),
        success("nix-store (Determinate Nix 3.22.1) 2.35.2\n"),
    ]));
    let root_operation = adapter.for_root_operation(
        RootNixOperation::Version,
        Instant::now() + Duration::from_secs(1),
    )?;

    assert_eq!(
        root_operation.version()?.nix_version().as_str(),
        STANDARD_DETERMINATE_NIX_VERSION
    );
    Ok(())
}

#[test]
fn fixed_mode_versions_reject_wrong_brand_version_and_legacy_banner() {
    let legacy_nix = RealNixAdapter::scripted(Scripted::new(vec![success("nix (Nix) 2.35.2\n")]));
    assert!(matches!(
        legacy_nix.version(),
        Err(NixAdapterError::UnsupportedUpstreamFormat { .. })
    ));

    let legacy_nix_store = RealNixAdapter::scripted(Scripted::new(vec![
        success("nix (Nix) 2.34.8\n"),
        success("nix-store (Nix) 2.35.2\n"),
    ]));
    assert!(matches!(
        legacy_nix_store.version(),
        Err(NixAdapterError::UnsupportedUpstreamFormat { .. })
    ));

    let unbranded_standard_nix = RealNixAdapter::scripted_standard_determinate(Scripted::new(
        vec![success("nix (Nix) 2.35.2\n")],
    ));
    assert!(matches!(
        unbranded_standard_nix.version(),
        Err(NixAdapterError::UnsupportedUpstreamFormat { .. })
    ));

    let wrong_determinate_release = RealNixAdapter::scripted_standard_determinate(Scripted::new(
        vec![success("nix (Determinate Nix 3.22.0) 2.35.2\n")],
    ));
    assert!(matches!(
        wrong_determinate_release.version(),
        Err(NixAdapterError::UnsupportedUpstreamFormat { .. })
    ));

    let wrong_standard_nix_version = RealNixAdapter::scripted_standard_determinate(Scripted::new(
        vec![success("nix (Determinate Nix 3.22.1) 2.35.1\n")],
    ));
    assert!(matches!(
        wrong_standard_nix_version.version(),
        Err(NixAdapterError::UnsupportedUpstreamFormat { .. })
    ));

    let wrong_standard_nix_store =
        RealNixAdapter::scripted_standard_determinate(Scripted::new(vec![
            success("nix (Determinate Nix 3.22.1) 2.35.2\n"),
            success("nix-store (Nix) 2.35.2\n"),
        ]));
    assert!(matches!(
        wrong_standard_nix_store.version(),
        Err(NixAdapterError::UnsupportedUpstreamFormat { .. })
    ));
}

#[test]
fn determinate_metadata_disables_lazy_trees_and_managed_metadata_does_not()
-> Result<(), Box<dyn std::error::Error>> {
    let pin = NixpkgsPin::new(
        "0123456789abcdef0123456789abcdef01234567",
        "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )?;
    let expected = br#"{"locked":{},"path":"private"}"#;
    let has_lazy_trees = |call: &[OsString]| {
        call.windows(3).any(|window| {
            window
                == [
                    OsString::from("--option"),
                    OsString::from("lazy-trees"),
                    OsString::from("false"),
                ]
        })
    };

    let managed_executor = Scripted::new(vec![success(expected.as_slice())]);
    let managed_calls = Arc::clone(&managed_executor.calls);
    let managed = RealNixAdapter::scripted(managed_executor);
    let _ = managed.run_metadata(&pin)?;
    let managed_calls = managed_calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(managed_calls.len(), 1);
    assert!(!has_lazy_trees(&managed_calls[0]));

    let determinate_executor = Scripted::new(vec![success(expected.as_slice())]);
    let determinate_calls = Arc::clone(&determinate_executor.calls);
    let determinate = RealNixAdapter::scripted_standard_determinate(determinate_executor);
    let _ = determinate.run_metadata(&pin)?;
    let determinate_calls = determinate_calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(determinate_calls.len(), 1);
    assert!(has_lazy_trees(&determinate_calls[0]));
    Ok(())
}

#[test]
fn nixpkgs_metadata_runner_reconstructs_only_the_fixed_command()
-> Result<(), Box<dyn std::error::Error>> {
    let pin = NixpkgsPin::new(
        "0123456789abcdef0123456789abcdef01234567",
        "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )?;
    let expected = br#"{"locked":{},"path":"private"}"#;
    let executor = Scripted::new(vec![success(expected.as_slice())]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);

    assert_eq!(adapter.run_metadata(&pin)?, expected);
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 1);
    let mut expected_args = base_args();
    expected_args.extend(os_args([
            "flake",
            "metadata",
            "--no-use-registries",
            "github:NixOS/nixpkgs/0123456789abcdef0123456789abcdef01234567?narHash=sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "--json",
        ]));
    assert_eq!(calls[0], expected_args);
    for forbidden in ["--impure", "--override-input", "--registry"] {
        assert!(!calls[0].iter().any(|argument| argument == forbidden));
    }
    Ok(())
}

#[test]
fn nixpkgs_index_projection_uses_only_the_fixed_expression_and_source()
-> Result<(), Box<dyn std::error::Error>> {
    let store_path = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-source";
    let source = PinnedNixpkgsSource::for_test(
        "0123456789abcdef0123456789abcdef01234567",
        "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        store_path,
    )?;
    let executor = Scripted::new(vec![success("[]")]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);

    assert_eq!(
        adapter.project_nixpkgs_index(&source, System::Aarch64Darwin)?,
        b"[]"
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    let mut expected = base_args();
    expected.extend(os_args(["--offline", "eval", "--json", "--apply"]));
    expected.push(INDEX_META_EXPR.into());
    expected.push(format!("{store_path}#legacyPackages.aarch64-darwin").into());
    assert_eq!(calls.as_slice(), [expected]);
    assert_eq!(
        calls[0]
            .iter()
            .filter(|argument| argument.as_os_str() == INDEX_META_EXPR)
            .count(),
        1
    );
    Ok(())
}

#[test]
fn managed_store_ping_uses_only_the_fixed_daemon_store() -> Result<(), Box<dyn std::error::Error>> {
    let executor = Scripted::new(vec![success(Vec::new())]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);

    adapter.ping_managed_store()?;

    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        [
            OsString::from("store"),
            OsString::from("ping"),
            OsString::from("--store"),
            OsString::from("daemon"),
        ]
    );
    Ok(())
}

#[test]
fn managed_store_wait_retries_transient_errors_until_success() {
    let started = Instant::now();
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let sleeps = std::cell::RefCell::new(Vec::new());
    let pings = std::cell::Cell::new(0);
    let mut outcomes = [
        Err(NixAdapterError::OperationFailed),
        Err(NixAdapterError::Unavailable),
        Err(NixAdapterError::Timeout),
        Ok(()),
    ]
    .into_iter();

    assert_eq!(
        wait_for_managed_store_with(
            |_| {
                pings.set(pings.get() + 1);
                outcomes.next().expect("bounded ping sequence")
            },
            || started + elapsed.get(),
            |duration| {
                sleeps.borrow_mut().push(duration);
                elapsed.set(elapsed.get() + duration);
            },
            Duration::from_millis(500),
            Duration::from_millis(50),
            Duration::from_secs(2),
        ),
        Ok(())
    );
    assert_eq!(pings.get(), 4);
    assert_eq!(sleeps.into_inner(), vec![Duration::from_millis(50); 3]);
}

#[test]
fn managed_store_wait_returns_terminal_error_without_sleeping() {
    let pings = std::cell::Cell::new(0);
    let sleeps = std::cell::Cell::new(0);

    assert_eq!(
        wait_for_managed_store_with(
            |_| {
                pings.set(pings.get() + 1);
                Err(NixAdapterError::PermissionDenied)
            },
            Instant::now,
            |_| sleeps.set(sleeps.get() + 1),
            Duration::from_millis(120),
            Duration::from_millis(50),
            Duration::from_secs(2),
        ),
        Err(NixAdapterError::PermissionDenied)
    );
    assert_eq!(pings.get(), 1);
    assert_eq!(sleeps.get(), 0);
}

#[test]
fn managed_store_wait_caps_the_last_sleep_and_times_out() {
    let started = Instant::now();
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let sleeps = std::cell::RefCell::new(Vec::new());
    let timeouts = std::cell::RefCell::new(Vec::new());
    let pings = std::cell::Cell::new(0);

    assert_eq!(
        wait_for_managed_store_with(
            |timeout| {
                pings.set(pings.get() + 1);
                timeouts.borrow_mut().push(timeout);
                Err(NixAdapterError::OperationFailed)
            },
            || started + elapsed.get(),
            |duration| {
                sleeps.borrow_mut().push(duration);
                elapsed.set(elapsed.get() + duration);
            },
            Duration::from_millis(120),
            Duration::from_millis(50),
            Duration::from_secs(2),
        ),
        Err(NixAdapterError::Timeout)
    );
    assert_eq!(pings.get(), 3);
    assert_eq!(
        timeouts.into_inner(),
        vec![
            Duration::from_millis(120),
            Duration::from_millis(70),
            Duration::from_millis(20),
        ]
    );
    assert_eq!(
        sleeps.into_inner(),
        vec![
            Duration::from_millis(50),
            Duration::from_millis(50),
            Duration::from_millis(20),
        ]
    );
}

#[test]
fn managed_store_wait_rejects_success_that_finishes_after_deadline() {
    let started = Instant::now();
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let pings = std::cell::Cell::new(0);
    let sleeps = std::cell::Cell::new(0);
    let timeout = std::cell::Cell::new(Duration::ZERO);

    assert_eq!(
        wait_for_managed_store_with(
            |attempt_timeout| {
                pings.set(pings.get() + 1);
                timeout.set(attempt_timeout);
                elapsed.set(Duration::from_millis(121));
                Ok(())
            },
            || started + elapsed.get(),
            |_| sleeps.set(sleeps.get() + 1),
            Duration::from_millis(120),
            Duration::from_millis(50),
            Duration::from_secs(2),
        ),
        Err(NixAdapterError::Timeout)
    );
    assert_eq!(pings.get(), 1);
    assert_eq!(sleeps.get(), 0);
    assert_eq!(timeout.get(), Duration::from_millis(120));
}

#[test]
fn managed_store_wait_stops_when_a_ping_finishes_at_deadline() {
    let started = Instant::now();
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let pings = std::cell::Cell::new(0);
    let sleeps = std::cell::Cell::new(0);

    assert_eq!(
        wait_for_managed_store_with(
            |_| {
                pings.set(pings.get() + 1);
                elapsed.set(Duration::from_millis(120));
                Err(NixAdapterError::OperationFailed)
            },
            || started + elapsed.get(),
            |_| sleeps.set(sleeps.get() + 1),
            Duration::from_millis(120),
            Duration::from_millis(50),
            Duration::from_secs(2),
        ),
        Err(NixAdapterError::Timeout)
    );
    assert_eq!(pings.get(), 1);
    assert_eq!(sleeps.get(), 0);
}

#[test]
fn managed_store_wait_uses_only_the_fixed_daemon_store() -> Result<(), Box<dyn std::error::Error>> {
    let executor = Scripted::new(vec![success(Vec::new())]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);

    adapter.wait_for_managed_store()?;

    assert_eq!(
        calls.lock().map_err(|_| "poisoned call log")?.as_slice(),
        [[
            OsString::from("store"),
            OsString::from("ping"),
            OsString::from("--store"),
            OsString::from("daemon"),
        ]]
    );
    Ok(())
}

#[test]
fn nixpkgs_metadata_runner_failure_is_closed() {
    let pin = NixpkgsPin::new(
        "0123456789abcdef0123456789abcdef01234567",
        "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
    )
    .unwrap();
    let adapter = RealNixAdapter::scripted(Scripted::new(vec![failure(1)]));

    assert_eq!(
        adapter.run_metadata(&pin).unwrap_err().code(),
        crate::NixpkgsSourceErrorCode::RunnerFailure
    );
}

#[test]
fn derivation_v4_normalizes_relative_paths_and_closed_fields()
-> Result<(), Box<dyn std::error::Error>> {
    let raw = br#"{"version":4,"derivations":{"00000000000000000000000000000000-demo.drv":{"args":[],"builder":"/nix/store/11111111111111111111111111111111-bash","env":{"dev":"/nix/store/44444444444444444444444444444444-demo-dev","out":"/nix/store/22222222222222222222222222222222-demo","outputs":"dev","pname":"legacy","version":"0.9"},"inputs":{"drvs":{"33333333333333333333333333333333-dep.drv":["out"]},"srcs":[]},"name":"demo-1.0","outputs":{"dev":{"path":"44444444444444444444444444444444-demo-dev"},"out":{"path":"22222222222222222222222222222222-demo"}},"structuredAttrs":{"__structuredAttrs":true,"meta":{"outputsToInstall":["out"]},"outputs":["dev","out"],"pname":"demo","version":"1.0"},"system":"aarch64-linux","version":4}}}"#;
    let executor = Scripted::new(vec![success(raw.as_slice()), success(raw.as_slice())]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);
    let request = EvaluateDerivationRequest::new(
        AttributePath::new("demo")?,
        System::Aarch64Linux,
        NixpkgsRevision::new("a62e6edd6d5e1fa0329b8653c801147986f8d446")?,
        NarHash::new("sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=")?,
        OutputSelection::default_selection(),
    )?;
    let report = adapter.evaluate_derivation(&request)?;
    assert_eq!(report.json_version(), 4);
    assert_eq!(report.pname(), "demo");
    assert_eq!(report.version().as_str(), "1.0");
    assert_eq!(report.outputs_to_install().len(), 1);
    assert_eq!(report.outputs_to_install()[0].as_str(), "out");
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 2);
    for call in calls.iter() {
        assert!(call.windows(3).any(|arguments| {
            arguments
                == [
                    OsString::from("--option"),
                    OsString::from("allow-import-from-derivation"),
                    OsString::from("false"),
                ]
        }));
    }
    Ok(())
}

#[test]
fn derivation_v4_rejects_malformed_structured_output_selection()
-> Result<(), Box<dyn std::error::Error>> {
    let root = "00000000000000000000000000000000-demo.drv";
    let raw = br#"{"version":4,"derivations":{"00000000000000000000000000000000-demo.drv":{"args":[],"builder":"/nix/store/11111111111111111111111111111111-bash","env":{"out":"/nix/store/22222222222222222222222222222222-demo"},"inputs":{"drvs":{},"srcs":[]},"name":"demo-1.0","outputs":{"out":{"path":"22222222222222222222222222222222-demo"}},"structuredAttrs":{"meta":{"outputsToInstall":"out"},"outputs":["out"],"pname":"demo","version":"1.0"},"system":"aarch64-linux","version":4}}}"#;
    let request = EvaluateDerivationRequest::new(
        AttributePath::new("demo")?,
        System::Aarch64Linux,
        NixpkgsRevision::new("a62e6edd6d5e1fa0329b8653c801147986f8d446")?,
        NarHash::new("sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=")?,
        OutputSelection::default_selection(),
    )?;

    assert_eq!(
        normalize_derivation(raw, &request, root)
            .unwrap_err()
            .code(),
        crate::NixAdapterErrorCode::OperationFailed
    );
    Ok(())
}

#[test]
fn path_info_v2_filters_upstream_self_reference_and_sums_closure()
-> Result<(), Box<dyn std::error::Error>> {
    let raw = br#"{"info":{"22222222222222222222222222222222-demo":{"ca":null,"deriver":"00000000000000000000000000000000-demo.drv","narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":5,"references":["22222222222222222222222222222222-demo","33333333333333333333333333333333-dep"],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"version":2},"33333333333333333333333333333333-dep":{"ca":{"hash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","method":"nar"},"deriver":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":7,"references":["33333333333333333333333333333333-dep"],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":false,"version":2}},"storeDir":"/nix/store","version":2}"#;
    let adapter = RealNixAdapter::scripted(Scripted::new(vec![success(raw.as_slice())]));
    let path = StorePath::new("/nix/store/22222222222222222222222222222222-demo")?;
    let report = adapter.path_info(&path)?;
    assert_eq!(report.nar_size(), 5);
    assert_eq!(report.closure_size(), 12);
    assert_eq!(report.references().len(), 1);
    Ok(())
}

#[test]
fn build_cache_probe_falls_back_from_failed_batch_to_exact_hits_and_misses()
-> Result<(), Box<dyn std::error::Error>> {
    let local = StorePath::new("/nix/store/22222222222222222222222222222222-local")?;
    let remote = StorePath::new("/nix/store/33333333333333333333333333333333-remote")?;
    let missing = StorePath::new("/nix/store/44444444444444444444444444444444-missing")?;
    let local_json = br#"{"info":{"22222222222222222222222222222222-local":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":11,"references":[],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":true,"url":null,"version":2},"33333333333333333333333333333333-remote":null,"44444444444444444444444444444444-missing":null},"storeDir":"/nix/store","version":2}"#;
    let exact_remote_json = br#"{"info":{"33333333333333333333333333333333-remote":{"ca":null,"compression":"xz","deriver":null,"downloadHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","downloadSize":7,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":13,"references":[],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"url":"nar/example.nar.xz","version":2}},"storeDir":"/nix/store","version":2}"#;
    let executor = Scripted::new(vec![
        success(Vec::new()),
        success(local_json.as_slice()),
        success(Vec::new()),
        failure(1),
        failure(1),
        success(exact_remote_json.as_slice()),
        failure(1),
        failure(1),
        success(Vec::new()),
    ]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);

    let observations = adapter.inspect(&[local.clone(), remote.clone(), missing.clone()])?;

    assert_eq!(
        observations,
        vec![
            CachePathObservation::hit(local.clone(), 0, 11),
            CachePathObservation::hit(remote.clone(), 7, 13),
            CachePathObservation::miss(missing.clone()),
        ]
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 9);
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.iter().any(|argument| argument == "--store"))
            .count(),
        7
    );
    assert!(calls.iter().any(|call| {
        [local.as_str(), remote.as_str(), missing.as_str()]
            .iter()
            .all(|path| call.iter().any(|argument| argument == path))
    }));
    assert!(calls.iter().any(|call| {
        [remote.as_str(), missing.as_str()]
            .iter()
            .all(|path| call.iter().any(|argument| argument == path))
            && !call.iter().any(|argument| argument == local.as_str())
    }));
    assert!(calls.iter().any(|call| {
        call.windows(4).any(|arguments| {
            arguments
                == [
                    OsString::from("--no-contents"),
                    OsString::from("--sigs-needed"),
                    OsString::from("1"),
                    OsString::from("/nix/store/33333333333333333333333333333333-remote"),
                ]
        })
    }));
    for call in calls.iter() {
        assert!(!call.iter().any(|argument| {
            argument == "--substituters" || argument == "--trusted-public-keys"
        }));
    }
    Ok(())
}

#[test]
fn download_probe_accounts_for_the_complete_missing_closure()
-> Result<(), Box<dyn std::error::Error>> {
    let root = StorePath::new("/nix/store/22222222222222222222222222222222-root")?;
    let dep = StorePath::new("/nix/store/33333333333333333333333333333333-dep")?;
    let remote_root_json = br#"{"info":{"22222222222222222222222222222222-root":{"ca":null,"compression":"xz","deriver":null,"downloadHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","downloadSize":7,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":13,"references":["33333333333333333333333333333333-dep"],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"url":"nar/root.nar.xz","version":2}},"storeDir":"/nix/store","version":2}"#;
    let remote_json = br#"{"info":{"22222222222222222222222222222222-root":{"ca":null,"compression":"xz","deriver":null,"downloadHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","downloadSize":7,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":13,"references":["33333333333333333333333333333333-dep"],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"url":"nar/root.nar.xz","version":2},"33333333333333333333333333333333-dep":{"ca":null,"compression":"xz","deriver":null,"downloadHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","downloadSize":5,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":11,"references":[],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"url":"nar/dep.nar.xz","version":2}},"storeDir":"/nix/store","version":2}"#;
    let executor = Scripted::new(vec![
        success(Vec::new()),
        failure(1),
        success(Vec::new()),
        success(remote_root_json.as_slice()),
        success(remote_json.as_slice()),
        failure(1),
        failure(1),
        success(Vec::new()),
    ]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);

    let closures = adapter.inspect_download_closures(std::slice::from_ref(&root))?;
    assert_eq!(
        closures,
        vec![CacheDownloadClosure::new(
            root.clone(),
            vec![
                CachePathObservation::hit(root.clone(), 7, 13),
                CachePathObservation::hit(dep.clone(), 5, 11),
            ],
        )?]
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    let verify = calls
        .iter()
        .filter(|call| call.iter().any(|argument| argument == "verify"))
        .collect::<Vec<_>>();
    assert_eq!(verify.len(), 1);
    assert!(verify[0].contains(&OsString::from(root.as_str())));
    assert!(verify[0].contains(&OsString::from(dep.as_str())));
    Ok(())
}

#[test]
fn download_probe_preserves_remote_root_miss_before_recursive_expansion()
-> Result<(), Box<dyn std::error::Error>> {
    let root = StorePath::new("/nix/store/44444444444444444444444444444444-missing")?;
    let executor = Scripted::new(vec![
        success(Vec::new()),
        failure(1),
        success(Vec::new()),
        failure(1),
    ]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);

    assert_eq!(
        adapter.inspect_download_closures(std::slice::from_ref(&root))?,
        vec![CacheDownloadClosure::new(
            root.clone(),
            vec![CachePathObservation::miss(root)],
        )?]
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 4);
    assert!(!calls[3].iter().any(|argument| argument == "--recursive"));
    Ok(())
}

#[test]
fn download_probe_refuses_recursive_failure_after_confirmed_root_hit()
-> Result<(), Box<dyn std::error::Error>> {
    let root = StorePath::new("/nix/store/22222222222222222222222222222222-root")?;
    let remote_root_json = br#"{"info":{"22222222222222222222222222222222-root":{"ca":null,"compression":"xz","deriver":null,"downloadHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","downloadSize":7,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":13,"references":[],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"url":"nar/root.nar.xz","version":2}},"storeDir":"/nix/store","version":2}"#;
    let adapter = RealNixAdapter::scripted(Scripted::new(vec![
        success(Vec::new()),
        failure(1),
        success(Vec::new()),
        success(remote_root_json.as_slice()),
        failure(1),
    ]));

    assert_eq!(
        adapter
            .inspect_download_closures(std::slice::from_ref(&root))
            .unwrap_err()
            .code(),
        BuildCacheErrorCode::ProbeFailed
    );
    Ok(())
}

#[test]
fn internal_json_build_progress_is_bounded_monotonic_and_path_free()
-> Result<(), Box<dyn std::error::Error>> {
    let mut parser = InternalBuildProgressParser::default();
    let mut observed = Vec::new();
    let mut collect = |estimate: BuildProgressEstimate| {
        observed.push(estimate.millionths());
        Ok(())
    };
    parser.push(
            b"noise\n@nix {\"action\":\"start\",\"fields\":[],\"id\":9,\"level\":3,\"parent\":0,\"text\":\"\",\"type\":104}\n",
            &mut collect,
        )?;
    parser.push(
            b"@nix {\"action\":\"start\",\"fields\":[\"/nix/store/private.drv\",\"\",1,1],\"id\":10,\"level\":3,\"parent\":9,\"text\":\"private\",\"type\":105}\n@nix {\"action\":\"result\",\"fields\":[1,4,1,0],\"id\":9,\"type\":105}\n",
            &mut collect,
        )?;
    parser.push(
            b"@nix {\"action\":\"result\",\"fields\":[1,5,1,0],\"id\":9,\"type\":105}\n@nix {\"action\":\"result\",\"fields\":[4,4,0,0],\"id\":9,\"type\":105}\n",
            &mut collect,
        )?;
    parser.finish(&mut collect)?;

    assert_eq!(observed, vec![250_000, 999_999]);
    Ok(())
}

#[test]
fn internal_json_parser_recovers_after_oversized_private_line()
-> Result<(), Box<dyn std::error::Error>> {
    let mut parser = InternalBuildProgressParser::default();
    let oversized = vec![b'x'; MAX_INTERNAL_JSON_LINE_BYTES + 1];
    parser.push(&oversized, &mut |_| Ok(()))?;
    let mut observed = Vec::new();
    parser.push(
            b"\n@nix {\"action\":\"start\",\"fields\":[],\"id\":7,\"level\":3,\"parent\":0,\"text\":\"\",\"type\":104}\n@nix {\"action\":\"result\",\"fields\":[1,2,1,0],\"id\":7,\"type\":105}\n",
            &mut |estimate| {
                observed.push(estimate.millionths());
                Ok(())
            },
        )?;
    assert_eq!(observed, vec![500_000]);
    Ok(())
}

#[test]
fn internal_json_progress_sink_failure_stops_parsing() {
    let mut parser = InternalBuildProgressParser::default();
    parser
            .push(
                b"@nix {\"action\":\"start\",\"fields\":[],\"id\":7,\"level\":3,\"parent\":0,\"text\":\"\",\"type\":104}\n",
                &mut |_| Ok(()),
            )
            .unwrap();
    assert_eq!(
        parser
            .push(
                b"@nix {\"action\":\"result\",\"fields\":[1,2,1,0],\"id\":7,\"type\":105}\n",
                &mut |_| Err(NixAdapterError::OperationFailed),
            )
            .unwrap_err()
            .code(),
        crate::NixAdapterErrorCode::OperationFailed
    );
}

#[test]
fn substitution_copies_signatures_before_local_metadata_and_fails_closed()
-> Result<(), Box<dyn std::error::Error>> {
    let path = StorePath::new("/nix/store/22222222222222222222222222222222-first")?;
    let path_info = br#"{"info":{"22222222222222222222222222222222-first":{"ca":null,"compression":"xz","deriver":null,"downloadHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","downloadSize":7,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":13,"references":[],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"url":"nar/first.nar.xz","version":2}},"storeDir":"/nix/store","version":2}"#;
    for (copy_sigs, succeeds) in [
        (Ok(success(Vec::new())), true),
        (Ok(failure(2)), false),
        (Err(NixAdapterError::Unavailable), false),
    ] {
        let executor = Scripted::with_results(vec![
            Ok(success(Vec::new())),
            Ok(success(path_info.as_slice())),
            Ok(success(Vec::new())),
            copy_sigs,
            Ok(success(path_info.as_slice())),
        ]);
        let calls = Arc::clone(&executor.calls);

        let result = RealNixAdapter::scripted(executor).substitute(&path);

        let calls = calls.lock().map_err(|_| "poisoned call log")?;
        assert!(calls[2].iter().any(|argument| argument == "copy"));
        assert_eq!(
            &calls[3][5..],
            &os_args([
                "store",
                "copy-sigs",
                "--substituter",
                CACHE_URL,
                "--recursive",
                path.as_str(),
            ])
        );
        if succeeds {
            assert_eq!(result?.outcome(), SubstituteOutcome::Fetched);
            assert_eq!(calls.len(), 5);
            assert!(calls[4].iter().any(|argument| argument == "path-info"));
            assert!(!calls[4].iter().any(|argument| argument == "--store"));
        } else {
            assert_eq!(
                result.expect_err("copy-sigs failure must refuse").code(),
                crate::NixAdapterErrorCode::TrustFailure
            );
            assert_eq!(calls.len(), 4);
        }
    }
    Ok(())
}

#[test]
fn substitution_batch_uses_one_remote_query_and_copy() -> Result<(), Box<dyn std::error::Error>> {
    let first = StorePath::new("/nix/store/22222222222222222222222222222222-first")?;
    let second = StorePath::new("/nix/store/33333333333333333333333333333333-second")?;
    let path_info = br#"{"info":{"22222222222222222222222222222222-first":{"ca":null,"compression":"xz","deriver":null,"downloadHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","downloadSize":7,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":13,"references":[],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"url":"nar/first.nar.xz","version":2},"33333333333333333333333333333333-second":{"ca":null,"compression":"xz","deriver":null,"downloadHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","downloadSize":5,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":11,"references":[],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"url":"nar/second.nar.xz","version":2}},"storeDir":"/nix/store","version":2}"#;
    let executor = Scripted::new(vec![
        success(Vec::new()),
        success(path_info.as_slice()),
        success(Vec::new()),
        success(Vec::new()),
        success(path_info.as_slice()),
    ]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);

    let reports = adapter.substitute_many(&[first.clone(), second.clone()])?;

    assert_eq!(reports.len(), 2);
    assert!(
        reports
            .iter()
            .all(|report| report.outcome() == SubstituteOutcome::Fetched)
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 5);
    for call in [&calls[1], &calls[2], &calls[3], &calls[4]] {
        assert!(call.contains(&OsString::from(first.as_str())));
        assert!(call.contains(&OsString::from(second.as_str())));
    }
    assert!(calls[1].iter().any(|argument| argument == "path-info"));
    assert!(calls[2].iter().any(|argument| argument == "copy"));
    assert_eq!(
        &calls[3][5..],
        &os_args([
            "store",
            "copy-sigs",
            "--substituter",
            CACHE_URL,
            "--recursive",
            first.as_str(),
            second.as_str(),
        ])
    );
    assert!(!calls[4].iter().any(|argument| argument == "--store"));
    Ok(())
}

#[test]
fn substitution_batch_confirms_an_omitted_remote_path() -> Result<(), Box<dyn std::error::Error>> {
    let first = StorePath::new("/nix/store/22222222222222222222222222222222-first")?;
    let second = StorePath::new("/nix/store/33333333333333333333333333333333-second")?;
    let first_only = br#"{"info":{"22222222222222222222222222222222-first":{"ca":null,"compression":"xz","deriver":null,"downloadHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","downloadSize":7,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":13,"references":[],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"url":"nar/first.nar.xz","version":2}},"storeDir":"/nix/store","version":2}"#;
    let second_only = br#"{"info":{"33333333333333333333333333333333-second":{"ca":null,"compression":"xz","deriver":null,"downloadHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","downloadSize":5,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":11,"references":[],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"url":"nar/second.nar.xz","version":2}},"storeDir":"/nix/store","version":2}"#;
    let both = br#"{"info":{"22222222222222222222222222222222-first":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":13,"references":[],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":true,"url":null,"version":2},"33333333333333333333333333333333-second":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":11,"references":[],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":true,"url":null,"version":2}},"storeDir":"/nix/store","version":2}"#;
    let executor = Scripted::new(vec![
        success(Vec::new()),
        success(first_only.as_slice()),
        success(second_only.as_slice()),
        success(Vec::new()),
        success(Vec::new()),
        success(both.as_slice()),
    ]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);

    let reports = adapter.substitute_many(&[first.clone(), second.clone()])?;

    assert!(
        reports
            .iter()
            .all(|report| report.outcome() == SubstituteOutcome::Fetched)
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 6);
    assert!(calls[2].contains(&OsString::from(second.as_str())));
    assert!(!calls[2].contains(&OsString::from(first.as_str())));
    Ok(())
}

#[cfg(unix)]
#[test]
fn noisy_stderr_cannot_starve_timeout_or_progress_cancellation()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    fs::create_dir(home.path().join("tmp"))?;
    let executor = ProcessExecutor {
        nix_binary: PathBuf::from("/bin/sh"),
        nix_store_binary: PathBuf::from("/bin/sh"),
        private_home: home.path().to_path_buf(),
        daemon_socket: Some(PathBuf::from(MANAGED_DAEMON_SOCKET)),
    };
    let noisy = || CommandSpec {
        program: NixProgram::Modern,
        args: os_args(["-c", "while :; do printf 'noise\\n' >&2; done"]),
        timeout: Duration::from_millis(100),
    };

    let started = Instant::now();
    let timed_out = execute_checked_with_stderr(
        &executor,
        NixProgram::Modern,
        noisy().args,
        Duration::from_millis(100),
        &|| false,
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(timed_out.code(), crate::NixAdapterErrorCode::Timeout);
    assert!(started.elapsed() < Duration::from_secs(5));

    let started = Instant::now();
    let cancelled = executor
        .execute_with_stderr(noisy(), &|| false, &mut |_| {
            Err(NixAdapterError::OperationFailed)
        })
        .unwrap_err();
    assert_eq!(
        cancelled.code(),
        crate::NixAdapterErrorCode::OperationFailed
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    Ok(())
}

#[cfg(unix)]
#[test]
fn progress_callback_failure_reaps_a_silent_child_process_group()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    fs::create_dir(home.path().join("tmp"))?;
    let executor = ProcessExecutor {
        nix_binary: PathBuf::from("/bin/sh"),
        nix_store_binary: PathBuf::from("/bin/sh"),
        private_home: home.path().to_path_buf(),
        daemon_socket: Some(PathBuf::from(MANAGED_DAEMON_SOCKET)),
    };
    let started = Instant::now();
    let disconnected = AtomicBool::new(false);
    let mut events = 0;
    let result = executor.execute_with_stderr(
        CommandSpec {
            program: NixProgram::Modern,
            args: os_args([
                "-c",
                "printf '%s' $$ > \"$HOME/child.pid\"; printf 'progress\\n' >&2; exec sleep 30",
            ]),
            timeout: Duration::from_secs(30),
        },
        &|| disconnected.load(Ordering::Acquire),
        &mut |_| {
            events += 1;
            let client_callback = Err::<(), _>(NixAdapterError::OperationFailed);
            if client_callback.is_err() {
                disconnected.store(true, Ordering::Release);
            }
            Ok(())
        },
    );

    assert_eq!(
        result.unwrap_err().code(),
        crate::NixAdapterErrorCode::Unavailable
    );
    assert_eq!(events, 1);
    assert!(started.elapsed() < Duration::from_secs(5));
    let pid = fs::read_to_string(home.path().join("child.pid"))?.parse::<i32>()?;
    let group = Pid::from_raw(pid).ok_or("invalid child process group")?;
    assert_eq!(kill_process_group(group, Signal::CONT), Err(Errno::SRCH));
    Ok(())
}

#[test]
fn build_cache_probe_never_contacts_remote_for_local_hits() -> Result<(), Box<dyn std::error::Error>>
{
    let path = StorePath::new("/nix/store/22222222222222222222222222222222-local")?;
    let local_json = br#"{"info":{"22222222222222222222222222222222-local":{"ca":null,"compression":null,"deriver":null,"downloadHash":null,"downloadSize":null,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":11,"references":[],"registrationTime":1,"signatures":[],"storeDir":"/nix/store","ultimate":true,"url":null,"version":2}},"storeDir":"/nix/store","version":2}"#;
    let executor = Scripted::new(vec![success(Vec::new()), success(local_json.as_slice())]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);

    assert_eq!(
        adapter.inspect(std::slice::from_ref(&path))?,
        vec![CachePathObservation::hit(path, 0, 11)]
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|call| !call.iter().any(|argument| argument == "--store"))
    );
    Ok(())
}

#[test]
fn build_cache_probe_refuses_non_missing_exact_failure() -> Result<(), Box<dyn std::error::Error>> {
    let path = StorePath::new("/nix/store/44444444444444444444444444444444-missing")?;
    let adapter = RealNixAdapter::scripted(Scripted::with_results(vec![
        Ok(success(Vec::new())),
        Ok(failure(1)),
        Ok(success(Vec::new())),
        Ok(failure(1)),
        Err(NixAdapterError::PermissionDenied),
    ]));

    assert_eq!(
        adapter.inspect(&[path]).unwrap_err().code(),
        BuildCacheErrorCode::ProbeFailed
    );
    Ok(())
}

#[test]
fn build_cache_probe_refuses_malformed_exact_remote_json() -> Result<(), Box<dyn std::error::Error>>
{
    let path = StorePath::new("/nix/store/44444444444444444444444444444444-missing")?;
    let adapter = RealNixAdapter::scripted(Scripted::new(vec![
        success(Vec::new()),
        failure(1),
        success(Vec::new()),
        failure(1),
        success(br#"{"info":[]}"#.as_slice()),
    ]));

    assert_eq!(
        adapter.inspect(&[path]).unwrap_err().code(),
        BuildCacheErrorCode::ProbeFailed
    );
    Ok(())
}

#[test]
fn build_cache_probe_refuses_unverified_remote_signature() -> Result<(), Box<dyn std::error::Error>>
{
    let path = StorePath::new("/nix/store/33333333333333333333333333333333-remote")?;
    let remote_json = br#"{"info":{"33333333333333333333333333333333-remote":{"ca":null,"compression":"xz","deriver":null,"downloadHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","downloadSize":7,"narHash":"sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=","narSize":13,"references":[],"registrationTime":1,"signatures":["cache.nixos.org-1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=="],"storeDir":"/nix/store","ultimate":false,"url":"nar/example.nar.xz","version":2}},"storeDir":"/nix/store","version":2}"#;
    let adapter = RealNixAdapter::scripted(Scripted::new(vec![
        success(Vec::new()),
        failure(1),
        success(Vec::new()),
        success(remote_json.as_slice()),
        failure(2),
    ]));

    assert_eq!(
        adapter.inspect(&[path]).unwrap_err().code(),
        BuildCacheErrorCode::ProbeFailed
    );
    Ok(())
}

#[test]
fn fixed_args_never_accept_caller_trust_or_store_controls() -> Result<(), Box<dyn std::error::Error>>
{
    let executor = Scripted::new(vec![
        success("nix (Nix) 2.34.8\n"),
        success("nix-store (Nix) 2.34.8\n"),
    ]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);
    let _ = adapter.version()?;
    let forbidden = [
        "--substituters",
        "--trusted-public-keys",
        "--builders",
        "--store",
    ];
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 2);
    for call in calls.iter() {
        for value in forbidden {
            assert!(!call.iter().any(|argument| argument == value));
        }
    }
    let _ = PolicyVersion::from_u64(1).ok_or("policy")?;
    Ok(())
}

#[test]
fn gc_preflights_dead_paths_then_reports_only_actual_deletions()
-> Result<(), Box<dyn std::error::Error>> {
    let first = "/nix/store/22222222222222222222222222222222-first";
    let second = "/nix/store/33333333333333333333333333333333-second";
    let executor = Scripted::new(vec![
        success(format!("{first}\n/nix/store/trash\n{second}\n")),
        success_with_stderr(format!(
            "finding garbage collector roots...\ndeleting garbage...\ndeleting '/nix/store/trash'\ndeleting '{second}'\n"
        )),
    ]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);

    let report = adapter.gc()?;

    assert_eq!(report.collected(), &[StorePath::new(second)?]);
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls[0], os_args(["--gc", "--print-dead"]));
    assert_eq!(calls[1], os_args(["--gc"]));
    Ok(())
}

#[test]
fn build_json_accepts_pinned_optional_timing_metrics() -> Result<(), NixAdapterError> {
    let raw = br#"[{"drvPath":"/nix/store/00000000000000000000000000000000-demo.drv","outputs":{"out":"/nix/store/22222222222222222222222222222222-demo"},"startTime":30,"stopTime":50,"cpuUser":1.25,"cpuSystem":0.5}]"#;
    let results: Vec<RawBuildResult> = parse_json(raw)?;

    assert_eq!(results.len(), 1);
    validate_build_metrics(&results[0])?;
    assert_eq!(results[0].start_time, Some(30));
    assert_eq!(results[0].stop_time, Some(50));
    assert_eq!(results[0].cpu_user, Some(1.25));
    assert_eq!(results[0].cpu_system, Some(0.5));
    Ok(())
}

#[test]
fn build_provenance_requires_ultimate_or_cryptographic_trust()
-> Result<(), Box<dyn std::error::Error>> {
    let path = StorePath::new("/nix/store/22222222222222222222222222222222-demo")?;
    let local = RealNixAdapter::scripted(Scripted::new(Vec::new()));
    assert_eq!(
        classify_build_provenance(&local, &path, true, &[])?,
        BuildOutputProvenance::LocalBuild
    );
    assert!(matches!(
        classify_build_provenance(&local, &path, false, &[]),
        Err(NixAdapterError::TrustFailure)
    ));

    let executor = Scripted::new(vec![success(Vec::new())]);
    let calls = Arc::clone(&executor.calls);
    let cached = RealNixAdapter::scripted(executor);
    let signature = Signature::new("cache.nixos.org-1:AAAA")?;
    assert_eq!(
        classify_build_provenance(&cached, &path, false, &[signature])?,
        BuildOutputProvenance::CacheSigned
    );
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 1);
    assert!(calls[0].iter().any(|argument| argument == "--no-contents"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn timeout_terminates_descendants_before_joining_capture_threads()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    fs::create_dir(home.path().join("tmp"))?;
    let executor = ProcessExecutor {
        nix_binary: PathBuf::from("/bin/sh"),
        nix_store_binary: PathBuf::from("/bin/sh"),
        private_home: home.path().to_path_buf(),
        daemon_socket: Some(PathBuf::from(MANAGED_DAEMON_SOCKET)),
    };
    for script in ["sleep 30 & wait", "sleep 30 &"] {
        let started = Instant::now();
        let outcome = executor.execute(CommandSpec {
            program: NixProgram::Modern,
            args: os_args(["-c", script]),
            timeout: Duration::from_millis(100),
        })?;

        assert!(outcome.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
    Ok(())
}

#[test]
fn recursive_verify_dimension_cannot_drop_closure_semantics()
-> Result<(), Box<dyn std::error::Error>> {
    let executor = Scripted::new(vec![success(Vec::new())]);
    let calls = Arc::clone(&executor.calls);
    let adapter = RealNixAdapter::scripted(executor);
    let path = StorePath::new("/nix/store/22222222222222222222222222222222-demo")?;
    assert!(verify_dimension(&adapter, &path, "--no-trust", 1, true)?);
    let calls = calls.lock().map_err(|_| "poisoned call log")?;
    assert_eq!(calls.len(), 1);
    assert!(calls[0].iter().any(|argument| argument == "--recursive"));
    Ok(())
}
