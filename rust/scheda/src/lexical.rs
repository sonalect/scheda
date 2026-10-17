//! The lexical side of the Index: a Tantivy index of the Documents, kept by
//! the indexer, and the reader that queries open it with.
//!
//! ```text
//! .scheda/index/lexical/    the Tantivy index, committed once per round
//! .scheda/index/columns     the columns each collection was indexed with
//! .scheda/index/stale       the collections whose columns changed since
//! ```
//!
//! **What is indexed.** Every Document of a declared collection that is not
//! `encrypted` and not `exclude_lexical_index`, and every line of its
//! journal segments, read from the snapshot of the round (isolated
//! collections from the nested repository). The Projector gives the values
//! of the columns; `raw` is moved into the call. Without a Projector, for
//! a collection with no columns, and for a Document the Projector gives no
//! values for, only the identity is stored, so a query on `id` still
//! works. A Document the Projector fails on or refuses is left out, its
//! file stays, and the round goes on; a failure that is retryable fails the
//! round instead, which is tried again.
//!
//! **One Tantivy document** has these fields:
//!
//! - `key` (`{collection}\0{id}`): replaces and deletes the document.
//! - `collection`, `id`: the identity, stored.
//! - `order`: the position the Document has in a listing, stored and a fast
//!   field. For `standard` and `singleton` it is the `id`. For a journal
//!   line it is the segment's place in append order and the line's place in
//!   the segment, in a form that compares as bytes (see [`journal_order`]),
//!   so a stream in append order is a sort by this field and a page after a
//!   line is a range after its `order`.
//! - `file`: the Vault-relative file the Document came from (a journal
//!   segment for a line); a changed or deleted segment deletes its lines by
//!   it.
//! - `text`: the `fts` columns, joined by newlines, tokenized: the text
//!   BM25 ranks.
//! - `eq`, `num`, `exists`, `null`, `tok`: the `find` columns, as the
//!   terms a Filter compiles to (see [`eq_term`]).
//!
//! **One commit.** A round deletes and adds in one writer and commits once;
//! the commit carries the state of the Vault the round reached as its
//! payload. So a reader sees the lexical side at one state whole, and a
//! round that fails before the commit leaves the side as it was. A side
//! that finds its payload is not the state the Index reflects (a build
//! without `find` moved the Index meanwhile, or a failed round committed
//! and did not record) makes itself again from the snapshot.
//!
//! **Columns.** The Projector is asked for the columns of each collection
//! once per round, and the answer is kept in `columns`. A collection
//! whose answer differs from the file is listed in `stale` and its changes
//! are no longer indexed: every reader is [`VaultErrorCode::IndexStale`]
//! until a rebuild, which takes the new columns. The Projector is asked
//! at a round, so a change is seen at the first round after it.

// The queries of the `Vault` service read the Index through `Reader`.
#![cfg_attr(not(test), expect(dead_code, reason = "the queries are not built yet"))]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use scheda_api::v1::{
    ColumnKind, IndexDoc, IndexSchema, IndexValue, VaultErrorCode, index_value::Value,
};
use serde::{Deserialize, Serialize};
use tantivy::schema::{FAST, Field, STORED, STRING, Schema, TEXT};
use tantivy::tokenizer::{PreTokenizedString, TextAnalyzer, Token};
use tantivy::{IndexWriter, ReloadPolicy, Searcher, TantivyDocument, TantivyError, Term};

use crate::collection::{Declaration, Kind};
use crate::documents::{self, Walk};
use crate::error::{self, Context, Error, Fault, OrFault, io};
use crate::index::{Index, Round, Side, State};
use crate::isolated;
use crate::journal;
use crate::ports::ProjectorPort;
use crate::repo::Snap;

/// The memory a round's writer may use before it flushes a segment.
const WRITER_HEAP: usize = 32_000_000;

/// How many Documents a round reads between two notes of progress.
const NOTE_EVERY: u64 = 256;

/// The version of the `columns` file.
const COLUMNS_FORMAT: u32 = 1;

fn broken(message: impl Into<String>) -> Fault {
    Fault::new(VaultErrorCode::IndexBroken, message)
}

/// A failure of Tantivy while a round writes: a failed disk is `STORAGE`,
/// anything else is the Index being damaged.
fn write_fault(err: TantivyError, what: &'static str) -> Fault {
    match err {
        TantivyError::IoError(err) => io(std::io::Error::new(err.kind(), err.to_string())),
        err => broken(what).with_source(err),
    }
}

/// The fields of the Tantivy schema.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fields {
    /// `{collection}\0{id}`, indexed raw.
    pub(crate) key: Field,
    /// The collection name, indexed raw and stored.
    pub(crate) collection: Field,
    /// The Document id, indexed raw and stored.
    pub(crate) id: Field,
    /// The listing position of the Document, indexed raw, stored, and fast.
    pub(crate) order: Field,
    /// The Vault-relative file the Document came from, indexed raw.
    pub(crate) file: Field,
    /// The `fts` columns, tokenized for BM25.
    pub(crate) text: Field,
    /// `{column}\0{value}` for each value of a `find` column, indexed raw.
    pub(crate) eq: Field,
    /// `{column}\0{key}` for a numeric `find` column, the key ordered as
    /// the number is, indexed raw.
    pub(crate) num: Field,
    /// The name of each `find` column the Document has a value in.
    pub(crate) exists: Field,
    /// The name of each `find` column the Document stores a null in.
    pub(crate) null: Field,
    /// `{column}\0{token}` for each token of each value of a `find`
    /// column, tokenized by the default analyzer.
    pub(crate) tok: Field,
}

impl Fields {
    fn build() -> (Schema, Fields) {
        let mut builder = Schema::builder();
        let key = builder.add_text_field("key", STRING);
        let collection = builder.add_text_field("collection", STRING | STORED);
        let id = builder.add_text_field("id", STRING | STORED);
        let order = builder.add_text_field("order", STRING | STORED | FAST);
        let file = builder.add_text_field("file", STRING);
        let text = builder.add_text_field("text", TEXT);
        let eq = builder.add_text_field("eq", STRING);
        let num = builder.add_text_field("num", STRING);
        let exists = builder.add_text_field("exists", STRING);
        let null = builder.add_text_field("null", STRING);
        let tok = builder.add_text_field("tok", TEXT);
        (
            builder.build(),
            Fields {
                key,
                collection,
                id,
                order,
                file,
                text,
                eq,
                num,
                exists,
                null,
                tok,
            },
        )
    }

    /// The fields of a schema read from disk; `INDEX_BROKEN` when one is
    /// missing.
    fn of(schema: &Schema) -> Result<Fields, Fault> {
        let get = |name: &str| {
            schema
                .get_field(name)
                .or_fault_with(VaultErrorCode::IndexBroken, || {
                    format!("the lexical index has no field {name}")
                })
        };
        Ok(Fields {
            key: get("key")?,
            collection: get("collection")?,
            id: get("id")?,
            order: get("order")?,
            file: get("file")?,
            text: get("text")?,
            eq: get("eq")?,
            num: get("num")?,
            exists: get("exists")?,
            null: get("null")?,
            tok: get("tok")?,
        })
    }
}

/// The kind of the values of a column, as the `columns` file records it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ValueKind {
    /// One text.
    Text,
    /// A signed integer.
    Integer,
    /// A floating-point number.
    Number,
    /// A boolean.
    Boolean,
    /// Several texts.
    TextList,
}

/// One column of a collection, as the `columns` file records it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct DumpColumn {
    /// The name a Filter uses.
    pub(crate) name: String,
    /// The kind of its values.
    pub(crate) kind: ValueKind,
    /// A Filter can name it.
    pub(crate) find: bool,
    /// The lexical leg of a search reads it.
    pub(crate) fts: bool,
    /// The vector leg embeds it.
    pub(crate) vector: bool,
}

/// The columns of each indexed collection, as the Projector gave them when
/// the Index was built. A collection with no columns is listed with none.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Dump {
    format: u32,
    collections: BTreeMap<String, Vec<DumpColumn>>,
}

impl Dump {
    fn new() -> Dump {
        Dump {
            format: COLUMNS_FORMAT,
            collections: BTreeMap::new(),
        }
    }

    /// The columns of `collection`; `None` when the Index does not hold the
    /// collection. An empty slice is a collection indexed by identity only.
    pub(crate) fn columns(&self, collection: &str) -> Option<&[DumpColumn]> {
        self.collections.get(collection).map(Vec::as_slice)
    }

    fn encode(&self) -> Result<Vec<u8>, Fault> {
        serde_json::to_vec_pretty(self)
            .or_fault(VaultErrorCode::IndexBroken, "the columns cannot be written")
    }

