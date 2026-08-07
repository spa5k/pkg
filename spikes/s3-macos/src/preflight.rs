//! Spike S3 (PR-7) — PREFLIGHT slice: the bounded command builders + pure
//! fail-closed parsers for the cache-coverage probes, NO orchestration.
//!
//! This slice owns ONLY:
//!   * the exact [`crate::command::CommandSpec`] builders for the five fixed
//!     Nix cache-coverage probes Preflight runs (`nix --version`, `nix flake
//!     prefetch`, `nix store info`, `nix derivation show`, `nix path-info` with
//!     and without `--recursive`);
//!   * the pure, bounded, fail-closed parsers/classifier for each probe's
//!     output: exact version line, prefetch NAR-hash + source-path verification,
//!     derivation-show v4 (one derivation, canonical system, input-addressed
//!     output path), path-info v2 (canonical storeDir + queried entry), and the
//!     nonzero-exit cache-miss classifier.
//!
//! It deliberately does NOT add the Preflight orchestration/state machine, the
//! runner report wiring, the CLI, or `main` integration: those live in
//! [`crate::runner`], [`crate::cli`], and the `s3-probe` binary (`src/main.rs`).
//! There is NO process spawn, NO network, NO Nix invocation, and NO `/nix`
//! mutation HERE — every parser is pure and unit-tested with constructed bytes
//! only. (The probes these builders DESCRIBE are build-free and activation-free,
//! but NOT read-only or mutation-free: a [`crate::command::RealRunner`] caller
//! that executes them may add the pinned source to the Nix store/fetch cache and
//! populate ordinary Nix-managed state. That execution lives in
//! [`crate::runner`], not here.)
//!
//! # Contract (frozen)
//! * [`PINNED_REF`] =
//!   `github:NixOS/nixpkgs/<rev>?narHash=<manifest narHash>` — the descriptor's
//!   exact rev PLUS the manifest NAR hash on every Nixpkgs reference (a test
//!   pins it to BOTH manifest constants to prevent a rev-only regression).
//! * cache URL = [`crate::validate::CACHE_STORE_URL`] (`https://cache.nixos.org/`).
//! * the caller supplies an ABSOLUTE `nix_bin`; [`crate::command::CommandSpec`]
//!   construction rejects a non-absolute/empty program via existing validation
//!   (surfaced here as [`PreflightBuildError::Spec`]).
//! * the global feature argv is EXACTLY `["--extra-experimental-features",
//!   "nix-command flakes"]` and precedes every subcommand EXCEPT `--version`.
//! * `--json-format` is emitted ONLY for the path-info probes (always `2`); it
//!   is NEVER emitted for flake prefetch or derivation show. No
//!   build/offline/out-link/substituter flag is ever emitted. Systems/attrs come
//!   ONLY from [`crate::validate::DARWIN_SYSTEMS`]/[`crate::validate::ATTRS`].
//! * FIXED global eval-hardening options use the universal stable
//!   `--option <name> <value>` form (NEVER the non-contract `--flag=false`
//!   single token): `--option accept-flake-config false` on BOTH prefetch and
//!   derivation-show, and `--option allow-import-from-derivation false` on
//!   derivation-show ONLY (prefetch evaluates a flake, not a derivation, so
//!   IFD does not apply). These fixed global option triplets are placed
//!   IMMEDIATELY after FEATURE_ARGS and before the subcommand tokens, so exact
//!   argv is deterministic. They are constants, never caller-controlled. No
//!   `restrict-eval`/`allowed-uris` is emitted in this slice.
//! * per-command caps/timeouts are bounded and `<=` [`crate::command::MAX_TIMEOUT`]
//!   (180 s); tests freeze them.
//!
//! # Bounded/redacted model
//! [`ParseError`] / [`ContractError`] / [`PreflightBuildError`] `Display` NEVER
//! echoes raw input, a store path, a hash, or a Nix diagnostic. [`StorePath`]
//! is an INTERNAL validated type: it can render `/nix/store/<base>` for the NEXT
//! command via [`StorePath::render`], but its `Display`/`Debug` are REDACTED and
//! it is deliberately NOT `Serialize`/`Deserialize`, so it can never leak into a
//! [`crate::report::Report`] or an error string. `#![forbid(unsafe_code)]`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::num::NonZeroU64;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::command::{CommandSpec, ProbeStatus, SpecError};
use crate::validate;

/// The pinned flake reference every Preflight probe targets: the descriptor's
/// exact rev PLUS the manifest NAR hash, exactly
/// `github:NixOS/nixpkgs/<rev>?narHash=<manifest narHash>` (consumed verbatim by
/// BOTH prefetch and derivation-show). A test pins it to BOTH manifest
/// constants to prevent a rev-only regression.
pub const PINNED_REF: &str = "github:NixOS/nixpkgs/a62e6edd6d5e1fa0329b8653c801147986f8d446?narHash=sha256-oamiKNfr2MS6yH64rUn99mIZjc45nGJlj9eGth/3Xuw=";

/// The EXACT global feature argv prepended before every subcommand except
/// `--version`.
const FEATURE_ARGS: [&str; 2] = ["--extra-experimental-features", "nix-command flakes"];

/// Fixed global eval-hardening option triplet: refuse to accept flake config
/// from upstream. Always emitted as the EXACT argv triplet
/// `--option accept-flake-config false` — the universal stable Nix form
/// generated by pinned Nix 2.34.8 `src/libutil/configuration.cc`, NOT the
/// non-contract `--accept-flake-config=false` single token. Deterministic,
/// never caller-controlled. Applied to BOTH prefetch and derivation-show.
const ACCEPT_FLAKE_CONFIG_OPTS: [&str; 3] = ["--option", "accept-flake-config", "false"];
/// Fixed global eval-hardening option triplet: forbid import-from-derivation
/// during evaluation (Preflight is build-free). Always emitted as the EXACT
/// argv triplet `--option allow-import-from-derivation false` (the universal
/// stable Nix form, NOT the non-contract `--allow-import-from-derivation=false`
/// single token). Deterministic, never caller-controlled. Applied to
/// derivation-show ONLY.
const NO_IFD_OPTS: [&str; 3] = ["--option", "allow-import-from-derivation", "false"];

/// The canonical Nix store prefix.
const STORE_PREFIX: &str = "/nix/store/";

/// Required top-level `version` for a derivation-show JSON document.
pub const DERIVATION_VERSION: u64 = 4;
/// Required top-level `version` for a path-info v2 JSON document.
pub const PATH_INFO_VERSION: u64 = 2;

/// The nix base-32 alphabet (digits + the lowercase letters nix admits: it
/// excludes `e`, `o`, `t`, `u`). A store-path hash is exactly 32 of these.
const STORE_BASE32_ALPHABET: &[u8] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Nix `StorePath::MaxPathLen`: the maximum byte length of the NAME portion of
/// a store path (everything after the `<32-char hash>-` prefix). Mirrors the
/// pinned Nix 2.34.8 `src/libstore/include/nix/store/path.hh` constant.
const STORE_PATH_MAX_NAME_LEN: usize = 211;

// ---- frozen per-command caps/timeouts (all <= MAX_TIMEOUT = 180 s) ----------

/// Retained stdout cap for `nix --version` (the line is ~17 bytes).
pub const VERSION_STDOUT_CAP: u64 = 4 * 1024;
/// Retained stdout cap for `nix flake prefetch --json` (small JSON).
pub const PREFETCH_STDOUT_CAP: u64 = 64 * 1024;
/// Retained stdout cap for `nix store info`: bounds retained
/// output/diagnostics even though success output is not parsed.
pub const STORE_INFO_STDOUT_CAP: u64 = 64 * 1024;
/// Retained stdout cap for `nix derivation show --json` (can be largish).
pub const DERIVATION_STDOUT_CAP: u64 = 256 * 1024;
/// Retained stdout cap for a nonrecursive `nix path-info --json` (small JSON).
pub const PATH_INFO_STDOUT_CAP: u64 = 64 * 1024;
/// Retained stdout cap for a recursive `nix path-info --json` (closure).
pub const RECURSIVE_STDOUT_CAP: u64 = 1024 * 1024;
/// Retained stderr cap shared by every Preflight probe (diagnostics only).
pub const PREFLIGHT_STDERR_CAP: u64 = 4 * 1024;

/// Wall-clock timeout for `nix --version`.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
/// Wall-clock timeout for `nix flake prefetch` (network + evaluation; cold
/// caches can take minutes, so this sits at the [`crate::command::MAX_TIMEOUT`]
/// ceiling).
pub const PREFETCH_TIMEOUT: Duration = crate::command::MAX_TIMEOUT;
/// Wall-clock timeout for `nix store info` (network).
pub const STORE_INFO_TIMEOUT: Duration = Duration::from_secs(30);
/// Wall-clock timeout for `nix derivation show` (evaluation; cold caches can
/// take minutes, so this sits at the [`crate::command::MAX_TIMEOUT`] ceiling).
pub const DERIVATION_TIMEOUT: Duration = crate::command::MAX_TIMEOUT;
/// Wall-clock timeout for a nonrecursive `nix path-info` (network).
pub const PATH_INFO_TIMEOUT: Duration = Duration::from_secs(30);
/// Wall-clock timeout for a recursive `nix path-info` (network closure query).
pub const RECURSIVE_TIMEOUT: Duration = Duration::from_secs(60);

// ===========================================================================
// Command builders
// ===========================================================================

/// A bounded, fail-closed failure while constructing a Preflight
/// [`CommandSpec`]: either the `nix_bin` path failed existing
/// [`CommandSpec`] validation (non-absolute/empty), or a requested system/attr
/// was not in the manifest canonical allowlist. `Display` never echoes an
/// unbounded value; the [`Spec`](Self::Spec) variant reuses the already-bounded
/// [`SpecError`] display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreflightBuildError {
    /// The `nix_bin` path failed [`CommandSpec`] validation.
    Spec(SpecError),
    /// The requested `system` is not one of [`validate::DARWIN_SYSTEMS`].
    SystemNotCanonical,
    /// The requested `attr` is not one of [`validate::ATTRS`].
    AttrNotCanonical,
}

impl fmt::Display for PreflightBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spec(e) => fmt::Display::fmt(e, f),
            Self::SystemNotCanonical => {
                f.write_str("preflight: system must be a canonical Darwin system")
            }
            Self::AttrNotCanonical => {
                f.write_str("preflight: attr must be a canonical fixture attribute")
            }
        }
    }
}

impl std::error::Error for PreflightBuildError {}

impl From<SpecError> for PreflightBuildError {
    fn from(e: SpecError) -> Self {
        Self::Spec(e)
    }
}

/// Build a validated [`CommandSpec`] for `nix_bin` with the given argv and
/// frozen caps/timeout. `nix_bin` is validated as absolute/non-empty by
/// [`CommandSpec::new`]; any failure becomes [`PreflightBuildError::Spec`].
fn build(
    nix_bin: &Path,
    argv: Vec<OsString>,
    stdout_cap: u64,
    timeout: Duration,
) -> Result<CommandSpec, PreflightBuildError> {
    CommandSpec::new(
        nix_bin.to_path_buf(),
        argv,
        nz(stdout_cap),
        nz(PREFLIGHT_STDERR_CAP),
        timeout,
    )
    .map_err(PreflightBuildError::Spec)
}

/// A nonzero [`NonZeroU64`] cap (panics only on a programmer error: a zero
/// frozen constant).
fn nz(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).expect("preflight cap must be nonzero")
}

/// Fresh feature-arg vector (the global `--extra-experimental-features
/// nix-command flakes` prefix).
fn feature_args() -> Vec<OsString> {
    FEATURE_ARGS.iter().copied().map(OsString::from).collect()
}

/// `nix --version` — argv is EXACTLY `["--version"]` with NO feature args.
pub fn version_spec(nix_bin: &Path) -> Result<CommandSpec, PreflightBuildError> {
    build(
        nix_bin,
        vec![OsString::from("--version")],
        VERSION_STDOUT_CAP,
        VERSION_TIMEOUT,
    )
}

/// `nix flake prefetch --json … <PINNED_REF>`. Feature args precede the
/// subcommand; NEVER emits `--json-format`. The FIXED global
/// `--option accept-flake-config false` eval-hardening triplet is placed
/// immediately after FEATURE_ARGS and before the `flake prefetch` subcommand.
pub fn prefetch_spec(nix_bin: &Path) -> Result<CommandSpec, PreflightBuildError> {
    let mut argv = feature_args();
    argv.extend(ACCEPT_FLAKE_CONFIG_OPTS.iter().copied().map(OsString::from));
    argv.extend(
        [
            "flake",
            "prefetch",
            "--json",
            "--no-pretty",
            "--no-write-lock-file",
            "--no-use-registries",
            PINNED_REF,
        ]
        .iter()
        .copied()
        .map(OsString::from),
    );
    build(nix_bin, argv, PREFETCH_STDOUT_CAP, PREFETCH_TIMEOUT)
}

/// `nix store info --store <CACHE_URL>`. Feature args precede the subcommand.
pub fn store_info_spec(nix_bin: &Path) -> Result<CommandSpec, PreflightBuildError> {
    let mut argv = feature_args();
    argv.extend(
        ["store", "info", "--store", validate::CACHE_STORE_URL]
            .iter()
            .copied()
            .map(OsString::from),
    );
    build(nix_bin, argv, STORE_INFO_STDOUT_CAP, STORE_INFO_TIMEOUT)
}

