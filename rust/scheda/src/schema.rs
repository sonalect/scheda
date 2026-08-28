//! Schema declarations loaded from Vault files.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Code, Context, ContextAs, ContextIo, bail};
use crate::id::require_slug;
use crate::store;
use crate::{
    Body, CollectionInfo, Edge, Error, Field, Journal, Object, Record, Schema as ApiSchema,
    Singleton, Text, Unit, VaultInfo,
};

const MIB: u64 = 1024 * 1024;
pub(crate) const DEFAULT_SEGMENT_MAX_BYTES: u64 = 8 * MIB;
pub(crate) const MAX_SEGMENT_MAX_BYTES: u64 = 32 * MIB;
const SCHEMA_YAML: &str = ".schema.yaml";
const SCHEMA_YML: &str = ".schema.yml";
const SCHEMA_JSON: &str = ".schema.json";
const SCHEMA_FILENAMES: [&str; 3] = [SCHEMA_YAML, SCHEMA_YML, SCHEMA_JSON];

/// Collection write mode: `document`, `journal`, or `singleton`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// One file per Record; `replace` is allowed.
    Document,
    /// Append-only JSONL segments; `replace` and `remove` are `unsupported`.
    Journal,
    /// One file for the collection; replace as a whole.
    Singleton,
}

