//! Deterministic construction of the schema-version 1 package index.

use std::collections::BTreeSet;
use std::fmt;

use jiff::Timestamp;
use pkg_core::state::{Digest, body_digest};
use pkg_core::{
    AttributePath, ChannelSequence, NixpkgsRevision, OutputName, SelectorInput, System,
};
use serde::{Deserialize, Serialize};

/// The only index schema emitted by this release.
pub const INDEX_SCHEMA_VERSION: u64 = 1;
/// Hard ceiling for one projection, before skipped/unsupported entries are removed.
pub const MAX_CANDIDATES: usize = 200_000;
/// Hard ceiling for serialized Nix projection input before JSON decoding.
pub const MAX_PROJECTION_BYTES: usize = 128 * 1024 * 1024;
/// Hard ceiling for every string supplied by the metadata projection.
pub const MAX_STRING_BYTES: usize = 4 * 1024;
/// Hard ceiling for each record's list-valued metadata field.
pub const MAX_LIST_ITEMS: usize = 256;

/// Authenticated inputs that identify one deterministic index artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildMetadata {
    channel_seq: ChannelSequence,
    system: System,
    nixpkgs_rev: NixpkgsRevision,
    generated_at: String,
}

impl BuildMetadata {
    /// Validates metadata and canonicalizes the RFC 3339 instant to UTC.
    pub fn new(
        channel_seq: ChannelSequence,
        system: System,
        nixpkgs_rev: NixpkgsRevision,
        generated_at: &str,
    ) -> Result<Self, IndexBuildError> {
        let timestamp = generated_at
            .parse::<Timestamp>()
            .map_err(|_| IndexBuildError::InvalidGeneratedAt)?;
        Ok(Self {
            channel_seq,
            system,
            nixpkgs_rev,
            generated_at: timestamp.to_string(),
        })
    }
}

/// One best-effort record emitted by the bounded Nix metadata projection.
///
/// Missing display fields are represented by `None` and normalize to empty
/// strings. A projection can mark an attribute `skipped` when `tryEval` caught
/// an ordinary evaluation failure.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IndexCandidate {
    /// Resolver-facing Nixpkgs attribute path.
    pub attr_path: String,
    /// Display package name.
    #[serde(default)]
    pub pname: Option<String>,
    /// Display package version.
    #[serde(default)]
    pub version: Option<String>,
    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,
    /// Human-readable homepage URL; never fetched by the index layer.
    #[serde(default)]
    pub homepage: Option<String>,
    /// Display license identifiers or names.
    #[serde(default)]
    pub licenses: Vec<String>,
    /// Best-effort supported-system triples.
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Best-effort availability signal for the target system.
    #[serde(default)]
    pub available_here: bool,
    /// Best-effort Nixpkgs broken flag.
    #[serde(default)]
    pub broken: bool,
    /// Sanitized source position, never a store path.
    #[serde(default)]
    pub position: Option<String>,
    /// Informational derivation output names.
    #[serde(default)]
    pub outputs: Vec<String>,
    /// Alternative resolver inputs for this canonical attribute.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Whether the Nix projection could not safely inspect this attribute.
    #[serde(default)]
    pub skipped: bool,
}

/// Provenance label stored in an index envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndexSource {
    /// Locally or publisher-side derived from the pinned Nixpkgs source.
    SelfBuilt,
}

/// A validated schema-version 1 index document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexDocument {
    schema_version: u64,
    channel_seq: u64,
    system: String,
    nixpkgs_rev: String,
    generated_at: String,
    source: IndexSource,
    records: Vec<IndexRecord>,
}

impl IndexDocument {
    /// Returns the schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u64 {
        self.schema_version
    }

    /// Returns the authenticated channel sequence used for this artifact.
    #[must_use]
    pub const fn channel_seq(&self) -> u64 {
        self.channel_seq
    }

    /// Returns the target system.
    #[must_use]
    pub fn system(&self) -> &str {
        &self.system
    }

    /// Returns the exact pinned Nixpkgs revision.
    #[must_use]
    pub fn nixpkgs_rev(&self) -> &str {
        &self.nixpkgs_rev
    }

    /// Returns the canonical UTC generation instant.
    #[must_use]
    pub fn generated_at(&self) -> &str {
        &self.generated_at
    }

    /// Returns the projection provenance.
    #[must_use]
    pub const fn source(&self) -> IndexSource {
        self.source
    }

    /// Returns the deterministic, attribute-sorted records.
    #[must_use]
    pub fn records(&self) -> &[IndexRecord] {
        &self.records
    }
}

