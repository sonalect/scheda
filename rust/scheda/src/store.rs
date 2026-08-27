//! On-disk `document` / `singleton` files. Journal segments are `journal`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use buffa::MessageField;
use buffa_types::google::protobuf::__buffa::oneof::value::Kind as ProtoKind;
use buffa_types::google::protobuf::{
    ListValue, NullValue, Struct as ProtoStruct, Value as ProtoValue,
};
use serde_json::{Map, Value};

use crate::error::{Code, ContextAs, ContextIo, bail};
use crate::schema::{CollectionDecl, FieldDecl, FieldType, Kind, Schema};
use crate::{Error, Record};

static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

const FS_NAME_MAX: usize = 255;

/// Consecutive integers in IEEE 754 binary64. `google.protobuf.Value`
/// `number_value` is that type; an `integer` field beyond this is not exact.
const BINARY64_EXACT_INT_MAGNITUDE: u64 = 1 << 53;

pub(crate) fn insert_document(
    vault: &Path,
    decl: &CollectionDecl,
    record: Record,
) -> Result<(String, String), Error> {
    let prepared = prepare_write(vault, decl, record)?;
    if prepared.path.exists() {
        bail!(Error::duplicate_id(format!(
            "record {} already exists in {}",
            prepared.id, decl.name
        )));
    }
    atomic_write(&prepared.path, &prepared.bytes)?;
    crate::isolated::restrict_record(vault, &prepared.path)?;
    let rel = rel_posix(vault, &prepared.path)?;
    Ok((prepared.id, rel))
}

pub(crate) fn replace_document(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
    record: Record,
) -> Result<(String, String), Error> {
    let Some(existing) = locate_file(vault, decl, id)? else {
        bail!(Error::not_found(format!(
            "record {id} is not in {}",
            decl.name
        )));
    };
    let prepared = prepare_write(vault, decl, record)?;
    if prepared.id != id {
        bail!(Error::invalid(format!(
            "replace id {id} does not match record id {}",
            prepared.id
        )));
    }
    if prepared.path != existing {
        bail!(Error::invalid(format!(
            "replace would move {id} off its path in {}",
            decl.name
        )));
    }
    atomic_write(&prepared.path, &prepared.bytes)?;
    crate::isolated::restrict_record(vault, &prepared.path)?;
    let rel = rel_posix(vault, &prepared.path)?;
    Ok((prepared.id, rel))
}

/// Vault-relative POSIX path of the Record file for `id`, whether or not
/// the file exists. `journal` is not a document path.
pub(crate) fn record_rel(decl: &CollectionDecl, id: &str) -> Result<String, Error> {
    Ok(vault_rel(decl, &record_file(decl, id)?))
}

/// Unlink the Record file. Missing → `not_found`. The unlink is the whole
/// file or no file; it does not rewrite bytes in place.
pub(crate) fn remove_document(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
) -> Result<String, Error> {
    let Some(path) = locate_file(vault, decl, id)? else {
        bail!(Error::not_found(format!(
            "record {id} is not in {}",
            decl.name
        )));
    };
    let rel = rel_posix(vault, &path)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(rel),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!(Error::not_found(format!(
                "record {id} is not in {}",
                decl.name
            )))
        }
        Err(err) => Err(err).context("[store::remove_document] unlink record"),
    }
}

pub(crate) fn find_document(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
) -> Result<Record, Error> {
    if id.is_empty() {
        bail!(Error::invalid("id is empty"));
    }
    if decl.kind == Kind::Singleton {
        let Some(fixed) = decl.singleton_id.as_deref() else {
            bail!(Error::invalid(format!(
                "collection {} is missing singleton_id",
                decl.name
            )));
        };
        if id != fixed {
            bail!(Error::not_found(format!(
                "record {id} is not in {}",
                decl.name
            )));
        }
    }
    let Some(path) = locate_file(vault, decl, id)? else {
        bail!(Error::not_found(format!(
            "record {id} is not in {}",
            decl.name
        )));
    };
    read_file(vault, decl, &path)
}

