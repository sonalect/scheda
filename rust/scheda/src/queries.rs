//! Queries and counts over the Index on Lance: `Collection.Query`,
//! `Collection.Count`, `Vault.QueryDocuments`, `Vault.CountDocuments`.
//!
//! A call reads one snapshot of the Index: the live generation's `commit`,
//! and each table at the version it names. It counts the matches and reads
//! the keys of its page there, at the start of the call; the Documents of
//! the page are then read from git, at the state that snapshot reflects,
//! one per item of the stream. A query selects: the exact set its `Filter`
//! matches, in a stable order (its keys, then the collection name, then `id`,
//! a journal in append order), without scores.
//!
//! A page that begins with `start_after` is a condition on the order, so a
//! deep page costs what the first one does and a Document added meanwhile
//! does not shift it.

use std::cmp::Ordering;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use futures_util::{StreamExt, stream};
use lance::Dataset;
use lance::dataset::scanner::ColumnOrdering;
use lance::deps::arrow_array::{
    Array, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray,
};
use lance::deps::datafusion::prelude::{Expr, col, lit};
use scheda_api::v1::query_item::Item as QueryArm;
use scheda_api::v1::{
    Document, DocumentKey, DocumentQuery, Filter, OrderBy, QueryItem, QuerySummary, VaultErrorCode,
    VaultQuery,
};
use scheda_blocking::Blocking;

use crate::collection::{Declaration, Declarations, Kind as CollectionKind};
use crate::documents;
use crate::error::{Error, Fault};
use crate::filter::{self, all, any};
use crate::gate::Pass;
use crate::generation::{Commit, Generation, Layout};
use crate::index::{Index, State};
use crate::journal::SegmentMemo;
use crate::ports::task;
use crate::repo::{Repo, Snap};
use crate::store::{Column, Kind, lance_fault, texts_path};
use crate::tables::{self, Calls, Plan};
use crate::vault::Vault;

/// How many collections of one Vault-wide query or search the Index reads at
/// once. Measured on 8 collections of 2000 and 5000 Documents (multi-thread
/// runtime): 4 at once took a third to two fifths of the time of one at a
/// time, 8 at once added nothing to a search. It also bounds the scans, and
/// the memory they hold, that one call opens, whatever the core count.
pub(crate) const TABLES_AT_ONCE: usize = 4;

/// The filter depth when `OpenOptions.filter_max_depth` is unset.
const DEFAULT_FILTER_DEPTH: u32 = 32;

fn invalid(message: impl Into<String>) -> Error {
    Fault::new(VaultErrorCode::InvalidArgument, message).into()
}

/// One reading of the Index for a call: the live generation, its commit,
/// the plan of this Vault (the Projector's columns), and the collections
/// declared now.
pub(crate) struct Reading {
    generation: Generation,
    commit: Commit,
    plan: Plan,
    declarations: Arc<Declarations>,
}

impl Reading {
    /// The Index as the Vault of `pass` reads it now.
    ///
    /// # Errors
    ///
    /// `INDEX_REBUILDING` before the first build of the Index ended;
    /// `INDEX_STALE` when the Index was built with other ports than this
    /// Vault's; the failure of git, of the Index, or of the Projector.
    pub(crate) async fn of(pass: &Pass) -> Result<Reading, Error> {
        let root = Index::new(pass.vault().path()).root().to_path_buf();
        let (head, declarations, live) = pass
            .step(move |cx, vault| {
                let (head, declarations) = vault.declarations_at_head()?;
                let live = match Layout::new(&root).current(cx)? {
                    Some(generation) => {
                        let commit = generation.read_commit(cx)?;
                        commit.map(|commit| (generation, commit))
                    }
                    None => None,
                };
                Ok((head, declarations, live))
            })
            .await?;
        let Some((generation, commit)) = live else {
            return Err(Fault::new(
                VaultErrorCode::IndexRebuilding,
                "the Index has not been built yet: its first build runs",
            )
            .into());
        };
        let calls = Calls::of(pass.vault().ports());
        // A Vault that lacks the embedder its `vector` columns need does
        // not index, and reads the Index all the same.
        let plan = tables::plan(&calls, head, Arc::clone(&declarations)).await?;
        if let Some(reason) = tables::stale(&commit, &plan) {
            return Err(Fault::new(VaultErrorCode::IndexStale, reason).into());
        }
        Ok(Reading {
            generation,
            commit,
            plan,
            declarations,
        })
    }

    /// The state of the Vault the Index reflects.
    pub(crate) fn state(&self) -> State {
        self.commit.state().unwrap_or_default()
    }