/// One validated display-only catalog record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexRecord {
    attr_path: String,
    pname: String,
    version: String,
    description: String,
    homepage: String,
    licenses: Vec<String>,
    platforms: Vec<String>,
    available_here: bool,
    broken: bool,
    position: String,
    outputs: Vec<String>,
    aliases: Vec<String>,
}

impl IndexRecord {
    /// Returns the canonical resolver-facing attribute path.
    #[must_use]
    pub fn attr_path(&self) -> &str {
        &self.attr_path
    }

    /// Returns the display package name.
    #[must_use]
    pub fn pname(&self) -> &str {
        &self.pname
    }

    /// Returns the display version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the display description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the display homepage without fetching it.
    #[must_use]
    pub fn homepage(&self) -> &str {
        &self.homepage
    }

    /// Returns display license identifiers or names.
    #[must_use]
    pub fn licenses(&self) -> &[String] {
        &self.licenses
    }

    /// Returns best-effort supported systems.
    #[must_use]
    pub fn platforms(&self) -> &[String] {
        &self.platforms
    }

    /// Returns resolver aliases in deterministic order.
    #[must_use]
    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }

    /// Returns the sanitized source position, if the projection supplied one.
    #[must_use]
    pub fn position(&self) -> &str {
        &self.position
    }

    /// Returns informational output names.
    #[must_use]
    pub fn outputs(&self) -> &[String] {
        &self.outputs
    }

    /// Returns whether metadata marks this package broken.
    #[must_use]
    pub const fn broken(&self) -> bool {
        self.broken
    }

    /// Returns the best-effort target-system availability signal.
    #[must_use]
    pub const fn available_here(&self) -> bool {
        self.available_here
    }
}

/// Canonical bytes plus their exact-byte SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltIndex {
    document: IndexDocument,
    bytes: Vec<u8>,
    digest: Digest,
}

impl BuiltIndex {
    /// Returns the validated document.
    #[must_use]
    pub const fn document(&self) -> &IndexDocument {
        &self.document
    }

    /// Returns RFC 8785 canonical JSON bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the SHA-256 digest of [`Self::bytes`].
    #[must_use]
    pub const fn digest(&self) -> Digest {
        self.digest
    }

    /// Returns the lowercase, unprefixed SHA-256 used by channel target metadata.
    #[must_use]
    pub fn sha256_hex(&self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.digest.as_bytes() {
            use fmt::Write as _;
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
        }
        output
    }
}

/// Closed failures from projection validation or deterministic serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexBuildError {
    /// `generatedAt` was not a valid RFC 3339 instant.
    InvalidGeneratedAt,
    /// The projection exceeded a fixed resource bound.
    LimitExceeded(&'static str),
    /// A non-skipped candidate violated the closed index schema.
    InvalidCandidate {
        /// Attribute path, when it was safe to retain for diagnostics.
        attr_path: String,
        /// Redacted validation reason.
        reason: &'static str,
    },
    /// Two records used the same canonical attribute path.
    DuplicateAttribute(String),
    /// RFC 8785 serialization failed.
    Serialization(String),
    /// The bounded Nix projection was not valid closed-schema JSON.
    InvalidProjection(String),
}

impl fmt::Display for IndexBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGeneratedAt => f.write_str("generatedAt must be an RFC 3339 instant"),
            Self::LimitExceeded(name) => write!(f, "index projection exceeds {name} limit"),
            Self::InvalidCandidate { attr_path, reason } => {
                write!(f, "invalid index candidate {attr_path:?}: {reason}")
            }
            Self::DuplicateAttribute(path) => write!(f, "duplicate index attribute {path:?}"),
            Self::Serialization(reason) => write!(f, "could not canonicalize index: {reason}"),
            Self::InvalidProjection(reason) => write!(f, "invalid index projection: {reason}"),
        }
    }
}

/// Decodes a bounded, closed-schema Nix projection and builds its index.
pub fn build_index_from_json(
    metadata: BuildMetadata,
    projection: &[u8],
) -> Result<BuiltIndex, IndexBuildError> {
    check_projection_len(projection.len())?;
    let candidates = serde_json::from_slice::<Vec<IndexCandidate>>(projection)
        .map_err(|error| IndexBuildError::InvalidProjection(error.to_string()))?;
    build_index(metadata, candidates)
}

