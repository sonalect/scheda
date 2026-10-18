//! Collection declarations: the file `{vault}/{name}/.collection.yaml` (also
//! `.yml` or `.json`) of every collection, read from the commit `HEAD`
//! names, never from the work tree.
//!
//! The declaration in memory is the contract's `Collection`, normalized when
//! it is loaded so that every default is filled in, kept together with the
//! unknown fields of its file and the file's name and format, so that a
//! later rewrite changes only what it means to change.

// The layers that write collections and index them call the rest.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the layers that write collections and Documents are not built yet"
    )
)]

use std::collections::BTreeMap;
use std::path::Path;

use scheda_api::v1::schema::Kind as SchemaKind;
use scheda_api::v1::{CollectionDeclaration, Journal, VaultErrorCode};

use crate::error::{Context, Error, Fault};
use crate::id::require_slug;
use crate::repo::{Child, NodeKind, Repo, Snap};
use crate::stored::{self, Format, Unknowns};

const MIB: u64 = 1024 * 1024;

/// Bytes per journal segment when the declaration sets no limit.
pub(crate) const DEFAULT_SEGMENT_MAX_BYTES: u64 = 8 * MIB;

const COLLECTION_FILENAMES: [&str; 3] = [".collection.yaml", ".collection.yml", ".collection.json"];
const LEFTOVER_SCHEMA_FILENAMES: [&str; 3] = [".schema.yaml", ".schema.yml", ".schema.json"];

/// The kind of a collection, without its payload: the arm of the `oneof`
/// of `Schema`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    /// A Document per file, replaced and deleted as a whole.
    Standard,
    /// Documents appended as lines to segment files.
    Journal,
    /// One Document with an `id` declared in the collection.
    Singleton,
}

/// The file a declaration was read from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeclFile {
    /// Vault-relative POSIX path, such as `notes/.collection.yaml`.
    pub(crate) rel: String,
    /// How the file is written.
    pub(crate) format: Format,
}

/// One collection as declared: the normalized `Collection`, the unknown
/// fields of its file, and the file.
#[derive(Clone, Debug)]
pub(crate) struct Declaration {
    /// The message, with every default filled in.
    pub(crate) collection: CollectionDeclaration,
    /// What the file had that this version does not know.
    pub(crate) unknown: Unknowns,
    /// The file it came from.
    pub(crate) file: DeclFile,
    kind: Kind,
}

impl Declaration {
    /// The collection name, which is also its directory.
    pub(crate) fn name(&self) -> &str {
        self.collection.name.as_deref().unwrap_or("")
    }

    /// The kind of the collection.
    pub(crate) fn kind(&self) -> Kind {
        self.kind
    }

    /// The `id` of the one Document of a `singleton` collection.
    pub(crate) fn singleton_id(&self) -> Option<&str> {
        match self.collection.schema.as_option()?.kind.as_ref()? {
            SchemaKind::Singleton(singleton) => singleton.id.as_deref(),
            SchemaKind::Standard(_) | SchemaKind::Journal(_) => None,
        }
    }

    /// The journal limits of a `journal` collection; the byte limit is
    /// always set.
    pub(crate) fn journal(&self) -> Option<&Journal> {
        match self.collection.schema.as_option()?.kind.as_ref()? {
            SchemaKind::Journal(journal) => Some(journal),
            SchemaKind::Standard(_) | SchemaKind::Singleton(_) => None,
        }
    }

    /// The repository the Documents live in.
    pub(crate) fn repo(&self) -> Repo {
        if self.isolated() {
            Repo::Isolated
        } else {
            Repo::Main
        }
    }

    /// Whether the Documents live in the isolated repository.
    pub(crate) fn isolated(&self) -> bool {
        self.collection
            .schema
            .as_option()
            .and_then(|schema| schema.isolated)
            .unwrap_or(false)
    }