/// `nix derivation show … <PINNED_REF>#legacyPackages.<system>.<attr>^out`.
/// Feature args precede the subcommand; NEVER emits `--json-format`. The FIXED
/// global `--option accept-flake-config false` AND
/// `--option allow-import-from-derivation false` eval-hardening triplets are
/// placed immediately after FEATURE_ARGS and before the `derivation show`
/// subcommand (Preflight is build-free, so IFD is disabled). `system` and
/// `attr` MUST come from the manifest canonical allowlists
/// ([`validate::DARWIN_SYSTEMS`] / [`validate::ATTRS`]); any other value is
/// rejected before any argv is built.
pub fn derivation_spec(
    nix_bin: &Path,
    system: &str,
    attr: &str,
) -> Result<CommandSpec, PreflightBuildError> {
    if !validate::DARWIN_SYSTEMS.contains(&system) {
        return Err(PreflightBuildError::SystemNotCanonical);
    }
    if !validate::ATTRS.contains(&attr) {
        return Err(PreflightBuildError::AttrNotCanonical);
    }
    let flake_ref = format!("{PINNED_REF}#legacyPackages.{system}.{attr}^out");
    let mut argv = feature_args();
    argv.extend(ACCEPT_FLAKE_CONFIG_OPTS.iter().copied().map(OsString::from));
    argv.extend(NO_IFD_OPTS.iter().copied().map(OsString::from));
    argv.extend(
        [
            "derivation",
            "show",
            "--no-pretty",
            "--no-write-lock-file",
            "--no-use-registries",
        ]
        .iter()
        .copied()
        .map(OsString::from),
    );
    argv.push(OsString::from(flake_ref));
    build(nix_bin, argv, DERIVATION_STDOUT_CAP, DERIVATION_TIMEOUT)
}

/// `nix path-info --json --json-format 2 … --store <CACHE_URL> <ABS_OUTPUT>` —
/// the NONRECURSIVE output-path query. Feature args precede the subcommand;
/// `--json-format 2` is emitted here. `output` is the validated
/// input-addressed store path rendered for the query.
pub fn output_path_spec(
    nix_bin: &Path,
    output: &StorePath,
) -> Result<CommandSpec, PreflightBuildError> {
    let abs = output.render();
    let mut argv = feature_args();
    argv.extend(
        [
            "path-info",
            "--json",
            "--json-format",
            "2",
            "--no-pretty",
            "--store",
            validate::CACHE_STORE_URL,
        ]
        .iter()
        .copied()
        .map(OsString::from),
    );
    argv.push(OsString::from(abs));
    build(nix_bin, argv, PATH_INFO_STDOUT_CAP, PATH_INFO_TIMEOUT)
}

/// The RECURSIVE path-info query: identical deterministic ordering to
/// [`output_path_spec`] with `--recursive` inserted immediately BEFORE the store
/// path (the positional `/nix/store/<base>` argument).
pub fn recursive_path_spec(
    nix_bin: &Path,
    output: &StorePath,
) -> Result<CommandSpec, PreflightBuildError> {
    let abs = output.render();
    let mut argv = feature_args();
    argv.extend(
        [
            "path-info",
            "--json",
            "--json-format",
            "2",
            "--no-pretty",
            "--store",
            validate::CACHE_STORE_URL,
        ]
        .iter()
        .copied()
        .map(OsString::from),
    );
    argv.push(OsString::from("--recursive"));
    argv.push(OsString::from(abs));
    build(nix_bin, argv, RECURSIVE_STDOUT_CAP, RECURSIVE_TIMEOUT)
}

// ===========================================================================
// StorePath — internal validated type
// ===========================================================================

/// An internally-validated Nix store path: a 32-char nix-base32 hash followed by
/// `-` and a nonempty name, matching `^[0123456789abcdfghijklmnpqrsvwxyz]{32}-.+$`
/// as a single `/nix/store/` component.
///
/// It is constructed ONLY by this module's parsers (after validation) and held
/// only long enough to render `/nix/store/<base>` for the NEXT Preflight command.
/// It is NEVER `Serialize`/`Deserialize` (so it can never enter a
/// [`crate::report::Report`]), and its `Display`/`Debug` are REDACTED so a stray
/// `format!("{path:?}")` in a log or error can never leak the actual path. The
/// type is `pub` only so it can flow through the module's public parser
/// signatures; its constructors/inspectors are crate-private.
#[derive(Clone, PartialEq, Eq)]
pub struct StorePath {
    /// The validated base (the single component after `/nix/store/`).
    base: String,
}

impl StorePath {
    /// Construct from a base the CALLER has already validated against the store
    /// regex. Private: only [`parse_derivation`] builds a `StorePath`.
    fn from_validated_base(base: String) -> Self {
        Self { base }
    }

    /// Render the full absolute `/nix/store/<base>` path. CRATE-INTERNAL: used
    /// only to build the next Preflight command's argv; never appears in
    /// `Display`/`Debug` or any report.
    pub(crate) fn render(&self) -> String {
        format!("{STORE_PREFIX}{}", self.base)
    }

    /// The validated base (the component after `/nix/store/`), for internal
    /// matching by the path-info/miss parsers. CRATE-INTERNAL.
    pub(crate) fn base(&self) -> &str {
        &self.base
    }
}

impl fmt::Debug for StorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // REDACTED: never echoes the hash/name, even in a debug dump.
        f.write_str("StorePath(<redacted>)")
    }
}

impl fmt::Display for StorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // REDACTED: never echoes the actual path.
        f.write_str("<redacted store path>")
    }
}

// ===========================================================================
// Closed success types
// ===========================================================================

/// Closed success marker for a verified flake prefetch (NAR hash matched the
/// manifest and the source path was a valid `/nix/store/<base>`). Carries NO raw
/// hash or path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrefetchVerified;

/// Closed success marker for a path-info query whose `info` object contained the
/// queried store path. Carries NO path or count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathInfoHit;

/// Closed success marker for a classified cache miss (a nonzero exit whose
/// stderr was the exact single-line "not valid" shape). Carries NO path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheMiss;

/// The closed result of parsing a `nix path-info --json --json-format 2`
/// document: either a cache HIT (the queried path — and, for a recursive query,
/// every sibling — carried a structurally valid v2 entry), or a cache MISS (a
/// ZERO-EXIT document whose `info` object mapped the queried path, or any
/// recursive sibling, to `null` — Nix's `--json-format 2` encoding for an
/// invalid/unavailable path). Carries NO path, hash, or count; the payload is
/// only the closed [`PathInfoHit`]/[`CacheMiss`] marker. Bounded/redacted
/// `Debug`+`Display`: never echoes a store path, hash, or raw input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathInfoProbe {
    /// The queried path (and every recursive sibling) carried a valid v2 entry.
    Hit(PathInfoHit),
    /// A zero-exit document mapped the queried path — or, recursively, ANY
    /// sibling — to `null`.
    Miss(CacheMiss),
}

impl fmt::Display for PathInfoProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hit(_) => f.write_str("preflight: path-info cache hit"),
            Self::Miss(_) => f.write_str("preflight: path-info cache miss"),
        }
    }
}

// ===========================================================================
// ParseError / ContractError
// ===========================================================================

/// A bounded, fail-closed failure while parsing a SUCCESS-PATH probe's stdout
/// (a zero-exit output). Covers structural/JSON, version/schema, and semantic
/// contract failures (hash, system, store-path, storeDir, info) discovered
/// while turning captured bytes into a validated value. `Display` NEVER echoes
/// raw input, a store path, or a hash; it carries only closed tags/numbers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The output was not valid UTF-8 (text parser path).
    NonUtf8,
    /// The version output text did not match the exact pinned version line.
    VersionText,
    /// The output was not the expected JSON shape (invalid JSON / wrong types).
    MalformedJson,
    /// A required JSON field was absent.
    MissingField,
    /// The required top-level `version` field was absent.
    MissingVersion,
    /// A top-level JSON `version` did not equal the expected one.
    VersionField {
        /// The version found in the document.
        got: u64,
        /// The required version.
        expected: u64,
    },
    /// The structure had the wrong cardinality (e.g. not exactly one derivation).
    Cardinality,
    /// The prefetch `hash` key did not equal the pinned manifest NAR hash.
    PrefetchHashMismatch,
    /// The prefetch `storePath` was not a valid `/nix/store/<base>` source path.
    PrefetchSourcePathInvalid,
    /// The derivation object was missing its inner `version` (or it was not a
    /// usable `u64`).
    DerivationVersionMissing,
    /// The derivation object's inner `version` did not equal 4.
    DerivationVersion { got: u64 },
    /// The derivation object was missing its `name`, or it was empty.
    DerivationName,
    /// The derivation's `system` did not equal the requested canonical Darwin
    /// system.
    DerivationSystemMismatch,
    /// The derivation had no `out` output (or it had no `path`).
    DerivationOutputMissing,
    /// A store path was not a valid input-addressed `/nix/store/<base>` path.
    StorePathInvalid,
    /// The path-info `storeDir` did not equal `/nix/store`.
    StoreDir,
    /// The path-info `info` object did not contain the queried store path.
    PathInfoQueriedAbsent,
    /// A NONRECURSIVE path-info carried an entry besides the queried one.
    PathInfoExtraEntry,
    /// A path-info v2 `info` entry value was a non-null JSON primitive/array.
    PathInfoEntryNotObject,
    /// A path-info v2 `info` entry object was missing its inner `version`, or it
    /// was not a usable `u64`.
    PathInfoEntryVersionMissing,
    /// A path-info v2 `info` entry object's inner `version` did not equal 2.
    PathInfoEntryVersion { got: u64 },
    /// A path-info v2 `info` entry object's inner `storeDir` was missing or was
    /// not `/nix/store`.
    PathInfoEntryStoreDir,
    /// A path-info v2 `info` entry object was missing a required field
    /// (`narHash`/`narSize`/`references`/`ca`).
    PathInfoEntryMissingField,
    /// A path-info v2 `info` entry object's required field had the wrong JSON
    /// type.
    PathInfoEntryFieldType,
    /// A path-info v2 `info` entry object's `references` array had a malformed
    /// or absolute store-path entry.
    PathInfoEntryReference,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonUtf8 => f.write_str("preflight: output was not valid UTF-8"),
            Self::VersionText => {
                f.write_str("preflight: version output did not match the exact pinned version line")
            }
            Self::MalformedJson => f.write_str("preflight: output was not the expected JSON shape"),
            Self::MissingField => f.write_str("preflight: a required JSON field was absent"),
            Self::MissingVersion => {
                f.write_str("preflight: the top-level version field was absent")
            }
            Self::VersionField { got, expected } => {
                write!(f, "preflight: JSON version must be {expected}, got {got}")
            }
            Self::Cardinality => f.write_str("preflight: expected exactly one derivation"),
            Self::PrefetchHashMismatch => {
                f.write_str("preflight: prefetch hash did not match the pinned manifest NAR hash")
            }
            Self::PrefetchSourcePathInvalid => {
                f.write_str("preflight: prefetch storePath was not a valid /nix/store source path")
            }
            Self::DerivationVersionMissing => {
                f.write_str("preflight: derivation object was missing its inner version")
            }
            Self::DerivationVersion { got } => write!(
                f,
                "preflight: derivation inner version must be {DERIVATION_VERSION}, got {got}"
            ),
            Self::DerivationName => {
                f.write_str("preflight: derivation object was missing a nonempty name")
            }
            Self::DerivationSystemMismatch => f.write_str(
                "preflight: derivation system did not match the requested canonical Darwin system",
            ),
            Self::DerivationOutputMissing => {
                f.write_str("preflight: derivation had no 'out' output")
            }
            Self::StorePathInvalid => {
                f.write_str("preflight: store path was not a valid input-addressed /nix/store path")
            }
            Self::StoreDir => f.write_str("preflight: path-info storeDir must be /nix/store"),
            Self::PathInfoQueriedAbsent => {
                f.write_str("preflight: path-info did not contain the queried store path")
            }
            Self::PathInfoExtraEntry => f.write_str(
                "preflight: nonrecursive path-info carried an entry besides the queried one",
            ),
            Self::PathInfoEntryNotObject => {
                f.write_str("preflight: path-info v2 info entry was a non-null primitive")
            }
            Self::PathInfoEntryVersionMissing => {
                f.write_str("preflight: path-info v2 info entry was missing its inner version")
            }
            Self::PathInfoEntryVersion { got } => write!(
                f,
                "preflight: path-info v2 info entry version must be {PATH_INFO_VERSION}, got {got}"
            ),
            Self::PathInfoEntryStoreDir => {
                f.write_str("preflight: path-info v2 info entry storeDir must be /nix/store")
            }
            Self::PathInfoEntryMissingField => {
                f.write_str("preflight: path-info v2 info entry was missing a required field")
            }
            Self::PathInfoEntryFieldType => {
                f.write_str("preflight: path-info v2 info entry had a field of the wrong type")
            }
            Self::PathInfoEntryReference => f.write_str(
                "preflight: path-info v2 info entry had a malformed or absolute reference",
            ),
        }
    }
}

impl std::error::Error for ParseError {}

/// A bounded, fail-closed failure while CLASSIFYING a NONZERO-exit probe's
/// retained stderr as a clean cache miss. A clean miss requires a nonzero exit
/// AND the exact single-line `path '/nix/store/<base>' is not valid` shape; any
/// signal, zero exit, extra line, prose, invalid/non-matching path, or non-UTF8
/// is a [`ContractError`]. `Display` NEVER echoes the raw diagnostic or any
/// store path; it carries only closed tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// The outcome exited zero (a miss requires a nonzero exit).
    ZeroExit,
    /// The outcome was terminated by a signal (a miss requires a nonzero exit).
    Signaled,
    /// The retained stderr was not valid UTF-8.
    NonUtf8,
    /// The stderr was not the exact single-line miss envelope (extra lines /
    /// prose / missing quotes).
    MissShape,
    /// The miss line's store path was syntactically invalid.
    MissPathInvalid,
    /// The miss line's store path did not equal the exact queried output path
    /// (nonrecursive output query only).
    MissPathMismatch,
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroExit => {
                f.write_str("preflight: a cache miss requires a nonzero exit, got zero")
            }
            Self::Signaled => {
                f.write_str("preflight: a cache miss requires a nonzero exit, got a signal")
            }
            Self::NonUtf8 => f.write_str("preflight: miss diagnostic was not valid UTF-8"),
            Self::MissShape => f.write_str(
                "preflight: nonzero-exit stderr was not the exact single-line miss shape",
            ),
            Self::MissPathInvalid => {
                f.write_str("preflight: miss line store path was syntactically invalid")
            }
            Self::MissPathMismatch => {
                f.write_str("preflight: miss line store path did not equal the queried output path")
            }
        }
    }
}

