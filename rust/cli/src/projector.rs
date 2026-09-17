//! CLI-only whole-file Projector: one text column `body`.

use std::sync::Arc;

use scheda::{ColumnKind, Error, IndexColumn, IndexDoc, IndexSchema, IndexValue, Projector};

/// Whole-file Projector used on every CLI `Vault::open_with`.
pub(crate) fn cli_projector() -> Arc<dyn Projector> {
    Arc::new(WholeFileProjector)
}

struct WholeFileProjector;

impl Projector for WholeFileProjector {
    fn schema(&self, _collection: &str) -> Result<IndexSchema, Error> {
        Ok(IndexSchema {
            columns: vec![
                IndexColumn::default()
                    .with_name("body")
                    .with_kind(ColumnKind::Text)
                    .with_find(true)
                    .with_fts(true)
                    .with_vector(true),
            ],
            ..IndexSchema::default()
        })
    }

    fn project(&self, _collection: &str, _id: &str, raw: &[u8]) -> Result<IndexDoc, Error> {
        let Ok(text) = str::from_utf8(raw) else {
            return Ok(IndexDoc::default());
        };
        Ok(IndexDoc {
            values: [(String::from("body"), IndexValue::Text(text.to_owned()))].into(),
        })
    }
}