pub(crate) fn check_documents(vault: &Path, schema: &Schema) -> Result<(), Error> {
    for decl in schema.collections() {
        if !matches!(decl.kind, Kind::Document | Kind::Singleton) {
            continue;
        }
        let mut seen = std::collections::BTreeSet::new();
        for path in list_files(vault, decl)? {
            let rel = rel_posix(vault, &path)?;
            let raw = fs::read(&path)
                .context("[store::check_documents] read record")
                .map_err(|err| err.with_path(&rel))?;
            let text = str::from_utf8(&raw)
                .context(Code::NotUtf8, "[store::check_documents] decode UTF-8")
                .map_err(|err| err.with_path(&rel))?;
            if has_conflict_markers(text) {
                bail!(Error::conflict_markers(format!(
                    "{rel} has git conflict markers"
                )));
            }
            let parsed = parse_file(text)?;
            if !seen.insert(parsed.id.clone()) {
                bail!(Error::duplicate_id(format!(
                    "duplicate id {} in {}",
                    parsed.id, decl.name
                )));
            }
            if let Err(err) = validate_fields(&parsed.fields, &decl.fields, true, false) {
                bail!(Error::invalid(format!("{rel}: {err}")));
            }
            let expected = vault_rel(decl, &record_file(decl, &parsed.id)?);
            if expected != rel {
                bail!(Error::invalid(format!(
                    "{rel} path does not match id {}",
                    parsed.id
                )));
            }
        }
    }
    Ok(())
}

struct Prepared {
    id: String,
    path: PathBuf,
    bytes: Vec<u8>,
}

struct ParsedFile {
    id: String,
    fields: Map<String, Value>,
    body: Option<String>,
}

fn prepare_write(vault: &Path, decl: &CollectionDecl, record: Record) -> Result<Prepared, Error> {
    let Some(id) = record.id.clone() else {
        unreachable!("Record id is required");
    };
    crate::id::require_slug("id", &id)?;
    if decl.kind == Kind::Journal {
        bail!(Error::invalid(
            "document write used on a journal collection",
        ));
    }
    if decl.kind == Kind::Singleton {
        let Some(fixed) = decl.singleton_id.as_deref() else {
            bail!(Error::invalid(format!(
                "collection {} is missing singleton_id",
                decl.name
            )));
        };
        if id != fixed {
            bail!(Error::invalid(format!(
                "singleton {} id must be {fixed}",
                decl.name
            )));
        }
        if record.body.as_deref().is_some_and(|body| !body.is_empty()) {
            bail!(Error::invalid(format!(
                "singleton {} does not allow a Markdown body",
                decl.name
            )));
        }
    }
    if record.body.as_deref().is_some_and(|body| !body.is_empty())
        && decl.body.as_ref().is_none_or(|body| !body.allowed)
    {
        bail!(Error::invalid(format!(
            "collection {} does not allow a Markdown body",
            decl.name
        )));
    }
    let mut fields = fields_map(&record)?;
    apply_defaults(&mut fields, &decl.fields);
    match fields.get(crate::id::FIELD) {
        Some(Value::String(existing)) if existing == &id => {}
        Some(Value::Null) | None => {
            fields.insert(String::from(crate::id::FIELD), Value::String(id.clone()));
        }
        Some(_) => {
            bail!(Error::invalid(format!(
                "header {} must equal record id {id}",
                crate::id::FIELD
            )));
        }
    }
    validate_fields(&fields, &decl.fields, true, false)?;
    let rel = record_file(decl, &id)?;
    let dir = crate::isolated::records_dir(vault, decl)?;
    let path = dir.join(&rel);
    let body = record.body.filter(|body| !body.is_empty());
    let bytes = encode_file(decl, &fields, body.as_deref())?;
    Ok(Prepared { id, path, bytes })
}

fn record_file(decl: &CollectionDecl, id: &str) -> Result<String, Error> {
    match decl.kind {
        Kind::Document => {
            let file = format!("{id}.md");
            if file.len() > FS_NAME_MAX {
                bail!(Error::invalid(format!(
                    "record file name exceeds {FS_NAME_MAX} bytes"
                )));
            }
            Ok(file)
        }
        Kind::Singleton => Ok(String::from("record.yaml")),
        Kind::Journal => bail!(Error::invalid("document path used on a journal collection",)),
    }
}

fn vault_rel(decl: &CollectionDecl, file: &str) -> String {
    crate::isolated::vault_rel(decl, file)
}