    fn parse(bytes: &[u8]) -> Result<Dump, Fault> {
        let dump: Dump = serde_json::from_slice(bytes)
            .or_fault(VaultErrorCode::IndexBroken, "the columns file is damaged")?;
        if dump.format != COLUMNS_FORMAT {
            return Err(broken("the columns file is of another format"));
        }
        Ok(dump)
    }
}

/// The columns of a schema the Projector gave.
fn dump_columns(schema: &IndexSchema) -> Result<Vec<DumpColumn>, Fault> {
    schema
        .columns
        .iter()
        .map(|column| {
            let kind = match column.kind.as_ref().and_then(|kind| kind.as_known()) {
                Some(ColumnKind::COLUMN_KIND_TEXT) => ValueKind::Text,
                Some(ColumnKind::COLUMN_KIND_INTEGER) => ValueKind::Integer,
                Some(ColumnKind::COLUMN_KIND_NUMBER) => ValueKind::Number,
                Some(ColumnKind::COLUMN_KIND_BOOLEAN) => ValueKind::Boolean,
                Some(ColumnKind::COLUMN_KIND_TEXT_LIST) => ValueKind::TextList,
                Some(ColumnKind::COLUMN_KIND_UNSPECIFIED) | None => {
                    return Err(Fault::new(
                        VaultErrorCode::InvalidArgument,
                        "the Projector gave a column of a kind Scheda does not know",
                    ));
                }
            };
            Ok(DumpColumn {
                name: column.name.clone().unwrap_or_default(),
                kind,
                find: column.find.unwrap_or(false),
                fts: column.fts.unwrap_or(false),
                vector: column.vector.unwrap_or(false),
            })
        })
        .collect()
}

/// The term of a value of a `find` column: `{column}\0{value}`. `None` when
/// either holds a NUL, which the term cannot carry.
pub(crate) fn eq_term(column: &str, value: &str) -> Option<String> {
    if column.contains('\0') || value.contains('\0') {
        return None;
    }
    Some(format!("{column}\0{value}"))
}

/// Fixed-width hex of an `i64`, in the order of the numbers.
fn i64_key(value: i64) -> String {
    format!("{:016x}", (value as u64) ^ (1 << 63))
}

/// Fixed-width hex of an `f64`, in the order of the numbers: the sign bit is
/// flipped for a positive value and every bit for a negative one. `-0.0`
/// and `0.0` share a key.
fn f64_key(value: f64) -> String {
    let bits = if value == 0.0 {
        0.0f64.to_bits()
    } else {
        value.to_bits()
    };
    let key = if bits >> 63 == 1 {
        !bits
    } else {
        bits ^ (1 << 63)
    };
    format!("{key:016x}")
}

/// The `num` key of `value` in a column of `kind`; `None` when the column
/// is not numeric or the value holds no finite number.
pub(crate) fn numeric_key(kind: ValueKind, value: &IndexValue) -> Option<String> {
    match (kind, value.value.as_ref()?) {
        (ValueKind::Integer, Value::Integer(n)) => Some(i64_key(*n)),
        (ValueKind::Integer, Value::Number(n))
            if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.2e18 =>
        {
            Some(i64_key(*n as i64))
        }
        (ValueKind::Number, Value::Number(n)) if n.is_finite() => Some(f64_key(*n)),
        (ValueKind::Number, Value::Integer(n)) => Some(f64_key(*n as f64)),
        _ => None,
    }
}

/// Calls `each` with every text a value stands for. A null, a value that is
/// not set, and a number that is not finite have none.
fn each_text(value: &IndexValue, mut each: impl FnMut(&str)) {
    match &value.value {
        Some(Value::Text(text)) => each(text),
        Some(Value::Integer(n)) => each(&n.to_string()),
        Some(Value::Number(n)) if n.is_finite() => each(&n.to_string()),
        Some(Value::Boolean(b)) => each(if *b { "true" } else { "false" }),
        Some(Value::TextList(list)) => list.texts.iter().for_each(|text| each(text)),
        Some(Value::Number(_) | Value::Null(_)) | None => {}
    }
}

fn is_null(value: &IndexValue) -> bool {
    matches!(value.value, Some(Value::Null(_)))
}

/// The key of the Tantivy document of `id` in `collection`.
pub(crate) fn key_of(collection: &str, id: &str) -> String {
    format!("{collection}\0{id}")
}

/// The `order` of a journal line: the segment's place in append order, then
/// the line's place in the segment, as text that compares as bytes the way
/// the lines are appended. A line is `{n}` lines after the first of its
/// segment.
pub(crate) fn journal_order(segment: &Path, line: usize) -> String {
    let (class, date, seq, writer, name) = journal::segment_order(segment);
    format!("{class}\u{1}{date}\u{1}{seq:08x}\u{1}{writer}\u{1}{name}\u{1}{line:010}")
}

/// Adds the token terms of `value` for `column` to `doc`, one term per
/// token, `{column}\0{token}`.
fn add_tokens(
    doc: &mut TantivyDocument,
    tok: Field,
    column: &str,
    value: &str,
    analyzer: &mut TextAnalyzer,
) -> bool {
    let mut stream = analyzer.token_stream(value);
    let mut tokens = Vec::new();
    while stream.advance() {
        let token = stream.token();
        let Some(text) = eq_term(column, &token.text) else {
            return false;
        };
        tokens.push(Token {
            offset_from: token.offset_from,
            offset_to: token.offset_to,
            position: tokens.len(),
            text,
            position_length: 1,
        });
    }
    if !tokens.is_empty() {
        doc.add_pre_tokenized_text(
            tok,
            PreTokenizedString {
                text: value.to_owned(),
                tokens,
            },
        );
    }
    true
}

/// What a Tantivy document is made of.
struct Parts<'a> {
    collection: &'a str,
    id: &'a str,
    order: &'a str,
    file: &'a str,
    columns: &'a [DumpColumn],
    values: Option<&'a IndexDoc>,
}

/// The Tantivy document of `parts`; `None` when a value cannot be indexed
/// (a NUL in a column name or a text), in which case the Document is left
/// out.
fn tantivy_document(
    fields: Fields,
    analyzer: &mut TextAnalyzer,
    parts: &Parts<'_>,
) -> Option<TantivyDocument> {
    let mut doc = TantivyDocument::default();
    doc.add_text(fields.key, key_of(parts.collection, parts.id));
    doc.add_text(fields.collection, parts.collection);
    doc.add_text(fields.id, parts.id);
    doc.add_text(fields.order, parts.order);
    doc.add_text(fields.file, parts.file);
    let Some(values) = parts.values else {
        return Some(doc);
    };
    let mut text = String::new();
    for column in parts.columns {
        let Some(value) = values.values.get(&column.name) else {
            continue;
        };
        if column.fts {
            each_text(value, |piece| {
                if !piece.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(piece);
                }
            });
        }
        if !column.find {
            continue;
        }
        if is_null(value) {
            doc.add_text(fields.null, &column.name);
            continue;
        }
        let mut any = false;
        let mut ok = true;
        each_text(value, |piece| {
            any = true;
            match eq_term(&column.name, piece) {
                Some(term) => doc.add_text(fields.eq, term),
                None => ok = false,
            }
            ok = ok && add_tokens(&mut doc, fields.tok, &column.name, piece, analyzer);
        });
        if !ok {
            return None;
        }
        if any {
            doc.add_text(fields.exists, &column.name);
            if let Some(key) = numeric_key(column.kind, value) {
                doc.add_text(fields.num, eq_term(&column.name, &key)?);
            }
        }
    }
    if !text.is_empty() {
        doc.add_text(fields.text, text);
    }
    Some(doc)
}

/// The state a Tantivy index was last committed at.
fn payload_of(index: &tantivy::Index) -> Result<Option<State>, Fault> {
    let metas = index
        .load_metas()
        .or_fault(VaultErrorCode::IndexBroken, "read the lexical index")?;
    Ok(metas.payload.as_deref().and_then(State::parse))
}

