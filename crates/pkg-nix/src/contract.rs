//! The validated `pkg`-owned request/report contract types that cross the
//! [`NixAdapter`](crate::NixAdapter) boundary, plus the public size-capped JSON
//! codec that is the single decode boundary for serialized reports.
//!
//! # What lives here
//!
//! - [`SchemaVersion`] and [`MethodKind`]: the closed schema/method vocabulary.
//! - [`NixVersion`] / [`VersionInfo`]: the managed-Nix version and the upstream
//!   per-command JSON format versions an adapter accepts/rejects
//!   (`plans/01` §11, `plans/09` §4.1).
//! - The seven unprivileged methods' request/report types, plus maintenance
//!   building blocks, all validated `pkg-nix` types that
//!   **compose `pkg-core` strong types** (`StorePath`, `NarHash`,
//!   `AttributePath`, `System`, `NixpkgsRevision`, `OutputSelection`, …).
//! - [`RootName`] / [`RootRef`]: validated, traversal-safe maintenance
//!   root-set naming.
//! - [`JsonCodec`]: the public size-capped decode boundary.
//!
//! # Serialization discipline
//!
//! Every serialized type carries an explicit `schemaVersion` (currently
//! [`SchemaVersion::CURRENT`] = 1), uses stable camelCase names, rejects unknown
//! fields strictly on decode, serializes deterministically, and decodes
//! fail-closed. `pkg-core` stays serde-free; all (de)serialization here goes
//! through **crate-private wire DTOs** with explicit, fallible conversion to
//! the validated public types (`plans/09` §4.2). Raw, version-specific Nix CLI
//! JSON is **not** modeled here; a real adapter normalizes into these reports
//! later, requesting explicit upstream format versions and rejecting any it
//! does not expect (`plans/01` §11).

use crate::error::{BoundedSummary, MalformedKind, NixAdapterError};
use pkg_core::channel::NixpkgsRevision;
use pkg_core::identity::{DerivationPath, NarHash, OutputName, StorePath};
use pkg_core::selector::{AttributePath, OutputSelection};
use pkg_core::system::System;
use serde::de::{DeserializeOwned, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Bounded bounds (checked invariants: nonempty, unique, bounded strings and
// collections, checked byte totals).
// ---------------------------------------------------------------------------

/// The single accepted current `pkg` contract schema version (`plans/05` §5).
const SCHEMA_VERSION_CURRENT: u32 = 1;

/// Maximum byte length of a managed-Nix version string.
const MAX_NIX_VERSION: usize = 64;
/// Maximum byte length of an opaque build-approval operation id.
const MAX_OPERATION_ID: usize = 64;
/// Maximum byte length of a single Nix signature (`name:base64`).
const MAX_SIGNATURE_LEN: usize = 256;
/// Maximum byte length of a validated GC-root name.
const MAX_ROOT_NAME: usize = 128;
/// Maximum byte length of an absolute GC-root reference path.
const MAX_ROOT_PATH: usize = 4096;

/// Maximum number of signatures on a single path-info report.
const MAX_SIGNATURES: usize = 1024;
/// Maximum total bytes of signatures on a single path-info report.
const MAX_SIGNATURES_BYTES: usize = 256 * 1024;
/// Maximum number of store-path references on a single path-info report.
const MAX_REFERENCES: usize = 65_536;
/// Maximum total bytes of references on a single path-info report.
const MAX_REFERENCES_BYTES: usize = 16 * 1024 * 1024;
/// Maximum number of build targets in one build request.
const MAX_TARGETS: usize = 1024;
/// Maximum total bytes of build targets in one build request.
const MAX_TARGETS_BYTES: usize = 1024 * 1024;
/// Maximum number of explicit output names in one eval-realize request.
/// Realization/build outputs already share [`MAX_TARGETS`]; the eval selection
/// is a per-evaluation output list, bounded conservatively at the same count.
const MAX_EVAL_OUTPUTS: usize = 1024;
/// Maximum total bytes of explicit output names in one eval-realize request.
const MAX_EVAL_OUTPUTS_BYTES: usize = 256 * 1024;
/// Maximum number of per-output entries in one realization report. Aligned
/// with the build/output bounds; a realization report names the outputs of a
/// single evaluation.
const MAX_REALIZATION_OUTPUTS: usize = 1024;
/// Maximum total bytes of output-name + store-path pairs in one realization
/// report.
const MAX_REALIZATION_OUTPUTS_BYTES: usize = 1024 * 1024;
/// Maximum number of paths in one verify request.
const MAX_PATH_LIST: usize = 65_536;
/// Maximum total bytes of paths in one verify request.
const MAX_PATH_LIST_BYTES: usize = 16 * 1024 * 1024;
/// Maximum number of paths reported collected by one gc report.
const MAX_GC_COLLECTED: usize = 1_048_576;
/// Maximum total bytes of paths reported collected by one gc report.
const MAX_GC_COLLECTED_BYTES: usize = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Crate-private decode/encode helpers.
// ---------------------------------------------------------------------------

/// Builds a redacted [`NixAdapterError::ValidationFailure`] from a static
/// category name. The name never includes the offending value.
fn invalid(what: &'static str) -> NixAdapterError {
    NixAdapterError::ValidationFailure {
        summary: BoundedSummary::new(what),
    }
}

/// Rejects input whose byte length exceeds the codec cap, **before** parsing.
fn check_size(codec: &JsonCodec, bytes: &[u8]) -> Result<(), NixAdapterError> {
    if bytes.len() > codec.limit() {
        return Err(NixAdapterError::OversizedInput {
            limit_bytes: codec.limit(),
        });
    }
    Ok(())
}

/// Maps a `serde_json` error into a redacted [`NixAdapterError`]: excessive
/// nesting (recursion-limit) is distinguished from other JSON problems, and
/// no payload bytes are retained.
///
/// serde_json `1.0.151` classifies `RecursionLimitExceeded` under
/// `Category::Syntax` and exposes no dedicated predicate; the stable,
/// pinned-version Display text (`"recursion limit exceeded"`) is the
/// documented signal. The crate's serde_json pin is exact, so this text is
/// fixed for the lock; a future pin bump is covered by the excessive-nesting
/// contract test.
fn map_json_err(e: serde_json::Error) -> NixAdapterError {
    if e.to_string().contains("recursion limit exceeded") {
        NixAdapterError::MalformedPayload {
            kind: MalformedKind::ExcessiveNesting,
        }
    } else {
        NixAdapterError::MalformedPayload {
            kind: MalformedKind::Json,
        }
    }
}

/// Size-checks then strictly parses `bytes` into a wire DTO `D`, preserving
/// serde_json's default recursion protection and rejecting trailing data and
/// unknown fields (via `#[serde(deny_unknown_fields)]` on each DTO).
fn parse_dto<D: DeserializeOwned>(codec: &JsonCodec, bytes: &[u8]) -> Result<D, NixAdapterError> {
    check_size(codec, bytes)?;
    serde_json::from_slice::<D>(bytes).map_err(map_json_err)
}

/// Rejects an unsupported observed schema version.
fn check_schema(observed: u32) -> Result<(), NixAdapterError> {
    if observed == SCHEMA_VERSION_CURRENT {
        Ok(())
    } else {
        Err(NixAdapterError::UnsupportedSchemaVersion { observed })
    }
}

/// Deterministically serializes a wire DTO to compact JSON bytes. This cannot
/// fail for the DTOs defined here (no floats, no non-string map keys); the
/// impossible serde error is mapped to a redacted validation failure rather
/// than a panic.
fn to_json<T: Serialize>(value: &T) -> Result<Vec<u8>, NixAdapterError> {
    serde_json::to_vec(value).map_err(|_| invalid("serialization"))
}

/// Rejects a collection whose string keys contain a duplicate (redacted).
fn ensure_unique_strings<'a, I>(iter: I, what: &'static str) -> Result<(), NixAdapterError>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = HashSet::new();
    for s in iter {
        if !seen.insert(s) {
            return Err(invalid(what));
        }
    }
    Ok(())
}

/// Bounds a collection of string-like items by **count and total bytes**,
/// using **checked accumulation** and failing closed on overflow.
///
/// This enforces the checked-byte-total invariant: the byte total is built with
/// [`usize::checked_add`] rather than an unchecked [`Iterator::sum`], so a
/// pathologically large collection surfaces as a bounded
/// [`NixAdapterError::ValidationFailure`] instead of silently wrapping. Both
/// the count and the byte total are checked against their caps **after** each
/// item, so the first over-cap item short-circuits.
fn check_size_bounds<'a, I>(
    items: I,
    max_count: usize,
    max_bytes: usize,
    what: &'static str,
) -> Result<(), NixAdapterError>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut count = 0usize;
    let mut total = 0usize;
    for s in items {
        count = count
            .checked_add(1)
            .ok_or_else(|| invalid("count overflow"))?;
        total = total
            .checked_add(s.len())
            .ok_or_else(|| invalid("size overflow"))?;
        if count > max_count || total > max_bytes {
            return Err(invalid(what));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bounded serde wrappers (crate-private).
//
// These enforce per-collection count and checked total-byte caps **during**
// deserialization (while visiting), before unbounded allocation or promotion,
// and (for maps) reject duplicate keys rather than last-wins. They serialize
// deterministically in the existing wire shapes (JSON array / object). A cap
// hit during decode becomes a `serde_json::Error`, which [`map_json_err`] maps
// to the redacted [`NixAdapterError::MalformedPayload`] category; no raw data
// or error strings are retained. The public constructors remain as
// defense-in-depth and keep their current [`NixAdapterError::ValidationFailure`]
// behavior.
//
// The sequence/map `size_hint` is never trusted: the reserved capacity is
// capped at the count cap, and visiting always stops at `max_count + 1`.
// ---------------------------------------------------------------------------

/// Redacted category string for a deserialization-time collection cap. The
/// message is discarded by [`map_json_err`]; it is a category only.
const COLLECTION_CAP_EXCEEDED: &str = "collection cap exceeded";
/// Redacted category string for a duplicate map key during deserialization.
const DUPLICATE_MAP_KEY: &str = "duplicate map key";

/// A `Vec<String>` deserialized under count + checked total-byte caps applied
/// **while visiting**, so a multi-million-element minimal array is rejected
/// before unbounded allocation. Serializes as a plain JSON array of strings,
/// preserving the existing wire shape.
struct BoundedStringSeq(Vec<String>);

impl BoundedStringSeq {
    /// Wraps an already-validated vector (used by encode).
    fn from_vec(items: Vec<String>) -> Self {
        Self(items)
    }

    /// Consumes the wrapper, returning the inner vector.
    fn into_inner(self) -> Vec<String> {
        self.0
    }

    /// Deserializes a JSON array of strings, enforcing `max_count` elements and
    /// a checked `max_bytes` total of all string bytes **while visiting**. The
    /// sequence `size_hint` is never trusted: the reserved capacity is capped at
    /// `max_count`, and visiting always stops at `max_count + 1`.
    fn deserialize_bounded<'de, D>(
        deserializer: D,
        max_count: usize,
        max_bytes: usize,
    ) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StringSeqVisitor {
            max_count: usize,
            max_bytes: usize,
        }

        impl<'de> Visitor<'de> for StringSeqVisitor {
            type Value = BoundedStringSeq;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence of strings")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                // Never trust an unbounded size_hint: cap the reserved
                // capacity at the count cap.
                let reserve = seq.size_hint().unwrap_or(0).min(self.max_count);
                let mut items = Vec::with_capacity(reserve);
                let mut total = 0usize;
                while let Some(s) = seq.next_element::<String>()? {
                    total = total
                        .checked_add(s.len())
                        .ok_or_else(|| serde::de::Error::custom(COLLECTION_CAP_EXCEEDED))?;
                    items.push(s);
                    // Stop at max+1 on count, or as soon as the byte budget is
                    // exceeded.
                    if items.len() > self.max_count || total > self.max_bytes {
                        return Err(serde::de::Error::custom(COLLECTION_CAP_EXCEEDED));
                    }
                }
                Ok(BoundedStringSeq(items))
            }
        }

        deserializer.deserialize_seq(StringSeqVisitor {
            max_count,
            max_bytes,
        })
    }
}