pub(crate) fn locate_file(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
) -> Result<Option<PathBuf>, Error> {
    if decl.kind == Kind::Singleton
        && decl
            .singleton_id
            .as_deref()
            .is_some_and(|fixed| fixed != id)
    {
        return Ok(None);
    }
    let file = record_file(decl, id)?;
    let dir = crate::isolated::records_dir(vault, decl)?;
    let path = dir.join(&file);
    Ok(path.is_file().then_some(path))
}

pub(crate) fn list_files(vault: &Path, decl: &CollectionDecl) -> Result<Vec<PathBuf>, Error> {
    let dir = crate::isolated::records_dir(vault, decl)?;
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    match decl.kind {
        Kind::Singleton => {
            let path = dir.join("record.yaml");
            Ok(path.is_file().then_some(path).into_iter().collect())
        }
        Kind::Document => {
            let mut out = Vec::new();
            let entries = fs::read_dir(&dir).context("[store::list_files] read collection")?;
            for entry in entries {
                let entry = entry.context("[store::list_files] read entry")?;
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                if name.starts_with('.') || !name.ends_with(".md") || !path.is_file() {
                    continue;
                }
                out.push(path);
            }
            out.sort();
            Ok(out)
        }
        Kind::Journal => Ok(Vec::new()),
    }
}

#[cfg(feature = "find")]
pub(crate) fn scan_documents(vault: &Path, decl: &CollectionDecl) -> Result<Vec<Record>, Error> {
    let mut out = Vec::new();
    for path in list_files(vault, decl)? {
        out.push(read_file(vault, decl, &path)?);
    }
    Ok(out)
}

pub(crate) fn reindex_stats(vault: &Path, decl: &CollectionDecl) -> Result<(u64, u64), Error> {
    let mut records = 0u64;
    let mut invalid = 0u64;
    for path in list_files(vault, decl)? {
        let record = read_file(vault, decl, &path)?;
        records += 1;
        if record.valid != Some(true) {
            invalid += 1;
        }
    }
    Ok((records, invalid))
}

pub(crate) fn read_file(vault: &Path, decl: &CollectionDecl, path: &Path) -> Result<Record, Error> {
    let rel = rel_posix(vault, path)?;
    let raw = fs::read(path)
        .context("[store::read_file] read record")
        .map_err(|err| err.with_path(&rel))?;
    let mut record = Record::default()
        .with_collection(decl.name.as_str())
        .with_path(rel.clone());
    let Ok(text) = str::from_utf8(&raw) else {
        return Ok(record.with_id("").with_valid(false).with_raw(raw));
    };
    if has_conflict_markers(text) {
        record.valid = Some(false);
        match parse_file(text) {
            Ok(parsed) => {
                let _ = fill_parsed(&mut record, parsed);
            }
            Err(_) => {
                if let Some(id) = fallback_id(decl) {
                    record.id = Some(id);
                }
            }
        }
        return Ok(record.with_raw(raw));
    }
    match parse_file(text) {
        Ok(parsed) => {
            let valid = validate_fields(&parsed.fields, &decl.fields, true, false).is_ok()
                && id_matches_header(&parsed)
                && record_file(decl, &parsed.id)
                    .is_ok_and(|expected| vault_rel(decl, &expected) == rel);
            if fill_parsed(&mut record, parsed).is_err() {
                record.valid = Some(false);
            } else {
                record.valid = Some(valid);
            }
            Ok(record.with_raw(raw))
        }
        Err(_) => {
            record.valid = Some(false);
            if let Some(id) = fallback_id(decl) {
                record.id = Some(id);
            }
            Ok(record.with_raw(raw))
        }
    }
}

fn fill_parsed(record: &mut Record, parsed: ParsedFile) -> Result<(), Error> {
    record.id = Some(parsed.id);
    record.body = parsed.body;
    put_fields(record, parsed.fields)
}

fn fallback_id(decl: &CollectionDecl) -> Option<String> {
    decl.singleton_id.clone()
}

fn id_matches_header(parsed: &ParsedFile) -> bool {
    match parsed.fields.get(crate::id::FIELD) {
        Some(Value::String(s)) => s == &parsed.id,
        _ => false,
    }
}

fn parse_file(text: &str) -> Result<ParsedFile, Error> {
    let (yaml, body) = split_frontmatter(text);
    let value: Value =
        yaml_serde::from_str(yaml).context(Code::Invalid, "[store::parse_file] decode YAML")?;
    let Value::Object(fields) = value else {
        bail!(Error::invalid("YAML header must be a mapping"));
    };
    let id = match fields.get(crate::id::FIELD) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            bail!(Error::invalid("header id is missing"));
        }
    };
    Ok(ParsedFile { id, fields, body })
}

