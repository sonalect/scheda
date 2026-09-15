//! On-disk `document` / `singleton` files. Journal segments are `journal`.

use std::borrow::Cow;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::iter::FusedIterator;
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
use crate::{Encryption, Error, Record};

static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

const FS_NAME_MAX: usize = 255;

/// Consecutive integers in IEEE 754 binary64. `google.protobuf.Value`
/// `number_value` is that type; an `integer` field beyond this is not exact.
const BINARY64_EXACT_INT_MAGNITUDE: u64 = 1 << 53;

pub(crate) fn insert_document(
    vault: &Path,
    decl: &CollectionDecl,
    record: &Record,
    encryption: Option<&Encryption>,
) -> Result<(String, String), Error> {
    let prepared = prepare_write(vault, decl, record)?;
    if prepared.path.exists() {
        bail!(Error::duplicate_id(format!(
            "record {} already exists in {}",
            prepared.id, decl.name
        )));
    }
    let on_disk = encode_record_file(&prepared.bytes, decl, encryption)?;
    atomic_write(&prepared.path, &on_disk)?;
    crate::isolated::restrict_record(vault, &prepared.path)?;
    let rel = rel_posix(vault, &prepared.path)?;
    Ok((prepared.id, rel))
}

pub(crate) fn replace_document(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
    record: &Record,
    encryption: Option<&Encryption>,
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
    let on_disk = encode_record_file(&prepared.bytes, decl, encryption)?;
    atomic_write(&prepared.path, &on_disk)?;
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
    let dir = crate::isolated::records_dir(vault, decl)?;
    match fs::remove_file(&path) {
        Ok(()) => {
            prune_empty_parents(&dir, &path)?;
            Ok(rel)
        }
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
    encryption: Option<&Encryption>,
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
    read_file(vault, decl, &path, encryption)
}

pub(crate) fn check_documents(
    vault: &Path,
    schema: &Schema,
    encryption: Option<&Encryption>,
) -> Result<(), Error> {
    for decl in schema.collections() {
        if !matches!(decl.kind, Kind::Document | Kind::Singleton) {
            continue;
        }
        if decl.encrypted {
            #[cfg(not(feature = "encrypt"))]
            {
                let _ = encryption;
                bail!(Error::unsupported_encryption(format!(
                    "encrypted collection {} was not checked: this build cannot decrypt it",
                    decl.name
                )));
            }
            #[cfg(feature = "encrypt")]
            if encryption.is_none() {
                bail!(Error::crypto(format!(
                    "encrypted collection {} was not checked: secret is missing",
                    decl.name
                )));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for path in list_files(vault, decl)? {
            let rel = rel_posix(vault, &path)?;
            let raw = fs::read(&path)
                .context("[store::check_documents] read record")
                .map_err(|err| err.with_path(&rel))?;
            let raw = decode_record_file(raw, decl, encryption, &rel)?;
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

fn prepare_write(vault: &Path, decl: &CollectionDecl, record: &Record) -> Result<Prepared, Error> {
    let Some(id) = record.id.as_deref() else {
        bail!(Error::invalid("record id is required"));
    };
    match decl.kind {
        Kind::Document => crate::id::require_document_id(id)?,
        Kind::Journal | Kind::Singleton => crate::id::require_slug("id", id)?,
    }
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
    let mut fields = fields_map(record)?;
    apply_defaults(&mut fields, &decl.fields);
    match fields.get(crate::id::FIELD) {
        Some(Value::String(existing)) if existing == id => {}
        Some(Value::Null) | None => {
            fields.insert(String::from(crate::id::FIELD), Value::String(id.to_owned()));
        }
        Some(_) => {
            bail!(Error::invalid(format!(
                "header {} must equal record id {id}",
                crate::id::FIELD
            )));
        }
    }
    validate_fields(&fields, &decl.fields, true, false)?;
    let rel = record_file(decl, id)?;
    let dir = crate::isolated::records_dir(vault, decl)?;
    let path = crate::schema::join_inside(&dir, &rel)?;
    let body = record.body.as_deref().filter(|body| !body.is_empty());
    let bytes = encode_file(decl, &fields, body)?;
    Ok(Prepared {
        id: id.to_owned(),
        path,
        bytes,
    })
}

fn record_file(decl: &CollectionDecl, id: &str) -> Result<String, Error> {
    match decl.kind {
        Kind::Document => {
            crate::id::require_document_id(id)?;
            let leaf = match id.rsplit('/').next() {
                Some(leaf) => leaf,
                None => unreachable!("document id is non-empty"),
            };
            let file = format!("{leaf}.md");
            if file.len() > FS_NAME_MAX {
                bail!(Error::invalid(format!(
                    "record file name exceeds {FS_NAME_MAX} bytes"
                )));
            }
            Ok(format!("{id}.md"))
        }
        Kind::Singleton => Ok(String::from("record.yaml")),
        Kind::Journal => bail!(Error::invalid("document path used on a journal collection",)),
    }
}

fn vault_rel(decl: &CollectionDecl, file: &str) -> String {
    crate::isolated::vault_rel(decl, file)
}

fn list_document_files(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut found = Vec::new();
    collect_document_files(dir, &mut found)?;
    let mut keyed = Vec::new();
    for path in found {
        let id = document_id_from_path(dir, &path)?;
        keyed.push((id, path));
    }
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(keyed.into_iter().map(|(_, path)| path).collect())
}

fn collect_document_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Error> {
    let entries = fs::read_dir(dir).context("[store::list_files] read collection")?;
    for entry in entries {
        let entry = entry.context("[store::list_files] read entry")?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let meta = fs::symlink_metadata(&path).context("[store::list_files] stat entry")?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if crate::id::require_slug("id", name).is_err() {
                continue;
            }
            collect_document_files(&path, out)?;
            continue;
        }
        if meta.is_file() && name.ends_with(".md") {
            out.push(path);
        }
    }
    Ok(())
}

fn document_id_from_path(records_dir: &Path, path: &Path) -> Result<String, Error> {
    let rel = path.strip_prefix(records_dir).map_err(|_| {
        Error::invalid(format!("path {} is outside the collection", path.display()))
    })?;
    let mut out = String::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                let Some(name) = part.to_str() else {
                    bail!(Error::invalid("record file name is not UTF-8"));
                };
                if !out.is_empty() {
                    out.push('/');
                }
                out.push_str(name);
            }
            Component::CurDir => {}
            _ => {
                bail!(Error::invalid(format!(
                    "path {} is outside the collection",
                    path.display()
                )));
            }
        }
    }
    let Some(id) = out.strip_suffix(".md") else {
        unreachable!("list_files only yields .md names for document");
    };
    Ok(id.to_owned())
}

fn prune_empty_parents(stop: &Path, file: &Path) -> Result<(), Error> {
    let Some(mut dir) = file.parent().map(Path::to_path_buf) else {
        return Ok(());
    };
    while dir.starts_with(stop) && dir != stop {
        match fs::remove_dir(&dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => {
                return Err(err).context("[store::remove_document] prune empty dir");
            }
        }
        let Some(parent) = dir.parent() else {
            break;
        };
        dir = parent.to_path_buf();
    }
    Ok(())
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
    let path = crate::schema::join_inside(&dir, &file)?;
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
        Kind::Document => list_document_files(&dir),
        Kind::Journal => Ok(Vec::new()),
    }
}

/// Id-order walk of one `document` or `singleton` collection.
///
/// Yields one Record at a time from the files the library names. Does not
/// open the Index. Order is the byte order of the full `id` (`{id}.md`).
/// A `document` `id` may contain `/`, each component a directory.
/// A `singleton` yields its one Record, or nothing before the first write.
/// `journal` is refused when the walk is created.
pub struct CollectionDocuments {
    vault: PathBuf,
    decl: CollectionDecl,
    encryption: Option<Encryption>,
    files: Vec<PathBuf>,
    next: usize,
    done: bool,
}

pub(crate) fn collection_documents(
    vault: &Path,
    decl: &CollectionDecl,
    encryption: Option<&Encryption>,
) -> Result<CollectionDocuments, Error> {
    if decl.kind == Kind::Journal {
        bail!(Error::unsupported(
            "documents is offered for document and singleton only"
        ));
    }
    Ok(CollectionDocuments {
        vault: vault.to_path_buf(),
        decl: decl.clone(),
        encryption: encryption.cloned(),
        files: list_files(vault, decl)?,
        next: 0,
        done: false,
    })
}

impl CollectionDocuments {
    fn next_record(&mut self) -> Result<Option<Record>, Error> {
        let Some(path) = self.files.get(self.next).cloned() else {
            return Ok(None);
        };
        self.next += 1;
        Ok(Some(read_file(
            &self.vault,
            &self.decl,
            &path,
            self.encryption.as_ref(),
        )?))
    }
}

impl Iterator for CollectionDocuments {
    type Item = Result<Record, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_record() {
            Ok(Some(record)) => Some(Ok(record)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(err) => {
                self.done = true;
                Some(Err(err))
            }
        }
    }
}

impl FusedIterator for CollectionDocuments {}

/// Ids of the Record files `list_files` named, in the same order. Does not
/// open a Record file. `journal` → `unsupported`.
pub(crate) fn collection_ids(vault: &Path, decl: &CollectionDecl) -> Result<Vec<String>, Error> {
    if decl.kind == Kind::Journal {
        bail!(Error::unsupported(
            "ids is offered for document and singleton only"
        ));
    }
    list_files(vault, decl)?
        .iter()
        .map(|path| id_from_listed_file(vault, decl, path))
        .collect()
}

fn id_from_listed_file(vault: &Path, decl: &CollectionDecl, path: &Path) -> Result<String, Error> {
    match decl.kind {
        Kind::Singleton => match decl.singleton_id.clone() {
            Some(id) => Ok(id),
            None => {
                bail!(Error::invalid(format!(
                    "collection {} is missing singleton_id",
                    decl.name
                )));
            }
        },
        Kind::Document => {
            let dir = crate::isolated::records_dir(vault, decl)?;
            document_id_from_path(&dir, path)
        }
        Kind::Journal => unreachable!("journal is refused before listing"),
    }
}

#[cfg(not(feature = "find"))]
pub(crate) fn reindex_stats(vault: &Path, decl: &CollectionDecl) -> Result<(u64, u64), Error> {
    if !decl.any_indexed() {
        return Ok((0, 0));
    }
    let mut records = 0u64;
    let mut invalid = 0u64;
    for path in list_files(vault, decl)? {
        let record = match read_file(vault, decl, &path, None) {
            Ok(record) => record,
            Err(err) if crate::reindex::is_absent(&err) => continue,
            Err(err) => return Err(err),
        };
        records += 1;
        if record.valid != Some(true) {
            invalid += 1;
        }
    }
    Ok((records, invalid))
}

pub(crate) fn read_file(
    vault: &Path,
    decl: &CollectionDecl,
    path: &Path,
    encryption: Option<&Encryption>,
) -> Result<Record, Error> {
    let rel = rel_posix(vault, path)?;
    let raw = fs::read(path)
        .context("[store::read_file] read record")
        .map_err(|err| err.with_path(&rel))?;
    let raw = decode_record_file(raw, decl, encryption, &rel)?;
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
            Some(value) => check_type_at(value, decl, &format!("field {}", decl.name))?,
        }
    }
    Ok(())
}

