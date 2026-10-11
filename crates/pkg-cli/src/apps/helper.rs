//! Pinned mac-app-util installation and supervised launcher updates.
use crate::config::Paths;
use crate::nix::{Nix, Outcome, run_direct};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
/// The only helper used for launcher and Dock sync, pinned to one exact
/// commit. Verified at this commit: builds with import-from-derivation
/// disabled, and `mac-app-util --help` lists `mktrampoline`,
/// `sync-trampolines`, and `sync-dock`. License AGPL-3.0; the helper is
/// fetched by Nix into the helper profile, never bundled by `pkg`.
const HELPER_FLAKE_REF: &str =
    "github:hraban/mac-app-util/039f33deef21782d4db97087f426504951239887";
/// Repository prefix of [`HELPER_FLAKE_REF`], matched against the helper
/// profile entry's native `original_url`.
const HELPER_REPO_PREFIX: &str = "github:hraban/mac-app-util/";
/// Exact locked revision required in the helper profile entry.
const HELPER_REVISION: &str = "039f33deef21782d4db97087f426504951239887";
/// The helper's upstream lock selects SBCL 2.6.4, whose static memory
/// address conflicts with macOS 27 on ARM64. Pin Nixpkgs with SBCL 2.6.8
/// (the fix landed in 2.6.6) while retaining the exact helper source.
const HELPER_INPUTS: &[(&str, &str)] = &[(
    "nixpkgs",
    "github:NixOS/nixpkgs/b6c8664de9b6cc07fe5666a29f91884ba81197c4",
)];

/// Location of the helper binary inside one of its store outputs.
const HELPER_BINARY_RELATIVE: &str = "bin/mac-app-util";

/// Returns the helper binary, installing the pinned reference first if the
/// helper profile is empty.
///
/// The profile is read, installed into and re-read only when empty, and
/// then checked by one verifier: exactly one active entry pinned to the
/// helper repository and revision, with the helper binary present. Native
/// entry names are opaque (Determinate macOS uses `originalUrl#attrPath`
/// keys), so identity is verified from the entry's own native fields
/// instead of its map key. Anything else is an error for the user to
/// resolve. A verified helper that cannot start because of the macOS 27
/// SBCL address conflict is rebuilt with the fixed dependency pin. Other
/// failures do not trigger a repair.
pub(super) fn ensure_helper(nix: &Nix, paths: &Paths) -> Result<PathBuf, String> {
    let profile = &paths.helper_profile;
    let mut entries = nix
        .profile_list(profile)
        .map_err(|e| format!("could not list helper profile {}: {e}", profile.display()))?;
    if entries.is_empty() {
        nix.profile_add_with_inputs(profile, &[HELPER_FLAKE_REF.to_string()], HELPER_INPUTS)
            .map_err(|e| format!("could not install helper {HELPER_FLAKE_REF}: {e}"))?;
        entries = nix.profile_list(profile).map_err(|e| {
            format!(
                "could not re-read helper profile {}: {e}",
                profile.display()
            )
        })?;
        if entries.is_empty() {
            return Err(format!(
                "installing {HELPER_FLAKE_REF} produced no profile entry"
            ));
        }
    }
    let helper = verify_pinned_helper(profile, &entries)?;
    if helper_runtime_compatible(&helper)? {
        return Ok(helper);
    }

    eprintln!("Updating app helper for macOS compatibility…");
    // Locked flake references cannot be upgraded by `nix profile upgrade`.
    // Build a fresh native profile and verify it before replacing the old
    // generation. A build or probe failure leaves that profile intact.
    let staging = tempfile::tempdir()
        .map_err(|e| format!("could not create an app helper staging directory: {e}"))?;
    let staged_profile = staging.path().join("helper");
    nix.profile_add_with_inputs(
        &staged_profile,
        &[HELPER_FLAKE_REF.to_string()],
        HELPER_INPUTS,
    )
    .map_err(|e| format!("could not update app helper runtime: {e}"))?;
    let staged_entries = nix
        .profile_list(&staged_profile)
        .map_err(|e| format!("could not read staged app helper profile: {e}"))?;
    let staged_helper = verify_pinned_helper(&staged_profile, &staged_entries)?;
    if !helper_runtime_compatible(&staged_helper)? {
        return Err("app helper still cannot start after updating its SBCL runtime".to_string());
    }
    let store_profile = fs::canonicalize(&staged_profile)
        .map_err(|e| format!("could not resolve staged app helper profile: {e}"))?;
    nix.profile_replace(profile, &store_profile)
        .map_err(|e| format!("could not activate updated app helper: {e}"))?;
    let entries = nix.profile_list(profile).map_err(|e| {
        format!(
            "could not re-read helper profile {}: {e}",
            profile.display()
        )
    })?;
    verify_pinned_helper(profile, &entries)
}

