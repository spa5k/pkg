//! Tests for the bootstrap module.

use super::{backend::*, provision::*, recovery::*};

use super::*;
use pkg_core::state::Digest;
use pkg_nix::ManagedGroupBindings;
use pkg_testkit::{ChaosCheckpoint, ChaosCommand, FsyncMode, publish_checkpoint};
use std::{
    cell::RefCell,
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
    os::unix::process::ExitStatusExt as _,
    path::Path,
    thread,
    time::{Duration, Instant},
};

const SUPERVISOR_LOSS_CHILD_ENV: &str = "PKG_TEST_DN15_SUPERVISOR_LOSS_CHILD";
const SUPERVISOR_LOSS_ROOT_ENV: &str = "PKG_TEST_DN15_SUPERVISOR_LOSS_ROOT";
const SUPERVISOR_LOSS_EXECUTABLE_ENV: &str = "PKG_TEST_DN15_SUPERVISOR_LOSS_EXECUTABLE";

#[test]
fn linux_recovery_context_binds_installation_and_scratch_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let digest = Digest::from_bytes([0x90; 32]);
    let groups = ManagedGroupBindings::new(100, 101)?;
    let context = |installation_root: &Path, scratch_parent: &Path| {
        linux_recovery_context_digest(
            digest,
            &InstallerProvisionRequest {
                repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
                datastore: Path::new("/state"),
                installation_root,
                scratch_parent,
                system: System::X8664Linux,
                groups,
            },
        )
    };

    let expected = context(Path::new("/"), Path::new("/scratch"));
    assert_ne!(
        expected,
        context(Path::new("/target"), Path::new("/scratch"))
    );
    assert_ne!(
        expected,
        context(Path::new("/"), Path::new("/other-scratch"))
    );
    Ok(())
}

#[test]
fn linux_auth_datastore_accepts_only_exact_private_restart_state()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let uid = nix::unistd::Uid::effective().as_raw();
    let gid = nix::unistd::Gid::effective().as_raw();

    let exact = root.path().join("exact");
    prepare_linux_auth_datastore_at(&exact, uid, gid)?;
    prepare_linux_auth_datastore_at(&exact, uid, gid)?;
    for name in ["pkg-channel.lock", "accepted-channel.initializing"] {
        fs::File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(exact.join(name))?;
    }
    for name in [
        "root.json",
        "timestamp.json",
        "snapshot.json",
        "targets.json",
        "latest_known_time.json",
    ] {
        let path = exact.join(name);
        fs::write(&path, b"{}")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    }
    prepare_linux_auth_datastore_at(&exact, uid, gid)?;
    assert_eq!(fs::read_dir(&exact)?.count(), 2);
    remove_linux_auth_datastore_at(&exact, uid, gid)?;
    assert!(!exact.exists());

    let legacy_pool = root.path().join("legacy-pool");
    prepare_private_directory_at(&legacy_pool, uid, gid)?;
    fs::File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(legacy_pool.join("pkg-channel.lock"))?;
    let legacy_metadata = legacy_pool.join("root.json");
    fs::write(&legacy_metadata, b"{}")?;
    fs::set_permissions(&legacy_metadata, fs::Permissions::from_mode(0o644))?;
    remove_legacy_linux_auth_datastore_files(&legacy_pool, uid, gid)?;
    assert_eq!(fs::read_dir(&legacy_pool)?.count(), 0);

    let legacy_foreign = root.path().join("legacy-foreign");
    prepare_private_directory_at(&legacy_foreign, uid, gid)?;
    fs::write(legacy_foreign.join("foreign"), [])?;
    assert!(remove_legacy_linux_auth_datastore_files(&legacy_foreign, uid, gid).is_err());
    assert!(legacy_foreign.join("foreign").exists());

    let unknown = root.path().join("unknown");
    prepare_linux_auth_datastore_at(&unknown, uid, gid)?;
    fs::write(unknown.join("foreign"), [])?;
    assert!(prepare_linux_auth_datastore_at(&unknown, uid, gid).is_err());

    let permissive = root.path().join("permissive");
    fs::DirBuilder::new().mode(0o755).create(&permissive)?;
    fs::set_permissions(&permissive, fs::Permissions::from_mode(0o755))?;
    assert!(prepare_linux_auth_datastore_at(&permissive, uid, gid).is_err());

    let linked = root.path().join("linked");
    symlink(root.path().join("missing"), &linked)?;
    assert!(prepare_linux_auth_datastore_at(&linked, uid, gid).is_err());

    // The macOS vendor temp directory is created traversable because the
    // vendor's unprivileged Nix build users must stat `TMPDIR`, while every
    // unprivileged write bit stays forbidden.
    let vendor_tmp = root.path().join("vendor-tmp");
    prepare_vendor_tmp_directory_at(&vendor_tmp, uid, gid)?;
    assert_eq!(
        fs::metadata(&vendor_tmp)?.permissions().mode() & 0o7777,
        0o755
    );
    prepare_vendor_tmp_directory_at(&vendor_tmp, uid, gid)?;
    fs::set_permissions(&vendor_tmp, fs::Permissions::from_mode(0o700))?;
    assert!(prepare_vendor_tmp_directory_at(&vendor_tmp, uid, gid).is_ok());
    fs::set_permissions(&vendor_tmp, fs::Permissions::from_mode(0o770))?;
    assert!(prepare_vendor_tmp_directory_at(&vendor_tmp, uid, gid).is_err());
    fs::set_permissions(&vendor_tmp, fs::Permissions::from_mode(0o757))?;
    assert!(prepare_vendor_tmp_directory_at(&vendor_tmp, uid, gid).is_err());
    let vendor_linked = root.path().join("vendor-linked");
    symlink(root.path().join("missing"), &vendor_linked)?;
    assert!(prepare_vendor_tmp_directory_at(&vendor_linked, uid, gid).is_err());
    let vendor_file = root.path().join("vendor-file");
    fs::write(&vendor_file, b"not a directory")?;
    assert!(prepare_vendor_tmp_directory_at(&vendor_file, uid, gid).is_err());

    let pool = root.path().join("pool");
    prepare_private_directory_at(&pool, uid, gid)?;
    let stale = pool.join(std::process::id().to_string());
    prepare_linux_auth_datastore_at(&stale, uid, gid)?;
    fs::File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(stale.join("pkg-channel.lock"))?;
    remove_stale_linux_auth_datastores(&pool, uid, gid)?;
    assert!(!stale.exists());
    assert!(process_is_alive(std::process::id())?);

    let foreign = pool.join("foreign");
    prepare_private_directory_at(&foreign, uid, gid)?;
    assert!(remove_stale_linux_auth_datastores(&pool, uid, gid).is_err());
    Ok(())
}

#[derive(Default)]
struct MemoryJournalPersistence {
    snapshots: RefCell<Vec<LinuxInstallJournal>>,
    committed: std::rc::Rc<std::cell::Cell<bool>>,
}

impl LinuxJournalPersistence for MemoryJournalPersistence {
    fn replace(&self, journal: &LinuxInstallJournal) -> Result<(), InstallError> {
        self.snapshots.borrow_mut().push(journal.clone());
        if journal.is_committed() {
            self.committed.set(true);
        }
        Ok(())
    }
}

#[derive(Default)]
struct MacMemoryJournalPersistence {
    snapshots: RefCell<Vec<MacOsInstallJournal>>,
}

impl MacOsJournalPersistence for MacMemoryJournalPersistence {
    fn replace(&self, journal: &MacOsInstallJournal) -> Result<(), MacOsError> {
        self.snapshots.borrow_mut().push(journal.clone());
        Ok(())
    }
}

struct StubProvisioner {
    calls: usize,
    rolled_back: std::rc::Rc<std::cell::Cell<bool>>,
}

impl BundleProvisioner for StubProvisioner {
    fn provision(
        &mut self,
        _request: &InstallerProvisionRequest<'_>,
    ) -> Result<BootstrapOutcome, BundleProvisionError> {
        self.calls = self.calls.saturating_add(1);
        Ok(BootstrapOutcome::Stub(self.rolled_back.clone()))
    }
}

struct ReauthProvisioner {
    calls: usize,
    reauthenticated: bool,
    reuse_existing: bool,
}

impl BundleProvisioner for ReauthProvisioner {
    fn reuse_existing(&mut self) -> Result<bool, BundleProvisionError> {
        Ok(self.reuse_existing)
    }

    fn reauthenticate_linux(
        &mut self,
        _request: &InstallerProvisionRequest<'_>,
        _backend: &mut dyn LinuxInstallBackend,
    ) -> Result<(), BundleProvisionError> {
        self.reauthenticated = true;
        Ok(())
    }

    fn provision(
        &mut self,
        _request: &InstallerProvisionRequest<'_>,
    ) -> Result<BootstrapOutcome, BundleProvisionError> {
        if !self.reauthenticated {
            return Err(BundleProvisionError::Failed);
        }
        self.calls = self.calls.saturating_add(1);
        Ok(BootstrapOutcome::Stub(std::rc::Rc::new(
            std::cell::Cell::new(false),
        )))
    }
}

struct RollbackFailedProvisioner;

impl BundleProvisioner for RollbackFailedProvisioner {
    fn provision(
        &mut self,
        _request: &InstallerProvisionRequest<'_>,
    ) -> Result<BootstrapOutcome, BundleProvisionError> {
        Err(BundleProvisionError::RollbackIncomplete)
    }
}

const TEST_INSTALLED_INSTALLER: &[u8] = b"test installed Determinate helper";

struct RealDeterminateFixture {
    temporary: tempfile::TempDir,
}