fn check_type_at(value: &Value, decl: &FieldDecl, loc: &str) -> Result<(), Error> {
    match decl.type_ {
        FieldType::String => {
            if !value.is_string() {
                bail!(Error::invalid(format!("{loc} must be a string")));
            }
        }
        FieldType::Boolean => {
            if !value.is_boolean() {
                bail!(Error::invalid(format!("{loc} must be a boolean")));
            }
        }
        FieldType::Integer => {
            if !is_integer(value) {
                bail!(Error::invalid(format!("{loc} must be an integer")));
            }
        }
        FieldType::Number => {
            if !is_finite_number(value) {
                bail!(Error::invalid(format!("{loc} must be a finite number")));
            }
        }
        FieldType::StringList => {
            let Some(items) = value.as_array() else {
                bail!(Error::invalid(format!("{loc} must be a string_list")));
            };
            if items.iter().any(|item| !item.is_string()) {
                bail!(Error::invalid(format!("{loc} must be a string_list")));
            }
        }
        FieldType::Object => {
            let Some(map) = value.as_object() else {
                bail!(Error::invalid(format!("{loc} must be an object")));
            };
            validate_object(map, decl, loc)?;
        }
        FieldType::ObjectList => {
            let Some(items) = value.as_array() else {
                bail!(Error::invalid(format!("{loc} must be an object_list")));
            };
            for item in items {
                let Some(map) = item.as_object() else {
                    bail!(Error::invalid(format!("{loc} must be an object_list")));
                };
                validate_object(map, decl, loc)?;
            }
        }
    }
    Ok(())
}

