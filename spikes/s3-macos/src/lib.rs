// Spike S3 (PR-7 / DR-003) — macOS binary coverage + signing/notarization
// EVIDENCE-MODEL slice (dependency-safe: data contract only).
//
// STANDALONE Cargo workspace (`[workspace]` in `Cargo.toml`), deliberately NOT
// part of the production workspace at the repo root. `publish = false`, NO
// `license` field, NO SPDX headers (DR-015). It is an evidence/data-contract
// harness, not production pkg code. Unsafe code is forbidden everywhere in this
// crate.
//
// This first slice owns ONLY:
//   * the immutable, strictly-validated pin summary (pinned Nix 2.34.8 + pinned
//     Nixpkgs rev/narHash, scoped to the two Darwin systems and three fixture
//     attributes, plus the single v1 cache store URL);
//   * a compact, closed evidence model: five lanes keyed by
//     `Mode={Fake,Detect,Preflight,BuildProbe,SignPlan}`, each a generic
//     `Lane<T>` in one of `LaneState={Pending,Incomplete,Complete}`; a closed
//     `PendingReason` set; a `Failure` carrying enum `stage`/`kind` only (never
//     raw child output/path/message); and an explicit `EvidenceSource` on every
//     observation so synthetic Fixture data can never be accepted as Real
//     evidence;
//   * a `Report` that is deterministic in both pretty-JSON and escaped-Markdown
//     form and carries NO hostname, user name, credential/profile value,
//     free-form diagnostic, or timestamp.
//
// It never signs/notarizes, never performs a package build or profile
// activation, and never does a shell/PATH lookup. The modes differ in what they
// DO touch: Fake mode never executes Nix, never reaches the network, and never
// accesses the keychain; Detect mode runs only the read-only
// `/usr/bin/security find-identity` host probe (reading identity metadata/counts
// from the default keychain — never credentials, never unlock/sign/notarize,
// never keychain writes); Preflight mode executes the caller-supplied
// absolute Nix binary (its exact Nix 2.34.8 version verified at runtime) for
// build-free probes only: `nix flake prefetch` fetches the pinned GitHub
// flake/source, while `nix store info`/`nix path-info` availability queries
// target cache.nixos.org. Preflight is build-free and activation-free,
// but NOT read-only or mutation-free: `nix flake prefetch` may download/add the
// pinned source to the Nix store/fetch cache and evaluation may populate
// ordinary Nix-managed state. Unit and Fake tests never access the keychain,
// and the repo-root validation lanes never run a live Detect.

#![forbid(unsafe_code)]

pub mod cli;
pub mod command;
pub mod detect;
pub mod manifest;
pub mod preflight;
pub mod report;
pub mod runner;
pub mod validate;

pub use cli::{Action, CliError, RunArgs, RunMode, USAGE, parse as parse_cli};
pub use command::{
    CommandError, CommandOutcome, CommandRunner, CommandSpec, ProbeStatus, RealRunner, SpecError,
    StreamCapture, run,
};
pub use detect::{BoundedProbeRunner, DetectOutcome, ProbeRunner, detect};
pub use manifest::{ManifestError, PinSummary, pin_summary};
pub use preflight::{
    CacheMiss, ContractError, ParseError, PathInfoHit, PathInfoProbe, PrefetchVerified,
    PreflightBuildError, StorePath, classify_cache_miss, derivation_spec, output_path_spec,
    parse_derivation, parse_path_info, parse_prefetch, parse_version, prefetch_spec,
    recursive_path_spec, store_info_spec, version_spec,
};
pub use report::{
    BuildProbeObservation, CoverageEntry, DetectObservation, EvidenceSource, Failure, FailureKind,
    FakeObservation, Lane, LaneError, LaneState, Lanes, Mode, PendingReason, PreflightObservation,
    Report, ReportError, SignPlanObservation, SignTarget, Stage, ToolCapabilities, XcodeSelection,
    render_json, render_markdown,
};
pub use runner::{detect_report, fake_report, preflight_report, sign_plan_report};
pub use validate::{ATTRS, CACHE_STORE_URL, DARWIN_SYSTEMS, PinError, validate_pin};