impl RealDeterminateFixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
        let installer = temporary.path().join("nix-installer");
        fs::write(&installer, TEST_INSTALLED_INSTALLER)?;
        fs::set_permissions(&installer, fs::Permissions::from_mode(0o755))?;
        Ok(Self { temporary })
    }

    fn handoff(&self) -> Result<DeterminateHandoff, Box<dyn std::error::Error>> {
        Ok(DeterminateHandoff::for_test_bytes(
            self.temporary.path(),
            0o600,
            TEST_INSTALLED_INSTALLER,
        )?)
    }

    fn receipt(&self) -> std::path::PathBuf {
        self.temporary.path().join("receipt.json")
    }

    fn marker(&self, name: &str) -> std::path::PathBuf {
        self.temporary.path().join(name)
    }

    fn write_receipt(&self) -> Result<(), Box<dyn std::error::Error>> {
        fs::write(self.receipt(), b"opaque test receipt")?;
        fs::set_permissions(self.receipt(), fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
}

fn vendor_exit_zero(
    handoff: &DeterminateHandoff,
    receipt: &Path,
    marker: &Path,
) -> Result<DeterminateProcessOutcome, BundleProvisionError> {
    if handoff.state() != Ok(DeterminateHandoffState::Started) {
        return Err(BundleProvisionError::Failed);
    }
    let status = std::process::Command::new("/bin/sh")
        .args([
            std::ffi::OsStr::new("-c"),
            std::ffi::OsStr::new(
                "umask 077; printf 'opaque test receipt' > \"$1\"; printf '%s\\n' $$ >> \"$2\"",
            ),
            std::ffi::OsStr::new("determinate-test"),
        ])
        .arg(receipt)
        .arg(marker)
        .status()
        .map_err(|_| BundleProvisionError::Failed)?;
    let terminal = status.code().map_or_else(
        || {
            std::os::unix::process::ExitStatusExt::signal(&status)
                .map(DeterminateTerminal::Signaled)
                .ok_or(BundleProvisionError::Failed)
        },
        |code| Ok(DeterminateTerminal::Exited(code)),
    )?;
    Ok(DeterminateProcessOutcome {
        terminal,
        stdout_truncated: false,
        stderr_truncated: false,
    })
}

struct RealDeterminateProvisioner {
    handoff: Option<DeterminateHandoff>,
    receipt: std::path::PathBuf,
    marker: std::path::PathBuf,
}

impl BundleProvisioner for RealDeterminateProvisioner {
    fn provision(
        &mut self,
        _request: &InstallerProvisionRequest<'_>,
    ) -> Result<BootstrapOutcome, BundleProvisionError> {
        let handoff = self.handoff.take().ok_or(BundleProvisionError::Failed)?;
        let outcome = run_with_new_determinate_handoff(&handoff, || {
            vendor_exit_zero(&handoff, &self.receipt, &self.marker)
        })?;
        if !determinate_succeeded(outcome) {
            return Err(BundleProvisionError::RollbackIncomplete);
        }
        Ok(BootstrapOutcome::DeterminateTestPending(Box::new(handoff)))
    }
}

fn write_vendor_script(
    fixture: &RealDeterminateFixture,
    name: &str,
    body: &str,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let directory = fixture.temporary.path().join("bin");
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let executable = directory.join(name);
    fs::write(&executable, format!("#!/bin/sh\n{body}\n"))?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o500))?;
    Ok(executable)
}

fn staged_installer_identity(
    path: &Path,
) -> Result<DeterminateInstaller, Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    Ok(DeterminateInstaller::new(
        u64::try_from(bytes.len())?,
        Digest::from_bytes(Sha256::digest(bytes).into()),
    ))
}

fn restart_refuses_vendor_start(handoff: &DeterminateHandoff, marker: &Path) -> bool {
    let retry = run_with_new_determinate_handoff(handoff, || {
        fs::write(marker, b"second start").map_err(|_| BundleProvisionError::Failed)?;
        Ok(())
    });
    matches!(retry, Err(BundleProvisionError::Failed)) && !marker.exists()
}

fn assert_restart_refuses_vendor_start(handoff: &DeterminateHandoff, marker: &Path) {
    assert!(restart_refuses_vendor_start(handoff, marker));
}

fn assert_terminal_failure_preserves_started_and_refuses_retry(
    name: &str,
    body: &str,
    expected: DeterminateTerminal,
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = RealDeterminateFixture::new()?;
    let handoff = fixture.handoff()?;
    let executable = write_vendor_script(&fixture, name, body)?;
    let outcome = run_with_new_determinate_handoff(&handoff, || {
        crate::determinate::run_test_install_with_process(
            &executable,
            &staged_installer_identity(&executable).map_err(|_| BundleProvisionError::Failed)?,
            fixture.temporary.path(),
            std::process::Command::spawn,
            std::process::Child::wait,
        )
        .map_err(|_| BundleProvisionError::Failed)
    })
    .map_err(|_| std::io::Error::other("vendor process failed"))?;
    assert_eq!(outcome.terminal, expected);
    assert!(!determinate_succeeded(outcome));
    assert_eq!(
        fixture.handoff()?.state()?,
        DeterminateHandoffState::Started
    );
    assert_restart_refuses_vendor_start(&fixture.handoff()?, &fixture.marker("retry"));
    Ok(())
}

#[test]
fn existing_handoff_refuses_before_vendor_spawn() -> Result<(), Box<dyn std::error::Error>> {
    for accepted in [false, true] {
        let fixture = RealDeterminateFixture::new()?;
        let handoff = fixture.handoff()?;
        handoff.record_started()?;
        if accepted {
            fixture.write_receipt()?;
            handoff.accept_after_installed_state_proof()?;
        }
        assert_restart_refuses_vendor_start(&handoff, &fixture.marker("unexpected-start"));
    }
    Ok(())
}

#[test]
fn spawn_and_wait_uncertainty_preserves_started_and_refuses_retry()
-> Result<(), Box<dyn std::error::Error>> {
    let spawn_fixture = RealDeterminateFixture::new()?;
    let spawn_handoff = spawn_fixture.handoff()?;
    let spawn_executable = write_vendor_script(
        &spawn_fixture,
        "spawn-installer",
        "printf ran > \"$TMPDIR/spawn-ran\"",
    )?;
    let spawn_result = run_with_new_determinate_handoff(&spawn_handoff, || {
        crate::determinate::run_test_install_with_process(
            &spawn_executable,
            &staged_installer_identity(&spawn_executable)
                .map_err(|_| BundleProvisionError::Failed)?,
            spawn_fixture.temporary.path(),
            |_| Err(std::io::Error::other("simulated spawn failure")),
            std::process::Child::wait,
        )
        .map_err(|_| BundleProvisionError::Failed)
    });
    assert!(matches!(spawn_result, Err(BundleProvisionError::Failed)));
    assert_eq!(spawn_handoff.state()?, DeterminateHandoffState::Started);
    assert!(!spawn_fixture.marker("spawn-ran").exists());
    assert_restart_refuses_vendor_start(
        &spawn_fixture.handoff()?,
        &spawn_fixture.marker("spawn-retry"),
    );

    let wait_fixture = RealDeterminateFixture::new()?;
    let wait_handoff = wait_fixture.handoff()?;
    let wait_executable = write_vendor_script(
        &wait_fixture,
        "wait-installer",
        "printf '%s' $$ > \"$TMPDIR/wait.pid\"; sleep 0.05; exit 0",
    )?;
    let wait_result = run_with_new_determinate_handoff(&wait_handoff, || {
        crate::determinate::run_test_install_with_process(
            &wait_executable,
            &staged_installer_identity(&wait_executable)
                .map_err(|_| BundleProvisionError::Failed)?,
            wait_fixture.temporary.path(),
            std::process::Command::spawn,
            |_| Err(std::io::Error::other("simulated wait failure")),
        )
        .map_err(|_| BundleProvisionError::Failed)
    });
    assert!(matches!(wait_result, Err(BundleProvisionError::Failed)));
    assert_eq!(wait_handoff.state()?, DeterminateHandoffState::Started);
    let pid = fs::read_to_string(wait_fixture.marker("wait.pid"))?.parse::<i32>()?;
    assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));
    assert_restart_refuses_vendor_start(
        &wait_fixture.handoff()?,
        &wait_fixture.marker("wait-retry"),
    );
    Ok(())
}

#[test]
fn crash_before_vendor_start_preserves_started_and_refuses_retry()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = RealDeterminateFixture::new()?;
    let handoff = fixture.handoff()?;
    let first =
        run_with_new_determinate_handoff(&handoff, || Err::<(), _>(BundleProvisionError::Failed));
    assert!(matches!(first, Err(BundleProvisionError::Failed)));
    assert_eq!(
        fixture.handoff()?.state()?,
        DeterminateHandoffState::Started
    );
    assert_restart_refuses_vendor_start(&fixture.handoff()?, &fixture.marker("vendor-ran"));
    Ok(())
}

#[test]
fn nonzero_exit_preserves_started_and_refuses_retry() -> Result<(), Box<dyn std::error::Error>> {
    assert_terminal_failure_preserves_started_and_refuses_retry(
        "nonzero-installer",
        "exit 23",
        DeterminateTerminal::Exited(23),
    )
}

#[test]
fn signal_preserves_started_and_refuses_retry() -> Result<(), Box<dyn std::error::Error>> {
    assert_terminal_failure_preserves_started_and_refuses_retry(
        "signaled-installer",
        "kill -TERM $$",
        DeterminateTerminal::Signaled(15),
    )
}

