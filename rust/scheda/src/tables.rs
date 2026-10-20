//! The Lance side of a round of the indexer: what each collection's table
//! of a generation is made of, and how a round brings it to a new state.
//!
//! **What is indexed.** Every collection that is not `encrypted`: each
//! Document of a `standard` or `singleton` collection, and each line of a
//! journal's segments, read from the commit of the state the round moves
//! to (isolated collections from the nested repository). Its identity
//! always; the values of its columns unless the collection is
//! `exclude_lexical_index`; the vectors of its `vector` columns unless it
//! is `exclude_vector_index`.
//!
//! **The Projector** gives the values, once per changed Document per
//! round; the Document's bytes are moved into the call. Without a
//! Projector, for a collection without columns, and for a Document it gives
//! no values for, only the identity is stored. A Document the Projector
//! refuses, or gives a value of another kind than its column's, is left
//! out of the Index; its file stays. A retryable failure of the Projector
//! fails the round instead, which is tried again.
//!
//! **The embedder** gets the texts of the `vector` columns whose digest
//! (the git blob id of the text) differs from the one the table holds for
//! that Document, so an unchanged text keeps its vector. The vectors it
//! gives are kept in memory until the round succeeds, so a round tried again
//! after a failure sends only the texts still without one. A text it
//! refuses keeps no vector.
//!
//! **A journal segment** that grew by appending (its old bytes are a
//! prefix of the new ones) gives only its new lines; one changed otherwise
//! is indexed again whole.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use git2::{ObjectType, Oid};
use scheda_api::v1::{
    EmbedderErrorCode, IndexDoc, IndexSchema, VaultErrorCode, index_value::Value,
};
use scheda_blocking::{Blocking, blocking};

use crate::collection::{Declaration, Declarations, Kind as CollectionKind};
use crate::documents::WalkState;
use crate::error::{Context, Error, Fault};
use crate::generation::{self, Commit, Embedder, Generation, Retired};
use crate::id::require_document_id;
use crate::index::{Counts, State};
use crate::ports::{EmbedderPort, Installed, ProjectorPort};
use crate::repo::{NodeKind, Repo, Repos, Snap};
use crate::store::{Column, Embedding, Kind, Row, Shape, Table};
use crate::{isolated, journal};

/// How many files a round reads, projects, and writes at a time.
const BATCH: usize = 256;

/// The ports a round of the Lance side calls, as the Vault holds them when
/// the round begins.
#[derive(Clone)]
pub(crate) struct Calls {
    projector: Option<Arc<ProjectorPort>>,
    embedder: Option<Arc<EmbedderPort>>,
}

impl Calls {
    /// The ports `installed` holds now.
    pub(crate) fn of(installed: &Installed) -> Calls {
        Calls {
            projector: installed.projector(),
            embedder: installed.embedder(),
        }
    }
}

/// What the Index would be, made by this Vault for one set of declared
/// collections: its ports, and the shape of the table of each collection it
/// indexes.
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub(crate) ports: generation::Ports,
    pub(crate) shapes: BTreeMap<String, Shape>,
    /// The collections the plan is of: a round brings exactly these
    /// forward, so a collection declared after the plan was made cannot
    /// lack its shape.
    pub(crate) declarations: Arc<Declarations>,
    /// The port this Vault lacks to index, when it lacks one: the embedder
    /// a `vector` column needs. Such a Vault reads the Index, and its
    /// searches run without the vector leg, but it writes nothing there;
    /// its shapes then carry no vector length.
    pub(crate) lacking: Option<String>,
}

/// Whether `decl` has a table: every collection but an encrypted one.
pub(crate) fn indexed(decl: &Declaration) -> bool {
    !decl.encrypted()
}

/// The plan of a Vault with `calls`, for `declarations`: the Projector is
/// asked for the columns of each collection.
///
/// # Errors
///
/// The failure of the Projector.
pub(crate) async fn plan(calls: &Calls, declarations: Arc<Declarations>) -> Result<Plan, Error> {
    let mut shapes = BTreeMap::new();
    let mut needs_embedder: Option<String> = None;
    for decl in declarations.iter().filter(|decl| indexed(decl)) {
        let schema: Option<IndexSchema> = match &calls.projector {
            Some(projector) => projector
                .columns(decl.name().to_owned())
                .await
                .with_context(|| format!("the columns of {}", decl.name()))?,
            None => None,
        };
        let columns: Vec<Column> = schema
            .map(|schema| schema.columns.iter().filter_map(Column::of).collect())
            .unwrap_or_default();
        let exclude_lexical = decl.excludes_columns();
        let exclude_vector = decl.excludes_vectors();
        let vectors: Vec<String> = if exclude_vector {
            Vec::new()
        } else {
            columns
                .iter()
                .filter(|column| column.vector)
                .map(|column| column.name.clone())
                .collect()
        };
        if !vectors.is_empty() && needs_embedder.is_none() {
            needs_embedder = Some(decl.name().to_owned());
        }
        shapes.insert(
            decl.name().to_owned(),
            Shape {
                columns: if exclude_lexical { Vec::new() } else { columns },
                vectors,
                dimensions: None,
            },
        );
    }
    let mut lacking = None;
    let embedder = match (&needs_embedder, &calls.embedder) {
        (None, _) => None,
        (Some(collection), None) => {
            lacking = Some(format!(
                "an embedder: the collection {collection} has a vector column"
            ));
            None
        }
        (Some(_), Some(embedder)) => {
            let dimensions = u32::try_from(embedder.dimensions()).unwrap_or(u32::MAX);
            for shape in shapes.values_mut() {
                if !shape.vectors.is_empty() {
                    shape.dimensions = Some(dimensions);
                }
            }
            Some(Embedder {
                model: embedder.model_id().to_owned(),
                dimensions,
            })
        }
    };
    let projection = calls
        .projector
        .as_ref()
        .map(|projector| projector.projection_id().to_owned());
    Ok(Plan {
        ports: generation::Ports {
            projection,
            embedder,
        },
        shapes,
        lacking,
        declarations,
    })
}

