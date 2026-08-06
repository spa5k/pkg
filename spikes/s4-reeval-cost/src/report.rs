// Spike S4 (PR-6 / DR-004) — REPORT slice: deterministic serde DTOs + a stable
// Markdown renderer for the benchmark run report.
//
// The report is the machine-readable + human-readable record of one Fake or
// Real run. It MUST be deterministic: given the same `Report` value, both the
// JSON form (via serde) and the Markdown form (via [`render_markdown`]) are
// byte-identical. Field order is fixed by struct declaration order, scenario
// rows follow `report.scenarios` order, sample rows follow `scenario.samples`
// order, and numeric fields are formatted exactly. No hashing or pointer
// identity participates in the output. All wall-time and statistic fields are
// integer `u64` milliseconds — never `f64` — so no NaN/Infinity can reach a
// "deterministic" report.
//
// Honesty invariants enforced by [`Report::validate`]:
//   * a Fake report is `harnessOnly`, carries NO detected `nixVersion`, and
//     labels EVERY sample [`CacheLabel::Fixture`] — Fake exercises the exact
//     pipeline with deterministic fixture children, so it can never assert a
//     real Nix state;
//   * a Real report is NOT `harnessOnly` and labels NO sample `Fixture`;
//   * no sample, in any mode, is *fabricated* — a skipped sample may never
//     carry a measured value (wall / rss / output); missing data is recorded as
//     missing, never invented;
//   * a FakeOnly report is a NONEMPTY, fully-exercised harness report: every
//     scenario carries its declared warmup + measured sample counts, every
//     sample is complete (non-skipped / exit 0 / wall + rss + output present),
//     sample indices are a contiguous in-order `0..N-1`, warmup records precede
//     measured records, and its wall + rss statistics exactly recompute from the
//     measured samples (via [`crate::stats::compute`]);
//   * a Complete Real report additionally requires the detected `nixVersion` to
//     equal `pin.nixVersion`, no recorded failures, a nonempty scenario set, and
//     the same per-scenario integrity as FakeOnly — complete samples WITH
//     `outputBytes`, contiguous in-order indices, warmup-first record ordering,
//     and exact wall + rss statistics recomputed from the measured samples;
//   * an Incomplete Real report MUST carry at least one recorded failure, so an
//     empty data set cannot masquerade as unexplained incompleteness; and
//   * the report makes NO budget claim whatsoever — raw evidence reports assert
//     no resource ceilings; budgets may only be PROPOSED in `findings.md` after
//     Real evidence.
//
// It never fabricates Real results and never claims a cache state it cannot
// establish: the only Real cache label is [`CacheLabel::SourceWarmProcessCold`]
// (the flake source was already fetched but every sample is a fresh Nix
// subprocess; the harness never clears the Nix store or evaluator caches).

use serde::{Deserialize, Serialize};

// Recomputation of scenario statistics reuses the canonical, overflow-safe
// integer algorithm in `stats` so the report can never disagree with the
// runner's own math.
use crate::stats;

/// The one-and-only report schema revision this module understands.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Whether the run exercised fixture children (Fake) or real Nix (Real).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Deterministic fixture children, no network, no Nix.
    Fake,
    /// Real pinned Nix against the pinned flake.
    Real,
}

/// How complete the recorded Real data is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Completeness {
    /// Entirely fixture-driven; no Real Nix touched. Implies [`Mode::Fake`].
    FakeOnly,
    /// A Real run that did not finish cleanly (Nix missing/wrong version, or
    /// some scenarios failed). Contains only honestly captured samples.
    Incomplete,
    /// A finished Real run with the full sample set.
    Complete,
}

/// Whether a captured iteration was a warmup or a measured sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Record {
    /// Discarded warmup iteration.
    Warmup,
    /// Counted measured iteration.
    Measured,
}

/// The cache-state *claim* attached to a sample.
///
/// This harness spawns a FRESH `nix` subprocess for every sample and never
/// clears the Nix store or the evaluator caches, so it cannot honestly claim
/// `evalCold`/`storeCold`/`storeWarm` states. The only honest Real label is
/// [`CacheLabel::SourceWarmProcessCold`] (the flake source was already fetched
/// by a prior step, the evaluator process is cold). Fake runs MUST label every
/// sample [`CacheLabel::Fixture`]; anything the harness could not classify is
/// [`CacheLabel::Unknown`]. The sample's `index` / `record` already distinguish
/// first vs. repeated measurements, so no separate "warm/cold" label is needed
/// for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CacheLabel {
    /// Fixture-driven value (Fake runs).
    Fixture,
    /// Real Nix, source already fetched, fresh (cold) evaluator subprocess.
    SourceWarmProcessCold,
    /// Cache state could not be determined.
    Unknown,
}

/// Host the run executed on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Host {
    /// Nix `system` triple of the host.
    pub system: String,
    /// Host machine name (best-effort, recorded not trusted).
    pub machine: String,
    /// Usable CPU core count.
    pub cores: u32,
}

/// The pinned inputs the run targeted (from the validated manifest).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Pin {
    /// The pinned Nix version the run TARGETED (from the manifest).
    pub nix_version: String,
    /// Flake `owner`.
    pub owner: String,
    /// Flake `repo`.
    pub repo: String,
    /// Pinned 40-char Nixpkgs revision.
    pub rev: String,
    /// Pinned flake NAR hash SRI.
    pub nar_hash: String,
    /// Single measured attribute.
    pub attr: String,
}

/// Summary statistics over one metric of a scenario's measured samples. All
/// values are integer `u64` milliseconds (wall) or KiB (rss) — never `f64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SampleStatistics {
    /// Number of measured samples the statistics were computed over.
    pub count: u32,
    /// Minimum.
    pub min: u64,
    /// Median (floor of the two middle elements for an even count).
    pub median: u64,
    /// 95th percentile (nearest-rank).
    pub p95: u64,
    /// Maximum.
    pub max: u64,
}

/// Per-scenario statistics (wall time and/or RSS), each optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Statistics {
    /// Wall-time statistics in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall: Option<SampleStatistics>,
    /// Maximum-RSS statistics in KiB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss: Option<SampleStatistics>,
}

/// One captured sample (warmup or measured) of one scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Sample {
    /// 0-based iteration index within the scenario.
    pub index: u32,
    /// Whether this was a warmup or measured iteration.
    pub record: Record,
    /// Whether the attribute threw / was skipped during evaluation.
    pub skipped: bool,
    /// Wall time in integer milliseconds (`None` when not captured, e.g. a skip).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_ms: Option<u64>,
    /// Maximum RSS in KiB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rss_kb: Option<u64>,
    /// Captured child output in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
    /// Child process exit code.
    pub exit: i32,
    /// Cache-state claim label.
    pub cache: CacheLabel,
}

impl Sample {
    /// A sample is *fabricated* iff it is marked skipped yet carries a measured
    /// value (wall / rss / output). A genuinely skipped attribute produces no
    /// measurement; inventing one is the fabrication this flags. This applies
    /// in every mode and completeness level.
    #[must_use]
    pub fn is_fabricated(&self) -> bool {
        self.skipped
            && (self.wall_ms.is_some() || self.rss_kb.is_some() || self.output_bytes.is_some())
    }
}