#[test]
fn real_supervisor_loss_preserves_started_and_refuses_second_start()
-> Result<(), Box<dyn std::error::Error>> {
    let checkpoint = ChaosCheckpoint::new("install-supervisor-lost")?;
    if std::env::var_os(SUPERVISOR_LOSS_CHILD_ENV).is_some() {
        let root = std::path::PathBuf::from(
            std::env::var_os(SUPERVISOR_LOSS_ROOT_ENV).ok_or("missing fixture root")?,
        );
        let executable = std::path::PathBuf::from(
            std::env::var_os(SUPERVISOR_LOSS_EXECUTABLE_ENV).ok_or("missing vendor executable")?,
        );
        let pid_path = root.join("supervisor-loss.pid");
        let handoff = DeterminateHandoff::for_test_bytes(&root, 0o600, TEST_INSTALLED_INSTALLER)?;
        let _ = run_with_new_determinate_handoff(&handoff, || {
            crate::determinate::run_test_install_with_process(
                &executable,
                &staged_installer_identity(&executable)
                    .map_err(|_| BundleProvisionError::Failed)?,
                &root,
                |command| {
                    let mut child = command.spawn()?;
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while !pid_path.try_exists()? {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "vendor pid was not published",
                            ));
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    let _ = publish_checkpoint(&checkpoint).map_err(std::io::Error::other)?;
                    Ok(child)
                },
                std::process::Child::wait,
            )
            .map_err(|_| BundleProvisionError::Failed)
        });
        return Err("supervisor was not terminated at its checkpoint".into());
    }

    let fixture = RealDeterminateFixture::new()?;
    let pid_path = fixture.marker("supervisor-loss.pid");
    let executable = write_vendor_script(
        &fixture,
        "supervisor-loss-installer",
        "printf '%s' $$ > \"$TMPDIR/supervisor-loss.pid\"; \
             attempts=0; \
             while [ ! -e \"$TMPDIR/vendor-release\" ] && [ \"$attempts\" -lt 1000 ]; do \
                 attempts=$((attempts + 1)); sleep 0.01; \
             done; \
             test -e \"$TMPDIR/vendor-release\"",
    )?;
    let mut command = ChaosCommand::new(
        std::env::current_exe()?,
        checkpoint,
        fixture.marker("install-supervisor-lost"),
        FsyncMode::Enabled,
    )?;
    command
        .arg("--exact")
        .arg("bootstrap::tests::real_supervisor_loss_preserves_started_and_refuses_second_start")
        .arg("--nocapture")
        .env(SUPERVISOR_LOSS_CHILD_ENV, "1")
        .env(SUPERVISOR_LOSS_ROOT_ENV, fixture.temporary.path())
        .env(SUPERVISOR_LOSS_EXECUTABLE_ENV, &executable);
    let mut supervisor = command.spawn()?;
    let status = supervisor.kill_at_checkpoint(Duration::from_secs(10))?;
    assert_eq!(status.signal(), Some(9));
    assert_eq!(
        fixture.handoff()?.state()?,
        DeterminateHandoffState::Started
    );
    let pid = fs::read_to_string(pid_path)?.parse::<i32>()?;
    assert_eq!(kill(Pid::from_raw(pid), None), Ok(()));
    fs::write(fixture.marker("vendor-release"), b"release")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while kill(Pid::from_raw(pid), None).is_ok() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));
    assert_restart_refuses_vendor_start(
        &fixture.handoff()?,
        &fixture.marker("second-supervisor-start"),
    );
    Ok(())
}

#[test]
fn only_exit_zero_is_vendor_success() {
    let outcome = |terminal| DeterminateProcessOutcome {
        terminal,
        stdout_truncated: false,
        stderr_truncated: false,
    };

    assert!(determinate_succeeded(outcome(DeterminateTerminal::Exited(
        0
    ))));
    assert!(!determinate_succeeded(outcome(
        DeterminateTerminal::Exited(1)
    )));
    assert!(!determinate_succeeded(outcome(
        DeterminateTerminal::Signaled(15)
    )));
}

#[derive(Default, PartialEq, Eq)]
enum LinuxBackendFailure {
    #[default]
    None,
    Asset,
    Unit,
    BaseNix,
    Activation,
    Health,
    Receipt,
    Finalize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum TestServiceState {
    #[default]
    Stable,
    MutationNeeded,
    Offline,
    EnabledInactive,
    Mixed,
    Unqueryable,
}

#[derive(Default)]
struct LinuxBackend {
    raw_provision_calls: usize,
    create: bool,
    replace_files: bool,
    mode: Option<crate::InstallMode>,
    active_install: bool,
    active_install_checks: usize,
    clean_host_checks: usize,
    service_state: TestServiceState,
    failure: LinuxBackendFailure,
    preflight_handoff: Option<DeterminateHandoffState>,
    managed_runtime_present: Option<bool>,
    mutation_calls: usize,
    file_mutation_calls: usize,
    offline_preflight_calls: usize,
    change_service_state_after_preflight: Option<usize>,
    rollback_calls: usize,
    finalize_calls: usize,
    finalize_requires_commit: Option<std::rc::Rc<std::cell::Cell<bool>>>,
    events: Vec<&'static str>,
}

impl LinuxInstallBackend for LinuxBackend {
    fn install_mode(&self) -> crate::InstallMode {
        self.mode.unwrap_or(crate::InstallMode::FreshInstall)
    }

    fn classify_active_install(&mut self) -> Result<bool, InstallError> {
        self.active_install_checks = self.active_install_checks.saturating_add(1);
        Ok(self.active_install)
    }

    fn preflight_product_mutation(&mut self) -> Result<(), InstallError> {
        if self.change_service_state_after_preflight == Some(self.offline_preflight_calls) {
            self.service_state = TestServiceState::EnabledInactive;
        }
        self.offline_preflight_calls = self.offline_preflight_calls.saturating_add(1);
        if self.install_mode() != crate::InstallMode::FreshInstall
            && self.service_state != TestServiceState::Offline
        {
            return Err(InstallError::offline_services_required());
        }
        Ok(())
    }

    fn preflight_fresh_recovery_mutation(
        &mut self,
        journal: &LinuxInstallJournal,
    ) -> Result<(), InstallError> {
        self.offline_preflight_calls = self.offline_preflight_calls.saturating_add(1);
        if journal.mode() != crate::InstallMode::FreshInstall
            || !journal.fresh_services_deactivated()
            || self.service_state != TestServiceState::Offline
        {
            return Err(InstallError::offline_services_required());
        }
        Ok(())
    }

    fn bind_authenticated_nix_config(
        &mut self,
        _config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), InstallError> {
        Ok(())
    }

    fn bind_authenticated_release_identity(
        &mut self,
        _system: System,
        _digest: Digest,
    ) -> Result<(), InstallError> {
        Ok(())
    }

