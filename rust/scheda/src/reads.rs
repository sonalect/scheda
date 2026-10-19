//! The reads of an open Vault that return many things: `GetMany`, `History`,
//! and `Check`.
//!
//! `GetMany` and `History` are lazy streams. A stream is made in one call
//! (it checks the request, and takes the snapshot it reads, so it sees the
//! commit `HEAD` named then and no other) and is then pulled item by item:
//! the blob of a Document, or of a version, is read when the stream reaches
//! it, and nothing is collected. A stream owns its snapshot, which holds a
//! repository of the pool until the stream is dropped, and a copy of the
//! declaration of its collection. What it needs of the Vault (the secret,
//! the id maps) is passed at each pull, so the stream itself borrows
//! nothing and can be boxed as `'static`.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::vec::IntoIter;

use buffa::MessageField;
use buffa_types::google::protobuf::Timestamp;
use git2::Oid;
use scheda_api::v1::document_selection::Selection;
use scheda_api::v1::{
    Author, CheckReport, Document, DocumentHistoryEntry, DocumentId, DocumentSelection,
    VaultErrorCode,
};

use crate::collection::{Declaration, Kind};
use crate::document_calls::note_of;
use crate::documents::{self, WalkState};
use crate::error::{Context, Error, Fault};
use crate::id::{require_document_id, require_slug};
use crate::journal::LinesState;
use crate::repo::{CommitInfo, NodeKind, Snap};
use crate::vault::Vault;
use crate::{check, isolated, stored};

/// A stream of Documents: what `Collection.GetMany` returns, before the
/// gate wraps it. Pull it with [`next`](Self::next), giving the Vault it
/// was made by.
pub(crate) struct DocumentStream {
    snap: Snap,
    decl: Declaration,
    include_raw: bool,
    source: Source,
    /// How many Documents the stream may still hand out; `None` is no limit.
    left: Option<u64>,
    done: bool,
}

/// Where the Documents of a stream come from.
enum Source {
    /// The ids the caller named, in their order.
    Ids(IntoIter<String>),
    /// The Document files of a `standard` or `singleton` collection.
    Files(WalkState),
    /// The lines of a journal. `after` is the id of the line the stream
    /// begins after, until that line has been passed. `hinted`: the walk
    /// began at the segment the id map gave for that line; if the line is
    /// not there (a segment changed by hand), the walk begins again from
    /// the first segment, once.
    Lines {
        state: LinesState,
        after: Option<String>,
        hinted: bool,
    },
}

fn not_declared(collection: &str) -> Fault {
    Fault::new(
        VaultErrorCode::NotFound,
        format!("the collection {collection} is not declared"),
    )
}