/// Probe only the verified helper. The exact SBCL static-address failure
/// allows one dependency repair; interruption and unrelated failures stop
/// the sync before any launcher is replaced.
fn helper_runtime_compatible(helper: &Path) -> Result<bool, String> {
    match run_direct(helper, &[String::from("--help")])? {
        Outcome::Success => Ok(true),
        Outcome::Failed { stderr, .. }
            if stderr.contains("failed to allocate 1048576 bytes at 0x300100000") =>
        {
            Ok(false)
        }
        Outcome::Failed { status, stderr } => Err(format!(
            "app helper could not start ({status}): {}",
            stderr.trim()
        )),
        Outcome::Interrupted { signal } => Err(format!(
            "app helper check was interrupted by signal {signal}"
        )),
    }
}

/// Verify the helper profile holds exactly one active entry pinned to the
/// helper repository and revision, and return its binary.
///
/// One verifier serves the freshly installed and the pre-existing profile
/// alike; the entry-count check applies to both.
fn verify_pinned_helper(
    profile: &Path,
    entries: &BTreeMap<String, crate::nix::ProfileEntry>,
) -> Result<PathBuf, String> {
    if entries.len() != 1 {
        let ids: Vec<String> = entries.keys().cloned().collect();
        return Err(format!(
            "helper profile {} contains unexpected entries: [{}]; \
             remove what you do not want and run `pkg apps sync` again",
            profile.display(),
            ids.join(", ")
        ));
    }
    let Some(entry) = entries.values().next() else {
        return Err(format!(
            "installing {HELPER_FLAKE_REF} produced no profile entry"
        ));
    };
    if !entry.active {
        return Err(format!(
            "helper profile {} holds an inactive entry; remove it and rerun \
             `pkg apps sync`",
            profile.display()
        ));
    }
    if !entry.original_url.starts_with(HELPER_REPO_PREFIX)
        || entry.locked_revision() != Some(HELPER_REVISION)
    {
        return Err(format!(
            "helper profile {} does not hold the pinned helper {HELPER_FLAKE_REF} \
             (found original {}, locked revision {}); remove the entry and rerun \
             `pkg apps sync`",
            profile.display(),
            entry.original_url,
            entry.locked_revision().unwrap_or("none")
        ));
    }
    helper_binary_in(&entry.store_paths).ok_or_else(|| {
        format!(
            "helper profile entry in {} has no {HELPER_BINARY_RELATIVE} binary in its \
             store paths",
            profile.display()
        )
    })
}

/// Finds the helper executable inside the entry's store outputs.
fn helper_binary_in(store_paths: &[String]) -> Option<PathBuf> {
    store_paths.iter().map(Path::new).find_map(|p| {
        let candidate = p.join(HELPER_BINARY_RELATIVE);
        candidate.is_file().then_some(candidate)
    })
}

