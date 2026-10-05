//! On-disk `document` / `singleton` files. Journal segments are `journal`.

use std::borrow::Cow;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::iter::FusedIterator;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Code, ContextAs, ContextIo, bail};
use crate::schema::{CollectionDecl, Decls, Kind};
use crate::{Encryption, Error, Record};

static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

const FS_NAME_MAX: usize = 255;

pub(crate) fn insert_document(
    vault: &Path,
    decl: &CollectionDecl,
    record: &Record,
    encryption: Option<&Encryption>,
) -> Result<(String, String), Error> {
    let prepared = prepare_write(vault, decl, record)?;
    if prepared.path.exists() {
        bail!(Error::duplicate_id(format!(
            "record {} already exists in {}",
            prepared.id, decl.name
        )));
    }
    let on_disk = encode_record_file(&prepared.bytes, decl, encryption)?;
    atomic_write(&prepared.path, &on_disk)?;
    crate::isolated::restrict_record(vault, &prepared.path)?;
    let rel = rel_posix(vault, &prepared.path)?;
    Ok((prepared.id, rel))
}

pub(crate) fn replace_document(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
    record: &Record,
    encryption: Option<&Encryption>,
) -> Result<(String, String), Error> {
    let Some(existing) = locate_file(vault, decl, id)? else {
        bail!(Error::not_found(format!(
            "record {id} is not in {}",
            decl.name
        )));
    };
    let prepared = prepare_write(vault, decl, record)?;
    if prepared.id != id {
        bail!(Error::invalid(format!(
            "replace id {id} does not match record id {}",
            prepared.id
        )));
    }
    if prepared.path != existing {
        bail!(Error::invalid(format!(
            "replace would move {id} off its path in {}",
            decl.name
        )));
    }
    let on_disk = encode_record_file(&prepared.bytes, decl, encryption)?;
    atomic_write(&prepared.path, &on_disk)?;
    crate::isolated::restrict_record(vault, &prepared.path)?;
    let rel = rel_posix(vault, &prepared.path)?;
    Ok((prepared.id, rel))
}

/// Vault-relative POSIX path of the Record file for `id`, whether or not
/// the file exists. `journal` is not a document path.
pub(crate) fn record_rel(decl: &CollectionDecl, id: &str) -> Result<String, Error> {
    Ok(vault_rel(decl, &record_file(decl, id)?))
}

/// Unlink the Record file. Missing → `not_found`. The unlink is the whole
/// file or no file; it does not rewrite bytes in place.
pub(crate) fn remove_document(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
) -> Result<String, Error> {
    let Some(path) = locate_file(vault, decl, id)? else {
        bail!(Error::not_found(format!(
            "record {id} is not in {}",
            decl.name
        )));
    };
    let rel = rel_posix(vault, &path)?;
    let dir = crate::isolated::records_dir(vault, decl)?;
    match fs::remove_file(&path) {
        Ok(()) => {
            prune_empty_parents(&dir, &path)?;
            Ok(rel)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!(Error::not_found(format!(
                "record {id} is not in {}",
                decl.name
            )))
        }
        Err(err) => Err(err).context("[store::remove_document] unlink record"),
    }
}

pub(crate) fn find_document(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
    encryption: Option<&Encryption>,
) -> Result<Record, Error> {
    if id.is_empty() {
        bail!(Error::invalid("id is empty"));
    }
    if decl.kind == Kind::Singleton {
        let Some(fixed) = decl.singleton_id.as_deref() else {
            bail!(Error::invalid(format!(
                "collection {} is missing singleton_id",
                decl.name
            )));
        };
        if id != fixed {
            bail!(Error::not_found(format!(
                "record {id} is not in {}",
                decl.name
            )));
        }
    }
    let Some(path) = locate_file(vault, decl, id)? else {
        bail!(Error::not_found(format!(
            "record {id} is not in {}",
            decl.name
        )));
    };
    read_file(vault, decl, &path, encryption)
}

