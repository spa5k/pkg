//! Strict schema-version 1 manifest, lock, and generation JSON contracts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::de::{DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};

use super::Digest;
use crate::{
    AttributePath, ChannelSequence, DerivationPath, NarHash, NixpkgsRevision, OutputName,
    OutputSelection, PackageVersion, Realization, SelectorId, SelectorInput, SourceRevision,
    StorePath, System, VersionBound, VersionPreference, VersionRange,
};

/// The only state schema version understood by this release.
pub const STATE_SCHEMA_VERSION: u64 = 1;
const MAX_STATE_JSON_BYTES: usize = 64 * 1024 * 1024;

/// A closed error returned while decoding or validating persisted state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateSchemaError {
    /// The bytes are not valid strict JSON for the requested state document.
    InvalidJson(String),
    /// The document uses a schema version this release cannot safely read.
    UnsupportedSchemaVersion(u64),
    /// A named field failed its domain validation.
    InvalidField {
        /// Canonical JSON field name.
        field: &'static str,
        /// Redacted validation reason.
        reason: String,
    },
    /// Two otherwise valid fields violate a document-level invariant.
    Invariant(String),
}

impl fmt::Display for StateSchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(reason) => write!(f, "invalid state JSON: {reason}"),
            Self::UnsupportedSchemaVersion(version) => {
                write!(f, "unsupported state schema version {version}")
            }
            Self::InvalidField { field, reason } => {
                write!(f, "invalid state field `{field}`: {reason}")
            }
            Self::Invariant(reason) => write!(f, "invalid state relationship: {reason}"),
        }
    }
}

impl std::error::Error for StateSchemaError {}

fn json_error(error: serde_json::Error) -> StateSchemaError {
    StateSchemaError::InvalidJson(error.to_string())
}

fn field_error(field: &'static str, error: impl fmt::Display) -> StateSchemaError {
    StateSchemaError::InvalidField {
        field,
        reason: error.to_string(),
    }
}

fn require_v1(version: u64) -> Result<(), StateSchemaError> {
    if version == STATE_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(StateSchemaError::UnsupportedSchemaVersion(version))
    }
}

pub(super) fn parse_unique_json(bytes: &[u8]) -> Result<Value, StateSchemaError> {
    if bytes.len() > MAX_STATE_JSON_BYTES {
        return Err(StateSchemaError::InvalidJson(
            "state document exceeds 64 MiB".into(),
        ));
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = UniqueValue::deserialize(&mut deserializer)
        .map_err(json_error)?
        .0;
    deserializer.end().map_err(json_error)?;
    Ok(value)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, StateSchemaError> {
    serde_json::from_value(parse_unique_json(bytes)?).map_err(json_error)
}

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueValueVisitor)
    }
}

struct UniqueValueVisitor;

impl<'de> Visitor<'de> for UniqueValueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Bool(value)))
    }
    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(Number::from(value))))
    }
    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(Number::from(value))))
    }
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .map(UniqueValue)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }
    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value.into())))
    }
    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value)))
    }
    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }
    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }
    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        UniqueValue::deserialize(deserializer)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(UniqueValue(Value::Array(values)))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Self::Value, A::Error> {
        let mut values = Map::new();
        while let Some((key, value)) = object.next_entry::<String, UniqueValue>()? {
            if values.insert(key.clone(), value.0).is_some() {
                return Err(serde::de::Error::custom(format!(
                    "duplicate object key {key}"
                )));
            }
        }
        Ok(UniqueValue(Value::Object(values)))
    }
}

/// Desired package state stored in `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    channel_seq: ChannelSequence,
    uid: u32,
    entries: Vec<ManifestEntry>,
    pins: Vec<SelectorId>,
}

impl Manifest {
    /// Decodes and validates a schema-version 1 manifest.
    pub fn from_json(bytes: &[u8]) -> Result<Self, StateSchemaError> {
        decode::<ManifestWire>(bytes)?.try_into()
    }

    /// Encodes the manifest with canonical field names and deterministic map order.
    pub fn to_json(&self) -> Result<Vec<u8>, StateSchemaError> {
        serde_json::to_vec(&ManifestWire::from(self)).map_err(json_error)
    }

    /// Returns the signed channel sequence this desired state used.
    #[must_use]
    pub const fn channel_seq(&self) -> ChannelSequence {
        self.channel_seq
    }

    /// Returns the owning OS user id.
    #[must_use]
    pub const fn uid(&self) -> u32 {
        self.uid
    }

    /// Returns desired entries in stable manifest order.
    #[must_use]
    pub fn entries(&self) -> &[ManifestEntry] {
        &self.entries
    }

    /// Returns the convenience index of pinned selector ids.
    #[must_use]
    pub fn pins(&self) -> &[SelectorId] {
        &self.pins
    }

    pub(crate) fn from_lifecycle_parts(
        channel_seq: ChannelSequence,
        uid: u32,
        entries: Vec<ManifestEntry>,
    ) -> Self {
        let pins = entries
            .iter()
            .filter(|entry| entry.pinned)
            .map(|entry| entry.id.clone())
            .collect();
        Self {
            channel_seq,
            uid,
            entries,
            pins,
        }
    }

    pub(crate) fn into_lifecycle_entries(self) -> Vec<ManifestEntry> {
        self.entries
    }
}

/// One desired selector in a [`Manifest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    id: SelectorId,
    selector: SelectorInput,
    attribute: AttributePath,
    version_preference: VersionPreference,
    outputs: OutputSelection,
    source_revision: SourceRevision,
    pinned: bool,
    pinned_to: Option<StorePath>,
    added_at: String,
    origin: String,
}

