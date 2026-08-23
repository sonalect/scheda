//! SCH-8 / QRY-2: apply `api/v1` `Filter` to a Record.

use serde_json::{Map, Value};

use crate::schema::{CollectionDecl, Kind, collection_dir};
use crate::{Error, Filter, Record};

pub(crate) fn validate(decl: &CollectionDecl, filter: &Filter) -> Result<(), Error> {
    let json = serde_json::to_value(filter)
        .map_err(|err| Error::invalid(format!("encode filter: {err}")))?;
    let clauses = json
        .get("clauses")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    check_clauses(decl, clauses)
}

pub(crate) fn matches(
    decl: &CollectionDecl,
    record: &Record,
    filter: &Filter,
) -> Result<bool, Error> {
    let json = serde_json::to_value(filter)
        .map_err(|err| Error::invalid(format!("encode filter: {err}")))?;
    let clauses = json
        .get("clauses")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if clauses.is_empty() {
        return Ok(true);
    }
    let fields = crate::store::fields_map(record)?;
    for clause in clauses {
        if !clause_matches(decl, record, &fields, clause)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn check_clauses(decl: &CollectionDecl, clauses: &[Value]) -> Result<(), Error> {
    for clause in clauses {
        let Some(obj) = clause.as_object() else {
            return Err(Error::invalid("filter clause must be an object"));
        };
        if let Some(equal) = obj.get("equal") {
            require_filter_key(decl, clause_field(equal)?)?;
            continue;
        }
        if let Some(set) = obj.get("set") {
            require_filter_key(decl, clause_field(set)?)?;
            continue;
        }
        if let Some(exists) = obj.get("exists") {
            require_filter_key(decl, clause_field(exists)?)?;
            continue;
        }
        if obj.contains_key("pathPrefix") || obj.contains_key("path_prefix") {
            if decl.kind == Kind::Journal {
                return Err(Error::invalid(
                    "path_prefix is only valid on document and singleton",
                ));
            }
            continue;
        }
        return Err(Error::invalid("filter clause is missing kind"));
    }
    Ok(())
}

fn clause_matches(
    decl: &CollectionDecl,
    record: &Record,
    fields: &Map<String, Value>,
    clause: &Value,
) -> Result<bool, Error> {
    let Some(obj) = clause.as_object() else {
        return Err(Error::invalid("filter clause must be an object"));
    };
    if let Some(equal) = obj.get("equal") {
        let field = clause_field(equal)?;
        require_filter_key(decl, field)?;
        let want = proto_json_value(equal.get("value").unwrap_or(&Value::Null));
        return Ok(match fields.get(field) {
            None => false,
            Some(got) => json_eq(got, want),
        });
    }
    if let Some(set) = obj.get("set") {
        let field = clause_field(set)?;
        require_filter_key(decl, field)?;
        let members = proto_json_list(set.get("values"));
        return Ok(match fields.get(field) {
            None => false,
            Some(got) => members.iter().any(|member| json_eq(got, member)),
        });
    }
    if let Some(exists) = obj.get("exists") {
        let field = clause_field(exists)?;
        require_filter_key(decl, field)?;
        return Ok(fields.contains_key(field));
    }
    if let Some(prefix) = obj.get("pathPrefix").or_else(|| obj.get("path_prefix")) {
        if decl.kind == Kind::Journal {
            return Err(Error::invalid(
                "path_prefix is only valid on document and singleton",
            ));
        }
        let prefix = prefix
            .get("prefix")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid("path_prefix.prefix is required"))?;
        return Ok(path_prefix_matches(decl, record, prefix));
    }
    Err(Error::invalid("filter clause is missing kind"))
}

fn require_filter_key(decl: &CollectionDecl, field: &str) -> Result<(), Error> {
    if decl.filter_keys.contains(field) {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "field {field} is not a filter key on {}",
            decl.name
        )))
    }
}

fn clause_field(node: &Value) -> Result<&str, Error> {
    node.get("field")
        .and_then(Value::as_str)
        .filter(|field| !field.is_empty())
        .ok_or_else(|| Error::invalid("filter clause field is required"))
}

fn path_prefix_matches(decl: &CollectionDecl, record: &Record, prefix: &str) -> bool {
    let Some(path) = record.path.as_deref() else {
        return false;
    };
    if path.starts_with(prefix) {
        return true;
    }
    let root = collection_dir(&decl.name);
    path.strip_prefix(&root)
        .map(|rest| rest.trim_start_matches('/'))
        .is_some_and(|rel| rel.starts_with(prefix))
}

fn proto_json_value(value: &Value) -> &Value {
    let Some(obj) = value.as_object() else {
        return value;
    };
    if obj.contains_key("nullValue") || obj.contains_key("null_value") {
        return &Value::Null;
    }
    if let Some(v) = obj.get("stringValue").or_else(|| obj.get("string_value")) {
        return v;
    }
    if let Some(v) = obj.get("numberValue").or_else(|| obj.get("number_value")) {
        return v;
    }
    if let Some(v) = obj.get("boolValue").or_else(|| obj.get("bool_value")) {
        return v;
    }
    if let Some(v) = obj.get("listValue").or_else(|| obj.get("list_value")) {
        return proto_json_value(v);
    }
    if let Some(v) = obj.get("structValue").or_else(|| obj.get("struct_value")) {
        return v;
    }
    value
}

fn proto_json_list(values: Option<&Value>) -> Vec<Value> {
    let Some(values) = values else {
        return Vec::new();
    };
    if let Some(arr) = values.as_array() {
        return arr
            .iter()
            .map(|item| proto_json_value(item).clone())
            .collect();
    }
    if let Some(inner) = values.get("values").and_then(Value::as_array) {
        return inner
            .iter()
            .map(|item| proto_json_value(item).clone())
            .collect();
    }
    Vec::new()
}

fn json_eq(left: &Value, right: &Value) -> bool {
    left == right
}