impl std::error::Error for ContractError {}

// ===========================================================================
// Pure parsers / classifier
// ===========================================================================

/// `true` iff `base` is a single `/nix/store/` component that mirrors the
/// pinned Nix 2.34.8 `StorePath::checkName` grammar exactly:
///   * exactly 32 nix-base32 hash chars, then `-`;
///   * a nonempty NAME whose byte length is <= [`STORE_PATH_MAX_NAME_LEN`]
///     (211);
///   * every name byte is an ASCII digit/letter or one of `+ - . _ ? =`;
///   * the name is not exactly `.` or `..`;
///   * the name's first dash-separated component is not `.` or `..`
///     (equivalently, the name does not start with `.-` or `..-`).
///
/// All checks are byte-level (no regex, no allocation); the function never
/// echoes or returns the raw input value.
fn is_valid_store_base(base: &str) -> bool {
    let b = base.as_bytes();
    // Need at least 32 hash bytes + 1 hyphen + 1 name byte.
    if b.len() < 34 {
        return false;
    }
    if b[32] != b'-' {
        return false;
    }
    if !b[..32].iter().all(|&c| STORE_BASE32_ALPHABET.contains(&c)) {
        return false;
    }
    let name = &b[33..];
    // name.len() >= 1 here (b.len() >= 34 guarantees a nonempty name).
    if name.len() > STORE_PATH_MAX_NAME_LEN {
        return false;
    }
    if !name.iter().all(|&c| is_valid_store_name_byte(c)) {
        return false;
    }
    // Reject `.` and `..` as the whole name.
    if name == b"." || name == b".." {
        return false;
    }
    // Reject a first dash-separated component of `.` or `..`: equivalently a
    // name starting `.-` or `..-`.
    if name.starts_with(b".-") || name.starts_with(b"..-") {
        return false;
    }
    true
}

/// `true` iff `c` is an admitted Nix store-path NAME byte: an ASCII digit or
/// letter, or one of `+ - . _ ? =`. Mirrors the pinned Nix 2.34.8 `validNameChar`
/// admission set; byte-level, no lookup table or regex.
fn is_valid_store_name_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
}

/// Strip the validated `<base>` off `/nix/store/<base>`; `None` if the prefix
/// is absent (the caller then treats the path as invalid).
fn store_base_of(path: &str) -> Option<&str> {
    path.strip_prefix(STORE_PREFIX)
}

/// Map any [`serde_json::Error`] to [`ParseError::MalformedJson`], discarding
/// the (input-echoing) serde message.
fn map_json_err(_: serde_json::Error) -> ParseError {
    ParseError::MalformedJson
}

/// Parse the EXACT `nix --version` line: the bytes must equal
/// `nix (Nix) <NIX_VERSION>` with an OPTIONAL single trailing `\n` and nothing
/// else. Rejects a wrong version, wrong prefix, leading/trailing whitespace,
/// extra newlines, non-UTF8, and empty input. Returns a closed success.
pub fn parse_version(stdout: &[u8]) -> Result<(), ParseError> {
    let s = std::str::from_utf8(stdout).map_err(|_| ParseError::NonUtf8)?;
    let expected = format!("nix (Nix) {}", validate::NIX_VERSION);
    // Optional single trailing newline only.
    let line = s.strip_suffix('\n').unwrap_or(s);
    if line == expected {
        Ok(())
    } else {
        Err(ParseError::VersionText)
    }
}

// ---- prefetch DTO + parser -------------------------------------------------

#[derive(Deserialize)]
struct PrefetchDto {
    /// `nix flake prefetch --json` emits `hash` (the prefetched source's NAR
    /// hash as a `sha256-…` SRI string) — NOT `narHash`.
    #[serde(rename = "hash", default)]
    hash: Option<String>,
    #[serde(rename = "storePath", default)]
    store_path: Option<String>,
}

/// Parse `nix flake prefetch --json` output: require the `hash` key (the
/// prefetched source's NAR hash) to EXACTLY equal the pinned manifest
/// [`validate::NIXPKGS_NAR_HASH`] and `storePath` to be a valid
/// `/nix/store/<base>` source path. Returns the closed [`PrefetchVerified`]
/// only; NEVER exposes the raw hash or path.
pub fn parse_prefetch(stdout: &[u8]) -> Result<PrefetchVerified, ParseError> {
    let dto: PrefetchDto = serde_json::from_slice(stdout).map_err(map_json_err)?;
    let hash = dto.hash.ok_or(ParseError::MissingField)?;
    let store_path = dto.store_path.ok_or(ParseError::MissingField)?;
    if hash != validate::NIXPKGS_NAR_HASH {
        return Err(ParseError::PrefetchHashMismatch);
    }
    let base = store_base_of(&store_path).unwrap_or("");
    if !is_valid_store_base(base) {
        return Err(ParseError::PrefetchSourcePathInvalid);
    }
    Ok(PrefetchVerified)
}

// ---- derivation-show DTO + parser -----------------------------------------

#[derive(Deserialize)]
struct DerivationShowDto {
    #[serde(default)]
    version: Option<u64>,
    #[serde(default)]
    derivations: BTreeMap<String, DerivationDto>,
}

#[derive(Deserialize)]
struct DerivationDto {
    /// Every v4 derivation value itself carries a required inner `version`
    /// (== 4). Validated, never exposed.
    #[serde(default)]
    version: Option<u64>,
    /// Every v4 derivation value carries a required nonempty `name`. Validated
    /// for presence/non-emptiness, never exposed.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    outputs: BTreeMap<String, OutputDto>,
}

#[derive(Deserialize)]
struct OutputDto {
    #[serde(default)]
    path: Option<String>,
}

/// Parse `nix derivation show` output (top-level `version` == 4): require
/// EXACTLY one derivation whose map key is a store-path base name ending in
/// `.drv` (e.g. `<hash>-hello.drv`, NOT an absolute `/nix/store/...` path),
/// whose `system` equals the requested canonical Darwin system, and whose
/// `outputs.out.path` is a store-path base name (also NOT an absolute
/// `/nix/store/...` path). Returns the validated internal [`StorePath`]
/// (redacted, never serializable) for the next command. `requested_system` must
/// be one of [`validate::DARWIN_SYSTEMS`].
pub fn parse_derivation(stdout: &[u8], requested_system: &str) -> Result<StorePath, ParseError> {
    let dto: DerivationShowDto = serde_json::from_slice(stdout).map_err(map_json_err)?;
    match dto.version {
        None => return Err(ParseError::MissingVersion),
        Some(v) if v != DERIVATION_VERSION => {
            return Err(ParseError::VersionField {
                got: v,
                expected: DERIVATION_VERSION,
            });
        }
        Some(_) => {}
    }
    if dto.derivations.len() != 1 {
        return Err(ParseError::Cardinality);
    }
    let (drv_key, drv) = dto.derivations.iter().next().expect("len == 1");
    let drv_key = drv_key.as_str();
    // The v4 derivations map key is a store-path base name ending in `.drv`
    // (e.g. `<hash>-hello.drv`), NOT an absolute `/nix/store/...` path.
    if !is_valid_store_base(drv_key) || !drv_key.ends_with(".drv") {
        return Err(ParseError::StorePathInvalid);
    }
    // Every v4 derivation value itself carries a required inner `version`
    // (== 4) and a nonempty `name`. Validate without exposing either value.
    match drv.version {
        None => return Err(ParseError::DerivationVersionMissing),
        Some(v) if v != DERIVATION_VERSION => {
            return Err(ParseError::DerivationVersion { got: v });
        }
        Some(_) => {}
    }
    let name = drv.name.as_deref().unwrap_or("");
    if name.is_empty() {
        return Err(ParseError::DerivationName);
    }
    let system = drv.system.as_deref().unwrap_or("");
    if !validate::DARWIN_SYSTEMS.contains(&system) || system != requested_system {
        return Err(ParseError::DerivationSystemMismatch);
    }
    let out = drv
        .outputs
        .get("out")
        .ok_or(ParseError::DerivationOutputMissing)?;
    let path = out
        .path
        .as_deref()
        .ok_or(ParseError::DerivationOutputMissing)?;
    // The v4 `outputs.out.path` is a store-path base name, NOT an absolute
    // `/nix/store/...` path. Reject an absolute path outright and require a
    // syntactically valid base; parse it into the internal [`StorePath`].
    if path.starts_with('/') || !is_valid_store_base(path) {
        return Err(ParseError::StorePathInvalid);
    }
    Ok(StorePath::from_validated_base(path.to_string()))
}

// ---- path-info DTO + parser -----------------------------------------------

#[derive(Deserialize)]
struct PathInfoDto {
    #[serde(default)]
    version: Option<u64>,
    #[serde(rename = "storeDir", default)]
    store_dir: Option<String>,
    #[serde(default)]
    info: BTreeMap<String, serde_json::Value>,
}

/// Parse `nix path-info --json --json-format 2` output. Validate the top-level
/// `version` (== 2), `storeDir` (== `/nix/store`), every `info` key as a
/// store-path base name, queried presence, and nonrecursive cardinality. Then
/// classify each `info` entry's VALUE:
///   * `null` — Nix's ZERO-EXIT encoding of an invalid/unavailable path
///     (a *cache miss*). A nonrecursive queried `null`, OR any recursive
///     sibling `null`, yields [`PathInfoProbe::Miss`].
///   * a non-null object — must be a structurally valid v2 entry (see
///     [`validate_info_entry`]); on success it is a hit candidate, on any
///     missing/wrong inner field a bounded semantic [`ParseError`].
///   * a non-null non-object — a closed [`ParseError::PathInfoEntryNotObject`].
///
/// The result is [`PathInfoProbe::Hit`] only when the queried path is present
/// AND every entry is a valid v2 object; it is [`PathInfoProbe::Miss`] when a
/// `null` entry is present (after every non-null object is validated). A
/// NONRECURSIVE query must carry EXACTLY the queried entry; a RECURSIVE query
/// may carry additional valid base-name keys. Carries NO path, hash, or count.
pub fn parse_path_info(
    stdout: &[u8],
    queried: &StorePath,
    recursive: bool,
) -> Result<PathInfoProbe, ParseError> {
    let dto: PathInfoDto = serde_json::from_slice(stdout).map_err(map_json_err)?;
    match dto.version {
        None => return Err(ParseError::MissingVersion),
        Some(v) if v != PATH_INFO_VERSION => {
            return Err(ParseError::VersionField {
                got: v,
                expected: PATH_INFO_VERSION,
            });
        }
        Some(_) => {}
    }
    let store_dir = dto.store_dir.as_deref().unwrap_or("");
    if store_dir != STORE_PREFIX.trim_end_matches('/') {
        return Err(ParseError::StoreDir);
    }
    // The v2 `info` object keys are store-path base names (NOT absolute
    // `/nix/store/...` paths). Validate every key, including any recursive
    // sibling results, before matching the queried path.
    for key in dto.info.keys() {
        if !is_valid_store_base(key.as_str()) {
            return Err(ParseError::StorePathInvalid);
        }
    }
    // The queried path is matched by its base name, not its rendered absolute
    // form.
    if !dto.info.contains_key(queried.base()) {
        return Err(ParseError::PathInfoQueriedAbsent);
    }
    if !recursive && dto.info.len() != 1 {
        return Err(ParseError::PathInfoExtraEntry);
    }
    // Validate every entry's VALUE, then classify hit/miss. A `null` entry is
    // Nix's zero-exit cache-miss encoding; a non-null non-object is a closed
    // parse error; a non-null object must be a structurally valid v2 entry. A
    // single queried `null` (nonrecursive) OR any `null` (recursive) => Miss;
    // every entry a valid v2 hit => Hit. BTreeMap iterates in sorted key order,
    // so the first malformed object is reported deterministically.
    let mut had_null = false;
    for value in dto.info.values() {
        match classify_info_entry(value)? {
            EntryOutcome::Null => had_null = true,
            EntryOutcome::Hit => {}
        }
    }
    if had_null {
        Ok(PathInfoProbe::Miss(CacheMiss))
    } else {
        Ok(PathInfoProbe::Hit(PathInfoHit))
    }
}

/// The classification of one v2 `info` entry value: a `null` cache-miss
/// marker or a structurally valid v2 entry object (a hit candidate).
enum EntryOutcome {
    /// `null` — Nix's zero-exit cache-miss encoding.
    Null,
    /// A structurally valid v2 entry object.
    Hit,
}

/// Classify one v2 `info` entry value: `null` => [`EntryOutcome::Null`]; a
/// non-null object validated by [`validate_info_entry`] =>
/// [`EntryOutcome::Hit`]; a non-null non-object => a closed
/// [`ParseError::PathInfoEntryNotObject`].
fn classify_info_entry(value: &serde_json::Value) -> Result<EntryOutcome, ParseError> {
    match value {
        serde_json::Value::Null => Ok(EntryOutcome::Null),
        serde_json::Value::Object(obj) => {
            validate_info_entry(obj)?;
            Ok(EntryOutcome::Hit)
        }
        _ => Err(ParseError::PathInfoEntryNotObject),
    }
}