/// One measured scenario (a single installable under measurement).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Scenario {
    /// Human-readable scenario name.
    pub name: String,
    /// System triple this scenario ran against.
    pub system: String,
    /// Pure flake installable that was measured.
    pub installable: String,
    /// Declared number of warmup iterations.
    pub warmup: u32,
    /// Declared number of measured iterations.
    pub measured: u32,
    /// Captured samples in iteration order.
    pub samples: Vec<Sample>,
    /// Summary statistics over the measured samples.
    pub statistics: Statistics,
}

/// A recorded failure (e.g. a scenario that could not be measured).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Failure {
    /// Scenario the failure occurred in.
    pub scenario: String,
    /// Pipeline stage that failed.
    pub stage: String,
    /// Bounded failure message.
    pub message: String,
}

/// The top-level benchmark run report. The report makes NO budget claim: raw
/// evidence asserts no resource ceilings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    /// Report schema revision. Must equal [`REPORT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Fake vs Real run.
    pub mode: Mode,
    /// How complete the recorded data is.
    pub completeness: Completeness,
    /// True iff only the harness (fixtures) executed, never real Nix.
    pub harness_only: bool,
    /// Host the run executed on.
    pub host: Host,
    /// Pinned inputs targeted by the run.
    pub pin: Pin,
    /// DETECTED runtime Nix version (absent when Nix was not present, e.g.
    /// Fake runs or a Real run where Nix was missing/wrong).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nix_version: Option<String>,
    /// Measured scenarios.
    pub scenarios: Vec<Scenario>,
    /// Recorded failures.
    pub failures: Vec<Failure>,
}

/// Error returned by [`Report::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    /// `schemaVersion` did not match [`REPORT_SCHEMA_VERSION`].
    SchemaVersion {
        /// Value found in the report.
        found: u32,
        /// Value this module expects.
        expected: u32,
    },
    /// A Fake report's completeness must be `fakeOnly`.
    FakeCompletenessMustBeFakeOnly {
        /// The completeness value found instead.
        found: Completeness,
    },
    /// A Real report can never be `fakeOnly`.
    RealCannotBeFakeOnly,
    /// A Fake report must set `harnessOnly`.
    FakeRequiresHarnessOnly,
    /// A Fake report must carry NO detected `nixVersion`.
    FakeRequiresNoNixVersion,
    /// A Fake report must label every sample `Fixture`.
    FakeRequiresFixtureLabels {
        /// Scenario that contained the non-`Fixture` sample.
        scenario: String,
        /// Index of the offending sample.
        index: u32,
    },
    /// A Real report must NOT set `harnessOnly`.
    RealRequiresHarnessOnlyFalse,
    /// A Real report must label NO sample `Fixture`.
    RealForbidsFixtureLabels {
        /// Scenario that contained a `Fixture`-labelled sample.
        scenario: String,
        /// Index of the offending sample.
        index: u32,
    },
    /// A sample is fabricated (skipped yet carries a measurement value).
    FabricatedSample {
        /// Scenario that contained the fabricated sample.
        scenario: String,
        /// Index of the fabricated sample.
        index: u32,
    },
    /// A Complete report's detected `nixVersion` must equal `pin.nixVersion`.
    CompleteRequiresNixVersionMatch {
        /// Detected version found in the report (empty when absent).
        got: String,
        /// Version targeted by the pin.
        expected: String,
    },
    /// A Complete report must record NO failures.
    CompleteRequiresNoFailures,
    /// A Complete report must contain at least one scenario.
    CompleteRequiresNonemptyScenarios,
    /// A FakeOnly report must contain at least one scenario.
    FakeOnlyRequiresNonemptyScenarios,
    /// An Incomplete report must record at least one failure (so empty data
    /// cannot masquerade as unexplained incompleteness).
    IncompleteRequiresFailure,
    /// A scenario's warmup-record count did not match its declared warmup.
    ScenarioWarmupCountMismatch {
        /// Scenario that mismatched.
        scenario: String,
        /// Declared warmup count.
        declared: u32,
        /// Count of `Warmup` records found.
        found: u32,
    },
    /// A scenario's measured-record count did not match its declared measured.
    ScenarioMeasuredCountMismatch {
        /// Scenario that mismatched.
        scenario: String,
        /// Declared measured count.
        declared: u32,
        /// Count of `Measured` records found.
        found: u32,
    },
    /// A fully-exercised report's (FakeOnly or Complete) sample is incomplete:
    /// skipped, nonzero exit, or missing wall / rss / output.
    SampleIncomplete {
        /// Scenario that contained the incomplete sample.
        scenario: String,
        /// Index of the incomplete sample.
        index: u32,
    },
    /// A fully-exercised scenario's sample indices are not a contiguous
    /// in-order `0..N-1` (a duplicate, a gap, or out-of-order iteration).
    ScenarioIndexNotContiguous {
        /// Scenario that contained the bad index.
        scenario: String,
        /// 0-based position in `samples` where the mismatch was found.
        expected: u32,
        /// `index` found at that position.
        found: u32,
    },
    /// A fully-exercised scenario has a warmup record after a measured record
    /// (records must be grouped warmup-first).
    ScenarioRecordOrder {
        /// Scenario with the mis-ordered record.
        scenario: String,
        /// Index of the offending warmup record.
        index: u32,
    },
    /// A Complete scenario is missing a required metric statistic block.
    StatisticsMissing {
        /// Scenario that is missing the statistic.
        scenario: String,
        /// `"<metric>"` (`"wall"` or `"rss"`).
        metric: &'static str,
    },
    /// A statistic `count` did not match the recomputed measured-sample count.
    StatisticsCountMismatch {
        /// Scenario that mismatched.
        scenario: String,
        /// `"<metric>"` (`"wall"` or `"rss"`).
        metric: &'static str,
        /// `count` declared in the report.
        declared: u32,
        /// Count recomputed from the measured samples.
        recomputed: usize,
    },
    /// A statistic value did not match the recomputed value.
    StatisticsValueMismatch {
        /// Scenario that mismatched.
        scenario: String,
        /// `"<metric>"` (`"wall"` or `"rss"`).
        metric: &'static str,
        /// `"<field>"` (`"min"` / `"median"` / `"p95"` / `"max"`).
        field: &'static str,
        /// Value declared in the report.
        declared: u64,
        /// Value recomputed from the measured samples.
        actual: u64,
    },
}

