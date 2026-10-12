//! Strict Protobuf JSON for `api.collection.v1.Schema` and on-disk
//! `CollectionInfo`.
//!
//! Generated serde drops unknown keys. Collection files and JSON that a caller
//! or the CLI feeds `create_collection` must fail the same way, or a leftover
//! `layout` / `fields` / `kind` / `body` / `declaration` key is written as if
//! it were never there.

use serde_json::Value;

use crate::error::bail;
use crate::{CollectionInfo, Error, Schema};
use scheda_api::api::v1::VaultMarker;

const COLLECTION_INFO_KEYS: &[&str] = &["version", "name", "schema"];

const VAULT_MARKER_KEYS: &[&str] = &["version"];

const SCHEMA_KEYS: &[&str] = &[
    "isolated",
    "encrypted",
    "excludeLexicalIndex",
    "exclude_lexical_index",
    "excludeVectorIndex",
    "exclude_vector_index",
    "document",
    "journal",
    "singleton",
];

const JOURNAL_KEYS: &[&str] = &[
    "segmentMaxBytes",
    "segment_max_bytes",
    "segmentMaxLines",
    "segment_max_lines",
    "daily",
];

const SINGLETON_KEYS: &[&str] = &["id"];

/// Parse Protobuf JSON `Schema` and refuse unknown keys.
///
/// Collection files refuse unknown keys the same way. Generated proto JSON does
/// not: a leftover `layout` or `fields` key would be dropped and
/// `create_collection` would write a file. This parser walks the object first,
/// then deserializes and runs `require_valid`. A leftover top-level
/// `declaration` key is invalid (retired name).
pub fn schema_from_json(text: &str) -> Result<Schema, Error> {
    let value: Value = serde_json::from_str(text).map_err(|err| Error::invalid(err.to_string()))?;
    schema_from_value(&value)
}

/// Parse a JSON value as `Schema` and refuse unknown keys.
pub fn schema_from_value(value: &Value) -> Result<Schema, Error> {
    reject_schema(value)?;
    let schema: Schema =
        serde_json::from_value(value.clone()).map_err(|err| Error::invalid(err.to_string()))?;
    schema.require_valid()?;
    Ok(schema)
}

/// Parse a JSON value as on-disk `CollectionInfo` and refuse unknown keys.
pub(crate) fn collection_info_from_value(value: &Value) -> Result<CollectionInfo, Error> {
    reject_collection_info(value)?;
    let info: CollectionInfo =
        serde_json::from_value(value.clone()).map_err(|err| Error::invalid(err.to_string()))?;
    info.require_valid()?;
    Ok(info)
}

/// Parse a JSON value as the on-disk `VaultMarker` and refuse unknown keys.
pub(crate) fn vault_marker_from_value(value: &Value) -> Result<VaultMarker, Error> {
    let map = object(value, "marker")?;
    reject_keys(map, VAULT_MARKER_KEYS, "marker")?;
    let marker: VaultMarker =
        serde_json::from_value(value.clone()).map_err(|err| Error::invalid(err.to_string()))?;
    marker.require_valid()?;
    Ok(marker)
}

fn reject_collection_info(value: &Value) -> Result<(), Error> {
    let map = object(value, "collection")?;
    reject_keys(map, COLLECTION_INFO_KEYS, "collection")?;
    if let Some(schema) = map.get("schema") {
        reject_schema(schema)?;
    }
    Ok(())
}

fn reject_schema(value: &Value) -> Result<(), Error> {
    let map = object(value, "schema")?;
    reject_keys(map, SCHEMA_KEYS, "schema")?;
    if let Some(document) = map.get("document") {
        reject_unit(document, "document")?;
    }
    if let Some(journal) = map.get("journal") {
        reject_journal(journal)?;
    }
    if let Some(singleton) = map.get("singleton") {
        reject_singleton(singleton)?;
    }
    Ok(())
}

fn reject_unit(value: &Value, what: &str) -> Result<(), Error> {
    let map = object(value, what)?;
    reject_keys(map, &[], what)
}

fn reject_journal(value: &Value) -> Result<(), Error> {
    let map = object(value, "journal")?;
    reject_keys(map, JOURNAL_KEYS, "journal")?;
    if let Some(daily) = map.get("daily") {
        reject_unit(daily, "daily")?;
    }
    Ok(())
}

fn reject_singleton(value: &Value) -> Result<(), Error> {
    let map = object(value, "singleton")?;
    reject_keys(map, SINGLETON_KEYS, "singleton")
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a serde_json::Map<String, Value>, Error> {
    value
        .as_object()
        .ok_or_else(|| Error::invalid(format!("{what} must be a JSON object")))
}

fn reject_keys(
    map: &serde_json::Map<String, Value>,
    allowed: &[&str],
    what: &str,
) -> Result<(), Error> {
    for key in map.keys() {
        if !allowed.iter().any(|allowed| *allowed == key) {
            bail!(Error::invalid(format!("unknown {what} key {key}")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{schema_from_json, schema_from_value};
    use crate::error::Code;
    use serde_json::json;

    #[test]
    fn layout_key_is_invalid() {
        let err = schema_from_json(r#"{"document":{},"layout":["c/{id}.md"]}"#).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("layout"));
    }

    #[test]
    fn fields_key_is_invalid() {
        let err = schema_from_value(&json!({
            "document": {},
            "fields": [{"name": "title", "string": {}}]
        }))
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("fields"));
    }

    #[test]
    fn known_schema_parses() {
        let schema = schema_from_json(r#"{"document":{}}"#).unwrap();
        assert!(schema.kind.is_some());
    }

    #[test]
    fn leftover_version_key_is_invalid() {
        let err = schema_from_json(r#"{"version":1,"document":{}}"#).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("version"));
    }

    #[test]
    fn leftover_body_key_is_invalid() {
        let err = schema_from_json(r#"{"document":{},"body":{}}"#).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("body"));
    }

    #[test]
    fn leftover_declaration_key_is_invalid() {
        let err = schema_from_json(r#"{"document":{},"declaration":{}}"#).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("declaration"));
    }
}
