//! SCH-8 / QRY-2: apply a protovalidate-checked `Filter` using Schema keys.

use serde_json::{Map, Value};

use scheda_generated::api::v1::filter_clause::Kind;
use scheda_generated::api::v1::{Equal, Exists, PathPrefix, Set};

use crate::schema::{CollectionDecl, Kind as CollectionKind, collection_dir};
use crate::{Error, Filter, Record};

pub(crate) fn is_unconstrained(filter: &Filter) -> bool {
    filter.clauses.is_empty()
}

pub(crate) fn validate(decl: &CollectionDecl, filter: &Filter) -> Result<(), Error> {
    for clause in &filter.clauses {
        match clause.kind.as_ref() {
            Some(Kind::Equal(equal)) => require_filter_key(decl, required_field(&equal.field))?,
            Some(Kind::Set(set)) => require_filter_key(decl, required_field(&set.field))?,
            Some(Kind::Exists(exists)) => require_filter_key(decl, required_field(&exists.field))?,
            Some(Kind::PathPrefix(_)) => require_document_path_prefix(decl)?,
            None => unreachable!("FilterClause kind is required"),
        }
    }
    Ok(())
}

pub(crate) fn matches(
    decl: &CollectionDecl,
    record: &Record,
    filter: &Filter,
) -> Result<bool, Error> {
    if filter.clauses.is_empty() {
        return Ok(true);
    }
    let fields = crate::store::fields_map(record)?;
    for clause in &filter.clauses {
        if !clause_matches(decl, record, &fields, clause.kind.as_ref())? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn clause_matches(
    decl: &CollectionDecl,
    record: &Record,
    fields: &Map<String, Value>,
    kind: Option<&Kind>,
) -> Result<bool, Error> {
    match kind {
        Some(Kind::Equal(equal)) => Ok(equal_matches(fields, equal)?),
        Some(Kind::Set(set)) => Ok(set_matches(fields, set)?),
        Some(Kind::Exists(exists)) => Ok(exists_matches(fields, exists)?),
        Some(Kind::PathPrefix(prefix)) => path_prefix_matches(decl, record, prefix),
        None => unreachable!("FilterClause kind is required"),
    }
}

fn equal_matches(fields: &Map<String, Value>, equal: &Equal) -> Result<bool, Error> {
    let field = required_field(&equal.field);
    let want = json_of(&equal.value)?;
    Ok(match fields.get(field) {
        None => false,
        Some(got) => got == &want,
    })
}

fn set_matches(fields: &Map<String, Value>, set: &Set) -> Result<bool, Error> {
    let field = required_field(&set.field);
    let members = json_list(&set.values)?;
    Ok(match fields.get(field) {
        None => false,
        Some(got) => members.iter().any(|member| got == member),
    })
}

fn exists_matches(fields: &Map<String, Value>, exists: &Exists) -> Result<bool, Error> {
    let field = required_field(&exists.field);
    Ok(fields.contains_key(field))
}

fn path_prefix_matches(
    decl: &CollectionDecl,
    record: &Record,
    prefix: &PathPrefix,
) -> Result<bool, Error> {
    require_document_path_prefix(decl)?;
    let prefix = match prefix.prefix.as_deref() {
        Some(value) if !value.is_empty() => value,
        _ => unreachable!("PathPrefix.prefix is required"),
    };
    let Some(path) = record.path.as_deref() else {
        return Ok(false);
    };
    if path.starts_with(prefix) {
        return Ok(true);
    }
    let root = collection_dir(&decl.name);
    Ok(path
        .strip_prefix(&root)
        .map(|rest| rest.trim_start_matches('/'))
        .is_some_and(|rel| rel.starts_with(prefix)))
}

fn require_document_path_prefix(decl: &CollectionDecl) -> Result<(), Error> {
    if decl.kind == CollectionKind::Journal {
        Err(Error::invalid(
            "path_prefix is only valid on document and singleton",
        ))
    } else {
        Ok(())
    }
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

fn required_field(field: &Option<String>) -> &str {
    match field.as_deref() {
        Some(name) if !name.is_empty() => name,
        _ => unreachable!("filter clause field is required"),
    }
}

fn json_of(value: &impl serde::Serialize) -> Result<Value, Error> {
    let json = serde_json::to_value(value)
        .map_err(|err| Error::invalid(format!("encode filter: {err}")))?;
    Ok(unwrap_wkt(json))
}

fn json_list(value: &impl serde::Serialize) -> Result<Vec<Value>, Error> {
    match json_of(value)? {
        Value::Array(items) => Ok(items),
        Value::Object(obj) => match obj.get("values") {
            Some(Value::Array(items)) => Ok(items.clone()),
            None => Ok(Vec::new()),
            Some(_) => Err(Error::invalid("set values must be a list")),
        },
        Value::Null => Ok(Vec::new()),
        _ => Err(Error::invalid("set values must be a list")),
    }
}

fn unwrap_wkt(value: Value) -> Value {
    let Some(obj) = value.as_object() else {
        return value;
    };
    if obj.contains_key("nullValue") || obj.contains_key("null_value") {
        return Value::Null;
    }
    if let Some(v) = obj.get("stringValue").or_else(|| obj.get("string_value")) {
        return v.clone();
    }
    if let Some(v) = obj.get("numberValue").or_else(|| obj.get("number_value")) {
        return v.clone();
    }
    if let Some(v) = obj.get("boolValue").or_else(|| obj.get("bool_value")) {
        return v.clone();
    }
    if let Some(v) = obj.get("listValue").or_else(|| obj.get("list_value")) {
        return unwrap_wkt(v.clone());
    }
    if let Some(v) = obj.get("structValue").or_else(|| obj.get("struct_value")) {
        return v.clone();
    }
    value
}
