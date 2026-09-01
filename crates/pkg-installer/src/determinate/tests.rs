//! Tests for the `determinate` module.

use super::*;
use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

fn write_script(root: &Path, body: &str) -> Result<std::path::PathBuf, io::Error> {
    let directory = root.join("bin");
    fs::create_dir_all(&directory)?;
    let path = directory.join("nix-installer");
    if path.exists() {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    fs::write(&path, format!("#!/bin/sh\n{body}\n"))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o500))?;
    Ok(path)
}

fn identity(path: &Path) -> Result<DeterminateInstaller, io::Error> {
    let bytes = fs::read(path)?;
    Ok(DeterminateInstaller::new(
        u64::try_from(bytes.len()).map_err(io::Error::other)?,
        Digest::from_bytes(Sha256::digest(bytes).into()),
    ))
}

fn settings(root: &Path) -> Result<ProcessSettings<'_>, io::Error> {
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    Ok(ProcessSettings {
        home: OsStr::new("/fixed-root-home"),
        path: OsStr::new("/usr/bin:/bin"),
        tmpdir: root,
        trust_root: root,
        owner: nix::unistd::Uid::effective().as_raw(),
    })
}

#[test]
fn terminal_uninstall_uses_exact_fixed_argv_and_environment()
-> Result<(), Box<dyn std::error::Error>> {
    let command = terminal_uninstall_command(
        Path::new(INSTALLED_INSTALLER),
        OsStr::new(HOME),
        OsStr::new(PATH),
    );
    assert_eq!(command.get_program(), OsStr::new(INSTALLED_INSTALLER));
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            OsStr::new("uninstall"),
            OsStr::new("--no-confirm"),
            OsStr::new(RECEIPT),
        ]
    );
    assert_eq!(
        command.get_envs().collect::<Vec<_>>(),
        [
            (
                OsStr::new("DETSYS_IDS_TELEMETRY"),
                Some(OsStr::new("disabled"))
            ),
            (OsStr::new("HOME"), Some(OsStr::new(HOME))),
            (OsStr::new("PATH"), Some(OsStr::new(PATH))),
        ]
    );
    assert!(
        command
            .get_envs()
            .all(|(name, _)| name != OsStr::new("TMPDIR"))
    );

    let temporary = tempfile::tempdir()?;
    let executable = write_script(
        temporary.path(),
        r#"
printf 'HOME=%s\nPATH=%s\nTMPDIR=%s\nTELEMETRY=%s\nAMBIENT=%s\n' "$HOME" "$PATH" "${TMPDIR-unset}" "$DETSYS_IDS_TELEMETRY" "${CARGO_MANIFEST_DIR-unset}"
for argument in "$@"; do printf 'ARG=<%s>\n' "$argument"; done
"#,
    )?;
    let output = terminal_uninstall_command(
        &executable,
        OsStr::new("/fixed-root-home"),
        OsStr::new("/fixed-path"),
    )
    .output()?;
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)?,
        "HOME=/fixed-root-home\nPATH=/fixed-path\nTMPDIR=unset\nTELEMETRY=disabled\nAMBIENT=unset\nARG=<uninstall>\nARG=<--no-confirm>\nARG=</nix/receipt.json>\n"
    );
    Ok(())
}

#[test]
fn operations_use_exact_argv_and_cleared_environment() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let executable = write_script(
        temporary.path(),
        r#"
printf 'HOME=%s\nPATH=%s\nTMPDIR=%s\nTELEMETRY=%s\nAMBIENT=%s\n' "$HOME" "$PATH" "$TMPDIR" "$DETSYS_IDS_TELEMETRY" "${PKG_C06_AMBIENT_SECRET-unset}"
for argument in "$@"; do printf 'ARG=<%s>\n' "$argument"; done
"#,
    )?;
    let identity = identity(&executable)?;
    let settings = settings(temporary.path())?;
    for (operation, expected) in [
        (
            Operation::Install,
            "ARG=<--diagnostic-endpoint>\nARG=<http://127.0.0.1:18080>\nARG=<install>\nARG=<--determinate>\nARG=<--no-confirm>\nARG=<--no-modify-profile>\n",
        ),
        (
            Operation::Uninstall,
            "ARG=<--diagnostic-endpoint>\nARG=<http://127.0.0.1:18080>\nARG=<uninstall>\nARG=<--no-confirm>\nARG=</nix/receipt.json>\n",
        ),
    ] {
        let result = run(&executable, &identity, operation, &settings)?;
        let output = String::from_utf8(result.stdout)?;
        assert!(output.starts_with(&format!(
                "HOME=/fixed-root-home\nPATH=/usr/bin:/bin\nTMPDIR={}\nTELEMETRY=disabled\nAMBIENT=unset\n",
                temporary.path().display()
            )));
        assert!(output.ends_with(expected));
        assert!(result.stderr.is_empty());
        assert_eq!(result.public.terminal, DeterminateTerminal::Exited(0));
    }
    Ok(())
}

