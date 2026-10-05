//! Collection declarations loaded from Vault files.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Code, Context, ContextAs, ContextIo, bail};
use crate::id::require_slug;
use crate::json::collection_info_from_value;
use crate::store;
use crate::{CollectionInfo, Error, Journal, Schema, Singleton, Unit, VaultInfo};

const MIB: u64 = 1024 * 1024;
pub(crate) const DEFAULT_SEGMENT_MAX_BYTES: u64 = 8 * MIB;
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
    /// Append-only `{id}: {payload}` segments; `replace` and `remove` are `unsupported`.
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

/// Loaded collection declarations, keyed by name. Not the proto `Schema`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Decls {
    collections: BTreeMap<String, CollectionDecl>,
}

impl Decls {
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

pub(crate) fn load(vault: &Path) -> Result<Decls, Error> {
    let mut collections = BTreeMap::new();
    let entries = match fs::read_dir(vault) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Decls::default());
        }
        Err(err) => {
            return Err(err).context("[Decls::load] read vault");
        }
    };
    for entry in entries {
        let entry = entry.context("[Decls::load] read vault entry")?;
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
        insert_decl(&mut collections, &dir_name, decl, &decl_path)?;
    }
    Ok(Decls { collections })
}

fn insert_decl(
    collections: &mut BTreeMap<String, CollectionDecl>,
    dir_name: &str,
    decl: CollectionDecl,
    decl_path: &Path,
) -> Result<(), Error> {
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
    Ok(())
}

/// Collection declarations as committed in `commit` of the repository at
/// `repo`, with the same rules as [`load`] on a work tree: a leftover
/// `.schema.*` file, two declaration files in one directory, or a
/// directory that is not the collection name is `invalid`.
#[cfg(feature = "find")]
pub(crate) fn load_at(repo: &Path, commit: &str) -> Result<Decls, Error> {
    let dirs: Vec<String> = crate::git::tree::root_dirs(repo, commit)?
        .into_iter()
        .filter(|name| is_collection_dir_name(name) && !name.contains('\n'))
        .collect();
    let names: Vec<&str> = COLLECTION_FILENAMES
        .iter()
        .chain(LEFTOVER_SCHEMA_FILENAMES.iter())
        .copied()
        .collect();
    let specs: Vec<String> = dirs
        .iter()
        .flat_map(|dir| {
            names
                .iter()
                .map(move |name| format!("{commit}:{dir}/{name}"))
        })
        .collect();
    let blobs = crate::git::tree::read_blobs(repo, &specs)?;
    let mut collections = BTreeMap::new();
    for (dir, files) in dirs.iter().zip(blobs.chunks(names.len())) {
        let (found, leftover) = files.split_at(COLLECTION_FILENAMES.len());
        if let Some(i) = leftover.iter().position(Option::is_some) {
            bail!(Error::invalid(format!(
                "leftover schema file {} in {dir}",
                LEFTOVER_SCHEMA_FILENAMES[i]
            )));
        }
        let mut present = found
            .iter()
            .zip(COLLECTION_FILENAMES)
            .filter_map(|(bytes, name)| bytes.as_deref().map(|bytes| (bytes, name)));
        let Some((bytes, name)) = present.next() else {
            continue;
        };
        if present.next().is_some() {
            bail!(Error::invalid(format!(
                "collection directory {dir} has more than one declaration file"
            )));
        }
        let path = Path::new(dir).join(name);
        let text =
            std::str::from_utf8(bytes).context(Code::NotUtf8, "[Decls::load_at] decode UTF-8")?;
        let decl = load_collection_text(text, is_yaml(&path), &path)?;
        insert_decl(&mut collections, dir, decl, &path)?;
    }
    Ok(Decls { collections })
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
    load_collection_text(&text, is_yaml(path), path)
}

fn is_yaml(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext != "json")
}

fn load_collection_text(text: &str, yaml: bool, path: &Path) -> Result<CollectionDecl, Error> {
    let loc = path.display();
    let value: Value = if yaml {
        yaml_serde::from_str(text).context(Code::Invalid, "[Decls::load] parse collection YAML")?
    } else {
        serde_json::from_str(text).context(Code::Invalid, "[Decls::load] parse collection JSON")?
    };
    let info = collection_info_from_value(&value)?;
    let name = info.name.as_deref().unwrap_or("");
    require_slug("collection name", name).with_context(|| format!("schema {loc}: name"))?;
    let Some(schema) = info.schema.as_option() else {
        bail!(Error::invalid(format!("schema {loc}: schema is required")));
    };
    decl_from_proto(name, schema)
}

