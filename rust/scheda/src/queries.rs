//! Queries over the Index: `Collection.Query` and `Collection.Count`,
//! `Vault.QueryDocuments` and `Vault.CountDocuments`.
//!
//! A query selects: the exact set of Documents a `Filter` matches, in a
//! stable order, without scores. It reads the lexical index of the Index
//! for the keys and the repositories at `HEAD` for the Documents.
//!
//! **Scope.** A query of one collection scans that collection. A query of
//! the Vault scans the collections it names, or, naming none, every
//! collection that is neither isolated nor encrypted (and takes part in the
//! lexical Index), in name order. A collection that is encrypted, or leaves
//! the lexical Index out (`exclude_lexical_index`), holds nothing there, so
//! naming it is `UNIMPLEMENTED` rather than an empty answer.
//!
//! **Order.** The `order` field of the Index: the `id` of a `standard` or
//! `singleton` Document, the place of a journal line in append order. A
//! query of the Vault orders by collection first. `start_after` is a
//! condition on that order, not a count of what comes before it, so a deep
//! page costs what the first one does and a Document added meanwhile moves
//! no page.
//!
//! **Streams.** A stream is made in one call that takes the snapshot of the
//! Index it reads (a Tantivy searcher, which keeps its state whatever the
//! indexer does meanwhile) and the snapshots of the repositories the
//! Documents are read from. It is then pulled: the Index is asked for one
//! page of keys at a time, each after the last `order` seen, and the file of
//! a Document is opened when the stream reaches it, so a caller who stops
//! early pays for what it read. Nothing written after the call is seen, and
//! no Document comes twice or is missed.
//!
//! **An Index behind `HEAD`.** The Index answers from the state it reflects,
//! the files from `HEAD`. A key the Index holds for a file `HEAD` no longer
//! has is skipped: it is not a Document. `limit` counts the Documents
//! handed out, `offset` the keys of the Index passed over.

use std::collections::{HashMap, VecDeque};
use std::ops::Bound;
use std::path::Path;
use std::sync::Arc;

use scheda_api::v1::{Document, DocumentQuery, Filter, VaultErrorCode, VaultQuery};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{BooleanQuery, Occur, Query, RangeQuery, TermQuery};
use tantivy::schema::{IndexRecordOption, Value as _};
use tantivy::{Order, TantivyDocument, Term};

use crate::collection::{Declaration, Declarations, Kind};
use crate::error::{Context, Error, Fault};
use crate::filter::{Compiler, Scope};
use crate::id::{require_document_id, require_slug};
use crate::journal;
use crate::lexical::{Fields, Snapshot, key_of, read_fault};
use crate::reads::Opener;
use crate::repo::Views;
use crate::vault::Vault;
use crate::{stored, who};

/// How many keys the Index is asked for at a time.
const PAGE: u64 = 256;

/// Where a query looks.
#[derive(Debug)]
enum Where {
    /// One collection.
    Collection(String),
    /// The collections of a Vault query; empty names every collection that
    /// is neither isolated nor encrypted.
    Vault(Vec<String>),
}

/// A request to select Documents, checked against the rules of the
/// contract and ready to plan.
#[derive(Debug)]
pub(crate) struct Request {
    call: &'static str,
    scope: Where,
    filter: Filter,
    include_raw: bool,
    offset: u64,
    limit: Option<u64>,
    /// The Document the query begins after: its collection (unset in a
    /// query of one collection) and id.
    start_after: Option<(Option<String>, String)>,
}

impl Request {
    /// The request of `Collection.Query` or `Collection.Count` (`call`) on
    /// `collection`.
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when the message breaks a rule of the contract.
    pub(crate) fn of_collection(
        call: &'static str,
        collection: &str,
        mut query: DocumentQuery,
    ) -> Result<Request, Error> {
        stored::validate(&query).with_context(|| format!("{call}: check the request"))?;
        Ok(Request {
            call,
            scope: Where::Collection(collection.to_owned()),
            filter: query.filter.take().unwrap_or_default(),
            include_raw: query.include_raw.unwrap_or(false),
            offset: u64::from(query.offset.unwrap_or(0)),
            limit: query.limit.map(u64::from),
            start_after: query.start_after.map(|id| (None, id)),
        })
    }

