//! A last read of an object, under the merge lock, for a reader whose own
//! retry lost a race with a merge of packs.
//!
//! The git library reads an object in two passes. It looks in the packs it
//! knows, loose objects, and the other backends; on a miss it lists the pack
//! folder once more and looks again. Listing registers a pack without
//! opening its index, and the index is opened at the first lookup. So a
//! reader can list the folder while an old pack still exists and the merged
//! pack is not there yet; a merge then places the new pack and removes the
//! old one; the one repeat opens the index of the old pack, finds it gone,
//! and the read fails although the object is in the new pack.
//!
//! [`add`] gives a database one more backend, below every other, that is
//! asked only after that repeat failed. It reads the object once more from a
//! fresh database over the same objects folder, holding
//! `{vault}/.scheda/pack.lock` shared. A merge holds that lock exclusively
//! while it removes packs, so under the shared lock no pack can be removed,
//! and every object a commit reaches is in some pack on disk, because a merge
//! places the new pack before it removes the old ones. The repeat therefore
//! finds the object, and a second miss is a missing object.
//!
//! The backend answers `read` and `read_header` only. It does not answer
//! `exists`: a false "absent" makes a write store an object again, which is
//! harmless, and an `exists` here would take the lock on every new object of
//! every write.

use std::cell::Cell;
use std::ffi::{CString, c_int, c_void};
use std::fs::File;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::ptr;

use git2::{Binding, Odb, Repository};
use libgit2_sys::{
    GIT_ODB_BACKEND_VERSION, git_object_t, git_odb, git_odb_backend, git_odb_object, git_oid,
};
use scheda_api::v1::VaultErrorCode;

use crate::error::{Fault, lib_fault};
use crate::isolated;

/// libgit2's "not found" and general error codes.
const NOT_FOUND: c_int = -3;
const FAILED: c_int = -1;

thread_local! {
    /// Whether this thread holds the merge lock exclusively.
    static EXCLUSIVE: Cell<bool> = const { Cell::new(false) };
}

/// Marks the thread as holding the merge lock exclusively until dropped. A
/// read that falls back on such a thread does not take the lock: no merge
/// can be removing packs while this thread holds it.
pub(crate) struct Exclusive {
    before: bool,
}

/// Starts the mark. The caller holds `pack.lock` exclusively and drops the
/// result before it lets the lock go.
pub(crate) fn exclusive() -> Exclusive {
    Exclusive {
        before: EXCLUSIVE.replace(true),
    }
}

impl Drop for Exclusive {
    fn drop(&mut self) {
        EXCLUSIVE.set(self.before);
    }
}

/// The state of one backend. `parent` is first: the library hands the
/// backend's address back to every callback, and it is also the address of
/// this struct.
#[repr(C)]
struct Fallback {
    parent: git_odb_backend,
    objects: CString,
    lock_file: PathBuf,
    /// Set by `refresh`, which the library calls after a miss; the next
    /// `read` or `read_header` takes it. A backend that was not refreshed is
    /// being asked in the first pass and says "not found".
    armed: bool,
}

/// The path of the merge lock of the Vault `repo` belongs to. The Vault is
/// the parent of the repository's `.git` folder, or the parent of that
/// folder's own parent when the repository is the nested one in
/// `.isolated/`. It does not depend on a work tree: a repository without
/// one (`core.bare`) gets the same lock file. `None` for a repository whose
/// `.git` is not a folder under a Vault.
pub(crate) fn lock_file_of(repo: &Repository) -> Option<PathBuf> {
    let git = repo.commondir();
    if git.file_name().is_none_or(|name| name != ".git") {
        return None;
    }
    let work = git.parent()?;
    let root = if work.file_name().is_some_and(|name| name == isolated::DIR) {
        work.parent()?
    } else {
        work
    };
    Some(root.join(".scheda").join("pack.lock"))
}

