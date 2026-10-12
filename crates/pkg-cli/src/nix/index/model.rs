//! Generated catalog protocol types.
use std::collections::BTreeMap;

/// The cask catalog index schema this client decodes (design D6).
///
/// The index is the generated `pkg-cask-catalog/4` envelope exposed by a
/// casks flake as `catalogIndex`. It carries one provenance object per
/// source (`inputs`), the target system list, and per-system status entries
/// keyed by the full entry identity `owner/tap/token` (the official source
/// is `homebrew/cask`). Schema 2 is gone: no compatibility layer exists.
pub const CATALOG_INDEX_SCHEMA: &str = "pkg-cask-catalog/4";

/// The source identity of the official generated cask catalog.
///
/// The official catalog is a schema-4 catalog like any tap catalog; its
/// source key is reserved and can never be added as a local tap.
pub const OFFICIAL_CASK_SOURCE: &str = "homebrew/cask";

/// The generator provenance recorded in a catalog index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CatalogGenerator {
    /// The generator name, for example `cask-catalog`.
    pub name: String,
    /// The generator version.
    pub version: String,
}

/// The raw-input provenance recorded for a locally imported tap source.
///
/// A local (tap) input's `url` is the canonical approved repository
/// origin, and this object carries what the importer actually fetched and
/// captured, including the backend envelope identity
/// (`pkg-tap-raw-export/1`). Unknown extra fields are accepted: the
/// backend envelope grows fields the client does not interpret. The
/// official catalog input has no `raw` object at all, so the field stays
/// optional and absent there. There is deliberately no `origin` inside
/// `raw`: the approved origin is the input's own `url`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawInputProvenance {
    /// The archive URL the importer fetched the raw checkout from.
    pub archive_url: String,
    /// The SHA-256 of the fetched archive.
    pub archive_sha256: String,
    /// The SHA-256 of the captured metadata (the import's provenance hash).
    pub capture_sha256: String,
    /// The backend raw-export envelope identity, for example
    /// `pkg-tap-raw-export/1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_schema: Option<String>,
}

/// The pinned input provenance recorded for one catalog source.
///
/// Unknown extra fields are accepted permissively on decode (generated
/// protocol data grows); the required identity fields must be present and
/// nonempty. On encode only the decoded fields are written back.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CatalogInput {
    /// The pinned source URL the catalog was generated from.
    pub url: String,
    /// The exact pinned revision.
    pub revision: String,
    /// The SHA-256 of the pinned input.
    pub sha256: String,
    /// The upstream data license attribution.
    pub license: String,
    /// The source kind, for example `official` or `tap`, when recorded.
    pub kind: Option<String>,
    /// The raw-input provenance, present only for locally imported taps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawInputProvenance>,
}

/// Whether one index entry is installable on its system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryStatus {
    /// The entry has a generated plan; a metadata claim, not a verified
    /// build.
    Eligible,
    /// The entry is excluded with a recorded reason.
    Excluded,
}

/// One per-system status entry of the catalog index.
///
/// Map keys are full entry identities `owner/tap/token`; the record must
/// repeat its `source` and bare `token` and both must match the key.
///
/// Null rules (validated in [`super::decode_catalog_index`]): `name`,
/// `description`, `version`, and `homepage` are string or null; an eligible
/// entry has a non-null `kind` and null `reason`/`detail`; an excluded entry
/// has a null `kind`, a non-null `reason`, and a string-or-null `detail`.
/// The full catalog projection lives in the backend flake; this client
/// decodes exactly one index envelope shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CatalogEntry {
    /// The source identity (`owner/tap`, or `homebrew/cask`); must match
    /// the source segment of the map key and name an `inputs` entry.
    pub source: String,
    /// The bare cask token; must equal the final segment of the map key.
    pub token: String,
    /// The upstream name, when the metadata provides one.
    pub name: Option<String>,
    /// The upstream description, when the metadata provides one.
    pub description: Option<String>,
    /// The effective version for this system, when known.
    pub version: Option<String>,
    /// The effective homepage for this system, when known.
    pub homepage: Option<String>,
    /// Whether the token is eligible or excluded on this system.
    pub status: EntryStatus,
    /// The package kind, for example `app+cli`; present when eligible.
    pub kind: Option<String>,
    /// The machine-readable exclusion reason; present when excluded.
    pub reason: Option<String>,
    /// Human-readable exclusion detail, when present.
    #[serde(default)]
    pub detail: Option<String>,
    /// The declared minimum macOS of the plan (`depends_on macos >=`),
    /// or null. Null on Linux records and when undeclared; validated as
    /// a numeric version by the decoder.
    #[serde(rename = "minMacos", default)]
    pub min_macos: Option<String>,
    /// The declared maximum macOS of the plan (`depends_on
    /// maximum_macos <=`), or null. Same null and validation rules as
    /// [`Self::min_macos`]; together they must form a coherent range.
    #[serde(rename = "maxMacos", default)]
    pub max_macos: Option<String>,
}