/// Convert a loaded collection to proto `Schema`. Never emits fields.
pub(crate) fn to_proto_schema(decl: &CollectionDecl) -> Result<Schema, Error> {
    let mut out = Schema::default();
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
    decls: &Decls,
) -> Result<VaultInfo, Error> {
    let mut info = VaultInfo::default()
        .with_path(path.to_string_lossy().into_owned())
        .with_index_path(index_path.to_string_lossy().into_owned())
        .with_auto_sync(auto_sync);
    if !remote.is_empty() {
        info = info.with_remote(remote.to_owned());
    }
    for decl in decls.collections() {
        let mut item = CollectionInfo::default()
            .with_version(1)
            .with_name(decl.name.as_str());
        item.schema = to_proto_schema(decl)?.into();
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
pub(crate) fn put_collection(vault: &Path, name: &str, incoming: &Schema) -> Result<String, Error> {
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
    load(vault)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("[Decls::put_collection] create collection dir")?;
    }
    if path.is_file() {
        let existing = load_collection_file(&path)?;
        if existing.name != decl.name {
            bail!(Error::invalid(format!(
                "schema file {rel} already declares collection {}",
                existing.name
            )));
        }
        if existing.kind != decl.kind {
            bail!(Error::invalid(format!(
                "cannot change the kind of collection {}: it is declared as {}",
                decl.name,
                existing.kind.as_str()
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
        if existing.isolated != decl.isolated && holds_record_files(vault, &existing)? {
            bail!(Error::invalid(
                "cannot set or clear isolated on a collection that already holds Record files"
            ));
        }
    }
    let mut file = CollectionInfo::default().with_version(1).with_name(name);
    file.schema = incoming.clone().into();
    let body = if rel.ends_with(".json") {
        json_from_info(&file)?
    } else {
        yaml_from_info(&file)?
    };
    store::atomic_write(&path, body.as_bytes())?;
    Ok(rel)
}

/// Whether the collection has any Record file or journal segment on disk.
fn holds_record_files(vault: &Path, decl: &CollectionDecl) -> Result<bool, Error> {
    match decl.kind {
        Kind::Document | Kind::Singleton => Ok(!store::collection_ids(vault, decl)?.is_empty()),
        Kind::Journal => crate::journal::has_segments(vault, decl),
    }
}

fn decl_from_proto(name: &str, incoming: &Schema) -> Result<CollectionDecl, Error> {
    use scheda_api::api::collection::v1::__buffa::oneof::schema::Kind as DeclKind;
    let (kind, singleton_id, segment_max_bytes, segment_max_lines, segment_rotate) =
        match incoming.kind.as_ref() {
            Some(DeclKind::Document(_)) => (Kind::Document, None, None, None, None),
            Some(DeclKind::Journal(journal)) => {
                let cap = journal
                    .segment_max_bytes
                    .unwrap_or(DEFAULT_SEGMENT_MAX_BYTES);
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

fn yaml_from_info(info: &CollectionInfo) -> Result<String, Error> {
    yaml_serde::to_string(info).context(Code::Invalid, "[Decls::put_collection] encode YAML")
}

fn json_from_info(info: &CollectionInfo) -> Result<String, Error> {
    serde_json::to_string_pretty(info).context(Code::Invalid, "[Decls::put_collection] encode JSON")
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
    let bytes = fs::read(path).context("[Decls::read_utf8] read file")?;
    String::from_utf8(bytes).context(Code::NotUtf8, "[Decls::read_utf8] decode UTF-8")
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
    use super::Kind;
    use crate::Code;
    use std::path::Path;

    fn parse(yaml: &str) -> Result<super::CollectionDecl, crate::Error> {
        super::load_collection_text(yaml, true, Path::new(".collection.yaml"))
    }

    const DOCUMENT: &str = "\
version: 1
name: c
schema:
  document: {}
";

    #[test]
    fn collection_name_must_be_a_path_component() {
        let err = parse("version: 1\nname: team/notes\nschema:\n  document: {}\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        let decl = parse("version: 1\nname: team-notes\nschema:\n  document: {}\n").unwrap();
        assert_eq!(decl.name, "team-notes");
        assert_eq!(super::collection_dir("team-notes"), "team-notes");
    }

    #[test]
    fn singleton_id_must_be_a_valid_record_id() {
        let err =
            parse("version: 1\nname: box\nschema:\n  singleton:\n    id: .hidden\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn missing_kind_is_invalid() {
        let err = parse(include_str!("../testdata/schema/missing_kind.yaml")).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn missing_version_is_invalid() {
        let err = parse(include_str!("../testdata/schema/missing_version.yaml")).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn version_2_is_invalid() {
        let err = parse("version: 2\nname: c\nschema:\n  document: {}\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn version_0_is_invalid() {
        let err = parse("version: 0\nname: c\nschema:\n  document: {}\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn leftover_version_alias_is_invalid() {
        let err = parse(include_str!(
            "../testdata/schema/unknown_collection_key.yaml"
        ))
        .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn executable_key_is_invalid() {
        let err = parse(include_str!("../testdata/schema/on_write.yaml")).unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
    }

    #[test]
    fn layout_key_is_invalid() {
        let err = parse("version: 1\nname: c\nschema:\n  document: {}\nlayout:\n  - '{id}.md'\n")
            .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("layout"));
    }

    #[test]
    fn leftover_fields_key_is_invalid() {
        let err = parse("version: 1\nname: c\nschema:\n  document: {}\nfields: []\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("fields"));
    }

    #[test]
    fn leftover_body_key_is_invalid() {
        let err = parse("version: 1\nname: c\nschema:\n  document: {}\nbody: {}\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("body"));
    }

    #[test]
    fn leftover_kind_key_is_invalid() {
        let err =
            parse("version: 1\nname: c\nkind: document\nschema:\n  document: {}\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("kind"));
    }

    #[test]
    fn leftover_declaration_key_is_invalid() {
        let err =
            parse("version: 1\nname: c\ndeclaration:\n  document: {}\nschema:\n  document: {}\n")
                .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(err.message.as_deref().unwrap_or("").contains("declaration"));
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
        let decl =
            parse("version: 1\nname: c\nschema:\n  document: {}\n  isolated: true\n").unwrap();
        assert!(decl.isolated);
    }

    #[test]
    fn encrypted_mark_parses() {
        let decl =
            parse("version: 1\nname: c\nschema:\n  document: {}\n  encrypted: true\n").unwrap();
        assert!(decl.encrypted);
        assert!(!decl.isolated);
    }

    #[test]
    fn encrypted_journal_yaml_is_invalid() {
        let err =
            parse("version: 1\nname: c\nschema:\n  journal: {}\n  encrypted: true\n").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(
            err.to_string()
                .contains("journal collections cannot be encrypted")
        );
    }

    #[test]
    fn exclude_index_marks_parse() {
        let decl = parse(
            "version: 1\nname: c\nschema:\n  document: {}\n  exclude_lexical_index: true\n  exclude_vector_index: true\n",
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
            "version: 1\nname: c\nschema:\n  document: {}\n  encrypted: true\n  exclude_lexical_index: true\n  exclude_vector_index: true\n",
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
            parse("version: 1\nname: c\nschema:\n  journal: {}\n  exclude_lexical_index: true\n")
                .unwrap();
        assert!(lexical.exclude_lexical_index);
        assert!(!lexical.exclude_vector_index);
        assert!(!lexical.lexical_indexed());
        assert!(lexical.vector_indexed());
        let vector =
            parse("version: 1\nname: c\nschema:\n  journal: {}\n  exclude_vector_index: true\n")
                .unwrap();
        assert!(vector.exclude_vector_index);
        assert!(vector.lexical_indexed());
        assert!(!vector.vector_indexed());
        let both = parse(
            "version: 1\nname: c\nschema:\n  journal: {}\n  exclude_lexical_index: true\n  exclude_vector_index: true\n",
        )
        .unwrap();
        assert!(!both.any_indexed());
    }

    #[test]
    fn document_kind_round_trips_through_proto() {
        let decl = parse(DOCUMENT).unwrap();
        let proto = super::to_proto_schema(&decl).unwrap();
        let json = serde_json::to_value(&proto).unwrap();
        assert!(json.get("document").is_some());
        assert!(json.get("fields").is_none());
        let back = super::decl_from_proto("c", &proto).unwrap();
        assert_eq!(back.kind, Kind::Document);
        assert!(!back.isolated);
    }
}
