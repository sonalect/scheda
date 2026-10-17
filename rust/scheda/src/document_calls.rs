//! Document calls of the `Vault` handle: read one Document, and write them
//! singly or as a batch.
//!
//! A write is checked first with what does not depend on the state of the
//! Vault, and fails `INVALID_ARGUMENT` before any lock is taken. Then it takes
//! the write lock, applies its operations in order, each against the state
//! the ones before it leave, and makes one commit that holds every file it
//! changes. The first failure fails the call and writes nothing.
//!
//! The message of that commit is built by [`message`]: its first line is the
//! first line of the first note, then a blank line, then one trailer per
//! write that changed something:
//!
//! ```text
//! Scheda-Note: {"collection":"notes","id":"a.md","note":"why it was written"}
//! ```
//!
//! The value is a JSON object of the collection, the id, and the note, on one
//! line (the newlines of a note are escaped). The history of a Document reads
//! the trailers whose collection and id are its own, so a commit that wrote
//! several Documents shows each one its own note.

use std::borrow::Cow;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use buffa_types::google::protobuf::Empty;
use protocontract::BoxIter;
use scheda_api::v1::document_operation::Operation;
use scheda_api::v1::document_result::Result as ResultArm;
use scheda_api::v1::{
    Document, DocumentBatchResult, DocumentCreation, DocumentDeletion, DocumentKey,
    DocumentOperation, DocumentResult, DocumentUpdate, DocumentUpsert, Encryption, VaultErrorCode,
};
use serde_json::json;

use crate::collection::{Declaration, Declarations, Kind};
use crate::commit::{Change, Repo};
use crate::documents::{self, Found, Plain};
use crate::error::{Context, Error, Fault, io};
use crate::fs::rel_of;
use crate::id::require_slug;
use crate::{isolated, journal, stored};

/// One write of a Document, as the single calls and the batch carry it.
enum Write {
    Create(DocumentCreation),
    Update(DocumentUpdate),
    Upsert(DocumentUpsert),
    Delete(DocumentDeletion),
}

/// What a write asks of a Document, apart from its bytes.
enum Verb {
    Create {
        keep_existing: bool,
    },
    Update {
        revision: Option<String>,
    },
    Upsert,
    Delete {
        ignore_missing: bool,
        revision: Option<String>,
    },
}

/// A write taken apart to be applied: its bytes are moved out of the
/// request.
struct Step {
    verb: Verb,
    collection: String,
    id: String,
    raw: Vec<u8>,
    note: String,
}

/// What the checks that need no lock make of one write.
enum Inspected {
    /// A line appended to a journal.
    Line { isolated: bool, line: Vec<u8> },
    /// The file of a `standard` or `singleton` Document.
    File { isolated: bool, path: String },
}

impl Inspected {
    fn isolated(&self) -> bool {
        match self {
            Inspected::Line { isolated, .. } | Inspected::File { isolated, .. } => *isolated,
        }
    }
}

/// Where the bytes of a reply come from.
enum Raw {
    /// Bytes the reply owns.
    Value(Vec<u8>),
    /// The bytes of the file the commit stores at this Vault-relative path,
    /// taken back from the change once it is stored: the bytes of the
    /// request travel through the commit into the reply without a copy.
    File(String),
}

/// What a write answers, before the commit has stored it.
enum Draft {
    Stored {
        collection: String,
        id: String,
        revision: Option<String>,
        raw: Raw,
    },
    Deleted,
}

/// What a write returns.
enum Outcome {
    Document(Document),
    Deleted,
}

fn invalid(message: impl Into<String>) -> Fault {
    Fault::new(VaultErrorCode::InvalidArgument, message)
}

fn storage(message: &str) -> Fault {
    Fault::new(VaultErrorCode::Storage, message)
}

fn document_key(document: Option<&Document>) -> (&str, &str) {
    document.map_or(("", ""), |document| {
        (
            document.collection.as_deref().unwrap_or(""),
            document.id.as_deref().unwrap_or(""),
        )
    })
}

impl Write {
    fn from_operation(operation: DocumentOperation) -> Result<Write, Fault> {
        match operation.operation {
            Some(Operation::Create(write)) => Ok(Write::Create(*write)),
            Some(Operation::Update(write)) => Ok(Write::Update(*write)),
            Some(Operation::Upsert(write)) => Ok(Write::Upsert(*write)),
            Some(Operation::Delete(write)) => Ok(Write::Delete(*write)),
            None => Err(invalid("the operation names no write")),
        }
    }

    /// The Document the request carries, if it carries one.
    fn document(&self) -> Option<&Document> {
        match self {
            Write::Create(write) => write.document.as_option(),
            Write::Update(write) => write.document.as_option(),
            Write::Upsert(write) => write.document.as_option(),
            Write::Delete(_) => None,
        }
    }

    /// The collection and the id the write is about.
    fn key(&self) -> (&str, &str) {
        match self {
            Write::Delete(write) => write.document.as_option().map_or(("", ""), |key| {
                (
                    key.collection.as_deref().unwrap_or(""),
                    key.id.as_deref().unwrap_or(""),
                )
            }),
            _ => document_key(self.document()),
        }
    }

    fn raw(&self) -> &[u8] {
        self.document()
            .and_then(|document| document.raw.as_deref())
            .unwrap_or(&[])
    }