impl Serialize for BoundedStringSeq {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

/// A `Vec<T>` of complex wire elements deserialized under a count cap applied
/// **while visiting**. Serializes as a plain JSON array. Each element is itself
/// strictly validated (deny-unknown-fields plus its own promotion); the cap
/// bounds how many are materialized before promotion.
struct BoundedSeq<T>(Vec<T>);

impl<T> BoundedSeq<T> {
    /// Wraps an already-validated vector (used by encode).
    fn from_vec(items: Vec<T>) -> Self {
        Self(items)
    }

    /// Consumes the wrapper, returning the inner vector.
    fn into_inner(self) -> Vec<T> {
        self.0
    }

    /// Deserializes a JSON array of `T`, enforcing `max_count` elements
    /// **while visiting**. The sequence `size_hint` is never trusted: the
    /// reserved capacity is capped at `max_count`, and visiting always stops at
    /// `max_count + 1`.
    fn deserialize_bounded<'de, D>(deserializer: D, max_count: usize) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        struct SeqVisitor<T>(usize, PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for SeqVisitor<T> {
            type Value = BoundedSeq<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded sequence")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let reserve = seq.size_hint().unwrap_or(0).min(self.0);
                let mut items = Vec::with_capacity(reserve);
                while let Some(elem) = seq.next_element::<T>()? {
                    items.push(elem);
                    if items.len() > self.0 {
                        return Err(serde::de::Error::custom(COLLECTION_CAP_EXCEEDED));
                    }
                }
                Ok(BoundedSeq(items))
            }
        }

        deserializer.deserialize_seq(SeqVisitor(max_count, PhantomData))
    }
}

impl<T: Serialize> Serialize for BoundedSeq<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

/// A `BTreeMap<String, String>` deserialized under count + checked
/// total-key/value-byte caps, **rejecting duplicate keys** during visiting
/// rather than last-wins. Serializes as a plain JSON object, preserving the
/// existing wire shape.
struct BoundedUniqueStringMap(BTreeMap<String, String>);

impl BoundedUniqueStringMap {
    /// Wraps an already-validated map (used by encode).
    fn from_map(entries: BTreeMap<String, String>) -> Self {
        Self(entries)
    }

    /// Consumes the wrapper, returning the inner map.
    fn into_inner(self) -> BTreeMap<String, String> {
        self.0
    }

    /// Deserializes a JSON object of string→string, enforcing `max_count`
    /// entries, a checked `max_bytes` total of all key+value bytes, and
    /// **rejecting duplicate keys** (rather than last-wins). The map
    /// `size_hint` is never trusted; visiting always stops at `max_count + 1`.
    fn deserialize_bounded<'de, D>(
        deserializer: D,
        max_count: usize,
        max_bytes: usize,
    ) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StringMapVisitor {
            max_count: usize,
            max_bytes: usize,
        }

        impl<'de> Visitor<'de> for StringMapVisitor {
            type Value = BoundedUniqueStringMap;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of strings")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut entries = BTreeMap::new();
                let mut total = 0usize;
                while let Some((key, value)) = map.next_entry::<String, String>()? {
                    total = total
                        .checked_add(key.len())
                        .and_then(|t| t.checked_add(value.len()))
                        .ok_or_else(|| serde::de::Error::custom(COLLECTION_CAP_EXCEEDED))?;
                    // Stop at max+1 on count, or as soon as the byte budget is
                    // exceeded.
                    if entries.len() >= self.max_count || total > self.max_bytes {
                        return Err(serde::de::Error::custom(COLLECTION_CAP_EXCEEDED));
                    }
                    // Reject duplicate keys during deserialization.
                    if entries.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom(DUPLICATE_MAP_KEY));
                    }
                }
                Ok(BoundedUniqueStringMap(entries))
            }
        }

        deserializer.deserialize_map(StringMapVisitor {
            max_count,
            max_bytes,
        })
    }
}

impl Serialize for BoundedUniqueStringMap {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

/// Deserializes an `Option<BoundedStringSeq>` (`null` → `None`, array →
/// bounded), used by the eval-realize `outputs` field to preserve the
/// default-vs-explicit wire shape under a cap.
fn deserialize_optional_string_seq<'de, D>(
    deserializer: D,
    max_count: usize,
    max_bytes: usize,
) -> Result<Option<BoundedStringSeq>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OptionalStringSeqVisitor {
        max_count: usize,
        max_bytes: usize,
    }

    impl<'de> Visitor<'de> for OptionalStringSeqVisitor {
        type Value = Option<BoundedStringSeq>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a sequence of strings or null")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_some<D2>(self, deserializer: D2) -> Result<Self::Value, D2::Error>
        where
            D2: Deserializer<'de>,
        {
            BoundedStringSeq::deserialize_bounded(deserializer, self.max_count, self.max_bytes)
                .map(Some)
        }
    }

    deserializer.deserialize_option(OptionalStringSeqVisitor {
        max_count,
        max_bytes,
    })
}

// Per-field `deserialize_with` adapters: each maps a wire collection to its
// existing constructor count/byte cap. They are tiny one-liners because the
// capped visitors carry all the enforcement.

fn deserialize_eval_outputs<'de, D>(deserializer: D) -> Result<Option<BoundedStringSeq>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_optional_string_seq(deserializer, MAX_EVAL_OUTPUTS, MAX_EVAL_OUTPUTS_BYTES)
}

fn deserialize_realization_outputs<'de, D>(
    deserializer: D,
) -> Result<BoundedUniqueStringMap, D::Error>
where
    D: Deserializer<'de>,
{
    BoundedUniqueStringMap::deserialize_bounded(
        deserializer,
        MAX_REALIZATION_OUTPUTS,
        MAX_REALIZATION_OUTPUTS_BYTES,
    )
}

fn deserialize_references<'de, D>(deserializer: D) -> Result<BoundedStringSeq, D::Error>
where
    D: Deserializer<'de>,
{
    BoundedStringSeq::deserialize_bounded(deserializer, MAX_REFERENCES, MAX_REFERENCES_BYTES)
}

fn deserialize_signatures<'de, D>(deserializer: D) -> Result<BoundedStringSeq, D::Error>
where
    D: Deserializer<'de>,
{
    BoundedStringSeq::deserialize_bounded(deserializer, MAX_SIGNATURES, MAX_SIGNATURES_BYTES)
}

fn deserialize_build_targets<'de, D>(deserializer: D) -> Result<BoundedStringSeq, D::Error>
where
    D: Deserializer<'de>,
{
    BoundedStringSeq::deserialize_bounded(deserializer, MAX_TARGETS, MAX_TARGETS_BYTES)
}

fn deserialize_build_outputs<'de, D>(deserializer: D) -> Result<BoundedStringSeq, D::Error>
where
    D: Deserializer<'de>,
{
    BoundedStringSeq::deserialize_bounded(deserializer, MAX_TARGETS, MAX_TARGETS_BYTES)
}

fn deserialize_verify_paths<'de, D>(deserializer: D) -> Result<BoundedStringSeq, D::Error>
where
    D: Deserializer<'de>,
{
    BoundedStringSeq::deserialize_bounded(deserializer, MAX_PATH_LIST, MAX_PATH_LIST_BYTES)
}

fn deserialize_verify_results<'de, D>(
    deserializer: D,
) -> Result<BoundedSeq<PathVerifyResultWire>, D::Error>
where
    D: Deserializer<'de>,
{
    BoundedSeq::<PathVerifyResultWire>::deserialize_bounded(deserializer, MAX_PATH_LIST)
}

fn deserialize_gc_collected<'de, D>(deserializer: D) -> Result<BoundedStringSeq, D::Error>
where
    D: Deserializer<'de>,
{
    BoundedStringSeq::deserialize_bounded(deserializer, MAX_GC_COLLECTED, MAX_GC_COLLECTED_BYTES)
}

// ===========================================================================
// Schema version and method vocabulary
// ===========================================================================