fn check_projection_len(length: usize) -> Result<(), IndexBuildError> {
    if length > MAX_PROJECTION_BYTES {
        Err(IndexBuildError::LimitExceeded("projection-byte"))
    } else {
        Ok(())
    }
}

impl std::error::Error for IndexBuildError {}

/// Validates, normalizes, sorts, canonicalizes, and hashes one index.
pub fn build_index(
    metadata: BuildMetadata,
    candidates: Vec<IndexCandidate>,
) -> Result<BuiltIndex, IndexBuildError> {
    if candidates.len() > MAX_CANDIDATES {
        return Err(IndexBuildError::LimitExceeded("candidate-count"));
    }

    let mut records = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if candidate.skipped || AttributePath::new(&candidate.attr_path).is_err() {
            continue;
        }
        records.push(normalize(candidate)?);
    }
    records.sort_unstable_by(|left, right| left.attr_path.cmp(&right.attr_path));
    for pair in records.windows(2) {
        if pair[0].attr_path == pair[1].attr_path {
            return Err(IndexBuildError::DuplicateAttribute(
                pair[0].attr_path.clone(),
            ));
        }
    }

    let document = IndexDocument {
        schema_version: INDEX_SCHEMA_VERSION,
        channel_seq: metadata.channel_seq.get().get(),
        system: metadata.system.to_string(),
        nixpkgs_rev: metadata.nixpkgs_rev.to_string(),
        generated_at: metadata.generated_at,
        source: IndexSource::SelfBuilt,
        records,
    };
    let bytes = serde_json_canonicalizer::to_vec(&document)
        .map_err(|error| IndexBuildError::Serialization(error.to_string()))?;
    let digest = body_digest(&bytes);
    Ok(BuiltIndex {
        document,
        bytes,
        digest,
    })
}

fn normalize(candidate: IndexCandidate) -> Result<IndexRecord, IndexBuildError> {
    let path = candidate.attr_path;
    let pname = checked_optional(&path, candidate.pname, "pname")?;
    let version = checked_optional(&path, candidate.version, "version")?;
    let description = checked_optional(&path, candidate.description, "description")?;
    let homepage = checked_optional(&path, candidate.homepage, "homepage")?;
    let position = checked_optional(&path, candidate.position, "position")?;
    let licenses = checked_list(&path, candidate.licenses, "licenses", |_| true)?;
    let platforms = checked_list(&path, candidate.platforms, "platforms", |value| {
        value.parse::<System>().is_ok()
    })?;
    let outputs = checked_list(&path, candidate.outputs, "outputs", |value| {
        OutputName::new(value).is_ok()
    })?;
    let aliases = checked_list(&path, candidate.aliases, "aliases", |value| {
        SelectorInput::new(value).is_ok()
    })?;

    Ok(IndexRecord {
        attr_path: path,
        pname,
        version,
        description,
        homepage,
        licenses,
        platforms,
        available_here: candidate.available_here,
        broken: candidate.broken,
        position,
        outputs,
        aliases,
    })
}

fn checked_optional(
    path: &str,
    value: Option<String>,
    field: &'static str,
) -> Result<String, IndexBuildError> {
    let value = value.unwrap_or_default();
    checked_string(path, &value, field)?;
    Ok(value)
}

fn checked_list(
    path: &str,
    values: Vec<String>,
    field: &'static str,
    valid: impl Fn(&str) -> bool,
) -> Result<Vec<String>, IndexBuildError> {
    if values.len() > MAX_LIST_ITEMS {
        return Err(invalid(path, "list contains too many items"));
    }
    let mut normalized = BTreeSet::new();
    for value in values {
        checked_string(path, &value, field)?;
        if value.is_empty() || !valid(&value) {
            return Err(invalid(path, "list contains an invalid item"));
        }
        normalized.insert(value);
    }
    Ok(normalized.into_iter().collect())
}

fn checked_string(path: &str, value: &str, _field: &'static str) -> Result<(), IndexBuildError> {
    if value.len() > MAX_STRING_BYTES {
        return Err(invalid(path, "string exceeds 4096 bytes"));
    }
    if value.contains("/nix/store/") || value.contains("/opt/pkg/nix/store/") {
        return Err(invalid(path, "store paths are forbidden in index metadata"));
    }
    Ok(())
}

