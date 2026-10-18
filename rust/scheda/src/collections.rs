//! Collection calls of an open Vault: list, get, create, update,
//! delete, and several of them as one batch.
//!
//! A write is checked first with what does not depend on the state of the
//! Vault, and fails `INVALID_ARGUMENT` before any lock is taken. Then it
//! takes the write lock, checks the rest against the declarations as the
//! Vault holds them, and makes one commit that holds every declaration file
//! it changes.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use buffa_types::google::protobuf::Empty;
use protocontract::BoxIter;
use scheda_api::v1::collection_operation::Operation;
use scheda_api::v1::collection_result::Result as ResultArm;
use scheda_api::v1::{
    CollectionBatchResult, CollectionDeclaration, CollectionDeletion, CollectionKey,
    CollectionOperation, CollectionResult, Collections, VaultErrorCode,
};

use crate::collection::{self, Declaration, Declarations, Kind};
use crate::commit::Change;
use crate::documents::Walk;
use crate::error::{Context, Error, Fault, io};
use crate::fs::restrict_dir;
use crate::repo::{NodeKind, Repo, Repos, Snap};
use crate::stored::{self, Unknowns};
use crate::{isolated, journal};

/// One write of a collection, as the single calls and the batch carry it.
enum Write {
    Create(CollectionDeclaration),
    Update(CollectionDeclaration),
    Delete(CollectionDeletion),
}

/// What a write returns.
enum Outcome {
    Collection(CollectionDeclaration),
    Deleted,
}

/// Directory changes that follow a commit: the places of isolated
/// collections, which git does not hold.
#[derive(Default)]
struct Places {
    remove: Vec<std::path::PathBuf>,
    make: Vec<std::path::PathBuf>,
}

impl Write {
    /// The checks that do not depend on the state of the Vault.
    fn check(&self) -> Result<(), Fault> {
        match self {
            Write::Create(collection) | Write::Update(collection) => collection::check(collection),
            Write::Delete(deletion) => stored::validate(deletion),
        }
    }

    fn from_operation(operation: CollectionOperation) -> Result<Write, Fault> {
        match operation.operation {
            Some(Operation::Create(collection)) => Ok(Write::Create(*collection)),
            Some(Operation::Update(collection)) => Ok(Write::Update(*collection)),
            Some(Operation::Delete(deletion)) => Ok(Write::Delete(*deletion)),
            None => Err(Fault::new(
                VaultErrorCode::InvalidArgument,
                "the operation names no write",
            )),
        }
    }
}

impl crate::vault::Vault {
    /// Every declared collection, isolated ones included, in name order.
    ///
    /// Each one is as stored, with its defaults filled in. The declarations
    /// are those of the commit `HEAD` names now: a collection another handle
    /// or process declared is listed, one a person has not committed is not.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when a collection file is not valid; `STORAGE` or
    /// `REPLICATION` when git cannot be read.
    pub(crate) fn list_collections(&self, empty: Empty) -> Result<Collections, Error> {
        let _ = empty;
        let declarations = self.declarations_now().context("list the collections")?;
        Ok(Collections {
            collections: declarations
                .iter()
                .map(|declaration| declaration.collection.clone())
                .collect(),
            ..Default::default()
        })
    }

    /// One collection, as stored, with its defaults filled in.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for a key that breaks the rules of the contract;
    /// `NOT_FOUND` for a collection that is not declared.
    pub(crate) fn get_collection(
        &self,
        collection_key: CollectionKey,
    ) -> Result<CollectionDeclaration, Error> {
        stored::validate(&collection_key).context("get a collection")?;
        let name = collection_key.name.as_deref().unwrap_or("");
        let declarations = self.declarations_now().context("get a collection")?;
        match declarations.get(name) {
            Some(declaration) => Ok(declaration.collection.clone()),
            None => Err(not_found(name)),
        }
    }

