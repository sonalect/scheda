//! Disposable Index under `{vault}/.scheda/index` (IDX-1, IDX-2, LIB-16).

use std::fs;
use std::path::Path;

use crate::journal;
use crate::schema::{Kind, Schema};
use crate::store;
use crate::{Error, ReindexCounts};

pub(crate) const FORMAT_VERSION: u32 = 1;
const FORMAT_FILE: &str = "format";
const WRITE_LOCK: &str = "write.lock";

pub(crate) fn rebuild(
    vault: &Path,
    schema: &Schema,
    index_dir: &Path,
) -> Result<ReindexCounts, Error> {
    clear_except_lock(index_dir)?;
    let mut records_read = 0u64;
    let mut skipped_invalid = 0u64;
    let mut segments_seen = 0u64;
    for decl in schema.collections() {
        match decl.kind {
            Kind::Journal => {
                let (records, invalid, segments) = journal::reindex_stats(vault, decl)?;
                records_read += records;
                skipped_invalid += invalid;
                segments_seen += segments;
            }
            Kind::Document | Kind::Singleton => {
                let (records, invalid) = store::reindex_stats(vault, decl)?;
                records_read += records;
                skipped_invalid += invalid;
            }
        }
    }
    write_format(index_dir)?;
    Ok(ReindexCounts::default()
        .with_records_read(records_read)
        .with_skipped_invalid(skipped_invalid)
        .with_segments_seen(segments_seen))
}

fn write_format(index_dir: &Path) -> Result<(), Error> {
    fs::create_dir_all(index_dir).map_err(|err| {
        Error::io(format!(
            "create index directory {}: {err}",
            index_dir.display()
        ))
    })?;
    let path = index_dir.join(FORMAT_FILE);
    fs::write(&path, format!("{FORMAT_VERSION}\n"))
        .map_err(|err| Error::io(format!("write {}: {err}", path.display())))
}

fn clear_except_lock(index_dir: &Path) -> Result<(), Error> {
    fs::create_dir_all(index_dir).map_err(|err| {
        Error::io(format!(
            "create index directory {}: {err}",
            index_dir.display()
        ))
    })?;
    let entries = fs::read_dir(index_dir)
        .map_err(|err| Error::io(format!("read {}: {err}", index_dir.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|err| Error::io(format!("read {}: {err}", index_dir.display())))?;
        let name = entry.file_name();
        if name == WRITE_LOCK {
            continue;
        }
        let path = entry.path();
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        result.map_err(|err| Error::io(format!("remove {}: {err}", path.display())))?;
    }
    Ok(())
}
