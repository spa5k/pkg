//! Spike S3 (PR-7) — RUNNER slice: validated [`Report`] constructors for the
//! Fake and Detect lanes.
//!
//! Every constructor builds a full five-lane [`Report`] (the inactive lanes are
//! `Pending`/`NotSelected`) and VALIDATES it before returning, so a
//! programmer/contract mistake surfaces as a [`ReportError`] (which the binary
//! maps to exit 70) rather than a silently-malformed artifact.
//!
//! This slice does NOT add preflight/build/sign execution: those lanes stay
//! `Pending`/`NotSelected`. `#![forbid(unsafe_code)]`.

use std::io;
use std::path::Path;

use crate::command::{CommandError, CommandRunner, ProbeStatus};
use crate::detect::{DetectOutcome, ProbeRunner, detect};
use crate::manifest::pin_summary;
use crate::preflight::{
    PathInfoProbe, StorePath, classify_cache_miss, derivation_spec, output_path_spec,
    parse_derivation, parse_path_info, parse_prefetch, parse_version, prefetch_spec,
    recursive_path_spec, store_info_spec, version_spec,
};
use crate::report::{
    CoverageEntry, DetectObservation, EvidenceSource, Failure, FailureKind, FakeObservation, Lane,
    LaneState, Lanes, Mode, PendingReason, PreflightObservation, REPORT_SCHEMA_VERSION, Report,
    ReportError, SignPlanObservation, SignTarget, Stage,
};
use crate::validate::{ATTRS, DARWIN_SYSTEMS};

/// Build the active **Fake** report: the Fake lane is `Complete` with a Fixture
/// observation exercising all six fixture (attr×system) pairs; every other lane
/// is `Pending`/`NotSelected`. The report is validated before return.
pub fn fake_report() -> Result<Report, ReportError> {
    let report = Report {
        schema_version: REPORT_SCHEMA_VERSION,
        mode: Mode::Fake,
        harness_only: true,
        pin: pin_summary().clone(),
        lanes: Lanes {
            fake: Lane {
                mode: Mode::Fake,
                state: LaneState::Complete,
                reason: None,
                observation: Some(FakeObservation {
                    source: EvidenceSource::Fixture,
                    fixture_pairs: 6,
                }),
                failures: vec![],
            },
            detect: pending_detect(),
            preflight: crate::report::Lane {
                mode: Mode::Preflight,
                state: LaneState::Pending,
                reason: Some(PendingReason::NotSelected),
                observation: None,
                failures: vec![],
            },
            build_probe: crate::report::Lane {
                mode: Mode::BuildProbe,
                state: LaneState::Pending,
                reason: Some(PendingReason::NotSelected),
                observation: None,
                failures: vec![],
            },
            sign_plan: crate::report::Lane {
                mode: Mode::SignPlan,
                state: LaneState::Pending,
                reason: Some(PendingReason::NotSelected),
                observation: None,
                failures: vec![],
            },
        },
    };
    report.validate()?;
    Ok(report)
}

/// Build the active **Detect** report by running the read-only host probes via
/// `runner` (an injectable [`ProbeRunner`]). Capability absence is a `Complete`
/// observation; only an internal probe failure (timeout/cap/malformed) makes the
/// Detect lane `Incomplete` carrying the partial observation and the failure(s).
/// Every other lane is `Pending`/`NotSelected`. The report is validated before
/// return.
pub fn detect_report(
    runner: &dyn ProbeRunner,
    nix_bin: Option<&Path>,
) -> Result<Report, ReportError> {
    let DetectOutcome {
        observation,
        failures,
    } = detect(runner, nix_bin, crate::detect::host_system());

    let detect_lane = if failures.is_empty() {
        Lane {
            mode: Mode::Detect,
            state: LaneState::Complete,
            reason: None,
            observation: Some(observation),
            failures: vec![],
        }
    } else {
        Lane {
            mode: Mode::Detect,
            state: LaneState::Incomplete,
            reason: None,
            observation: Some(observation),
            failures,
        }
    };

    let report = Report {
        schema_version: REPORT_SCHEMA_VERSION,
        mode: Mode::Detect,
        harness_only: false,
        pin: pin_summary().clone(),
        lanes: Lanes {
            fake: Lane {
                mode: Mode::Fake,
                state: LaneState::Pending,
                reason: Some(PendingReason::NotSelected),
                observation: None,
                failures: vec![],
            },
            detect: detect_lane,
            preflight: crate::report::Lane {
                mode: Mode::Preflight,
                state: LaneState::Pending,
                reason: Some(PendingReason::NotSelected),
                observation: None,
                failures: vec![],
            },
            build_probe: crate::report::Lane {
                mode: Mode::BuildProbe,
                state: LaneState::Pending,
                reason: Some(PendingReason::NotSelected),
                observation: None,
                failures: vec![],
            },
            sign_plan: crate::report::Lane {
                mode: Mode::SignPlan,
                state: LaneState::Pending,
                reason: Some(PendingReason::NotSelected),
                observation: None,
                failures: vec![],
            },
        },
    };
    report.validate()?;
    Ok(report)
}

/// Build the active **SignPlan** report: a pure DESIGNED-ONLY design artifact.
/// The SignPlan lane is `Complete` carrying a [`SignPlanObservation`] whose
/// [`EvidenceSource`] is [`EvidenceSource::Designed`] (a plan, never observed
/// signing evidence), `executed == false` (this slice never performs signing),
/// and `targets == [Runtime, Installer]`. Every other lane is
/// `Pending`/`NotSelected`. The report is validated before return.
///
/// This constructor takes NO runner and performs NO process spawn, host probe,
/// Nix execution, build, signing, notarization, Apple submission, or credential
/// lookup: it is a deterministic design artifact only.
pub fn sign_plan_report() -> Result<Report, ReportError> {
    let report = Report {
        schema_version: REPORT_SCHEMA_VERSION,
        mode: Mode::SignPlan,
        harness_only: false,
        pin: pin_summary().clone(),
        lanes: Lanes {
            fake: pending_lane(Mode::Fake),
            detect: pending_lane(Mode::Detect),
            preflight: pending_lane(Mode::Preflight),
            build_probe: pending_lane(Mode::BuildProbe),
            sign_plan: Lane {
                mode: Mode::SignPlan,
                state: LaneState::Complete,
                reason: None,
                observation: Some(SignPlanObservation {
                    source: EvidenceSource::Designed,
                    executed: false,
                    targets: vec![SignTarget::Runtime, SignTarget::Installer],
                }),
                failures: vec![],
            },
        },
    };
    report.validate()?;
    Ok(report)
}

// ============================================================================
// Preflight orchestration (build-free; injected [`CommandRunner`])
// ============================================================================