impl std::fmt::Display for ReportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReportError::SchemaVersion { found, expected } => {
                write!(f, "report schemaVersion is {found}, expected {expected}")
            }
            ReportError::FakeCompletenessMustBeFakeOnly { found } => write!(
                f,
                "a Fake report's completeness must be fakeOnly, found {found:?}"
            ),
            ReportError::RealCannotBeFakeOnly => f.write_str("a Real report can never be fakeOnly"),
            ReportError::FakeRequiresHarnessOnly => {
                f.write_str("a Fake report must set harnessOnly")
            }
            ReportError::FakeRequiresNoNixVersion => {
                f.write_str("a Fake report must carry no detected nixVersion")
            }
            ReportError::FakeRequiresFixtureLabels { scenario, index } => write!(
                f,
                "a Fake report must label every sample Fixture (scenario {scenario:?}, index {index})"
            ),
            ReportError::RealRequiresHarnessOnlyFalse => {
                f.write_str("a Real report must not set harnessOnly")
            }
            ReportError::RealForbidsFixtureLabels { scenario, index } => write!(
                f,
                "a Real report must label no sample Fixture (scenario {scenario:?}, index {index})"
            ),
            ReportError::FabricatedSample { scenario, index } => write!(
                f,
                "report has a fabricated sample (scenario {scenario:?}, index {index})"
            ),
            ReportError::CompleteRequiresNixVersionMatch { got, expected } => write!(
                f,
                "a Complete report's nixVersion must equal pin.nixVersion ({expected:?}), got {got:?}"
            ),
            ReportError::CompleteRequiresNoFailures => {
                f.write_str("a Complete report must record no failures")
            }
            ReportError::CompleteRequiresNonemptyScenarios => {
                f.write_str("a Complete report must contain at least one scenario")
            }
            ReportError::FakeOnlyRequiresNonemptyScenarios => {
                f.write_str("a FakeOnly report must contain at least one scenario")
            }
            ReportError::IncompleteRequiresFailure => {
                f.write_str("an Incomplete report must record at least one failure")
            }
            ReportError::ScenarioWarmupCountMismatch {
                scenario,
                declared,
                found,
            } => write!(
                f,
                "scenario {scenario:?} declared warmup {declared} but has {found} warmup records"
            ),
            ReportError::ScenarioMeasuredCountMismatch {
                scenario,
                declared,
                found,
            } => write!(
                f,
                "scenario {scenario:?} declared measured {declared} but has {found} measured records"
            ),
            ReportError::SampleIncomplete { scenario, index } => write!(
                f,
                "a fully-exercised report sample must be non-skipped, exit 0, and carry wall + rss + output (scenario {scenario:?}, index {index})"
            ),
            ReportError::ScenarioIndexNotContiguous {
                scenario,
                expected,
                found,
            } => write!(
                f,
                "scenario {scenario:?} sample indices are not a contiguous in-order 0..N-1 (at position {expected} found index {found})"
            ),
            ReportError::ScenarioRecordOrder { scenario, index } => write!(
                f,
                "scenario {scenario:?} has a warmup record after a measured record (index {index})"
            ),
            ReportError::StatisticsMissing { scenario, metric } => {
                write!(f, "scenario {scenario:?} is missing {metric} statistics")
            }
            ReportError::StatisticsCountMismatch {
                scenario,
                metric,
                declared,
                recomputed,
            } => write!(
                f,
                "scenario {scenario:?} {metric} statistics count is {declared}, recomputed {recomputed}"
            ),
            ReportError::StatisticsValueMismatch {
                scenario,
                metric,
                field,
                declared,
                actual,
            } => write!(
                f,
                "scenario {scenario:?} {metric}.{field} is {declared}, recomputed {actual}"
            ),
        }
    }
}

impl std::error::Error for ReportError {}

impl Report {
    /// Validate the report's honesty invariants (see the module docs). Returns
    /// `Ok(())` if every invariant holds.
    pub fn validate(&self) -> Result<(), ReportError> {
        if self.schema_version != REPORT_SCHEMA_VERSION {
            return Err(ReportError::SchemaVersion {
                found: self.schema_version,
                expected: REPORT_SCHEMA_VERSION,
            });
        }

        // Mode <-> completeness consistency.
        match (self.mode, self.completeness) {
            (Mode::Fake, Completeness::FakeOnly) => {}
            (Mode::Fake, found) => {
                return Err(ReportError::FakeCompletenessMustBeFakeOnly { found });
            }
            (Mode::Real, Completeness::FakeOnly) => return Err(ReportError::RealCannotBeFakeOnly),
            (Mode::Real, _) => {}
        }

        // Fake-specific hard requirements.
        if self.mode == Mode::Fake {
            if !self.harness_only {
                return Err(ReportError::FakeRequiresHarnessOnly);
            }
            if self.nix_version.is_some() {
                return Err(ReportError::FakeRequiresNoNixVersion);
            }
            for scenario in &self.scenarios {
                for sample in &scenario.samples {
                    if sample.cache != CacheLabel::Fixture {
                        return Err(ReportError::FakeRequiresFixtureLabels {
                            scenario: scenario.name.clone(),
                            index: sample.index,
                        });
                    }
                }
            }
        }

        // Real-specific hard requirements.
        if self.mode == Mode::Real {
            if self.harness_only {
                return Err(ReportError::RealRequiresHarnessOnlyFalse);
            }
            for scenario in &self.scenarios {
                for sample in &scenario.samples {
                    if sample.cache == CacheLabel::Fixture {
                        return Err(ReportError::RealForbidsFixtureLabels {
                            scenario: scenario.name.clone(),
                            index: sample.index,
                        });
                    }
                }
            }
        }

        // Universal fabrication check: no sample, in any mode, may be skipped
        // while carrying a measured value.
        for scenario in &self.scenarios {
            for sample in &scenario.samples {
                if sample.is_fabricated() {
                    return Err(ReportError::FabricatedSample {
                        scenario: scenario.name.clone(),
                        index: sample.index,
                    });
                }
            }
        }

        // An Incomplete Real report MUST carry at least one recorded failure,
        // so an empty data set cannot masquerade as unexplained incompleteness.
        if self.completeness == Completeness::Incomplete && self.failures.is_empty() {
            return Err(ReportError::IncompleteRequiresFailure);
        }

        // FakeOnly integrity: a nonempty, fully-exercised harness report. Every
        // scenario must carry its declared counts, complete samples (incl.
        // output), contiguous in-order indices, warmup-first record ordering, and
        // exactly recomputed wall + rss statistics.
        if self.completeness == Completeness::FakeOnly {
            if self.scenarios.is_empty() {
                return Err(ReportError::FakeOnlyRequiresNonemptyScenarios);
            }
            for scenario in &self.scenarios {
                check_full_scenario(scenario)?;
            }
        }

        // Complete Real integrity: detected version, no failures, nonempty
        // scenarios, declared counts, complete samples (incl. output), contiguous
        // in-order indices, warmup-first record ordering, exact statistics.
        if self.completeness == Completeness::Complete {
            let detected = self.nix_version.as_deref().unwrap_or("");
            if detected != self.pin.nix_version {
                return Err(ReportError::CompleteRequiresNixVersionMatch {
                    got: detected.to_owned(),
                    expected: self.pin.nix_version.clone(),
                });
            }
            if !self.failures.is_empty() {
                return Err(ReportError::CompleteRequiresNoFailures);
            }
            if self.scenarios.is_empty() {
                return Err(ReportError::CompleteRequiresNonemptyScenarios);
            }
            for scenario in &self.scenarios {
                check_full_scenario(scenario)?;
            }
        }

        Ok(())
    }
}

/// Count the samples in `scenario` whose `record` equals `record`.
fn count_record(scenario: &Scenario, record: Record) -> u32 {
    scenario
        .samples
        .iter()
        .filter(|s| s.record == record)
        .count() as u32
}

