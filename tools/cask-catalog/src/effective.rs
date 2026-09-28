//! Effective-record computation: base merged with the target variation.
//!
//! A variation extends the base record generically (every key it carries
//! replaces the base value; explicit null replaces too). No whitelist:
//! whitelists drift from the data. Only `token` and `variations` are never
//! taken from a variation. A missing variation means the base record
//! already serves that target. Variation presence is never evidence of OS
//! support; that comes only from `supported_platforms` and `depends_on`.

use serde_json::Value;

/// One raw cask record with the fields the generator reads.
#[derive(Debug, Clone)]
pub struct RawRecord {
    /// The full raw JSON object (borrowed shapes vary too much to model).
    pub raw: Value,
}

impl RawRecord {
    /// Wrap a decoded record object.
    #[must_use]
    pub fn new(raw: Value) -> Self {
        Self { raw }
    }

    /// The token string, when present and a string.
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        self.raw.get("token").and_then(Value::as_str)
    }
}

/// The variation key that serves one catalog target.
///
/// `aarch64-darwin` maps to the `arm64_sequoia` platform (the declared
/// baseline is macOS 15.7.7, and Sequoia is macOS 15); its variation is
/// merged over the base when present because the base can be newer.
/// `x86_64-linux` merges `x86_64_linux`. Intel macOS variations are never
/// selected: x86_64-darwin is not a catalog target.
#[must_use]
pub fn variation_key_for(system: &str) -> Option<&'static str> {
    match system {
        "aarch64-darwin" => Some("arm64_sequoia"),
        "x86_64-linux" => Some("x86_64_linux"),
        _ => None,
    }
}

/// Compute the effective record for one target.
///
/// Errors when the record carries a variation for the target that is not
/// an object: that is a malformed record, never a silent base fallback.
pub fn effective_for(record: &RawRecord, system: &str) -> Result<Value, String> {
    let mut eff = record.raw.clone();
    let Some(key) = variation_key_for(system) else {
        return Ok(eff);
    };
    // A non-object `variations` field must never fall back to the base
    // record silently: the target lookup would just miss.
    let variations = match record.raw.get("variations") {
        None | Some(&Value::Null) => None,
        Some(v) => Some(
            v.as_object()
                .ok_or_else(|| "variations is not an object".to_string())?,
        ),
    };
    let Some(variation) = variations.and_then(|map| map.get(key)) else {
        return Ok(eff);
    };
    let map = variation
        .as_object()
        .ok_or_else(|| format!("variation {key} is not an object"))?;
    let object = eff
        .as_object_mut()
        .ok_or_else(|| "record is not an object".to_string())?;
    for (field, value) in map {
        if field == "token" || field == "variations" {
            continue;
        }
        object.insert(field.clone(), value.clone());
    }
    Ok(eff)
}

/// Read a nullable string field from an effective record.
#[must_use]
pub fn str_field(eff: &Value, key: &str) -> Option<String> {
    eff.get(key)?.as_str().map(ToString::to_string)
}

/// The display name: first entry of the `name` array.
#[must_use]
pub fn name_field(eff: &Value) -> Option<String> {
    let first = eff.get("name")?.as_array()?.first()?;
    first.as_str().map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(variations: &Value) -> RawRecord {
        RawRecord::new(json!({
            "token": "t", "version": "1", "url": "https://x/mac.zip",
            "sha256": "aaa", "name": ["T"],
            "supported_platforms": ["arm64_sequoia", "x86_64_linux"],
            "variations": variations
        }))
    }

    #[test]
    fn sha_only_variation_replaces_only_hash() {
        let eff = effective_for(
            &record(&json!({"x86_64_linux": {"sha256": "bbb"}})),
            "x86_64-linux",
        )
        .expect("effective");
        assert_eq!(str_field(&eff, "sha256").as_deref(), Some("bbb"));
        assert_eq!(str_field(&eff, "url").as_deref(), Some("https://x/mac.zip"));
    }

    #[test]
    fn darwin_merges_the_arm64_sequoia_variation() {
        let eff = effective_for(
            &record(&json!({
                "arm64_sequoia": {"url": "https://x/sequoia.zip", "sha256": "ccc"},
                "x86_64_linux": {"sha256": "bbb"}
            })),
            "aarch64-darwin",
        )
        .expect("effective");
        assert_eq!(
            str_field(&eff, "url").as_deref(),
            Some("https://x/sequoia.zip")
        );
        assert_eq!(str_field(&eff, "sha256").as_deref(), Some("ccc"));
        // Without an arm64_sequoia variation the base record serves darwin.
        let base = effective_for(
            &record(&json!({"x86_64_linux": {"sha256": "bbb"}})),
            "aarch64-darwin",
        )
        .expect("effective");
        assert_eq!(
            str_field(&base, "url").as_deref(),
            Some("https://x/mac.zip")
        );
    }

    #[test]
    fn generic_extends_every_key_including_containers_and_specs() {
        let eff = effective_for(
            &record(&json!({
                "x86_64_linux": {"container": {"type": "naked"},
                                  "url_specs": {"user_agent": ":fake"}}
            })),
            "x86_64-linux",
        )
        .expect("effective");
        assert_eq!(eff["container"]["type"], "naked");
        assert_eq!(eff["url_specs"]["user_agent"], ":fake");
    }

    #[test]
    fn artifacts_replacement_is_wholesale_and_null_sha_is_explicit() {
        let eff = effective_for(
            &record(&json!({"x86_64_linux": {"artifacts": [{"binary": ["op"]}], "sha256": null}})),
            "x86_64-linux",
        )
        .expect("effective");
        let artifacts = eff
            .get("artifacts")
            .and_then(Value::as_array)
            .expect("artifacts");
        assert_eq!(artifacts.len(), 1);
        assert!(artifacts[0].get("binary").is_some());
        assert!(eff.get("sha256").map(Value::is_null).unwrap_or(true));
    }

    #[test]
    fn malformed_variation_is_an_error_not_a_silent_base() {
        let result = effective_for(
            &record(&json!({"x86_64_linux": "not-an-object"})),
            "x86_64-linux",
        );
        assert!(result.unwrap_err().contains("not an object"));
        // A non-object `variations` field must not miss the target lookup
        // and silently serve the base record.
        let mut raw = json!({
            "token": "t", "url": "https://x/mac.zip", "sha256": "aaa",
            "variations": "garbage"
        });
        let result = effective_for(&RawRecord::new(raw.clone()), "x86_64-linux");
        assert!(result.unwrap_err().contains("variations"));
        raw.as_object_mut()
            .unwrap()
            .insert("variations".into(), serde_json::Value::Null);
        assert!(effective_for(&RawRecord::new(raw), "x86_64-linux").is_ok());
    }

    #[test]
    fn flags_are_strict_booleans_on_the_effective_record() {
        let mut raw = json!({"token": "t", "disabled": false, "deprecated": true});
        let eff = effective_for(&RawRecord::new(raw.clone()), "aarch64-darwin").expect("effective");
        assert_ne!(eff.get("disabled"), Some(&Value::Bool(true)));
        assert_eq!(eff.get("deprecated"), Some(&Value::Bool(true)));
        raw.as_object_mut()
            .unwrap()
            .insert("disabled".into(), json!(0.5));
        let eff = effective_for(&RawRecord::new(raw), "aarch64-darwin").expect("effective");
        assert_ne!(
            eff.get("disabled"),
            Some(&Value::Bool(true)),
            "numeric values are never truthy"
        );
    }
}