/// The `pkg` contract schema version carried by every serialized report.
///
/// This is the **product's own** contract version (`plans/05` §5), deliberately
/// decoupled from Nix's upstream per-command JSON format versions
/// (`plans/09` §4.2). Exactly one version is currently accepted
/// ([`SchemaVersion::CURRENT`] = 1); the type is a checked, closed value so an
/// unsupported version cannot be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SchemaVersion(u32);

impl SchemaVersion {
    /// The single accepted schema version value.
    pub const CURRENT_VALUE: u32 = SCHEMA_VERSION_CURRENT;

    /// The current (only accepted) schema version.
    pub const CURRENT: SchemaVersion = SchemaVersion(SCHEMA_VERSION_CURRENT);

    /// Returns the current schema version.
    #[must_use]
    pub const fn current() -> Self {
        Self::CURRENT
    }

    /// Constructs a schema version, accepting only the current value.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::UnsupportedSchemaVersion`] for any value
    /// other than [`SchemaVersion::CURRENT_VALUE`].
    pub fn new(value: u32) -> Result<Self, NixAdapterError> {
        if value == Self::CURRENT_VALUE {
            Ok(Self(value))
        } else {
            Err(NixAdapterError::UnsupportedSchemaVersion { observed: value })
        }
    }

    /// Returns the numeric schema version.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Which [`NixAdapter`](crate::NixAdapter) method a call/transcript entry is.
///
/// This is the `pkg-nix` contract enum shared by the trait and the
/// `pkg-testkit` transcript (`plans/09` §4.4), so `pkg-testkit` depends on
/// `pkg-nix` one way and **never** the reverse. Stable camelCase names are
/// available via [`MethodKind::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MethodKind {
    /// `version()`.
    Version,
    /// `eval_realize()`.
    EvalRealize,
    /// `path_info()`.
    PathInfo,
    /// `substitute()`.
    Substitute,
    /// `build()`.
    Build,
    /// `verify()`.
    Verify,
    /// `gc()`.
    Gc,
}

impl MethodKind {
    /// All seven unprivileged methods, in canonical order.
    pub const ALL: [MethodKind; 7] = [
        MethodKind::Version,
        MethodKind::EvalRealize,
        MethodKind::PathInfo,
        MethodKind::Substitute,
        MethodKind::Build,
        MethodKind::Verify,
        MethodKind::Gc,
    ];

    /// Returns the stable camelCase name of this method.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MethodKind::Version => "version",
            MethodKind::EvalRealize => "evalRealize",
            MethodKind::PathInfo => "pathInfo",
            MethodKind::Substitute => "substitute",
            MethodKind::Build => "build",
            MethodKind::Verify => "verify",
            MethodKind::Gc => "gc",
        }
    }
}

impl fmt::Display for MethodKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MethodKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "version" => Ok(MethodKind::Version),
            "evalRealize" => Ok(MethodKind::EvalRealize),
            "pathInfo" => Ok(MethodKind::PathInfo),
            "substitute" => Ok(MethodKind::Substitute),
            "build" => Ok(MethodKind::Build),
            "verify" => Ok(MethodKind::Verify),
            "gc" => Ok(MethodKind::Gc),
            _ => Err(()),
        }
    }
}

// ===========================================================================
// Managed-Nix version and accepted upstream per-command formats
// ===========================================================================

/// A bounded, validated managed-Nix version string (e.g. `2.33.5`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NixVersion(String);

impl NixVersion {
    /// Validates and constructs a managed-Nix version.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] if `value` is empty,
    /// longer than 64 bytes, or contains bytes other than ASCII
    /// alphanumeric, `.`, `+`, or `-`.
    pub fn new(value: &str) -> Result<Self, NixAdapterError> {
        if is_nix_version(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(invalid("invalid nix version"))
        }
    }

    /// Returns the version string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Returns `true` if `s` is a valid [`NixVersion`].
fn is_nix_version(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_NIX_VERSION
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

/// A validated, positive upstream Nix JSON format version for a single command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FormatVersion(NonZeroU32);

impl FormatVersion {
    /// Constructs a format version, rejecting zero.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] for `0`.
    pub fn new(value: u32) -> Result<Self, NixAdapterError> {
        NonZeroU32::new(value)
            .map(Self)
            .ok_or_else(|| invalid("invalid format version"))
    }

    /// Returns the numeric format version.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// The upstream Nix JSON format versions an adapter accepts, per command.
///
/// Today only `nix store path-info --json-format` exposes a negotiated upstream
/// format version among this contract's methods (`plans/01` §11); as further
/// commands gain format versions, named fields are added here. Anything not
/// listed is rejected by the real adapter before normalization
/// ([`NixAdapterError::UnsupportedUpstreamFormat`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedFormats {
    path_info: FormatVersion,
}

impl AcceptedFormats {
    /// Constructs the accepted-format set.
    #[must_use]
    pub fn new(path_info: FormatVersion) -> Self {
        Self { path_info }
    }

    /// Returns the accepted `nix store path-info` JSON format version.
    #[must_use]
    pub const fn path_info(&self) -> FormatVersion {
        self.path_info
    }
}

/// A read-only capability probe: the pinned managed-Nix version plus the
/// upstream per-command JSON format versions this adapter accepts/rejects
/// (`plans/01` §11, `plans/09` §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    nix_version: NixVersion,
    accepted_formats: AcceptedFormats,
}

impl VersionInfo {
    /// Constructs a `version()` report.
    #[must_use]
    pub fn new(nix_version: NixVersion, accepted_formats: AcceptedFormats) -> Self {
        Self {
            nix_version,
            accepted_formats,
        }
    }

    /// Returns the managed-Nix version.
    #[must_use]
    pub fn nix_version(&self) -> &NixVersion {
        &self.nix_version
    }