#[test]
fn spawn_failure_is_reported_without_terminal_outcome() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let executable = write_script(
        temporary.path(),
        "printf ran > \"$TMPDIR/unexpected-start\"",
    )?;

    let result = run_with_process(
        &executable,
        &identity(&executable)?,
        Operation::Install,
        &settings(temporary.path())?,
        |_| Err(io::Error::other("simulated spawn failure")),
        std::process::Child::wait,
    );

    assert!(matches!(result, Err(DeterminateProcessError::SpawnFailed)));
    assert!(!temporary.path().join("unexpected-start").exists());
    Ok(())
}

#[test]
fn wait_failure_is_reported_after_one_vendor_start() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let executable = write_script(
        temporary.path(),
        "printf '%s' $$ > \"$TMPDIR/vendor.pid\"; sleep 0.05; exit 0",
    )?;
    let result = run_with_process(
        &executable,
        &identity(&executable)?,
        Operation::Install,
        &settings(temporary.path())?,
        Command::spawn,
        |_| Err(io::Error::other("simulated wait failure")),
    );

    assert!(matches!(result, Err(DeterminateProcessError::WaitFailed)));
    let pid = fs::read_to_string(temporary.path().join("vendor.pid"))?.parse::<i32>()?;
    assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));
    Ok(())
}

#[test]
fn executable_authentication_rejects_every_invalid_shape() -> Result<(), Box<dyn std::error::Error>>
{
    let temporary = tempfile::tempdir()?;
    let executable = write_script(temporary.path(), "exit 0")?;
    let valid = identity(&executable)?;
    let owner = nix::unistd::Uid::effective().as_raw();
    let root = temporary.path();

    assert!(authenticate_executable(Path::new("relative"), &valid, owner, root).is_err());
    assert!(authenticate_executable(&root.join("missing"), &valid, owner, root).is_err());
    let directory = root.join("not-a-file");
    fs::create_dir(&directory)?;
    assert!(authenticate_executable(&directory, &valid, owner, root).is_err());
    assert!(authenticate_executable(&executable, &valid, owner.wrapping_add(1), root).is_err());
    assert!(
        authenticate_executable(
            &executable,
            &DeterminateInstaller::new(valid.length + 1, valid.sha256),
            owner,
            root,
        )
        .is_err()
    );
    assert!(
        authenticate_executable(
            &executable,
            &DeterminateInstaller::new(valid.length, Digest::from_bytes([0; 32])),
            owner,
            root,
        )
        .is_err()
    );

    let link = root.join("linked-installer");
    symlink(&executable, &link)?;
    assert!(authenticate_executable(&link, &valid, owner, root).is_err());

    let ancestor_link = root.join("linked-dir");
    symlink(root.join("bin"), &ancestor_link)?;
    assert!(
        authenticate_executable(&ancestor_link.join("nix-installer"), &valid, owner, root).is_err()
    );

    fs::set_permissions(&executable, fs::Permissions::from_mode(0o600))?;
    assert!(authenticate_executable(&executable, &valid, owner, root).is_err());
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o522))?;
    assert!(authenticate_executable(&executable, &valid, owner, root).is_err());
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o500))?;

    let parent = executable.parent().ok_or("missing parent")?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o722))?;
    assert!(authenticate_executable(&executable, &valid, owner, root).is_err());
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;

    let unsafe_ancestor = root.join("unsafe-ancestor");
    let nested = write_script(&unsafe_ancestor, "exit 0")?;
    let nested_identity = identity(&nested)?;
    fs::set_permissions(&unsafe_ancestor, fs::Permissions::from_mode(0o722))?;
    assert!(authenticate_executable(&nested, &nested_identity, owner, root).is_err());

    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    fs::write(
        &executable,
        fs::read(&executable)?
            .into_iter()
            .map(|byte| byte ^ 1)
            .collect::<Vec<_>>(),
    )?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o500))?;
    assert!(authenticate_executable(&executable, &valid, owner, root).is_err());
    Ok(())
}

