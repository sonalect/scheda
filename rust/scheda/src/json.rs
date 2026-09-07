//! Strict Protobuf JSON for `api.v1.Schema`.
//!
//! Generated serde drops unknown keys. YAML Schema files already deny them.
//! JSON that a caller or the CLI feeds `create_collection` must fail the same
//! way, or a leftover `layout` key is written as if it were never there.

use serde_json::Value;

use crate::error::bail;
use crate::{Error, Schema};

const SCHEMA_KEYS: &[&str] = &[
    "version",
    "fields",
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

const FIELD_KEYS: &[&str] = &[
    "name",
    "required",
    "nullable",
    "excludeFind",
    "exclude_find",
    "default",
    "string",
    "integer",
    "number",
    "boolean",
    "stringList",
    "string_list",
    "object",
    "objectList",
    "object_list",
    "edge",
];

const TEXT_KEYS: &[&str] = &[
    "excludeFts",
    "exclude_fts",
    "excludeVector",
    "exclude_vector",
];

const JOURNAL_KEYS: &[&str] = &[
    "segmentMaxBytes",
    "segment_max_bytes",
    "segmentMaxLines",
    "segment_max_lines",
    "daily",
];

const SINGLETON_KEYS: &[&str] = &["id"];

const OBJECT_KEYS: &[&str] = &["fields", "values"];

const EDGE_KEYS: &[&str] = &["collection", "one", "many", "strict", "text"];

const BODY_KEYS: &[&str] = &["noBody", "no_body", "text"];

/// Parse Protobuf JSON `Schema` and refuse unknown keys.
///
/// YAML Schema files already deny unknown keys. Generated proto JSON does
/// not: a leftover `layout` key would be dropped and `create_collection`
/// would write a file. This parser walks the object first, then deserializes
/// and runs `require_valid`.
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

fn reject_schema(value: &Value) -> Result<(), Error> {
    let map = object(value, "schema")?;
    reject_keys(map, SCHEMA_KEYS, "schema")?;
    if let Some(fields) = map.get("fields") {
        reject_field_list(fields, "schema fields")?;
    }
    if let Some(document) = map.get("document") {
        reject_body(document)?;
    }
    if let Some(journal) = map.get("journal") {
        reject_journal(journal)?;
    }
    if let Some(singleton) = map.get("singleton") {
        reject_singleton(singleton)?;
    }
    Ok(())
}

fn reject_field_list(value: &Value, what: &str) -> Result<(), Error> {
    let Some(items) = value.as_array() else {
        bail!(Error::invalid(format!("{what} must be a JSON array")));
    };
    for item in items {
        reject_field(item)?;
    }
    Ok(())
}

fn reject_field(value: &Value) -> Result<(), Error> {
    let map = object(value, "field")?;
    reject_keys(map, FIELD_KEYS, "field")?;
    if let Some(text) = map.get("string") {
        reject_text(text, "string")?;
    }
    if let Some(text) = map.get("stringList").or_else(|| map.get("string_list")) {
        reject_text(text, "string_list")?;
    }
    if let Some(unit) = map.get("integer") {
        reject_unit(unit, "integer")?;
    }
    if let Some(unit) = map.get("number") {
        reject_unit(unit, "number")?;
    }
    if let Some(unit) = map.get("boolean") {
        reject_unit(unit, "boolean")?;
    }
    if let Some(object) = map.get("object") {
        reject_object(object)?;
    }
    if let Some(object) = map.get("objectList").or_else(|| map.get("object_list")) {
        reject_object(object)?;
    }
    if let Some(edge) = map.get("edge") {
        reject_edge(edge)?;
    }
    Ok(())
}

fn reject_text(value: &Value, what: &str) -> Result<(), Error> {
    let map = object(value, what)?;
    reject_keys(map, TEXT_KEYS, what)
}

fn reject_unit(value: &Value, what: &str) -> Result<(), Error> {
    let map = object(value, what)?;
    reject_keys(map, &[], what)
}

fn reject_object(value: &Value) -> Result<(), Error> {
    let map = object(value, "object")?;
    reject_keys(map, OBJECT_KEYS, "object")?;
    if let Some(fields) = map.get("fields") {
        reject_field_list(fields, "object fields")?;
    }
    if let Some(values) = map.get("values") {
        reject_field(values)?;
    }
    Ok(())
}

fn reject_edge(value: &Value) -> Result<(), Error> {
    let map = object(value, "edge")?;
    reject_keys(map, EDGE_KEYS, "edge")?;
    if let Some(unit) = map.get("one") {
        reject_unit(unit, "one")?;
    }
    if let Some(unit) = map.get("many") {
        reject_unit(unit, "many")?;
    }
    if let Some(text) = map.get("text") {
        reject_text(text, "edge text")?;
    }
    Ok(())
}

fn reject_body(value: &Value) -> Result<(), Error> {
    let map = object(value, "document")?;
    reject_keys(map, BODY_KEYS, "document")?;
    if let Some(text) = map.get("text") {
        reject_text(text, "document text")?;
    }
    Ok(())
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
        let err =
            schema_from_json(r#"{"version":1,"document":{},"layout":["c/{id}.md"],"fields":[]}"#)
                .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("layout"));
    }

    #[test]
    fn nested_unknown_field_key_is_invalid() {
        let err = schema_from_value(&json!({
            "version": 1,
            "document": {},
            "fields": [{"name": "title", "string": {}, "layout": true}]
        }))
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn known_schema_parses() {
        let schema = schema_from_json(
            r#"{"version":1,"document":{},"fields":[{"name":"title","string":{}}]}"#,
        )
        .unwrap();
        assert_eq!(schema.version, Some(1));
    }
}
