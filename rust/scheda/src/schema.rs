//! Schema declarations loaded from Vault files (`05`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use regex::Regex;
use serde::Deserialize;

use crate::Error;

const MIB: u64 = 1024 * 1024;
pub(crate) const DEFAULT_SEGMENT_MAX_BYTES: u64 = 8 * MIB;
pub(crate) const MAX_SEGMENT_MAX_BYTES: u64 = 32 * MIB;

const DEFAULT_SCHEMA_DIR: &str = "schema";
const DEFAULT_ID_FIELD: &str = "id";

/// Collection write mode (`03` MOD-2).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Document,
    Journal,
    Singleton,
}

impl Kind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Journal => "journal",
            Self::Singleton => "singleton",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FieldType {
    String,
    Integer,
    Number,
    Boolean,
    StringList,
    Object,
    ObjectList,
}

impl FieldType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::StringList => "string_list",
            Self::Object => "object",
            Self::ObjectList => "object_list",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SegmentRotate {
    Daily,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoot {
    schema_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawId {
    field: Option<String>,
    pattern: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawField {
    name: String,
    #[serde(rename = "type")]
    type_: FieldType,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    nullable: bool,
    #[serde(default)]
    filter: bool,
    default: Option<serde_json::Value>,
    #[serde(default)]
    fields: Vec<RawField>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCollection {
    name: String,
    kind: Kind,
    schema_version: u32,
    #[serde(deserialize_with = "deserialize_layout")]
    layout: Vec<String>,
    id: Option<RawId>,
    fields: Vec<RawField>,
    segment_max_bytes: Option<u64>,
    segment_max_lines: Option<u64>,
    segment_rotate: Option<SegmentRotate>,
    singleton_id: Option<String>,
}

fn deserialize_layout<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(one) => Ok(vec![one]),
        OneOrMany::Many(many) => Ok(many),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FieldDecl {
    pub name: String,
    pub type_: FieldType,
    pub required: bool,
    pub nullable: bool,
    pub filter: bool,
    pub default: Option<serde_json::Value>,
    pub fields: Vec<FieldDecl>,
}

#[derive(Clone, Debug)]
pub(crate) struct CollectionDecl {
    pub name: String,
    pub kind: Kind,
    pub schema_version: u32,
    pub layout: Vec<String>,
    pub id_field: String,
    pub id_pattern: Option<Regex>,
    pub filter_keys: BTreeSet<String>,
    pub segment_max_bytes: Option<u64>,
    pub segment_max_lines: Option<u64>,
    pub segment_rotate: Option<SegmentRotate>,
    pub singleton_id: Option<String>,
    pub fields: Vec<FieldDecl>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Schema {
    collections: BTreeMap<String, CollectionDecl>,
}

impl Schema {
    #[must_use]
    pub(crate) fn get(&self, name: &str) -> Option<&CollectionDecl> {
        self.collections.get(name)
    }

    pub(crate) fn collections(&self) -> impl Iterator<Item = &CollectionDecl> {
        self.collections.values()
    }

    #[must_use]
    pub(crate) fn versions(&self) -> Vec<u32> {
        let mut versions: Vec<u32> = self
            .collections
            .values()
            .map(|decl| decl.schema_version)
            .collect();
        versions.sort_unstable();
        versions.dedup();
        versions
    }
}

pub(crate) fn load(vault: &Path) -> Result<Schema, Error> {
    let rel = load_schema_dir(vault)?;
    let dir = join_inside(vault, &rel)?;
    if !dir.exists() {
        return Ok(Schema::default());
    }
    if !dir.is_dir() {
        return Err(Error::invalid(format!(
            "schema_dir is not a directory: {}",
            dir.display()
        )));
    }
    let mut collections = BTreeMap::new();
    let entries = fs::read_dir(&dir)
        .map_err(|err| Error::io(format!("read schema directory {}: {err}", dir.display())))?;
    for entry in entries {
        let entry = entry
            .map_err(|err| Error::io(format!("read schema directory {}: {err}", dir.display())))?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        match ext {
            "yaml" | "yml" | "json" => {}
            _ => continue,
        }
        let decl = load_collection_file(&path)?;
        if collections.contains_key(&decl.name) {
            return Err(Error::invalid(format!(
                "duplicate collection name {}",
                decl.name
            )));
        }
        collections.insert(decl.name.clone(), decl);
    }
    Ok(Schema { collections })
}

fn load_schema_dir(vault: &Path) -> Result<String, Error> {
    let path = vault.join("schema.yaml");
    if !path.exists() {
        return Ok(DEFAULT_SCHEMA_DIR.to_owned());
    }
    let text = read_utf8(&path)?;
    if text.trim().is_empty() {
        return Ok(DEFAULT_SCHEMA_DIR.to_owned());
    }
    let root: RawRoot = yaml_serde::from_str(&text)
        .map_err(|err| Error::invalid(format!("schema {}: {err}", path.display())))?;
    match root.schema_dir {
        Some(dir) if dir.is_empty() => Err(Error::invalid(format!(
            "schema {}: schema_dir must be non-empty",
            path.display()
        ))),
        Some(dir) => Ok(dir),
        None => Ok(DEFAULT_SCHEMA_DIR.to_owned()),
    }
}

fn load_collection_file(path: &Path) -> Result<CollectionDecl, Error> {
    let text = read_utf8(path)?;
    let yaml = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext != "json");
    let raw: RawCollection = if yaml {
        yaml_serde::from_str(&text)
            .map_err(|err| Error::invalid(format!("schema {}: {err}", path.display())))?
    } else {
        serde_json::from_str(&text)
            .map_err(|err| Error::invalid(format!("schema {}: {err}", path.display())))?
    };
    finish_collection(raw, path)
}

fn finish_collection(raw: RawCollection, path: &Path) -> Result<CollectionDecl, Error> {
    let loc = path.display();
    if raw.name.is_empty() {
        return Err(Error::invalid(format!("schema {loc}: name is required")));
    }
    if raw.name.contains('/') || raw.name.contains('\\') {
        return Err(Error::invalid(format!(
            "schema {loc}: name must not contain a path separator"
        )));
    }
    if raw.schema_version < 1 {
        return Err(Error::invalid(format!(
            "schema {loc}: schema_version must be >= 1"
        )));
    }
    if raw.layout.is_empty() || raw.layout.iter().any(|t| t.is_empty()) {
        return Err(Error::invalid(format!(
            "schema {loc}: layout must be a non-empty template"
        )));
    }
    if raw.kind == Kind::Document && raw.layout.iter().any(|t| !t.contains("{id}")) {
        return Err(Error::invalid(format!(
            "schema {loc}: document layout must contain {{id}}"
        )));
    }
    match raw.kind {
        Kind::Journal => {
            if raw.singleton_id.is_some() {
                return Err(Error::invalid(format!(
                    "schema {loc}: singleton_id is not valid on journal"
                )));
            }
        }
        Kind::Document => {
            if raw.segment_max_bytes.is_some()
                || raw.segment_max_lines.is_some()
                || raw.segment_rotate.is_some()
            {
                return Err(Error::invalid(format!(
                    "schema {loc}: journal segment keys are not valid on document"
                )));
            }
            if raw.singleton_id.is_some() {
                return Err(Error::invalid(format!(
                    "schema {loc}: singleton_id is not valid on document"
                )));
            }
        }
        Kind::Singleton => {
            if raw.segment_max_bytes.is_some()
                || raw.segment_max_lines.is_some()
                || raw.segment_rotate.is_some()
            {
                return Err(Error::invalid(format!(
                    "schema {loc}: journal segment keys are not valid on singleton"
                )));
            }
            match raw.singleton_id.as_deref() {
                Some(id) if !id.is_empty() => {}
                _ => {
                    return Err(Error::invalid(format!(
                        "schema {loc}: singleton_id is required"
                    )));
                }
            }
        }
    }
    let (segment_max_bytes, segment_max_lines, segment_rotate) = if raw.kind == Kind::Journal {
        let cap = raw.segment_max_bytes.unwrap_or(DEFAULT_SEGMENT_MAX_BYTES);
        if cap > MAX_SEGMENT_MAX_BYTES {
            return Err(Error::invalid(format!(
                "schema {loc}: segment_max_bytes {cap} exceeds 32 MiB"
            )));
        }
        if raw.segment_max_lines == Some(0) {
            return Err(Error::invalid(format!(
                "schema {loc}: segment_max_lines must be >= 1"
            )));
        }
        (Some(cap), raw.segment_max_lines, raw.segment_rotate)
    } else {
        (None, None, None)
    };
    let id_field = match raw.id.as_ref().and_then(|id| id.field.as_deref()) {
        Some("") => {
            return Err(Error::invalid(format!("schema {loc}: id.field is empty")));
        }
        Some(field) => field.to_owned(),
        None => DEFAULT_ID_FIELD.to_owned(),
    };
    let id_pattern = match raw.id.as_ref().and_then(|id| id.pattern.as_deref()) {
        Some(pattern) => Some(Regex::new(pattern).map_err(|err| {
            Error::invalid(format!("schema {loc}: id.pattern is not a regex: {err}"))
        })?),
        None => None,
    };
    let fields = finish_fields(raw.fields, path)?;
    let mut filter_keys = BTreeSet::new();
    collect_filter_keys(&fields, &mut filter_keys);
    Ok(CollectionDecl {
        name: raw.name,
        kind: raw.kind,
        schema_version: raw.schema_version,
        layout: raw.layout,
        id_field,
        id_pattern,
        filter_keys,
        segment_max_bytes,
        segment_max_lines,
        segment_rotate,
        singleton_id: raw.singleton_id,
        fields,
    })
}

fn finish_fields(raw: Vec<RawField>, path: &Path) -> Result<Vec<FieldDecl>, Error> {
    let loc = path.display();
    let mut out = Vec::with_capacity(raw.len());
    for field in raw {
        if field.name.is_empty() {
            return Err(Error::invalid(format!("schema {loc}: field name is empty")));
        }
        let nested_ok = matches!(field.type_, FieldType::Object | FieldType::ObjectList);
        if !field.fields.is_empty() && !nested_ok {
            return Err(Error::invalid(format!(
                "schema {loc}: nested fields are only valid on object types"
            )));
        }
        let nested = finish_fields(field.fields, path)?;
        out.push(FieldDecl {
            name: field.name,
            type_: field.type_,
            required: field.required,
            nullable: field.nullable,
            filter: field.filter,
            default: field.default,
            fields: nested,
        });
    }
    Ok(out)
}

fn collect_filter_keys(fields: &[FieldDecl], out: &mut BTreeSet<String>) {
    for field in fields {
        if field.filter {
            out.insert(field.name.clone());
        }
        collect_filter_keys(&field.fields, out);
    }
}

fn read_utf8(path: &Path) -> Result<String, Error> {
    let bytes = fs::read(path)
        .map_err(|err| Error::io(format!("read schema {}: {err}", path.display())))?;
    String::from_utf8(bytes)
        .map_err(|_| Error::invalid(format!("schema {} is not UTF-8", path.display())))
}

pub(crate) fn join_inside(vault: &Path, rel: &str) -> Result<PathBuf, Error> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err(Error::invalid(
            "schema_dir must be a path relative to the Vault",
        ));
    }
    let mut out = vault.to_path_buf();
    for component in rel_path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => out.push(part),
            Component::ParentDir => {
                if !out.pop() || out.as_path() == Path::new("") || !out.starts_with(vault) {
                    return Err(Error::invalid("schema_dir must stay inside the Vault"));
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(Error::invalid(
                    "schema_dir must be a path relative to the Vault",
                ));
            }
        }
    }
    if !out.starts_with(vault) {
        return Err(Error::invalid("schema_dir must stay inside the Vault"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{FieldType, Kind, finish_collection};
    use crate::Code;
    use std::path::Path;

    fn parse(yaml: &str) -> Result<super::CollectionDecl, crate::Error> {
        let raw: super::RawCollection =
            yaml_serde::from_str(yaml).map_err(|err| crate::Error::invalid(err.to_string()))?;
        finish_collection(raw, Path::new("schema/c.yaml"))
    }

    #[test]
    fn core_field_types_parse() {
        let decl = parse(include_str!("../testdata/schema/types.yaml")).unwrap();
        let types: Vec<_> = decl.fields.iter().map(|f| f.type_).collect();
        assert_eq!(
            types,
            [
                FieldType::String,
                FieldType::Integer,
                FieldType::Number,
                FieldType::Boolean,
                FieldType::StringList,
                FieldType::Object,
                FieldType::ObjectList,
            ]
        );
        assert_eq!(decl.id_field, "id");
        assert_eq!(decl.kind, Kind::Document);
    }

    #[test]
    fn missing_kind_is_invalid() {
        let err = yaml_serde::from_str::<super::RawCollection>(include_str!(
            "../testdata/schema/missing_kind.yaml"
        ))
        .expect_err("kind is required");
        let _ = err;
    }

    #[test]
    fn missing_schema_version_is_invalid() {
        let err = yaml_serde::from_str::<super::RawCollection>(include_str!(
            "../testdata/schema/missing_schema_version.yaml"
        ))
        .expect_err("schema_version is required");
        let _ = err;
    }

    #[test]
    fn executable_key_is_invalid() {
        let err = yaml_serde::from_str::<super::RawCollection>(include_str!(
            "../testdata/schema/on_write.yaml"
        ))
        .expect_err("unknown key");
        let _ = err;
    }

    #[test]
    fn identity_field_override() {
        let decl = parse(include_str!("../testdata/schema/slug.yaml")).unwrap();
        assert_eq!(decl.id_field, "slug");
    }

    #[test]
    fn filter_marks_parse() {
        let decl = parse(include_str!("../testdata/schema/filter.yaml")).unwrap();
        assert!(decl.filter_keys.contains("tagged"));
        assert!(!decl.filter_keys.contains("prose"));
    }

    #[test]
    fn journal_default_segment_cap() {
        let decl = parse(include_str!("../testdata/schema/journal.yaml")).unwrap();
        assert_eq!(
            decl.segment_max_bytes,
            Some(super::DEFAULT_SEGMENT_MAX_BYTES)
        );
    }

    #[test]
    fn journal_cap_above_32_mib_is_invalid() {
        let err = parse(include_str!("../testdata/schema/journal_oversize.yaml")).unwrap_err();
        assert_eq!(Code::of(&err), Some(Code::Invalid));
    }

    #[test]
    fn required_nullable_flags_parse() {
        let decl = parse(include_str!("../testdata/schema/required_nullable.yaml")).unwrap();
        assert!(decl.fields[0].required);
        assert!(!decl.fields[0].nullable);
        assert!(!decl.fields[1].required);
        assert!(decl.fields[1].nullable);
    }
}