/// Whether a Vault with `calls`, for `declarations`, can rebuild the Index
/// whose live commit is `live`: it has the embedder its `vector` columns
/// need, and a Projector when the Index was built with one.
///
/// # Errors
///
/// `FAILED_PRECONDITION` naming the port the Vault lacks; the failure of the
/// Projector.
pub(crate) async fn can_rebuild(
    calls: &Calls,
    declarations: Arc<Declarations>,
    live: Option<&Commit>,
) -> Result<(), Error> {
    if let Some(what) = plan(calls, declarations).await?.lacking {
        return Err(Fault::new(
            VaultErrorCode::FailedPrecondition,
            format!("this Vault lacks {what}; give it one to rebuild the Index"),
        )
        .into());
    }
    if calls.projector.is_none() && live.is_some_and(|commit| commit.ports.projection.is_some()) {
        return Err(Fault::new(
            VaultErrorCode::FailedPrecondition,
            "this Vault lacks a Projector, and the Index was built with one; give it one to rebuild the Index",
        )
        .into());
    }
    Ok(())
}

/// Why `commit`, an Index built with other ports, is stale for a Vault of
/// `plan`; `None` when it is not.
pub(crate) fn stale(commit: &Commit, plan: &Plan) -> Option<String> {
    if commit.ports.projection != plan.ports.projection {
        return Some(String::from(
            "the Index was built by a Projector of another projection than this Vault's",
        ));
    }
    // An Index that holds no vector yet takes the embedder of the first
    // Vault that needs one; one that holds vectors keeps its model.
    if let (Some(built), Some(mine)) = (&commit.ports.embedder, &plan.ports.embedder)
        && built != mine
    {
        return Some(String::from(
            "the Index was built with another embedder model or vector length than this Vault's",
        ));
    }
    // A Vault without an embedder knows no vector length; the embedder of
    // one that has it was compared above.
    for (collection, table) in &commit.tables {
        if let Some(shape) = plan.shapes.get(collection)
            && (shape.columns != table.shape.columns
                || shape.vectors != table.shape.vectors
                || (plan.ports.embedder.is_some() && shape.dimensions != table.shape.dimensions))
        {
            return Some(format!(
                "the Index holds other columns of the collection {collection} than this Vault's Projector gives"
            ));
        }
    }
    None
}

/// The vectors a Vault keeps between two tries of a round: those the
/// embedder gave, by the digest of their text, and the texts it refused.
#[derive(Default)]
pub(crate) struct Cache {
    model: String,
    vectors: HashMap<String, Vec<f32>>,
    refused: HashSet<String>,
}

impl Cache {
    /// Empties the cache when it holds what another model made.
    fn keep_for(&mut self, model: &str) {
        if self.model != model {
            *self = Cache {
                model: model.to_owned(),
                ..Cache::default()
            };
        }
    }

    /// Forgets the vectors: the round that needed them succeeded.
    pub(crate) fn clear(&mut self) {
        self.vectors.clear();
        self.refused.clear();
    }
}

/// What a round of the Lance side works with.
pub(crate) struct Work<'a> {
    pub(crate) repos: Arc<Repos>,
    pub(crate) generation: Generation,
    pub(crate) commit: Commit,
    /// The plan of the state the next [`Work::bring_to`] brings the tables
    /// to.
    pub(crate) plan: Plan,
    pub(crate) calls: &'a Calls,
    pub(crate) cache: &'a mut Cache,
    /// Called with what was read; an error ends the round (the indexer
    /// stops).
    pub(crate) tick: &'a mut (dyn FnMut(Counts) -> Result<(), Error> + Send + Sync),
    /// Whether the indexer stops: looked at inside a call to the embedder.
    pub(crate) stopped: Arc<dyn Fn() -> bool + Send + Sync>,
    pub(crate) counts: Counts,
}

/// How a round finds what changed in one collection.
enum Scope {
    /// Every file of the collection.
    Whole,
    /// The files that differ from the state of the table, as a tree
    /// comparison gave them.
    Deltas(Vec<crate::repo::Delta>),
    /// The commit of the table is gone: compare the tree with the files the
    /// table holds.
    Compare,
}

/// One file to apply, as listed.
#[derive(Clone)]
enum Item {
    Document {
        id: String,
        path: String,
        blob: Oid,
    },
    DocumentGone {
        id: String,
    },
    Segment {
        path: String,
        blob: Oid,
        old: Option<Oid>,
    },
    SegmentGone {
        path: String,
    },
}

/// One file to apply, as read.
enum Read {
    Document {
        id: String,
        path: String,
        blob: Oid,
        bytes: Vec<u8>,
    },
    DocumentGone {
        id: String,
    },
    Lines {
        path: String,
        blob: Oid,
        /// Every line of the segment: its old rows go first.
        whole: bool,
        lines: Vec<(String, String, Vec<u8>)>,
        bad: u64,
    },
    SegmentGone {
        path: String,
    },
}

/// One Document to project: its identity, and its bytes.
struct Source {
    id: String,
    file: String,
    order: String,
    raw: Vec<u8>,
}