fn validate_object(map: &Map<String, Value>, decl: &FieldDecl, loc: &str) -> Result<(), Error> {
    if let Some(values) = &decl.values {
        for (key, value) in map {
            let entry = format!("{loc} values {} key {key}", values.name);
            match value {
                Value::Null if values.nullable => {}
                Value::Null => {
                    bail!(Error::invalid(format!("{entry} is not nullable")));
                }
                value => check_type_at(value, values, &entry)?,
            }
        }
        return Ok(());
    }
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

fn encode_record_file<'a>(
    plaintext: &'a [u8],
    decl: &CollectionDecl,
    encryption: Option<&Encryption>,
) -> Result<Cow<'a, [u8]>, Error> {
    if !decl.encrypted {
        return Ok(Cow::Borrowed(plaintext));
    }
    #[cfg(feature = "encrypt")]
    {
        Ok(Cow::Owned(crate::crypto::encrypt(plaintext, encryption)?))
    }
    #[cfg(not(feature = "encrypt"))]
    {
        let _ = (plaintext, encryption);
        bail!(Error::unsupported_encryption(
            "this build cannot encrypt Record files"
        ));
    }
}

pub(crate) fn decode_record_file(
    raw: Vec<u8>,
    decl: &CollectionDecl,
    encryption: Option<&Encryption>,
    rel: &str,
) -> Result<Vec<u8>, Error> {
    if !decl.encrypted {
        return Ok(raw);
    }
    #[cfg(not(feature = "encrypt"))]
    {
        let _ = encryption;
        bail!(
            Error::unsupported_encryption(format!(
                "collection {} is encrypted; this build cannot read it",
                decl.name
            ))
            .with_path(rel)
        );
    }
    #[cfg(feature = "encrypt")]
    crate::crypto::decrypt(&raw, encryption).map_err(|err| err.with_path(rel))
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

/// Owner-only file mode (`0600` on Unix). Other systems leave ACLs unchanged.
pub(crate) fn restrict_file(path: &Path) -> Result<(), Error> {
    set_mode(path, 0o600)
}

/// Owner-only directory mode (`0700` on Unix). Other systems leave ACLs unchanged.
pub(crate) fn restrict_dir(path: &Path) -> Result<(), Error> {
    set_mode(path, 0o700)
}

/// Walk `root` and apply owner-only modes to every directory and file.
///
/// Used after writes that create files the caller did not open (Tantivy
/// segments under `{vault}/.scheda/`). Missing `root` is a no-op. Symlinks
/// are not followed.
pub(crate) fn restrict_tree(root: &Path) -> Result<(), Error> {
    let meta = match fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).context("[store] read owner-only path"),
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        restrict_dir(root)?;
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(err).context("[store] read dir for owner-only mode");
            }
        };
        for entry in entries {
            let entry = entry.context("[store] read dir entry for owner-only mode")?;
            restrict_tree(&entry.path())?;
        }
        return Ok(());
    }
    if meta.is_file() {
        restrict_file(root)?;
    }
    Ok(())
}