pub(crate) fn check_documents(
    vault: &Path,
    decls: &Decls,
    encryption: Option<&Encryption>,
) -> Result<(), Error> {
    for decl in decls.collections() {
        if !matches!(decl.kind, Kind::Document | Kind::Singleton) {
            continue;
        }
        if decl.encrypted {
            #[cfg(not(feature = "encrypt"))]
            {
                let _ = encryption;
                bail!(Error::unsupported_encryption(format!(
                    "encrypted collection {} was not checked: this build cannot decrypt it",
                    decl.name
                )));
            }
            #[cfg(feature = "encrypt")]
            if encryption.is_none() {
                bail!(Error::crypto(format!(
                    "encrypted collection {} was not checked: secret is missing",
                    decl.name
                )));
            }
        }
        check_no_symlinks(vault, decl)?;
        let mut seen = std::collections::BTreeSet::new();
        for path in list_files(vault, decl)? {
            let rel = rel_posix(vault, &path)?;
            let raw = fs::read(&path)
                .context("[store::check_documents] read record")
                .map_err(|err| err.with_path(&rel))?;
            let raw = decode_record_file(raw, decl, encryption, &rel)?;
            let text = str::from_utf8(&raw)
                .context(Code::NotUtf8, "[store::check_documents] decode UTF-8")
                .map_err(|err| err.with_path(&rel))?;
            if has_conflict_markers(text) {
                bail!(Error::conflict_markers(format!(
                    "{rel} has git conflict markers"
                )));
            }
            let id = id_from_listed_file(vault, decl, &path)?;
            if !seen.insert(id.clone()) {
                bail!(Error::duplicate_id(format!(
                    "duplicate id {id} in {}",
                    decl.name
                )));
            }
            let expected = vault_rel(decl, &record_file(decl, &id)?);
            if expected != rel {
                bail!(Error::invalid(format!("{rel} path does not match id {id}")));
            }
        }
    }
    Ok(())
}

struct Prepared {
    id: String,
    path: PathBuf,
    bytes: Vec<u8>,
}

fn prepare_write(vault: &Path, decl: &CollectionDecl, record: &Record) -> Result<Prepared, Error> {
    let Some(id) = record.id.as_deref() else {
        bail!(Error::invalid("record id is required"));
    };
    match decl.kind {
        Kind::Document => crate::id::require_document_id(id)?,
        Kind::Journal | Kind::Singleton => crate::id::require_slug("id", id)?,
    }
    if decl.kind == Kind::Journal {
        bail!(Error::invalid(
            "document write used on a journal collection",
        ));
    }
    if decl.kind == Kind::Singleton {
        let Some(fixed) = decl.singleton_id.as_deref() else {
            bail!(Error::invalid(format!(
                "collection {} is missing singleton_id",
                decl.name
            )));
        };
        if id != fixed {
            bail!(Error::invalid(format!(
                "singleton {} id must be {fixed}",
                decl.name
            )));
        }
    }
    let rel = record_file(decl, id)?;
    let dir = crate::isolated::records_dir(vault, decl)?;
    let path = crate::schema::join_inside(&dir, &rel)?;
    reject_symlinks(vault, &path)?;
    let bytes = match decl.kind {
        Kind::Document | Kind::Singleton => {
            utf8_raw(record, "[store::prepare_write] decode UTF-8")?.to_vec()
        }
        Kind::Journal => unreachable!("journal write is refused above"),
    };
    Ok(Prepared {
        id: id.to_owned(),
        path,
        bytes,
    })
}