    /// Whether the Document files are age armor.
    pub(crate) fn encrypted(&self) -> bool {
        self.collection
            .schema
            .as_option()
            .and_then(|schema| schema.encrypted)
            .unwrap_or(false)
    }

    /// Whether Documents enter the lexical Index.
    pub(crate) fn lexical_indexed(&self) -> bool {
        let excluded = self
            .collection
            .schema
            .as_option()
            .and_then(|schema| schema.exclude_lexical_index)
            .unwrap_or(false);
        !self.encrypted() && !excluded
    }

    /// Whether Documents are embedded and enter the vector Index.
    pub(crate) fn vector_indexed(&self) -> bool {
        let excluded = self
            .collection
            .schema
            .as_option()
            .and_then(|schema| schema.exclude_vector_index)
            .unwrap_or(false);
        !self.encrypted() && !excluded
    }
}

/// The collection whose declaration file `path` (Vault-relative, POSIX) is:
/// `{name}/.collection.yaml`, `.yml`, or `.json` directly in a root
/// directory whose name does not start with `.`.
pub(crate) fn declaration_dir(path: &str) -> Option<&str> {
    let (dir, file) = path.split_once('/')?;
    (!dir.starts_with('.') && COLLECTION_FILENAMES.contains(&file)).then_some(dir)
}

/// The declarations of a Vault, by collection name.
#[derive(Clone, Debug, Default)]
pub(crate) struct Declarations {
    collections: BTreeMap<String, Declaration>,
}

impl Declarations {
    /// The declaration of collection `name`.
    pub(crate) fn get(&self, name: &str) -> Option<&Declaration> {
        self.collections.get(name)
    }

    /// Every declaration, in name order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Declaration> {
        self.collections.values()
    }

    /// How many collections are declared.
    pub(crate) fn len(&self) -> usize {
        self.collections.len()
    }

    /// Declares `declaration`, replacing a declaration of the same name.
    pub(crate) fn insert(&mut self, declaration: Declaration) {
        self.collections
            .insert(declaration.name().to_owned(), declaration);
    }

    /// Forgets the declaration of collection `name`.
    pub(crate) fn remove(&mut self, name: &str) {
        self.collections.remove(name);
    }
}

/// Reads the declarations of the commit of `snap`.
///
/// Every directory at the root whose name does not start with `.` and that
/// holds one declaration file is a collection. A directory with two
/// declaration files, a leftover `.schema.*` file, a file that is not a
/// valid `Collection`, or a directory name other than the collection's
/// `name` is `INVALID_ARGUMENT`, with the path of the file; one bad file
/// fails the whole load. A commit with no tree (an unborn branch) has no
/// declarations.
pub(crate) fn load(snap: &Snap) -> Result<Declarations, Error> {
    let mut collections = BTreeMap::new();
    let Some(entries) = snap.list("").context("read the root of the Vault")? else {
        return Ok(Declarations::default());
    };
    for entry in entries {
        if entry.node.kind != NodeKind::Dir || entry.name.starts_with('.') {
            continue;
        }
        let dir_name = entry.name;
        let files = snap
            .list(&dir_name)
            .map_err(|fault| fault.with_path(dir_name.clone()))
            .context("read a collection directory")?
            .unwrap_or_default();
        refuse_leftover_schema_file(&files, &dir_name)?;
        let Some(file_name) = find_declaration_file(&files, &dir_name)? else {
            continue;
        };
        let rel = format!("{dir_name}/{file_name}");
        let bytes = snap
            .read(&rel)
            .map_err(|fault| fault.with_path(rel.clone()))
            .context("read a collection file")?
            .map(|blob| blob.bytes)
            .unwrap_or_default();
        let declaration = declare(&bytes, &rel, &dir_name)
            .map_err(|fault| fault.with_path(rel))
            .context("load a collection")?;
        collections.insert(dir_name, declaration);
    }
    Ok(Declarations { collections })
}