    /// The declaration of `name`, which a query or a search may read.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` for a collection that is not declared; `UNIMPLEMENTED`
    /// for an encrypted one, which no Index holds, and, when `filtered`,
    /// for one `INDEX_SCOPE_IDS`, whose columns the Index does not hold.
    pub(crate) fn collection(&self, name: &str, filtered: bool) -> Result<Declaration, Error> {
        let Some(decl) = self.declarations.get(name) else {
            return Err(Fault::new(
                VaultErrorCode::NotFound,
                format!("the collection {name} is not declared"),
            )
            .into());
        };
        if decl.encrypted() {
            return Err(Fault::new(
                VaultErrorCode::Unimplemented,
                format!("the collection {name} is encrypted: no Index holds it"),
            )
            .into());
        }
        if filtered && !decl.holds_columns() {
            return Err(Fault::new(
                VaultErrorCode::Unimplemented,
                format!(
                    "the collection {name} is INDEX_SCOPE_IDS: the Index holds none of its columns"
                ),
            )
            .into());
        }
        Ok(decl.clone())
    }

    /// The collections of a call of the Vault: the named ones, or every
    /// collection that is neither isolated nor encrypted.
    pub(crate) fn scope(
        &self,
        names: &[String],
        filtered: bool,
    ) -> Result<Vec<Declaration>, Error> {
        if names.is_empty() {
            return Ok(self
                .declarations
                .iter()
                .filter(|decl| !decl.isolated() && !decl.encrypted())
                .filter(|decl| !filtered || decl.holds_columns())
                .cloned()
                .collect());
        }
        names
            .iter()
            .map(|name| self.collection(name, filtered))
            .collect()
    }

    /// The Projector's columns of `collection`, as this Vault sees them.
    pub(crate) fn columns(&self, collection: &str) -> &[Column] {
        self.plan
            .shapes
            .get(collection)
            .map_or(&[], |shape| shape.columns.as_slice())
    }

    /// The `vector` columns of `collection` a search can use: those the
    /// Vault's declaration keeps (none unless the collection is
    /// `INDEX_SCOPE_ALL`) that the Index's table holds. A table not
    /// rebuilt yet after the scope moved holds other vectors than the
    /// declaration gives; a search follows both. A collection the Index
    /// holds no table of yet has the declaration's.
    pub(crate) fn vectors(&self, collection: &str) -> Vec<String> {
        let held = self.commit.tables.get(collection);
        self.plan
            .shapes
            .get(collection)
            .map_or(&[][..], |shape| shape.vectors.as_slice())
            .iter()
            .filter(|name| held.is_none_or(|table| table.shape.vectors.contains(name)))
            .cloned()
            .collect()
    }

    /// Whether the Vault's declaration of `collection` keeps vectors in its
    /// column `name`: the Projector marks it `vector`, and the collection is
    /// `INDEX_SCOPE_ALL`.
    pub(crate) fn declares_vector(&self, collection: &str, name: &str) -> bool {
        self.plan
            .shapes
            .get(collection)
            .is_some_and(|shape| shape.vectors.iter().any(|vector| vector == name))
    }

    /// The length of the vectors the Index's table of `collection` holds,
    /// when it holds a `vector` column.
    pub(crate) fn dimensions(&self, collection: &str) -> Option<u32> {
        self.commit
            .tables
            .get(collection)
            .and_then(|table| table.shape.dimensions)
    }

    /// The `fts` columns of `collection` a search can use: those the
    /// Vault's declaration keeps (none when the collection is
    /// `INDEX_SCOPE_IDS`) that the Index's table holds.
    pub(crate) fn fts_columns(&self, collection: &str) -> Vec<String> {
        let Some(held) = self.commit.tables.get(collection) else {
            return Vec::new();
        };
        self.columns(collection)
            .iter()
            .filter(|column| column.fts && held.shape.columns.iter().any(|c| c.name == column.name))
            .map(|column| column.name.clone())
            .collect()
    }

    /// The table of `collection` in the snapshot, when the Index holds one.
    pub(crate) fn table(&self, collection: &str) -> Option<Table> {
        let dir = self.generation.dir();
        self.commit.tables.get(collection).map(|table| Table {
            path: dir.join(crate::store::rows_name(&table.name)),
            version: table.versions.rows,
            columns: table.shape.columns.clone(),
            texts: table
                .versions
                .texts
                .iter()
                .filter_map(|(column, version)| {
                    let number = table.texts.get(column)?;
                    Some((
                        column.clone(),
                        (texts_path(dir, &table.name, *number), *version),
                    ))
                })
                .collect(),
        })
    }
}

/// A table to read: its rows dataset, and the texts dataset of each `text
/// list` vector column, at the versions a commit names.
#[derive(Clone, Debug)]
pub(crate) struct Table {
    path: std::path::PathBuf,
    version: u64,
    columns: Vec<Column>,
    texts: BTreeMap<String, (std::path::PathBuf, u64)>,
}

