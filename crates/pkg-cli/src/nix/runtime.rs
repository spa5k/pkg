//! External Nix executable discovery and version probing.
use super::{Nix, NixError};
use std::path::{Path, PathBuf};

/// The stable system-wide Nix profile executable used by the Determinate
/// Nix installer and the multi-user Nix installer. Checked first.
const SYSTEM_PROFILE_NIX: &str = "/nix/var/nix/profiles/default/bin/nix";

/// The modern per-user Nix profile used by current Nix releases
/// (the same location the shipped `nix-daemon.sh` profile script uses).
const STATE_PROFILE_NIX: &str = "nix/profile/bin/nix";
const STATE_BASE: &str = ".local/state";

/// The legacy per-user profile of older single-user Nix installations.
const LEGACY_PROFILE_NIX: &str = ".nix-profile/bin/nix";

/// Whether `path` is a regular file with an executable bit.
///
/// `metadata` follows symlinks, so a link to an executable counts and a
/// dangling or directory candidate does not.
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

/// An environment value usable as an absolute search base. Empty and
/// relative values never become a search base: a relative base would
/// silently depend on the current directory.
fn absolute_base(value: Option<&std::ffi::OsStr>) -> Option<&Path> {
    let base = Path::new(value?);
    base.is_absolute().then_some(base)
}

