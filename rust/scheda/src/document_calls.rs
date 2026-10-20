//! Document calls of an open Vault: read one Document, and write them singly
//! or as a batch, within one collection or across them.
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
use std::path::PathBuf;
use std::sync::Arc;

use buffa::MessageField;
use buffa_types::google::protobuf::Empty;
use protocontract::BoxIter;
use scheda_api::v1::document_operation::Operation;
use scheda_api::v1::document_result::Result as ResultArm;
use scheda_api::v1::{
    Document, DocumentBatchResult, DocumentCreation, DocumentDeletion, DocumentId,
    DocumentOperation, DocumentResult, DocumentUpdate, DocumentUpsert, DocumentWriteResult,
    Encryption, VaultDocumentOperation, VaultErrorCode,
};
use serde_json::json;

use scheda_blocking::Blocking;

use crate::collection::{Declaration, Declarations, Kind};
use crate::commit::{Change, Held};
use crate::documents::{self, Found, Plain};
use crate::error::{Context, Error, Fault};
use crate::id::require_slug;
use crate::idmap::{IdMaps, Lookup};
use crate::repo::{Repo, Snap};
use crate::{isolated, journal, stored};

/// One write of a Document, as the single calls and the batches carry it:
/// the collection it is in, and the request of the call.
pub(crate) struct Write {
    collection: String,
    request: Request,
}

/// The writes of one call, checked or not yet: one write, or a batch read
/// whole. A call that waits for the write lock somewhere else checks them
/// first ([`Vault::precheck_documents`](crate::vault::Vault::precheck_documents)) and hands them
/// back to be written, so no request is copied.
pub(crate) struct Writes {
    writes: Vec<Write>,
    /// Whether the position of a failing write is part of its error.
    indexed: bool,
}

impl Writes {
    /// Whether the call holds no write at all: an empty batch.
    pub(crate) fn is_empty(&self) -> bool {
        self.writes.is_empty()
    }

    /// The one write `request` asks for in `collection`.
    pub(crate) fn one(collection: &str, request: impl Into<Request>) -> Writes {
        Writes {
            writes: vec![Write::new(collection, request.into())],
            indexed: false,
        }
    }

    /// The writes of a batch in `collection`, read whole.
    ///
    /// # Errors
    ///
    /// The `Err` of an item, and an operation that names no write, with its
    /// index.
    pub(crate) fn of_collection(
        collection: &str,
        operations: BoxIter<'static, Result<DocumentOperation, Error>>,
    ) -> Result<Writes, Error> {
        Writes::collect(operations.map(|item| {
            item.and_then(|item| {
                Write::from_operation(collection.to_owned(), item).map_err(Error::from)
            })
        }))
    }

    /// The writes of a batch across collections, read whole.
    ///
    /// # Errors
    ///
    /// As [`Writes::of_collection`].
    pub(crate) fn of_vault(
        operations: BoxIter<'static, Result<VaultDocumentOperation, Error>>,
    ) -> Result<Writes, Error> {
        Writes::collect(operations.map(|item| {
            item.and_then(|item| Write::from_vault_operation(item).map_err(Error::from))
        }))
    }

    fn collect(writes: impl Iterator<Item = Result<Write, Error>>) -> Result<Writes, Error> {
        let mut all = Vec::new();
        for (index, write) in writes.enumerate() {
            all.push(write.with_context(|| format!("operation {index}"))?);
        }
        Ok(Writes {
            writes: all,
            indexed: true,
        })
    }
}

/// The request of one Document write.
pub(crate) enum Request {
    Create(DocumentCreation),
    Update(DocumentUpdate),
    Upsert(DocumentUpsert),
    Delete(DocumentDeletion),
}

impl From<DocumentCreation> for Request {
    fn from(request: DocumentCreation) -> Request {
        Request::Create(request)
    }
}

impl From<DocumentUpdate> for Request {
    fn from(request: DocumentUpdate) -> Request {
        Request::Update(request)
    }
}

impl From<DocumentUpsert> for Request {
    fn from(request: DocumentUpsert) -> Request {
        Request::Upsert(request)
    }
}