impl Table {
    /// The rows dataset at its version.
    pub(crate) async fn open(&self) -> Result<Dataset, Error> {
        open_at(&self.path, self.version).await
    }

    /// The columns its rows hold.
    pub(crate) fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// Whether the `vector` column `name` is a `text list`, whose vectors
    /// a texts dataset holds.
    pub(crate) fn is_list(&self, name: &str) -> bool {
        self.columns
            .iter()
            .any(|column| column.name == name && column.kind == Kind::TextList)
    }

    /// The texts dataset of the `text list` vector column `name` at its
    /// version; `None` when the table has none (no text embedded yet).
    pub(crate) async fn open_texts(&self, name: &str) -> Result<Option<Dataset>, Error> {
        match self.texts.get(name) {
            Some((path, version)) => Ok(Some(open_at(path, *version).await?)),
            None => Ok(None),
        }
    }
}

/// The dataset at `path` at `version`.
async fn open_at(path: &std::path::Path, version: u64) -> Result<Dataset, Error> {
    let uri = path.to_str().ok_or_else(|| {
        Error::from(Fault::new(
            VaultErrorCode::Storage,
            "the path of the Index is not valid UTF-8",
        ))
    })?;
    let latest = Dataset::open(uri).await.map_err(lance_fault)?;
    if latest.version().version == version {
        return Ok(latest);
    }
    Ok(latest
        .checkout_version(version)
        .await
        .map_err(lance_fault)?)
}

/// One key of an order: the column, and whether largest first.
#[derive(Clone, Debug)]
struct Key {
    column: String,
    descending: bool,
}

/// A value of a key; a Document without one comes after every Document
/// with one, whatever the direction.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Null,
    Text(String),
    Integer(i64),
    Number(f64),
    Boolean(bool),
}

impl Value {
    fn literal(&self) -> Option<Expr> {
        match self {
            Value::Null => None,
            Value::Text(text) => Some(lit(text.clone())),
            Value::Integer(n) => Some(lit(*n)),
            Value::Number(n) => Some(lit(*n)),
            Value::Boolean(b) => Some(lit(*b)),
        }
    }

    /// The order of two values of one key.
    fn order(&self, other: &Value, descending: bool) -> Ordering {
        let plain = match (self, other) {
            (Value::Null, Value::Null) => return Ordering::Equal,
            (Value::Null, _) => return Ordering::Greater,
            (_, Value::Null) => return Ordering::Less,
            (Value::Text(a), Value::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
            (Value::Integer(a), Value::Integer(b)) => a.cmp(b),
            (Value::Number(a), Value::Number(b)) => a.total_cmp(b),
            (Value::Boolean(a), Value::Boolean(b)) => a.cmp(b),
            _ => Ordering::Equal,
        };
        if descending { plain.reverse() } else { plain }
    }
}

/// One match of a query: where the Document is, and its keys.
#[derive(Clone, Debug)]
pub(crate) struct Hit {
    pub(crate) collection: String,
    pub(crate) id: String,
    values: Vec<Value>,
    /// `id`, or a journal line's place in append order.
    tie: String,
}

/// The column that breaks ties last in a collection: `order` for a journal,
/// whose default order is append order, `id` otherwise.
fn tie_column(decl: &Declaration) -> &'static str {
    if decl.kind() == CollectionKind::Journal {
        "order"
    } else {
        "id"
    }
}

/// The keys of `order_by`, checked against the collections of `scope`.
fn keys(order_by: &[OrderBy], scope: &[(&Declaration, &[Column])]) -> Result<Vec<Key>, Error> {
    let mut keys = Vec::new();
    for order in order_by {
        let column = order.column.clone().unwrap_or_default();
        if column == "id" {
            keys.push(Key {
                column,
                descending: order.descending.unwrap_or(false),
            });
            continue;
        }
        if column == "order" {
            if scope
                .iter()
                .any(|(decl, _)| decl.kind() != CollectionKind::Journal)
            {
                return Err(invalid(
                    "order names the append order of a journal; a collection in scope is not one",
                ));
            }
            keys.push(Key {
                column,
                descending: order.descending.unwrap_or(false),
            });
            continue;
        }
        let mut kind = None;
        for (_, columns) in scope {
            if let Some(found) = columns.iter().find(|c| c.name == column) {
                if found.kind == Kind::TextList {
                    return Err(invalid(format!(
                        "the column {column} is a text list: it has no order"
                    )));
                }
                if kind.is_some_and(|kind| kind != found.kind) {
                    return Err(invalid(format!(
                        "the column {column} is of one kind in one collection and of another in a second"
                    )));
                }
                kind = Some(found.kind);
            }
        }
        if kind.is_none() {
            return Err(invalid(format!(
                "the column {column} is not declared by any collection in scope"
            )));
        }
        keys.push(Key {
            column,
            descending: order.descending.unwrap_or(false),
        });
    }
    Ok(keys)
}