/// Build the active **Preflight** report by driving the build-free
/// cache-coverage probes through `runner` (a dependency-injected
/// [`CommandRunner`]) targeting `nix_bin`. This orchestration is NOT a pure
/// function; the contract must be stated honestly. It does NOT itself call
/// `std::process::Command::spawn`, a shell, or a `PATH` lookup, and it is
/// Fake-testable with [`crate::command::FakeCommandRunner`] (these tests drive
/// exactly that, with no process/network/fs). But the execution EFFECTS are
/// exactly those of the supplied `runner`: a [`crate::command::RealRunner`]
/// caller (wired by the `preflight` CLI mode in the `s3-probe` binary) DOES
/// execute the fixed build-free Nix probe [`crate::command::CommandSpec`]s —
/// `nix --version` verifies the exact Nix 2.34.8 version at runtime from the
/// caller-supplied absolute binary, `nix flake prefetch` fetches the pinned
/// GitHub flake/source, and the `nix store info`/`nix path-info` availability
/// queries target cache.nixos.org — so a real run may reach the network and
/// write ordinary Nix-managed fetch/evaluation state (e.g. add the pinned
/// source to the Nix store/fetch cache).
///
/// What IS pure here is the bounded outcome INTERPRETATION: the fixed
/// [`crate::command::CommandSpec`] BUILDERS and the fail-closed
/// PARSERS/classifier live in [`crate::preflight`] and are pure
/// (constructed-byte unit-tested); this function only SEQUENCES them against the
/// injected runner. The probes are build-free and activation-free (no package
/// build/profile activation/signing), but NOT read-only or mutation-free.
///
/// The fixed stop-on-first-failure sequence is:
///   1. `nix --version` — require a normal exit 0 then exact-version parse ⇒
///      `nixVersionExact`.
///   2. `nix flake prefetch --json` — require a normal exit 0 then a NAR-hash-
///      verified parse ⇒ `flakePrefetchVerified`.
///   3. `nix store info --store <cache>` — require a normal exit 0.
///   4. for each canonical (system, attr) cell in system-major order
///      ([`DARWIN_SYSTEMS`] × [`ATTRS`]): `nix derivation show` (exit 0 + one
///      v4 derivation) yields the output path; a nonrecursive `nix path-info`
///      query classifies the output; and — only on an output hit — a recursive
///      `nix path-info` query classifies the closure.
///
/// A cache MISS (output or closure) is an availability observation (a `false`
/// coverage row), NEVER a failure. Only an internal failure stops the lane: the
/// lane becomes [`LaneState::Incomplete`] with exactly ONE closed
/// [`Stage::Preflight`] [`Failure`], no `reason`, and a partial observation
/// carrying ONLY the fully completed canonical prefix (an empty prefix is
/// valid; the partially-probed current cell is NEVER appended). On full success
/// the lane is [`LaneState::Complete`] carrying all six canonical cells. Every
/// other lane is [`LaneState::Pending`] / [`PendingReason::NotSelected`]. The
/// report is validated before return.
pub fn preflight_report(runner: &dyn CommandRunner, nix_bin: &Path) -> Result<Report, ReportError> {
    let mut obs = PreflightObservation {
        source: EvidenceSource::Observed,
        nix_version_exact: false,
        flake_prefetch_verified: false,
        coverage: Vec::new(),
    };
    let mut failure: Option<FailureKind> = None;

    if failure.is_none()
        && let Err(kind) = preflight_version(runner, nix_bin, &mut obs.nix_version_exact)
    {
        failure = Some(kind);
    }
    if failure.is_none()
        && let Err(kind) = preflight_prefetch(runner, nix_bin, &mut obs.flake_prefetch_verified)
    {
        failure = Some(kind);
    }
    if failure.is_none()
        && let Err(kind) = preflight_store_info(runner, nix_bin)
    {
        failure = Some(kind);
    }
    if failure.is_none() {
        'cells: for system in DARWIN_SYSTEMS {
            for attr in ATTRS {
                match preflight_cell(runner, nix_bin, system, attr) {
                    Ok(entry) => obs.coverage.push(entry),
                    Err(kind) => {
                        failure = Some(kind);
                        break 'cells;
                    }
                }
            }
        }
    }

    let preflight_lane = build_preflight_lane(obs, failure);
    let report = Report {
        schema_version: REPORT_SCHEMA_VERSION,
        mode: Mode::Preflight,
        harness_only: false,
        pin: pin_summary().clone(),
        lanes: Lanes {
            fake: pending_lane(Mode::Fake),
            detect: pending_lane(Mode::Detect),
            preflight: preflight_lane,
            build_probe: pending_lane(Mode::BuildProbe),
            sign_plan: pending_lane(Mode::SignPlan),
        },
    };
    report.validate()?;
    Ok(report)
}

/// Assemble the Preflight lane from the (possibly partial) observation and the
/// first failure (if any): `Complete` on success, `Incomplete` with one closed
/// `Stage::Preflight` failure and the retained partial observation otherwise.
fn build_preflight_lane(
    obs: PreflightObservation,
    failure: Option<FailureKind>,
) -> Lane<PreflightObservation> {
    match failure {
        None => Lane {
            mode: Mode::Preflight,
            state: LaneState::Complete,
            reason: None,
            observation: Some(obs),
            failures: vec![],
        },
        Some(kind) => Lane {
            mode: Mode::Preflight,
            state: LaneState::Incomplete,
            reason: None,
            observation: Some(obs),
            failures: vec![Failure {
                stage: Stage::Preflight,
                kind,
            }],
        },
    }
}

/// A `Pending` / `NotSelected` lane of any observation type (the inactive lanes
/// of a Preflight report).
fn pending_lane<T>(mode: Mode) -> Lane<T> {
    Lane {
        mode,
        state: LaneState::Pending,
        reason: Some(PendingReason::NotSelected),
        observation: None,
        failures: vec![],
    }
}

/// Map a runner [`CommandError`] to a closed [`FailureKind`]. A
/// [`CommandError::Timeout`] at ANY stage is [`FailureKind::Timeout`]; a
/// [`CommandError::Spawn`] `{ NotFound }` at the VERSION stage is the
/// version-special [`FailureKind::NixMissing`]; every other runner error is
/// [`FailureKind::Unknown`]. `version_stage` selects the version-specific
/// `NixMissing` mapping (a later-stage `NotFound` is `Unknown`, since version
/// already proved Nix is present).
fn runner_failure(err: CommandError, version_stage: bool) -> FailureKind {
    match err {
        CommandError::Timeout { .. } => FailureKind::Timeout,
        CommandError::Spawn { kind } if version_stage && kind == io::ErrorKind::NotFound => {
            FailureKind::NixMissing
        }
        _ => FailureKind::Unknown,
    }
}

/// Stage 1: `nix --version`. A `Spawn { NotFound }` is `NixMissing`; any other
/// runner error is `Timeout`/`Unknown`; a nonzero/signal exit OR a parse
/// mismatch is `VersionMismatch`. On success sets `nix_version_exact = true`.
fn preflight_version(
    runner: &dyn CommandRunner,
    nix_bin: &Path,
    exact: &mut bool,
) -> Result<(), FailureKind> {
    let spec = version_spec(nix_bin).map_err(|_| FailureKind::Unknown)?;
    match runner.run_probe(&spec) {
        Err(e) => Err(runner_failure(e, true)),
        Ok(outcome) => {
            if !outcome.status.is_zero_exit() {
                return Err(FailureKind::VersionMismatch);
            }
            match parse_version(&outcome.stdout) {
                Ok(()) => {
                    *exact = true;
                    Ok(())
                }
                Err(_) => Err(FailureKind::VersionMismatch),
            }
        }
    }
}

/// Stage 2: `nix flake prefetch --json`. A nonzero/signal exit, a parse failure,
/// or any non-timeout runner error is `Unknown`; a timeout is `Timeout`. On
/// success sets `flake_prefetch_verified = true`.
fn preflight_prefetch(
    runner: &dyn CommandRunner,
    nix_bin: &Path,
    verified: &mut bool,
) -> Result<(), FailureKind> {
    let spec = prefetch_spec(nix_bin).map_err(|_| FailureKind::Unknown)?;
    match runner.run_probe(&spec) {
        Err(e) => Err(runner_failure(e, false)),
        Ok(outcome) => {
            if !outcome.status.is_zero_exit() {
                return Err(FailureKind::Unknown);
            }
            match parse_prefetch(&outcome.stdout) {
                Ok(_) => {
                    *verified = true;
                    Ok(())
                }
                Err(_) => Err(FailureKind::Unknown),
            }
        }
    }
}

/// Stage 3: `nix store info --store <cache>`. A nonzero/signal exit is
/// `CacheQueryFailed`; a timeout is `Timeout`; any other runner error is
/// `Unknown`. Success requires only a normal exit 0 (no output is parsed).
fn preflight_store_info(runner: &dyn CommandRunner, nix_bin: &Path) -> Result<(), FailureKind> {
    let spec = store_info_spec(nix_bin).map_err(|_| FailureKind::Unknown)?;
    match runner.run_probe(&spec) {
        Err(e) => Err(runner_failure(e, false)),
        Ok(outcome) => {
            if outcome.status.is_zero_exit() {
                Ok(())
            } else {
                Err(FailureKind::CacheQueryFailed)
            }
        }
    }
}

