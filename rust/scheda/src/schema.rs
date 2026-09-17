//! Collection declarations loaded from Vault files.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Code, Context, ContextAs, ContextIo, bail};
use crate::id::require_slug;
use crate::store;
use crate::{CollectionInfo, Declaration, Error, Journal, Singleton, Unit, VaultInfo};

const MIB: u64 = 1024 * 1024;
pub(crate) const DEFAULT_SEGMENT_MAX_BYTES: u64 = 8 * MIB;
pub(crate) const MAX_SEGMENT_MAX_BYTES: u64 = 32 * MIB;
const COLLECTION_YAML: &str = ".collection.yaml";
const COLLECTION_YML: &str = ".collection.yml";
const COLLECTION_JSON: &str = ".collection.json";
const COLLECTION_FILENAMES: [&str; 3] = [COLLECTION_YAML, COLLECTION_YML, COLLECTION_JSON];
const LEFTOVER_SCHEMA_FILENAMES: [&str; 3] = [".schema.yaml", ".schema.yml", ".schema.json"];

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
    /// Kind literal: `document`, `journal`, or `singleton`.
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
pub(crate) enum SegmentRotate {
    Daily,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUnit {}

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
    version: Option<u32>,
    segment_max_bytes: Option<u64>,
    segment_max_lines: Option<u64>,
    daily: Option<RawUnit>,
    singleton: Option<RawSingleton>,
    #[serde(default)]
    isolated: bool,
    #[serde(default)]
    encrypted: bool,
    #[serde(default)]
    exclude_lexical_index: bool,
    #[serde(default)]
    exclude_vector_index: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct CollectionDecl {
    pub name: String,
    pub kind: Kind,
    pub version: u32,
    pub segment_max_bytes: Option<u64>,
    pub segment_max_lines: Option<u64>,
    pub segment_rotate: Option<SegmentRotate>,
    pub singleton_id: Option<String>,
    pub isolated: bool,
    pub encrypted: bool,
    pub exclude_lexical_index: bool,
    pub exclude_vector_index: bool,
}

impl CollectionDecl {
    /// Whether Records enter the Tantivy Index (`find`, filters, BM25).
    pub(crate) const fn lexical_indexed(&self) -> bool {
        !self.encrypted && !self.exclude_lexical_index
    }

    /// Whether Records are embedded and stored in the vector Index.
    pub(crate) const fn vector_indexed(&self) -> bool {
        !self.encrypted && !self.exclude_vector_index
    }