fn split_frontmatter(text: &str) -> (&str, Option<String>) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text.strip_prefix("---") else {
        return (text, None);
    };
    let rest = rest.strip_prefix('\r').unwrap_or(rest);
    let Some(rest) = rest.strip_prefix('\n') else {
        return (text, None);
    };
    let closer = if let Some(i) = rest.find("\n---\n") {
        Some((i, 5usize))
    } else if let Some(i) = rest.find("\n---\r\n") {
        Some((i, 6))
    } else if rest.ends_with("\n---") {
        Some((rest.len() - 4, 4))
    } else {
        None
    };
    let Some((i, n)) = closer else {
        return (text, None);
    };
    let yaml = &rest[..i];
    let body = rest[i + n..].to_owned();
    if body.is_empty() {
        (yaml, None)
    } else {
        (yaml, Some(body))
    }
}

fn encode_file(
    decl: &CollectionDecl,
    fields: &Map<String, Value>,
    body: Option<&str>,
) -> Result<Vec<u8>, Error> {
    let yaml =
        yaml_serde::to_string(fields).context(Code::Invalid, "[store::encode_file] encode YAML")?;
    let yaml = yaml.strip_prefix("---\n").unwrap_or(&yaml);
    if decl.kind == Kind::Document
        && let Some(body) = body
    {
        let mut out = String::from("---\n");
        out.push_str(yaml);
        if !yaml.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("---\n");
        out.push_str(body);
        return Ok(out.into_bytes());
    }
    Ok(yaml.as_bytes().to_vec())
}

pub(crate) fn apply_defaults(fields: &mut Map<String, Value>, decls: &[FieldDecl]) {
    for decl in decls {
        if let Some(default) = &decl.default {
            fields
                .entry(decl.name.clone())
                .or_insert_with(|| default.clone());
        }
    }
}

pub(crate) fn validate_fields(
    fields: &Map<String, Value>,
    decls: &[FieldDecl],
    skip_id: bool,
    open: bool,
) -> Result<(), Error> {
    if open {
        for (key, value) in fields {
            if !value.is_string() && !value.is_null() {
                bail!(Error::invalid(format!(
                    "open object key {key} must be a string"
                )));
            }
        }
        return Ok(());
    }
    for key in fields.keys() {
        if skip_id && key == crate::id::FIELD {
            continue;
        }
        if !decls.iter().any(|decl| decl.name == *key) {
            bail!(Error::invalid(format!("unknown field {key}")));
        }
    }
    for decl in decls {
        match fields.get(&decl.name) {
            None => {
                if decl.required {
                    bail!(Error::invalid(format!(
                        "required field {} is missing",
                        decl.name
                    )));
                }
            }
            Some(Value::Null) => {
                if !decl.nullable {
                    bail!(Error::invalid(format!(
                        "field {} is not nullable",
                        decl.name
                    )));
                }
            }
            Some(value) => check_type(value, decl)?,
        }
    }
    Ok(())
}

fn check_type(value: &Value, decl: &FieldDecl) -> Result<(), Error> {
    match decl.type_ {
        FieldType::String => {
            if !value.is_string() {
                bail!(Error::invalid(format!(
                    "field {} must be a string",
                    decl.name
                )));
            }
        }
        FieldType::Boolean => {
            if !value.is_boolean() {
                bail!(Error::invalid(format!(
                    "field {} must be a boolean",
                    decl.name
                )));
            }
        }
        FieldType::Integer => {
            if !is_integer(value) {
                bail!(Error::invalid(format!(
                    "field {} must be an integer",
                    decl.name
                )));
            }
        }
        FieldType::Number => {
            if !is_finite_number(value) {
                bail!(Error::invalid(format!(
                    "field {} must be a finite number",
                    decl.name
                )));
            }
        }
        FieldType::StringList => {
            let Some(items) = value.as_array() else {
                bail!(Error::invalid(format!(
                    "field {} must be a string_list",
                    decl.name
                )));
            };
            if items.iter().any(|item| !item.is_string()) {
                bail!(Error::invalid(format!(
                    "field {} must be a string_list",
                    decl.name
                )));
            }
        }
        FieldType::Object => {
            let Some(map) = value.as_object() else {
                bail!(Error::invalid(format!(
                    "field {} must be an object",
                    decl.name
                )));
            };
            validate_object(map, decl)?;
        }
        FieldType::ObjectList => {
            let Some(items) = value.as_array() else {
                bail!(Error::invalid(format!(
                    "field {} must be an object_list",
                    decl.name
                )));
            };
            for item in items {
                let Some(map) = item.as_object() else {
                    bail!(Error::invalid(format!(
                        "field {} must be an object_list",
                        decl.name
                    )));
                };
                validate_object(map, decl)?;
            }
        }
    }
    Ok(())
}

