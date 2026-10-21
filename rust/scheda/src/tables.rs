//! The Lance side of a round of the indexer: what each collection's table
//! of a generation is made of, and how a round brings it to a new state.
//!
//! **What is indexed.** Every collection that is not `encrypted`: each
//! Document of a `standard` or `singleton` collection, and each line of a
//! journal's segments, read from the commit of the state the round moves
//! to (isolated collections from the nested repository). Its identity
//! always; the values of its columns and the vectors of its `vector`
//! columns as its `IndexScope` says: both for `INDEX_SCOPE_ALL`, the values
//! only for `INDEX_SCOPE_NO_VECTORS`, neither for `INDEX_SCOPE_IDS`.
//!
//! **The Projector** gives the values, once per changed Document per
//! round; the Document's bytes are moved into the call. Its answers are
//! kept in memory until the round succeeds, by Document and the digest of
//! its bytes, so a round tried again after a failure asks it only for the
//! Documents it has not answered. Without a
//! Projector, for a collection without columns, and for a Document it gives
//! no values for, only the identity is stored. A Document the Projector
//! refuses, or gives a value of another kind than its column's, is left
//! out of the Index; its file stays. A retryable failure of the Projector
//! fails the round instead, which is tried again.
//!
//! **The embedder** gets the texts of the `vector` columns (the text of a
//! `text` column, each text of a `text list` one) whose digest (the git
//! blob id of the text) the table holds for none of that Document's texts,
//! so a text that did not change keeps its vector wherever it moved in its
//! list. A rebuild with the model and vector length of the live generation
//! takes, likewise, the vector the live generation holds for that
//! Document's text. The vectors it gives are kept in memory until the round
//! succeeds, so a round tried again after a failure sends only the texts
//! still without one. A text it refuses keeps no vector, and only that
//! text: a refusal of a portion is split until the text that causes it is
//! found. A blank text (only whitespace) has no vector.
//!
//! **A declaration's `index_scope`** decides what a table holds, not
//! whether the Index is fresh: the Index records what the Projector
//! declared for a collection beside the shape of its table, [`stale`]
//! compares the first, and a round rebuilds a table whose shape is not the
//! one the declaration gives now.
//!
//! **A journal segment** that grew by appending (its old bytes are a
//! prefix of the new ones) gives only its new lines; one changed otherwise
//! is indexed again whole.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use git2::{ObjectType, Oid};
use scheda_api::v1::{
    EmbedderErrorCode, IndexDoc, IndexSchema, VaultErrorCode, index_value::Value,
};
use scheda_blocking::{Blocking, blocking};

use crate::collection::{Declaration, Declarations, Kind as CollectionKind};
use crate::documents::WalkState;
use crate::error::{Context, Error, Fault};
use crate::generation::{self, Commit, Declared, Embedder, Generation, Replaced, Retired};
use crate::id::require_document_id;
use crate::index::{Counts, State};
use crate::ports::{EmbedderPort, Installed, ProjectorPort};
use crate::repo::{NodeKind, Repo, Repos, Snap};
use crate::store::{Column, Embedding, Embeddings, Kind, Row, Shape, Table, Vectors, texts_name};
use crate::{isolated, journal};

/// How many files a round reads, projects, and writes at a time.
pub(crate) const BATCH: usize = 256;

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
/// indexes (the declaration's `index_scope` applied) beside what the Projector
/// declared for it.
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub(crate) ports: generation::Ports,
    pub(crate) shapes: BTreeMap<String, Shape>,
    /// What the Projector declared for each of those collections, before
    /// the scope: what [`stale`] compares.
    pub(crate) declared: BTreeMap<String, Declared>,
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