/// Adds the fallback to the database of `repo` (a repository opened for a
/// lease). A repository that is not a `.git` folder of a Vault gets none.
///
/// # Errors
///
/// `STORAGE`, `DATA_LOSS`, or `REPLICATION` when the library fails.
pub(crate) fn register(repo: &Repository) -> Result<(), Fault> {
    let Some(lock_file) = lock_file_of(repo) else {
        return Ok(());
    };
    let odb = repo.odb().map_err(lib_fault)?;
    add(&odb, &repo.commondir().join("objects"), &lock_file)
}

/// Adds the fallback to `odb`, reading the objects folder `objects` under
/// the lock file `lock_file`, at priority 0: below the packs (2), the loose
/// objects (1), and the write overlay's memory (1000).
///
/// # Errors
///
/// `STORAGE` when the path holds a NUL byte, otherwise the library's.
pub(crate) fn add(odb: &Odb<'_>, objects: &Path, lock_file: &Path) -> Result<(), Fault> {
    let objects = CString::new(objects.as_os_str().as_encoded_bytes()).map_err(|_| {
        Fault::new(
            VaultErrorCode::Storage,
            "the path of the object database holds a NUL byte",
        )
    })?;
    // SAFETY: `git_odb_backend` is plain data: integers, a null-able
    // pointer, and optional function pointers, for which all-zero bytes
    // are a valid value.
    let mut parent: git_odb_backend = unsafe { std::mem::zeroed() };
    // SAFETY: `parent` is a live, writable backend struct.
    let status =
        unsafe { libgit2_sys::git_odb_init_backend(&raw mut parent, GIT_ODB_BACKEND_VERSION) };
    if status < 0 {
        return Err(lib_fault(git2::Error::last_error(status)));
    }
    parent.read = Some(read);
    parent.read_header = Some(read_header);
    parent.refresh = Some(refresh);
    parent.free = Some(free);
    let raw = Box::into_raw(Box::new(Fallback {
        parent,
        objects,
        lock_file: lock_file.to_path_buf(),
        armed: false,
    }));
    // SAFETY: `odb.raw()` is the database `odb` keeps alive through the
    // call; `raw` is a heap `Fallback` that starts with its backend struct.
    // On success the database owns it and calls `free`.
    let status = unsafe { libgit2_sys::git_odb_add_backend(odb.raw(), raw.cast(), 0) };
    if status < 0 {
        // SAFETY: the database did not take `raw`, so it is still ours.
        drop(unsafe { Box::from_raw(raw) });
        return Err(lib_fault(git2::Error::last_error(status)));
    }
    Ok(())
}

/// Runs `body`, answering `FAILED` if it panics: a panic must not unwind
/// into the library.
fn guarded(body: impl FnOnce() -> c_int) -> c_int {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or(FAILED)
}

/// The shared lock on the lock file, let go on drop.
struct Shared(File);

impl Drop for Shared {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Takes the lock file shared, waiting for a merge. No file means no merge
/// ever ran: a merge that removed a pack before the retry that failed would
/// have made the file first, so there is nothing to wait for.
fn lock_shared(path: &Path) -> std::io::Result<Option<Shared>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    file.lock_shared()?;
    Ok(Some(Shared(file)))
}

/// Opens a fresh database over the objects folder under the shared lock and
/// runs `work` on it. `NOT_FOUND` when the backend is not armed.
///
/// # Safety
///
/// `backend` is a pointer the library gave to a callback of a [`Fallback`].
unsafe fn with_fresh_odb(
    backend: *mut git_odb_backend,
    work: impl FnOnce(*mut git_odb) -> c_int,
) -> c_int {
    let this = backend.cast::<Fallback>();
    // SAFETY: the library calls a backend under the lock of its database, so
    // nothing else touches the struct; it lives until `free`.
    let armed = unsafe { std::mem::take(&mut (*this).armed) };
    if !armed {
        return NOT_FOUND;
    }
    // SAFETY: as above; the shared borrows end before `work` runs.
    let (objects, lock_file) = unsafe { ((*this).objects.as_ptr(), &(*this).lock_file) };
    #[cfg(test)]
    tests::count(lock_file);
    let _lock = if EXCLUSIVE.get() {
        None
    } else {
        match lock_shared(lock_file) {
            Ok(lock) => lock,
            Err(err) => {
                set_error(&format!("lock the packs for a last read: {err}"));
                return FAILED;
            }
        }
    };
    let mut odb: *mut git_odb = ptr::null_mut();
    // SAFETY: `odb` is a live out pointer; `objects` is NUL-terminated and
    // owned by the backend.
    let status = unsafe { libgit2_sys::git_odb_open(&raw mut odb, objects) };
    if status < 0 {
        return status;
    }
    let status = work(odb);
    // SAFETY: `odb` came from `git_odb_open` and `work` kept nothing of it.
    unsafe { libgit2_sys::git_odb_free(odb) };
    status
}