fn record_file(decl: &CollectionDecl, id: &str) -> Result<String, Error> {
    match decl.kind {
        Kind::Document => {
            crate::id::require_document_id(id)?;
            let leaf = match id.rsplit('/').next() {
                Some(leaf) => leaf,
                None => unreachable!("document id is non-empty"),
            };
            if leaf.len() > FS_NAME_MAX {
                bail!(Error::invalid(format!(
                    "record file name exceeds {FS_NAME_MAX} bytes"
                )));
            }
            Ok(id.to_owned())
        }
        Kind::Singleton => {
            crate::id::require_slug("id", id)?;
            if id.len() > FS_NAME_MAX {
                bail!(Error::invalid(format!(
                    "record file name exceeds {FS_NAME_MAX} bytes"
                )));
            }
            Ok(id.to_owned())
        }
        Kind::Journal => bail!(Error::invalid("document path used on a journal collection",)),
    }
}

fn vault_rel(decl: &CollectionDecl, file: &str) -> String {
    crate::isolated::vault_rel(decl, file)
}

fn list_document_files(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut found = Vec::new();
    collect_document_files(dir, &mut found)?;
    let mut keyed = Vec::new();
    for path in found {
        let id = document_id_from_path(dir, &path)?;
        keyed.push((id, path));
    }
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(keyed.into_iter().map(|(_, path)| path).collect())
}

fn collect_document_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Error> {
    let entries = fs::read_dir(dir).context("[store::list_files] read collection")?;
    for entry in entries {
        let entry = entry.context("[store::list_files] read entry")?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let meta = fs::symlink_metadata(&path).context("[store::list_files] stat entry")?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if crate::id::require_slug("id", name).is_err() {
                continue;
            }
            collect_document_files(&path, out)?;
            continue;
        }
        if meta.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn document_id_from_path(records_dir: &Path, path: &Path) -> Result<String, Error> {
    let rel = path.strip_prefix(records_dir).map_err(|_| {
        Error::invalid(format!("path {} is outside the collection", path.display()))
    })?;
    let mut out = String::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                let Some(name) = part.to_str() else {
                    bail!(Error::invalid("record file name is not UTF-8"));
                };
                if !out.is_empty() {
                    out.push('/');
                }
                out.push_str(name);
            }
            Component::CurDir => {}
            _ => {
                bail!(Error::invalid(format!(
                    "path {} is outside the collection",
                    path.display()
                )));
            }
        }
    }
    if out.is_empty() {
        bail!(Error::invalid("record path is empty"));
    }
    Ok(out)
}

fn prune_empty_parents(stop: &Path, file: &Path) -> Result<(), Error> {
    let Some(mut dir) = file.parent().map(Path::to_path_buf) else {
        return Ok(());
    };
    while dir.starts_with(stop) && dir != stop {
        match fs::remove_dir(&dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => break,
            Err(err) => {
                return Err(err).context("[store::remove_document] prune empty dir");
            }
        }
        let Some(parent) = dir.parent() else {
            break;
        };
        dir = parent.to_path_buf();
    }
    Ok(())
}

pub(crate) fn locate_file(
    vault: &Path,
    decl: &CollectionDecl,
    id: &str,
) -> Result<Option<PathBuf>, Error> {
    if decl.kind == Kind::Singleton
        && decl
            .singleton_id
            .as_deref()
            .is_some_and(|fixed| fixed != id)
    {
        return Ok(None);
    }
    let file = record_file(decl, id)?;
    let dir = crate::isolated::records_dir(vault, decl)?;
    let path = crate::schema::join_inside(&dir, &file)?;
    reject_symlinks(vault, &path)?;
    Ok(path.is_file().then_some(path))
}

pub(crate) fn list_files(vault: &Path, decl: &CollectionDecl) -> Result<Vec<PathBuf>, Error> {
    let dir = crate::isolated::records_dir(vault, decl)?;
    reject_symlinks(vault, &dir)?;
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    match decl.kind {
        Kind::Singleton => {
            let Some(id) = decl.singleton_id.as_deref() else {
                bail!(Error::invalid(format!(
                    "collection {} is missing singleton_id",
                    decl.name
                )));
            };
            let file = record_file(decl, id)?;
            let path = crate::schema::join_inside(&dir, &file)?;
            let is_file = fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file());
            Ok(is_file.then_some(path).into_iter().collect())
        }
        Kind::Document => list_document_files(&dir),
        Kind::Journal => Ok(Vec::new()),
    }
}

