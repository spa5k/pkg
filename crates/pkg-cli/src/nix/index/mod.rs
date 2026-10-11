//! Strict generated catalog decoding.
mod model;
#[cfg(test)]
mod tests;

pub use model::{
    CATALOG_INDEX_SCHEMA, CatalogEntry, CatalogGenerator, CatalogIndex, CatalogInput,
    CatalogSystem, EntryStatus, OFFICIAL_CASK_SOURCE, RawInputProvenance,
};
use serde::Deserialize as _;
use std::collections::BTreeMap;

/// Decode one captured `catalogIndex` envelope (schema 4).
///
/// Require [`CATALOG_INDEX_SCHEMA`] and per-system entry maps. Each full
/// `owner/tap/token` key must match its record and name an input with provenance.
/// Invalid identity, status, or version bounds fail the whole decode.
/// Unknown provenance fields are accepted. The backend owns index projection.
pub fn decode_catalog_index(raw: &serde_json::Value) -> Result<CatalogIndex, super::NixError> {
    decode_index(raw, CATALOG_INDEX_SCHEMA, "cask catalog index")
}

/// Decode the Formula envelope with the shared identity and status checks.
pub fn decode_formula_index(raw: &serde_json::Value) -> Result<CatalogIndex, super::NixError> {
    decode_index(raw, "pkg-formula-catalog/1", "formula catalog index")
}

fn decode_index(
    raw: &serde_json::Value,
    expected: &str,
    what: &'static str,
) -> Result<CatalogIndex, super::NixError> {
    let gate = |detail: String| super::NixError::Decode { what, detail };
    let found = raw.get("schema").and_then(serde_json::Value::as_str);
    let Some(schema) = found else {
        return Err(gate(format!(
            "the index reports no schema; schema {expected:?} is required"
        )));
    };
    if schema != expected {
        return Err(gate(format!(
            "index schema {schema:?} is not the decoded schema {expected:?}"
        )));
    }
    #[derive(serde::Deserialize)]
    struct Header {
        generator: CatalogGenerator,
        inputs: BTreeMap<String, CatalogInput>,
        targets: Vec<String>,
        #[serde(rename = "macosBaseline")]
        macos_baseline: String,
    }
    let Header {
        generator,
        inputs,
        targets,
        macos_baseline,
    } = Header::deserialize(raw).map_err(|error| gate(format!("invalid index header: {error}")))?;
    if inputs.is_empty() {
        return Err(gate(String::from(
            "the `inputs` provenance map is empty; a catalog names at least one source",
        )));
    }
    for (source, input) in &inputs {
        if !valid_catalog_source(source) {
            return Err(gate(format!(
                "inputs key {source:?} is not a valid owner/tap source identity"
            )));
        }
        for (field, value) in [
            ("url", &input.url),
            ("revision", &input.revision),
            ("sha256", &input.sha256),
            ("license", &input.license),
        ] {
            if value.trim().is_empty() {
                return Err(gate(format!(
                    "inputs.{source}.{field} must be a nonempty string"
                )));
            }
        }
        if let Some(raw) = &input.raw {
            for (field, value) in [
                ("raw.archiveUrl", &raw.archive_url),
                ("raw.archiveSha256", &raw.archive_sha256),
                ("raw.captureSha256", &raw.capture_sha256),
            ] {
                if value.trim().is_empty() {
                    return Err(gate(format!(
                        "inputs.{source}.{field} must be a nonempty string"
                    )));
                }
            }
        }
    }

    // The per-system sections are the one decoded layout; an envelope
    // without them is not an index envelope.
    let Some(systems_raw) = raw.get("systems").and_then(serde_json::Value::as_object) else {
        return Err(gate(String::from(
            "the `systems` per-system sections are absent from the index",
        )));
    };
    for target in &targets {
        if !systems_raw.contains_key(target) {
            return Err(gate(format!(
                "target system {target:?} has no `systems` section; \
                 the envelope is inconsistent"
            )));
        }
    }
    for system in systems_raw.keys() {
        if !targets.contains(system) {
            return Err(gate(format!(
                "`systems` carries {system:?} which is not a declared target"
            )));
        }
    }

    let mut systems = BTreeMap::new();
    for (system, section) in systems_raw {
        let entries_raw = section
            .get("entries")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| gate(format!("system {system:?} has no `entries` map")))?;
        let mut entries = BTreeMap::new();
        for (full_id, record) in entries_raw {
            let entry = validate_entry(full_id, record, &inputs)
                .map_err(|detail| gate(format!("{system}/{full_id}: {detail}")))?;
            entries.insert(full_id.clone(), entry);
        }
        systems.insert(system.clone(), CatalogSystem { entries });
    }
    Ok(CatalogIndex {
        schema: expected.to_string(),
        generator,
        inputs,
        targets,
        macos_baseline: macos_baseline.to_string(),
        systems,
    })
}

