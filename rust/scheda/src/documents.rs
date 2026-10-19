//! Document files: the file of one Document of a `standard` or `singleton`
//! collection, isolated ones included.
//!
//! A Document is bytes (D20). The bytes of a file are the bytes of the
//! Document, or, in an encrypted collection, their age armor. Nothing here
//! asks whether they are text, and nothing scans them for conflict markers.
//!
//! The paths follow the id: the file of `work/meeting.md` in `notes` is
//! `notes/work/meeting.md` (under `.isolated/` for an isolated collection),
//! and a `singleton` has the one file named by its declared id. Everything
//! is read from a snapshot of the commit `HEAD` names, never from the work
//! tree; writing is the committer's, and [`encode`] only says which path
//! and which bytes.

use std::borrow::Cow;

use git2::{ObjectType, Oid};
use scheda_api::v1::{Encryption, Problem, VaultErrorCode};

use crate::check::{problem, problem_of};
use crate::collection::{Declaration, Declarations, Kind};
use crate::error::{Context, Error, Fault, lib_fault};
use crate::id::{require_document_id, require_slug};
use crate::isolated;
use crate::repo::{NodeKind, Snap, Views};

fn invalid(message: impl Into<String>) -> Fault {
    Fault::new(VaultErrorCode::InvalidArgument, message)
}

/// `INVALID_ARGUMENT` unless `decl` keeps one file per Document.
fn require_files(decl: &Declaration) -> Result<(), Fault> {
    match decl.kind() {
        Kind::Standard | Kind::Singleton => Ok(()),
        Kind::Journal => Err(invalid(format!(
            "the collection {} is a journal; it has lines, not Document files",
            decl.name()
        ))),
    }
}

/// The Vault-relative POSIX path of the file of Document `id`, whether or
/// not the file exists.
///
/// The id is checked: a `standard` id is slugs joined by `/`, a `singleton`
/// id is the one declared. Anything else is `INVALID_ARGUMENT`, a
/// `journal` collection included.
pub(crate) fn path_of(decl: &Declaration, id: &str) -> Result<String, Fault> {
    require_files(decl)?;
    match decl.kind() {
        Kind::Standard => require_document_id(id)?,
        Kind::Singleton | Kind::Journal => {
            require_slug("id", id)?;
            if decl.singleton_id() != Some(id) {
                return Err(invalid(format!(
                    "the id of the one Document of {} is {}",
                    decl.name(),
                    decl.singleton_id().unwrap_or("")
                )));
            }
        }
    }
    Ok(isolated::vault_rel(decl, id))
}

/// Reads the Document `id`: the bytes of its file in the commit of `snap`,
/// or, in an encrypted collection, the bytes the armor holds.
///
/// An absent Document is `None`, not an error: no file, a directory where
/// the file would be, or (for a `singleton`) an id other than its own. A
/// symlink on the way is `INVALID_ARGUMENT`. An encrypted collection needs
/// the secret of the handle (`CRYPTO`); a build without the `encrypt`
/// feature cannot read one (`UNIMPLEMENTED`).
#[cfg(any(test, feature = "find"))]
pub(crate) fn read(
    snap: &Snap,
    decl: &Declaration,
    id: &str,
    encryption: Option<&Encryption>,
) -> Result<Option<Vec<u8>>, Fault> {
    require_files(decl)?;
    if decl.kind() == Kind::Singleton && decl.singleton_id() != Some(id) {
        return Ok(None);
    }
    let rel = path_of(decl, id)?;
    read_file(snap, &rel, decl, encryption)
}

/// Reads the file at the Vault-relative `rel`: `None` when the commit holds
/// no file there.
fn read_file(
    snap: &Snap,
    rel: &str,
    decl: &Declaration,
    encryption: Option<&Encryption>,
) -> Result<Option<Vec<u8>>, Fault> {
    let Some(blob) = snap.read(rel)? else {
        return Ok(None);
    };
    unseal(decl, blob.bytes, encryption, rel).map(Some)
}

/// What the Document held in a file, as the caller knows it.
#[derive(Debug)]
pub(crate) enum Plain {
    /// The file is the Document: the collection is not encrypted.
    Same,
    /// The Document, opened from the armor of the file.
    Opened(Vec<u8>),
    /// A sealed file whose Document the caller did not keep.
    Unknown,
}

/// A Document file: the bytes in the commit, and what they hold.
#[derive(Debug)]
pub(crate) struct Found {
    /// The bytes of the file (the armor, in an encrypted collection).
    pub(crate) file: Vec<u8>,
    /// The Document in them.
    pub(crate) plain: Plain,
    /// The id of the blob that holds the file.
    id: Oid,
}

impl Found {
    /// A file that is to be stored with `file` as its bytes: its blob id is
    /// worked out, not looked up.
    pub(crate) fn new(file: Vec<u8>, plain: Plain) -> Result<Found, Fault> {
        let id = blob_id(&file)?;
        Ok(Found { file, plain, id })
    }

    /// The revision of the Document: the id of its blob, in lowercase hex.
    pub(crate) fn revision(&self) -> String {
        self.id.to_string()
    }

    /// The bytes of the Document, if they are known.
    pub(crate) fn plain(&self) -> Option<&[u8]> {
        match &self.plain {
            Plain::Same => Some(&self.file),
            Plain::Opened(bytes) => Some(bytes),
            Plain::Unknown => None,
        }
    }