    /// The request of `Vault.QueryDocuments` or `Vault.CountDocuments`
    /// (`call`).
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when the message breaks a rule of the contract.
    pub(crate) fn of_vault(call: &'static str, mut query: VaultQuery) -> Result<Request, Error> {
        stored::validate(&query).with_context(|| format!("{call}: check the request"))?;
        let names = query
            .collections
            .take()
            .map(|collections| collections.names)
            .unwrap_or_default();
        Ok(Request {
            call,
            scope: Where::Vault(names),
            filter: query.filter.take().unwrap_or_default(),
            include_raw: query.include_raw.unwrap_or(false),
            offset: u64::from(query.offset.unwrap_or(0)),
            limit: query.limit.map(u64::from),
            start_after: query
                .start_after
                .take()
                .map(|key| (key.collection, key.id.unwrap_or_default())),
        })
    }
}

/// One collection a query scans.
struct Part {
    decl: Declaration,
    /// The Documents of the collection the Filter matches.
    base: Box<dyn Query>,
    /// The `order` after which the scan goes on: the last key seen, or the
    /// `start_after` of the request. `None` begins at the first key.
    cursor: Option<String>,
}

impl Part {
    /// The matches of this collection after the cursor.
    fn after_cursor(&self, fields: Fields) -> Box<dyn Query> {
        let Some(cursor) = &self.cursor else {
            return self.base.box_clone();
        };
        let after = RangeQuery::new(
            Bound::Excluded(Term::from_field_text(fields.order, cursor)),
            Bound::Unbounded,
        );
        Box::new(BooleanQuery::new(vec![
            (Occur::Must, self.base.box_clone()),
            (Occur::Must, Box::new(after)),
        ]))
    }
}

/// What a request scans, from the Index it reads.
struct Plan {
    snapshot: Arc<Snapshot>,
    parts: Vec<Part>,
}

fn not_declared(collection: &str) -> Fault {
    Fault::new(
        VaultErrorCode::NotFound,
        format!("the collection {collection} is not declared"),
    )
}

/// `UNIMPLEMENTED` when `decl` holds nothing in the lexical Index.
fn require_indexed(decl: &Declaration) -> Result<(), Fault> {
    if decl.encrypted() {
        return Err(Fault::new(
            VaultErrorCode::Unimplemented,
            format!(
                "the collection {} is encrypted; no Index holds its Documents",
                decl.name()
            ),
        ));
    }
    if !decl.lexical_indexed() {
        return Err(Fault::new(
            VaultErrorCode::Unimplemented,
            format!(
                "the collection {} leaves the lexical Index out (exclude_lexical_index); a query of it has nothing to read",
                decl.name()
            ),
        ));
    }
    Ok(())
}

/// The collections a query scans, in name order.
fn targets<'a>(
    declarations: &'a Declarations,
    scope: &Where,
) -> Result<Vec<&'a Declaration>, Fault> {
    match scope {
        Where::Collection(name) => {
            let decl = declarations.get(name).ok_or_else(|| not_declared(name))?;
            require_indexed(decl)?;
            Ok(vec![decl])
        }
        Where::Vault(names) if names.is_empty() => Ok(declarations
            .iter()
            .filter(|decl| !decl.isolated() && decl.lexical_indexed())
            .collect()),
        Where::Vault(names) => {
            let sorted: std::collections::BTreeSet<&str> =
                names.iter().map(String::as_str).collect();
            let mut found = Vec::with_capacity(sorted.len());
            for name in sorted {
                let decl = declarations.get(name).ok_or_else(|| not_declared(name))?;
                require_indexed(decl)?;
                found.push(decl);
            }
            Ok(found)
        }
    }
}