/// The value at row `n` of the column `name` of `batch`; `Null` when the
/// batch lacks the column.
fn value_at(batch: &RecordBatch, name: &str, n: usize) -> Value {
    let Some(array) = batch.column_by_name(name) else {
        return Value::Null;
    };
    if array.is_null(n) {
        return Value::Null;
    }
    let any = array.as_any();
    if let Some(texts) = any.downcast_ref::<StringArray>() {
        Value::Text(texts.value(n).to_owned())
    } else if let Some(numbers) = any.downcast_ref::<Int64Array>() {
        Value::Integer(numbers.value(n))
    } else if let Some(numbers) = any.downcast_ref::<Float64Array>() {
        Value::Number(numbers.value(n))
    } else if let Some(flags) = any.downcast_ref::<BooleanArray>() {
        Value::Boolean(flags.value(n))
    } else {
        Value::Null
    }
}

/// Where a page begins: after the Document whose keys these are.
#[derive(Clone, Debug)]
struct After {
    values: Vec<Value>,
    collection: String,
    tie: String,
}

/// The condition that a row of `collection` (whose last tie column is
/// `tie`, and that holds the columns `present`) comes after `after` in the
/// order of `keys`, then the collection name, then the tie.
fn after_expr(
    keys: &[Key],
    after: &After,
    collection: &str,
    tie: &str,
    present: &[String],
) -> Expr {
    let mut disjuncts = Vec::new();
    let mut equal_so_far: Vec<Expr> = Vec::new();
    for (key, value) in keys.iter().zip(&after.values) {
        let has = key.column == "id" || key.column == "order" || present.contains(&key.column);
        if !has {
            // Every row of this collection has no value here.
            match value {
                Value::Null => continue,
                _ => {
                    disjuncts.push(all(equal_so_far.iter().cloned()));
                    return any(disjuncts.into_iter());
                }
            }
        }
        let column = col(key.column.as_str());
        let strictly = match value.literal() {
            None => lit(false),
            Some(literal) => {
                let past = if key.descending {
                    column.clone().lt(literal)
                } else {
                    column.clone().gt(literal)
                };
                past.or(column.clone().is_null())
            }
        };
        let mut parts = equal_so_far.clone();
        parts.push(strictly);
        disjuncts.push(all(parts.into_iter()));
        equal_so_far.push(match value.literal() {
            None => column.is_null(),
            Some(literal) => column.eq(literal),
        });
    }
    match collection.cmp(after.collection.as_str()) {
        Ordering::Greater => disjuncts.push(all(equal_so_far.into_iter())),
        Ordering::Equal => {
            equal_so_far.push(col(tie).gt(lit(after.tie.clone())));
            disjuncts.push(all(equal_so_far.into_iter()));
        }
        Ordering::Less => {}
    }
    any(disjuncts.into_iter())
}

/// What one collection gave a query.
struct Part {
    total: u64,
    hits: Vec<Hit>,
}

/// What to read of one collection.
struct Ask {
    table: Table,
    collection: String,
    filter: Option<Filter>,
    after: Option<After>,
    keys: Vec<Key>,
    /// The column that breaks ties last.
    tie: &'static str,
    /// Matches passed over first.
    skip: Option<u64>,
    /// Matches read at most; `Some(0)`: count only.
    limit: Option<u64>,
}

/// Reads the matches of one collection: their count, and the keys of up to
/// `limit` of them after `skip`, in the order of `keys` and then `tie`.
async fn read_part(ask: Ask) -> Result<Part, Error> {
    let Ask {
        table,
        collection,
        filter,
        after,
        keys,
        tie,
        skip,
        limit,
    } = ask;
    task(async move {
        let dataset = table.open().await?;
        let base = filter.as_ref().map_or_else(
            || lit(true),
            |filter| filter::compile(filter, table.columns()),
        );
        let mut counting = dataset.scan();
        counting.filter_expr(base.clone());
        let total = counting.count_rows().await.map_err(lance_fault)?;
        if limit == Some(0) {
            return Ok(Part {
                total,
                hits: Vec::new(),
            });
        }
        let present: Vec<String> = table.columns().iter().map(|c| c.name.clone()).collect();
        let condition = match &after {
            Some(after) => base.and(after_expr(&keys, after, &collection, tie, &present)),
            None => base,
        };
        let mut scanner = dataset.scan();
        let mut projection = vec![String::from("id"), String::from("order")];
        let mut ordering = Vec::new();
        for key in &keys {
            let known =
                key.column == "id" || key.column == "order" || present.contains(&key.column);
            if !known {
                continue;
            }
            if !projection.contains(&key.column) {
                projection.push(key.column.clone());
            }
            ordering.push(ColumnOrdering {
                ascending: !key.descending,
                nulls_first: false,
                column_name: key.column.clone(),
            });
        }
        ordering.push(ColumnOrdering {
            ascending: true,
            nulls_first: false,
            column_name: tie.to_owned(),
        });
        scanner
            .project(&projection)
            .map_err(lance_fault)?
            .filter_expr(condition)
            .order_by(Some(ordering))
            .map_err(lance_fault)?;
        if limit.is_some() || skip.is_some() {
            scanner
                .limit(
                    limit.map(|n| i64::try_from(n).unwrap_or(i64::MAX)),
                    skip.map(|n| i64::try_from(n).unwrap_or(i64::MAX)),
                )
                .map_err(lance_fault)?;
        }
        let batch = scanner.try_into_batch().await.map_err(lance_fault)?;
        let mut hits = Vec::with_capacity(batch.num_rows());
        for n in 0..batch.num_rows() {
            let Value::Text(id) = value_at(&batch, "id", n) else {
                continue;
            };
            let tie_value = match value_at(&batch, tie, n) {
                Value::Text(text) => text,
                _ => id.clone(),
            };
            hits.push(Hit {
                collection: collection.clone(),
                id,
                values: keys
                    .iter()
                    .map(|key| value_at(&batch, &key.column, n))
                    .collect(),
                tie: tie_value,
            });
        }
        Ok(Part { total, hits })
    })
    .await
}