    /// The bytes of the Document, moved out; empty if they are not known.
    pub(crate) fn into_plain(self) -> Vec<u8> {
        match self.plain {
            Plain::Same => self.file,
            Plain::Opened(bytes) => bytes,
            Plain::Unknown => Vec::new(),
        }
    }
}

/// The id git gives a blob of `file`, without storing it.
fn blob_id(file: &[u8]) -> Result<Oid, Fault> {
    Oid::hash_object(ObjectType::Blob, file).map_err(lib_fault)
}

/// Reads the file of Document `id` in the commit of `snap` and opens it: the
/// bytes there and the Document in them. An absent Document is `None`, as
/// for [`read`], and a symlink on the way is `INVALID_ARGUMENT`. An
/// encrypted collection needs the secret of the handle (`CRYPTO`) and a
/// build with the `encrypt` feature (`UNIMPLEMENTED`).
pub(crate) fn read_found(
    snap: &Snap,
    decl: &Declaration,
    id: &str,
    encryption: Option<&Encryption>,
) -> Result<Option<Found>, Fault> {
    require_files(decl)?;
    if decl.kind() == Kind::Singleton && decl.singleton_id() != Some(id) {
        return Ok(None);
    }
    let rel = path_of(decl, id)?;
    let Some(blob) = snap.read(&rel)? else {
        return Ok(None);
    };
    let plain = if decl.encrypted() {
        Plain::Opened(open_armor(decl, &blob.bytes, encryption, &rel)?)
    } else {
        Plain::Same
    };
    Ok(Some(Found {
        file: blob.bytes,
        plain,
        id: blob.id,
    }))
}

/// `Ok` when the collection `decl` can be written and read with this
/// handle: not encrypted, or encrypted with a secret on the handle in a
/// build with the `encrypt` feature. Otherwise `CRYPTO` (no secret) or
/// `UNIMPLEMENTED` (no feature). A secret that is wrong shows when the bytes
/// are sealed or opened.
pub(crate) fn require_secret(
    decl: &Declaration,
    encryption: Option<&Encryption>,
) -> Result<(), Fault> {
    if !decl.encrypted() {
        return Ok(());
    }
    #[cfg(feature = "encrypt")]
    {
        if encryption.is_some_and(|encryption| encryption.secret.is_some()) {
            Ok(())
        } else {
            Err(Fault::new(
                VaultErrorCode::Crypto,
                format!(
                    "the collection {} is encrypted; the handle has no secret",
                    decl.name()
                ),
            ))
        }
    }
    #[cfg(not(feature = "encrypt"))]
    {
        let _ = encryption;
        Err(no_encrypt(decl))
    }
}

/// The Document in the armor `file` of the encrypted collection `decl`.
#[cfg(feature = "encrypt")]
fn open_armor(
    decl: &Declaration,
    file: &[u8],
    encryption: Option<&Encryption>,
    rel: &str,
) -> Result<Vec<u8>, Fault> {
    let _ = decl;
    crate::crypto::open(file, encryption).map_err(|fault| fault.with_path(rel))
}

#[cfg(not(feature = "encrypt"))]
fn open_armor(
    decl: &Declaration,
    file: &[u8],
    encryption: Option<&Encryption>,
    rel: &str,
) -> Result<Vec<u8>, Fault> {
    let _ = (file, encryption);
    Err(no_encrypt(decl).with_path(rel))
}

#[cfg(feature = "encrypt")]
pub(crate) fn unseal(
    decl: &Declaration,
    stored: Vec<u8>,
    encryption: Option<&Encryption>,
    rel: &str,
) -> Result<Vec<u8>, Fault> {
    if !decl.encrypted() {
        return Ok(stored);
    }
    crate::crypto::open(&stored, encryption).map_err(|fault| fault.with_path(rel))
}

#[cfg(not(feature = "encrypt"))]
pub(crate) fn unseal(
    decl: &Declaration,
    stored: Vec<u8>,
    encryption: Option<&Encryption>,
    rel: &str,
) -> Result<Vec<u8>, Fault> {
    let _ = encryption;
    if !decl.encrypted() {
        return Ok(stored);
    }
    Err(no_encrypt(decl).with_path(rel))
}

#[cfg(not(feature = "encrypt"))]
fn no_encrypt(decl: &Declaration) -> Fault {
    Fault::new(
        VaultErrorCode::Unimplemented,
        format!(
            "the collection {} is encrypted; this build has no `encrypt` feature",
            decl.name()
        ),
    )
}

/// Encodes the Document `id` with the bytes `raw` for a write: the bytes
/// its file is to hold, the Document as it is or, for an encrypted
/// collection, its age armor. Nothing is written.
///
/// The id is checked as [`path_of`] does. An encrypted collection needs the
/// secret of the handle (`CRYPTO`); a build without the `encrypt` feature
/// cannot write one (`UNIMPLEMENTED`).
pub(crate) fn encode<'a>(
    decl: &Declaration,
    id: &str,
    raw: &'a [u8],
    encryption: Option<&Encryption>,
) -> Result<Cow<'a, [u8]>, Fault> {
    let path = path_of(decl, id)?;
    seal(decl, raw, encryption, &path)
}