/// The `order` of the journal line `id` of `collection` in the Index, or
/// `None` when the Index holds no such line.
fn line_order(snapshot: &Snapshot, collection: &str, id: &str) -> Result<Option<String>, Error> {
    let fields = snapshot.fields();
    let query = TermQuery::new(
        Term::from_field_text(fields.key, &key_of(collection, id)),
        IndexRecordOption::Basic,
    );
    let searcher = snapshot.searcher();
    let top = searcher
        .search(&query, &TopDocs::with_limit(1).order_by_score())
        .map_err(|err| read_fault(err, "look up a line in the lexical index"))?;
    let Some((_, address)) = top.into_iter().next() else {
        return Ok(None);
    };
    let doc: TantivyDocument = searcher
        .doc(address)
        .map_err(|err| read_fault(err, "read a line of the lexical index"))?;
    Ok(doc
        .get_first(fields.order)
        .and_then(|value| value.as_str())
        .map(str::to_owned))
}

impl Vault {
    /// Checks the request against the Vault and the Index and plans the
    /// scan: the collections, the query of each, and where each begins.
    fn plan(&self, request: &Request) -> Result<Plan, Error> {
        let declarations = self.declarations_now()?;
        let decls = targets(&declarations, &request.scope)?;
        if let Some((collection, id)) = &request.start_after {
            let name = match (&request.scope, collection) {
                (Where::Collection(own), _) => own.as_str(),
                (Where::Vault(_), Some(named)) => named.as_str(),
                (Where::Vault(_), None) => "",
            };
            let decl = declarations.get(name).ok_or_else(|| not_declared(name))?;
            match decl.kind() {
                Kind::Standard => require_document_id(id),
                Kind::Singleton | Kind::Journal => require_slug("id", id),
            }
            .context("check start_after")?;
        }
        let names: Vec<&str> = decls.iter().map(|decl| decl.name()).collect();
        let snapshot = self.lexical().snapshot(&names)?;
        if self.indexer().projection_stale() {
            return Err(who::stale_fault().into());
        }
        let scope = match &request.scope {
            Where::Collection(own) => Scope::Collection(own),
            Where::Vault(_) => Scope::Vault,
        };
        if decls.is_empty() {
            // Nothing to scan, and a Filter that breaks a rule is still wrong.
            Compiler::new(&snapshot, &declarations, scope, None).compile(&request.filter)?;
        }
        let fields = snapshot.fields();
        let mut parts = Vec::with_capacity(decls.len());
        for decl in decls {
            let name = decl.name();
            let cursor = match &request.start_after {
                Some((collection, id)) => {
                    let after = match &request.scope {
                        Where::Collection(own) => own.as_str(),
                        Where::Vault(_) => collection.as_deref().unwrap_or_default(),
                    };
                    match name.cmp(after) {
                        std::cmp::Ordering::Less => continue,
                        std::cmp::Ordering::Greater => None,
                        std::cmp::Ordering::Equal if decl.kind() == Kind::Journal => {
                            // A line that is not in the Index is no place in
                            // the order: nothing follows it.
                            match line_order(&snapshot, name, id)? {
                                Some(order) => Some(order),
                                None => continue,
                            }
                        }
                        std::cmp::Ordering::Equal => Some(id.clone()),
                    }
                }
                None => None,
            };
            let matches = Compiler::new(&snapshot, &declarations, scope, Some(name))
                .compile(&request.filter)?;
            let in_collection = TermQuery::new(
                Term::from_field_text(fields.collection, name),
                IndexRecordOption::Basic,
            );
            let base = BooleanQuery::new(vec![
                (Occur::Must, Box::new(in_collection) as Box<dyn Query>),
                (Occur::Must, matches),
            ]);
            parts.push(Part {
                decl: decl.clone(),
                base: Box::new(base),
                cursor,
            });
        }
        Ok(Plan { snapshot, parts })
    }