/// The collections the stale file lists.
fn read_stale(index: &Index) -> Result<BTreeSet<String>, Error> {
    match fs::read_to_string(index.stale_file()) {
        Ok(text) => Ok(text.lines().map(str::to_owned).collect()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(err) => Err(io(err)).context("read the stale collections"),
    }
}

/// The columns file; `None` when there is none.
fn read_dump(index: &Index) -> Result<Option<Dump>, Error> {
    match fs::read(index.columns_file()) {
        Ok(bytes) => Ok(Some(Dump::parse(&bytes)?)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io(err)).context("read the columns"),
    }
}

/// The lexical side of the Index.
pub(crate) struct Lexical {
    projector: Option<Arc<ProjectorPort>>,
}

impl Lexical {
    /// The side of a handle, with the Projector it was opened with.
    pub(crate) fn new(projector: Option<Arc<ProjectorPort>>) -> Lexical {
        Lexical { projector }
    }

    /// The columns of every indexed collection now, and what changed from
    /// the file: the dump to record, and the collections that are stale.
    fn columns_now(
        &self,
        eligible: &[&Declaration],
        previous: Option<&Dump>,
        mut stale: BTreeSet<String>,
    ) -> Result<(Dump, BTreeSet<String>), Error> {
        let mut dump = Dump::new();
        for decl in eligible {
            let name = decl.name();
            let now = match &self.projector {
                Some(projector) => projector
                    .columns(name.to_owned())
                    .context("ask for the columns")?
                    .as_ref()
                    .map(dump_columns)
                    .transpose()
                    .context("read the columns")?
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            let before = previous.and_then(|dump| dump.collections.get(name));
            let keep = match before {
                Some(before) if stale.contains(name) || *before != now => {
                    stale.insert(name.to_owned());
                    before.clone()
                }
                _ => now,
            };
            dump.collections.insert(name.to_owned(), keep);
        }
        Ok((dump, stale))
    }
}

/// What a round writes, and what it has read since it last said so.
struct Work<'a> {
    writer: IndexWriter,
    fields: Fields,
    analyzer: TextAnalyzer,
    projector: Option<&'a ProjectorPort>,
    read: u64,
    skipped: u64,
}

impl Work<'_> {
    fn flush(&mut self, round: &mut Round<'_>) -> Result<(), Error> {
        let (read, skipped) = (self.read, self.skipped);
        self.read = 0;
        self.skipped = 0;
        round.note(read, skipped, 0)
    }

    fn tick(&mut self, round: &mut Round<'_>) -> Result<(), Error> {
        if self.read + self.skipped >= NOTE_EVERY {
            self.flush(round)?;
        }
        Ok(())
    }

    fn forget(&self, collection: &str, id: &str) {
        self.writer.delete_term(Term::from_field_text(
            self.fields.key,
            &key_of(collection, id),
        ));
    }

    fn forget_file(&self, file: &str) {
        self.writer
            .delete_term(Term::from_field_text(self.fields.file, file));
    }

    fn forget_collection(&self, collection: &str) {
        self.writer
            .delete_term(Term::from_field_text(self.fields.collection, collection));
    }

    /// Replaces the Tantivy document of `id`. `raw` goes to the Projector,
    /// which is asked only when the collection has columns. A Document the
    /// Projector fails on is left out; a retryable failure is returned.
    fn put(
        &mut self,
        collection: &str,
        columns: &[DumpColumn],
        id: &str,
        order: &str,
        file: &str,
        raw: Vec<u8>,
    ) -> Result<(), Error> {
        self.forget(collection, id);
        let values = match self.projector {
            Some(projector) if !columns.is_empty() => {
                match projector.project(collection.to_owned(), id.to_owned(), raw) {
                    Ok(values) => values,
                    Err(error) if error.retryable() => {
                        return Err(error).context("project a Document");
                    }
                    Err(_) => {
                        self.skipped += 1;
                        return Ok(());
                    }
                }
            }
            _ => None,
        };
        let parts = Parts {
            collection,
            id,
            order,
            file,
            columns,
            values: values.as_ref(),
        };
        match tantivy_document(self.fields, &mut self.analyzer, &parts) {
            Some(doc) => {
                self.writer
                    .add_document(doc)
                    .map_err(|err| write_fault(err, "add a document to the lexical index"))?;
            }
            None => self.skipped += 1,
        }
        Ok(())
    }

    /// The Documents of the files of a `standard` or `singleton` collection.
    fn files(
        &mut self,
        round: &mut Round<'_>,
        decl: &Declaration,
        columns: &[DumpColumn],
        whole: bool,
        deltas: &[(String, bool)],
    ) -> Result<(), Error> {
        let snap = round.views().of(decl.repo());
        let name = decl.name();
        if whole {
            self.forget_collection(name);
            let mut walk = Walk::new(snap, decl, None).context("walk the collection")?;
            while let Some(id) = walk.next_id().context("walk the collection")? {
                let id = id.to_owned();
                let Some(raw) = walk.read().context("read a Document")? else {
                    continue;
                };
                let file = isolated::vault_rel(decl, &id);
                self.put(name, columns, &id, &id, &file, raw)?;
                self.read += 1;
                self.tick(round)?;
            }
            return Ok(());
        }
        let dir = isolated::vault_rel(decl, "");
        for (path, deleted) in deltas {
            let Some(id) = path.strip_prefix(dir.as_str()) else {
                continue;
            };
            if documents::path_of(decl, id).is_err() {
                continue;
            }
            if *deleted {
                self.forget(name, id);
                continue;
            }
            match documents::read(snap, decl, id, None) {
                Ok(Some(raw)) => {
                    self.put(name, columns, id, id, path, raw)?;
                    self.read += 1;
                }
                Ok(None) => self.forget(name, id),
                Err(_) => {
                    self.forget(name, id);
                    self.skipped += 1;
                }
            }
            self.tick(round)?;
        }
        Ok(())
    }

    /// The lines of the segments of a journal.
    fn segments(
        &mut self,
        round: &mut Round<'_>,
        decl: &Declaration,
        columns: &[DumpColumn],
        whole: bool,
        deltas: &[(String, bool)],
    ) -> Result<(), Error> {
        let snap = round.views().of(decl.repo());
        let name = decl.name();
        let dir = isolated::vault_rel(decl, "");
        let touched: BTreeSet<&str> = deltas
            .iter()
            .filter(|(path, _)| path.starts_with(dir.as_str()))
            .map(|(path, _)| path.as_str())
            .collect();
        if whole {
            self.forget_collection(name);
        } else {
            for path in &touched {
                self.forget_file(path);
            }
        }
        for path in journal::list_segments(snap, decl).context("list the segments")? {
            let rel = journal::rel_str(&path);
            if !whole && !touched.contains(rel.as_str()) {
                continue;
            }
            self.segment(round, snap, name, columns, &path, &rel)?;
        }
        Ok(())
    }

    /// The lines of one segment. The lines are the journal's, which the id
    /// maps count; only the lines left out are noted here.
    fn segment(
        &mut self,
        round: &mut Round<'_>,
        snap: &Snap,
        collection: &str,
        columns: &[DumpColumn],
        path: &Path,
        rel: &str,
    ) -> Result<(), Error> {
        let Some(bytes) = journal::read_segment(snap, path).context("read a segment")? else {
            return Ok(());
        };
        let (lines, _tail) = journal::split_lines(&bytes);
        for (at, line) in lines.into_iter().enumerate() {
            let Ok(line) = journal::parse_line(line) else {
                continue;
            };
            let order = journal_order(path, at);
            self.put(collection, columns, &line.id, &order, rel, line.payload)?;
            self.tick(round)?;
        }
        Ok(())
    }
}

impl Side for Lexical {
    fn apply(&mut self, round: &mut Round<'_>) -> Result<(), Error> {
        let index = round.index();
        let declarations = round.declarations();
        let eligible: Vec<&Declaration> = declarations
            .iter()
            .filter(|decl| decl.lexical_indexed())
            .collect();
        let previous = read_dump(index).context("read the recorded columns")?;
        let (dump, stale) = self
            .columns_now(&eligible, previous.as_ref(), read_stale(index)?)
            .context("compare the columns")?;

        let tantivy = open_or_create(index)?;
        let fields = Fields::of(&tantivy.schema())?;
        let to = round.to();
        let reflected = payload_of(&tantivy)?;
        let incremental = previous.is_some() && round.from().is_some() && reflected == round.from();
        let reached = reflected == Some(to) && previous.is_some();

        let writer = tantivy
            .writer_with_num_threads(1, WRITER_HEAP)
            .map_err(|err| write_fault(err, "open the writer of the lexical index"))?;
        let analyzer = tantivy
            .tokenizer_for_field(fields.tok)
            .map_err(|err| write_fault(err, "open the tokenizer of the lexical index"))?;
        let mut work = Work {
            writer,
            fields,
            analyzer,
            projector: self.projector.as_deref(),
            read: 0,
            skipped: 0,
        };
        if !reached {
            if !incremental {
                work.writer
                    .delete_all_documents()
                    .map_err(|err| write_fault(err, "empty the lexical index"))?;
            }
            let eligible_names: BTreeSet<&str> = eligible.iter().map(|decl| decl.name()).collect();
            let gone: Vec<String> = round
                .declaration_changes()
                .filter(|name| !eligible_names.contains(name))
                .map(str::to_owned)
                .collect();
            for name in &gone {
                work.forget_collection(name);
            }
            let deltas: Vec<(String, bool)> = round
                .deltas()
                .iter()
                .map(|delta| (delta.path.clone(), delta.deleted))
                .collect();
            for decl in &eligible {
                let name = decl.name();
                if stale.contains(name) {
                    continue;
                }
                let columns = dump.columns(name).unwrap_or_default();
                let whole = !incremental || round.is_whole(name);
                match decl.kind() {
                    Kind::Standard | Kind::Singleton => {
                        work.files(round, decl, columns, whole, &deltas)
                            .context("index the Documents of a collection")?;
                    }
                    Kind::Journal => {
                        work.segments(round, decl, columns, whole, &deltas)
                            .context("index the lines of a journal")?;
                    }
                }
            }
            work.flush(round)?;
        }
        commit(work.writer, to)?;
        crate::fs::restrict_tree(&index.lexical_dir())?;

        write_if_changed(index, &index.columns_file(), &dump.encode()?)?;
        let listed: String = stale.iter().fold(String::new(), |mut out, name| {
            out.push_str(name);
            out.push('\n');
            out
        });
        if !stale.is_empty() {
            write_if_changed(index, &index.stale_file(), listed.as_bytes())?;
        }
        Ok(())
    }
}