#[cfg(feature = "encrypt")]
fn seal<'a>(
    decl: &Declaration,
    raw: &'a [u8],
    encryption: Option<&Encryption>,
    rel: &str,
) -> Result<Cow<'a, [u8]>, Fault> {
    if !decl.encrypted() {
        return Ok(Cow::Borrowed(raw));
    }
    crate::crypto::seal(raw, encryption)
        .map(Cow::Owned)
        .map_err(|fault| fault.with_path(rel))
}

#[cfg(not(feature = "encrypt"))]
fn seal<'a>(
    decl: &Declaration,
    raw: &'a [u8],
    encryption: Option<&Encryption>,
    rel: &str,
) -> Result<Cow<'a, [u8]>, Fault> {
    let _ = encryption;
    if !decl.encrypted() {
        return Ok(Cow::Borrowed(raw));
    }
    Err(no_encrypt(decl).with_path(rel))
}

/// One entry of a directory the walk has listed.
#[derive(Debug)]
struct Entry {
    name: String,
    dir: bool,
    /// The id of the blob or of the tree.
    node: Oid,
}

/// A directory the walk is inside: the id prefix of its entries (empty, or
/// ending in `/`) and the entries not yet visited.
struct Level {
    prefix: String,
    entries: std::vec::IntoIter<Entry>,
}

/// A lazy walk of the Document ids of one collection, in id order: the
/// byte order of the whole id, the path below the collection directory with
/// its extension.
///
/// It reads the commit of the snapshot it was given, so it sees one state
/// of the Vault from its first id to its last, whatever is committed
/// meanwhile. It lists one tree at a time, only when the walk reaches it,
/// and sorts that tree alone. No blob is read to find an id; [`Walk::read`]
/// reads the blob of the current one. A directory whose name is not a slug,
/// a name that starts with `.`, and a symlink are not part of a collection
/// and are passed over. A `singleton` yields its one id when its file
/// exists. A collection with no directory in the commit yields nothing.
///
/// A walk borrows its snapshot and declaration. A stream that must own them
/// keeps a [`WalkState`] next to them and passes them at each step.
pub(crate) struct Walk<'a> {
    snap: &'a Snap,
    decl: &'a Declaration,
    encryption: Option<&'a Encryption>,
    state: WalkState,
}

impl<'a> Walk<'a> {
    /// Starts a walk of `decl` over `snap`. A symlink on the way to the
    /// collection directory is `INVALID_ARGUMENT`; a `journal` collection is
    /// too.
    pub(crate) fn new(
        snap: &'a Snap,
        decl: &'a Declaration,
        encryption: Option<&'a Encryption>,
    ) -> Result<Walk<'a>, Fault> {
        Ok(Walk {
            snap,
            decl,
            encryption,
            state: WalkState::new(snap, decl)?,
        })
    }

    /// Moves to the next Document and returns its id, or `None` at the end.
    /// After a failure the walk is at its end.
    pub(crate) fn next_id(&mut self) -> Result<Option<&str>, Fault> {
        self.state.next_id(self.snap, self.decl)
    }

    /// The id the walk is at, if it is at one.
    pub(crate) fn id(&self) -> Option<&str> {
        self.state.id()
    }

    /// Reads the Document the walk is at, as [`read`] does by id but
    /// without checking the id again: `None` when the walk is at no
    /// Document.
    pub(crate) fn read(&self) -> Result<Option<Vec<u8>>, Fault> {
        let Some(id) = self.state.id() else {
            return Ok(None);
        };
        let rel = isolated::vault_rel(self.decl, id);
        read_file(self.snap, &rel, self.decl, self.encryption)
    }
}

/// Where a walk of the ids of a collection is, apart from the snapshot and
/// the declaration it walks: the trees it is inside and the entries of each
/// not yet visited. It owns all of it, so a stream can keep it; every step
/// is given the same snapshot and declaration it was started with.
pub(crate) struct WalkState {
    stack: Vec<Level>,
    current: Option<String>,
    /// The id of the blob of the current Document.
    current_blob: Option<Oid>,
    /// Ids up to and including this one are passed over.
    after: Option<String>,
    started: bool,
    done: bool,
}

impl WalkState {
    /// Starts a walk of `decl` over `snap`, as [`Walk::new`] does.
    pub(crate) fn new(snap: &Snap, decl: &Declaration) -> Result<WalkState, Fault> {
        require_files(decl)?;
        snap.lookup(&isolated::vault_rel(decl, ""))?;
        Ok(WalkState {
            stack: Vec::new(),
            current: None,
            current_blob: None,
            after: None,
            started: false,
            done: false,
        })
    }

    /// Begins the walk after the id `after`, which need not exist: the
    /// first id is the first one that sorts after it. A tree whose ids all
    /// sort before it is not listed.
    pub(crate) fn starting_after(mut self, after: String) -> WalkState {
        self.after = Some(after);
        self
    }

    /// Moves to the next Document and returns its id, or `None` at the end.
    /// After a failure the walk is at its end.
    pub(crate) fn next_id(
        &mut self,
        snap: &Snap,
        decl: &Declaration,
    ) -> Result<Option<&str>, Fault> {
        self.current = None;
        self.current_blob = None;
        if self.done {
            return Ok(None);
        }
        match self.advance(snap, decl) {
            Ok(Some((id, blob))) => {
                self.current = Some(id);
                self.current_blob = Some(blob);
                Ok(self.current.as_deref())
            }
            Ok(None) => {
                self.done = true;
                Ok(None)
            }
            Err(fault) => {
                self.done = true;
                Err(fault)
            }
        }
    }