    /// The Documents `request` selects, as a lazy stream: pull it with
    /// [`QueryStream::next`], giving this Vault.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` for a collection that is not declared; `UNIMPLEMENTED`
    /// for a named collection that is encrypted or leaves the lexical Index
    /// out; `INVALID_ARGUMENT` for a Filter that breaks a rule of the
    /// collections it names or an id that breaks the rules of its
    /// collection; `INDEX_REBUILDING` (retryable) while the Index is not
    /// built; `INDEX_STALE` for a Vault whose Projector is of another
    /// projection than the Index, or a collection whose columns changed;
    /// `INDEX_BROKEN` for a damaged Index. A failure while the stream is
    /// read is an item of the stream.
    pub(crate) fn query(&self, request: Request) -> Result<QueryStream, Error> {
        let call = request.call;
        let plan = self.plan(&request).context(call)?;
        let views = self.repos().views().context(call)?;
        Ok(QueryStream {
            call,
            snapshot: plan.snapshot,
            views,
            parts: plan.parts.into(),
            offset: request.offset,
            left: request.limit,
            include_raw: request.include_raw,
            page: VecDeque::new(),
            part_done: false,
            segment: None,
            done: false,
        })
    }

    /// How many Documents `request` would stream, counted in the Index
    /// without opening a file: the matches after `start_after`, less
    /// `offset`, up to `limit`.
    ///
    /// # Errors
    ///
    /// As [`Vault::query`].
    pub(crate) fn count(&self, request: Request) -> Result<u64, Error> {
        let call = request.call;
        let plan = self.plan(&request).context(call)?;
        let fields = plan.snapshot.fields();
        let searcher = plan.snapshot.searcher();
        let mut total = 0u64;
        for part in &plan.parts {
            let found = searcher
                .search(&part.after_cursor(fields), &Count)
                .map_err(|err| read_fault(err, "count in the lexical index"))
                .context(call)?;
            total = total.saturating_add(u64::try_from(found).unwrap_or(u64::MAX));
        }
        let mut count = total.saturating_sub(request.offset);
        if let Some(limit) = request.limit {
            count = count.min(limit);
        }
        Ok(count)
    }
}

/// A key the Index gave: the id of a Document.
struct Hit {
    id: String,
}

/// The lines of the journal segment `rel`, by id.
struct Segment {
    rel: String,
    lines: HashMap<String, Vec<u8>>,
}

/// A lazy stream of the Documents a query selects. Pull it with
/// [`next`](Self::next), giving the Vault it was made by.
pub(crate) struct QueryStream {
    call: &'static str,
    snapshot: Arc<Snapshot>,
    views: Views,
    /// The collections still to scan, the one being scanned first.
    parts: VecDeque<Part>,
    /// How many keys are still to be passed over.
    offset: u64,
    /// How many Documents may still be handed out; `None` is no limit.
    left: Option<u64>,
    include_raw: bool,
    /// The keys of the last page of the first collection, not yet handed
    /// out.
    page: VecDeque<Hit>,
    /// The first collection has no key after the last page.
    part_done: bool,
    /// The journal segment the last line was read from, for the next.
    segment: Option<Segment>,
    done: bool,
}