    /// Whether either Index leg is written for this collection.
    pub(crate) const fn any_indexed(&self) -> bool {
        self.lexical_indexed() || self.vector_indexed()
    }
}

/// Loaded collection declarations, keyed by name. Not `api.v1.Declaration`.
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
        refuse_leftover_schema_file(&path)?;
        let Some(decl_path) = find_collection_file(&path)? else {
            continue;
        };
        let decl = load_collection_file(&decl_path)?;
        if decl.name != dir_name {
            bail!(Error::invalid(format!(
                "schema {}: directory {dir_name} is not the collection name {}",
                decl_path.display(),
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
    Ok(Schema { collections })
}

fn is_collection_dir_name(name: &str) -> bool {
    !name.starts_with('.')
}

fn refuse_leftover_schema_file(dir: &Path) -> Result<(), Error> {
    for name in LEFTOVER_SCHEMA_FILENAMES {
        let path = dir.join(name);
        if path.is_file() {
            bail!(Error::invalid(format!(
                "leftover schema file {name} in {}",
                dir.display()
            )));
        }
    }
    Ok(())
}

fn find_collection_file(dir: &Path) -> Result<Option<PathBuf>, Error> {
    let mut found = None;
    for name in COLLECTION_FILENAMES {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        if found.is_some() {
            bail!(Error::invalid(format!(
                "collection directory {} has more than one declaration file",
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
    let version = match raw.version {
        None => {
            bail!(Error::invalid(format!("schema {loc}: version is required")));
        }
        Some(1) => 1,
        Some(n) => {
            bail!(Error::invalid(format!(
                "schema {loc}: version {n} is not implemented"
            )));
        }
    };
    match raw.kind {
        Kind::Journal => {
            if raw.singleton.is_some() {
                bail!(Error::invalid(format!(
                    "schema {loc}: singleton is not valid on journal"
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
        }
    }
    if raw.encrypted && raw.kind == Kind::Journal {
        bail!(Error::invalid(format!(
            "schema {loc}: journal collections cannot be encrypted: an age file cannot be appended to"
        )));
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
    let singleton_id = raw.singleton.map(|s| s.id);
    Ok(CollectionDecl {
        name: raw.name,
        kind: raw.kind,
        version,
        segment_max_bytes,
        segment_max_lines,
        segment_rotate,
        singleton_id,
        isolated: raw.isolated,
        encrypted: raw.encrypted,
        exclude_lexical_index: raw.exclude_lexical_index,
        exclude_vector_index: raw.exclude_vector_index,
    })
}

/// Convert a loaded collection to `api.v1.Declaration`. Never emits fields.
pub(crate) fn to_proto_declaration(decl: &CollectionDecl) -> Result<Declaration, Error> {
    let mut out = Declaration::default();
    if decl.isolated {
        out = out.with_isolated(true);
    }
    if decl.encrypted {
        out = out.with_encrypted(true);
    }
    if decl.exclude_lexical_index {
        out = out.with_exclude_lexical_index(true);
    }
    if decl.exclude_vector_index {
        out = out.with_exclude_vector_index(true);
    }
    out.kind = match decl.kind {
        Kind::Document => Unit::default().into(),
        Kind::Journal => journal_to_proto(decl).into(),
        Kind::Singleton => {
            let id = decl.singleton_id.as_deref().unwrap_or("");
            Singleton::default().with_id(id).into()
        }
    };
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
        item.declaration = to_proto_declaration(decl)?.into();
        info.collections.push(item);
    }
    Ok(info)
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

/// Write one collection declaration. Returns the vault-relative POSIX path.
pub(crate) fn put_collection(
    vault: &Path,
    name: &str,
    incoming: &Declaration,
) -> Result<String, Error> {
    incoming.require_valid()?;
    require_slug("collection name", name)?;
    let decl = decl_from_proto(name, incoming)?;
    let dir = join_inside(vault, collection_dir(name))?;
    refuse_leftover_schema_file(&dir)?;
    let rel = match find_collection_file(&dir)? {
        Some(path) => posix_rel(vault, &path)?,
        None => format!("{}/{COLLECTION_YAML}", collection_dir(name)),
    };
    let path = join_inside(vault, &rel)?;
    let mut schema = load(vault)?;
    schema.collections.insert(decl.name.clone(), decl.clone());
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
        if existing.encrypted != decl.encrypted
            && matches!(existing.kind, Kind::Document | Kind::Singleton)
            && !store::collection_ids(vault, &existing)?.is_empty()
        {
            bail!(Error::invalid(
                "cannot set or clear encrypted on a collection that already holds Record files"
            ));
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

fn decl_from_proto(name: &str, incoming: &Declaration) -> Result<CollectionDecl, Error> {
    use scheda_generated::api::v1::__buffa::oneof::declaration::Kind as DeclKind;
    let (kind, singleton_id, segment_max_bytes, segment_max_lines, segment_rotate) =
        match incoming.kind.as_ref() {
            Some(DeclKind::Document(_)) => (Kind::Document, None, None, None, None),
            Some(DeclKind::Journal(journal)) => {
                let cap = journal
                    .segment_max_bytes
                    .unwrap_or(DEFAULT_SEGMENT_MAX_BYTES);
                if cap == 0 || cap > MAX_SEGMENT_MAX_BYTES {
                    bail!(Error::invalid("schema: segment_max_bytes out of range"));
                }
                (
                    Kind::Journal,
                    None,
                    Some(cap),
                    journal.segment_max_lines,
                    journal.daily.as_option().map(|_| SegmentRotate::Daily),
                )
            }
            Some(DeclKind::Singleton(singleton)) => {
                let id = singleton
                    .id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| Error::invalid("schema: singleton.id is required"))?;
                require_slug("singleton id", id)?;
                (Kind::Singleton, Some(id.to_owned()), None, None, None)
            }
            None => bail!(Error::invalid("schema: kind is required")),
        };
    let decl = CollectionDecl {
        name: name.to_owned(),
        kind,
        version: 1,
        segment_max_bytes,
        segment_max_lines,
        segment_rotate,
        singleton_id,
        isolated: incoming.isolated.unwrap_or(false),
        encrypted: incoming.encrypted.unwrap_or(false),
        exclude_lexical_index: incoming.exclude_lexical_index.unwrap_or(false),
        exclude_vector_index: incoming.exclude_vector_index.unwrap_or(false),
    };
    if decl.encrypted && decl.kind == Kind::Journal {
        bail!(Error::invalid(
            "journal collections cannot be encrypted: an age file cannot be appended to"
        ));
    }
    Ok(decl)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    segment_max_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    segment_max_lines: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daily: Option<DiskUnit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    singleton: Option<DiskSingleton>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    isolated: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    encrypted: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    exclude_lexical_index: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    exclude_vector_index: bool,
}

#[derive(Serialize)]
struct DiskUnit {}

#[derive(Serialize)]
struct DiskSingleton {
    id: String,
}

impl From<&CollectionDecl> for DiskCollection {
    fn from(decl: &CollectionDecl) -> Self {
        let segment_max_bytes = decl
            .segment_max_bytes
            .filter(|cap| decl.kind == Kind::Journal && *cap != DEFAULT_SEGMENT_MAX_BYTES);
        Self {
            name: decl.name.clone(),
            kind: decl.kind.as_str(),
            version: decl.version,
            segment_max_bytes,
            segment_max_lines: decl.segment_max_lines,
            daily: (decl.segment_rotate == Some(SegmentRotate::Daily)).then_some(DiskUnit {}),
            singleton: decl.singleton_id.clone().map(|id| DiskSingleton { id }),
            isolated: decl.isolated,
            encrypted: decl.encrypted,
            exclude_lexical_index: decl.exclude_lexical_index,
            exclude_vector_index: decl.exclude_vector_index,
        }
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

#[cfg(test)]
mod tests {
    use super::{Kind, finish_collection};
    use crate::Code;
    use std::path::Path;

    fn parse(yaml: &str) -> Result<super::CollectionDecl, crate::Error> {
        let raw: super::RawCollection =
            yaml_serde::from_str(yaml).map_err(|err| crate::Error::invalid(err.to_string()))?;
        finish_collection(raw, Path::new(".collection.yaml"))
    }

    #[test]
    fn collection_name_must_be_a_path_component() {
        let err = parse("name: team/notes\nkind: document\nversion: 1\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        let decl = parse("name: team-notes\nkind: document\nversion: 1\n").unwrap();
        assert_eq!(decl.name, "team-notes");
        assert_eq!(super::collection_dir("team-notes"), "team-notes");
    }

    #[test]
    fn singleton_id_must_be_a_valid_record_id() {
        let err = parse("name: box\nkind: singleton\nversion: 1\nsingleton:\n  id: .hidden\n")
            .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
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
        let err = parse(include_str!("../testdata/schema/missing_version.yaml")).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(
            err.message
                .as_deref()
                .unwrap_or("")
                .contains("version is required")
        );
    }

    #[test]
    fn version_2_is_invalid() {
        let err = parse("name: c\nkind: document\nversion: 2\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(
            err.message
                .as_deref()
                .unwrap_or("")
                .contains("version 2 is not implemented")
        );
    }

    #[test]
    fn version_0_is_invalid() {
        let err = parse("name: c\nkind: document\nversion: 0\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
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
            "name: c\nkind: document\nversion: 1\nlayout:\n  - '{id}.md'\n",
        )
        .expect_err("layout is unknown");
        let _ = err;
    }

    #[test]
    fn leftover_fields_key_is_invalid() {
        let err = yaml_serde::from_str::<super::RawCollection>(
            "name: c\nkind: document\nversion: 1\nfields: []\n",
        )
        .expect_err("fields is unknown");
        let _ = err;
    }

    #[test]
    fn leftover_body_key_is_invalid() {
        let err = yaml_serde::from_str::<super::RawCollection>(
            "name: c\nkind: document\nversion: 1\nbody: {}\n",
        )
        .expect_err("body is unknown");
        let _ = err;
    }

    #[test]
    fn leftover_document_mapping_key_is_invalid() {
        let err = yaml_serde::from_str::<super::RawCollection>(
            "name: c\nkind: document\nversion: 1\ndocument: {}\n",
        )
        .expect_err("document is unknown");
        let _ = err;
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
    fn isolated_mark_parses() {
        let decl = parse("name: c\nkind: document\nversion: 1\nisolated: true\n").unwrap();
        assert!(decl.isolated);
    }

    #[test]
    fn encrypted_mark_parses() {
        let decl = parse("name: c\nkind: document\nversion: 1\nencrypted: true\n").unwrap();
        assert!(decl.encrypted);
        assert!(!decl.isolated);
    }

    #[test]
    fn encrypted_journal_yaml_is_invalid() {
        let err = parse("name: c\nkind: journal\nversion: 1\nencrypted: true\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(
            err.to_string()
                .contains("journal collections cannot be encrypted")
        );
    }

    #[test]
    fn exclude_index_marks_parse() {
        let decl = parse(
            "name: c\nkind: document\nversion: 1\nexclude_lexical_index: true\nexclude_vector_index: true\n",
        )
        .unwrap();
        assert!(decl.exclude_lexical_index);
        assert!(decl.exclude_vector_index);
        assert!(!decl.lexical_indexed());
        assert!(!decl.vector_indexed());
        assert!(!decl.any_indexed());
    }

    #[test]
    fn exclude_index_flags_on_encrypted_document_are_allowed() {
        let decl = parse(
            "name: c\nkind: document\nversion: 1\nencrypted: true\nexclude_lexical_index: true\nexclude_vector_index: true\n",
        )
        .unwrap();
        assert!(decl.encrypted);
        assert!(decl.exclude_lexical_index);
        assert!(decl.exclude_vector_index);
        assert!(!decl.any_indexed());
    }

    #[test]
    fn journal_may_set_either_exclude_index_flag() {
        let lexical =
            parse("name: c\nkind: journal\nversion: 1\nexclude_lexical_index: true\n").unwrap();
        assert!(lexical.exclude_lexical_index);
        assert!(!lexical.exclude_vector_index);
        assert!(!lexical.lexical_indexed());
        assert!(lexical.vector_indexed());
        let vector =
            parse("name: c\nkind: journal\nversion: 1\nexclude_vector_index: true\n").unwrap();
        assert!(vector.exclude_vector_index);
        assert!(vector.lexical_indexed());
        assert!(!vector.vector_indexed());
        let both = parse(
            "name: c\nkind: journal\nversion: 1\nexclude_lexical_index: true\nexclude_vector_index: true\n",
        )
        .unwrap();
        assert!(!both.any_indexed());
    }

    #[test]
    fn document_kind_round_trips_through_proto() {
        let decl = parse("name: c\nkind: document\nversion: 1\n").unwrap();
        let proto = super::to_proto_declaration(&decl).unwrap();
        let json = serde_json::to_value(&proto).unwrap();
        assert!(json.get("document").is_some());
        assert!(json.get("fields").is_none());
        let back = super::decl_from_proto("c", &proto).unwrap();
        assert_eq!(back.kind, Kind::Document);
        assert!(!back.isolated);
    }
}