    /// The id the walk is at, if it is at one.
    pub(crate) fn id(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// The id of the blob that holds the Document the walk is at: its
    /// revision, known from the tree without reading the blob.
    pub(crate) fn blob(&self) -> Option<Oid> {
        self.current_blob
    }

    /// Whether the ids below the directory whose ids begin with `prefix`
    /// (it ends in `/`) all sort at or before the id the walk starts after.
    fn passed(&self, prefix: &str) -> bool {
        self.after
            .as_deref()
            .is_some_and(|after| after > prefix && !after.starts_with(prefix))
    }

    fn advance(&mut self, snap: &Snap, decl: &Declaration) -> Result<Option<(String, Oid)>, Fault> {
        if decl.kind() == Kind::Singleton {
            return self.advance_singleton(snap, decl);
        }
        if !self.started {
            self.started = true;
            if let Some(level) = Self::list(snap, decl, String::new())? {
                self.stack.push(level);
            }
        }
        loop {
            let Some(level) = self.stack.last_mut() else {
                return Ok(None);
            };
            let Some(entry) = level.entries.next() else {
                self.stack.pop();
                continue;
            };
            let id = format!("{}{}", level.prefix, entry.name);
            if entry.dir {
                let prefix = format!("{id}/");
                if self.passed(&prefix) {
                    continue;
                }
                if let Some(level) = Self::list(snap, decl, prefix)? {
                    self.stack.push(level);
                }
            } else if self
                .after
                .as_deref()
                .is_none_or(|after| id.as_str() > after)
            {
                return Ok(Some((id, entry.node)));
            }
        }
    }

    fn advance_singleton(
        &mut self,
        snap: &Snap,
        decl: &Declaration,
    ) -> Result<Option<(String, Oid)>, Fault> {
        if self.started {
            return Ok(None);
        }
        self.started = true;
        let Some(id) = decl.singleton_id() else {
            return Ok(None);
        };
        if self.after.as_deref().is_some_and(|after| id <= after) {
            return Ok(None);
        }
        let rel = isolated::vault_rel(decl, id);
        match snap.lookup(&rel) {
            Ok(Some(node)) if node.kind == NodeKind::File => Ok(Some((id.to_owned(), node.id))),
            Ok(_) => Ok(None),
            // The file itself is a symlink: not a Document.
            Err(fault) if fault.path() == Some(rel.as_str()) => Ok(None),
            Err(fault) => Err(fault),
        }
    }

    /// Lists the tree `prefix` below the collection directory (`""` is the
    /// collection directory itself), sorted for the walk. `None` when the
    /// commit holds no such tree or nothing the walk visits in it.
    fn list(snap: &Snap, decl: &Declaration, prefix: String) -> Result<Option<Level>, Fault> {
        let rel = isolated::vault_rel(decl, &prefix);
        let Some(children) = snap.list(&rel)? else {
            return Ok(None);
        };
        let mut entries = Vec::new();
        for child in children {
            if child.name.starts_with('.') {
                continue;
            }
            match child.node.kind {
                NodeKind::Dir => {
                    if require_slug("id", &child.name).is_ok() {
                        entries.push(Entry {
                            name: child.name,
                            dir: true,
                            node: child.node.id,
                        });
                    }
                }
                NodeKind::File => entries.push(Entry {
                    name: child.name,
                    dir: false,
                    node: child.node.id,
                }),
                NodeKind::Link | NodeKind::Other => {}
            }
        }
        if entries.is_empty() {
            return Ok(None);
        }
        // A directory sorts as its name and a `/`: every id below it begins
        // with that, so the order of the walk is the order of the whole ids.
        entries.sort_by_cached_key(|entry| {
            if entry.dir {
                format!("{}/", entry.name)
            } else {
                entry.name.clone()
            }
        });
        Ok(Some(Level {
            prefix,
            entries: entries.into_iter(),
        }))
    }
}

/// Checks the Document files of every `standard` and `singleton`
/// collection, as the commits of `views` hold them, and collects what is
/// wrong, in collection and id order:
///
/// - a symlink under the collection directory (`INVALID_ARGUMENT`, with the
///   link's path);
/// - a file whose name is not a valid id (`INVALID_ARGUMENT`);
/// - in an encrypted collection, a file that will not open with the secret
///   of the handle (`CRYPTO`).
///
/// The content is never checked, beyond decryption: a Document is bytes
/// (D20). The call fails only when it cannot run: an I/O failure, or an
/// encrypted collection when the handle has no secret (`CRYPTO`) or the
/// build has no `encrypt` feature (`UNIMPLEMENTED`), because then its files
/// cannot be opened to be checked.
pub(crate) fn check_documents(
    views: &Views,
    declarations: &Declarations,
    encryption: Option<&Encryption>,
) -> Result<Vec<Problem>, Error> {
    let mut problems = Vec::new();
    for decl in declarations.iter() {
        if decl.kind() == Kind::Journal {
            continue;
        }
        if decl.encrypted() {
            require_openable(decl, encryption)?;
        }
        let snap = views.of(decl.repo());
        let dir_rel = isolated::vault_rel(decl, "");
        let dir_rel = dir_rel.trim_end_matches('/');
        let links = snap
            .symlinks_below(dir_rel)
            .context("check_documents: look for symlinks")?;
        let blocked = links
            .iter()
            .any(|link| dir_rel == link || dir_rel.starts_with(&format!("{link}/")));
        for link in links {
            let message = format!("{link} is a symlink");
            problems.push(problem(
                VaultErrorCode::InvalidArgument,
                Some(&link),
                message,
            ));
        }
        if blocked {
            continue;
        }
        check_files(snap, decl, encryption, &mut problems)
            .context("check_documents: check the files")?;
    }
    Ok(problems)
}

fn check_files(
    snap: &Snap,
    decl: &Declaration,
    encryption: Option<&Encryption>,
    problems: &mut Vec<Problem>,
) -> Result<(), Fault> {
    let mut walk = Walk::new(snap, decl, encryption)?;
    while walk.next_id()?.is_some() {
        let Some(id) = walk.id() else { break };
        let rel = isolated::vault_rel(decl, id);
        if let Err(fault) = path_of(decl, id) {
            problems.push(problem_of(&fault.with_path(rel)));
            continue;
        }
        if decl.encrypted() {
            match walk.read() {
                Ok(_) => {}
                Err(fault) if fault.vault_code() == VaultErrorCode::Crypto => {
                    problems.push(problem_of(&fault));
                }
                Err(fault) => return Err(fault),
            }
        }
    }
    Ok(())
}

/// Fails when the files of the encrypted collection `decl` cannot be opened
/// with `encryption`.
fn require_openable(decl: &Declaration, encryption: Option<&Encryption>) -> Result<(), Error> {
    #[cfg(feature = "encrypt")]
    {
        let has_secret = encryption.is_some_and(|encryption| encryption.secret.is_some());
        if has_secret {
            Ok(())
        } else {
            Err(Fault::new(
                VaultErrorCode::Crypto,
                format!(
                    "the encrypted collection {} was not checked: the handle has no secret",
                    decl.name()
                ),
            )
            .into())
        }
    }
    #[cfg(not(feature = "encrypt"))]
    {
        let _ = encryption;
        Err(no_encrypt(decl).into())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use scheda_api::v1::{Encryption, VaultErrorCode};

    use super::{Walk, check_documents, encode, path_of, read};
    use crate::collection::{self, Declaration, Declarations};
    use crate::error::Error;
    use crate::repo::{Repos, Snap, Views, fixture};

    /// A temporary Vault directory that holds collection files and
    /// Document files.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-documents-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn declare(&self, name: &str, schema: &str) {
            let dir = self.0.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(".collection.yaml"),
                format!("name: {name}\nschema:\n{schema}"),
            )
            .unwrap();
        }