/// Validate the inner fields of a non-null v2 `info` entry object: inner
/// `version` (present `u64` == 2), inner `storeDir` (present string ==
/// `/nix/store`), `narHash` (present string), `narSize` (present `u64`),
/// `references` (present array of valid base-name strings, each NON-absolute),
/// and `ca` (present, `null` or object). Additive fields are accepted; NONE of
/// the data is retained or exposed. Only bounded semantic errors are emitted.
fn validate_info_entry(obj: &serde_json::Map<String, serde_json::Value>) -> Result<(), ParseError> {
    use serde_json::Value;
    // version: present u64 == 2.
    match obj.get("version") {
        Some(Value::Number(n)) => match n.as_u64() {
            Some(v) if v == PATH_INFO_VERSION => {}
            Some(v) => return Err(ParseError::PathInfoEntryVersion { got: v }),
            None => return Err(ParseError::PathInfoEntryVersionMissing),
        },
        _ => return Err(ParseError::PathInfoEntryVersionMissing),
    }
    // storeDir: present string == /nix/store.
    match obj.get("storeDir") {
        Some(Value::String(s)) if s == STORE_PREFIX.trim_end_matches('/') => {}
        _ => return Err(ParseError::PathInfoEntryStoreDir),
    }
    // narHash: present string.
    match obj.get("narHash") {
        Some(Value::String(_)) => {}
        None => return Err(ParseError::PathInfoEntryMissingField),
        Some(_) => return Err(ParseError::PathInfoEntryFieldType),
    }
    // narSize: present u64.
    match obj.get("narSize") {
        Some(Value::Number(n)) if n.as_u64().is_some() => {}
        None => return Err(ParseError::PathInfoEntryMissingField),
        Some(_) => return Err(ParseError::PathInfoEntryFieldType),
    }
    // references: present array of valid base-name strings (each NON-absolute).
    match obj.get("references") {
        Some(Value::Array(arr)) => {
            for r in arr {
                match r {
                    Value::String(s) if is_valid_store_base(s) => {}
                    _ => return Err(ParseError::PathInfoEntryReference),
                }
            }
        }
        None => return Err(ParseError::PathInfoEntryMissingField),
        Some(_) => return Err(ParseError::PathInfoEntryFieldType),
    }
    // ca: present, null or object.
    match obj.get("ca") {
        Some(Value::Null) | Some(Value::Object(_)) => {}
        None => return Err(ParseError::PathInfoEntryMissingField),
        Some(_) => return Err(ParseError::PathInfoEntryFieldType),
    }
    Ok(())
}

