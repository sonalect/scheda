//! The Projector of the command line: one text column, `body`, that holds
//! the whole Document.

use buffa_types::google::protobuf::Empty;
use scheda::Error;
use scheda::v1::projector_ref::ProjectorRef;
use scheda::v1::{
    CollectionKey, ColumnKind, IndexColumn, IndexDoc, IndexSchema, IndexValue, ProjectorDescriptor,
    ProjectorSync, RawDocument, index_value,
};

/// The identity of the projection: the Index built by it is rebuilt when it
/// changes.
const PROJECTION_ID: &str = "scheda-cli/whole-file@1";

/// The reference the command line gives every Vault it opens.
pub(crate) fn whole_file() -> ProjectorRef {
    ProjectorRef::from_sync(WholeFile)
}

/// Says that every collection has one text column, `body`, for the filter,
/// the full-text search, and the vectors, and that its value is the whole
/// Document when it is UTF-8 text.
struct WholeFile;

impl ProjectorSync for WholeFile {
    fn describe(&self, _empty: Empty) -> Result<ProjectorDescriptor, Error> {
        Ok(ProjectorDescriptor::default().with_projection_id(PROJECTION_ID))
    }

    fn columns(&self, _collection: CollectionKey) -> Result<IndexSchema, Error> {
        Ok(IndexSchema {
            columns: vec![
                IndexColumn::default()
                    .with_name("body")
                    .with_kind(ColumnKind::COLUMN_KIND_TEXT)
                    .with_find(true)
                    .with_fts(true)
                    .with_vector(true),
            ],
            ..IndexSchema::default()
        })
    }

    fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
        let raw = document.raw.unwrap_or_default();
        let Ok(text) = String::from_utf8(raw) else {
            return Ok(IndexDoc::default());
        };
        let value = IndexValue {
            value: Some(index_value::Value::Text(text)),
            ..IndexValue::default()
        };
        let mut doc = IndexDoc::default();
        doc.values.insert(String::from("body"), value);
        Ok(doc)
    }
}
