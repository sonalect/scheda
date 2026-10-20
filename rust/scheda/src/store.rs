//! The tables of the Index on Lance: one pair of Lance datasets per
//! indexed collection, inside a generation of the Index.
//!
//! ```text
//! .scheda/index/{generation}/commit             what the generation holds, written last
//! .scheda/index/{generation}/t{n}.lance         the rows of one collection
//! .scheda/index/{generation}/t{n}.files.lance   the files those rows came from
//! ```
//!
//! **Rows.** One row per Document, or per journal line: `id`, `file` (the
//! Vault-relative path of the Document, or of the journal segment of a
//! line), `order` (the place in a listing: the `id`, or for a journal line
//! the segment's place in append order and the line's), then the columns
//! the Projector declares for the collection, typed, and for each `vector`
//! column `_vector_{name}` (its embedding) and `_digest_{name}` (the git
//! blob id of the text that was embedded, so an unchanged text keeps its
//! vector). A caller's column never starts with `_`.
//!
//! **Files.** One row per file the rows came from: `path` and `blob`, the
//! git blob id of the file as indexed. A round that cannot know what
//! changed since the Index's state (the commit is gone from its repository)
//! compares the tree of `HEAD` with this table.
//!
//! **Versions.** Every write makes a new Lance version of a dataset. The
//! generation's `commit` names, for each table, the versions a reader opens,
//! so one call sees one state across all collections. A round writes new
//! versions and then `commit`; a dataset ahead of `commit` (a round died
//! after writing it) is restored to the version `commit` names before
//! anything else is written to it.
//!
//! **Typed only.** A delete, an upsert, and a filter are built as typed
//! expressions; no text of a query language is ever made from values.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lance::Dataset;
use lance::dataset::cleanup::CleanupPolicy;
use lance::dataset::optimize::{CompactionOptions, compact_files};
use lance::dataset::{MergeInsertBuilder, WhenMatched, WhenNotMatched, WriteMode, WriteParams};
use lance::deps::arrow_array::builder::{
    BooleanBuilder, FixedSizeListBuilder, Float32Builder, Float64Builder, Int64Builder,
    ListBuilder, StringBuilder,
};
use lance::deps::arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator,
    StringArray,
};
use lance::deps::arrow_schema::{DataType, Field, Schema, SchemaRef};
use lance::deps::datafusion::prelude::{Expr, col, lit};
use lance::index::DatasetIndexExt;
use lance::index::vector::VectorIndexParams;
use lance_index::IndexType;
use lance_index::optimize::OptimizeOptions;
use lance_index::scalar::{InvertedIndexParams, ScalarIndexParams};
use lance_linalg::distance::DistanceType;
use scheda_api::v1::{ColumnKind, IndexColumn, VaultErrorCode, index_value::Value};
use serde::{Deserialize, Serialize};

use crate::error::{Context, Error, Fault};

/// The rows a fragment holds after compaction: Lance's own default (about
/// a million) rewrites the largest fragment on every compaction.
const TARGET_ROWS_PER_FRAGMENT: usize = 16_384;

/// The small fragments (fewer rows than [`TARGET_ROWS_PER_FRAGMENT`]) a
/// table may hold before a round compacts them. A round that writes
/// adds one small fragment, and Lance merges every small fragment with
/// its neighbours, so compacting at each round rewrites the partly full
/// tail again and again: one Document written to a table with vectors
/// rewrote the whole tail (about 4 KB a row at 1024 dimensions; 4 MB for a
/// tail of 1 000 rows, measured). Compacting once per this many fragments
/// writes the tail `SMALL_FRAGMENTS - 1` times less often per round, and a
/// read pays for at most `SMALL_FRAGMENTS - 1` fragments beyond the
/// compacted ones, each small: eight is a few more files to open next to
/// the tens that a table of 16 384-row fragments reaches at a million rows.
const SMALL_FRAGMENTS: usize = 8;

/// The share of a fragment's rows that may be deleted before the fragment
/// is rewritten on its own (Lance's own default is 0.1). Past one half a
/// rewrite writes no more live rows than the deleted ones it reclaims, so
/// the bytes written stay within one row for each row deleted, however
/// often the rows are replaced; below one half the rewrite would write
/// the larger share, again and again, for a smaller one reclaimed.
const MATERIALIZE_DELETIONS: f32 = 0.5;

/// Below this many rows a table gets no vector index: an exact scan is
/// fast enough, and IVF has too little to train on.
pub(crate) const VECTOR_INDEX_ROWS: usize = 10_000;

/// Rows not yet in an index that make a round bring the indices over them.
const UNINDEXED_ROWS: usize = 10_000;

/// The rounds after which the indices are brought over new rows anyway.
const ROUNDS_PER_INDEX_UPDATE: u64 = 50;

/// Deltas of a vector index merged into one once there are more.
const VECTOR_INDEX_DELTAS: usize = 4;

/// How long a version stays after it was replaced: no read of the Index
/// lasts longer than one call, so this is a margin over seconds.
pub(crate) const VERSION_LIFETIME: Duration = Duration::from_secs(3_600);

/// The prefix of the columns the Index keeps for itself beside a caller's.
const VECTOR_PREFIX: &str = "_vector_";
const DIGEST_PREFIX: &str = "_digest_";

/// The failure of Lance on the Index: the storage below it (`STORAGE`), or
/// an Index that is not what it should be (`INDEX_BROKEN`).
pub(crate) fn lance_fault(error: lance::Error) -> Fault {
    let code = match &error {
        lance::Error::IO { .. }
        | lance::Error::DiskCapExceeded { .. }
        | lance::Error::Timeout { .. }
        | lance::Error::Backpressure { .. }
        | lance::Error::TooMuchWriteContention { .. }
        | lance::Error::RetryableCommitConflict { .. }
        | lance::Error::CommitConflict { .. } => VaultErrorCode::Storage,
        _ => VaultErrorCode::IndexBroken,
    };
    Fault::new(code, "the Index on Lance failed").with_source(error)
}