/// The keys of the Document `id` in the table of `collection`: where a page
/// that starts after it begins.
///
/// # Errors
///
/// `FAILED_PRECONDITION` when the snapshot does not hold it.
async fn find_after(
    table: Table,
    collection: String,
    id: String,
    keys: Vec<Key>,
    tie: &'static str,
) -> Result<After, Error> {
    task(async move {
        let dataset = table.open().await?;
        let mut scanner = dataset.scan();
        scanner.filter_expr(col("id").eq(lit(id.clone())));
        let batch = scanner.try_into_batch().await.map_err(lance_fault)?;
        if batch.num_rows() == 0 {
            return Err(Fault::new(
                VaultErrorCode::FailedPrecondition,
                format!(
                    "the Document {id} the page starts after is no longer in the Index; start again"
                ),
            )
            .into());
        }
        let tie_value = match value_at(&batch, tie, 0) {
            Value::Text(text) => text,
            _ => id.clone(),
        };
        Ok(After {
            values: keys
                .iter()
                .map(|key| value_at(&batch, &key.column, 0))
                .collect(),
            collection,
            tie: tie_value,
        })
    })
    .await
}

/// The order of two hits of a Vault query: the keys, the collection name,
/// then the tie.
fn hit_order(keys: &[Key], a: &Hit, b: &Hit) -> Ordering {
    for (n, key) in keys.iter().enumerate() {
        let order = a.values[n].order(&b.values[n], key.descending);
        if order != Ordering::Equal {
            return order;
        }
    }
    a.collection
        .cmp(&b.collection)
        .then_with(|| a.tie.as_bytes().cmp(b.tie.as_bytes()))
}

/// What a query found: the total, and the page's hits in order.
pub(crate) struct Found {
    pub(crate) total: u64,
    pub(crate) hits: Vec<Hit>,
}

/// The filter depth the Vault allows.
pub(crate) fn max_depth(vault: &Vault) -> u32 {
    vault.filter_max_depth().unwrap_or(DEFAULT_FILTER_DEPTH)
}

/// `Collection.Query` and `Collection.Count` without the Documents: the
/// total, and the keys of the page.
///
/// # Errors
///
/// `INVALID_ARGUMENT` for a request that breaks a rule; `NOT_FOUND`,
/// `UNIMPLEMENTED`, `INDEX_REBUILDING`, `INDEX_STALE` as [`Reading`] says;
/// `FAILED_PRECONDITION` for a `start_after` the snapshot does not hold.
pub(crate) async fn find_in_collection(
    pass: &Pass,
    reading: &Reading,
    collection: &str,
    query: &DocumentQuery,
    page: bool,
) -> Result<Found, Error> {
    let filter = query.filter.as_option().cloned();
    let decl = reading.collection(collection, filter.is_some() || !query.order_by.is_empty())?;
    let columns = reading.columns(collection);
    if let Some(filter) = &filter {
        filter::check(filter, &[columns], max_depth(pass.vault()))?;
    }
    let keys = keys(&query.order_by, &[(&decl, columns)])?;
    let Some(table) = reading.table(collection) else {
        // The Index has not reached the collection yet.
        return Ok(Found {
            total: 0,
            hits: Vec::new(),
        });
    };
    let tie = tie_column(&decl);
    let after = match &query.start_after {
        None => None,
        Some(id) if keys.is_empty() && tie == "id" => Some(After {
            values: Vec::new(),
            collection: collection.to_owned(),
            tie: id.clone(),
        }),
        Some(id) => Some(
            find_after(
                table.clone(),
                collection.to_owned(),
                id.clone(),
                keys.clone(),
                tie,
            )
            .await?,
        ),
    };
    let part = read_part(Ask {
        table,
        collection: collection.to_owned(),
        filter,
        after,
        keys,
        tie,
        skip: page.then(|| query.offset.map(u64::from)).flatten(),
        limit: if page {
            query.limit.map(u64::from)
        } else {
            Some(0)
        },
    })
    .await?;
    Ok(Found {
        total: part.total,
        hits: part.hits,
    })
}