/// Encodes `collection` as the bytes of a declaration file in `format`,
/// with the `unknown` fields of the file it replaces put back.
pub(crate) fn encode(
    collection: &CollectionDeclaration,
    unknown: &Unknowns,
    format: Format,
) -> Result<Vec<u8>, Error> {
    stored::encode(collection, unknown, format)
        .map_err(Error::from)
        .context("encode a collection file")
}

/// The declaration file a new collection `name` gets:
/// `{name}/.collection.yaml`.
pub(crate) fn new_file(name: &str) -> DeclFile {
    DeclFile {
        rel: format!("{name}/{}", COLLECTION_FILENAMES[0]),
        format: Format::Yaml,
    }
}

/// Checks a `Collection` a caller sends, before anything is locked or
/// written: the rules of the contract, the slug of the name and of a
/// singleton `id`. Reads nothing.
pub(crate) fn check(collection: &CollectionDeclaration) -> Result<(), Fault> {
    stored::validate(collection)?;
    require_slug("collection name", collection.name.as_deref().unwrap_or(""))?;
    let mut copy = collection.clone();
    normalize(&mut copy)?;
    Ok(())
}

/// The bytes of the declaration file `file` for `collection`, with the
/// `unknown` fields of the file it replaces put back, and the declaration
/// as a later load of those bytes finds it, defaults filled in. The bytes
/// hold what the caller declared and no default.
pub(crate) fn declare_new(
    collection: &CollectionDeclaration,
    unknown: &Unknowns,
    file: DeclFile,
) -> Result<(Vec<u8>, Declaration), Error> {
    let bytes = encode(collection, unknown, file.format)?;
    let name = collection.name.as_deref().unwrap_or("");
    let declaration = declare(&bytes, &file.rel, name)
        .map_err(|fault| fault.with_path(file.rel.clone()))
        .context("declare a collection")?;
    Ok((bytes, declaration))
}

/// Reads and normalizes one declaration file: `bytes` is the file at the
/// Vault-relative path `rel`, which must be in the directory `dir_name`.
fn declare(bytes: &[u8], rel: &str, dir_name: &str) -> Result<Declaration, Fault> {
    let format = Format::of_path(Path::new(rel)).unwrap_or(Format::Yaml);
    let stored::Stored {
        mut message,
        unknown,
    } = stored::read::<CollectionDeclaration>(bytes, format)?;
    let name = message.name.as_deref().unwrap_or("");
    require_slug("collection name", name)?;
    if name != dir_name {
        return Err(Fault::new(
            VaultErrorCode::InvalidArgument,
            format!("the directory {dir_name} is not the collection name {name}"),
        ));
    }
    let kind = normalize(&mut message)?;
    Ok(Declaration {
        collection: message,
        unknown,
        file: DeclFile {
            rel: rel.to_owned(),
            format,
        },
        kind,
    })
}

/// Fills in the defaults of `collection` and returns its kind. The journal
/// byte limit is the only default with a value; every other unset field
/// means "off" and stays unset. The checks that need the library (the slug
/// of a singleton `id`) run here; the rules of the contract already ran.
fn normalize(collection: &mut CollectionDeclaration) -> Result<Kind, Fault> {
    let no_kind = || Fault::new(VaultErrorCode::InvalidArgument, "the schema names no kind");
    let schema = collection.schema.as_option_mut().ok_or_else(no_kind)?;
    match schema.kind.as_mut().ok_or_else(no_kind)? {
        SchemaKind::Standard(_) => Ok(Kind::Standard),
        SchemaKind::Journal(journal) => {
            journal
                .segment_max_bytes
                .get_or_insert(DEFAULT_SEGMENT_MAX_BYTES);
            Ok(Kind::Journal)
        }
        SchemaKind::Singleton(singleton) => {
            require_slug("singleton id", singleton.id.as_deref().unwrap_or(""))?;
            Ok(Kind::Singleton)
        }
    }
}

