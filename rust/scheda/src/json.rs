//! Strict Protobuf JSON for `api.v1.Declaration`.
//!
//! Generated serde drops unknown keys. YAML declaration files already deny them.
//! JSON that a caller or the CLI feeds `create_collection` must fail the same
//! way, or a leftover `layout` / `fields` / `version` key is written as if it
//! were never there.

use serde_json::Value;

use crate::error::bail;
use crate::{Declaration, Error};

const DECLARATION_KEYS: &[&str] = &[
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

/// Parse Protobuf JSON `Declaration` and refuse unknown keys.
///
/// YAML declaration files already deny unknown keys. Generated proto JSON does
/// not: a leftover `layout` or `fields` key would be dropped and
/// `create_collection` would write a file. This parser walks the object first,
/// then deserializes and runs `require_valid`.
pub fn declaration_from_json(text: &str) -> Result<Declaration, Error> {
    let value: Value = serde_json::from_str(text).map_err(|err| Error::invalid(err.to_string()))?;
    declaration_from_value(&value)
}

/// Parse a JSON value as `Declaration` and refuse unknown keys.
pub fn declaration_from_value(value: &Value) -> Result<Declaration, Error> {
    reject_declaration(value)?;
    let decl: Declaration =
        serde_json::from_value(value.clone()).map_err(|err| Error::invalid(err.to_string()))?;
    decl.require_valid()?;
    Ok(decl)
}

fn reject_declaration(value: &Value) -> Result<(), Error> {
    let map = object(value, "declaration")?;
    reject_keys(map, DECLARATION_KEYS, "declaration")?;
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
    use super::{declaration_from_json, declaration_from_value};
    use crate::error::Code;
    use serde_json::json;

    #[test]
    fn layout_key_is_invalid() {
        let err = declaration_from_json(r#"{"document":{},"layout":["c/{id}.md"]}"#).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("layout"));
    }

    #[test]
    fn fields_key_is_invalid() {
        let err = declaration_from_value(&json!({
            "document": {},
            "fields": [{"name": "title", "string": {}}]
        }))
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("fields"));
    }

    #[test]
    fn known_declaration_parses() {
        let decl = declaration_from_json(r#"{"document":{}}"#).unwrap();
        assert!(decl.kind.is_some());
    }

    #[test]
    fn leftover_version_key_is_invalid() {
        let err = declaration_from_json(r#"{"version":1,"document":{}}"#).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("version"));
    }

    #[test]
    fn leftover_body_key_is_invalid() {
        let err = declaration_from_json(r#"{"document":{},"body":{}}"#).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("body"));
    }
}