/// Validate one index entry record against its full-id key and the inputs.
///
/// The key must be `owner/tap/token` with a valid source slug and a valid
/// bare token; the record must repeat exactly that `source` and `token`;
/// the source must carry provenance in `inputs`; and the status null rules
/// must hold. Anything else fails the whole decode with the entry named.
fn validate_entry(
    full_id: &str,
    record: &serde_json::Value,
    inputs: &BTreeMap<String, CatalogInput>,
) -> Result<CatalogEntry, String> {
    let Some((source, token)) = split_entry_id(full_id) else {
        return Err(String::from(
            "the key is not a full owner/tap/token entry identity",
        ));
    };
    let entry = CatalogEntry::deserialize(record)
        .map_err(|error| format!("does not follow the schema: {error}"))?;
    if entry.source != source {
        return Err(format!(
            "record source {:?} does not match its key {source:?}",
            entry.source
        ));
    }
    if entry.token != token {
        return Err(format!(
            "record token {:?} does not match its key {token:?}",
            entry.token
        ));
    }
    if !inputs.contains_key(source) {
        return Err(format!(
            "record source {source:?} has no `inputs` provenance"
        ));
    }
    match entry.status {
        EntryStatus::Eligible => {
            if entry.kind.is_none() {
                return Err(String::from(
                    "an eligible entry must carry a non-null `kind`",
                ));
            }
            if entry.reason.is_some() {
                return Err(String::from("an eligible entry must carry a null `reason`"));
            }
            if entry.detail.is_some() {
                return Err(String::from("an eligible entry must carry a null `detail`"));
            }
        }
        EntryStatus::Excluded => {
            if entry.kind.is_some() {
                return Err(String::from("an excluded entry must carry a null `kind`"));
            }
            if entry.reason.is_none() {
                return Err(String::from(
                    "an excluded entry must carry a non-null `reason`",
                ));
            }
        }
    }
    // The declared macOS range fails closed: each bound must be a
    // non-empty numeric version, and together they must be coherent.
    for (field, value) in [
        ("minMacos", &entry.min_macos),
        ("maxMacos", &entry.max_macos),
    ] {
        match value {
            None => {}
            Some(value) if super::host::parse_numeric_version(value).is_none() => {
                return Err(format!(
                    "{field} must be a non-empty numeric version, found {value:?}"
                ));
            }
            Some(_) => {}
        }
    }
    if let (Some(min), Some(max)) = (&entry.min_macos, &entry.max_macos)
        && !super::host::in_range(min, None, Some(max))
    {
        return Err(format!(
            "macOS range is inverted: minMacos {min:?} exceeds maxMacos {max:?}"
        ));
    }
    Ok(entry)
}

pub use cask_catalog::token::{
    full_entry_id, is_valid_source as valid_catalog_source, is_valid_token as valid_catalog_token,
    split_entry_id,
};