#[test]
fn trusted_non_writable_chain_is_the_spawn_immutability_boundary()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let executable = write_script(temporary.path(), "exit 0")?;
    let owner = nix::unistd::Uid::effective().as_raw();
    authenticate_executable(
        &executable,
        &identity(&executable)?,
        owner,
        temporary.path(),
    )?;
    let mut current = executable.parent().ok_or("missing parent")?;
    loop {
        let metadata = fs::symlink_metadata(current)?;
        assert_eq!(metadata.uid(), owner);
        assert_eq!(metadata.mode() & 0o022, 0);
        if current == temporary.path() {
            break;
        }
        current = current.parent().ok_or("missing trusted root")?;
    }
    Ok(())
}

#[test]
fn both_large_streams_are_drained_and_capped() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let executable = write_script(
        temporary.path(),
        "i=0; while [ $i -lt 20000 ]; do printf '1234567890123456'; printf 'abcdefghijklmnop' >&2; i=$((i + 1)); done; exit 23",
    )?;
    let result = run(
        &executable,
        &identity(&executable)?,
        Operation::Install,
        &settings(temporary.path())?,
    )?;
    assert_eq!(result.stdout.len(), OUTPUT_LIMIT);
    assert_eq!(result.stderr.len(), OUTPUT_LIMIT);
    assert!(result.public.stdout_truncated);
    assert!(result.public.stderr_truncated);
    assert_eq!(result.public.terminal, DeterminateTerminal::Exited(23));
    let mut diagnostics = Vec::new();
    result.write_failure_diagnostics(&mut diagnostics);
    let metadata = format!("determinate installer outcome: {}\n", result.public);
    assert_eq!(diagnostics, metadata.as_bytes());
    assert!(
        !diagnostics
            .windows(16)
            .any(|window| window == b"1234567890123456")
    );
    assert!(
        !diagnostics
            .windows(16)
            .any(|window| window == b"abcdefghijklmnop")
    );
    Ok(())
}

#[test]
fn exit_nonzero_and_signal_are_distinct() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let executable = write_script(
        temporary.path(),
        r"printf 'ignored-stdout'; printf '\033[31mvendor-error\033[0m' >&2; exit 23",
    )?;
    let nonzero = run(
        &executable,
        &identity(&executable)?,
        Operation::Install,
        &settings(temporary.path())?,
    )?;
    assert_eq!(nonzero.public.terminal, DeterminateTerminal::Exited(23));
    assert_eq!(nonzero.stdout, b"ignored-stdout");
    assert!(
        nonzero
            .stderr
            .windows(12)
            .any(|value| value == b"vendor-error")
    );
    assert!(nonzero.stderr.contains(&0x1b));
    let mut diagnostics = Vec::new();
    nonzero.write_failure_diagnostics(&mut diagnostics);
    assert_eq!(
            diagnostics,
            b"determinate installer outcome: terminal=Exited(23), stdout_truncated=false, stderr_truncated=false\n"
        );
    assert!(
        !diagnostics
            .windows(14)
            .any(|value| value == b"ignored-stdout")
    );
    assert!(
        !diagnostics
            .windows(12)
            .any(|value| value == b"vendor-error")
    );
    assert!(!diagnostics.contains(&0x1b));

    let executable = write_script(
        temporary.path(),
        "printf 'signal-error\n' >&2; kill -TERM $$",
    )?;
    let signaled = run(
        &executable,
        &identity(&executable)?,
        Operation::Install,
        &settings(temporary.path())?,
    )?;
    assert_eq!(signaled.public.terminal, DeterminateTerminal::Signaled(15));
    assert!(
        signaled
            .stderr
            .windows(12)
            .any(|value| value == b"signal-error")
    );
    diagnostics.clear();
    signaled.write_failure_diagnostics(&mut diagnostics);
    assert_eq!(
            diagnostics,
            b"determinate installer outcome: terminal=Signaled(15), stdout_truncated=false, stderr_truncated=false\n"
        );
    assert!(
        !diagnostics
            .windows(12)
            .any(|value| value == b"signal-error")
    );
    Ok(())
}