/// Stage 4: one canonical (system, attr) cell. `derivation show` (exit 0 + one
/// v4 derivation) yields the output [`StorePath`]; a nonrecursive path-info
/// query classifies the output; and — only on an output hit — a recursive
/// path-info query classifies the closure. Returns the completed
/// [`CoverageEntry`].
///
///   * output MISS ⇒ `{ output=false, closure=false }`, no recursive query;
///   * output HIT + recursive HIT ⇒ `{ true, true }`;
///   * output HIT + recursive MISS ⇒ `{ true, false }`.
///
/// A MISS is an honest observation, never a failure. A nonzero/signal
/// derivation exit or a derivation parse failure is `Unknown`.
fn preflight_cell(
    runner: &dyn CommandRunner,
    nix_bin: &Path,
    system: &str,
    attr: &str,
) -> Result<CoverageEntry, FailureKind> {
    let drv_spec = derivation_spec(nix_bin, system, attr).map_err(|_| FailureKind::Unknown)?;
    let output = match runner.run_probe(&drv_spec) {
        Err(e) => return Err(runner_failure(e, false)),
        Ok(outcome) => {
            if !outcome.status.is_zero_exit() {
                return Err(FailureKind::Unknown);
            }
            match parse_derivation(&outcome.stdout, system) {
                Ok(o) => o,
                Err(_) => return Err(FailureKind::Unknown),
            }
        }
    };

    // Output query (nonrecursive).
    let output_available = match query(runner, nix_bin, &output, false)? {
        QueryOutcome::Hit => true,
        QueryOutcome::Miss => {
            // Output miss ⇒ closure unknown-but-absent; do NOT recurse.
            return Ok(CoverageEntry {
                attr: attr.to_string(),
                system: system.to_string(),
                output_available: false,
                closure_available: false,
            });
        }
    };

    // Recursive closure query (output hit only).
    let closure_available = match query(runner, nix_bin, &output, true)? {
        QueryOutcome::Hit => true,
        QueryOutcome::Miss => false,
    };

    Ok(CoverageEntry {
        attr: attr.to_string(),
        system: system.to_string(),
        output_available,
        closure_available,
    })
}

/// The closed outcome of one path-info query: an availability HIT or a cache
/// MISS (both honest observations), distinct from any internal failure kind.
enum QueryOutcome {
    Hit,
    Miss,
}

/// Run one path-info query (`recursive` selects output vs closure) and classify
/// its outcome. Query semantics:
///   * zero exit ⇒ [`parse_path_info`] (Hit or Miss);
///   * normal nonzero exit ⇒ [`classify_cache_miss`] with the retained stderr
///     (a clean miss is a Miss; anything else is `CacheQueryFailed`);
///   * a signal is `CacheQueryFailed`;
///   * any runner error is `Timeout`/`Unknown`.
fn query(
    runner: &dyn CommandRunner,
    nix_bin: &Path,
    output: &StorePath,
    recursive: bool,
) -> Result<QueryOutcome, FailureKind> {
    let spec = if recursive {
        recursive_path_spec(nix_bin, output)
    } else {
        output_path_spec(nix_bin, output)
    }
    .map_err(|_| FailureKind::Unknown)?;
    let outcome = match runner.run_probe(&spec) {
        Err(e) => return Err(runner_failure(e, false)),
        Ok(o) => o,
    };
    match outcome.status {
        ProbeStatus::Exited(0) => match parse_path_info(&outcome.stdout, output, recursive) {
            Ok(PathInfoProbe::Hit(_)) => Ok(QueryOutcome::Hit),
            Ok(PathInfoProbe::Miss(_)) => Ok(QueryOutcome::Miss),
            Err(_) => Err(FailureKind::CacheQueryFailed),
        },
        ProbeStatus::Exited(_) => {
            match classify_cache_miss(outcome.status, &outcome.stderr, output, recursive) {
                Ok(_) => Ok(QueryOutcome::Miss),
                Err(_) => Err(FailureKind::CacheQueryFailed),
            }
        }
        ProbeStatus::Signaled(_) => Err(FailureKind::CacheQueryFailed),
    }
}