    /// Returns the accepted upstream per-command format versions.
    #[must_use]
    pub fn accepted_formats(&self) -> &AcceptedFormats {
        &self.accepted_formats
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionInfoWire {
    schema_version: u32,
    nix_version: String,
    accepted_formats: AcceptedFormatsWire,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AcceptedFormatsWire {
    path_info: u32,
}

impl VersionInfo {
    /// Deterministically encodes this report to JSON bytes.
    ///
    /// # Errors
    ///
    /// Only if serde itself fails (impossible for this shape); mapped to a
    /// redacted validation failure rather than a panic.
    pub fn encode(&self) -> Result<Vec<u8>, NixAdapterError> {
        let dto = VersionInfoWire {
            schema_version: SCHEMA_VERSION_CURRENT,
            nix_version: self.nix_version.as_str().to_owned(),
            accepted_formats: AcceptedFormatsWire {
                path_info: self.accepted_formats.path_info().get(),
            },
        };
        to_json(&dto)
    }

    /// Size-checks, strictly parses, schema-checks, and promotes JSON bytes into
    /// a validated [`VersionInfo`].
    pub fn decode(codec: &JsonCodec, bytes: &[u8]) -> Result<Self, NixAdapterError> {
        let dto: VersionInfoWire = parse_dto(codec, bytes)?;
        check_schema(dto.schema_version)?;
        let nix_version = NixVersion::new(&dto.nix_version)?;
        let path_info = FormatVersion::new(dto.accepted_formats.path_info)?;
        Ok(Self::new(nix_version, AcceptedFormats::new(path_info)))
    }
}

// ===========================================================================
// Signatures (path-info component)
// ===========================================================================

/// A validated Nix path signature of the form `name:standard-base64`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Signature(String);

impl Signature {
    /// Validates and constructs a signature.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] unless `value` is a
    /// single `name:base64` pair with a nonempty `[A-Za-z0-9._-]` name and
    /// well-formed standard base64 (standard-alphabet, correct padding
    /// placement and count) within 256 bytes. The validator checks the
    /// alphabet and padding placement/count but **not** zero low bits, so
    /// "canonical" padding is intentionally not claimed.
    pub fn new(value: &str) -> Result<Self, NixAdapterError> {
        if is_signature(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(invalid("invalid signature"))
        }
    }

    /// Returns the signature string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Returns `true` if `s` is a valid `name:base64` Nix signature.
fn is_signature(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_SIGNATURE_LEN {
        return false;
    }
    let Some(colon) = bytes.iter().position(|&b| b == b':') else {
        return false;
    };
    let (name, rest) = bytes.split_at(colon);
    let sig = &rest[1..]; // skip the colon
    if name.is_empty() || sig.is_empty() {
        return false;
    }
    name.iter()
        .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && is_standard_base64(sig)
}

/// Returns `true` if `s` is well-formed standard-alphabet base64 with standard
/// padding placement and count: length is a multiple of 4, `=` appears only as
/// trailing padding of at most two bytes, and every byte is in the standard
/// alphabet (`A`-`Z`, `a`-`z`, `0`-`9`, `+`, `/`) or `=`. This checks the
/// alphabet, padding placement, and padding count; it does **not** verify the
/// low bits of the last sextet are zero, so padding correctness (in the sense
/// of zero low bits) is not asserted.
fn is_standard_base64(s: &[u8]) -> bool {
    if s.is_empty() || !s.len().is_multiple_of(4) {
        return false;
    }
    if !s
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    {
        return false;
    }
    if let Some(eq) = s.iter().position(|&b| b == b'=') {
        // All bytes from the first '=' onward must be '=', and at most two.
        if !s[eq..].iter().all(|&b| b == b'=') || s.len() - eq > 2 {
            return false;
        }
    }
    true
}

// ===========================================================================
// eval_realize
// ===========================================================================

/// A request to evaluate and realize a selector into store outputs
/// (`plans/09` §4.1). Built by the later resolver from a selector plus the
/// accepted channel descriptor; carries **no** trust/flag knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalRealizeRequest {
    attribute: AttributePath,
    system: System,
    nixpkgs_revision: NixpkgsRevision,
    nixpkgs_nar_hash: NarHash,
    outputs: OutputSelection,
}

impl EvalRealizeRequest {
    /// Constructs an eval-realize request from already-validated strong types.
    ///
    /// `EvalRealizeRequest` is advertised as a validated, ready-to-serialize
    /// value, so the same count and checked total-byte caps the wire codec
    /// enforces on explicit outputs (the crate's private `MAX_EVAL_OUTPUTS` /
    /// `MAX_EVAL_OUTPUTS_BYTES`) are enforced **here**, at construction, not
    /// only at decode. The [`OutputSelection::default_selection`] is always
    /// accepted; an explicit selection is bounded by count and checked byte
    /// total, so a `pkg-core` [`OutputSelection`] larger than the wire caps
    /// cannot reach [`EvalRealizeRequest::encode`], which only wraps
    /// already-validated data.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] if an explicit
    /// `outputs` selection exceeds the count or total-byte cap.
    pub fn new(
        attribute: AttributePath,
        system: System,
        nixpkgs_revision: NixpkgsRevision,
        nixpkgs_nar_hash: NarHash,
        outputs: OutputSelection,
    ) -> Result<Self, NixAdapterError> {
        if let Some(names) = outputs.explicit_outputs() {
            check_size_bounds(
                names.iter().map(OutputName::as_str),
                MAX_EVAL_OUTPUTS,
                MAX_EVAL_OUTPUTS_BYTES,
                "too many eval outputs",
            )?;
        }
        Ok(Self {
            attribute,
            system,
            nixpkgs_revision,
            nixpkgs_nar_hash,
            outputs,
        })
    }

    /// Returns the resolved Nixpkgs attribute path.
    #[must_use]
    pub fn attribute(&self) -> &AttributePath {
        &self.attribute
    }

    /// Returns the target system.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns the pinned Nixpkgs revision.
    #[must_use]
    pub fn nixpkgs_revision(&self) -> &NixpkgsRevision {
        &self.nixpkgs_revision
    }

    /// Returns the NAR-hash pin of the Nixpkgs source.
    #[must_use]
    pub fn nixpkgs_nar_hash(&self) -> &NarHash {
        &self.nixpkgs_nar_hash
    }

    /// Returns the output selection.
    #[must_use]
    pub fn outputs(&self) -> &OutputSelection {
        &self.outputs
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvalRealizeRequestWire {
    schema_version: u32,
    attribute: String,
    system: String,
    nixpkgs_revision: String,
    nixpkgs_nar_hash: String,
    #[serde(default, deserialize_with = "deserialize_eval_outputs")]
    outputs: Option<BoundedStringSeq>,
}

impl EvalRealizeRequest {
    /// Deterministically encodes this request to JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, NixAdapterError> {
        let outputs = self.outputs.explicit_outputs().map(|xs| {
            BoundedStringSeq::from_vec(
                xs.iter()
                    .map(pkg_core::identity::OutputName::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
            )
        });
        let dto = EvalRealizeRequestWire {
            schema_version: SCHEMA_VERSION_CURRENT,
            attribute: self.attribute.as_str().to_owned(),
            system: self.system.as_str().to_owned(),
            nixpkgs_revision: self.nixpkgs_revision.as_str().to_owned(),
            nixpkgs_nar_hash: self.nixpkgs_nar_hash.as_str().to_owned(),
            outputs,
        };
        to_json(&dto)
    }

    /// Size-checks, strictly parses, schema-checks, and promotes JSON bytes into
    /// a validated [`EvalRealizeRequest`].
    pub fn decode(codec: &JsonCodec, bytes: &[u8]) -> Result<Self, NixAdapterError> {
        let dto: EvalRealizeRequestWire = parse_dto(codec, bytes)?;
        check_schema(dto.schema_version)?;
        let attribute =
            AttributePath::new(&dto.attribute).map_err(|_| invalid("invalid attribute"))?;
        let system = System::from_str(&dto.system).map_err(|_| invalid("unknown system"))?;
        let nixpkgs_revision = NixpkgsRevision::new(&dto.nixpkgs_revision)
            .map_err(|_| invalid("invalid nixpkgs revision"))?;
        let nixpkgs_nar_hash =
            NarHash::new(&dto.nixpkgs_nar_hash).map_err(|_| invalid("invalid nar hash"))?;
        let outputs = match dto.outputs {
            None => OutputSelection::default_selection(),
            Some(seq) => {
                let names = seq.into_inner();
                let mut built = Vec::with_capacity(names.len());
                for n in &names {
                    built.push(OutputName::new(n).map_err(|_| invalid("invalid output name"))?);
                }
                OutputSelection::explicit(built).map_err(|_| invalid("invalid output selection"))?
            }
        };
        Self::new(
            attribute,
            system,
            nixpkgs_revision,
            nixpkgs_nar_hash,
            outputs,
        )
    }
}

/// The report returned by `eval_realize()`: the realized store path, its
/// deriver, and the per-output store paths (`plans/09` §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealizationReport {
    store_path: StorePath,
    deriver: DerivationPath,
    outputs: BTreeMap<OutputName, StorePath>,
}

impl RealizationReport {
    /// Constructs and validates a realization report.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] if `outputs` is empty or
    /// `store_path` is not among the `outputs` values.
    pub fn new(
        store_path: StorePath,
        deriver: DerivationPath,
        outputs: BTreeMap<OutputName, StorePath>,
    ) -> Result<Self, NixAdapterError> {
        if outputs.is_empty() {
            return Err(invalid("empty outputs"));
        }
        if !outputs.values().any(|p| p == &store_path) {
            return Err(invalid("primary store path not an output"));
        }
        // Defense-in-depth count + checked total key/value byte cap (also
        // enforced by the wire visitor during decode).
        let mut count = 0usize;
        let mut total = 0usize;
        for (name, path) in &outputs {
            count = count
                .checked_add(1)
                .ok_or_else(|| invalid("count overflow"))?;
            total = total
                .checked_add(name.as_str().len())
                .and_then(|t| t.checked_add(path.as_str().len()))
                .ok_or_else(|| invalid("size overflow"))?;
            if count > MAX_REALIZATION_OUTPUTS || total > MAX_REALIZATION_OUTPUTS_BYTES {
                return Err(invalid("too many outputs"));
            }
        }
        Ok(Self {
            store_path,
            deriver,
            outputs,
        })
    }

    /// Returns the primary (realized) store path.
    #[must_use]
    pub fn store_path(&self) -> &StorePath {
        &self.store_path
    }

    /// Returns the deriver.
    #[must_use]
    pub fn deriver(&self) -> &DerivationPath {
        &self.deriver
    }

    /// Returns the per-output store paths (sorted by output name).
    #[must_use]
    pub fn outputs(&self) -> &BTreeMap<OutputName, StorePath> {
        &self.outputs
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RealizationReportWire {
    schema_version: u32,
    store_path: String,
    deriver: String,
    #[serde(deserialize_with = "deserialize_realization_outputs")]
    outputs: BoundedUniqueStringMap,
}

impl RealizationReport {
    /// Deterministically encodes this report to JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, NixAdapterError> {
        let outputs = BoundedUniqueStringMap::from_map(
            self.outputs
                .iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.as_str().to_owned()))
                .collect::<BTreeMap<_, _>>(),
        );
        let dto = RealizationReportWire {
            schema_version: SCHEMA_VERSION_CURRENT,
            store_path: self.store_path.as_str().to_owned(),
            deriver: self.deriver.as_str().to_owned(),
            outputs,
        };
        to_json(&dto)
    }

    /// Size-checks, strictly parses, schema-checks, and promotes JSON bytes into
    /// a validated [`RealizationReport`].
    pub fn decode(codec: &JsonCodec, bytes: &[u8]) -> Result<Self, NixAdapterError> {
        let dto: RealizationReportWire = parse_dto(codec, bytes)?;
        check_schema(dto.schema_version)?;
        let store_path =
            StorePath::new(&dto.store_path).map_err(|_| invalid("invalid store path"))?;
        let deriver =
            DerivationPath::from_str(&dto.deriver).map_err(|_| invalid("invalid deriver"))?;
        let mut outputs = BTreeMap::new();
        for (name, path) in dto.outputs.into_inner() {
            let n = OutputName::new(&name).map_err(|_| invalid("invalid output name"))?;
            let p = StorePath::new(&path).map_err(|_| invalid("invalid output store path"))?;
            outputs.insert(n, p);
        }
        Self::new(store_path, deriver, outputs)
    }
}

// ===========================================================================
// path_info
// ===========================================================================

/// The report returned by `path_info()`: NAR hash, signatures, references, the
/// optional deriver, and NAR/closure sizes for one store path
/// (`plans/09` §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathInfoReport {
    store_path: StorePath,
    nar_hash: NarHash,
    signatures: Vec<Signature>,
    references: Vec<StorePath>,
    deriver: Option<DerivationPath>,
    nar_size: u64,
    closure_size: u64,
}

impl PathInfoReport {
    /// Constructs and validates a path-info report.
    ///
    /// Signatures and references are canonicalized (sorted, de-duplicated) and
    /// bounded. The closure size must be at least the NAR size, and the store
    /// path must not reference itself.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] on duplicates, an
    /// over-large collection, a self-reference, or an inverted size pair.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store_path: StorePath,
        nar_hash: NarHash,
        mut signatures: Vec<Signature>,
        mut references: Vec<StorePath>,
        deriver: Option<DerivationPath>,
        nar_size: u64,
        closure_size: u64,
    ) -> Result<Self, NixAdapterError> {
        if closure_size < nar_size {
            return Err(invalid("closure smaller than nar"));
        }
        // Signatures: canonical set.
        ensure_unique_strings(
            signatures.iter().map(Signature::as_str),
            "duplicate signature",
        )?;
        signatures.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        check_size_bounds(
            signatures.iter().map(Signature::as_str),
            MAX_SIGNATURES,
            MAX_SIGNATURES_BYTES,
            "too many signatures",
        )?;
        // References: canonical set, no self-reference.
        if references.iter().any(|r| r == &store_path) {
            return Err(invalid("self reference"));
        }
        ensure_unique_strings(
            references.iter().map(StorePath::as_str),
            "duplicate reference",
        )?;
        references.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        check_size_bounds(
            references.iter().map(StorePath::as_str),
            MAX_REFERENCES,
            MAX_REFERENCES_BYTES,
            "too many references",
        )?;
        Ok(Self {
            store_path,
            nar_hash,
            signatures,
            references,
            deriver,
            nar_size,
            closure_size,
        })
    }

    /// Returns the store path.
    #[must_use]
    pub fn store_path(&self) -> &StorePath {
        &self.store_path
    }

    /// Returns the NAR hash (sha256 SRI) of the store path.
    #[must_use]
    pub fn nar_hash(&self) -> &NarHash {
        &self.nar_hash
    }

    /// Returns the signatures (canonicalized: sorted, de-duplicated).
    #[must_use]
    pub fn signatures(&self) -> &[Signature] {
        &self.signatures
    }

    /// Returns the references (canonicalized: sorted, de-duplicated).
    #[must_use]
    pub fn references(&self) -> &[StorePath] {
        &self.references
    }

    /// Returns the optional deriver.
    #[must_use]
    pub fn deriver(&self) -> Option<&DerivationPath> {
        self.deriver.as_ref()
    }

    /// Returns the NAR size in bytes.
    #[must_use]
    pub const fn nar_size(&self) -> u64 {
        self.nar_size
    }

    /// Returns the closure NAR size in bytes.
    #[must_use]
    pub const fn closure_size(&self) -> u64 {
        self.closure_size
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PathInfoReportWire {
    schema_version: u32,
    store_path: String,
    nar_hash: String,
    nar_size: u64,
    closure_size: u64,
    deriver: Option<String>,
    #[serde(deserialize_with = "deserialize_references")]
    references: BoundedStringSeq,
    #[serde(deserialize_with = "deserialize_signatures")]
    signatures: BoundedStringSeq,
}

impl PathInfoReport {
    /// Deterministically encodes this report to JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, NixAdapterError> {
        let dto = PathInfoReportWire {
            schema_version: SCHEMA_VERSION_CURRENT,
            store_path: self.store_path.as_str().to_owned(),
            nar_hash: self.nar_hash.as_str().to_owned(),
            nar_size: self.nar_size,
            closure_size: self.closure_size,
            deriver: self
                .deriver
                .as_ref()
                .map(DerivationPath::as_str)
                .map(str::to_owned),
            references: BoundedStringSeq::from_vec(
                self.references
                    .iter()
                    .map(StorePath::as_str)
                    .map(str::to_owned)
                    .collect(),
            ),
            signatures: BoundedStringSeq::from_vec(
                self.signatures
                    .iter()
                    .map(Signature::as_str)
                    .map(str::to_owned)
                    .collect(),
            ),
        };
        to_json(&dto)
    }

    /// Size-checks, strictly parses, schema-checks, and promotes JSON bytes into
    /// a validated [`PathInfoReport`].
    pub fn decode(codec: &JsonCodec, bytes: &[u8]) -> Result<Self, NixAdapterError> {
        let dto: PathInfoReportWire = parse_dto(codec, bytes)?;
        check_schema(dto.schema_version)?;
        let store_path =
            StorePath::new(&dto.store_path).map_err(|_| invalid("invalid store path"))?;
        let nar_hash = NarHash::new(&dto.nar_hash).map_err(|_| invalid("invalid nar hash"))?;
        let deriver = match dto.deriver {
            None => None,
            Some(d) => Some(DerivationPath::from_str(&d).map_err(|_| invalid("invalid deriver"))?),
        };
        let sig_vec = dto.signatures.into_inner();
        let mut signatures = Vec::with_capacity(sig_vec.len());
        for s in sig_vec {
            signatures.push(Signature::new(&s)?);
        }
        let ref_vec = dto.references.into_inner();
        let mut references = Vec::with_capacity(ref_vec.len());
        for r in ref_vec {
            references.push(StorePath::new(&r).map_err(|_| invalid("invalid reference"))?);
        }
        Self::new(
            store_path,
            nar_hash,
            signatures,
            references,
            deriver,
            dto.nar_size,
            dto.closure_size,
        )
    }
}

// ===========================================================================
// substitute
// ===========================================================================

/// The closed outcome of a cache-only substitute (`plans/09` §4.1).
///
/// Trust/signature failures are **not** outcomes: they are
/// [`NixAdapterError`]. Only normal cache outcomes appear here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SubstituteOutcome {
    /// The path was fetched from a substituter.
    Fetched,
    /// The path was absent from all configured substituters.
    AbsentFromSubstituters,
    /// No binary substitute exists for this path (a build would be required).
    NoBinaryAvailable,
}

/// The report returned by `substitute()`: a cache-only outcome for one store
/// path (`plans/09` §4.1). Substitute is cache-only; trust or signature
/// failures are [`NixAdapterError`], never an outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstituteReport {
    store_path: StorePath,
    outcome: SubstituteOutcome,
}

impl SubstituteReport {
    /// Constructs a substitute report.
    #[must_use]
    pub fn new(store_path: StorePath, outcome: SubstituteOutcome) -> Self {
        Self {
            store_path,
            outcome,
        }
    }

