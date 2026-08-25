//! Schema declarations loaded from Vault files (`05`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use regex::Regex;
use serde::Deserialize;

use crate::error::{Code, ContextAs, ContextIo, bail};
use crate::{Catalog, CatalogBody, CatalogCollection, CatalogEdge, CatalogField, CatalogId, Error};

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

impl SegmentRotate {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Multiplicity {
    One,
    Many,
}

impl Multiplicity {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::One => "one",
            Self::Many => "many",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Dangling {
    Lax,
    Strict,
}

impl Dangling {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Lax => "lax",
            Self::Strict => "strict",
        }
    }
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
    #[serde(default)]
    fts: bool,
    #[serde(default)]
    vector: bool,
    field_id: Option<String>,
    edge: Option<RawEdge>,
    default: Option<serde_json::Value>,
    #[serde(default)]
    fields: Vec<RawField>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEdge {
    collection: String,
    multiplicity: Option<Multiplicity>,
    dangling: Option<Dangling>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBody {
    allowed: Option<bool>,
    fts: Option<bool>,
    vector: Option<bool>,
    field_id: Option<String>,
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
    body: Option<RawBody>,
    #[serde(default)]
    isolated: bool,
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
pub(crate) struct EdgeDecl {
    pub collection: String,
    pub multiplicity: Multiplicity,
    pub dangling: Dangling,
}

#[derive(Clone, Debug)]
pub(crate) struct BodyDecl {
    pub allowed: bool,
    pub fts: bool,
    pub vector: bool,
    pub field_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct FieldDecl {
    pub name: String,
    pub type_: FieldType,
    pub required: bool,
    pub nullable: bool,
    pub filter: bool,
    pub fts: bool,
    pub vector: bool,
    pub field_id: Option<String>,
    pub edge: Option<EdgeDecl>,
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
    pub id_pattern_src: Option<String>,
    pub filter_keys: BTreeSet<String>,
    pub segment_max_bytes: Option<u64>,
    pub segment_max_lines: Option<u64>,
    pub segment_rotate: Option<SegmentRotate>,
    pub singleton_id: Option<String>,
    pub body: Option<BodyDecl>,
    pub fields: Vec<FieldDecl>,
    pub isolated: bool,
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
        bail!(Error::invalid(format!(
            "schema_dir is not a directory: {}",
            dir.display()
        )));
    }
    let mut collections = BTreeMap::new();
    let mut by_hash = BTreeMap::new();
    let entries = fs::read_dir(&dir).context("[Schema::load] read schema dir")?;
    for entry in entries {
        let entry = entry.context("[Schema::load] read schema entry")?;
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
            bail!(Error::invalid(format!(
                "duplicate collection name {}",
                decl.name
            )));
        }
        let hash_dir = collection_dir(&decl.name);
        if let Some(other) = by_hash.get(&hash_dir) {
            bail!(Error::invalid(format!(
                "collections {} and {other} hash to the same directory {hash_dir}",
                decl.name
            )));
        }
        by_hash.insert(hash_dir, decl.name.clone());
        collections.insert(decl.name.clone(), decl);
    }
    validate_edge_targets(&collections)?;
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
    let root: RawRoot =
        yaml_serde::from_str(&text).context(Code::Invalid, "[Schema::load] parse schema.yaml")?;
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
            .context(Code::Invalid, "[Schema::load] parse collection YAML")?
    } else {
        serde_json::from_str(&text)
            .context(Code::Invalid, "[Schema::load] parse collection JSON")?
    };
    finish_collection(raw, path)
}

fn finish_collection(raw: RawCollection, path: &Path) -> Result<CollectionDecl, Error> {
    let loc = path.display();
    if raw.name.is_empty() {
        bail!(Error::invalid(format!("schema {loc}: name is required")));
    }
    if raw.schema_version < 1 {
        bail!(Error::invalid(format!(
            "schema {loc}: schema_version must be >= 1"
        )));
    }
    if raw.layout.is_empty() || raw.layout.iter().any(|t| t.is_empty()) {
        bail!(Error::invalid(format!(
            "schema {loc}: layout must be a non-empty template"
        )));
    }
    if raw.kind == Kind::Document && raw.layout.iter().any(|t| !t.contains("{id}")) {
        bail!(Error::invalid(format!(
            "schema {loc}: document layout must contain {{id}}"
        )));
    }
    match raw.kind {
        Kind::Journal => {
            if raw.singleton_id.is_some() {
                bail!(Error::invalid(format!(
                    "schema {loc}: singleton_id is not valid on journal"
                )));
            }
            if raw.body.is_some() {
                bail!(Error::invalid(format!(
                    "schema {loc}: body is not valid on journal"
                )));
            }
        }
        Kind::Document => {
            if raw.segment_max_bytes.is_some()
                || raw.segment_max_lines.is_some()
                || raw.segment_rotate.is_some()
            {
                bail!(Error::invalid(format!(
                    "schema {loc}: journal segment keys are not valid on document"
                )));
            }
            if raw.singleton_id.is_some() {
                bail!(Error::invalid(format!(
                    "schema {loc}: singleton_id is not valid on document"
                )));
            }
        }
        Kind::Singleton => {
            if raw.segment_max_bytes.is_some()
                || raw.segment_max_lines.is_some()
                || raw.segment_rotate.is_some()
            {
                bail!(Error::invalid(format!(
                    "schema {loc}: journal segment keys are not valid on singleton"
                )));
            }
            match raw.singleton_id.as_deref() {
                Some(id) if !id.is_empty() => {}
                _ => {
                    bail!(Error::invalid(format!(
                        "schema {loc}: singleton_id is required"
                    )));
                }
            }
            if raw.body.is_some() {
                bail!(Error::invalid(format!(
                    "schema {loc}: body is not valid on singleton"
                )));
            }
        }
    }
    let (segment_max_bytes, segment_max_lines, segment_rotate) = if raw.kind == Kind::Journal {
        let cap = raw.segment_max_bytes.unwrap_or(DEFAULT_SEGMENT_MAX_BYTES);
        if cap > MAX_SEGMENT_MAX_BYTES {
            bail!(Error::invalid(format!(
                "schema {loc}: segment_max_bytes {cap} exceeds 32 MiB"
            )));
        }
        if raw.segment_max_lines == Some(0) {
            bail!(Error::invalid(format!(
                "schema {loc}: segment_max_lines must be >= 1"
            )));
        }
        (Some(cap), raw.segment_max_lines, raw.segment_rotate)
    } else {
        (None, None, None)
    };
    let id_field = match raw.id.as_ref().and_then(|id| id.field.as_deref()) {
        Some("") => {
            bail!(Error::invalid(format!("schema {loc}: id.field is empty")));
        }
        Some(field) => field.to_owned(),
        None => DEFAULT_ID_FIELD.to_owned(),
    };
    let id_pattern_src = raw
        .id
        .as_ref()
        .and_then(|id| id.pattern.clone())
        .filter(|pattern| !pattern.is_empty());
    let id_pattern = match id_pattern_src.as_deref() {
        Some(pattern) => Some(Regex::new(pattern).map_err(|err| {
            Error::invalid(format!("schema {loc}: id.pattern is not a regex: {err}"))
        })?),
        None => None,
    };
    let fields = finish_fields(raw.fields, path)?;
    let mut filter_keys = BTreeSet::new();
    collect_filter_keys(&fields, &mut filter_keys);
    let body = match (raw.kind, raw.body) {
        (Kind::Document, None) => Some(BodyDecl {
            allowed: true,
            fts: false,
            vector: false,
            field_id: String::from("body"),
        }),
        (Kind::Document, Some(body)) => Some(finish_body(body, path)?),
        (_, None) => None,
        (_, Some(_)) => {
            bail!(Error::invalid(format!(
                "schema {loc}: body is only valid on document"
            )));
        }
    };
    Ok(CollectionDecl {
        name: raw.name,
        kind: raw.kind,
        schema_version: raw.schema_version,
        layout: raw.layout,
        id_field,
        id_pattern,
        id_pattern_src,
        filter_keys,
        segment_max_bytes,
        segment_max_lines,
        segment_rotate,
        singleton_id: raw.singleton_id,
        body,
        fields,
        isolated: raw.isolated,
    })
}

fn finish_body(raw: RawBody, path: &Path) -> Result<BodyDecl, Error> {
    let loc = path.display();
    let vector = raw.vector.unwrap_or(false);
    if raw.field_id.is_some() && !vector {
        bail!(Error::invalid(format!(
            "schema {loc}: body.field_id requires vector"
        )));
    }
    let field_id = match raw.field_id.as_deref() {
        Some("") => {
            bail!(Error::invalid(format!(
                "schema {loc}: body.field_id is empty"
            )));
        }
        Some(id) => id.to_owned(),
        None => String::from("body"),
    };
    Ok(BodyDecl {
        allowed: raw.allowed.unwrap_or(true),
        fts: raw.fts.unwrap_or(false),
        vector,
        field_id,
    })
}

fn finish_fields(raw: Vec<RawField>, path: &Path) -> Result<Vec<FieldDecl>, Error> {
    let loc = path.display();
    let mut out = Vec::with_capacity(raw.len());
    for field in raw {
        if field.name.is_empty() {
            bail!(Error::invalid(format!("schema {loc}: field name is empty")));
        }
        let nested_ok = matches!(field.type_, FieldType::Object | FieldType::ObjectList);
        if !field.fields.is_empty() && !nested_ok {
            bail!(Error::invalid(format!(
                "schema {loc}: nested fields are only valid on object types"
            )));
        }
        let nested = finish_fields(field.fields, path)?;
        let searchable = matches!(field.type_, FieldType::String | FieldType::StringList);
        if field.fts && !searchable {
            bail!(Error::invalid(format!(
                "schema {loc}: fts is only valid on string and string_list"
            )));
        }
        if field.vector && !searchable {
            bail!(Error::invalid(format!(
                "schema {loc}: vector is only valid on string and string_list"
            )));
        }
        if field.field_id.is_some() && !field.vector {
            bail!(Error::invalid(format!(
                "schema {loc}: field_id requires vector"
            )));
        }
        let field_id = match field.field_id.as_deref() {
            Some("") => {
                bail!(Error::invalid(format!("schema {loc}: field_id is empty")));
            }
            Some(id) => Some(id.to_owned()),
            None if field.vector => Some(field.name.clone()),
            None => None,
        };
        let edge = match field.edge {
            Some(edge) => Some(finish_edge(&field.name, field.type_, edge, path)?),
            None => None,
        };
        out.push(FieldDecl {
            name: field.name,
            type_: field.type_,
            required: field.required,
            nullable: field.nullable,
            filter: field.filter,
            fts: field.fts,
            vector: field.vector,
            field_id,
            edge,
            default: field.default,
            fields: nested,
        });
    }
    Ok(out)
}

fn finish_edge(name: &str, type_: FieldType, raw: RawEdge, path: &Path) -> Result<EdgeDecl, Error> {
    let loc = path.display();
    if raw.collection.is_empty() {
        bail!(Error::invalid(format!(
            "schema {loc}: edge.collection is required"
        )));
    }
    let multiplicity = raw.multiplicity.unwrap_or(Multiplicity::One);
    match (multiplicity, type_) {
        (Multiplicity::One, FieldType::String) | (Multiplicity::Many, FieldType::StringList) => {}
        (Multiplicity::One, _) => {
            bail!(Error::invalid(format!(
                "schema {loc}: edge {name} multiplicity one requires type string"
            )));
        }
        (Multiplicity::Many, _) => {
            bail!(Error::invalid(format!(
                "schema {loc}: edge {name} multiplicity many requires type string_list"
            )));
        }
    }
    Ok(EdgeDecl {
        collection: raw.collection,
        multiplicity,
        dangling: raw.dangling.unwrap_or(Dangling::Lax),
    })
}

fn validate_edge_targets(collections: &BTreeMap<String, CollectionDecl>) -> Result<(), Error> {
    for decl in collections.values() {
        check_edge_targets(&decl.fields, collections, &decl.name)?;
    }
    Ok(())
}

fn check_edge_targets(
    fields: &[FieldDecl],
    collections: &BTreeMap<String, CollectionDecl>,
    owner: &str,
) -> Result<(), Error> {
    for field in fields {
        if let Some(edge) = &field.edge
            && !collections.contains_key(&edge.collection)
        {
            bail!(Error::invalid(format!(
                "schema: edge {}.{} targets unknown collection {}",
                owner, field.name, edge.collection
            )));
        }
        check_edge_targets(&field.fields, collections, owner)?;
    }
    Ok(())
}

impl CollectionDecl {
    pub(crate) fn edge(&self, name: &str) -> Option<&EdgeDecl> {
        find_edge(&self.fields, name)
    }
}

pub(crate) fn to_catalog(schema: &Schema) -> Result<Catalog, Error> {
    let mut catalog = Catalog::default();
    for decl in schema.collections() {
        catalog.collections.push(collection_catalog(decl)?);
    }
    Ok(catalog)
}

fn collection_catalog(decl: &CollectionDecl) -> Result<CatalogCollection, Error> {
    let mut col = CatalogCollection::default()
        .with_name(decl.name.as_str())
        .with_kind(decl.kind.as_str())
        .with_schema_version(decl.schema_version);
    col.layout.clone_from(&decl.layout);
    let mut id = CatalogId::default().with_field(decl.id_field.as_str());
    if let Some(pattern) = &decl.id_pattern_src {
        id = id.with_pattern(pattern.as_str());
    }
    col.id = id.into();
    if let Some(bytes) = decl.segment_max_bytes {
        col = col.with_segment_max_bytes(bytes);
    }
    if let Some(lines) = decl.segment_max_lines {
        col = col.with_segment_max_lines(lines);
    }
    if let Some(rotate) = decl.segment_rotate {
        col = col.with_segment_rotate(rotate.as_str());
    }
    if let Some(singleton_id) = &decl.singleton_id {
        col = col.with_singleton_id(singleton_id.as_str());
    }
    if decl.isolated {
        return Ok(col.with_isolated(true));
    }
    col.fields = catalog_fields(&decl.fields)?;
    if let Some(body) = &decl.body {
        col.body = catalog_body(body).into();
    }
    Ok(col)
}

fn catalog_body(body: &BodyDecl) -> CatalogBody {
    let mut msg = CatalogBody::default().with_allowed(body.allowed);
    if body.fts {
        msg = msg.with_fts(true);
    }
    if body.vector {
        msg = msg.with_vector(true).with_field_id(body.field_id.as_str());
    }
    msg
}

fn catalog_fields(fields: &[FieldDecl]) -> Result<Vec<CatalogField>, Error> {
    let mut out = Vec::with_capacity(fields.len());
    for field in fields {
        let mut msg = CatalogField::default()
            .with_name(field.name.as_str())
            .with_type(field.type_.as_str());
        if field.required {
            msg = msg.with_required(true);
        }
        if field.nullable {
            msg = msg.with_nullable(true);
        }
        if field.filter {
            msg = msg.with_filter(true);
        }
        if field.fts {
            msg = msg.with_fts(true);
        }
        if field.vector {
            msg = msg.with_vector(true);
        }
        if let Some(field_id) = &field.field_id {
            msg = msg.with_field_id(field_id.as_str());
        }
        if let Some(edge) = &field.edge {
            msg.edge = CatalogEdge::default()
                .with_collection(edge.collection.as_str())
                .with_multiplicity(edge.multiplicity.as_str())
                .with_dangling(edge.dangling.as_str())
                .into();
        }
        if let Some(default) = &field.default {
            msg.default = catalog_field_default(default)?.default;
        }
        if !field.fields.is_empty() {
            msg.fields = catalog_fields(&field.fields)?;
        }
        out.push(msg);
    }
    Ok(out)
}

fn catalog_field_default(value: &serde_json::Value) -> Result<CatalogField, Error> {
    serde_json::from_value(serde_json::json!({
        "name": "_",
        "type": "string",
        "default": value,
    }))
    .context(
        Code::Invalid,
        "[Schema::catalog_field_default] encode default",
    )
}

fn find_edge<'a>(fields: &'a [FieldDecl], name: &str) -> Option<&'a EdgeDecl> {
    for field in fields {
        if field.name == name {
            return field.edge.as_ref();
        }
        if let Some(edge) = find_edge(&field.fields, name) {
            return Some(edge);
        }
    }
    None
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
    let bytes = fs::read(path).context("[Schema::read_utf8] read file")?;
    String::from_utf8(bytes).context(Code::NotUtf8, "[Schema::read_utf8] decode UTF-8")
}