/// Enforce the per-scenario integrity shared by fully-exercised reports
/// (FakeOnly and Complete Real): declared warmup/measured counts, every sample
/// complete (non-skipped / exit 0 / wall + rss + output present), contiguous
/// in-order `0..N-1` indices, warmup records preceding measured records, and
/// wall/rss statistics matching an exact recomputation from the measured
/// samples.
fn check_full_scenario(scenario: &Scenario) -> Result<(), ReportError> {
    let warmup_found = count_record(scenario, Record::Warmup);
    let measured_found = count_record(scenario, Record::Measured);

    if warmup_found != scenario.warmup {
        return Err(ReportError::ScenarioWarmupCountMismatch {
            scenario: scenario.name.clone(),
            declared: scenario.warmup,
            found: warmup_found,
        });
    }
    // A fully-exercised scenario must declare at least one measured sample
    // (matches the manifest's MIN_SAMPLES = 1) so the statistics recompute is
    // over a nonempty set.
    if scenario.measured == 0 {
        return Err(ReportError::ScenarioMeasuredCountMismatch {
            scenario: scenario.name.clone(),
            declared: 0,
            found: measured_found,
        });
    }
    if measured_found != scenario.measured {
        return Err(ReportError::ScenarioMeasuredCountMismatch {
            scenario: scenario.name.clone(),
            declared: scenario.measured,
            found: measured_found,
        });
    }

    // Every sample in a fully-exercised report must be complete: non-skipped,
    // exit 0, and carrying wall + rss + output measurements (warmup iterations
    // are real Nix / fixture invocations too, so they are captured).
    for sample in &scenario.samples {
        if sample.skipped
            || sample.exit != 0
            || sample.wall_ms.is_none()
            || sample.rss_kb.is_none()
            || sample.output_bytes.is_none()
        {
            return Err(ReportError::SampleIncomplete {
                scenario: scenario.name.clone(),
                index: sample.index,
            });
        }
    }

    // Sample indices are a contiguous, in-order `0..N-1`: at position `pos` the
    // index MUST equal `pos`. This single check rejects duplicates, gaps, and
    // out-of-order iteration in one pass.
    for (pos, sample) in scenario.samples.iter().enumerate() {
        if sample.index != pos as u32 {
            return Err(ReportError::ScenarioIndexNotContiguous {
                scenario: scenario.name.clone(),
                expected: pos as u32,
                found: sample.index,
            });
        }
    }

    // Records are grouped: every warmup record must precede every measured
    // record. Once a measured record has been seen, a later warmup is an error.
    let mut seen_measured = false;
    for sample in &scenario.samples {
        match sample.record {
            Record::Measured => seen_measured = true,
            Record::Warmup if seen_measured => {
                return Err(ReportError::ScenarioRecordOrder {
                    scenario: scenario.name.clone(),
                    index: sample.index,
                });
            }
            Record::Warmup => {}
        }
    }

    // Recompute wall + rss statistics over the measured samples and require an
    // exact match with the declared statistics.
    let wall_vals: Vec<u64> = scenario
        .samples
        .iter()
        .filter(|s| s.record == Record::Measured)
        .map(|s| {
            s.wall_ms
                .expect("measured sample has wall_ms (checked above)")
        })
        .collect();
    let rss_vals: Vec<u64> = scenario
        .samples
        .iter()
        .filter(|s| s.record == Record::Measured)
        .map(|s| {
            s.rss_kb
                .expect("measured sample has rss_kb (checked above)")
        })
        .collect();

    let wall_stats =
        scenario
            .statistics
            .wall
            .as_ref()
            .ok_or_else(|| ReportError::StatisticsMissing {
                scenario: scenario.name.clone(),
                metric: "wall",
            })?;
    let rss_stats =
        scenario
            .statistics
            .rss
            .as_ref()
            .ok_or_else(|| ReportError::StatisticsMissing {
                scenario: scenario.name.clone(),
                metric: "rss",
            })?;

    check_recompute(&scenario.name, "wall", wall_stats, &wall_vals)?;
    check_recompute(&scenario.name, "rss", rss_stats, &rss_vals)?;
    Ok(())
}

/// Recompute statistics over `vals` with [`crate::stats::compute`] and require
/// `stats` to match the recomputation exactly (count + min/median/p95/max).
fn check_recompute(
    scenario: &str,
    metric: &'static str,
    stats: &SampleStatistics,
    vals: &[u64],
) -> Result<(), ReportError> {
    let recomputed = match stats::compute(vals) {
        Ok(s) => s,
        // `vals` is the measured-sample metric vector; for a Complete scenario
        // it is nonempty (guarded by the `measured >= 1` check). An empty
        // recompute is therefore a count mismatch.
        Err(_) => {
            return Err(ReportError::StatisticsCountMismatch {
                scenario: scenario.to_owned(),
                metric,
                declared: stats.count,
                recomputed: vals.len(),
            });
        }
    };
    if stats.count != vals.len() as u32 {
        return Err(ReportError::StatisticsCountMismatch {
            scenario: scenario.to_owned(),
            metric,
            declared: stats.count,
            recomputed: vals.len(),
        });
    }
    for (field, declared, actual) in [
        ("min", stats.min, recomputed.min),
        ("median", stats.median, recomputed.median),
        ("p95", stats.p95, recomputed.p95),
        ("max", stats.max, recomputed.max),
    ] {
        if declared != actual {
            return Err(ReportError::StatisticsValueMismatch {
                scenario: scenario.to_owned(),
                metric,
                field,
                declared,
                actual,
            });
        }
    }
    Ok(())
}

/// Render `report` as a deterministic Markdown document.
///
/// Determinism guarantees: each metadata key is emitted exactly once; scenario
/// rows follow `report.scenarios` order; sample rows follow `scenario.samples`
/// order; integer fields are formatted exactly; missing optionals render as the
/// single glyph `—`. No map types are used, so the output depends only on the
/// input `Report` value. Caller-controlled strings (scenario headings, failure
/// cells, metadata values) are passed through [`escape_md`], which collapses
/// newlines/CR (preventing heading or table injection) and escapes the table
/// delimiter `|`.
#[must_use]
pub fn render_markdown(report: &Report) -> String {
    let mut out = String::new();
    out.push_str("# S4 reeval-cost report\n\n");
    out.push_str("| field | value |\n|---|---|\n");
    md_kv(
        &mut out,
        "schemaVersion",
        &report.schema_version.to_string(),
    );
    md_kv(&mut out, "mode", mode_str(report.mode));
    md_kv(
        &mut out,
        "completeness",
        completeness_str(report.completeness),
    );
    md_kv(&mut out, "harnessOnly", bool_str(report.harness_only));
    md_kv(&mut out, "host.system", &report.host.system);
    md_kv(&mut out, "host.machine", &report.host.machine);
    md_kv(&mut out, "host.cores", &report.host.cores.to_string());
    md_kv(&mut out, "pin.owner", &report.pin.owner);
    md_kv(&mut out, "pin.repo", &report.pin.repo);
    md_kv(&mut out, "pin.rev", &report.pin.rev);
    md_kv(&mut out, "pin.narHash", &report.pin.nar_hash);
    md_kv(&mut out, "pin.attr", &report.pin.attr);
    md_kv(&mut out, "pin.nixVersion", &report.pin.nix_version);
    md_kv(
        &mut out,
        "nixVersion",
        report.nix_version.as_deref().unwrap_or("—"),
    );
    out.push('\n');

    for scenario in &report.scenarios {
        // Heading text is escaped: newlines/CR are flattened so a malicious
        // scenario name cannot start a new Markdown block or table.
        out.push_str(&format!("## {}\n\n", escape_md(&scenario.name)));
        out.push_str(
            "| index | record | skipped | wall (ms) | rss (KB) | output (B) | exit | cache |\n",
        );
        out.push_str("|---|---|---|---|---|---|---|---|\n");
        for sample in &scenario.samples {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                sample.index,
                record_str(sample.record),
                bool_str(sample.skipped),
                opt_u64(sample.wall_ms),
                opt_u64(sample.rss_kb),
                opt_u64(sample.output_bytes),
                sample.exit,
                cache_str(sample.cache),
            ));
        }
        out.push('\n');
        out.push_str("| metric | count | min | median | p95 | max |\n");
        out.push_str("|---|---|---|---|---|---|\n");
        md_stat_row(&mut out, "wall (ms)", scenario.statistics.wall.as_ref());
        md_stat_row(&mut out, "rss (KB)", scenario.statistics.rss.as_ref());
        out.push('\n');
    }

    if !report.failures.is_empty() {
        out.push_str("## Failures\n\n");
        out.push_str("| scenario | stage | message |\n|---|---|---|\n");
        for failure in &report.failures {
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                escape_md(&failure.scenario),
                escape_md(&failure.stage),
                escape_md(&failure.message),
            ));
        }
        out.push('\n');
    }

    out
}