/// A `Pending`/`NotSelected` Detect lane.
fn pending_detect() -> Lane<DetectObservation> {
    Lane {
        mode: Mode::Detect,
        state: LaneState::Pending,
        reason: Some(PendingReason::NotSelected),
        observation: None,
        failures: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandOutcome, ProbeStatus};
    use crate::detect::FakeProbeRunner;
    use crate::report::{Failure, FailureKind, Observation, Stage};

    fn ok_zero(stdout: &[u8]) -> CommandOutcome {
        CommandOutcome {
            status: ProbeStatus::Exited(0),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
            stdout_total_bytes: stdout.len() as u64,
            stderr_total_bytes: 0,
            wall_ms: 1,
        }
    }

    #[test]
    fn fake_report_validates_and_marks_others_not_selected() {
        let r = fake_report().unwrap();
        assert_eq!(r.mode, Mode::Fake);
        assert!(r.harness_only);
        assert_eq!(r.lanes.fake.state, LaneState::Complete);
        assert_eq!(
            r.lanes.fake.observation.as_ref().unwrap().source,
            EvidenceSource::Fixture
        );
        for (mode, state) in [
            (Mode::Detect, r.lanes.detect.state),
            (Mode::Preflight, r.lanes.preflight.state),
            (Mode::BuildProbe, r.lanes.build_probe.state),
            (Mode::SignPlan, r.lanes.sign_plan.state),
        ] {
            assert_eq!(state, LaneState::Pending, "{mode:?}");
        }
        assert_eq!(r.lanes.detect.reason, Some(PendingReason::NotSelected));
    }

    fn fully_scripted_runner() -> FakeProbeRunner {
        let mut r = FakeProbeRunner::new();
        r.mark_all_tools_present();
        r.set_probe(
            &crate::detect::xcode_select_spec(),
            Ok(ok_zero(b"/Applications/Xcode.app/Contents/Developer\n")),
        );
        r.set_probe(&crate::detect::notarytool_spec(), Ok(ok_zero(b"/x\n")));
        r.set_probe(
            &crate::detect::security_application_spec(),
            Ok(ok_zero(
                b"  1) 0123456789abcdef0123456789abcdef01234567 \"Developer ID Application: Acme (TEAM1)\"\n",
            )),
        );
        r.set_probe(
            &crate::detect::security_installer_spec(),
            Ok(ok_zero(
                b"  1) fedcba9876543210fedcba9876543210fedcba98 \"Developer ID Installer: Acme (TEAM1)\"\n",
            )),
        );
        r.set_probe(
            &crate::detect::dscl_spec(),
            Ok(ok_zero(b"GroupMembership: _nixbld1\n")),
        );
        r
    }

    #[test]
    fn detect_report_complete_on_capability_presence() {
        let r = detect_report(&fully_scripted_runner(), None).unwrap();
        assert_eq!(r.mode, Mode::Detect);
        assert!(!r.harness_only);
        assert_eq!(r.lanes.detect.state, LaneState::Complete);
        assert!(r.lanes.detect.failures.is_empty());
        assert!(!r.lanes.detect.observation.as_ref().unwrap().nix_present);
        // Inactive lanes Pending.
        assert_eq!(r.lanes.fake.state, LaneState::Pending);
    }

    #[test]
    fn detect_report_complete_on_capability_absence() {
        // All probes zero-exit / nonzero-absence ⇒ all absence, still Complete
        // (no failures). The security probes use ZERO-exit "0 valid identities
        // found" (genuine identity absence); a nonzero security exit would be a
        // failure under the hardened identity-probe contract.
        let mut r = FakeProbeRunner::new();
        r.mark_all_tools_present();
        r.set_probe(
            &crate::detect::xcode_select_spec(),
            Ok(CommandOutcome {
                status: ProbeStatus::Exited(2),
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdout_total_bytes: 0,
                stderr_total_bytes: 0,
                wall_ms: 1,
            }),
        );
        r.set_probe(
            &crate::detect::notarytool_spec(),
            Ok(CommandOutcome {
                status: ProbeStatus::Exited(1),
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdout_total_bytes: 0,
                stderr_total_bytes: 0,
                wall_ms: 1,
            }),
        );
        r.set_probe(
            &crate::detect::security_application_spec(),
            Ok(ok_zero(b"     0 valid identities found\n")),
        );
        r.set_probe(
            &crate::detect::security_installer_spec(),
            Ok(ok_zero(b"     0 valid identities found\n")),
        );
        r.set_probe(
            &crate::detect::dscl_spec(),
            Ok(CommandOutcome {
                status: ProbeStatus::Exited(1),
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdout_total_bytes: 0,
                stderr_total_bytes: 0,
                wall_ms: 1,
            }),
        );
        let report = detect_report(&r, None).unwrap();
        assert_eq!(report.lanes.detect.state, LaneState::Complete);
        assert!(report.lanes.detect.failures.is_empty());
    }

    #[test]
    fn detect_report_incomplete_on_probe_failure_with_partial_observation() {
        let mut r = fully_scripted_runner();
        // Make xcode-select time out: the lane becomes Incomplete with the rest
        // of the (successfully probed) observation retained.
        r.set_probe(
            &crate::detect::xcode_select_spec(),
            Err(crate::command::CommandError::Timeout { killed: true }),
        );
        let report = detect_report(&r, None).unwrap();
        assert_eq!(report.lanes.detect.state, LaneState::Incomplete);
        assert_eq!(report.lanes.detect.failures.len(), 1);
        assert_eq!(
            report.lanes.detect.failures[0],
            Failure {
                stage: Stage::Detect,
                kind: FailureKind::Timeout
            }
        );
        // Partial observation retained and still valid.
        let obs = report.lanes.detect.observation.as_ref().unwrap();
        assert_eq!(obs.installer_identity_count, 1);
        obs.validate().unwrap();
    }

    // =====================================================================
    // sign_plan_report (designed-only SignPlan design artifact)
    // =====================================================================

    use crate::report::{render_json, render_markdown};

    #[test]
    fn sign_plan_report_active_lane_is_complete_designed_with_exact_targets() {
        let r = sign_plan_report().unwrap();
        assert_eq!(r.mode, Mode::SignPlan);
        assert!(!r.harness_only);
        // Exact embedded pin.
        assert_eq!(&r.pin, crate::manifest::pin_summary());
        // Active SignPlan lane: Complete, no reason, no failures.
        let lane = &r.lanes.sign_plan;
        assert_eq!(lane.mode, Mode::SignPlan);
        assert_eq!(lane.state, LaneState::Complete);
        assert_eq!(lane.reason, None);
        assert!(lane.failures.is_empty());
        let obs = lane.observation.as_ref().expect("signPlan observation");
        // Designed — a plan, never Observed signing evidence.
        assert_eq!(obs.source, EvidenceSource::Designed);
        assert_ne!(obs.source, EvidenceSource::Observed);
        // Never executed in this slice.
        assert!(!obs.executed);
        // Exact target order: [Runtime, Installer].
        assert_eq!(
            obs.targets,
            vec![SignTarget::Runtime, SignTarget::Installer]
        );
    }

    #[test]
    fn sign_plan_report_inactive_lanes_pending_not_selected() {
        let r = sign_plan_report().unwrap();
        // All four inactive lanes: Pending / NotSelected, no observation, no failures.
        assert_eq!(r.lanes.fake.state, LaneState::Pending);
        assert_eq!(r.lanes.fake.reason, Some(PendingReason::NotSelected));
        assert!(r.lanes.fake.observation.is_none());
        assert!(r.lanes.fake.failures.is_empty());

        assert_eq!(r.lanes.detect.state, LaneState::Pending);
        assert_eq!(r.lanes.detect.reason, Some(PendingReason::NotSelected));
        assert!(r.lanes.detect.observation.is_none());
        assert!(r.lanes.detect.failures.is_empty());

        assert_eq!(r.lanes.preflight.state, LaneState::Pending);
        assert_eq!(r.lanes.preflight.reason, Some(PendingReason::NotSelected));
        assert!(r.lanes.preflight.observation.is_none());
        assert!(r.lanes.preflight.failures.is_empty());

        assert_eq!(r.lanes.build_probe.state, LaneState::Pending);
        assert_eq!(r.lanes.build_probe.reason, Some(PendingReason::NotSelected));
        assert!(r.lanes.build_probe.observation.is_none());
        assert!(r.lanes.build_probe.failures.is_empty());
    }

    #[test]
    fn sign_plan_report_build_probe_remains_pending() {
        // Explicit: BuildProbe is never executed in this slice.
        let r = sign_plan_report().unwrap();
        assert_eq!(r.lanes.build_probe.state, LaneState::Pending);
        assert_eq!(r.lanes.build_probe.reason, Some(PendingReason::NotSelected));
        assert!(r.lanes.build_probe.observation.is_none());
        assert!(r.lanes.build_probe.failures.is_empty());
    }

    #[test]
    fn sign_plan_report_validates_and_json_round_trips() {
        let r = sign_plan_report().unwrap();
        // Constructor already validates; re-validating must still pass.
        r.validate().unwrap();
        let json = render_json(&r);
        let back: Report = serde_json::from_str(&json).expect("report JSON parses");
        back.validate().unwrap();
        assert_eq!(back, r);
        // Mode/harness + active-lane observation serialize exactly, and the
        // signPlan evidence is "designed" (never "observed").
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["mode"], serde_json::json!("signPlan"));
        assert_eq!(v["harnessOnly"], serde_json::json!(false));
        let obs = &v["lanes"]["signPlan"]["observation"];
        assert_eq!(obs["source"], serde_json::json!("designed"));
        assert_ne!(obs["source"], serde_json::json!("observed"));
        assert_eq!(obs["executed"], serde_json::json!(false));
        assert_eq!(obs["targets"], serde_json::json!(["runtime", "installer"]));
    }

    #[test]
    fn sign_plan_report_is_deterministic_json_and_markdown() {
        let a = sign_plan_report().unwrap();
        let b = sign_plan_report().unwrap();
        assert_eq!(render_json(&a), render_json(&b));
        assert_eq!(render_markdown(&a), render_markdown(&b));
    }

    #[test]
    fn sign_plan_report_lanes_in_fixed_order() {
        let r = sign_plan_report().unwrap();
        let md = render_markdown(&r);
        let fake = md.find("## Lane fake").expect("fake header");
        let detect = md.find("## Lane detect").expect("detect header");
        let preflight = md.find("## Lane preflight").expect("preflight header");
        let build_probe = md.find("## Lane buildProbe").expect("buildProbe header");
        let sign_plan = md.find("## Lane signPlan").expect("signPlan header");
        assert!(fake < detect, "fake before detect");
        assert!(detect < preflight, "detect before preflight");
        assert!(preflight < build_probe, "preflight before buildProbe");
        assert!(build_probe < sign_plan, "buildProbe before signPlan");
    }

    #[test]
    fn sign_plan_report_carries_no_credential_or_identity_values() {
        let r = sign_plan_report().unwrap();
        let json = render_json(&r);
        let md = render_markdown(&r);
        for s in [&json, &md] {
            for needle in [
                "Developer ID",
                "keychain",
                "profile",
                "identity",
                "TEAM",
                "Apple ID",
                "password",
                "secret",
                "token",
                "credential",
                "certificate",
                "private key",
            ] {
                assert!(
                    !s.contains(needle),
                    "credential-shaped {needle:?} leaked into output"
                );
            }
        }
    }

    #[test]
    fn sign_plan_report_needs_no_runner_or_process() {
        // Pure design artifact: no ProbeRunner/CommandRunner argument, no host
        // probe, no process spawn, no network. Constructed in isolation it must
        // succeed and yield a validating report.
        let r = sign_plan_report().unwrap();
        r.validate().unwrap();
    }
}

// ============================================================================
// Preflight orchestration tests (FakeCommandRunner only; no process/network/fs)
// ============================================================================

#[cfg(test)]
mod preflight_tests {
    use super::*;
    use crate::command::{CommandOutcome, FakeCommandRunner};
    use crate::report::{Observation, render_json, render_markdown};
    use crate::validate;

    /// A valid 32-char nix-base32 hash (the alphabet itself, each char once).
    const H32: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    const NIX_BIN: &str = "/nix/var/nix/bin/nix";

    // ---- outcome builders --------------------------------------------------

    fn ok_outcome(stdout: &[u8]) -> CommandOutcome {
        CommandOutcome {
            status: ProbeStatus::Exited(0),
            stdout: stdout.to_vec(),
            stderr: Vec::new(),
            stdout_total_bytes: stdout.len() as u64,
            stderr_total_bytes: 0,
            wall_ms: 1,
        }
    }

    fn nonzero_outcome(code: i32, stderr: &[u8]) -> CommandOutcome {
        CommandOutcome {
            status: ProbeStatus::Exited(code),
            stdout: Vec::new(),
            stderr: stderr.to_vec(),
            stdout_total_bytes: 0,
            stderr_total_bytes: stderr.len() as u64,
            wall_ms: 1,
        }
    }