    /// Returns the store path.
    #[must_use]
    pub fn store_path(&self) -> &StorePath {
        &self.store_path
    }

    /// Returns the cache-only outcome.
    #[must_use]
    pub const fn outcome(&self) -> SubstituteOutcome {
        self.outcome
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum SubstituteOutcomeWire {
    Fetched,
    AbsentFromSubstituters,
    NoBinaryAvailable,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubstituteReportWire {
    schema_version: u32,
    store_path: String,
    outcome: SubstituteOutcomeWire,
}

impl SubstituteReport {
    /// Deterministically encodes this report to JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, NixAdapterError> {
        let dto = SubstituteReportWire {
            schema_version: SCHEMA_VERSION_CURRENT,
            store_path: self.store_path.as_str().to_owned(),
            outcome: match self.outcome {
                SubstituteOutcome::Fetched => SubstituteOutcomeWire::Fetched,
                SubstituteOutcome::AbsentFromSubstituters => {
                    SubstituteOutcomeWire::AbsentFromSubstituters
                }
                SubstituteOutcome::NoBinaryAvailable => SubstituteOutcomeWire::NoBinaryAvailable,
            },
        };
        to_json(&dto)
    }

    /// Size-checks, strictly parses, schema-checks, and promotes JSON bytes into
    /// a validated [`SubstituteReport`].
    pub fn decode(codec: &JsonCodec, bytes: &[u8]) -> Result<Self, NixAdapterError> {
        let dto: SubstituteReportWire = parse_dto(codec, bytes)?;
        check_schema(dto.schema_version)?;
        let store_path =
            StorePath::new(&dto.store_path).map_err(|_| invalid("invalid store path"))?;
        let outcome = match dto.outcome {
            SubstituteOutcomeWire::Fetched => SubstituteOutcome::Fetched,
            SubstituteOutcomeWire::AbsentFromSubstituters => {
                SubstituteOutcome::AbsentFromSubstituters
            }
            SubstituteOutcomeWire::NoBinaryAvailable => SubstituteOutcome::NoBinaryAvailable,
        };
        Ok(Self::new(store_path, outcome))
    }
}

// ===========================================================================
// build approval + build request/report
// ===========================================================================

/// A bounded, validated opaque operation id carried by a build-approval
/// receipt.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OperationId(String);

impl OperationId {
    /// Validates and constructs an operation id (`[A-Za-z0-9_-]+`).
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] for empty, over-long, or
    /// non-`[A-Za-z0-9_-]` input.
    pub fn new(value: &str) -> Result<Self, NixAdapterError> {
        if is_operation_id(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(invalid("invalid operation id"))
        }
    }

    /// Returns the operation id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Returns `true` if `s` is a valid [`OperationId`].
fn is_operation_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_OPERATION_ID
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// A stable, **opaque** single-operation build-approval receipt carrying a
/// bounded operation id.
///
/// **PR-3 defines only this stable opaque carrier and its validation.** It is a
/// token carried through the trait; it **does not prove authorization**. PR-26
/// owns its production issuance, journal binding, single-use verification, and
/// rejection (`plans/09` §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildApprovalReceipt {
    operation_id: OperationId,
}

impl BuildApprovalReceipt {
    /// Constructs a receipt from a validated operation id.
    #[must_use]
    pub fn new(operation_id: OperationId) -> Self {
        Self { operation_id }
    }

    /// Returns the operation id.
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }
}

/// The closed status of a local build (`plans/04`, `plans/09` §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BuildStatus {
    /// The targets built locally and produced outputs.
    Built,
    /// A binary could not be acquired and a local build is not permitted
    /// (`ACQUIRE_NO_BINARY`, `plans/08` AC-S13).
    AcquireNoBinary,
}

/// A request for an approved, sandboxed local build (`plans/09` §4.1). Carries
/// **no** sandbox/substituter/key/builders/build-user knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRequest {
    targets: Vec<DerivationPath>,
    system: System,
    receipt: BuildApprovalReceipt,
}

impl BuildRequest {
    /// Constructs and validates a build request.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] if `targets` is empty,
    /// contains duplicates, or exceeds the bounded collection caps.
    pub fn new(
        targets: Vec<DerivationPath>,
        system: System,
        receipt: BuildApprovalReceipt,
    ) -> Result<Self, NixAdapterError> {
        if targets.is_empty() {
            return Err(invalid("empty build targets"));
        }
        ensure_unique_strings(
            targets.iter().map(DerivationPath::as_str),
            "duplicate build target",
        )?;
        check_size_bounds(
            targets.iter().map(DerivationPath::as_str),
            MAX_TARGETS,
            MAX_TARGETS_BYTES,
            "too many build targets",
        )?;
        Ok(Self {
            targets,
            system,
            receipt,
        })
    }

    /// Returns the derivation targets (caller order preserved).
    #[must_use]
    pub fn targets(&self) -> &[DerivationPath] {
        &self.targets
    }

    /// Returns the target system.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }

