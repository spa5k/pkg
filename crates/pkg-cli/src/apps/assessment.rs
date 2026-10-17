//! Read-only signature and Gatekeeper checks for public tap apps.
use super::error::AppError;
use super::{payload::prepare_bundle, scan::scan_apps};
use crate::nix::{Outcome, run_direct, run_direct_captured};
use std::collections::BTreeMap;
use std::path::Path;
/// Apple's code-signature verifier, run read-only on vendor bundles.
const CODESIGN_BIN: &str = "/usr/bin/codesign";
/// The signature check every public tap vendor bundle must pass.
const CODESIGN_CHECK: &str = "codesign --verify --deep --strict";
/// Apple's Gatekeeper assessor, run read-only on vendor bundles.
const SPCTL_BIN: &str = "/usr/sbin/spctl";
/// The Gatekeeper check every public tap vendor bundle must pass.
const SPCTL_CHECK: &str = "spctl --assess --type execute --verbose=4";
/// The read-only Gatekeeper status probe run before any assessment.
const SPCTL_STATUS_CHECK: &str = "spctl --status";
/// The exact stdout line `spctl --status` prints when assessment is on.
const ASSESSMENTS_ENABLED: &str = "assessments enabled";

/// Select active entries from this state home's local public tap store.
///
/// [`crate::tap::store::source_of_reference`] matches the entry's `original_url`
/// against `pkg/taps/src/{owner}/{tap}/current`. This also matches entries
/// whose source is no longer registered. The result borrows the profile entries.
fn public_tap_entries<'a>(
    state_home: &'a Path,
    entries: &'a BTreeMap<String, crate::nix::ProfileEntry>,
) -> impl Iterator<Item = (&'a String, &'a crate::nix::ProfileEntry)> {
    entries.iter().filter(move |(_, entry)| {
        entry.active
            && crate::tap::store::source_of_reference(state_home, &entry.original_url).is_some()
    })
}

/// Check vendor signatures and Gatekeeper approval for local public tap apps.
///
/// [`scan_apps`] finds the selected entries' `.app` bundles. If it finds any,
/// [`require_assessments_enabled`] must pass before [`assess_bundle`] checks
/// each bundle. These checks run before destination checks, helper installation,
/// or launcher changes. A failure preserves the launchers and source map.
/// Entries without app bundles and bundles from other sources need no check here.
pub(super) fn assess_public_tap_apps(
    state_home: &Path,
    entries: &BTreeMap<String, crate::nix::ProfileEntry>,
) -> Result<(), AppError> {
    let apps = scan_apps(public_tap_entries(state_home, entries))?;
    if apps.is_empty() {
        // CLI-only public outputs carry no vendor bundle to expose.
        return Ok(());
    }
    // `spctl --assess` cannot confirm approval when Gatekeeper is disabled.
    require_assessments_enabled()?;
    for (_name, bundle) in apps {
        assess_bundle(&prepare_bundle(&bundle)?)?;
    }
    Ok(())
}

/// Whether a `spctl --status` stdout line confirms enabled assessment.
///
/// Accept only `assessments enabled`. All other output fails the check.
fn confirms_enabled_assessment(stdout: &str) -> bool {
    stdout.trim() == ASSESSMENTS_ENABLED
}

/// Check that Gatekeeper assessment is enabled.
///
/// Run `/usr/sbin/spctl --status` with signal forwarding and child reaping.
/// Require a successful exit and the exact `assessments enabled` line.
/// The user must enable assessment in System Settings if it is disabled.
fn require_assessments_enabled() -> Result<(), AppError> {
    match run_direct_captured(Path::new(SPCTL_BIN), &["--status"]) {
        Ok((Outcome::Success, stdout)) if confirms_enabled_assessment(&stdout) => Ok(()),
        Ok((Outcome::Success, stdout)) => Err(format!(
            "cannot confirm that Gatekeeper assessment is enabled; {SPCTL_STATUS_CHECK} \
             printed {:?} instead of {ASSESSMENTS_ENABLED:?}; pkg will not expose \
             public tap app bundles; enable assessment in System Settings and \
             run `pkg apps sync` again",
            stdout.trim()
        )
        .into()),
        Ok((Outcome::Interrupted { signal }, _)) => {
            Err(AppError::interrupted(SPCTL_STATUS_CHECK, signal))
        }
        Ok((Outcome::Failed { status, stderr }, _)) => Err(format!(
            "cannot confirm that Gatekeeper assessment is enabled; {SPCTL_STATUS_CHECK} \
             failed ({status}): {}; no launcher was changed",
            stderr.trim()
        )
        .into()),
        Err(error) => Err(AppError::process(
            format!(
                "cannot confirm that Gatekeeper assessment is enabled; could not run \
             {SPCTL_STATUS_CHECK}; no launcher was changed"
            ),
            error,
        )),
    }
}