    fn preflight_privilege(&mut self) -> Result<(), InstallError> {
        Ok(())
    }
    fn recover_repair_assets(&mut self) -> Result<(), InstallError> {
        Ok(())
    }
    fn preflight_clean_host(&mut self, _system: System) -> Result<(), InstallError> {
        self.clean_host_checks = self.clean_host_checks.saturating_add(1);
        if self.mode == Some(crate::InstallMode::OfflineRepair) {
            if self.preflight_handoff != Some(DeterminateHandoffState::Accepted)
                || self.service_state != TestServiceState::Offline
            {
                return Err(InstallError::backend_failure());
            }
            return Ok(());
        }
        self.preflight_handoff.map_or(Ok(()), |state| {
            crate::linux_backend::validate_determinate_handoff_preflight(state).map(|_| ())
        })
    }
    fn preflight_recovery(
        &mut self,
        mode: crate::InstallMode,
        _system: System,
    ) -> Result<(), InstallError> {
        if self.install_mode() != mode {
            return Err(InstallError::recovery_mode_mismatch());
        }
        if mode != crate::InstallMode::FreshInstall
            && self.service_state != TestServiceState::Offline
        {
            return Err(InstallError::backend_failure());
        }
        Ok(())
    }
    fn classify_asset(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<crate::AssetPresence, InstallError> {
        Ok(
            if self.create || (self.replace_files && asset.kind() == crate::LinuxAssetKind::File) {
                crate::AssetPresence::Absent
            } else {
                crate::AssetPresence::ExactPresent
            },
        )
    }
    fn classify_ownership_receipt(
        &mut self,
        asset: LinuxInstallAsset,
    ) -> Result<crate::AssetPresence, InstallError> {
        if self.mode == Some(crate::InstallMode::OfflineRepair) {
            Ok(crate::AssetPresence::ExactPresent)
        } else {
            self.classify_asset(asset)
        }
    }
    fn classify_managed_runtime(&mut self) -> Result<crate::AssetPresence, InstallError> {
        Ok(if self.managed_runtime_present.unwrap_or(!self.create) {
            crate::AssetPresence::ExactPresent
        } else {
            crate::AssetPresence::Absent
        })
    }
    fn classify_services(&mut self) -> Result<crate::AssetPresence, InstallError> {
        Ok(
            if self.create || self.service_state == TestServiceState::Offline {
                crate::AssetPresence::Absent
            } else {
                crate::AssetPresence::ExactPresent
            },
        )
    }
    fn recover_fresh_services(&mut self) -> Result<(), InstallError> {
        self.events.push("quiesce-services");
        self.service_state = TestServiceState::Offline;
        Ok(())
    }
    fn services_need_mutation(&self, _prior_active: bool) -> bool {
        self.install_mode() == crate::InstallMode::FreshInstall && self.create
    }
    fn ensure_asset(&mut self, asset: LinuxInstallAsset) -> Result<bool, InstallError> {
        self.mutation_calls = self.mutation_calls.saturating_add(1);
        if asset.kind() == crate::LinuxAssetKind::File {
            self.file_mutation_calls = self.file_mutation_calls.saturating_add(1);
        }
        self.events.push("ensure-asset");
        if self.failure == LinuxBackendFailure::Asset {
            Err(InstallError::backend_failure())
        } else {
            Ok(self.create || (self.replace_files && asset.kind() == crate::LinuxAssetKind::File))
        }
    }
    fn install_systemd_unit(
        &mut self,
        asset: LinuxInstallAsset,
        _contents: &'static str,
    ) -> Result<bool, InstallError> {
        self.mutation_calls = self.mutation_calls.saturating_add(1);
        self.file_mutation_calls = self.file_mutation_calls.saturating_add(1);
        if self.failure == LinuxBackendFailure::Unit {
            Err(InstallError::backend_failure())
        } else {
            Ok(self.create || (self.replace_files && asset.kind() == crate::LinuxAssetKind::File))
        }
    }
    fn provision_managed_runtime(&mut self) -> Result<bool, InstallError> {
        self.mutation_calls = self.mutation_calls.saturating_add(1);
        self.raw_provision_calls = self.raw_provision_calls.saturating_add(1);
        Err(InstallError::backend_failure())
    }
    fn rollback_managed_runtime(&mut self) -> Result<(), InstallError> {
        self.mutation_calls = self.mutation_calls.saturating_add(1);
        self.rollback_calls = self.rollback_calls.saturating_add(1);
        Ok(())
    }
    fn validate_base_nix(&mut self) -> Result<(), InstallError> {
        self.events.push("validate-base-nix");
        if self.failure == LinuxBackendFailure::BaseNix {
            Err(InstallError::backend_failure())
        } else {
            Ok(())
        }
    }
    fn accept_base_nix_handoff(&mut self) -> Result<(), InstallError> {
        self.events.push("accept-handoff");
        Ok(())
    }
    fn activate_services(&mut self) -> Result<bool, InstallError> {
        if self.install_mode() != crate::InstallMode::FreshInstall {
            return (self.service_state == TestServiceState::Offline)
                .then_some(false)
                .ok_or_else(InstallError::backend_failure);
        }
        self.mutation_calls = self.mutation_calls.saturating_add(1);
        self.events.push("activate-services");
        let changed = self.create || self.service_state == TestServiceState::MutationNeeded;
        self.service_state = TestServiceState::Stable;
        if self.failure == LinuxBackendFailure::Activation {
            Err(InstallError::backend_failure())
        } else {
            Ok(changed)
        }
    }
    fn rollback_services(&mut self) -> Result<(), InstallError> {
        if self.install_mode() != crate::InstallMode::FreshInstall {
            return Err(InstallError::backend_failure());
        }
        self.mutation_calls = self.mutation_calls.saturating_add(1);
        self.rollback_calls = self.rollback_calls.saturating_add(1);
        self.events.push("quiesce-services");
        self.service_state = TestServiceState::Offline;
        Ok(())
    }
    fn finish_fresh_services_rollback(&mut self) -> Result<(), InstallError> {
        self.events.push("resume-services");
        Ok(())
    }
    fn check_managed_daemon(&mut self) -> Result<(), InstallError> {
        if self.install_mode() != crate::InstallMode::FreshInstall {
            return (self.service_state == TestServiceState::Offline)
                .then_some(())
                .ok_or_else(InstallError::backend_failure);
        }
        self.events.push("validate-services");
        if self.failure == LinuxBackendFailure::Health {
            Err(InstallError::backend_failure())
        } else {
            Ok(())
        }
    }
    fn publish_ownership_receipt(&mut self) -> Result<bool, InstallError> {
        self.mutation_calls = self.mutation_calls.saturating_add(1);
        self.events.push("publish-receipt");
        if self.failure == LinuxBackendFailure::Receipt {
            Err(InstallError::backend_failure())
        } else {
            Ok(self.mode != Some(crate::InstallMode::OfflineRepair)
                && (self.create || self.replace_files))
        }
    }
    fn finalize_ownership_receipt(&mut self) -> Result<(), InstallError> {
        self.finalize_calls = self.finalize_calls.saturating_add(1);
        if self
            .finalize_requires_commit
            .as_ref()
            .is_some_and(|committed| !committed.get())
            || self.failure == LinuxBackendFailure::Finalize
        {
            Err(InstallError::backend_failure())
        } else {
            Ok(())
        }
    }
    fn rollback_asset(&mut self, _asset: LinuxInstallAsset) -> Result<(), InstallError> {
        self.mutation_calls = self.mutation_calls.saturating_add(1);
        self.rollback_calls = self.rollback_calls.saturating_add(1);
        self.events.push("rollback-asset");
        Ok(())
    }
}

struct RealDeterminateInstallObservation {
    fixture: RealDeterminateFixture,
    result: Result<(), InstallError>,
    rollback_calls: usize,
    accepted_before_journal_completion: bool,
}

struct DeterminateJournalPersistence {
    handoff: DeterminateHandoff,
    accepted_before_completion: std::cell::Cell<bool>,
}

impl LinuxJournalPersistence for DeterminateJournalPersistence {
    fn replace(&self, journal: &LinuxInstallJournal) -> Result<(), InstallError> {
        if !journal.is_committed() && self.handoff.state() == Ok(DeterminateHandoffState::Accepted)
        {
            self.accepted_before_completion.set(true);
        }
        Ok(())
    }
}

fn run_real_determinate_install(
    failure: LinuxBackendFailure,
) -> Result<RealDeterminateInstallObservation, Box<dyn std::error::Error>> {
    let fixture = RealDeterminateFixture::new()?;
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut provisioner = RealDeterminateProvisioner {
        handoff: Some(fixture.handoff()?),
        receipt: fixture.receipt(),
        marker: fixture.marker("vendor-starts"),
    };
    let persistence = DeterminateJournalPersistence {
        handoff: fixture.handoff()?,
        accepted_before_completion: std::cell::Cell::new(false),
    };
    let journal = LinuxInstallJournal::new(
        crate::InstallMode::FreshInstall,
        request.system,
        Digest::from_bytes([0xb1; 32]),
        Digest::from_bytes([0xb2; 32]),
    )?;
    let mut backend = LinuxBackend {
        create: true,
        failure,
        ..LinuxBackend::default()
    };
    let result = install_linux_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut provisioner,
        &persistence,
        journal,
    )
    .map(|_| ());
    Ok(RealDeterminateInstallObservation {
        fixture,
        result,
        rollback_calls: backend.rollback_calls,
        accepted_before_journal_completion: persistence.accepted_before_completion.get(),
    })
}

#[test]
fn started_handoff_preflight_prevents_product_mutation_and_vendor_start()
-> Result<(), Box<dyn std::error::Error>> {
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let rolled_back = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut provisioner = StubProvisioner {
        calls: 0,
        rolled_back: rolled_back.clone(),
    };
    let mut backend = LinuxBackend {
        create: true,
        preflight_handoff: Some(DeterminateHandoffState::Started),
        ..LinuxBackend::default()
    };

    let result =
        install_linux_with_provisioner(request.system, &request, &mut backend, &mut provisioner)
            .map(|_| ());

    assert_eq!(
        result.map_err(InstallError::code),
        Err(crate::InstallErrorCode::UnmanagedNix)
    );
    assert_eq!(backend.mutation_calls, 0);
    assert_eq!(provisioner.calls, 0);
    assert!(!rolled_back.get());
    Ok(())
}

#[test]
fn crash_after_exit_zero_before_acceptance_preserves_started()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = RealDeterminateFixture::new()?;
    let handoff = fixture.handoff()?;
    let outcome = run_with_new_determinate_handoff(&handoff, || {
        vendor_exit_zero(
            &handoff,
            &fixture.receipt(),
            &fixture.marker("vendor-starts"),
        )
    })
    .map_err(|_| "vendor run failed")?;
    assert!(determinate_succeeded(outcome));
    assert_eq!(
        fixture.handoff()?.state()?,
        DeterminateHandoffState::Started
    );
    assert_eq!(
        fs::read_to_string(fixture.marker("vendor-starts"))?
            .lines()
            .count(),
        1
    );
    assert_restart_refuses_vendor_start(&fixture.handoff()?, &fixture.marker("post-exit-retry"));
    Ok(())
}

#[test]
fn failed_installed_state_validation_preserves_started() -> Result<(), Box<dyn std::error::Error>> {
    let observation = run_real_determinate_install(LinuxBackendFailure::BaseNix)?;
    assert!(observation.result.is_err());
    assert_eq!(
        observation.fixture.handoff()?.state()?,
        DeterminateHandoffState::Started
    );
    assert_eq!(
        fs::read_to_string(observation.fixture.marker("vendor-starts"))?
            .lines()
            .count(),
        1
    );
    assert_restart_refuses_vendor_start(
        &observation.fixture.handoff()?,
        &observation.fixture.marker("health-retry"),
    );
    assert!(observation.rollback_calls > 0);
    Ok(())
}

#[test]
fn failed_product_receipt_publication_keeps_accepted_handoff()
-> Result<(), Box<dyn std::error::Error>> {
    let observation = run_real_determinate_install(LinuxBackendFailure::Receipt)?;
    assert!(observation.result.is_err());
    assert_eq!(
        observation.fixture.handoff()?.state()?,
        DeterminateHandoffState::Accepted
    );
    assert_eq!(
        fs::read_to_string(observation.fixture.marker("vendor-starts"))?
            .lines()
            .count(),
        1
    );
    assert_restart_refuses_vendor_start(
        &observation.fixture.handoff()?,
        &observation.fixture.marker("receipt-retry"),
    );
    assert!(observation.rollback_calls > 0);
    Ok(())
}

#[test]
fn accepted_fresh_install_continues_with_the_same_journal_on_the_next_invocation()
-> Result<(), Box<dyn std::error::Error>> {
    // The public entry authenticates before calling this core. The small signed
    // fixture has no production-pinned Determinate target, so a positive load
    // remains part of the native real-release-bundle proof.
    for failure in [
        LinuxBackendFailure::Activation,
        LinuxBackendFailure::Health,
        LinuxBackendFailure::Receipt,
    ] {
        assert_accepted_fresh_continuation(failure)?;
    }
    Ok(())
}