/// Gives the library an error message for a failure that is not its own.
fn set_error(message: &str) {
    if let Ok(text) = CString::new(message) {
        // SAFETY: `text` is NUL-terminated; the library copies it.
        unsafe {
            libgit2_sys::git_error_set_str(libgit2_sys::GIT_ERROR_ODB as c_int, text.as_ptr());
        }
    }
}

extern "C" fn refresh(backend: *mut git_odb_backend) -> c_int {
    guarded(|| {
        // SAFETY: see `with_fresh_odb`.
        unsafe { (*backend.cast::<Fallback>()).armed = true };
        0
    })
}

extern "C" fn read(
    data: *mut *mut c_void,
    len: *mut usize,
    kind: *mut git_object_t,
    backend: *mut git_odb_backend,
    id: *const git_oid,
) -> c_int {
    guarded(|| {
        // SAFETY: the library passes its own backend.
        unsafe {
            with_fresh_odb(backend, |odb| {
                let mut object: *mut git_odb_object = ptr::null_mut();
                let status = libgit2_sys::git_odb_read(&raw mut object, odb, id);
                if status < 0 {
                    return status;
                }
                let size = libgit2_sys::git_odb_object_size(object);
                let buffer = libgit2_sys::git_odb_backend_data_alloc(backend, size);
                if buffer.is_null() {
                    libgit2_sys::git_odb_object_free(object);
                    return FAILED;
                }
                ptr::copy_nonoverlapping(
                    libgit2_sys::git_odb_object_data(object).cast::<u8>(),
                    buffer.cast::<u8>(),
                    size,
                );
                *data = buffer;
                *len = size;
                *kind = libgit2_sys::git_odb_object_type(object);
                libgit2_sys::git_odb_object_free(object);
                0
            })
        }
    })
}

extern "C" fn read_header(
    len: *mut usize,
    kind: *mut git_object_t,
    backend: *mut git_odb_backend,
    id: *const git_oid,
) -> c_int {
    guarded(|| {
        // SAFETY: the library passes its own backend and live out pointers.
        unsafe {
            with_fresh_odb(backend, |odb| {
                libgit2_sys::git_odb_read_header(len, kind, odb, id)
            })
        }
    })
}