/// `Vault.QueryDocuments` and `Vault.CountDocuments` without the
/// Documents.
///
/// # Errors
///
/// As [`find_in_collection`]; a column of one kind in one collection and
/// of another in a second in the order is `INVALID_ARGUMENT`.
pub(crate) async fn find_in_vault(
    pass: &Pass,
    reading: &Reading,
    query: &VaultQuery,
    page: bool,
) -> Result<Found, Error> {
    let filter = query.filter.as_option().cloned();
    let names = query
        .collections
        .as_option()
        .map(|names| names.names.clone())
        .unwrap_or_default();
    let filtered = filter.is_some() || !query.order_by.is_empty();
    let scope = reading.scope(&names, filtered)?;
    let with_columns: Vec<(&Declaration, &[Column])> = scope
        .iter()
        .map(|decl| (decl, reading.columns(decl.name())))
        .collect();
    if let Some(filter) = &filter {
        let columns: Vec<&[Column]> = with_columns.iter().map(|(_, columns)| *columns).collect();
        filter::check(filter, &columns, max_depth(pass.vault()))?;
    }
    let keys = keys(&query.order_by, &with_columns)?;
    let after = match query.start_after.as_option() {
        None => None,
        Some(DocumentKey {
            collection: Some(collection),
            id: Some(id),
            ..
        }) => {
            let decl = reading.collection(collection, false)?;
            let Some(table) = reading.table(collection) else {
                return Err(Fault::new(
                    VaultErrorCode::FailedPrecondition,
                    format!("the Document {id} the page starts after is no longer in the Index; start again"),
                )
                .into());
            };
            Some(
                find_after(
                    table,
                    collection.clone(),
                    id.clone(),
                    keys.clone(),
                    tie_column(&decl),
                )
                .await?,
            )
        }
        Some(_) => return Err(invalid("start_after names a collection and an id")),
    };
    let reach = match (page, query.limit) {
        (false, _) => Some(0),
        (true, Some(limit)) => Some(u64::from(limit) + u64::from(query.offset.unwrap_or(0))),
        (true, None) => None,
    };
    let mut asks = Vec::new();
    for decl in &scope {
        let Some(table) = reading.table(decl.name()) else {
            continue;
        };
        asks.push(Ask {
            table,
            collection: decl.name().to_owned(),
            filter: filter.clone(),
            after: after.clone(),
            keys: keys.clone(),
            tie: tie_column(decl),
            skip: None,
            limit: reach,
        });
    }
    // `buffered` yields in the order of `asks`, whichever table ends first,
    // so the merge below does not depend on completion order.
    let parts: Vec<Result<Part, Error>> = stream::iter(asks)
        .map(read_part)
        .buffered(TABLES_AT_ONCE)
        .collect()
        .await;
    let mut total = 0;
    let mut hits = Vec::new();
    for part in parts {
        let part = part?;
        total += part.total;
        hits.extend(part.hits);
    }
    hits.sort_by(|a, b| hit_order(&keys, a, b));
    let skip = usize::try_from(query.offset.unwrap_or(0)).unwrap_or(usize::MAX);
    let hits: Vec<Hit> = match query.limit {
        Some(limit) => hits
            .into_iter()
            .skip(skip)
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .collect(),
        None => hits.into_iter().skip(skip).collect(),
    };
    Ok(Found {
        total,
        hits: if page { hits } else { Vec::new() },
    })
}

/// The items a query streams: the summary, then each Document of the page,
/// read from git at the state the Index reflects.
pub(crate) struct Page {
    summary: Option<QueryItem>,
    hits: VecDeque<Hit>,
    state: State,
    include_raw: bool,
    declarations: BTreeMap<String, Declaration>,
    reader: Reader,
}

impl Page {
    /// The stream of `found`, read in `reading`.
    pub(crate) fn new(found: Found, reading: &Reading, include_raw: bool) -> Page {
        let declarations = found
            .hits
            .iter()
            .filter_map(|hit| {
                reading
                    .declarations
                    .get(&hit.collection)
                    .map(|decl| (hit.collection.clone(), decl.clone()))
            })
            .collect();
        Page {
            summary: Some(QueryItem {
                item: Some(QueryArm::Summary(Box::new(
                    QuerySummary::default().with_total(found.total),
                ))),
                ..QueryItem::default()
            }),
            hits: found.hits.into(),
            state: reading.state(),
            include_raw,
            declarations,
            reader: Reader::default(),
        }
    }

