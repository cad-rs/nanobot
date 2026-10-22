//! Canonical serialization helpers: the determinism convention the manifest
//! checksum is computed over ("canonical keys + CRC32").
//!
//! The canonical form is **JSON with object keys sorted recursively** — so a
//! checksum over it is stable across serializers and platforms, and does not
//! silently depend on serde_json's `preserve_order` feature (with it on,
//! `Value::Object` is an insertion-ordered map; sorting the keys alone would
//! not be enough — nested values are canonicalized too).

use serde::Serialize;

use crate::error::BootError;

/// Serializes `value` to canonical JSON: object keys sorted at every depth,
/// arrays in their given order (array order is content, not formatting).
pub(crate) fn to_canonical_json_string<T: Serialize + ?Sized>(
    value: &T,
) -> Result<String, BootError> {
    let value = serde_json::to_value(value)
        .map_err(|e| BootError::Serialize(format!("manifest -> json: {e}")))?;
    serde_json::to_string(&canonicalize(value))
        .map_err(|e| BootError::Serialize(format!("canonical json: {e}")))
}

fn canonicalize(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            // BTreeMap iteration is key-sorted; rebuilding the JSON map in that
            // order keeps it sorted even when serde_json's Map is an IndexMap
            // (preserve_order).
            let sorted: std::collections::BTreeMap<String, serde_json::Value> =
                map.into_iter().collect();
            serde_json::Value::Object(
                sorted
                    .into_iter()
                    .map(|(k, v)| (k, canonicalize(v)))
                    .collect(),
            )
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(canonicalize).collect())
        }
        other => other,
    }
}

/// CRC32 (IEEE).
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys_at_every_depth() {
        let value = serde_json::json!({
            "b": 1,
            "a": { "d": 2, "c": [ { "f": 3, "e": 4 } ] },
        });
        let canonical = to_canonical_json_string(&value).expect("canonical json");
        assert_eq!(canonical, r#"{"a":{"c":[{"e":4,"f":3}],"d":2},"b":1}"#);
    }

    #[test]
    fn crc32_matches_the_ieee_check_value() {
        // The standard CRC32 check value for "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