/// The plan of a Vault with `calls`, for `declarations` read from the commit
/// `head` of the main repository: the Projector is asked for the columns of
/// each collection, once per collection and `head` (it keeps its answers).
///
/// # Errors
///
/// The failure of the Projector.
pub(crate) async fn plan(
    calls: &Calls,
    head: Option<Oid>,
    declarations: Arc<Declarations>,
) -> Result<Plan, Error> {
    let mut shapes = BTreeMap::new();
    let mut declared = BTreeMap::new();
    let mut needs_embedder: Option<String> = None;
    for decl in declarations.iter().filter(|decl| indexed(decl)) {
        let schema: Option<IndexSchema> = match &calls.projector {
            Some(projector) => projector
                .columns(head, decl.name().to_owned())
                .await
                .with_context(|| format!("the columns of {}", decl.name()))?,
            None => None,
        };
        let columns: Vec<Column> = schema
            .map(|schema| schema.columns.iter().filter_map(Column::of).collect())
            .unwrap_or_default();
        let vectors: Vec<String> = if decl.holds_vectors() {
            columns
                .iter()
                .filter(|column| column.vector)
                .map(|column| column.name.clone())
                .collect()
        } else {
            Vec::new()
        };
        if !vectors.is_empty() && needs_embedder.is_none() {
            needs_embedder = Some(decl.name().to_owned());
        }
        declared.insert(
            decl.name().to_owned(),
            Declared {
                vectors: columns
                    .iter()
                    .filter(|column| column.vector)
                    .map(|column| column.name.clone())
                    .collect(),
                columns: columns.clone(),
            },
        );
        shapes.insert(
            decl.name().to_owned(),
            Shape {
                columns: if decl.holds_columns() {
                    columns
                } else {
                    Vec::new()
                },
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
        declared,
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
    head: Option<Oid>,
    declarations: Arc<Declarations>,
    live: Option<&Commit>,
) -> Result<(), Error> {
    if let Some(what) = plan(calls, head, declarations).await?.lacking {
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
/// `plan`; `None` when it is not. A collection's `index_scope` never makes it
/// stale.
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
    // What the Projector declared, not what the table holds: the
    // `index_scope` of the collection's declaration changes the table, and
    // the round rebuilds it, but it leaves the Index fresh. The vector
    // length is not compared per collection: the Index's embedder, compared
    // above, fixes it, and a collection that is not `INDEX_SCOPE_ALL` holds
    // none, so a Vault with an embedder and one without agree on it.
    for (collection, table) in &commit.tables {
        if let Some(declared) = plan.declared.get(collection)
            && *declared != table.declared
        {
            return Some(format!(
                "the Index holds other columns of the collection {collection} than this Vault's Projector gives"
            ));
        }
    }
    None
}

/// What a Vault keeps between two tries of a round, so a try after a
/// failure asks the ports only for what the earlier tries did not get:
/// the vectors the embedder gave, by the digest of their text, and the
/// texts it refused; the Projector's answers.
#[derive(Default)]
pub(crate) struct Cache {
    model: String,
    vectors: HashMap<String, Vec<f32>>,
    refused: HashSet<String>,
    projection: String,
    answers: HashMap<Asked, Answer>,
}

/// One `Project` call: the collection, the Document's id, and the digest
/// of the bytes projected.
#[derive(Eq, Hash, PartialEq)]
struct Asked {
    collection: String,
    id: String,
    digest: String,
}

/// What the Projector answered.
enum Answer {
    /// The values; none when it gave none (the Document is indexed by its
    /// identity).
    Values(Option<IndexDoc>),
    /// It refused the Document, or gave a reply that breaks its rules.
    Refused,
}

impl Cache {
    /// Forgets the vectors when they are of another model than `model`.
    fn keep_vectors_of(&mut self, model: &str) {
        if self.model != model {
            model.clone_into(&mut self.model);
            self.vectors.clear();
            self.refused.clear();
        }
    }

    /// Forgets the answers when they are of another projection than
    /// `projection`.
    fn keep_answers_of(&mut self, projection: &str) {
        if self.projection != projection {
            projection.clone_into(&mut self.projection);
            self.answers.clear();
        }
    }

    /// Forgets everything: the round that needed it succeeded.
    pub(crate) fn clear(&mut self) {
        self.vectors.clear();
        self.refused.clear();
        self.answers.clear();
    }
}

/// The live generation a rebuild replaces, when its vectors were made by
/// the model and vector length the rebuild embeds with: a text whose
/// digest it holds for the same Document is not embedded again.
pub(crate) struct Earlier {
    /// Its folder.
    pub(crate) dir: PathBuf,
    /// Its `commit`.
    pub(crate) commit: Commit,
}

/// What a round of the Lance side works with.
pub(crate) struct Work<'a> {
    pub(crate) repos: Arc<Repos>,
    pub(crate) generation: Generation,
    pub(crate) commit: Commit,
    /// The generation a rebuild replaces, as a source of vectors.
    pub(crate) earlier: Option<Earlier>,
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

/// One Document to project: its identity, its bytes, and their digest.
struct Source {
    id: String,
    file: String,
    order: String,
    raw: Vec<u8>,
    digest: String,
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
            self.cache.keep_vectors_of(embedder.model_id());
        }
        if let Some(projector) = &self.calls.projector {
            self.cache.keep_answers_of(projector.projection_id());
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
                self.commit.dropped.push(Retired::now(table.name));
            }
        }
        for decl in declarations.iter().filter(|decl| indexed(decl)) {
            let name = decl.name();
            let (Some(shape), Some(declared)) = (
                self.plan.shapes.get(name).cloned(),
                self.plan.declared.get(name).cloned(),
            ) else {
                unreachable!("a plan has the shape of every collection it indexes");
            };
            let known = self.commit.tables.get(name).cloned();
            let scope = match (&known, &deltas) {
                (None, _) | (_, None) => Scope::Whole,
                // A table of another shape (a declaration flag moved, or
                // the vector length) gains or loses its columns: whole.
                (Some(known), _) if known.shape != shape => Scope::Whole,
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
                (Some(table), _) => (table.name.clone(), Some(table.versions.clone())),
                (None, _) => (self.commit.new_table(), None),
            };
            self.keep_texts_of(known.as_ref(), &table_name, &shape);
            let mut table = Table::open(
                self.generation.dir(),
                &table_name,
                shape.clone(),
                committed.as_ref(),
            )
            .await
            .with_context(|| format!("open the table of {name}"))?;
            let earlier = self.earlier_vectors(name).await;
            self.apply(&mut table, earlier.as_ref(), decl, to, scope)
                .await?;
            // What a reader of the live `commit`, or of one it replaced
            // less than an hour ago, may still open.
            let keep = known
                .as_ref()
                .map(|known| known.keep_from(generation::now_millis()));
            table
                .maintain(self.commit.rounds + 1, keep.as_ref())
                .await
                .with_context(|| format!("maintain the table of {name}"))?;
            let versions = table.versions();
            let mut replaced = Vec::new();
            if let Some(known) = known {
                replaced = known.replaced;
                if known.versions != versions {
                    // Named by the live `commit` until the next one: the
                    // round after this one stamps the moment.
                    replaced.push(Replaced {
                        versions: known.versions,
                        since: None,
                    });
                }
            }
            self.commit.tables.insert(
                name.to_owned(),
                generation::Table {
                    name: table_name,
                    shape,
                    declared,
                    versions,
                    replaced,
                },
            );
        }
        self.commit.state = to.line();
        self.commit.rounds += 1;
        Ok(())
    }

    /// Brings the list of dropped texts datasets in step with the table
    /// `name` of `shape`, which was `known`: the texts dataset of a `text
    /// list` vector column the table no longer has is dropped (a reader of
    /// an older `commit` may still read it for an hour), and one it has is
    /// not, though an earlier shape of the table dropped it.
    fn keep_texts_of(&mut self, known: Option<&generation::Table>, name: &str, shape: &Shape) {
        let kept: HashSet<String> = shape
            .list_vectors()
            .map(|column| texts_name(name, column))
            .collect();
        self.commit
            .dropped
            .retain(|dropped| !kept.contains(&dropped.name));
        let Some(known) = known else {
            return;
        };
        for column in known.versions.texts.keys() {
            let texts = texts_name(name, column);
            if !kept.contains(&texts) {
                self.commit.dropped.push(Retired::now(texts));
            }
        }
    }

    /// The vectors the earlier generation holds for `collection`. `None`
    /// when it holds none, or cannot be read: a rebuild is also how a
    /// broken Index is repaired, so its texts then go to the embedder.
    async fn earlier_vectors(&self, collection: &str) -> Option<Vectors> {
        let earlier = self.earlier.as_ref()?;
        let table = earlier.commit.tables.get(collection)?;
        if table.shape.vectors.is_empty() {
            return None;
        }
        Vectors::open(
            &earlier.dir,
            &table.name,
            table.shape.clone(),
            &table.versions,
        )
        .await
        .ok()
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

    /// Brings `table`, the table of `decl`, to the state `to`; `earlier`
    /// holds the vectors of the generation a rebuild replaces.
    async fn apply(
        &mut self,
        table: &mut Table,
        earlier: Option<&Vectors>,
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
            self.write(table, earlier, decl, reads).await?;
        }
        Ok(())
    }

    /// Writes what `reads` holds into `table`.
    async fn write(
        &mut self,
        table: &mut Table,
        earlier: Option<&Vectors>,
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
                    let blob = blob.to_string();
                    files.push((path.clone(), blob.clone()));
                    sources.push(Source {
                        order: id.clone(),
                        id,
                        file: path,
                        raw: bytes,
                        digest: blob,
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
                            digest: digest_of(&payload),
                            raw: payload,
                        });
                    }
                }
                Read::SegmentGone { path } => gone_files.push(path),
            }
        }
        let (rows, left_out) = self.rows(table, earlier, decl, sources).await?;
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
        earlier: Option<&Vectors>,
        decl: &Declaration,
        sources: Vec<Source>,
    ) -> Result<(Vec<Row>, Vec<String>), Error> {
        let shape = table.shape().clone();
        let projects = !shape.columns.is_empty() || !shape.vectors.is_empty();
        let mut rows = Vec::with_capacity(sources.len());
        let mut left_out = Vec::new();
        let mut texts: Vec<(usize, Wanted)> = Vec::new();
        for source in sources {
            let mut row = Row {
                id: source.id,
                file: source.file,
                order: source.order,
                ..Row::default()
            };
            if projects && let Some(projector) = &self.calls.projector {
                let asked = Asked {
                    collection: decl.name().to_owned(),
                    id: row.id.clone(),
                    digest: source.digest,
                };
                let answer = match self.cache.answers.entry(asked) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => {
                        let answer = match projector
                            .project(decl.name().to_owned(), row.id.clone(), source.raw)
                            .await
                        {
                            Ok(values) => Answer::Values(values),
                            Err(error) if error.retryable() => {
                                return Err(error.context(format!("project {}", row.id)));
                            }
                            Err(_) => Answer::Refused,
                        };
                        entry.insert(answer)
                    }
                };
                let projected = match answer {
                    Answer::Values(values) => values_of(&shape, values.as_ref()),
                    // Refused, or a reply that breaks its rules: left out.
                    Answer::Refused => None,
                };
                match projected {
                    Some((values, vector_texts)) => {
                        row.values = values;
                        for wanted in vector_texts {
                            texts.push((rows.len(), wanted));
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
            self.embed(table, earlier, &mut rows, texts).await?;
        }
        Ok((rows, left_out))
    }

    /// Gives `rows` the embeddings of `texts` (a row, and a text of one
    /// of its `vector` columns): the one the table, or else `earlier`,
    /// holds for the same text of the same Document, else the one the
    /// cache holds, else a new one from the embedder.
    async fn embed(
        &mut self,
        table: &Table,
        earlier: Option<&Vectors>,
        rows: &mut [Row],
        texts: Vec<(usize, Wanted)>,
    ) -> Result<(), Error> {
        let texts: Vec<Text> = texts
            .into_iter()
            .map(|(row, wanted)| Text {
                id: rows[row].id.clone(),
                row,
                column: wanted.column,
                position: wanted.position,
                digest: digest_of(&wanted.text),
                text: wanted.text,
            })
            .collect();
        let all: Vec<&Text> = texts.iter().collect();
        let stored = table.embeddings(&ids_of(&all)).await?;
        self.keep(&stored, &all);
        if let Some(earlier) = earlier {
            let missing: Vec<&Text> = texts
                .iter()
                .filter(|text| {
                    !self.cache.vectors.contains_key(&text.digest)
                        && !self.cache.refused.contains(&text.digest)
                })
                .collect();
            if !missing.is_empty() {
                // Read only as a source of vectors already made: one that
                // cannot be read leaves its texts to the embedder.
                if let Ok(stored) = earlier.embeddings(&ids_of(&missing)).await {
                    self.keep(&stored, &missing);
                }
            }
        }
        let mut wanted: Vec<(String, String)> = Vec::new();
        for text in texts {
            if self.cache.refused.contains(&text.digest) {
                continue;
            }
            if !self.cache.vectors.contains_key(&text.digest) {
                wanted.push((text.digest.clone(), text.text));
            }
            rows[text.row]
                .embeddings
                .entry(text.column)
                .or_default()
                .push(Embedding {
                    position: text.position,
                    vector: Vec::new(),
                    digest: text.digest,
                });
        }
        self.embed_wanted(wanted).await?;
        for row in rows.iter_mut() {
            for embeddings in row.embeddings.values_mut() {
                embeddings.retain_mut(|embedding| {
                    match self.cache.vectors.get(&embedding.digest) {
                        Some(vector) => {
                            embedding.vector.clone_from(vector);
                            true
                        }
                        None => false,
                    }
                });
            }
            row.embeddings
                .retain(|_, embeddings| !embeddings.is_empty());
        }
        Ok(())
    }

    /// Puts in the cache the vectors `stored` holds for `texts`: the one of
    /// the same Document whose text has the same digest, of any of its
    /// `vector` columns and places.
    fn keep(&mut self, stored: &Embeddings, texts: &[&Text]) {
        for text in texts {
            let kept = stored.get(&text.id).and_then(|embeddings| {
                embeddings
                    .iter()
                    .find(|embedding| embedding.digest == text.digest)
            });
            if let Some(embedding) = kept {
                self.cache
                    .vectors
                    .insert(text.digest.clone(), embedding.vector.clone());
            }
        }
    }

    /// Asks the embedder for the vectors of `wanted` (digest, text) the
    /// cache lacks, once per distinct text, and keeps them in the cache.
    ///
    /// A refusal is charged to the text alone: an embedder that batches
    /// refuses a whole portion for one text in it, and the vectors it
    /// returned before the refusal are kept. The first text left without a
    /// vector is sent alone; refused alone, it is remembered and keeps no
    /// vector. Embedded alone, it is innocent, and the culprit is looked
    /// for among the texts after it by halving, so a portion of `n` texts
    /// costs about `log n` more calls per culprit, not one call per text.
    /// The texts after the culprit go on as one call. Any other failure
    /// ends the call.
    async fn embed_wanted(&mut self, wanted: Vec<(String, String)>) -> Result<(), Error> {
        let Some(embedder) = self.calls.embedder.clone() else {
            return Err(Fault::new(
                VaultErrorCode::FailedPrecondition,
                "the Vault has no embedder for its vector columns",
            )
            .into());
        };
        let mut seen = HashSet::new();
        let pending: Vec<(String, String)> = wanted
            .into_iter()
            .filter(|(digest, _)| seen.insert(digest.clone()))
            .collect();
        let mut start = 0;
        while start < pending.len() {
            let Some(done) = self.send(&embedder, &pending[start..]).await? else {
                break;
            };
            start = self.confirm(&embedder, &pending, start + done).await?;
        }
        Ok(())
    }

    /// Finds the text that caused a refusal. `pending[first]` is the first
    /// text without a vector after a refused call. Returns the index of the
    /// text after the culprit found, from which the texts go on untried.
    async fn confirm(
        &mut self,
        embedder: &Arc<EmbedderPort>,
        pending: &[(String, String)],
        first: usize,
    ) -> Result<usize, Error> {
        if self
            .send(embedder, &pending[first..=first])
            .await?
            .is_some()
        {
            self.cache.refused.insert(pending[first].0.clone());
            return Ok(first + 1);
        }
        // The suspects are `pending[lo..hi]`; each step moves `lo` up or
        // `hi` down, so the loop ends.
        let mut lo = first + 1;
        let mut hi = pending.len();
        while lo < hi {
            if hi - lo == 1 {
                if self.send(embedder, &pending[lo..hi]).await?.is_some() {
                    self.cache.refused.insert(pending[lo].0.clone());
                }
                return Ok(hi);
            }
            let mid = lo + (hi - lo) / 2;
            match self.send(embedder, &pending[lo..mid]).await? {
                None => lo = mid,
                Some(done) => {
                    lo += done;
                    hi = mid;
                }
            }
        }
        Ok(lo)
    }

    /// One call to the embedder with `texts`: keeps the vectors it gives.
    /// `None` when it embedded them all, `Some(n)` when it refused after
    /// `n` vectors.
    ///
    /// # Errors
    ///
    /// Any failure of the embedder but a refusal, a stop included.
    async fn send(
        &mut self,
        embedder: &Arc<EmbedderPort>,
        texts: &[(String, String)],
    ) -> Result<Option<usize>, Error> {
        let batch: Vec<String> = texts.iter().map(|(_, text)| text.clone()).collect();
        let stopped = Arc::clone(&self.stopped);
        let embedded = embedder.embed(batch, move || stopped()).await;
        let done = embedded.vectors.len();
        for ((digest, _), vector) in texts.iter().zip(embedded.vectors) {
            self.cache.vectors.insert(digest.clone(), vector);
        }
        match embedded.failure {
            None => Ok(None),
            Some(error) if error.is(EmbedderErrorCode::Refused) && done < texts.len() => {
                Ok(Some(done))
            }
            Some(error) => Err(error),
        }
    }
}

/// A text of a `vector` column to embed.
struct Text {
    /// The Document's id.
    id: String,
    /// Its row among those a batch writes.
    row: usize,
    column: String,
    /// Its place in the column's `text list`; 0 in a `text` column.
    position: u32,
    digest: String,
    text: String,
}

/// A text of a `vector` column a Document gives: the column, the place of
/// the text in it, and the text.
struct Wanted {
    column: String,
    /// The place in the column's `text list`; 0 in a `text` column.
    position: u32,
    text: String,
}

/// The distinct Document ids of `texts`.
fn ids_of(texts: &[&Text]) -> Vec<String> {
    texts
        .iter()
        .map(|text| text.id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect()
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

/// The digest of a text or of bytes: their git blob id.
fn digest_of(bytes: impl AsRef<[u8]>) -> String {
    Oid::hash_object(ObjectType::Blob, bytes.as_ref())
        .map(|oid| oid.to_string())
        .unwrap_or_default()
}

/// What a Document gives a table: the value of each column, by name, and
/// the texts of its `vector` columns to embed.
type Projected = (BTreeMap<String, Value>, Vec<Wanted>);

/// The values of `values` for the columns of `shape`, and the texts of its
/// `vector` columns that are not blank, each with its place in its column
/// (0 in a `text` column); `None` when a value is of another kind than its
/// column's, which leaves the Document out. A key no column declares is
/// ignored, and a value that is not finite is left out.
fn values_of(shape: &Shape, values: Option<&IndexDoc>) -> Option<Projected> {
    let mut out = BTreeMap::new();
    let mut texts = Vec::new();
    let Some(values) = values else {
        return Some((out, texts));
    };
    let values = &values.values;
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
        let is_list = shape.is_list(column);
        let found: Vec<&String> = match values.get(column).and_then(|value| value.value.as_ref()) {
            Some(Value::Text(text)) if !is_list => vec![text],
            Some(Value::TextList(list)) if is_list => list.texts.iter().collect(),
            None => Vec::new(),
            Some(_) => return None,
        };
        for (position, text) in found.into_iter().enumerate() {
            if text.trim().is_empty() {
                continue;
            }
            // A message of at most 2 GiB holds fewer than 2^32 texts.
            let Ok(position) = u32::try_from(position) else {
                break;
            };
            texts.push(Wanted {
                column: column.clone(),
                position,
                text: text.clone(),
            });
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
                // Taken from the comparison of the trees: a lookup by path
                // reads every folder on the way again, for each file.
                let file = delta.file();
                let path = delta.path;
                if journal {
                    if !is_segment(decl, &path) {
                        continue;
                    }
                    match file? {
                        Some(blob) => items.push(Item::Segment {
                            old: old_of(&path),
                            path,
                            blob,
                        }),
                        None => items.push(Item::SegmentGone { path }),
                    }
                } else {
                    let Some(id) = document_id(decl, &path) else {
                        continue;
                    };
                    match file? {
                        Some(blob) => items.push(Item::Document { id, path, blob }),
                        None => items.push(Item::DocumentGone { id }),
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
    use scheda_api::v1::index_value::TextList;
    use scheda_api::v1::projector_ref::ProjectorRef;
    use scheda_api::v1::rebuild::{Cause, Stage};
    use scheda_api::v1::{
        CollectionKey, ColumnKind, DocumentCreation, DocumentOperation, EmbedderErrorCode,
        IndexColumn, IndexDoc, IndexSchema, IndexState, IndexValue, OpenOptions,
        ProjectorDescriptor, ProjectorSync, RawDocument, VaultErrorCode,
    };
    use scheda_blocking::blocking;

    use crate::error::Error;
    use crate::generation::Layout;
    use crate::index::Index;
    use crate::ports::{FixedEmbedder, Ports};
    use crate::store::texts_name;
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
            crate::testing::remove_when_the_test_ends(&dir);
            Scratch(dir)
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
        ports_of("model-a")
    }

    /// The ports of the tests, with an embedder of `model`.
    fn ports_of(model: &str) -> (Ports, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let projected = Arc::new(AtomicUsize::new(0));
        let embedded = Arc::new(AtomicUsize::new(0));
        (counting(model, &projected, &embedded), projected, embedded)
    }

    /// The ports of the tests, with an embedder of `model`, counting their
    /// `Project` calls into `projected` and the texts they embed into
    /// `embedded`.
    fn counting(model: &str, projected: &Arc<AtomicUsize>, embedded: &Arc<AtomicUsize>) -> Ports {
        let count = Arc::clone(embedded);
        let embedder = FixedEmbedder::new(
            model,
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
        Ports {
            projector: Some(ProjectorRef::from_sync(Pipes {
                projected: Arc::clone(projected),
            })),
            embedder: Some(EmbedderRef::from_sync(embedder)),
            reranker: None,
        }
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
        // A table of an `INDEX_SCOPE_IDS` collection holds no value column,
        // and one of a collection without vectors holds no vector column.
        let titles = batch.column_by_name("title").map(|column| {
            column
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .clone()
        });
        let ns = batch.column_by_name("n").map(|column| {
            column
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .clone()
        });
        let vectors = batch.column_by_name("_vector_body").cloned();
        (0..batch.num_rows())
            .map(|n| {
                (
                    ids.value(n).to_owned(),
                    (
                        titles
                            .as_ref()
                            .filter(|titles| !titles.is_null(n))
                            .map(|titles| titles.value(n).to_owned()),
                        ns.as_ref()
                            .filter(|ns| !ns.is_null(n))
                            .map(|ns| ns.value(n)),
                        vectors.as_ref().is_some_and(|vectors| !vectors.is_null(n)),
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
        let mut watch = vault.indexer().watch();
        // The state now; each item after it is one the indexer published.
        watch.next().await.unwrap().unwrap();
        crate::indexer::hooks::release(&path);
        wait_indexed(&vault).await;
        assert_eq!(rows(&vault, "notes").await.len(), 1);
        assert_eq!(rows(&vault, "log").await.len(), 1);
        // The state the round published is not ahead of the stored state
        // it shows.
        let head = Some(crate::testing::head(&vault).await);
        loop {
            let state = watch.next().await.unwrap().unwrap();
            if state.version == head {
                assert_eq!(state.stored_version, state.version);
                break;
            }
        }
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
        let projected = Arc::new(AtomicUsize::new(0));
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
                projected: Arc::clone(&projected),
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
        let failures = Arc::new(AtomicUsize::new(0));
        let counted = {
            let mut watch = vault.indexer().watch();
            let failures = Arc::clone(&failures);
            tokio::spawn(async move {
                while let Some(Ok(state)) = watch.next().await {
                    if state.last_error.is_set() {
                        failures.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        };

        broken.store(true, Ordering::SeqCst);
        batch(&vault, "notes", notes(10, 0)).await.unwrap();
        wait_until("the failure", || {
            vault.indexer().status().last_error.is_set() && embedded.lock().unwrap().len() == 5
        })
        .await;
        assert_eq!(live_commit(&vault).await, live, "no commit moved");
        batch(&vault, "notes", notes(1, 10)).await.unwrap();

        // Tried again until it fails twice more.
        let tries = failures.load(Ordering::SeqCst);
        wait_until("two more tries", || {
            failures.load(Ordering::SeqCst) >= tries + 2
        })
        .await;
        broken.store(false, Ordering::SeqCst);
        wait_indexed(&vault).await;
        let done = embedded.lock().unwrap().clone();
        assert_eq!(done.len(), 11);
        assert!(
            done.values().all(|times| *times == 1),
            "a text embedded before the failure is not sent again: {done:?}"
        );
        assert_eq!(
            projected.load(Ordering::SeqCst),
            11,
            "a Document projected before the failure is not projected again"
        );
        let rows = rows(&vault, "notes").await;
        assert_eq!(rows.len(), 11);
        assert!(rows.values().all(|(_, _, vector)| *vector));
        vault.close().await.unwrap();
        counted.await.unwrap();
    }

    /// The names of the columns of the table of `collection`, and the number
    /// of rows that hold a vector, at the versions the live commit names.
    async fn held(vault: &Arc<Vault>, collection: &str) -> (Vec<String>, usize) {
        let (dir, name) = live_table(vault, collection).await;
        let root = Index::new(vault.path()).root().to_path_buf();
        let collection = collection.to_owned();
        let version = blocking(move |cx| {
            let generation = Layout::new(&root).current(cx)?.unwrap();
            let commit = generation.read_commit(cx)?.unwrap();
            Ok(commit.tables.get(&collection).unwrap().versions.rows)
        })
        .await
        .unwrap();
        let dataset = Dataset::open(dir.join(format!("{name}.lance")).to_str().unwrap())
            .await
            .unwrap()
            .checkout_version(version)
            .await
            .unwrap();
        let names = dataset
            .schema()
            .fields
            .iter()
            .map(|field| field.name.clone())
            .collect();
        let batch = dataset.scan().try_into_batch().await.unwrap();
        let vectors = batch
            .column_by_name("_vector_body")
            .map_or(0, |column| column.len() - column.null_count());
        (names, vectors)
    }

    /// The count of `collection` as `Reading` and the queries give it now.
    async fn counted(vault: &Arc<Vault>, collection: &str) -> Result<u64, Error> {
        counted_by(vault, collection, serde_json::json!({})).await
    }

    /// The count of the Documents of `collection` that match the JSON
    /// `query`.
    async fn counted_by(
        vault: &Arc<Vault>,
        collection: &str,
        query: serde_json::Value,
    ) -> Result<u64, Error> {
        let pass = crate::gate::Pass::enter(vault)?;
        let reading = crate::queries::Reading::of(&pass).await?;
        let query: scheda_api::v1::DocumentQuery = serde_json::from_value(query).unwrap();
        Ok(
            crate::queries::find_in_collection(&pass, &reading, collection, &query, false)
                .await?
                .total,
        )
    }

    /// A text search of `collection`, as it answers now.
    async fn searched(vault: &Arc<Vault>, collection: &str) -> Result<(), Error> {
        let pass = crate::gate::Pass::enter(vault)?;
        let reading = crate::queries::Reading::of(&pass).await?;
        let search: scheda_api::v1::DocumentSearch =
            serde_json::from_value(serde_json::json!({"text": "body number"})).unwrap();
        crate::search::in_collection(&pass, &reading, collection, &search).await?;
        Ok(())
    }

    /// Declares `notes` with `scope` (the name of its `IndexScope`), with
    /// the indexer held, and asks what a caller can ask before the round
    /// that rebuilds the table runs: a count and a search answer (a search
    /// of an `INDEX_SCOPE_IDS` collection is refused), and a filter on the
    /// columns of an `INDEX_SCOPE_IDS` collection is refused as the
    /// declaration says. Then lets the round run.
    async fn moved_to(vault: &Arc<Vault>, scope: &str) {
        let ids = scope == "INDEX_SCOPE_IDS";
        let path = vault.path().to_path_buf();
        crate::indexer::hooks::hold(&path);
        crate::testing::update_collection(
            vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}, "indexScope": scope}}),
        )
        .await
        .unwrap();
        wait_until("the held round", || crate::indexer::hooks::waiting(&path)).await;
        assert_eq!(counted(vault, "notes").await.unwrap(), 10);
        let found = searched(vault, "notes").await;
        let filtered = counted_by(
            vault,
            "notes",
            serde_json::json!({"filter": {"term": {"column": "n", "value": {"integer": 3}}}}),
        )
        .await;
        if ids {
            assert!(found.unwrap_err().is(VaultErrorCode::Unimplemented));
            assert!(filtered.unwrap_err().is(VaultErrorCode::Unimplemented));
        } else {
            assert!(found.is_ok(), "{found:?}");
            assert!(filtered.is_ok(), "{filtered:?}");
        }
        crate::indexer::hooks::release(&path);
        wait_indexed(vault).await;
        assert!(
            !vault.indexer().status().last_error.is_set(),
            "{:?}",
            vault.indexer().status()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn moving_a_collection_between_index_scopes_rebuilds_the_table_and_leaves_the_index_fresh()
     {
        let scratch = Scratch::new("move-scopes");
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
        batch(&vault, "notes", notes(10, 0)).await.unwrap();
        wait_indexed(&vault).await;
        let (names, vectors) = held(&vault, "notes").await;
        assert!(names.contains(&String::from("title")) && vectors == 10);

        // ALL to NO_VECTORS: the table loses its vectors, keeps its values.
        moved_to(&vault, "INDEX_SCOPE_NO_VECTORS").await;
        assert_eq!(counted(&vault, "notes").await.unwrap(), 10);
        let (names, vectors) = held(&vault, "notes").await;
        assert!(names.contains(&String::from("title")));
        assert_eq!(vectors, 0);

        // NO_VECTORS to IDS: the table loses its values too.
        moved_to(&vault, "INDEX_SCOPE_IDS").await;
        assert_eq!(counted(&vault, "notes").await.unwrap(), 10);
        let (names, vectors) = held(&vault, "notes").await;
        assert!(!names.contains(&String::from("title")) && !names.contains(&String::from("n")));
        assert_eq!(vectors, 0);

        // IDS to ALL: the values and every vector are back.
        moved_to(&vault, "INDEX_SCOPE_ALL").await;
        let notes_rows = rows(&vault, "notes").await;
        assert_eq!(notes_rows.len(), 10);
        assert!(
            notes_rows
                .values()
                .all(|(title, _, vector)| title.is_some() && *vector)
        );
        assert_eq!(
            notes_rows.get("n003.md"),
            Some(&(Some(String::from("Note 3")), Some(3), true))
        );
        assert_eq!(
            counted_by(
                &vault,
                "notes",
                serde_json::json!({"filter": {"term": {"column": "n", "value": {"integer": 3}}}})
            )
            .await
            .unwrap(),
            1
        );

        // ALL to IDS, then IDS to NO_VECTORS, then back to ALL.
        moved_to(&vault, "INDEX_SCOPE_IDS").await;
        let (names, vectors) = held(&vault, "notes").await;
        assert!(!names.contains(&String::from("title")));
        assert_eq!(vectors, 0);
        moved_to(&vault, "INDEX_SCOPE_NO_VECTORS").await;
        let notes_rows = rows(&vault, "notes").await;
        assert_eq!(notes_rows.len(), 10);
        assert!(
            notes_rows
                .values()
                .all(|(title, _, vector)| title.is_some() && !*vector)
        );
        moved_to(&vault, "INDEX_SCOPE_ALL").await;
        let notes_rows = rows(&vault, "notes").await;
        assert_eq!(
            notes_rows.get("n007.md"),
            Some(&(Some(String::from("Note 7")), Some(7), true))
        );
        vault.close().await.unwrap();
    }

    /// Indexes 30 Documents of `notes` with an embedder of portion 8 that
    /// refuses a whole portion that holds a text with `bad`, and the
    /// Document number `bad_at` has such a text. Returns the calls the
    /// embedder got.
    async fn embed_with_a_refusing_portion(name: &str, bad_at: usize) -> usize {
        let scratch = Scratch::new(name);
        let calls = Arc::new(AtomicUsize::new(0));
        let embedder = {
            let calls = Arc::clone(&calls);
            FixedEmbedder::new(
                "model-a",
                4,
                NonZeroUsize::new(8).unwrap(),
                move |texts: Vec<String>| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if texts.iter().any(|text| text.contains("bad")) {
                        return Err(Failure::new(EmbedderErrorCode::Refused, "a bad text").into());
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
                projected: Arc::new(AtomicUsize::new(0)),
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
        let operations: Vec<DocumentOperation> = (0..30)
            .map(|n| {
                let body = if n == bad_at {
                    String::from("bad body")
                } else {
                    format!("body number {n}")
                };
                DocumentOperation {
                    operation: Some(Operation::Upsert(Box::new(upsert_of(
                        &format!("n{n:03}.md"),
                        format!("Note {n}|{n}|{body}").as_bytes(),
                    )))),
                    ..DocumentOperation::default()
                }
            })
            .collect();
        batch(&vault, "notes", operations).await.unwrap();
        wait_indexed(&vault).await;
        assert!(
            !vault.indexer().status().last_error.is_set(),
            "the round succeeded"
        );
        let rows = rows(&vault, "notes").await;
        assert_eq!(rows.len(), 30);
        for (id, (_, _, vector)) in &rows {
            assert_eq!(
                *vector,
                *id != format!("n{bad_at:03}.md"),
                "only the Document with the bad text lacks its vector: {id}"
            );
        }
        vault.close().await.unwrap();
        calls.load(Ordering::SeqCst)
    }

    /// A refusal of a portion is charged to the text that causes it. The
    /// calls (one per portion of 8) are at most: the first call through the
    /// failing portion (4 portions of 30), the culprit's predecessors alone
    /// (1), the halving (at most ceil(log2 29) = 5 steps of at most 2
    /// portions), the culprit alone (1), and the texts after it in one call
    /// (4 portions at most).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_portion_costs_only_the_text_that_causes_it_its_vector() {
        let bound = 4 + 1 + 5 * 2 + 1 + 4;
        let middle = embed_with_a_refusing_portion("refused-middle", 12).await;
        assert!(middle <= bound, "{middle} calls, at most {bound}");
        let first = embed_with_a_refusing_portion("refused-first", 16).await;
        assert!(first <= bound, "{first} calls, at most {bound}");
    }

    #[test]
    fn the_answers_of_another_projection_and_the_vectors_of_another_model_are_forgotten() {
        let mut cache = super::Cache::default();
        cache.keep_answers_of("a@1");
        cache.keep_vectors_of("model-a");
        cache.answers.insert(
            super::Asked {
                collection: String::from("notes"),
                id: String::from("a.md"),
                digest: String::from("d"),
            },
            super::Answer::Refused,
        );
        cache.vectors.insert(String::from("d"), vec![1.0]);
        cache.keep_answers_of("a@1");
        cache.keep_vectors_of("model-a");
        assert_eq!((cache.answers.len(), cache.vectors.len()), (1, 1));
        cache.keep_vectors_of("model-b");
        assert_eq!((cache.answers.len(), cache.vectors.len()), (1, 0));
        cache.keep_answers_of("a@2");
        assert!(cache.answers.is_empty());
    }

    /// Asks `vault` for a rebuild and waits until it is done.
    async fn reindexed(vault: &Arc<Vault>) {
        let mut watch = vault.indexer().watch();
        // The state now; each item after it is one the indexer published.
        watch.next().await.unwrap().unwrap();
        vault.indexer().reindex().await.unwrap();
        loop {
            let state = watch.next().await.unwrap().unwrap();
            let Some(rebuild) = state.rebuild.as_option() else {
                continue;
            };
            let stage = rebuild.stage.and_then(|stage| stage.as_known());
            let cause = rebuild.cause.and_then(|cause| cause.as_known());
            assert_ne!(stage, Some(Stage::STAGE_FAILED), "{state:?}");
            if stage == Some(Stage::STAGE_DONE) && cause == Some(Cause::CAUSE_REINDEX) {
                return;
            }
        }
    }

    /// The live generation's folder and the table name of `collection`.
    async fn live_table(vault: &Arc<Vault>, collection: &str) -> (PathBuf, String) {
        let root = Index::new(vault.path()).root().to_path_buf();
        let collection = collection.to_owned();
        blocking(move |cx| {
            let generation = Layout::new(&root).current(cx)?.unwrap();
            let commit = generation.read_commit(cx)?.unwrap();
            Ok((
                generation.dir().to_path_buf(),
                commit.tables.get(&collection).unwrap().name.clone(),
            ))
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_rebuild_with_the_same_model_embeds_no_text_again_one_with_another_embeds_each() {
        let scratch = Scratch::new("rebuild-vectors");
        let (ports, _, embedded) = ports();
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
        batch(&vault, "notes", notes(30, 0)).await.unwrap();
        wait_indexed(&vault).await;
        assert_eq!(embedded.load(Ordering::SeqCst), 30);
        let (before, _) = live_table(&vault, "notes").await;

        reindexed(&vault).await;
        let (after, _) = live_table(&vault, "notes").await;
        assert_ne!(after, before, "the rebuild made a new generation");
        assert_eq!(
            embedded.load(Ordering::SeqCst),
            30,
            "the rebuild took the vectors of the live generation"
        );
        let kept = rows(&vault, "notes").await;
        assert_eq!(kept.len(), 30);
        assert!(kept.values().all(|(_, _, vector)| *vector));
        vault.close().await.unwrap();

        // Another model: the rebuild embeds every text with it.
        let (ports, _, embedded) = ports_of("model-b");
        let vault = open_with(
            &scratch.0,
            OpenOptions::default().with_index_poll_ms(10),
            ports,
        )
        .await
        .unwrap();
        reindexed(&vault).await;
        assert_eq!(embedded.load(Ordering::SeqCst), 30);
        let rows = rows(&vault, "notes").await;
        assert_eq!(rows.len(), 30);
        assert!(rows.values().all(|(_, _, vector)| *vector));
        vault.close().await.unwrap();
    }

    /// Two Vaults on one path ask for a rebuild at once: one builds it, and
    /// the other's request is served by it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_rebuild_asked_by_two_vaults_at_once_is_built_once_and_done_for_both() {
        let scratch = Scratch::new("rebuild-twice");
        let projected = Arc::new(AtomicUsize::new(0));
        let embedded = Arc::new(AtomicUsize::new(0));
        let options = OpenOptions::default().with_index_poll_ms(10);
        let one = open_with(
            &scratch.0,
            options.clone(),
            counting("model-a", &projected, &embedded),
        )
        .await
        .unwrap();
        create_collection(
            &one,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        batch(&one, "notes", notes(20, 0)).await.unwrap();
        let two = open_with(
            &scratch.0,
            options,
            counting("model-a", &projected, &embedded),
        )
        .await
        .unwrap();
        wait_indexed(&one).await;
        wait_indexed(&two).await;
        assert_eq!(projected.load(Ordering::SeqCst), 20);
        let (before, _) = live_table(&one, "notes").await;

        // Both ask while the first round to take the lock waits after its
        // look: both see the same live generation.
        let path = one.path().to_path_buf();
        crate::indexer::hooks::hold(&path);
        let mut watches = [one.indexer().watch(), two.indexer().watch()];
        for watch in &mut watches {
            watch.next().await.unwrap().unwrap();
        }
        one.indexer().reindex().await.unwrap();
        two.indexer().reindex().await.unwrap();
        wait_until("a held round", || crate::indexer::hooks::waiting(&path)).await;
        crate::indexer::hooks::release(&path);
        for watch in &mut watches {
            loop {
                let state = watch.next().await.unwrap().unwrap();
                let (stage, cause) = stage_of(&state);
                assert_ne!(stage, Some(Stage::STAGE_FAILED), "{state:?}");
                if (stage, cause) == (Some(Stage::STAGE_DONE), Some(Cause::CAUSE_REINDEX)) {
                    break;
                }
            }
        }
        assert_eq!(
            projected.load(Ordering::SeqCst),
            40,
            "one rebuild projected each Document once"
        );
        assert_eq!(embedded.load(Ordering::SeqCst), 20);
        let (after, _) = live_table(&one, "notes").await;
        assert_ne!(after, before);
        let generations = std::fs::read_dir(after.parent().unwrap())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("g-")
            })
            .count();
        assert_eq!(generations, 2, "the live one and the one it replaced");
        // A request after it is a new one: it builds again.
        reindexed(&one).await;
        assert_eq!(projected.load(Ordering::SeqCst), 60);
        one.close().await.unwrap();
        two.close().await.unwrap();
    }

    /// A rebuild is how a broken Index is repaired: the vectors of a live
    /// generation that cannot be read are made again.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_rebuild_beside_a_generation_whose_vectors_cannot_be_read_embeds_them_again() {
        let scratch = Scratch::new("rebuild-broken");
        let (ports, _, embedded) = ports();
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
        batch(&vault, "notes", notes(30, 0)).await.unwrap();
        wait_indexed(&vault).await;
        assert_eq!(embedded.load(Ordering::SeqCst), 30);
        let (dir, table) = live_table(&vault, "notes").await;
        std::fs::remove_dir_all(dir.join(format!("{table}.lance")).join("data")).unwrap();

        reindexed(&vault).await;
        assert_eq!(embedded.load(Ordering::SeqCst), 60);
        let rows = rows(&vault, "notes").await;
        assert_eq!(rows.len(), 30);
        assert!(rows.values().all(|(_, _, vector)| *vector));
        vault.close().await.unwrap();
    }

    /// The stage and cause of the rebuild `state` reports.
    fn stage_of(state: &IndexState) -> (Option<Stage>, Option<Cause>) {
        let rebuild = state.rebuild.as_option();
        (
            rebuild
                .and_then(|rebuild| rebuild.stage)
                .and_then(|stage| stage.as_known()),
            rebuild
                .and_then(|rebuild| rebuild.cause)
                .and_then(|cause| cause.as_known()),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_rebuild_that_fails_is_tried_again_until_it_is_done() {
        let scratch = Scratch::new("rebuild-again");
        let broken = Arc::new(AtomicBool::new(false));
        let embedder = {
            let broken = Arc::clone(&broken);
            FixedEmbedder::new(
                "model-a",
                4,
                NonZeroUsize::MIN,
                move |texts: Vec<String>| {
                    if broken.load(Ordering::SeqCst) {
                        return Err(Failure::new(EmbedderErrorCode::Unavailable, "down")
                            .with_retryable(true)
                            .into());
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
        batch(&vault, "notes", notes(3, 0)).await.unwrap();
        wait_indexed(&vault).await;

        // A text the live generation lacks, while the embedder is down.
        broken.store(true, Ordering::SeqCst);
        batch(&vault, "notes", notes(1, 3)).await.unwrap();
        let mut watch = vault.indexer().watch();
        watch.next().await.unwrap().unwrap();
        vault.indexer().reindex().await.unwrap();
        loop {
            let state = watch.next().await.unwrap().unwrap();
            if stage_of(&state) == (Some(Stage::STAGE_FAILED), Some(Cause::CAUSE_REINDEX)) {
                break;
            }
        }
        // The request stands until a rebuild serves it.
        let again = vault.indexer().reindex().await.unwrap_err();
        assert!(again.is(VaultErrorCode::Aborted));
        broken.store(false, Ordering::SeqCst);
        let done = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let state = watch.next().await.unwrap().unwrap();
                if stage_of(&state) == (Some(Stage::STAGE_DONE), Some(Cause::CAUSE_REINDEX)) {
                    return state;
                }
            }
        })
        .await
        .expect("the rebuild was tried again");
        assert_eq!(done.rebuild.as_option().unwrap().documents_read, Some(4));
        let rows = rows(&vault, "notes").await;
        assert_eq!(rows.len(), 4);
        assert!(rows.values().all(|(_, _, vector)| *vector));
        // Served: a new request is taken.
        vault.indexer().reindex().await.unwrap();
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
        let before = asked.load(Ordering::SeqCst);
        let started = Instant::now();
        vault.close().await.unwrap();
        let took = started.elapsed();
        // The whole round would take 40 portions.
        assert!(took < PORTION * 4, "close took {took:?}");
        // Close returned before four more portions could be asked. Counted
        // from the start of close, not from a number: a busy machine can
        // run the round on for a while before this task looks at it.
        let after = asked.load(Ordering::SeqCst);
        assert!(after <= before + 4, "{before} portions, then {after}");
        // A blocking embedder's replies are pulled on the blocking pool:
        // right after it sends one vector it starts on the next, and only
        // the send that follows finds the round gone. So one portion may
        // begin after close returned, never a second.
        tokio::time::sleep(PORTION * 2).await;
        let last = asked.load(Ordering::SeqCst);
        assert!(last <= after + 1, "{after} portions at close, {last} later");
    }

    /// A Projector of Documents of lines: the first is `title` (fts), each
    /// other a text of `phrasings`, a `text list` vector column.
    struct Lines;

    impl ProjectorSync for Lines {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("lines@1"))
        }

        fn columns(&self, _collection: CollectionKey) -> Result<IndexSchema, Error> {
            Ok(IndexSchema {
                columns: vec![
                    column("title", ColumnKind::COLUMN_KIND_TEXT).with_fts(true),
                    column("phrasings", ColumnKind::COLUMN_KIND_TEXT_LIST).with_vector(true),
                ],
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            let text = String::from_utf8(document.raw.unwrap_or_default()).unwrap_or_default();
            let mut lines = text.split('\n').map(str::to_owned);
            let title = lines.next().unwrap_or_default();
            let phrasings = TextList {
                texts: lines.collect(),
                ..TextList::default()
            };
            let value = |value: Value| IndexValue {
                value: Some(value),
                ..IndexValue::default()
            };
            Ok(IndexDoc {
                values: [
                    (String::from("title"), value(Value::Text(title))),
                    (
                        String::from("phrasings"),
                        value(Value::TextList(Box::new(phrasings))),
                    ),
                ]
                .into_iter()
                .collect(),
                ..IndexDoc::default()
            })
        }
    }

    /// The ports of [`Lines`], with an embedder that refuses a text with
    /// `bad` in it and counts each text it embeds.
    fn lines_ports(embedded: &Arc<Mutex<HashMap<String, usize>>>) -> Ports {
        let embedded = Arc::clone(embedded);
        let embedder = FixedEmbedder::new(
            "model-a",
            4,
            NonZeroUsize::MIN,
            move |texts: Vec<String>| {
                if texts.iter().any(|text| text.contains("bad")) {
                    return Err(Failure::new(EmbedderErrorCode::Refused, "a bad text").into());
                }
                let mut done = embedded.lock().unwrap();
                for text in &texts {
                    *done.entry(text.clone()).or_default() += 1;
                }
                Ok(texts
                    .iter()
                    .map(|text| vec![text.len() as f32, 1.0, 2.0, 3.0])
                    .collect())
            },
        );
        Ports {
            projector: Some(ProjectorRef::from_sync(Lines)),
            embedder: Some(EmbedderRef::from_sync(embedder)),
            reranker: None,
        }
    }

    /// The live commit of `vault`.
    async fn live(vault: &Arc<Vault>) -> (PathBuf, crate::generation::Commit) {
        let root = Index::new(vault.path()).root().to_path_buf();
        blocking(move |cx| {
            let generation = Layout::new(&root).current(cx)?.unwrap();
            let commit = generation.read_commit(cx)?.unwrap();
            Ok((generation.dir().to_path_buf(), commit))
        })
        .await
        .unwrap()
    }

    /// The texts the texts dataset of `column` of `collection` holds at the
    /// live commit, by Document: each place and the length of its text
    /// (the first value of its vector).
    async fn texts_of(
        vault: &Arc<Vault>,
        collection: &str,
        column: &str,
    ) -> BTreeMap<String, Vec<(u32, f32)>> {
        let (dir, commit) = live(vault).await;
        let table = commit.tables.get(collection).unwrap();
        let version = *table.versions.texts.get(column).unwrap();
        let path = crate::store::texts_path(&dir, &table.name, column);
        let dataset = Dataset::open(path.to_str().unwrap())
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
        let positions = crate::store::positions_in(&batch).unwrap().clone();
        let vectors = crate::store::vectors_in(&batch, "vector").unwrap().clone();
        let mut out: BTreeMap<String, Vec<(u32, f32)>> = BTreeMap::new();
        for n in 0..batch.num_rows() {
            let vector = crate::store::vector_at(&vectors, n).unwrap();
            out.entry(ids.value(n).to_owned())
                .or_default()
                .push((positions.value(n), vector[0]));
        }
        for texts in out.values_mut() {
            texts.sort_by_key(|(position, _)| *position);
        }
        out
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_text_of_a_list_has_its_vector_and_only_a_new_text_is_embedded() {
        let scratch = Scratch::new("text-list");
        let embedded: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let vault = open_with(
            &scratch.0,
            OpenOptions::default().with_index_poll_ms(10),
            lines_ports(&embedded),
        )
        .await
        .unwrap();
        create_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        let put = |id: &'static str, raw: &'static str| {
            let vault = Arc::clone(&vault);
            async move {
                upsert(&vault, "notes", upsert_of(id, raw.as_bytes()))
                    .await
                    .unwrap();
                wait_indexed(&vault).await;
            }
        };
        let count = |text: &str| embedded.lock().unwrap().get(text).copied().unwrap_or(0);
        let total = || embedded.lock().unwrap().values().sum::<usize>();

        // A blank text and a refused one have no vector; the others do,
        // at their places.
        put("a.md", "A\none\n   \nthree!\nbad four").await;
        put("b.md", "B\nfive").await;
        let texts = texts_of(&vault, "notes", "phrasings").await;
        assert_eq!(texts.get("a.md"), Some(&vec![(0, 3.0), (2, 6.0)]));
        assert_eq!(texts.get("b.md"), Some(&vec![(0, 4.0)]));
        assert_eq!((count("one"), count("three!"), count("five")), (1, 1, 1));
        let rows_held = held(&vault, "notes").await.0;
        assert!(
            !rows_held.iter().any(|name| name.starts_with("_vector_")),
            "the vectors of a list are not in the rows: {rows_held:?}"
        );

        // Moved, added, edited, and removed texts: only a new text is
        // embedded, and each text keeps its vector at its new place.
        let before = total();
        put("a.md", "A\nthree!\none").await;
        assert_eq!(total(), before, "moved texts keep their vectors");
        assert_eq!(
            texts_of(&vault, "notes", "phrasings").await.get("a.md"),
            Some(&vec![(0, 6.0), (1, 3.0)])
        );
        put("a.md", "A\nthree!\none\nseven").await;
        assert_eq!((total(), count("seven")), (before + 1, 1));
        put("a.md", "A\nthree!\neight\nseven").await;
        assert_eq!((total(), count("eight")), (before + 2, 1));
        put("a.md", "A\nseven").await;
        assert_eq!(total(), before + 2);
        assert_eq!(
            texts_of(&vault, "notes", "phrasings").await.get("a.md"),
            Some(&vec![(0, 5.0)])
        );

        // A rebuild with the same model embeds nothing; a deleted
        // Document takes its texts with it.
        reindexed(&vault).await;
        assert_eq!(total(), before + 2, "the rebuild took the stored vectors");
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
        let texts = texts_of(&vault, "notes", "phrasings").await;
        assert_eq!(texts.keys().collect::<Vec<_>>(), ["a.md"]);
        assert!(embedded.lock().unwrap().values().all(|times| *times == 1));
        vault.close().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_table_that_loses_its_list_vectors_drops_their_texts_and_one_that_gets_them_back_keeps_them()
     {
        let scratch = Scratch::new("text-list-scopes");
        let embedded: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let vault = open_with(
            &scratch.0,
            OpenOptions::default().with_index_poll_ms(10),
            lines_ports(&embedded),
        )
        .await
        .unwrap();
        create_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        upsert(&vault, "notes", upsert_of("a.md", b"A\none\ntwo"))
            .await
            .unwrap();
        wait_indexed(&vault).await;
        let (_, commit) = live(&vault).await;
        let name = commit.tables.get("notes").unwrap().name.clone();
        let texts = texts_name(&name, "phrasings");

        crate::testing::update_collection(
            &vault,
            serde_json::json!({
                "name": "notes",
                "schema": {"standard": {}, "indexScope": "INDEX_SCOPE_NO_VECTORS"}
            }),
        )
        .await
        .unwrap();
        wait_indexed(&vault).await;
        let (_, commit) = live(&vault).await;
        assert!(
            commit
                .tables
                .get("notes")
                .unwrap()
                .versions
                .texts
                .is_empty()
        );
        assert!(
            commit.dropped.iter().any(|dropped| dropped.name == texts),
            "{:?}",
            commit.dropped
        );

        crate::testing::update_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        wait_indexed(&vault).await;
        let (_, commit) = live(&vault).await;
        assert!(
            !commit.dropped.iter().any(|dropped| dropped.name == texts),
            "a texts dataset the table has again is not removed: {:?}",
            commit.dropped
        );
        assert_eq!(
            texts_of(&vault, "notes", "phrasings").await.get("a.md"),
            Some(&vec![(0, 3.0), (1, 3.0)])
        );
        vault.close().await.unwrap();
    }
}