impl Work<'_> {
    /// Brings every collection of the generation from the state of its
    /// commit (none: from nothing) to the state `to`, whose snapshots are
    /// at the commits it names; `self.plan` is of the collections declared
    /// at `to`. Updates `self.commit`; writes no `commit`.
    ///
    /// # Errors
    ///
    /// A failure of git, of Lance, or of a port; the error the tick
    /// returns.
    pub(crate) async fn bring_to(&mut self, to: State) -> Result<(), Error> {
        let declarations = Arc::clone(&self.plan.declarations);
        if let Some(embedder) = &self.calls.embedder {
            self.cache.keep_for(embedder.model_id());
        }
        if self.commit.ports.embedder.is_none() {
            self.commit.ports.embedder = self.plan.ports.embedder.clone();
        }
        let from = self
            .commit
            .state()
            .filter(|_| !self.commit.tables.is_empty());
        let deltas = match from {
            Some(from) if from != to => Some(self.deltas(from, to).await?),
            Some(_) => Some(Some(Vec::new())),
            None => None,
        };
        // The tables of collections that went, or became encrypted.
        let gone: Vec<String> = self
            .commit
            .tables
            .keys()
            .filter(|name| declarations.get(name).is_none_or(|decl| !indexed(decl)))
            .cloned()
            .collect();
        for name in gone {
            if let Some(table) = self.commit.tables.remove(&name) {
                self.commit.dropped.push(Retired {
                    name: table.name,
                    at: generation::now_millis(),
                });
            }
        }
        for decl in declarations.iter().filter(|decl| indexed(decl)) {
            let name = decl.name();
            let Some(shape) = self.plan.shapes.get(name).cloned() else {
                unreachable!("a plan has the shape of every collection it indexes");
            };
            let known = self.commit.tables.get(name).cloned();
            let scope = match (&known, &deltas) {
                (None, _) | (_, None) => Scope::Whole,
                (Some(_), Some(None)) => Scope::Compare,
                (Some(_), Some(Some(deltas))) => {
                    if deltas
                        .iter()
                        .any(|delta| crate::collection::declaration_dir(&delta.path) == Some(name))
                    {
                        Scope::Whole
                    } else {
                        let dir = isolated::vault_rel(decl, "");
                        let mine: Vec<crate::repo::Delta> = deltas
                            .iter()
                            .filter(|delta| delta.path.starts_with(&dir))
                            .cloned()
                            .collect();
                        if mine.is_empty() {
                            continue;
                        }
                        Scope::Deltas(mine)
                    }
                }
            };
            let (table_name, committed) = match (&known, &scope) {
                (Some(table), Scope::Whole) => (table.name.clone(), None),
                (Some(table), _) => (table.name.clone(), Some(table.versions)),
                (None, _) => (self.commit.new_table(), None),
            };
            let mut table =
                Table::open(self.generation.dir(), &table_name, shape.clone(), committed)
                    .await
                    .with_context(|| format!("open the table of {name}"))?;
            self.apply(&mut table, decl, to, scope).await?;
            table
                .maintain(self.commit.rounds + 1)
                .await
                .with_context(|| format!("maintain the table of {name}"))?;
            self.commit.tables.insert(
                name.to_owned(),
                generation::Table {
                    name: table_name,
                    shape,
                    versions: table.versions(),
                },
            );
        }
        self.commit.state = to.line();
        self.commit.rounds += 1;
        Ok(())
    }

    /// The files that differ between the states `from` and `to`; `None`
    /// when a commit of `from` is gone from its repository.
    async fn deltas(
        &self,
        from: State,
        to: State,
    ) -> Result<Option<Vec<crate::repo::Delta>>, Error> {
        let repos = Arc::clone(&self.repos);
        blocking(move |_cx| {
            let Some(views) = repos.views_at(to.oid(Repo::Main), to.oid(Repo::Isolated))? else {
                return Err(gone_state());
            };
            let mut out = Vec::new();
            for repo in [Repo::Main, Repo::Isolated] {
                match views
                    .of(repo)
                    .changed_since(from.oid(repo), None)
                    .context("compare the commits")?
                {
                    Some(mut deltas) => out.append(&mut deltas),
                    None => return Ok(None),
                }
            }
            Ok(Some(out))
        })
        .await
    }

    /// Brings `table`, the table of `decl`, to the state `to`.
    async fn apply(
        &mut self,
        table: &mut Table,
        decl: &Declaration,
        to: State,
        scope: Scope,
    ) -> Result<(), Error> {
        let known = match scope {
            Scope::Compare => Some(table.files().await?),
            Scope::Deltas(_) if decl.kind() == CollectionKind::Journal => {
                Some(table.files().await?)
            }
            _ => None,
        };
        let items = {
            let repos = Arc::clone(&self.repos);
            let decl = decl.clone();
            blocking(move |cx| list(cx, &repos, &decl, to, scope, known.as_ref())).await?
        };
        for chunk in items.chunks(BATCH).map(<[Item]>::to_vec) {
            let reads = {
                let repos = Arc::clone(&self.repos);
                let decl = decl.clone();
                blocking(move |cx| read(cx, &repos, &decl, chunk)).await?
            };
            self.write(table, decl, reads).await?;
        }
        Ok(())
    }

    /// Writes what `reads` holds into `table`.
    async fn write(
        &mut self,
        table: &mut Table,
        decl: &Declaration,
        reads: Vec<Read>,
    ) -> Result<(), Error> {
        let mut gone_ids = Vec::new();
        let mut gone_files = Vec::new();
        let mut whole_files = Vec::new();
        let mut files = Vec::new();
        let mut sources = Vec::new();
        let mut counts = Counts::default();
        for read in reads {
            match read {
                Read::Document {
                    id,
                    path,
                    blob,
                    bytes,
                } => {
                    counts.documents_read += 1;
                    files.push((path.clone(), blob.to_string()));
                    sources.push(Source {
                        order: id.clone(),
                        id,
                        file: path,
                        raw: bytes,
                    });
                }
                Read::DocumentGone { id } => gone_ids.push(id),
                Read::Lines {
                    path,
                    blob,
                    whole,
                    lines,
                    bad,
                } => {
                    counts.segments_read += 1;
                    counts.documents_skipped += bad;
                    if whole {
                        whole_files.push(path.clone());
                    }
                    files.push((path.clone(), blob.to_string()));
                    for (id, order, payload) in lines {
                        counts.documents_read += 1;
                        sources.push(Source {
                            id,
                            file: path.clone(),
                            order,
                            raw: payload,
                        });
                    }
                }
                Read::SegmentGone { path } => gone_files.push(path),
            }
        }
        let (rows, left_out) = self.rows(table, decl, sources).await?;
        counts.documents_skipped += u64::try_from(left_out.len()).unwrap_or(u64::MAX);
        gone_ids.extend(left_out);
        let mut by_file = gone_files.clone();
        by_file.extend(whole_files);
        table.delete_rows("file", &by_file).await?;
        table.delete_rows("id", &gone_ids).await?;
        table.upsert(&rows).await?;
        table.remove_files(&gone_files).await?;
        table.put_files(&files).await?;
        self.counts.documents_read += counts.documents_read;
        self.counts.documents_skipped += counts.documents_skipped;
        self.counts.segments_read += counts.segments_read;
        (self.tick)(self.counts)
    }

    /// The rows of `sources`, projected and embedded, and the ids of the
    /// Documents left out of the Index.
    async fn rows(
        &mut self,
        table: &Table,
        decl: &Declaration,
        sources: Vec<Source>,
    ) -> Result<(Vec<Row>, Vec<String>), Error> {
        let shape = table.shape().clone();
        let projects = !shape.columns.is_empty() || !shape.vectors.is_empty();
        let mut rows = Vec::with_capacity(sources.len());
        let mut left_out = Vec::new();
        let mut texts: Vec<(usize, String, String)> = Vec::new();
        for source in sources {
            let mut row = Row {
                id: source.id,
                file: source.file,
                order: source.order,
                ..Row::default()
            };
            if projects && let Some(projector) = &self.calls.projector {
                let values = match projector
                    .project(decl.name().to_owned(), row.id.clone(), source.raw)
                    .await
                {
                    Ok(values) => values,
                    Err(error) if error.retryable() => {
                        return Err(error.context(format!("project {}", row.id)));
                    }
                    // Refused, or a reply that breaks its rules: left out.
                    Err(_) => {
                        left_out.push(row.id);
                        continue;
                    }
                };
                match values_of(&shape, values) {
                    Some((values, vector_texts)) => {
                        row.values = values;
                        for (column, text) in vector_texts {
                            texts.push((rows.len(), column, text));
                        }
                    }
                    None => {
                        left_out.push(row.id);
                        continue;
                    }
                }
            }
            rows.push(row);
        }
        if !texts.is_empty() {
            self.embed(table, &mut rows, texts).await?;
        }
        Ok((rows, left_out))
    }

    /// Gives `rows` the embeddings of `texts` (row, column, text): the one
    /// the table holds for the same text, else the one the cache holds,
    /// else a new one from the embedder.
    async fn embed(
        &mut self,
        table: &Table,
        rows: &mut [Row],
        texts: Vec<(usize, String, String)>,
    ) -> Result<(), Error> {
        let ids: Vec<String> = texts
            .iter()
            .map(|(row, _, _)| rows[*row].id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let stored = table.embeddings(&ids).await?;
        let mut wanted: Vec<(usize, String, String)> = Vec::new();
        for (row, column, text) in texts {
            let digest = digest_of(&text);
            let kept = stored
                .get(&rows[row].id)
                .and_then(|columns| columns.get(&column))
                .filter(|embedding| embedding.digest == digest)
                .map(|embedding| embedding.vector.clone());
            if let Some(vector) = kept {
                self.cache.vectors.insert(digest.clone(), vector);
            }
            if self.cache.refused.contains(&digest) {
                continue;
            }
            if !self.cache.vectors.contains_key(&digest) {
                wanted.push((row, column.clone(), text));
            }
            rows[row].embeddings.insert(
                column,
                Embedding {
                    vector: Vec::new(),
                    digest,
                },
            );
        }
        self.embed_wanted(wanted).await?;
        for row in rows.iter_mut() {
            row.embeddings.retain(
                |_, embedding| match self.cache.vectors.get(&embedding.digest) {
                    Some(vector) => {
                        embedding.vector = vector.clone();
                        true
                    }
                    None => false,
                },
            );
        }
        Ok(())
    }

    /// Asks the embedder for the vectors of `wanted` the cache lacks, once
    /// per distinct text, and keeps them in the cache. A text it refuses is
    /// remembered and keeps no vector.
    async fn embed_wanted(&mut self, wanted: Vec<(usize, String, String)>) -> Result<(), Error> {
        let Some(embedder) = self.calls.embedder.clone() else {
            return Err(Fault::new(
                VaultErrorCode::FailedPrecondition,
                "the Vault has no embedder for its vector columns",
            )
            .into());
        };
        let mut seen = HashSet::new();
        let mut texts: Vec<(String, String)> = wanted
            .into_iter()
            .filter_map(|(_, _, text)| {
                let digest = digest_of(&text);
                seen.insert(digest.clone()).then_some((digest, text))
            })
            .collect();
        while !texts.is_empty() {
            let (digests, batch): (Vec<String>, Vec<String>) = texts.drain(..).unzip();
            let stopped = Arc::clone(&self.stopped);
            let embedded = embedder.embed(batch.clone(), move || stopped()).await;
            let done = embedded.vectors.len();
            for (digest, vector) in digests.iter().zip(embedded.vectors) {
                self.cache.vectors.insert(digest.clone(), vector);
            }
            match embedded.failure {
                None => {}
                Some(error) if error.is(EmbedderErrorCode::Refused) && done < digests.len() => {
                    self.cache.refused.insert(digests[done].clone());
                    texts = digests.into_iter().zip(batch).skip(done + 1).collect();
                }
                Some(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// The failure of a state whose commit left its repository while a round
/// read it.
fn gone_state() -> Error {
    Fault::new(
        VaultErrorCode::Aborted,
        "a commit the round reads is gone from its repository",
    )
    .into()
}

/// The digest of a text: the git blob id of its bytes.
fn digest_of(text: &str) -> String {
    Oid::hash_object(ObjectType::Blob, text.as_bytes())
        .map(|oid| oid.to_string())
        .unwrap_or_default()
}

/// What a Document gives a table: the value of each column, by name, and
/// the text of each `vector` column, with that column.
type Projected = (BTreeMap<String, Value>, Vec<(String, String)>);

/// The values of `values` for the columns of `shape`, and the texts of its
/// `vector` columns; `None` when a value is of another kind than its
/// column's, which leaves the Document out. A key no column declares is
/// ignored, and a value that is not finite is left out.
fn values_of(shape: &Shape, values: Option<IndexDoc>) -> Option<Projected> {
    let mut out = BTreeMap::new();
    let mut texts = Vec::new();
    let Some(values) = values else {
        return Some((out, texts));
    };
    let mut values = values.values;
    for column in &shape.columns {
        let Some(value) = values
            .get(&column.name)
            .and_then(|value| value.value.clone())
        else {
            continue;
        };
        let fits = matches!(
            (column.kind, &value),
            (Kind::Text, Value::Text(_))
                | (Kind::Integer, Value::Integer(_))
                | (Kind::Number, Value::Number(_))
                | (Kind::Boolean, Value::Boolean(_))
                | (Kind::TextList, Value::TextList(_))
        );
        if !fits {
            return None;
        }
        if matches!(value, Value::Number(n) if !n.is_finite()) {
            continue;
        }
        out.insert(column.name.clone(), value);
    }
    for column in &shape.vectors {
        match values.remove(column).and_then(|value| value.value) {
            Some(Value::Text(text)) if !text.trim().is_empty() => {
                texts.push((column.clone(), text))
            }
            Some(Value::Text(_)) | None => {}
            Some(_) => return None,
        }
    }
    Some((out, texts))
}

/// The order of a journal line: the segment's place in append order, then
/// the line's place in the segment, as text that compares as bytes the way
/// the lines are appended.
pub(crate) fn journal_order(segment: &std::path::Path, line: usize) -> String {
    let (class, date, seq, writer, name) = journal::segment_order(segment);
    format!("{class}\u{1}{date}\u{1}{seq:08x}\u{1}{writer}\u{1}{name}\u{1}{line:010}")
}

/// The id of the Document at the Vault-relative `path` of `decl`, when the
/// path is one of its Document files.
fn document_id(decl: &Declaration, path: &str) -> Option<String> {
    let dir = isolated::vault_rel(decl, "");
    let id = path.strip_prefix(&dir)?;
    match decl.kind() {
        CollectionKind::Standard => require_document_id(id).ok().map(|()| id.to_owned()),
        CollectionKind::Singleton => (decl.singleton_id() == Some(id)).then(|| id.to_owned()),
        CollectionKind::Journal => None,
    }
}

/// Whether the Vault-relative `path` is a segment of the journal `decl`.
fn is_segment(decl: &Declaration, path: &str) -> bool {
    let dir = isolated::vault_rel(decl, "");
    path.strip_prefix(&dir).is_some_and(|name| {
        !name.contains('/') && !name.starts_with('.') && name.ends_with(".jsonl")
    })
}

/// The blob of the file at `path` in `snap`, when it is a regular file.
fn blob_at(snap: &Snap, path: &str) -> Result<Option<Oid>, Fault> {
    Ok(snap
        .lookup(path)?
        .filter(|node| node.kind == NodeKind::File)
        .map(|node| node.id))
}

/// The files of `decl` in the state `to`, by path, with their blob.
fn tree_files(snap: &Snap, decl: &Declaration) -> Result<BTreeMap<String, Oid>, Fault> {
    let mut out = BTreeMap::new();
    if decl.kind() == CollectionKind::Journal {
        for segment in journal::list_segments(snap, decl)? {
            let path = journal::rel_str(&segment);
            if let Some(blob) = blob_at(snap, &path)? {
                out.insert(path, blob);
            }
        }
        return Ok(out);
    }
    let mut walk = WalkState::new(snap, decl)?;
    while let Some(id) = walk.next_id(snap, decl)? {
        let path = isolated::vault_rel(decl, id);
        if let Some(blob) = walk.blob() {
            out.insert(path, blob);
        }
    }
    Ok(out)
}

/// The files a round applies to the table of `decl`, in the state `to`.
fn list(
    _cx: &Blocking,
    repos: &Repos,
    decl: &Declaration,
    to: State,
    scope: Scope,
    known: Option<&BTreeMap<String, String>>,
) -> Result<Vec<Item>, Error> {
    let Some(snap) = repos.snapshot_at(decl.repo(), to.oid(decl.repo()))? else {
        return Err(gone_state());
    };
    let journal = decl.kind() == CollectionKind::Journal;
    let old_of = |path: &str| {
        known
            .and_then(|known| known.get(path))
            .and_then(|blob| Oid::from_str(blob).ok())
    };
    let mut items = Vec::new();
    match scope {
        Scope::Whole => {
            for (path, blob) in tree_files(&snap, decl)? {
                items.push(if journal {
                    Item::Segment {
                        path,
                        blob,
                        old: None,
                    }
                } else {
                    let Some(id) = document_id(decl, &path) else {
                        continue;
                    };
                    Item::Document { id, path, blob }
                });
            }
        }
        Scope::Deltas(deltas) => {
            for delta in deltas {
                let path = delta.path;
                if journal {
                    if !is_segment(decl, &path) {
                        continue;
                    }
                    match blob_at(&snap, &path)? {
                        Some(blob) if !delta.deleted => items.push(Item::Segment {
                            old: old_of(&path),
                            path,
                            blob,
                        }),
                        _ => items.push(Item::SegmentGone { path }),
                    }
                } else {
                    let Some(id) = document_id(decl, &path) else {
                        continue;
                    };
                    match blob_at(&snap, &path)? {
                        Some(blob) if !delta.deleted => {
                            items.push(Item::Document { id, path, blob });
                        }
                        _ => items.push(Item::DocumentGone { id }),
                    }
                }
            }
        }
        Scope::Compare => {
            let tree = tree_files(&snap, decl)?;
            let known = known.cloned().unwrap_or_default();
            for (path, blob) in &tree {
                if known.get(path).is_some_and(|old| *old == blob.to_string()) {
                    continue;
                }
                items.push(if journal {
                    Item::Segment {
                        path: path.clone(),
                        blob: *blob,
                        old: old_of(path),
                    }
                } else {
                    let Some(id) = document_id(decl, path) else {
                        continue;
                    };
                    Item::Document {
                        id,
                        path: path.clone(),
                        blob: *blob,
                    }
                });
            }
            for path in known.keys().filter(|path| !tree.contains_key(*path)) {
                items.push(if journal {
                    Item::SegmentGone { path: path.clone() }
                } else {
                    let Some(id) = document_id(decl, path) else {
                        continue;
                    };
                    Item::DocumentGone { id }
                });
            }
        }
    }
    Ok(items)
}

/// Reads the files of `items`.
fn read(
    _cx: &Blocking,
    repos: &Repos,
    decl: &Declaration,
    items: Vec<Item>,
) -> Result<Vec<Read>, Error> {
    let snap = repos.snapshot(decl.repo())?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(match item {
            Item::Document { id, path, blob } => Read::Document {
                bytes: snap
                    .blob(blob)
                    .map_err(|fault| fault.with_path(path.clone()))?,
                id,
                path,
                blob,
            },
            Item::DocumentGone { id } => Read::DocumentGone { id },
            Item::SegmentGone { path } => Read::SegmentGone { path },
            Item::Segment { path, blob, old } => {
                let bytes = snap
                    .blob(blob)
                    .map_err(|fault| fault.with_path(path.clone()))?;
                // The old bytes, when the object database still holds them.
                let old = old.and_then(|old| snap.blob(old).ok());
                let start = match &old {
                    Some(old) if bytes.starts_with(old) => journal::split_lines(old).0.len(),
                    _ => 0,
                };
                let segment = std::path::Path::new(&path);
                let mut lines = Vec::new();
                let mut bad = 0;
                for (index, line) in journal::split_lines(&bytes)
                    .0
                    .into_iter()
                    .enumerate()
                    .skip(start)
                {
                    match journal::parse_line(line) {
                        Ok(line) => {
                            lines.push((line.id, journal_order(segment, index), line.payload));
                        }
                        Err(_) => bad += 1,
                    }
                }
                Read::Lines {
                    whole: start == 0,
                    path,
                    blob,
                    lines,
                    bad,
                }
            }
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::num::NonZeroUsize;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use buffa_types::google::protobuf::Empty;
    use lance::Dataset;
    use lance::deps::arrow_array::{Array, Int64Array, StringArray};
    use protocontract::Failure;
    use scheda_api::v1::document_operation::Operation;
    use scheda_api::v1::embedder_ref::EmbedderRef;
    use scheda_api::v1::projector_ref::ProjectorRef;
    use scheda_api::v1::{
        CollectionKey, ColumnKind, DocumentCreation, DocumentOperation, EmbedderErrorCode,
        IndexColumn, IndexDoc, IndexSchema, IndexValue, OpenOptions, ProjectorDescriptor,
        ProjectorSync, RawDocument,
    };
    use scheda_blocking::blocking;

    use crate::error::Error;
    use crate::generation::Layout;
    use crate::index::Index;
    use crate::ports::{FixedEmbedder, Ports};
    use crate::testing::{
        batch, create, create_collection, open_with, upsert, upsert_of, wait_indexed,
    };
    use crate::vault::Vault;
    use scheda_api::v1::index_value::Value;

    /// A scratch directory removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            crate::git::isolate_test_git_env();
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-tables-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A Projector of `title|n|body` Documents: `title` (fts), `n`
    /// (scalar), `body` (vector). It counts its calls.
    struct Pipes {
        projected: Arc<AtomicUsize>,
    }

    fn column(name: &str, kind: ColumnKind) -> IndexColumn {
        IndexColumn::default().with_name(name).with_kind(kind)
    }

    impl ProjectorSync for Pipes {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("pipes@1"))
        }

        fn columns(&self, _collection: CollectionKey) -> Result<IndexSchema, Error> {
            Ok(IndexSchema {
                columns: vec![
                    column("title", ColumnKind::COLUMN_KIND_TEXT).with_fts(true),
                    column("n", ColumnKind::COLUMN_KIND_INTEGER).with_scalar_index(true),
                    column("body", ColumnKind::COLUMN_KIND_TEXT).with_vector(true),
                ],
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            self.projected.fetch_add(1, Ordering::SeqCst);
            let text = String::from_utf8(document.raw.unwrap_or_default()).unwrap_or_default();
            let mut parts = text.splitn(3, '|');
            let mut values = std::collections::HashMap::new();
            let mut put = |name: &str, value: Value| {
                values.insert(
                    name.to_owned(),
                    IndexValue {
                        value: Some(value),
                        ..IndexValue::default()
                    },
                );
            };
            if let Some(title) = parts.next() {
                put("title", Value::Text(title.to_owned()));
            }
            if let Some(n) = parts.next().and_then(|n| n.parse().ok()) {
                put("n", Value::Integer(n));
            }
            if let Some(body) = parts.next() {
                put("body", Value::Text(body.to_owned()));
            }
            Ok(IndexDoc {
                values: values.into_iter().collect(),
                ..IndexDoc::default()
            })
        }
    }

    /// The ports of the tests, and the count of `Project` calls and of
    /// embedded texts.
    fn ports() -> (Ports, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let projected = Arc::new(AtomicUsize::new(0));
        let embedded = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&embedded);
        let embedder = FixedEmbedder::new(
            "model-a",
            4,
            NonZeroUsize::new(8).unwrap(),
            move |texts: Vec<String>| {
                count.fetch_add(texts.len(), Ordering::SeqCst);
                Ok(texts
                    .iter()
                    .map(|text| vec![text.len() as f32, 1.0, 2.0, 3.0])
                    .collect())
            },
        );
        let ports = Ports {
            projector: Some(ProjectorRef::from_sync(Pipes {
                projected: Arc::clone(&projected),
            })),
            embedder: Some(EmbedderRef::from_sync(embedder)),
            reranker: None,
        };
        (ports, projected, embedded)
    }

    /// The rows of the table of `collection`, at the versions the live
    /// commit names: id → (title, n, has a vector).
    async fn rows(
        vault: &Arc<Vault>,
        collection: &str,
    ) -> BTreeMap<String, (Option<String>, Option<i64>, bool)> {
        let root = Index::new(vault.path()).root().to_path_buf();
        let collection = collection.to_owned();
        let (dir, version) = blocking(move |cx| {
            let generation = Layout::new(&root).current(cx)?.unwrap();
            let commit = generation.read_commit(cx)?.unwrap();
            let table = commit.tables.get(&collection).unwrap();
            Ok((
                generation.dir().join(format!("{}.lance", table.name)),
                table.versions.rows,
            ))
        })
        .await
        .unwrap();
        let dataset = Dataset::open(dir.to_str().unwrap())
            .await
            .unwrap()
            .checkout_version(version)
            .await
            .unwrap();
        let batch = dataset.scan().try_into_batch().await.unwrap();
        let ids = batch
            .column_by_name("id")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .clone();
        let titles = batch
            .column_by_name("title")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .clone();
        let ns = batch
            .column_by_name("n")
            .unwrap()
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .clone();
        let vectors = batch.column_by_name("_vector_body").unwrap().clone();
        (0..batch.num_rows())
            .map(|n| {
                (
                    ids.value(n).to_owned(),
                    (
                        (!titles.is_null(n)).then(|| titles.value(n).to_owned()),
                        (!ns.is_null(n)).then(|| ns.value(n)),
                        !vectors.is_null(n),
                    ),
                )
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn documents_and_lines_become_rows_that_follow_their_files() {
        let scratch = Scratch::new("follow");
        let (ports, projected, embedded) = ports();
        let vault = open_with(
            &scratch.0,
            OpenOptions::default().with_index_poll_ms(10),
            ports,
        )
        .await
        .unwrap();
        create_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        create_collection(
            &vault,
            serde_json::json!({"name": "log", "schema": {"journal": {}}}),
        )
        .await
        .unwrap();
        upsert(&vault, "notes", upsert_of("a.md", b"Alpha|1|first body"))
            .await
            .unwrap();
        upsert(&vault, "notes", upsert_of("b.md", b"Beta|2|second body"))
            .await
            .unwrap();
        for id in ["l1", "l2"] {
            create(
                &vault,
                "log",
                DocumentCreation::default()
                    .with_id(id)
                    .with_raw(format!("Line {id}|7|said {id}").into_bytes())
                    .with_note("test"),
            )
            .await
            .unwrap();
        }
        wait_indexed(&vault).await;
        assert_eq!(
            projected.load(Ordering::SeqCst),
            4,
            "one Project per changed Document"
        );
        let notes = rows(&vault, "notes").await;
        assert_eq!(
            notes.get("a.md"),
            Some(&(Some(String::from("Alpha")), Some(1), true))
        );
        assert_eq!(
            notes.get("b.md"),
            Some(&(Some(String::from("Beta")), Some(2), true))
        );
        assert_eq!(rows(&vault, "log").await.len(), 2);

        // A change replaces the row, a deletion removes it, an unchanged
        // text keeps its vector.
        let before = embedded.load(Ordering::SeqCst);
        upsert(
            &vault,
            "notes",
            upsert_of("a.md", b"Alpha two|3|first body"),
        )
        .await
        .unwrap();
        crate::testing::delete(
            &vault,
            "notes",
            scheda_api::v1::DocumentDeletion::default()
                .with_id("b.md")
                .with_note("test"),
        )
        .await
        .unwrap();
        wait_indexed(&vault).await;
        let notes = rows(&vault, "notes").await;
        assert_eq!(notes.len(), 1);
        assert_eq!(
            notes.get("a.md"),
            Some(&(Some(String::from("Alpha two")), Some(3), true))
        );
        assert_eq!(
            embedded.load(Ordering::SeqCst),
            before,
            "the body did not change: no text embedded"
        );

        // A segment of 200 lines and one more appended: the one line is
        // projected.
        let lines: Vec<DocumentOperation> = (0..200)
            .map(|n| DocumentOperation {
                operation: Some(Operation::Create(Box::new(
                    DocumentCreation::default()
                        .with_id(format!("m{n:03}"))
                        .with_raw(format!("Line {n}|{n}|said {n}").into_bytes())
                        .with_note("test"),
                ))),
                ..DocumentOperation::default()
            })
            .collect();
        batch(&vault, "log", lines).await.unwrap();
        wait_indexed(&vault).await;
        let before = projected.load(Ordering::SeqCst);
        create(
            &vault,
            "log",
            DocumentCreation::default()
                .with_id("l3")
                .with_raw(b"Line l3|8|said l3".to_vec())
                .with_note("test"),
        )
        .await
        .unwrap();
        wait_indexed(&vault).await;
        assert_eq!(rows(&vault, "log").await.len(), 203);
        assert_eq!(projected.load(Ordering::SeqCst), before + 1);
        vault.close().await.unwrap();
    }

    /// A collection declared after a round looked at `HEAD` and before it
    /// read `HEAD` again for its work is in the Index the round makes: the
    /// plan is of the collections where the round brings the Index.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_collection_declared_while_a_round_runs_gets_its_table() {
        let scratch = Scratch::new("declared-meanwhile");
        let (ports, _, _) = ports();
        let vault = open_with(
            &scratch.0,
            OpenOptions::default().with_index_poll_ms(10),
            ports,
        )
        .await
        .unwrap();
        create_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        wait_indexed(&vault).await;
        let path = vault.path().to_path_buf();
        crate::indexer::hooks::hold(&path);
        upsert(&vault, "notes", upsert_of("a.md", b"Alpha|1|first body"))
            .await
            .unwrap();
        wait_until("the held round", || crate::indexer::hooks::waiting(&path)).await;
        create_collection(
            &vault,
            serde_json::json!({"name": "log", "schema": {"journal": {}}}),
        )
        .await
        .unwrap();
        create(
            &vault,
            "log",
            DocumentCreation::default()
                .with_id("l1")
                .with_raw(b"Line|7|said".to_vec())
                .with_note("test"),
        )
        .await
        .unwrap();
        crate::indexer::hooks::release(&path);
        wait_indexed(&vault).await;
        assert_eq!(rows(&vault, "notes").await.len(), 1);
        assert_eq!(rows(&vault, "log").await.len(), 1);
        vault.close().await.unwrap();
    }

    /// The bytes of the `commit` of the live generation.
    async fn live_commit(vault: &Arc<Vault>) -> Vec<u8> {
        let root = Index::new(vault.path()).root().to_path_buf();
        blocking(move |cx| {
            let generation = Layout::new(&root).current(cx)?.unwrap();
            Ok(std::fs::read(generation.dir().join("commit")).unwrap())
        })
        .await
        .unwrap()
    }

    /// Waits up to a minute for `done`.
    async fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// `count` Documents of `notes` in one batch, each with a body of its
    /// own.
    fn notes(count: usize, from: usize) -> Vec<DocumentOperation> {
        (from..from + count)
            .map(|n| DocumentOperation {
                operation: Some(Operation::Upsert(Box::new(upsert_of(
                    &format!("n{n:03}.md"),
                    format!("Note {n}|{n}|body number {n}").as_bytes(),
                )))),
                ..DocumentOperation::default()
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_embedder_that_fails_moves_no_commit_fails_no_write_and_is_asked_only_for_the_rest()
    {
        let scratch = Scratch::new("failing-embedder");
        let broken = Arc::new(AtomicBool::new(false));
        let embedded: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let embedder = {
            let broken = Arc::clone(&broken);
            let embedded = Arc::clone(&embedded);
            FixedEmbedder::new(
                "model-a",
                4,
                NonZeroUsize::MIN,
                move |texts: Vec<String>| {
                    let mut done = embedded.lock().unwrap();
                    if broken.load(Ordering::SeqCst) && done.len() >= 5 {
                        return Err(Failure::new(EmbedderErrorCode::Unavailable, "down")
                            .with_retryable(true)
                            .into());
                    }
                    for text in &texts {
                        *done.entry(text.clone()).or_default() += 1;
                    }
                    Ok(texts
                        .iter()
                        .map(|text| vec![text.len() as f32, 1.0, 2.0, 3.0])
                        .collect())
                },
            )
        };
        let ports = Ports {
            projector: Some(ProjectorRef::from_sync(Pipes {
                projected: Arc::default(),
            })),
            embedder: Some(EmbedderRef::from_sync(embedder)),
            reranker: None,
        };
        let vault = open_with(
            &scratch.0,
            OpenOptions::default().with_index_poll_ms(10),
            ports,
        )
        .await
        .unwrap();
        create_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        wait_indexed(&vault).await;
        let live = live_commit(&vault).await;

        broken.store(true, Ordering::SeqCst);
        batch(&vault, "notes", notes(10, 0)).await.unwrap();
        wait_until("the failure", || {
            vault.indexer().status().last_error.is_set() && embedded.lock().unwrap().len() == 5
        })
        .await;
        assert_eq!(live_commit(&vault).await, live, "no commit moved");
        batch(&vault, "notes", notes(1, 10)).await.unwrap();

        broken.store(false, Ordering::SeqCst);
        wait_indexed(&vault).await;
        let done = embedded.lock().unwrap().clone();
        assert_eq!(done.len(), 11);
        assert!(
            done.values().all(|times| *times == 1),
            "a text embedded before the failure is not sent again: {done:?}"
        );
        let rows = rows(&vault, "notes").await;
        assert_eq!(rows.len(), 11);
        assert!(rows.values().all(|(_, _, vector)| *vector));
        vault.close().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn closing_a_vault_in_the_middle_of_a_slow_embedding_returns_within_one_portion() {
        const PORTION: Duration = Duration::from_millis(200);
        let scratch = Scratch::new("slow-embedder");
        let asked = Arc::new(AtomicUsize::new(0));
        let embedder = {
            let asked = Arc::clone(&asked);
            FixedEmbedder::new(
                "model-a",
                4,
                NonZeroUsize::MIN,
                move |texts: Vec<String>| {
                    asked.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(PORTION);
                    Ok(texts.iter().map(|_| vec![1.0, 1.0, 2.0, 3.0]).collect())
                },
            )
        };
        let ports = Ports {
            projector: Some(ProjectorRef::from_sync(Pipes {
                projected: Arc::default(),
            })),
            embedder: Some(EmbedderRef::from_sync(embedder)),
            reranker: None,
        };
        let vault = open_with(
            &scratch.0,
            OpenOptions::default().with_index_poll_ms(10),
            ports,
        )
        .await
        .unwrap();
        create_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        wait_indexed(&vault).await;
        batch(&vault, "notes", notes(40, 0)).await.unwrap();
        wait_until("the embedding", || asked.load(Ordering::SeqCst) >= 2).await;
        let started = Instant::now();
        vault.close().await.unwrap();
        let took = started.elapsed();
        // The whole round would take 40 portions.
        assert!(took < PORTION * 4, "close took {took:?}");
        assert!(asked.load(Ordering::SeqCst) < 10);
    }
}