impl QueryStream {
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
                Some(Err(error.context(format!("{}: read a Document", self.call))))
            }
        }
    }

    fn step(&mut self, vault: &Vault) -> Result<Option<Document>, Error> {
        loop {
            if let Some(hit) = self.page.pop_front() {
                if self.offset > 0 {
                    self.offset -= 1;
                    continue;
                }
                if let Some(document) = self.document(vault, hit.id)? {
                    return Ok(Some(document));
                }
                continue;
            }
            if self.parts.is_empty() {
                return Ok(None);
            }
            if self.part_done {
                self.parts.pop_front();
                self.part_done = false;
                continue;
            }
            self.fill()?;
        }
    }

    /// Asks the Index for the next page of keys of the first collection.
    fn fill(&mut self) -> Result<(), Error> {
        let fields = self.snapshot.fields();
        let Some(part) = self.parts.front_mut() else {
            return Ok(());
        };
        let wanted = self
            .offset
            .saturating_add(self.left.unwrap_or(u64::MAX))
            .clamp(1, PAGE);
        let wanted = usize::try_from(wanted).unwrap_or(1);
        let searcher = self.snapshot.searcher();
        let top = searcher
            .search(
                &part.after_cursor(fields),
                &TopDocs::with_limit(wanted).order_by_string_fast_field("order", Order::Asc),
            )
            .map_err(|err| read_fault(err, "read a page of the lexical index"))?;
        self.part_done = top.len() < wanted;
        for (order, address) in top {
            let doc: TantivyDocument = searcher
                .doc(address)
                .map_err(|err| read_fault(err, "read a key of the lexical index"))?;
            let id = doc
                .get_first(fields.id)
                .and_then(|value| value.as_str())
                .map(str::to_owned);
            let (Some(id), Some(order)) = (id, order) else {
                return Err(Fault::new(
                    VaultErrorCode::IndexBroken,
                    "a Document of the lexical index has no id or no order",
                )
                .into());
            };
            part.cursor = Some(order);
            self.page.push_back(Hit { id });
        }
        Ok(())
    }

    /// The Document `id` of the first collection, or `None` when `HEAD` has
    /// no such file (the Index is behind).
    fn document(&mut self, vault: &Vault, id: String) -> Result<Option<Document>, Error> {
        let Some(part) = self.parts.front() else {
            return Ok(None);
        };
        let decl = &part.decl;
        let snap = self.views.of(decl.repo());
        let opener = Opener::new(snap, decl, self.include_raw);
        if decl.kind() != Kind::Journal {
            return opener.open(vault, id);
        }
        if !self.include_raw {
            return Ok(Some(opener.line_document(id, Vec::new())));
        }
        // A page of lines reads the segment once, not once for each line.
        if let Some(rel) = vault.id_maps().segment_of(decl, &id) {
            if self
                .segment
                .as_ref()
                .is_none_or(|segment| segment.rel != rel)
            {
                self.segment = journal::read_segment(snap, Path::new(&rel))?.map(|bytes| Segment {
                    lines: journal::parse_segment(&bytes)
                        .into_iter()
                        .map(|line| (line.id, line.payload))
                        .collect(),
                    rel,
                });
            }
            if let Some(payload) = self
                .segment
                .as_ref()
                .and_then(|segment| segment.lines.get(&id))
            {
                return Ok(Some(opener.line_document(id, payload.clone())));
            }
        }
        opener.open(vault, id)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use scheda_api::v1::{
        CollectionDeclaration, DocumentDeletion, DocumentQuery, DocumentUpsert, OpenOptions,
        VaultErrorCode,
    };

    use super::Request;
    use crate::commit::hooks;
    use crate::index::State;
    use crate::ports::{Ports, indexing_ports};
    use crate::repo::{Repos, blob_reads};
    use crate::vault::Vault;

    /// A Vault on disk made by `Vault::open`, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-queries-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Scratch(dir)
        }

        fn options() -> OpenOptions {
            OpenOptions::default()
                .with_write_timeout_ms(2000)
                .with_index_poll_ms(10)
        }

        /// A handle that indexes: it has a Projector.
        fn indexing(&self) -> Vault {
            Vault::open(&self.0, Scratch::options(), indexing_ports()).unwrap()
        }

        /// A handle that only reads: it has no port.
        fn reading(&self) -> Vault {
            Vault::open(&self.0, Scratch::options(), Ports::default()).unwrap()
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

    fn upsert(id: &str) -> DocumentUpsert {
        DocumentUpsert::default()
            .with_id(id)
            .with_raw(format!("document {id}").into_bytes())
            .with_note("test")
    }

    /// Waits until the Index of `scratch` reflects `HEAD`.
    fn wait_indexed(scratch: &Scratch, vault: &Vault) {
        let head = State::of(
            &Repos::new(&scratch.0.canonicalize().unwrap())
                .views()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(snapshot) = vault.lexical().snapshot(&["notes"])
                && snapshot.state() == head
            {
                return;
            }
            assert!(Instant::now() < deadline, "the Index did not reach HEAD");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn query(json: serde_json::Value) -> Request {
        let query: DocumentQuery = serde_json::from_value(json).unwrap();
        Request::of_collection("Collection.Query", "notes", query).unwrap()
    }

    fn all(include_raw: bool) -> Request {
        query(serde_json::json!({"filter": {"all": {}}, "includeRaw": include_raw}))
    }

    fn ids(vault: &Vault, request: Request) -> Vec<String> {
        let mut stream = vault.query(request).unwrap();
        let mut ids = Vec::new();
        while let Some(item) = stream.next(vault) {
            ids.push(item.unwrap().id.unwrap_or_default());
        }
        ids
    }

    #[test]
    fn a_count_opens_no_file_and_a_stream_opens_one_as_it_reaches_a_document() {
        let scratch = Scratch::new("lazy");
        let vault = scratch.indexing();
        declare(&vault, "notes");
        for n in 0..30 {
            vault.upsert("notes", upsert(&format!("d{n:02}"))).unwrap();
        }
        wait_indexed(&scratch, &vault);

        let before = blob_reads();
        let counted = vault.count(all(true)).unwrap();
        assert_eq!(counted, 30);
        assert_eq!(
            vault
                .count(query(serde_json::json!({
                    "filter": {"all": {}}, "offset": 5, "limit": 10, "startAfter": "d09"
                })))
                .unwrap(),
            10
        );
        assert_eq!(blob_reads() - before, 0, "a count opens no Document file");

        // Keys and revisions come from the tree: no blob either.
        let before = blob_reads();
        assert_eq!(ids(&vault, all(false)).len(), 30);
        assert_eq!(blob_reads() - before, 0);

        // With the bytes, one file for each Document the stream reaches.
        let mut stream = vault.query(all(true)).unwrap();
        let before = blob_reads();
        let first = stream.next(&vault).unwrap().unwrap();
        assert_eq!(first.raw.as_deref(), Some(&b"document d00"[..]));
        assert_eq!(blob_reads() - before, 1, "one file for the first Document");
        stream.next(&vault).unwrap().unwrap();
        assert_eq!(blob_reads() - before, 2);
        drop(stream);
        assert_eq!(blob_reads() - before, 2, "a dropped stream reads no more");

        // Skipped keys are passed over without opening their files.
        let before = blob_reads();
        let paged = ids(
            &vault,
            query(serde_json::json!({"filter": {"all": {}}, "includeRaw": true, "offset": 25})),
        );
        assert_eq!(paged.len(), 5);
        assert_eq!(blob_reads() - before, 5);
    }

    #[test]
    fn a_write_that_has_not_returned_is_seen_by_no_query() {
        let scratch = Scratch::new("not-returned");
        let vault = scratch.indexing();
        declare(&vault, "notes");
        vault.upsert("notes", upsert("old.md")).unwrap();
        wait_indexed(&scratch, &vault);
        let other = scratch.reading();
        // The call stops after the list of its paths, before the swap on
        // `HEAD`, as a kill would.
        hooks::stop_after_list();
        let error = vault.upsert("notes", upsert("new.md")).unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
        std::thread::sleep(Duration::from_millis(100));
        for handle in [&vault, &other] {
            assert_eq!(ids(handle, all(true)), ["old.md"]);
            assert_eq!(handle.count(all(false)).unwrap(), 1);
        }
    }

    #[test]
    fn a_key_the_index_holds_for_a_file_head_no_longer_has_is_skipped() {
        let scratch = Scratch::new("behind");
        let vault = scratch.indexing();
        declare(&vault, "notes");
        for id in ["a.md", "b.md", "c.md"] {
            vault.upsert("notes", upsert(id)).unwrap();
        }
        wait_indexed(&scratch, &vault);
        // The indexer ends, so the Index stays at the state before the
        // delete while `HEAD` moves on.
        vault.close().unwrap();
        vault
            .delete(
                "notes",
                DocumentDeletion::default()
                    .with_id("b.md")
                    .with_note("test"),
            )
            .unwrap();
        for request in [all(false), all(true)] {
            assert_eq!(ids(&vault, request), ["a.md", "c.md"]);
        }
        // `limit` counts the Documents handed out, so a page is not short
        // for a key that was skipped.
        assert_eq!(
            ids(
                &vault,
                query(serde_json::json!({"filter": {"all": {}}, "limit": 2}))
            ),
            ["a.md", "c.md"]
        );
        // A count is the Index's.
        assert_eq!(vault.count(all(false)).unwrap(), 3);
    }
}