fn validate_object(map: &Map<String, Value>, decl: &FieldDecl) -> Result<(), Error> {
    if decl.fields.is_empty() {
        for (key, value) in map {
            if !value.is_string() && !value.is_null() {
                bail!(Error::invalid(format!(
                    "open object {} key {key} must be a string",
                    decl.name
                )));
            }
        }
        return Ok(());
    }
    for key in map.keys() {
        if !decl.fields.iter().any(|f| &f.name == key) {
            bail!(Error::invalid(format!(
                "unknown field {key} on {}",
                decl.name
            )));
        }
    }
    validate_fields(map, &decl.fields, false, false)
}

fn is_integer(value: &Value) -> bool {
    let Value::Number(n) = value else {
        return false;
    };
    if let Some(u) = n.as_u64() {
        return u <= BINARY64_EXACT_INT_MAGNITUDE;
    }
    if let Some(i) = n.as_i64() {
        return i.unsigned_abs() <= BINARY64_EXACT_INT_MAGNITUDE;
    }
    n.as_f64().is_some_and(|f| {
        f.is_finite() && f.fract() == 0.0 && f.abs() <= BINARY64_EXACT_INT_MAGNITUDE as f64
    })
}

/// `google.protobuf.Value` numbers are binary64. An exact integer whose
/// magnitude is at most 2^53 is stored as a JSON integer so a journal line
/// prints `1` rather than `1.0`.
fn json_number_from_proto_f64(n: f64) -> Result<Value, Error> {
    if !n.is_finite() {
        bail!(Error::invalid("number must be finite"));
    }
    if n.fract() == 0.0 && n.abs() <= BINARY64_EXACT_INT_MAGNITUDE as f64 {
        if n >= 0.0 {
            Ok(Value::Number((n as u64).into()))
        } else {
            Ok(Value::Number((n as i64).into()))
        }
    } else {
        let Some(number) = serde_json::Number::from_f64(n) else {
            bail!(Error::invalid("number must be finite"));
        };
        Ok(Value::Number(number))
    }
}

fn is_finite_number(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.as_f64().is_some_and(f64::is_finite) || n.is_i64() || n.is_u64(),
        _ => false,
    }
}

pub(crate) fn fields_map(record: &Record) -> Result<Map<String, Value>, Error> {
    let Some(fields) = record.fields.as_option() else {
        return Ok(Map::new());
    };
    proto_struct_to_map(fields)
}

pub(crate) fn put_fields(record: &mut Record, map: Map<String, Value>) -> Result<(), Error> {
    record.fields = MessageField::some(map_to_proto_struct(map)?);
    Ok(())
}

fn proto_struct_to_map(fields: &ProtoStruct) -> Result<Map<String, Value>, Error> {
    let mut map = Map::new();
    for (key, value) in &fields.fields {
        map.insert(key.clone(), proto_value_to_json(value)?);
    }
    Ok(map)
}

fn proto_value_to_json(value: &ProtoValue) -> Result<Value, Error> {
    match value.kind.as_ref() {
        None | Some(ProtoKind::NullValue(_)) => Ok(Value::Null),
        Some(ProtoKind::NumberValue(n)) => json_number_from_proto_f64(*n),
        Some(ProtoKind::StringValue(text)) => Ok(Value::String(text.clone())),
        Some(ProtoKind::BoolValue(flag)) => Ok(Value::Bool(*flag)),
        Some(ProtoKind::StructValue(nested)) => Ok(Value::Object(proto_struct_to_map(nested)?)),
        Some(ProtoKind::ListValue(list)) => {
            let mut items = Vec::with_capacity(list.values.len());
            for item in &list.values {
                items.push(proto_value_to_json(item)?);
            }
            Ok(Value::Array(items))
        }
    }
}