fn md_kv(out: &mut String, key: &str, value: &str) {
    out.push_str(&format!("| {} | {} |\n", escape_md(key), escape_md(value)));
}

fn md_stat_row(out: &mut String, metric: &str, stats: Option<&SampleStatistics>) {
    match stats {
        Some(stats) => out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            metric, stats.count, stats.min, stats.median, stats.p95, stats.max,
        )),
        None => out.push_str(&format!("| {metric} | — | — | — | — | — |\n")),
    }
}

/// Escape a caller-controlled string for a Markdown table cell or heading:
/// escape the table delimiter `|`, flatten newlines to a single space (which
/// also prevents a cell/heading from injecting a new block), and drop CR.
fn escape_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '|' => out.push_str("\\|"),
            '\n' => out.push(' '),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

fn bool_str(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

fn opt_u64(x: Option<u64>) -> String {
    match x {
        Some(v) => v.to_string(),
        None => "—".to_owned(),
    }
}

fn mode_str(m: Mode) -> &'static str {
    match m {
        Mode::Fake => "fake",
        Mode::Real => "real",
    }
}

fn completeness_str(c: Completeness) -> &'static str {
    match c {
        Completeness::FakeOnly => "fakeOnly",
        Completeness::Incomplete => "incomplete",
        Completeness::Complete => "complete",
    }
}

fn record_str(r: Record) -> &'static str {
    match r {
        Record::Warmup => "warmup",
        Record::Measured => "measured",
    }
}