    fn note(&self) -> &str {
        let note = match self {
            Write::Create(write) => &write.note,
            Write::Update(write) => &write.note,
            Write::Upsert(write) => &write.note,
            Write::Delete(write) => &write.note,
        };
        note.as_deref().unwrap_or("")
    }

    /// The rules of the contract: the request validates (a `note`, not
    /// empty; a Document with a collection and an id), and a Document in a
    /// request carries no revision, since Scheda gives it.
    fn validate(&self) -> Result<(), Fault> {
        match self {
            Write::Create(write) => stored::validate(write)?,
            Write::Update(write) => stored::validate(write)?,
            Write::Upsert(write) => stored::validate(write)?,
            Write::Delete(write) => stored::validate(write)?,
        }
        if self
            .document()
            .is_some_and(|document| document.revision.is_some())
        {
            return Err(invalid(
                "a Document in a write carries no revision; Scheda gives it",
            ));
        }
        Ok(())
    }

    /// Takes the request apart, moving the bytes of the Document out.
    fn into_step(self) -> Step {
        match self {
            Write::Create(mut write) => {
                let document = write.document.take().unwrap_or_default();
                Step {
                    verb: Verb::Create {
                        keep_existing: write.keep_existing.unwrap_or(false),
                    },
                    collection: document.collection.unwrap_or_default(),
                    id: document.id.unwrap_or_default(),
                    raw: document.raw.unwrap_or_default(),
                    note: write.note.unwrap_or_default(),
                }
            }
            Write::Update(mut write) => {
                let document = write.document.take().unwrap_or_default();
                Step {
                    verb: Verb::Update {
                        revision: write.revision,
                    },
                    collection: document.collection.unwrap_or_default(),
                    id: document.id.unwrap_or_default(),
                    raw: document.raw.unwrap_or_default(),
                    note: write.note.unwrap_or_default(),
                }
            }
            Write::Upsert(mut write) => {
                let document = write.document.take().unwrap_or_default();
                Step {
                    verb: Verb::Upsert,
                    collection: document.collection.unwrap_or_default(),
                    id: document.id.unwrap_or_default(),
                    raw: document.raw.unwrap_or_default(),
                    note: write.note.unwrap_or_default(),
                }
            }
            Write::Delete(mut write) => {
                let key = write.document.take().unwrap_or_default();
                Step {
                    verb: Verb::Delete {
                        ignore_missing: write.ignore_missing.unwrap_or(false),
                        revision: write.revision,
                    },
                    collection: key.collection.unwrap_or_default(),
                    id: key.id.unwrap_or_default(),
                    raw: Vec::new(),
                    note: write.note.unwrap_or_default(),
                }
            }
        }
    }
}

/// The checks of one write that need no lock, against `declarations`: the
/// collection is declared, its kind takes the operation, the id and the
/// payload are well formed, and encryption is available when the collection
/// needs it.
fn inspect(
    write: &Write,
    declarations: &Declarations,
    encryption: Option<&Encryption>,
) -> Result<Inspected, Fault> {
    let (collection, id) = write.key();
    let decl = declarations.get(collection).ok_or_else(|| {
        Fault::new(
            VaultErrorCode::NotFound,
            format!("the collection {collection} is not declared"),
        )
    })?;
    let inspected = match decl.kind() {
        Kind::Journal => {
            if !matches!(write, Write::Create(_)) {
                return Err(Fault::new(
                    VaultErrorCode::Unimplemented,
                    format!(
                        "the collection {collection} is a journal; a line is appended and never \
                         replaced or deleted"
                    ),
                ));
            }
            Inspected::Line {
                isolated: decl.isolated(),
                line: journal::encode_line(id, write.raw())?,
            }
        }
        Kind::Standard | Kind::Singleton => Inspected::File {
            isolated: decl.isolated(),
            path: documents::path_of(decl, id)?,
        },
    };
    documents::require_secret(decl, encryption)?;
    Ok(inspected)
}

/// [`inspect`] for every write, and the repository they all go to: one call
/// writes the main tree or the isolated collections, never both.
fn inspect_all(
    writes: &[Write],
    declarations: &Declarations,
    encryption: Option<&Encryption>,
    indexed: bool,
) -> Result<(Vec<Inspected>, Repo), Error> {
    let mut all = Vec::with_capacity(writes.len());
    let mut repo = None;
    for (index, write) in writes.iter().enumerate() {
        let inspected = inspect(write, declarations, encryption)
            .map_err(Error::from)
            .with_context(|| frame(indexed, index))?;
        let this = if inspected.isolated() {
            Repo::Isolated
        } else {
            Repo::Main
        };
        if *repo.get_or_insert(this) != this {
            return Err(Error::from(invalid(
                "one write holds Documents of isolated collections and of others; they are \
                 stored in two repositories, which cannot take one step",
            )))
            .with_context(|| frame(indexed, index));
        }
        all.push(inspected);
    }
    Ok((all, repo.unwrap_or(Repo::Main)))
}

fn frame(indexed: bool, index: usize) -> String {
    if indexed {
        format!("operation {index}")
    } else {
        String::from("write a Document")
    }
}