fn map_to_proto_struct(map: Map<String, Value>) -> Result<ProtoStruct, Error> {
    map_ref_to_proto_struct(&map)
}

fn map_ref_to_proto_struct(map: &Map<String, Value>) -> Result<ProtoStruct, Error> {
    let mut fields = ProtoStruct::default();
    for (key, value) in map {
        fields
            .fields
            .insert(key.clone(), json_to_proto_value(value)?);
    }
    Ok(fields)
}

fn json_to_proto_value(value: &Value) -> Result<ProtoValue, Error> {
    let kind = match value {
        Value::Null => ProtoKind::NullValue(buffa::EnumValue::from(NullValue::NULL_VALUE)),
        Value::Bool(flag) => ProtoKind::BoolValue(*flag),
        Value::Number(n) => {
            let Some(number) = n.as_f64() else {
                bail!(Error::invalid("number must be finite"));
            };
            ProtoKind::NumberValue(number)
        }
        Value::String(text) => ProtoKind::StringValue(text.clone()),
        Value::Array(items) => {
            let mut list = ListValue::default();
            for item in items {
                list.values.push(json_to_proto_value(item)?);
            }
            ProtoKind::from(list)
        }
        Value::Object(map) => ProtoKind::from(map_ref_to_proto_struct(map)?),
    };
    Ok(ProtoValue {
        kind: Some(kind),
        ..ProtoValue::default()
    })
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("[store::atomic_write] create dir")?;
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let n = NEXT_TMP.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".scheda.{n}.tmp"));
    let written = write_tmp_then_rename(&tmp, path, bytes);
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

fn write_tmp_then_rename(tmp: &Path, path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .context("[store::atomic_write] create temp file")?;
    file.write_all(bytes)
        .context("[store::atomic_write] write temp file")?;
    file.sync_all()
        .context("[store::atomic_write] fsync temp file")?;
    drop(file);
    rename_replace(tmp, path)
}

/// `fs::rename` cannot replace an existing file on Windows.
fn rename_replace(tmp: &Path, path: &Path) -> Result<(), Error> {
    #[cfg(windows)]
    {
        rename_replace_windows(tmp, path)
    }
    #[cfg(not(windows))]
    {
        fs::rename(tmp, path).context("[store::atomic_write] rename temp file")
    }
}

#[cfg(windows)]
fn rename_replace_windows(tmp: &Path, path: &Path) -> Result<(), Error> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;

    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let from = wide(tmp);
    let to = wide(path);
    // SAFETY: both buffers are NUL-terminated UTF-16 from `encode_wide`.
    let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) };
    if ok == 0 {
        Err(std::io::Error::last_os_error()).context("[store::atomic_write] replace file")
    } else {
        Ok(())
    }
}

pub(crate) fn rel_posix(vault: &Path, path: &Path) -> Result<String, Error> {
    let rel = path
        .strip_prefix(vault)
        .map_err(|_| Error::invalid(format!("path {} is outside the Vault", path.display())))?;
    let mut out = String::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                if !out.is_empty() {
                    out.push('/');
                }
                out.push_str(&part.to_string_lossy());
            }
            Component::CurDir => {}
            _ => {
                bail!(Error::invalid(format!(
                    "path {} is outside the Vault",
                    path.display()
                )));
            }
        }
    }
    Ok(out)
}

pub(crate) fn has_conflict_markers(text: &str) -> bool {
    text.contains("<<<<<<<") || text.contains(">>>>>>>")
}

#[cfg(test)]
mod tests {
    use super::record_file;
    use crate::schema::{CollectionDecl, Kind};

    fn decl(kind: Kind) -> CollectionDecl {
        CollectionDecl {
            name: String::from("c"),
            kind,
            version: 1,
            filter_keys: Default::default(),
            segment_max_bytes: None,
            segment_max_lines: None,
            segment_rotate: None,
            singleton_id: Some(String::from("inbox")),
            body: None,
            fields: Vec::new(),
            isolated: false,
        }
    }

    #[test]
    fn document_file_is_id_md() {
        assert_eq!(
            record_file(&decl(Kind::Document), "alpha").unwrap(),
            "alpha.md"
        );
        assert_eq!(
            record_file(&decl(Kind::Singleton), "inbox").unwrap(),
            "record.yaml"
        );
    }
}