/// Whether `files` (the entries of a collection directory) holds a file
/// called `name`.
fn has_file(files: &[Child], name: &str) -> bool {
    files
        .iter()
        .any(|child| child.name == name && child.node.kind == NodeKind::File)
}

fn refuse_leftover_schema_file(files: &[Child], dir_name: &str) -> Result<(), Error> {
    for name in LEFTOVER_SCHEMA_FILENAMES {
        if has_file(files, name) {
            return Err(Fault::new(
                VaultErrorCode::InvalidArgument,
                format!("{dir_name} holds the leftover schema file {name}"),
            )
            .with_path(format!("{dir_name}/{name}"))
            .into());
        }
    }
    Ok(())
}

fn find_declaration_file(files: &[Child], dir_name: &str) -> Result<Option<&'static str>, Error> {
    let mut found = None;
    for name in COLLECTION_FILENAMES {
        if !has_file(files, name) {
            continue;
        }
        if found.is_some() {
            return Err(Fault::new(
                VaultErrorCode::InvalidArgument,
                format!("the collection directory {dir_name} has more than one declaration file"),
            )
            .with_path(format!("{dir_name}/{name}"))
            .into());
        }
        found = Some(name);
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use scheda_api::v1::{CollectionDeclaration, VaultErrorCode};

    use super::{DEFAULT_SEGMENT_MAX_BYTES, Kind, declare, encode, load};
    use crate::error::{Error, Fault};
    use crate::repo::fixture;
    use crate::stored::{Format, path_string};

    /// A scratch Vault directory that is removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-collection-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }

        /// The declarations of what the files hold once committed.
        fn load(&self) -> Result<super::Declarations, Error> {
            fixture::commit_all(&self.0);
            load(&fixture::snap(&self.0))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn declared(text: &str) -> Result<super::Declaration, Error> {
        declare(text.as_bytes(), "c/.collection.yaml", "c").map_err(Error::from)
    }

    fn invalid(text: &str) -> bool {
        declared(text).is_err_and(|error| error.is(VaultErrorCode::InvalidArgument))
    }

    #[test]
    fn the_three_kinds_are_read_from_the_oneof() {
        let standard = declared("name: c\nschema:\n  standard: {}\n").unwrap();
        assert_eq!(standard.kind(), Kind::Standard);
        let journal = declared("name: c\nschema:\n  journal: {}\n").unwrap();
        assert_eq!(journal.kind(), Kind::Journal);
        let single = declared("name: c\nschema:\n  singleton:\n    id: inbox\n").unwrap();
        assert_eq!(single.kind(), Kind::Singleton);
        assert_eq!(single.singleton_id(), Some("inbox"));
        assert_eq!(standard.singleton_id(), None);
    }

    #[test]
    fn a_leftover_version_is_read_past_and_kept() {
        let text = include_str!("../testdata/schema/leftover_version.yaml");
        let declaration = declared(text).unwrap();
        let paths: Vec<String> = declaration
            .unknown
            .iter()
            .map(|field| path_string(field.path()))
            .collect();
        assert_eq!(paths, ["version"]);
        let bytes = encode(&declaration.collection, &declaration.unknown, Format::Yaml).unwrap();
        let again = declare(&bytes, "c/.collection.yaml", "c").unwrap();
        assert_eq!(again.collection, declaration.collection);
        assert_eq!(again.unknown.iter().count(), 1);
    }

    #[test]
    fn unknown_keys_are_read_past_wherever_they_are() {
        let text = include_str!("../testdata/schema/on_write.yaml");
        assert_eq!(declared(text).unwrap().kind(), Kind::Standard);
        let text = "name: c\nschema:\n  journal:\n    later: 1\n  newFlag: true\nnote: x\n";
        let declaration = declared(text).unwrap();
        assert_eq!(declaration.unknown.iter().count(), 3);
    }

    #[test]
    fn the_old_kind_key_names_no_kind_and_is_invalid_argument() {
        assert!(invalid(include_str!(
            "../testdata/schema/document_kind.yaml"
        )));
        assert!(invalid("name: c\nschema:\n  isolated: true\n"));
    }

    #[test]
    fn a_file_without_a_schema_is_invalid_argument() {
        assert!(invalid(include_str!(
            "../testdata/schema/missing_kind.yaml"
        )));
        assert!(invalid("schema:\n  standard: {}\n"));
    }

    #[test]
    fn an_encrypted_journal_is_invalid_argument() {
        assert!(invalid(
            "name: c\nschema:\n  journal: {}\n  encrypted: true\n"
        ));
        assert!(declared("name: c\nschema:\n  standard: {}\n  encrypted: true\n").is_ok());
    }

    #[test]
    fn slugs_are_checked_by_the_library() {
        assert!(invalid("name: team/notes\nschema:\n  standard: {}\n"));
        assert!(invalid("name: .hidden\nschema:\n  standard: {}\n"));
        assert!(invalid("name: c\nschema:\n  singleton:\n    id: .hidden\n"));
        assert!(invalid("name: c\nschema:\n  singleton: {}\n"));
    }

    #[test]
    fn the_directory_must_be_the_collection_name() {
        let error = declare(
            b"name: other\nschema:\n  standard: {}\n",
            "c/.collection.yaml",
            "c",
        )
        .unwrap_err();
        assert!(Error::from(error).is(VaultErrorCode::InvalidArgument));
    }

    #[test]
    fn journal_limits_are_checked_and_the_default_cap_is_filled_in() {
        let text = include_str!("../testdata/schema/journal.yaml");
        let journal = declared(text).unwrap();
        let limits = journal.journal().unwrap();
        assert_eq!(limits.segment_max_bytes, Some(DEFAULT_SEGMENT_MAX_BYTES));
        assert_eq!(limits.segment_max_lines, None);
        let set = declared("name: c\nschema:\n  journal:\n    segmentMaxBytes: 32\n    segmentMaxLines: 4\n    daily: {}\n").unwrap();
        let limits = set.journal().unwrap();
        assert_eq!(limits.segment_max_bytes, Some(32));
        assert_eq!(limits.segment_max_lines, Some(4));
        assert!(limits.daily.is_set());
        assert!(invalid(include_str!(
            "../testdata/schema/journal_oversize.yaml"
        )));
        assert!(invalid(
            "name: c\nschema:\n  journal:\n    segmentMaxLines: 0\n"
        ));
    }

    #[test]
    fn the_marks_are_read_and_decide_what_is_indexed() {
        let plain = declared("name: c\nschema:\n  standard: {}\n").unwrap();
        assert!(!plain.isolated() && !plain.encrypted());
        assert!(plain.lexical_indexed() && plain.vector_indexed());
        let marked = declared(
            "name: c\nschema:\n  standard: {}\n  isolated: true\n  excludeLexicalIndex: true\n  exclude_vector_index: true\n",
        )
        .unwrap();
        assert!(marked.isolated());
        assert!(!marked.lexical_indexed() && !marked.vector_indexed());
        let sealed = declared("name: c\nschema:\n  standard: {}\n  encrypted: true\n").unwrap();
        assert!(sealed.encrypted());
        assert!(!sealed.lexical_indexed() && !sealed.vector_indexed());
    }

    #[test]
    fn json_declarations_are_read_and_written_as_json() {
        let text = include_str!("../testdata/schema/west.json");
        let declaration =
            declare(text.as_bytes(), "west_docs/.collection.json", "west_docs").unwrap();
        assert_eq!(declaration.file.format, Format::Json);
        let bytes = encode(&declaration.collection, &declaration.unknown, Format::Json).unwrap();
        assert!(bytes.starts_with(b"{\n"));
        assert_eq!(
            crate::stored::read::<CollectionDeclaration>(&bytes, Format::Json)
                .unwrap()
                .message,
            declaration.collection
        );
    }

    #[test]
    fn load_reads_every_collection_directory_and_skips_the_rest() {
        let vault = Scratch::new("load");
        vault.write(
            "notes/.collection.yaml",
            "name: notes\nschema:\n  standard: {}\n",
        );
        vault.write(
            "log/.collection.json",
            r#"{"name":"log","schema":{"journal":{}}}"#,
        );
        vault.write("plain/readme.md", "no declaration");
        vault.write(".scheda/x/.collection.yaml", "not read");
        vault.write(".isolated/y/.collection.yaml", "not read");
        vault.write("file.txt", "x");
        let declarations = vault.load().unwrap();
        assert_eq!(declarations.len(), 2);
        let names: Vec<&str> = declarations.iter().map(|d| d.name()).collect();
        assert_eq!(names, ["log", "notes"]);
        assert_eq!(
            declarations.get("notes").unwrap().file.rel,
            "notes/.collection.yaml"
        );
        assert_eq!(declarations.get("log").unwrap().file.format, Format::Json);
        assert!(declarations.get("missing").is_none());
        // A commit that does not exist yet has no declarations.
        let empty = Scratch::new("unborn");
        std::fs::create_dir_all(empty.0.join("notes")).unwrap();
        std::fs::write(
            empty.0.join("notes/.collection.yaml"),
            "name: notes\nschema:\n  standard: {}\n",
        )
        .unwrap();
        fixture::init(&empty.0);
        assert_eq!(load(&fixture::snap(&empty.0)).unwrap().len(), 0);
    }

    #[test]
    fn load_fails_whole_on_one_invalid_file_and_names_its_path() {
        let vault = Scratch::new("bad");
        vault.write(
            "notes/.collection.yaml",
            "name: notes\nschema:\n  standard: {}\n",
        );
        vault.write(
            "bad/.collection.yaml",
            "name: bad\nschema:\n  document: {}\n",
        );
        let error = vault.load().unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let fault = error.downcast_ref::<Fault>().unwrap();
        assert_eq!(fault.path(), Some("bad/.collection.yaml"));
    }

    #[test]
    fn load_refuses_a_directory_named_otherwise() {
        let vault = Scratch::new("dirname");
        vault.write(
            "notes/.collection.yaml",
            "name: pages\nschema:\n  standard: {}\n",
        );
        assert!(
            vault
                .load()
                .unwrap_err()
                .is(VaultErrorCode::InvalidArgument)
        );
    }

    #[test]
    fn load_refuses_two_declaration_files_in_one_directory() {
        let vault = Scratch::new("two");
        let text = "name: notes\nschema:\n  standard: {}\n";
        vault.write("notes/.collection.yaml", text);
        vault.write(
            "notes/.collection.json",
            r#"{"name":"notes","schema":{"standard":{}}}"#,
        );
        assert!(
            vault
                .load()
                .unwrap_err()
                .is(VaultErrorCode::InvalidArgument)
        );
    }

    #[test]
    fn load_refuses_a_leftover_schema_file() {
        let vault = Scratch::new("leftover");
        vault.write(
            "notes/.collection.yaml",
            "name: notes\nschema:\n  standard: {}\n",
        );
        vault.write("notes/.schema.yaml", "fields: []\n");
        assert!(
            vault
                .load()
                .unwrap_err()
                .is(VaultErrorCode::InvalidArgument)
        );
    }

    #[test]
    fn the_fixtures_of_generation_two_load() {
        let consumer = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/consumer");
        if consumer.is_dir() {
            let vault = Scratch::new("fixtures");
            copy_tree(&consumer, &vault.0);
            let declarations = vault.load().unwrap();
            assert!(declarations.len() >= 10);
            assert_eq!(
                declarations.get("run_heads").unwrap().singleton_id(),
                Some("cursor")
            );
            assert!(declarations.get("alias_bindings").unwrap().isolated());
        }
    }
}