impl crate::Vault {
    /// One Document, read from its file, not from the Index.
    ///
    /// A `standard` or `singleton` Document is the file in the work tree,
    /// opened for an encrypted collection; a journal line is found by a scan
    /// of the segments. `raw` is always set. `revision` is the SHA-256, in
    /// lowercase hex, of the bytes of the stored file (the armor, for an
    /// encrypted collection); a journal line has none. The read uses the
    /// collections as the handle holds them, and reads them again, once,
    /// only for a collection it does not know.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for a key that breaks the rules of the contract;
    /// `NOT_FOUND` for an unknown collection or Document; `CRYPTO` for an
    /// encrypted collection when the handle has no secret or the wrong one;
    /// `UNIMPLEMENTED` for an encrypted collection in a build without the
    /// `encrypt` feature; `STORAGE` when a file cannot be read.
    pub fn get_document(&self, document_key: DocumentKey) -> Result<Document, Error> {
        stored::validate(&document_key).context("get a Document")?;
        let collection = document_key.collection.unwrap_or_default();
        let id = document_key.id.unwrap_or_default();
        let declarations = self
            .declarations_knowing([collection.as_str()])
            .context("get a Document")?;
        let decl = declarations.get(&collection).ok_or_else(|| {
            Fault::new(
                VaultErrorCode::NotFound,
                format!("the collection {collection} is not declared"),
            )
        })?;
        let absent = || {
            Fault::new(
                VaultErrorCode::NotFound,
                format!("the Document {id} is not in the collection {collection}"),
            )
        };
        if decl.kind() == Kind::Journal {
            require_slug("id", &id).context("get a Document")?;
            let line = journal::find_journal_scan(self.path(), decl, &id)
                .context("get a Document")?
                .ok_or_else(absent)?;
            return Ok(Document {
                collection: Some(collection),
                id: Some(id),
                raw: Some(line.payload),
                ..Default::default()
            });
        }
        let found = documents::read_found(self.path(), decl, &id, self.encryption())
            .context("get a Document")?
            .ok_or_else(absent)?;
        let revision = found.revision();
        Ok(Document {
            collection: Some(collection),
            id: Some(id),
            raw: Some(found.into_plain()),
            revision: Some(revision),
            ..Default::default()
        })
    }

    /// Creates a Document and returns it as stored, with its revision. In a
    /// journal, appends a line. Present, `ALREADY_EXISTS`, or, with
    /// `keep_existing`, the Document already stored is returned and nothing
    /// is written.
    ///
    /// The request is checked before the write lock is taken: it validates
    /// (a `note`, not empty), the Document carries no `revision`, the
    /// collection is declared, the id (a singleton's must be its declared
    /// one) and a journal payload (no `\n`, no `\r`) are well formed, and
    /// encryption is available for an encrypted collection. Identical bytes
    /// store nothing and keep the revision.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for the checks above; `NOT_FOUND` for an unknown
    /// collection; `ALREADY_EXISTS`; `CRYPTO`, `UNIMPLEMENTED` as for
    /// [`Vault::get_document`](crate::Vault::get_document); `ABORTED`
    /// (retryable) when the write lock is not free within
    /// `write_timeout_ms`, or `HEAD` moved under the write; nothing is
    /// written then. `STORAGE` or `REPLICATION` when a file or git fails.
    pub fn create_document(&self, document_creation: DocumentCreation) -> Result<Document, Error> {
        self.write_document(Write::Create(document_creation))
    }

    /// Replaces an existing Document and returns it as stored. Absent,
    /// `NOT_FOUND`. With `revision`, the stored revision must be that one, or
    /// `FAILED_PRECONDITION`. A journal, `UNIMPLEMENTED`. The same bytes store
    /// nothing and keep the revision.
    ///
    /// # Errors
    ///
    /// As [`Vault::create_document`](crate::Vault::create_document), with
    /// `NOT_FOUND` for an absent Document and `FAILED_PRECONDITION` for a
    /// stale `revision`.
    pub fn update_document(&self, document_update: DocumentUpdate) -> Result<Document, Error> {
        self.write_document(Write::Update(document_update))
    }

    /// Creates the Document, or replaces it when present, and returns it as
    /// stored. A journal, `UNIMPLEMENTED`. The same bytes store nothing and
    /// keep the revision.
    ///
    /// # Errors
    ///
    /// As [`Vault::create_document`](crate::Vault::create_document), but a
    /// Document that is present is not an error.
    pub fn upsert_document(&self, document_upsert: DocumentUpsert) -> Result<Document, Error> {
        self.write_document(Write::Upsert(document_upsert))
    }

    /// Deletes a Document. Absent, `NOT_FOUND`, or nothing with
    /// `ignore_missing`. With `revision`, the stored revision must be that
    /// one, or `FAILED_PRECONDITION`. A journal, `UNIMPLEMENTED`: a journal
    /// line never leaves.
    ///
    /// # Errors
    ///
    /// As [`Vault::update_document`](crate::Vault::update_document).
    pub fn delete_document(&self, document_deletion: DocumentDeletion) -> Result<Empty, Error> {
        self.write_document(Write::Delete(document_deletion))
            .map(|_| Empty::default())
    }