#[test]
fn exact_active_install_returns_without_journal_or_provisioning()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let scratch_parent = temporary.path().join("scratch");
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: temporary.path(),
        installation_root: Path::new("/"),
        scratch_parent: &scratch_parent,
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let location = LinuxJournalLocation::At {
        base: temporary.path().to_path_buf(),
        user_id: nix::unistd::Uid::effective().as_raw(),
        group_id: nix::unistd::Gid::effective().as_raw(),
    };
    let release = Digest::from_bytes([0xe1; 32]);
    let context = linux_recovery_context_digest(release, &request);
    let mut backend = LinuxBackend {
        active_install: true,
        ..LinuxBackend::default()
    };
    let mut provisioner = ReauthProvisioner {
        calls: 0,
        reauthenticated: false,
        reuse_existing: true,
    };

    let report = continue_linux_bundle_install(
        &request,
        &mut backend,
        &mut provisioner,
        release,
        &location,
    )?;

    assert_eq!(report.platform().created_artifacts(), 0);
    assert_eq!(
        report.platform().existing_artifacts(),
        crate::assets::linux_product_mutation_assets().count()
    );
    assert_eq!(backend.active_install_checks, 1);
    assert_eq!(backend.clean_host_checks, 0);
    assert_eq!(backend.mutation_calls, 0);
    assert_eq!(backend.raw_provision_calls, 0);
    assert_eq!(provisioner.calls, 0);
    assert!(
        location
            .open_existing(request.system, release, context)?
            .is_none()
    );
    Ok(())
}

fn assert_accepted_fresh_continuation(
    first_failure: LinuxBackendFailure,
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = RealDeterminateFixture::new()?;
    let journal_root = tempfile::tempdir()?;
    let user_id = nix::unistd::Uid::effective().as_raw();
    let group_id = nix::unistd::Gid::effective().as_raw();
    let journal_location = LinuxJournalLocation::At {
        base: journal_root.path().to_path_buf(),
        user_id,
        group_id,
    };
    let scratch_parent = journal_root.path().join("scratch");
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: journal_root.path(),
        installation_root: Path::new("/"),
        scratch_parent: &scratch_parent,
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let release_digest = Digest::from_bytes([0xc1; 32]);
    let recovery_context_digest = linux_recovery_context_digest(release_digest, &request);
    let mut backend = LinuxBackend {
        create: true,
        failure: first_failure,
        managed_runtime_present: Some(false),
        ..LinuxBackend::default()
    };
    let mut first_provisioner = RealDeterminateProvisioner {
        handoff: Some(fixture.handoff()?),
        receipt: fixture.receipt(),
        marker: fixture.marker("vendor-starts"),
    };

    assert_eq!(
        continue_linux_bundle_install(
            &request,
            &mut backend,
            &mut first_provisioner,
            release_digest,
            &journal_location,
        )
        .map(|_| ())
        .map_err(InstallError::code),
        Err(crate::InstallErrorCode::FreshRecoveryRetained)
    );
    assert_eq!(
        fixture.handoff()?.state()?,
        DeterminateHandoffState::Accepted
    );
    assert_eq!(vendor_start_count(&fixture)?, 1);
    assert_eq!(backend.service_state, TestServiceState::Offline);
    let retained_storage = journal_location
        .open_existing(request.system, release_digest, recovery_context_digest)?
        .ok_or_else(|| std::io::Error::other("missing retained Fresh journal storage"))?;
    let retained = retained_storage
        .load()?
        .ok_or_else(|| std::io::Error::other("missing retained Fresh journal"))?;
    assert_eq!(retained.mode(), crate::InstallMode::FreshInstall);
    assert!(!retained.is_committed());
    assert!(retained.fresh_services_deactivated());
    drop(retained_storage);

    backend.failure = LinuxBackendFailure::None;
    backend.managed_runtime_present = Some(true);
    backend.preflight_handoff = Some(DeterminateHandoffState::Accepted);
    backend.active_install = true;
    let active_install_checks = backend.active_install_checks;

    let mut second_provisioner = ReauthProvisioner {
        calls: 0,
        reauthenticated: false,
        reuse_existing: true,
    };
    let _report = continue_linux_bundle_install(
        &request,
        &mut backend,
        &mut second_provisioner,
        release_digest,
        &journal_location,
    )?;

    assert_eq!(backend.active_install_checks, active_install_checks);
    assert_eq!(second_provisioner.calls, 0);
    assert_eq!(backend.raw_provision_calls, 0);
    assert_eq!(vendor_start_count(&fixture)?, 1);
    assert_eq!(backend.service_state, TestServiceState::Stable);
    assert_eq!(backend.events.last(), Some(&"publish-receipt"));
    assert!(
        journal_location
            .open_existing(request.system, release_digest, recovery_context_digest)?
            .is_none()
    );
    Ok(())
}

fn vendor_start_count(fixture: &RealDeterminateFixture) -> std::io::Result<usize> {
    fs::read_to_string(fixture.marker("vendor-starts")).map(|bytes| bytes.lines().count())
}

#[test]
fn exit_zero_plus_installed_state_validation_accepts_handoff_exactly_once()
-> Result<(), Box<dyn std::error::Error>> {
    let observation = run_real_determinate_install(LinuxBackendFailure::None)?;
    assert!(observation.result.is_ok());
    assert_eq!(
        observation.fixture.handoff()?.state()?,
        DeterminateHandoffState::Accepted
    );
    assert_eq!(
        fs::read_to_string(observation.fixture.marker("vendor-starts"))?
            .lines()
            .count(),
        1
    );
    let handoff_bytes =
        fs::read_to_string(observation.fixture.marker("determinate-handoff-v1.json"))?;
    assert_eq!(handoff_bytes.matches("\"accepted\"").count(), 1);
    assert_eq!(
        observation
            .fixture
            .handoff()?
            .accept_after_installed_state_proof(),
        Err(crate::determinate_handoff::DeterminateHandoffError::InvalidTransition)
    );
    assert_eq!(observation.rollback_calls, 0);
    assert!(observation.accepted_before_journal_completion);
    Ok(())
}

#[test]
fn journaled_linux_install_persists_each_intent_completion_and_commit()
-> Result<(), Box<dyn std::error::Error>> {
    let persistence = MemoryJournalPersistence::default();
    let journal = LinuxInstallJournal::new(
        crate::InstallMode::FreshInstall,
        System::X8664Linux,
        Digest::from_bytes([0x91; 32]),
        Digest::from_bytes([0xa1; 32]),
    )?;
    let mut backend = LinuxBackend {
        create: true,
        finalize_requires_commit: Some(persistence.committed.clone()),
        ..LinuxBackend::default()
    };
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut provisioner = StubProvisioner {
        calls: 0,
        rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
    };
    let (report, outcome) = install_linux_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut provisioner,
        &persistence,
        journal,
    )?;

    assert!(matches!(outcome, BootstrapOutcome::Stub(_)));
    drop(outcome);
    assert_eq!(
        report.created_artifacts(),
        crate::assets::linux_product_mutation_assets().count()
    );
    let snapshots = persistence.snapshots.borrow();
    assert!(snapshots.len() > crate::assets::linux_product_mutation_assets().count());
    assert!(
        snapshots
            .last()
            .is_some_and(LinuxInstallJournal::is_committed)
    );
    assert_eq!(backend.finalize_calls, 1);
    Ok(())
}

#[test]
fn post_commit_cleanup_failure_keeps_a_resumable_committed_journal()
-> Result<(), Box<dyn std::error::Error>> {
    let persistence = MemoryJournalPersistence::default();
    let journal = LinuxInstallJournal::new(
        crate::InstallMode::FreshInstall,
        System::X8664Linux,
        Digest::from_bytes([0xc1; 32]),
        Digest::from_bytes([0xc2; 32]),
    )?;
    let mut backend = LinuxBackend {
        create: true,
        failure: LinuxBackendFailure::Finalize,
        finalize_requires_commit: Some(persistence.committed.clone()),
        ..LinuxBackend::default()
    };
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut provisioner = StubProvisioner {
        calls: 0,
        rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
    };

    let error = install_linux_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut provisioner,
        &persistence,
        journal,
    )
    .err()
    .ok_or_else(|| std::io::Error::other("expected cleanup failure"))?;
    assert_eq!(error.code(), crate::InstallErrorCode::RollbackIncomplete);
    assert!(persistence.committed.get());
    assert_eq!(backend.finalize_calls, 1);
    assert_eq!(backend.rollback_calls, 0);
    let committed = persistence
        .snapshots
        .borrow()
        .last()
        .cloned()
        .ok_or_else(|| std::io::Error::other("missing committed snapshot"))?;
    assert!(committed.is_committed());

    backend.failure = LinuxBackendFailure::None;
    finalize_committed_linux_install(&committed, &mut backend)?;
    assert_eq!(backend.finalize_calls, 2);
    Ok(())
}