/// Id-order walk of one `document` or `singleton` collection.
///
/// Yields one Record at a time from the files the library names. Does not
/// open the Index. Order is the byte order of the full `id` (the relative
/// POSIX path under the collection, including any extension).
/// A `document` `id` may contain `/`, each component a directory.
/// A `singleton` yields its one Record, or nothing before the first write.
/// `journal` is refused when the walk is created.
pub struct CollectionDocuments {
    vault: PathBuf,
    decl: CollectionDecl,
    encryption: Option<Encryption>,
    files: Vec<PathBuf>,
    next: usize,
    done: bool,
}

pub(crate) fn collection_documents(
    vault: &Path,
    decl: &CollectionDecl,
    encryption: Option<&Encryption>,
) -> Result<CollectionDocuments, Error> {
    if decl.kind == Kind::Journal {
        bail!(Error::unsupported(
            "documents is offered for document and singleton only"
        ));
    }
    Ok(CollectionDocuments {
        vault: vault.to_path_buf(),
        decl: decl.clone(),
        encryption: encryption.cloned(),
        files: list_files(vault, decl)?,
        next: 0,
        done: false,
    })
}

impl CollectionDocuments {
    fn next_record(&mut self) -> Result<Option<Record>, Error> {
        let Some(path) = self.files.get(self.next).cloned() else {
            return Ok(None);
        };
        self.next += 1;
        Ok(Some(read_file(
            &self.vault,
            &self.decl,
            &path,
            self.encryption.as_ref(),
        )?))
    }
}

impl Iterator for CollectionDocuments {
    type Item = Result<Record, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_record() {
            Ok(Some(record)) => Some(Ok(record)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(err) => Some(Err(err)),
        }
    }
}

impl FusedIterator for CollectionDocuments {}

/// Ids of the Record files `list_files` named, in the same order. Does not
/// open a Record file. `journal` → `unsupported`.
pub(crate) fn collection_ids(vault: &Path, decl: &CollectionDecl) -> Result<Vec<String>, Error> {
    if decl.kind == Kind::Journal {
        bail!(Error::unsupported(
            "ids is offered for document and singleton only"
        ));
    }
    list_files(vault, decl)?
        .iter()
        .map(|path| id_from_listed_file(vault, decl, path))
        .collect()
}

fn id_from_listed_file(vault: &Path, decl: &CollectionDecl, path: &Path) -> Result<String, Error> {
    match decl.kind {
        Kind::Singleton => match decl.singleton_id.clone() {
            Some(id) => Ok(id),
            None => {
                bail!(Error::invalid(format!(
                    "collection {} is missing singleton_id",
                    decl.name
                )));
            }
        },
        Kind::Document => {
            let dir = crate::isolated::records_dir(vault, decl)?;
            document_id_from_path(&dir, path)
        }
        Kind::Journal => unreachable!("journal is refused before listing"),
    }
}

#[cfg(not(feature = "find"))]
pub(crate) fn reindex_stats(vault: &Path, decl: &CollectionDecl) -> Result<(u64, u64), Error> {
    if !decl.any_indexed() {
        return Ok((0, 0));
    }
    let mut records = 0u64;
    for path in list_files(vault, decl)? {
        match read_file(vault, decl, &path, None) {
            Ok(_) => records += 1,
            Err(err) if crate::reindex::is_absent(&err) => continue,
            Err(err) => return Err(err),
        }
    }
    Ok((records, 0))
}