/// `{vault}/.scheda/` must be owner-only on Unix (`0700` directories, `0600`
/// files). Wider modes fail `check` with the vault-relative path in `message`.
/// Other systems do not inspect ACLs.
pub(crate) fn check_scheda_owner_only(vault: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        let scheda = vault.join(".scheda");
        if !scheda.exists() {
            return Ok(());
        }
        check_owner_only_tree(vault, &scheda)?;
    }
    #[cfg(not(unix))]
    {
        let _ = vault;
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;
        match fs::set_permissions(path, Permissions::from_mode(mode)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).context("[store] set owner-only mode"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

#[cfg(unix)]
fn check_owner_only_tree(vault: &Path, path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).context("[store::check] read mode"),
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let rel = rel_posix(vault, path)?;
    let mode = meta.permissions().mode() & 0o777;
    if meta.is_dir() {
        if mode != 0o700 {
            bail!(Error::invalid(format!(
                "{rel} mode is {mode:04o}; owner-only directory is 0700"
            )));
        }
        let entries = fs::read_dir(path).context("[store::check] read dir")?;
        for entry in entries {
            let entry = entry.context("[store::check] read dir entry")?;
            check_owner_only_tree(vault, &entry.path())?;
        }
        return Ok(());
    }
    if meta.is_file() && mode != 0o600 {
        bail!(Error::invalid(format!(
            "{rel} mode is {mode:04o}; owner-only file is 0600"
        )));
    }
    Ok(())
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
            encrypted: false,
            exclude_lexical_index: false,
            exclude_vector_index: false,
        }
    }

    #[test]
    fn document_file_is_id_md() {
        assert_eq!(
            record_file(&decl(Kind::Document), "alpha").unwrap(),
            "alpha.md"
        );
        assert_eq!(
            record_file(&decl(Kind::Document), "work/meeting").unwrap(),
            "work/meeting.md"
        );
        assert_eq!(
            record_file(&decl(Kind::Singleton), "inbox").unwrap(),
            "record.yaml"
        );
    }
}