/// On-disk directory for a collection: 16 lowercase hex digits of FNV-1a-64
/// of the UTF-8 name (MOD-8).
#[must_use]
pub fn collection_dir(name: &str) -> String {
    crate::fnv::hex(name.as_bytes())
}

pub(crate) fn join_in_collection(vault: &Path, name: &str, rel: &str) -> Result<PathBuf, Error> {
    let dir = collection_dir(name);
    if rel.is_empty() {
        join_inside(vault, &dir)
    } else {
        join_inside(vault, &format!("{dir}/{rel}"))
    }
}

pub(crate) fn join_inside(vault: &Path, rel: &str) -> Result<PathBuf, Error> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        bail!(Error::invalid(
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
                    bail!(Error::invalid("schema_dir must stay inside the Vault"));
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                bail!(Error::invalid(
                    "schema_dir must be a path relative to the Vault",
                ));
            }
        }
    }
    if !out.starts_with(vault) {
        bail!(Error::invalid("schema_dir must stay inside the Vault"));
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
    fn collection_name_may_contain_a_slash() {
        let decl = parse(
            "name: team/notes\nkind: document\nschema_version: 1\nlayout: n/{id}.md\nfields: []\n",
        )
        .unwrap();
        assert_eq!(decl.name, "team/notes");
        assert_eq!(super::collection_dir("team/notes").len(), 16);
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
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn required_nullable_flags_parse() {
        let decl = parse(include_str!("../testdata/schema/required_nullable.yaml")).unwrap();
        assert!(decl.fields[0].required);
        assert!(!decl.fields[0].nullable);
        assert!(!decl.fields[1].required);
        assert!(decl.fields[1].nullable);
    }

    #[test]
    fn search_marks_and_body_parse() {
        let decl = parse(
            "name: c\nkind: document\nschema_version: 1\nlayout: c/{id}.md\nbody:\n  fts: true\n  vector: true\nfields:\n  - name: title\n    type: string\n    fts: true\n    vector: true\n  - name: secret\n    type: string\n",
        )
        .unwrap();
        let body = decl.body.as_ref().unwrap();
        assert!(body.allowed);
        assert!(body.fts);
        assert!(body.vector);
        assert_eq!(body.field_id, "body");
        assert!(decl.fields[0].fts);
        assert!(decl.fields[0].vector);
        assert_eq!(decl.fields[0].field_id.as_deref(), Some("title"));
        assert!(!decl.fields[1].fts);
        assert!(!decl.fields[1].vector);
    }

    #[test]
    fn fts_on_integer_is_invalid() {
        let err = parse(
            "name: c\nkind: document\nschema_version: 1\nlayout: c/{id}.md\nfields:\n  - name: n\n    type: integer\n    fts: true\n",
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn body_on_journal_is_invalid() {
        let err = parse(
            "name: c\nkind: journal\nschema_version: 1\nlayout: log/\nbody:\n  fts: true\nfields: []\n",
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn edge_one_on_string_list_is_invalid() {
        let err = parse(
            "name: c\nkind: document\nschema_version: 1\nlayout: c/{id}.md\nfields:\n  - name: refs\n    type: string_list\n    edge:\n      collection: c\n      multiplicity: one\n",
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn isolated_mark_parses() {
        let decl = parse(
            "name: c\nkind: document\nschema_version: 1\nlayout: c/{id}.md\nisolated: true\nfields:\n  - name: secret\n    type: string\n",
        )
        .unwrap();
        assert!(decl.isolated);
        assert_eq!(decl.fields[0].name, "secret");
    }
}
