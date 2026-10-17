//! Strict generated catalog decoding.
mod model;
#[cfg(test)]
mod tests;

use model::IndexDocument;
pub use model::{
    CATALOG_INDEX_SCHEMA, CatalogEntry, CatalogGenerator, CatalogIndex, CatalogInput,
    CatalogSystem, EntryStatus, OFFICIAL_CASK_SOURCE, RawInputProvenance,
};
use std::collections::BTreeMap;

const FORMULA_INDEX_SCHEMA: &str = "pkg-formula-catalog/1";

/// Decode one captured `catalogIndex` envelope (schema 4).
///
/// Require [`CATALOG_INDEX_SCHEMA`] and per-system entry maps. Each full
/// `owner/tap/token` key must match its record and name an input with provenance.
/// Invalid identity, status, or version bounds fail the whole decode.
/// Unknown provenance fields are accepted. The backend owns index projection.
/// # Errors
///
/// Returns an error when the value has an unsupported schema or invalid catalog fields.
pub fn decode_catalog_index(raw: &serde_json::Value) -> Result<CatalogIndex, super::NixError> {
    decode_index(raw, CATALOG_INDEX_SCHEMA, "cask catalog index")
}

/// Decode the Formula envelope with the shared identity and status checks.
/// # Errors
///
/// Returns an error when the value has an unsupported schema or invalid Formula fields.
pub fn decode_formula_index(raw: &serde_json::Value) -> Result<CatalogIndex, super::NixError> {
    decode_index(raw, FORMULA_INDEX_SCHEMA, "formula catalog index")
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
    let document: IndexDocument = serde_path_to_error::deserialize(raw)
        .map_err(|error| gate(format!("invalid index: {error}")))?;
    let inputs = &document.inputs;
    let targets = &document.targets;
    let systems = &document.systems;
    if inputs.is_empty() {
        return Err(gate(String::from(
            "the `inputs` provenance map is empty; a catalog names at least one source",
        )));
    }
    for (source, input) in inputs {
        let complete = [&input.url, &input.revision, &input.sha256, &input.license]
            .into_iter()
            .chain(
                input
                    .raw
                    .iter()
                    .flat_map(|raw| [&raw.archive_url, &raw.archive_sha256, &raw.capture_sha256]),
            )
            .all(|value| !value.trim().is_empty());
        if !valid_catalog_source(source) || !complete {
            return Err(gate(format!("source {source:?} has invalid provenance")));
        }
    }
    let mut declared = targets.iter().collect::<Vec<_>>();
    declared.sort_unstable();
    declared.dedup();
    if !declared.into_iter().eq(systems.keys()) {
        return Err(gate(format!(
            "declared targets {targets:?} differ from system sections {:?}",
            systems.keys().collect::<Vec<_>>()
        )));
    }
    for (system, section) in systems {
        for (full_id, entry) in &section.entries {
            validate_entry(full_id, entry, inputs)
                .map_err(|detail| gate(format!("{system}/{full_id}: {detail}")))?;
        }
    }
    Ok(CatalogIndex(document))
}

/// Validate one index entry record against its full-id key and the inputs.
///
/// The key must be `owner/tap/token` with a valid source slug and a valid
/// bare token; the record must repeat exactly that `source` and `token`;
/// the source must carry provenance in `inputs`; and the status null rules
/// must hold. Anything else fails the whole decode with the entry named.
fn validate_entry(
    full_id: &str,
    entry: &CatalogEntry,
    inputs: &BTreeMap<String, CatalogInput>,
) -> Result<(), String> {
    let Some((source, token)) = split_entry_id(full_id) else {
        return Err("the key is not a full owner/tap/token entry identity".to_string());
    };
    if entry.source != source || entry.token != token || !inputs.contains_key(source) {
        return Err(format!(
            "record source {:?} and token {:?} do not match its key and input provenance",
            entry.source, entry.token
        ));
    }
    let valid_status = match entry.status {
        EntryStatus::Eligible => {
            entry.kind.is_some() && entry.reason.is_none() && entry.detail.is_none()
        }
        EntryStatus::Excluded => entry.kind.is_none() && entry.reason.is_some(),
    };
    if !valid_status {
        return Err("record kind, reason, or detail violates its status".to_string());
    }
    if entry
        .min_macos
        .iter()
        .chain(entry.max_macos.iter())
        .any(|bound| super::host::parse_numeric_version(bound).is_none())
    {
        return Err("macOS bounds must be non-empty numeric versions".to_string());
    }
    if let (Some(min), Some(max)) = (&entry.min_macos, &entry.max_macos)
        && !super::host::in_range(min, None, Some(max))
    {
        return Err(format!(
            "macOS range is inverted: minMacos {min:?} exceeds maxMacos {max:?}"
        ));
    }
    Ok(())
}

pub use cask_catalog::token::{
    full_entry_id, is_valid_source as valid_catalog_source, is_valid_token as valid_catalog_token,
    split_entry_id,
};