impl Kind {
    /// Schema kind literal: `document`, `journal`, or `singleton`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Journal => "journal",
            Self::Singleton => "singleton",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SegmentRotate {
    Daily,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Multiplicity {
    One,
    Many,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Dangling {
    Lax,
    Strict,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawText {
    #[serde(default)]
    exclude_fts: bool,
    #[serde(default)]
    exclude_vector: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUnit {}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawObject {
    #[serde(default)]
    fields: Vec<RawField>,
    values: Option<Box<RawField>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEdge {
    collection: String,
    one: Option<RawUnit>,
    many: Option<RawUnit>,
    dangling: Option<Dangling>,
    #[serde(default)]
    exclude_fts: bool,
    #[serde(default)]
    exclude_vector: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawField {
    name: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    nullable: bool,
    #[serde(default)]
    exclude_find: bool,
    default: Option<serde_json::Value>,
    string: Option<RawText>,
    integer: Option<RawUnit>,
    number: Option<RawUnit>,
    boolean: Option<RawUnit>,
    string_list: Option<RawText>,
    object: Option<RawObject>,
    object_list: Option<RawObject>,
    edge: Option<RawEdge>,
}

#[derive(Debug)]
enum RawSpec {
    String(RawText),
    Integer,
    Number,
    Boolean,
    StringList(RawText),
    Object(RawObject),
    ObjectList(RawObject),
    Edge(RawEdge),
}

impl RawField {
    fn spec(&self) -> Result<RawSpec, Error> {
        let mut n = 0u8;
        let mut spec = None;
        if let Some(text) = &self.string {
            n += 1;
            spec = Some(RawSpec::String(text.clone()));
        }
        if self.integer.is_some() {
            n += 1;
            spec = Some(RawSpec::Integer);
        }
        if self.number.is_some() {
            n += 1;
            spec = Some(RawSpec::Number);
        }
        if self.boolean.is_some() {
            n += 1;
            spec = Some(RawSpec::Boolean);
        }
        if let Some(text) = &self.string_list {
            n += 1;
            spec = Some(RawSpec::StringList(text.clone()));
        }
        if let Some(object) = &self.object {
            n += 1;
            spec = Some(RawSpec::Object(object.clone()));
        }
        if let Some(object) = &self.object_list {
            n += 1;
            spec = Some(RawSpec::ObjectList(object.clone()));
        }
        if let Some(edge) = &self.edge {
            n += 1;
            spec = Some(RawSpec::Edge(edge.clone()));
        }
        match (n, spec) {
            (1, Some(spec)) => Ok(spec),
            _ => Err(Error::invalid(format!(
                "field {} must have exactly one type spec",
                self.name
            ))),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBody {
    allowed: Option<bool>,
    #[serde(default)]
    exclude_fts: bool,
    #[serde(default)]
    exclude_vector: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSingleton {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCollection {
    name: String,
    kind: Kind,
    version: u32,
    fields: Vec<RawField>,
    segment_max_bytes: Option<u64>,
    segment_max_lines: Option<u64>,
    daily: Option<RawUnit>,
    #[serde(alias = "document")]
    body: Option<RawBody>,
    singleton: Option<RawSingleton>,
    #[serde(default)]
    isolated: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct EdgeDecl {
    pub collection: String,
    pub multiplicity: Multiplicity,
    pub dangling: Dangling,
    pub fts: bool,
    pub vector: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct BodyDecl {
    pub allowed: bool,
    pub fts: bool,
    pub vector: bool,
    #[cfg(feature = "search")]
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
    #[cfg(feature = "search")]
    pub field_id: Option<String>,
    pub edge: Option<EdgeDecl>,
    pub default: Option<serde_json::Value>,
    pub fields: Vec<FieldDecl>,
    pub values: Option<Box<FieldDecl>>,
}

#[derive(Clone, Debug)]
pub(crate) struct CollectionDecl {
    pub name: String,
    pub kind: Kind,
    pub version: u32,
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
        let mut versions: Vec<u32> = self.collections.values().map(|decl| decl.version).collect();
        versions.sort_unstable();
        versions.dedup();
        versions
    }
}

pub(crate) fn load(vault: &Path) -> Result<Schema, Error> {
    let mut collections = BTreeMap::new();
    let entries = match fs::read_dir(vault) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Schema::default());
        }
        Err(err) => {
            return Err(err).context("[Schema::load] read vault");
        }
    };
    for entry in entries {
        let entry = entry.context("[Schema::load] read vault entry")?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(dir_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !is_collection_dir_name(&dir_name) {
            continue;
        }
        let Some(schema_path) = find_schema_file(&path)? else {
            continue;
        };
        let decl = load_collection_file(&schema_path)?;
        if decl.name != dir_name {
            bail!(Error::invalid(format!(
                "schema {}: directory {dir_name} is not the collection name {}",
                schema_path.display(),
                decl.name
            )));
        }
        if collections.contains_key(&decl.name) {
            bail!(Error::invalid(format!(
                "duplicate collection name {}",
                decl.name
            )));
        }
        collections.insert(decl.name.clone(), decl);
    }
    validate_edge_targets(&collections)?;
    Ok(Schema { collections })
}

fn is_collection_dir_name(name: &str) -> bool {
    !name.starts_with('.')
}

fn find_schema_file(dir: &Path) -> Result<Option<PathBuf>, Error> {
    let mut found = None;
    for name in SCHEMA_FILENAMES {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        if found.is_some() {
            bail!(Error::invalid(format!(
                "collection directory {} has more than one schema file",
                dir.display()
            )));
        }
        found = Some(path);
    }
    Ok(found)
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
    require_slug("collection name", &raw.name).with_context(|| format!("schema {loc}: name"))?;
    if raw.version < 1 {
        bail!(Error::invalid(format!(
            "schema {loc}: version must be >= 1"
        )));
    }
    match raw.kind {
        Kind::Journal => {
            if raw.singleton.is_some() {
                bail!(Error::invalid(format!(
                    "schema {loc}: singleton is not valid on journal"
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
                || raw.daily.is_some()
            {
                bail!(Error::invalid(format!(
                    "schema {loc}: journal segment keys are not valid on document"
                )));
            }
            if raw.singleton.is_some() {
                bail!(Error::invalid(format!(
                    "schema {loc}: singleton is not valid on document"
                )));
            }
        }
        Kind::Singleton => {
            if raw.segment_max_bytes.is_some()
                || raw.segment_max_lines.is_some()
                || raw.daily.is_some()
            {
                bail!(Error::invalid(format!(
                    "schema {loc}: journal segment keys are not valid on singleton"
                )));
            }
            match raw.singleton.as_ref() {
                Some(singleton) if !singleton.id.is_empty() => {
                    require_slug("singleton id", &singleton.id)
                        .with_context(|| format!("schema {loc}: singleton.id"))?;
                }
                _ => {
                    bail!(Error::invalid(format!(
                        "schema {loc}: singleton.id is required"
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
        if cap == 0 {
            bail!(Error::invalid(format!(
                "schema {loc}: segment_max_bytes must be >= 1"
            )));
        }
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
        let rotate = raw.daily.map(|_| SegmentRotate::Daily);
        (Some(cap), raw.segment_max_lines, rotate)
    } else {
        (None, None, None)
    };
    let fields = finish_fields(raw.fields, path)?;
    let mut filter_keys = BTreeSet::new();
    collect_filter_keys(&fields, &mut filter_keys);
    let body = match (raw.kind, raw.body) {
        (Kind::Document, None) => Some(BodyDecl {
            allowed: true,
            fts: true,
            vector: true,
            #[cfg(feature = "search")]
            field_id: String::from("body"),
        }),
        (Kind::Document, Some(body)) => Some(finish_body(body)),
        (_, None) => None,
        (_, Some(_)) => {
            bail!(Error::invalid(format!(
                "schema {loc}: body is only valid on document"
            )));
        }
    };
    add_body_find_key(body.as_ref(), &mut filter_keys);
    let singleton_id = raw.singleton.map(|s| s.id);
    Ok(CollectionDecl {
        name: raw.name,
        kind: raw.kind,
        version: raw.version,
        filter_keys,
        segment_max_bytes,
        segment_max_lines,
        segment_rotate,
        singleton_id,
        body,
        fields,
        isolated: raw.isolated,
    })
}

fn finish_body(raw: RawBody) -> BodyDecl {
    BodyDecl {
        allowed: raw.allowed.unwrap_or(true),
        fts: !raw.exclude_fts,
        vector: !raw.exclude_vector,
        #[cfg(feature = "search")]
        field_id: String::from("body"),
    }
}

// A field name is a key in the Record, where `id` and `body` are taken. The
// `values` entry of an open map names no key: it only labels that entry in
// error messages, so both names are free there.
#[derive(Clone, Copy, PartialEq)]
enum NameRole {
    Key,
    Label,
}

fn finish_fields(raw: Vec<RawField>, path: &Path) -> Result<Vec<FieldDecl>, Error> {
    finish_fields_as(raw, path, NameRole::Key)
}

fn finish_fields_as(
    raw: Vec<RawField>,
    path: &Path,
    role: NameRole,
) -> Result<Vec<FieldDecl>, Error> {
    let loc = path.display();
    let mut out = Vec::with_capacity(raw.len());
    for field in raw {
        if field.name.is_empty() {
            bail!(Error::invalid(format!("schema {loc}: field name is empty")));
        }
        if role == NameRole::Key && (field.name == "id" || field.name == "body") {
            bail!(Error::invalid(format!(
                "schema {loc}: field name {} is reserved",
                field.name
            )));
        }
        let spec = field.spec()?;
        let (type_, nested, values, edge, fts, vector) = match spec {
            RawSpec::String(text) => (
                FieldType::String,
                Vec::new(),
                None,
                None,
                !text.exclude_fts,
                !text.exclude_vector,
            ),
            RawSpec::Integer => (FieldType::Integer, Vec::new(), None, None, false, false),
            RawSpec::Number => (FieldType::Number, Vec::new(), None, None, false, false),
            RawSpec::Boolean => (FieldType::Boolean, Vec::new(), None, None, false, false),
            RawSpec::StringList(text) => (
                FieldType::StringList,
                Vec::new(),
                None,
                None,
                !text.exclude_fts,
                !text.exclude_vector,
            ),
            RawSpec::Object(object) => {
                let (nested, values) = finish_object(&field.name, object, path)?;
                (FieldType::Object, nested, values, None, false, false)
            }
            RawSpec::ObjectList(object) => {
                let (nested, values) = finish_object(&field.name, object, path)?;
                (FieldType::ObjectList, nested, values, None, false, false)
            }
            RawSpec::Edge(raw_edge) => {
                let edge = finish_edge(&field.name, raw_edge, path)?;
                let type_ = match edge.multiplicity {
                    Multiplicity::One => FieldType::String,
                    Multiplicity::Many => FieldType::StringList,
                };
                let fts = edge.fts;
                let vector = edge.vector;
                (type_, Vec::new(), None, Some(edge), fts, vector)
            }
        };
        #[cfg(feature = "search")]
        let field_id = vector.then(|| field.name.clone());
        out.push(FieldDecl {
            name: field.name,
            type_,
            required: field.required,
            nullable: field.nullable,
            filter: !field.exclude_find,
            fts,
            vector,
            #[cfg(feature = "search")]
            field_id,
            edge,
            default: field.default,
            fields: nested,
            values,
        });
    }
    Ok(out)
}

fn finish_object(
    name: &str,
    raw: RawObject,
    path: &Path,
) -> Result<(Vec<FieldDecl>, Option<Box<FieldDecl>>), Error> {
    let loc = path.display();
    if !raw.fields.is_empty() && raw.values.is_some() {
        bail!(Error::invalid(format!(
            "schema {loc}: object {name} must not set both fields and values"
        )));
    }
    if let Some(values) = raw.values {
        refuse_values_flags(&values, path)?;
        let values = finish_one_field(*values, path)?;
        if let Some(offender) = edge_under_values(&values) {
            bail!(Error::invalid(format!(
                "schema {loc}: values {offender} must not set edge"
            )));
        }
        Ok((Vec::new(), Some(Box::new(values))))
    } else {
        Ok((finish_fields(raw.fields, path)?, None))
    }
}

fn finish_one_field(raw: RawField, path: &Path) -> Result<FieldDecl, Error> {
    let mut fields = finish_fields_as(vec![raw], path, NameRole::Label)?;
    match fields.pop() {
        Some(field) if fields.is_empty() => Ok(field),
        _ => bail!(Error::invalid(format!(
            "schema {}: values is empty",
            path.display()
        ))),
    }
}

fn refuse_values_flags(field: &RawField, path: &Path) -> Result<(), Error> {
    let loc = path.display();
    if field.required {
        bail!(Error::invalid(format!(
            "schema {loc}: values {} must not set required",
            field.name
        )));
    }
    if field.exclude_find {
        bail!(Error::invalid(format!(
            "schema {loc}: values {} must not set exclude_find",
            field.name
        )));
    }
    if field.default.is_some() {
        bail!(Error::invalid(format!(
            "schema {loc}: values {} must not set default",
            field.name
        )));
    }
    Ok(())
}

// An edge is a dangling check *and* a `find` key. Under a key Schema does
// not name it could only ever be the first, so it is refused everywhere
// beneath `values`, not only as the value type itself. Returns the name of
// the first offending field.
fn edge_under_values(field: &FieldDecl) -> Option<&str> {
    if field.edge.is_some() {
        return Some(&field.name);
    }
    for nested in &field.fields {
        if let Some(name) = edge_under_values(nested) {
            return Some(name);
        }
    }
    field.values.as_deref().and_then(edge_under_values)
}

fn finish_edge(name: &str, raw: RawEdge, path: &Path) -> Result<EdgeDecl, Error> {
    let loc = path.display();
    if raw.collection.is_empty() {
        bail!(Error::invalid(format!(
            "schema {loc}: edge.collection is required"
        )));
    }
    require_slug("edge collection", &raw.collection)
        .with_context(|| format!("schema {loc}: edge.collection"))?;
    let multiplicity = match (raw.one.is_some(), raw.many.is_some()) {
        (true, false) => Multiplicity::One,
        (false, true) => Multiplicity::Many,
        _ => {
            bail!(Error::invalid(format!(
                "schema {loc}: edge {name} must set exactly one of one or many"
            )));
        }
    };
    Ok(EdgeDecl {
        collection: raw.collection,
        multiplicity,
        dangling: raw.dangling.unwrap_or(Dangling::Lax),
        fts: !raw.exclude_fts,
        vector: !raw.exclude_vector,
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

pub(crate) fn to_proto_schema(
    decl: &CollectionDecl,
    omit_fields: bool,
) -> Result<ApiSchema, Error> {
    let mut out = ApiSchema::default().with_version(decl.version);
    if decl.isolated {
        out = out.with_isolated(true);
    }
    out.kind = match decl.kind {
        Kind::Document => body_to_proto(decl.body.as_ref()).into(),
        Kind::Journal => journal_to_proto(decl).into(),
        Kind::Singleton => {
            let id = decl.singleton_id.as_deref().unwrap_or("");
            Singleton::default().with_id(id).into()
        }
    };
    if !omit_fields {
        out.fields = fields_to_proto(&decl.fields)?;
    }
    Ok(out)
}

pub(crate) fn to_vault_info(
    path: &Path,
    index_path: &Path,
    remote: &str,
    auto_sync: bool,
    schema: &Schema,
) -> Result<VaultInfo, Error> {
    let mut info = VaultInfo::default()
        .with_path(path.to_string_lossy().into_owned())
        .with_index_path(index_path.to_string_lossy().into_owned())
        .with_auto_sync(auto_sync);
    if !remote.is_empty() {
        info = info.with_remote(remote.to_owned());
    }
    for decl in schema.collections() {
        let mut item = CollectionInfo::default().with_name(decl.name.as_str());
        item.schema = to_proto_schema(decl, decl.isolated)?.into();
        info.collections.push(item);
    }
    Ok(info)
}

fn body_to_proto(body: Option<&BodyDecl>) -> Body {
    let Some(body) = body else {
        return Body::default();
    };
    let mut msg = Body::default();
    if !body.allowed {
        msg = msg.with_no_body(true);
    }
    let mut text = Text::default();
    if !body.fts {
        text = text.with_exclude_fts(true);
    }
    if !body.vector {
        text = text.with_exclude_vector(true);
    }
    if text.exclude_fts.is_some() || text.exclude_vector.is_some() {
        msg.text = text.into();
    }
    msg
}

fn journal_to_proto(decl: &CollectionDecl) -> Journal {
    let mut msg = Journal::default();
    if let Some(bytes) = decl
        .segment_max_bytes
        .filter(|n| *n != DEFAULT_SEGMENT_MAX_BYTES)
    {
        msg = msg.with_segment_max_bytes(bytes);
    }
    if let Some(lines) = decl.segment_max_lines {
        msg = msg.with_segment_max_lines(lines);
    }
    if decl.segment_rotate == Some(SegmentRotate::Daily) {
        msg.daily = Unit::default().into();
    }
    msg
}

fn fields_to_proto(fields: &[FieldDecl]) -> Result<Vec<Field>, Error> {
    let mut out = Vec::with_capacity(fields.len());
    for field in fields {
        let mut msg = Field::default().with_name(field.name.as_str());
        if field.required {
            msg = msg.with_required(true);
        }
        if field.nullable {
            msg = msg.with_nullable(true);
        }
        if !field.filter {
            msg = msg.with_exclude_find(true);
        }
        if let Some(default) = &field.default {
            msg.default = serde_json::from_value(default.clone())
                .context(Code::Invalid, "[Schema] encode field default")?;
        }
        msg.spec = Some(field_spec_to_proto(field)?);
        out.push(msg);
    }
    Ok(out)
}

fn field_spec_to_proto(
    field: &FieldDecl,
) -> Result<scheda_generated::api::v1::__buffa::oneof::field::Spec, Error> {
    use scheda_generated::api::v1::__buffa::oneof::field::Spec;
    if let Some(edge) = &field.edge {
        return Ok(Spec::Edge(Box::new(edge_to_proto(edge))));
    }
    let text = || {
        let mut t = Text::default();
        if !field.fts {
            t = t.with_exclude_fts(true);
        }
        if !field.vector {
            t = t.with_exclude_vector(true);
        }
        t
    };
    Ok(match field.type_ {
        FieldType::String => Spec::String(Box::new(text())),
        FieldType::Integer => Spec::Integer(Box::default()),
        FieldType::Number => Spec::Number(Box::default()),
        FieldType::Boolean => Spec::Boolean(Box::default()),
        FieldType::StringList => Spec::StringList(Box::new(text())),
        FieldType::Object => Spec::Object(Box::new(object_to_proto(field)?)),
        FieldType::ObjectList => Spec::ObjectList(Box::new(object_to_proto(field)?)),
    })
}

fn object_to_proto(field: &FieldDecl) -> Result<Object, Error> {
    let mut obj = Object::default();
    if let Some(values) = &field.values {
        let mut fields = fields_to_proto(std::slice::from_ref(values.as_ref()))?;
        let Some(msg) = fields.pop() else {
            bail!(Error::invalid("schema: values is empty"));
        };
        obj.values = msg.into();
    } else {
        obj.fields = fields_to_proto(&field.fields)?;
    }
    Ok(obj)
}

fn edge_to_proto(edge: &EdgeDecl) -> Edge {
    use scheda_generated::api::v1::__buffa::oneof::edge::Arity;
    let mut msg = Edge::default().with_collection(edge.collection.as_str());
    if edge.dangling == Dangling::Strict {
        msg = msg.with_strict(true);
    }
    msg.arity = Some(match edge.multiplicity {
        Multiplicity::One => Arity::One(Box::default()),
        Multiplicity::Many => Arity::Many(Box::default()),
    });
    let mut text = Text::default();
    if !edge.fts {
        text = text.with_exclude_fts(true);
    }
    if !edge.vector {
        text = text.with_exclude_vector(true);
    }
    if text.exclude_fts.is_some() || text.exclude_vector.is_some() {
        msg.text = text.into();
    }
    msg
}

/// Write one collection declaration. Returns the vault-relative POSIX path.
pub(crate) fn put_collection(
    vault: &Path,
    name: &str,
    incoming: &ApiSchema,
) -> Result<String, Error> {
    incoming.require_valid()?;
    require_slug("collection name", name)?;
    let decl = decl_from_proto(name, incoming)?;
    let dir = join_inside(vault, collection_dir(name))?;
    let rel = match find_schema_file(&dir)? {
        Some(path) => posix_rel(vault, &path)?,
        None => format!("{}/{SCHEMA_YAML}", collection_dir(name)),
    };
    let path = join_inside(vault, &rel)?;
    let mut schema = load(vault)?;
    schema.collections.insert(decl.name.clone(), decl.clone());
    validate_edge_targets(&schema.collections)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("[Schema::put_collection] create collection dir")?;
    }
    if path.is_file() {
        let existing = load_collection_file(&path)?;
        if existing.name != decl.name {
            bail!(Error::invalid(format!(
                "schema file {rel} already declares collection {}",
                existing.name
            )));
        }
    }
    let body = if rel.ends_with(".json") {
        json_from_decl(&decl)?
    } else {
        yaml_from_decl(&decl)?
    };
    store::atomic_write(&path, body.as_bytes())?;
    Ok(rel)
}

fn decl_from_proto(name: &str, schema: &ApiSchema) -> Result<CollectionDecl, Error> {
    use scheda_generated::api::v1::__buffa::oneof::schema::Kind as SchemaKind;
    let version = schema
        .version
        .ok_or_else(|| Error::invalid("schema: version is required"))?;
    let (kind, body, singleton_id, segment_max_bytes, segment_max_lines, segment_rotate) =
        match schema.kind.as_ref() {
            Some(SchemaKind::Document(body)) => (
                Kind::Document,
                Some(body_from_proto(body)),
                None,
                None,
                None,
                None,
            ),
            Some(SchemaKind::Journal(journal)) => {
                let cap = journal
                    .segment_max_bytes
                    .unwrap_or(DEFAULT_SEGMENT_MAX_BYTES);
                if cap == 0 || cap > MAX_SEGMENT_MAX_BYTES {
                    bail!(Error::invalid("schema: segment_max_bytes out of range"));
                }
                (
                    Kind::Journal,
                    None,
                    None,
                    Some(cap),
                    journal.segment_max_lines,
                    journal.daily.as_option().map(|_| SegmentRotate::Daily),
                )
            }
            Some(SchemaKind::Singleton(singleton)) => {
                let id = singleton
                    .id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| Error::invalid("schema: singleton.id is required"))?;
                require_slug("singleton id", id)?;
                (Kind::Singleton, None, Some(id.to_owned()), None, None, None)
            }
            None => bail!(Error::invalid("schema: kind is required")),
        };
    let fields = fields_from_proto(&schema.fields)?;
    let mut filter_keys = BTreeSet::new();
    collect_filter_keys(&fields, &mut filter_keys);
    add_body_find_key(body.as_ref(), &mut filter_keys);
    Ok(CollectionDecl {
        name: name.to_owned(),
        kind,
        version,
        filter_keys,
        segment_max_bytes,
        segment_max_lines,
        segment_rotate,
        singleton_id,
        body,
        fields,
        isolated: schema.isolated.unwrap_or(false),
    })
}

fn body_from_proto(body: &Body) -> BodyDecl {
    let (exclude_fts, exclude_vector) = match body.text.as_option() {
        Some(text) => (
            text.exclude_fts.unwrap_or(false),
            text.exclude_vector.unwrap_or(false),
        ),
        None => (false, false),
    };
    BodyDecl {
        allowed: !body.no_body.unwrap_or(false),
        fts: !exclude_fts,
        vector: !exclude_vector,
        #[cfg(feature = "search")]
        field_id: String::from("body"),
    }
}

fn fields_from_proto(fields: &[Field]) -> Result<Vec<FieldDecl>, Error> {
    fields_from_proto_as(fields, NameRole::Key)
}

fn fields_from_proto_as(fields: &[Field], role: NameRole) -> Result<Vec<FieldDecl>, Error> {
    use scheda_generated::api::v1::__buffa::oneof::edge::Arity;
    use scheda_generated::api::v1::__buffa::oneof::field::Spec;
    let mut out = Vec::with_capacity(fields.len());
    for field in fields {
        let name = field
            .name
            .as_deref()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| Error::invalid("schema: field name is required"))?;
        if role == NameRole::Key && (name == "id" || name == "body") {
            bail!(Error::invalid(format!(
                "schema: field name {name} is reserved"
            )));
        }
        let default = match field.default.as_option() {
            Some(value) => Some(
                serde_json::to_value(value)
                    .context(Code::Invalid, "[Schema] decode field default")?,
            ),
            None => None,
        };
        let (type_, nested, values, edge, fts, vector) = match field.spec.as_ref() {
            Some(Spec::String(text)) => (
                FieldType::String,
                Vec::new(),
                None,
                None,
                !text.exclude_fts.unwrap_or(false),
                !text.exclude_vector.unwrap_or(false),
            ),
            Some(Spec::Integer(_)) => (FieldType::Integer, Vec::new(), None, None, false, false),
            Some(Spec::Number(_)) => (FieldType::Number, Vec::new(), None, None, false, false),
            Some(Spec::Boolean(_)) => (FieldType::Boolean, Vec::new(), None, None, false, false),
            Some(Spec::StringList(text)) => (
                FieldType::StringList,
                Vec::new(),
                None,
                None,
                !text.exclude_fts.unwrap_or(false),
                !text.exclude_vector.unwrap_or(false),
            ),
            Some(Spec::Object(object)) => {
                let (nested, values) = object_from_proto(object)?;
                (FieldType::Object, nested, values, None, false, false)
            }
            Some(Spec::ObjectList(object)) => {
                let (nested, values) = object_from_proto(object)?;
                (FieldType::ObjectList, nested, values, None, false, false)
            }
            Some(Spec::Edge(raw)) => {
                let collection = raw
                    .collection
                    .as_deref()
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| Error::invalid("schema: edge.collection is required"))?;
                require_slug("edge collection", collection)?;
                let multiplicity = match raw.arity.as_ref() {
                    Some(Arity::One(_)) => Multiplicity::One,
                    Some(Arity::Many(_)) => Multiplicity::Many,
                    None => bail!(Error::invalid("schema: edge arity is required")),
                };
                let (exclude_fts, exclude_vector) = match raw.text.as_option() {
                    Some(text) => (
                        text.exclude_fts.unwrap_or(false),
                        text.exclude_vector.unwrap_or(false),
                    ),
                    None => (false, false),
                };
                let edge = EdgeDecl {
                    collection: collection.to_owned(),
                    multiplicity,
                    dangling: if raw.strict.unwrap_or(false) {
                        Dangling::Strict
                    } else {
                        Dangling::Lax
                    },
                    fts: !exclude_fts,
                    vector: !exclude_vector,
                };
                let type_ = match multiplicity {
                    Multiplicity::One => FieldType::String,
                    Multiplicity::Many => FieldType::StringList,
                };
                let fts = edge.fts;
                let vector = edge.vector;
                (type_, Vec::new(), None, Some(edge), fts, vector)
            }
            None => bail!(Error::invalid(format!("schema: field {name} has no spec"))),
        };
        #[cfg(feature = "search")]
        let field_id = vector.then(|| name.to_owned());
        out.push(FieldDecl {
            name: name.to_owned(),
            type_,
            required: field.required.unwrap_or(false),
            nullable: field.nullable.unwrap_or(false),
            filter: !field.exclude_find.unwrap_or(false),
            fts,
            vector,
            #[cfg(feature = "search")]
            field_id,
            edge,
            default,
            fields: nested,
            values,
        });
    }
    Ok(out)
}

fn object_from_proto(object: &Object) -> Result<(Vec<FieldDecl>, Option<Box<FieldDecl>>), Error> {
    if !object.fields.is_empty() && object.values.as_option().is_some() {
        bail!(Error::invalid(
            "schema: object must not set both fields and values"
        ));
    }
    let values = match object.values.as_option() {
        Some(field) => {
            let mut fields = fields_from_proto_as(std::slice::from_ref(field), NameRole::Label)?;
            let Some(values) = fields.pop() else {
                bail!(Error::invalid("schema: values is empty"));
            };
            if let Some(offender) = edge_under_values(&values) {
                bail!(Error::invalid(format!(
                    "schema: values {offender} must not set edge"
                )));
            }
            Some(Box::new(values))
        }
        None => None,
    };
    Ok((fields_from_proto(&object.fields)?, values))
}

fn yaml_from_decl(decl: &CollectionDecl) -> Result<String, Error> {
    yaml_serde::to_string(&DiskCollection::from(decl))
        .context(Code::Invalid, "[Schema::put_collection] encode YAML")
}

fn json_from_decl(decl: &CollectionDecl) -> Result<String, Error> {
    serde_json::to_string_pretty(&DiskCollection::from(decl))
        .context(Code::Invalid, "[Schema::put_collection] encode JSON")
}

#[derive(Serialize)]
struct DiskCollection {
    name: String,
    kind: &'static str,
    version: u32,
    fields: Vec<DiskField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    segment_max_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    segment_max_lines: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daily: Option<DiskUnit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    singleton: Option<DiskSingleton>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<DiskBody>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    isolated: bool,
}

#[derive(Serialize)]
struct DiskUnit {}

#[derive(Serialize)]
struct DiskSingleton {
    id: String,
}

#[derive(Serialize)]
struct DiskBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed: Option<bool>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    exclude_fts: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    exclude_vector: bool,
}

#[derive(Serialize)]
struct DiskText {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    exclude_fts: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    exclude_vector: bool,
}

#[derive(Serialize)]
struct DiskObject {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    fields: Vec<DiskField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    values: Option<Box<DiskField>>,
}

#[derive(Serialize)]
struct DiskEdge {
    collection: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    one: Option<DiskUnit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    many: Option<DiskUnit>,
    #[serde(skip_serializing_if = "is_default_lax")]
    dangling: Dangling,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    exclude_fts: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    exclude_vector: bool,
}

#[derive(Serialize)]
struct DiskField {
    name: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    required: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    nullable: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    exclude_find: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    string: Option<DiskText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    integer: Option<DiskUnit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    number: Option<DiskUnit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    boolean: Option<DiskUnit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    string_list: Option<DiskText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object: Option<DiskObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object_list: Option<DiskObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    edge: Option<DiskEdge>,
}

fn is_default_lax(value: &Dangling) -> bool {
    *value == Dangling::Lax
}

impl From<&CollectionDecl> for DiskCollection {
    fn from(decl: &CollectionDecl) -> Self {
        let body = decl.body.as_ref().and_then(|body| {
            if body.allowed && body.fts && body.vector {
                return None;
            }
            Some(DiskBody {
                allowed: (!body.allowed).then_some(false),
                exclude_fts: !body.fts,
                exclude_vector: !body.vector,
            })
        });
        let segment_max_bytes = decl
            .segment_max_bytes
            .filter(|cap| decl.kind == Kind::Journal && *cap != DEFAULT_SEGMENT_MAX_BYTES);
        Self {
            name: decl.name.clone(),
            kind: decl.kind.as_str(),
            version: decl.version,
            fields: decl.fields.iter().map(DiskField::from).collect(),
            segment_max_bytes,
            segment_max_lines: decl.segment_max_lines,
            daily: (decl.segment_rotate == Some(SegmentRotate::Daily)).then_some(DiskUnit {}),
            singleton: decl.singleton_id.clone().map(|id| DiskSingleton { id }),
            body,
            isolated: decl.isolated,
        }
    }
}

impl From<&FieldDecl> for DiskField {
    fn from(field: &FieldDecl) -> Self {
        let text = || DiskText {
            exclude_fts: !field.fts,
            exclude_vector: !field.vector,
        };
        let mut disk = DiskField {
            name: field.name.clone(),
            required: field.required,
            nullable: field.nullable,
            exclude_find: !field.filter,
            default: field.default.clone(),
            string: None,
            integer: None,
            number: None,
            boolean: None,
            string_list: None,
            object: None,
            object_list: None,
            edge: None,
        };
        if let Some(edge) = &field.edge {
            disk.edge = Some(DiskEdge {
                collection: edge.collection.clone(),
                one: (edge.multiplicity == Multiplicity::One).then_some(DiskUnit {}),
                many: (edge.multiplicity == Multiplicity::Many).then_some(DiskUnit {}),
                dangling: edge.dangling,
                exclude_fts: !edge.fts,
                exclude_vector: !edge.vector,
            });
            return disk;
        }
        match field.type_ {
            FieldType::String => disk.string = Some(text()),
            FieldType::Integer => disk.integer = Some(DiskUnit {}),
            FieldType::Number => disk.number = Some(DiskUnit {}),
            FieldType::Boolean => disk.boolean = Some(DiskUnit {}),
            FieldType::StringList => disk.string_list = Some(text()),
            FieldType::Object => {
                disk.object = Some(disk_object(field));
            }
            FieldType::ObjectList => {
                disk.object_list = Some(disk_object(field));
            }
        }
        disk
    }
}

fn disk_object(field: &FieldDecl) -> DiskObject {
    DiskObject {
        fields: field.fields.iter().map(DiskField::from).collect(),
        values: field
            .values
            .as_deref()
            .map(|v| Box::new(DiskField::from(v))),
    }
}

fn posix_rel(vault: &Path, path: &Path) -> Result<String, Error> {
    let rel = path
        .strip_prefix(vault)
        .map_err(|_| Error::invalid("schema path is outside the Vault"))?;
    let mut out = String::new();
    for component in rel.iter() {
        let part = component
            .to_str()
            .ok_or_else(|| Error::invalid("schema path is not UTF-8"))?;
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(part);
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

fn add_body_find_key(body: Option<&BodyDecl>, keys: &mut BTreeSet<String>) {
    if body.is_some_and(|body| body.allowed && body.fts) {
        keys.insert(String::from("body"));
    }
}

fn read_utf8(path: &Path) -> Result<String, Error> {
    let bytes = fs::read(path).context("[Schema::read_utf8] read file")?;
    String::from_utf8(bytes).context(Code::NotUtf8, "[Schema::read_utf8] decode UTF-8")
}

/// On-disk directory for a collection: the collection name.
#[must_use]
pub(crate) fn collection_dir(name: &str) -> &str {
    name
}

pub(crate) fn join_in_collection(vault: &Path, name: &str, rel: &str) -> Result<PathBuf, Error> {
    let dir = collection_dir(name);
    if rel.is_empty() {
        join_inside(vault, dir)
    } else {
        join_inside(vault, &format!("{dir}/{rel}"))
    }
}

pub(crate) fn join_inside(vault: &Path, rel: &str) -> Result<PathBuf, Error> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        bail!(Error::invalid("path must be relative to the Vault"));
    }
    let mut out = vault.to_path_buf();
    for component in rel_path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => out.push(part),
            Component::ParentDir => {
                if !out.pop() || out.as_path() == Path::new("") || !out.starts_with(vault) {
                    bail!(Error::invalid("path must stay inside the Vault"));
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                bail!(Error::invalid("path must be relative to the Vault"));
            }
        }
    }
    if !out.starts_with(vault) {
        bail!(Error::invalid("path must stay inside the Vault"));
    }
    Ok(out)
}

pub(crate) fn check_strict_edges(
    schema: &Schema,
    decl: &CollectionDecl,
    record: &Record,
    lookup: impl Fn(&str, &str) -> Result<Record, Error>,
) -> Result<(), Error> {
    let fields = store::fields_map(record)?;
    check_strict_fields(&decl.fields, &fields, schema, &lookup)
}

fn check_strict_fields(
    decls: &[FieldDecl],
    values: &serde_json::Map<String, serde_json::Value>,
    schema: &Schema,
    lookup: &impl Fn(&str, &str) -> Result<Record, Error>,
) -> Result<(), Error> {
    for field in decls {
        if let Some(edge) = &field.edge
            && edge.dangling == Dangling::Strict
        {
            if schema.get(&edge.collection).is_none() {
                bail!(Error::invalid(format!(
                    "edge {} targets unknown collection {}",
                    field.name, edge.collection
                )));
            }
            for id in edge_target_ids(values.get(&field.name)) {
                match lookup(&edge.collection, &id) {
                    Ok(_) => {}
                    Err(err) if err.known_code() == Some(crate::Code::NotFound) => {
                        bail!(Error::invalid(format!(
                            "strict edge {} target {id} is missing",
                            field.name
                        )));
                    }
                    Err(err) => bail!(err),
                }
            }
        }
        match values.get(&field.name) {
            Some(serde_json::Value::Object(map)) => {
                check_strict_fields(&field.fields, map, schema, lookup)?;
            }
            Some(serde_json::Value::Array(items)) => {
                for item in items {
                    if let Some(map) = item.as_object() {
                        check_strict_fields(&field.fields, map, schema, lookup)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn edge_target_ids(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::String(id)) if !id.is_empty() => vec![id.clone()],
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{FieldType, Kind, finish_collection};
    use crate::Code;
    use std::path::Path;

    fn parse(yaml: &str) -> Result<super::CollectionDecl, crate::Error> {
        let raw: super::RawCollection =
            yaml_serde::from_str(yaml).map_err(|err| crate::Error::invalid(err.to_string()))?;
        finish_collection(raw, Path::new(".schema.yaml"))
    }

    #[test]
    fn collection_name_must_be_a_path_component() {
        let err = parse("name: team/notes\nkind: document\nversion: 1\nfields: []\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        let decl = parse("name: team-notes\nkind: document\nversion: 1\nfields: []\n").unwrap();
        assert_eq!(decl.name, "team-notes");
        assert_eq!(super::collection_dir("team-notes"), "team-notes");
    }

    #[test]
    fn singleton_id_must_be_a_valid_record_id() {
        let err = parse(
            "name: box\nkind: singleton\nversion: 1\nsingleton:\n  id: .hidden\nfields: []\n",
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
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
    fn missing_version_is_invalid() {
        let err = yaml_serde::from_str::<super::RawCollection>(include_str!(
            "../testdata/schema/missing_version.yaml"
        ))
        .expect_err("version is required");
        let _ = err;
    }

    #[test]
    fn leftover_version_alias_is_invalid() {
        let err = yaml_serde::from_str::<super::RawCollection>(include_str!(
            "../testdata/schema/unknown_collection_key.yaml"
        ))
        .expect_err("unknown key");
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
    fn layout_key_is_invalid() {
        let err = yaml_serde::from_str::<super::RawCollection>(
            "name: c\nkind: document\nversion: 1\nlayout:\n  - '{id}.md'\nfields: []\n",
        )
        .expect_err("layout is unknown");
        let _ = err;
    }

    #[test]
    fn field_named_id_is_invalid() {
        let err = parse(include_str!("../testdata/schema/slug.yaml")).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn field_named_body_is_invalid() {
        let err =
            parse("name: c\nkind: document\nversion: 1\nfields:\n  - name: body\n    string: {}\n")
                .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn exclude_fts_body_is_not_a_find_key() {
        let decl = parse(
            "name: c\nkind: document\nversion: 1\nbody:\n  exclude_fts: true\nfields:\n  - name: title\n    string: {}\n",
        )
        .unwrap();
        assert!(!decl.filter_keys.contains("body"));
        assert!(decl.body.as_ref().is_some_and(|body| !body.fts));
    }

    #[test]
    fn filter_marks_parse() {
        let decl = parse(include_str!("../testdata/schema/filter.yaml")).unwrap();
        assert!(decl.filter_keys.contains("tagged"));
        assert!(decl.filter_keys.contains("body"));
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
            "name: c\nkind: document\nversion: 1\nbody: {}\nfields:\n  - name: title\n    string: {}\n  - name: secret\n    string:\n      exclude_fts: true\n      exclude_vector: true\n",
        )
        .unwrap();
        let body = decl.body.as_ref().unwrap();
        assert!(body.allowed);
        assert!(body.fts);
        assert!(body.vector);
        #[cfg(feature = "search")]
        assert_eq!(body.field_id, "body");
        assert!(decl.filter_keys.contains("body"));
        assert!(decl.fields[0].fts);
        assert!(decl.fields[0].vector);
        #[cfg(feature = "search")]
        assert_eq!(decl.fields[0].field_id.as_deref(), Some("title"));
        assert!(!decl.fields[1].fts);
        assert!(!decl.fields[1].vector);
    }

    #[test]
    fn fts_on_integer_is_invalid() {
        let err = parse(
            "name: c\nkind: document\nversion: 1\nfields:\n  - name: n\n    integer: {}\n    fts: true\n",
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn body_on_journal_is_invalid() {
        let err = parse("name: c\nkind: journal\nversion: 1\nbody: {}\nfields: []\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn edge_arity_is_required() {
        let err = parse(
            "name: c\nkind: document\nversion: 1\nfields:\n  - name: refs\n    edge:\n      collection: c\n",
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn isolated_mark_parses() {
        let decl = parse(
            "name: c\nkind: document\nversion: 1\nisolated: true\nfields:\n  - name: secret\n    string: {}\n",
        )
        .unwrap();
        assert!(decl.isolated);
        assert_eq!(decl.fields[0].name, "secret");
    }

    #[test]
    fn object_values_parses_and_is_not_a_find_key() {
        let decl = parse(
            "name: c\nkind: document\nversion: 1\nfields:\n  - name: env\n    object:\n      values:\n        name: axis\n        object:\n          fields:\n            - name: significant\n              integer: {}\n",
        )
        .unwrap();
        let env = &decl.fields[0];
        assert_eq!(env.name, "env");
        let values = env.values.as_ref().expect("values");
        assert_eq!(values.name, "axis");
        assert_eq!(values.type_, FieldType::Object);
        assert_eq!(values.fields[0].name, "significant");
        assert!(decl.filter_keys.contains("env"));
        assert!(decl.filter_keys.contains("body"));
        assert!(!decl.filter_keys.contains("axis"));
        assert!(!decl.filter_keys.contains("significant"));
        assert!(!decl.filter_keys.contains("prod"));
    }

    #[test]
    fn object_fields_and_values_together_is_invalid() {
        let err = parse(
            "name: c\nkind: document\nversion: 1\nfields:\n  - name: env\n    object:\n      fields:\n        - name: prod\n          string: {}\n      values:\n        name: axis\n        integer: {}\n",
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.to_string().contains("both fields and values"));
    }

    #[test]
    fn object_values_refuses_required_default_exclude_find_and_edge() {
        for (yaml, needle) in [
            (
                "name: c\nkind: document\nversion: 1\nfields:\n  - name: env\n    object:\n      values:\n        name: axis\n        required: true\n        integer: {}\n",
                "must not set required",
            ),
            (
                "name: c\nkind: document\nversion: 1\nfields:\n  - name: env\n    object:\n      values:\n        name: axis\n        default: 1\n        integer: {}\n",
                "must not set default",
            ),
            (
                "name: c\nkind: document\nversion: 1\nfields:\n  - name: env\n    object:\n      values:\n        name: axis\n        exclude_find: true\n        integer: {}\n",
                "must not set exclude_find",
            ),
            (
                "name: c\nkind: document\nversion: 1\nfields:\n  - name: env\n    object:\n      values:\n        name: axis\n        edge:\n          collection: c\n          one: {}\n",
                "must not set edge",
            ),
        ] {
            let err = parse(yaml).unwrap_err();
            assert_eq!(err.known_code(), Some(Code::Invalid));
            assert!(err.to_string().contains(needle), "{err}");
        }
    }

    #[test]
    fn object_values_refuses_an_edge_nested_under_the_value_type() {
        let err = parse(
            "name: c\nkind: document\nversion: 1\nfields:\n  - name: env\n    object:\n      values:\n        name: axis\n        object:\n          fields:\n            - name: owner\n              edge:\n                collection: c\n                one: {}\n",
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.to_string().contains("owner"), "{err}");
        assert!(err.to_string().contains("must not set edge"), "{err}");
    }

    #[test]
    fn object_values_round_trips_through_proto() {
        let decl = parse(
            "name: c\nkind: document\nversion: 1\nfields:\n  - name: env\n    object:\n      values:\n        name: axis\n        integer: {}\n",
        )
        .unwrap();
        let proto = super::to_proto_schema(&decl, false).unwrap();
        let json = serde_json::to_value(&proto).unwrap();
        assert_eq!(json["fields"][0]["object"]["values"]["name"], "axis");
        assert!(
            json["fields"][0]["object"]["values"]
                .get("integer")
                .is_some()
        );
        assert!(json["fields"][0]["object"].get("fields").is_none());
        let back = super::decl_from_proto("c", &proto).unwrap();
        assert_eq!(back.fields[0].values.as_ref().unwrap().name, "axis");
        assert_eq!(
            back.fields[0].values.as_ref().unwrap().type_,
            FieldType::Integer
        );
    }

    #[test]
    fn object_values_takes_id_as_a_label_but_not_as_a_nested_key() {
        let decl = parse(
            "name: c\nkind: document\nversion: 1\nfields:\n  - name: env\n    object:\n      values:\n        name: id\n        integer: {}\n",
        )
        .unwrap();
        assert_eq!(decl.fields[0].values.as_ref().unwrap().name, "id");
        let proto = super::to_proto_schema(&decl, false).unwrap();
        let back = super::decl_from_proto("c", &proto).unwrap();
        assert_eq!(back.fields[0].values.as_ref().unwrap().name, "id");

        let err = parse(
            "name: c\nkind: document\nversion: 1\nfields:\n  - name: env\n    object:\n      values:\n        name: axis\n        object:\n          fields:\n            - name: body\n              string: {}\n",
        )
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.to_string().contains("reserved"), "{err}");
    }
}