impl Vault {
    /// The Documents of `collection` the selection names, as a lazy stream:
    /// the given ids in the given order (one that is not stored is skipped),
    /// or the collection in its order (`id` order for `standard` and
    /// `singleton`, append order for a journal), beginning after
    /// `start_after` and stopping after `limit` Documents.
    ///
    /// The stream reads the commit `HEAD` names when this is called. Without
    /// `include_raw` it reads no blob: a Document has its keys and its
    /// revision (the id of its blob, found in the tree), and a journal line
    /// has no revision. With it, `raw` is the Document as [`get`] gives it,
    /// opened for an encrypted collection.
    ///
    /// A `start_after` that no Document has is a place in the order for a
    /// `standard` or `singleton` collection: the stream begins at the first
    /// id after it. In a journal there is no such place, so an id that is
    /// not a line gives nothing.
    ///
    /// [`get`]: Vault::get
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for a selection that names neither `ids` nor
    /// `listing`, or an id that breaks the rules of the collection;
    /// `NOT_FOUND` for an unknown collection; `CRYPTO` or `UNIMPLEMENTED`
    /// for `include_raw` on an encrypted collection the handle cannot open.
    /// A failure while the stream is read is an item of the stream.
    pub(crate) fn get_many(
        &self,
        collection: &str,
        selection: DocumentSelection,
    ) -> Result<DocumentStream, Error> {
        stored::validate(&selection).context("Collection.GetMany: check the request")?;
        let include_raw = selection.include_raw.unwrap_or(false);
        let declarations = self.declarations_now().context("Collection.GetMany")?;
        let decl = declarations
            .get(collection)
            .ok_or_else(|| not_declared(collection))?
            .clone();
        if include_raw {
            documents::require_secret(&decl, self.encryption())
                .context("Collection.GetMany: open the collection")?;
        }
        let snap = self
            .repos()
            .snapshot(decl.repo())
            .context("Collection.GetMany")?;
        let mut left = None;
        let source = match selection.selection {
            Some(Selection::Ids(ids)) => {
                for id in &ids.ids {
                    match decl.kind() {
                        Kind::Standard => require_document_id(id),
                        Kind::Singleton | Kind::Journal => require_slug("id", id),
                    }
                    .context("Collection.GetMany: check the ids")?;
                }
                Source::Ids(ids.ids.into_iter())
            }
            Some(Selection::Listing(listing)) => {
                left = listing.limit.map(u64::from);
                let after = listing.start_after;
                if decl.kind() == Kind::Journal {
                    let mut state = LinesState::new(&snap, &decl).context("Collection.GetMany")?;
                    // A page begins in the segment of the line it follows,
                    // not at the first segment.
                    let segment = after
                        .as_deref()
                        .and_then(|after| self.id_maps().segment_of(&decl, after));
                    if let Some(segment) = &segment {
                        state.start_at(std::path::Path::new(segment));
                    }
                    Source::Lines {
                        state,
                        after,
                        hinted: segment.is_some(),
                    }
                } else {
                    let mut state = WalkState::new(&snap, &decl).context("Collection.GetMany")?;
                    if let Some(after) = after {
                        state = state.starting_after(after);
                    }
                    Source::Files(state)
                }
            }
            None => {
                return Err(Fault::new(
                    VaultErrorCode::InvalidArgument,
                    "the selection names neither ids nor a listing",
                )
                .into());
            }
        };
        Ok(DocumentStream {
            snap,
            decl,
            include_raw,
            source,
            left,
            done: false,
        })
    }

    /// The versions of the Document `document_id` of `collection`, newest
    /// first, as a lazy stream.
    ///
    /// The stream walks the commits reachable from the `HEAD` of the
    /// repository the collection lives in, the newest by commit time first,
    /// and lists the ones that wrote the Document: a commit is a version
    /// when the file differs from the file in every parent (from nothing, in
    /// a root commit), so a join commit that only takes one side's version
    /// is not listed and the write on either side is. A Document that was
    /// never stored has no versions: the stream is empty.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` for an id that breaks the rules of the collection;
    /// `NOT_FOUND` for an unknown collection; `UNIMPLEMENTED` for a journal.
    /// A failure while the stream is read, `CRYPTO` for a version that will
    /// not open included, is an item of the stream.
    pub(crate) fn history(
        &self,
        collection: &str,
        document_id: DocumentId,
    ) -> Result<HistoryStream, Error> {
        stored::validate(&document_id).context("Collection.History: check the request")?;
        let id = document_id.id.unwrap_or_default();
        let declarations = self.declarations_now().context("Collection.History")?;
        let decl = declarations
            .get(collection)
            .ok_or_else(|| not_declared(collection))?
            .clone();
        let stored_id = match decl.kind() {
            Kind::Journal => {
                return Err(Fault::new(
                    VaultErrorCode::Unimplemented,
                    format!(
                        "the collection {collection} is a journal; a line never changes, so it has no history"
                    ),
                )
                .into());
            }
            Kind::Standard => require_document_id(&id).map(|()| true),
            Kind::Singleton => require_slug("id", &id)
                .map(|()| decl.singleton_id() == Some(id.as_str())),
        }
        .context("Collection.History: check the id")?;
        let snap = self
            .repos()
            .snapshot(decl.repo())
            .context("Collection.History")?;
        let mut stream = HistoryStream {
            rel: isolated::vault_rel(&decl, &id),
            snap,
            decl,
            id,
            heap: BinaryHeap::new(),
            seen: HashSet::new(),
            done: !stored_id,
        };
        if !stream.done
            && let Some(head) = stream.snap.commit()
        {
            let info = stream
                .snap
                .commit_info(head)
                .context("Collection.History")?;
            stream.seen.insert(head);
            stream.heap.push(Pending::new(head, info));
        }
        Ok(stream)
    }

