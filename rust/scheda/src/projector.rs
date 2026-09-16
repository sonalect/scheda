//! Caller-supplied projection of opaque Record bytes into Index columns.
//!
//! Handles live in [`crate::vault`]. The trait is not an `api/v1` field.

use std::collections::BTreeMap;

use crate::{Error, IndexSchema};
#[cfg(feature = "find")]
use crate::{ColumnKind, IndexColumn};

/// Caller-supplied mapping from Record bytes to Index columns. Not an `api/v1`
/// field.
///
/// `schema(collection)` names columns and marks (`find` / `fts` / `vector`).
/// `project` fills those columns from `{collection, id, raw}`. Scheda does not
/// parse consumer contents. No projector → the Index holds identity only:
/// `find` `all` and `term` on `id` still work; any other Filter field is
/// `invalid`. `add` / `replace` do not call [`project`](Self::project) to
/// accept or reject the write. A `project` error or empty [`IndexDoc`] omits
/// that Record from the lexical Index; the file remains.
pub trait Projector: Send + Sync {
    /// Columns this projector exposes for `collection`.
    ///
    /// Unknown collection: empty columns or an error are both allowed. Empty
    /// columns (or an error) omit that collection from the Index projector
    /// dump. Column names `id` and `collection` are reserved (identity);
    /// empty or duplicate names are `invalid` when the schema is used.
    fn schema(&self, collection: &str) -> Result<IndexSchema, Error>;
    /// Column values for one Record. `raw` is the file bytes (empty slice
    /// when unset).
    ///
    /// Omit a key in [`IndexDoc`] for a missing column. [`IndexValue::Null`]
    /// is a stored null (Filter term null / `f_null`). An empty `values` map
    /// omits the Record from the lexical Index.
    fn project(&self, collection: &str, id: &str, raw: &[u8]) -> Result<IndexDoc, Error>;
}

/// Find-marked column named `name`.
#[cfg(feature = "find")]
pub(crate) fn find_marked_column<'a>(
    schema: &'a IndexSchema,
    name: &str,
) -> Option<&'a IndexColumn> {
    schema.columns.iter().find(|column| {
        column.find.unwrap_or(false) && column.name.as_deref() == Some(name)
    })
}

/// One projected column value.
#[derive(Clone, Debug, PartialEq)]
pub enum IndexValue {
    /// UTF-8 string.
    Text(String),
    /// Signed integer.
    Integer(i64),
    /// Floating point. Non-finite values are skipped at index time.
    Number(f64),
    /// Boolean (`true` / `false` as eq text).
    Boolean(bool),
    /// List of UTF-8 strings; each string is an eq/token value.
    TextList(Vec<String>),
    /// Stored null (`f_null` only; not exists/eq).
    Null,
}

/// Projected column values for one Record.
///
/// A missing key is absent. [`IndexValue::Null`] is present and null.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IndexDoc {
    /// Column name → value. Empty omits the Record from the lexical Index.
    pub values: BTreeMap<String, IndexValue>,
}

#[cfg(feature = "find")]
/// `identity\n` dump when the handle has no projector.
pub(crate) const IDENTITY_DUMP: &str = "identity\n";

/// Encode one collection's projector schema as a dump line, or `None` when
/// `schema` is empty or `schema()` failed.
#[cfg(feature = "find")]
pub(crate) fn collection_dump_line(
    projector: &dyn Projector,
    collection: &str,
) -> Result<Option<String>, Error> {
    let schema = match projector.schema(collection) {
        Ok(schema) => schema,
        Err(_) => return Ok(None),
    };
    schema.require_valid()?;
    if schema.columns.is_empty() {
        return Ok(None);
    }
    Ok(Some(encode_collection_line(collection, &schema)))
}

/// `{collection}\\t{name}:{kind}:{marks}…` for a non-empty validated schema.
#[cfg(feature = "find")]
pub(crate) fn encode_collection_line(collection: &str, schema: &IndexSchema) -> String {
    let mut line = collection.to_owned();
    for column in &schema.columns {
        line.push('\t');
        line.push_str(&encode_column(column));
    }
    line
}

