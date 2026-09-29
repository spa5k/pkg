//! Strict JSON input for untrusted persisted state.
//!
//! `serde_json::Value` keeps the last of duplicate object keys silently, so
//! decoding through a `Value` cannot claim duplicate keys were rejected. The
//! registry and the saved catalog captures are untrusted persisted input:
//! they decode through this module, which rejects duplicate keys at every
//! object level and then validates the typed shape.

use serde::de::{self, DeserializeSeed, Visitor};
use serde_json::Value;

/// Decode `text` into `T`, rejecting duplicate keys in every object.
///
/// The duplicate rule is checked while the JSON streams; the typed decode
/// of `T` then runs over the checked value, so field-level strictness
/// (denied unknown fields, exact types) stays with each type.
pub fn from_str<T: de::DeserializeOwned>(what: &str, text: &str) -> Result<T, String> {
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let value = StrictSeed
        .deserialize(&mut deserializer)
        .and_then(|value| deserializer.end().map(|()| value))
        .map_err(|error| format!("{what} is malformed: {error}"))?;
    T::deserialize(value).map_err(|error| format!("{what} is malformed: {error}"))
}

/// One seed that routes every value through the duplicate-key visitor.
struct StrictSeed;

impl<'de> DeserializeSeed<'de> for StrictSeed {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

/// The visitor that builds one `Value` and rejects duplicate object keys.
struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::from(value))
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::from(value))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Ok(Value::from(value))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_string()))
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut rows = Vec::new();
        while let Some(row) = seq.next_element_seed(StrictSeed)? {
            rows.push(row);
        }
        Ok(Value::Array(rows))
    }

    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value_seed(StrictSeed)?;
            if object.insert(key.clone(), value).is_some() {
                return Err(de::Error::custom(format!(
                    "duplicate object key {key:?}; \
                     persisted state must not repeat keys"
                )));
            }
        }
        Ok(Value::Object(object))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Shape {
        #[allow(
            dead_code,
            reason = "the field exists to be strictly decoded; the assertions target the errors"
        )]
        value: String,
    }

    #[test]
    fn duplicate_keys_fail_at_every_level() {
        assert!(from_str::<Shape>("state", r#"{"value":"a"}"#).is_ok());
        for rejected in [
            r#"{"value":"a","value":"b"}"#,
            r#"{"a":{"x":1,"x":2},"value":"b"}"#,
            r#"{"a":[{"y":1,"y":2}],"value":"b"}"#,
        ] {
            let error = from_str::<Shape>("state", rejected)
                .err()
                .unwrap_or_else(|| panic!("{rejected} must be rejected"));
            assert!(error.contains("duplicate"), "{error}");
        }
        // Typed strictness still applies after the duplicate check:
        // a type error is denied.
        let error = from_str::<Shape>("state", r#"{"value":3}"#).expect_err("type errors fail");
        assert!(error.contains("malformed"), "{error}");
        // An unknown field is denied even though its key is distinct.
        assert!(from_str::<Shape>("state", r#"{"a":1,"value":"b"}"#).is_err());
    }
}
