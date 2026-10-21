//! `Collection.GetVectors`: the vectors the Index holds in one `vector`
//! column for given Documents, as they were stored.
//!
//! The stream reads one snapshot of the Index, the `commit` its call read,
//! in portions of [`PORTION`] ids, one Lance read each as the stream
//! reaches it: the memory it holds is that of one portion, however many ids
//! it was given. What a `commit` names stays on disk for
//! [`VERSION_LIFETIME`] from the moment before a reader read it (see
//! `generation`'s **Removal**), so a portion asked later than that is
//! `FAILED_PRECONDITION`, and so is a read that fails once that time is
//! over: its files may be gone.
//!
//! A `text` column holds its vector in the rows (`_vector_{name}`, with the
//! digest of its text in `_digest_{name}`, set with the vector and only
//! with it); a `text list` column holds one row per text with a vector in
//! its texts dataset. An id the snapshot holds no vector of is one `absent`
//! item.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use lance::Dataset;
use lance::deps::arrow_array::{Array, RecordBatch, StringArray};
use lance::deps::datafusion::prelude::{Expr, col, lit};
use protocontract::Stream;
use scheda_api::v1::vector_item::Item;
use scheda_api::v1::{StoredVector, VaultErrorCode, VectorItem, VectorSelection};
use tokio::time::Instant;

use crate::error::{Error, Fault};
use crate::gate::Pass;
use crate::ports::task;
use crate::queries::{Reading, Table};
use crate::store::{
    self, DIGEST, POSITION, VECTOR, VERSION_LIFETIME, digest_name, lance_fault, vector_name,
};
use crate::vault::Vault;

/// How many ids one read of the stream takes. A `text` column gives one
/// vector per id, so a portion holds at most this many vectors; a `text
/// list` one vector per text.
const PORTION: usize = 64;

/// What a stream reads, between two portions.
struct Source {
    /// The table of the collection in the snapshot; `None` when the Index
    /// has not reached the collection, or holds no vector in the column:
    /// every id is absent.
    table: Option<Table>,
    column: String,
    /// The column is a `text list`: its vectors are in a texts dataset.
    list: bool,
    /// The dataset the vectors are in, opened at the first portion.
    opened: Option<Dataset>,
    ids: Vec<String>,
    /// The first id not read yet.
    next: usize,
    /// No portion is read from this moment on.
    deadline: Instant,
}

/// The read of one portion, which gives the source back.
type Portion = Pin<Box<dyn Future<Output = (Source, Result<Vec<VectorItem>, Error>)> + Send>>;

/// The stream of `Collection.GetVectors`.
pub(crate) struct Vectors {
    vault: Arc<Vault>,
    /// `None` while a portion is read, and once the stream ended.
    source: Option<Source>,
    ready: VecDeque<VectorItem>,
    pending: Option<Portion>,
}

impl Vectors {
    /// The stream of `selection` over the collection `collection`, in
    /// `reading`, which was read after `began`.
    ///
    /// # Errors
    ///
    /// The codes of [`Reading::collection`]; `UNIMPLEMENTED` for a
    /// collection whose `index_scope` keeps no vectors; `INVALID_ARGUMENT`
    /// for a column the Projector does not mark `vector` for it.
    pub(crate) fn new(
        vault: &Arc<Vault>,
        reading: &Reading,
        collection: &str,
        selection: VectorSelection,
        began: Instant,
    ) -> Result<Vectors, Error> {
        let decl = reading.collection(collection, false)?;
        if !decl.holds_vectors() {
            return Err(Fault::new(
                VaultErrorCode::Unimplemented,
                format!("the index_scope of the collection {collection} keeps no vectors"),
            )
            .into());
        }
        let column = selection.column.unwrap_or_default();
        if !reading.declares_vector(collection, &column) {
            return Err(Fault::new(
                VaultErrorCode::InvalidArgument,
                format!("the collection {collection} has no vector column {column}"),
            )
            .into());
        }
        let table = reading
            .table(collection)
            .filter(|_| reading.vectors(collection).contains(&column));
        let list = table.as_ref().is_some_and(|table| table.is_list(&column));
        Ok(Vectors {
            vault: Arc::clone(vault),
            source: Some(Source {
                table,
                column,
                list,
                opened: None,
                ids: selection.ids,
                next: 0,
                deadline: began + VERSION_LIFETIME,
            }),
            ready: VecDeque::new(),
            pending: None,
        })
    }
}