fn cache_str(c: CacheLabel) -> &'static str {
    match c {
        CacheLabel::Fixture => "fixture",
        CacheLabel::SourceWarmProcessCold => "sourceWarmProcessCold",
        CacheLabel::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin() -> Pin {
        Pin {
            nix_version: "2.34.8".to_owned(),
            owner: "NixOS".to_owned(),
            repo: "nixpkgs".to_owned(),
            rev: "a62e6edd6d5e1fa0329b8653c801147986f8d446".to_owned(),
            nar_hash: "sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=".to_owned(),
            attr: "ripgrep".to_owned(),
        }
    }

    fn host() -> Host {
        Host {
            system: "x86_64-linux".to_owned(),
            machine: "bench-host".to_owned(),
            cores: 8,
        }
    }

    /// A measured Real sample (non-skipped, exit 0, full measurements), labelled
    /// `SourceWarmProcessCold`.
    fn measured_sample(index: u32, wall: u64, rss: u64) -> Sample {
        Sample {
            index,
            record: Record::Measured,
            skipped: false,
            wall_ms: Some(wall),
            rss_kb: Some(rss),
            output_bytes: Some(64),
            exit: 0,
            cache: CacheLabel::SourceWarmProcessCold,
        }
    }

    /// A warmup Real sample (captured but discarded from stats).
    fn warmup_sample(index: u32, wall: u64, rss: u64) -> Sample {
        Sample {
            index,
            record: Record::Warmup,
            skipped: false,
            wall_ms: Some(wall),
            rss_kb: Some(rss),
            output_bytes: Some(0),
            exit: 0,
            cache: CacheLabel::SourceWarmProcessCold,
        }
    }

    fn fake_sample(index: u32) -> Sample {
        Sample {
            index,
            record: Record::Measured,
            skipped: false,
            wall_ms: Some(12),
            rss_kb: Some(2048),
            output_bytes: Some(0),
            exit: 0,
            cache: CacheLabel::Fixture,
        }
    }

    /// Build a [`SampleStatistics`] from raw values via the canonical
    /// [`stats::compute`], so happy-path tests can never disagree with the
    /// validator's recomputation.
    fn stats_from(vals: &[u64]) -> SampleStatistics {
        let s = stats::compute(vals).unwrap();
        SampleStatistics {
            count: vals.len() as u32,
            min: s.min,
            median: s.median,
            p95: s.p95,
            max: s.max,
        }
    }

    /// A self-consistent Complete-Real scenario: 1 warmup + 3 measured samples
    /// whose statistics exactly recompute.
    fn real_complete_scenario() -> Scenario {
        let wall_vals: [u64; 3] = [10, 12, 16];
        let rss_vals: [u64; 3] = [100_000, 102_000, 106_000];
        Scenario {
            name: "single-attr:ripgrep".to_owned(),
            system: "x86_64-linux".to_owned(),
            installable: "github:NixOS/nixpkgs/x#legacyPackages.x86_64-linux.ripgrep.drvPath"
                .to_owned(),
            warmup: 1,
            measured: 3,
            samples: vec![
                warmup_sample(0, 9, 99_000),
                measured_sample(1, wall_vals[0], rss_vals[0]),
                measured_sample(2, wall_vals[1], rss_vals[1]),
                measured_sample(3, wall_vals[2], rss_vals[2]),
            ],
            statistics: Statistics {
                wall: Some(stats_from(&wall_vals)),
                rss: Some(stats_from(&rss_vals)),
            },
        }
    }

    fn fake_scenario() -> Scenario {
        Scenario {
            name: "single-attr:ripgrep".to_owned(),
            system: "x86_64-linux".to_owned(),
            installable: "github:NixOS/nixpkgs/x#legacyPackages.x86_64-linux.ripgrep.drvPath"
                .to_owned(),
            warmup: 0,
            measured: 2,
            samples: vec![fake_sample(0), fake_sample(1)],
            statistics: Statistics {
                wall: Some(stats_from(&[12, 12])),
                rss: Some(stats_from(&[2048, 2048])),
            },
        }
    }

    fn fake_report() -> Report {
        Report {
            schema_version: REPORT_SCHEMA_VERSION,
            mode: Mode::Fake,
            completeness: Completeness::FakeOnly,
            harness_only: true,
            host: host(),
            pin: pin(),
            nix_version: None,
            scenarios: vec![fake_scenario()],
            failures: vec![],
        }
    }

    fn real_complete_report() -> Report {
        Report {
            schema_version: REPORT_SCHEMA_VERSION,
            mode: Mode::Real,
            completeness: Completeness::Complete,
            harness_only: false,
            host: host(),
            pin: pin(),
            nix_version: Some("2.34.8".to_owned()),
            scenarios: vec![real_complete_scenario()],
            failures: vec![],
        }
    }

    // ---- happy paths --------------------------------------------------------

    #[test]
    fn validate_accepts_fake() {
        fake_report().validate().unwrap();
    }

    #[test]
    fn validate_accepts_complete_real() {
        real_complete_report().validate().unwrap();
    }

    #[test]
    fn validate_accepts_incomplete_real_with_partial_samples() {
        // An honest Incomplete Real run may keep partial samples: a measured
        // sample with a value, and a skipped sample with NO value. Statistics
        // may be absent. It MUST record at least one failure explaining the
        // incompleteness.
        let mut report = real_complete_report();
        report.completeness = Completeness::Incomplete;
        report.scenarios[0].samples = vec![
            measured_sample(0, 10, 100_000),
            Sample {
                index: 1,
                record: Record::Measured,
                skipped: true,
                wall_ms: None,
                rss_kb: None,
                output_bytes: None,
                exit: 1,
                cache: CacheLabel::SourceWarmProcessCold,
            },
        ];
        report.scenarios[0].statistics = Statistics {
            wall: None,
            rss: None,
        };
        report.failures.push(Failure {
            scenario: "single-attr:ripgrep".to_owned(),
            stage: "eval".to_owned(),
            message: "nix version mismatch".to_owned(),
        });
        report.validate().unwrap();
    }

    // ---- serde round-trip + determinism ------------------------------------

    #[test]
    fn roundtrip_serde_fake() {
        let report = fake_report();
        report.validate().unwrap();
        let json = serde_json::to_string(&report).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }

    #[test]
    fn roundtrip_serde_complete_real() {
        let report = real_complete_report();
        report.validate().unwrap();
        let json = serde_json::to_string(&report).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }

    #[test]
    fn roundtrip_serde_incomplete_real() {
        let mut report = real_complete_report();
        report.completeness = Completeness::Incomplete;
        report.scenarios[0].samples = vec![
            measured_sample(0, 10, 100_000),
            Sample {
                index: 1,
                record: Record::Measured,
                skipped: true,
                wall_ms: None,
                rss_kb: None,
                output_bytes: None,
                exit: 1,
                cache: CacheLabel::SourceWarmProcessCold,
            },
        ];
        report.scenarios[0].statistics = Statistics {
            wall: None,
            rss: None,
        };
        report.failures.push(Failure {
            scenario: "single-attr:ripgrep".to_owned(),
            stage: "eval".to_owned(),
            message: "nix version mismatch".to_owned(),
        });
        report.validate().unwrap();
        let json = serde_json::to_string(&report).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }

    #[test]
    fn schema_uses_camelcase_keys_and_rejects_unknown() {
        let report = fake_report();
        let json = serde_json::to_string(&report).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = value.as_object().unwrap();
        // camelCase top-level keys present.
        assert!(obj.contains_key("schemaVersion"));
        assert!(obj.contains_key("harnessOnly"));
        assert!(obj.contains_key("completeness"));
        // No budget field anywhere in the schema.
        assert!(!obj.contains_key("budget"));
        // Optional nixVersion absent when None.
        assert!(!obj.contains_key("nixVersion"));
        // `pin` carries its own nested nixVersion (the targeted pin).
        assert!(
            obj.get("pin")
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("nixVersion"),
        );
        // Scenario declares `measured`.
        assert!(
            obj.get("scenarios").unwrap().as_array().unwrap()[0]
                .as_object()
                .unwrap()
                .contains_key("measured"),
        );

        // deny_unknown_fields: an extra top-level key is rejected on parse.
        let mut tampered = value.clone();
        tampered
            .as_object_mut()
            .unwrap()
            .insert("__unknown".to_owned(), 9.into());
        let tampered_json = serde_json::to_string(&tampered).unwrap();
        assert!(serde_json::from_str::<Report>(&tampered_json).is_err());
    }

    #[test]
    fn serialization_and_render_are_deterministic() {
        let j1 = serde_json::to_string(&fake_report()).unwrap();
        let j2 = serde_json::to_string(&fake_report()).unwrap();
        assert_eq!(j1, j2);

        let md1 = render_markdown(&fake_report());
        let md2 = render_markdown(&fake_report());
        assert_eq!(md1, md2);

        // Render carries the fixed stat/sample column headers.
        assert!(md1.contains("wall (ms)"));
        assert!(md1.contains("count | min | median | p95 | max"));
        assert!(md1.contains("| index | record | skipped |"));
    }

    // ---- render correctness -------------------------------------------------

    #[test]
    fn render_emits_each_metadata_key_once_and_no_budget() {
        let md = render_markdown(&real_complete_report());
        // pin.rev appears exactly once (no duplicate emission).
        assert_eq!(md.matches("pin.rev").count(), 1);
        // No budget rows are ever rendered.
        assert!(!md.contains("budget"));
        assert!(!md.contains("Ceiling"));
        // SourceWarmProcessCold renders for Real samples; Fixture never does.
        assert!(md.contains("sourceWarmProcessCold"));
        assert!(!md.contains("fixture"));
    }

    #[test]
    fn render_escapes_pipes_and_renders_none_glyph() {
        let mut report = fake_report();
        report.failures = vec![Failure {
            scenario: "idx".to_owned(),
            stage: "eval|meta".to_owned(),
            message: "boom\nline2".to_owned(),
        }];
        report.scenarios[0].samples[0].wall_ms = None;
        let md = render_markdown(&report);
        // Pipe inside a cell is escaped.
        assert!(md.contains("eval\\|meta"));
        // Newline inside a cell is flattened to a space.
        assert!(md.contains("boom line2"));
        // A None wall renders as the em-dash glyph.
        assert!(md.contains("| 0 | measured | false | — |"));
    }

    #[test]
    fn render_escapes_scenario_heading_against_newline_injection() {
        let mut report = fake_report();
        // A malicious scenario name attempts to start a new block + table.
        report.scenarios[0].name =
            "evil\n\n| field | value |\n|---|---|\n| pwned | yes |".to_owned();
        let md = render_markdown(&report);
        // Escaping flattens newlines, so the heading is a SINGLE physical line
        // and the injected payload cannot start a new Markdown block/table.
        let heading_line = md
            .lines()
            .find(|l| l.starts_with("## "))
            .expect("heading present");
        assert!(heading_line.contains("evil"));
        assert!(!heading_line.contains('\n'));
        // The metadata table's separator is the ONLY standalone `|---|---|`
        // line: the injected separator was glued into the heading line, so it
        // never created a second table.
        assert_eq!(
            md.lines().filter(|l| *l == "|---|---|").count(),
            1,
            "no second table separator should appear"
        );
        // Critically, the payload never became a standalone data row.
        assert!(!md.lines().any(|l| l == "| pwned | yes |"));
    }

    #[test]
    fn render_uses_integer_wall_ms() {
        let md = render_markdown(&real_complete_report());
        // Wall values 10/12/16 render as bare integers, not fixed-point floats.
        assert!(md.contains("| 1 | measured | false | 10 |"));
        assert!(md.contains("| 3 | measured | false | 16 |"));
        assert!(!md.contains("10.000"));
    }

    // ---- schema / mode / completeness --------------------------------------

    #[test]
    fn validate_rejects_wrong_schema_version() {
        let mut report = fake_report();
        report.schema_version = 2;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::SchemaVersion {
                found: 2,
                expected: REPORT_SCHEMA_VERSION,
            },
        );
    }

    #[test]
    fn validate_fake_completeness_must_be_fake_only() {
        let mut report = fake_report();
        report.completeness = Completeness::Incomplete;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::FakeCompletenessMustBeFakeOnly {
                found: Completeness::Incomplete,
            },
        );
    }

    #[test]
    fn validate_real_cannot_be_fake_only() {
        let mut report = real_complete_report();
        report.completeness = Completeness::FakeOnly;
        // mode is still Real here; the mode<->completeness guard fires first.
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::RealCannotBeFakeOnly,
        );
    }

    // ---- Fake hard requirements --------------------------------------------

    #[test]
    fn validate_fake_requires_harness_only() {
        let mut report = fake_report();
        report.harness_only = false;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::FakeRequiresHarnessOnly,
        );
    }

    #[test]
    fn validate_fake_requires_no_nix_version() {
        let mut report = fake_report();
        report.nix_version = Some("2.34.8".to_owned());
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::FakeRequiresNoNixVersion,
        );
    }

    #[test]
    fn validate_fake_requires_fixture_labels() {
        let mut report = fake_report();
        report.scenarios[0].samples[0].cache = CacheLabel::SourceWarmProcessCold;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::FakeRequiresFixtureLabels {
                scenario: "single-attr:ripgrep".to_owned(),
                index: 0,
            },
        );
    }

    // ---- Real hard requirements --------------------------------------------

    #[test]
    fn validate_real_requires_harness_only_false() {
        let mut report = real_complete_report();
        report.harness_only = true;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::RealRequiresHarnessOnlyFalse,
        );
    }

    #[test]
    fn validate_real_forbids_fixture_labels() {
        let mut report = real_complete_report();
        report.scenarios[0].samples[0].cache = CacheLabel::Fixture;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::RealForbidsFixtureLabels {
                scenario: "single-attr:ripgrep".to_owned(),
                index: 0,
            },
        );
    }

    // ---- fabrication (universal) -------------------------------------------

    #[test]
    fn validate_rejects_fabricated_sample_in_any_mode() {
        // A Fake report with a skipped sample carrying a value is fabricated.
        let mut report = fake_report();
        report.scenarios[0].samples.push(Sample {
            index: 9,
            record: Record::Measured,
            skipped: true,
            wall_ms: Some(99),
            rss_kb: None,
            output_bytes: None,
            exit: 0,
            cache: CacheLabel::Fixture,
        });
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::FabricatedSample {
                scenario: "single-attr:ripgrep".to_owned(),
                index: 9,
            },
        );
    }

    #[test]
    fn validate_rejects_fabricated_sample_in_incomplete_real() {
        let mut report = real_complete_report();
        report.completeness = Completeness::Incomplete;
        report.scenarios[0].samples.push(Sample {
            index: 9,
            record: Record::Measured,
            skipped: true,
            wall_ms: Some(99),
            rss_kb: None,
            output_bytes: None,
            exit: 1,
            cache: CacheLabel::SourceWarmProcessCold,
        });
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::FabricatedSample {
                scenario: "single-attr:ripgrep".to_owned(),
                index: 9,
            },
        );
    }

    // ---- Complete-Real integrity -------------------------------------------

    #[test]
    fn validate_complete_requires_nix_version_match() {
        let mut report = real_complete_report();
        report.nix_version = Some("2.34.9".to_owned());
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::CompleteRequiresNixVersionMatch {
                got: "2.34.9".to_owned(),
                expected: "2.34.8".to_owned(),
            },
        );
    }

    #[test]
    fn validate_complete_requires_nix_version_present() {
        let mut report = real_complete_report();
        report.nix_version = None;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::CompleteRequiresNixVersionMatch {
                got: String::new(),
                expected: "2.34.8".to_owned(),
            },
        );
    }

    #[test]
    fn validate_complete_requires_no_failures() {
        let mut report = real_complete_report();
        report.failures.push(Failure {
            scenario: "x".to_owned(),
            stage: "eval".to_owned(),
            message: "boom".to_owned(),
        });
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::CompleteRequiresNoFailures,
        );
    }

    #[test]
    fn validate_complete_requires_nonempty_scenarios() {
        let mut report = real_complete_report();
        report.scenarios.clear();
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::CompleteRequiresNonemptyScenarios,
        );
    }

    #[test]
    fn validate_complete_scenario_warmup_count_mismatch() {
        let mut report = real_complete_report();
        report.scenarios[0].warmup = 2; // only one warmup sample present
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::ScenarioWarmupCountMismatch {
                scenario: "single-attr:ripgrep".to_owned(),
                declared: 2,
                found: 1,
            },
        );
    }

    #[test]
    fn validate_complete_scenario_measured_count_mismatch() {
        let mut report = real_complete_report();
        report.scenarios[0].measured = 2; // three measured samples present
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::ScenarioMeasuredCountMismatch {
                scenario: "single-attr:ripgrep".to_owned(),
                declared: 2,
                found: 3,
            },
        );
    }

    #[test]
    fn validate_complete_scenario_measured_zero_rejected() {
        let mut report = real_complete_report();
        // Drop the measured samples so the scenario honestly has only warmup,
        // and declare measured = 0. The warmup count still matches, so the
        // measured-zero guard is what fires.
        report.scenarios[0].measured = 0;
        report.scenarios[0].warmup = 1;
        report.scenarios[0].samples = vec![warmup_sample(0, 9, 99_000)];
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::ScenarioMeasuredCountMismatch {
                scenario: "single-attr:ripgrep".to_owned(),
                declared: 0,
                found: 0,
            },
        );
    }

    #[test]
    fn validate_complete_rejects_incomplete_sample_skipped() {
        let mut report = real_complete_report();
        // A skipped sample with NO values is not fabricated (so it passes the
        // universal fabrication check) but is still incomplete for a Complete
        // run, so the Complete-sample check fires.
        report.scenarios[0].samples[1].skipped = true;
        report.scenarios[0].samples[1].wall_ms = None;
        report.scenarios[0].samples[1].rss_kb = None;
        report.scenarios[0].samples[1].output_bytes = None;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::SampleIncomplete {
                scenario: "single-attr:ripgrep".to_owned(),
                index: 1,
            },
        );
    }

    #[test]
    fn validate_complete_rejects_incomplete_sample_nonzero_exit() {
        let mut report = real_complete_report();
        report.scenarios[0].samples[1].exit = 2;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::SampleIncomplete {
                scenario: "single-attr:ripgrep".to_owned(),
                index: 1,
            },
        );
    }

    #[test]
    fn validate_complete_rejects_incomplete_sample_missing_wall() {
        let mut report = real_complete_report();
        report.scenarios[0].samples[1].wall_ms = None;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::SampleIncomplete {
                scenario: "single-attr:ripgrep".to_owned(),
                index: 1,
            },
        );
    }

    #[test]
    fn validate_complete_rejects_missing_wall_statistics() {
        let mut report = real_complete_report();
        report.scenarios[0].statistics.wall = None;
        assert!(matches!(
            report.validate().unwrap_err(),
            ReportError::StatisticsMissing {
                scenario,
                metric: "wall",
            } if scenario == "single-attr:ripgrep"
        ));
    }

    #[test]
    fn validate_complete_rejects_statistics_count_mismatch() {
        let mut report = real_complete_report();
        // Declare a count that disagrees with the 3 measured samples.
        let mut bad = report.scenarios[0].statistics.wall.unwrap();
        bad.count = 2;
        report.scenarios[0].statistics.wall = Some(bad);
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::StatisticsCountMismatch {
                scenario: "single-attr:ripgrep".to_owned(),
                metric: "wall",
                declared: 2,
                recomputed: 3,
            },
        );
    }

    #[test]
    fn validate_complete_rejects_statistics_value_mismatch() {
        let mut report = real_complete_report();
        // Recomputed median over [10,12,16] is 12; lie about it.
        let mut bad = report.scenarios[0].statistics.wall.unwrap();
        bad.median = 11;
        report.scenarios[0].statistics.wall = Some(bad);
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::StatisticsValueMismatch {
                scenario: "single-attr:ripgrep".to_owned(),
                metric: "wall",
                field: "median",
                declared: 11,
                actual: 12,
            },
        );
    }

    #[test]
    fn validate_complete_rejects_rss_p95_mismatch() {
        let mut report = real_complete_report();
        let mut bad = report.scenarios[0].statistics.rss.unwrap();
        // Recomputed p95 over [100_000, 102_000, 106_000] is the max (n = 3).
        assert_eq!(bad.p95, 106_000);
        bad.p95 += 1;
        report.scenarios[0].statistics.rss = Some(bad);
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::StatisticsValueMismatch {
                scenario: "single-attr:ripgrep".to_owned(),
                metric: "rss",
                field: "p95",
                declared: 106_001,
                actual: 106_000,
            },
        );
    }

    // ---- is_fabricated unit -------------------------------------------------

    #[test]
    fn sample_is_fabricated_only_when_skipped_with_value() {
        // Skipped + value => fabricated.
        assert!(
            Sample {
                index: 0,
                record: Record::Measured,
                skipped: true,
                wall_ms: Some(1),
                rss_kb: None,
                output_bytes: None,
                exit: 0,
                cache: CacheLabel::Fixture,
            }
            .is_fabricated()
        );
        // Skipped + NO value => honest.
        assert!(
            !Sample {
                index: 0,
                record: Record::Measured,
                skipped: true,
                wall_ms: None,
                rss_kb: None,
                output_bytes: None,
                exit: 1,
                cache: CacheLabel::SourceWarmProcessCold,
            }
            .is_fabricated()
        );
        // Measured + value => honest.
        assert!(!measured_sample(0, 5, 1_000).is_fabricated());
    }

    // ---- new honesty invariants (FakeOnly fully exercised / output /
    //      index sequence / record ordering / incomplete-needs-failure) -------

    #[test]
    fn validate_fakeonly_requires_nonempty_scenarios() {
        // A FakeOnly report with NO scenarios is not a fully-exercised harness
        // report; an empty data set must not pass as FakeOnly.
        let mut report = fake_report();
        report.scenarios.clear();
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::FakeOnlyRequiresNonemptyScenarios,
        );
    }

    #[test]
    fn validate_fakeonly_rejects_incomplete_sample_missing_output() {
        // FakeOnly is fully exercised: a sample missing `outputBytes` is
        // incomplete, exactly as it is for Complete Real.
        let mut report = fake_report();
        report.scenarios[0].samples[0].output_bytes = None;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::SampleIncomplete {
                scenario: "single-attr:ripgrep".to_owned(),
                index: 0,
            },
        );
    }

    #[test]
    fn validate_complete_rejects_incomplete_sample_missing_output() {
        let mut report = real_complete_report();
        report.scenarios[0].samples[1].output_bytes = None;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::SampleIncomplete {
                scenario: "single-attr:ripgrep".to_owned(),
                index: 1,
            },
        );
    }

    #[test]
    fn validate_complete_rejects_duplicate_index() {
        // indices become [0, 1, 1, 3]: at position 2 the index is 1, not 2.
        let mut report = real_complete_report();
        report.scenarios[0].samples[2].index = 1;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::ScenarioIndexNotContiguous {
                scenario: "single-attr:ripgrep".to_owned(),
                expected: 2,
                found: 1,
            },
        );
    }

    #[test]
    fn validate_complete_rejects_gapped_index() {
        // indices become [0, 5, 2, 3]: at position 1 the index is 5, not 1.
        let mut report = real_complete_report();
        report.scenarios[0].samples[1].index = 5;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::ScenarioIndexNotContiguous {
                scenario: "single-attr:ripgrep".to_owned(),
                expected: 1,
                found: 5,
            },
        );
    }

    #[test]
    fn validate_complete_rejects_out_of_order_index() {
        // indices become [0, 2, 1, 3]: at position 1 the index is 2, not 1.
        let mut report = real_complete_report();
        report.scenarios[0].samples[1].index = 2;
        report.scenarios[0].samples[2].index = 1;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::ScenarioIndexNotContiguous {
                scenario: "single-attr:ripgrep".to_owned(),
                expected: 1,
                found: 2,
            },
        );
    }

    #[test]
    fn validate_complete_rejects_warmup_after_measured() {
        // Swap the records of the first two samples (counts stay 1 warmup + 3
        // measured, indices stay 0..3, all samples complete), so the ONLY fault
        // is a warmup record appearing after a measured record.
        let mut report = real_complete_report();
        report.scenarios[0].samples[0].record = Record::Measured;
        report.scenarios[0].samples[1].record = Record::Warmup;
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::ScenarioRecordOrder {
                scenario: "single-attr:ripgrep".to_owned(),
                index: 1,
            },
        );
    }

    #[test]
    fn validate_incomplete_requires_at_least_one_failure() {
        // An Incomplete Real report with no recorded failure cannot explain its
        // own incompleteness, so an empty data set cannot masquerade here.
        let mut report = real_complete_report();
        report.completeness = Completeness::Incomplete;
        assert!(report.failures.is_empty());
        assert_eq!(
            report.validate().unwrap_err(),
            ReportError::IncompleteRequiresFailure,
        );
    }

    #[test]
    fn render_emits_host_machine() {
        let md = render_markdown(&real_complete_report());
        // host.machine is rendered exactly once, between system and cores.
        assert!(md.contains("| host.system | x86_64-linux |"));
        assert!(md.contains("| host.machine | bench-host |"));
        assert!(md.contains("| host.cores | 8 |"));
        assert_eq!(md.matches("host.machine").count(), 1);
    }
}