    /// The next item: the summary, then one Document per call.
    pub(crate) fn next(
        &mut self,
        cx: &Blocking,
        vault: &Vault,
    ) -> Option<Result<QueryItem, Error>> {
        if let Some(summary) = self.summary.take() {
            return Some(Ok(summary));
        }
        loop {
            let hit = self.hits.pop_front()?;
            let Some(decl) = self.declarations.get(&hit.collection) else {
                continue;
            };
            return match read_document(
                cx,
                vault,
                decl,
                self.state,
                &hit.id,
                self.include_raw,
                &mut self.reader,
            ) {
                Ok(Some(document)) => Some(Ok(QueryItem {
                    item: Some(QueryArm::Document(Box::new(document))),
                    ..QueryItem::default()
                })),
                // The Index holds a Document the state it reflects lacks:
                // nothing to give.
                Ok(None) => continue,
                Err(error) => {
                    self.hits.clear();
                    Some(Err(error.context("read a Document of the page")))
                }
            };
        }
    }
}

/// What a reader of many Documents of one page keeps between them: the
/// snapshot of each repository it read at, and the last journal segment it
/// read (see [`SegmentMemo`]). A page of lines of one segment takes one
/// snapshot and reads the segment once, not once per line.
#[derive(Debug, Default)]
pub(crate) struct Reader {
    snaps: Vec<Snap>,
    segment: SegmentMemo,
}

/// The snapshot of `repo` in `snaps`, taken once: at the commit `state`
/// names, or the state of `repo` now when the commit left it.
fn snap_of<'a>(
    snaps: &'a mut Vec<Snap>,
    vault: &Vault,
    repo: Repo,
    state: State,
) -> Result<&'a Snap, Fault> {
    if !snaps.iter().any(|snap| snap.repo() == repo) {
        let snap = match vault.repos().snapshot_at(repo, state.oid(repo))? {
            Some(snap) => snap,
            None => vault.repos().snapshot(repo)?,
        };
        snaps.push(snap);
    }
    snaps
        .iter()
        .find(|snap| snap.repo() == repo)
        .ok_or_else(|| Fault::new(VaultErrorCode::Storage, "the snapshot of a page is gone"))
}

/// The Document `id` of `decl` at `state`: its keys and revision, and its
/// bytes with `include_raw`. `None` when that state does not hold it.
/// `reader` is the one of the page the call belongs to; every call of a
/// page passes the same `state` and the same `reader`.
pub(crate) fn read_document(
    _cx: &Blocking,
    vault: &Vault,
    decl: &Declaration,
    state: State,
    id: &str,
    include_raw: bool,
    reader: &mut Reader,
) -> Result<Option<Document>, Error> {
    let Reader { snaps, segment } = reader;
    let snap = snap_of(snaps, vault, decl.repo(), state)?;
    let mut document = Document::default()
        .with_collection(decl.name().to_owned())
        .with_id(id.to_owned());
    if decl.kind() == CollectionKind::Journal {
        if include_raw {
            let Some(line) = vault.id_maps().find_with(snap, decl, id, segment)? else {
                return Ok(None);
            };
            document.raw = Some(line.payload);
        }
        return Ok(Some(document));
    }
    if include_raw {
        let Some(found) = documents::read_found(snap, decl, id, vault.encryption())? else {
            return Ok(None);
        };
        document.revision = Some(found.revision());
        document.raw = Some(found.into_plain());
        return Ok(Some(document));
    }
    documents::path_of(decl, id)?;
    let Some(node) = documents::node_of(snap, decl, id)? else {
        return Ok(None);
    };
    document.revision = Some(node.id.to_string());
    Ok(Some(document))
}