#[test]
fn journaled_linux_install_keeps_uncertain_intent_on_mutation_error()
-> Result<(), Box<dyn std::error::Error>> {
    let persistence = MemoryJournalPersistence::default();
    let journal = LinuxInstallJournal::new(
        crate::InstallMode::FreshInstall,
        System::X8664Linux,
        Digest::from_bytes([0x92; 32]),
        Digest::from_bytes([0xa2; 32]),
    )?;
    let mut backend = LinuxBackend {
        create: true,
        failure: LinuxBackendFailure::Asset,
        ..LinuxBackend::default()
    };
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut provisioner = StubProvisioner {
        calls: 0,
        rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
    };
    let asset = crate::linux_install_assets()
        .iter()
        .copied()
        .find(|asset| asset.kind() != crate::LinuxAssetKind::File)
        .ok_or_else(|| std::io::Error::other("missing fixed Linux asset"))?;
    let result = install_linux_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut provisioner,
        &persistence,
        journal,
    )
    .map(|_| ());

    assert_eq!(
        result.map_err(InstallError::code),
        Err(crate::InstallErrorCode::BackendFailure)
    );
    let mutation = asset_mutation(asset);
    let snapshot = persistence
        .snapshots
        .borrow()
        .last()
        .cloned()
        .ok_or_else(|| std::io::Error::other("missing retained recovery snapshot"))?;
    assert!(!snapshot.is_committed());
    assert!(snapshot.recovery_actions().is_empty());
    assert_eq!(snapshot.mutation_state(&mutation)?, None);
    Ok(())
}

#[test]
fn journaled_linux_install_preserves_provision_rollback_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let persistence = MemoryJournalPersistence::default();
    let journal = LinuxInstallJournal::new(
        crate::InstallMode::FreshInstall,
        System::X8664Linux,
        Digest::from_bytes([0x95; 32]),
        Digest::from_bytes([0xa5; 32]),
    )?;
    let mut backend = LinuxBackend {
        create: true,
        ..LinuxBackend::default()
    };
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let result = install_linux_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut RollbackFailedProvisioner,
        &persistence,
        journal,
    )
    .map(|_| ());

    assert_eq!(
        result.map_err(InstallError::code),
        Err(crate::InstallErrorCode::RollbackIncomplete)
    );
    assert_eq!(
        persistence
            .snapshots
            .borrow()
            .last()
            .ok_or_else(|| std::io::Error::other("missing runtime intent snapshot"))?
            .mutation_state(&LinuxInstallMutation::ManagedRuntime)?,
        Some(crate::LinuxInstallMutationState::Intended)
    );
    Ok(())
}

#[test]
fn journaled_linux_reinstall_records_exact_state_without_created_artifacts()
-> Result<(), Box<dyn std::error::Error>> {
    let persistence = MemoryJournalPersistence::default();
    let journal = LinuxInstallJournal::new(
        crate::InstallMode::OfflineUpgrade,
        System::X8664Linux,
        Digest::from_bytes([0x93; 32]),
        Digest::from_bytes([0xa3; 32]),
    )?;
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut backend = LinuxBackend {
        mode: Some(crate::InstallMode::OfflineUpgrade),
        service_state: TestServiceState::Offline,
        ..LinuxBackend::default()
    };
    let mut provisioner = StubProvisioner {
        calls: 0,
        rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
    };

    let (report, outcome) = install_linux_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut provisioner,
        &persistence,
        journal,
    )?;

    assert!(matches!(outcome, BootstrapOutcome::Stub(_)));
    assert_eq!(report.created_artifacts(), 0);
    assert_eq!(
        report.existing_artifacts(),
        crate::assets::linux_product_mutation_assets().count()
    );
    assert!(
        persistence
            .snapshots
            .borrow()
            .last()
            .is_some_and(LinuxInstallJournal::is_committed)
    );
    Ok(())
}

#[test]
fn journaled_existing_product_update_stays_offline_and_never_starts_determinate()
-> Result<(), Box<dyn std::error::Error>> {
    let persistence = MemoryJournalPersistence::default();
    let journal = LinuxInstallJournal::new(
        crate::InstallMode::OfflineUpgrade,
        System::X8664Linux,
        Digest::from_bytes([0xd1; 32]),
        Digest::from_bytes([0xd2; 32]),
    )?;
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut backend = LinuxBackend {
        replace_files: true,
        mode: Some(crate::InstallMode::OfflineUpgrade),
        service_state: TestServiceState::Offline,
        preflight_handoff: Some(DeterminateHandoffState::Accepted),
        ..LinuxBackend::default()
    };
    let mut provisioner = ReauthProvisioner {
        calls: 0,
        reauthenticated: false,
        reuse_existing: true,
    };

    let (_, outcome) = install_linux_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut provisioner,
        &persistence,
        journal,
    )?;

    assert!(matches!(outcome, BootstrapOutcome::Existing));
    drop(outcome);
    assert_eq!(provisioner.calls, 0);
    assert_eq!(backend.raw_provision_calls, 0);
    let receipt = backend
        .events
        .iter()
        .position(|event| *event == "publish-receipt")
        .ok_or_else(|| std::io::Error::other("missing receipt publication"))?;
    let base_nix = backend
        .events
        .iter()
        .position(|event| *event == "validate-base-nix")
        .ok_or_else(|| std::io::Error::other("missing Base Nix validation"))?;
    assert!(base_nix < receipt);
    assert!(!backend.events.iter().any(|event| matches!(
        *event,
        "activate-services" | "quiesce-services" | "resume-services"
    )));
    let committed = persistence
        .snapshots
        .borrow()
        .last()
        .cloned()
        .ok_or_else(|| std::io::Error::other("missing committed journal"))?;
    assert!(committed.is_committed());
    assert_eq!(
        committed.mutation_state(&LinuxInstallMutation::Services)?,
        Some(crate::LinuxInstallMutationState::PreExisting)
    );
    Ok(())
}

#[test]
fn offline_state_change_blocks_the_next_file_mutation_and_rollback()
-> Result<(), Box<dyn std::error::Error>> {
    let persistence = MemoryJournalPersistence::default();
    let journal = LinuxInstallJournal::new(
        crate::InstallMode::OfflineUpgrade,
        System::X8664Linux,
        Digest::from_bytes([0xd3; 32]),
        Digest::from_bytes([0xd4; 32]),
    )?;
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut backend = LinuxBackend {
        replace_files: true,
        mode: Some(crate::InstallMode::OfflineUpgrade),
        service_state: TestServiceState::Offline,
        change_service_state_after_preflight: Some(1),
        ..LinuxBackend::default()
    };
    let mut provisioner = ReauthProvisioner {
        calls: 0,
        reauthenticated: false,
        reuse_existing: true,
    };

    assert_eq!(
        install_linux_with_provisioner_journaled(
            request.system,
            &request,
            &mut backend,
            &mut provisioner,
            &persistence,
            journal,
        )
        .map(|_| ())
        .map_err(InstallError::code),
        Err(crate::InstallErrorCode::RollbackIncomplete)
    );
    assert_eq!(backend.mutation_calls, 1);
    assert_eq!(backend.file_mutation_calls, 0);
    assert!(backend.offline_preflight_calls >= 3);
    assert_eq!(backend.service_state, TestServiceState::EnabledInactive);
    Ok(())
}

#[test]
fn journaled_offline_repair_changes_product_files_without_service_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    // This is the closest injectable seam below `install_linux_from_bundle`.
    // The public entry owns real signed TUF loading and fixed root-only `/run`
    // journal storage, so the native clean-host proof covers that final boundary.
    let persistence = MemoryJournalPersistence::default();
    let journal = LinuxInstallJournal::new(
        crate::InstallMode::OfflineRepair,
        System::X8664Linux,
        Digest::from_bytes([0xd3; 32]),
        Digest::from_bytes([0xd4; 32]),
    )?;
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut backend = LinuxBackend {
        replace_files: true,
        mode: Some(crate::InstallMode::OfflineRepair),
        service_state: TestServiceState::Offline,
        preflight_handoff: Some(DeterminateHandoffState::Accepted),
        ..LinuxBackend::default()
    };
    let mut provisioner = ReauthProvisioner {
        calls: 0,
        reauthenticated: false,
        reuse_existing: true,
    };

    install_linux_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut provisioner,
        &persistence,
        journal,
    )?;

    assert_eq!(provisioner.calls, 0);
    assert_eq!(backend.raw_provision_calls, 0);
    assert!(backend.events.contains(&"ensure-asset"));
    assert!(!backend.events.iter().any(|event| matches!(
        *event,
        "activate-services" | "quiesce-services" | "resume-services" | "validate-services"
    )));
    let committed = persistence
        .snapshots
        .borrow()
        .last()
        .cloned()
        .ok_or_else(|| std::io::Error::other("missing committed repair journal"))?;
    assert!(committed.is_committed());
    assert_eq!(
        committed.mutation_state(&LinuxInstallMutation::Services)?,
        Some(crate::LinuxInstallMutationState::PreExisting)
    );
    Ok(())
}

#[test]
fn journaled_repair_refuses_non_offline_service_state_before_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    for state in [
        TestServiceState::Stable,
        TestServiceState::MutationNeeded,
        TestServiceState::EnabledInactive,
        TestServiceState::Mixed,
        TestServiceState::Unqueryable,
    ] {
        let persistence = MemoryJournalPersistence::default();
        let journal = LinuxInstallJournal::new(
            crate::InstallMode::OfflineRepair,
            System::X8664Linux,
            Digest::from_bytes([0xd5; 32]),
            Digest::from_bytes([0xd6; 32]),
        )?;
        let mut backend = LinuxBackend {
            replace_files: true,
            mode: Some(crate::InstallMode::OfflineRepair),
            service_state: state,
            preflight_handoff: Some(DeterminateHandoffState::Accepted),
            ..LinuxBackend::default()
        };
        let mut provisioner = ReauthProvisioner {
            calls: 0,
            reauthenticated: false,
            reuse_existing: true,
        };

        let result = install_linux_with_provisioner_journaled(
            request.system,
            &request,
            &mut backend,
            &mut provisioner,
            &persistence,
            journal,
        );
        assert_eq!(
            result.err().map(InstallError::code),
            Some(crate::InstallErrorCode::BackendFailure)
        );
        assert_eq!(backend.mutation_calls, 0);
        assert!(backend.events.is_empty());
        assert_eq!(provisioner.calls, 0);
        assert!(persistence.snapshots.borrow().is_empty());
    }
    Ok(())
}