impl ManifestEntry {
    /// Returns the stable selector id.
    #[must_use]
    pub fn id(&self) -> &SelectorId {
        &self.id
    }
    /// Returns the original user-facing selector input.
    #[must_use]
    pub fn selector(&self) -> &SelectorInput {
        &self.selector
    }
    /// Returns the canonical resolved Nixpkgs attribute.
    #[must_use]
    pub fn attribute(&self) -> &AttributePath {
        &self.attribute
    }
    /// Returns the requested version constraint.
    #[must_use]
    pub fn version_preference(&self) -> &VersionPreference {
        &self.version_preference
    }
    /// Returns the selected outputs, or the package defaults.
    #[must_use]
    pub fn outputs(&self) -> &OutputSelection {
        &self.outputs
    }
    /// Returns the exact source-selection intent for this entry.
    #[must_use]
    pub fn source_revision(&self) -> &SourceRevision {
        &self.source_revision
    }
    /// Returns whether this selector is pinned.
    #[must_use]
    pub const fn is_pinned(&self) -> bool {
        self.pinned
    }
    /// Returns the exact pinned store path, when pinned to a realization.
    #[must_use]
    pub fn pinned_to(&self) -> Option<&StorePath> {
        self.pinned_to.as_ref()
    }

    pub(crate) fn retarget_for_upgrade(mut self, attribute: AttributePath, bump_pin: bool) -> Self {
        self.attribute = attribute;
        self.source_revision = SourceRevision::CurrentChannel;
        if bump_pin {
            self.pinned = false;
            self.pinned_to = None;
        }
        self
    }
}

/// Exact realized package state stored in `lock.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedState {
    channel_seq: ChannelSequence,
    system: System,
    uid: u32,
    entries: BTreeMap<SelectorId, LockEntry>,
}

impl LockedState {
    /// Decodes and validates a schema-version 1 lock file.
    pub fn from_json(bytes: &[u8]) -> Result<Self, StateSchemaError> {
        decode::<LockedStateWire>(bytes)?.try_into()
    }

    /// Encodes the lock file with stable selector-key ordering.
    pub fn to_json(&self) -> Result<Vec<u8>, StateSchemaError> {
        serde_json::to_vec(&LockedStateWire::from(self)).map_err(json_error)
    }

    /// Returns the owning OS user id.
    #[must_use]
    pub const fn uid(&self) -> u32 {
        self.uid
    }
    /// Returns the verified channel sequence used to resolve this lock.
    #[must_use]
    pub const fn channel_seq(&self) -> ChannelSequence {
        self.channel_seq
    }
    /// Returns the target Nix system.
    #[must_use]
    pub const fn system(&self) -> System {
        self.system
    }
    /// Returns lock entries keyed by stable selector id.
    #[must_use]
    pub fn entries(&self) -> &BTreeMap<SelectorId, LockEntry> {
        &self.entries
    }

    pub(crate) fn from_lifecycle_parts(
        channel_seq: ChannelSequence,
        system: System,
        uid: u32,
        entries: BTreeMap<SelectorId, LockEntry>,
    ) -> Self {
        Self {
            channel_seq,
            system,
            uid,
            entries,
        }
    }

    pub(crate) fn into_lifecycle_entries(self) -> BTreeMap<SelectorId, LockEntry> {
        self.entries
    }
}

/// One exact locked realization and its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    attribute: AttributePath,
    realization: Realization,
    locked_at: String,
    provenance: String,
    signatures_observed: Vec<String>,
}

impl LockEntry {
    /// Constructs one exact locked realization with validated provenance text.
    pub fn new(
        attribute: AttributePath,
        realization: Realization,
        locked_at: String,
        provenance: String,
        signatures_observed: Vec<String>,
    ) -> Result<Self, StateSchemaError> {
        Ok(Self {
            attribute,
            realization,
            locked_at: nonempty("lockedAt", locked_at)?,
            provenance: nonempty("provenance", provenance)?,
            signatures_observed,
        })
    }

    /// Returns the canonical attribute that produced this realization.
    #[must_use]
    pub fn attribute(&self) -> &AttributePath {
        &self.attribute
    }
    /// Returns the validated realization.
    #[must_use]
    pub fn realization(&self) -> &Realization {
        &self.realization
    }
    /// Returns the sanitized acquisition provenance.
    #[must_use]
    pub fn provenance(&self) -> &str {
        &self.provenance
    }
}

/// Immutable record for one activated generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    uid: u32,
    id: String,
    parent: Option<String>,
    created_at: String,
    channel_seq: ChannelSequence,
    manifest_hash: String,
    lock_hash: String,
    manifest_snapshot: String,
    lock_snapshot: String,
    activation: Activation,
    outputs: Vec<GenerationOutput>,
    operation: GenerationOperation,
    generation_hash: String,
}

impl Generation {
    /// Decodes and validates a schema-version 1 generation record.
    pub fn from_json(bytes: &[u8]) -> Result<Self, StateSchemaError> {
        decode::<GenerationWire>(bytes)?.try_into()
    }

    /// Encodes this generation record deterministically.
    pub fn to_json(&self) -> Result<Vec<u8>, StateSchemaError> {
        serde_json::to_vec(&GenerationWire::from(self)).map_err(json_error)
    }

    /// Returns the monotonic generation id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Returns the authenticated user identity owning this generation.
    #[must_use]
    pub const fn uid(&self) -> u32 {
        self.uid
    }
    /// Returns the channel sequence captured by this generation.
    #[must_use]
    pub const fn channel_seq(&self) -> ChannelSequence {
        self.channel_seq
    }
    /// Returns the exact manifest snapshot body digest.
    #[must_use]
    pub fn manifest_hash(&self) -> &str {
        &self.manifest_hash
    }
    /// Returns the exact lock snapshot body digest.
    #[must_use]
    pub fn lock_hash(&self) -> &str {
        &self.lock_hash
    }
    /// Returns the relative generation-scoped manifest snapshot path.
    #[must_use]
    pub fn manifest_snapshot(&self) -> &str {
        &self.manifest_snapshot
    }
    /// Returns the relative generation-scoped lock snapshot path.
    #[must_use]
    pub fn lock_snapshot(&self) -> &str {
        &self.lock_snapshot
    }
    /// Returns the activation record.
    #[must_use]
    pub fn activation(&self) -> &Activation {
        &self.activation
    }
    /// Returns exact selector realizations documented by this generation.
    #[must_use]
    pub fn outputs(&self) -> &[GenerationOutput] {
        &self.outputs
    }
    /// Returns the operation provenance captured by this generation.
    #[must_use]
    pub const fn operation(&self) -> &GenerationOperation {
        &self.operation
    }
    /// Returns the generation record's self-digest.
    #[must_use]
    pub fn generation_hash(&self) -> &str {
        &self.generation_hash
    }
}