pub(crate) fn read_file(
    vault: &Path,
    decl: &CollectionDecl,
    path: &Path,
    encryption: Option<&Encryption>,
) -> Result<Record, Error> {
    let rel = rel_posix(vault, path)?;
    let raw = fs::read(path)
        .context("[store::read_file] read record")
        .map_err(|err| err.with_path(&rel))?;
    let raw = decode_record_file(raw, decl, encryption, &rel)?;
    let id = match decl.kind {
        Kind::Document => {
            let dir = crate::isolated::records_dir(vault, decl)?;
            document_id_from_path(&dir, path)?
        }
        Kind::Singleton => match decl.singleton_id.clone() {
            Some(id) => id,
            None => {
                bail!(Error::invalid(format!(
                    "collection {} is missing singleton_id",
                    decl.name
                )));
            }
        },
        Kind::Journal => bail!(Error::invalid("document read used on a journal collection",)),
    };
    Ok(Record::default()
        .with_collection(decl.name.as_str())
        .with_id(id)
        .with_raw(raw))
}

pub(crate) fn record_raw(record: &Record) -> &[u8] {
    record.raw.as_deref().unwrap_or(&[])
}

pub(crate) fn utf8_raw<'a>(record: &'a Record, frame: &'static str) -> Result<&'a [u8], Error> {
    let raw = record_raw(record);
    str::from_utf8(raw).context(Code::NotUtf8, frame)?;
    Ok(raw)
}

fn encode_record_file<'a>(
    plaintext: &'a [u8],
    decl: &CollectionDecl,
    encryption: Option<&Encryption>,
) -> Result<Cow<'a, [u8]>, Error> {
    if !decl.encrypted {
        return Ok(Cow::Borrowed(plaintext));
    }
    #[cfg(feature = "encrypt")]
    {
        Ok(Cow::Owned(crate::crypto::encrypt(plaintext, encryption)?))
    }
    #[cfg(not(feature = "encrypt"))]
    {
        let _ = (plaintext, encryption);
        bail!(Error::unsupported_encryption(
            "this build cannot encrypt Record files"
        ));
    }
}

pub(crate) fn decode_record_file(
    raw: Vec<u8>,
    decl: &CollectionDecl,
    encryption: Option<&Encryption>,
    rel: &str,
) -> Result<Vec<u8>, Error> {
    if !decl.encrypted {
        return Ok(raw);
    }
    #[cfg(not(feature = "encrypt"))]
    {
        let _ = encryption;
        bail!(
            Error::unsupported_encryption(format!(
                "collection {} is encrypted; this build cannot read it",
                decl.name
            ))
            .with_path(rel)
        );
    }
    #[cfg(feature = "encrypt")]
    crate::crypto::decrypt(&raw, encryption).map_err(|err| err.with_path(rel))
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context("[store::atomic_write] create dir")?;
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let n = NEXT_TMP.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".scheda.{n}.tmp"));
    let written = write_tmp_then_rename(&tmp, path, bytes);
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

fn write_tmp_then_rename(tmp: &Path, path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .context("[store::atomic_write] create temp file")?;
    file.write_all(bytes)
        .context("[store::atomic_write] write temp file")?;
    file.sync_all()
        .context("[store::atomic_write] fsync temp file")?;
    drop(file);
    rename_replace(tmp, path)
}

/// `fs::rename` cannot replace an existing file on Windows.
fn rename_replace(tmp: &Path, path: &Path) -> Result<(), Error> {
    #[cfg(windows)]
    {
        rename_replace_windows(tmp, path)
    }
    #[cfg(not(windows))]
    {
        fs::rename(tmp, path).context("[store::atomic_write] rename temp file")
    }
}

#[cfg(windows)]
fn rename_replace_windows(tmp: &Path, path: &Path) -> Result<(), Error> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;

    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let from = wide(tmp);
    let to = wide(path);
    // SAFETY: both buffers are NUL-terminated UTF-16 from `encode_wide`.
    let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING) };
    if ok == 0 {
        Err(std::io::Error::last_os_error()).context("[store::atomic_write] replace file")
    } else {
        Ok(())
    }
}