#[test]
fn recovery_never_switches_between_upgrade_and_repair_modes()
-> Result<(), Box<dyn std::error::Error>> {
    for (journal_mode, requested_mode) in [
        (
            crate::InstallMode::OfflineRepair,
            crate::InstallMode::OfflineUpgrade,
        ),
        (
            crate::InstallMode::OfflineUpgrade,
            crate::InstallMode::OfflineRepair,
        ),
    ] {
        let mut journal = LinuxInstallJournal::new(
            journal_mode,
            System::X8664Linux,
            Digest::from_bytes([0xf1; 32]),
            Digest::from_bytes([0xf2; 32]),
        )?;
        let mut backend = LinuxBackend {
            mode: Some(requested_mode),
            service_state: TestServiceState::Offline,
            ..LinuxBackend::default()
        };
        assert_eq!(
            crate::installer::recover_linux_install(
                &mut journal,
                &mut backend,
                &mut || Ok(()),
                &mut |_| Ok(()),
            )
            .map_err(InstallError::code),
            Err(crate::InstallErrorCode::RecoveryModeMismatch)
        );
        assert_eq!(backend.mutation_calls, 0);
    }
    Ok(())
}

#[test]
fn failed_offline_repair_rolls_forward_files_without_service_mutation()
-> Result<(), Box<dyn std::error::Error>> {
    let persistence = MemoryJournalPersistence::default();
    let journal = LinuxInstallJournal::new(
        crate::InstallMode::OfflineRepair,
        System::X8664Linux,
        Digest::from_bytes([0xd7; 32]),
        Digest::from_bytes([0xd8; 32]),
    )?;
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut backend = LinuxBackend {
        replace_files: true,
        mode: Some(crate::InstallMode::OfflineRepair),
        service_state: TestServiceState::Offline,
        failure: LinuxBackendFailure::Unit,
        preflight_handoff: Some(DeterminateHandoffState::Accepted),
        ..LinuxBackend::default()
    };
    let mut provisioner = ReauthProvisioner {
        calls: 0,
        reauthenticated: false,
        reuse_existing: true,
    };

    let result = install_linux_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut provisioner,
        &persistence,
        journal,
    );

    assert_eq!(
        result.err().map(InstallError::code),
        Some(crate::InstallErrorCode::RollbackIncomplete)
    );
    assert!(backend.events.contains(&"rollback-asset"));
    assert!(!backend.events.iter().any(|event| matches!(
        *event,
        "activate-services" | "quiesce-services" | "resume-services" | "validate-services"
    )));
    assert_eq!(provisioner.calls, 0);
    assert!(
        persistence
            .snapshots
            .borrow()
            .last()
            .is_some_and(|journal| !journal.is_committed())
    );
    Ok(())
}

#[test]
fn failed_existing_product_update_restores_files_and_stays_offline()
-> Result<(), Box<dyn std::error::Error>> {
    let persistence = MemoryJournalPersistence::default();
    let journal = LinuxInstallJournal::new(
        crate::InstallMode::OfflineUpgrade,
        System::X8664Linux,
        Digest::from_bytes([0xe1; 32]),
        Digest::from_bytes([0xe2; 32]),
    )?;
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut backend = LinuxBackend {
        replace_files: true,
        mode: Some(crate::InstallMode::OfflineUpgrade),
        service_state: TestServiceState::Offline,
        failure: LinuxBackendFailure::Receipt,
        preflight_handoff: Some(DeterminateHandoffState::Accepted),
        ..LinuxBackend::default()
    };
    let mut provisioner = ReauthProvisioner {
        calls: 0,
        reauthenticated: false,
        reuse_existing: true,
    };

    let result = install_linux_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut provisioner,
        &persistence,
        journal,
    );

    assert_eq!(
        result.map(|_| ()).map_err(InstallError::code),
        Err(crate::InstallErrorCode::ReceiptFailure)
    );
    assert_eq!(provisioner.calls, 0);
    assert_eq!(backend.raw_provision_calls, 0);
    assert!(backend.events.contains(&"rollback-asset"));
    assert!(backend.events.contains(&"publish-receipt"));
    assert!(!backend.events.iter().any(|event| matches!(
        *event,
        "activate-services" | "quiesce-services" | "resume-services"
    )));
    Ok(())
}

#[test]
fn journaled_linux_reinstall_rolls_back_its_temporary_daemon()
-> Result<(), Box<dyn std::error::Error>> {
    let persistence = MemoryJournalPersistence::default();
    let journal = LinuxInstallJournal::new(
        crate::InstallMode::FreshInstall,
        System::X8664Linux,
        Digest::from_bytes([0x94; 32]),
        Digest::from_bytes([0xa4; 32]),
    )?;
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let rolled_back = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut provisioner = StubProvisioner {
        calls: 0,
        rolled_back: rolled_back.clone(),
    };
    let mut backend = LinuxBackend {
        failure: LinuxBackendFailure::Health,
        ..LinuxBackend::default()
    };

    let result = install_linux_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut provisioner,
        &persistence,
        journal,
    )
    .map(|_| ());

    assert_eq!(
        result.map_err(InstallError::code),
        Err(crate::InstallErrorCode::ServiceUnhealthy)
    );
    assert!(rolled_back.get());
    Ok(())
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "test double mirrors the production classification flags"
)]
#[derive(Default)]
struct MacBackend {
    raw_provision_calls: usize,
    fail_health: bool,
    fail_finalize: bool,
    fail_receipt: bool,
    create_store: bool,
    runtime_present: bool,
}

impl MacOsInstallBackend for MacBackend {
    fn bind_authenticated_installer_payloads(
        &mut self,
        _payloads: &AuthenticatedInstallerPayloads,
    ) -> Result<(), MacOsError> {
        Ok(())
    }

    fn bind_authenticated_nix_config(
        &mut self,
        _config: &AuthenticatedManagedNixConfig,
    ) -> Result<(), MacOsError> {
        Ok(())
    }

    fn bind_authenticated_release_identity(
        &mut self,
        _system: System,
        _release_identity_digest: Digest,
    ) -> Result<(), MacOsError> {
        Ok(())
    }

    fn begin_authenticated_recovery(
        &mut self,
        _mode: crate::InstallMode,
    ) -> Result<(), MacOsError> {
        Ok(())
    }