    /// Several Document writes as one, across collections: all happen, or
    /// none. An `Err` item, a broken stream, or one write that fails, and none
    /// happened; the error names the operation by its index. One result per
    /// operation, in order. Collections are not written here
    /// (`batch_collections`).
    ///
    /// The stream is read whole first and no lock is held while it is read.
    /// The operations are applied in order, each against the state the ones
    /// before it leave: two operations on one Document see each other. The
    /// whole batch is one commit, and a batch that changes nothing makes
    /// none. One call writes the Documents of the main tree or those of
    /// isolated collections, never both (`INVALID_ARGUMENT`).
    ///
    /// # Errors
    ///
    /// The errors of the single calls, and the `Err` of an item.
    pub fn batch_documents(
        &self,
        document_operations: BoxIter<'static, Result<DocumentOperation, Error>>,
    ) -> Result<DocumentBatchResult, Error> {
        let mut writes = Vec::new();
        for (index, item) in document_operations.enumerate() {
            let write = item
                .and_then(|item| Write::from_operation(item).map_err(Error::from))
                .with_context(|| format!("operation {index}"))?;
            writes.push(write);
        }
        let outcomes = self.write_documents(writes, true)?;
        Ok(DocumentBatchResult {
            results: outcomes
                .into_iter()
                .map(|outcome| DocumentResult {
                    result: Some(match outcome {
                        Outcome::Document(document) => ResultArm::from(document),
                        Outcome::Deleted => ResultArm::from(Empty::default()),
                    }),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
    }

    /// The collections the handle holds, or, when one of `names` is not
    /// among them, the collections read again once.
    fn declarations_knowing<'a>(
        &self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<Arc<Declarations>, Error> {
        let cached = self.declarations_cached()?;
        if names.into_iter().all(|name| cached.get(name).is_some()) {
            return Ok(cached);
        }
        self.declarations_now()
    }

    /// One write, as the single calls make it.
    fn write_document(&self, write: Write) -> Result<Document, Error> {
        let mut outcomes = self.write_documents(vec![write], false)?;
        match outcomes.pop() {
            Some(Outcome::Document(document)) => Ok(document),
            Some(Outcome::Deleted) => Ok(Document::default()),
            None => Err(storage("the write returned nothing").into()),
        }
    }

    /// The writes as one commit. `indexed` puts the position of a failing
    /// write into the error.
    fn write_documents(&self, writes: Vec<Write>, indexed: bool) -> Result<Vec<Outcome>, Error> {
        if writes.is_empty() {
            return Ok(Vec::new());
        }
        let encryption = self.encryption();
        for (index, write) in writes.iter().enumerate() {
            write
                .validate()
                .map_err(Error::from)
                .with_context(|| frame(indexed, index))?;
        }
        let before = self.declarations_knowing(writes.iter().map(|write| write.key().0))?;
        let (mut inspected, mut repo) = inspect_all(&writes, &before, encryption, indexed)?;
        let first_note = first_line(writes.first().map_or("", Write::note));
        let lock = self.committer().lock().context("take the write lock")?;
        let declarations = self.declarations_locked(&lock)?;
        if !Arc::ptr_eq(&before, &declarations) {
            // The collections changed since the checks: do them again
            // against the ones that are in force now.
            (inspected, repo) = inspect_all(&writes, &declarations, encryption, indexed)?;
        }
        let mut ledger = Ledger::new(self.path(), &declarations, encryption, self.writer());
        ledger.count(&inspected);
        let mut drafts = Vec::with_capacity(writes.len());
        for (index, (write, inspected)) in writes.into_iter().zip(inspected).enumerate() {
            let draft = ledger
                .apply(write.into_step(), inspected)
                .with_context(|| frame(indexed, index))?;
            drafts.push(draft);
        }
        let trailers = std::mem::take(&mut ledger.trailers);
        let mut changes = ledger.into_changes()?;
        if changes.is_empty() {
            return drafts.into_iter().map(|draft| draft.settle(None)).collect();
        }
        let root = self.path();
        if repo == Repo::Isolated {
            let mut with_ignore = isolated::ensure(root)?;
            with_ignore.append(&mut changes);
            changes = with_ignore;
        }
        let head = lock.commit_in(repo, &message(&first_note, &trailers), &changes)?;
        let main_head = match repo {
            Repo::Main => head,
            Repo::Isolated => lock.head()?,
        };
        self.remember(main_head, declarations)?;
        let mut files = take_files(&drafts, changes);
        drafts
            .into_iter()
            .map(|draft| draft.settle(Some(&mut files)))
            .collect()
    }
}

/// The first line of `note` that has a character, trimmed; a fixed text for
/// a note that has none.
fn first_line(note: &str) -> String {
    note.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("write Documents")
        .to_owned()
}

/// The message of a commit: `first_line`, a blank line, then the trailers.
fn message(first_line: &str, trailers: &[String]) -> String {
    let mut out = String::from(first_line);
    out.push('\n');
    for (index, trailer) in trailers.iter().enumerate() {
        if index == 0 {
            out.push('\n');
        }
        out.push_str(trailer);
        out.push('\n');
    }
    out
}

/// The `Scheda-Note:` trailer of one Document write: a JSON object of the
/// collection, the id, and the note, on one line.
fn trailer(collection: &str, id: &str, note: &str) -> String {
    format!(
        "Scheda-Note: {}",
        json!({"collection": collection, "id": id, "note": note})
    )
}

/// The bytes of the `Put` changes the drafts take back, by path.
fn take_files(drafts: &[Draft], changes: Vec<Change>) -> HashMap<String, Vec<u8>> {
    let wanted: HashSet<&str> = drafts
        .iter()
        .filter_map(|draft| match draft {
            Draft::Stored {
                raw: Raw::File(path),
                ..
            } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    let mut files = HashMap::new();
    for change in changes {
        if let Change::Put { path, bytes, .. } = change
            && wanted.contains(path.as_str())
        {
            files.insert(path, bytes);
        }
    }
    files
}

impl Draft {
    /// The outcome of the write once it is stored. `files` holds the bytes
    /// of the files the commit stored, when there was a commit.
    fn settle(self, files: Option<&mut HashMap<String, Vec<u8>>>) -> Result<Outcome, Error> {
        match self {
            Draft::Deleted => Ok(Outcome::Deleted),
            Draft::Stored {
                collection,
                id,
                revision,
                raw,
            } => {
                let raw = match raw {
                    Raw::Value(bytes) => bytes,
                    Raw::File(path) => files
                        .and_then(|files| files.remove(&path))
                        .ok_or_else(|| storage("a stored file is missing from the commit"))?,
                };
                Ok(Outcome::Document(Document {
                    collection: Some(collection),
                    id: Some(id),
                    raw: Some(raw),
                    revision,
                    ..Default::default()
                }))
            }
        }
    }
}

/// A Document file as the call sees it: as it lay when first touched, and
/// what the writes of the call have made of it.
struct Slot {
    original: Option<Found>,
    /// `None`: untouched. `Some(None)`: deleted. `Some(Some(_))`: replaced.
    replaced: Option<Option<Found>>,
}

impl Slot {
    fn current(&self) -> Option<&Found> {
        match &self.replaced {
            Some(replaced) => replaced.as_ref(),
            None => self.original.as_ref(),
        }
    }
}

/// The lines a call has appended to a journal, and the ids the journal has.
struct JournalState {
    ids: HashSet<String>,
    /// The whole new bytes of each segment appended to, by path.
    pending: BTreeMap<PathBuf, Vec<u8>>,
}

/// The state of a write call under the lock: the files it touches, as the
/// operations so far leave them.
struct Ledger<'a> {
    vault: &'a Path,
    declarations: &'a Declarations,
    encryption: Option<&'a Encryption>,
    writer: &'a str,
    /// The files more than one operation of the call is about.
    repeated: HashSet<String>,
    slots: BTreeMap<String, Slot>,
    journals: BTreeMap<String, JournalState>,
    trailers: Vec<String>,
}

impl<'a> Ledger<'a> {
    fn new(
        vault: &'a Path,
        declarations: &'a Declarations,
        encryption: Option<&'a Encryption>,
        writer: &'a str,
    ) -> Ledger<'a> {
        Ledger {
            vault,
            declarations,
            encryption,
            writer,
            repeated: HashSet::new(),
            slots: BTreeMap::new(),
            journals: BTreeMap::new(),
            trailers: Vec::new(),
        }
    }

    /// Notes which files more than one operation is about: those keep a copy
    /// of their bytes for the operations after them.
    fn count(&mut self, inspected: &[Inspected]) {
        let mut seen = HashSet::new();
        for inspected in inspected {
            if let Inspected::File { path, .. } = inspected
                && !seen.insert(path.as_str())
            {
                self.repeated.insert(path.clone());
            }
        }
    }

    fn apply(&mut self, step: Step, inspected: Inspected) -> Result<Draft, Error> {
        let Some(decl) = self.declarations.get(&step.collection) else {
            return Err(Fault::new(
                VaultErrorCode::NotFound,
                format!("the collection {} is not declared", step.collection),
            )
            .into());
        };
        match inspected {
            Inspected::Line { line, .. } => self.append(decl, step, &line),
            Inspected::File { path, .. } => self.write_file(decl, step, path),
        }
    }

    /// Appends one line to a journal.
    fn append(&mut self, decl: &Declaration, step: Step, line: &[u8]) -> Result<Draft, Error> {
        let Verb::Create { keep_existing } = step.verb else {
            return Err(Fault::new(
                VaultErrorCode::Unimplemented,
                "a journal line is appended and never replaced or deleted",
            )
            .into());
        };
        let state = match self.journals.entry(step.collection.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(JournalState {
                ids: journal::stored_ids(self.vault, decl)?,
                pending: BTreeMap::new(),
            }),
        };
        if state.ids.contains(&step.id) {
            if !keep_existing {
                return Err(Fault::new(
                    VaultErrorCode::AlreadyExists,
                    format!(
                        "the line {} is already in the journal {}",
                        step.id, step.collection
                    ),
                )
                .into());
            }
            let payload = state
                .pending
                .values()
                .find_map(|bytes| {
                    journal::parse_segment(bytes)
                        .into_iter()
                        .find(|stored| stored.id == step.id)
                })
                .map_or_else(
                    || journal::find_journal_scan(self.vault, decl, &step.id),
                    |stored| Ok(Some(stored)),
                )?
                .ok_or_else(|| storage("a line of the journal could not be read again"))?
                .payload;
            return Ok(Draft::Stored {
                collection: step.collection,
                id: step.id,
                revision: None,
                raw: Raw::Value(payload),
            });
        }
        let today = journal::today_utc()?;
        let path = journal::segment_for_append_in(
            self.vault,
            decl,
            line.len() as u64,
            self.writer,
            &today,
            &state.pending,
        )?;
        let content = match state.pending.entry(path.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(read_segment(self.vault, &path)?),
        };
        content.extend_from_slice(line);
        state.ids.insert(step.id.clone());
        self.trailers
            .push(trailer(&step.collection, &step.id, &step.note));
        Ok(Draft::Stored {
            collection: step.collection,
            id: step.id,
            revision: None,
            raw: Raw::Value(step.raw),
        })
    }

    /// One write of a `standard` or `singleton` Document.
    fn write_file(&mut self, decl: &Declaration, step: Step, path: String) -> Result<Draft, Error> {
        if !self.slots.contains_key(&path) {
            let original = documents::read_found(self.vault, decl, &step.id, self.encryption)
                .map_err(|fault| fault.with_path(path.clone()))?;
            self.slots.insert(
                path.clone(),
                Slot {
                    original,
                    replaced: None,
                },
            );
        }
        let repeated = self.repeated.contains(&path);
        let encryption = self.encryption;
        let Some(slot) = self.slots.get_mut(&path) else {
            return Err(storage("a file of the write is missing").into());
        };
        match &step.verb {
            Verb::Create { keep_existing } => {
                if let Some(found) = slot.current() {
                    if !*keep_existing {
                        return Err(Fault::new(
                            VaultErrorCode::AlreadyExists,
                            format!(
                                "the Document {} is already in the collection {}",
                                step.id, step.collection
                            ),
                        )
                        .with_path(path)
                        .into());
                    }
                    let raw = found
                        .plain()
                        .ok_or_else(|| storage("a Document of the call could not be read again"))?
                        .to_vec();
                    return Ok(Draft::Stored {
                        revision: Some(found.revision()),
                        collection: step.collection,
                        id: step.id,
                        raw: Raw::Value(raw),
                    });
                }
            }
            Verb::Update { revision } => {
                let Some(found) = slot.current() else {
                    return Err(absent(&step, path).into());
                };
                check_revision(found, revision.as_deref(), &step, &path)?;
            }
            Verb::Upsert => {}
            Verb::Delete {
                ignore_missing,
                revision,
            } => {
                let Some(found) = slot.current() else {
                    if *ignore_missing {
                        return Ok(Draft::Deleted);
                    }
                    return Err(absent(&step, path).into());
                };
                check_revision(found, revision.as_deref(), &step, &path)?;
                slot.replaced = Some(None);
                self.trailers
                    .push(trailer(&step.collection, &step.id, &step.note));
                return Ok(Draft::Deleted);
            }
        }
        let Step {
            collection,
            id,
            raw,
            note,
            ..
        } = step;
        if let Some(found) = slot.current()
            && found.plain() == Some(raw.as_slice())
        {
            // Identical bytes change nothing and keep the revision.
            return Ok(Draft::Stored {
                revision: Some(found.revision()),
                collection,
                id,
                raw: Raw::Value(raw),
            });
        }
        let sealed = {
            let encoded = documents::encode(decl, &id, &raw, encryption)
                .map_err(|fault| fault.with_path(path.clone()))?;
            match encoded.bytes {
                Cow::Owned(sealed) => Some(sealed),
                Cow::Borrowed(_) => None,
            }
        };
        let (found, revision, reply) = match sealed {
            Some(file) => {
                let revision = documents::revision_of(&file);
                let (plain, reply) = if repeated {
                    (Plain::Opened(raw.clone()), Raw::Value(raw))
                } else {
                    (Plain::Unknown, Raw::Value(raw))
                };
                (Found { file, plain }, revision, reply)
            }
            None => {
                let revision = documents::revision_of(&raw);
                let reply = if repeated {
                    Raw::Value(raw.clone())
                } else {
                    Raw::File(path.clone())
                };
                (
                    Found {
                        file: raw,
                        plain: Plain::Same,
                    },
                    revision,
                    reply,
                )
            }
        };
        slot.replaced = Some(Some(found));
        self.trailers.push(trailer(&collection, &id, &note));
        Ok(Draft::Stored {
            collection,
            id,
            revision: Some(revision),
            raw: reply,
        })
    }

    /// The changes the call makes to the files, in path order: what differs
    /// from how the files lay when the call began.
    fn into_changes(self) -> Result<Vec<Change>, Error> {
        let mut changes = Vec::new();
        for (path, slot) in self.slots {
            match slot.replaced {
                None => {}
                Some(None) => {
                    if slot.original.is_some() {
                        changes.push(Change::Remove { path });
                    }
                }
                Some(Some(found)) => {
                    let same = slot
                        .original
                        .as_ref()
                        .is_some_and(|original| original.file == found.file);
                    if !same {
                        changes.push(Change::put(path, found.file));
                    }
                }
            }
        }
        for state in self.journals.into_values() {
            for (path, bytes) in state.pending {
                changes.push(Change::put(rel_of(self.vault, &path)?, bytes));
            }
        }
        Ok(changes)
    }
}

fn absent(step: &Step, path: String) -> Fault {
    Fault::new(
        VaultErrorCode::NotFound,
        format!(
            "the Document {} is not in the collection {}",
            step.id, step.collection
        ),
    )
    .with_path(path)
}

/// `FAILED_PRECONDITION` unless `wanted` is unset or is the revision of
/// `found`.
fn check_revision(
    found: &Found,
    wanted: Option<&str>,
    step: &Step,
    path: &str,
) -> Result<(), Fault> {
    match wanted {
        Some(wanted) if wanted != found.revision() => Err(Fault::new(
            VaultErrorCode::FailedPrecondition,
            format!(
                "the Document {} of the collection {} is not at revision {wanted}",
                step.id, step.collection
            ),
        )
        .with_path(path)),
        _ => Ok(()),
    }
}

/// The bytes of a journal segment, empty when it is not there yet.
fn read_segment(vault: &Path, path: &Path) -> Result<Vec<u8>, Error> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(io(err).with_path(rel_of(vault, path)?).into()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use scheda_api::v1::{
        Collection, DocumentCreation, DocumentDeletion, DocumentKey, DocumentUpsert, OpenOptions,
        VaultErrorCode,
    };

    use super::{Document, first_line, message, trailer};
    use crate::commit::hooks;
    use crate::git::{git, spawned};
    use crate::vault::Vault;

    /// A Vault on disk made by `Vault::open`, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-docs-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Scratch(dir)
        }

        fn open(&self) -> Vault {
            Vault::open(&self.0, OpenOptions::default().with_write_timeout_ms(2000)).unwrap()
        }

        fn out(&self, dir: &str, args: &[&str]) -> String {
            let mut cmd = git().current_dir(&self.0.join(dir));
            for arg in args {
                cmd = cmd.arg(arg);
            }
            cmd.run()
                .unwrap()
                .stdout_str()
                .unwrap()
                .trim_end()
                .to_owned()
        }

        fn status(&self, dir: &str) -> String {
            self.out(dir, &["status", "--porcelain"])
        }

        fn lock(&self) -> Vec<u8> {
            std::fs::read(self.0.join(".scheda/commit.lock")).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn declare(vault: &Vault, name: &str, isolated: bool) {
        let collection: Collection = serde_json::from_value(serde_json::json!({
            "name": name, "schema": {"standard": {}, "isolated": isolated}
        }))
        .unwrap();
        vault.create_collection(collection).unwrap();
    }

    fn upsert(collection: &str, id: &str, raw: &str) -> DocumentUpsert {
        DocumentUpsert {
            document: Document {
                collection: Some(collection.to_owned()),
                id: Some(id.to_owned()),
                raw: Some(raw.as_bytes().to_vec()),
                ..Default::default()
            }
            .into(),
            note: Some("test".to_owned()),
            ..Default::default()
        }
    }

    fn key(collection: &str, id: &str) -> DocumentKey {
        DocumentKey {
            collection: Some(collection.to_owned()),
            id: Some(id.to_owned()),
            ..Default::default()
        }
    }

    fn creation(collection: &str, id: &str, raw: &str) -> DocumentCreation {
        DocumentCreation {
            document: upsert(collection, id, raw).document,
            note: Some("test".to_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn a_read_starts_no_git_process_for_a_collection_it_holds() {
        let scratch = Scratch::new("read-no-git");
        let vault = scratch.open();
        declare(&vault, "notes", false);
        vault.upsert_document(upsert("notes", "a.md", "x")).unwrap();
        let before = spawned();
        for _ in 0..3 {
            let got = vault.get_document(key("notes", "a.md")).unwrap();
            assert_eq!(got.raw.as_deref(), Some(&b"x"[..]));
        }
        assert!(
            vault
                .get_document(key("notes", "absent.md"))
                .unwrap_err()
                .is(VaultErrorCode::NotFound)
        );
        assert_eq!(
            spawned(),
            before,
            "a read of a known collection runs no git"
        );
        // A collection the handle does not know is looked for once.
        let error = vault.get_document(key("unknown", "a")).unwrap_err();
        assert!(error.is(VaultErrorCode::NotFound));
        assert!(spawned() > before);
    }

    #[test]
    fn a_write_that_has_not_returned_is_seen_by_no_read_and_touches_no_file() {
        let scratch = Scratch::new("not-returned");
        let vault = scratch.open();
        declare(&vault, "notes", false);
        vault
            .upsert_document(upsert("notes", "old.md", "old"))
            .unwrap();
        let other = scratch.open();
        let head = scratch.out(".", &["rev-parse", "HEAD"]);
        std::fs::write(scratch.0.join("person.txt"), "mine").unwrap();
        // The call stops after the list of its paths, before the swap on
        // `HEAD`, as a kill would.
        hooks::stop_after_list();
        let error = vault
            .upsert_document(upsert("notes", "new.md", "new"))
            .unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
        assert_eq!(scratch.lock(), b"notes/new.md\0", "paths only, never bytes");
        for handle in [&vault, &other] {
            assert!(
                handle
                    .get_document(key("notes", "new.md"))
                    .unwrap_err()
                    .is(VaultErrorCode::NotFound)
            );
            assert_eq!(
                handle
                    .get_document(key("notes", "old.md"))
                    .unwrap()
                    .raw
                    .as_deref(),
                Some(&b"old"[..])
            );
        }
        assert_eq!(scratch.out(".", &["rev-parse", "HEAD"]), head);
        assert!(!scratch.0.join("notes/new.md").exists());
        assert_eq!(scratch.status("."), "?? person.txt");
        // The next write of a handle recovers the note.
        other
            .upsert_document(upsert("notes", "next.md", "next"))
            .unwrap();
        assert!(scratch.lock().is_empty());
        assert!(!scratch.0.join("notes/new.md").exists());
        assert_eq!(scratch.status("."), "?? person.txt");
    }

    #[test]
    fn a_nested_write_cut_after_the_swap_is_restored_from_the_nested_head() {
        let scratch = Scratch::new("nested-swap");
        let vault = scratch.open();
        declare(&vault, "diary", true);
        vault
            .upsert_document(upsert("diary", "kept", "kept"))
            .unwrap();
        let main = scratch.out(".", &["rev-parse", "HEAD"]);
        let nested = scratch.out(".isolated", &["rev-parse", "HEAD"]);
        // A person edits another nested file by hand.
        std::fs::write(scratch.0.join(".isolated/diary/person"), "mine").unwrap();
        hooks::stop_after_swap();
        let error = vault
            .create_document(creation("diary", "cut", "the whole write"))
            .unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
        // The commit landed in the nested repository only; its file lags.
        assert_ne!(scratch.out(".isolated", &["rev-parse", "HEAD"]), nested);
        assert_eq!(scratch.out(".", &["rev-parse", "HEAD"]), main);
        assert!(!scratch.0.join(".isolated/diary/cut").exists());
        assert_eq!(scratch.lock(), b".isolated/diary/cut\0");
        drop(vault);
        let vault = scratch.open();
        assert_eq!(
            std::fs::read(scratch.0.join(".isolated/diary/cut")).unwrap(),
            b"the whole write"
        );
        assert!(scratch.lock().is_empty());
        assert_eq!(scratch.status(".isolated"), "?? diary/person");
        assert_eq!(scratch.status("."), "");
        assert_eq!(scratch.out(".", &["rev-parse", "HEAD"]), main);
        assert_eq!(
            vault
                .get_document(key("diary", "cut"))
                .unwrap()
                .raw
                .as_deref(),
            Some(&b"the whole write"[..])
        );
    }

    #[test]
    fn a_nested_write_cut_before_the_swap_is_dropped_from_the_nested_tree() {
        let scratch = Scratch::new("nested-list");
        let vault = scratch.open();
        declare(&vault, "diary", true);
        vault
            .upsert_document(upsert("diary", "kept", "kept"))
            .unwrap();
        let nested = scratch.out(".isolated", &["rev-parse", "HEAD"]);
        hooks::stop_after_list();
        assert!(
            vault
                .create_document(creation("diary", "cut", "lost"))
                .unwrap_err()
                .is(VaultErrorCode::Storage)
        );
        assert_eq!(scratch.out(".isolated", &["rev-parse", "HEAD"]), nested);
        assert!(!scratch.0.join(".isolated/diary/cut").exists());
        vault
            .upsert_document(upsert("diary", "next", "next"))
            .unwrap();
        assert!(scratch.lock().is_empty());
        assert!(!scratch.0.join(".isolated/diary/cut").exists());
        assert_eq!(scratch.status(".isolated"), "");
    }

    #[test]
    fn a_hand_made_commit_in_the_nested_repository_aborts_the_write() {
        let scratch = Scratch::new("nested-race");
        let vault = scratch.open();
        declare(&vault, "diary", true);
        vault
            .upsert_document(upsert("diary", "kept", "kept"))
            .unwrap();
        let nested = scratch.out(".isolated", &["rev-list", "--count", "HEAD"]);
        hooks::run_before_swap(|root| {
            git()
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "Hand")
                .env("GIT_AUTHOR_EMAIL", "hand@example.test")
                .env("GIT_COMMITTER_NAME", "Hand")
                .env("GIT_COMMITTER_EMAIL", "hand@example.test")
                .arg("commit")
                .arg("--quiet")
                .arg("--allow-empty")
                .arg("--message")
                .arg("by hand")
                .run()
                .unwrap();
        });
        let error = vault
            .create_document(creation("diary", "raced", "x"))
            .unwrap_err();
        assert!(error.is(VaultErrorCode::Aborted));
        assert!(error.retryable());
        assert!(!scratch.0.join(".isolated/diary/raced").exists());
        assert!(scratch.lock().is_empty());
        let after = scratch.out(".isolated", &["rev-list", "--count", "HEAD"]);
        assert_eq!(
            after.parse::<usize>().unwrap(),
            nested.parse::<usize>().unwrap() + 1
        );
        vault
            .create_document(creation("diary", "raced", "x"))
            .unwrap();
        assert_eq!(scratch.status(".isolated"), "");
    }

    #[test]
    fn a_delete_writes_no_bytes_into_the_note_either() {
        let scratch = Scratch::new("delete-list");
        let vault = scratch.open();
        declare(&vault, "notes", false);
        vault
            .upsert_document(upsert("notes", "a.md", "secret body"))
            .unwrap();
        hooks::stop_after_list();
        let deletion = DocumentDeletion {
            document: key("notes", "a.md").into(),
            note: Some("remove".to_owned()),
            ..Default::default()
        };
        assert!(vault.delete_document(deletion).is_err());
        assert_eq!(scratch.lock(), b"notes/a.md\0");
        assert!(
            scratch.0.join("notes/a.md").is_file(),
            "the file is untouched"
        );
        vault.upsert_document(upsert("notes", "b.md", "b")).unwrap();
        assert_eq!(
            std::fs::read(scratch.0.join("notes/a.md")).unwrap(),
            b"secret body"
        );
    }

    #[test]
    fn the_message_is_the_first_note_line_and_one_trailer_per_write() {
        assert_eq!(
            first_line("\n  why it was made  \nsecond"),
            "why it was made"
        );
        assert_eq!(first_line("   "), "write Documents");
        assert_eq!(message("subject", &[]), "subject\n");
        let one = trailer("notes", "a.md", "line one\nline \"two\"");
        assert_eq!(
            one,
            r#"Scheda-Note: {"collection":"notes","id":"a.md","note":"line one\nline \"two\""}"#
        );
        assert!(!one.contains('\n'));
        assert_eq!(
            message("subject", &[one.clone(), trailer("log", "e", "n")]),
            format!(
                "subject\n\n{one}\nScheda-Note: {{\"collection\":\"log\",\"id\":\"e\",\"note\":\"n\"}}\n"
            )
        );
    }
}