/// A column of a collection as the Index holds it: what the Projector
/// declared.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Column {
    pub(crate) name: String,
    pub(crate) kind: Kind,
    pub(crate) scalar_index: bool,
    pub(crate) fts: bool,
    pub(crate) vector: bool,
}

/// The kind of the values of a column.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum Kind {
    Text,
    Integer,
    Number,
    Boolean,
    TextList,
}

impl Column {
    /// The column a Projector declared, checked already
    /// (`IndexSchema` validates).
    pub(crate) fn of(column: &IndexColumn) -> Option<Column> {
        let kind = match column.kind?.as_known()? {
            ColumnKind::COLUMN_KIND_TEXT => Kind::Text,
            ColumnKind::COLUMN_KIND_INTEGER => Kind::Integer,
            ColumnKind::COLUMN_KIND_NUMBER => Kind::Number,
            ColumnKind::COLUMN_KIND_BOOLEAN => Kind::Boolean,
            ColumnKind::COLUMN_KIND_TEXT_LIST => Kind::TextList,
            ColumnKind::COLUMN_KIND_UNSPECIFIED => return None,
        };
        Some(Column {
            name: column.name.clone().unwrap_or_default(),
            kind,
            scalar_index: column.scalar_index.unwrap_or(false),
            fts: column.fts.unwrap_or(false),
            vector: column.vector.unwrap_or(false),
        })
    }

    fn data_type(&self) -> DataType {
        match self.kind {
            Kind::Text => DataType::Utf8,
            Kind::Integer => DataType::Int64,
            Kind::Number => DataType::Float64,
            Kind::Boolean => DataType::Boolean,
            Kind::TextList => DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))),
        }
    }
}

/// The shape of one collection's table: the columns whose values it
/// holds, the `vector` columns it embeds, and the length of a vector.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Shape {
    /// The columns whose values the table holds (none for a collection
    /// `INDEX_SCOPE_IDS`).
    pub(crate) columns: Vec<Column>,
    /// The `text` columns whose text is embedded, by name (none for a
    /// collection that is not `INDEX_SCOPE_ALL`).
    pub(crate) vectors: Vec<String>,
    /// The length of a vector, when there is a `vector` column.
    pub(crate) dimensions: Option<u32>,
}

impl Shape {
    /// The names of the `vector` columns, in order.
    pub(crate) fn vector_columns(&self) -> impl Iterator<Item = &str> {
        self.vectors.iter().map(String::as_str)
    }

    fn rows_schema(&self) -> SchemaRef {
        let mut fields = vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("file", DataType::Utf8, false),
            Field::new("order", DataType::Utf8, false),
        ];
        for column in &self.columns {
            fields.push(Field::new(&column.name, column.data_type(), true));
        }
        if let Some(dimensions) = self.dimensions {
            for column in self.vector_columns() {
                fields.push(Field::new(
                    vector_name(column),
                    DataType::FixedSizeList(
                        Arc::new(Field::new("item", DataType::Float32, true)),
                        i32::try_from(dimensions).unwrap_or(i32::MAX),
                    ),
                    true,
                ));
                fields.push(Field::new(digest_name(column), DataType::Utf8, true));
            }
        }
        Arc::new(Schema::new(fields))
    }
}

/// The name of the vector column of the `vector` column `name`.
pub(crate) fn vector_name(name: &str) -> String {
    format!("{VECTOR_PREFIX}{name}")
}

/// The name of the digest column of the `vector` column `name`.
pub(crate) fn digest_name(name: &str) -> String {
    format!("{DIGEST_PREFIX}{name}")
}

fn files_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("path", DataType::Utf8, false),
        Field::new("blob", DataType::Utf8, false),
    ]))
}

/// The embedding of one `vector` column of a row, with the digest of the
/// text it came from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Embedding {
    pub(crate) vector: Vec<f32>,
    pub(crate) digest: String,
}

/// One row of a table.
#[derive(Clone, Debug, Default)]
pub(crate) struct Row {
    pub(crate) id: String,
    pub(crate) file: String,
    pub(crate) order: String,
    /// The value of each column of the shape, by name; a column left out
    /// has no value.
    pub(crate) values: BTreeMap<String, Value>,
    /// The embedding of each `vector` column, by name; a column left out
    /// has none (no text, or not embedded).
    pub(crate) embeddings: BTreeMap<String, Embedding>,
}

/// The rows as one batch of the shape's schema. A value of another kind
/// than its column's is left out: the Projector's values are checked
/// before they get here.
fn rows_batch(shape: &Shape, rows: &[Row]) -> Result<RecordBatch, Fault> {
    let schema = shape.rows_schema();
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(schema.fields().len());
    arrays.push(Arc::new(StringArray::from_iter_values(
        rows.iter().map(|row| row.id.as_str()),
    )));
    arrays.push(Arc::new(StringArray::from_iter_values(
        rows.iter().map(|row| row.file.as_str()),
    )));
    arrays.push(Arc::new(StringArray::from_iter_values(
        rows.iter().map(|row| row.order.as_str()),
    )));
    for column in &shape.columns {
        arrays.push(column_array(column, rows));
    }
    if let Some(dimensions) = shape.dimensions {
        let length = i32::try_from(dimensions).unwrap_or(i32::MAX);
        for column in shape.vector_columns() {
            let mut vectors = FixedSizeListBuilder::new(Float32Builder::new(), length);
            let mut digests = StringBuilder::new();
            for row in rows {
                match row.embeddings.get(column) {
                    Some(embedding) if embedding.vector.len() == dimensions as usize => {
                        vectors.values().append_slice(&embedding.vector);
                        vectors.append(true);
                        digests.append_value(&embedding.digest);
                    }
                    _ => {
                        for _ in 0..dimensions {
                            vectors.values().append_null();
                        }
                        vectors.append(false);
                        digests.append_null();
                    }
                }
            }
            arrays.push(Arc::new(vectors.finish()));
            arrays.push(Arc::new(digests.finish()));
        }
    }
    RecordBatch::try_new(schema, arrays).map_err(|err| {
        Fault::new(
            VaultErrorCode::IndexBroken,
            "cannot make the rows of the Index",
        )
        .with_source(err)
    })
}