    /// Returns the opaque build-approval receipt.
    #[must_use]
    pub fn receipt(&self) -> &BuildApprovalReceipt {
        &self.receipt
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildRequestWire {
    schema_version: u32,
    #[serde(deserialize_with = "deserialize_build_targets")]
    targets: BoundedStringSeq,
    system: String,
    receipt: ReceiptWire,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReceiptWire {
    operation_id: String,
}

impl BuildRequest {
    /// Deterministically encodes this request to JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, NixAdapterError> {
        let dto = BuildRequestWire {
            schema_version: SCHEMA_VERSION_CURRENT,
            targets: BoundedStringSeq::from_vec(
                self.targets
                    .iter()
                    .map(DerivationPath::as_str)
                    .map(str::to_owned)
                    .collect(),
            ),
            system: self.system.as_str().to_owned(),
            receipt: ReceiptWire {
                operation_id: self.receipt.operation_id().as_str().to_owned(),
            },
        };
        to_json(&dto)
    }

    /// Size-checks, strictly parses, schema-checks, and promotes JSON bytes into
    /// a validated [`BuildRequest`].
    pub fn decode(codec: &JsonCodec, bytes: &[u8]) -> Result<Self, NixAdapterError> {
        let dto: BuildRequestWire = parse_dto(codec, bytes)?;
        check_schema(dto.schema_version)?;
        let t_vec = dto.targets.into_inner();
        let mut targets = Vec::with_capacity(t_vec.len());
        for t in t_vec {
            targets.push(DerivationPath::from_str(&t).map_err(|_| invalid("invalid target"))?);
        }
        let system = System::from_str(&dto.system).map_err(|_| invalid("unknown system"))?;
        let operation_id = OperationId::new(&dto.receipt.operation_id)?;
        Self::new(targets, system, BuildApprovalReceipt::new(operation_id))
    }
}

/// The report returned by `build()`: a closed status plus built outputs
/// (`plans/09` §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildReport {
    status: BuildStatus,
    outputs: Vec<StorePath>,
}

impl BuildReport {
    /// Constructs and validates a build report with status/payload
    /// consistency: [`BuildStatus::Built`] requires nonempty outputs;
    /// [`BuildStatus::AcquireNoBinary`] requires empty outputs.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] on an inconsistent
    /// status/payload combination, duplicate outputs, or an over-large
    /// collection.
    pub fn new(status: BuildStatus, outputs: Vec<StorePath>) -> Result<Self, NixAdapterError> {
        match status {
            BuildStatus::Built => {
                if outputs.is_empty() {
                    return Err(invalid("built with no outputs"));
                }
                ensure_unique_strings(outputs.iter().map(StorePath::as_str), "duplicate output")?;
                check_size_bounds(
                    outputs.iter().map(StorePath::as_str),
                    MAX_TARGETS,
                    MAX_TARGETS_BYTES,
                    "too many outputs",
                )?;
            }
            BuildStatus::AcquireNoBinary => {
                if !outputs.is_empty() {
                    return Err(invalid("acquireNoBinary with outputs"));
                }
            }
        }
        Ok(Self { status, outputs })
    }

    /// Returns the build status.
    #[must_use]
    pub const fn status(&self) -> BuildStatus {
        self.status
    }

    /// Returns the built outputs (empty for [`BuildStatus::AcquireNoBinary`]).
    #[must_use]
    pub fn outputs(&self) -> &[StorePath] {
        &self.outputs
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum BuildStatusWire {
    Built,
    AcquireNoBinary,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildReportWire {
    schema_version: u32,
    status: BuildStatusWire,
    #[serde(deserialize_with = "deserialize_build_outputs")]
    outputs: BoundedStringSeq,
}

impl BuildReport {
    /// Deterministically encodes this report to JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, NixAdapterError> {
        let dto = BuildReportWire {
            schema_version: SCHEMA_VERSION_CURRENT,
            status: match self.status {
                BuildStatus::Built => BuildStatusWire::Built,
                BuildStatus::AcquireNoBinary => BuildStatusWire::AcquireNoBinary,
            },
            outputs: BoundedStringSeq::from_vec(
                self.outputs
                    .iter()
                    .map(StorePath::as_str)
                    .map(str::to_owned)
                    .collect(),
            ),
        };
        to_json(&dto)
    }

    /// Size-checks, strictly parses, schema-checks, and promotes JSON bytes into
    /// a validated [`BuildReport`].
    pub fn decode(codec: &JsonCodec, bytes: &[u8]) -> Result<Self, NixAdapterError> {
        let dto: BuildReportWire = parse_dto(codec, bytes)?;
        check_schema(dto.schema_version)?;
        let status = match dto.status {
            BuildStatusWire::Built => BuildStatus::Built,
            BuildStatusWire::AcquireNoBinary => BuildStatus::AcquireNoBinary,
        };
        let o_vec = dto.outputs.into_inner();
        let mut outputs = Vec::with_capacity(o_vec.len());
        for o in o_vec {
            outputs.push(StorePath::new(&o).map_err(|_| invalid("invalid output"))?);
        }
        Self::new(status, outputs)
    }
}

// ===========================================================================
// verify (read-only)
// ===========================================================================

/// The scope of a read-only verify (`plans/09` §4.1). Verify **never** mutates
/// the store and carries **no** per-call trust-policy knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VerifyMode {
    /// Verify just the listed paths.
    Shallow,
    /// Verify the closures of the listed paths.
    Recursive,
}

/// A read-only verification request (`plans/09` §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyRequest {
    paths: Vec<StorePath>,
    mode: VerifyMode,
}

impl VerifyRequest {
    /// Constructs and validates a verify request.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] if `paths` is empty,
    /// contains duplicates, or exceeds the bounded collection caps.
    pub fn new(paths: Vec<StorePath>, mode: VerifyMode) -> Result<Self, NixAdapterError> {
        if paths.is_empty() {
            return Err(invalid("empty verify paths"));
        }
        ensure_unique_strings(paths.iter().map(StorePath::as_str), "duplicate verify path")?;
        check_size_bounds(
            paths.iter().map(StorePath::as_str),
            MAX_PATH_LIST,
            MAX_PATH_LIST_BYTES,
            "too many verify paths",
        )?;
        Ok(Self { paths, mode })
    }

    /// Returns the paths (caller order preserved).
    #[must_use]
    pub fn paths(&self) -> &[StorePath] {
        &self.paths
    }

    /// Returns the verify scope.
    #[must_use]
    pub const fn mode(&self) -> VerifyMode {
        self.mode
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum VerifyModeWire {
    Shallow,
    Recursive,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VerifyRequestWire {
    schema_version: u32,
    #[serde(deserialize_with = "deserialize_verify_paths")]
    paths: BoundedStringSeq,
    mode: VerifyModeWire,
}

impl VerifyRequest {
    /// Deterministically encodes this request to JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, NixAdapterError> {
        let dto = VerifyRequestWire {
            schema_version: SCHEMA_VERSION_CURRENT,
            paths: BoundedStringSeq::from_vec(
                self.paths
                    .iter()
                    .map(StorePath::as_str)
                    .map(str::to_owned)
                    .collect(),
            ),
            mode: match self.mode {
                VerifyMode::Shallow => VerifyModeWire::Shallow,
                VerifyMode::Recursive => VerifyModeWire::Recursive,
            },
        };
        to_json(&dto)
    }

    /// Size-checks, strictly parses, schema-checks, and promotes JSON bytes into
    /// a validated [`VerifyRequest`].
    pub fn decode(codec: &JsonCodec, bytes: &[u8]) -> Result<Self, NixAdapterError> {
        let dto: VerifyRequestWire = parse_dto(codec, bytes)?;
        check_schema(dto.schema_version)?;
        let p_vec = dto.paths.into_inner();
        let mut paths = Vec::with_capacity(p_vec.len());
        for p in p_vec {
            paths.push(StorePath::new(&p).map_err(|_| invalid("invalid verify path"))?);
        }
        let mode = match dto.mode {
            VerifyModeWire::Shallow => VerifyMode::Shallow,
            VerifyModeWire::Recursive => VerifyMode::Recursive,
        };
        Self::new(paths, mode)
    }
}

/// The observed NAR integrity of one path under a read-only verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NarIntegrity {
    /// The NAR is intact.
    Intact,
    /// The NAR is corrupt.
    Corrupt,
}

/// The observed trust status of one path under a read-only verify. This is an
/// **observation** only; verify carries no per-call trust-enforcement knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TrustStatus {
    /// Signed by a trusted key.
    Trusted,
    /// Not signed by a trusted key.
    Untrusted,
}

/// The read-only result for a single path under verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathVerifyResult {
    path: StorePath,
    nar_integrity: NarIntegrity,
    trust: TrustStatus,
}

impl PathVerifyResult {
    /// Constructs a per-path verify result from validated types (infallible).
    #[must_use]
    pub fn new(path: StorePath, nar_integrity: NarIntegrity, trust: TrustStatus) -> Self {
        Self {
            path,
            nar_integrity,
            trust,
        }
    }

    /// Returns the path.
    #[must_use]
    pub fn path(&self) -> &StorePath {
        &self.path
    }

    /// Returns the observed NAR integrity.
    #[must_use]
    pub const fn nar_integrity(&self) -> NarIntegrity {
        self.nar_integrity
    }

    /// Returns the observed trust status.
    #[must_use]
    pub const fn trust(&self) -> TrustStatus {
        self.trust
    }
}

/// The read-only report returned by `verify()` (`plans/09` §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    results: Vec<PathVerifyResult>,
}

impl VerifyReport {
    /// Constructs and validates a verify report.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] if `results` is empty,
    /// names a path more than once, or exceeds the bounded collection caps.
    pub fn new(results: Vec<PathVerifyResult>) -> Result<Self, NixAdapterError> {
        if results.is_empty() {
            return Err(invalid("empty verify results"));
        }
        ensure_unique_strings(
            results.iter().map(|r| r.path().as_str()),
            "duplicate verify result",
        )?;
        check_size_bounds(
            results.iter().map(|r| r.path().as_str()),
            MAX_PATH_LIST,
            MAX_PATH_LIST_BYTES,
            "too many verify results",
        )?;
        Ok(Self { results })
    }