/// One system section of the catalog index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CatalogSystem {
    /// The status entries for this system, keyed by the full entry
    /// identity `owner/tap/token`.
    pub entries: BTreeMap<String, CatalogEntry>,
}

/// The decoded `catalogIndex` envelope of a casks flake.
///
/// The envelope serializes back to the exact shape it decodes from — the
/// `schema` identity and the `macosBaseline` field name included — so a
/// published generation's capture file is the same strict schema-4 index
/// the decoder consumes. No parallel encoding exists in this client.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CatalogIndex {
    /// The schema identity accepted by the strict decoder and written on encode.
    schema: String,
    /// The generator provenance.
    generator: CatalogGenerator,
    /// The pinned input provenance of every source in this catalog, keyed
    /// by source identity (`owner/tap`, or `homebrew/cask`).
    inputs: BTreeMap<String, CatalogInput>,
    /// The target systems the catalog was generated for.
    targets: Vec<String>,
    /// The declared macOS baseline, for example `15.7.7`.
    #[serde(rename = "macosBaseline")]
    macos_baseline: String,
    /// The per-system status sections.
    systems: BTreeMap<String, CatalogSystem>,
}

impl CatalogIndex {
    /// The schema accepted by the strict decoder.
    #[must_use]
    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// Generator provenance from the decoded envelope.
    #[must_use]
    pub const fn generator(&self) -> &CatalogGenerator {
        &self.generator
    }

    /// Pinned input provenance from the decoded envelope.
    #[must_use]
    pub const fn inputs(&self) -> &BTreeMap<String, CatalogInput> {
        &self.inputs
    }

    /// Target systems from the decoded envelope.
    #[must_use]
    pub fn targets(&self) -> &[String] {
        &self.targets
    }

    /// Declared macOS baseline from the decoded envelope.
    #[must_use]
    pub fn macos_baseline(&self) -> &str {
        &self.macos_baseline
    }

    /// Per-system entry sections from the decoded envelope.
    #[must_use]
    pub const fn systems(&self) -> &BTreeMap<String, CatalogSystem> {
        &self.systems
    }

    pub(super) fn from_decoded(
        schema: String,
        generator: CatalogGenerator,
        inputs: BTreeMap<String, CatalogInput>,
        targets: Vec<String>,
        macos_baseline: String,
        systems: BTreeMap<String, CatalogSystem>,
    ) -> Self {
        Self {
            schema,
            generator,
            inputs,
            targets,
            macos_baseline,
            systems,
        }
    }
}

impl<'de> serde::Deserialize<'de> for CatalogIndex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let schema = raw.get("schema").and_then(serde_json::Value::as_str);
        let result = match schema {
            Some(super::CATALOG_INDEX_SCHEMA) => super::decode_catalog_index(&raw),
            Some(super::FORMULA_INDEX_SCHEMA) => super::decode_formula_index(&raw),
            _ => {
                return Err(<D::Error as serde::de::Error>::custom(
                    "catalog index has no supported schema",
                ));
            }
        };
        result.map_err(<D::Error as serde::de::Error>::custom)
    }
}
