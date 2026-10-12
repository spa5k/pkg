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

/// Active entries installed from this state home's local public tap store.
///
/// An entry counts when its `original_url` is the store's stable
/// `pkg/taps/src/{owner}/{tap}/current` reference, recognized by
/// [`crate::tap::store::source_of_reference`] on path components alone,
/// even if the source was later removed from the registry. Entries from
/// Nixpkgs, self-built outputs, or any other source are not selected,
/// and inactive entries are skipped. Selection borrows the profile entries.
fn public_tap_entries<'a>(
    state_home: &'a Path,
    entries: &'a BTreeMap<String, crate::nix::ProfileEntry>,
) -> impl Iterator<Item = (&'a String, &'a crate::nix::ProfileEntry)> {
    entries.iter().filter(move |(_, entry)| {
        entry.active
            && crate::tap::store::source_of_reference(state_home, &entry.original_url).is_some()
    })
}

/// macOS vendor assessment gate for local public tap apps.
///
/// The selected entries are re-scanned with [`scan_apps`], and when they
/// expose any `.app` bundle, Gatekeeper assessment must first be enabled
/// ([`require_assessments_enabled`]) and then every resulting vendor
/// bundle must pass [`assess_bundle`] before the sync continues. The gate
/// runs before the destination write guard, marker handling, helper
/// installation, and any launcher change, so a failure leaves the previous
/// launchers and the recorded source map exactly as they were. Entries
/// that expose no `.app` bundle (CLI-only outputs) need no assessment, and
/// bundles from other sources (Nixpkgs, self-built, ad-hoc signed) are
/// never assessed here.
pub(super) fn assess_public_tap_apps(
    state_home: &Path,
    entries: &BTreeMap<String, crate::nix::ProfileEntry>,
) -> Result<(), AppError> {
    let apps = scan_apps(public_tap_entries(state_home, entries))?;
    if apps.is_empty() {
        // CLI-only public outputs carry no vendor bundle to expose.
        return Ok(());
    }
    // Gatekeeper assessment must be enabled before any vendor bundle is
    // trusted to pass `spctl --assess`: with assessment disabled, that
    // check cannot mean anything.
    require_assessments_enabled()?;
    for (_name, bundle) in apps {
        assess_bundle(&prepare_bundle(&bundle)?)?;
    }
    Ok(())
}

/// Whether a `spctl --status` stdout line confirms enabled assessment.
///
/// Apple's `spctl` prints exactly `assessments enabled` or
/// `assessments disabled`; anything else — including a disabled system,
/// an unknown string, or tool noise — does not confirm the gate and
/// must refuse.
fn confirms_enabled_assessment(stdout: &str) -> bool {
    stdout.trim() == ASSESSMENTS_ENABLED
}

/// Require, read-only, that Gatekeeper assessment is enabled.
///
/// Runs `/usr/sbin/spctl --status` through the shared signal forward/reap
/// boundary and accepts only a successful exit with the exact
/// `assessments enabled` line. A disabled system, an unknown answer, a
/// nonzero exit, or a failed execution refuses; system policy is never
/// changed, bypassed, or recorded — the user enables assessment in System
/// Settings.
fn require_assessments_enabled() -> Result<(), AppError> {
    let args = [String::from("--status")];
    match run_direct_captured(Path::new(SPCTL_BIN), &args) {
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
        Err(detail) => Err(format!(
            "cannot confirm that Gatekeeper assessment is enabled; could not run \
             {SPCTL_STATUS_CHECK}: {detail}; no launcher was changed"
        )
        .into()),
    }
}

/// Verifies one vendor bundle with Apple's own read-only tools:
/// signature first, Gatekeeper second.
///
/// Nothing is modified: no quarantine attribute is stripped, no
/// signature is changed or re-created, the bundle is never opened, and
/// a failed check has no bypass. Success records only that both checks
/// passed; it does not claim the bundle is free of malware.
fn assess_bundle(bundle: &Path) -> Result<(), AppError> {
    let bundle_arg = || bundle.to_string_lossy().into_owned();
    run_assessment_check(
        Path::new(CODESIGN_BIN),
        &[
            "--verify".to_string(),
            "--deep".to_string(),
            "--strict".to_string(),
            bundle_arg(),
        ],
        bundle,
        CODESIGN_CHECK,
    )?;
    run_assessment_check(
        Path::new(SPCTL_BIN),
        &[
            "--assess".to_string(),
            "--type".to_string(),
            "execute".to_string(),
            "--verbose=4".to_string(),
            bundle_arg(),
        ],
        bundle,
        SPCTL_CHECK,
    )
}

/// Runs one assessment tool through the shared signal forward/reap
/// boundary and reports an exact outcome for the named bundle.
fn run_assessment_check(
    tool: &Path,
    args: &[String],
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
        Err(detail) => {
            Err(format!("could not run {check} on {}: {detail}", bundle.display()).into())
        }
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

        // Nothing about the destination changed: this proves the gate
        // itself is read-only. It calls the gate directly and does not
        // prove the full sync ordering, which the parent verifies on a VM.
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