/// The values of `column` for `rows`, null where a row has none of its
/// kind.
fn column_array(column: &Column, rows: &[Row]) -> ArrayRef {
    let values = rows.iter().map(|row| row.values.get(&column.name));
    match column.kind {
        Kind::Text => {
            let mut builder = StringBuilder::new();
            for value in values {
                match value {
                    Some(Value::Text(text)) => builder.append_value(text),
                    _ => builder.append_null(),
                }
            }
            Arc::new(builder.finish())
        }
        Kind::Integer => {
            let mut builder = Int64Builder::new();
            for value in values {
                match value {
                    Some(Value::Integer(n)) => builder.append_value(*n),
                    _ => builder.append_null(),
                }
            }
            Arc::new(builder.finish())
        }
        Kind::Number => {
            let mut builder = Float64Builder::new();
            for value in values {
                match value {
                    Some(Value::Number(n)) if n.is_finite() => builder.append_value(*n),
                    _ => builder.append_null(),
                }
            }
            Arc::new(builder.finish())
        }
        Kind::Boolean => {
            let mut builder = BooleanBuilder::new();
            for value in values {
                match value {
                    Some(Value::Boolean(b)) => builder.append_value(*b),
                    _ => builder.append_null(),
                }
            }
            Arc::new(builder.finish())
        }
        Kind::TextList => {
            let mut builder = ListBuilder::new(StringBuilder::new());
            for value in values {
                match value {
                    Some(Value::TextList(list)) => {
                        for text in &list.texts {
                            builder.values().append_value(text);
                        }
                        builder.append(true);
                    }
                    _ => builder.append(false),
                }
            }
            Arc::new(builder.finish())
        }
    }
}

/// The files as one batch of the files table.
fn files_batch(files: &[(String, String)]) -> Result<RecordBatch, Fault> {
    RecordBatch::try_new(
        files_schema(),
        vec![
            Arc::new(StringArray::from_iter_values(
                files.iter().map(|(path, _)| path.as_str()),
            )),
            Arc::new(StringArray::from_iter_values(
                files.iter().map(|(_, blob)| blob.as_str()),
            )),
        ],
    )
    .map_err(|err| {
        Fault::new(
            VaultErrorCode::IndexBroken,
            "cannot make the files of the Index",
        )
        .with_source(err)
    })
}

/// The versions of a table's two datasets that a generation's `commit`
/// names.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Versions {
    pub(crate) rows: u64,
    pub(crate) files: u64,
}

/// The URI of a dataset at `path`, as Lance takes a local one.
fn uri(path: &Path) -> Result<String, Fault> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        Fault::new(
            VaultErrorCode::Storage,
            "the path of the Index is not valid UTF-8",
        )
    })
}

/// The dataset at `path` at `version`, or the latest one.
async fn open_at(path: &Path, version: Option<u64>) -> Result<Dataset, Fault> {
    let dataset = Dataset::open(&uri(path)?).await.map_err(lance_fault)?;
    match version {
        Some(version) if version != dataset.version().version => {
            dataset.checkout_version(version).await.map_err(lance_fault)
        }
        _ => Ok(dataset),
    }
}

/// A new, empty dataset of `schema` at `path`, replacing what is there.
async fn create(path: &Path, schema: SchemaRef) -> Result<Dataset, Fault> {
    let empty = RecordBatchIterator::new(Vec::<Result<RecordBatch, _>>::new(), schema);
    let params = WriteParams {
        mode: WriteMode::Overwrite,
        ..WriteParams::default()
    };
    Dataset::write(empty, uri(path)?.as_str(), Some(params))
        .await
        .map_err(lance_fault)
}

/// The dataset at `path` for a round: at `committed` when a round wrote
/// past it and died (restored, so the round starts from the committed
/// state), a new empty one when `commit` names none (whatever is there was
/// never believed).
async fn for_round(
    path: &Path,
    schema: SchemaRef,
    committed: Option<u64>,
) -> Result<Dataset, Fault> {
    let Some(committed) = committed else {
        return create(path, schema).await;
    };
    let latest = open_at(path, None).await?;
    if latest.version().version == committed {
        return Ok(latest);
    }
    let mut at = latest
        .checkout_version(committed)
        .await
        .map_err(lance_fault)?;
    at.restore().await.map_err(lance_fault)?;
    Ok(at)
}

/// A string list as the typed expression `column IN (values)`.
fn one_of(column: &str, values: &[String]) -> Expr {
    col(column).in_list(
        values.iter().map(|value| lit(value.as_str())).collect(),
        false,
    )
}

/// The embeddings of some rows, by id and then by `vector` column.
pub(crate) type Embeddings = HashMap<String, BTreeMap<String, Embedding>>;