/// Classify whether a NONZERO-exit path-info outcome is a clean cache miss.
///
/// A clean miss requires [`ProbeStatus::Exited`]`(nonzero)` AND retained stderr
/// that is EXACTLY `path '/nix/store/<base>' is not valid` with an optional
/// single trailing newline (no extra lines, no prose, no leading `error:`).
/// The `<base>` must be syntactically valid. For the NONRECURSIVE output query
/// (`recursive == false`) the path must EXACTLY equal the queried output path;
/// for a RECURSIVE query any syntactically valid store path is accepted. A
/// signal, a zero exit, extra lines, prose, an invalid/non-matching path, or
/// non-UTF8 is a [`ContractError`]. Returns the closed [`CacheMiss`] only.
pub fn classify_cache_miss(
    status: ProbeStatus,
    stderr: &[u8],
    queried: &StorePath,
    recursive: bool,
) -> Result<CacheMiss, ContractError> {
    match status {
        ProbeStatus::Signaled(_) => return Err(ContractError::Signaled),
        ProbeStatus::Exited(0) => return Err(ContractError::ZeroExit),
        ProbeStatus::Exited(_) => {}
    }
    let s = std::str::from_utf8(stderr).map_err(|_| ContractError::NonUtf8)?;
    // Optional single trailing newline; anything else (extra lines) is MissShape.
    let line = s.strip_suffix('\n').unwrap_or(s);
    if line.contains('\n') {
        return Err(ContractError::MissShape);
    }
    // Exact envelope: `path '<abspath>' is not valid`.
    let mid = line
        .strip_prefix("path '")
        .and_then(|m| m.strip_suffix("' is not valid"))
        .ok_or(ContractError::MissShape)?;
    let base = match store_base_of(mid) {
        Some(b) if !b.is_empty() => b,
        _ => return Err(ContractError::MissPathInvalid),
    };
    if !is_valid_store_base(base) {
        return Err(ContractError::MissPathInvalid);
    }
    if !recursive && base != queried.base() {
        return Err(ContractError::MissPathMismatch);
    }
    Ok(CacheMiss)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandRunner, CommandSpec, FakeCommandRunner, ProbeStatus, SpecError};
    use crate::validate;
    use std::path::PathBuf;

    // A valid 32-char nix-base32 hash (the alphabet itself, each char once).
    const H32: &str = "0123456789abcdfghijklmnpqrsvwxyz";
    // A canonical store-path base built from H32 (the part after /nix/store/).
    const BASE: &str = "0123456789abcdfghijklmnpqrsvwxyz-hello-2.12.1";
    // The full absolute store path (/nix/store/ + BASE).
    const ABS_FULL: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-hello-2.12.1";
    // A canonical .drv map key (v4 derivations keys are base names ending in .drv).
    const DRV_BASE: &str = "0123456789abcdfghijklmnpqrsvwxyz-hello.drv";

    const NIX_BIN: &str = "/nix/var/nix/bin/nix";

    fn args(spec: &CommandSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn sp(base: &str) -> StorePath {
        StorePath::from_validated_base(base.to_string())
    }

    fn abs(path: &str) -> String {
        format!("{STORE_PREFIX}{path}")
    }

    /// `true` iff `argv` carries the exact global option triplet
    /// `[--option, <name>, <value>]` as three consecutive tokens.
    fn has_opt_triplet(argv: &[String], name: &str, value: &str) -> bool {
        for w in argv.windows(3) {
            if w[0] == "--option" && w[1] == name && w[2] == value {
                return true;
            }
        }
        false
    }

    /// Assert `argv` carries NO enabling form for setting `name`: no bare
    /// `--<name>` stem, no `--<name>=…` equals token, and no
    /// `--option <name> <v>` triplet whose value is NOT exactly `false`.
    fn assert_no_enabling_form(argv: &[String], name: &str) {
        let stem = format!("--{name}");
        let eq = format!("--{name}=");
        for (i, tok) in argv.iter().enumerate() {
            assert!(
                tok != &stem && !tok.starts_with(&eq),
                "enabling form {tok:?} for {name:?} in {argv:?}"
            );
            if tok == "--option" && i + 2 < argv.len() && argv[i + 1] == name {
                assert_eq!(
                    argv[i + 2],
                    "false",
                    "non-disabling --option value for {name:?} in {argv:?}"
                );
            }
        }
    }

    // =====================================================================
    // constants pinned to the manifest
    // =====================================================================

    #[test]
    fn pinned_ref_matches_manifest_constants() {
        // The full rev+narHash form: rev-only is a regression. Both manifest
        // constants must appear exactly.
        assert_eq!(
            PINNED_REF,
            format!(
                "github:{}/{}/{}?narHash={}",
                validate::NIXPKGS_OWNER,
                validate::NIXPKGS_REPO,
                validate::NIXPKGS_REV,
                validate::NIXPKGS_NAR_HASH,
            )
        );
        // Sanity: the rev-only form is NOT what we carry.
        assert_ne!(
            PINNED_REF,
            format!(
                "github:{}/{}/{}",
                validate::NIXPKGS_OWNER,
                validate::NIXPKGS_REPO,
                validate::NIXPKGS_REV,
            )
        );
    }

    #[test]
    fn cache_url_is_manifest_url() {
        // The builders reference the manifest URL directly.
        let s = store_info_spec(Path::new(NIX_BIN)).unwrap();
        assert!(args(&s).contains(&validate::CACHE_STORE_URL.to_string()));
        assert_eq!(validate::CACHE_STORE_URL, "https://cache.nixos.org/");
    }

    #[test]
    fn all_timeouts_within_max_and_caps_nonzero() {
        assert!(VERSION_TIMEOUT <= crate::command::MAX_TIMEOUT);
        assert!(PREFETCH_TIMEOUT <= crate::command::MAX_TIMEOUT);
        assert!(STORE_INFO_TIMEOUT <= crate::command::MAX_TIMEOUT);
        assert!(DERIVATION_TIMEOUT <= crate::command::MAX_TIMEOUT);
        assert!(PATH_INFO_TIMEOUT <= crate::command::MAX_TIMEOUT);
        assert!(RECURSIVE_TIMEOUT <= crate::command::MAX_TIMEOUT);
        // The two network/evaluation probes sit at the ceiling.
        assert_eq!(PREFETCH_TIMEOUT, crate::command::MAX_TIMEOUT);
        assert_eq!(DERIVATION_TIMEOUT, crate::command::MAX_TIMEOUT);
        for cap in [
            VERSION_STDOUT_CAP,
            PREFETCH_STDOUT_CAP,
            STORE_INFO_STDOUT_CAP,
            DERIVATION_STDOUT_CAP,
            PATH_INFO_STDOUT_CAP,
            RECURSIVE_STDOUT_CAP,
            PREFLIGHT_STDERR_CAP,
        ] {
            assert!(cap > 0);
            assert!(NonZeroU64::new(cap).is_some());
        }
    }

    // =====================================================================
    // version_spec
    // =====================================================================

    #[test]
    fn version_spec_argv_is_version_only_no_feature_args() {
        let s = version_spec(Path::new(NIX_BIN)).unwrap();
        assert_eq!(s.program, PathBuf::from(NIX_BIN));
        assert_eq!(args(&s), vec!["--version"]);
        // NO feature args.
        assert!(!args(&s).contains(&"--extra-experimental-features".to_string()));
        assert_eq!(s.stdout_cap, nz(VERSION_STDOUT_CAP));
        assert_eq!(s.stderr_cap, nz(PREFLIGHT_STDERR_CAP));
        assert_eq!(s.timeout, VERSION_TIMEOUT);
    }

    #[test]
    fn version_spec_rejects_nonabsolute_nix_bin() {
        for bad in ["nix", "./nix", "bin/nix", ""] {
            let err = version_spec(Path::new(bad)).unwrap_err();
            assert!(matches!(err, PreflightBuildError::Spec(_)), "{bad:?}");
        }
        // The Spec variant forwards to the bounded SpecError display.
        let s = PreflightBuildError::Spec(SpecError::ProgramEmpty).to_string();
        assert!(s.contains("program path must not be empty"));
    }

    // =====================================================================
    // prefetch_spec
    // =====================================================================

    #[test]
    fn prefetch_spec_exact_argv_and_ordering() {
        let s = prefetch_spec(Path::new(NIX_BIN)).unwrap();
        assert_eq!(
            args(&s),
            vec![
                "--extra-experimental-features",
                "nix-command flakes",
                // FIXED global eval-hardening triplet immediately after the
                // feature args, before the `flake prefetch` subcommand.
                "--option",
                "accept-flake-config",
                "false",
                "flake",
                "prefetch",
                "--json",
                "--no-pretty",
                "--no-write-lock-file",
                "--no-use-registries",
                PINNED_REF,
            ]
        );
        // The FIXED global eval-hardening triplet sits immediately after
        // FEATURE_ARGS and immediately before the `flake prefetch` subcommand.
        let a = args(&s);
        let feat = a
            .iter()
            .position(|x| x == "--extra-experimental-features")
            .unwrap();
        assert_eq!(a[feat + 1], "nix-command flakes");
        assert_eq!(a[feat + 2], "--option");
        assert_eq!(a[feat + 3], "accept-flake-config");
        assert_eq!(a[feat + 4], "false");
        assert_eq!(a[feat + 5], "flake");
        // Prefetch NEVER disables IFD (it evaluates a flake, not a derivation).
        assert!(
            !a.contains(&"--allow-import-from-derivation".to_string()),
            "{a:?}"
        );
        // No equals-form or bare-form accept-flake-config token survives: only
        // the `--option` triplet is permitted.
        assert!(
            !a.iter().any(|x| x.starts_with("--accept-flake-config")),
            "{a:?}"
        );
        assert_eq!(s.stdout_cap, nz(PREFETCH_STDOUT_CAP));
        assert_eq!(s.timeout, PREFETCH_TIMEOUT);
    }

    // =====================================================================
    // store_info_spec
    // =====================================================================

    #[test]
    fn store_info_spec_exact_argv_and_ordering() {
        let s = store_info_spec(Path::new(NIX_BIN)).unwrap();
        assert_eq!(
            args(&s),
            vec![
                "--extra-experimental-features",
                "nix-command flakes",
                "store",
                "info",
                "--store",
                validate::CACHE_STORE_URL,
            ]
        );
        assert_eq!(s.stdout_cap, nz(STORE_INFO_STDOUT_CAP));
        assert_eq!(s.timeout, STORE_INFO_TIMEOUT);
    }

    // =====================================================================
    // derivation_spec
    // =====================================================================

    #[test]
    fn derivation_spec_exact_argv_per_canonical_system_attr() {
        for (system, attr) in [("x86_64-darwin", "hello"), ("aarch64-darwin", "git")] {
            let s = derivation_spec(Path::new(NIX_BIN), system, attr).unwrap();
            let flake_ref = format!("{PINNED_REF}#legacyPackages.{system}.{attr}^out");
            assert_eq!(
                args(&s),
                vec![
                    "--extra-experimental-features",
                    "nix-command flakes",
                    // FIXED global eval-hardening triplets immediately after
                    // the feature args, before the `derivation show` subcommand.
                    "--option",
                    "accept-flake-config",
                    "false",
                    "--option",
                    "allow-import-from-derivation",
                    "false",
                    "derivation",
                    "show",
                    "--no-pretty",
                    "--no-write-lock-file",
                    "--no-use-registries",
                    &flake_ref,
                ],
                "{system}/{attr}"
            );
            // The two FIXED global eval-hardening triplets sit in exact order
            // (accept-flake-config, then allow-import-from-derivation)
            // immediately after FEATURE_ARGS and before the subcommand.
            let a = args(&s);
            let feat = a
                .iter()
                .position(|x| x == "--extra-experimental-features")
                .unwrap();
            assert_eq!(a[feat + 2], "--option");
            assert_eq!(a[feat + 3], "accept-flake-config");
            assert_eq!(a[feat + 4], "false");
            assert_eq!(a[feat + 5], "--option");
            assert_eq!(a[feat + 6], "allow-import-from-derivation");
            assert_eq!(a[feat + 7], "false");
            assert_eq!(a[feat + 8], "derivation");
            assert_eq!(s.stdout_cap, nz(DERIVATION_STDOUT_CAP));
            assert_eq!(s.timeout, DERIVATION_TIMEOUT);
        }
    }

    #[test]
    fn derivation_spec_rejects_non_canonical_system_or_attr() {
        // Non-canonical system.
        assert_eq!(
            derivation_spec(Path::new(NIX_BIN), "x86_64-linux", "hello").unwrap_err(),
            PreflightBuildError::SystemNotCanonical
        );
        // Non-canonical attr.
        assert_eq!(
            derivation_spec(Path::new(NIX_BIN), "x86_64-darwin", "curl").unwrap_err(),
            PreflightBuildError::AttrNotCanonical
        );
        // Non-absolute nix_bin is still rejected (validation precedes argv).
        assert!(matches!(
            derivation_spec(Path::new("nix"), "x86_64-darwin", "hello").unwrap_err(),
            PreflightBuildError::Spec(_)
        ));
    }

    // =====================================================================
    // output_path_spec / recursive_path_spec
    // =====================================================================

    #[test]
    fn output_path_spec_exact_argv_and_ordering() {
        let out = sp(BASE);
        let s = output_path_spec(Path::new(NIX_BIN), &out).unwrap();
        assert_eq!(
            args(&s),
            vec![
                "--extra-experimental-features",
                "nix-command flakes",
                "path-info",
                "--json",
                "--json-format",
                "2",
                "--no-pretty",
                "--store",
                validate::CACHE_STORE_URL,
                ABS_FULL,
            ]
        );
        assert_eq!(s.stdout_cap, nz(PATH_INFO_STDOUT_CAP));
        assert_eq!(s.timeout, PATH_INFO_TIMEOUT);
    }

    #[test]
    fn recursive_path_spec_inserts_recursive_before_store_path() {
        let out = sp(BASE);
        let s = recursive_path_spec(Path::new(NIX_BIN), &out).unwrap();
        let a = args(&s);
        // Same deterministic ordering as output_path_spec, plus `--recursive`
        // immediately before the store path (the positional /nix/store/...).
        assert_eq!(
            a,
            vec![
                "--extra-experimental-features",
                "nix-command flakes",
                "path-info",
                "--json",
                "--json-format",
                "2",
                "--no-pretty",
                "--store",
                validate::CACHE_STORE_URL,
                "--recursive",
                ABS_FULL,
            ]
        );
        // `--recursive` is immediately before the store path.
        let rec_idx = a.iter().position(|x| x == "--recursive").unwrap();
        assert_eq!(a[rec_idx + 1], abs(BASE));
        assert_eq!(s.stdout_cap, nz(RECURSIVE_STDOUT_CAP));
        assert_eq!(s.timeout, RECURSIVE_TIMEOUT);
    }

    // =====================================================================
    // forbidden/unsupported flag absence + feature-arg placement
    // =====================================================================

    #[test]
    fn no_forbidden_or_unsupported_flags_anywhere() {
        let out = sp(BASE);
        let specs = [
            version_spec(Path::new(NIX_BIN)).unwrap(),
            prefetch_spec(Path::new(NIX_BIN)).unwrap(),
            store_info_spec(Path::new(NIX_BIN)).unwrap(),
            derivation_spec(Path::new(NIX_BIN), "x86_64-darwin", "hello").unwrap(),
            output_path_spec(Path::new(NIX_BIN), &out).unwrap(),
            recursive_path_spec(Path::new(NIX_BIN), &out).unwrap(),
        ];
        const FORBIDDEN: &[&str] = &[
            "--offline",
            "--out-link",
            "-o",
            "--substituter",
            "--substituters",
            "--builders",
            "--max-jobs",
            // restrict-eval/allowed-uris are explicitly NOT in this slice: they
            // must never appear in ANY form. (The eval-hardening
            // `--accept-flake-config`/`--allow-import-from-derivation` stems are
            // NOT here because their disabling `--option … false` triplet IS
            // emitted; their enabling/bare forms are forbidden by the dedicated
            // test below.)
            "--restrict-eval",
            "--allowed-uris",
        ];
        for spec in &specs {
            let a = args(spec);
            for &bad in FORBIDDEN {
                // Match a forbidden flag as a WHOLE argv token, or in its
                // `--flag=value` form — NOT as a bare substring. A substring
                // match would false-positive on legitimate content (e.g. the
                // narHash `sha256-oami…` legitimately contains the substring
                // `-o`).
                let bad_eq = bad.to_string();
                let bad_assign = format!("{bad}=");
                assert!(
                    !a.iter().any(|x| x == &bad_eq || x.starts_with(&bad_assign)),
                    "forbidden flag {bad:?} in {a:?}"
                );
            }
        }
    }

    #[test]
    fn eval_hardening_args_present_and_fixed_never_caller_controlled() {
        // The FIXED disabling triplets use the universal stable `--option <name>
        // <value>` form (NEVER the non-contract `--flag=false` single token).
        // They are CONSTANTS (not derived from any caller input), pinning the
        // per-call policy.
        let pref = args(&prefetch_spec(Path::new(NIX_BIN)).unwrap());
        // Prefetch carries EXACTLY the accept-flake-config disabling triplet.
        assert!(has_opt_triplet(&pref, "accept-flake-config", "false"));
        // Prefetch evaluates a flake, not a derivation: the IFD triplet is
        // absent.
        assert!(!has_opt_triplet(
            &pref,
            "allow-import-from-derivation",
            "false"
        ));
        let drv = args(&derivation_spec(Path::new(NIX_BIN), "aarch64-darwin", "ripgrep").unwrap());
        // Derivation carries BOTH disabling triplets.
        assert!(has_opt_triplet(&drv, "accept-flake-config", "false"));
        assert!(has_opt_triplet(
            &drv,
            "allow-import-from-derivation",
            "false"
        ));
        // The eval-hardening triplets are NEVER carried by the non-eval probes.
        let out = sp(BASE);
        for spec in [
            version_spec(Path::new(NIX_BIN)).unwrap(),
            store_info_spec(Path::new(NIX_BIN)).unwrap(),
            output_path_spec(Path::new(NIX_BIN), &out).unwrap(),
            recursive_path_spec(Path::new(NIX_BIN), &out).unwrap(),
        ] {
            let a = args(&spec);
            assert!(
                !has_opt_triplet(&a, "accept-flake-config", "false"),
                "{a:?}"
            );
            assert!(
                !has_opt_triplet(&a, "allow-import-from-derivation", "false"),
                "{a:?}"
            );
        }
        // The ENABLING forms must NEVER appear on ANY spec: the only permitted
        // shape for either setting is the `--option <name> false` triplet.
        // Reject bare stems, any `--flag=…` equals token, and any `--option`
        // triplet whose value for either setting is NOT exactly `false`.
        let all_specs: Vec<Vec<String>> = [
            version_spec(Path::new(NIX_BIN)).unwrap(),
            prefetch_spec(Path::new(NIX_BIN)).unwrap(),
            store_info_spec(Path::new(NIX_BIN)).unwrap(),
            derivation_spec(Path::new(NIX_BIN), "x86_64-darwin", "hello").unwrap(),
            output_path_spec(Path::new(NIX_BIN), &out).unwrap(),
            recursive_path_spec(Path::new(NIX_BIN), &out).unwrap(),
        ]
        .iter()
        .map(args)
        .collect();
        for a in &all_specs {
            assert_no_enabling_form(a, "accept-flake-config");
            assert_no_enabling_form(a, "allow-import-from-derivation");
        }
    }

    #[test]
    fn json_format_only_on_path_info_never_prefetch_or_derivation() {
        let out = sp(BASE);
        // version, prefetch, store-info, derivation must NOT carry --json-format.
        for spec in [
            version_spec(Path::new(NIX_BIN)).unwrap(),
            prefetch_spec(Path::new(NIX_BIN)).unwrap(),
            store_info_spec(Path::new(NIX_BIN)).unwrap(),
            derivation_spec(Path::new(NIX_BIN), "x86_64-darwin", "hello").unwrap(),
        ] {
            assert!(
                !args(&spec).contains(&"--json-format".to_string()),
                "unexpected --json-format: {:?}",
                args(&spec)
            );
        }
        // path-info probes DO carry --json-format 2.
        for spec in [
            output_path_spec(Path::new(NIX_BIN), &out).unwrap(),
            recursive_path_spec(Path::new(NIX_BIN), &out).unwrap(),
        ] {
            let a = args(&spec);
            let i = a.iter().position(|x| x == "--json-format").unwrap();
            assert_eq!(a[i + 1], "2");
        }
    }

    #[test]
    fn feature_args_precede_subcommand_and_version_has_none() {
        // version: no feature args at all.
        assert!(
            !args(&version_spec(Path::new(NIX_BIN)).unwrap())
                .contains(&"--extra-experimental-features".to_string())
        );
        // Every other builder: feature args come before the subcommand.
        for (spec, sub) in [
            (prefetch_spec(Path::new(NIX_BIN)).unwrap(), "flake"),
            (store_info_spec(Path::new(NIX_BIN)).unwrap(), "store"),
            (
                derivation_spec(Path::new(NIX_BIN), "aarch64-darwin", "ripgrep").unwrap(),
                "derivation",
            ),
        ] {
            let a = args(&spec);
            let feat = a
                .iter()
                .position(|x| x == "--extra-experimental-features")
                .unwrap();
            assert_eq!(a[feat + 1], "nix-command flakes");
            let sub_idx = a.iter().position(|x| x == sub).unwrap();
            assert!(sub_idx > feat + 1, "subcommand must follow feature args");
        }
        // path-info probes too.
        let out = sp(BASE);
        for spec in [
            output_path_spec(Path::new(NIX_BIN), &out).unwrap(),
            recursive_path_spec(Path::new(NIX_BIN), &out).unwrap(),
        ] {
            let a = args(&spec);
            let feat = a
                .iter()
                .position(|x| x == "--extra-experimental-features")
                .unwrap();
            let sub_idx = a.iter().position(|x| x == "path-info").unwrap();
            assert!(sub_idx > feat + 1);
        }
    }

    // =====================================================================
    // FakeCommandRunner::set_spec is usable with the built specs
    // (this also keeps set_spec live under --all-targets)
    // =====================================================================

    #[test]
    fn built_specs_are_scriptable_via_set_spec() {
        let mut fake = FakeCommandRunner::new();
        let v = version_spec(Path::new(NIX_BIN)).unwrap();
        fake.set_spec(
            &v,
            Ok(crate::command::CommandOutcome {
                status: ProbeStatus::Exited(0),
                stdout: b"nix (Nix) 2.34.8\n".to_vec(),
                stderr: Vec::new(),
                stdout_total_bytes: 16,
                stderr_total_bytes: 0,
                wall_ms: 1,
            }),
        );
        let out = fake.run_probe(&v).unwrap();
        assert!(out.is_success());
        parse_version(&out.stdout).unwrap();
    }

    // =====================================================================
    // parse_version
    // =====================================================================

    #[test]
    fn version_parses_exact_with_optional_single_newline() {
        parse_version(b"nix (Nix) 2.34.8").unwrap();
        parse_version(b"nix (Nix) 2.34.8\n").unwrap();
    }

    #[test]
    fn version_rejects_wrong_malformed_extra() {
        // wrong version
        assert_eq!(
            parse_version(b"nix (Nix) 2.34.9\n").unwrap_err(),
            ParseError::VersionText
        );
        // wrong prefix
        assert_eq!(
            parse_version(b"Nix (Nix) 2.34.8\n").unwrap_err(),
            ParseError::VersionText
        );
        assert_eq!(
            parse_version(b"nix 2.34.8\n").unwrap_err(),
            ParseError::VersionText
        );
        // leading/trailing whitespace
        assert_eq!(
            parse_version(b" nix (Nix) 2.34.8\n").unwrap_err(),
            ParseError::VersionText
        );
        assert_eq!(
            parse_version(b"nix (Nix) 2.34.8 \n").unwrap_err(),
            ParseError::VersionText
        );
        // extra trailing newline / extra content
        assert_eq!(
            parse_version(b"nix (Nix) 2.34.8\n\n").unwrap_err(),
            ParseError::VersionText
        );
        assert_eq!(
            parse_version(b"nix (Nix) 2.34.8 extra\n").unwrap_err(),
            ParseError::VersionText
        );
        // empty
        assert_eq!(parse_version(b"").unwrap_err(), ParseError::VersionText);
        // non-UTF8
        assert_eq!(parse_version(&[0xff]).unwrap_err(), ParseError::NonUtf8);
        assert_eq!(
            parse_version(b"nix (Nix) 2.34.8\xff").unwrap_err(),
            ParseError::NonUtf8
        );
    }

    // =====================================================================
    // parse_prefetch
    // =====================================================================

    fn prefetch_json(hash: &str, store_path: &str) -> String {
        serde_json::json!({
            "hash": hash,
            "storePath": store_path,
            "lastModified": 0,
            "rev": "deadbeef",
        })
        .to_string()
    }

    #[test]
    fn prefetch_parses_matching_hash_and_valid_source_path() {
        // `nix flake prefetch --json` emits `hash` (not `narHash`).
        let j = prefetch_json(
            validate::NIXPKGS_NAR_HASH,
            &abs("0123456789abcdfghijklmnpqrsvwxyz-source"),
        );
        assert_eq!(parse_prefetch(j.as_bytes()).unwrap(), PrefetchVerified);
    }

    #[test]
    fn prefetch_rejects_wrong_hash() {
        let j = prefetch_json(
            "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            &abs("0123456789abcdfghijklmnpqrsvwxyz-source"),
        );
        assert_eq!(
            parse_prefetch(j.as_bytes()).unwrap_err(),
            ParseError::PrefetchHashMismatch
        );
    }

    #[test]
    fn prefetch_rejects_invalid_source_path() {
        // not a /nix/store path
        let j = prefetch_json(validate::NIXPKGS_NAR_HASH, "/tmp/source");
        assert_eq!(
            parse_prefetch(j.as_bytes()).unwrap_err(),
            ParseError::PrefetchSourcePathInvalid
        );
        // /nix/store prefix but malformed base (too short)
        let j = prefetch_json(validate::NIXPKGS_NAR_HASH, "/nix/store/short");
        assert_eq!(
            parse_prefetch(j.as_bytes()).unwrap_err(),
            ParseError::PrefetchSourcePathInvalid
        );
        // base with an out-of-alphabet char
        let j = prefetch_json(
            validate::NIXPKGS_NAR_HASH,
            "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-bad",
        );
        assert_eq!(
            parse_prefetch(j.as_bytes()).unwrap_err(),
            ParseError::PrefetchSourcePathInvalid
        );
    }

    #[test]
    fn prefetch_rejects_nar_hash_only_key_as_missing_hash() {
        // Real prefetch output uses `hash`; a stale `narHash`-only document is
        // rejected as a MISSING `hash` field (not a hash mismatch).
        let j = serde_json::json!({
            "narHash": validate::NIXPKGS_NAR_HASH,
            "storePath": abs("0123456789abcdfghijklmnpqrsvwxyz-source"),
        })
        .to_string();
        assert_eq!(
            parse_prefetch(j.as_bytes()).unwrap_err(),
            ParseError::MissingField
        );
        // A document carrying BOTH keys still succeeds via the `hash` value.
        let j = serde_json::json!({
            "hash": validate::NIXPKGS_NAR_HASH,
            "narHash": "sha256-deadbeef",
            "storePath": abs("0123456789abcdfghijklmnpqrsvwxyz-source"),
        })
        .to_string();
        assert_eq!(parse_prefetch(j.as_bytes()).unwrap(), PrefetchVerified);
    }

    #[test]
    fn prefetch_rejects_missing_fields_and_malformed_and_non_utf8() {
        // missing hash
        let j = serde_json::json!({ "storePath": abs("0123456789abcdfghijklmnpqrsvwxyz-source") })
            .to_string();
        assert_eq!(
            parse_prefetch(j.as_bytes()).unwrap_err(),
            ParseError::MissingField
        );
        // missing storePath
        let j = serde_json::json!({ "hash": validate::NIXPKGS_NAR_HASH }).to_string();
        assert_eq!(
            parse_prefetch(j.as_bytes()).unwrap_err(),
            ParseError::MissingField
        );
        // malformed JSON
        assert_eq!(
            parse_prefetch(b"{ not json").unwrap_err(),
            ParseError::MalformedJson
        );
        // non-UTF8
        assert_eq!(
            parse_prefetch(&[0xff, 0xfe]).unwrap_err(),
            ParseError::MalformedJson
        );
    }

    // =====================================================================
    // parse_derivation
    // =====================================================================

    fn drv_json(version: serde_json::Value, system: &str, out_path: impl AsRef<str>) -> String {
        let out_path = out_path.as_ref();
        // Realistic v4 `nix derivation show` shape: the derivations map key is a
        // store-path base name ending in `.drv`; the derivation value itself
        // carries a required inner `version`==4 and a nonempty `name`;
        // `outputs.out.path` is a store-path base name (NOT an absolute
        // `/nix/store` path); and for an INPUT-ADDRESSED output the `out` object
        // contains ONLY `path` (no stale `hashAlgo`/`hash`). inputs use the v4
        // `{"drvs":{},"srcs":[]}` shape (no v3 inputDrvs/inputSrcs).
        serde_json::json!({
            "version": version,
            "derivations": {
                DRV_BASE: {
                    "version": DERIVATION_VERSION,
                    "name": "hello-2.12.1",
                    "system": system,
                    "outputs": {
                        "out": {
                            "path": out_path,
                        }
                    },
                    "builder": "/bin/sh",
                    "args": [],
                    "env": {},
                    "inputs": { "drvs": {}, "srcs": [] },
                }
            }
        })
        .to_string()
    }

    #[test]
    fn derivation_parses_v4_one_drv_canonical_system_and_out_path() {
        // The v4 doc: derivations key is a base name ending in `.drv`, and
        // outputs.out.path is a store-path base name (NOT an absolute path).
        let j = drv_json(serde_json::json!(4), "x86_64-darwin", BASE);
        let p = parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap();
        assert_eq!(p.base(), BASE);
        assert_eq!(p.render(), abs(BASE));
    }

    #[test]
    fn derivation_rejects_wrong_or_missing_version() {
        for v in [
            serde_json::json!(3),
            serde_json::json!(5),
            serde_json::json!(0),
        ] {
            let got = v.as_u64().unwrap();
            let j = drv_json(v, "x86_64-darwin", BASE);
            assert_eq!(
                parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap_err(),
                ParseError::VersionField {
                    got,
                    expected: DERIVATION_VERSION
                }
            );
        }
        // missing version entirely (the version check fires before key/path
        // validation, so the absolute-free v4 key/path here are never reached)
        let j = serde_json::json!({
            "derivations": { DRV_BASE: { "system": "x86_64-darwin", "outputs": { "out": { "path": BASE } } } }
        })
        .to_string();
        assert_eq!(
            parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::MissingVersion
        );
    }

    #[test]
    fn derivation_rejects_wrong_or_missing_inner_version() {
        // Every v4 derivation VALUE carries its own required inner version==4.
        // wrong inner version (object present, value != 4)
        for v in [0u64, 3, 5] {
            let j = serde_json::json!({
                "version": 4,
                "derivations": { DRV_BASE: {
                    "version": v,
                    "name": "hello-2.12.1",
                    "system": "x86_64-darwin",
                    "outputs": { "out": { "path": BASE } }
                } }
            })
            .to_string();
            assert_eq!(
                parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap_err(),
                ParseError::DerivationVersion { got: v }
            );
        }
        // missing inner version (fires after the drv key check, before name)
        let j = serde_json::json!({
            "version": 4,
            "derivations": { DRV_BASE: {
                "name": "hello-2.12.1",
                "system": "x86_64-darwin",
                "outputs": { "out": { "path": BASE } }
            } }
        })
        .to_string();
        assert_eq!(
            parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::DerivationVersionMissing
        );
        // inner version present but not a u64 (a float): the typed `Option<u64>`
        // field rejects a non-integer at DESERIALIZATION time, surfacing as a
        // closed MalformedJson parse error (the whole document is unusable).
        let j = serde_json::json!({
            "version": 4,
            "derivations": { DRV_BASE: {
                "version": 4.5,
                "name": "hello-2.12.1",
                "system": "x86_64-darwin",
                "outputs": { "out": { "path": BASE } }
            } }
        })
        .to_string();
        assert_eq!(
            parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::MalformedJson
        );
    }

    #[test]
    fn derivation_rejects_missing_or_empty_name() {
        // missing name (fires after the inner version check)
        let missing = serde_json::json!({
            "version": 4,
            "derivations": { DRV_BASE: {
                "version": DERIVATION_VERSION,
                "system": "x86_64-darwin",
                "outputs": { "out": { "path": BASE } }
            } }
        })
        .to_string();
        assert_eq!(
            parse_derivation(missing.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::DerivationName
        );
        // empty name
        let empty = serde_json::json!({
            "version": 4,
            "derivations": { DRV_BASE: {
                "version": DERIVATION_VERSION,
                "name": "",
                "system": "x86_64-darwin",
                "outputs": { "out": { "path": BASE } }
            } }
        })
        .to_string();
        assert_eq!(
            parse_derivation(empty.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::DerivationName
        );
        // whitespace-only name is nonempty (a nonempty string) and accepted;
        // only a truly empty string is rejected. The name is never exposed.
        let ws = serde_json::json!({
            "version": 4,
            "derivations": { DRV_BASE: {
                "version": DERIVATION_VERSION,
                "name": " ",
                "system": "x86_64-darwin",
                "outputs": { "out": { "path": BASE } }
            } }
        })
        .to_string();
        let p = parse_derivation(ws.as_bytes(), "x86_64-darwin").unwrap();
        // The validated name is NOT exposed on StorePath.
        assert_eq!(p.base(), BASE);
    }

    #[test]
    fn derivation_rejects_not_exactly_one_derivation() {
        // zero derivations
        let zero = serde_json::json!({ "version": 4, "derivations": {} }).to_string();
        assert_eq!(
            parse_derivation(zero.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::Cardinality
        );
        // two derivations (cardinality fires before key/path validation)
        let two = serde_json::json!({
            "version": 4,
            "derivations": {
                "0123456789abcdfghijklmnpqrsvwxyz-hello.drv": { "system": "x86_64-darwin", "outputs": { "out": { "path": BASE } } },
                "0123456789abcdfghijklmnpqrsvwxyz-glibc.drv": { "system": "x86_64-darwin", "outputs": { "out": { "path": BASE } } }
            }
        })
        .to_string();
        assert_eq!(
            parse_derivation(two.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::Cardinality
        );
    }

    #[test]
    fn derivation_rejects_wrong_or_non_canonical_system() {
        // canonical but different from requested
        let j = drv_json(serde_json::json!(4), "aarch64-darwin", BASE);
        assert_eq!(
            parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::DerivationSystemMismatch
        );
        // non-canonical system
        let j = drv_json(serde_json::json!(4), "x86_64-linux", BASE);
        assert_eq!(
            parse_derivation(j.as_bytes(), "x86_64-linux").unwrap_err(),
            ParseError::DerivationSystemMismatch
        );
        // missing system (the drv key + inner version/name are valid, so the
        // system check is what fires)
        let j = serde_json::json!({
            "version": 4,
            "derivations": { DRV_BASE: {
                "version": DERIVATION_VERSION,
                "name": "hello-2.12.1",
                "outputs": { "out": { "path": BASE } }
            } }
        })
        .to_string();
        assert_eq!(
            parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::DerivationSystemMismatch
        );
    }

    #[test]
    fn derivation_rejects_missing_out_or_invalid_out_path() {
        // no `out` output at all
        let no_out = serde_json::json!({
            "version": 4,
            "derivations": { DRV_BASE: {
                "version": DERIVATION_VERSION,
                "name": "hello-2.12.1",
                "system": "x86_64-darwin",
                "outputs": { "man": { "path": BASE } }
            } }
        })
        .to_string();
        assert_eq!(
            parse_derivation(no_out.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::DerivationOutputMissing
        );
        // out present but no path
        let no_path = serde_json::json!({
            "version": 4,
            "derivations": { DRV_BASE: {
                "version": DERIVATION_VERSION,
                "name": "hello-2.12.1",
                "system": "x86_64-darwin",
                "outputs": { "out": { "hashAlgo": "r:sha256" } }
            } }
        })
        .to_string();
        assert_eq!(
            parse_derivation(no_path.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::DerivationOutputMissing
        );
        // out path is an ABSOLUTE /nix/store path: v4 requires a base name
        let j = drv_json(serde_json::json!(4), "x86_64-darwin", abs(BASE));
        assert_eq!(
            parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::StorePathInvalid
        );
        // out path is some other absolute path
        let j = drv_json(serde_json::json!(4), "x86_64-darwin", "/tmp/hello");
        assert_eq!(
            parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::StorePathInvalid
        );
        // out path a malformed base (hash too short)
        let j = drv_json(serde_json::json!(4), "x86_64-darwin", "short-name");
        assert_eq!(
            parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::StorePathInvalid
        );
        // valid base32 LENGTH but an out-of-alphabet char (e/i/o/t/u excluded)
        let bad_hash = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-name";
        let j = drv_json(serde_json::json!(4), "x86_64-darwin", bad_hash);
        assert_eq!(
            parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::StorePathInvalid
        );
        // base with an embedded slash (two components)
        let j = drv_json(
            serde_json::json!(4),
            "x86_64-darwin",
            format!("{H32}-name/sub"),
        );
        assert_eq!(
            parse_derivation(j.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::StorePathInvalid
        );
    }

    #[test]
    fn derivation_rejects_non_base_or_non_drv_key() {
        // absolute derivations key (v4 requires a base name, not /nix/store/...)
        let abs_key = serde_json::json!({
            "version": 4,
            "derivations": {
                abs("0123456789abcdfghijklmnpqrsvwxyz-hello.drv"): {
                    "system": "x86_64-darwin",
                    "outputs": { "out": { "path": BASE } }
                }
            }
        })
        .to_string();
        assert_eq!(
            parse_derivation(abs_key.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::StorePathInvalid
        );
        // valid store-path base but NOT ending in `.drv`
        let not_drv = serde_json::json!({
            "version": 4,
            "derivations": {
                BASE: {
                    "system": "x86_64-darwin",
                    "outputs": { "out": { "path": BASE } }
                }
            }
        })
        .to_string();
        assert_eq!(
            parse_derivation(not_drv.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::StorePathInvalid
        );
        // malformed derivations key (hash too short)
        let short = serde_json::json!({
            "version": 4,
            "derivations": {
                "short.drv": {
                    "system": "x86_64-darwin",
                    "outputs": { "out": { "path": BASE } }
                }
            }
        })
        .to_string();
        assert_eq!(
            parse_derivation(short.as_bytes(), "x86_64-darwin").unwrap_err(),
            ParseError::StorePathInvalid
        );
    }

    #[test]
    fn derivation_rejects_malformed_json_and_non_utf8() {
        assert_eq!(
            parse_derivation(b"{ not json", "x86_64-darwin").unwrap_err(),
            ParseError::MalformedJson
        );
        assert_eq!(
            parse_derivation(&[0xff], "x86_64-darwin").unwrap_err(),
            ParseError::MalformedJson
        );
    }

    #[test]
    fn derivation_storepath_is_redacted_and_round_trips_to_next_command() {
        let j = drv_json(serde_json::json!(4), "aarch64-darwin", BASE);
        let p = parse_derivation(j.as_bytes(), "aarch64-darwin").unwrap();
        // Debug/Display never leak the hash/name/path.
        assert!(!format!("{p:?}").contains(H32));
        assert!(!format!("{p:?}").contains("hello"));
        assert!(!format!("{p:?}").contains(STORE_PREFIX));
        assert_eq!(format!("{p:?}"), "StorePath(<redacted>)");
        assert_eq!(format!("{p}"), "<redacted store path>");
        // It flows into the next command's argv correctly.
        let s = output_path_spec(Path::new(NIX_BIN), &p).unwrap();
        assert!(args(&s).contains(&abs(BASE)));
    }

    // =====================================================================
    // parse_path_info (recursive vs nonrecursive)
    // =====================================================================

    /// A full, realistic v2 `info` entry object (a cache hit). The field VALUES
    /// are arbitrary-but-well-shaped; none are retained or exposed by the
    /// parser, so they only need to satisfy the structural checks.
    fn v2_entry() -> serde_json::Value {
        serde_json::json!({
            "version": PATH_INFO_VERSION,
            "storeDir": "/nix/store",
            "narHash": "sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa=",
            "narSize": 100u64,
            "references": [],
            "ca": null,
        })
    }

    fn path_info_json(
        version: u64,
        store_dir: &str,
        info: &[(String, serde_json::Value)],
    ) -> String {
        let mut map = serde_json::Map::new();
        for (k, v) in info {
            map.insert(k.clone(), v.clone());
        }
        serde_json::json!({
            "version": version,
            "storeDir": store_dir,
            "info": map,
        })
        .to_string()
    }

    /// Build a path-info document whose `info` keys all carry a valid v2 hit.
    /// Convenience over [`path_info_json`] for the common all-hit case.
    fn path_info_hits(version: u64, store_dir: &str, info: &[String]) -> String {
        let info: Vec<(String, serde_json::Value)> =
            info.iter().map(|k| (k.clone(), v2_entry())).collect();
        path_info_json(version, store_dir, &info)
    }

    fn single(version: u64, store_dir: &str, value: serde_json::Value) -> String {
        path_info_json(version, store_dir, &[(BASE.to_string(), value)])
    }

    #[test]
    fn path_info_nonrecursive_accepts_exactly_queried() {
        let q = sp(BASE);
        // v2 info keys are store-path base names (NOT absolute /nix/store/...);
        // a full valid v2 entry => Hit.
        let j = path_info_hits(2, "/nix/store", &[BASE.to_string()]);
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, false).unwrap(),
            PathInfoProbe::Hit(PathInfoHit)
        );
        // recursive on the same single-hit doc is also a Hit.
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, true).unwrap(),
            PathInfoProbe::Hit(PathInfoHit)
        );
    }

    #[test]
    fn path_info_recursive_allows_more_entries() {
        let q = sp(BASE);
        let other = "0123456789abcdfghijklmnpqrsvwxyz-glibc-2.39".to_string();
        let j = path_info_hits(2, "/nix/store", &[BASE.to_string(), other.clone()]);
        // recursive: contains queried, more valid entries allowed => Hit.
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, true).unwrap(),
            PathInfoProbe::Hit(PathInfoHit)
        );
        // nonrecursive: the extra entry is rejected.
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoExtraEntry
        );
    }

    #[test]
    fn path_info_zero_exit_null_queried_is_miss() {
        let q = sp(BASE);
        // Nix encodes an invalid/unavailable QUERIED path as a ZERO-EXIT doc
        // with info[queried] = null. Nonrecursive => Miss (not a hit, not an
        // error) — the bug was treating this as a cache hit.
        let j = single(2, "/nix/store", serde_json::Value::Null);
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, false).unwrap(),
            PathInfoProbe::Miss(CacheMiss)
        );
        // recursive on the same null-queried doc is also a Miss.
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, true).unwrap(),
            PathInfoProbe::Miss(CacheMiss)
        );
    }

    #[test]
    fn path_info_recursive_sibling_null_is_miss() {
        let q = sp(BASE);
        let other = "0123456789abcdfghijklmnpqrsvwxyz-glibc-2.39".to_string();
        // recursive: the queried entry is a valid hit, but a SIBLING is null
        // => Miss (any null => miss, once every non-null entry is valid).
        let j = path_info_json(
            2,
            "/nix/store",
            &[
                (BASE.to_string(), v2_entry()),
                (other, serde_json::Value::Null),
            ],
        );
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, true).unwrap(),
            PathInfoProbe::Miss(CacheMiss)
        );
    }

    #[test]
    fn path_info_primitive_entry_value_is_parse_error() {
        let q = sp(BASE);
        // a non-null, non-object info value is a closed parse error, never a
        // miss — regardless of the JSON primitive kind.
        for prim in [
            serde_json::json!(42),
            serde_json::json!("oops"),
            serde_json::json!([1, 2, 3]),
            serde_json::json!(true),
        ] {
            let j = single(2, "/nix/store", prim.clone());
            assert_eq!(
                parse_path_info(j.as_bytes(), &q, false).unwrap_err(),
                ParseError::PathInfoEntryNotObject,
                "primitive {prim:?}"
            );
        }
        // recursive: a primitive sibling is an error even with a null
        // elsewhere (error precedence over miss classification).
        let other = "0123456789abcdfghijklmnpqrsvwxyz-glibc-2.39".to_string();
        let j = path_info_json(
            2,
            "/nix/store",
            &[
                (BASE.to_string(), serde_json::Value::Null),
                (other, serde_json::json!(42)),
            ],
        );
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, true).unwrap_err(),
            ParseError::PathInfoEntryNotObject
        );
    }

    #[test]
    fn path_info_entry_inner_version_and_store_dir_errors() {
        let q = sp(BASE);
        // inner version missing
        let mut e = v2_entry();
        e.as_object_mut().unwrap().remove("version");
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryVersionMissing
        );
        // inner version wrong
        let mut e = v2_entry();
        e["version"] = serde_json::json!(3);
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryVersion { got: 3 }
        );
        // inner version present but not a u64 (float) => unusable => Missing
        let mut e = v2_entry();
        e["version"] = serde_json::json!(2.5);
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryVersionMissing
        );
        // inner storeDir missing
        let mut e = v2_entry();
        e.as_object_mut().unwrap().remove("storeDir");
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryStoreDir
        );
        // inner storeDir wrong
        let mut e = v2_entry();
        e["storeDir"] = serde_json::json!("/var/store");
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryStoreDir
        );
    }

    #[test]
    fn path_info_entry_required_field_errors() {
        let q = sp(BASE);
        // narHash missing
        let mut e = v2_entry();
        e.as_object_mut().unwrap().remove("narHash");
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryMissingField
        );
        // narHash wrong type (not a string)
        let mut e = v2_entry();
        e["narHash"] = serde_json::json!(42);
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryFieldType
        );
        // narSize missing
        let mut e = v2_entry();
        e.as_object_mut().unwrap().remove("narSize");
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryMissingField
        );
        // narSize wrong type (negative => not a u64)
        let mut e = v2_entry();
        e["narSize"] = serde_json::json!(-5);
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryFieldType
        );
        // references missing
        let mut e = v2_entry();
        e.as_object_mut().unwrap().remove("references");
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryMissingField
        );
        // references wrong type (not an array)
        let mut e = v2_entry();
        e["references"] = serde_json::json!("not-array");
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryFieldType
        );
        // ca missing
        let mut e = v2_entry();
        e.as_object_mut().unwrap().remove("ca");
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryMissingField
        );
        // ca wrong type (a number)
        let mut e = v2_entry();
        e["ca"] = serde_json::json!(42);
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryFieldType
        );
    }

    #[test]
    fn path_info_entry_reference_errors() {
        let q = sp(BASE);
        // a reference that is not a valid base name (too short / out of
        // alphabet)
        let mut e = v2_entry();
        e["references"] = serde_json::json!(["not-a-valid-base"]);
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryReference
        );
        // an ABSOLUTE reference (/nix/store/... has an embedded slash) is
        // rejected: v2 references are base names.
        let mut e = v2_entry();
        e["references"] = serde_json::json!([abs("0123456789abcdfghijklmnpqrsvwxyz-glibc-2.39")]);
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryReference
        );
        // a non-string reference entry
        let mut e = v2_entry();
        e["references"] = serde_json::json!([42]);
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoEntryReference
        );
        // a VALID base-name reference is accepted => Hit.
        let mut e = v2_entry();
        e["references"] = serde_json::json!(["0123456789abcdfghijklmnpqrsvwxyz-glibc-2.39"]);
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap(),
            PathInfoProbe::Hit(PathInfoHit)
        );
    }

    #[test]
    fn path_info_entry_accepts_additive_fields_and_content_addressed_ca() {
        let q = sp(BASE);
        // additive fields (deriver, registrationTime, sigs, ...) are accepted.
        let mut e = v2_entry();
        e["deriver"] = serde_json::json!("0123456789abcdfghijklmnpqrsvwxyz-hello-2.12.1.drv");
        e["registrationTime"] = serde_json::json!(0u64);
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap(),
            PathInfoProbe::Hit(PathInfoHit)
        );
        // a content-addressed `ca` object (not null) is accepted => Hit.
        let mut e = v2_entry();
        e["ca"] = serde_json::json!({ "method": "fixed:r:sha256", "hash": "sha256-aaaa=" });
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap(),
            PathInfoProbe::Hit(PathInfoHit)
        );
    }

    #[test]
    fn path_info_rejects_wrong_or_missing_version() {
        let q = sp(BASE);
        for v in [0u64, 1, 3] {
            let j = path_info_hits(v, "/nix/store", &[BASE.to_string()]);
            assert_eq!(
                parse_path_info(j.as_bytes(), &q, false).unwrap_err(),
                ParseError::VersionField {
                    got: v,
                    expected: PATH_INFO_VERSION
                }
            );
        }
        // missing version (version check fires before key validation)
        let j = serde_json::json!({ "storeDir": "/nix/store", "info": { BASE: v2_entry() } })
            .to_string();
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, false).unwrap_err(),
            ParseError::MissingVersion
        );
    }

    #[test]
    fn path_info_rejects_wrong_or_missing_store_dir() {
        let q = sp(BASE);
        for bad in ["/var/store", "/nix/store/", ""] {
            let j = path_info_hits(2, bad, &[BASE.to_string()]);
            assert_eq!(
                parse_path_info(j.as_bytes(), &q, false).unwrap_err(),
                ParseError::StoreDir,
                "storeDir {bad:?}"
            );
        }
        // missing storeDir
        let j = serde_json::json!({ "version": 2, "info": { BASE: v2_entry() } }).to_string();
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, false).unwrap_err(),
            ParseError::StoreDir
        );
    }

    #[test]
    fn path_info_rejects_queried_absent() {
        let q = sp(BASE);
        // info has a different valid base, not the queried one.
        let other = "0123456789abcdfghijklmnpqrsvwxyz-other".to_string();
        let j = path_info_hits(2, "/nix/store", &[other]);
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, false).unwrap_err(),
            ParseError::PathInfoQueriedAbsent
        );
        // recursive too: queried must still be present.
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, true).unwrap_err(),
            ParseError::PathInfoQueriedAbsent
        );
    }

    #[test]
    fn path_info_rejects_absolute_info_key() {
        let q = sp(BASE);
        // An absolute /nix/store info key is rejected even when it would match
        // the queried path: v2 keys must be base names.
        let j = path_info_hits(2, "/nix/store", &[abs(BASE)]);
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, false).unwrap_err(),
            ParseError::StorePathInvalid
        );
        // recursive too: absolute keys are never allowed.
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, true).unwrap_err(),
            ParseError::StorePathInvalid
        );
    }

    #[test]
    fn path_info_recursive_rejects_malformed_key() {
        let q = sp(BASE);
        // recursive: the queried base is present, but a sibling key is a
        // malformed (non-base) value.
        let j = path_info_hits(
            2,
            "/nix/store",
            &[BASE.to_string(), "not-a-valid-base".to_string()],
        );
        assert_eq!(
            parse_path_info(j.as_bytes(), &q, true).unwrap_err(),
            ParseError::StorePathInvalid
        );
    }

    #[test]
    fn path_info_rejects_malformed_json_and_non_utf8() {
        let q = sp(BASE);
        assert_eq!(
            parse_path_info(b"{ not json", &q, false).unwrap_err(),
            ParseError::MalformedJson
        );
        assert_eq!(
            parse_path_info(&[0xff], &q, true).unwrap_err(),
            ParseError::MalformedJson
        );
    }

    #[test]
    fn path_info_probe_display_is_bounded_and_redacted() {
        let q = sp(BASE);
        let j = path_info_hits(2, "/nix/store", &[BASE.to_string()]);
        let hit = parse_path_info(j.as_bytes(), &q, false).unwrap();
        let miss = parse_path_info(
            single(2, "/nix/store", serde_json::Value::Null).as_bytes(),
            &q,
            false,
        )
        .unwrap();
        for probe in [hit, miss] {
            let dbg = format!("{probe:?}");
            let disp = format!("{probe}");
            assert!(disp.starts_with("preflight:"));
            assert!(disp.len() < 200);
            // never echoes a store path, hash, or the queried base.
            assert!(!dbg.contains(STORE_PREFIX), "{dbg}");
            assert!(!dbg.contains(H32), "{dbg}");
            assert!(!disp.contains(STORE_PREFIX), "{disp}");
            assert!(!disp.contains(H32), "{disp}");
        }
    }

    // =====================================================================
    // classify_cache_miss (nonrecursive vs recursive)
    // =====================================================================

    fn miss_stderr(path: &str) -> Vec<u8> {
        format!("path '{path}' is not valid").into_bytes()
    }

    #[test]
    fn miss_nonrecursive_accepts_exact_queried_with_optional_newline() {
        let q = sp(BASE);
        // no trailing newline
        classify_cache_miss(ProbeStatus::Exited(1), &miss_stderr(&abs(BASE)), &q, false).unwrap();
        // single trailing newline
        let mut with_nl = miss_stderr(&abs(BASE));
        with_nl.push(b'\n');
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), &with_nl, &q, false).unwrap(),
            CacheMiss
        );
    }

    #[test]
    fn miss_recursive_accepts_any_valid_store_path() {
        let q = sp(BASE);
        // recursive: a DIFFERENT valid path is accepted.
        let other = abs("0123456789abcdfghijklmnpqrsvwxyz-glibc-2.39");
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), &miss_stderr(&other), &q, true).unwrap(),
            CacheMiss
        );
        // nonrecursive: that different path is a mismatch.
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), &miss_stderr(&other), &q, false)
                .unwrap_err(),
            ContractError::MissPathMismatch
        );
    }

    #[test]
    fn miss_rejects_zero_exit_and_signal() {
        let q = sp(BASE);
        // zero exit: a miss requires nonzero.
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(0), &miss_stderr(&abs(BASE)), &q, false)
                .unwrap_err(),
            ContractError::ZeroExit
        );
        // signal: never a miss.
        assert_eq!(
            classify_cache_miss(ProbeStatus::Signaled(9), &miss_stderr(&abs(BASE)), &q, true)
                .unwrap_err(),
            ContractError::Signaled
        );
    }

    #[test]
    fn miss_rejects_extra_lines_and_prose() {
        let q = sp(BASE);
        // extra line
        let two = format!("path '{}' is not valid\nsecond line\n", abs(BASE));
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), two.as_bytes(), &q, false).unwrap_err(),
            ContractError::MissShape
        );
        // double trailing newline
        let dbl = format!("path '{}' is not valid\n\n", abs(BASE));
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), dbl.as_bytes(), &q, false).unwrap_err(),
            ContractError::MissShape
        );
        // prose with an `error:` prefix (the exact envelope is absent)
        let prose = format!("error: path '{}' is not valid\n", abs(BASE));
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), prose.as_bytes(), &q, false).unwrap_err(),
            ContractError::MissShape
        );
        // bare prose, no envelope
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), b"something else broke\n", &q, true)
                .unwrap_err(),
            ContractError::MissShape
        );
        // missing opening quote
        let bad = format!("path {}' is not valid\n", abs(BASE));
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), bad.as_bytes(), &q, false).unwrap_err(),
            ContractError::MissShape
        );
        // missing closing suffix
        let bad = format!("path '{}' is invalid\n", abs(BASE));
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), bad.as_bytes(), &q, false).unwrap_err(),
            ContractError::MissShape
        );
    }

    #[test]
    fn miss_rejects_invalid_path() {
        let q = sp(BASE);
        // envelope correct but inner is not a /nix/store path
        let notstore = "path '/tmp/hello' is not valid\n";
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), notstore.as_bytes(), &q, true).unwrap_err(),
            ContractError::MissPathInvalid
        );
        // /nix/store prefix but malformed base (too short)
        let short = "path '/nix/store/short' is not valid\n";
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), short.as_bytes(), &q, true).unwrap_err(),
            ContractError::MissPathInvalid
        );
        // valid length but out-of-alphabet char (e excluded)
        let badhash = "path '/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-name' is not valid\n";
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), badhash.as_bytes(), &q, true).unwrap_err(),
            ContractError::MissPathInvalid
        );
        // empty path inside the store prefix
        let empty = "path '/nix/store/' is not valid\n";
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), empty.as_bytes(), &q, true).unwrap_err(),
            ContractError::MissPathInvalid
        );
    }

    #[test]
    fn miss_rejects_non_utf8() {
        let q = sp(BASE);
        assert_eq!(
            classify_cache_miss(ProbeStatus::Exited(1), &[0xff, 0xfe], &q, false).unwrap_err(),
            ContractError::NonUtf8
        );
    }

    // =====================================================================
    // error displays are bounded and leak no raw input/path
    // =====================================================================

    #[test]
    fn parse_error_display_bounded_no_raw_input_or_path() {
        let cases = [
            ParseError::NonUtf8,
            ParseError::VersionText,
            ParseError::MalformedJson,
            ParseError::MissingField,
            ParseError::MissingVersion,
            ParseError::VersionField {
                got: 99,
                expected: 4,
            },
            ParseError::Cardinality,
            ParseError::PrefetchHashMismatch,
            ParseError::PrefetchSourcePathInvalid,
            ParseError::DerivationVersionMissing,
            ParseError::DerivationVersion { got: 9 },
            ParseError::DerivationName,
            ParseError::DerivationSystemMismatch,
            ParseError::DerivationOutputMissing,
            ParseError::StorePathInvalid,
            ParseError::StoreDir,
            ParseError::PathInfoQueriedAbsent,
            ParseError::PathInfoExtraEntry,
            ParseError::PathInfoEntryNotObject,
            ParseError::PathInfoEntryVersionMissing,
            ParseError::PathInfoEntryVersion { got: 9 },
            ParseError::PathInfoEntryStoreDir,
            ParseError::PathInfoEntryMissingField,
            ParseError::PathInfoEntryFieldType,
            ParseError::PathInfoEntryReference,
        ];
        for e in cases {
            let s = e.to_string();
            assert!(s.starts_with("preflight:"), "{e:?}: {s}");
            assert!(s.len() < 200, "unbounded display ({e:?}): {s}");
            // never echoes a store path, the hash, or the secret-ish markers.
            assert!(!s.contains(STORE_PREFIX), "{e:?}: {s}");
            assert!(!s.contains(H32), "{e:?}: {s}");
            assert!(!s.contains(validate::NIXPKGS_NAR_HASH), "{e:?}: {s}");
        }
    }

    #[test]
    fn contract_error_display_bounded_no_raw_input_or_path() {
        let cases = [
            ContractError::ZeroExit,
            ContractError::Signaled,
            ContractError::NonUtf8,
            ContractError::MissShape,
            ContractError::MissPathInvalid,
            ContractError::MissPathMismatch,
        ];
        for e in cases {
            let s = e.to_string();
            assert!(s.starts_with("preflight:"), "{e:?}: {s}");
            assert!(s.len() < 200, "unbounded display ({e:?}): {s}");
            assert!(!s.contains(STORE_PREFIX), "{e:?}: {s}");
            assert!(!s.contains(H32), "{e:?}: {s}");
        }
    }

    #[test]
    fn preflight_build_error_display_bounded() {
        for e in [
            PreflightBuildError::SystemNotCanonical,
            PreflightBuildError::AttrNotCanonical,
        ] {
            let s = e.to_string();
            assert!(s.starts_with("preflight:"), "{e:?}: {s}");
            assert!(s.len() < 200);
        }
        // The Spec variant forwards to the already-bounded SpecError; a huge
        // path is still truncated.
        let huge = PathBuf::from(format!("x{}", "Y".repeat(50_000)));
        let err: PreflightBuildError = version_spec(&huge).unwrap_err();
        let s = err.to_string();
        assert!(s.len() < 256, "too long: {s:?}");
        assert!(s.contains("must be absolute"));
    }

    // =====================================================================
    // is_valid_store_base: Nix 2.34.8 StorePath::checkName grammar
    // =====================================================================

    #[test]
    fn is_valid_store_base_mirrors_nix_checkname_grammar() {
        // The hash prefix is always the valid 32-char nix-base32 alphabet; only
        // the NAME (after the `-`) varies across the accept/reject tables below.
        let base = |name: &str| format!("{H32}-{name}");

        // Boundary name lengths around StorePath::MaxPathLen (211).
        let name_211 = "a".repeat(STORE_PATH_MAX_NAME_LEN); // exactly the cap
        let name_212 = "a".repeat(STORE_PATH_MAX_NAME_LEN + 1); // one over

        // ---- ACCEPT: representative valid names + the 211-byte boundary ----
        let accept: Vec<(&str, String)> = vec![
            ("ordinary", base("hello-2.12.1")),
            ("uppercase", base("Hello-WORLD")),
            ("all punctuation", base("a+b-c.d_e?f=g")),
            ("dot-hidden", base(".hidden")),
            ("three dots", base("...")),
            ("trailing dot", base("name.")),
            ("single char", base("x")),
            ("211-byte name", base(&name_211)),
        ];
        for (label, b) in &accept {
            assert!(is_valid_store_base(b), "expected ACCEPT: {label}");
        }

        // ---- REJECT: illegal name bytes (space, slash, controls, unicode, ...)
        let illegal_bytes: &[(u8, &str)] = &[
            (b' ', "space"),
            (b'/', "slash"),
            (b'\n', "newline"),
            (0x00, "NUL"),
            (0x01, "SOH control"),
            (0x7f, "DEL control"),
            (b':', "colon"),
        ];
        for &(byte, label) in illegal_bytes {
            let b = base(&format!("ok{}ok", byte as char));
            assert!(
                !is_valid_store_base(&b),
                "expected REJECT: name with {label}"
            );
        }
        // A multi-byte Unicode codepoint (non-ASCII) in the name.
        assert!(
            !is_valid_store_base(&base(&format!("ok{}ok", '\u{1F600}'))),
            "expected REJECT: name with a Unicode codepoint"
        );

        // ---- REJECT: Nix-forbidden dot components ----
        assert!(!is_valid_store_base(&base(".")), "name == .");
        assert!(!is_valid_store_base(&base("..")), "name == ..");
        assert!(
            !is_valid_store_base(&base(".-x")),
            "first dash-component is ."
        );
        assert!(
            !is_valid_store_base(&base("..-x")),
            "first dash-component is .."
        );

        // ---- REJECT: pathological length / empty name ----
        assert!(!is_valid_store_base(&base(&name_212)), "212-byte name");
        assert!(!is_valid_store_base(&format!("{H32}-")), "empty name");

        // ---- REJECT: malformed hash / hyphen structure ----
        assert!(!is_valid_store_base(H32), "hash only, no hyphen/name");
        assert!(
            !is_valid_store_base(&format!("{H32}name")),
            "valid-length base but no hyphen at byte 32"
        );
        assert!(
            !is_valid_store_base(&format!("e{}-name", &H32[1..])),
            "hash byte 0 is out of the base-32 alphabet"
        );
    }

    #[test]
    fn path_info_rejects_reference_with_illegal_name() {
        let q = sp(BASE);
        // Each reference below has a valid 32-char hash + `-` but an ILLEGAL
        // name. These slip through the old (too loose) grammar, which only
        // checked the hash, hyphen, length, and absence of `/`; the tightened
        // checkName mirror must reject them at the parser contract level
        // (ParseError::PathInfoEntryReference), proving the helper is wired in.
        let illegal_refs: [String; 3] = [
            format!("{H32}-.-traversal"), // forbidden dot-component
            format!("{H32}-bad name"),    // space in the name
            format!("{H32}-."),           // name exactly `.`
        ];
        for bad in illegal_refs {
            let mut e = v2_entry();
            e["references"] = serde_json::json!([bad]);
            assert_eq!(
                parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap_err(),
                ParseError::PathInfoEntryReference
            );
        }
        // Sanity: a well-formed reference is still accepted => Hit.
        let mut e = v2_entry();
        e["references"] = serde_json::json!([format!("{H32}-glibc-2.39")]);
        assert_eq!(
            parse_path_info(single(2, "/nix/store", e).as_bytes(), &q, false).unwrap(),
            PathInfoProbe::Hit(PathInfoHit)
        );
    }

    #[test]
    fn storepath_is_not_serialize() {
        // StorePath deliberately has no Serialize impl: it can never be embedded
        // in a report. We assert the type carries no serde derive by confirming
        // its Debug/Display stay redacted regardless of content.
        let p = sp(BASE);
        let dbg = format!("{p:?}");
        let disp = format!("{p}");
        assert_eq!(dbg, "StorePath(<redacted>)");
        assert_eq!(disp, "<redacted store path>");
        // base()/render() are the only inspectors; they are crate-private and
        // used solely to drive the next command.
        assert_eq!(p.base(), BASE);
        assert_eq!(p.render(), abs(BASE));
    }
}