    /// Returns the per-path results (caller order preserved).
    #[must_use]
    pub fn results(&self) -> &[PathVerifyResult] {
        &self.results
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum NarIntegrityWire {
    Intact,
    Corrupt,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum TrustStatusWire {
    Trusted,
    Untrusted,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PathVerifyResultWire {
    path: String,
    nar_integrity: NarIntegrityWire,
    trust: TrustStatusWire,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VerifyReportWire {
    schema_version: u32,
    #[serde(deserialize_with = "deserialize_verify_results")]
    results: BoundedSeq<PathVerifyResultWire>,
}

impl VerifyReport {
    /// Deterministically encodes this report to JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, NixAdapterError> {
        let dto = VerifyReportWire {
            schema_version: SCHEMA_VERSION_CURRENT,
            results: BoundedSeq::from_vec(
                self.results
                    .iter()
                    .map(|r| PathVerifyResultWire {
                        path: r.path().as_str().to_owned(),
                        nar_integrity: match r.nar_integrity() {
                            NarIntegrity::Intact => NarIntegrityWire::Intact,
                            NarIntegrity::Corrupt => NarIntegrityWire::Corrupt,
                        },
                        trust: match r.trust() {
                            TrustStatus::Trusted => TrustStatusWire::Trusted,
                            TrustStatus::Untrusted => TrustStatusWire::Untrusted,
                        },
                    })
                    .collect(),
            ),
        };
        to_json(&dto)
    }

    /// Size-checks, strictly parses, schema-checks, and promotes JSON bytes into
    /// a validated [`VerifyReport`].
    pub fn decode(codec: &JsonCodec, bytes: &[u8]) -> Result<Self, NixAdapterError> {
        let dto: VerifyReportWire = parse_dto(codec, bytes)?;
        check_schema(dto.schema_version)?;
        let r_vec = dto.results.into_inner();
        let mut results = Vec::with_capacity(r_vec.len());
        for r in r_vec {
            let path =
                StorePath::new(&r.path).map_err(|_| invalid("invalid verify result path"))?;
            let nar_integrity = match r.nar_integrity {
                NarIntegrityWire::Intact => NarIntegrity::Intact,
                NarIntegrityWire::Corrupt => NarIntegrity::Corrupt,
            };
            let trust = match r.trust {
                TrustStatusWire::Trusted => TrustStatus::Trusted,
                TrustStatusWire::Untrusted => TrustStatus::Untrusted,
            };
            results.push(PathVerifyResult::new(path, nar_integrity, trust));
        }
        Self::new(results)
    }
}

// ===========================================================================
// gc
// ===========================================================================

/// The closed status of a GC run (`plans/09` §4.1; `plans/05` T-STATE-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum GcStatus {
    /// Unreachable paths were collected.
    Collected,
    /// Collection was refused because an operation holds the lease.
    RefusedUnderLease,
}

/// The report returned by `gc()` (`plans/09` §4.1). `gc()` takes **no** roots
/// argument; it consults the on-disk GC-roots tree. In addition to the closed
/// status and the collected paths, it carries a **checked** `freed_bytes`
/// total — the bytes the backend reports freed, which must be consistent with
/// the status ([`GcStatus::RefusedUnderLease`] requires zero).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcReport {
    status: GcStatus,
    collected: Vec<StorePath>,
    freed_bytes: u64,
}

impl GcReport {
    /// Constructs and validates a GC report.
    ///
    /// [`GcStatus::RefusedUnderLease`] requires an empty `collected` list **and**
    /// `freed_bytes == 0`; [`GcStatus::Collected`] canonicalizes `collected`
    /// (sorted, de-duplicated) and bounds it. `freed_bytes` is a single `u64`
    /// field reported by the backend; it is consistency-checked here (never
    /// summed with checked arithmetic at this boundary because it is not
    /// computed from the collected paths).
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] on an inconsistent
    /// status/payload combination, duplicates, or an over-large collection.
    pub fn new(
        status: GcStatus,
        mut collected: Vec<StorePath>,
        freed_bytes: u64,
    ) -> Result<Self, NixAdapterError> {
        match status {
            GcStatus::Collected => {
                ensure_unique_strings(
                    collected.iter().map(StorePath::as_str),
                    "duplicate collected",
                )?;
                collected.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                check_size_bounds(
                    collected.iter().map(StorePath::as_str),
                    MAX_GC_COLLECTED,
                    MAX_GC_COLLECTED_BYTES,
                    "too many collected",
                )?;
            }
            GcStatus::RefusedUnderLease => {
                if !collected.is_empty() {
                    return Err(invalid("refused with collected"));
                }
                if freed_bytes != 0 {
                    return Err(invalid("refused with freed bytes"));
                }
            }
        }
        Ok(Self {
            status,
            collected,
            freed_bytes,
        })
    }

    /// Returns the GC status.
    #[must_use]
    pub const fn status(&self) -> GcStatus {
        self.status
    }

    /// Returns the collected paths (canonicalized for [`GcStatus::Collected`]).
    #[must_use]
    pub fn collected(&self) -> &[StorePath] {
        &self.collected
    }

    /// Returns the total bytes reported freed by the backend.
    #[must_use]
    pub const fn freed_bytes(&self) -> u64 {
        self.freed_bytes
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum GcStatusWire {
    Collected,
    RefusedUnderLease,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GcReportWire {
    schema_version: u32,
    status: GcStatusWire,
    #[serde(deserialize_with = "deserialize_gc_collected")]
    collected: BoundedStringSeq,
    freed_bytes: u64,
}

impl GcReport {
    /// Deterministically encodes this report to JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, NixAdapterError> {
        let dto = GcReportWire {
            schema_version: SCHEMA_VERSION_CURRENT,
            status: match self.status {
                GcStatus::Collected => GcStatusWire::Collected,
                GcStatus::RefusedUnderLease => GcStatusWire::RefusedUnderLease,
            },
            collected: BoundedStringSeq::from_vec(
                self.collected
                    .iter()
                    .map(StorePath::as_str)
                    .map(str::to_owned)
                    .collect(),
            ),
            freed_bytes: self.freed_bytes,
        };
        to_json(&dto)
    }

    /// Size-checks, strictly parses, schema-checks, and promotes JSON bytes into
    /// a validated [`GcReport`].
    pub fn decode(codec: &JsonCodec, bytes: &[u8]) -> Result<Self, NixAdapterError> {
        let dto: GcReportWire = parse_dto(codec, bytes)?;
        check_schema(dto.schema_version)?;
        let status = match dto.status {
            GcStatusWire::Collected => GcStatus::Collected,
            GcStatusWire::RefusedUnderLease => GcStatus::RefusedUnderLease,
        };
        let c_vec = dto.collected.into_inner();
        let mut collected = Vec::with_capacity(c_vec.len());
        for c in c_vec {
            collected.push(StorePath::new(&c).map_err(|_| invalid("invalid collected path"))?);
        }
        Self::new(status, collected, dto.freed_bytes)
    }
}

// ===========================================================================
// maintenance root-set path components
// ===========================================================================

/// A validated GC-root name (`plans/01` §11.1). Rejects path separators,
/// control characters, leading-dot names (`.`, `..`, `.hidden`, …), and
/// overlength input.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RootName(String);

impl RootName {
    /// Validates and constructs a root name.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] for empty, over-long
    /// input, any byte outside `[A-Za-z0-9._-]`, or any leading-dot name
    /// (`.`, `..`, `.hidden`, `...`, …).
    pub fn new(value: &str) -> Result<Self, NixAdapterError> {
        if is_root_name(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(invalid("invalid root name"))
        }
    }

    /// Returns the root name string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Returns `true` if `s` is a traversal-safe GC-root name.
///
/// In addition to the allowlist, this rejects **every leading-dot** name
/// (`.`, `..`, `.hidden`, `...`, …), so a root name can never be a hidden-file
/// or traversal token when used as a path component.
fn is_root_name(s: &str) -> bool {
    is_name_component(s, MAX_ROOT_NAME)
}

/// Returns `true` if `seg` is a canonical, traversal-safe path component:
/// nonempty, at most `max_len` bytes, not a leading-dot name (`.`/`..`/
/// `.hidden`/…), and every byte in `[A-Za-z0-9._-]`. Shared by [`RootName`]
/// and the rest components of [`RootRef`].
fn is_name_component(seg: &str, max_len: usize) -> bool {
    if seg.is_empty() || seg.len() > max_len {
        return false;
    }
    if seg.starts_with('.') {
        return false;
    }
    seg.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl fmt::Display for RootName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The canonical managed generation-root reference returned by the privileged
/// maintenance adapter (`plans/09` §4.1.2, `plans/01` §9.1).
///
/// This is an **adapter-computed** output. It is validated as the canonical
/// managed gcroot layout `/nix/var/nix/gcroots/pkg/users/<numeric-uid>/...`
/// (ARCH-INV-04, D-17): a bounded absolute path with no NUL/control/del bytes,
/// the fixed `/nix/var/nix/gcroots/pkg/users/` prefix, a canonical numeric
/// uid, and one or more traversal-safe rest components. Empty, `.`, `..`,
/// repeated-separator (`//`), trailing-slash, control-byte, and noncanonical
/// components are all rejected — so this is **not** merely any absolute string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootRef(String);

impl RootRef {
    /// Validates and constructs a root reference from a canonical managed
    /// gcroot path.
    ///
    /// # Errors
    ///
    /// Returns [`NixAdapterError::ValidationFailure`] unless `path` is the
    /// canonical managed layout `/nix/var/nix/gcroots/pkg/users/<uid>/...`
    /// with a numeric uid and traversal-safe rest components.
    pub fn new(path: &str) -> Result<Self, NixAdapterError> {
        if is_root_ref(path) {
            Ok(Self(path.to_owned()))
        } else {
            Err(invalid("invalid root ref"))
        }
    }

    /// Returns the absolute GC-root path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RootRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The fixed prefix components (after splitting on `/`) of a managed gcroot
/// path: `""` (leading slash), `nix`, `var`, `nix`, `gcroots`, `pkg`, `users`.
const ROOT_REF_PREFIX: [&str; 7] = ["", "nix", "var", "nix", "gcroots", "pkg", "users"];

/// Returns `true` if `s` is a canonical managed gcroot path:
/// `/nix/var/nix/gcroots/pkg/users/<numeric-uid>/<rest...>`.
fn is_root_ref(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_ROOT_PATH {
        return false;
    }
    // No NUL / control / del bytes anywhere.
    if s.bytes().any(|b| b == 0 || b < 0x20 || b == 0x7f) {
        return false;
    }
    let parts: Vec<&str> = s.split('/').collect();
    // Need the 7 prefix components, a numeric uid, and at least one rest
    // component. (A bare uid with no rest is not a root.)
    if parts.len() < ROOT_REF_PREFIX.len() + 2 {
        return false;
    }
    for (i, expect) in ROOT_REF_PREFIX.iter().enumerate() {
        if parts[i] != *expect {
            return false;
        }
    }
    if !is_canonical_uid(parts[ROOT_REF_PREFIX.len()]) {
        return false;
    }
    for seg in &parts[ROOT_REF_PREFIX.len() + 1..] {
        if !is_name_component(seg, MAX_ROOT_NAME) {
            return false;
        }
    }
    true
}

/// Returns `true` if `s` is a canonical POSIX-style numeric uid: `"0"` or a
/// nonzero-leading decimal that fits in `u32` (no leading zeros, no sign, no
/// non-digits).
fn is_canonical_uid(s: &str) -> bool {
    if s.is_empty() || s.len() > 10 {
        return false;
    }
    let first = s.as_bytes()[0];
    if first == b'0' {
        return s == "0";
    }
    if !first.is_ascii_digit() {
        return false;
    }
    s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u32>().is_ok()
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RootRefWire {
    schema_version: u32,
    path: String,
}

impl RootRef {
    /// Deterministically encodes this report to JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, NixAdapterError> {
        let dto = RootRefWire {
            schema_version: SCHEMA_VERSION_CURRENT,
            path: self.0.clone(),
        };
        to_json(&dto)
    }

    /// Size-checks, strictly parses, schema-checks, and promotes JSON bytes into
    /// a validated [`RootRef`].
    pub fn decode(codec: &JsonCodec, bytes: &[u8]) -> Result<Self, NixAdapterError> {
        let dto: RootRefWire = parse_dto(codec, bytes)?;
        check_schema(dto.schema_version)?;
        Self::new(&dto.path)
    }
}

// ===========================================================================
// Size-capped JSON codec
// ===========================================================================

/// The public size-capped JSON decode boundary for `pkg`-owned serialized
/// reports (`plans/09` §4.2).
///
/// The codec size-checks input **before** parsing (so oversized input is
/// rejected without materializing it), preserves serde_json's default recursion
/// protection, and is the single path through which every report/request is
/// decoded. Production uses the 64 MiB cap ([`JsonCodec::PRODUCTION_LIMIT`]);
/// tests construct a smaller [`JsonCodec::with_limit`] to exercise exact
/// boundary behavior without allocating 64 MiB.
#[derive(Debug, Clone, Copy)]
pub struct JsonCodec {
    limit: usize,
}

impl JsonCodec {
    /// The production byte cap: 64 MiB.
    pub const PRODUCTION_LIMIT: usize = 64 * 1024 * 1024;

    /// Returns the production codec (64 MiB cap).
    #[must_use]
    pub const fn production() -> Self {
        Self {
            limit: Self::PRODUCTION_LIMIT,
        }
    }

    /// Returns a codec with a custom byte cap (primarily for tests). The
    /// production cap is [`JsonCodec::PRODUCTION_LIMIT`].
    #[must_use]
    pub const fn with_limit(limit: usize) -> Self {
        Self { limit }
    }

    /// Returns the byte cap this codec enforces.
    #[must_use]
    pub const fn limit(self) -> usize {
        self.limit
    }
}

impl Default for JsonCodec {
    fn default() -> Self {
        Self::production()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::NixAdapterErrorCode;

    #[test]
    fn schema_version_only_accepts_current() {
        assert_eq!(SchemaVersion::current().get(), 1);
        assert!(SchemaVersion::new(1).is_ok());
        assert_eq!(
            SchemaVersion::new(2).unwrap_err().code(),
            NixAdapterErrorCode::UnsupportedSchemaVersion
        );
    }

    #[test]
    fn method_kind_round_trips() {
        for m in MethodKind::ALL {
            let s = m.as_str();
            assert_eq!(MethodKind::from_str(s).unwrap(), m);
            assert_eq!(m.to_string(), s);
        }
        assert_eq!(MethodKind::ALL.len(), 7);
        assert!(MethodKind::from_str("nope").is_err());
    }

    #[test]
    fn root_name_rejects_traversal_and_leading_dot() {
        for ok in ["gen-0007", "ripgrep", "a.b.c", "x_y-z.1"] {
            assert!(RootName::new(ok).is_ok(), "{ok}");
        }
        // Every leading-dot name is rejected (".", "..", "...", ".hidden", …).
        for bad in [
            "",
            ".",
            "..",
            "...",
            ".hidden",
            ".x",
            ".-x",
            "..-x",
            "../etc",
            "a/b",
            "a\\b",
            "a b",
            "a;b",
            "café",
            "a\0b",
            &"a".repeat(MAX_ROOT_NAME + 1),
        ] {
            assert!(RootName::new(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn root_ref_validates_canonical_managed_gcroot() {
        // Canonical managed gcroot layout: prefix + numeric uid + rest.
        for ok in [
            "/nix/var/nix/gcroots/pkg/users/1001/gen-0007",
            "/nix/var/nix/gcroots/pkg/users/0/gen-0007",
            "/nix/var/nix/gcroots/pkg/users/4294967295/gen-0007",
            "/nix/var/nix/gcroots/pkg/users/1001/sub/gen-0007",
        ] {
            assert!(RootRef::new(ok).is_ok(), "should accept {ok}");
        }
        let huge = format!(
            "/nix/var/nix/gcroots/pkg/users/1001/{}",
            "a".repeat(MAX_ROOT_PATH)
        );
        for bad in [
            "",
            "relative/path",
            "/tmp/foo",
            "/nix/var/nix/gcroots/other/users/1001/gen-0007",
            "/nix/var/nix/gcroots/pkg/users/gen-0007",
            "/nix/var/nix/gcroots/pkg/users/abc/gen-0007",
            "/nix/var/nix/gcroots/pkg/users/01001/gen-0007",
            "/nix/var/nix/gcroots/pkg/users/1001/../etc",
            "/nix/var/nix/gcroots/pkg/users/1001/.",
            "/nix/var/nix/gcroots/pkg/users/1001//gen-0007",
            "/nix/var/nix/gcroots/pkg/users/1001/gen-0007/",
            "/nix/var/nix/gcroots/pkg/users/1001/.hidden",
            huge.as_str(),
        ] {
            assert!(RootRef::new(bad).is_err(), "should reject {bad:?}");
        }
    }

    // -------------------------------------------------------------------
    // Bounded serde visitors: count caps and duplicate-key rejection at small
    // const caps. These prove the deserialization-time enforcement that the
    // production wire fields use (each production field calls the same generic
    // visitor with its constructor cap); the integration tests exercise the
    // production fields directly at the real caps.
    // -------------------------------------------------------------------

    #[test]
    fn bounded_string_seq_visitor_enforces_count_and_byte_caps() {
        // Exactly at the count cap: accepted.
        let mut de = serde_json::Deserializer::from_str("[\"a\",\"b\",\"c\"]");
        let at = BoundedStringSeq::deserialize_bounded(&mut de, 3, 1024).unwrap();
        assert_eq!(
            at.into_inner(),
            vec!["a".to_owned(), "b".into(), "c".into()]
        );

        // One over the count cap: rejected during visiting, before promotion.
        let mut de = serde_json::Deserializer::from_str("[\"a\",\"b\",\"c\",\"d\"]");
        let over_count = BoundedStringSeq::deserialize_bounded(&mut de, 3, 1024);
        assert!(over_count.is_err());

        // Over the total-byte cap: rejected during visiting.
        let mut de = serde_json::Deserializer::from_str("[\"aaaa\",\"bbbb\",\"cccc\"]");
        let over_bytes = BoundedStringSeq::deserialize_bounded(&mut de, 3, 5);
        assert!(over_bytes.is_err());
    }

    #[test]
    fn bounded_seq_visitor_enforces_count_cap() {
        // Exactly at the count cap: accepted.
        let mut de = serde_json::Deserializer::from_str("[1,2,3]");
        let at = BoundedSeq::<u32>::deserialize_bounded(&mut de, 3).unwrap();
        assert_eq!(at.into_inner(), vec![1u32, 2, 3]);

        // One over the count cap: rejected during visiting, before promotion.
        let mut de = serde_json::Deserializer::from_str("[1,2,3,4]");
        let over = BoundedSeq::<u32>::deserialize_bounded(&mut de, 3);
        assert!(over.is_err());

        // A much larger array under a small cap is still rejected; the
        // size_hint is capped at the cap, never trusted.
        let mut de = serde_json::Deserializer::from_str("[1,2,3,4,5,6,7,8,9,10]");
        let big = BoundedSeq::<u32>::deserialize_bounded(&mut de, 3);
        assert!(big.is_err());
    }

    #[test]
    fn bounded_unique_string_map_rejects_duplicates_and_caps() {
        // Exactly at the count cap, no duplicates: accepted.
        let mut de = serde_json::Deserializer::from_str(r#"{"a":"1","b":"2","c":"3"}"#);
        let at = BoundedUniqueStringMap::deserialize_bounded(&mut de, 3, 1024).unwrap();
        assert_eq!(at.into_inner().len(), 3);

        // One over the count cap: rejected during visiting.
        let mut de = serde_json::Deserializer::from_str(r#"{"a":"1","b":"2","c":"3","d":"4"}"#);
        let over = BoundedUniqueStringMap::deserialize_bounded(&mut de, 3, 1024);
        assert!(over.is_err());

        // Duplicate JSON keys are rejected during visiting (not last-wins).
        let mut de = serde_json::Deserializer::from_str(r#"{"a":"1","a":"2"}"#);
        let dup = BoundedUniqueStringMap::deserialize_bounded(&mut de, 8, 1024);
        assert!(dup.is_err());

        // Over the total key/value byte budget: rejected during visiting.
        let mut de = serde_json::Deserializer::from_str(r#"{"aaaa":"1111","bbbb":"2222"}"#);
        let over_bytes = BoundedUniqueStringMap::deserialize_bounded(&mut de, 8, 5);
        assert!(over_bytes.is_err());
    }
}
