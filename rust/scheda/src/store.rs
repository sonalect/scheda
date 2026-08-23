//! On-disk `document` / `singleton` files (C3). Journal segments are `journal`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

use crate::schema::{self, CollectionDecl, FieldDecl, FieldType, Kind, Schema};
use crate::{Error, Record};

static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

pub(crate) fn insert_document(
    vault: &Path,
    decl: &CollectionDecl,
    record: Record,
) -> Result<String, Error> {
    let prepared = prepare_write(vault, decl, record)?;
    if prepared.path.exists() {
        return Err(Error::conflict(format!(
            "record {} already exists in {}",
            prepared.id, decl.name
        )));
    }
    atomic_write(&prepared.path, &prepared.bytes)?;
    Ok(prepared.id)
}

pub(crate) fn replace_document(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
    record: Record,
) -> Result<String, Error> {
    let Some(existing) = locate_file(vault, decl, id)? else {
        return Err(Error::not_found(format!(
            "record {id} is not in {}",
            decl.name
        )));
    };
    let prepared = prepare_write(vault, decl, record)?;
    if prepared.id != id {
        return Err(Error::invalid(format!(
            "replace_one id {id} does not match record id {}",
            prepared.id
        )));
    }
    if prepared.path != existing {
        return Err(Error::invalid(format!(
            "replace_one would move {id} off its path in {}",
            decl.name
        )));
    }
    atomic_write(&prepared.path, &prepared.bytes)?;
    Ok(prepared.id)
}