    /// Declares a collection and returns it as stored, defaults filled in.
    ///
    /// The declaration is the file `{name}/.collection.yaml`, written with
    /// what the caller set and no default, in one commit. An isolated
    /// collection also gets its place under `{vault}/.isolated/`. The
    /// request is checked (`INVALID_ARGUMENT`, a slug for the name) before
    /// the write lock is taken.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT`; `ALREADY_EXISTS` for a name that is declared;
    /// `ABORTED` (retryable) when the write lock is not free within
    /// `write_timeout_ms`, or `HEAD` moved under the write; nothing is
    /// written then. `STORAGE` or `REPLICATION` when a file or git fails.
    pub(crate) fn create_collection(
        &self,
        collection: CollectionDeclaration,
    ) -> Result<CollectionDeclaration, Error> {
        self.write_one(Write::Create(collection))
    }

    /// Changes the schema of an existing collection and returns it as
    /// stored, defaults filled in.
    ///
    /// The file is rewritten in its own format with what the caller
    /// declared, and no default the caller did not set; the keys of the file
    /// that this version does not know stay as they were. The kind,
    /// `isolated`, and `encrypted` change only while the collection holds no
    /// Document.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT`; `NOT_FOUND` for a name that is not declared;
    /// `FAILED_PRECONDITION` for a change of the kind, `isolated`, or
    /// `encrypted` of a collection that holds Documents; `ABORTED` as for
    /// [`create_collection`](Self::create_collection).
    pub(crate) fn update_collection(
        &self,
        collection: CollectionDeclaration,
    ) -> Result<CollectionDeclaration, Error> {
        self.write_one(Write::Update(collection))
    }

    /// Deletes an empty collection: its declaration file and its directory
    /// (and its place under `{vault}/.isolated/`). The name can be declared
    /// again afterwards.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT`; `FAILED_PRECONDITION` for a collection that holds
    /// Documents; `NOT_FOUND` for a name that is not declared, unless
    /// `ignore_missing` is set, which makes it a success that stores
    /// nothing; `ABORTED` as for
    /// [`create_collection`](Self::create_collection).
    pub(crate) fn delete_collection(
        &self,
        collection_deletion: CollectionDeletion,
    ) -> Result<Empty, Error> {
        self.write_one(Write::Delete(collection_deletion))
            .map(|_| Empty::default())
    }