/// Commits the writer with the state `to` as its payload and waits for the
/// merges it started, so the files are final and the writer lock is let go.
fn commit(mut writer: IndexWriter, to: State) -> Result<(), Error> {
    let mut prepared = writer
        .prepare_commit()
        .map_err(|err| write_fault(err, "prepare the commit of the lexical index"))?;
    prepared.set_payload(&to.line());
    prepared
        .commit()
        .map_err(|err| write_fault(err, "commit the lexical index"))?;
    writer
        .wait_merging_threads()
        .map_err(|err| write_fault(err, "finish the merges of the lexical index"))?;
    Ok(())
}

/// Writes `bytes` whole to `path` of the Index unless it holds them.
fn write_if_changed(index: &Index, path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if fs::read(path).is_ok_and(|have| have == bytes) {
        return Ok(());
    }
    index.write(path, bytes)
}

/// The Tantivy index of the Index, made when there is none.
fn open_or_create(index: &Index) -> Result<tantivy::Index, Error> {
    let dir = index.lexical_dir();
    let exists = dir.join("meta.json").is_file();
    if exists {
        return Ok(tantivy::Index::open_in_dir(&dir)
            .or_fault(VaultErrorCode::IndexBroken, "open the lexical index")?);
    }
    fs::create_dir_all(&dir)
        .map_err(io)
        .context("create the lexical index folder")?;
    let (schema, _) = Fields::build();
    Ok(tantivy::Index::create_in_dir(&dir, schema)
        .map_err(|err| write_fault(err, "create the lexical index"))?)
}

/// The lexical index at one state, for a query: a searcher that keeps the
/// state whatever the indexer does meanwhile, the fields, and the columns.
#[derive(Debug)]
pub(crate) struct Snapshot {
    committed: State,
    state: State,
    searcher: Searcher,
    index: tantivy::Index,
    fields: Fields,
    columns: Dump,
}

impl Snapshot {
    /// The state of the Vault this snapshot reflects.
    pub(crate) fn state(&self) -> State {
        self.state
    }

    /// The searcher. It keeps the state it was made at for as long as it is
    /// held.
    pub(crate) fn searcher(&self) -> &Searcher {
        &self.searcher
    }

    /// The Tantivy index, for the parsers and tokenizers a query needs.
    pub(crate) fn tantivy(&self) -> &tantivy::Index {
        &self.index
    }

    /// The fields of the schema.
    pub(crate) fn fields(&self) -> Fields {
        self.fields
    }

    /// The columns the Index holds for `collection`; see [`Dump::columns`].
    pub(crate) fn columns(&self, collection: &str) -> Option<&[DumpColumn]> {
        self.columns.columns(collection)
    }
}

/// The reader of the lexical index of one handle. It opens the index at the
/// state the Index's `commit` names and keeps that snapshot until `commit`
/// changes.
#[derive(Debug)]
pub(crate) struct Reader {
    index: Index,
    cache: Mutex<Option<Arc<Snapshot>>>,
}

impl Reader {
    /// The reader of `index`. Nothing is read.
    pub(crate) fn new(index: Index) -> Reader {
        Reader {
            index,
            cache: Mutex::new(None),
        }
    }

    fn rebuilding(what: &'static str) -> Error {
        Fault::new(VaultErrorCode::IndexRebuilding, what).into()
    }

    /// The lexical index at the state the Index reflects now. A snapshot
    /// obtained earlier keeps its own state.
    ///
    /// # Errors
    ///
    /// `INDEX_REBUILDING` (retryable) when the Index has no commit, because
    /// no round has run yet or a rebuild runs, and when it is of another
    /// format, which the indexer makes again; `INDEX_STALE` when the
    /// Projector's columns changed since the Index was built (`Reindex`);
    /// `INDEX_BROKEN` when the lexical index cannot be opened or read
    /// (`Reindex`).
    pub(crate) fn snapshot(&self) -> Result<Arc<Snapshot>, Error> {
        if !self.index.has_format()? {
            return Err(Self::rebuilding(
                "the Index is not built in this format yet",
            ));
        }
        let Some(committed) = self.index.read_commit()? else {
            return Err(Self::rebuilding("the Index has no commit yet"));
        };
        if self.index.stale_file().is_file() {
            return Err(Fault::new(
                VaultErrorCode::IndexStale,
                "the columns of the Projector changed since the Index was built; run Reindex",
            )
            .into());
        }
        let mut cache = error::mutex(&self.cache, "read the lexical snapshot")?;
        if let Some(snapshot) = cache.as_ref()
            && snapshot.committed == committed
        {
            return Ok(Arc::clone(snapshot));
        }
        match self.open(committed) {
            Ok(snapshot) => {
                let snapshot = Arc::new(snapshot);
                *cache = Some(Arc::clone(&snapshot));
                Ok(snapshot)
            }
            Err(fault) => {
                *cache = None;
                // A rebuild may have emptied the folder under this look.
                if self.index.read_commit()? == Some(committed) && self.index.has_format()? {
                    Err(fault.into())
                } else {
                    Err(Self::rebuilding("the Index is being rebuilt"))
                }
            }
        }
    }