pub(crate) fn find_document(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
) -> Result<Record, Error> {
    if id.is_empty() {
        return Err(Error::invalid("id is empty"));
    }
    if decl.kind == Kind::Singleton {
        let Some(fixed) = decl.singleton_id.as_deref() else {
            return Err(Error::invalid(format!(
                "collection {} is missing singleton_id",
                decl.name
            )));
        };
        if id != fixed {
            return Err(Error::not_found(format!(
                "record {id} is not in {}",
                decl.name
            )));
        }
    }
    let Some(path) = locate_file(vault, decl, id)? else {
        return Err(Error::not_found(format!(
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
                .map_err(|err| Error::io(format!("read {}: {err}", path.display())))?;
            let text =
                str::from_utf8(&raw).map_err(|_| Error::invalid(format!("{rel} is not UTF-8")))?;
            if has_conflict_markers(text) {
                return Err(Error::conflict(format!("{rel} has git conflict markers")));
            }
            let parsed = parse_file(decl, text)?;
            if !seen.insert(parsed.id.clone()) {
                return Err(Error::conflict(format!(
                    "duplicate id {} in {}",
                    parsed.id, decl.name
                )));
            }
            if let Err(err) = validate_fields(
                &parsed.fields,
                &decl.fields,
                Some(decl.id_field.as_str()),
                false,
            ) {
                return Err(Error::invalid(format!("{rel}: {err}")));
            }
            let expected = vault_rel(decl, &instantiate_path(decl, &parsed.id, &parsed.fields)?);
            if expected != rel {
                return Err(Error::invalid(format!(
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
    let id = record
        .id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::invalid("id is empty"))?
        .to_owned();
    let collection = record
        .collection
        .as_deref()
        .ok_or_else(|| Error::invalid("collection is empty"))?;
    if collection != decl.name {
        return Err(Error::invalid(format!(
            "record collection {collection} does not match {}",
            decl.name
        )));
    }
    if decl.kind == Kind::Journal {
        return Err(Error::invalid(
            "document write used on a journal collection",
        ));
    }
    if let Some(pattern) = &decl.id_pattern
        && !pattern.is_match(&id)
    {
        return Err(Error::invalid(format!(
            "id {id} does not match collection {} pattern",
            decl.name
        )));
    }
    if decl.kind == Kind::Singleton {
        let Some(fixed) = decl.singleton_id.as_deref() else {
            return Err(Error::invalid(format!(
                "collection {} is missing singleton_id",
                decl.name
            )));
        };
        if id != fixed {
            return Err(Error::invalid(format!(
                "singleton {} id must be {fixed}",
                decl.name
            )));
        }
        if record.body.as_deref().is_some_and(|body| !body.is_empty()) {
            return Err(Error::invalid(format!(
                "singleton {} does not allow a Markdown body",
                decl.name
            )));
        }
    }
    if id.contains('/') || id.contains('\\') {
        return Err(Error::invalid("id must not contain a path separator"));
    }
    let mut fields = fields_map(&record)?;
    apply_defaults(&mut fields, &decl.fields);
    match fields.get(&decl.id_field) {
        Some(Value::String(existing)) if existing == &id => {}
        Some(Value::Null) | None => {
            fields.insert(decl.id_field.clone(), Value::String(id.clone()));
        }
        Some(_) => {
            return Err(Error::invalid(format!(
                "header {} must equal record id {id}",
                decl.id_field
            )));
        }
    }
    validate_fields(&fields, &decl.fields, Some(decl.id_field.as_str()), false)?;
    let rel = instantiate_path(decl, &id, &fields)?;
    let path = schema::join_in_collection(vault, &decl.name, &rel)?;
    let body = record.body.filter(|body| !body.is_empty());
    let bytes = encode_file(decl, &fields, body.as_deref())?;
    Ok(Prepared { id, path, bytes })
}

fn instantiate_path(
    decl: &CollectionDecl,
    id: &str,
    fields: &Map<String, Value>,
) -> Result<String, Error> {
    let mut found: Option<(usize, String)> = None;
    for template in &decl.layout {
        let Some(rel) = fill_template(template, id, fields)? else {
            continue;
        };
        let n = placeholder_count(template);
        match &found {
            None => found = Some((n, rel)),
            Some((m, _)) if n > *m => found = Some((n, rel)),
            Some((m, existing)) if n == *m && existing != &rel => {
                return Err(Error::invalid(format!(
                    "id {id} matches more than one layout in {}",
                    decl.name
                )));
            }
            _ => {}
        }
    }
    found.map(|(_, rel)| rel).ok_or_else(|| {
        Error::invalid(format!(
            "id {id} does not fill a layout template in {}",
            decl.name
        ))
    })
}

fn vault_rel(decl: &CollectionDecl, layout_rel: &str) -> String {
    format!("{}/{layout_rel}", schema::collection_dir(&decl.name))
}

fn placeholder_count(template: &str) -> usize {
    let mut n = 0;
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            break;
        };
        n += 1;
        rest = &rest[start + end + 1..];
    }
    n
}

fn fill_template(
    template: &str,
    id: &str,
    fields: &Map<String, Value>,
) -> Result<Option<String>, Error> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            return Err(Error::invalid(format!(
                "layout template {template} has an unclosed '{{'"
            )));
        };
        let end = start + end;
        out.push_str(&rest[..start]);
        let name = &rest[start + 1..end];
        if name.is_empty() {
            return Err(Error::invalid(format!(
                "layout template {template} has an empty placeholder"
            )));
        }
        let value = if name == "id" {
            id
        } else {
            match fields.get(name) {
                Some(Value::String(s)) if !s.is_empty() => s.as_str(),
                Some(Value::String(_)) | Some(Value::Null) | None => return Ok(None),
                Some(_) => {
                    return Err(Error::invalid(format!("path slot {name} must be a string")));
                }
            }
        };
        if value.contains('/') || value.contains('\\') {
            return Err(Error::invalid(format!(
                "path slot {name} must not contain a path separator"
            )));
        }
        out.push_str(value);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    if out.is_empty() {
        return Err(Error::invalid("layout template produced an empty path"));
    }
    Ok(Some(out))
}

fn locate_file(vault: &Path, decl: &CollectionDecl, id: &str) -> Result<Option<PathBuf>, Error> {
    let mut found = None;
    for template in &decl.layout {
        for path in match_template(vault, decl, template, Some(id))? {
            if found.is_some() {
                return Err(Error::invalid(format!(
                    "id {id} matches more than one file in {}",
                    decl.name
                )));
            }
            found = Some(path);
        }
    }
    Ok(found)
}

pub(crate) fn list_files(vault: &Path, decl: &CollectionDecl) -> Result<Vec<PathBuf>, Error> {
    let mut out = Vec::new();
    for template in &decl.layout {
        out.extend(match_template(vault, decl, template, None)?);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

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

fn match_template(
    vault: &Path,
    decl: &CollectionDecl,
    template: &str,
    id: Option<&str>,
) -> Result<Vec<PathBuf>, Error> {
    let root = schema::join_in_collection(vault, &decl.name, "")?;
    let segs: Vec<&str> = template.split('/').collect();
    let mut out = Vec::new();
    walk_match(&root, &segs, id, &mut out)?;
    Ok(out)
}

fn walk_match(
    dir: &Path,
    segs: &[&str],
    id: Option<&str>,
    out: &mut Vec<PathBuf>,
) -> Result<(), Error> {
    let Some((head, tail)) = segs.split_first() else {
        return Ok(());
    };
    if !dir.is_dir() {
        return Ok(());
    }
    let last = tail.is_empty();
    if let Some(exact) = exact_component(head, id) {
        let next = dir.join(&exact);
        if last {
            if next.is_file() {
                out.push(next);
            }
        } else {
            walk_match(&next, tail, id, out)?;
        }
        return Ok(());
    }
    let entries =
        fs::read_dir(dir).map_err(|err| Error::io(format!("read {}: {err}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|err| Error::io(format!("read {}: {err}", dir.display())))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if !component_matches(head, name, id) {
            continue;
        }
        let next = entry.path();
        if last {
            if next.is_file() {
                out.push(next);
            }
        } else if next.is_dir() {
            walk_match(&next, tail, id, out)?;
        }
    }
    Ok(())
}

fn exact_component(pattern: &str, id: Option<&str>) -> Option<String> {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        let end = rest[start..].find('}')?;
        let end = start + end;
        out.push_str(&rest[..start]);
        let name = &rest[start + 1..end];
        if name == "id" {
            out.push_str(id?);
        } else {
            return None;
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(out)
}

fn component_matches(pattern: &str, name: &str, id: Option<&str>) -> bool {
    match_component(pattern, name, id)
}

fn match_component(pat: &str, name: &str, id: Option<&str>) -> bool {
    match pat.find('{') {
        None => pat == name,
        Some(start) => {
            if !name.starts_with(&pat[..start]) {
                return false;
            }
            let name = &name[start..];
            let Some(end_rel) = pat[start..].find('}') else {
                return false;
            };
            let end = start + end_rel;
            let ph = &pat[start + 1..end];
            let after = &pat[end + 1..];
            if ph == "id" {
                if let Some(want) = id {
                    name.starts_with(want)
                        && match_component(after, &name[want.len()..], Some(want))
                } else {
                    consume_slot(after, name, id)
                }
            } else {
                consume_slot(after, name, id)
            }
        }
    }
}

fn consume_slot(after: &str, name: &str, id: Option<&str>) -> bool {
    if after.is_empty() {
        return !name.is_empty();
    }
    (1..=name.len()).any(|i| match_component(after, &name[i..], id))
}

pub(crate) fn read_file(vault: &Path, decl: &CollectionDecl, path: &Path) -> Result<Record, Error> {
    let rel = rel_posix(vault, path)?;
    let raw = fs::read(path).map_err(|err| Error::io(format!("read {rel}: {err}")))?;
    let mut record = Record::default()
        .with_collection(decl.name.clone())
        .with_path(rel.clone())
        .with_raw(raw.clone());
    let Ok(text) = str::from_utf8(&raw) else {
        return Ok(record.with_id("").with_valid(false));
    };
    if has_conflict_markers(text) {
        record.valid = Some(false);
        match parse_file(decl, text) {
            Ok(parsed) => {
                let _ = fill_parsed(&mut record, parsed);
            }
            Err(_) => {
                if let Some(id) = fallback_id(decl) {
                    record.id = Some(id);
                }
            }
        }
        return Ok(record);
    }
    match parse_file(decl, text) {
        Ok(parsed) => {
            let valid = validate_fields(
                &parsed.fields,
                &decl.fields,
                Some(decl.id_field.as_str()),
                false,
            )
            .is_ok()
                && id_matches_header(decl, &parsed)
                && instantiate_path(decl, &parsed.id, &parsed.fields)
                    .is_ok_and(|expected| vault_rel(decl, &expected) == rel)
                && decl
                    .id_pattern
                    .as_ref()
                    .is_none_or(|pattern| pattern.is_match(&parsed.id));
            if fill_parsed(&mut record, parsed).is_err() {
                record.valid = Some(false);
            } else {
                record.valid = Some(valid);
            }
            Ok(record)
        }
        Err(_) => {
            record.valid = Some(false);
            if let Some(id) = fallback_id(decl) {
                record.id = Some(id);
            }
            Ok(record)
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

fn id_matches_header(decl: &CollectionDecl, parsed: &ParsedFile) -> bool {
    match parsed.fields.get(&decl.id_field) {
        Some(Value::String(s)) => s == &parsed.id,
        _ => false,
    }
}

fn parse_file(decl: &CollectionDecl, text: &str) -> Result<ParsedFile, Error> {
    let (yaml, body) = split_frontmatter(text);
    let value: Value =
        yaml_serde::from_str(yaml).map_err(|err| Error::invalid(format!("YAML header: {err}")))?;
    let Value::Object(fields) = value else {
        return Err(Error::invalid("YAML header must be a mapping"));
    };
    let id = match fields.get(&decl.id_field) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => {
            return Err(Error::invalid(format!(
                "header {} is missing",
                decl.id_field
            )));
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
    let json = Value::Object(fields.clone());
    if decl.kind == Kind::Singleton && decl.layout.iter().any(|t| t.ends_with(".json")) {
        let bytes = serde_json::to_vec_pretty(&json)
            .map_err(|err| Error::invalid(format!("encode JSON: {err}")))?;
        return Ok(bytes);
    }
    let yaml = yaml_serde::to_string(&json)
        .map_err(|err| Error::invalid(format!("encode YAML: {err}")))?;
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
    id_field: Option<&str>,
    open: bool,
) -> Result<(), Error> {
    if open {
        for (key, value) in fields {
            if !value.is_string() && !value.is_null() {
                return Err(Error::invalid(format!(
                    "open object key {key} must be a string"
                )));
            }
        }
        return Ok(());
    }
    for key in fields.keys() {
        if id_field == Some(key.as_str()) {
            continue;
        }
        if !decls.iter().any(|decl| decl.name == *key) {
            return Err(Error::invalid(format!("unknown field {key}")));
        }
    }
    for decl in decls {
        match fields.get(&decl.name) {
            None => {
                if decl.required {
                    return Err(Error::invalid(format!(
                        "required field {} is missing",
                        decl.name
                    )));
                }
            }
            Some(Value::Null) => {
                if !decl.nullable {
                    return Err(Error::invalid(format!(
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
                return Err(Error::invalid(format!(
                    "field {} must be a string",
                    decl.name
                )));
            }
        }
        FieldType::Boolean => {
            if !value.is_boolean() {
                return Err(Error::invalid(format!(
                    "field {} must be a boolean",
                    decl.name
                )));
            }
        }
        FieldType::Integer => {
            if !is_integer(value) {
                return Err(Error::invalid(format!(
                    "field {} must be an integer",
                    decl.name
                )));
            }
        }
        FieldType::Number => {
            if !is_finite_number(value) {
                return Err(Error::invalid(format!(
                    "field {} must be a finite number",
                    decl.name
                )));
            }
        }
        FieldType::StringList => {
            let Some(items) = value.as_array() else {
                return Err(Error::invalid(format!(
                    "field {} must be a string_list",
                    decl.name
                )));
            };
            if items.iter().any(|item| !item.is_string()) {
                return Err(Error::invalid(format!(
                    "field {} must be a string_list",
                    decl.name
                )));
            }
        }
        FieldType::Object => {
            let Some(map) = value.as_object() else {
                return Err(Error::invalid(format!(
                    "field {} must be an object",
                    decl.name
                )));
            };
            validate_object(map, decl)?;
        }
        FieldType::ObjectList => {
            let Some(items) = value.as_array() else {
                return Err(Error::invalid(format!(
                    "field {} must be an object_list",
                    decl.name
                )));
            };
            for item in items {
                let Some(map) = item.as_object() else {
                    return Err(Error::invalid(format!(
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
                return Err(Error::invalid(format!(
                    "open object {} key {key} must be a string",
                    decl.name
                )));
            }
        }
        return Ok(());
    }
    for key in map.keys() {
        if !decl.fields.iter().any(|f| &f.name == key) {
            return Err(Error::invalid(format!(
                "unknown field {key} on {}",
                decl.name
            )));
        }
    }
    validate_fields(map, &decl.fields, None, false)
}

fn is_integer(value: &Value) -> bool {
    match value {
        Value::Number(n) => {
            n.is_i64()
                || n.is_u64()
                || n.as_f64()
                    .is_some_and(|f| f.is_finite() && f.fract() == 0.0)
        }
        _ => false,
    }
}

fn is_finite_number(value: &Value) -> bool {
    match value {
        Value::Number(n) => n.as_f64().is_some_and(f64::is_finite) || n.is_i64() || n.is_u64(),
        _ => false,
    }
}

pub(crate) fn fields_map(record: &Record) -> Result<Map<String, Value>, Error> {
    let value = serde_json::to_value(record)
        .map_err(|err| Error::invalid(format!("encode fields: {err}")))?;
    match value.get("fields") {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map.clone()),
        Some(_) => Err(Error::invalid("fields must be a JSON object")),
    }
}

pub(crate) fn put_fields(record: &mut Record, map: Map<String, Value>) -> Result<(), Error> {
    let mut envelope = serde_json::Map::new();
    if let Some(id) = &record.id {
        envelope.insert(String::from("id"), Value::String(id.clone()));
    }
    if let Some(collection) = &record.collection {
        envelope.insert(
            String::from("collection"),
            Value::String(collection.clone()),
        );
    }
    envelope.insert(String::from("fields"), Value::Object(map));
    let mut next: Record = serde_json::from_value(Value::Object(envelope))
        .map_err(|err| Error::invalid(format!("decode fields: {err}")))?;
    next.path = record.path.clone();
    next.valid = record.valid;
    next.body = record.body.clone();
    next.raw = record.raw.clone();
    *record = next;
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|err| Error::io(format!("create {}: {err}", dir.display())))?;
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let n = NEXT_TMP.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(
        ".{}.{n}.tmp",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("record")
    ));
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
        .map_err(|err| Error::io(format!("create {}: {err}", tmp.display())))?;
    file.write_all(bytes)
        .map_err(|err| Error::io(format!("write {}: {err}", tmp.display())))?;
    file.sync_all()
        .map_err(|err| Error::io(format!("fsync {}: {err}", tmp.display())))?;
    drop(file);
    fs::rename(tmp, path).map_err(|err| {
        Error::io(format!(
            "rename {} -> {}: {err}",
            tmp.display(),
            path.display()
        ))
    })
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
                return Err(Error::invalid(format!(
                    "path {} is outside the Vault",
                    path.display()
                )));
            }
        }
    }
    Ok(out)
}

pub(crate) fn has_conflict_markers(text: &str) -> bool {
    text.contains("\n<<<<<<< ") || text.starts_with("<<<<<<< ")
}

#[cfg(test)]
mod tests {
    use super::{component_matches, fill_template, placeholder_count};
    use serde_json::{Map, Value};

    #[test]
    fn extra_slot_component_matches() {
        assert!(component_matches(
            "{project}-{id}.md",
            "foo-alpha.md",
            Some("alpha")
        ));
        assert!(!component_matches(
            "{project}-{id}.md",
            "foo-beta.md",
            Some("alpha")
        ));
        assert!(component_matches("{id}.md", "alpha.md", Some("alpha")));
        assert!(!component_matches("{id}.md", "alphabet.md", Some("alpha")));
    }

    #[test]
    fn fill_skips_unfilled_extra_slot() {
        let mut fields = Map::new();
        fields.insert(String::from("project"), Value::String(String::from("p1")));
        let id = "alpha";
        assert_eq!(
            fill_template("personal/{id}.md", id, &fields)
                .unwrap()
                .as_deref(),
            Some("personal/alpha.md")
        );
        assert_eq!(
            fill_template("projects/{project}/{id}.md", id, &fields)
                .unwrap()
                .as_deref(),
            Some("projects/p1/alpha.md")
        );
        let empty = Map::new();
        assert_eq!(
            fill_template("projects/{project}/{id}.md", id, &empty).unwrap(),
            None
        );
        assert_eq!(placeholder_count("projects/{project}/{id}.md"), 2);
    }
}