impl Stream for Vectors {
    type Item = Result<VectorItem, Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(item) = this.ready.pop_front() {
                return Poll::Ready(Some(Ok(item)));
            }
            if this.pending.is_none() {
                let Some(source) = this.source.take() else {
                    return Poll::Ready(None);
                };
                if source.next >= source.ids.len() {
                    return Poll::Ready(None);
                }
                let pass = match Pass::enter(&this.vault) {
                    Ok(pass) => pass,
                    Err(error) => return Poll::Ready(Some(Err(error))),
                };
                this.pending = Some(Box::pin(async move {
                    let mut source = source;
                    let read = source.portion().await;
                    drop(pass);
                    (source, read)
                }));
            }
            let Some(pending) = this.pending.as_mut() else {
                return Poll::Ready(None);
            };
            let Poll::Ready((source, read)) = pending.as_mut().poll(cx) else {
                return Poll::Pending;
            };
            this.pending = None;
            match read {
                Ok(items) => {
                    this.ready.extend(items);
                    this.source = Some(source);
                }
                // The source went with the read: the stream ends after
                // this.
                Err(error) => return Poll::Ready(Some(Err(error))),
            }
        }
    }
}

impl Source {
    /// The items of the next portion of ids, in their order.
    async fn portion(&mut self) -> Result<Vec<VectorItem>, Error> {
        if Instant::now() >= self.deadline {
            return Err(expired().into());
        }
        let end = self.ids.len().min(self.next + PORTION);
        let ids: Vec<String> = self.ids[self.next..end].to_vec();
        let stored = match self.read(&ids).await {
            Ok(stored) => stored,
            Err(error) if Instant::now() >= self.deadline => {
                return Err(expired().with_source(error).into());
            }
            Err(error) => return Err(error),
        };
        self.next = end;
        let mut items = Vec::with_capacity(ids.len());
        for id in ids {
            match stored.get(&id) {
                Some(vectors) => items.extend(vectors.iter().map(|vector| VectorItem {
                    item: Some(Item::Vector(Box::new(vector.clone()))),
                    ..VectorItem::default()
                })),
                None => items.push(VectorItem {
                    item: Some(Item::Absent(id)),
                    ..VectorItem::default()
                }),
            }
        }
        Ok(items)
    }

    /// The vectors the snapshot holds for `ids`, by id, each id's in the
    /// order of its texts.
    async fn read(&mut self, ids: &[String]) -> Result<HashMap<String, Vec<StoredVector>>, Error> {
        let Some(table) = self.table.clone() else {
            return Ok(HashMap::new());
        };
        let dataset = match self.opened.take() {
            Some(dataset) => Some(dataset),
            None if self.list => {
                let column = self.column.clone();
                task(async move { table.open_texts(&column).await }).await?
            }
            None => Some(task(async move { table.open().await }).await?),
        };
        let Some(dataset) = dataset else {
            // No text of the column has a vector yet.
            return Ok(HashMap::new());
        };
        self.opened = Some(dataset.clone());
        let filter = col("id").in_list(ids.iter().map(|id| lit(id.as_str())).collect(), false);
        if self.list {
            task(async move { texts_vectors(&dataset, filter).await }).await
        } else {
            let column = self.column.clone();
            task(async move { row_vectors(&dataset, &column, filter).await }).await
        }
    }
}

/// The vectors of the rows `filter` admits in the rows dataset `rows`, in
/// the `text` column `column`.
async fn row_vectors(
    rows: &Dataset,
    column: &str,
    filter: Expr,
) -> Result<HashMap<String, Vec<StoredVector>>, Error> {
    let vector = vector_name(column);
    let digest = digest_name(column);
    let mut scanner = rows.scan();
    scanner
        .project(&["id", vector.as_str(), digest.as_str()])
        .map_err(lance_fault)?
        .filter_expr(filter.and(col(digest.as_str()).is_not_null()));
    let batch = scanner.try_into_batch().await.map_err(lance_fault)?;
    let ids = strings(&batch, "id")?;
    let digests = strings(&batch, &digest)?;
    let vectors = store::vectors_in(&batch, &vector)?;
    let mut out: HashMap<String, Vec<StoredVector>> = HashMap::new();
    for n in (0..batch.num_rows()).filter(|n| !vectors.is_null(*n)) {
        out.entry(ids.value(n).to_owned()).or_default().push(stored(
            ids.value(n),
            0,
            digests.value(n),
            store::vector_at(vectors, n)?,
        ));
    }
    Ok(out)
}