/// The embeddings `rows`, a rows dataset of `shape`, holds for the rows
/// `ids`.
async fn embeddings(rows: &Dataset, shape: &Shape, ids: &[String]) -> Result<Embeddings, Error> {
    let mut out = Embeddings::new();
    let columns = &shape.vectors;
    if ids.is_empty() || columns.is_empty() || shape.dimensions.is_none() {
        return Ok(out);
    }
    let mut projection = vec![String::from("id")];
    for name in columns {
        projection.push(vector_name(name));
        projection.push(digest_name(name));
    }
    let mut scanner = rows.scan();
    scanner
        .project(&projection)
        .map_err(lance_fault)?
        .filter_expr(one_of("id", ids));
    let batch = scanner
        .try_into_batch()
        .await
        .map_err(lance_fault)
        .context("read the stored vectors")?;
    let id = strings(&batch, "id")?;
    for name in columns {
        let digests = strings(&batch, &digest_name(name))?;
        let vectors = batch
            .column_by_name(&vector_name(name))
            .and_then(|array| array.as_any().downcast_ref::<FixedSizeListArray>())
            .ok_or_else(|| broken("a vector column of the Index is not a vector"))?;
        for n in 0..batch.num_rows() {
            if digests.is_null(n) || vectors.is_null(n) {
                continue;
            }
            let values = vectors.value(n);
            let Some(values) = values.as_any().downcast_ref::<Float32Array>() else {
                return Err(broken("a vector of the Index is not of floats").into());
            };
            out.entry(id.value(n).to_owned()).or_default().insert(
                name.clone(),
                Embedding {
                    vector: values.values().to_vec(),
                    digest: digests.value(n).to_owned(),
                },
            );
        }
    }
    Ok(out)
}

/// The vectors one collection's table of another generation holds, open
/// only to read, at the version that generation's `commit` names.
pub(crate) struct Vectors {
    shape: Shape,
    rows: Dataset,
}

impl Vectors {
    /// The table `name` of the generation at `dir`, of `shape`, at
    /// `versions`.
    ///
    /// # Errors
    ///
    /// `INDEX_BROKEN` or `STORAGE` when Lance cannot open it.
    pub(crate) async fn open(
        dir: &Path,
        name: &str,
        shape: Shape,
        versions: Versions,
    ) -> Result<Vectors, Error> {
        let rows = open_at(&dir.join(format!("{name}.lance")), Some(versions.rows))
            .await
            .context("open the rows of a collection")?;
        Ok(Vectors { shape, rows })
    }

    /// The embeddings the table holds for the rows `ids`.
    ///
    /// # Errors
    ///
    /// `INDEX_BROKEN` or `STORAGE` when Lance fails.
    pub(crate) async fn embeddings(&self, ids: &[String]) -> Result<Embeddings, Error> {
        embeddings(&self.rows, &self.shape, ids).await
    }
}

/// One collection's table, open for a round of the indexer.
pub(crate) struct Table {
    shape: Shape,
    rows: Arc<Dataset>,
    files: Arc<Dataset>,
    /// Whether this round wrote to it.
    written: bool,
}

impl Table {
    /// The table `name` (its folder name, `t{n}`) of the generation at
    /// `dir`, of `shape`, at the versions `committed` names (a new, empty
    /// table without them).
    ///
    /// # Errors
    ///
    /// `INDEX_BROKEN` or `STORAGE` when Lance cannot open or restore it.
    pub(crate) async fn open(
        dir: &Path,
        name: &str,
        shape: Shape,
        committed: Option<Versions>,
    ) -> Result<Table, Error> {
        let rows = for_round(
            &dir.join(format!("{name}.lance")),
            shape.rows_schema(),
            committed.map(|versions| versions.rows),
        )
        .await
        .context("open the rows of a collection")?;
        let files = for_round(
            &dir.join(format!("{name}.files.lance")),
            files_schema(),
            committed.map(|versions| versions.files),
        )
        .await
        .context("open the files of a collection")?;
        Ok(Table {
            shape,
            rows: Arc::new(rows),
            files: Arc::new(files),
            written: committed.is_none(),
        })
    }

    /// The shape of the table.
    pub(crate) fn shape(&self) -> &Shape {
        &self.shape
    }

    /// The versions to name in `commit`.
    pub(crate) fn versions(&self) -> Versions {
        Versions {
            rows: self.rows.version().version,
            files: self.files.version().version,
        }
    }

    /// Writes `rows` whole: a row whose `id` the table holds is replaced,
    /// another is added.
    ///
    /// # Errors
    ///
    /// `INDEX_BROKEN` or `STORAGE` when Lance fails.
    pub(crate) async fn upsert(&mut self, rows: &[Row]) -> Result<(), Error> {
        if rows.is_empty() {
            return Ok(());
        }
        let batch = rows_batch(&self.shape, rows)?;
        let schema = batch.schema();
        let mut builder = MergeInsertBuilder::try_new(Arc::clone(&self.rows), vec!["id".into()])
            .map_err(lance_fault)?;
        builder
            .when_matched(WhenMatched::UpdateAll)
            .when_not_matched(WhenNotMatched::InsertAll);
        let job = builder.try_build().map_err(lance_fault)?;
        let (dataset, _) = job
            .execute_reader(RecordBatchIterator::new(vec![Ok(batch)], schema))
            .await
            .map_err(lance_fault)
            .context("upsert rows")?;
        self.rows = dataset;
        self.written = true;
        Ok(())
    }

    /// Deletes the rows whose `column` (`id` or `file`) is one of
    /// `values`.
    ///
    /// # Errors
    ///
    /// `INDEX_BROKEN` or `STORAGE` when Lance fails.
    pub(crate) async fn delete_rows(
        &mut self,
        column: &str,
        values: &[String],
    ) -> Result<(), Error> {
        if values.is_empty() {
            return Ok(());
        }
        let deleted = lance::dataset::DeleteBuilder::from_expr(
            Arc::clone(&self.rows),
            one_of(column, values),
        )
        .execute()
        .await
        .map_err(lance_fault)
        .context("delete rows")?;
        self.rows = deleted.new_dataset;
        self.written = true;
        Ok(())
    }