        fn write(&self, rel: &str, bytes: &[u8]) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }

        /// Commits what the files hold, in the Vault and in the nested
        /// repository when there is one: reads see commits only.
        fn commit(&self) {
            // The nested repository is never tracked by the Vault.
            self.write(".gitignore", b".scheda/\n.isolated/\n");
            fixture::commit_all(&self.0);
            if self.0.join(".isolated").is_dir() {
                fixture::commit_all(&self.0.join(".isolated"));
            }
        }

        fn declarations(&self) -> Declarations {
            self.commit();
            collection::load(&fixture::snap(&self.0)).unwrap()
        }

        /// A snapshot of the commit that holds the files now, in the
        /// repository `decl` lives in.
        fn snap(&self, decl: &Declaration) -> Snap {
            self.commit();
            Repos::new(&self.0).snapshot(decl.repo()).unwrap()
        }

        fn views(&self) -> Views {
            self.commit();
            Repos::new(&self.0).views().unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn decl(declarations: &Declarations, name: &str) -> Declaration {
        declarations.get(name).unwrap().clone()
    }

    /// A PNG header and chunks, then bytes that are not UTF-8, then every
    /// value of a byte.
    pub(crate) fn binary() -> Vec<u8> {
        let mut bytes = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H',
            b'D', b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xAE,
            0x42, 0x60, 0x82, 0xFF, 0xFE, 0xC3, 0x28,
        ];
        bytes.extend(0..=255u8);
        bytes
    }

    fn ids(walk: &mut Walk<'_>) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(id) = walk.next_id().unwrap() {
            out.push(id.to_owned());
        }
        out
    }

    fn code_of(error: impl Into<Error>) -> Option<VaultErrorCode> {
        let error: Error = error.into();
        [
            VaultErrorCode::InvalidArgument,
            VaultErrorCode::Crypto,
            VaultErrorCode::Unimplemented,
        ]
        .into_iter()
        .find(|code| error.is(*code))
    }

    #[test]
    fn paths_follow_the_id_and_the_kind() {
        let scratch = Scratch::new("paths");
        scratch.declare("notes", "  standard: {}\n");
        scratch.declare("diary", "  standard: {}\n  isolated: true\n");
        scratch.declare("inbox", "  singleton:\n    id: today.md\n");
        scratch.declare("log", "  journal: {}\n");
        let declarations = scratch.declarations();
        let notes = decl(&declarations, "notes");
        assert_eq!(
            path_of(&notes, "work/meeting.md").unwrap(),
            "notes/work/meeting.md"
        );
        assert_eq!(
            path_of(&decl(&declarations, "diary"), "a").unwrap(),
            ".isolated/diary/a"
        );
        let inbox = decl(&declarations, "inbox");
        assert_eq!(path_of(&inbox, "today.md").unwrap(), "inbox/today.md");
        for bad in ["", "../x", "a//b", "/a", "a/", "CON", ".hidden"] {
            assert_eq!(
                code_of(path_of(&notes, bad).unwrap_err()),
                Some(VaultErrorCode::InvalidArgument)
            );
        }
        assert_eq!(
            code_of(path_of(&inbox, "other").unwrap_err()),
            Some(VaultErrorCode::InvalidArgument)
        );
        let log = decl(&declarations, "log");
        assert_eq!(
            code_of(path_of(&log, "a").unwrap_err()),
            Some(VaultErrorCode::InvalidArgument)
        );
        assert_eq!(
            code_of(read(&scratch.snap(&log), &log, "a", None).unwrap_err()),
            Some(VaultErrorCode::InvalidArgument)
        );
    }

    #[test]
    fn png_and_non_utf8_bytes_round_trip_in_standard_and_singleton() {
        let scratch = Scratch::new("roundtrip");
        scratch.declare("notes", "  standard: {}\n");
        scratch.declare("inbox", "  singleton:\n    id: today\n");
        let declarations = scratch.declarations();
        let png = binary();
        assert!(std::str::from_utf8(&png).is_err());
        for (name, id) in [
            ("notes", "img/logo.png"),
            ("notes", "raw"),
            ("inbox", "today"),
        ] {
            let decl = decl(&declarations, name);
            assert_eq!(read(&scratch.snap(&decl), &decl, id, None).unwrap(), None);
            let bytes = encode(&decl, id, &png, None).unwrap();
            assert_eq!(&*bytes, png.as_slice());
            scratch.write(&path_of(&decl, id).unwrap(), &bytes);
            assert_eq!(
                read(&scratch.snap(&decl), &decl, id, None).unwrap(),
                Some(png.clone())
            );
        }
    }

    #[test]
    fn a_missing_document_and_a_directory_in_its_place_are_none() {
        let scratch = Scratch::new("absent");
        scratch.declare("notes", "  standard: {}\n");
        scratch.write("notes/a/b", b"x");
        let notes = decl(&scratch.declarations(), "notes");
        let snap = scratch.snap(&notes);
        assert_eq!(read(&snap, &notes, "missing", None).unwrap(), None);
        assert_eq!(read(&snap, &notes, "a", None).unwrap(), None);
        assert_eq!(read(&snap, &notes, "a/b/c", None).unwrap(), None);
        assert_eq!(
            read(&snap, &notes, "a/b", None).unwrap(),
            Some(b"x".to_vec())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_on_the_way_is_invalid_argument_with_its_path() {
        let scratch = Scratch::new("link");
        scratch.declare("notes", "  standard: {}\n");
        scratch.write("outside", b"x");
        std::os::unix::fs::symlink(scratch.0.join("outside"), scratch.0.join("notes/link"))
            .unwrap();
        let notes = decl(&scratch.declarations(), "notes");
        let fault = read(&scratch.snap(&notes), &notes, "link", None).unwrap_err();
        assert_eq!(fault.path(), Some("notes/link"));
        assert_eq!(code_of(fault), Some(VaultErrorCode::InvalidArgument));
    }

    #[cfg(feature = "encrypt")]
    #[test]
    fn an_encrypted_collection_round_trips_any_bytes_and_needs_the_secret() {
        use crate::crypto::{test_identity, test_passphrase};

        let scratch = Scratch::new("encrypted");
        scratch.declare("vault", "  standard: {}\n  encrypted: true\n");
        scratch.declare("one", "  singleton:\n    id: only\n  encrypted: true\n");
        let declarations = scratch.declarations();
        let png = binary();
        for (name, id, secret) in [
            ("vault", "img/a.png", test_passphrase("pass")),
            ("vault", "b", test_identity()),
            ("one", "only", test_identity()),
        ] {
            let decl = decl(&declarations, name);
            let bytes = encode(&decl, id, &png, Some(&secret)).unwrap();
            let path = path_of(&decl, id).unwrap();
            assert_ne!(&*bytes, png.as_slice());
            assert!(bytes.starts_with(b"-----BEGIN AGE ENCRYPTED FILE-----"));
            scratch.write(&path, &bytes);
            let snap = scratch.snap(&decl);
            assert_eq!(
                read(&snap, &decl, id, Some(&secret)).unwrap(),
                Some(png.clone())
            );
            assert_eq!(
                code_of(read(&snap, &decl, id, None).unwrap_err()),
                Some(VaultErrorCode::Crypto)
            );
            let wrong = test_passphrase("wrong");
            let fault = read(&snap, &decl, id, Some(&wrong)).unwrap_err();
            assert_eq!(fault.path(), Some(path.as_str()));
            assert_eq!(code_of(fault), Some(VaultErrorCode::Crypto));
            assert_eq!(
                code_of(encode(&decl, id, &png, None).unwrap_err()),
                Some(VaultErrorCode::Crypto)
            );
        }
    }

    #[cfg(not(feature = "encrypt"))]
    #[test]
    fn an_encrypted_collection_is_unimplemented_without_the_feature() {
        let scratch = Scratch::new("no-encrypt");
        scratch.declare("vault", "  standard: {}\n  encrypted: true\n");
        let decl = decl(&scratch.declarations(), "vault");
        scratch.write("vault/a", b"x");
        let secret = Encryption::default();
        assert_eq!(
            code_of(encode(&decl, "a", b"x", Some(&secret)).unwrap_err()),
            Some(VaultErrorCode::Unimplemented)
        );
        assert_eq!(
            code_of(read(&scratch.snap(&decl), &decl, "a", Some(&secret)).unwrap_err()),
            Some(VaultErrorCode::Unimplemented)
        );
        assert_eq!(
            code_of(check_documents(&scratch.views(), &scratch.declarations(), None).unwrap_err()),
            Some(VaultErrorCode::Unimplemented)
        );
    }

    #[test]
    fn the_walk_yields_ids_in_id_order_across_nested_directories() {
        let scratch = Scratch::new("walk");
        scratch.declare("notes", "  standard: {}\n");
        for rel in [
            "notes/b",
            "notes/a/x",
            "notes/a/y/z",
            "notes/a.md",
            "notes/a0",
            "notes/a/y.md",
            "notes/c/d/e/f",
            "notes/.hidden",
            "notes/-/under",
            "notes/Z",
        ] {
            scratch.write(rel, rel.as_bytes());
        }
        // A directory that is not a slug is not part of the collection.
        scratch.write("notes/ bad/q", b"q");
        let notes = decl(&scratch.declarations(), "notes");
        let snap = scratch.snap(&notes);
        let mut walk = Walk::new(&snap, &notes, None).unwrap();
        let walked = ids(&mut walk);
        let mut sorted = walked.clone();
        sorted.sort();
        assert_eq!(walked, sorted, "the walk is in the byte order of the ids");
        assert_eq!(
            walked,
            ["Z", "a.md", "a/x", "a/y.md", "a/y/z", "a0", "b", "c/d/e/f"].map(str::to_owned)
        );
        assert_eq!(walk.next_id().unwrap(), None);
        assert_eq!(walk.id(), None);
    }

    #[test]
    fn the_walk_reads_a_blob_only_when_asked_to_read_the_current_one() {
        let scratch = Scratch::new("lazy");
        scratch.declare("notes", "  standard: {}\n  encrypted: false\n");
        scratch.write("notes/a", b"first");
        scratch.write("notes/b", b"second");
        let notes = decl(&scratch.declarations(), "notes");
        let snap = scratch.snap(&notes);
        // The object of `a` is gone: listing it is no obstacle, reading it is.
        let id = snap.lookup("notes/a").unwrap().unwrap().id.to_string();
        std::fs::remove_file(scratch.0.join(".git/objects").join(&id[..2]).join(&id[2..])).unwrap();
        let mut walk = Walk::new(&snap, &notes, None).unwrap();
        assert_eq!(walk.read().unwrap(), None, "before the first id");
        assert_eq!(walk.next_id().unwrap(), Some("a"));
        assert!(walk.read().is_err());
        assert_eq!(walk.next_id().unwrap(), Some("b"));
        assert_eq!(walk.read().unwrap(), Some(b"second".to_vec()));
        assert_eq!(walk.next_id().unwrap(), None);
    }

    #[test]
    fn the_walk_reads_the_commit_it_started_on_whatever_is_committed_after() {
        let scratch = Scratch::new("deferred");
        scratch.declare("notes", "  standard: {}\n");
        scratch.write("notes/a/x", b"x");
        scratch.write("notes/b/y", b"y");
        let notes = decl(&scratch.declarations(), "notes");
        let snap = scratch.snap(&notes);
        let mut walk = Walk::new(&snap, &notes, None).unwrap();
        assert_eq!(walk.next_id().unwrap(), Some("a/x"));
        // A commit made after the walk started: a file added to a folder the
        // walk has not reached, one changed, one gone.
        scratch.write("notes/b/w", b"w");
        scratch.write("notes/a/x", b"changed");
        std::fs::remove_file(scratch.0.join("notes/b/y")).unwrap();
        scratch.commit();
        assert_eq!(walk.read().unwrap(), Some(b"x".to_vec()));
        assert_eq!(ids(&mut walk), ["b/y"]);
        assert_eq!(walk.read().unwrap(), None, "the walk is at its end");
        let again = scratch.snap(&notes);
        let mut next = Walk::new(&again, &notes, None).unwrap();
        assert_eq!(ids(&mut next), ["a/x", "b/w"]);
    }

    #[test]
    fn an_empty_or_missing_collection_directory_yields_nothing() {
        let scratch = Scratch::new("empty");
        scratch.declare("notes", "  standard: {}\n");
        scratch.declare("diary", "  standard: {}\n  isolated: true\n");
        scratch.declare("inbox", "  singleton:\n    id: only\n");
        let declarations = scratch.declarations();
        for name in ["notes", "diary", "inbox"] {
            let decl = decl(&declarations, name);
            let snap = scratch.snap(&decl);
            let mut walk = Walk::new(&snap, &decl, None).unwrap();
            assert_eq!(walk.next_id().unwrap(), None, "{name}");
        }
        std::fs::remove_dir_all(scratch.0.join("notes")).unwrap();
        let notes = decl(&declarations, "notes");
        let snap = scratch.snap(&notes);
        let mut walk = Walk::new(&snap, &notes, None).unwrap();
        assert_eq!(walk.next_id().unwrap(), None);
    }

    #[test]
    fn a_singleton_walk_yields_its_one_id_and_an_isolated_walk_its_own_tree() {
        let scratch = Scratch::new("kinds");
        scratch.declare("inbox", "  singleton:\n    id: only\n");
        scratch.declare("diary", "  standard: {}\n  isolated: true\n");
        scratch.write("inbox/only", b"1");
        scratch.write("inbox/stray", b"2");
        scratch.write(".isolated/diary/a/b", b"3");
        scratch.write("diary/not-here", b"4");
        let declarations = scratch.declarations();
        let inbox = decl(&declarations, "inbox");
        let snap = scratch.snap(&inbox);
        assert_eq!(ids(&mut Walk::new(&snap, &inbox, None).unwrap()), ["only"]);
        let diary = decl(&declarations, "diary");
        let snap = scratch.snap(&diary);
        let mut walk = Walk::new(&snap, &diary, None).unwrap();
        assert_eq!(walk.next_id().unwrap(), Some("a/b"));
        assert_eq!(walk.read().unwrap(), Some(b"3".to_vec()));
        assert_eq!(walk.next_id().unwrap(), None);
    }

    #[test]
    fn a_journal_cannot_be_walked() {
        let scratch = Scratch::new("journal");
        scratch.declare("log", "  journal: {}\n");
        let log = decl(&scratch.declarations(), "log");
        let snap = scratch.snap(&log);
        assert_eq!(
            code_of(Walk::new(&snap, &log, None).err().unwrap()),
            Some(VaultErrorCode::InvalidArgument)
        );
    }

    fn codes_and_paths(problems: &[scheda_api::v1::Problem]) -> Vec<(VaultErrorCode, String)> {
        problems
            .iter()
            .map(|p| {
                (
                    p.code.and_then(|code| code.as_known()).unwrap(),
                    p.path.clone().unwrap_or_default(),
                )
            })
            .collect()
    }

    #[test]
    fn a_binary_file_with_conflict_marker_lines_passes_the_check() {
        let scratch = Scratch::new("markers");
        scratch.declare("notes", "  standard: {}\n");
        let mut bytes = binary();
        bytes.extend_from_slice(b"\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> other\n");
        scratch.write("notes/img.png", &bytes);
        scratch.write("notes/text.md", b"<<<<<<< HEAD\nx\n>>>>>>> y\n");
        let problems = check_documents(&scratch.views(), &scratch.declarations(), None).unwrap();
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn the_check_collects_every_problem_it_finds() {
        let scratch = Scratch::new("collect");
        scratch.declare("notes", "  standard: {}\n");
        scratch.declare("diary", "  standard: {}\n  isolated: true\n");
        scratch.write("notes/ok", b"x");
        scratch.write("notes/ trailing", b"x");
        scratch.write("notes/nested/CON", b"x");
        scratch.write(".isolated/diary/fine", b"x");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(scratch.0.join("notes/ok"), scratch.0.join("notes/link"))
                .unwrap();
            std::os::unix::fs::symlink(
                scratch.0.join("notes/ok"),
                scratch.0.join(".isolated/diary/link2"),
            )
            .unwrap();
        }
        let problems = check_documents(&scratch.views(), &scratch.declarations(), None).unwrap();
        let mut expected = vec![
            (
                VaultErrorCode::InvalidArgument,
                ".isolated/diary/link2".to_owned(),
            ),
            (
                VaultErrorCode::InvalidArgument,
                "notes/ trailing".to_owned(),
            ),
            (VaultErrorCode::InvalidArgument, "notes/link".to_owned()),
            (
                VaultErrorCode::InvalidArgument,
                "notes/nested/CON".to_owned(),
            ),
        ];
        if cfg!(not(unix)) {
            expected.retain(|(_, path)| !path.contains("link"));
        }
        let mut got = codes_and_paths(&problems);
        got.sort_by(|a, b| a.1.cmp(&b.1));
        expected.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(got, expected);
    }

    #[cfg(feature = "encrypt")]
    #[test]
    fn the_check_opens_encrypted_files_and_reports_each_that_will_not_open() {
        use crate::crypto::test_passphrase;

        let scratch = Scratch::new("check-crypto");
        scratch.declare("vault", "  standard: {}\n  encrypted: true\n");
        let declarations = scratch.declarations();
        let vault = decl(&declarations, "vault");
        let right = test_passphrase("right");
        for id in ["a", "b"] {
            let plain = binary();
            let bytes = encode(&vault, id, &plain, Some(&right)).unwrap();
            scratch.write(&path_of(&vault, id).unwrap(), &bytes);
        }
        scratch.write("vault/plain", b"not armor");
        let views = scratch.views();
        assert!(
            check_documents(&views, &declarations, Some(&right))
                .unwrap()
                .len()
                == 1
        );
        let wrong = test_passphrase("wrong");
        let problems = check_documents(&views, &declarations, Some(&wrong)).unwrap();
        assert_eq!(
            codes_and_paths(&problems),
            [
                (VaultErrorCode::Crypto, "vault/a".to_owned()),
                (VaultErrorCode::Crypto, "vault/b".to_owned()),
                (VaultErrorCode::Crypto, "vault/plain".to_owned()),
            ]
        );
        // Without a secret the files cannot be opened: the check cannot run.
        let error = check_documents(&views, &declarations, None).unwrap_err();
        assert!(error.is(VaultErrorCode::Crypto));
        let none = Encryption::default();
        let error = check_documents(&views, &declarations, Some(&none)).unwrap_err();
        assert!(error.is(VaultErrorCode::Crypto));
    }
}