/// The vectors of the texts `filter` admits in the texts dataset `texts`,
/// each Document's in the order of its texts.
async fn texts_vectors(
    texts: &Dataset,
    filter: Expr,
) -> Result<HashMap<String, Vec<StoredVector>>, Error> {
    let mut scanner = texts.scan();
    scanner
        .project(&["id", POSITION, DIGEST, VECTOR])
        .map_err(lance_fault)?
        .filter_expr(filter);
    let batch = scanner.try_into_batch().await.map_err(lance_fault)?;
    let ids = strings(&batch, "id")?;
    let positions = store::positions_in(&batch)?;
    let digests = strings(&batch, DIGEST)?;
    let vectors = store::vectors_in(&batch, VECTOR)?;
    let mut out: HashMap<String, Vec<StoredVector>> = HashMap::new();
    for n in 0..batch.num_rows() {
        out.entry(ids.value(n).to_owned()).or_default().push(stored(
            ids.value(n),
            positions.value(n),
            digests.value(n),
            store::vector_at(vectors, n)?,
        ));
    }
    for vectors in out.values_mut() {
        vectors.sort_by_key(|vector| vector.position);
    }
    Ok(out)
}

fn stored(id: &str, position: u32, digest: &str, values: Vec<f32>) -> StoredVector {
    StoredVector {
        id: Some(id.to_owned()),
        position: Some(position),
        digest: Some(digest.to_owned()),
        values,
        ..StoredVector::default()
    }
}

/// The string column `name` of a batch of the Index.
fn strings<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray, Fault> {
    batch
        .column_by_name(name)
        .and_then(|array| array.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| {
            Fault::new(
                VaultErrorCode::IndexBroken,
                format!("the column {name} of the Index is missing"),
            )
        })
}