#[cfg(feature = "find")]
fn encode_column(column: &IndexColumn) -> String {
    format!(
        "{}:{}:{}{}{}",
        column.name.as_deref().unwrap_or(""),
        kind_token(column),
        mark(column.find.unwrap_or(false), 'f'),
        mark(column.fts.unwrap_or(false), 't'),
        mark(column.vector.unwrap_or(false), 'v'),
    )
}

#[cfg(feature = "find")]
fn kind_token(column: &IndexColumn) -> &'static str {
    match column.kind.as_ref().and_then(|kind| kind.as_known()) {
        Some(ColumnKind::Text) => "text",
        Some(ColumnKind::Integer) => "integer",
        Some(ColumnKind::Number) => "number",
        Some(ColumnKind::Boolean) => "boolean",
        Some(ColumnKind::TextList) => "text_list",
        Some(ColumnKind::Unspecified) | None => unreachable!(),
    }
}

#[cfg(feature = "find")]
fn mark(on: bool, flag: char) -> char {
    if on { flag } else { '-' }
}

/// Full dump text for `collections` (sorted by name). Empty projector schemas
/// are omitted. No projector → [`IDENTITY_DUMP`].
#[cfg(feature = "find")]
pub(crate) fn encode_dump(
    projector: Option<&dyn Projector>,
    collections: impl IntoIterator<Item = impl AsRef<str>>,
) -> Result<String, Error> {
    let Some(projector) = projector else {
        return Ok(IDENTITY_DUMP.to_owned());
    };
    let mut lines = BTreeMap::new();
    for name in collections {
        let name = name.as_ref();
        if let Some(line) = collection_dump_line(projector, name)? {
            lines.insert(name.to_owned(), line);
        }
    }
    Ok(join_dump_lines(&lines))
}

#[cfg(feature = "find")]
fn join_dump_lines(lines: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    for line in lines.values() {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Parsed `{generation}/projector` file.
#[cfg(feature = "find")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProjectorDump {
    /// Single line `identity`: Index built without a projector.
    Identity,
    /// Collection name → full dump line (no trailing newline).
    Collections(BTreeMap<String, String>),
}

#[cfg(feature = "find")]
impl ProjectorDump {
    pub(crate) fn parse(text: &str) -> Self {
        let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
        if lines == ["identity"] {
            return Self::Identity;
        }
        let mut collections = BTreeMap::new();
        for line in lines {
            let name = line.split_once('\t').map_or(line, |(name, _)| name);
            collections.insert(name.to_owned(), line.to_owned());
        }
        Self::Collections(collections)
    }

    pub(crate) fn line(&self, collection: &str) -> Option<&str> {
        match self {
            Self::Identity => None,
            Self::Collections(map) => map.get(collection).map(String::as_str),
        }
    }

    pub(crate) fn insert_line(&mut self, collection: String, line: String) {
        match self {
            Self::Identity => {
                let mut map = BTreeMap::new();
                map.insert(collection, line);
                *self = Self::Collections(map);
            }
            Self::Collections(map) => {
                map.insert(collection, line);
            }
        }
    }

    pub(crate) fn to_text(&self) -> String {
        match self {
            Self::Identity => IDENTITY_DUMP.to_owned(),
            Self::Collections(map) => join_dump_lines(map),
        }
    }
}

/// Eq texts for a projected value. `Null` and non-finite numbers yield none.
#[cfg(feature = "find")]
pub(crate) fn eq_texts_owned(value: &IndexValue) -> Vec<String> {
    match value {
        IndexValue::Text(text) => vec![text.clone()],
        IndexValue::Integer(n) => vec![n.to_string()],
        IndexValue::Number(n) if n.is_finite() => vec![n.to_string()],
        IndexValue::Number(_) => Vec::new(),
        IndexValue::Boolean(b) => vec![b.to_string()],
        IndexValue::TextList(items) => items.clone(),
        IndexValue::Null => Vec::new(),
    }
}