    fn signal_outcome() -> CommandOutcome {
        CommandOutcome {
            status: ProbeStatus::Signaled(9),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_total_bytes: 0,
            stderr_total_bytes: 0,
            wall_ms: 1,
        }
    }

    // ---- realistic fixture documents --------------------------------------

    /// The output store-path base for one (system, attr) cell. Distinct per cell
    /// so each cell's output/recursive path-info probe carries a distinct argv.
    fn out_base(system: &str, attr: &str) -> String {
        format!("{H32}-{system}-{attr}")
    }

    /// The derivation map-key base (a store-path base ending in `.drv`).
    fn drv_base(system: &str, attr: &str) -> String {
        format!("{H32}-{system}-{attr}.drv")
    }

    /// A realistic `nix --version` line for the pinned release.
    fn version_line() -> Vec<u8> {
        format!("nix (Nix) {}\n", validate::NIX_VERSION).into_bytes()
    }

    /// A realistic `nix flake prefetch --json` document: `hash` equals the
    /// pinned manifest NAR hash and `storePath` is a valid `/nix/store/<base>`.
    fn prefetch_doc() -> Vec<u8> {
        serde_json::json!({
            "hash": validate::NIXPKGS_NAR_HASH,
            "storePath": format!("/nix/store/{H32}-source"),
            "lastModified": 0,
            "rev": "deadbeef",
        })
        .to_string()
        .into_bytes()
    }