/// Waits until the Index reflects a state that contains the one the token
/// `not_before` names (`None`: no wait). A token whose commit is not in the
/// history of `HEAD` of its repository (a reset, a pull that rewrote it)
/// does not wait.
///
/// # Errors
///
/// `INVALID_ARGUMENT` for text that is not a token; `INDEX_STALE` when the
/// Index was built with other ports than this Vault's and this handle has
/// no rebuild asked for or running (waiting cannot mend it; a rebuild
/// asked for, `Index.Reindex`, is waited for); `DEADLINE_EXCEEDED`
/// (retryable) when the Index does not reach it within
/// `OpenOptions.index_wait_timeout_ms`; `FAILED_PRECONDITION` when the
/// Vault closes meanwhile.
pub(crate) async fn not_before(pass: &Pass, not_before: Option<&str>) -> Result<(), Error> {
    let Some(token) = not_before else {
        return Ok(());
    };
    let want = State::of_token(token).ok_or_else(|| invalid("not_before is not a state token"))?;
    let vault = Arc::clone(pass.vault());
    let deadline = tokio::time::Instant::now() + vault.index_wait();
    let mut watch = vault.indexer().watch();
    loop {
        let have = vault
            .indexer()
            .status()
            .version
            .as_deref()
            .and_then(State::of_token);
        // The flag is read before the Index: a rebuild that ends between the
        // two has already published its generation, which the read sees.
        let rebuilding = vault.indexer().rebuild_pending();
        if pass
            .step(move |cx, vault| reached(cx, vault, have, want))
            .await?
        {
            return Ok(());
        }
        // An Index stale for this Vault stays so unless a rebuild of this
        // handle makes it fresh: waiting out the timeout would hide the
        // reason behind a retryable `DEADLINE_EXCEEDED`.
        if !rebuilding {
            match Reading::of(pass).await {
                Err(error) if error.is(VaultErrorCode::IndexStale) => return Err(error),
                // Fresh, not built yet (`INDEX_REBUILDING`), or a failure the
                // query itself meets next: the wait goes on.
                _ => {}
            }
        }
        tokio::select! {
            () = vault.gate().shutting() => return Err(crate::gate::closed()),
            () = tokio::time::sleep_until(deadline) => {
                return Err(Fault::new(
                    VaultErrorCode::DeadlineExceeded,
                    "the Index did not reach the state of not_before within index_wait_timeout_ms",
                )
                .into());
            }
            item = watch.next() => {
                if item.is_none() {
                    return Err(crate::gate::closed());
                }
            }
        }
    }
}

/// Whether the Index at `have` reflects a state that contains `want`: in
/// each repository, the commit `want` names is the Index's or one of its
/// ancestors, or it left the history of `HEAD` and is waited for no more.
fn reached(_cx: &Blocking, vault: &Vault, have: Option<State>, want: State) -> Result<bool, Error> {
    for repo in [crate::repo::Repo::Main, crate::repo::Repo::Isolated] {
        let Some(wanted) = want.oid(repo) else {
            continue;
        };
        let Some(git) = vault.repos().lease(repo)? else {
            continue;
        };
        let head = vault.repos().snapshot(repo)?.commit();
        let in_history = match head {
            Some(head) => head == wanted || descends(&git, head, wanted)?,
            None => false,
        };
        if !in_history {
            continue;
        }
        let Some(had) = have.and_then(|have| have.oid(repo)) else {
            return Ok(false);
        };
        if had != wanted && !descends(&git, had, wanted)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether `commit` descends from `ancestor`; `false` when either is not in
/// the repository.
fn descends(git: &git2::Repository, commit: git2::Oid, ancestor: git2::Oid) -> Result<bool, Error> {
    match git.graph_descendant_of(commit, ancestor) {
        Ok(descends) => Ok(descends),
        Err(err) if err.code() == git2::ErrorCode::NotFound => Ok(false),
        Err(err) => Err(crate::error::lib_fault(err).into()),
    }
}

#[cfg(test)]
mod tests {
    use scheda_api::v1::OpenOptions;

    use super::*;
    use crate::repo::blob_reads;
    use crate::testing::{journal_of_lines, open, step};

    #[tokio::test]
    async fn a_page_of_journal_lines_reads_their_segment_once() {
        let dir = std::env::var_os("TEST_TMPDIR")
            .map_or_else(std::env::temp_dir, std::path::PathBuf::from)
            .join(format!("scheda-page-segment-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let vault = open(&dir, OpenOptions::default().with_write_timeout_ms(2000))
            .await
            .unwrap();
        journal_of_lines(&vault, "log", 60).await;

        let pass = Pass::enter(&vault).unwrap();
        let reading = Reading::of(&pass).await.unwrap();
        let query = DocumentQuery::default()
            .with_limit(50)
            .with_include_raw(true);
        let found = find_in_collection(&pass, &reading, "log", &query, true)
            .await
            .unwrap();
        assert_eq!(found.hits.len(), 50);
        let mut page = Page::new(found, &reading, true);
        // The count of blob reads is of one thread: the page is read to the
        // end inside one step.
        let (documents, reads) = step(&vault, move |cx, vault| {
            let before = blob_reads();
            let mut documents = 0;
            while let Some(item) = page.next(cx, vault) {
                if matches!(item?.item, Some(QueryArm::Document(_))) {
                    documents += 1;
                }
            }
            Ok((documents, blob_reads() - before))
        })
        .await
        .unwrap();
        assert_eq!(documents, 50, "every line of the page is given");
        // The id map names the segment of each line, and the blob of that
        // segment is read for the first line and kept for the 49 others.
        assert_eq!(reads, 1, "the segment is read once for the page");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