/// Rust-owned activation-forest metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    tree_path: String,
    tree_digest: String,
    entry_count: u64,
    collision_policy: CollisionPolicy,
    output_roots: Vec<StorePath>,
    collision_resolutions: Vec<CollisionResolution>,
}

impl Activation {
    /// Returns the relative retained forest path.
    #[must_use]
    pub fn tree_path(&self) -> &str {
        &self.tree_path
    }
    /// Returns the recorded deterministic forest digest.
    #[must_use]
    pub fn tree_digest(&self) -> &str {
        &self.tree_digest
    }
    /// Returns the number of leaf links in the forest.
    #[must_use]
    pub const fn entry_count(&self) -> u64 {
        self.entry_count
    }
    /// Returns the collision policy applied during staging.
    #[must_use]
    pub const fn collision_policy(&self) -> CollisionPolicy {
        self.collision_policy
    }
    /// Returns every selected output root in canonical order.
    #[must_use]
    pub fn output_roots(&self) -> &[StorePath] {
        &self.output_roots
    }
    /// Returns every recorded collision decision.
    #[must_use]
    pub fn collision_resolutions(&self) -> &[CollisionResolution] {
        &self.collision_resolutions
    }
}

/// Collision handling recorded for an activation forest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollisionPolicy {
    /// Refuse the activation when any relative path collides.
    Abort,
    /// Keep the first deterministic provider of a colliding path.
    KeepFirst,
    /// Keep the last deterministic provider of a colliding path.
    KeepLast,
}

/// One recorded collision winner and losers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollisionResolution {
    relative_path: String,
    winner: CollisionChoice,
    losers: Vec<CollisionChoice>,
}

/// A selector/output pair participating in a collision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollisionChoice {
    source_selector: SelectorId,
    output: OutputName,
}

/// One selected output captured by a generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationOutput {
    id: SelectorId,
    attribute: AttributePath,
    nixpkgs_revision: NixpkgsRevision,
    store_path: StorePath,
    deriver: DerivationPath,
    outputs_to_install: Vec<OutputName>,
    nar_hash: NarHash,
    closure_nar_size: u64,
    provenance: String,
    pinned: bool,
}

impl GenerationOutput {
    /// Returns the stable selector id.
    #[must_use]
    pub fn id(&self) -> &SelectorId {
        &self.id
    }
    /// Returns the canonical attribute resolved for this selector.
    #[must_use]
    pub fn attribute(&self) -> &AttributePath {
        &self.attribute
    }
    /// Returns the exact pinned Nixpkgs revision.
    #[must_use]
    pub fn nixpkgs_revision(&self) -> &NixpkgsRevision {
        &self.nixpkgs_revision
    }
    /// Returns the primary realized store path.
    #[must_use]
    pub fn store_path(&self) -> &StorePath {
        &self.store_path
    }
    /// Returns the exact derivation path.
    #[must_use]
    pub fn deriver(&self) -> &DerivationPath {
        &self.deriver
    }
    /// Returns selected output names in deterministic order.
    #[must_use]
    pub fn outputs_to_install(&self) -> &[OutputName] {
        &self.outputs_to_install
    }
    /// Returns the verified NAR hash.
    #[must_use]
    pub fn nar_hash(&self) -> &NarHash {
        &self.nar_hash
    }
    /// Returns the verified closure NAR byte count.
    #[must_use]
    pub const fn closure_nar_size(&self) -> u64 {
        self.closure_nar_size
    }
    /// Returns the sanitized acquisition provenance.
    #[must_use]
    pub fn provenance(&self) -> &str {
        &self.provenance
    }
    /// Returns whether desired state pinned this selector.
    #[must_use]
    pub const fn is_pinned(&self) -> bool {
        self.pinned
    }
}

/// Operation provenance attached to a generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationOperation {
    op_id: String,
    kind: String,
    approval: OperationApproval,
}

impl GenerationOperation {
    /// Returns the stable operation identifier.
    #[must_use]
    pub fn op_id(&self) -> &str {
        &self.op_id
    }
    /// Returns the product operation kind.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }
}