    /// A realistic v4 `nix derivation show` document for one cell: top-level
    /// `version`==4, exactly one derivation whose key is a store base ending in
    /// `.drv`, an inner `version`==4 and nonempty `name`, the requested
    /// canonical `system`, and an `outputs.out.path` store base (NOT absolute).
    fn derivation_doc(system: &str, attr: &str) -> Vec<u8> {
        serde_json::json!({
            "version": crate::preflight::DERIVATION_VERSION,
            "derivations": {
                drv_base(system, attr): {
                    "version": crate::preflight::DERIVATION_VERSION,
                    "name": format!("{attr}-2.12.1"),
                    "system": system,
                    "outputs": { "out": { "path": out_base(system, attr) } },
                    "builder": "/bin/sh",
                    "args": [],
                    "env": {},
                    "inputs": { "drvs": {}, "srcs": [] },
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    /// A realistic v2 path-info HIT document: a single `info` entry (the queried
    /// base) carrying every required inner v2 field.
    fn path_info_hit_doc(base: &str) -> Vec<u8> {
        serde_json::json!({
            "version": crate::preflight::PATH_INFO_VERSION,
            "storeDir": "/nix/store",
            "info": {
                base: {
                    "version": crate::preflight::PATH_INFO_VERSION,
                    "storeDir": "/nix/store",
                    "narHash": "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa=",
                    "narSize": 100u64,
                    "references": [],
                    "ca": null,
                }
            }
        })
        .to_string()
        .into_bytes()
    }

    /// A zero-exit v2 cache-MISS document: the queried base mapped to `null`
    /// (Nix's `--json-format 2` encoding for an unavailable path).
    fn path_info_null_miss_doc(base: &str) -> Vec<u8> {
        serde_json::json!({
            "version": crate::preflight::PATH_INFO_VERSION,
            "storeDir": "/nix/store",
            "info": { base: serde_json::Value::Null }
        })
        .to_string()
        .into_bytes()
    }

    /// The exact single-line nonzero-exit cache-miss diagnostic for `base`.
    fn miss_stderr(base: &str) -> Vec<u8> {
        format!("path '/nix/store/{base}' is not valid\n").into_bytes()
    }

    // ---- runner builders ---------------------------------------------------

    /// Script the three gate probes (version/prefetch/store-info) as success.
    fn gates_ok_runner(nix_bin: &Path) -> FakeCommandRunner {
        let mut r = FakeCommandRunner::new();
        r.set_spec(
            &version_spec(nix_bin).unwrap(),
            Ok(ok_outcome(&version_line())),
        );
        r.set_spec(
            &prefetch_spec(nix_bin).unwrap(),
            Ok(ok_outcome(&prefetch_doc())),
        );
        r.set_spec(&store_info_spec(nix_bin).unwrap(), Ok(ok_outcome(b"ok")));
        r
    }

    /// Gates ok + every cell's `derivation show` ok; the `output`/`recursive`
    /// closures script each cell's path-info probe result by (system, attr).
    fn cells_runner<F, G>(nix_bin: &Path, output: F, recursive: G) -> FakeCommandRunner
    where
        F: Fn(&str, &str) -> Result<CommandOutcome, CommandError>,
        G: Fn(&str, &str) -> Result<CommandOutcome, CommandError>,
    {
        let mut r = gates_ok_runner(nix_bin);
        for system in DARWIN_SYSTEMS {
            for attr in ATTRS {
                let drv = derivation_spec(nix_bin, system, attr).unwrap();
                r.set_spec(&drv, Ok(ok_outcome(&derivation_doc(system, attr))));
                let out = parse_derivation(&derivation_doc(system, attr), system).unwrap();
                let oq = output_path_spec(nix_bin, &out).unwrap();
                r.set_spec(&oq, output(system, attr));
                let rq = recursive_path_spec(nix_bin, &out).unwrap();
                r.set_spec(&rq, recursive(system, attr));
            }
        }
        r
    }

    /// Gates ok + all six cells output-HIT + recursive-HIT.
    fn all_hits_runner(nix_bin: &Path) -> FakeCommandRunner {
        cells_runner(
            nix_bin,
            |sys, attr| Ok(ok_outcome(&path_info_hit_doc(&out_base(sys, attr)))),
            |sys, attr| Ok(ok_outcome(&path_info_hit_doc(&out_base(sys, attr)))),
        )
    }

    /// The canonical six (system, attr) cells in system-major order.
    fn canonical_cells() -> [(&'static str, &'static str); 6] {
        [
            (DARWIN_SYSTEMS[0], ATTRS[0]),
            (DARWIN_SYSTEMS[0], ATTRS[1]),
            (DARWIN_SYSTEMS[0], ATTRS[2]),
            (DARWIN_SYSTEMS[1], ATTRS[0]),
            (DARWIN_SYSTEMS[1], ATTRS[1]),
            (DARWIN_SYSTEMS[1], ATTRS[2]),
        ]
    }

    fn nix_bin() -> &'static Path {
        Path::new(NIX_BIN)
    }

    // =====================================================================
    // 1. all six hits Complete in canonical order
    // =====================================================================

    #[test]
    fn preflight_all_six_hits_complete_canonical_order() {
        let report = preflight_report(&all_hits_runner(nix_bin()), nix_bin()).unwrap();
        assert_eq!(report.mode, Mode::Preflight);
        assert!(!report.harness_only);
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Complete);
        assert_eq!(lane.reason, None);
        assert!(lane.failures.is_empty());
        let obs = lane.observation.as_ref().unwrap();
        assert_eq!(obs.source, EvidenceSource::Observed);
        assert!(obs.nix_version_exact);
        assert!(obs.flake_prefetch_verified);
        assert_eq!(obs.coverage.len(), 6);
        for (entry, (sys, attr)) in obs.coverage.iter().zip(canonical_cells()) {
            assert_eq!(entry.system, sys);
            assert_eq!(entry.attr, attr);
            assert!(entry.output_available);
            assert!(entry.closure_available);
        }
        // Inactive lanes Pending/NotSelected.
        for (mode, state) in [
            (Mode::Fake, report.lanes.fake.state),
            (Mode::Detect, report.lanes.detect.state),
            (Mode::BuildProbe, report.lanes.build_probe.state),
            (Mode::SignPlan, report.lanes.sign_plan.state),
        ] {
            assert_eq!(state, LaneState::Pending, "{mode:?}");
        }
        assert_eq!(report.lanes.fake.reason, Some(PendingReason::NotSelected));
    }

    // =====================================================================
    // 2. zero-exit null misses + exact nonzero diagnostic misses are false
    //    rows, no failures (Complete)
    // =====================================================================

    #[test]
    fn preflight_zero_exit_null_misses_are_false_rows_no_failures() {
        let r = cells_runner(
            nix_bin(),
            |sys, attr| Ok(ok_outcome(&path_info_null_miss_doc(&out_base(sys, attr)))),
            // recursive never consulted on an output miss; script a hit anyway.
            |sys, attr| Ok(ok_outcome(&path_info_hit_doc(&out_base(sys, attr)))),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Complete);
        assert!(lane.failures.is_empty());
        let obs = lane.observation.as_ref().unwrap();
        assert_eq!(obs.coverage.len(), 6);
        for (entry, (sys, attr)) in obs.coverage.iter().zip(canonical_cells()) {
            assert_eq!(entry.system, sys);
            assert_eq!(entry.attr, attr);
            assert!(!entry.output_available);
            assert!(!entry.closure_available);
        }
    }

    #[test]
    fn preflight_nonzero_diagnostic_misses_are_false_rows_no_failures() {
        let r = cells_runner(
            nix_bin(),
            |sys, attr| Ok(nonzero_outcome(1, &miss_stderr(&out_base(sys, attr)))),
            |sys, attr| Ok(nonzero_outcome(1, &miss_stderr(&out_base(sys, attr)))),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Complete);
        assert!(lane.failures.is_empty());
        let obs = lane.observation.as_ref().unwrap();
        assert_eq!(obs.coverage.len(), 6);
        for entry in &obs.coverage {
            assert!(!entry.output_available);
            assert!(!entry.closure_available);
        }
    }

    // =====================================================================
    // 3. output hit + recursive miss (null and nonzero)
    // =====================================================================

    #[test]
    fn preflight_output_hit_recursive_null_miss() {
        let r = cells_runner(
            nix_bin(),
            |sys, attr| Ok(ok_outcome(&path_info_hit_doc(&out_base(sys, attr)))),
            |sys, attr| Ok(ok_outcome(&path_info_null_miss_doc(&out_base(sys, attr)))),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Complete);
        assert!(lane.failures.is_empty());
        let obs = lane.observation.as_ref().unwrap();
        for entry in &obs.coverage {
            assert!(entry.output_available);
            assert!(!entry.closure_available);
        }
    }

    #[test]
    fn preflight_output_hit_recursive_nonzero_miss() {
        let r = cells_runner(
            nix_bin(),
            |sys, attr| Ok(ok_outcome(&path_info_hit_doc(&out_base(sys, attr)))),
            |sys, attr| Ok(nonzero_outcome(1, &miss_stderr(&out_base(sys, attr)))),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Complete);
        assert!(lane.failures.is_empty());
        let obs = lane.observation.as_ref().unwrap();
        for entry in &obs.coverage {
            assert!(entry.output_available);
            assert!(!entry.closure_available);
        }
    }

    // =====================================================================
    // 4. Nix NotFound / wrong version / nonzero version exit / timeout
    // =====================================================================

    #[test]
    fn preflight_nix_not_found_is_nix_missing() {
        // Nothing scripted: version is unmapped => Spawn{NotFound} => NixMissing.
        let r = FakeCommandRunner::new();
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.reason, None);
        assert_eq!(
            lane.failures,
            vec![Failure {
                stage: Stage::Preflight,
                kind: FailureKind::NixMissing
            }]
        );
        let obs = lane.observation.as_ref().unwrap();
        assert!(!obs.nix_version_exact);
        assert!(!obs.flake_prefetch_verified);
        assert!(obs.coverage.is_empty());
    }

    #[test]
    fn preflight_wrong_version_text_is_version_mismatch() {
        let mut r = FakeCommandRunner::new();
        r.set_spec(
            &version_spec(nix_bin()).unwrap(),
            Ok(ok_outcome(b"nix (Nix) 2.34.9\n")),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(
            lane.failures,
            vec![Failure {
                stage: Stage::Preflight,
                kind: FailureKind::VersionMismatch
            }]
        );
        assert!(lane.observation.as_ref().unwrap().coverage.is_empty());
    }

    #[test]
    fn preflight_version_nonzero_exit_is_version_mismatch() {
        let mut r = FakeCommandRunner::new();
        r.set_spec(
            &version_spec(nix_bin()).unwrap(),
            Ok(nonzero_outcome(1, b"")),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.failures[0].kind, FailureKind::VersionMismatch);
    }

    #[test]
    fn preflight_version_timeout_is_timeout() {
        let mut r = FakeCommandRunner::new();
        r.set_spec(
            &version_spec(nix_bin()).unwrap(),
            Err(CommandError::Timeout { killed: true }),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(
            lane.failures,
            vec![Failure {
                stage: Stage::Preflight,
                kind: FailureKind::Timeout
            }]
        );
    }

    #[test]
    fn preflight_version_cap_overflow_is_unknown() {
        // A cap overflow is neither NotFound nor a status/parse mismatch: it
        // falls to the catch-all Unknown at the version stage.
        let mut r = FakeCommandRunner::new();
        r.set_spec(
            &version_spec(nix_bin()).unwrap(),
            Err(CommandError::CapOverflow {
                stream: crate::command::Stream::Stdout,
            }),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.failures[0].kind, FailureKind::Unknown);
    }

    #[test]
    fn preflight_non_absolute_nix_bin_is_unknown() {
        // A non-absolute nix_bin fails spec build at version => Unknown, with no
        // runner invocation at all.
        let r = FakeCommandRunner::new();
        let report = preflight_report(&r, Path::new("nix")).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.failures[0].kind, FailureKind::Unknown);
        assert!(lane.observation.as_ref().unwrap().coverage.is_empty());
    }

    // =====================================================================
    // 5. store-info failure (nonzero/signal) => CacheQueryFailed
    // =====================================================================

    #[test]
    fn preflight_store_info_nonzero_is_cache_query_failed() {
        let mut r = gates_ok_runner(nix_bin());
        r.set_spec(
            &store_info_spec(nix_bin()).unwrap(),
            Ok(nonzero_outcome(1, b"")),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(
            lane.failures,
            vec![Failure {
                stage: Stage::Preflight,
                kind: FailureKind::CacheQueryFailed
            }]
        );
        let obs = lane.observation.as_ref().unwrap();
        // version + prefetch succeeded before store-info failed.
        assert!(obs.nix_version_exact);
        assert!(obs.flake_prefetch_verified);
        assert!(obs.coverage.is_empty());
    }

    #[test]
    fn preflight_store_info_signal_is_cache_query_failed() {
        let mut r = gates_ok_runner(nix_bin());
        r.set_spec(&store_info_spec(nix_bin()).unwrap(), Ok(signal_outcome()));
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.failures[0].kind, FailureKind::CacheQueryFailed);
    }

    #[test]
    fn preflight_store_info_timeout_is_timeout() {
        let mut r = gates_ok_runner(nix_bin());
        r.set_spec(
            &store_info_spec(nix_bin()).unwrap(),
            Err(CommandError::Timeout { killed: true }),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.failures[0].kind, FailureKind::Timeout);
    }

    // =====================================================================
    // 6. prefetch failures => Unknown (status/parse) / Timeout
    // =====================================================================

    #[test]
    fn preflight_prefetch_nonzero_is_unknown() {
        let mut r = gates_ok_runner(nix_bin());
        // Override prefetch to nonzero; version still ok.
        r.set_spec(
            &prefetch_spec(nix_bin()).unwrap(),
            Ok(nonzero_outcome(2, b"")),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.failures[0].kind, FailureKind::Unknown);
        // version succeeded; prefetch did not.
        assert!(lane.observation.as_ref().unwrap().nix_version_exact);
        assert!(!lane.observation.as_ref().unwrap().flake_prefetch_verified);
    }

    #[test]
    fn preflight_prefetch_hash_mismatch_is_unknown() {
        let mut r = gates_ok_runner(nix_bin());
        let bad = serde_json::json!({
            "hash": "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "storePath": format!("/nix/store/{H32}-source"),
        })
        .to_string();
        r.set_spec(
            &prefetch_spec(nix_bin()).unwrap(),
            Ok(ok_outcome(bad.as_bytes())),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.failures[0].kind, FailureKind::Unknown);
    }

    // =====================================================================
    // 7. derivation failure (nonzero/parse) => Unknown
    // =====================================================================

    #[test]
    fn preflight_derivation_nonzero_is_unknown() {
        let mut r = gates_ok_runner(nix_bin());
        let first = derivation_spec(nix_bin(), DARWIN_SYSTEMS[0], ATTRS[0]).unwrap();
        r.set_spec(&first, Ok(nonzero_outcome(1, b"err")));
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(
            lane.failures,
            vec![Failure {
                stage: Stage::Preflight,
                kind: FailureKind::Unknown
            }]
        );
        // No current partial row appended; coverage stays empty.
        assert!(lane.observation.as_ref().unwrap().coverage.is_empty());
    }

    #[test]
    fn preflight_derivation_malformed_json_is_unknown() {
        let mut r = gates_ok_runner(nix_bin());
        let first = derivation_spec(nix_bin(), DARWIN_SYSTEMS[0], ATTRS[0]).unwrap();
        r.set_spec(&first, Ok(ok_outcome(b"{ not json")));
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.failures[0].kind, FailureKind::Unknown);
    }

    #[test]
    fn preflight_derivation_signal_is_unknown() {
        let mut r = gates_ok_runner(nix_bin());
        let first = derivation_spec(nix_bin(), DARWIN_SYSTEMS[0], ATTRS[0]).unwrap();
        r.set_spec(&first, Ok(signal_outcome()));
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.failures[0].kind, FailureKind::Unknown);
    }

    // =====================================================================
    // 8. malformed/unexpected output query => CacheQueryFailed
    // =====================================================================

    #[test]
    fn preflight_output_query_signal_is_cache_query_failed() {
        let r = cells_runner(
            nix_bin(),
            |_sys, _attr| Ok(signal_outcome()),
            |_sys, _attr| Ok(ok_outcome(&path_info_hit_doc(H32))),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.failures[0].kind, FailureKind::CacheQueryFailed);
        // The current (first) cell row is NOT appended.
        assert!(lane.observation.as_ref().unwrap().coverage.is_empty());
    }

    #[test]
    fn preflight_output_query_malformed_json_is_cache_query_failed() {
        let r = cells_runner(
            nix_bin(),
            |_sys, _attr| Ok(ok_outcome(b"{ not json")),
            |_sys, _attr| Ok(ok_outcome(&path_info_hit_doc(H32))),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.failures[0].kind, FailureKind::CacheQueryFailed);
    }

    #[test]
    fn preflight_output_query_unexpected_nonzero_is_cache_query_failed() {
        // A nonzero exit whose stderr is NOT the exact closed miss shape.
        let r = cells_runner(
            nix_bin(),
            |_sys, _attr| Ok(nonzero_outcome(1, b"error: something broke\n")),
            |_sys, _attr| Ok(ok_outcome(&path_info_hit_doc(H32))),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.failures[0].kind, FailureKind::CacheQueryFailed);
    }

    // =====================================================================
    // 9. malformed/unexpected recursive query => CacheQueryFailed
    // =====================================================================

    #[test]
    fn preflight_recursive_query_malformed_is_cache_query_failed() {
        let r = cells_runner(
            nix_bin(),
            |sys, attr| Ok(ok_outcome(&path_info_hit_doc(&out_base(sys, attr)))),
            |_sys, _attr| Ok(ok_outcome(b"{ not json")),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.failures[0].kind, FailureKind::CacheQueryFailed);
        // The current (first) cell row is NOT appended (output hit but closure
        // query failed internally): coverage stays empty.
        assert!(lane.observation.as_ref().unwrap().coverage.is_empty());
    }

    #[test]
    fn preflight_recursive_query_unexpected_nonzero_is_cache_query_failed() {
        let r = cells_runner(
            nix_bin(),
            |sys, attr| Ok(ok_outcome(&path_info_hit_doc(&out_base(sys, attr)))),
            |_sys, _attr| Ok(nonzero_outcome(1, b"error: closure broke\n")),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.failures[0].kind, FailureKind::CacheQueryFailed);
        assert!(lane.observation.as_ref().unwrap().coverage.is_empty());
    }

    #[test]
    fn preflight_recursive_query_signal_is_cache_query_failed() {
        let r = cells_runner(
            nix_bin(),
            |sys, attr| Ok(ok_outcome(&path_info_hit_doc(&out_base(sys, attr)))),
            |_sys, _attr| Ok(signal_outcome()),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.failures[0].kind, FailureKind::CacheQueryFailed);
    }

    // =====================================================================
    // 10. prefix retention + no current partial row
    // =====================================================================

    #[test]
    fn preflight_retains_completed_prefix_on_later_cell_failure() {
        // First three cells (x86_64-darwin: hello/ripgrep/git) complete as hits;
        // the fourth cell (aarch64-darwin/hello) derivation fails.
        let mut r = gates_ok_runner(nix_bin());
        for attr in ATTRS {
            let sys = DARWIN_SYSTEMS[0];
            let drv = derivation_spec(nix_bin(), sys, attr).unwrap();
            r.set_spec(&drv, Ok(ok_outcome(&derivation_doc(sys, attr))));
            let out = parse_derivation(&derivation_doc(sys, attr), sys).unwrap();
            let oq = output_path_spec(nix_bin(), &out).unwrap();
            r.set_spec(&oq, Ok(ok_outcome(&path_info_hit_doc(out.base()))));
            let rq = recursive_path_spec(nix_bin(), &out).unwrap();
            r.set_spec(&rq, Ok(ok_outcome(&path_info_hit_doc(out.base()))));
        }
        let fail = derivation_spec(nix_bin(), DARWIN_SYSTEMS[1], ATTRS[0]).unwrap();
        r.set_spec(&fail, Ok(nonzero_outcome(1, b"err")));

        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.failures[0].kind, FailureKind::Unknown);
        let obs = lane.observation.as_ref().unwrap();
        assert!(obs.nix_version_exact);
        assert!(obs.flake_prefetch_verified);
        // Exactly the first three canonical cells, in order, all true/true. The
        // failing fourth cell is NOT present (no partial current row).
        assert_eq!(obs.coverage.len(), 3);
        for (entry, (sys, attr)) in obs.coverage.iter().zip(canonical_cells()) {
            assert_eq!(entry.system, sys);
            assert_eq!(entry.attr, attr);
            assert!(entry.output_available);
            assert!(entry.closure_available);
        }
        assert_eq!(obs.coverage[2].system, DARWIN_SYSTEMS[0]);
        assert_eq!(obs.coverage[2].attr, ATTRS[2]);
        obs.validate().unwrap();
    }