/// Runs the one helper command this module needs. `sync-trampolines`
/// rebuilds the destination from the source view and refreshes Dock
/// entries for the trampolined apps. The command goes through the shared
/// signal forward/reap boundary, so an interrupted run is reaped and
/// reported here instead of killing pkg.
pub(super) fn run_sync_trampolines(helper: &Path, from: &Path, to: &Path) -> Result<(), String> {
    let args = [
        String::from("sync-trampolines"),
        from.to_string_lossy().into_owned(),
        to.to_string_lossy().into_owned(),
    ];
    match run_direct(helper, &args) {
        Ok(Outcome::Success) => Ok(()),
        Ok(Outcome::Interrupted { signal }) => Err(format!(
            "mac-app-util sync-trampolines was interrupted by signal {signal} \
             ({} -> {}); the launcher folder may be partially replaced; \
             run `pkg apps sync` again",
            from.display(),
            to.display()
        )),
        Ok(Outcome::Failed { status, stderr }) => Err(format!(
            "mac-app-util sync-trampolines failed ({} -> {}; {}): {}",
            from.display(),
            to.display(),
            status,
            stderr.trim()
        )),
        Err(detail) => Err(format!("could not run {}: {detail}", helper.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fake runtime records promotion separately from building, so
    /// failures prove the old helper remains selected.
    fn check_helper_repair(old_error: Option<&str>, build_fails: bool, new_error: Option<&str>) {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::from_bases(dir.path(), dir.path(), dir.path());
        let promoted = dir.path().join("promoted");
        let built = dir.path().join("built");
        let calls = dir.path().join("calls");
        let old = dir.path().join("old");
        let new = dir.path().join("new");
        for (root, error) in [(&old, old_error), (&new, new_error)] {
            fs::create_dir_all(root.join("bin")).expect("create bin");
            let binary = root.join(HELPER_BINARY_RELATIVE);
            let body = error.map_or_else(
                || String::from("exit 0"),
                |error| format!("printf '%s\\n' '{error}' >&2; exit 1"),
            );
            fs::write(&binary, format!("#!/bin/sh\n{body}\n")).expect("write helper");
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let manifest = |root: &Path| {
            serde_json::json!({
                "version": 3,
                "elements": {
                    "opaque-helper-id": {
                        "active": true,
                        "attrPath": "packages.aarch64-darwin.default",
                        "originalUrl": HELPER_FLAKE_REF,
                        "url": HELPER_FLAKE_REF,
                        "storePaths": [root.display().to_string()]
                    }
                }
            })
        };
        let runtime = dir.path().join("nix");
        fs::write(
            &runtime,
            format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$*\" >> '{calls}'\n\
                 args=$*\n\
                 case \"$args\" in *--version*) echo 'nix (Nix) 2.35.2'; exit 0;; esac\n\
                 while [ \"$1\" != '--profile' ]; do shift; done\n\
                 profile=$2\n\
                 case \"$args\" in\n\
                 *'profile list'*)\n\
                   if [ \"$profile\" = '{profile}' ] && [ ! -f '{promoted}' ]; then\n\
                     printf '%s\\n' '{old_manifest}'\n\
                   elif [ -f '{built}' ]; then printf '%s\\n' '{new_manifest}'\n\
                   else echo '{{\"version\":3,\"elements\":{{}}}}'; fi;;\n\
                 *'profile add'*)\n\
                   if {build_fails}; then echo 'error: replacement build failed' >&2; exit 1; fi\n\
                   mkdir -p \"$profile\"; touch '{built}';;\n\
                 *'build --no-link'*) touch '{promoted}';;\n\
                 *) echo \"unexpected nix call: $args\" >&2; exit 9;;\n\
                 esac\n",
                calls = calls.display(),
                profile = paths.helper_profile.display(),
                promoted = promoted.display(),
                built = built.display(),
                old_manifest = manifest(&old),
                new_manifest = manifest(&new),
            ),
        )
        .expect("write runtime");
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).expect("chmod");
        let nix = crate::nix::discover(runtime.to_str()).expect("discover runtime");
        let result = ensure_helper(&nix, &paths);
        let expected_repair = old_error
            .is_some_and(|error| error.contains("failed to allocate 1048576 bytes at 0x300100000"));
        let expected_success =
            old_error.is_none() || expected_repair && !build_fails && new_error.is_none();
        assert_eq!(result.is_ok(), expected_success, "{result:?}");
        assert_eq!(promoted.exists(), expected_success && expected_repair);
        if expected_success {
            let root = if expected_repair { &new } else { &old };
            assert_eq!(
                result.expect("repaired helper"),
                root.join(HELPER_BINARY_RELATIVE)
            );
        }
        let calls = fs::read_to_string(calls).expect("read calls");
        assert_eq!(calls.contains("profile add"), expected_repair);
        if expected_repair {
            assert!(
                calls.contains(&format!(
                    "--override-input nixpkgs {} -- {HELPER_FLAKE_REF}",
                    HELPER_INPUTS[0].1
                )),
                "{calls}"
            );
        }
        assert!(!calls.contains("profile remove"), "{calls}");
        assert!(!calls.contains("profile upgrade"), "{calls}");
    }

    #[test]
    fn helper_address_conflict_repairs_only_after_replacement_is_verified() {
        let error = Some("failed to allocate 1048576 bytes at 0x300100000");
        check_helper_repair(error, false, None);
        check_helper_repair(error, true, None);
        check_helper_repair(error, false, Some("replacement could not start"));
    }

    #[test]
    fn unrelated_helper_startup_failure_does_not_modify_profile() {
        check_helper_repair(Some("permission denied"), false, None);
    }

    #[test]
    fn working_helper_is_reused_without_a_profile_change() {
        check_helper_repair(None, true, None);
    }
}