impl From<DocumentDeletion> for Request {
    fn from(request: DocumentDeletion) -> Request {
        Request::Delete(request)
    }
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

/// What a call of writes did, once it is stored.
struct Written {
    /// One outcome per write, in order.
    outcomes: Vec<Outcome>,
    /// The state token of the Vault once the call is stored, read under the
    /// write lock the call held; of the state the call found when it stored
    /// nothing. A call with no write at all takes no lock and reads the
    /// state now ([`Vault::state_now`](crate::vault::Vault::state_now)).
    version: String,
}

fn invalid(message: impl Into<String>) -> Fault {
    Fault::new(VaultErrorCode::InvalidArgument, message)
}

fn storage(message: &str) -> Fault {
    Fault::new(VaultErrorCode::Storage, message)
}

impl Write {
    fn new(collection: impl Into<String>, request: Request) -> Write {
        Write {
            collection: collection.into(),
            request,
        }
    }

    /// The write of one item of a batch in `collection`.
    fn from_operation(collection: String, operation: DocumentOperation) -> Result<Write, Fault> {
        let request = match operation.operation {
            Some(Operation::Create(write)) => Request::Create(*write),
            Some(Operation::Update(write)) => Request::Update(*write),
            Some(Operation::Upsert(write)) => Request::Upsert(*write),
            Some(Operation::Delete(write)) => Request::Delete(*write),
            None => return Err(invalid("the operation names no write")),
        };
        Ok(Write::new(collection, request))
    }

    /// The write of one item of a batch across collections.
    fn from_vault_operation(item: VaultDocumentOperation) -> Result<Write, Fault> {
        stored::validate(&item)?;
        let collection = item.collection.unwrap_or_default();
        let operation = item.operation.into_option().unwrap_or_default();
        Write::from_operation(collection, operation)
    }

    /// The collection and the id the write is about.
    fn key(&self) -> (&str, &str) {
        let id = match &self.request {
            Request::Create(write) => &write.id,
            Request::Update(write) => &write.id,
            Request::Upsert(write) => &write.id,
            Request::Delete(write) => &write.id,
        };
        (&self.collection, id.as_deref().unwrap_or(""))
    }

    fn raw(&self) -> &[u8] {
        let raw = match &self.request {
            Request::Create(write) => &write.raw,
            Request::Update(write) => &write.raw,
            Request::Upsert(write) => &write.raw,
            Request::Delete(_) => &None,
        };
        raw.as_deref().unwrap_or(&[])
    }

    fn note(&self) -> &str {
        let note = match &self.request {
            Request::Create(write) => &write.note,
            Request::Update(write) => &write.note,
            Request::Upsert(write) => &write.note,
            Request::Delete(write) => &write.note,
        };
        note.as_deref().unwrap_or("")
    }

    /// The rules of the contract: the request validates (an `id` and a
    /// `note`, not empty).
    fn validate(&self) -> Result<(), Fault> {
        match &self.request {
            Request::Create(write) => stored::validate(write),
            Request::Update(write) => stored::validate(write),
            Request::Upsert(write) => stored::validate(write),
            Request::Delete(write) => stored::validate(write),
        }
    }