    /// Checks the whole Vault and reports every problem it finds, the empty
    /// report when it is sound.
    ///
    /// # Errors
    ///
    /// Only when a check cannot run (an I/O or git failure, a secret the
    /// handle lacks for an encrypted collection); what the checks find is
    /// the report.
    pub(crate) fn check(&self) -> Result<CheckReport, Error> {
        let problems = check::run(self).context("Vault.Check")?;
        Ok(CheckReport {
            problems,
            ..CheckReport::default()
        })
    }
}

impl DocumentStream {
    /// The next Document, `None` at the end. A failure is the last item.
    pub(crate) fn next(&mut self, vault: &Vault) -> Option<Result<Document, Error>> {
        if self.done || self.left == Some(0) {
            self.done = true;
            return None;
        }
        match self.step(vault) {
            Ok(Some(document)) => {
                if let Some(left) = &mut self.left {
                    *left -= 1;
                }
                Some(Ok(document))
            }
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error.context("Collection.GetMany: read a Document")))
            }
        }
    }

    fn step(&mut self, vault: &Vault) -> Result<Option<Document>, Error> {
        loop {
            let id = match &mut self.source {
                Source::Ids(ids) => match ids.next() {
                    Some(id) => id,
                    None => return Ok(None),
                },
                Source::Files(state) => {
                    let Some(id) = state.next_id(&self.snap, &self.decl)? else {
                        return Ok(None);
                    };
                    let id = id.to_owned();
                    let Some(blob) = state.blob() else {
                        return Ok(None);
                    };
                    return self.walked_document(vault, id, blob).map(Some);
                }
                Source::Lines {
                    state,
                    after,
                    hinted,
                } => {
                    let Some(line) = state.next_line(&self.snap)? else {
                        if after.is_some() && *hinted {
                            *hinted = false;
                            *state = LinesState::new(&self.snap, &self.decl)?;
                            continue;
                        }
                        return Ok(None);
                    };
                    if let Some(wanted) = after {
                        if *wanted == line.id {
                            *after = None;
                        }
                        continue;
                    }
                    return Ok(Some(self.opener().line_document(line.id, line.payload)));
                }
            };
            if let Some(document) = self.opener().open(vault, id)? {
                return Ok(Some(document));
            }
        }
    }

    fn opener(&self) -> Opener<'_> {
        Opener {
            snap: &self.snap,
            decl: &self.decl,
            include_raw: self.include_raw,
        }
    }

    /// The Document of a file the walk has reached: its blob id is known
    /// from the tree, so no path is looked up again.
    fn walked_document(&self, vault: &Vault, id: String, blob: Oid) -> Result<Document, Error> {
        let document = self
            .opener()
            .keys(id.clone())
            .with_revision(blob.to_string());
        if !self.include_raw {
            return Ok(document);
        }
        let rel = isolated::vault_rel(&self.decl, &id);
        let stored = self
            .snap
            .blob(blob)
            .map_err(|fault| fault.with_path(&rel))?;
        let plain = documents::unseal(&self.decl, stored, vault.encryption(), &rel)?;
        Ok(document.with_raw(plain))
    }
}

/// Opens Documents of one collection from one snapshot, as a stream does:
/// the keys and the revision from the tree, and the bytes only when
/// `include_raw` asks.
pub(crate) struct Opener<'a> {
    snap: &'a Snap,
    decl: &'a Declaration,
    include_raw: bool,
}