extern "C" fn free(backend: *mut git_odb_backend) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `add` made the backend with `Box::new` and handed the
        // database the only pointer; the database frees it once.
        drop(unsafe { Box::from_raw(backend.cast::<Fallback>()) });
    }));
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use super::*;
    use crate::pack::tests::{Scratch, noise, write};
    use crate::pack::{Limits, Merges, Step, merge_due};
    use crate::repo::{Repo, Repos};

    static READS: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

    /// Counts a fallback read under `lock_file`.
    pub(super) fn count(lock_file: &Path) {
        let mut all = READS.lock().unwrap_or_else(PoisonError::into_inner);
        *all.get_or_insert_with(HashMap::new)
            .entry(lock_file.to_path_buf())
            .or_default() += 1;
    }

    /// The fallback reads made under `lock_file` so far.
    fn reads(lock_file: &Path) -> usize {
        let all = READS.lock().unwrap_or_else(PoisonError::into_inner);
        all.as_ref()
            .and_then(|all| all.get(lock_file))
            .copied()
            .unwrap_or(0)
    }

    /// A test backend that runs a merge of all packs when the library
    /// refreshes it for the first time.
    #[repr(C)]
    struct Intervention {
        parent: git_odb_backend,
        dir: PathBuf,
        done: bool,
    }

    extern "C" fn intervene(backend: *mut git_odb_backend) -> c_int {
        guarded(|| {
            // SAFETY: the library passes the backend `intervention` made.
            let this = unsafe { &mut *backend.cast::<Intervention>() };
            if std::mem::take(&mut this.done) {
                return 0;
            }
            this.done = true;
            let repo = Repository::open(&this.dir).unwrap();
            let merges = Merges::new(Limits { due: 2, hard: 32 }, Arc::default());
            let cancel = AtomicBool::new(false);
            while merge_due(&repo, &merges, &cancel).unwrap() == Step::Merged {}
            0
        })
    }

    extern "C" fn pass(
        _: *mut *mut c_void,
        _: *mut usize,
        _: *mut git_object_t,
        _: *mut git_odb_backend,
        _: *const git_oid,
    ) -> c_int {
        NOT_FOUND
    }

    extern "C" fn drop_intervention(backend: *mut git_odb_backend) {
        // SAFETY: made by `intervention` with `Box::new`.
        drop(unsafe { Box::from_raw(backend.cast::<Intervention>()) });
    }

    /// Adds the intervention to `odb` at priority 1.
    fn intervention(odb: &Odb<'_>, dir: &Path) {
        // SAFETY: plain data, all-zero is valid.
        let mut parent: git_odb_backend = unsafe { std::mem::zeroed() };
        // SAFETY: a live struct.
        let status =
            unsafe { libgit2_sys::git_odb_init_backend(&raw mut parent, GIT_ODB_BACKEND_VERSION) };
        assert!(status >= 0);
        parent.read = Some(pass);
        parent.refresh = Some(intervene);
        parent.free = Some(drop_intervention);
        let raw = Box::into_raw(Box::new(Intervention {
            parent,
            dir: dir.to_path_buf(),
            done: false,
        }));
        // SAFETY: the database takes the heap struct and frees it.
        let status = unsafe { libgit2_sys::git_odb_add_backend(odb.raw(), raw.cast(), 1) };
        assert!(status >= 0);
    }

    fn idx_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir.join(".git/objects/pack"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".idx"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_lock_file_is_the_vaults_for_both_modes_and_both_repositories() {
        let scratch = Scratch::new("lock-file-of");
        crate::repo::init_library().unwrap();
        let root = scratch.0.canonicalize().unwrap();
        for (dir, no_files) in [("with", false), ("without", true)] {
            let vault = root.join(dir);
            crate::repo::init_repository(&vault, no_files).unwrap();
            crate::repo::init_repository(&vault.join(".isolated"), no_files).unwrap();
            let want = vault.join(".scheda/pack.lock");
            for repo_dir in [vault.clone(), vault.join(".isolated")] {
                let repo = Repository::open(&repo_dir).unwrap();
                assert_eq!(repo.is_bare(), no_files);
                assert_eq!(lock_file_of(&repo), Some(want.clone()), "{repo_dir:?}");
            }
        }
    }

    fn vault(name: &str) -> (Scratch, Repository) {
        let scratch = Scratch::new(name);
        crate::repo::init_library().unwrap();
        let repo = Repository::init(&scratch.0).unwrap();
        std::fs::create_dir_all(scratch.0.join(".scheda")).unwrap();
        std::fs::write(scratch.0.join(".scheda/pack.lock"), b"").unwrap();
        (scratch, repo)
    }

    /// Reads a blob a merge replaced the pack of, in the window between
    /// the library's listing and its lookup. Returns the read and whether
    /// the pack of the blob is gone afterwards.
    fn window(
        name: &str,
        with_fallback: bool,
    ) -> (Result<Vec<u8>, git2::ErrorCode>, bool, Vec<u8>) {
        let (scratch, repo) = vault(name);
        let dir = &scratch.0;
        let c0 = write(&repo, "a", &noise(1, 300));
        let reader = Repository::open(dir).unwrap();
        if with_fallback {
            register(&reader).unwrap();
        }
        // Opens the index of the first pack.
        reader.find_commit(c0).unwrap().tree().unwrap();
        // The library shares an opened pack between the handles of a
        // process that hold it, so the handle that writes the new pack is
        // let go before the reader looks: the reader lists the pack and
        // opens its index at the lookup, as a reader of another process
        // does.
        {
            let writer = Repository::open(dir).unwrap();
            write(&writer, "b", &noise(2, 300));
        }
        let x = git2::Oid::hash_object(git2::ObjectType::Blob, &noise(2, 300)).unwrap();
        let before = idx_names(dir);
        assert_eq!(before.len(), 2);
        let odb = reader.odb().unwrap();
        intervention(&odb, dir);
        let read = odb
            .read(x)
            .map(|object| object.data().to_vec())
            .map_err(|err| err.code());
        let after = idx_names(dir);
        let merged = after.len() == 1 && before.iter().all(|name| !after.contains(name));
        (read, merged, noise(2, 300))
    }

    #[test]
    fn a_merge_inside_the_window_fails_the_read_without_the_fallback_and_not_with_it() {
        let (plain, merged, _) = window("fallback-plain", false);
        assert!(merged, "the merge did not run inside the window");
        assert_eq!(plain.unwrap_err(), git2::ErrorCode::NotFound);

        let (guarded, merged, bytes) = window("fallback-armed", true);
        assert!(merged, "the merge did not run inside the window");
        assert_eq!(guarded.unwrap(), bytes);
    }

    #[test]
    fn an_id_no_pack_holds_is_not_found_through_the_fallback() {
        let (scratch, repo) = vault("fallback-missing");
        write(&repo, "a", &noise(1, 300));
        let reader = Repository::open(&scratch.0).unwrap();
        register(&reader).unwrap();
        let id = git2::Oid::from_str("1234567890123456789012345678901234567890").unwrap();
        let err = reader.odb().unwrap().read(id).err().unwrap();
        assert_eq!(err.code(), git2::ErrorCode::NotFound);
        assert_eq!(reads(&scratch.0.join(".scheda/pack.lock")), 1);
    }

    #[test]
    fn the_normal_path_never_reads_through_the_fallback() {
        let (scratch, repo) = vault("fallback-normal");
        let commit = write(&repo, "a", &noise(1, 300));
        write(&repo, "b", &noise(2, 300));
        let repos = Repos::new(&scratch.0);
        let lease = repos.lease(Repo::Main).unwrap().unwrap();
        let tree = lease.find_commit(commit).unwrap().tree().unwrap();
        let blob = tree.get_name("a").unwrap().id();
        assert_eq!(lease.find_blob(blob).unwrap().content(), noise(1, 300));
        assert_eq!(reads(&scratch.0.join(".scheda/pack.lock")), 0);
    }

    #[test]
    fn a_miss_on_the_thread_that_holds_the_merge_lock_does_not_wait() {
        let (scratch, repo) = vault("fallback-held");
        write(&repo, "a", &noise(1, 300));
        let lock_path = scratch.0.join(".scheda/pack.lock");
        // Held by this test, so that a shared lock would wait.
        let held = File::open(&lock_path).unwrap();
        held.lock().unwrap();
        let dir = scratch.0.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let reader = Repository::open(&dir).unwrap();
            register(&reader).unwrap();
            let _mark = exclusive();
            let id = git2::Oid::from_str("1234567890123456789012345678901234567890").unwrap();
            let code = reader.odb().unwrap().read(id).err().map(|err| err.code());
            let _ = tx.send(code);
        });
        let code = rx.recv_timeout(Duration::from_secs(20));
        // Let go either way: a thread that waited can end.
        drop(held);
        let _ = thread.join();
        assert_eq!(code.unwrap(), Some(git2::ErrorCode::NotFound));
    }
}