    fn open(&self, committed: State) -> Result<Snapshot, Fault> {
        let index = tantivy::Index::open_in_dir(self.index.lexical_dir())
            .or_fault(VaultErrorCode::IndexBroken, "open the lexical index")?;
        let fields = Fields::of(&index.schema())?;
        let state = payload_of(&index)?
            .ok_or_else(|| broken("the lexical index has no state of the Vault"))?;
        let reader: tantivy::IndexReader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .or_fault(VaultErrorCode::IndexBroken, "read the lexical index")?;
        let searcher = reader.searcher();
        let columns = match fs::read(self.index.columns_file()) {
            Ok(bytes) => Dump::parse(&bytes)?,
            Err(err) => {
                return Err(broken("the columns of the Index cannot be read").with_source(err));
            }
        };
        Ok(Snapshot {
            committed,
            state,
            searcher,
            index,
            fields,
            columns,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use buffa_types::google::protobuf::Empty;
    use protocontract::Failure;
    use scheda_api::v1::{
        Collection, CollectionKey, ColumnKind, Document, DocumentCreation, DocumentDeletion,
        DocumentKey, DocumentUpsert, IndexColumn, IndexDoc, IndexSchema, IndexState, IndexValue,
        OpenOptions, ProjectorErrorCode, ProjectorSync, RawDocument, VaultErrorCode, index_value,
        rebuild,
    };
    use tantivy::collector::DocSetCollector;
    use tantivy::query::{AllQuery, Query, QueryParser, TermQuery};
    use tantivy::schema::{IndexRecordOption, Value as _};
    use tantivy::{DocAddress, TantivyDocument, Term};

    use super::{Fields, Reader, Snapshot, key_of};
    use crate::Vault;
    use crate::error::Error;
    use crate::index::{Index, State};
    use crate::indexer::hooks;
    use crate::ports::{Port, Ports};
    use crate::repo::Repos;

    const DEADLINE: Duration = Duration::from_secs(30);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-lexical-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// What a test tells its projector, and what the projector tells back.
    struct Control {
        columns: Mutex<Vec<IndexColumn>>,
        unavailable: AtomicBool,
        calls: Mutex<Vec<(String, String)>>,
    }

    fn column(name: &str, kind: ColumnKind, find: bool, fts: bool) -> IndexColumn {
        IndexColumn::default()
            .with_name(name)
            .with_kind(kind)
            .with_find(find)
            .with_fts(fts)
    }

    fn default_columns() -> Vec<IndexColumn> {
        vec![
            column("title", ColumnKind::COLUMN_KIND_TEXT, true, true),
            column("body", ColumnKind::COLUMN_KIND_TEXT, false, true),
            column("n", ColumnKind::COLUMN_KIND_INTEGER, true, false),
            column("tags", ColumnKind::COLUMN_KIND_TEXT_LIST, true, false),
            column("done", ColumnKind::COLUMN_KIND_BOOLEAN, true, false),
            column("score", ColumnKind::COLUMN_KIND_NUMBER, true, false),
        ]
    }

    impl Control {
        fn new() -> Arc<Control> {
            Arc::new(Control {
                columns: Mutex::new(default_columns()),
                unavailable: AtomicBool::new(false),
                calls: Mutex::new(Vec::new()),
            })
        }
    }

    /// A projector of JSON Documents: each member of the object whose name is
    /// a column is the value of that column.
    struct Json(Arc<Control>);

    impl ProjectorSync for Json {
        fn columns(&self, _collection: CollectionKey) -> Result<IndexSchema, Error> {
            Ok(IndexSchema {
                columns: self.0.columns.lock().unwrap().clone(),
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            let collection = document.collection.clone().unwrap_or_default();
            let id = document.id.clone().unwrap_or_default();
            self.0.calls.lock().unwrap().push((collection, id));
            let raw = document.raw.unwrap_or_default();
            let text = String::from_utf8_lossy(&raw).into_owned();
            if text.contains("unavailable") && self.0.unavailable.load(Ordering::SeqCst) {
                return Err(Failure::new(ProjectorErrorCode::Unavailable, "down")
                    .with_retryable(true)
                    .into());
            }
            if text.contains("refuse") {
                return Err(Failure::new(ProjectorErrorCode::Refused, "no").into());
            }
            let json: serde_json::Value = serde_json::from_slice(&raw).unwrap_or_default();
            let mut doc = IndexDoc::default();
            let names: Vec<String> = self
                .0
                .columns
                .lock()
                .unwrap()
                .iter()
                .filter_map(|column| column.name.clone())
                .collect();
            for name in names {
                let Some(value) = json.get(&name) else {
                    continue;
                };
                let value = match value {
                    serde_json::Value::Null => {
                        index_value::Value::from(index_value::Null::default())
                    }
                    serde_json::Value::String(text) => index_value::Value::Text(text.clone()),
                    serde_json::Value::Bool(b) => index_value::Value::Boolean(*b),
                    serde_json::Value::Number(n) if n.is_i64() => {
                        index_value::Value::Integer(n.as_i64().unwrap_or(0))
                    }
                    serde_json::Value::Number(n) => {
                        index_value::Value::Number(n.as_f64().unwrap_or(0.0))
                    }
                    serde_json::Value::Array(items) => {
                        index_value::Value::from(index_value::TextList {
                            texts: items
                                .iter()
                                .filter_map(|item| item.as_str().map(str::to_owned))
                                .collect(),
                            ..Default::default()
                        })
                    }
                    serde_json::Value::Object(_) => continue,
                };
                doc.values.insert(
                    name,
                    IndexValue {
                        value: Some(value),
                        ..Default::default()
                    },
                );
            }
            Ok(doc)
        }
    }

    fn options(extra: serde_json::Value) -> OpenOptions {
        let mut base = serde_json::json!({"indexPollMs": 10, "writeTimeoutMs": 60000});
        if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
            base.extend(extra.clone());
        }
        serde_json::from_value(base).unwrap()
    }

    fn ports(control: &Arc<Control>) -> Ports {
        Ports {
            projector: Some(Port::Sync(Arc::new(Json(Arc::clone(control))))),
            ..Ports::default()
        }
    }

    fn collection(value: serde_json::Value) -> Collection {
        serde_json::from_value(value).unwrap()
    }

    fn head(path: &Path) -> String {
        State::of(&Repos::new(path).views().unwrap()).token()
    }

    /// The states of a handle's indexer, forwarded by a thread so a wait can
    /// have a deadline.
    struct Items(Receiver<IndexState>);

    impl Items {
        fn of(vault: &Vault) -> Items {
            let mut watch = vault.watch_index_status(Empty::default()).unwrap();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                while let Some(Ok(state)) = watch.next() {
                    if tx.send(state).is_err() {
                        break;
                    }
                }
            });
            Items(rx)
        }

        /// Reads items until `done` is true of one, or fails the test at the
        /// deadline.
        fn until(&self, what: &str, mut done: impl FnMut(&IndexState) -> bool) {
            let deadline = Instant::now() + DEADLINE;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.0.recv_timeout(left) {
                    Ok(state) if done(&state) => return,
                    Ok(_) => {}
                    Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for {what}"),
                    Err(RecvTimeoutError::Disconnected) => panic!("the stream ended: {what}"),
                }
            }
        }

        /// Waits until the Index of `vault` has reached `HEAD` of the Vault at
        /// `path`: it has already, or the stream says so.
        fn built(&self, vault: &Vault, path: &Path) {
            let want = head(path);
            let reached = |state: &IndexState| {
                state.version.as_deref() == Some(want.as_str())
                    && state.pending == Some(0)
                    && state.role != Some(true)
            };
            if reached(&vault.get_index_status(Empty::default()).unwrap()) {
                return;
            }
            self.until("the Index to reach HEAD", reached);
        }
    }

    /// A Vault on disk with the handle, its stream, and the control.
    struct Fixture {
        _scratch: Scratch,
        path: PathBuf,
        vault: Vault,
        items: Items,
        control: Arc<Control>,
    }

    impl Fixture {
        fn new(name: &str, collections: &[serde_json::Value]) -> Fixture {
            Fixture::open(name, collections, true)
        }

        fn open(name: &str, collections: &[serde_json::Value], projector: bool) -> Fixture {
            let scratch = Scratch::new(name);
            let control = Control::new();
            let ports = if projector {
                ports(&control)
            } else {
                Ports::default()
            };
            let vault = Vault::open(
                scratch.0.join("vault"),
                options(serde_json::json!({})),
                ports,
            )
            .unwrap();
            for value in collections {
                vault.create_collection(collection(value.clone())).unwrap();
            }
            let path = scratch.0.join("vault").canonicalize().unwrap();
            let items = Items::of(&vault);
            let fixture = Fixture {
                _scratch: scratch,
                path,
                vault,
                items,
                control,
            };
            fixture.built();
            fixture
        }

        fn built(&self) {
            self.items.built(&self.vault, &self.path);
        }

        fn upsert(&self, collection: &str, id: &str, raw: &str) {
            self.vault
                .upsert_document(DocumentUpsert {
                    document: Document {
                        collection: Some(collection.to_owned()),
                        id: Some(id.to_owned()),
                        raw: Some(raw.as_bytes().to_vec()),
                        ..Default::default()
                    }
                    .into(),
                    note: Some("test".to_owned()),
                    ..Default::default()
                })
                .unwrap();
        }

        fn append(&self, collection: &str, id: &str, raw: &str) {
            self.vault
                .create_document(DocumentCreation {
                    document: Document {
                        collection: Some(collection.to_owned()),
                        id: Some(id.to_owned()),
                        raw: Some(raw.as_bytes().to_vec()),
                        ..Default::default()
                    }
                    .into(),
                    note: Some("test".to_owned()),
                    ..Default::default()
                })
                .unwrap();
        }

        fn remove(&self, collection: &str, id: &str) {
            self.vault
                .delete_document(DocumentDeletion {
                    document: DocumentKey {
                        collection: Some(collection.to_owned()),
                        id: Some(id.to_owned()),
                        ..Default::default()
                    }
                    .into(),
                    note: Some("test".to_owned()),
                    ..Default::default()
                })
                .unwrap();
        }

        fn snapshot(&self) -> Arc<Snapshot> {
            self.vault.lexical().snapshot().unwrap()
        }

        fn index(&self) -> Index {
            Index::new(&self.path)
        }
    }

    fn standard(name: &str) -> serde_json::Value {
        serde_json::json!({"name": name, "schema": {"standard": {}}})
    }

    fn journal(name: &str) -> serde_json::Value {
        serde_json::json!({"name": name, "schema": {"journal": {}}})
    }

    fn doc_of(snapshot: &Snapshot, address: DocAddress) -> TantivyDocument {
        snapshot.searcher().doc(address).unwrap()
    }

    fn stored(doc: &TantivyDocument, field: tantivy::schema::Field) -> String {
        doc.get_first(field)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned()
    }

    /// `(collection, id)` of the documents `query` matches, sorted.
    fn found(snapshot: &Snapshot, query: &dyn Query) -> Vec<(String, String)> {
        let fields = snapshot.fields();
        let mut out: Vec<(String, String)> = snapshot
            .searcher()
            .search(query, &DocSetCollector)
            .unwrap()
            .into_iter()
            .map(|address| {
                let doc = doc_of(snapshot, address);
                (stored(&doc, fields.collection), stored(&doc, fields.id))
            })
            .collect();
        out.sort();
        out
    }

    fn term(field: tantivy::schema::Field, text: &str) -> TermQuery {
        TermQuery::new(Term::from_field_text(field, text), IndexRecordOption::Basic)
    }

    fn by_key(snapshot: &Snapshot, collection: &str, id: &str) -> bool {
        !found(
            snapshot,
            &term(snapshot.fields().key, &key_of(collection, id)),
        )
        .is_empty()
    }

    fn by_text(snapshot: &Snapshot, text: &str) -> Vec<String> {
        let parser = QueryParser::for_index(snapshot.tantivy(), vec![snapshot.fields().text]);
        let query = parser.parse_query(text).unwrap();
        found(snapshot, &*query)
            .into_iter()
            .map(|(_, id)| id)
            .collect()
    }

    fn by_eq(snapshot: &Snapshot, column: &str, value: &str) -> Vec<String> {
        let eq = super::eq_term(column, value).unwrap();
        found(snapshot, &term(snapshot.fields().eq, &eq))
            .into_iter()
            .map(|(_, id)| id)
            .collect()
    }

    #[test]
    fn a_document_is_found_by_id_and_by_a_term_replaced_and_deleted() {
        let fx = Fixture::new("standard", &[standard("notes")]);
        fx.upsert(
            "notes",
            "alpha",
            r#"{"title":"Quokka facts","body":"marsupial","n":7,"tags":["a","b"],"done":true,"score":2.5}"#,
        );
        fx.upsert("notes", "beta", r#"{"title":"Other","body":"plain"}"#);
        fx.built();
        let snap = fx.snapshot();
        assert_eq!(snap.state().token(), head(&fx.path));
        assert!(by_key(&snap, "notes", "alpha"));
        assert!(by_key(&snap, "notes", "beta"));
        assert_eq!(by_text(&snap, "quokka"), ["alpha"]);
        assert_eq!(by_text(&snap, "marsupial"), ["alpha"], "an fts column");
        assert_eq!(by_eq(&snap, "title", "Quokka facts"), ["alpha"]);
        assert_eq!(by_eq(&snap, "n", "7"), ["alpha"]);
        assert_eq!(by_eq(&snap, "tags", "b"), ["alpha"]);
        assert_eq!(by_eq(&snap, "done", "true"), ["alpha"]);
        assert_eq!(by_eq(&snap, "score", "2.5"), ["alpha"]);
        assert_eq!(
            by_eq(&snap, "body", "marsupial"),
            Vec::<String>::new(),
            "an fts column is not a find column"
        );

        fx.upsert("notes", "alpha", r#"{"title":"Numbat facts","n":8}"#);
        fx.built();
        let after = fx.snapshot();
        assert_eq!(by_text(&after, "numbat"), ["alpha"]);
        assert!(by_text(&after, "quokka").is_empty(), "the old term is gone");
        assert!(by_text(&after, "marsupial").is_empty());
        assert_eq!(after.searcher().num_docs(), 2, "replaced, not added");

        fx.remove("notes", "alpha");
        fx.built();
        let after = fx.snapshot();
        assert!(!by_key(&after, "notes", "alpha"));
        assert!(by_text(&after, "numbat").is_empty());
        assert!(by_key(&after, "notes", "beta"));
        assert_eq!(after.searcher().num_docs(), 1);
    }

    #[test]
    fn a_stored_null_and_a_missing_value_are_told_apart() {
        let fx = Fixture::new("null", &[standard("notes")]);
        fx.upsert("notes", "has", r#"{"title":"t","n":1}"#);
        fx.upsert("notes", "null", r#"{"title":"t","n":null}"#);
        fx.upsert("notes", "none", r#"{"title":"t"}"#);
        fx.built();
        let snap = fx.snapshot();
        let fields = snap.fields();
        let ids = |field, name: &str| -> Vec<String> {
            found(&snap, &term(field, name))
                .into_iter()
                .map(|(_, id)| id)
                .collect()
        };
        assert_eq!(ids(fields.exists, "n"), ["has"]);
        assert_eq!(ids(fields.null, "n"), ["null"]);
        let key = super::eq_term("n", &super::i64_key(1)).unwrap();
        assert_eq!(ids(fields.num, &key), ["has"]);
    }

    #[test]
    fn a_journal_line_is_found_and_its_order_follows_append_order_across_segments() {
        let fx = Fixture::new(
            "journal",
            &[serde_json::json!({
                "name": "log", "schema": {"journal": {"segmentMaxBytes": 90}}
            })],
        );
        let ids: Vec<String> = (0..12).map(|n| format!("l{n:02}")).collect();
        for id in &ids {
            fx.append("log", id, &format!(r#"{{"title":"line {id}"}}"#));
        }
        fx.built();
        let segments = std::fs::read_dir(fx.path.join("log"))
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().ends_with(".jsonl"))
            })
            .count();
        assert!(segments >= 3, "{segments} segments");

        let snap = fx.snapshot();
        assert_eq!(snap.searcher().num_docs(), 12);
        assert!(by_key(&snap, "log", "l07"));
        assert_eq!(by_text(&snap, "l07"), ["l07"]);
        let fields = snap.fields();
        let mut lines: Vec<(String, String)> = snap
            .searcher()
            .search(&AllQuery, &DocSetCollector)
            .unwrap()
            .into_iter()
            .map(|address| {
                let doc = doc_of(&snap, address);
                (stored(&doc, fields.order), stored(&doc, fields.id))
            })
            .collect();
        lines.sort();
        let by_order: Vec<String> = lines.into_iter().map(|(_, id)| id).collect();
        assert_eq!(
            by_order, ids,
            "the order key sorts as the lines were appended"
        );
    }

    #[test]
    fn a_segment_changed_by_hand_replaces_its_lines_and_a_removed_one_deletes_them() {
        let fx = Fixture::new("segment", &[journal("log")]);
        fx.append("log", "one", r#"{"title":"first"}"#);
        fx.append("log", "two", r#"{"title":"second"}"#);
        fx.built();
        assert_eq!(fx.snapshot().searcher().num_docs(), 2);
        let segment = std::fs::read_dir(fx.path.join("log"))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&fx.path)
                .args(["-c", "user.name=T", "-c", "user.email=t@example.test"])
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        };
        std::fs::write(&segment, "one: {\"title\":\"changed\"}\n").unwrap();
        git(&["add", "--all"]);
        git(&["commit", "--quiet", "--message", "by hand"]);
        fx.built();
        let snap = fx.snapshot();
        assert_eq!(by_text(&snap, "changed"), ["one"]);
        assert!(!by_key(&snap, "log", "two"), "a line the segment lost");
        std::fs::remove_file(&segment).unwrap();
        git(&["add", "--all"]);
        git(&["commit", "--quiet", "--message", "removed"]);
        fx.built();
        assert_eq!(fx.snapshot().searcher().num_docs(), 0);
    }

    #[test]
    fn no_projector_or_an_empty_index_doc_stores_identity_only() {
        // No projector.
        let fx = Fixture::open("identity", &[standard("notes"), journal("log")], false);
        fx.upsert("notes", "alpha", r#"{"title":"findable"}"#);
        fx.append("log", "line", r#"{"title":"findable"}"#);
        fx.built();
        let snap = fx.snapshot();
        assert!(by_key(&snap, "notes", "alpha"));
        assert!(by_key(&snap, "log", "line"));
        assert!(by_text(&snap, "findable").is_empty(), "nothing to rank");
        assert!(by_eq(&snap, "title", "findable").is_empty());

        // A projector that has values for no Document.
        let fx = Fixture::new("empty-doc", &[standard("notes")]);
        fx.upsert("notes", "alpha", r#"{"unknown":"member"}"#);
        fx.built();
        let snap = fx.snapshot();
        assert!(by_key(&snap, "notes", "alpha"));
        assert!(by_text(&snap, "member").is_empty());

        // A collection with no columns is indexed by identity too, and the
        // projector is not asked for its Documents.
        let fx = Fixture::open("no-columns", &[], true);
        fx.control.columns.lock().unwrap().clear();
        fx.vault
            .create_collection(collection(standard("notes")))
            .unwrap();
        fx.built();
        fx.upsert("notes", "alpha", r#"{"title":"findable"}"#);
        fx.built();
        let snap = fx.snapshot();
        assert!(by_key(&snap, "notes", "alpha"));
        assert!(fx.control.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_projector_that_refuses_a_document_leaves_it_out_and_the_round_goes_on() {
        let fx = Fixture::new("refuse", &[standard("notes")]);
        fx.upsert("notes", "bad", r#"{"title":"refuse this"}"#);
        fx.upsert("notes", "good", r#"{"title":"fine"}"#);
        fx.built();
        let snap = fx.snapshot();
        assert!(!by_key(&snap, "notes", "bad"));
        assert!(by_key(&snap, "notes", "good"));
        assert!(
            fx.path.join("notes/bad").is_file(),
            "the file of a Document left out stays"
        );
        let state = fx.vault.get_index_status(Empty::default()).unwrap();
        assert!(!state.last_error.is_set());
        // A Document that was indexed and is then refused leaves the Index.
        fx.upsert("notes", "good", r#"{"title":"refuse now"}"#);
        fx.built();
        assert!(!by_key(&fx.snapshot(), "notes", "good"));
    }

    #[test]
    fn a_retryable_projector_failure_fails_the_round_and_it_is_tried_again() {
        let fx = Fixture::new("unavailable", &[standard("notes")]);
        fx.control.unavailable.store(true, Ordering::SeqCst);
        fx.upsert("notes", "late", r#"{"title":"unavailable"}"#);
        fx.items
            .until("the failure", |state| state.last_error.is_set());
        let state = fx.vault.get_index_status(Empty::default()).unwrap();
        assert!(state.last_error.is_set());
        assert_ne!(state.version.as_deref(), Some(head(&fx.path).as_str()));
        fx.control.unavailable.store(false, Ordering::SeqCst);
        fx.built();
        assert!(by_key(&fx.snapshot(), "notes", "late"));
        assert!(
            !fx.vault
                .get_index_status(Empty::default())
                .unwrap()
                .last_error
                .is_set()
        );
    }

    #[test]
    fn a_collection_left_out_or_isolated_is_handled_as_declared() {
        let fx = Fixture::new(
            "declared",
            &[
                serde_json::json!({"name": "quiet", "schema": {"standard": {}, "excludeLexicalIndex": true}}),
                serde_json::json!({"name": "hidden", "schema": {"standard": {}, "isolated": true}}),
                serde_json::json!({"name": "hidden-log", "schema": {"journal": {}, "isolated": true}}),
                standard("notes"),
            ],
        );
        fx.upsert("quiet", "q", r#"{"title":"hush"}"#);
        fx.upsert("hidden", "h", r#"{"title":"nested"}"#);
        fx.append("hidden-log", "line", r#"{"title":"nested line"}"#);
        fx.upsert("notes", "n", r#"{"title":"plain"}"#);
        fx.built();
        let snap = fx.snapshot();
        assert!(!by_key(&snap, "quiet", "q"), "exclude_lexical_index");
        assert!(by_key(&snap, "hidden", "h"), "from the nested repository");
        assert!(by_key(&snap, "hidden-log", "line"));
        assert_eq!(by_text(&snap, "nested"), ["h", "line"]);
        assert!(by_key(&snap, "notes", "n"));
        assert!(
            fx.control
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|(c, _)| c != "quiet"),
            "the projector never saw it"
        );
        fx.remove("notes", "n");
        fx.built();
        assert!(!by_key(&fx.snapshot(), "notes", "n"));
    }

    #[cfg(feature = "encrypt")]
    #[test]
    fn the_plaintext_of_an_encrypted_collection_is_never_in_the_index() {
        const IDENTITY: &str =
            "AGE-SECRET-KEY-1RN2D82PZQK97437LWJVQJK4Q6EQVHLQ9A8Z0QTAXZ424HV8XZZPS5Q9860";
        const MARKER: &str = "zebraquokkamarker";
        let scratch = Scratch::new("sealed");
        let control = Control::new();
        let vault = Vault::open(
            scratch.0.join("vault"),
            options(serde_json::json!({"encryption": {"identity": IDENTITY}})),
            ports(&control),
        )
        .unwrap();
        vault
            .create_collection(collection(serde_json::json!({
                "name": "sealed", "schema": {"standard": {}, "encrypted": true}
            })))
            .unwrap();
        vault
            .create_collection(collection(serde_json::json!({
                "name": "sealed-log", "schema": {"journal": {}, "encrypted": true}
            })))
            .ok();
        vault
            .create_collection(collection(standard("notes")))
            .unwrap();
        let items = Items::of(&vault);
        let path = scratch.0.join("vault").canonicalize().unwrap();
        let raw = format!(r#"{{"title":"{MARKER}","body":"{MARKER}"}}"#);
        vault
            .upsert_document(DocumentUpsert {
                document: Document {
                    collection: Some("sealed".to_owned()),
                    id: Some("secret".to_owned()),
                    raw: Some(raw.clone().into_bytes()),
                    ..Default::default()
                }
                .into(),
                note: Some("test".to_owned()),
                ..Default::default()
            })
            .unwrap();
        vault
            .upsert_document(DocumentUpsert {
                document: Document {
                    collection: Some("notes".to_owned()),
                    id: Some("open".to_owned()),
                    raw: Some(br#"{"title":"visible"}"#.to_vec()),
                    ..Default::default()
                }
                .into(),
                note: Some("test".to_owned()),
                ..Default::default()
            })
            .unwrap();
        items.built(&vault, &path);
        let snap = vault.lexical().snapshot().unwrap();
        assert!(!by_key(&snap, "sealed", "secret"));
        assert!(by_key(&snap, "notes", "open"));
        assert!(by_text(&snap, MARKER).is_empty());
        assert!(
            control
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|(c, _)| c == "notes")
        );
        drop(snap);
        let mut files = Vec::new();
        collect_files(&path.join(".scheda/index"), &mut files);
        assert!(!files.is_empty());
        for file in files {
            let bytes = std::fs::read(&file).unwrap();
            assert!(
                !bytes
                    .windows(MARKER.len())
                    .any(|window| window == MARKER.as_bytes()),
                "the plaintext is in {}",
                file.display()
            );
        }
    }

    fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_files(&path, out);
            } else {
                out.push(path);
            }
        }
    }

    #[test]
    fn changed_columns_make_the_reader_stale_until_reindex_and_the_new_columns_are_used() {
        let fx = Fixture::new("stale", &[standard("notes")]);
        fx.upsert("notes", "alpha", r#"{"title":"old","extra":"newcolumn"}"#);
        fx.built();
        assert!(fx.vault.lexical().snapshot().is_ok());
        assert!(fx.snapshot().columns("notes").is_some_and(|c| c.len() == 6));

        fx.control.columns.lock().unwrap().push(column(
            "extra",
            ColumnKind::COLUMN_KIND_TEXT,
            true,
            true,
        ));
        fx.upsert("notes", "beta", r#"{"title":"second","extra":"newcolumn"}"#);
        fx.built();
        let error = fx.vault.lexical().snapshot().unwrap_err();
        assert!(error.is(VaultErrorCode::IndexStale), "{error}");
        assert!(!error.retryable());
        // Another handle's reader sees it too: the file is the record.
        let other = Reader::new(fx.index());
        assert!(other.snapshot().unwrap_err().is(VaultErrorCode::IndexStale));

        fx.vault.reindex(Empty::default()).unwrap();
        fx.items.until("the rebuild", |state| {
            state
                .rebuild
                .as_option()
                .and_then(|r| r.stage)
                .and_then(|s| s.as_known())
                == Some(rebuild::Stage::STAGE_DONE)
                && state.version.as_deref() == Some(head(&fx.path).as_str())
                && state.pending == Some(0)
                && !fx.index().stale_file().exists()
        });
        let snap = fx.snapshot();
        assert!(snap.columns("notes").is_some_and(|c| c.len() == 7));
        assert_eq!(by_text(&snap, "newcolumn"), ["alpha", "beta"]);
        assert!(other.snapshot().is_ok());
    }

    #[test]
    fn the_reader_is_rebuilding_without_a_commit_or_in_another_format_and_reads_afterwards() {
        let fx = Fixture::new("rebuilding", &[standard("notes")]);
        fx.upsert("notes", "alpha", r#"{"title":"kept"}"#);
        fx.built();
        assert!(fx.vault.lexical().snapshot().is_ok());

        // A rebuild held at its work: the commit is gone.
        hooks::stretch(&fx.path, Duration::from_millis(1500));
        fx.vault.reindex(Empty::default()).unwrap();
        fx.items.until("the rebuild to begin", |state| {
            state.version.is_none()
                && state
                    .rebuild
                    .as_option()
                    .and_then(|r| r.stage)
                    .and_then(|s| s.as_known())
                    == Some(rebuild::Stage::STAGE_RUNNING)
        });
        let error = fx.vault.lexical().snapshot().unwrap_err();
        assert!(error.is(VaultErrorCode::IndexRebuilding), "{error}");
        assert!(error.retryable());
        hooks::stretch(&fx.path, Duration::ZERO);
        fx.built();
        let snap = fx.snapshot();
        assert!(by_key(&snap, "notes", "alpha"));

        // Another format reads as a rebuild that is to come.
        let scratch = Scratch::new("format");
        let index = Index::new(&scratch.0);
        std::fs::create_dir_all(scratch.0.join(".scheda")).unwrap();
        let reader = Reader::new(index.clone());
        assert!(
            reader
                .snapshot()
                .unwrap_err()
                .is(VaultErrorCode::IndexRebuilding)
        );
        index.reset().unwrap();
        index.write_commit(State::default()).unwrap();
        std::fs::write(index.root().join("format"), "1\n").unwrap();
        let error = reader.snapshot().unwrap_err();
        assert!(error.is(VaultErrorCode::IndexRebuilding), "{error}");
    }

    #[test]
    fn a_damaged_lexical_directory_is_broken_and_reindex_repairs_it() {
        let fx = Fixture::new("damaged", &[standard("notes")]);
        for n in 0..5 {
            fx.upsert(
                "notes",
                &format!("d{n}"),
                &format!(r#"{{"title":"doc {n}","body":"text {n}"}}"#),
            );
        }
        fx.built();
        let dir = fx.index().lexical_dir();
        let mut files = Vec::new();
        collect_files(&dir, &mut files);
        let victim = files
            .iter()
            .filter(|file| file.extension().is_some_and(|ext| ext == "term"))
            .max_by_key(|file| std::fs::metadata(file).map_or(0, |m| m.len()))
            .unwrap();
        let len = std::fs::metadata(victim).unwrap().len();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(victim)
            .unwrap();
        file.set_len(len / 2).unwrap();
        drop(file);
        let reader = Reader::new(fx.index());
        let error = reader.snapshot().unwrap_err();
        assert!(error.is(VaultErrorCode::IndexBroken), "{error}");
        assert!(!error.retryable());

        fx.vault.reindex(Empty::default()).unwrap();
        fx.items.until("the rebuild", |state| {
            state
                .rebuild
                .as_option()
                .and_then(|r| r.stage)
                .and_then(|s| s.as_known())
                == Some(rebuild::Stage::STAGE_DONE)
                && state.version.as_deref() == Some(head(&fx.path).as_str())
        });
        let snap = reader.snapshot().unwrap();
        assert_eq!(snap.searcher().num_docs(), 5);
    }

    #[test]
    fn a_snapshot_keeps_its_state_after_a_round_and_a_new_one_sees_the_new_state() {
        let fx = Fixture::new("snapshot", &[standard("notes")]);
        fx.upsert("notes", "one", r#"{"title":"first"}"#);
        fx.built();
        let before = fx.snapshot();
        let again = fx.snapshot();
        assert!(Arc::ptr_eq(&before, &again), "cached while the commit is");
        fx.upsert("notes", "two", r#"{"title":"second"}"#);
        fx.remove("notes", "one");
        fx.built();
        assert_eq!(before.searcher().num_docs(), 1);
        assert!(by_key(&before, "notes", "one"), "the old state, whole");
        assert!(!by_key(&before, "notes", "two"));
        let after = fx.snapshot();
        assert!(!Arc::ptr_eq(&before, &after));
        assert!(by_key(&after, "notes", "two"));
        assert!(!by_key(&after, "notes", "one"));
        assert_ne!(before.state(), after.state());
        assert_eq!(after.state().token(), head(&fx.path));
    }

    #[test]
    fn pending_counts_files_and_not_documents() {
        let fx = Fixture::new("pending", &[standard("notes"), journal("log")]);
        for n in 0..5 {
            fx.append("log", &format!("l{n}"), r#"{"title":"x"}"#);
        }
        fx.upsert("notes", "a", r#"{"title":"x"}"#);
        fx.upsert("notes", "b", r#"{"title":"x"}"#);
        fx.built();
        hooks::stretch(&fx.path, Duration::from_millis(1500));
        fx.vault.reindex(Empty::default()).unwrap();
        fx.items
            .until("the rebuild to begin", |state| state.version.is_none());
        let pending = fx.vault.get_index_status(Empty::default()).unwrap().pending;
        hooks::stretch(&fx.path, Duration::ZERO);
        // One segment with five lines, and two Document files; the
        // declaration files are not read as Documents.
        assert_eq!(pending, Some(3));
        fx.built();
    }

    const CHILD_VAULT: &str = "SCHEDA_TEST_LEXICAL_VAULT";
    const CHILD_NAME: &str = "SCHEDA_TEST_LEXICAL_NAME";
    const PER_HANDLE: usize = 8;

    fn child(path: &Path, name: &str) {
        let control = Control::new();
        let vault = Vault::open(path, options(serde_json::json!({})), ports(&control)).unwrap();
        let items = Items::of(&vault);
        for n in 0..PER_HANDLE {
            vault
                .upsert_document(DocumentUpsert {
                    document: Document {
                        collection: Some("notes".to_owned()),
                        id: Some(format!("{name}-{n}")),
                        raw: Some(format!(r#"{{"title":"from {name}"}}"#).into_bytes()),
                        ..Default::default()
                    }
                    .into(),
                    note: Some("test".to_owned()),
                    ..Default::default()
                })
                .unwrap();
        }
        std::fs::write(path.join(format!(".scheda/ready-{name}")), "").unwrap();
        let go = path.join(".scheda/go");
        let deadline = Instant::now() + DEADLINE;
        while !go.exists() {
            assert!(Instant::now() < deadline, "no go");
            std::thread::sleep(Duration::from_millis(5));
        }
        items.built(&vault, path);
        vault.close().unwrap();
    }

    #[test]
    fn the_indexers_of_two_processes_take_turns_and_the_index_ends_with_every_document() {
        if let (Ok(path), Ok(name)) = (std::env::var(CHILD_VAULT), std::env::var(CHILD_NAME)) {
            child(Path::new(&path), &name);
            return;
        }
        let fx = Fixture::new("processes", &[standard("notes")]);
        let exe = std::env::current_exe().unwrap();
        let mut children: Vec<_> = ["x", "y"]
            .into_iter()
            .map(|name| {
                Command::new(&exe)
                    .args([
                        "--exact",
                        "lexical::tests::the_indexers_of_two_processes_take_turns_and_the_index_ends_with_every_document",
                        "--test-threads=1",
                    ])
                    .env(CHILD_VAULT, &fx.path)
                    .env(CHILD_NAME, name)
                    .spawn()
                    .unwrap()
            })
            .collect();
        for n in 0..PER_HANDLE {
            fx.upsert("notes", &format!("p-{n}"), r#"{"title":"from parent"}"#);
        }
        let deadline = Instant::now() + DEADLINE;
        while !["x", "y"]
            .iter()
            .all(|name| fx.path.join(format!(".scheda/ready-{name}")).exists())
        {
            assert!(Instant::now() < deadline, "the children did not write");
            std::thread::sleep(Duration::from_millis(5));
        }
        fx.built();
        std::fs::write(fx.path.join(".scheda/go"), "").unwrap();
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        fx.built();
        let snap = fx.snapshot();
        assert_eq!(snap.searcher().num_docs(), 3 * PER_HANDLE as u64);
        for name in ["x", "y", "p"] {
            for n in 0..PER_HANDLE {
                assert!(by_key(&snap, "notes", &format!("{name}-{n}")), "{name}-{n}");
            }
        }
        assert!(
            !fx.vault
                .get_index_status(Empty::default())
                .unwrap()
                .last_error
                .is_set()
        );
    }

    #[test]
    fn the_fields_of_a_schema_read_back() {
        let (schema, fields) = Fields::build();
        let back = Fields::of(&schema).unwrap();
        assert_eq!(back.order, fields.order);
        assert_eq!(back.tok, fields.tok);
    }
}