    /// Records `files` (path, blob) as indexed, replacing what the table
    /// held for those paths.
    ///
    /// # Errors
    ///
    /// `INDEX_BROKEN` or `STORAGE` when Lance fails.
    pub(crate) async fn put_files(&mut self, files: &[(String, String)]) -> Result<(), Error> {
        if files.is_empty() {
            return Ok(());
        }
        let batch = files_batch(files)?;
        let schema = batch.schema();
        let mut builder = MergeInsertBuilder::try_new(Arc::clone(&self.files), vec!["path".into()])
            .map_err(lance_fault)?;
        builder
            .when_matched(WhenMatched::UpdateAll)
            .when_not_matched(WhenNotMatched::InsertAll);
        let job = builder.try_build().map_err(lance_fault)?;
        let (dataset, _) = job
            .execute_reader(RecordBatchIterator::new(vec![Ok(batch)], schema))
            .await
            .map_err(lance_fault)
            .context("record the files")?;
        self.files = dataset;
        self.written = true;
        Ok(())
    }

    /// Forgets the files at `paths`.
    ///
    /// # Errors
    ///
    /// `INDEX_BROKEN` or `STORAGE` when Lance fails.
    pub(crate) async fn remove_files(&mut self, paths: &[String]) -> Result<(), Error> {
        if paths.is_empty() {
            return Ok(());
        }
        let deleted = lance::dataset::DeleteBuilder::from_expr(
            Arc::clone(&self.files),
            one_of("path", paths),
        )
        .execute()
        .await
        .map_err(lance_fault)
        .context("forget files")?;
        self.files = deleted.new_dataset;
        self.written = true;
        Ok(())
    }

    /// Every file the table holds, with its blob id.
    ///
    /// # Errors
    ///
    /// `INDEX_BROKEN` or `STORAGE` when Lance fails.
    pub(crate) async fn files(&self) -> Result<BTreeMap<String, String>, Error> {
        let batch = self
            .files
            .scan()
            .try_into_batch()
            .await
            .map_err(lance_fault)
            .context("read the files")?;
        let paths = strings(&batch, "path")?;
        let blobs = strings(&batch, "blob")?;
        Ok((0..batch.num_rows())
            .map(|n| (paths.value(n).to_owned(), blobs.value(n).to_owned()))
            .collect())
    }

    /// The embeddings the table holds for the rows `ids`, by id and then by
    /// `vector` column, so a text whose digest did not change keeps its
    /// vector.
    ///
    /// # Errors
    ///
    /// `INDEX_BROKEN` or `STORAGE` when Lance fails.
    pub(crate) async fn embeddings(&self, ids: &[String]) -> Result<Embeddings, Error> {
        embeddings(&self.rows, &self.shape, ids).await
    }

    /// Keeps the table cheap to read after a round that wrote to it:
    /// compaction of the small fragments once [`SMALL_FRAGMENTS`] of them
    /// have piled up (or one has most of its rows deleted); the indices
    /// on `id`, the
    /// `scalar_index` and `fts` columns, and, from
    /// [`VECTOR_INDEX_ROWS`] rows on, the `vector` columns; the indices
    /// brought over new rows when many are unindexed or every
    /// [`ROUNDS_PER_INDEX_UPDATE`] rounds; and the versions replaced more
    /// than [`VERSION_LIFETIME`] ago pruned. `round` counts the rounds of
    /// the generation.
    ///
    /// # Errors
    ///
    /// `INDEX_BROKEN` or `STORAGE` when Lance fails.
    pub(crate) async fn maintain(&mut self, round: u64) -> Result<(), Error> {
        if !self.written {
            return Ok(());
        }
        let mut rows = Dataset::clone(&self.rows);
        let options = CompactionOptions {
            target_rows_per_fragment: TARGET_ROWS_PER_FRAGMENT,
            materialize_deletions_threshold: MATERIALIZE_DELETIONS,
            ..CompactionOptions::default()
        };
        if wants_compaction(&rows) {
            compact_files(&mut rows, options.clone(), None)
                .await
                .map_err(lance_fault)
                .context("compact the rows")?;
        }
        let mut files = Dataset::clone(&self.files);
        if wants_compaction(&files) {
            compact_files(&mut files, options, None)
                .await
                .map_err(lance_fault)
                .context("compact the files")?;
        }
        let created = self.create_indices(&mut rows).await?;
        let total = rows.count_rows(None).await.map_err(lance_fault)?;
        if total > 0 && !created {
            let unindexed = unindexed_rows(&rows).await?;
            if unindexed > UNINDEXED_ROWS || round.is_multiple_of(ROUNDS_PER_INDEX_UPDATE) {
                let options = OptimizeOptions::merge(VECTOR_INDEX_DELTAS);
                rows.optimize_indices(&options)
                    .await
                    .map_err(lance_fault)
                    .context("bring the indices over new rows")?;
            }
        }
        prune(&rows).await.context("prune the rows")?;
        prune(&files).await.context("prune the files")?;
        self.rows = Arc::new(rows);
        self.files = Arc::new(files);
        Ok(())
    }