fn invalid(path: &str, reason: &'static str) -> IndexBuildError {
    IndexBuildError::InvalidCandidate {
        attr_path: path.to_owned(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(at: &str) -> BuildMetadata {
        BuildMetadata::new(
            ChannelSequence::from_u64(42).unwrap(),
            System::Aarch64Darwin,
            NixpkgsRevision::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
            at,
        )
        .unwrap()
    }

    fn candidate(path: &str) -> IndexCandidate {
        IndexCandidate {
            attr_path: path.into(),
            pname: Some(path.into()),
            version: Some("1.0".into()),
            description: None,
            homepage: None,
            licenses: vec!["MIT".into()],
            platforms: vec!["aarch64-darwin".into()],
            available_here: true,
            broken: false,
            position: None,
            outputs: vec!["out".into()],
            aliases: Vec::new(),
            skipped: false,
        }
    }

    #[test]
    fn order_and_duplicates_in_lists_do_not_change_bytes() {
        let mut left = candidate("ripgrep");
        left.licenses = vec!["MIT".into(), "Apache-2.0".into(), "MIT".into()];
        let mut right = left.clone();
        right.licenses.reverse();
        let a = build_index(metadata("2025-01-01T00:00:00Z"), vec![left]).unwrap();
        let b = build_index(metadata("2024-12-31T19:00:00-05:00"), vec![right]).unwrap();
        assert_eq!(a.bytes(), b.bytes());
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn record_order_is_canonical_and_skips_failed_or_unsafe_attrs() {
        let built = build_index(
            metadata("2025-01-01T00:00:00Z"),
            vec![
                candidate("zoxide"),
                IndexCandidate {
                    skipped: true,
                    ..candidate("throwing")
                },
                candidate("bad+attribute"),
                candidate("bat"),
            ],
        )
        .unwrap();
        let paths: Vec<_> = built
            .document()
            .records()
            .iter()
            .map(IndexRecord::attr_path)
            .collect();
        assert_eq!(paths, ["bat", "zoxide"]);
    }

    #[test]
    fn duplicate_attributes_fail_closed() {
        let error = build_index(
            metadata("2025-01-01T00:00:00Z"),
            vec![candidate("bat"), candidate("bat")],
        )
        .unwrap_err();
        assert_eq!(error, IndexBuildError::DuplicateAttribute("bat".into()));
    }

    #[test]
    fn store_material_and_oversized_strings_are_rejected() {
        let mut store = candidate("bad-store");
        store.description = Some("see /nix/store/abc-secret".into());
        assert!(matches!(
            build_index(metadata("2025-01-01T00:00:00Z"), vec![store]),
            Err(IndexBuildError::InvalidCandidate { .. })
        ));

        let mut huge = candidate("huge");
        huge.description = Some("x".repeat(MAX_STRING_BYTES + 1));
        assert!(matches!(
            build_index(metadata("2025-01-01T00:00:00Z"), vec![huge]),
            Err(IndexBuildError::InvalidCandidate { .. })
        ));
    }

    #[test]
    fn malformed_timestamp_is_rejected() {
        assert_eq!(
            BuildMetadata::new(
                ChannelSequence::from_u64(1).unwrap(),
                System::X8664Linux,
                NixpkgsRevision::new("0123456789abcdef0123456789abcdef01234567").unwrap(),
                "yesterday",
            ),
            Err(IndexBuildError::InvalidGeneratedAt)
        );
    }

    #[test]
    fn channel_hash_is_unprefixed_lowercase_hex() {
        let built = build_index(metadata("2025-01-01T00:00:00Z"), vec![candidate("bat")]).unwrap();
        assert_eq!(built.sha256_hex().len(), 64);
        assert!(
            built
                .sha256_hex()
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_eq!(
            built.digest().to_string(),
            format!("sha256-{}", built.sha256_hex())
        );
    }

    #[test]
    fn projection_json_is_closed_and_bounded_before_decode() {
        let unknown = br#"[{"attrPath":"bat","storePath":"/nix/store/forbidden"}]"#;
        assert!(matches!(
            build_index_from_json(metadata("2025-01-01T00:00:00Z"), unknown),
            Err(IndexBuildError::InvalidProjection(_))
        ));
        assert_eq!(
            check_projection_len(MAX_PROJECTION_BYTES + 1),
            Err(IndexBuildError::LimitExceeded("projection-byte"))
        );
    }
}