    fn preflight_privilege(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
    fn preflight_clean_host(&mut self, _system: System) -> Result<(), MacOsError> {
        Ok(())
    }
    fn broker_uid(&mut self) -> Result<u32, MacOsError> {
        Ok(333)
    }
    fn classify_asset(&mut self, _asset: MacOsInstallAsset) -> Result<AssetPresence, MacOsError> {
        Ok(AssetPresence::ExactPresent)
    }
    fn classify_managed_runtime(&mut self) -> Result<AssetPresence, MacOsError> {
        Ok(if self.runtime_present {
            AssetPresence::ExactPresent
        } else {
            AssetPresence::Absent
        })
    }
    fn classify_services(&mut self) -> Result<AssetPresence, MacOsError> {
        Ok(AssetPresence::ExactPresent)
    }
    fn classify_ownership_receipt(&mut self) -> Result<AssetPresence, MacOsError> {
        Ok(AssetPresence::Absent)
    }
    fn recover_asset(&mut self, _asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        Ok(())
    }
    fn recover_services(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
    fn recover_ownership_receipt(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
    fn verify_release_bundle(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
    fn ensure_asset(&mut self, asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        Ok(self.create_store && asset.id() == "nix-root")
    }
    fn install_launchd_plist(
        &mut self,
        _asset: MacOsInstallAsset,
        _contents: &'static str,
    ) -> Result<bool, MacOsError> {
        Ok(false)
    }
    fn install_nix_config(&mut self, _asset: MacOsInstallAsset) -> Result<bool, MacOsError> {
        Ok(false)
    }
    fn provision_managed_runtime(&mut self) -> Result<bool, MacOsError> {
        self.raw_provision_calls = self.raw_provision_calls.saturating_add(1);
        Err(MacOsError::backend_failure())
    }
    fn rollback_managed_runtime(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
    fn accept_base_nix_handoff(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
    fn verify_installed_code(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
    fn activate_services(&mut self) -> Result<bool, MacOsError> {
        Ok(false)
    }
    fn rollback_services(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
    fn check_managed_daemon(&mut self) -> Result<(), MacOsError> {
        if self.fail_health {
            Err(MacOsError::backend_failure())
        } else {
            Ok(())
        }
    }
    fn observe_build_readiness(
        &mut self,
        system: System,
    ) -> Result<MacOsBuildReadiness, MacOsError> {
        Ok(MacOsBuildReadiness::observed(
            system,
            crate::MacOsSandboxReadiness::Enforced,
            crate::MacOsBuildUsersReadiness::Ready,
            crate::MacOsToolchainReadiness::Ready,
        ))
    }
    fn publish_ownership_receipt(&mut self) -> Result<bool, MacOsError> {
        if self.fail_receipt {
            Err(MacOsError::backend_failure())
        } else {
            Ok(true)
        }
    }
    fn rollback_asset(&mut self, _asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        Ok(())
    }
    fn finalize_replaced_asset(&mut self, _asset: MacOsInstallAsset) -> Result<(), MacOsError> {
        if self.fail_finalize {
            Err(MacOsError::backend_failure())
        } else {
            Ok(())
        }
    }

    fn classify_store_volume(&mut self) -> Result<AssetPresence, MacOsError> {
        Ok(if self.create_store {
            AssetPresence::Absent
        } else {
            AssetPresence::ExactPresent
        })
    }
    fn provision_store_volume(&mut self) -> Result<bool, MacOsError> {
        Ok(self.create_store)
    }
    fn rollback_store_volume(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
    fn recover_store_volume(&mut self) -> Result<(), MacOsError> {
        Ok(())
    }
}

#[test]
fn linux_adapter_routes_runtime_only_through_authenticated_provisioner()
-> Result<(), Box<dyn std::error::Error>> {
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut backend = LinuxBackend::default();
    let mut provisioner = ReauthProvisioner {
        calls: 0,
        reauthenticated: false,
        reuse_existing: false,
    };
    let (report, outcome) =
        install_linux_with_provisioner(request.system, &request, &mut backend, &mut provisioner)?;
    assert!(matches!(&outcome, BootstrapOutcome::Stub(_)));
    assert_eq!(report.created_artifacts(), 0);
    drop(outcome);
    assert_eq!(provisioner.calls, 1);
    assert!(provisioner.reauthenticated);
    assert_eq!(backend.raw_provision_calls, 0);
    Ok(())
}

#[test]
fn exact_linux_runtime_does_not_reacquire_the_broker_channel_lease()
-> Result<(), Box<dyn std::error::Error>> {
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut backend = LinuxBackend::default();
    let mut provisioner = ReauthProvisioner {
        calls: 0,
        reauthenticated: false,
        reuse_existing: true,
    };

    let (report, outcome) =
        install_linux_with_provisioner(request.system, &request, &mut backend, &mut provisioner)?;

    assert!(matches!(&outcome, BootstrapOutcome::Existing));
    drop(outcome);
    assert_eq!(report.created_artifacts(), 0);
    assert_eq!(provisioner.calls, 0);
    assert!(!provisioner.reauthenticated);
    Ok(())
}

#[test]
fn linux_adapter_rolls_back_through_the_authenticated_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::X8664Linux,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let rolled_back = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut provisioner = StubProvisioner {
        calls: 0,
        rolled_back: rolled_back.clone(),
    };
    let mut backend = LinuxBackend {
        failure: LinuxBackendFailure::Health,
        ..LinuxBackend::default()
    };

    let result =
        install_linux_with_provisioner(request.system, &request, &mut backend, &mut provisioner)
            .map(|_| ());

    assert_eq!(
        result.map_err(InstallError::code),
        Err(crate::InstallErrorCode::ServiceUnhealthy)
    );
    assert!(rolled_back.get());
    assert_eq!(backend.raw_provision_calls, 0);
    Ok(())
}

#[test]
fn macos_adapter_rolls_back_through_the_authenticated_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::Aarch64Darwin,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let rolled_back = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut provisioner = StubProvisioner {
        calls: 0,
        rolled_back: rolled_back.clone(),
    };
    let mut backend = MacBackend {
        fail_health: true,
        ..MacBackend::default()
    };

    let result =
        install_macos_with_provisioner(request.system, &request, &mut backend, &mut provisioner)
            .map(|_| ());

    assert_eq!(
        result.map_err(MacOsError::code),
        Err(crate::MacOsErrorCode::ServiceUnhealthy)
    );
    assert!(rolled_back.get());
    assert_eq!(backend.raw_provision_calls, 0);
    Ok(())
}

#[test]
fn journaled_macos_install_persists_receipt_last_commit() -> Result<(), Box<dyn std::error::Error>>
{
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::Aarch64Darwin,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let persistence = MacMemoryJournalPersistence::default();
    let journal = MacOsInstallJournal::new(
        request.system,
        Digest::from_bytes([0x95; 32]),
        Digest::from_bytes([0xa5; 32]),
    )?;
    let mut provisioner = StubProvisioner {
        calls: 0,
        rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
    };
    let mut backend = MacBackend {
        create_store: false,
        ..MacBackend::default()
    };

    let (report, outcome) = install_macos_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut provisioner,
        &persistence,
        journal,
    )?;

    assert!(matches!(outcome, BootstrapOutcome::Stub(_)));
    assert_eq!(report.created_artifacts(), 0);
    let snapshots = persistence.snapshots.borrow();
    let committed = snapshots
        .last()
        .ok_or_else(|| std::io::Error::other("missing committed snapshot"))?;
    assert!(committed.is_committed());
    assert_eq!(
        committed.mutation_state(&MacOsInstallMutation::OwnershipReceipt)?,
        Some(crate::MacOsInstallMutationState::Created)
    );
    assert_eq!(
        committed.mutation_state(&MacOsInstallMutation::Asset {
            id: "nix-root".to_owned(),
        })?,
        Some(crate::MacOsInstallMutationState::PreExisting)
    );
    Ok(())
}

#[test]
fn committed_macos_cleanup_failure_retains_a_resumable_journal()
-> Result<(), Box<dyn std::error::Error>> {
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: Path::new("/state"),
        installation_root: Path::new("/"),
        scratch_parent: Path::new("/scratch"),
        system: System::Aarch64Darwin,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let persistence = MacMemoryJournalPersistence::default();
    let journal = MacOsInstallJournal::new(
        request.system,
        Digest::from_bytes([0x96; 32]),
        Digest::from_bytes([0xa6; 32]),
    )?;
    let mut provisioner = StubProvisioner {
        calls: 0,
        rolled_back: std::rc::Rc::new(std::cell::Cell::new(false)),
    };
    let mut backend = MacBackend {
        fail_finalize: true,
        ..MacBackend::default()
    };

    let result = install_macos_with_provisioner_journaled(
        request.system,
        &request,
        &mut backend,
        &mut provisioner,
        &persistence,
        journal,
    );

    assert_eq!(
        result.map(|_| ()).map_err(MacOsError::code),
        Err(crate::MacOsErrorCode::RollbackIncomplete)
    );
    assert!(
        persistence
            .snapshots
            .borrow()
            .last()
            .is_some_and(MacOsInstallJournal::is_committed)
    );
    Ok(())
}

#[test]
fn accepted_macos_fresh_install_continues_without_a_second_vendor_start()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = RealDeterminateFixture::new()?;
    let scratch = fixture.temporary.path().join("scratch");
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: fixture.temporary.path(),
        installation_root: Path::new("/"),
        scratch_parent: &scratch,
        system: System::Aarch64Darwin,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let persistence = MacMemoryJournalPersistence::default();
    let journal = MacOsInstallJournal::new(
        request.system,
        Digest::from_bytes([0x97; 32]),
        Digest::from_bytes([0xa7; 32]),
    )?;
    let marker = fixture.marker("macos-vendor-starts");
    let mut first_provisioner = RealDeterminateProvisioner {
        handoff: Some(fixture.handoff()?),
        receipt: fixture.receipt(),
        marker: marker.clone(),
    };
    let mut first_backend = MacBackend {
        fail_receipt: true,
        ..MacBackend::default()
    };

    let first = install_macos_with_provisioner_journaled(
        request.system,
        &request,
        &mut first_backend,
        &mut first_provisioner,
        &persistence,
        journal,
    );
    assert_eq!(
        first.map(|_| ()).map_err(MacOsError::code),
        Err(crate::MacOsErrorCode::RollbackIncomplete)
    );
    assert_eq!(
        fixture.handoff()?.state()?,
        DeterminateHandoffState::Accepted
    );
    let retained = persistence
        .snapshots
        .borrow()
        .last()
        .cloned()
        .ok_or_else(|| std::io::Error::other("missing retained macOS journal"))?;
    assert!(!retained.is_committed());

    let mut second_backend = MacBackend {
        runtime_present: true,
        ..MacBackend::default()
    };
    let mut second_provisioner = ReauthProvisioner {
        calls: 0,
        reauthenticated: false,
        reuse_existing: true,
    };
    let (_, outcome) = install_macos_with_provisioner_journaled(
        request.system,
        &request,
        &mut second_backend,
        &mut second_provisioner,
        &persistence,
        retained,
    )?;

    assert!(matches!(outcome, BootstrapOutcome::Existing));
    drop(outcome);
    assert_eq!(second_provisioner.calls, 0);
    assert_eq!(fs::read_to_string(marker)?.lines().count(), 1);
    assert!(
        persistence
            .snapshots
            .borrow()
            .last()
            .is_some_and(MacOsInstallJournal::is_committed)
    );
    Ok(())
}

#[test]
fn macos_recovery_returns_uncommitted_fresh_storage_instead_of_deleting_it()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
    let ownership = Digest::from_bytes([0x98; 32]);
    let context = Digest::from_bytes([0xa8; 32]);
    let uid = nix::unistd::Uid::current().as_raw();
    let gid = nix::unistd::Gid::current().as_raw();
    let storage = MacOsInstallJournalStorage::prepare_for_test(
        temporary.path(),
        uid,
        gid,
        System::Aarch64Darwin,
        ownership,
        context,
    )?;
    let journal = MacOsInstallJournal::new(System::Aarch64Darwin, ownership, context)?;
    storage.create(&journal)?;
    let scratch = temporary.path().join("scratch");
    let request = InstallerProvisionRequest {
        repository: pkg_nix::InstallerRepository::Bundle(Path::new("/bundle")),
        datastore: temporary.path(),
        installation_root: Path::new("/"),
        scratch_parent: &scratch,
        system: System::Aarch64Darwin,
        groups: ManagedGroupBindings::new(100, 101)?,
    };
    let mut backend = MacBackend::default();

    let (storage, recovered) =
        recover_macos_bundle_install_from_storage(storage, &request, &mut backend)?
            .ok_or_else(|| std::io::Error::other("fresh journal was not retained"))?;

    assert_eq!(recovered, journal);
    assert_eq!(storage.load()?, Some(journal));
    storage.remove()?;
    Ok(())
}