/// Build approval state captured by a generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationApproval {
    build: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestWire {
    schema_version: u64,
    channel_seq: u64,
    uid: u32,
    entries: Vec<ManifestEntryWire>,
    pins: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestEntryWire {
    id: String,
    selector: String,
    attribute: String,
    version_pref: VersionPreferenceWire,
    outputs: Option<Vec<String>>,
    source_rev: String,
    pinned: bool,
    pinned_to: Option<String>,
    added_at: String,
    origin: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum VersionPreferenceWire {
    Any,
    Exact {
        version: String,
    },
    Min {
        version: String,
    },
    Range {
        lower: Option<VersionBoundWire>,
        upper: Option<VersionBoundWire>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionBoundWire {
    version: String,
    inclusive: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LockedStateWire {
    schema_version: u64,
    channel_seq: u64,
    system: String,
    uid: u32,
    entries: BTreeMap<String, LockEntryWire>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LockEntryWire {
    attribute: String,
    nixpkgs_rev: String,
    realized: RealizationWire,
    locked_at: String,
    provenance: String,
    sigs_observed: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RealizationWire {
    store_path: String,
    deriver: String,
    outputs: BTreeMap<String, String>,
    outputs_to_install: Vec<String>,
    system: String,
    nar_hash: String,
    closure_nar_size: u64,
    pname: String,
    version: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenerationWire {
    schema_version: u64,
    uid: u32,
    id: String,
    parent: Option<String>,
    created_at: String,
    channel_seq: u64,
    manifest_hash: String,
    lock_hash: String,
    manifest_snapshot: String,
    lock_snapshot: String,
    activation: ActivationWire,
    outputs: Vec<GenerationOutputWire>,
    operation: GenerationOperationWire,
    generation_hash: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivationWire {
    kind: String,
    tree_path: String,
    tree_digest: String,
    entry_count: u64,
    collision_policy: CollisionPolicyWire,
    output_roots: Vec<String>,
    collision_resolutions: Vec<CollisionResolutionWire>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CollisionPolicyWire {
    Abort,
    KeepFirst,
    KeepLast,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CollisionResolutionWire {
    relative_path: String,
    winner: CollisionChoiceWire,
    losers: Vec<CollisionChoiceWire>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CollisionChoiceWire {
    source_selector: String,
    output: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenerationOutputWire {
    id: String,
    attribute: String,
    nixpkgs_rev: String,
    store_path: String,
    deriver: String,
    outputs_to_install: Vec<String>,
    nar_hash: String,
    closure_nar_size: u64,
    provenance: String,
    pinned: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenerationOperationWire {
    op_id: String,
    kind: String,
    approval: OperationApprovalWire,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OperationApprovalWire {
    build: String,
}

impl TryFrom<ManifestWire> for Manifest {
    type Error = StateSchemaError;
    fn try_from(wire: ManifestWire) -> Result<Self, Self::Error> {
        require_v1(wire.schema_version)?;
        let channel_seq = ChannelSequence::from_u64(wire.channel_seq)
            .ok_or_else(|| field_error("channelSeq", "must be non-zero"))?;
        let entries = wire
            .entries
            .into_iter()
            .map(ManifestEntry::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let mut ids = BTreeSet::new();
        for entry in &entries {
            if !ids.insert(entry.id.clone()) {
                return Err(StateSchemaError::Invariant(
                    "duplicate manifest entry id".into(),
                ));
            }
            if entry.pinned != entry.pinned_to.is_some() {
                return Err(StateSchemaError::Invariant(
                    "pinned and pinnedTo must agree".into(),
                ));
            }
        }
        let pins = wire
            .pins
            .into_iter()
            .map(|v| SelectorId::new(&v).map_err(|e| field_error("pins", e)))
            .collect::<Result<Vec<_>, _>>()?;
        let pin_set = pins.iter().cloned().collect::<BTreeSet<_>>();
        let expected = entries
            .iter()
            .filter(|e| e.pinned)
            .map(|e| e.id.clone())
            .collect::<BTreeSet<_>>();
        if pin_set != expected {
            return Err(StateSchemaError::Invariant(
                "pins must exactly index pinned entries".into(),
            ));
        }
        Ok(Self {
            channel_seq,
            uid: wire.uid,
            entries,
            pins,
        })
    }
}

impl TryFrom<ManifestEntryWire> for ManifestEntry {
    type Error = StateSchemaError;
    fn try_from(w: ManifestEntryWire) -> Result<Self, Self::Error> {
        Ok(Self {
            id: SelectorId::new(&w.id).map_err(|e| field_error("id", e))?,
            selector: SelectorInput::new(&w.selector).map_err(|e| field_error("selector", e))?,
            attribute: AttributePath::new(&w.attribute).map_err(|e| field_error("attribute", e))?,
            version_preference: w.version_pref.try_into()?,
            outputs: match w.outputs {
                None => OutputSelection::default_selection(),
                Some(values) => OutputSelection::explicit(
                    values
                        .into_iter()
                        .map(|v| OutputName::new(&v).map_err(|e| field_error("outputs", e)))
                        .collect::<Result<Vec<_>, _>>()?,
                )
                .map_err(|e| field_error("outputs", e))?,
            },
            source_revision: SourceRevision::from_str(&w.source_rev)
                .map_err(|e| field_error("sourceRev", e))?,
            pinned: w.pinned,
            pinned_to: w
                .pinned_to
                .map(|v| StorePath::new(&v).map_err(|e| field_error("pinnedTo", e)))
                .transpose()?,
            added_at: nonempty("addedAt", w.added_at)?,
            origin: nonempty("origin", w.origin)?,
        })
    }
}

impl TryFrom<VersionPreferenceWire> for VersionPreference {
    type Error = StateSchemaError;
    fn try_from(w: VersionPreferenceWire) -> Result<Self, Self::Error> {
        Ok(match w {
            VersionPreferenceWire::Any => Self::Any,
            VersionPreferenceWire::Exact { version } => Self::Exact(PackageVersion::new(version)),
            VersionPreferenceWire::Min { version } => Self::Minimum(PackageVersion::new(version)),
            VersionPreferenceWire::Range { lower, upper } => Self::Range(
                VersionRange::new(
                    lower.map(bound_from_wire).transpose()?,
                    upper.map(bound_from_wire).transpose()?,
                )
                .map_err(|e| field_error("versionPref", e))?,
            ),
        })
    }
}

fn bound_from_wire(w: VersionBoundWire) -> Result<VersionBound, StateSchemaError> {
    let version = PackageVersion::new(w.version);
    Ok(if w.inclusive {
        VersionBound::inclusive(version)
    } else {
        VersionBound::exclusive(version)
    })
}

impl TryFrom<LockedStateWire> for LockedState {
    type Error = StateSchemaError;
    fn try_from(w: LockedStateWire) -> Result<Self, Self::Error> {
        require_v1(w.schema_version)?;
        let channel_seq = ChannelSequence::from_u64(w.channel_seq)
            .ok_or_else(|| field_error("channelSeq", "must be non-zero"))?;
        let system = System::from_str(&w.system).map_err(|e| field_error("system", e))?;
        let entries = w
            .entries
            .into_iter()
            .map(|(id, entry)| {
                Ok((
                    SelectorId::new(&id).map_err(|e| field_error("entries", e))?,
                    lock_entry_from_wire(entry, system)?,
                ))
            })
            .collect::<Result<_, StateSchemaError>>()?;
        Ok(Self {
            channel_seq,
            system,
            uid: w.uid,
            entries,
        })
    }
}

fn lock_entry_from_wire(
    w: LockEntryWire,
    lock_system: System,
) -> Result<LockEntry, StateSchemaError> {
    let attribute = AttributePath::new(&w.attribute).map_err(|e| field_error("attribute", e))?;
    let revision =
        NixpkgsRevision::new(&w.nixpkgs_rev).map_err(|e| field_error("nixpkgsRev", e))?;
    let r = w.realized;
    let system = System::from_str(&r.system).map_err(|e| field_error("realized.system", e))?;
    if system != lock_system {
        return Err(StateSchemaError::Invariant(
            "realized system must match lock system".into(),
        ));
    }
    let outputs = r
        .outputs
        .into_iter()
        .map(|(name, path)| {
            Ok((
                OutputName::new(&name).map_err(|e| field_error("realized.outputs", e))?,
                StorePath::new(&path).map_err(|e| field_error("realized.outputs", e))?,
            ))
        })
        .collect::<Result<_, StateSchemaError>>()?;
    let selected = r
        .outputs_to_install
        .into_iter()
        .map(|v| OutputName::new(&v).map_err(|e| field_error("outputsToInstall", e)))
        .collect::<Result<_, _>>()?;
    let realization = Realization::new(
        StorePath::new(&r.store_path).map_err(|e| field_error("storePath", e))?,
        DerivationPath::from_str(&r.deriver).map_err(|e| field_error("deriver", e))?,
        outputs,
        selected,
        system,
        revision,
        NarHash::new(&r.nar_hash).map_err(|e| field_error("narHash", e))?,
        r.closure_nar_size,
        nonempty("pname", r.pname)?,
        PackageVersion::new(r.version),
    )
    .map_err(|e| field_error("realized", e))?;
    Ok(LockEntry {
        attribute,
        realization,
        locked_at: nonempty("lockedAt", w.locked_at)?,
        provenance: nonempty("provenance", w.provenance)?,
        signatures_observed: w.sigs_observed,
    })
}

impl TryFrom<GenerationWire> for Generation {
    type Error = StateSchemaError;
    fn try_from(w: GenerationWire) -> Result<Self, Self::Error> {
        require_v1(w.schema_version)?;
        validate_generation_id("id", &w.id)?;
        if let Some(parent) = &w.parent {
            validate_generation_id("parent", parent)?;
        }
        let channel_seq = ChannelSequence::from_u64(w.channel_seq)
            .ok_or_else(|| field_error("channelSeq", "must be non-zero"))?;
        let outputs = w
            .outputs
            .into_iter()
            .map(GenerationOutput::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let mut output_ids = BTreeSet::new();
        if outputs
            .iter()
            .any(|output| !output_ids.insert(output.id.as_str()))
        {
            return Err(StateSchemaError::Invariant(
                "generation output ids must be unique".into(),
            ));
        }
        Ok(Self {
            uid: w.uid,
            id: w.id,
            parent: w.parent,
            created_at: nonempty("createdAt", w.created_at)?,
            channel_seq,
            manifest_hash: digest_text("manifestHash", w.manifest_hash)?,
            lock_hash: digest_text("lockHash", w.lock_hash)?,
            manifest_snapshot: relative_path("manifestSnapshot", w.manifest_snapshot)?,
            lock_snapshot: relative_path("lockSnapshot", w.lock_snapshot)?,
            activation: w.activation.try_into()?,
            outputs,
            operation: w.operation.try_into()?,
            generation_hash: digest_text("generationHash", w.generation_hash)?,
        })
    }
}

impl TryFrom<ActivationWire> for Activation {
    type Error = StateSchemaError;
    fn try_from(w: ActivationWire) -> Result<Self, Self::Error> {
        if w.kind != "pkg-symlink-forest" {
            return Err(field_error("activation.kind", "must be pkg-symlink-forest"));
        }
        let output_roots = w
            .output_roots
            .into_iter()
            .map(|v| StorePath::new(&v).map_err(|e| field_error("outputRoots", e)))
            .collect::<Result<Vec<_>, _>>()?;
        if !is_strictly_sorted_unique(output_roots.iter().map(StorePath::as_str)) {
            return Err(StateSchemaError::Invariant(
                "outputRoots must be sorted and unique".into(),
            ));
        }
        let collision_policy = CollisionPolicy::from(w.collision_policy);
        let collision_resolutions = w
            .collision_resolutions
            .into_iter()
            .map(CollisionResolution::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        if collision_policy == CollisionPolicy::Abort && !collision_resolutions.is_empty() {
            return Err(StateSchemaError::Invariant(
                "abort collision policy cannot contain resolutions".into(),
            ));
        }
        Ok(Self {
            tree_path: relative_path("treePath", w.tree_path)?,
            tree_digest: digest_text("treeDigest", w.tree_digest)?,
            entry_count: w.entry_count,
            collision_policy,
            output_roots,
            collision_resolutions,
        })
    }
}

impl From<CollisionPolicyWire> for CollisionPolicy {
    fn from(v: CollisionPolicyWire) -> Self {
        match v {
            CollisionPolicyWire::Abort => Self::Abort,
            CollisionPolicyWire::KeepFirst => Self::KeepFirst,
            CollisionPolicyWire::KeepLast => Self::KeepLast,
        }
    }
}

impl TryFrom<CollisionResolutionWire> for CollisionResolution {
    type Error = StateSchemaError;
    fn try_from(w: CollisionResolutionWire) -> Result<Self, Self::Error> {
        if w.losers.is_empty() {
            return Err(StateSchemaError::Invariant(
                "collision losers cannot be empty".into(),
            ));
        }
        let winner = CollisionChoice::try_from(w.winner)?;
        let losers = w
            .losers
            .into_iter()
            .map(CollisionChoice::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        if losers.iter().any(|loser| loser == &winner) {
            return Err(StateSchemaError::Invariant(
                "collision winner cannot also be a loser".into(),
            ));
        }
        if losers
            .iter()
            .enumerate()
            .any(|(index, loser)| losers[..index].contains(loser))
        {
            return Err(StateSchemaError::Invariant(
                "collision losers must be unique".into(),
            ));
        }
        Ok(Self {
            relative_path: relative_path("relativePath", w.relative_path)?,
            winner,
            losers,
        })
    }
}
impl TryFrom<CollisionChoiceWire> for CollisionChoice {
    type Error = StateSchemaError;
    fn try_from(w: CollisionChoiceWire) -> Result<Self, Self::Error> {
        Ok(Self {
            source_selector: SelectorId::new(&w.source_selector)
                .map_err(|e| field_error("sourceSelector", e))?,
            output: OutputName::new(&w.output).map_err(|e| field_error("output", e))?,
        })
    }
}
impl TryFrom<GenerationOutputWire> for GenerationOutput {
    type Error = StateSchemaError;
    fn try_from(w: GenerationOutputWire) -> Result<Self, Self::Error> {
        let outputs_to_install = w
            .outputs_to_install
            .into_iter()
            .map(|v| OutputName::new(&v).map_err(|e| field_error("outputsToInstall", e)))
            .collect::<Result<Vec<_>, _>>()?;
        if outputs_to_install.is_empty() {
            return Err(StateSchemaError::Invariant(
                "outputsToInstall cannot be empty".into(),
            ));
        }
        let mut output_names = BTreeSet::new();
        if outputs_to_install
            .iter()
            .any(|output| !output_names.insert(output.as_str()))
        {
            return Err(StateSchemaError::Invariant(
                "outputsToInstall must be unique".into(),
            ));
        }
        Ok(Self {
            id: SelectorId::new(&w.id).map_err(|e| field_error("id", e))?,
            attribute: AttributePath::new(&w.attribute).map_err(|e| field_error("attribute", e))?,
            nixpkgs_revision: NixpkgsRevision::new(&w.nixpkgs_rev)
                .map_err(|e| field_error("nixpkgsRev", e))?,
            store_path: StorePath::new(&w.store_path).map_err(|e| field_error("storePath", e))?,
            deriver: DerivationPath::from_str(&w.deriver).map_err(|e| field_error("deriver", e))?,
            outputs_to_install,
            nar_hash: NarHash::new(&w.nar_hash).map_err(|e| field_error("narHash", e))?,
            closure_nar_size: w.closure_nar_size,
            provenance: nonempty("provenance", w.provenance)?,
            pinned: w.pinned,
        })
    }
}
impl TryFrom<GenerationOperationWire> for GenerationOperation {
    type Error = StateSchemaError;
    fn try_from(w: GenerationOperationWire) -> Result<Self, Self::Error> {
        Ok(Self {
            op_id: nonempty("opId", w.op_id)?,
            kind: nonempty("kind", w.kind)?,
            approval: OperationApproval {
                build: nonempty("approval.build", w.approval.build)?,
            },
        })
    }
}

fn nonempty(field: &'static str, value: String) -> Result<String, StateSchemaError> {
    if value.is_empty() {
        Err(field_error(field, "must not be empty"))
    } else {
        Ok(value)
    }
}
fn digest_text(field: &'static str, value: String) -> Result<String, StateSchemaError> {
    Digest::from_str(&value).map_err(|error| field_error(field, error))?;
    Ok(value)
}
fn relative_path(field: &'static str, value: String) -> Result<String, StateSchemaError> {
    if value.is_empty()
        || value.starts_with('/')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        Err(field_error(field, "must be a normalized relative path"))
    } else {
        Ok(value)
    }
}
fn validate_generation_id(field: &'static str, value: &str) -> Result<(), StateSchemaError> {
    if value
        .strip_prefix("gen-")
        .is_some_and(|v| v.len() >= 4 && v.bytes().all(|b| b.is_ascii_digit()))
    {
        Ok(())
    } else {
        Err(field_error(field, "must match gen-<digits>"))
    }
}
fn is_strictly_sorted_unique<'a>(mut values: impl Iterator<Item = &'a str>) -> bool {
    let Some(mut previous) = values.next() else {
        return true;
    };
    for value in values {
        if previous >= value {
            return false;
        }
        previous = value;
    }
    true
}

// Serialization conversions deliberately live after validation conversions so
// persisted output can only originate from a validated public model.
impl From<&Manifest> for ManifestWire {
    fn from(v: &Manifest) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            channel_seq: v.channel_seq.get().get(),
            uid: v.uid,
            entries: v.entries.iter().map(ManifestEntryWire::from).collect(),
            pins: v.pins.iter().map(|x| x.as_str().into()).collect(),
        }
    }
}
impl From<&ManifestEntry> for ManifestEntryWire {
    fn from(v: &ManifestEntry) -> Self {
        Self {
            id: v.id.as_str().into(),
            selector: v.selector.as_str().into(),
            attribute: v.attribute.as_str().into(),
            version_pref: VersionPreferenceWire::from(&v.version_preference),
            outputs: v
                .outputs
                .explicit_outputs()
                .map(|xs| xs.iter().map(|x| x.as_str().into()).collect()),
            source_rev: v.source_revision.to_canonical_string(),
            pinned: v.pinned,
            pinned_to: v.pinned_to.as_ref().map(|x| x.as_str().into()),
            added_at: v.added_at.clone(),
            origin: v.origin.clone(),
        }
    }
}
impl From<&VersionPreference> for VersionPreferenceWire {
    fn from(v: &VersionPreference) -> Self {
        match v {
            VersionPreference::Any => Self::Any,
            VersionPreference::Exact(x) => Self::Exact {
                version: x.as_str().into(),
            },
            VersionPreference::Minimum(x) => Self::Min {
                version: x.as_str().into(),
            },
            VersionPreference::Range(r) => Self::Range {
                lower: r.lower().map(VersionBoundWire::from),
                upper: r.upper().map(VersionBoundWire::from),
            },
        }
    }
}
impl From<&VersionBound> for VersionBoundWire {
    fn from(v: &VersionBound) -> Self {
        Self {
            version: v.version().as_str().into(),
            inclusive: v.is_inclusive(),
        }
    }
}
impl From<&LockedState> for LockedStateWire {
    fn from(v: &LockedState) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            channel_seq: v.channel_seq.get().get(),
            system: v.system.as_str().into(),
            uid: v.uid,
            entries: v
                .entries
                .iter()
                .map(|(id, entry)| (id.as_str().into(), LockEntryWire::from(entry)))
                .collect(),
        }
    }
}
impl From<&LockEntry> for LockEntryWire {
    fn from(v: &LockEntry) -> Self {
        let r = &v.realization;
        Self {
            attribute: v.attribute.as_str().into(),
            nixpkgs_rev: r.nixpkgs_revision().as_str().into(),
            realized: RealizationWire {
                store_path: r.store_path().as_str().into(),
                deriver: r.deriver().as_str().into(),
                outputs: r
                    .outputs()
                    .iter()
                    .map(|(n, p)| (n.as_str().into(), p.as_str().into()))
                    .collect(),
                outputs_to_install: r
                    .outputs_to_install()
                    .iter()
                    .map(|x| x.as_str().into())
                    .collect(),
                system: r.system().as_str().into(),
                nar_hash: r.nar_hash().as_str().into(),
                closure_nar_size: r.closure_nar_size(),
                pname: r.pname().into(),
                version: r.version().as_str().into(),
            },
            locked_at: v.locked_at.clone(),
            provenance: v.provenance.clone(),
            sigs_observed: v.signatures_observed.clone(),
        }
    }
}
impl From<&Generation> for GenerationWire {
    fn from(v: &Generation) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            uid: v.uid,
            id: v.id.clone(),
            parent: v.parent.clone(),
            created_at: v.created_at.clone(),
            channel_seq: v.channel_seq.get().get(),
            manifest_hash: v.manifest_hash.clone(),
            lock_hash: v.lock_hash.clone(),
            manifest_snapshot: v.manifest_snapshot.clone(),
            lock_snapshot: v.lock_snapshot.clone(),
            activation: ActivationWire::from(&v.activation),
            outputs: v.outputs.iter().map(GenerationOutputWire::from).collect(),
            operation: GenerationOperationWire::from(&v.operation),
            generation_hash: v.generation_hash.clone(),
        }
    }
}
impl From<&Activation> for ActivationWire {
    fn from(v: &Activation) -> Self {
        Self {
            kind: "pkg-symlink-forest".into(),
            tree_path: v.tree_path.clone(),
            tree_digest: v.tree_digest.clone(),
            entry_count: v.entry_count,
            collision_policy: match v.collision_policy {
                CollisionPolicy::Abort => CollisionPolicyWire::Abort,
                CollisionPolicy::KeepFirst => CollisionPolicyWire::KeepFirst,
                CollisionPolicy::KeepLast => CollisionPolicyWire::KeepLast,
            },
            output_roots: v.output_roots.iter().map(|x| x.as_str().into()).collect(),
            collision_resolutions: v
                .collision_resolutions
                .iter()
                .map(CollisionResolutionWire::from)
                .collect(),
        }
    }
}
impl From<&CollisionResolution> for CollisionResolutionWire {
    fn from(v: &CollisionResolution) -> Self {
        Self {
            relative_path: v.relative_path.clone(),
            winner: CollisionChoiceWire::from(&v.winner),
            losers: v.losers.iter().map(CollisionChoiceWire::from).collect(),
        }
    }
}
impl From<&CollisionChoice> for CollisionChoiceWire {
    fn from(v: &CollisionChoice) -> Self {
        Self {
            source_selector: v.source_selector.as_str().into(),
            output: v.output.as_str().into(),
        }
    }
}
impl From<&GenerationOutput> for GenerationOutputWire {
    fn from(v: &GenerationOutput) -> Self {
        Self {
            id: v.id.as_str().into(),
            attribute: v.attribute.as_str().into(),
            nixpkgs_rev: v.nixpkgs_revision.as_str().into(),
            store_path: v.store_path.as_str().into(),
            deriver: v.deriver.as_str().into(),
            outputs_to_install: v
                .outputs_to_install
                .iter()
                .map(|x| x.as_str().into())
                .collect(),
            nar_hash: v.nar_hash.as_str().into(),
            closure_nar_size: v.closure_nar_size,
            provenance: v.provenance.clone(),
            pinned: v.pinned,
        }
    }
}
impl From<&GenerationOperation> for GenerationOperationWire {
    fn from(v: &GenerationOperation) -> Self {
        Self {
            op_id: v.op_id.clone(),
            kind: v.kind.clone(),
            approval: OperationApprovalWire {
                build: v.approval.build.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE: &str = "/nix/store/00000000000000000000000000000000-ripgrep";
    const DRV: &str = "/nix/store/11111111111111111111111111111111-ripgrep.drv";
    const NAR: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn manifest_json() -> String {
        r#"{"schemaVersion":1,"channelSeq":42,"uid":1001,"entries":[{"id":"sel_rg","selector":"ripgrep","attribute":"ripgrep","versionPref":{"kind":"any"},"outputs":null,"sourceRev":"channel:current","pinned":false,"pinnedTo":null,"addedAt":"2026-01-01T00:00:00Z","origin":"user:install"}],"pins":[]}"#.to_string()
    }

    fn generation_json() -> String {
        String::from_utf8(
            include_bytes!("../../../../fixtures/state-v1/generation-v1.json").to_vec(),
        )
        .unwrap()
    }

    fn generation_output_json(id: &str, outputs: &str) -> String {
        format!(
            r#"{{"id":"{id}","attribute":"ripgrep","nixpkgsRev":"0123456789abcdef0123456789abcdef01234567","storePath":"{STORE}","deriver":"{DRV}","outputsToInstall":{outputs},"narHash":"{NAR}","closureNarSize":42,"provenance":"cache:cache.nixos.org","pinned":false}}"#
        )
    }

    #[test]
    fn manifest_round_trip_uses_camel_case() {
        let parsed = Manifest::from_json(manifest_json().as_bytes()).unwrap();
        let encoded = String::from_utf8(parsed.to_json().unwrap()).unwrap();
        assert!(encoded.contains("\"schemaVersion\""));
        assert!(encoded.contains("\"versionPref\""));
        assert_eq!(Manifest::from_json(encoded.as_bytes()).unwrap(), parsed);
    }
    #[test]
    fn manifest_rejects_unknown_future_duplicate_and_bad_pins() {
        assert!(
            Manifest::from_json(
                manifest_json()
                    .replace("\"pins\":[]", "\"extra\":true,\"pins\":[]")
                    .as_bytes()
            )
            .is_err()
        );
        assert_eq!(
            Manifest::from_json(
                manifest_json()
                    .replace("\"schemaVersion\":1", "\"schemaVersion\":2")
                    .as_bytes()
            )
            .unwrap_err(),
            StateSchemaError::UnsupportedSchemaVersion(2)
        );
        assert!(
            Manifest::from_json(
                manifest_json()
                    .replace("\"uid\":1001", "\"uid\":1001,\"uid\":1002")
                    .as_bytes()
            )
            .is_err()
        );
        assert!(
            Manifest::from_json(
                manifest_json()
                    .replace("\"pins\":[]", "\"pins\":[\"sel_rg\"]")
                    .as_bytes()
            )
            .is_err()
        );
        assert!(
            Manifest::from_json(
                manifest_json()
                    .replace("\"kind\":\"any\"", "\"kind\":\"any\",\"kind\":\"exact\"")
                    .as_bytes()
            )
            .is_err()
        );
        assert!(Manifest::from_json(format!("{} garbage", manifest_json()).as_bytes()).is_err());
    }
    #[test]
    fn lock_round_trip_validates_strong_types() {
        let json = format!(
            r#"{{"schemaVersion":1,"channelSeq":42,"system":"x86_64-linux","uid":1001,"entries":{{"sel_rg":{{"attribute":"ripgrep","nixpkgsRev":"0123456789abcdef0123456789abcdef01234567","realized":{{"storePath":"{STORE}","deriver":"{DRV}","outputs":{{"out":"{STORE}"}},"outputsToInstall":["out"],"system":"x86_64-linux","narHash":"{NAR}","closureNarSize":42,"pname":"ripgrep","version":"14.1.0"}},"lockedAt":"2026-01-01T00:00:00Z","provenance":"cache:cache.nixos.org","sigsObserved":[]}}}}}}"#
        );
        let parsed = LockedState::from_json(json.as_bytes()).unwrap();
        assert_eq!(
            LockedState::from_json(&parsed.to_json().unwrap()).unwrap(),
            parsed
        );
        assert_eq!(parsed.entries().len(), 1);
    }

    #[test]
    fn committed_v1_fixtures_parse_and_generation_round_trips() {
        Manifest::from_json(include_bytes!(
            "../../../../fixtures/state-v1/manifest-v1.json"
        ))
        .unwrap();
        LockedState::from_json(include_bytes!("../../../../fixtures/state-v1/lock-v1.json"))
            .unwrap();
        let generation = Generation::from_json(include_bytes!(
            "../../../../fixtures/state-v1/generation-v1.json"
        ))
        .unwrap();
        assert_eq!(
            Generation::from_json(&generation.to_json().unwrap()).unwrap(),
            generation
        );
    }

    #[test]
    fn generation_rejects_ambiguous_outputs_and_collision_resolutions() {
        let output = generation_output_json("sel_rg", r#"["out"]"#);
        let duplicate_ids = generation_json().replace(
            "\"outputs\":[]",
            &format!("\"outputs\":[{output},{output}]"),
        );
        assert!(Generation::from_json(duplicate_ids.as_bytes()).is_err());

        for invalid_outputs in [r#"[]"#, r#"["out","out"]"#] {
            let output = generation_output_json("sel_rg", invalid_outputs);
            let json =
                generation_json().replace("\"outputs\":[]", &format!("\"outputs\":[{output}]"));
            assert!(Generation::from_json(json.as_bytes()).is_err());
        }

        let resolution = r#"{"relativePath":"bin/rg","winner":{"sourceSelector":"sel_rg","output":"out"},"losers":[{"sourceSelector":"sel_other","output":"out"}]}"#;
        let abort_with_resolution = generation_json().replace(
            "\"collisionResolutions\":[]",
            &format!("\"collisionResolutions\":[{resolution}]"),
        );
        assert!(Generation::from_json(abort_with_resolution.as_bytes()).is_err());

        let winner_as_loser = resolution.replace("sel_other", "sel_rg");
        let invalid_resolution = generation_json()
            .replace(
                "\"collisionPolicy\":\"abort\"",
                "\"collisionPolicy\":\"keep-first\"",
            )
            .replace(
                "\"collisionResolutions\":[]",
                &format!("\"collisionResolutions\":[{winner_as_loser}]"),
            );
        assert!(Generation::from_json(invalid_resolution.as_bytes()).is_err());
    }
}