    /// Creates the indices the table lacks; whether it created one.
    async fn create_indices(&self, rows: &mut Dataset) -> Result<bool, Error> {
        let total = rows.count_rows(None).await.map_err(lance_fault)?;
        if total == 0 {
            return Ok(false);
        }
        let existing: Vec<String> = rows
            .load_indices()
            .await
            .map_err(lance_fault)?
            .iter()
            .map(|index| index.name.clone())
            .collect();
        let mut created = false;
        let scalar = ScalarIndexParams::default();
        let mut wanted: Vec<(String, IndexType)> = vec![(String::from("id"), IndexType::BTree)];
        for column in &self.shape.columns {
            if column.scalar_index && column.kind != Kind::TextList {
                wanted.push((column.name.clone(), IndexType::BTree));
            }
            if column.fts {
                wanted.push((column.name.clone(), IndexType::Inverted));
            }
        }
        for (column, kind) in wanted {
            let name = index_name(&column, kind);
            if existing.contains(&name) {
                continue;
            }
            match kind {
                IndexType::Inverted => {
                    let params = InvertedIndexParams::default().stem(false);
                    rows.create_index(&[column.as_str()], kind, Some(name), &params, true)
                        .await
                }
                _ => {
                    rows.create_index(&[column.as_str()], kind, Some(name), &scalar, true)
                        .await
                }
            }
            .map_err(lance_fault)
            .context("make an index of a column")?;
            created = true;
        }
        if total >= VECTOR_INDEX_ROWS
            && let Some(dimensions) = self.shape.dimensions
        {
            for column in self.shape.vector_columns() {
                let target = vector_name(column);
                let name = index_name(&target, IndexType::Vector);
                if existing.contains(&name) {
                    continue;
                }
                let params = VectorIndexParams::ivf_pq(
                    partitions(total),
                    8,
                    sub_vectors(dimensions),
                    DistanceType::Cosine,
                    50,
                );
                rows.create_index(
                    &[target.as_str()],
                    IndexType::Vector,
                    Some(name),
                    &params,
                    true,
                )
                .await
                .map_err(lance_fault)
                .context("make the vector index")?;
                created = true;
            }
        }
        Ok(created)
    }
}

/// The name Scheda gives the index of `column` of `kind`.
fn index_name(column: &str, kind: IndexType) -> String {
    let tag = match kind {
        IndexType::BTree => "btree",
        IndexType::Inverted => "fts",
        _ => "ivf_pq",
    };
    format!("{column}_{tag}")
}

/// IVF partitions for `rows` rows: about the square root, at least one.
fn partitions(rows: usize) -> usize {
    (rows as f64).sqrt().round().max(1.0) as usize
}

/// PQ sub-vectors for vectors of `dimensions`: the largest divisor of the
/// length up to a sixteenth of it, at least one.
fn sub_vectors(dimensions: u32) -> usize {
    let dimensions = dimensions as usize;
    let most = (dimensions / 16).max(1);
    (1..=most)
        .rev()
        .find(|n| dimensions.is_multiple_of(*n))
        .unwrap_or(1)
}