/// The stable Nix installation profile candidates for one environment,
/// in check order: the system profile, the modern per-user state profile
/// (absolute `XDG_STATE_HOME` first, else `$HOME/.local/state`), then the
/// legacy `$HOME/.nix-profile`. Only these fixed locations are checked;
/// the disk is never scanned.
fn standard_candidates(
    state: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Vec<PathBuf> {
    let mut standard = vec![PathBuf::from(SYSTEM_PROFILE_NIX)];
    let state_base = absolute_base(state)
        .map(Path::to_path_buf)
        .or_else(|| absolute_base(home).map(|home| home.join(STATE_BASE)));
    if let Some(state_base) = state_base {
        standard.push(state_base.join(STATE_PROFILE_NIX));
    }
    if let Some(home) = absolute_base(home) {
        standard.push(home.join(LEGACY_PROFILE_NIX));
    }
    standard
}

/// Resolve the runtime executable from pure search inputs (design D1, D2).
///
/// A configured value is the only candidate and never falls back. Default
/// discovery takes the first executable-file candidate from the `PATH`
/// entries, then the stable profile paths. The inputs are parameters so the
/// selection rules are testable without touching the process environment.
fn resolve(
    configured: Option<&str>,
    path: Option<&std::ffi::OsStr>,
    standard: &[PathBuf],
) -> Result<PathBuf, NixError> {
    let path_entries: Vec<PathBuf> = path
        .map(std::env::split_paths)
        .map(Iterator::collect)
        .unwrap_or_default();
    if let Some(requested) = configured {
        let candidate = if Path::new(requested).components().count() > 1 {
            Some(PathBuf::from(requested))
        } else {
            path_entries
                .iter()
                .map(|dir| dir.join(requested))
                .find(|candidate| is_executable_file(candidate))
        };
        return candidate.filter(|c| is_executable_file(c)).ok_or_else(|| {
            NixError::ConfiguredUnavailable {
                requested: requested.to_string(),
                detail: if Path::new(requested).components().count() > 1 {
                    String::from("the path holds no executable regular file")
                } else {
                    String::from("no executable regular file with this name is on PATH")
                },
            }
        });
    }
    let from_path = path_entries
        .iter()
        .map(|dir| dir.join("nix"))
        .find(|candidate| is_executable_file(candidate));
    let from_standard = standard
        .iter()
        .find(|candidate| is_executable_file(candidate))
        .cloned();
    from_path
        .or(from_standard)
        .ok_or_else(|| NixError::NotFound {
            path_entries: path_entries.len(),
            standard: standard
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
        })
}

/// Discover the external Nix runtime.
///
/// Default discovery searches the executable `PATH` entries, then the
/// stable Nix installation profile paths (system profile, modern per-user
/// state profile, legacy per-user profile). A configured value stays the
/// only candidate and never falls back. Nothing is installed or repaired
/// here, and no invented version floor is enforced: capability is proven
/// by the decoded shapes.
/// # Errors
///
/// Returns an error if no usable runtime is found, a configured runtime is unusable, the current directory cannot be read for a relative path, or the version probe cannot start, complete, or be reaped.
pub fn discover(configured: Option<&str>) -> Result<Nix, NixError> {
    let path = std::env::var_os("PATH");
    let state = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty());
    let home = std::env::var_os("HOME").filter(|v| !v.is_empty());
    let standard = standard_candidates(state.as_deref(), home.as_deref());
    let resolved = resolve(configured, path.as_deref(), &standard)?;
    // The runtime keeps an absolute executable path from here on: a
    // candidate found through a relative PATH entry is pinned against the
    // current directory before any symlink is resolved away.
    let absolute = if resolved.is_absolute() {
        resolved
    } else {
        let cwd = std::env::current_dir().map_err(NixError::Spawn)?;
        cwd.join(resolved)
    };
    let executable = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    let mut runtime = Nix {
        executable,
        version: String::new(),
        verbose: false,
        no_color: false,
    };
    runtime.version = runtime
        .capture(&["--version"])
        .map(|text| text.trim().to_string())
        .map_err(|error| match (configured, error) {
            (_, error @ NixError::Interrupted { .. }) | (None, error) => error,
            // A configured runtime that cannot even answer `--version` is
            // unusable; report it against the configured value, not as an
            // anonymous native failure.
            (Some(requested), source) => NixError::ConfiguredProbe {
                requested: requested.to_string(),
                source: Box::new(source),
            },
        })?;
    Ok(runtime)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::nix::{Outcome, run_direct, run_direct_captured};

    /// Write one executable `/bin/sh` script and return its path together
    /// with the directory that keeps it alive for the test.
    fn fake_runtime(body: &str) -> (PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nix");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        make_executable(&path);
        (path, dir)
    }

    /// Write one non-executable regular file.
    fn plain_file(path: &Path) {
        std::fs::write(path, "not executable").expect("write plain file");
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod");
    }

    /// `resolve` default discovery: PATH entries first, then the stable
    /// profile paths in order; non-executable files are skipped everywhere.
    #[test]
    fn resolve_searches_path_then_standard_and_skips_non_executables() {
        let path_dir = tempfile::tempdir().expect("tempdir");
        let modern_dir = tempfile::tempdir().expect("tempdir");
        let legacy_dir = tempfile::tempdir().expect("tempdir");
        let path_nix = path_dir.path().join("nix");
        let modern_nix = modern_dir.path().join("nix");
        let legacy_nix = legacy_dir.path().join("nix");
        let standard = vec![modern_nix.clone(), legacy_nix.clone()];

        // Nothing usable: plain files on PATH and at the standard
        // locations are all ignored, and the error reports the searched
        // locations in order.
        plain_file(&path_nix);
        plain_file(&modern_nix);
        plain_file(&legacy_nix);
        let error = resolve(None, Some(path_dir.path().as_os_str()), &standard)
            .expect_err("no executable candidate");
        let NixError::NotFound {
            path_entries,
            standard: reported,
        } = &error
        else {
            panic!("expected not-found, got: {error}");
        };
        assert_eq!(*path_entries, 1);
        assert_eq!(
            reported,
            &[
                modern_nix.display().to_string(),
                legacy_nix.display().to_string()
            ]
        );

        // The first usable standard candidate in list order wins when
        // PATH holds nothing usable: the modern profile beats the legacy
        // one even though both are executable.
        make_executable(&legacy_nix);
        let found = resolve(None, Some(path_dir.path().as_os_str()), &standard)
            .expect("a standard profile candidate is found");
        assert_eq!(found, legacy_nix);
        make_executable(&modern_nix);
        let found = resolve(None, Some(path_dir.path().as_os_str()), &standard)
            .expect("a standard profile candidate is found");
        assert_eq!(found, modern_nix);

        // A usable PATH candidate wins over the standard paths.
        make_executable(&path_nix);
        let found = resolve(None, Some(path_dir.path().as_os_str()), &standard)
            .expect("path candidate is found");
        assert_eq!(found, path_nix);
    }

    /// A configured value is the only candidate: it wins when usable and
    /// fails alone when not, even with usable candidates elsewhere
    /// (design D2).
    #[test]
    fn resolve_never_falls_back_from_a_configured_value() {
        let (good, _keep) = fake_runtime("exit 0");
        let other_dir = tempfile::tempdir().expect("tempdir");
        let (other, _keep_other) = {
            let path = other_dir.path().join("nix");
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write script");
            make_executable(&path);
            (path, ())
        };

        // Usable configured value: it is used, not the PATH candidate.
        let path_env = other_dir.path().as_os_str();
        let standard = std::slice::from_ref(&other);
        let found = resolve(
            Some(good.to_str().expect("utf8 path")),
            Some(path_env),
            standard,
        )
        .expect("configured candidate is used");
        assert_eq!(found, good);

        // Unusable configured value: the error names it, and the usable
        // PATH and standard candidates are not selected.
        let bad = other_dir.path().join("not-nix");
        plain_file(&bad);
        let error = resolve(
            Some(bad.to_str().expect("utf8 path")),
            Some(path_env),
            standard,
        )
        .expect_err("configured value must fail alone");
        let NixError::ConfiguredUnavailable { requested, .. } = &error else {
            panic!("expected configured-unavailable, got: {error}");
        };
        assert_eq!(requested, &bad.display().to_string());

        // A configured bare name is searched on PATH only, and an
        // executable file at a standard path is not a fallback for it.
        plain_file(&other_dir.path().join("weirdname"));
        let error = resolve(Some("weirdname"), Some(path_env), standard)
            .expect_err("bare configured name must fail when not on PATH");
        assert!(
            matches!(error, NixError::ConfiguredUnavailable { .. }),
            "got: {error}"
        );
    }

    /// The stable candidate list always holds the system profile first,
    /// then the modern per-user state profile, then the legacy per-user
    /// profile; only absolute bases apply.
    #[test]
    fn standard_candidates_follow_the_environment_bases() {
        // No usable bases: only the system profile remains.
        assert_eq!(
            standard_candidates(None, None),
            [PathBuf::from(SYSTEM_PROFILE_NIX)]
        );
        // Relative bases never become search bases.
        let relative = OsString::from("relative/home");
        assert_eq!(
            standard_candidates(Some(relative.as_os_str()), Some(relative.as_os_str())),
            [PathBuf::from(SYSTEM_PROFILE_NIX)]
        );
        // Absolute HOME without XDG_STATE_HOME: the default state base.
        let home = OsString::from("/home/u");
        assert_eq!(
            standard_candidates(None, Some(home.as_os_str())),
            [
                PathBuf::from(SYSTEM_PROFILE_NIX),
                Path::new("/home/u/.local/state").join(STATE_PROFILE_NIX),
                Path::new("/home/u").join(LEGACY_PROFILE_NIX),
            ]
        );
        // Absolute XDG_STATE_HOME wins over the HOME default.
        let state = OsString::from("/var/lib/pkg-state");
        assert_eq!(
            standard_candidates(Some(state.as_os_str()), Some(home.as_os_str())),
            [
                PathBuf::from(SYSTEM_PROFILE_NIX),
                Path::new("/var/lib/pkg-state").join(STATE_PROFILE_NIX),
                Path::new("/home/u").join(LEGACY_PROFILE_NIX),
            ]
        );
    }

    /// Regression: `discover` and every captured operation run a real child
    /// through the shared forward/reap boundary. A capture path that
    /// misclassifies its reaped child fails here instead of failing on every
    /// installed machine.
    #[test]
    fn discover_captures_the_version_from_a_real_child() {
        use std::error::Error as _;
        let (path, _keep) = fake_runtime("echo 'nix (Nix) 2.35.2'");
        let runtime = discover(Some(path.to_str().expect("utf8 path")))
            .expect("discovery captures the child's version");
        assert_eq!(runtime.version(), "nix (Nix) 2.35.2");
        assert_eq!(
            runtime.executable(),
            path.canonicalize().expect("canonical")
        );
        std::fs::remove_file(&path).unwrap();
        let error = runtime.system().expect_err("the runtime was removed");
        assert_eq!(
            error
                .source()
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::NotFound
        );

        let (path, _keep) = fake_runtime("echo probe-failed >&2; exit 7");
        let error = discover(Some(path.to_str().unwrap())).expect_err("probe failed");
        assert!(
            matches!(error.source().unwrap().downcast_ref::<NixError>(), Some(NixError::Failed { status, stderr, .. }) if status == "exit code 7" && stderr.contains("probe-failed"))
        );

        let (path, _keep) = fake_runtime("exit 130");
        let error = discover(Some(path.to_str().unwrap())).expect_err("probe cancelled");
        assert!(matches!(error, NixError::Interrupted { signal: 2, .. }));
    }

    /// Captured success, captured failure with stderr, and streamed success
    /// classify exactly through one real child each.
    #[test]
    fn captured_and_streamed_children_classify_exactly() {
        let profile = tempfile::tempdir().expect("tempdir").keep();
        let profile = profile.join("profile");

        // A captured success returns its stdout.
        let (path, _keep) = fake_runtime("echo 'history text'");
        let runtime = discover(Some(path.to_str().expect("utf8 path"))).expect("discovers");
        assert_eq!(
            runtime.profile_history(&profile).expect("captures stdout"),
            "history text\n"
        );

        // A captured failure keeps the exit status and stderr. The probe
        // still answers `--version`; every other command fails.
        let (path, _keep) = fake_runtime(
            "case \"$*\" in *--version) echo 'nix (Nix) 2.35.2'; exit 0;; \
             *) echo 'native diagnostic' >&2; exit 7;; esac",
        );
        let runtime = discover(Some(path.to_str().expect("utf8 path"))).expect("discovers");
        let error = runtime.profile_history(&profile).expect_err("fails");
        let NixError::Failed {
            status,
            stderr,
            args,
        } = &error
        else {
            panic!("expected a failed native command, got: {error}");
        };
        assert_eq!(status, "exit code 7");
        assert!(stderr.contains("native diagnostic"), "{stderr}");
        assert!(
            args.contains(&String::from("history")),
            "the argument vector is preserved: {args:?}"
        );

        // A filtered removal child runs to completion and is reaped.
        let marker = tempfile::tempdir()
            .expect("tempdir")
            .keep()
            .join("streamed-marker");
        let body = format!("printf ran >> {}", marker.display());
        let (path, _keep) = fake_runtime(&body);
        let runtime = discover(Some(path.to_str().expect("utf8 path"))).expect("discovers");
        runtime
            .profile_remove(&profile, &[])
            .expect("filtered child is reaped and classified");
        assert!(marker.is_file(), "the filtered child must have run");
    }

    /// Direct helper commands share the boundary and the classification.
    #[test]
    fn direct_commands_share_the_classification() {
        let (ok, _keep) = fake_runtime("exit 0");
        assert_eq!(
            run_direct(&ok, &[String::from("sync-trampolines")]).expect("runs"),
            Outcome::Success
        );

        let (fail, _keep) = fake_runtime("echo 'helper diagnostic' >&2; exit 3");
        assert_eq!(
            run_direct(&fail, &[]).expect("runs"),
            Outcome::Failed {
                status: String::from("exit code 3"),
                stderr: String::from("helper diagnostic\n"),
            }
        );

        // The captured variant returns the child's stdout next to the
        // outcome; `run_direct` is its discard-stdout wrapper.
        let (capture, _keep) = fake_runtime("echo 'status text'");
        let (outcome, stdout) = run_direct_captured(&capture, &[]).expect("runs and captures");
        assert_eq!(outcome, Outcome::Success);
        assert_eq!(stdout, "status text\n");
    }
}