    #[test]
    fn preflight_no_partial_current_row_on_recursive_failure_mid_run() {
        // Two cells complete (hits); the third cell's output hits but its
        // recursive query fails internally. Coverage must hold exactly the two
        // prior completed rows (the third's partially-known row is dropped).
        let mut r = gates_ok_runner(nix_bin());
        let cells = canonical_cells();
        for (sys, attr) in &cells[..2] {
            let drv = derivation_spec(nix_bin(), sys, attr).unwrap();
            r.set_spec(&drv, Ok(ok_outcome(&derivation_doc(sys, attr))));
            let out = parse_derivation(&derivation_doc(sys, attr), sys).unwrap();
            let oq = output_path_spec(nix_bin(), &out).unwrap();
            r.set_spec(&oq, Ok(ok_outcome(&path_info_hit_doc(out.base()))));
            let rq = recursive_path_spec(nix_bin(), &out).unwrap();
            r.set_spec(&rq, Ok(ok_outcome(&path_info_hit_doc(out.base()))));
        }
        // Third cell: derivation ok + output hit + recursive malformed.
        let (sys, attr) = cells[2];
        let drv = derivation_spec(nix_bin(), sys, attr).unwrap();
        r.set_spec(&drv, Ok(ok_outcome(&derivation_doc(sys, attr))));
        let out = parse_derivation(&derivation_doc(sys, attr), sys).unwrap();
        let oq = output_path_spec(nix_bin(), &out).unwrap();
        r.set_spec(&oq, Ok(ok_outcome(&path_info_hit_doc(out.base()))));
        let rq = recursive_path_spec(nix_bin(), &out).unwrap();
        r.set_spec(&rq, Ok(ok_outcome(b"{ not json")));

        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.failures[0].kind, FailureKind::CacheQueryFailed);
        let obs = lane.observation.as_ref().unwrap();
        assert_eq!(obs.coverage.len(), 2, "third partial row must be dropped");
    }

    // =====================================================================
    // 11. mixed hits/misses stay Complete
    // =====================================================================

    #[test]
    fn preflight_mixed_hits_and_misses_complete() {
        // Cells 0,1 output-hit+recursive-hit; cell 2 output-miss; cell 3
        // output-hit+recursive-miss; cells 4,5 nonzero output-miss. Complete,
        // with the exact per-cell availability and no failures.
        let r = cells_runner(
            nix_bin(),
            |sys, attr| {
                let b = out_base(sys, attr);
                let idx = canonical_cells()
                    .iter()
                    .position(|(s, a)| *s == sys && *a == attr)
                    .unwrap();
                match idx {
                    2 => Ok(nonzero_outcome(1, &miss_stderr(&b))),
                    4 | 5 => Ok(nonzero_outcome(1, &miss_stderr(&b))),
                    _ => Ok(ok_outcome(&path_info_hit_doc(&b))),
                }
            },
            |sys, attr| {
                let b = out_base(sys, attr);
                let idx = canonical_cells()
                    .iter()
                    .position(|(s, a)| *s == sys && *a == attr)
                    .unwrap();
                // recursive only consulted on output hit (cells 0,1,3).
                if idx == 3 {
                    Ok(ok_outcome(&path_info_null_miss_doc(&b)))
                } else {
                    Ok(ok_outcome(&path_info_hit_doc(&b)))
                }
            },
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Complete);
        assert!(lane.failures.is_empty());
        let obs = lane.observation.as_ref().unwrap();
        assert_eq!(obs.coverage.len(), 6);
        let expected = [
            (true, true),
            (true, true),
            (false, false),
            (true, false),
            (false, false),
            (false, false),
        ];
        for (entry, want) in obs.coverage.iter().zip(expected) {
            assert_eq!(
                (entry.output_available, entry.closure_available),
                want,
                "{}/{}",
                entry.system,
                entry.attr
            );
        }
    }

    // =====================================================================
    // 12. stop-on-first-failure: no later probe is executed
    // =====================================================================

    /// A runner that returns a fixed version result and COUNTS every other
    /// probe run. Used to PROVE stop-on-first-failure: after a version failure
    /// no later probe must run.
    struct StopAfterVersionRunner {
        version_result: Result<CommandOutcome, CommandError>,
        others: std::cell::Cell<usize>,
    }

    impl CommandRunner for StopAfterVersionRunner {
        fn run_probe(
            &self,
            spec: &crate::command::CommandSpec,
        ) -> Result<CommandOutcome, CommandError> {
            // version_spec argv is exactly ["--version"].
            let is_version = spec.args.len() == 1 && spec.args[0].to_string_lossy() == "--version";
            if is_version {
                self.version_result.clone()
            } else {
                self.others.set(self.others.get() + 1);
                Err(CommandError::Spawn {
                    kind: io::ErrorKind::NotFound,
                })
            }
        }
    }

    #[test]
    fn preflight_stops_on_first_failure_no_later_probe() {
        let runner = StopAfterVersionRunner {
            version_result: Err(CommandError::Timeout { killed: true }),
            others: std::cell::Cell::new(0),
        };
        let report = preflight_report(&runner, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.state, LaneState::Incomplete);
        assert_eq!(lane.failures.len(), 1);
        assert_eq!(lane.failures[0].kind, FailureKind::Timeout);
        // PROOF: not a single later probe ran.
        assert_eq!(runner.others.get(), 0);
        let obs = lane.observation.as_ref().unwrap();
        assert!(!obs.nix_version_exact);
        assert!(!obs.flake_prefetch_verified);
        assert!(obs.coverage.is_empty());
    }

    #[test]
    fn preflight_stops_on_store_info_failure_no_cell_probe() {
        // Version + prefetch succeed; store-info fails. Prove no cell probe ran
        // by matching on the real built specs (robust to argv ordering) and
        // counting any non-gate probe as a violation.
        struct CountingRunner {
            store_result: Result<CommandOutcome, CommandError>,
            others: std::cell::Cell<usize>,
        }
        impl CommandRunner for CountingRunner {
            fn run_probe(
                &self,
                spec: &crate::command::CommandSpec,
            ) -> Result<CommandOutcome, CommandError> {
                if spec.program == version_spec(nix_bin()).unwrap().program
                    && spec.args == version_spec(nix_bin()).unwrap().args
                {
                    Ok(ok_outcome(&version_line()))
                } else if spec.program == prefetch_spec(nix_bin()).unwrap().program
                    && spec.args == prefetch_spec(nix_bin()).unwrap().args
                {
                    Ok(ok_outcome(&prefetch_doc()))
                } else if spec.program == store_info_spec(nix_bin()).unwrap().program
                    && spec.args == store_info_spec(nix_bin()).unwrap().args
                {
                    self.store_result.clone()
                } else {
                    self.others.set(self.others.get() + 1);
                    Err(CommandError::Spawn {
                        kind: io::ErrorKind::NotFound,
                    })
                }
            }
        }
        let runner = CountingRunner {
            store_result: Err(CommandError::Timeout { killed: true }),
            others: std::cell::Cell::new(0),
        };
        let report = preflight_report(&runner, nix_bin()).unwrap();
        let lane = &report.lanes.preflight;
        assert_eq!(lane.failures[0].kind, FailureKind::Timeout);
        assert_eq!(runner.others.get(), 0);
        assert!(lane.observation.as_ref().unwrap().coverage.is_empty());
    }

    // =====================================================================
    // 13. serialization: validates, no secrets/raw output, deterministic
    // =====================================================================

    #[test]
    fn preflight_complete_report_serializes_validates_no_secrets() {
        let report = preflight_report(&all_hits_runner(nix_bin()), nix_bin()).unwrap();
        let json = render_json(&report);
        let md = render_markdown(&report);
        // Round-trips and re-validates.
        let back: Report = serde_json::from_str(&json).unwrap();
        back.validate().unwrap();
        assert_eq!(back, report);
        // No store path, fixture hash, probe-output hash, or argv leaks.
        for s in [&json, &md] {
            assert!(!s.contains("/nix/store/"), "store path leaked: {s:?}");
            assert!(!s.contains(H32), "fixture hash leaked: {s:?}");
            assert!(
                !s.contains("sha256-aaaaaaaa"),
                "probe-output narHash leaked: {s:?}"
            );
            assert!(
                !s.contains("derivation show") && !s.contains("path-info"),
                "argv leaked: {s:?}"
            );
        }
    }

    #[test]
    fn preflight_incomplete_report_serializes_validates_no_secrets() {
        let mut r = FakeCommandRunner::new();
        r.set_spec(
            &version_spec(nix_bin()).unwrap(),
            Ok(ok_outcome(b"nix (Nix) 2.34.9\n")),
        );
        let report = preflight_report(&r, nix_bin()).unwrap();
        let json = render_json(&report);
        let md = render_markdown(&report);
        let back: Report = serde_json::from_str(&json).unwrap();
        back.validate().unwrap();
        for s in [&json, &md] {
            assert!(!s.contains("/nix/store/"));
            assert!(!s.contains("nix (Nix) 2.34.9"), "raw version stdout leaked");
            assert!(
                !s.contains("VersionMismatch"),
                "internal kind name must serialize as camelCase, not Rust ident"
            );
        }
        // The failure kind serializes as its camelCase JSON form.
        assert!(json.contains("versionMismatch"));
    }

    #[test]
    fn preflight_complete_and_incomplete_deterministic_across_runs() {
        let complete_a = preflight_report(&all_hits_runner(nix_bin()), nix_bin()).unwrap();
        let complete_b = preflight_report(&all_hits_runner(nix_bin()), nix_bin()).unwrap();
        assert_eq!(render_json(&complete_a), render_json(&complete_b));
        assert_eq!(render_markdown(&complete_a), render_markdown(&complete_b));

        let mut r = FakeCommandRunner::new();
        r.set_spec(
            &version_spec(nix_bin()).unwrap(),
            Ok(ok_outcome(b"nix (Nix) 2.34.9\n")),
        );
        let inc_a = preflight_report(&r, nix_bin()).unwrap();
        let inc_b = preflight_report(&r, nix_bin()).unwrap();
        assert_eq!(render_json(&inc_a), render_json(&inc_b));
        assert_eq!(render_markdown(&inc_a), render_markdown(&inc_b));
    }
}