impl Opener<'_> {
    /// The Document `id`, or `None` when the commit of the snapshot holds
    /// none. A journal line is found through the id map.
    pub(crate) fn open(&self, vault: &Vault, id: String) -> Result<Option<Document>, Error> {
        if self.decl.kind() == Kind::Journal {
            self.find_line(vault, id)
        } else {
            self.file_document(vault, id)
        }
    }

    /// The journal line `id`, or `None` when the journal has none.
    fn find_line(&self, vault: &Vault, id: String) -> Result<Option<Document>, Error> {
        let line = vault.id_maps().find(self.snap, self.decl, &id)?;
        Ok(line.map(|line| self.line_document(id, line.payload)))
    }

    /// The journal line `id` with the bytes `payload`; the bytes only with
    /// `include_raw`.
    pub(crate) fn line_document(&self, id: String, payload: Vec<u8>) -> Document {
        let document = self.keys(id);
        if self.include_raw {
            document.with_raw(payload)
        } else {
            document
        }
    }

    /// The Document file `id`, or `None` when the commit holds none.
    fn file_document(&self, vault: &Vault, id: String) -> Result<Option<Document>, Error> {
        if self.decl.kind() == Kind::Singleton && self.decl.singleton_id() != Some(id.as_str()) {
            return Ok(None);
        }
        if self.include_raw {
            let found = documents::read_found(self.snap, self.decl, &id, vault.encryption())?;
            return Ok(found.map(|found| {
                let revision = found.revision();
                self.keys(id)
                    .with_raw(found.into_plain())
                    .with_revision(revision)
            }));
        }
        let node = self.snap.lookup(&isolated::vault_rel(self.decl, &id))?;
        Ok(node
            .filter(|node| node.kind == NodeKind::File)
            .map(|node| self.keys(id).with_revision(node.id.to_string())))
    }

    /// A Document with its keys only.
    fn keys(&self, id: String) -> Document {
        Document {
            collection: Some(self.decl.name().to_owned()),
            id: Some(id),
            ..Document::default()
        }
    }
}

/// A stream of the versions of one Document: what `Collection.History`
/// returns, before the gate wraps it. Pull it with
/// [`next`](Self::next), giving the Vault it was made by.
///
/// It holds the commits it has met and not yet visited in a heap, newest
/// first, with the set of the ones met; it does not hold a revision walk,
/// which would borrow the repository. Each pull looks the commit up through
/// the snapshot.
pub(crate) struct HistoryStream {
    snap: Snap,
    decl: Declaration,
    id: String,
    /// The Vault-relative path of the Document file.
    rel: String,
    heap: BinaryHeap<Pending>,
    seen: HashSet<Oid>,
    done: bool,
}

/// A commit met and not yet visited.
struct Pending {
    id: Oid,
    info: CommitInfo,
}

impl Pending {
    fn new(id: Oid, info: CommitInfo) -> Pending {
        Pending { id, info }
    }
}

impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Pending {}

impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Pending {
    /// Newest first when the heap pops its greatest; equal times by id, so
    /// the order does not depend on the order commits were met.
    fn cmp(&self, other: &Self) -> Ordering {
        (self.info.seconds, self.id).cmp(&(other.info.seconds, other.id))
    }
}