/// How many rows of `rows` no index covers yet, the most of any index.
async fn unindexed_rows(rows: &Dataset) -> Result<usize, Error> {
    let indices = rows.load_indices().await.map_err(lance_fault)?;
    let total = rows.count_rows(None).await.map_err(lance_fault)?;
    let mut most = 0;
    for index in indices.iter() {
        let covered: usize = index
            .fragment_bitmap
            .as_ref()
            .map(|bitmap| {
                rows.get_fragments()
                    .iter()
                    .filter(|fragment| bitmap.contains(fragment.id() as u32))
                    .map(|fragment| fragment.metadata().num_rows().unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);
        most = most.max(total.saturating_sub(covered));
    }
    Ok(most)
}

/// Prunes the versions of `dataset` replaced more than
/// [`VERSION_LIFETIME`] ago: every version before the newest one whose
/// successor is older than that. Lance prunes by a version's own creation
/// time, which says nothing of when it was replaced, so the versions are
/// named by number. Files a writer may still be making are never touched
/// (`delete_unverified` stays off).
async fn prune(dataset: &Dataset) -> Result<(), Error> {
    let versions = dataset.versions().await.map_err(lance_fault)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64);
    let lifetime = VERSION_LIFETIME.as_millis() as i64;
    let Some(before) = prunable(
        versions
            .iter()
            .map(|version| (version.version, version.timestamp.timestamp_millis())),
        now,
        lifetime,
    ) else {
        return Ok(());
    };
    let policy = CleanupPolicy {
        before_version: Some(before),
        ..CleanupPolicy::default()
    };
    dataset
        .cleanup_with_policy(policy)
        .await
        .map_err(lance_fault)?;
    Ok(())
}

/// The first version to keep, of `versions` (number, creation time in
/// milliseconds) at `now`: a version is pruned once its successor is older
/// than `lifetime`. `None` when nothing is to be pruned.
fn prunable(versions: impl Iterator<Item = (u64, i64)>, now: i64, lifetime: i64) -> Option<u64> {
    let mut versions: Vec<(u64, i64)> = versions.collect();
    versions.sort_unstable();
    let mut keep_from = None;
    for pair in versions.windows(2) {
        let (successor, made) = pair[1];
        if now - made > lifetime {
            keep_from = Some(successor);
        }
    }
    keep_from
}

/// The string column `name` of `batch`.
fn strings<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray, Fault> {
    batch
        .column_by_name(name)
        .and_then(|array| array.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| broken(format!("the column {name} of the Index is missing")))
}

/// Whether `dataset` holds [`SMALL_FRAGMENTS`] small fragments, or one
/// with more than [`MATERIALIZE_DELETIONS`] of its rows deleted. Read from
/// the fragment metadata of the manifest; no data is read.
fn wants_compaction(dataset: &Dataset) -> bool {
    let mut small = 0;
    for fragment in dataset.get_fragments() {
        let metadata = fragment.metadata();
        let Some(physical) = metadata.physical_rows else {
            continue;
        };
        if physical < TARGET_ROWS_PER_FRAGMENT {
            small += 1;
        }
        let deleted = metadata
            .deletion_file
            .as_ref()
            .and_then(|file| file.num_deleted_rows)
            .unwrap_or(0);
        if physical > 0 && deleted as f32 / physical as f32 > MATERIALIZE_DELETIONS {
            return true;
        }
    }
    small >= SMALL_FRAGMENTS
}

fn broken(message: impl Into<String>) -> Fault {
    Fault::new(VaultErrorCode::IndexBroken, message)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use lance_index::scalar::FullTextSearchQuery;

    use super::*;

    const HOUR: i64 = 3_600_000;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-store-{name}-{}", std::process::id()));
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

    fn column(name: &str, scalar_index: bool, fts: bool, vector: bool) -> Column {
        Column {
            name: name.to_owned(),
            kind: Kind::Text,
            scalar_index,
            fts,
            vector,
        }
    }

    /// A vector of `dimensions` values from `seed`, the same each time.
    fn vector_of(seed: usize, dimensions: usize) -> Vec<f32> {
        let mut state = (seed as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
        (0..dimensions)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state % 1000) as f32 / 1000.0
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_indices_are_made_and_the_scans_use_them() {
        const DIMENSIONS: usize = 16;
        let scratch = Scratch::new("indices");
        let shape = Shape {
            columns: vec![
                column("kind", true, false, false),
                column("title", false, true, false),
                column("topic", false, false, true),
            ],
            vectors: vec![String::from("topic")],
            dimensions: Some(DIMENSIONS as u32),
        };
        let mut table = Table::open(&scratch.0, "t0", shape, None).await.unwrap();
        let rows: Vec<Row> = (0..VECTOR_INDEX_ROWS)
            .map(|n| {
                let id = format!("r{n:05}");
                Row {
                    file: format!("notes/{id}"),
                    order: id.clone(),
                    id,
                    values: BTreeMap::from([
                        (String::from("kind"), Value::Text(format!("k{}", n % 7))),
                        (
                            String::from("title"),
                            Value::Text(format!("word{} common", n % 100)),
                        ),
                    ]),
                    embeddings: BTreeMap::from([(
                        String::from("topic"),
                        Embedding {
                            vector: vector_of(n, DIMENSIONS),
                            digest: format!("d{n}"),
                        },
                    )]),
                }
            })
            .collect();
        table.upsert(&rows).await.unwrap();
        table.maintain(1).await.unwrap();
        let rows = Dataset::clone(&table.rows);

        let mut scanner = rows.scan();
        scanner.filter_expr(col("kind").eq(lit("k3")));
        let plan = scanner.explain_plan(false).await.unwrap();
        assert!(plan.contains("ScalarIndexQuery"), "{plan}");

        let mut scanner = rows.scan();
        scanner
            .full_text_search(
                FullTextSearchQuery::new(String::from("word42"))
                    .with_columns(&[String::from("title")])
                    .unwrap(),
            )
            .unwrap();
        let plan = scanner.explain_plan(false).await.unwrap();
        assert!(plan.contains("MatchQuery: column=title"), "{plan}");
        assert!(!plan.contains("FlatMatchQuery"), "{plan}");

        let query = Float32Array::from(vector_of(7, DIMENSIONS));
        let mut scanner = rows.scan();
        scanner
            .nearest(&vector_name("topic"), &query, 10)
            .unwrap()
            .distance_metric(DistanceType::Cosine)
            .refine(10);
        let plan = scanner.explain_plan(false).await.unwrap();
        assert!(plan.contains("ANNIvfPartition"), "{plan}");
    }

    /// A shape of text columns only: `title` (fts) and `kind` (a scalar
    /// index).
    fn texts() -> Shape {
        Shape {
            columns: vec![
                column("title", false, true, false),
                column("kind", true, false, false),
            ],
            vectors: Vec::new(),
            dimensions: None,
        }
    }

    /// The row `n` with the title `title`.
    fn titled(n: usize, title: &str) -> Row {
        let id = format!("r{n:05}");
        Row {
            file: format!("notes/{id}"),
            order: id.clone(),
            id,
            values: BTreeMap::from([
                (String::from("title"), Value::Text(title.to_owned())),
                (String::from("kind"), Value::Text(format!("k{}", n % 3))),
            ]),
            embeddings: BTreeMap::new(),
        }
    }

    /// Every title of `rows` by id, a row that is there twice with two.
    async fn titles_of(rows: &Dataset) -> BTreeMap<String, Vec<String>> {
        let mut scanner = rows.scan();
        scanner.project(&["id", "title"]).unwrap();
        let batch = scanner.try_into_batch().await.unwrap();
        let ids = strings(&batch, "id").unwrap();
        let titles = strings(&batch, "title").unwrap();
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for n in 0..batch.num_rows() {
            out.entry(ids.value(n).to_owned())
                .or_default()
                .push(titles.value(n).to_owned());
        }
        out
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_round_cut_short_is_rolled_back_to_the_versions_the_commit_names() {
        let scratch = Scratch::new("restore");
        let first: Vec<Row> = (0..10).map(|n| titled(n, "one")).collect();
        let second: Vec<Row> = (5..20).map(|n| titled(n, "two")).collect();
        let gone = [String::from("r00000")];

        // The round that ran to its end, in a table of its own.
        let mut whole = Table::open(&scratch.0, "t1", texts(), None).await.unwrap();
        whole.upsert(&first).await.unwrap();
        whole.upsert(&second).await.unwrap();
        whole.delete_rows("id", &gone).await.unwrap();
        let wanted = titles_of(&whole.rows).await;

        let mut table = Table::open(&scratch.0, "t0", texts(), None).await.unwrap();
        table.upsert(&first).await.unwrap();
        let committed = table.versions();
        // A round that wrote past the commit, then was killed.
        table.upsert(&second).await.unwrap();
        drop(table);
        // A reader opens the version the commit names.
        let path = scratch.0.join("t0.lance");
        let reader = Dataset::open(path.to_str().unwrap())
            .await
            .unwrap()
            .checkout_version(committed.rows)
            .await
            .unwrap();
        let read = titles_of(&reader).await;
        assert_eq!(read.len(), 10);
        assert!(read.values().all(|titles| titles == &["one"]));

        // The next round starts from the commit and ends as the whole one.
        let mut again = Table::open(&scratch.0, "t0", texts(), Some(committed))
            .await
            .unwrap();
        assert_eq!(titles_of(&again.rows).await, read);
        again.upsert(&second).await.unwrap();
        again.delete_rows("id", &gone).await.unwrap();
        assert_eq!(titles_of(&again.rows).await, wanted);
    }

    /// lancedb #3177 / #3515: an upsert of existing rows after compaction
    /// and index optimisation must not leave a row twice.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_hundred_upserts_of_the_same_rows_never_leave_a_row_twice() {
        let scratch = Scratch::new("upserts");
        let mut table = Table::open(&scratch.0, "t0", texts(), None).await.unwrap();
        for round in 0..200_u64 {
            let marker = format!("m{round}");
            let rows: Vec<Row> = (0..50).map(|n| titled(n, &marker)).collect();
            table.upsert(&rows).await.unwrap();
            // Compaction as the small fragments pile up, the indices made
            // at once and brought over the new rows every fiftieth.
            table.maintain(round + 1).await.unwrap();
            if round % 50 == 49 || round == 0 {
                let titles = titles_of(&table.rows).await;
                assert_eq!(titles.len(), 50, "round {round}");
                assert!(
                    titles.values().all(|titles| titles == &[marker.as_str()]),
                    "round {round}: {titles:?}"
                );
            }
        }
    }

    /// The bytes of each data file of the table at `dir`, by name.
    fn data_files(dir: &Path) -> BTreeMap<String, u64> {
        std::fs::read_dir(dir.join("data"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.metadata().unwrap().len(),
                )
            })
            .collect()
    }

    /// The rows of each fragment of `rows`, and the deleted ones of each.
    fn fragment_rows(rows: &Dataset) -> Vec<(usize, usize)> {
        rows.get_fragments()
            .iter()
            .map(|fragment| {
                let metadata = fragment.metadata();
                (
                    metadata.physical_rows.unwrap_or(0),
                    metadata
                        .deletion_file
                        .as_ref()
                        .and_then(|file| file.num_deleted_rows)
                        .unwrap_or(0),
                )
            })
            .collect()
    }

    /// A table with one large fragment of 20 000 rows, made and kept as it
    /// is.
    async fn table_with_a_large_fragment(scratch: &Scratch) -> Table {
        let mut table = Table::open(&scratch.0, "t0", texts(), None).await.unwrap();
        let large: Vec<Row> = (0..20_000)
            .map(|n| titled(n, &format!("a title of some length, number {n}")))
            .collect();
        table.upsert(&large).await.unwrap();
        table.maintain(1).await.unwrap();
        assert_eq!(fragment_rows(&table.rows), vec![(20_000, 0)]);
        table
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rounds_of_one_document_compact_only_once_the_small_fragments_pile_up() {
        let scratch = Scratch::new("compaction");
        let mut table = table_with_a_large_fragment(&scratch).await;
        let dir = scratch.0.join("t0.lance");
        let before = data_files(&dir);
        let large_bytes: u64 = before.values().sum();
        for write in 0..SMALL_FRAGMENTS - 1 {
            table
                .upsert(&[titled(30_000 + write, "a small write")])
                .await
                .unwrap();
            table.maintain(2 + write as u64).await.unwrap();
            assert_eq!(
                table.rows.get_fragments().len(),
                2 + write,
                "round {write}: each Document adds a fragment, none is rewritten"
            );
        }
        // The round that brings the small fragments to the count.
        table
            .upsert(&[titled(40_000, "the last small write")])
            .await
            .unwrap();
        table.maintain(100).await.unwrap();
        assert_eq!(
            fragment_rows(&table.rows),
            vec![(20_000, 0), (SMALL_FRAGMENTS, 0)],
            "the small fragments are one"
        );
        let after = data_files(&dir);
        assert!(
            before.keys().all(|name| after.contains_key(name)),
            "the large fragment is kept as it was"
        );
        let written: u64 = after
            .iter()
            .filter(|(name, _)| !before.contains_key(*name))
            .map(|(_, bytes)| bytes)
            .sum();
        assert!(
            written * 20 < large_bytes,
            "{written} bytes written for {SMALL_FRAGMENTS} rows next to {large_bytes}"
        );
        assert_eq!(titles_of(&table.rows).await.len(), 20_000 + SMALL_FRAGMENTS);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_fragment_is_rewritten_once_most_of_its_rows_are_deleted() {
        let scratch = Scratch::new("deletions");
        let mut table = table_with_a_large_fragment(&scratch).await;
        // Deleted rows below the share stay in the fragment, hidden by its
        // deletion file.
        let some: Vec<String> = (0..9_000).map(|n| format!("r{n:05}")).collect();
        table.delete_rows("id", &some).await.unwrap();
        table.maintain(2).await.unwrap();
        assert_eq!(fragment_rows(&table.rows), vec![(20_000, 9_000)]);
        // Past it the fragment is written again with the rows it still has.
        let more: Vec<String> = (9_000..11_000).map(|n| format!("r{n:05}")).collect();
        table.delete_rows("id", &more).await.unwrap();
        table.maintain(3).await.unwrap();
        assert_eq!(fragment_rows(&table.rows), vec![(9_000, 0)]);
        assert_eq!(titles_of(&table.rows).await.len(), 9_000);
    }

    #[test]
    fn a_version_is_pruned_once_its_successor_is_an_hour_old() {
        let now = 10 * HOUR;
        // v1 made long ago, replaced a moment ago by v2: v1 stays.
        assert_eq!(
            prunable([(1, 0), (2, now - 1_000)].into_iter(), now, HOUR),
            None
        );
        // Once v2 is an hour old, v1 goes: keep from v2.
        assert_eq!(
            prunable([(1, 0), (2, now - HOUR - 1)].into_iter(), now, HOUR),
            Some(2)
        );
        // The newest version whose successor is old enough sets the line.
        assert_eq!(
            prunable(
                [(1, 0), (2, HOUR), (3, 2 * HOUR), (4, now - 10)].into_iter(),
                now,
                HOUR
            ),
            Some(3)
        );
        assert_eq!(prunable([(7, 0)].into_iter(), now, HOUR), None);
    }
}