/// The failure of a portion asked once the snapshot's hour is over.
fn expired() -> Fault {
    Fault::new(
        VaultErrorCode::FailedPrecondition,
        "the snapshot of the Index this stream reads is older than an hour and may be gone: \
         ask again for the ids not given yet",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(deadline: Instant) -> Source {
        Source {
            table: None,
            column: String::from("topic"),
            list: false,
            opened: None,
            ids: vec![String::from("a"), String::from("b")],
            next: 0,
            deadline,
        }
    }

    #[tokio::test]
    async fn a_portion_asked_once_the_snapshots_hour_is_over_is_a_failed_precondition() {
        let mut fresh = source(Instant::now() + VERSION_LIFETIME);
        let items = fresh.portion().await.unwrap();
        assert_eq!(
            items.len(),
            2,
            "an Index that has not reached the collection"
        );
        assert!(
            items
                .iter()
                .all(|item| matches!(item.item, Some(Item::Absent(_))))
        );
        let mut late = source(Instant::now());
        let error = late.portion().await.unwrap_err();
        assert!(error.is(VaultErrorCode::FailedPrecondition), "{error}");
        assert_eq!(late.next, 0, "no id is given as read");
    }
}

#[cfg(test)]
mod memory {
    use std::num::NonZeroUsize;
    use std::path::PathBuf;

    use buffa_types::google::protobuf::Empty;
    use scheda_api::v1::embedder_ref::EmbedderRef;
    use scheda_api::v1::projector_ref::ProjectorRef;
    use scheda_api::v1::{
        CollectionKey, ColumnKind, DocumentOperation, IndexColumn, IndexDoc, IndexSchema,
        IndexValue, OpenOptions, ProjectorDescriptor, ProjectorSync, RawDocument,
        document_operation::Operation, index_value,
    };

    use super::*;
    use crate::ports::{FixedEmbedder, Ports};
    use crate::testing::{batch, create_collection, measured, open_with, upsert_of, wait_indexed};

    const DOCUMENTS: usize = 10_000;
    const DIMENSIONS: usize = 256;

    /// What two reads that hold one portion at a time may differ by.
    const MARGIN: usize = 2 * 1024 * 1024;

    /// The vector of `text`, the same each time.
    fn vector_of(text: &str) -> Vec<f32> {
        let mut state = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        }) | 1;
        (0..DIMENSIONS)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state % 1000) as f32 / 1000.0
            })
            .collect()
    }

    /// A Projector whose one column, `body`, a `vector` one, is the text
    /// of the Document.
    struct Bodies;

    impl ProjectorSync for Bodies {
        fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
            Ok(ProjectorDescriptor::default().with_projection_id("bodies@1"))
        }

        fn columns(&self, _collection: CollectionKey) -> Result<IndexSchema, Error> {
            Ok(IndexSchema {
                columns: vec![
                    IndexColumn::default()
                        .with_name("body")
                        .with_kind(ColumnKind::COLUMN_KIND_TEXT)
                        .with_vector(true),
                ],
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            let text = String::from_utf8(document.raw.unwrap_or_default()).unwrap_or_default();
            let mut doc = IndexDoc::default();
            doc.values.insert(
                String::from("body"),
                IndexValue {
                    value: Some(index_value::Value::Text(text)),
                    ..IndexValue::default()
                },
            );
            Ok(doc)
        }
    }

    fn text(n: usize) -> String {
        format!("the text of Document {n}")
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "measures the memory of the process: run alone, by the memory target"]
    async fn reading_ten_thousand_stored_vectors_holds_what_reading_a_thousand_holds() {
        let _alone = crate::testing::alone().await;
        crate::git::isolate_test_git_env();
        let base = std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let dir = base.join(format!("scheda-vectors-memory-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::testing::remove_when_the_test_ends(&dir);
        let ports = || Ports {
            projector: Some(ProjectorRef::from_sync(Bodies)),
            embedder: Some(EmbedderRef::from_sync(FixedEmbedder::new(
                "bodies@1",
                DIMENSIONS as u32,
                NonZeroUsize::new(256).unwrap(),
                |texts: Vec<String>| Ok(texts.iter().map(|text| vector_of(text)).collect()),
            ))),
            reranker: None,
        };
        let options = OpenOptions::default()
            .with_index_poll_ms(10)
            .with_write_timeout_ms(60_000);
        let vault = open_with(&dir, options.clone(), ports()).await.unwrap();
        create_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        let operations: Vec<DocumentOperation> = (0..DOCUMENTS)
            .map(|n| DocumentOperation {
                operation: Some(Operation::Upsert(Box::new(upsert_of(
                    &format!("d{n:05}"),
                    text(n).as_bytes(),
                )))),
                ..DocumentOperation::default()
            })
            .collect();
        batch(&vault, "notes", operations).await.unwrap();
        wait_indexed(&vault).await;
        // Opened again, its indexer finds the Index fresh and waits: what it
        // frees after a round would hide what the read holds.
        vault.close().await.unwrap();
        let vault = open_with(&dir, options.with_index_poll_ms(600_000), ports())
            .await
            .unwrap();
        wait_indexed(&vault).await;

        let pass = Pass::enter(&vault).unwrap();
        let reading = Reading::of(&pass).await.unwrap();
        let mut peaks = Vec::new();
        for count in [1_000, DOCUMENTS] {
            let selection = VectorSelection {
                ids: (0..count).map(|n| format!("d{n:05}")).collect(),
                ..VectorSelection::default().with_column("body")
            };
            let mut stream =
                Vectors::new(&vault, &reading, "notes", selection, Instant::now()).unwrap();
            let (read, memory) = measured(async {
                let mut read = 0;
                while let Some(item) =
                    std::future::poll_fn(|cx| Pin::new(&mut stream).poll_next(cx)).await
                {
                    let Some(Item::Vector(vector)) = item.unwrap().item else {
                        panic!("Document {read} has its vector");
                    };
                    assert_eq!(vector.id.as_deref(), Some(format!("d{read:05}").as_str()));
                    assert_eq!(vector.values, vector_of(&text(read)), "Document {read}");
                    read += 1;
                }
                read
            })
            .await;
            assert_eq!(read, count, "every vector, in the order of the ids");
            eprintln!(
                "reading {count} stored vectors: peak {} KB, allocated {} KB",
                memory.peak / 1024,
                memory.allocated / 1024
            );
            peaks.push(memory.peak);
        }
        assert!(peaks[1] < peaks[0] + MARGIN, "{peaks:?}");
        drop(pass);
        vault.close().await.unwrap();
    }
}