/// Check a vendor bundle's signature, then its Gatekeeper approval.
///
/// The tools leave the bundle, its signature, and quarantine attributes
/// unchanged. Passing both checks does not prove the bundle is free of malware.
fn assess_bundle(bundle: &Path) -> Result<(), AppError> {
    let bundle_arg = || bundle.to_string_lossy();
    run_assessment_check(
        Path::new(CODESIGN_BIN),
        &["--verify", "--deep", "--strict", &bundle_arg()],
        bundle,
        CODESIGN_CHECK,
    )?;
    run_assessment_check(
        Path::new(SPCTL_BIN),
        &[
            "--assess",
            "--type",
            "execute",
            "--verbose=4",
            &bundle_arg(),
        ],
        bundle,
        SPCTL_CHECK,
    )
}

/// Run an assessment tool with signal forwarding and child reaping.
/// Include the bundle name in any error.
fn run_assessment_check(
    tool: &Path,
    args: &[&str],
    bundle: &Path,
    check: &str,
) -> Result<(), AppError> {
    match run_direct(tool, args) {
        Ok(Outcome::Success) => Ok(()),
        Ok(Outcome::Interrupted { signal }) => Err(AppError::interrupted(
            format!("{check} on {}", bundle.display()),
            signal,
        )),
        Ok(Outcome::Failed { status, stderr }) => Err(format!(
            "{check} rejected {}: {}; {}; no launcher was changed",
            bundle.display(),
            status,
            stderr.trim()
        )
        .into()),
        Err(error) => Err(AppError::process(
            format!("could not run {check} on {}", bundle.display()),
            error,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::destination::{write_marker, write_source_map};
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn public_tap_selection_takes_only_active_store_entries() {
        // The state home deliberately contains spaces and is never
        // registered in any tap registry: selection must use the real
        // store reference shape, not a hardcoded path or registry lookup.
        let state = tempfile::tempdir().expect("tempdir");
        let home = state.path().join("state home");
        fs::create_dir_all(&home).expect("state home");
        let public_url = crate::tap::store::current_ref(&home, "acme/tools").expect("ref");
        let entry = |active: bool, original_url: &str| crate::nix::ProfileEntry {
            active,
            attr_path: "apps.demo".to_string(),
            original_url: original_url.to_string(),
            locked_url: String::new(),
            store_paths: Vec::new(),
        };
        // A tap reference below a different state home must not match.
        let other_home = state.path().join("other home");
        fs::create_dir_all(&other_home).expect("other home");
        let other_url = crate::tap::store::current_ref(&other_home, "acme/tools").expect("ref");
        let entries = BTreeMap::from([
            ("public#apps.demo".to_string(), entry(true, &public_url)),
            ("inactive#apps.demo".to_string(), entry(false, &public_url)),
            (
                "nixpkgs".to_string(),
                entry(true, "github:NixOS/nixpkgs/nixpkgs-unstable"),
            ),
            ("other-home".to_string(), entry(true, &other_url)),
        ]);

        let selected: Vec<_> = public_tap_entries(&home, &entries)
            .map(|(id, _)| id.as_str())
            .collect();

        // Only the active entry below this state home's tap store counts;
        // the shape is recognized even though the source was never
        // registered or published, Nixpkgs is not a tap, and a tap-shaped
        // reference below another state home does not match.
        assert_eq!(selected, ["public#apps.demo"]);
    }

    #[test]
    fn status_gate_accepts_only_the_exact_enabled_line() {
        // The exact line, with surrounding whitespace, confirms the gate.
        assert!(confirms_enabled_assessment("assessments enabled"));
        assert!(confirms_enabled_assessment("assessments enabled\n"));
        assert!(confirms_enabled_assessment("  assessments enabled  \n"));
        // A disabled system, an unknown string, tool noise, and an empty
        // answer all refuse.
        assert!(!confirms_enabled_assessment("assessments disabled"));
        assert!(!confirms_enabled_assessment("assessments disabled\n"));
        assert!(!confirms_enabled_assessment("Assessments enabled"));
        assert!(!confirms_enabled_assessment("assessments enabled.\n"));
        assert!(!confirms_enabled_assessment(""));
        assert!(!confirms_enabled_assessment("spctl: note\n"));
    }

    #[test]
    fn assessment_gate_is_read_only_and_names_the_failed_bundle() {
        let state = tempfile::tempdir().expect("tempdir");
        let home = state.path().join("state home");
        fs::create_dir_all(&home).expect("state home");

        // One fake store output holding a synthetic vendor bundle,
        // referenced by an active local public tap entry.
        let output = state.path().join("store-output");
        let bundle = output.join("Applications").join("Vendor.app");
        fs::create_dir_all(bundle.join("Contents")).expect("synthetic bundle");
        let original_url = crate::tap::store::current_ref(&home, "acme/tools").expect("ref");
        let entries = BTreeMap::from([(
            "public#apps.vendor".to_string(),
            crate::nix::ProfileEntry {
                active: true,
                attr_path: "apps.vendor".to_string(),
                original_url,
                locked_url: String::new(),
                store_paths: vec![output.display().to_string()],
            },
        )]);

        // A pre-existing owned destination with one launcher and map that
        // a failed assessment must leave untouched.
        let dest = state.path().join("dest");
        fs::create_dir_all(dest.join("Old.app")).expect("old launcher");
        fs::write(dest.join("Old.app").join("trampoline"), b"x").expect("old content");
        write_marker(&dest).expect("marker");
        write_source_map(
            &dest,
            &BTreeMap::from([("Old.app".to_string(), PathBuf::from("/old/store/Old.app"))]),
        )
        .expect("source map");

        // The status gate runs first: where `spctl` is absent the run
        // itself fails, and the refusal names the exact check. On a
        // system where the tool exists but assessment is disabled, the
        // refusal names `assessments disabled` instead. Both refuse.
        let error = assess_public_tap_apps(&home, &entries)
            .expect_err("the vendor gate must refuse the synthetic unsigned bundle");
        // The status gate runs first, so on a host without a usable
        // `spctl` the refusal names the status check; where the tool
        // exists and confirms assessment, the bundle's own signature
        // refusal names the signature check instead. Either way the
        // refusal names an exact read-only check and no bypass.
        let detail = error.to_string();
        assert!(
            detail.contains(SPCTL_STATUS_CHECK) || detail.contains(CODESIGN_CHECK),
            "{error}"
        );
        assert!(!detail.contains("bypass"), "{error}");

        // The bundle check also names the bundle and its check: an
        // unsigned synthetic bundle fails the signature check on macOS;
        // where the tool is absent the run itself fails. Both shapes
        // refuse.
        let error = assess_bundle(&bundle).expect_err("an unsigned synthetic bundle must not pass");
        let detail = error.to_string();
        assert!(detail.contains("Vendor.app"), "{error}");
        assert!(detail.contains(CODESIGN_CHECK), "{error}");

        // This direct check covers destination preservation, not sync ordering.
        assert!(dest.join("Old.app").join("trampoline").exists());
        assert!(
            fs::read_to_string(dest.join(".pkg-owned")).is_ok_and(|t| t == "pkg-apps-owned/1\n")
        );
        assert!(dest.join(".pkg-launcher-sources").exists());
    }

    #[test]
    fn assessment_skips_nixpkgs_and_cli_only_public_outputs() {
        let state = tempfile::tempdir().expect("tempdir");
        let home = state.path().join("state home");
        fs::create_dir_all(&home).expect("state home");
        let public_url = crate::tap::store::current_ref(&home, "acme/tools").expect("ref");
        let entry = |original_url: &str, store_paths: Vec<String>| crate::nix::ProfileEntry {
            active: true,
            attr_path: "apps.demo".to_string(),
            original_url: original_url.to_string(),
            locked_url: String::new(),
            store_paths,
        };

        // An ordinary Nixpkgs entry with an unsigned bundle: not a public
        // tap source, so its bundle is not assessed here.
        let nixpkgs_output = state.path().join("nixpkgs-output");
        let unsigned = nixpkgs_output.join("Applications").join("Unsigned.app");
        fs::create_dir_all(unsigned.join("Contents")).expect("unsigned bundle");

        // A public tap entry whose output is CLI-only: no `.app` bundle,
        // so nothing to assess even though the entry is selected.
        let cli_output = state.path().join("cli-output");
        fs::create_dir_all(cli_output.join("bin")).expect("cli output");

        let entries = BTreeMap::from([
            (
                "nixpkgs".to_string(),
                entry(
                    "github:NixOS/nixpkgs/nixpkgs-unstable",
                    vec![nixpkgs_output.display().to_string()],
                ),
            ),
            (
                "public#apps.cli".to_string(),
                entry(&public_url, vec![cli_output.display().to_string()]),
            ),
        ]);

        assert!(
            assess_public_tap_apps(&home, &entries).is_ok(),
            "no codesign check must run for a Nixpkgs bundle or a CLI-only public output"
        );
    }
}