pub(crate) fn rel_posix(vault: &Path, path: &Path) -> Result<String, Error> {
    let rel = path
        .strip_prefix(vault)
        .map_err(|_| Error::invalid(format!("path {} is outside the Vault", path.display())))?;
    let mut out = String::new();
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                if !out.is_empty() {
                    out.push('/');
                }
                out.push_str(&part.to_string_lossy());
            }
            Component::CurDir => {}
            _ => {
                bail!(Error::invalid(format!(
                    "path {} is outside the Vault",
                    path.display()
                )));
            }
        }
    }
    Ok(out)
}

/// Whether one line is a git conflict marker: `<<<<<<<` or `>>>>>>>`
/// followed by a space or the end of the line. `=======` is not counted on
/// its own: a conflict always has an opening and a closing marker, and a
/// bare `=======` is also a Markdown heading underline or a rule.
pub(crate) fn is_conflict_marker_line(line: &str) -> bool {
    let line = line.strip_suffix('\r').unwrap_or(line);
    for marker in ["<<<<<<<", ">>>>>>>"] {
        if let Some(rest) = line.strip_prefix(marker)
            && (rest.is_empty() || rest.starts_with(' '))
        {
            return true;
        }
    }
    false
}

/// Whether any line of `text` is a git conflict marker line. A marker
/// inside a line (a quoted diff in a journal payload, a code sample) does
/// not count.
pub(crate) fn has_conflict_markers(text: &str) -> bool {
    text.split('\n').any(is_conflict_marker_line)
}

/// Refuse a path that goes through a symlink below the Vault root.
///
/// Walks every component of `path` under `vault` (the collection
/// directory, any `.isolated/` directory, subdirectories, the file) with
/// `symlink_metadata`. A symlink is `invalid` and names its vault-relative
/// path. A component that does not exist ends the walk: nothing below it
/// can be a link. The Vault root itself may be reached through a link.
pub(crate) fn reject_symlinks(vault: &Path, path: &Path) -> Result<(), Error> {
    let Ok(rest) = path.strip_prefix(vault) else {
        return Ok(());
    };
    let mut cur = vault.to_path_buf();
    for component in rest.components() {
        cur.push(component);
        match fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let rel = rel_posix(vault, &cur)?;
                bail!(Error::invalid(format!("{rel} is a symlink")).with_path(&rel));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                let rel = rel_posix(vault, &cur)?;
                return Err(err)
                    .context("[store::reject_symlinks] stat component")
                    .map_err(|err| err.with_path(&rel));
            }
        }
    }
    Ok(())
}

/// `check` of one collection: the records directory and everything under
/// it must be free of symlinks. Reports the first link with its path.
pub(crate) fn check_no_symlinks(vault: &Path, decl: &CollectionDecl) -> Result<(), Error> {
    let dir = crate::isolated::records_dir(vault, decl)?;
    reject_symlinks(vault, &dir)?;
    if !dir.is_dir() {
        return Ok(());
    }
    walk_no_symlinks(vault, &dir)
}

fn walk_no_symlinks(vault: &Path, dir: &Path) -> Result<(), Error> {
    let entries = fs::read_dir(dir).context("[store::check] read collection")?;
    for entry in entries {
        let entry = entry.context("[store::check] read entry")?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).context("[store::check] stat entry")?;
        if meta.file_type().is_symlink() {
            let rel = rel_posix(vault, &path)?;
            bail!(Error::invalid(format!("{rel} is a symlink")).with_path(&rel));
        }
        if meta.is_dir() {
            walk_no_symlinks(vault, &path)?;
        }
    }
    Ok(())
}

/// Owner-only file mode (`0600` on Unix). Other systems leave ACLs unchanged.
pub(crate) fn restrict_file(path: &Path) -> Result<(), Error> {
    set_mode(path, 0o600)
}

/// Owner-only directory mode (`0700` on Unix). Other systems leave ACLs unchanged.
pub(crate) fn restrict_dir(path: &Path) -> Result<(), Error> {
    set_mode(path, 0o700)
}