impl HistoryStream {
    /// The next version, `None` at the end. A failure is the last item.
    pub(crate) fn next(&mut self, vault: &Vault) -> Option<Result<DocumentHistoryEntry, Error>> {
        if self.done {
            return None;
        }
        match self.step(vault) {
            Ok(Some(entry)) => Some(Ok(entry)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error.context("Collection.History: read a version")))
            }
        }
    }

    /// The id of the blob that holds the Document in the tree `tree`, `None`
    /// when the tree holds no file there.
    fn blob_in(&self, tree: Oid) -> Result<Option<Oid>, Fault> {
        Ok(self
            .snap
            .lookup_in(tree, &self.rel)?
            .filter(|node| node.kind == NodeKind::File)
            .map(|node| node.id))
    }

    fn step(&mut self, vault: &Vault) -> Result<Option<DocumentHistoryEntry>, Error> {
        while let Some(Pending { info, .. }) = self.heap.pop() {
            let mut parents = Vec::with_capacity(info.parents.len());
            for parent in &info.parents {
                parents.push((*parent, self.snap.commit_info(*parent)?));
            }
            let here = self.blob_in(info.tree)?;
            let mut written = if parents.is_empty() {
                here.is_some()
            } else {
                true
            };
            for (parent, parent_info) in parents {
                if written && self.blob_in(parent_info.tree)? == here {
                    written = false;
                }
                if self.seen.insert(parent) {
                    self.heap.push(Pending::new(parent, parent_info));
                }
            }
            if written {
                return self.entry(vault, here, &info).map(Some);
            }
        }
        Ok(None)
    }

    fn entry(
        &self,
        vault: &Vault,
        blob: Option<Oid>,
        info: &CommitInfo,
    ) -> Result<DocumentHistoryEntry, Error> {
        let mut entry = DocumentHistoryEntry {
            time: MessageField::some(Timestamp::from_unix_secs(info.seconds)),
            author: MessageField::some(
                Author::default()
                    .with_name(info.author_name.clone())
                    .with_email(info.author_email.clone()),
            ),
            ..DocumentHistoryEntry::default()
        };
        if let Some(note) = note_of(&info.message, self.decl.name(), &self.id) {
            entry = entry.with_note(note);
        }
        let raw = match blob {
            Some(blob) => {
                entry = entry.with_revision(blob.to_string());
                let stored = self
                    .snap
                    .blob(blob)
                    .map_err(|fault| fault.with_path(&self.rel))?;
                documents::unseal(&self.decl, stored, vault.encryption(), &self.rel)?
            }
            None => Vec::new(),
        };
        Ok(entry.with_raw(raw))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use scheda_api::v1::document_selection::Selection;
    use scheda_api::v1::{
        CollectionDeclaration, DocumentId, DocumentSelection, DocumentUpsert, Listing, OpenOptions,
        VaultErrorCode,
    };

    use crate::commit::hooks;
    use crate::git::git;
    use crate::ports::Ports;
    use crate::repo::blob_reads;
    use crate::vault::Vault;

    /// How many Document files the big collection holds: the size the work
    /// order names.
    const BIG: usize = 100_000;

    /// A Vault on disk made by `Vault::open`, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-reads-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Scratch(dir)
        }

        fn open(&self) -> Vault {
            Vault::open(
                &self.0,
                OpenOptions::default().with_write_timeout_ms(2000),
                Ports::default(),
            )
            .unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn declare(vault: &Vault, name: &str) {
        let collection: CollectionDeclaration = serde_json::from_value(serde_json::json!({
            "name": name, "schema": {"standard": {}}
        }))
        .unwrap();
        vault.create_collection(collection).unwrap();
    }

    fn upsert(id: &str, raw: &str) -> DocumentUpsert {
        DocumentUpsert::default()
            .with_id(id)
            .with_raw(raw.as_bytes().to_vec())
            .with_note("test")
    }

    fn listing(include_raw: bool) -> DocumentSelection {
        DocumentSelection {
            include_raw: Some(include_raw),
            selection: Some(Selection::Listing(Box::<Listing>::default())),
            ..DocumentSelection::default()
        }
    }

    #[test]
    fn a_journal_page_begins_in_the_segment_of_the_line_it_follows() {
        use scheda_api::v1::DocumentCreation;

        let scratch = Scratch::new("journal-page");
        let vault = Vault::open(
            &scratch.0,
            OpenOptions::default().with_index_poll_ms(10),
            crate::ports::Ports::default(),
        )
        .unwrap();
        let log: CollectionDeclaration = serde_json::from_value(serde_json::json!({
            "name": "log", "schema": {"journal": {"segment_max_lines": 2}}
        }))
        .unwrap();
        vault.create_collection(log).unwrap();
        for n in 0..20 {
            vault
                .create(
                    "log",
                    DocumentCreation::default()
                        .with_id(format!("l{n:02}"))
                        .with_raw(format!("{{\"n\":{n}}}").into_bytes())
                        .with_note("test"),
                )
                .unwrap();
        }
        // The id map of the journal is made by the indexer.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while vault
            .id_maps()
            .segment_of(vault.declarations_now().unwrap().get("log").unwrap(), "l17")
            .is_none()
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the id map was not made"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let page = |after: &str| DocumentSelection {
            include_raw: Some(true),
            selection: Some(Selection::Listing(Box::new(
                Listing::default().with_start_after(after).with_limit(2),
            ))),
            ..DocumentSelection::default()
        };
        let before = blob_reads();
        let mut stream = vault.get_many("log", page("l17")).unwrap();
        let mut ids = Vec::new();
        while let Some(item) = stream.next(&vault) {
            ids.push(item.unwrap().id.unwrap_or_default());
        }
        assert_eq!(ids, ["l18", "l19"]);
        assert!(
            blob_reads() - before <= 2,
            "the page read the segment of l17 and the next, not the ones before"
        );

        // A page in the first segments.
        let mut stream = vault.get_many("log", page("l03")).unwrap();
        let first = stream.next(&vault).unwrap().unwrap();
        assert_eq!(first.id.as_deref(), Some("l04"));
    }

    #[test]
    fn the_first_document_of_a_big_collection_reads_one_blob_and_a_dropped_stream_no_more() {
        let scratch = Scratch::new("lazy-documents");
        let vault = scratch.open();
        declare(&vault, "notes");
        let dir = scratch.0.join("notes");
        for n in 0..BIG {
            std::fs::write(dir.join(format!("d{n:06}.md")), format!("document {n}")).unwrap();
        }
        // One commit with all of them, made with git itself.
        for args in [
            &["add", "--all"][..],
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "--quiet",
                "--message",
                "the big collection",
            ][..],
        ] {
            let mut cmd = git().current_dir(&scratch.0);
            for arg in args {
                cmd = cmd.arg(arg);
            }
            cmd.run().unwrap();
        }

        let mut stream = vault.get_many("notes", listing(true)).unwrap();
        let before = blob_reads();
        let first = stream.next(&vault).unwrap().unwrap();
        assert_eq!(first.id.as_deref(), Some("d000000.md"));
        assert_eq!(first.raw.as_deref(), Some(&b"document 0"[..]));
        assert_eq!(blob_reads() - before, 1, "one Document file was opened");
        let second = stream.next(&vault).unwrap().unwrap();
        assert_eq!(second.id.as_deref(), Some("d000001.md"));
        assert_eq!(blob_reads() - before, 2, "one more per Document");
        drop(stream);
        assert_eq!(blob_reads() - before, 2, "a dropped stream reads no more");

        // Keys and revisions need no blob at all, whatever the size.
        let mut keys = vault.get_many("notes", listing(false)).unwrap();
        let before = blob_reads();
        let mut count = 0;
        while let Some(item) = keys.next(&vault) {
            let document = item.unwrap();
            assert!(document.raw.is_none());
            assert!(document.revision.is_some());
            count += 1;
        }
        assert_eq!(count, BIG);
        assert_eq!(blob_reads() - before, 0);
    }

    #[test]
    fn the_first_version_of_a_long_history_reads_one_blob_and_a_dropped_stream_no_more() {
        let scratch = Scratch::new("lazy-history");
        let vault = scratch.open();
        declare(&vault, "notes");
        for n in 0..200 {
            vault
                .upsert("notes", upsert("a.md", &format!("version {n}")))
                .unwrap();
            vault
                .upsert("notes", upsert("b.md", &format!("other {n}")))
                .unwrap();
        }
        let mut stream = vault
            .history("notes", DocumentId::default().with_id("a.md"))
            .unwrap();
        let before = blob_reads();
        let first = stream.next(&vault).unwrap().unwrap();
        assert_eq!(first.raw.as_deref(), Some(&b"version 199"[..]));
        assert_eq!(blob_reads() - before, 1, "one version was opened");
        let second = stream.next(&vault).unwrap().unwrap();
        assert_eq!(second.raw.as_deref(), Some(&b"version 198"[..]));
        assert_eq!(blob_reads() - before, 2);
        drop(stream);
        assert_eq!(blob_reads() - before, 2, "a dropped stream reads no more");
    }

    #[test]
    fn a_write_that_has_not_returned_is_seen_by_no_get_many() {
        let scratch = Scratch::new("not-returned");
        let vault = scratch.open();
        declare(&vault, "notes");
        vault.upsert("notes", upsert("old.md", "old")).unwrap();
        let other = scratch.open();
        // The call stops after the list of its paths, before the swap on
        // `HEAD`, as a kill would.
        hooks::stop_after_list();
        let error = vault.upsert("notes", upsert("new.md", "new")).unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
        for handle in [&vault, &other] {
            let mut stream = handle.get_many("notes", listing(true)).unwrap();
            let only = stream.next(handle).unwrap().unwrap();
            assert_eq!(only.id.as_deref(), Some("old.md"));
            assert!(stream.next(handle).is_none(), "new.md is seen by no read");
        }
    }
}