    /// Several collection writes as one: all happen, in one commit, or none.
    ///
    /// The stream is read whole first and no lock is held while it is read.
    /// An `Err` item, an item that breaks a rule, or a write that fails, and
    /// nothing is written; the error names the operation by its index. The
    /// operations are checked in order, each against the state the ones
    /// before it leave. One result per operation, in order.
    ///
    /// # Errors
    ///
    /// The errors of the single calls, and the `Err` of an item.
    pub(crate) fn batch_collections(
        &self,
        collection_operations: BoxIter<'static, Result<CollectionOperation, Error>>,
    ) -> Result<CollectionBatchResult, Error> {
        let mut writes = Vec::new();
        for (index, item) in collection_operations.enumerate() {
            let write = item
                .and_then(|item| Write::from_operation(item).map_err(Error::from))
                .with_context(|| format!("operation {index}"))?;
            writes.push(write);
        }
        let outcomes = self.write(&writes, true)?;
        Ok(CollectionBatchResult {
            results: outcomes
                .into_iter()
                .map(|outcome| CollectionResult {
                    result: Some(match outcome {
                        Outcome::Collection(collection) => ResultArm::from(collection),
                        Outcome::Deleted => ResultArm::from(Empty::default()),
                    }),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
    }

    /// One write, as the single calls make it.
    fn write_one(&self, write: Write) -> Result<CollectionDeclaration, Error> {
        let mut outcomes = self.write(&[write], false)?;
        match outcomes.pop() {
            Some(Outcome::Collection(collection)) => Ok(collection),
            Some(Outcome::Deleted) => Ok(CollectionDeclaration::default()),
            None => Err(Fault::new(VaultErrorCode::Storage, "the write returned nothing").into()),
        }
    }

    /// The writes as one commit. `indexed` puts the position of a failing
    /// write into the error.
    fn write(&self, writes: &[Write], indexed: bool) -> Result<Vec<Outcome>, Error> {
        let (outcomes, pushable) = self.write_locked(writes, indexed)?;
        // The write lock is let go: a push waits for no writer.
        if pushable {
            self.push_after_write();
        }
        Ok(outcomes)
    }

    /// The writes as one commit under the write lock, which is let go when
    /// this returns. Also says whether a commit was made: only then is there
    /// anything to push.
    fn write_locked(&self, writes: &[Write], indexed: bool) -> Result<(Vec<Outcome>, bool), Error> {
        for (index, write) in writes.iter().enumerate() {
            write
                .check()
                .map_err(Error::from)
                .with_context(|| frame(indexed, index))?;
        }
        if writes.is_empty() {
            return Ok((Vec::new(), false));
        }
        let lock = self.committer().lock().context("take the write lock")?;
        let snap = lock.snapshot(Repo::Main)?;
        let mut declarations = Declarations::clone(&*self.declarations_in(&snap)?);
        let root = self.path();
        let mut changes: BTreeMap<String, Change> = BTreeMap::new();
        let mut places = Places::default();
        let mut lines = Vec::new();
        let mut outcomes = Vec::with_capacity(writes.len());
        for (index, write) in writes.iter().enumerate() {
            let outcome = apply(
                self.repos(),
                &snap,
                write,
                &mut declarations,
                &mut changes,
                &mut places,
                &mut lines,
            )
            .with_context(|| frame(indexed, index))?;
            outcomes.push(outcome);
        }
        let changes: Vec<Change> = changes.into_values().collect();
        let head = lock.commit_in(&snap, &message(&lines), &changes)?;
        places.finish(root)?;
        let pushable = head != snap.commit();
        self.remember(head, Arc::new(declarations))?;
        Ok((outcomes, pushable))
    }
}

fn frame(indexed: bool, index: usize) -> String {
    if indexed {
        format!("operation {index}")
    } else {
        String::from("write a collection")
    }
}

/// Applies one write to the in-memory `declarations` and records what it
/// changes in the work tree. `snap` is the commit the write is checked
/// against and built on.
fn apply(
    repos: &Repos,
    snap: &Snap,
    write: &Write,
    declarations: &mut Declarations,
    changes: &mut BTreeMap<String, Change>,
    places: &mut Places,
    lines: &mut Vec<String>,
) -> Result<Outcome, Error> {
    let root = repos.root();
    match write {
        Write::Create(collection) => {
            let name = collection.name.as_deref().unwrap_or("");
            if declarations.get(name).is_some() {
                return Err(Fault::new(
                    VaultErrorCode::AlreadyExists,
                    format!("the collection {name} is already declared"),
                )
                .into());
            }
            refuse_file_in_the_way(snap, name)?;
            let file = collection::new_file(name);
            let (bytes, declaration) =
                collection::declare_new(collection, &Unknowns::none(), file)?;
            changes.insert(
                declaration.file.rel.clone(),
                Change::put(declaration.file.rel.clone(), bytes),
            );
            if declaration.isolated() {
                places.make.push(isolated::records_dir(root, &declaration));
            }
            lines.push(format!(
                "create collection {name} ({})",
                describe(&declaration)
            ));
            let reply = declaration.collection.clone();
            declarations.insert(declaration);
            Ok(Outcome::Collection(reply))
        }
        Write::Update(collection) => {
            let name = collection.name.as_deref().unwrap_or("");
            let existing = declarations.get(name).ok_or_else(|| not_found(name))?;
            let (bytes, declaration) =
                collection::declare_new(collection, &existing.unknown, existing.file.clone())?;
            let moves = declaration.kind() != existing.kind()
                || declaration.isolated() != existing.isolated()
                || declaration.encrypted() != existing.encrypted();
            if moves && holds_documents(repos, existing)? {
                return Err(Fault::new(
                    VaultErrorCode::FailedPrecondition,
                    format!(
                        "the collection {name} holds Documents; its kind, isolated, and \
                         encrypted change only while it is empty"
                    ),
                )
                .into());
            }
            if declaration.isolated() != existing.isolated() {
                places.remove.push(isolated::records_dir(root, existing));
                places.make.push(isolated::records_dir(root, &declaration));
            }
            changes.insert(
                declaration.file.rel.clone(),
                Change::put(declaration.file.rel.clone(), bytes),
            );
            lines.push(format!(
                "update collection {name} ({})",
                describe(&declaration)
            ));
            let reply = declaration.collection.clone();
            declarations.insert(declaration);
            Ok(Outcome::Collection(reply))
        }
        Write::Delete(deletion) => {
            let name = deletion.name.as_deref().unwrap_or("");
            let Some(existing) = declarations.get(name) else {
                if deletion.ignore_missing.unwrap_or(false) {
                    return Ok(Outcome::Deleted);
                }
                return Err(not_found(name));
            };
            if holds_documents(repos, existing)? {
                return Err(Fault::new(
                    VaultErrorCode::FailedPrecondition,
                    format!("the collection {name} holds Documents; delete them first"),
                )
                .into());
            }
            changes.insert(
                existing.file.rel.clone(),
                Change::Remove {
                    path: existing.file.rel.clone(),
                },
            );
            places.remove.push(root.join(name));
            if existing.isolated() {
                places.remove.push(isolated::records_dir(root, existing));
            }
            lines.push(format!("delete collection {name}"));
            declarations.remove(name);
            Ok(Outcome::Deleted)
        }
    }
}

fn not_found(name: &str) -> Error {
    Fault::new(
        VaultErrorCode::NotFound,
        format!("the collection {name} is not declared"),
    )
    .into()
}

/// A file named like the new collection's directory is in the way.
fn refuse_file_in_the_way(snap: &Snap, name: &str) -> Result<(), Error> {
    match snap.lookup(name) {
        Ok(Some(node)) if node.kind != NodeKind::Dir => Err(Fault::new(
            VaultErrorCode::AlreadyExists,
            format!("a file named {name} is in the way"),
        )
        .with_path(name)
        .into()),
        Ok(_) => Ok(()),
        Err(fault) => Err(fault.into()),
    }
}

/// Whether `declaration` has any Document, or any journal segment, in the
/// commit `HEAD` of its repository names now.
fn holds_documents(repos: &Repos, declaration: &Declaration) -> Result<bool, Error> {
    let snap = repos.snapshot(declaration.repo())?;
    if declaration.kind() == Kind::Journal {
        return Ok(!journal::list_segments(&snap, declaration)?.is_empty());
    }
    let mut walk = Walk::new(&snap, declaration, None)?;
    Ok(walk.next_id()?.is_some())
}

/// The kind of a declaration, with `isolated` and `encrypted` when set.
fn describe(declaration: &Declaration) -> String {
    let kind = match declaration.kind() {
        Kind::Standard => "standard",
        Kind::Journal => "journal",
        Kind::Singleton => "singleton",
    };
    let mut out = String::from(kind);
    if declaration.isolated() {
        out.push_str(", isolated");
    }
    if declaration.encrypted() {
        out.push_str(", encrypted");
    }
    out
}

/// The commit message of a collection write: Scheda's own text. One write
/// is its line; a batch has a first line that counts the writes, and lists
/// them.
fn message(lines: &[String]) -> String {
    match lines {
        [] => String::from("change no collection"),
        [line] => line.clone(),
        many => {
            let mut out = format!("change {} collections\n", many.len());
            for line in many {
                out.push_str("\n- ");
                out.push_str(line);
            }
            out
        }
    }
}

impl Places {
    /// Removes the directories of deleted collections that hold no file,
    /// then makes the places of isolated ones, owner-only.
    fn finish(self, root: &Path) -> Result<(), Error> {
        for dir in &self.remove {
            remove_empty_tree(dir)?;
        }
        for dir in &self.make {
            if let Ok(rest) = dir.strip_prefix(root) {
                let mut current = root.to_path_buf();
                for part in rest.components() {
                    current.push(part);
                    fs::create_dir_all(&current)
                        .map_err(io)
                        .context("create the place of an isolated collection")?;
                    restrict_dir(&current)?;
                }
            }
        }
        Ok(())
    }
}

/// Removes `dir` with the directories below it, if no file is below it. A
/// directory that holds a file stays, and so does everything above it.
fn remove_empty_tree(dir: &Path) -> Result<(), Error> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(io(err)).context("read a collection directory"),
    };
    let mut empty = true;
    for entry in entries {
        let entry = entry.map_err(io).context("read a collection directory")?;
        let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if is_dir {
            remove_empty_tree(&entry.path())?;
        }
        if !is_dir || entry.path().exists() {
            empty = false;
        }
    }
    if empty {
        match fs::remove_dir(dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io(err)).context("remove a collection directory"),
        }
    }
    Ok(())
}