/// Owner-only mode on `file` and on every directory between `base` and it
/// (`base` itself excluded). For a file this crate just wrote under a
/// directory it may have just created: two or three `chmod` calls instead
/// of a walk of the whole tree. A `file` outside `base` gets its own mode
/// only.
pub(crate) fn restrict_file_below(base: &Path, file: &Path) -> Result<(), Error> {
    if let Some(parent) = file.parent()
        && let Ok(rel) = parent.strip_prefix(base)
    {
        let mut dir = base.to_path_buf();
        for part in rel.components() {
            dir.push(part);
            restrict_dir(&dir)?;
        }
    }
    restrict_file(file)
}

/// Walk `root` and apply owner-only modes to every directory and file.
///
/// Used after writes that create files the caller did not open (Tantivy
/// segments under `{vault}/.scheda/`). Missing `root` is a no-op. Symlinks
/// are not followed.
pub(crate) fn restrict_tree(root: &Path) -> Result<(), Error> {
    let meta = match fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).context("[store] read owner-only path"),
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        restrict_dir(root)?;
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(err).context("[store] read dir for owner-only mode");
            }
        };
        for entry in entries {
            let entry = entry.context("[store] read dir entry for owner-only mode")?;
            restrict_tree(&entry.path())?;
        }
        return Ok(());
    }
    if meta.is_file() {
        restrict_file(root)?;
    }
    Ok(())
}

/// `{vault}/.scheda/` must be owner-only on Unix (`0700` directories, `0600`
/// files). Wider modes fail `check` with the vault-relative path in `message`.
/// Other systems do not inspect ACLs.
pub(crate) fn check_scheda_owner_only(vault: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        let scheda = vault.join(".scheda");
        if !scheda.exists() {
            return Ok(());
        }
        check_owner_only_tree(vault, &scheda)?;
    }
    #[cfg(not(unix))]
    {
        let _ = vault;
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;
        match fs::set_permissions(path, Permissions::from_mode(mode)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).context("[store] set owner-only mode"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

#[cfg(unix)]
fn check_owner_only_tree(vault: &Path, path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).context("[store::check] read mode"),
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let rel = rel_posix(vault, path)?;
    let mode = meta.permissions().mode() & 0o777;
    if meta.is_dir() {
        if mode != 0o700 {
            bail!(Error::invalid(format!(
                "{rel} mode is {mode:04o}; owner-only directory is 0700"
            )));
        }
        let entries = fs::read_dir(path).context("[store::check] read dir")?;
        for entry in entries {
            let entry = entry.context("[store::check] read dir entry")?;
            check_owner_only_tree(vault, &entry.path())?;
        }
        return Ok(());
    }
    if meta.is_file() && mode != 0o600 {
        bail!(Error::invalid(format!(
            "{rel} mode is {mode:04o}; owner-only file is 0600"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::record_file;
    use crate::schema::{CollectionDecl, Kind};

    fn decl(kind: Kind) -> CollectionDecl {
        CollectionDecl {
            name: String::from("c"),
            kind,
            version: 1,
            segment_max_bytes: None,
            segment_max_lines: None,
            segment_rotate: None,
            singleton_id: Some(String::from("inbox")),
            isolated: false,
            encrypted: false,
            exclude_lexical_index: false,
            exclude_vector_index: false,
        }
    }

    #[test]
    fn document_file_is_id() {
        assert_eq!(
            record_file(&decl(Kind::Document), "alpha").unwrap(),
            "alpha"
        );
        assert_eq!(
            record_file(&decl(Kind::Document), "work/meeting").unwrap(),
            "work/meeting"
        );
        assert_eq!(
            record_file(&decl(Kind::Document), "work/meeting.toml").unwrap(),
            "work/meeting.toml"
        );
        assert_eq!(
            record_file(&decl(Kind::Document), "foo.md").unwrap(),
            "foo.md"
        );
        assert_eq!(
            record_file(&decl(Kind::Singleton), "inbox").unwrap(),
            "inbox"
        );
        assert_eq!(
            record_file(&decl(Kind::Singleton), "inbox.toml").unwrap(),
            "inbox.toml"
        );
    }
}