    /// Takes the request apart, moving its bytes out.
    fn into_step(self) -> Step {
        let collection = self.collection;
        match self.request {
            Request::Create(write) => Step {
                verb: Verb::Create {
                    keep_existing: write.keep_existing.unwrap_or(false),
                },
                collection,
                id: write.id.unwrap_or_default(),
                raw: write.raw.unwrap_or_default(),
                note: write.note.unwrap_or_default(),
            },
            Request::Update(write) => Step {
                verb: Verb::Update {
                    revision: write.revision,
                },
                collection,
                id: write.id.unwrap_or_default(),
                raw: write.raw.unwrap_or_default(),
                note: write.note.unwrap_or_default(),
            },
            Request::Upsert(write) => Step {
                verb: Verb::Upsert,
                collection,
                id: write.id.unwrap_or_default(),
                raw: write.raw.unwrap_or_default(),
                note: write.note.unwrap_or_default(),
            },
            Request::Delete(write) => Step {
                verb: Verb::Delete {
                    ignore_missing: write.ignore_missing.unwrap_or(false),
                    revision: write.revision,
                },
                collection,
                id: write.id.unwrap_or_default(),
                raw: Vec::new(),
                note: write.note.unwrap_or_default(),
            },
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
            if !matches!(write.request, Request::Create(_)) {
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

impl crate::vault::Vault {
    /// One Document of `collection`, read from the commit `HEAD` names, not
    /// from the Index and not from the work tree.
    ///
    /// A `standard` or `singleton` Document is the blob at its path in the
    /// tree of that commit, opened for an encrypted collection; a journal
    /// line is found by a scan of the segments. `raw` is always set.
    /// `revision` is the id of the blob, in lowercase hex (what `git
    /// rev-parse HEAD:path` prints; the blob of the armor, for an encrypted
    /// collection); a journal line has none. A person's uncommitted edit is
    /// not seen, and a commit made by hand is seen at once.
    ///
    /// A journal line is looked up in the id map the indexer keeps; a line
    /// the map does not list yet is looked for in the segments that changed
    /// since the map was made, so a line written a moment ago is found
    /// before the indexer has run. Only a journal with no map is scanned
    /// whole.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for an id that breaks the rules of the contract;
    /// `NOT_FOUND` for an unknown collection or Document; `CRYPTO` for an
    /// encrypted collection when the Vault has no secret or the wrong one;
    /// `UNIMPLEMENTED` for an encrypted collection in a build without the
    /// `encrypt` feature; `STORAGE` when a file cannot be read.
    pub(crate) fn get(
        &self,
        _cx: &Blocking,
        collection: &str,
        document_id: DocumentId,
    ) -> Result<Document, Error> {
        stored::validate(&document_id).context("get a Document")?;
        let id = document_id.id.unwrap_or_default();
        let declarations = self.declarations_now().context("get a Document")?;
        let decl = declarations.get(collection).ok_or_else(|| {
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
        let snap = self
            .repos()
            .snapshot(decl.repo())
            .context("get a Document")?;
        if decl.kind() == Kind::Journal {
            require_slug("id", &id).context("get a Document")?;
            let line = self
                .id_maps()
                .find(&snap, decl, &id)
                .context("get a Document")?
                .ok_or_else(absent)?;
            return Ok(Document {
                collection: Some(collection.to_owned()),
                id: Some(id),
                raw: Some(line.payload),
                ..Default::default()
            });
        }
        let found = documents::read_found(&snap, decl, &id, self.encryption())
            .context("get a Document")?
            .ok_or_else(absent)?;
        let revision = found.revision();
        Ok(Document {
            collection: Some(collection.to_owned()),
            id: Some(id),
            raw: Some(found.into_plain()),
            revision: Some(revision),
            ..Default::default()
        })
    }

    /// The reply of a batch: the results in order and the state token.
    fn batch_result(written: Written) -> DocumentBatchResult {
        DocumentBatchResult {
            version: Some(written.version),
            results: written
                .outcomes
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
        }
    }

    /// The checks of `writes` that need no lock, as the write makes them
    /// before it waits for the lock: each request validates, the
    /// collections are declared and take the operation, the ids and the
    /// payloads are well formed, encryption is available, and one batch
    /// holds the Documents of one repository. The writes come back as they
    /// went in, to be written under the lock with
    /// [`write_document_held`](Self::write_document_held) or
    /// [`write_documents_held`](Self::write_documents_held), which check again
    /// against the declarations in force then.
    ///
    /// # Errors
    ///
    /// The errors of the write calls that need no lock: `INVALID_ARGUMENT`,
    /// `NOT_FOUND`, `UNIMPLEMENTED`, `CRYPTO`.
    pub(crate) fn precheck_documents(
        &self,
        _cx: &Blocking,
        writes: Writes,
    ) -> Result<Writes, Error> {
        if !writes.writes.is_empty() {
            self.inspect_writes(&writes.writes, writes.indexed)?;
        }
        Ok(writes)
    }

    /// The one write of `writes`, as the single calls make it, under the
    /// write lock `held`, which the caller waited for: the Document as
    /// stored (none after a deletion) and the state token of the write, and
    /// whether a commit of the main repository was made (only then is
    /// there anything to push).
    pub(crate) fn write_document_held(
        &self,
        cx: &Blocking,
        held: Held,
        writes: Writes,
    ) -> Result<(DocumentWriteResult, bool), Error> {
        let Some(write) = writes.writes.into_iter().next() else {
            return Err(storage("the write holds no request").into());
        };
        let (mut written, pushable) = self.write_documents(cx, held, vec![write], false)?;
        let document = match written.outcomes.pop() {
            Some(Outcome::Document(document)) => document.into(),
            Some(Outcome::Deleted) => MessageField::none(),
            None => return Err(storage("the write returned nothing").into()),
        };
        let result = DocumentWriteResult {
            document,
            version: Some(written.version),
            ..Default::default()
        };
        Ok((result, pushable))
    }

    /// The batch `writes` as one commit, under the write lock `held`, which
    /// the caller waited for: all happen, or none. The operations are
    /// applied in order, each against the state the ones before it leave:
    /// two operations on one Document see each other. A batch that changes
    /// nothing makes no commit. One call writes the Documents of the main
    /// tree or those of isolated collections, never both
    /// (`INVALID_ARGUMENT`). Also says whether a commit of the main
    /// repository was made.
    pub(crate) fn write_documents_held(
        &self,
        cx: &Blocking,
        held: Held,
        writes: Writes,
    ) -> Result<(DocumentBatchResult, bool), Error> {
        let (written, pushable) = self.write_documents(cx, held, writes.writes, writes.indexed)?;
        Ok((Self::batch_result(written), pushable))
    }

    /// The batch `writes` when it holds no write: it stores nothing, takes
    /// no lock, and answers with the state now, read as `Vault.Info` reads
    /// it.
    pub(crate) fn write_no_documents(
        &self,
        cx: &Blocking,
        writes: Writes,
    ) -> Result<DocumentBatchResult, Error> {
        let _ = writes;
        Ok(Self::batch_result(Written {
            outcomes: Vec::new(),
            version: self.state_now(cx)?.token(),
        }))
    }

    /// Validates every write and inspects them against the declarations now.
    fn inspect_writes(
        &self,
        writes: &[Write],
        indexed: bool,
    ) -> Result<(Vec<Inspected>, Repo, Arc<Declarations>), Error> {
        for (index, write) in writes.iter().enumerate() {
            write
                .validate()
                .map_err(Error::from)
                .with_context(|| frame(indexed, index))?;
        }
        let before = self.declarations_now()?;
        let (inspected, repo) = inspect_all(writes, &before, self.encryption(), indexed)?;
        Ok((inspected, repo, before))
    }

    /// The writes as one commit under the write lock `held`, which is let
    /// go when this returns. The state token of the call is read under that
    /// lock: the commit the call made in its repository and `HEAD` of the
    /// other one, or, when the call stored nothing, the state its checks
    /// found. Also says whether a commit of the main repository was made:
    /// only then is there anything to push.
    fn write_documents(
        &self,
        cx: &Blocking,
        held: Held,
        writes: Vec<Write>,
        indexed: bool,
    ) -> Result<(Written, bool), Error> {
        let encryption = self.encryption();
        let (mut inspected, mut repo, before) = self.inspect_writes(&writes, indexed)?;
        let first_note = first_line(writes.first().map_or("", Write::note));
        let lock = self.lock(cx, held)?;
        let main = lock.snapshot(Repo::Main)?;
        let declarations = self.declarations_in(&main)?;
        if !Arc::ptr_eq(&before, &declarations) {
            // The collections changed since the checks: do them again
            // against the ones that are in force now.
            (inspected, repo) = inspect_all(&writes, &declarations, encryption, indexed)?;
        }
        // The checks and the commit read one state of the repository the
        // write goes to: this snapshot, taken under the lock.
        let mut snap = match repo {
            Repo::Main => main,
            Repo::Isolated => {
                drop(main);
                lock.snapshot(Repo::Isolated)?
            }
        };
        let mut ledger = Ledger::new(
            &snap,
            &declarations,
            self.id_maps(),
            encryption,
            self.writer(),
        );
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
            let outcomes = drafts
                .into_iter()
                .map(|draft| draft.settle(None))
                .collect::<Result<_, _>>()?;
            // Nothing is stored: the state the checks found.
            let version = lock.state_after(repo, snap.commit())?.token();
            return Ok((Written { outcomes, version }, false));
        }
        if repo == Repo::Isolated {
            isolated::ensure(self.path())?;
            if snap.repository().is_none() {
                // The nested repository was made just now: it is empty.
                snap = lock.snapshot(Repo::Isolated)?;
            }
            let mut with_ignore = isolated::gitignore_change(&snap)?;
            with_ignore.append(&mut changes);
            changes = with_ignore;
        }
        let before = snap.commit();
        let head = lock.commit_in(&snap, &message(&first_note, &trailers), &changes)?;
        let pushable = repo == Repo::Main && head != before;
        if repo == Repo::Main {
            self.remember(head, declarations)?;
        }
        // The commit this call made (or, if the tree did not change, the one
        // it found), and `HEAD` of the other repository, under the lock.
        let version = lock.state_after(repo, head)?.token();
        let mut files = take_files(&drafts, changes);
        let outcomes = drafts
            .into_iter()
            .map(|draft| draft.settle(Some(&mut files)))
            .collect::<Result<_, _>>()?;
        Ok((Written { outcomes, version }, pushable))
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

/// The note of the Document `id` of `collection` in the message of a commit:
/// that of the last `Scheda-Note:` trailer that names them. `None` for a
/// commit without one, a commit made by hand, say.
pub(crate) fn note_of(message: &str, collection: &str, id: &str) -> Option<String> {
    message
        .lines()
        .filter_map(|line| line.strip_prefix("Scheda-Note: "))
        .filter_map(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .filter_map(|value| {
            let names =
                |key: &str, wanted: &str| value.get(key).and_then(|v| v.as_str()) == Some(wanted);
            if names("collection", collection) && names("id", id) {
                value
                    .get("note")
                    .and_then(|note| note.as_str())
                    .map(str::to_owned)
            } else {
                None
            }
        })
        .next_back()
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
struct JournalState<'a> {
    /// The lines the journal had before the call, found through its id map.
    stored: Lookup<'a>,
    /// The ids this call has appended.
    appended: HashSet<String>,
    /// The whole new bytes of each segment appended to, by path.
    pending: BTreeMap<PathBuf, Vec<u8>>,
}

/// The state of a write call under the lock: the files it touches, as the
/// operations so far leave them.
struct Ledger<'a> {
    snap: &'a Snap,
    declarations: &'a Declarations,
    id_maps: &'a IdMaps,
    encryption: Option<&'a Encryption>,
    writer: &'a str,
    /// The files more than one operation of the call is about.
    repeated: HashSet<String>,
    slots: BTreeMap<String, Slot>,
    journals: BTreeMap<String, JournalState<'a>>,
    trailers: Vec<String>,
}

impl<'a> Ledger<'a> {
    fn new(
        snap: &'a Snap,
        declarations: &'a Declarations,
        id_maps: &'a IdMaps,
        encryption: Option<&'a Encryption>,
        writer: &'a str,
    ) -> Ledger<'a> {
        Ledger {
            snap,
            declarations,
            id_maps,
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
        let declarations: &'a Declarations = self.declarations;
        let Some(decl) = declarations.get(&step.collection) else {
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
    fn append(&mut self, decl: &'a Declaration, step: Step, line: &[u8]) -> Result<Draft, Error> {
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
                stored: self.id_maps.lookup(self.snap, decl),
                appended: HashSet::new(),
                pending: BTreeMap::new(),
            }),
        };
        let existing = if state.appended.contains(&step.id) {
            let line = state
                .pending
                .values()
                .find_map(|bytes| journal::find_line(bytes, &step.id));
            Some(line.ok_or_else(|| storage("a line of the journal could not be read again"))?)
        } else {
            state.stored.find(&step.id)?
        };
        if let Some(existing) = existing {
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
            return Ok(Draft::Stored {
                collection: step.collection,
                id: step.id,
                revision: None,
                raw: Raw::Value(existing.payload),
            });
        }
        let today = journal::today_utc()?;
        let path = journal::segment_for_append_in(
            self.snap,
            decl,
            line.len() as u64,
            self.writer,
            &today,
            &state.pending,
        )?;
        let content = match state.pending.entry(path.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(read_segment(self.snap, &path)?),
        };
        content.extend_from_slice(line);
        state.appended.insert(step.id.clone());
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
            let original = documents::read_found(self.snap, decl, &step.id, self.encryption)
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
            let bytes = documents::encode(decl, &id, &raw, encryption)
                .map_err(|fault| fault.with_path(path.clone()))?;
            match bytes {
                Cow::Owned(sealed) => Some(sealed),
                Cow::Borrowed(_) => None,
            }
        };
        let (found, revision, reply) = match sealed {
            Some(file) => {
                let (plain, reply) = if repeated {
                    (Plain::Opened(raw.clone()), Raw::Value(raw))
                } else {
                    (Plain::Unknown, Raw::Value(raw))
                };
                let found =
                    Found::new(file, plain).map_err(|fault| fault.with_path(path.clone()))?;
                let revision = found.revision();
                (found, revision, reply)
            }
            None => {
                let reply = if repeated {
                    Raw::Value(raw.clone())
                } else {
                    Raw::File(path.clone())
                };
                let found =
                    Found::new(raw, Plain::Same).map_err(|fault| fault.with_path(path.clone()))?;
                let revision = found.revision();
                (found, revision, reply)
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
                changes.push(Change::put(journal::rel_str(&path), bytes));
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

/// The bytes of a journal segment in the commit of `snap`, empty when it is
/// not there yet.
fn read_segment(snap: &Snap, path: &std::path::Path) -> Result<Vec<u8>, Error> {
    let rel = journal::rel_str(path);
    Ok(snap
        .read(&rel)
        .map_err(|fault| fault.with_path(rel))?
        .map(|blob| blob.bytes)
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use scheda_api::v1::{
        Document, DocumentCreation, DocumentDeletion, DocumentId, DocumentOperation,
        DocumentUpsert, OpenOptions, VaultErrorCode,
    };

    use super::{Operation, first_line, message, note_of, trailer};
    use crate::commit::hooks;
    use crate::error::Error;
    use crate::git::{git, spawned_in};
    use crate::testing::{batch, create, create_collection, delete, open, step, upsert as write};
    use crate::vault::Vault;

    /// A Vault on disk made by `Vault::open`, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-docs-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            crate::testing::remove_when_the_test_ends(&dir);
            Scratch(dir)
        }

        async fn open(&self) -> Arc<Vault> {
            open(&self.0, OpenOptions::default().with_write_timeout_ms(2000))
                .await
                .unwrap()
        }

        fn canonical(&self) -> PathBuf {
            self.0.canonicalize().unwrap()
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

    async fn declare(vault: &Arc<Vault>, name: &str, isolated: bool) {
        create_collection(
            vault,
            serde_json::json!({
                "name": name, "schema": {"standard": {}, "isolated": isolated}
            }),
        )
        .await
        .unwrap();
    }

    /// `Collection.Get` of `id`.
    async fn get(vault: &Arc<Vault>, collection: &str, id: DocumentId) -> Result<Document, Error> {
        let collection = collection.to_owned();
        step(vault, move |cx, vault| vault.get(cx, &collection, id)).await
    }

    fn upsert(id: &str, raw: &str) -> DocumentUpsert {
        DocumentUpsert {
            id: Some(id.to_owned()),
            raw: Some(raw.as_bytes().to_vec()),
            note: Some("test".to_owned()),
            ..Default::default()
        }
    }

    fn key(id: &str) -> DocumentId {
        DocumentId {
            id: Some(id.to_owned()),
            ..Default::default()
        }
    }

    fn creation(id: &str, raw: &str) -> DocumentCreation {
        DocumentCreation {
            id: Some(id.to_owned()),
            raw: Some(raw.as_bytes().to_vec()),
            note: Some("test".to_owned()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_read_and_a_write_without_a_mirror_start_no_process() {
        let scratch = Scratch::new("no-process");
        let vault = scratch.open().await;
        let path = scratch.canonical();
        let before = (spawned_in(&path), spawned_in(&path.join(".isolated")));
        declare(&vault, "notes", false).await;
        declare(&vault, "diary", true).await;
        write(&vault, "notes", upsert("a.md", "x")).await.unwrap();
        create(&vault, "notes", creation("b.md", "y"))
            .await
            .unwrap();
        create(&vault, "diary", creation("d", "secret"))
            .await
            .unwrap();
        let operations: Vec<DocumentOperation> = vec![
            DocumentOperation {
                operation: Some(Operation::Upsert(Box::new(upsert("c.md", "z")))),
                ..Default::default()
            },
            DocumentOperation {
                operation: Some(Operation::Delete(Box::new(DocumentDeletion {
                    id: Some("b.md".to_owned()),
                    note: Some("gone".to_owned()),
                    ..Default::default()
                }))),
                ..Default::default()
            },
        ];
        batch(&vault, "notes", operations).await.unwrap();
        for _ in 0..3 {
            let got = get(&vault, "notes", key("a.md")).await.unwrap();
            assert_eq!(got.raw.as_deref(), Some(&b"x"[..]));
        }
        assert!(
            get(&vault, "notes", key("absent.md"))
                .await
                .unwrap_err()
                .is(VaultErrorCode::NotFound)
        );
        assert!(
            get(&vault, "unknown", key("a"))
                .await
                .unwrap_err()
                .is(VaultErrorCode::NotFound)
        );
        assert_eq!(
            get(&vault, "diary", key("d")).await.unwrap().raw.as_deref(),
            Some(&b"secret"[..])
        );
        assert_eq!(
            (spawned_in(&path), spawned_in(&path.join(".isolated"))),
            before,
            "no call started a git process"
        );
    }

    #[tokio::test]
    async fn a_write_that_has_not_returned_is_seen_by_no_read_and_touches_no_file() {
        let scratch = Scratch::new("not-returned");
        let vault = scratch.open().await;
        declare(&vault, "notes", false).await;
        write(&vault, "notes", upsert("old.md", "old"))
            .await
            .unwrap();
        let other = scratch.open().await;
        let head = scratch.out(".", &["rev-parse", "HEAD"]);
        std::fs::write(scratch.0.join("person.txt"), "mine").unwrap();
        // The call stops after the list of its paths, before the swap on
        // `HEAD`, as a kill would.
        hooks::stop_after_list(&scratch.canonical());
        let error = write(&vault, "notes", upsert("new.md", "new"))
            .await
            .unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
        assert_eq!(scratch.lock(), b"notes/new.md\0", "paths only, never bytes");
        for handle in [&vault, &other] {
            assert!(
                get(handle, "notes", key("new.md"))
                    .await
                    .unwrap_err()
                    .is(VaultErrorCode::NotFound)
            );
            assert_eq!(
                get(handle, "notes", key("old.md"))
                    .await
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
        write(&other, "notes", upsert("next.md", "next"))
            .await
            .unwrap();
        assert!(scratch.lock().is_empty());
        assert!(!scratch.0.join("notes/new.md").exists());
        assert_eq!(scratch.status("."), "?? person.txt");
    }

    #[tokio::test]
    async fn a_nested_write_cut_after_the_swap_is_restored_from_the_nested_head() {
        let scratch = Scratch::new("nested-swap");
        let vault = scratch.open().await;
        declare(&vault, "diary", true).await;
        write(&vault, "diary", upsert("kept", "kept"))
            .await
            .unwrap();
        let main = scratch.out(".", &["rev-parse", "HEAD"]);
        let nested = scratch.out(".isolated", &["rev-parse", "HEAD"]);
        // A person edits another nested file by hand.
        std::fs::write(scratch.0.join(".isolated/diary/person"), "mine").unwrap();
        hooks::stop_after_swap(&scratch.canonical());
        let error = create(&vault, "diary", creation("cut", "the whole write"))
            .await
            .unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
        // The commit landed in the nested repository only; its file lags.
        assert_ne!(scratch.out(".isolated", &["rev-parse", "HEAD"]), nested);
        assert_eq!(scratch.out(".", &["rev-parse", "HEAD"]), main);
        assert!(!scratch.0.join(".isolated/diary/cut").exists());
        assert_eq!(scratch.lock(), b".isolated/diary/cut\0");
        vault.close().await.unwrap();
        let vault = scratch.open().await;
        assert_eq!(
            std::fs::read(scratch.0.join(".isolated/diary/cut")).unwrap(),
            b"the whole write"
        );
        assert!(scratch.lock().is_empty());
        assert_eq!(scratch.status(".isolated"), "?? diary/person");
        assert_eq!(scratch.status("."), "");
        assert_eq!(scratch.out(".", &["rev-parse", "HEAD"]), main);
        assert_eq!(
            get(&vault, "diary", key("cut"))
                .await
                .unwrap()
                .raw
                .as_deref(),
            Some(&b"the whole write"[..])
        );
    }

    #[tokio::test]
    async fn a_nested_write_cut_before_the_swap_is_dropped_from_the_nested_tree() {
        let scratch = Scratch::new("nested-list");
        let vault = scratch.open().await;
        declare(&vault, "diary", true).await;
        write(&vault, "diary", upsert("kept", "kept"))
            .await
            .unwrap();
        let nested = scratch.out(".isolated", &["rev-parse", "HEAD"]);
        hooks::stop_after_list(&scratch.canonical());
        assert!(
            create(&vault, "diary", creation("cut", "lost"))
                .await
                .unwrap_err()
                .is(VaultErrorCode::Storage)
        );
        assert_eq!(scratch.out(".isolated", &["rev-parse", "HEAD"]), nested);
        assert!(!scratch.0.join(".isolated/diary/cut").exists());
        write(&vault, "diary", upsert("next", "next"))
            .await
            .unwrap();
        assert!(scratch.lock().is_empty());
        assert!(!scratch.0.join(".isolated/diary/cut").exists());
        assert_eq!(scratch.status(".isolated"), "");
    }

    #[tokio::test]
    async fn a_hand_made_commit_in_the_nested_repository_aborts_the_write() {
        let scratch = Scratch::new("nested-race");
        let vault = scratch.open().await;
        declare(&vault, "diary", true).await;
        write(&vault, "diary", upsert("kept", "kept"))
            .await
            .unwrap();
        let nested = scratch.out(".isolated", &["rev-list", "--count", "HEAD"]);
        hooks::run_before_swap(&scratch.canonical(), |root| {
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
        let error = create(&vault, "diary", creation("raced", "x"))
            .await
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
        create(&vault, "diary", creation("raced", "x"))
            .await
            .unwrap();
        assert_eq!(scratch.status(".isolated"), "");
    }

    #[tokio::test]
    async fn a_delete_writes_no_bytes_into_the_note_either() {
        let scratch = Scratch::new("delete-list");
        let vault = scratch.open().await;
        declare(&vault, "notes", false).await;
        write(&vault, "notes", upsert("a.md", "secret body"))
            .await
            .unwrap();
        hooks::stop_after_list(&scratch.canonical());
        let deletion = DocumentDeletion {
            id: Some("a.md".to_owned()),
            note: Some("remove".to_owned()),
            ..Default::default()
        };
        assert!(delete(&vault, "notes", deletion).await.is_err());
        assert_eq!(scratch.lock(), b"notes/a.md\0");
        assert!(
            scratch.0.join("notes/a.md").is_file(),
            "the file is untouched"
        );
        write(&vault, "notes", upsert("b.md", "b")).await.unwrap();
        assert_eq!(
            std::fs::read(scratch.0.join("notes/a.md")).unwrap(),
            b"secret body"
        );
    }

    #[tokio::test]
    async fn the_message_is_the_first_note_line_and_one_trailer_per_write() {
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

    #[tokio::test]
    async fn the_note_of_a_document_is_the_trailer_that_names_its_collection_and_id() {
        let text = message(
            "first",
            &[
                trailer("notes", "a.md", "for a\nsecond line"),
                trailer("notes", "b.md", "for b"),
                trailer("other", "a.md", "elsewhere"),
            ],
        );
        assert_eq!(
            note_of(&text, "notes", "a.md").as_deref(),
            Some("for a\nsecond line")
        );
        assert_eq!(note_of(&text, "notes", "b.md").as_deref(), Some("for b"));
        assert_eq!(note_of(&text, "notes", "c.md"), None);
        assert_eq!(note_of("by hand\n", "notes", "a.md"), None);
        assert_eq!(
            note_of("x\n\nScheda-Note: not json\n", "notes", "a.md"),
            None
        );
    }
}