#[test]
fn process_errors_emit_only_fixed_classifications() {
    let cases = [
        (
            DeterminateProcessError::InvalidExecutable,
            "determinate installer error: invalid vendor executable\n",
        ),
        (
            DeterminateProcessError::InvalidEnvironment,
            "determinate installer error: invalid process environment\n",
        ),
        (
            DeterminateProcessError::SpawnFailed,
            "determinate installer error: vendor process spawn failed\n",
        ),
        (
            DeterminateProcessError::WaitFailed,
            "determinate installer error: vendor process wait failed\n",
        ),
        (
            DeterminateProcessError::OutputFailed,
            "determinate installer error: vendor process output failed\n",
        ),
    ];

    for (error, expected) in cases {
        let mut diagnostics = Vec::new();
        write_process_error_diagnostic(error, &mut diagnostics);
        assert_eq!(diagnostics, expected.as_bytes());
        assert!(
            !diagnostics
                .windows(14)
                .any(|value| value == b"private-marker")
        );
        assert!(!diagnostics.contains(&0x1b));
    }
}

#[test]
fn late_success_is_not_reclassified_as_failure() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let executable = write_script(temporary.path(), "sleep 0.1; exit 0")?;
    let started = std::time::Instant::now();
    let result = run(
        &executable,
        &identity(&executable)?,
        Operation::Install,
        &settings(temporary.path())?,
    )?;
    assert!(started.elapsed() >= std::time::Duration::from_millis(90));
    assert_eq!(result.public.terminal, DeterminateTerminal::Exited(0));
    let mut diagnostics = Vec::new();
    result.write_failure_diagnostics(&mut diagnostics);
    assert!(diagnostics.is_empty());
    Ok(())
}

#[test]
fn synchronous_supervisor_reaps_child_before_return() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let executable = write_script(temporary.path(), "printf '%s' $$; exit 0")?;
    let result = run(
        &executable,
        &identity(&executable)?,
        Operation::Install,
        &settings(temporary.path())?,
    )?;
    let pid = std::str::from_utf8(&result.stdout)?.parse::<i32>()?;
    assert_eq!(kill(Pid::from_raw(pid), None), Err(Errno::ESRCH));
    Ok(())
}

#[test]
fn private_tmpdir_rejects_group_or_other_write() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let tmpdir = temporary.path().join("tmp");
    fs::create_dir(&tmpdir)?;
    let owner = nix::unistd::Uid::effective().as_raw();
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))?;
    fs::set_permissions(&tmpdir, fs::Permissions::from_mode(0o700))?;
    assert!(validate_private_tmpdir(&tmpdir, owner, temporary.path()).is_ok());
    fs::set_permissions(&tmpdir, fs::Permissions::from_mode(0o755))?;
    assert!(validate_private_tmpdir(&tmpdir, owner, temporary.path()).is_ok());
    fs::set_permissions(&tmpdir, fs::Permissions::from_mode(0o750))?;
    assert!(validate_private_tmpdir(&tmpdir, owner, temporary.path()).is_ok());
    fs::set_permissions(&tmpdir, fs::Permissions::from_mode(0o770))?;
    assert!(validate_private_tmpdir(&tmpdir, owner, temporary.path()).is_err());
    fs::set_permissions(&tmpdir, fs::Permissions::from_mode(0o702))?;
    assert!(validate_private_tmpdir(&tmpdir, owner, temporary.path()).is_err());
    fs::set_permissions(&tmpdir, fs::Permissions::from_mode(0o777))?;
    assert!(validate_private_tmpdir(&tmpdir, owner, temporary.path()).is_err());
    Ok(())
}

#[test]
fn diagnostics_never_expose_captured_bytes_or_paths() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let executable = write_script(
        temporary.path(),
        "printf 'fake-secret'; printf 'fake-secret' >&2; exit 4",
    )?;
    let result = run(
        &executable,
        &identity(&executable)?,
        Operation::Install,
        &settings(temporary.path())?,
    )?;
    for rendered in [format!("{:?}", result.public), result.public.to_string()] {
        assert!(!rendered.contains("fake-secret"));
        assert!(!rendered.contains(&executable.display().to_string()));
        assert!(!rendered.contains("--diagnostic-endpoint"));
    }
    let secret_path = Path::new("/fake-secret/missing");
    let Err(error) =
        authenticate_executable(secret_path, &identity(&executable)?, 0, Path::new("/"))
    else {
        return Err("missing executable did not fail".into());
    };
    for rendered in [format!("{error:?}"), error.to_string()] {
        assert!(!rendered.contains("fake-secret"));
    }
    Ok(())
}

#[test]
fn operation_surface_has_no_update_route() {
    let operations = [Operation::Install, Operation::Uninstall];
    assert_eq!(operations.len(), 2);
    assert!(
        operations
            .iter()
            .all(|operation| !operation.arguments().contains(&"update"))
    );
    assert!(
        operations
            .iter()
            .all(|operation| !operation.arguments().contains(&"upgrade"))
    );
}
