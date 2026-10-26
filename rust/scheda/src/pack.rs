//! The packs of a repository: one pack for each write, and the background
//! merge that keeps their number bounded.
//!
//! A write stores exactly its own new objects (the blobs of its Documents,
//! the trees on their paths, and the commit) as one pack, complete on disk
//! before the branch moves: [`Overlay`] builds the commit in an object
//! database in memory that reads the repository's, then streams only the
//! new objects through the library's pack builder into the repository's
//! pack writer. Nothing is left loose.
//!
//! The packs pile up, one a write. A [`Packer`], one task for each open
//! Vault, merges them in the background, off the write lock, under
//! `{vault}/.scheda/pack.lock`. [`plan`] is the rule, a pure function of
//! the sizes of the packs: geometric, factor 2, the split rule of
//! `git repack --geometric`, among the packs below a cap. A pack at the cap
//! is never merged again, so a merge reads and writes a bounded amount.
//! The loose objects a person's `git` commands leave join a merge.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs;
use std::io::{BufReader, Read, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use git2::{Binding, ObjectType, Odb, OdbLookupFlags, Oid, PackBuilder, Repository};
use scheda_api::v1::VaultErrorCode;
use scheda_blocking::{Blocking, blocking};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::commit::{open_lock_file, open_lock_file_at};
use crate::error::{Error, Fault, io, lib_fault};
use crate::fallback;
use crate::fs::Flock;
use crate::repo::{Repo, Repos};
use crate::sha1::Sha1;

/// A pack file of this many bytes or more is at the cap.
pub(crate) const CAP_BYTES: u64 = 64 * 1024 * 1024;

/// A pack of this many objects or more is at the cap. The merge of such a
/// pack costs about 500 bytes of memory an object.
pub(crate) const CAP_OBJECTS: u64 = 100_000;

/// A merge is due when this many packs are below the cap.
pub(crate) const DUE_PACKS: usize = 16;

/// A write merges first when it would leave this many packs below the cap.
pub(crate) const HARD_PACKS: usize = 32;

/// Each pack of a progression is at least this many times the one before.
const FACTOR: u64 = 2;

/// Loose objects alone cause a merge when there are more than this: a
/// directory with tens of thousands of entries slows every lookup, and the
/// person's own `git` lists them.
pub(crate) const DUE_LOOSE: usize = 1000;

/// The name of the merge lock's file under `.scheda/`.
const LOCK_FILE: &str = "pack.lock";

/// The packs of an object directory are not merged while a multi-pack
/// index names them: the library trusts the index and would not look for a
/// deleted pack's objects elsewhere. Scheda never writes one; a person's
/// `git multi-pack-index write` does.
const MIDX: &str = "multi-pack-index";

/// How many packs below the cap a Vault lets pile up.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Limits {
    /// A merge is due when this many packs are below the cap.
    pub(crate) due: usize,
    /// The packs below the cap number fewer than this at every moment: a
    /// write that would leave this many merges first.
    pub(crate) hard: usize,
}

impl Limits {
    /// The limits of a Vault.
    pub(crate) const DEFAULT: Limits = Limits {
        due: DUE_PACKS,
        hard: HARD_PACKS,
    };
}

/// The size of a pack, what [`plan`] looks at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Size {
    /// Bytes of the pack file.
    pub(crate) bytes: u64,
    /// Objects in the pack.
    pub(crate) objects: u64,
}

impl Size {
    /// Whether a merge may still take the pack.
    fn below_cap(&self) -> bool {
        self.bytes < CAP_BYTES && self.objects < CAP_OBJECTS
    }
}

/// What one merge takes.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Plan {
    /// The indexes of the packs to merge, smallest first.
    pub(crate) packs: Vec<usize>,
    /// Whether the loose objects join the merge.
    pub(crate) loose: bool,
}

impl Plan {
    /// Whether there is nothing to merge.
    pub(crate) fn is_empty(&self) -> bool {
        self.packs.is_empty() && !self.loose
    }
}

/// The merge rule. `packs` are the sizes of the packs a merge may take,
/// `loose` the number of loose objects.
///
/// Nothing is due while fewer than `limits.due` packs are below the cap.
/// Then the packs below the cap, smallest first, are split the way
/// `git repack --geometric=2` does it: from the largest down, the packs that
/// are each at least twice the one before stay; the first that is not, and
/// every pack under it, are merged, and so is each larger pack as long as
/// twice the size of what is merged is more than it. A pack at the cap is
/// never taken, and a merge stops taking packs once what it has taken
/// reaches the cap, so it ends at most one pack past it. The loose objects
/// join a merge, and alone cause one when there are more than
/// [`DUE_LOOSE`].
pub(crate) fn plan(packs: &[Size], loose: usize, limits: &Limits) -> Plan {
    let mut below: Vec<usize> = (0..packs.len())
        .filter(|&at| packs[at].below_cap())
        .collect();
    let mut taken = Vec::new();
    if below.len() >= limits.due {
        below.sort_by_key(|&at| (packs[at].bytes, at));
        let sizes: Vec<u64> = below.iter().map(|&at| packs[at].bytes.max(1)).collect();
        let broken = (1..sizes.len())
            .rev()
            .find(|&at| sizes[at] < FACTOR.saturating_mul(sizes[at - 1]));
        if let Some(broken) = broken {
            let mut end = broken + 1;
            let mut sum: u64 = sizes[..end].iter().sum();
            while end < sizes.len() && FACTOR.saturating_mul(sum) > sizes[end] {
                sum = sum.saturating_add(sizes[end]);
                end += 1;
            }
            taken = below[..end].to_vec();
            // Smallest first, while what is taken is below the cap.
            let (mut bytes, mut objects) = (0u64, 0u64);
            let mut fit = 0;
            for &at in &taken {
                if bytes >= CAP_BYTES || objects >= CAP_OBJECTS {
                    break;
                }
                bytes = bytes.saturating_add(packs[at].bytes);
                objects = objects.saturating_add(packs[at].objects);
                fit += 1;
            }
            taken.truncate(fit);
        }
    }
    let loose = if taken.is_empty() {
        loose > DUE_LOOSE
    } else {
        loose > 0
    };
    Plan {
        packs: taken,
        loose,
    }
}

/// Streams the pack `builder` makes into the pack writer of `into`, and
/// commits it: the library writes the index and flushes the pack, its
/// index, and their folder before this returns. Returns the name of the
/// pack (`pack-{hash}`, the hash being the trailer of the stream), or `None`
/// when `cancel` was set: nothing is left then.
fn stream(
    builder: &mut PackBuilder<'_>,
    into: &Repository,
    cancel: Option<&AtomicBool>,
) -> Result<Option<String>, Fault> {
    let odb: Odb<'_> = into.odb().map_err(lib_fault)?;
    let mut writer = odb.packwriter().map_err(lib_fault)?;
    let mut tail: Vec<u8> = Vec::with_capacity(64);
    let mut stopped = false;
    let mut failed: Option<std::io::Error> = None;
    let streamed = builder.foreach(|chunk| {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            stopped = true;
            return false;
        }
        if let Err(err) = writer.write_all(chunk) {
            failed = Some(err);
            return false;
        }
        tail.extend_from_slice(chunk);
        if tail.len() > 20 {
            tail.drain(..tail.len() - 20);
        }
        true
    });
    if stopped {
        return Ok(None);
    }
    if let Some(err) = failed {
        return Err(io(err));
    }
    streamed.map_err(lib_fault)?;
    writer.commit().map_err(lib_fault)?;
    let hash = Oid::from_bytes(&tail).map_err(lib_fault)?;
    Ok(Some(format!("pack-{hash}")))
}

/// A repository in memory over a real one: it reads every object the real
/// one holds, and what is written to it stays in memory. A write builds its
/// commit in it, then [`Overlay::store`] writes exactly the new objects to
/// the real repository as one pack.
pub(crate) struct Overlay {
    repo: Repository,
}

impl Overlay {
    /// An overlay of `real`.
    ///
    /// # Errors
    ///
    /// `STORAGE`, `DATA_LOSS`, or `REPLICATION` when the library fails.
    pub(crate) fn over(real: &Repository) -> Result<Overlay, Fault> {
        let objects = real.commondir().join("objects");
        let objects = CString::new(objects.as_os_str().as_encoded_bytes()).map_err(|_| {
            Fault::new(
                VaultErrorCode::Storage,
                "the path of the object database holds a NUL byte",
            )
        })?;
        let odb = Odb::new().map_err(lib_fault)?;
        // The library takes the bytes of the path as they are, so a Vault
        // on a path that is not UTF-8 is written as one on any other.
        //
        // SAFETY: `odb.raw()` is the database `odb` owns and keeps alive
        // through the call; `objects` is NUL-terminated and lives past it.
        let status =
            unsafe { libgit2_sys::git_odb_add_disk_alternate(odb.raw(), objects.as_ptr()) };
        if status < 0 {
            return Err(lib_fault(git2::Error::last_error(status)));
        }
        // Higher than the disk backends: the writes go to memory. The
        // backend belongs to the database, which the repository owns.
        odb.add_new_mempack_backend(1000).map_err(lib_fault)?;
        // Below every other backend: a read the packs lost to a merge.
        if let Some(lock_file) = fallback::lock_file_of(real) {
            fallback::add(&odb, &real.commondir().join("objects"), &lock_file)?;
        }
        let repo = Repository::from_odb(odb).map_err(lib_fault)?;
        Ok(Overlay { repo })
    }

    /// The repository to build in.
    pub(crate) fn repo(&self) -> &Repository {
        &self.repo
    }

    /// Writes the objects the commit `commit` made new to `real`, as one
    /// pack, complete on disk when this returns: the commit, the trees on
    /// the `touched` paths, and the blobs the paths name (`None` for a
    /// removal). An object `real` holds already is not written again.
    ///
    /// # Errors
    ///
    /// `STORAGE`, `DATA_LOSS`, or `REPLICATION` when a file or the library fails.
    pub(crate) fn store(
        self,
        real: &Repository,
        commit: Oid,
        touched: &[(&str, Option<Oid>)],
    ) -> Result<(), Fault> {
        let root = self.repo.find_commit(commit).map_err(lib_fault)?.tree_id();
        let mut trees: Vec<(Oid, Option<&str>)> = vec![(root, None)];
        let mut blobs: Vec<(Oid, &str)> = Vec::new();
        for (path, blob) in touched {
            let mut parts: Vec<&str> = path.split('/').collect();
            let file = parts.pop();
            let mut node = self.repo.find_tree(root).map_err(lib_fault)?;
            for dir in parts {
                let id = match node.get_name(dir) {
                    Some(entry) if entry.kind() == Some(ObjectType::Tree) => entry.id(),
                    _ => break,
                };
                trees.push((id, Some(dir)));
                node = self.repo.find_tree(id).map_err(lib_fault)?;
            }
            if let (Some(blob), Some(file)) = (blob, file) {
                blobs.push((*blob, file));
            }
        }
        let odb = real.odb().map_err(lib_fault)?;
        let mut seen: HashSet<Oid> = HashSet::new();
        let mut builder = self.repo.packbuilder().map_err(lib_fault)?;
        // The commit is new whatever the real repository holds.
        seen.insert(commit);
        builder.insert_object(commit, None).map_err(lib_fault)?;
        let mut insert = |id: Oid, name: Option<&str>| -> Result<(), Fault> {
            // No refresh: an object that is not found is written, and
            // twice at worst.
            if seen.insert(id) && !odb.exists_ext(id, OdbLookupFlags::NO_REFRESH) {
                builder.insert_object(id, name).map_err(lib_fault)?;
            }
            Ok(())
        };
        for (id, name) in trees {
            insert(id, name)?;
        }
        for (id, name) in blobs {
            insert(id, Some(name))?;
        }
        stream(&mut builder, real, None)?;
        Ok(())
    }
}

/// What a handle knows of the merges of its Vault: the limits, what wakes
/// the merge task and what stops a merge, and the size of each pack, kept
/// by its name.
///
/// A pack is never changed once it is complete (a merge makes another and
/// removes it), so its size and its object count are read once, when its
/// name first shows in a listing of the folder, and every later look at the
/// packs costs the listing and the new names, not the history.
#[derive(Debug)]
pub(crate) struct Merges {
    limits: Limits,
    cancel: AtomicBool,
    wake: Notify,
    /// The packs of each object directory by name.
    sizes: Mutex<HashMap<PathBuf, HashMap<String, Size>>>,
    /// Raised after a merge removed packs: the pool of repository handles
    /// lets go of the handles that saw them.
    generation: Arc<AtomicU64>,
}

impl Default for Merges {
    fn default() -> Merges {
        Merges::new(Limits::DEFAULT, Arc::default())
    }
}

impl Merges {
    /// The merges of a Vault with `limits`, raising `generation` (that of
    /// its repositories) after a merge removed packs.
    pub(crate) fn new(limits: Limits, generation: Arc<AtomicU64>) -> Merges {
        Merges {
            generation,
            limits,
            cancel: AtomicBool::new(false),
            wake: Notify::new(),
            sizes: Mutex::new(HashMap::new()),
        }
    }

    /// A write ended: look at the packs. A wake that comes while a merge
    /// runs counts once it is done.
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }

    /// Stops the task, and cancels the merge it runs.
    fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    /// The flag a merge reads.
    fn flag(&self) -> &AtomicBool {
        &self.cancel
    }

    /// Waits for a wake. Returns `false` when stopped.
    async fn wait(&self) -> bool {
        if !self.cancel.load(Ordering::Acquire) {
            self.wake.notified().await;
        }
        !self.cancel.load(Ordering::Acquire)
    }

    /// The packs a merge may take in the object directory `objects`, each
    /// with its name (`pack-{hash}`) and size: those with an index, not
    /// those a `.keep` file protects, and none at all while a multi-pack
    /// index names them. Only a name not seen before is looked into.
    fn list(&self, objects: &Path) -> Result<Vec<(String, Size)>, Fault> {
        let pack_dir = objects.join("pack");
        if pack_dir.join(MIDX).exists() {
            return Ok(Vec::new());
        }
        let stems = pack_stems(&pack_dir)?;
        let mut all = self.sizes.lock().unwrap_or_else(PoisonError::into_inner);
        let known = all.entry(objects.to_path_buf()).or_default();
        let mut found = Vec::with_capacity(stems.len());
        let mut seen: HashSet<&str> = HashSet::with_capacity(stems.len());
        for stem in &stems {
            seen.insert(stem);
            if let Some(size) = known.get(stem) {
                found.push((stem.clone(), *size));
                continue;
            }
            // A pack another hand removed since the listing has nothing
            // to merge.
            let bytes = match fs::metadata(pack_dir.join(format!("{stem}.pack"))) {
                Ok(meta) => meta.len(),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(io(err)),
            };
            let Some(count) = index_count(&pack_dir.join(format!("{stem}.idx")))? else {
                continue;
            };
            let size = Size {
                bytes,
                objects: count,
            };
            known.insert(stem.clone(), size);
            found.push((stem.clone(), size));
        }
        known.retain(|stem, _| seen.contains(stem.as_str()));
        Ok(found)
    }

    /// How many packs below the cap `objects` holds.
    fn below_cap(&self, objects: &Path) -> Result<usize, Fault> {
        Ok(self
            .list(objects)?
            .iter()
            .filter(|(_, size)| size.below_cap())
            .count())
    }
}

/// The merge task of one Vault.
#[derive(Debug)]
pub(crate) struct Packer {
    merges: Arc<Merges>,
    root: PathBuf,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Packer {
    /// A packer of the Vault at `root` that is not running. `merges` is the
    /// one the write path wakes.
    pub(crate) fn new(merges: Arc<Merges>, root: &Path) -> Packer {
        Packer {
            merges,
            root: root.to_path_buf(),
            task: Mutex::new(None),
        }
    }

    /// Starts the task on the runtime of the caller. It looks at the packs
    /// at once, then after each wake.
    ///
    /// The task never takes the write lock: a write that finds too many
    /// packs takes the write lock and then waits for the merge lock
    /// ([`make_room`]), so the merge lock is always the second, and the
    /// task, which takes only that one and without waiting, cannot be part
    /// of a cycle.
    ///
    /// A merge of the task that fails leaves every object readable and is
    /// told to no one: the next wake tries again. A failure that lasts
    /// surfaces through the bound on the packs, as the failure of the write
    /// that has to merge before it can store anything.
    pub(crate) fn start(&self, repos: Arc<Repos>) {
        let merges = Arc::clone(&self.merges);
        let root = self.root.clone();
        let task = tokio::spawn(async move {
            loop {
                if merges.flag().load(Ordering::Acquire) {
                    return;
                }
                let (repos, root, merges) = (Arc::clone(&repos), root.clone(), Arc::clone(&merges));
                let step = {
                    let merges = Arc::clone(&merges);
                    blocking(move |cx| merge_all(cx, &root, &repos, &merges)).await
                };
                drop(step);
                if !merges.wait().await {
                    return;
                }
            }
        });
        *self.task.lock().unwrap_or_else(PoisonError::into_inner) = Some(task);
    }

    /// Cancels the merge in progress, if any, and waits for the task to end:
    /// no lock is held after this returns. A cancelled merge leaves what a
    /// killed one leaves, and the merge runs again after the next open.
    pub(crate) async fn stop(&self) {
        self.merges.cancel();
        let task = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

impl Drop for Packer {
    /// A handle dropped without `Close` cannot wait: a merge in progress is
    /// cancelled and the task ends on its own.
    fn drop(&mut self) {
        self.merges.cancel();
    }
}

/// What a test does to the merges.
#[cfg(test)]
pub(crate) mod hooks {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use scheda_api::v1::VaultErrorCode;

    use super::Limits;
    use crate::error::Fault;

    /// Called with the cancel flag of the merge; an error fails the merge.
    type Hook = Arc<dyn Fn(&AtomicBool) -> Result<(), Fault> + Send + Sync>;

    static HOOKS: Mutex<Vec<(PathBuf, Hook)>> = Mutex::new(Vec::new());
    static LIMITS: Mutex<Option<HashMap<PathBuf, Limits>>> = Mutex::new(None);

    /// Runs `hook` in the middle of each merge of the object database at
    /// `objects`, until it is cleared: after the packs to merge are chosen,
    /// before anything is written.
    pub(crate) fn on_merge(objects: &Path, hook: impl Fn(&AtomicBool) + Send + Sync + 'static) {
        set(
            objects,
            Arc::new(move |cancel| {
                hook(cancel);
                Ok(())
            }),
        );
    }

    /// Makes each merge of the object database at `objects` fail with
    /// `STORAGE`, until the hook is cleared.
    pub(crate) fn fail_merges(objects: &Path) {
        set(
            objects,
            Arc::new(|_| Err(Fault::new(VaultErrorCode::Storage, "a merge failed (test)"))),
        );
    }

    fn set(objects: &Path, hook: Hook) {
        let mut hooks = HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
        hooks.retain(|(path, _)| path != objects);
        hooks.push((objects.to_path_buf(), hook));
    }

    /// Removes the hook of `objects`.
    pub(crate) fn clear(objects: &Path) {
        HOOKS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(path, _)| path != objects);
    }

    /// Holds each merge of `objects` in its middle, with the merge lock,
    /// until the guard is dropped or the merge is cancelled: dropping lets
    /// go on success and on panic, and the wait has a deadline.
    pub(crate) fn hold_merges(objects: &Path) -> Hold {
        let released = Arc::new(AtomicBool::new(false));
        let (entered, flag) = (Arc::new(AtomicBool::new(false)), Arc::clone(&released));
        let seen = Arc::clone(&entered);
        on_merge(objects, move |cancel| {
            seen.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(30);
            while !flag.load(Ordering::Acquire)
                && !cancel.load(Ordering::Acquire)
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        Hold {
            objects: objects.to_path_buf(),
            released,
            entered,
        }
    }

    /// The hold of [`hold_merges`].
    pub(crate) struct Hold {
        objects: PathBuf,
        released: Arc<AtomicBool>,
        entered: Arc<AtomicBool>,
    }

    impl Hold {
        /// Whether a merge is held now.
        pub(crate) fn entered(&self) -> bool {
            self.entered.load(Ordering::Acquire)
        }
    }

    impl Drop for Hold {
        fn drop(&mut self) {
            self.released.store(true, Ordering::Release);
            clear(&self.objects);
        }
    }

    pub(super) fn in_merge(objects: &Path, cancel: &AtomicBool) -> Result<(), Fault> {
        let hook = HOOKS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(path, _)| path == objects)
            .map(|(_, hook)| Arc::clone(hook));
        match hook {
            Some(hook) => hook(cancel),
            None => Ok(()),
        }
    }

    /// Gives the Vault at `root` these limits when it is opened.
    pub(crate) fn set_limits(root: &Path, limits: Limits) {
        LIMITS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(HashMap::new)
            .insert(root.to_path_buf(), limits);
    }

    pub(super) fn limits_of(root: &Path) -> Option<Limits> {
        LIMITS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(|all| all.get(root).copied())
    }
}

/// The limits of the Vault at `root`.
pub(crate) fn limits_of(root: &Path) -> Limits {
    #[cfg(test)]
    if let Some(limits) = hooks::limits_of(root) {
        return limits;
    }
    let _ = root;
    Limits::DEFAULT
}

/// How a merge of one repository ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    /// Nothing was due.
    Idle,
    /// A merge was made.
    Merged,
    /// The merge was cancelled: nothing it was merging was removed.
    Cancelled,
}

/// Merges what is due in the main and the nested repository of the Vault at
/// `root`, under `{root}/.scheda/pack.lock`, taken without waiting: when
/// another handle holds it, its merge runs and there is nothing to do. The
/// write lock is never taken.
///
/// # Errors
///
/// `STORAGE`, `DATA_LOSS`, or `REPLICATION` when a file or the library fails; the first
/// failure, after the other repository was tried.
pub(crate) fn merge_all(
    cx: &Blocking,
    root: &Path,
    repos: &Repos,
    merges: &Merges,
) -> Result<(), Error> {
    let mut leases = Vec::with_capacity(2);
    for which in [Repo::Main, Repo::Isolated] {
        if let Some(lease) = repos.lease(which)? {
            leases.push(lease);
        }
    }
    // Looked at without the lock, so that a Vault with nothing to merge
    // leaves no file behind; looked at again under it.
    let mut due = false;
    for lease in &leases {
        due |= is_due(lease, merges)?;
    }
    if !due {
        return Ok(());
    }
    let file = open_lock_file(cx, root, LOCK_FILE)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(std::fs::TryLockError::Error(err)) => return Err(io(err).into()),
    }
    let _lock = Flock::held(file);
    let _exclusive = fallback::exclusive();
    let mut first: Option<Fault> = None;
    for lease in leases {
        loop {
            match merge_due(&lease, merges, merges.flag()) {
                Ok(Step::Merged) => {}
                Ok(Step::Idle) => break,
                Ok(Step::Cancelled) => return Ok(()),
                Err(fault) => {
                    first.get_or_insert(fault);
                    break;
                }
            }
        }
    }
    first.map_or(Ok(()), |fault| Err(fault.into()))
}

/// Whether [`plan`] says a merge is due in `git`.
fn is_due(git: &Repository, merges: &Merges) -> Result<bool, Fault> {
    let objects = git.commondir().join("objects");
    let sizes: Vec<Size> = merges
        .list(&objects)?
        .iter()
        .map(|(_, size)| *size)
        .collect();
    // Loose objects are only counted when the packs say nothing: they are
    // cheap to count only up to the point they matter.
    if !plan(&sizes, 0, &merges.limits).is_empty() {
        return Ok(true);
    }
    Ok(loose_objects(&objects)?.len() > DUE_LOOSE)
}

/// Makes room for a write in `git`, the repository of the Vault at `root`:
/// when the packs below the cap would number `hard` or more with the one the
/// write is about to add, merges until they would number fewer. The caller
/// holds the write lock and has not built its commit yet.
///
/// The merge lock is taken waiting, so a merge of the task, which holds it,
/// is waited for, and then the same merge code runs, a step at a time, not
/// until nothing is due. The order is the write lock, then the merge lock;
/// the task takes only the merge lock and never waits for the write lock.
///
/// The check costs the listing of the pack folder and the packs it has not
/// seen: [`Merges::list`] keeps the size of each pack by its name.
///
/// # Errors
///
/// The error of the merge, `STORAGE` for a file: the write then stores
/// nothing, and `HEAD` and the work tree are as they were.
pub(crate) fn make_room(git: &Repository, root: &Path, merges: &Merges) -> Result<(), Error> {
    let objects = git.commondir().join("objects");
    let hard = merges.limits.hard;
    if merges.below_cap(&objects)? + 1 < hard {
        return Ok(());
    }
    let file = open_lock_file_at(root, LOCK_FILE)?;
    file.lock().map_err(io)?;
    let _lock = Flock::held(file);
    let _exclusive = fallback::exclusive();
    // Not cancelled by anyone: this is the write's own merge.
    let never = AtomicBool::new(false);
    while merges.below_cap(&objects)? + 1 >= hard {
        match merge_due(git, merges, &never)? {
            Step::Merged => {}
            // Nothing to merge is nothing more to make room with.
            Step::Idle | Step::Cancelled => break,
        }
    }
    Ok(())
}

/// One merge in `git`, if [`plan`] says one is due. The caller holds the
/// merge lock.
///
/// The merge copies: the compressed entries of the packs it merges, and of
/// the loose objects that join it, go into one pack stream that the
/// library's indexer takes, checks, and indexes, as it does for the pack of
/// a write ([`copy_pack`]). Nothing is compressed again.
///
/// The new pack is complete on disk (the library flushes its pack, its
/// index, and the folder) before any file it replaces is removed, the index
/// of a pack first, and what is removed is only what the pack holds, so a
/// reader (this process, another one, the person's `git`) finds every
/// object at every moment: one that misses a file looks in the packs again.
/// `cancel` is read between the chunks of the stream; a cancelled merge
/// removes nothing.
///
/// # Errors
///
/// `STORAGE`, `DATA_LOSS`, or `REPLICATION` when a file or the library fails. Nothing
/// that was readable becomes unreadable: a failure leaves loose objects
/// that are also packed, or packs that are also merged.
pub(crate) fn merge_due(
    git: &Repository,
    merges: &Merges,
    cancel: &AtomicBool,
) -> Result<Step, Fault> {
    let objects = git.commondir().join("objects");
    let pack_dir = objects.join("pack");
    let loose = loose_objects(&objects)?;
    let listed = merges.list(&objects)?;
    let sizes: Vec<Size> = listed.iter().map(|(_, size)| *size).collect();
    let chosen = plan(&sizes, loose.len(), &merges.limits);
    if chosen.is_empty() {
        return Ok(Step::Idle);
    }
    let merging: Vec<&str> = chosen
        .packs
        .iter()
        .map(|&at| listed[at].0.as_str())
        .collect();
    let joining: Vec<Oid> = if chosen.loose {
        loose.iter().map(|(id, _)| *id).collect()
    } else {
        Vec::new()
    };
    #[cfg(test)]
    hooks::in_merge(&objects, cancel)?;
    let fresh = match copy_pack(git, &pack_dir, &merging, &joining, cancel)? {
        Copied::Written(fresh) => fresh,
        Copied::Nothing => return Ok(Step::Idle),
        Copied::Cancelled => return Ok(Step::Cancelled),
    };
    // The pack is complete. Only now do the files it replaces go.
    let mut first: Option<Fault> = None;
    if chosen.loose {
        for (_, path) in &loose {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    first.get_or_insert(io(err));
                }
            }
        }
    }
    let mut removed = false;
    for stem in merging {
        if stem == fresh {
            continue;
        }
        // A pack that cannot be removed (Windows keeps a file another
        // reader has open) stays: its objects are in the new pack too, and
        // a later merge takes it again.
        if remove_pack(&pack_dir, stem).is_ok() {
            removed = true;
        }
    }
    if removed {
        merges.generation.fetch_add(1, Ordering::AcqRel);
    }
    first.map_or(Ok(Step::Merged), Err)
}

/// Where a copy ended.
enum Copied {
    /// The pack is written; its name is `pack-{hash}`.
    Written(String),
    /// There was nothing to copy.
    Nothing,
    /// `cancel` was set; nothing is left.
    Cancelled,
}

/// Why the stream of a pack stopped.
enum Halt {
    Cancelled,
    Fault(Fault),
}

impl From<Fault> for Halt {
    fn from(fault: Fault) -> Halt {
        Halt::Fault(fault)
    }
}

fn halt(err: std::io::Error) -> Halt {
    Halt::Fault(io(err))
}

/// The number of bytes the stream gathers before it hands them on.
const CHUNK: usize = 64 * 1024;

/// What a pack stream is written to: the bytes go through the SHA-1 of the
/// trailer, are gathered, and are handed to the pack writer of the library
/// a chunk at a time, where `cancel` is looked at.
struct Sink<'a, 'w> {
    writer: &'a mut git2::OdbPackwriter<'w>,
    hash: Sha1,
    gathered: Vec<u8>,
    written: u64,
    cancel: &'a AtomicBool,
}

impl Sink<'_, '_> {
    /// Bytes put so far: where the next entry begins.
    fn position(&self) -> u64 {
        self.written + self.gathered.len() as u64
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), Halt> {
        self.hash.update(bytes);
        self.gathered.extend_from_slice(bytes);
        if self.gathered.len() >= CHUNK {
            self.hand_on()?;
        }
        Ok(())
    }

    fn hand_on(&mut self) -> Result<(), Halt> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(Halt::Cancelled);
        }
        self.writer.write_all(&self.gathered).map_err(halt)?;
        self.written += self.gathered.len() as u64;
        self.gathered.clear();
        Ok(())
    }

    /// Copies the next `count` bytes of `from`.
    fn copy(
        &mut self,
        from: &mut impl Read,
        mut count: u64,
        scratch: &mut [u8],
    ) -> Result<(), Halt> {
        while count > 0 {
            let take = scratch
                .len()
                .min(usize::try_from(count).unwrap_or(usize::MAX));
            from.read_exact(&mut scratch[..take]).map_err(halt)?;
            self.put(&scratch[..take])?;
            count -= take as u64;
        }
        Ok(())
    }

    /// Ends the stream with the trailer, and returns its hash.
    fn finish(mut self) -> Result<[u8; 20], Halt> {
        let trailer = std::mem::replace(&mut self.hash, Sha1::new()).finish();
        self.gathered.extend_from_slice(&trailer);
        self.hand_on()?;
        Ok(trailer)
    }
}

/// A pack being merged: where its entries are, and what becomes of each.
struct Source {
    stem: String,
    /// The offset and the id of each entry, in offset order.
    entries: Vec<(u64, Oid)>,
    /// Whether the entry is copied: false when its object came already.
    keep: Vec<bool>,
    /// Where each copied entry begins in the new pack.
    placed: Vec<Option<u64>>,
    /// The length of the pack file.
    length: u64,
}

/// Writes the pack that holds the objects of the packs `stems` of
/// `pack_dir` and the loose objects `loose`, copying the compressed entries
/// of the packs byte for byte, each pack's entries together and in their
/// order, then the loose objects, each as an entry of its own, to the
/// object database's pack writer.
///
/// A pack whose objects are all in another pack of the merge is dropped
/// (what a crash between the new pack and the removal of the old ones
/// leaves). Of an object that is in several packs, the first entry is
/// copied; a delta by offset on an entry that was not copied is written as
/// a delta by id, and the offset of a delta whose base moved is written
/// again. The library's indexer then reads the stream, checks every object
/// and the trailer, and writes the index.
fn copy_pack(
    git: &Repository,
    pack_dir: &Path,
    stems: &[&str],
    loose: &[Oid],
    cancel: &AtomicBool,
) -> Result<Copied, Fault> {
    let mut sources: Vec<Source> = Vec::with_capacity(stems.len());
    for stem in stems {
        let Some(entries) = index_entries(&pack_dir.join(format!("{stem}.idx")))? else {
            continue;
        };
        let length = match fs::metadata(pack_dir.join(format!("{stem}.pack"))) {
            Ok(meta) => meta.len(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(io(err)),
        };
        sources.push(Source {
            stem: (*stem).to_owned(),
            keep: vec![true; entries.len()],
            placed: vec![None; entries.len()],
            entries,
            length,
        });
    }
    // A pack whose objects are all in another pack of the merge is dropped:
    // it is not read.
    let mut holders: HashMap<Oid, u32> = HashMap::new();
    for source in &sources {
        for (_, id) in &source.entries {
            *holders.entry(*id).or_insert(0) += 1;
        }
    }
    let mut live: Vec<Source> = Vec::with_capacity(sources.len());
    for source in sources {
        if source.entries.iter().all(|(_, id)| holders[id] > 1) {
            for (_, id) in &source.entries {
                if let Some(count) = holders.get_mut(id) {
                    *count -= 1;
                }
            }
        } else {
            live.push(source);
        }
    }
    drop(holders);
    // What is copied: the first entry of each object.
    let mut emitted: HashSet<Oid> = HashSet::new();
    let mut count: u64 = 0;
    for source in &mut live {
        for (at, (_, id)) in source.entries.iter().enumerate() {
            if emitted.insert(*id) {
                count += 1;
            } else {
                source.keep[at] = false;
            }
        }
    }
    let loose: Vec<Oid> = loose
        .iter()
        .filter(|id| !emitted.contains(id))
        .copied()
        .collect();
    drop(emitted);
    count += loose.len() as u64;
    if count == 0 {
        return Ok(Copied::Nothing);
    }
    let count = u32::try_from(count).map_err(|_| {
        Fault::new(
            VaultErrorCode::Storage,
            "a merge holds more objects than a pack can",
        )
    })?;
    let odb = git.odb().map_err(lib_fault)?;
    let mut writer = odb.packwriter().map_err(lib_fault)?;
    let mut sink = Sink {
        writer: &mut writer,
        hash: Sha1::new(),
        gathered: Vec::with_capacity(CHUNK + 4096),
        written: 0,
        cancel,
    };
    let streamed = (|| -> Result<[u8; 20], Halt> {
        let mut head = [0u8; 12];
        head[..4].copy_from_slice(b"PACK");
        head[4..8].copy_from_slice(&2u32.to_be_bytes());
        head[8..].copy_from_slice(&count.to_be_bytes());
        sink.put(&head)?;
        let mut scratch = vec![0u8; CHUNK];
        for source in &mut live {
            copy_source(&mut sink, pack_dir, source, &mut scratch)?;
        }
        for id in &loose {
            let object = odb.read(*id).map_err(|err| Halt::Fault(lib_fault(err)))?;
            sink.put(&entry_of(object.kind(), object.data())?)?;
        }
        sink.finish()
    })();
    match streamed {
        Ok(trailer) => {
            writer.commit().map_err(lib_fault)?;
            let hash = Oid::from_bytes(&trailer).map_err(lib_fault)?;
            Ok(Copied::Written(format!("pack-{hash}")))
        }
        Err(Halt::Cancelled) => Ok(Copied::Cancelled),
        Err(Halt::Fault(fault)) => Err(fault),
    }
}

/// Copies the entries of `source` that are kept into `sink`.
fn copy_source(
    sink: &mut Sink<'_, '_>,
    pack_dir: &Path,
    source: &mut Source,
    scratch: &mut [u8],
) -> Result<(), Halt> {
    let file = fs::File::open(pack_dir.join(format!("{}.pack", source.stem))).map_err(halt)?;
    let mut pack = BufReader::with_capacity(256 * 1024, file);
    let mut head = [0u8; 12];
    pack.read_exact(&mut head).map_err(halt)?;
    let bad = |what: &str| Halt::Fault(Fault::new(VaultErrorCode::Storage, what.to_owned()));
    if &head[..4] != b"PACK" || head[4..8] != 2u32.to_be_bytes() {
        return Err(bad("a pack is not of the version 2 format"));
    }
    if u64::from(u32::from_be_bytes([head[8], head[9], head[10], head[11]]))
        != source.entries.len() as u64
    {
        return Err(bad("a pack and its index count different objects"));
    }
    let mut cursor: u64 = 12;
    for at in 0..source.entries.len() {
        let (begin, _) = source.entries[at];
        let end = match source.entries.get(at + 1) {
            Some((next, _)) => *next,
            None => source.length.saturating_sub(20),
        };
        if end < begin || begin < cursor {
            return Err(bad("the entries of a pack overlap"));
        }
        if begin != cursor {
            pack.seek_relative(
                i64::try_from(begin - cursor).map_err(|_| bad("a pack is too large"))?,
            )
            .map_err(halt)?;
        }
        cursor = end;
        let size = end - begin;
        if !source.keep[at] {
            pack.seek_relative(i64::try_from(size).map_err(|_| bad("a pack is too large"))?)
                .map_err(halt)?;
            continue;
        }
        source.placed[at] = Some(sink.position());
        // The head of the entry: its kind, its size, and the base of a delta.
        let seen = usize::try_from(size.min(32)).unwrap_or(32);
        let mut first = [0u8; 32];
        pack.read_exact(&mut first[..seen]).map_err(halt)?;
        let first = &first[..seen];
        let Some((kind, length, header)) = parse_header(first) else {
            return Err(bad("an entry of a pack has a head that is cut off"));
        };
        if kind != OFS_DELTA {
            sink.put(first)?;
            sink.copy(&mut pack, size - seen as u64, scratch)?;
            continue;
        }
        let Some((back, taken)) = parse_offset(&first[header..]) else {
            return Err(bad("a delta of a pack has an offset that is cut off"));
        };
        let base = begin
            .checked_sub(back)
            .and_then(|base| {
                source
                    .entries
                    .binary_search_by_key(&base, |(offset, _)| *offset)
                    .ok()
            })
            .ok_or_else(|| bad("a delta of a pack has a base that is not an entry"))?;
        let rest = &first[header + taken..];
        match source.placed[base] {
            Some(placed) => {
                let moved = sink.position() - placed;
                if moved == back {
                    sink.put(first)?;
                } else {
                    sink.put(&encode_header(OFS_DELTA, length))?;
                    sink.put(&encode_offset(moved))?;
                    sink.put(rest)?;
                }
            }
            None => {
                sink.put(&encode_header(REF_DELTA, length))?;
                sink.put(source.entries[base].1.as_bytes())?;
                sink.put(rest)?;
            }
        }
        sink.copy(&mut pack, size - seen as u64, scratch)?;
    }
    Ok(())
}

const OFS_DELTA: u8 = 6;
const REF_DELTA: u8 = 7;

/// The kind, the size, and the length of the head of the entry that
/// `bytes` begins: a kind in bits 4 to 6 of the first byte, the size in
/// the low four bits and then seven bits a byte, least significant first,
/// a high bit saying another byte follows.
fn parse_header(bytes: &[u8]) -> Option<(u8, u64, usize)> {
    let first = *bytes.first()?;
    let kind = (first >> 4) & 7;
    let mut size = u64::from(first & 15);
    let mut shift = 4;
    let mut at = 1;
    let mut more = first & 0x80 != 0;
    while more {
        let byte = *bytes.get(at)?;
        size |= u64::from(byte & 0x7f).checked_shl(shift)?;
        shift += 7;
        at += 1;
        more = byte & 0x80 != 0;
    }
    Some((kind, size, at))
}

fn encode_header(kind: u8, mut size: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    let mut byte = (kind << 4) | (size & 15) as u8;
    size >>= 4;
    while size > 0 {
        out.push(byte | 0x80);
        byte = (size & 0x7f) as u8;
        size >>= 7;
    }
    out.push(byte);
    out
}

/// The distance back to the base of a delta by offset, and the length of
/// its encoding: seven bits a byte, most significant first, a high bit
/// saying another byte follows, and one added for each byte after the first.
fn parse_offset(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut at = 0;
    let mut byte = *bytes.first()?;
    let mut back = u64::from(byte & 0x7f);
    while byte & 0x80 != 0 {
        at += 1;
        byte = *bytes.get(at)?;
        back = back.checked_add(1)?.checked_mul(128)? | u64::from(byte & 0x7f);
    }
    Some((back, at + 1))
}

fn encode_offset(mut back: u64) -> Vec<u8> {
    let mut out = vec![(back & 0x7f) as u8];
    back >>= 7;
    while back > 0 {
        back -= 1;
        out.push(0x80 | (back & 0x7f) as u8);
        back >>= 7;
    }
    out.reverse();
    out
}

/// The entry of a loose object: its head, then its content compressed.
fn entry_of(kind: ObjectType, content: &[u8]) -> Result<Vec<u8>, Halt> {
    let kind = match kind {
        ObjectType::Commit => 1,
        ObjectType::Tree => 2,
        ObjectType::Blob => 3,
        ObjectType::Tag => 4,
        _ => {
            return Err(Halt::Fault(Fault::new(
                VaultErrorCode::Storage,
                "a loose object is of a kind no pack holds",
            )));
        }
    };
    let mut entry = encode_header(kind, content.len() as u64);
    entry.extend_from_slice(&deflate(content)?);
    Ok(entry)
}

/// `content` as a zlib stream.
fn deflate(content: &[u8]) -> Result<Vec<u8>, Fault> {
    let failed = || {
        Fault::new(
            VaultErrorCode::Storage,
            "zlib refused to compress an object",
        )
    };
    let size = libz_sys::uLong::try_from(content.len()).map_err(|_| failed())?;
    // SAFETY: `compressBound` only computes a number from its argument.
    let bound = unsafe { libz_sys::compressBound(size) };
    let mut out = vec![0u8; usize::try_from(bound).map_err(|_| failed())?];
    let mut length = bound;
    // SAFETY: `out` has `bound` bytes, which is what `length` says on the
    // way in; `content` has `size` bytes; both live through the call.
    let status = unsafe {
        libz_sys::compress2(
            out.as_mut_ptr(),
            &mut length,
            content.as_ptr(),
            size,
            libz_sys::Z_DEFAULT_COMPRESSION,
        )
    };
    if status != libz_sys::Z_OK {
        return Err(failed());
    }
    out.truncate(usize::try_from(length).map_err(|_| failed())?);
    Ok(out)
}

/// Every loose object of `objects` with the path of its file.
fn loose_objects(objects: &Path) -> Result<Vec<(Oid, PathBuf)>, Fault> {
    let mut found = Vec::new();
    for dir in 0..256u32 {
        let prefix = format!("{dir:02x}");
        let path = objects.join(&prefix);
        let Some(entries) = read_dir_if_there(&path)? else {
            continue;
        };
        for entry in entries {
            let entry = entry.map_err(io)?;
            let name = entry.file_name();
            if !is_object_name(&name) {
                continue;
            }
            let Some(rest) = name.to_str() else { continue };
            if let Ok(id) = Oid::from_str(&format!("{prefix}{rest}")) {
                found.push((id, entry.path()));
            }
        }
    }
    Ok(found)
}

/// Whether `name` is the file name of a loose object below its two-digit
/// directory: 38 hexadecimal digits. A temporary file of a write in
/// progress is not.
fn is_object_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.len() == 38 && name.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The entries of `dir`, or `None` when it is not there.
fn read_dir_if_there(dir: &Path) -> Result<Option<fs::ReadDir>, Fault> {
    match fs::read_dir(dir) {
        Ok(entries) => Ok(Some(entries)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io(err)),
    }
}

/// The names (`pack-{hash}`) of the packs in `pack_dir` that a merge may
/// take: those with an index, as a reader finds them, and not those a
/// `.keep` file protects. A pack file without its index is no pack: Windows
/// leaves one when the removal of a merged pack takes the index of a pack
/// file another reader has open, and git writes the pack file of a new pack
/// before its index.
fn pack_stems(pack_dir: &Path) -> Result<Vec<String>, Fault> {
    let Some(entries) = read_dir_if_there(pack_dir)? else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    let mut indexed = HashSet::new();
    let mut kept = HashSet::new();
    for entry in entries {
        let entry = entry.map_err(io)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(stem) = name.strip_suffix(".pack") {
            found.push(stem.to_owned());
        } else if let Some(stem) = name.strip_suffix(".idx") {
            indexed.insert(stem.to_owned());
        } else if let Some(stem) = name.strip_suffix(".keep") {
            kept.insert(stem.to_owned());
        }
    }
    found.retain(|stem| indexed.contains(stem) && !kept.contains(stem));
    Ok(found)
}

/// The number of objects a pack holds: the last counter of the fan-out table
/// of its index. `None` when the index is not there: the pack was removed.
fn index_count(idx: &Path) -> Result<Option<u64>, Fault> {
    use std::io::Read as _;

    const HEADER: usize = 8;
    const FANOUT: usize = 256 * 4;
    let mut bytes = [0u8; HEADER + FANOUT];
    let read = fs::File::open(idx).and_then(|mut file| file.read_exact(&mut bytes));
    match read {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io(err)),
    }
    if bytes[..HEADER] != [0xff, b't', b'O', b'c', 0, 0, 0, 2] {
        return Err(Fault::new(
            VaultErrorCode::Storage,
            "a pack index is not of the version 2 format",
        ));
    }
    let mut last = [0u8; 4];
    last.copy_from_slice(&bytes[HEADER + FANOUT - 4..]);
    Ok(Some(u64::from(u32::from_be_bytes(last))))
}

/// The offset and the id of each object a pack holds, in offset order, read
/// from its index (version 2: a header of 8 bytes, a fan-out table of 256
/// counters, the ids 20 bytes each in order, their checksums, their
/// offsets in 4 bytes, and a table of the offsets that need 8). `None` when
/// the index is not there: the pack was removed.
fn index_entries(idx: &Path) -> Result<Option<Vec<(u64, Oid)>>, Fault> {
    const HEADER: usize = 8;
    const FANOUT: usize = 256 * 4;
    let bytes = match fs::read(idx) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io(err)),
    };
    let bad = || {
        Fault::new(
            VaultErrorCode::Storage,
            "a pack index is not of the version 2 format",
        )
    };
    if bytes.get(..HEADER) != Some(&[0xff, b't', b'O', b'c', 0, 0, 0, 2][..]) {
        return Err(bad());
    }
    let count = bytes
        .get(HEADER + FANOUT - 4..HEADER + FANOUT)
        .and_then(|last| <[u8; 4]>::try_from(last).ok())
        .map(u32::from_be_bytes)
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(bad)?;
    let ids = HEADER + FANOUT;
    let offsets = ids + count.checked_mul(24).ok_or_else(bad)?;
    let large = offsets + count.checked_mul(4).ok_or_else(bad)?;
    let table = bytes.get(ids..ids + count * 20).ok_or_else(bad)?;
    let small = bytes.get(offsets..large).ok_or_else(bad)?;
    let wide = bytes.get(large..).ok_or_else(bad)?;
    let mut found = Vec::with_capacity(count);
    for (id, offset) in table
        .as_chunks::<20>()
        .0
        .iter()
        .zip(small.as_chunks::<4>().0)
    {
        let offset = u32::from_be_bytes(*offset);
        let offset = if offset & 0x8000_0000 == 0 {
            u64::from(offset)
        } else {
            let at = usize::try_from(offset & 0x7fff_ffff)
                .ok()
                .and_then(|at| at.checked_mul(8))
                .ok_or_else(bad)?;
            let eight = wide.get(at..at + 8).ok_or_else(bad)?;
            u64::from_be_bytes(<[u8; 8]>::try_from(eight).map_err(|_| bad())?)
        };
        found.push((offset, Oid::from_bytes(id).map_err(lib_fault)?));
    }
    found.sort_unstable_by_key(|(offset, _)| *offset);
    Ok(Some(found))
}

/// Removes the files of the pack `stem` from `pack_dir`, its index first:
/// a pack is found through its index, so a reader that lists the folder
/// meanwhile sees the pack whole or not at all.
fn remove_pack(pack_dir: &Path, stem: &str) -> Result<(), Fault> {
    let mut others = Vec::new();
    for entry in fs::read_dir(pack_dir).map_err(io)? {
        let name = entry.map_err(io)?.file_name();
        let Some(name) = name.to_str() else { continue };
        if name
            .strip_prefix(stem)
            .is_some_and(|rest| rest.starts_with('.'))
            && name != format!("{stem}.idx")
        {
            others.push(name.to_owned());
        }
    }
    fs::remove_file(pack_dir.join(format!("{stem}.idx"))).map_err(io)?;
    for name in others {
        match fs::remove_file(pack_dir.join(name)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io(err)),
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    use git2::build::TreeUpdateBuilder;
    use git2::{Oid, Repository, Signature};

    use super::{
        CAP_BYTES, CAP_OBJECTS, DUE_LOOSE, DUE_PACKS, Limits, Merges, Overlay, Packer, Size, Step,
        hooks, index_count, index_entries, loose_objects, merge_due, pack_stems, plan,
    };
    use crate::repo::{Repo, Repos};

    /// A repository in a scratch folder, removed when the test ends.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Scratch {
        pub(crate) fn new(name: &str) -> Scratch {
            crate::git::isolate_test_git_env();
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-pack-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            crate::testing::remove_when_the_test_ends(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        /// A repository with nothing in it.
        fn repo(&self) -> Repository {
            crate::repo::init_library().unwrap();
            Repository::init(&self.0).unwrap()
        }
    }

    /// The packs of `pack_dir` with the size of each.
    fn packs(pack_dir: &Path) -> Result<Vec<(String, u64)>, crate::error::Fault> {
        let mut found = Vec::new();
        for stem in pack_stems(pack_dir)? {
            // A merge may remove a pack between the listing and the size.
            match std::fs::metadata(pack_dir.join(format!("{stem}.pack"))) {
                Ok(meta) => found.push((stem, meta.len())),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(crate::error::io(err)),
            }
        }
        Ok(found)
    }

    /// `len` bytes that do not compress.
    pub(crate) fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    /// One write: a commit on `HEAD` that puts `bytes` at `path`, built in
    /// an overlay and stored as one pack, then the branch moved. Returns
    /// the commit.
    pub(crate) fn write(repo: &Repository, path: &str, bytes: &[u8]) -> Oid {
        let overlay = Overlay::over(repo).unwrap();
        let mem = overlay.repo();
        let blob = mem.blob(bytes).unwrap();
        let branch = repo
            .find_reference("HEAD")
            .unwrap()
            .symbolic_target()
            .unwrap()
            .unwrap()
            .to_owned();
        let parent = repo.refname_to_id(&branch).ok();
        let parent = parent.map(|id| mem.find_commit(id).unwrap());
        let mut update = TreeUpdateBuilder::new();
        update.upsert(path, blob, git2::FileMode::Blob);
        let base = match &parent {
            Some(parent) => parent.tree().unwrap(),
            None => mem
                .find_tree(mem.treebuilder(None).unwrap().write().unwrap())
                .unwrap(),
        };
        let tree = mem
            .find_tree(update.create_updated(mem, &base).unwrap())
            .unwrap();
        let when = git2::Time::new(1_700_000_000, 0);
        let who = Signature::new("Test", "test@example.test", &when).unwrap();
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        let id = mem
            .commit(None, &who, &who, "write", &tree, &parents)
            .unwrap();
        drop((tree, base, parent));
        overlay.store(repo, id, &[(path, Some(blob))]).unwrap();
        repo.reference(&branch, id, true, "write").unwrap();
        id
    }

    /// The Vault-less merge step on the repository at `dir`, on the
    /// blocking pool as the task runs it.
    fn merge_step(dir: &Path, cancel: &Arc<AtomicBool>) -> Step {
        let (dir, cancel) = (dir.to_path_buf(), Arc::clone(cancel));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime
            .block_on(scheda_blocking::blocking(move |_cx| {
                let repo = Repository::open(&dir).unwrap();
                Ok(merge_due(&repo, &Merges::default(), &cancel).unwrap())
            }))
            .unwrap()
    }

    /// Merges until nothing is due; returns the number of merges.
    fn settle(dir: &Path) -> usize {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut merges = 0;
        while merge_step(dir, &cancel) == Step::Merged {
            merges += 1;
            assert!(merges < 1000, "the merges do not end");
        }
        merges
    }

    fn pack_dir(repo: &Repository) -> PathBuf {
        repo.path().join("objects").join("pack")
    }

    fn pack_count(repo: &Repository) -> usize {
        packs(&pack_dir(repo)).unwrap().len()
    }

    fn loose_count(repo: &Repository) -> usize {
        loose_objects(&repo.path().join("objects")).unwrap().len()
    }

    /// `git fsck` of the repository finds nothing wrong.
    fn fsck(dir: &Path) {
        crate::git::git()
            .current_dir(dir)
            .arg("fsck")
            .arg("--full")
            .arg("--strict")
            .run()
            .unwrap();
    }

    /// Every commit of the history holds the version of the segment it was
    /// made with, and every object the database lists can be read.
    fn assert_readable(repo: &Repository, path: &str, versions: &[Vec<u8>]) {
        let mut walk = repo.revwalk().unwrap();
        walk.push_head().unwrap();
        let commits: Vec<Oid> = walk.map(Result::unwrap).collect();
        assert_eq!(commits.len(), versions.len());
        for (id, version) in commits.iter().rev().zip(versions) {
            let tree = repo.find_commit(*id).unwrap().tree().unwrap();
            let entry = tree.get_path(Path::new(path)).unwrap();
            let blob = repo.find_blob(entry.id()).unwrap();
            assert_eq!(blob.content(), version.as_slice());
        }
        let odb = repo.odb().unwrap();
        let mut all = Vec::new();
        odb.foreach(|id| {
            all.push(*id);
            true
        })
        .unwrap();
        for id in all {
            odb.read(id).unwrap();
        }
    }

    /// `n` writes of a growing segment; returns each version.
    fn append(repo: &Repository, n: usize, line: usize, between: impl Fn()) -> Vec<Vec<u8>> {
        let mut segment = Vec::new();
        let mut versions = Vec::new();
        for i in 0..n {
            segment.extend(noise(i as u64 + 1, line));
            write(repo, "log/segment", &segment);
            versions.push(segment.clone());
            between();
        }
        versions
    }

    // The rule.

    fn sized(bytes: u64) -> Size {
        Size { bytes, objects: 10 }
    }

    #[test]
    fn nothing_is_due_below_sixteen_packs() {
        let few: Vec<Size> = (0..DUE_PACKS - 1).map(|_| sized(1000)).collect();
        assert!(plan(&few, 0, &Limits::DEFAULT).is_empty());
        // Packs at the cap do not count among the sixteen.
        let mut capped: Vec<Size> = (0..DUE_PACKS - 1).map(|_| sized(1000)).collect();
        capped.extend((0..10).map(|_| sized(CAP_BYTES)));
        assert!(plan(&capped, 0, &Limits::DEFAULT).is_empty());
        let many: Vec<Size> = (0..DUE_PACKS).map(|_| sized(1000)).collect();
        assert!(!plan(&many, 0, &Limits::DEFAULT).is_empty());
    }

    #[test]
    fn the_split_is_geometric_with_factor_two() {
        // Sixteen packs: ten of 1 KB, then 25 KB, 60 KB, 200 KB, 600 KB,
        // 2 MB, 6 MB. From the largest down each is at least twice the one
        // before until the ten of 1 KB, which break the progression; they
        // are rolled up, and the next pack, 25 KB, is more than twice their
        // 10 KB sum, so it stays.
        let mut sizes: Vec<Size> = (0..10).map(|_| sized(1000)).collect();
        for kb in [25, 60, 200, 600, 2000, 6000] {
            sizes.push(sized(kb * 1000));
        }
        let taken = plan(&sizes, 0, &Limits::DEFAULT);
        assert_eq!(taken.packs, (0..10).collect::<Vec<_>>());
        // A pack that is less than twice the sum of what is rolled up is
        // rolled up with it: 15 KB < 2 * 10 KB; the next, 60 KB, is more
        // than twice 25 KB.
        sizes[10] = sized(15_000);
        let taken = plan(&sizes, 0, &Limits::DEFAULT);
        assert_eq!(taken.packs, (0..=10).collect::<Vec<_>>());
        // The order of the listing does not matter, only the sizes.
        sizes.reverse();
        let mut taken: Vec<u64> = plan(&sizes, 0, &Limits::DEFAULT)
            .packs
            .iter()
            .map(|&at| sizes[at].bytes)
            .collect();
        taken.sort_unstable();
        assert_eq!(taken.len(), 11);
        assert_eq!(taken.last(), Some(&15_000));
    }

    #[test]
    fn a_merge_never_takes_a_pack_at_the_cap() {
        let mut sizes: Vec<Size> = (0..DUE_PACKS).map(|_| sized(1000)).collect();
        sizes.push(sized(CAP_BYTES));
        sizes.push(Size {
            bytes: 10,
            objects: CAP_OBJECTS,
        });
        sizes.push(sized(CAP_BYTES - 1));
        let taken = plan(&sizes, 0, &Limits::DEFAULT);
        // The pack just below the cap is a candidate, the two at the cap
        // are not.
        assert!(!taken.packs.contains(&DUE_PACKS));
        assert!(!taken.packs.contains(&(DUE_PACKS + 1)));
    }

    #[test]
    fn loose_objects_join_a_merge_and_alone_cause_one_past_a_thousand() {
        let few: Vec<Size> = (0..3).map(|_| sized(1000)).collect();
        assert!(plan(&few, 0, &Limits::DEFAULT).is_empty());
        assert!(plan(&few, DUE_LOOSE, &Limits::DEFAULT).is_empty());
        let alone = plan(&few, DUE_LOOSE + 1, &Limits::DEFAULT);
        assert!(alone.packs.is_empty() && alone.loose);
        let many: Vec<Size> = (0..DUE_PACKS).map(|_| sized(1000)).collect();
        let joined = plan(&many, 3, &Limits::DEFAULT);
        assert!(!joined.packs.is_empty() && joined.loose);
        let without = plan(&many, 0, &Limits::DEFAULT);
        assert!(!without.packs.is_empty() && !without.loose);
    }

    #[test]
    fn the_pack_count_stays_bounded_over_a_simulated_million_writes() {
        // A write is a pack of 3 KB and 6 objects: the blob, three trees,
        // the commit, and a tree of the layout.
        let write = Size {
            bytes: 3000,
            objects: 6,
        };
        let mut state: Vec<Size> = Vec::new();
        let mut biggest = 0u64;
        let (mut bytes, mut objects) = (0u64, 0u64);
        for n in 1..=1_000_000u64 {
            state.push(write);
            bytes += write.bytes;
            objects += write.objects;
            loop {
                let taken = plan(&state, 0, &Limits::DEFAULT);
                if taken.is_empty() {
                    break;
                }
                let mut merged = Size {
                    bytes: 0,
                    objects: 0,
                };
                let mut at = 0;
                state.retain(|pack| {
                    let keep = !taken.packs.contains(&at);
                    at += 1;
                    if !keep {
                        merged.bytes += pack.bytes;
                        merged.objects += pack.objects;
                    }
                    keep
                });
                biggest = biggest.max(merged.bytes);
                state.push(merged);
            }
            let below = state
                .iter()
                .filter(|pack| pack.bytes < CAP_BYTES && pack.objects < CAP_OBJECTS)
                .count();
            assert!(
                below < DUE_PACKS,
                "{below} packs below the cap after {n} writes"
            );
            // A pack at the cap holds a cap of bytes or of objects.
            let capped = (state.len() - below) as u64;
            assert!(
                capped <= bytes / CAP_BYTES + objects / CAP_OBJECTS,
                "{capped} packs at the cap after {n} writes"
            );
        }
        // A merge reads and writes about a cap, never several.
        assert!(biggest < 3 * CAP_BYTES, "a merge of {biggest} bytes");
        assert!(state.len() < 140, "{} packs", state.len());
    }

    // One pack for each write.

    #[test]
    fn a_write_stores_exactly_its_new_objects_as_one_pack_and_nothing_loose() {
        let scratch = Scratch::new("one-pack");
        let repo = scratch.repo();
        write(&repo, "notes/a/doc", b"first");
        assert_eq!(pack_count(&repo), 1);
        assert_eq!(loose_count(&repo), 0);
        let before = pack_count(&repo);
        write(&repo, "notes/a/doc", b"second");
        assert_eq!(pack_count(&repo), before + 1);
        assert_eq!(loose_count(&repo), 0);
        // A blob, the trees of `notes/a`, `notes`, and the root, and the
        // commit: five objects, and the pack holds those.
        let newest = packs(&pack_dir(&repo))
            .unwrap()
            .into_iter()
            .map(|(stem, _)| stem)
            .find(|stem| {
                let entries = index_entries(&pack_dir(&repo).join(format!("{stem}.idx")))
                    .unwrap()
                    .unwrap();
                let tip = repo.head().unwrap().target().unwrap();
                entries.iter().any(|(_, id)| *id == tip)
            })
            .unwrap();
        let count = index_count(&pack_dir(&repo).join(format!("{newest}.idx")))
            .unwrap()
            .unwrap();
        assert_eq!(count, 5);
        fsck(&scratch.0);
    }

    #[test]
    fn an_object_the_repository_holds_is_not_stored_again() {
        let scratch = Scratch::new("no-duplicates");
        let repo = scratch.repo();
        write(&repo, "a", b"same");
        // The same content at another path: the blob exists, so the pack
        // holds the root tree and the commit.
        write(&repo, "b", b"same");
        let ids: Vec<usize> = packs(&pack_dir(&repo))
            .unwrap()
            .into_iter()
            .map(|(stem, _)| {
                index_count(&pack_dir(&repo).join(format!("{stem}.idx")))
                    .unwrap()
                    .unwrap() as usize
            })
            .collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&2), "{ids:?}");
        fsck(&scratch.0);
    }

    // The merge.

    #[test]
    fn a_merge_loses_no_object_and_keeps_the_pack_count_bounded() {
        let scratch = Scratch::new("merge");
        let repo = scratch.repo();
        let dir = scratch.0.clone();
        let versions = append(&repo, 120, 100, || {
            settle(&dir);
        });
        assert!(pack_count(&repo) < DUE_PACKS, "{} packs", pack_count(&repo));
        assert_eq!(loose_count(&repo), 0);
        assert_readable(&repo, "log/segment", &versions);
        fsck(&scratch.0);
    }

    #[test]
    fn the_loose_objects_of_a_person_join_a_merge_and_alone_cause_one_past_a_thousand() {
        let scratch = Scratch::new("loose");
        let repo = scratch.repo();
        for i in 0..5 {
            repo.blob(format!("loose {i}").as_bytes()).unwrap();
        }
        write(&repo, "a", b"x");
        // Few packs and few loose objects: nothing is due.
        assert_eq!(settle(&scratch.0), 0);
        assert_eq!(loose_count(&repo), 5);
        // Sixteen packs: the loose objects join the merge.
        for i in 0..DUE_PACKS {
            write(&repo, "a", format!("v{i}").as_bytes());
        }
        assert!(settle(&scratch.0) >= 1);
        assert_eq!(loose_count(&repo), 0);
        for i in 0..5 {
            let id =
                Oid::hash_object(git2::ObjectType::Blob, format!("loose {i}").as_bytes()).unwrap();
            assert!(repo.find_blob(id).is_ok());
        }
        // More than a thousand cause a merge of their own.
        for i in 0..=DUE_LOOSE {
            repo.blob(format!("many {i}").as_bytes()).unwrap();
        }
        assert_eq!(loose_count(&repo), DUE_LOOSE + 1);
        assert_eq!(settle(&scratch.0), 1);
        assert_eq!(loose_count(&repo), 0);
        fsck(&scratch.0);
    }

    #[test]
    fn a_pack_a_keep_file_protects_and_a_multi_pack_index_are_left_alone() {
        let scratch = Scratch::new("keep");
        let repo = scratch.repo();
        let mut versions = Vec::new();
        let mut segment = Vec::new();
        for i in 0..4 {
            segment.extend(noise(i + 1, 100));
            write(&repo, "log/segment", &segment);
            versions.push(segment.clone());
        }
        let kept = packs(&pack_dir(&repo)).unwrap()[0].0.clone();
        std::fs::write(pack_dir(&repo).join(format!("{kept}.keep")), "").unwrap();
        for i in 4..40 {
            segment.extend(noise(i + 1, 100));
            write(&repo, "log/segment", &segment);
            versions.push(segment.clone());
        }
        assert!(settle(&scratch.0) >= 1);
        assert!(pack_dir(&repo).join(format!("{kept}.pack")).is_file());
        assert_readable(&repo, "log/segment", &versions);
        // With a multi-pack index no pack is merged.
        for i in 40..70 {
            segment.extend(noise(i + 1, 100));
            write(&repo, "log/segment", &segment);
            versions.push(segment.clone());
        }
        crate::git::git()
            .current_dir(&scratch.0)
            .arg("multi-pack-index")
            .arg("write")
            .run()
            .unwrap();
        let before = pack_count(&repo);
        assert!(before >= DUE_PACKS);
        assert_eq!(settle(&scratch.0), 0);
        assert_eq!(pack_count(&repo), before);
        assert_readable(&repo, "log/segment", &versions);
        fsck(&scratch.0);
    }

    /// Windows keeps a file another reader has open: the removal of a
    /// merged pack can take its index and leave its pack file. That file is
    /// no pack, and a merge passes it over.
    #[test]
    fn a_pack_file_without_its_index_is_not_merged() {
        let scratch = Scratch::new("orphan");
        let repo = scratch.repo();
        let orphan = "pack-0000000000000000000000000000000000000000";
        std::fs::create_dir_all(pack_dir(&repo)).unwrap();
        std::fs::write(pack_dir(&repo).join(format!("{orphan}.pack")), b"PACK").unwrap();
        assert!(
            packs(&pack_dir(&repo))
                .unwrap()
                .iter()
                .all(|(stem, _)| stem != orphan)
        );
        let dir = scratch.0.clone();
        let versions = append(&repo, 40, 100, || {
            settle(&dir);
        });
        assert_readable(&repo, "log/segment", &versions);
    }

    /// The index of a pack another hand removed after the listing: no
    /// objects, and no failure.
    #[test]
    fn the_index_of_a_removed_pack_holds_nothing() {
        let scratch = Scratch::new("removed-index");
        let gone = scratch
            .0
            .join("pack-0000000000000000000000000000000000000000.idx");
        assert!(index_entries(&gone).unwrap().is_none());
        assert!(index_count(&gone).unwrap().is_none());
    }

    #[test]
    fn a_cancelled_merge_removes_nothing() {
        let scratch = Scratch::new("cancelled");
        let repo = scratch.repo();
        let versions = append(&repo, DUE_PACKS + 4, 100, || {});
        let before = pack_count(&repo);
        let cancel = Arc::new(AtomicBool::new(true));
        assert_eq!(merge_step(&scratch.0, &cancel), Step::Cancelled);
        assert_eq!(pack_count(&repo), before);
        assert_readable(&repo, "log/segment", &versions);
        // What it left is what a merge starts from again.
        cancel.store(false, Ordering::Release);
        assert_eq!(merge_step(&scratch.0, &cancel), Step::Merged);
        assert!(pack_count(&repo) < before);
        assert_readable(&repo, "log/segment", &versions);
        fsck(&scratch.0);
    }

    // The task.

    /// Waits on a condition with a deadline.
    fn within(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether the merge lock of the Vault at `root` can be taken now.
    fn lock_is_free(root: &Path) -> bool {
        let Ok(file) = std::fs::File::open(root.join(".scheda").join("pack.lock")) else {
            return true;
        };
        file.try_lock().is_ok()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_task_merges_after_a_wake_and_holds_no_lock_when_it_is_done() {
        let scratch = Scratch::new("task");
        let repo = scratch.repo();
        let versions = append(&repo, 3 * DUE_PACKS, 100, || {});
        let signal = Arc::new(Merges::default());
        let packer = Packer::new(Arc::clone(&signal), &scratch.0);
        packer.start(Arc::new(Repos::new(&scratch.0)));
        let root = scratch.0.clone();
        let objects = pack_dir(&repo);
        tokio::task::spawn_blocking(move || {
            within("the merge at start did not end", || {
                packs(&objects).unwrap().len() < DUE_PACKS && lock_is_free(&root)
            });
        })
        .await
        .unwrap();
        // A write wakes it.
        for i in 0..DUE_PACKS {
            write(&repo, "log/segment", &noise(900 + i as u64, 50));
        }
        signal.wake();
        let (root, objects) = (scratch.0.clone(), pack_dir(&repo));
        tokio::task::spawn_blocking(move || {
            within("the merge after a wake did not end", || {
                packs(&objects).unwrap().len() < DUE_PACKS && lock_is_free(&root)
            });
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(20), packer.stop())
            .await
            .unwrap();
        assert!(lock_is_free(&scratch.0));
        drop(versions);
        fsck(&scratch.0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn close_cancels_a_running_merge_and_holds_no_lock_afterwards() {
        let scratch = Scratch::new("close");
        let repo = scratch.repo();
        let versions = append(&repo, DUE_PACKS + 2, 100, || {});
        let before = pack_count(&repo);
        let objects = repo.path().join("objects");
        // The merge stops in its middle until it is cancelled.
        let (started, running) = mpsc::channel::<()>();
        let started = std::sync::Mutex::new(started);
        hooks::on_merge(&objects, move |cancel| {
            let _ = started
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .send(());
            let deadline = Instant::now() + Duration::from_secs(30);
            while !cancel.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let signal = Arc::new(Merges::default());
        let packer = Packer::new(Arc::clone(&signal), &scratch.0);
        packer.start(Arc::new(Repos::new(&scratch.0)));
        tokio::task::spawn_blocking(move || running.recv_timeout(Duration::from_secs(30)))
            .await
            .unwrap()
            .unwrap();
        assert!(!lock_is_free(&scratch.0), "the merge holds the lock");
        let began = Instant::now();
        tokio::time::timeout(Duration::from_secs(20), packer.stop())
            .await
            .unwrap();
        assert!(began.elapsed() < Duration::from_secs(10));
        hooks::clear(&objects);
        assert!(lock_is_free(&scratch.0), "no lock after the stop");
        assert_eq!(pack_count(&repo), before);
        assert_readable(&repo, "log/segment", &versions);
        fsck(&scratch.0);
    }

    // The copy.

    /// Runs `git args` in `dir` with `config` set, feeding it `input`;
    /// returns what it prints. Plain `git` on a local scratch repository.
    fn run_git(dir: &Path, config: &[&str], args: &[&str], input: &str) -> String {
        use std::io::Write as _;
        let mut command = std::process::Command::new("git");
        command.args(["-c", "gc.auto=0", "-c", "user.name=Test"]);
        command.args(["-c", "user.email=test@example.test", "-C"]);
        command.arg(dir);
        for setting in config {
            command.args(["-c", setting]);
        }
        command.args(args);
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// A repository made with the `git` command line, as a person's would
    /// be: `rounds` rounds of `per_round` commits that each add a line to
    /// one file, and a pack of the objects of each round. `config` is set
    /// for every git command. Returns the content of `f.txt` at each commit.
    fn repository_of_rounds(
        scratch: &Scratch,
        rounds: usize,
        per_round: usize,
        config: &[&str],
    ) -> Vec<String> {
        let dir = &scratch.0;
        run_git(dir, config, &["init", "-q"], "");
        let mut text = String::new();
        let mut versions = Vec::new();
        for round in 0..rounds {
            for n in 0..per_round {
                text.push_str(&hex(&noise((round * per_round + n) as u64 + 1, 40)));
                text.push('\n');
                std::fs::write(dir.join("f.txt"), &text).unwrap();
                run_git(dir, config, &["add", "f.txt"], "");
                run_git(dir, config, &["commit", "-q", "-m", "v"], "");
                versions.push(text.clone());
            }
            run_git(dir, config, &["repack", "-q", "-d"], "");
        }
        versions
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// The kinds of the entries of the pack `stem` (1 to 4 objects, 6 a
    /// delta by offset, 7 a delta by id).
    fn kinds_of(dir: &Path, stem: &str) -> Vec<u8> {
        let pack_dir = dir.join(".git/objects/pack");
        let entries = index_entries(&pack_dir.join(format!("{stem}.idx")))
            .unwrap()
            .unwrap();
        let bytes = std::fs::read(pack_dir.join(format!("{stem}.pack"))).unwrap();
        entries
            .iter()
            .map(|(offset, _)| {
                let at = usize::try_from(*offset).unwrap();
                super::parse_header(&bytes[at..at + 16]).unwrap().0
            })
            .collect()
    }

    fn merge_all_with(dir: &Path, limits: Limits) -> usize {
        let merges = Merges::new(limits, Arc::default());
        let cancel = AtomicBool::new(false);
        let repo = Repository::open(dir).unwrap();
        let mut merged = 0;
        while merge_due(&repo, &merges, &cancel).unwrap() == Step::Merged {
            merged += 1;
            assert!(merged < 100);
        }
        merged
    }

    /// `git verify-pack` of every pack in the folder, and `git fsck`.
    fn verified(dir: &Path) {
        for stem in pack_stems(&dir.join(".git/objects/pack")).unwrap() {
            let idx = dir.join(".git/objects/pack").join(format!("{stem}.idx"));
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["verify-pack"])
                .arg(&idx)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "verify-pack {stem}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        fsck(dir);
    }

    /// Every commit holds the version of `f.txt` it was made with.
    fn assert_versions(dir: &Path, versions: &[String]) {
        let repo = Repository::open(dir).unwrap();
        let mut walk = repo.revwalk().unwrap();
        walk.push_head().unwrap();
        let commits: Vec<Oid> = walk.map(Result::unwrap).collect();
        assert_eq!(commits.len(), versions.len());
        for (id, version) in commits.iter().rev().zip(versions) {
            let tree = repo.find_commit(*id).unwrap().tree().unwrap();
            let entry = tree.get_path(Path::new("f.txt")).unwrap();
            assert_eq!(
                repo.find_blob(entry.id()).unwrap().content(),
                version.as_bytes()
            );
        }
    }

    #[test]
    fn copied_deltas_by_offset_survive_a_merge() {
        let scratch = Scratch::new("copy-ofs");
        let versions = repository_of_rounds(&scratch, 4, 12, &[]);
        let before: usize = pack_stems(&scratch.0.join(".git/objects/pack"))
            .unwrap()
            .iter()
            .map(|stem| {
                kinds_of(&scratch.0, stem)
                    .iter()
                    .filter(|k| **k == 6)
                    .count()
            })
            .sum();
        assert!(before > 0, "the fixture has no delta by offset");
        assert_eq!(merge_all_with(&scratch.0, Limits { due: 2, hard: 99 }), 1);
        let stems = pack_stems(&scratch.0.join(".git/objects/pack")).unwrap();
        assert_eq!(stems.len(), 1);
        let after = kinds_of(&scratch.0, &stems[0])
            .iter()
            .filter(|k| **k == 6)
            .count();
        assert_eq!(after, before, "the deltas are copied, not made again");
        verified(&scratch.0);
        assert_versions(&scratch.0, &versions);
    }

    #[test]
    fn copied_deltas_by_id_survive_a_merge() {
        let scratch = Scratch::new("copy-ref");
        let versions = repository_of_rounds(&scratch, 4, 12, &["repack.useDeltaBaseOffset=false"]);
        let before: usize = pack_stems(&scratch.0.join(".git/objects/pack"))
            .unwrap()
            .iter()
            .map(|stem| {
                kinds_of(&scratch.0, stem)
                    .iter()
                    .filter(|k| **k == 7)
                    .count()
            })
            .sum();
        assert!(before > 0, "the fixture has no delta by id");
        assert_eq!(merge_all_with(&scratch.0, Limits { due: 2, hard: 99 }), 1);
        let stems = pack_stems(&scratch.0.join(".git/objects/pack")).unwrap();
        assert_eq!(stems.len(), 1);
        let after = kinds_of(&scratch.0, &stems[0])
            .iter()
            .filter(|k| **k == 7)
            .count();
        assert_eq!(after, before);
        verified(&scratch.0);
        assert_versions(&scratch.0, &versions);
    }

    #[test]
    fn a_pack_a_crash_left_behind_with_all_its_objects_in_another_is_dropped() {
        let scratch = Scratch::new("copy-leftover");
        let versions = repository_of_rounds(&scratch, 3, 10, &[]);
        // A pack of everything while the three it was made from stay: what a
        // crash between the new pack and the removal of the old ones leaves.
        run_git(&scratch.0, &[], &["repack", "-q", "-a"], "");
        assert_eq!(
            pack_stems(&scratch.0.join(".git/objects/pack"))
                .unwrap()
                .len(),
            4
        );
        merge_all_with(&scratch.0, Limits { due: 2, hard: 99 });
        let stems = pack_stems(&scratch.0.join(".git/objects/pack")).unwrap();
        assert_eq!(stems.len(), 1, "{stems:?}");
        // Every object once.
        let count = index_count(
            &scratch
                .0
                .join(".git/objects/pack")
                .join(format!("{}.idx", stems[0])),
        )
        .unwrap()
        .unwrap();
        let listed = run_git(&scratch.0, &[], &["rev-list", "--objects", "--all"], "");
        assert_eq!(count, listed.lines().count() as u64);
        verified(&scratch.0);
        assert_versions(&scratch.0, &versions);
    }

    #[test]
    fn packs_that_overlap_in_part_keep_each_object_once_and_their_deltas_whole() {
        let scratch = Scratch::new("copy-overlap");
        let versions = repository_of_rounds(&scratch, 1, 24, &[]);
        let dir = &scratch.0;
        let pack_dir = dir.join(".git/objects/pack");
        let commits: Vec<String> = run_git(dir, &[], &["rev-list", "--reverse", "HEAD"], "")
            .lines()
            .map(str::to_owned)
            .collect();
        // Two packs of the same history: the objects of the first 17
        // commits, and those of the last 14 (seven commits are in both).
        // The smaller is copied first, so the larger loses its overlap and
        // its deltas on it.
        let first = run_git(dir, &[], &["rev-list", "--objects", &commits[16]], "");
        let second = run_git(
            dir,
            &[],
            &[
                "rev-list",
                "--objects",
                &commits[23],
                &format!("^{}", commits[9]),
            ],
            "",
        );
        let whole = pack_stems(&pack_dir).unwrap();
        for list in [&first, &second] {
            let ids: String = list
                .lines()
                .map(|line| format!("{}\n", line.split(' ').next().unwrap_or("")))
                .collect();
            run_git(
                dir,
                &[],
                &[
                    "pack-objects",
                    "-q",
                    "--delta-base-offset",
                    &pack_dir.join("pack").to_string_lossy(),
                ],
                &ids,
            );
        }
        for stem in &whole {
            for extension in ["pack", "idx"] {
                std::fs::remove_file(pack_dir.join(format!("{stem}.{extension}"))).unwrap();
            }
        }
        assert_eq!(pack_stems(&pack_dir).unwrap().len(), 2);
        verified(dir);
        assert_eq!(merge_all_with(dir, Limits { due: 2, hard: 99 }), 1);
        let stems = pack_stems(&pack_dir).unwrap();
        assert_eq!(stems.len(), 1);
        let count = index_count(&pack_dir.join(format!("{}.idx", stems[0])))
            .unwrap()
            .unwrap();
        let all = run_git(dir, &[], &["rev-list", "--objects", "--all"], "");
        assert_eq!(count, all.lines().count() as u64, "every object once");
        let kinds = kinds_of(dir, &stems[0]);
        assert!(
            kinds.contains(&7),
            "a delta whose base came already is a delta by id: {kinds:?}"
        );
        verified(dir);
        assert_versions(dir, &versions);
    }

    #[cfg(unix)]
    #[test]
    fn a_write_goes_to_a_repository_whose_path_is_not_utf_8() {
        use std::os::unix::ffi::OsStrExt;

        crate::git::isolate_test_git_env();
        let base = std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let mut name = format!("scheda-pack-non-utf8-{}-", std::process::id()).into_bytes();
        name.extend_from_slice(&[0xff, 0xfe]);
        let dir = base.join(std::ffi::OsStr::from_bytes(&name));
        let _ = std::fs::remove_dir_all(&dir);
        crate::testing::remove_when_the_test_ends(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::repo::init_library().unwrap();
        let repo = Repository::init(&dir).unwrap();
        assert!(repo.path().to_str().is_none(), "the path is UTF-8");
        write(&repo, "a/b", b"one");
        write(&repo, "a/b", b"two");
        assert_eq!(pack_count(&repo), 2);
        assert_eq!(loose_count(&repo), 0);
        let tree = repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap();
        let entry = tree.get_path(Path::new("a/b")).unwrap();
        assert_eq!(repo.find_blob(entry.id()).unwrap().content(), b"two");
    }

    #[test]
    fn a_merge_takes_packs_only_while_what_it_has_is_below_the_cap() {
        // Sixteen packs of 30 MiB: the third brings the sum past 64 MiB,
        // and no pack is taken after it.
        let sizes: Vec<Size> = (0..DUE_PACKS)
            .map(|_| Size {
                bytes: 30 * 1024 * 1024,
                objects: 1000,
            })
            .collect();
        assert_eq!(plan(&sizes, 0, &Limits::DEFAULT).packs.len(), 3);
        // The same for objects.
        let sizes: Vec<Size> = (0..DUE_PACKS)
            .map(|_| Size {
                bytes: 1000,
                objects: 40_000,
            })
            .collect();
        assert_eq!(plan(&sizes, 0, &Limits::DEFAULT).packs.len(), 3);
    }

    /// The git library never forgets a pack, so a handle that saw the packs
    /// a merge removed gets slower with every merge. The pool lets such a
    /// handle go and opens a new one, and keeps one that saw no removal.
    #[test]
    fn a_repository_handle_that_saw_packs_a_merge_removed_is_not_leased_again() {
        let scratch = Scratch::new("generation");
        let repo = scratch.repo();
        append(&repo, 6, 100, || {});
        let repos = Repos::new(&scratch.0);
        drop(repos.lease(Repo::Main).unwrap().unwrap());
        assert_eq!(repos.idle_main(), 1, "a handle that saw no removal is kept");
        let merges = Merges::new(Limits { due: 2, hard: 99 }, repos.pack_generation());
        let lease = repos.lease(Repo::Main).unwrap().unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(merge_due(&lease, &merges, &cancel).unwrap(), Step::Merged);
        // The handle in hand works on; once it is back it is let go.
        assert!(lease.head().is_ok());
        drop(lease);
        assert_eq!(
            repos.idle_main(),
            0,
            "a handle that saw removed packs is let go"
        );
        // And the next one is new, and is kept.
        drop(repos.lease(Repo::Main).unwrap().unwrap());
        assert_eq!(repos.idle_main(), 1);
    }

    // The bound on the packs.

    /// The packs below the cap of the Vault at `root`.
    fn below_cap(root: &Path) -> usize {
        Merges::default()
            .below_cap(&root.join(".git").join("objects"))
            .unwrap()
    }

    fn vault_root(scratch: &Scratch, limits: Limits) -> PathBuf {
        let root = std::fs::canonicalize(&scratch.0).unwrap().join("vault");
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        hooks::set_limits(&root, limits);
        root
    }

    fn note(n: u64) -> scheda_api::v1::DocumentCreation {
        scheda_api::v1::DocumentCreation::default()
            .with_id(format!("d{n:03}.md"))
            .with_raw(format!("document {n}").into_bytes())
            .with_note("n")
    }

    const SHRUNK: Limits = Limits { due: 3, hard: 6 };

    #[tokio::test(flavor = "multi_thread")]
    async fn a_run_of_open_write_close_never_leaves_hard_packs_below_the_cap() {
        let scratch = Scratch::new("bound");
        let root = vault_root(&scratch, SHRUNK);
        let options = || scheda_api::v1::OpenOptions::default().with_write_timeout_ms(20_000);
        let vault = crate::testing::open(&root, options()).await.unwrap();
        crate::testing::create_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        vault.close().await.unwrap();
        let mut most = 0;
        // The command line: a Vault is closed milliseconds after it is
        // opened, which cancels the merge of its task.
        for n in 0..60 {
            let vault = crate::testing::open(&root, options()).await.unwrap();
            crate::testing::create(&vault, "notes", note(n))
                .await
                .unwrap();
            // Counted with the task stopped: while a merge of the task
            // removes the packs it replaced, its new pack is there beside
            // them for a moment.
            vault.close().await.unwrap();
            most = most.max(below_cap(&root));
        }
        assert!(most < SHRUNK.hard, "{most} packs below the cap");
        fsck(&root);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_whose_merge_fails_fails_with_its_code_and_moves_nothing() {
        let scratch = Scratch::new("inline-fails");
        let root = vault_root(&scratch, Limits { due: 3, hard: 5 });
        let objects = root.join(".git").join("objects");
        let options = scheda_api::v1::OpenOptions::default().with_write_timeout_ms(20_000);
        let vault = crate::testing::open(&root, options).await.unwrap();
        crate::testing::create_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        // From here every merge fails, the task's and the write's.
        hooks::fail_merges(&objects);
        let head = || {
            Repository::open(&root)
                .unwrap()
                .refname_to_id("HEAD")
                .unwrap()
        };
        let mut failed = None;
        for n in 0..20 {
            let before = head();
            match crate::testing::create(&vault, "notes", note(n)).await {
                Ok(_) => assert_ne!(head(), before),
                Err(error) => {
                    failed = Some((n, error, before));
                    break;
                }
            }
        }
        let (n, error, before) = failed.expect("no write needed to merge");
        assert!(error.is(scheda_api::v1::VaultErrorCode::Storage), "{error}");
        assert_eq!(head(), before, "HEAD moved");
        assert!(!root.join("notes").join(format!("d{n:03}.md")).exists());
        // The merge works again, and so does the write.
        hooks::clear(&objects);
        crate::testing::create(&vault, "notes", note(n))
            .await
            .unwrap();
        assert_ne!(head(), before);
        vault.close().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_that_finds_the_merge_lock_held_by_the_task_waits_then_merges_and_writes() {
        let scratch = Scratch::new("inline-waits");
        let root = vault_root(&scratch, Limits { due: 3, hard: 5 });
        let objects = root.join(".git").join("objects");
        let options = scheda_api::v1::OpenOptions::default().with_write_timeout_ms(20_000);
        let vault = crate::testing::open(&root, options).await.unwrap();
        crate::testing::create_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        // The task's merge is held in its middle, with the lock, until the
        // guard goes: on the way out of the test too.
        let hold = hooks::hold_merges(&objects);
        let mut n = 0;
        while !hold.entered() {
            crate::testing::create(&vault, "notes", note(n))
                .await
                .unwrap();
            n += 1;
            assert!(n < 30, "the task never started a merge");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Writes go on until one has to merge, and waits for the lock.
        let waiting = loop {
            let write = {
                let vault = Arc::clone(&vault);
                let at = n;
                tokio::spawn(async move { crate::testing::create(&vault, "notes", note(at)).await })
            };
            n += 1;
            tokio::time::sleep(Duration::from_millis(150)).await;
            if !write.is_finished() {
                break write;
            }
            write.await.unwrap().unwrap();
            assert!(n < 40, "no write waited for the lock");
        };
        assert!(!lock_is_free(&root), "the task holds the lock");
        let before = Repository::open(&root)
            .unwrap()
            .refname_to_id("HEAD")
            .unwrap();
        drop(hold);
        tokio::time::timeout(Duration::from_secs(30), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let after = Repository::open(&root)
            .unwrap()
            .refname_to_id("HEAD")
            .unwrap();
        assert_ne!(after, before, "the write landed");
        assert!(below_cap(&root) < 5);
        vault.close().await.unwrap();
        fsck(&root);
    }

    // The object cache.

    /// The cache budget is 16 MiB after the first `Open` of a process whose
    /// `git2` filled the cache before. The setting is the process's, so the
    /// check runs in a process of its own: the test binary again, running
    /// [`cache_child`].
    #[test]
    fn the_cache_budget_is_16_mib_after_the_first_open_when_git2_filled_the_cache_before() {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let exe = std::env::current_exe().unwrap();
        let mut child = Child(
            std::process::Command::new(exe)
                .args(["--exact", "--ignored", "--nocapture"])
                .arg("pack::tests::cache_child")
                .env("SCHEDA_CACHE_CHILD", "1")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(120);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "the child did not end");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "the child failed: {status}");
    }

    /// The bytes the library's cache holds and its budget.
    fn cached() -> (isize, isize) {
        // SAFETY: only reads two counters of the library, which guards them.
        unsafe { git2::opts::get_cached_memory() }.unwrap()
    }

    /// The child of the test above. Ignored: it is run by that test alone,
    /// and only as a process of its own.
    #[tokio::test]
    #[ignore = "run by the test that spawns it, in a process of its own"]
    async fn cache_child() {
        if std::env::var_os("SCHEDA_CACHE_CHILD").is_none() {
            return;
        }
        let scratch = Scratch::new("cache");
        // `git2` alone fills the cache, with more than the budget: trees of
        // 60 entries, a few KB each.
        let repo = Repository::init(&scratch.0).unwrap();
        let blob = repo.blob(b"x").unwrap();
        let mut ids = Vec::new();
        for n in 0..9000u32 {
            let mut tree = repo.treebuilder(None).unwrap();
            for entry in 0..60u32 {
                tree.insert(format!("entry-{n:05}-{entry:03}.md"), blob, 0o100_644)
                    .unwrap();
            }
            ids.push(tree.write().unwrap());
        }
        for id in &ids {
            repo.find_tree(*id).unwrap();
        }
        let (used, max) = cached();
        assert_eq!(max, 256 * 1024 * 1024, "libgit2's own default");
        assert!(used > 16 * 1024 * 1024, "git2 filled {used} bytes");
        // The first Open of this process.
        let vault = crate::testing::open(&scratch.0.join("vault"), Default::default())
            .await
            .unwrap();
        let (_, max) = cached();
        assert_eq!(max, 16 * 1024 * 1024);
        vault.close().await.unwrap();
    }

    /// Bytes of anonymous memory the process holds, from the kernel.
    #[cfg(target_os = "linux")]
    pub(super) fn anonymous_bytes() -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let kb: u64 = status
            .lines()
            .find_map(|line| line.strip_prefix("RssAnon:"))
            .unwrap()
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .unwrap();
        kb * 1024
    }

    /// The path of the Document `n` in a layout of three levels of 16
    /// folders, by a hash of `n`: about 24 entries a leaf at 100 000.
    #[cfg(target_os = "linux")]
    pub(super) fn document_path(n: u64) -> String {
        let h = (n + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 20;
        format!(
            "{:x}/{:x}/{:x}/d{n:08}.md",
            h & 15,
            (h >> 4) & 15,
            (h >> 8) & 15
        )
    }

    /// The memory tests: ignored in the other targets, run alone by hand
    /// (`cargo test -p scheda --lib -- --ignored --test-threads=1 memory::`).
    #[cfg(target_os = "linux")]
    mod memory {
        use super::*;

        /// Packs written one by one with the merge task running: the number
        /// of packs stays bounded and the memory of the process does not
        /// grow with the history.
        #[test]
        #[ignore = "measures memory: run by hand, alone"]
        fn five_thousand_writes_with_the_merge_task_running_keep_the_packs_and_the_memory_bounded()
        {
            let scratch = Scratch::new("memory-writes");
            let repo = scratch.repo();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let _guard = runtime.enter();
            let repos = Arc::new(Repos::new(&scratch.0));
            let signal = Arc::new(Merges::new(Limits::DEFAULT, repos.pack_generation()));
            let packer = Packer::new(Arc::clone(&signal), &scratch.0);
            packer.start(Arc::clone(&repos));
            let (mut early, mut most) = (0, 0);
            for n in 0..5000u64 {
                let lease = repos.lease(Repo::Main).unwrap().unwrap();
                write(&lease, &document_path(n), &noise(n + 1, 600));
                drop(lease);
                signal.wake();
                if n == 1000 {
                    early = anonymous_bytes();
                }
                if n > 1000 {
                    most = most.max(pack_count(&repo));
                }
            }
            within("the merges did not end", || {
                pack_count(&repo) < DUE_PACKS && lock_is_free(&scratch.0)
            });
            let late = anonymous_bytes();
            runtime.block_on(packer.stop());
            assert!(most < 4 * DUE_PACKS, "{most} packs at most");
            assert!(
                late < early + 64 * 1024 * 1024,
                "anonymous memory {early} after 1000 writes, {late} after 5000"
            );
            fsck(&scratch.0);
        }
    }

    /// The full-size measurements, run by hand once, in release, without
    /// features: `cargo test -p scheda --lib --release --no-default-features
    /// -- --ignored --nocapture --test-threads=1 measure::`.
    #[cfg(target_os = "linux")]
    mod measure {
        use std::sync::atomic::AtomicU64;

        use super::*;

        /// Seconds between two progress lines.
        const EVERY: Duration = Duration::from_secs(30);

        fn count_from_env(name: &str, default: u64) -> u64 {
            std::env::var(name)
                .ok()
                .and_then(|text| text.parse().ok())
                .unwrap_or(default)
        }

        /// The mean time of `reads` random reads of a Document by its path
        /// from `HEAD`, in microseconds.
        fn read_micros(repo: &Repository, documents: u64, reads: u64) -> f64 {
            let mut x = 0x1234_5678_9abc_def1_u64;
            let began = Instant::now();
            for _ in 0..reads {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let n = x % documents;
                let tree = repo
                    .head()
                    .unwrap()
                    .peel_to_commit()
                    .unwrap()
                    .tree()
                    .unwrap();
                let entry = tree.get_path(Path::new(&document_path(n))).unwrap();
                repo.find_blob(entry.id()).unwrap();
            }
            began.elapsed().as_secs_f64() * 1e6 / reads as f64
        }

        /// Writes with the merge task running; the read time, the anonymous
        /// memory, and the packs after `EARLY` writes and at the end.
        /// `SCHEDA_MEASURE_WRITES` sets the count (100 000).
        #[test]
        #[ignore = "measures time and memory: run by hand"]
        fn reads_and_memory_after_a_hundred_thousand_writes_with_the_merge_task_running() {
            const EARLY: u64 = 2000;
            let total = count_from_env("SCHEDA_MEASURE_WRITES", 100_000);
            let scratch = Scratch::new("measure-writes");
            let repo = scratch.repo();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let _guard = runtime.enter();
            let repos = Arc::new(Repos::new(&scratch.0));
            let signal = Arc::new(Merges::new(Limits::DEFAULT, repos.pack_generation()));
            let packer = Packer::new(Arc::clone(&signal), &scratch.0);
            packer.start(Arc::clone(&repos));
            let (began, mut last) = (Instant::now(), Instant::now());
            let report = |label: &str, n: u64| {
                within("the merges did not end", || {
                    pack_count(&repo) < DUE_PACKS && lock_is_free(&scratch.0)
                });
                eprintln!(
                    "{label}: {n} writes, {} packs, read {:.1} us, anonymous {} MiB, {:.0} s",
                    pack_count(&repo),
                    read_micros(&repo, n, 20_000),
                    anonymous_bytes() >> 20,
                    began.elapsed().as_secs_f64()
                );
            };
            for n in 0..total {
                let lease = repos.lease(Repo::Main).unwrap().unwrap();
                write(&lease, &document_path(n), &noise(n + 1, 600));
                drop(lease);
                signal.wake();
                if total > 10_000 && n + 1 == EARLY {
                    report("early", n + 1);
                }
                if last.elapsed() > EVERY {
                    last = Instant::now();
                    eprintln!(
                        "progress: {n} writes, {:.0} s",
                        began.elapsed().as_secs_f64()
                    );
                }
            }
            report("end", total);
            runtime.block_on(packer.stop());
        }

        /// The id of the Document `n`, in the shape a consumer uses: `l` and
        /// 32 hex digits.
        fn id_of(n: u64) -> String {
            let hi = (n + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let lo = (n + 1).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ (hi >> 29);
            format!("l{:032x}", (u128::from(hi) << 64) | u128::from(lo))
        }

        /// The next number of a xorshift sequence.
        fn next(x: &mut u64) -> u64 {
            *x ^= *x << 13;
            *x ^= *x >> 7;
            *x ^= *x << 17;
            *x
        }

        fn none() -> scheda_api::v1::OpenOptions {
            scheda_api::v1::OpenOptions::default()
                .with_work_tree(scheda_api::v1::WorkTree::WORK_TREE_NONE)
                .with_write_timeout_ms(60_000)
        }

        /// The mean time of `reads` random `get`s out of `documents`
        /// Documents, in microseconds.
        async fn get_micros(vault: &Arc<crate::vault::Vault>, documents: u64, reads: u64) -> f64 {
            let mut x = 0x1234_5678_9abc_def1_u64;
            let began = Instant::now();
            for _ in 0..reads {
                let id = id_of(next(&mut x) % documents);
                let got = crate::testing::step(vault, move |cx, vault| {
                    vault.get(
                        cx,
                        "docs",
                        scheda_api::v1::DocumentId {
                            id: Some(id),
                            ..Default::default()
                        },
                    )
                })
                .await
                .unwrap();
                assert!(got.raw.is_some());
            }
            began.elapsed().as_secs_f64() * 1e6 / reads as f64
        }

        /// The mean of the last `window` of `times` (seconds), in microseconds.
        fn mean_micros(times: &[f64], window: usize) -> f64 {
            let tail = &times[times.len().saturating_sub(window)..];
            tail.iter().sum::<f64>() * 1e6 / tail.len() as f64
        }

        /// G2: writes through a Vault with no work tree, one Document a
        /// write: the time of a write, the time of a read, the anonymous
        /// memory, and the packs, after the early writes and at the end.
        /// `SCHEDA_MEASURE_WRITES` sets the count (100 000).
        #[tokio::test]
        #[ignore = "measures time and memory: run by hand"]
        async fn writes_through_a_vault_without_a_work_tree_keep_their_time_read_and_memory() {
            let total = count_from_env("SCHEDA_MEASURE_WRITES", 100_000);
            let early = 2000.min(total / 3).max(1);
            let window = 1000.min(early as usize);
            let scratch = Scratch::new("measure-g2");
            let vault = crate::testing::open(&scratch.0, none()).await.unwrap();
            crate::testing::create_collection(
                &vault,
                serde_json::json!({"name": "docs", "schema": {"standard": {}}}),
            )
            .await
            .unwrap();
            let (began, mut last) = (Instant::now(), Instant::now());
            let mut times: Vec<f64> = Vec::with_capacity(total as usize);
            // (write us, read us, anonymous bytes, packs)
            let mut seen: Vec<(f64, f64, u64, usize)> = Vec::new();
            for n in 0..total {
                let id = id_of(n);
                let request = crate::testing::upsert_of(&id, &noise(n + 1, 2048));
                let at = Instant::now();
                crate::testing::upsert(&vault, "docs", request)
                    .await
                    .unwrap();
                times.push(at.elapsed().as_secs_f64());
                if n + 1 == early || n + 1 == total {
                    let packs = pack_count(&Repository::open(&scratch.0).unwrap());
                    let read = get_micros(&vault, n + 1, 5000).await;
                    let write = mean_micros(&times, window);
                    let memory = anonymous_bytes();
                    eprintln!(
                        "{} writes: write {write:.0} us (last {window}), read {read:.0} us, anonymous {} MiB, {packs} packs, {:.0} s",
                        n + 1,
                        memory >> 20,
                        began.elapsed().as_secs_f64()
                    );
                    seen.push((write, read, memory, packs));
                }
                if last.elapsed() > EVERY {
                    last = Instant::now();
                    eprintln!(
                        "progress: {n} writes, {:.0} s",
                        began.elapsed().as_secs_f64()
                    );
                }
            }
            vault.close().await.unwrap();
            let (early, late) = (seen[0], seen[seen.len() - 1]);
            assert!(
                late.0 <= 3.0 * early.0,
                "write {} us then {} us",
                early.0,
                late.0
            );
            assert!(
                late.1 <= 3.0 * early.1,
                "read {} us then {} us",
                early.1,
                late.1
            );
            assert!(
                late.2 <= early.2 + 200 * 1024 * 1024,
                "anonymous memory {} then {}",
                early.2,
                late.2
            );
        }

        /// Writes the folder of `tree` into `repo`, over `base` when given:
        /// every entry a blob holding its name, every subfolder under its
        /// chain of labels. Returns the tree.
        fn write_folder(
            repo: &Repository,
            tree: &crate::layout::Tree,
            base: Option<&git2::Tree<'_>>,
            made: &mut u64,
            progress: &mut Instant,
        ) -> Oid {
            let mut builder = repo.treebuilder(base).unwrap();
            for name in tree.entries() {
                let blob = repo.blob(name).unwrap();
                builder
                    .insert(std::str::from_utf8(name).unwrap(), blob, 0o100_644)
                    .unwrap();
                *made += 1;
                if progress.elapsed() > EVERY {
                    *progress = Instant::now();
                    eprintln!("progress: {made} blobs written");
                }
            }
            for (added, child) in tree.children() {
                let mut oid = write_folder(repo, child, None, made, progress);
                let labels = crate::layout::encode(added);
                for label in labels[1..].iter().rev() {
                    let mut wrap = repo.treebuilder(None).unwrap();
                    wrap.insert(label, oid, 0o040_000).unwrap();
                    oid = wrap.write().unwrap();
                }
                builder.insert(&labels[0], oid, 0o040_000).unwrap();
            }
            builder.write().unwrap()
        }

        /// G3: a write into a collection of N Documents, N in
        /// `SCHEDA_MEASURE_DOCUMENTS` (`100000,1000000`). The collection is
        /// laid out and written straight into the repository as one commit
        /// and packed, then the Vault is opened through the API.
        #[tokio::test]
        #[ignore = "measures time and memory: run by hand"]
        async fn a_write_into_a_collection_of_a_million_documents() {
            let sizes = std::env::var("SCHEDA_MEASURE_DOCUMENTS")
                .unwrap_or_else(|_| "100000,1000000".to_owned());
            for size in sizes.split(',') {
                let documents: u64 = size.trim().parse().unwrap();
                let scratch = Scratch::new(&format!("measure-g3-{documents}"));
                let vault = crate::testing::open(&scratch.0, none()).await.unwrap();
                crate::testing::create_collection(
                    &vault,
                    serde_json::json!({"name": "docs", "schema": {"standard": {}}}),
                )
                .await
                .unwrap();
                vault.close().await.unwrap();
                drop(vault);

                let began = Instant::now();
                let repo = Repository::open(&scratch.0).unwrap();
                let head = repo.head().unwrap();
                let branch = head.name().unwrap().to_owned();
                let parent = head.peel_to_commit().unwrap();
                let root = parent.tree().unwrap();
                let docs = root
                    .get_name("docs")
                    .unwrap()
                    .to_object(&repo)
                    .unwrap()
                    .into_tree()
                    .unwrap();
                let tree = crate::layout::Tree::build(b"", (0..documents).map(id_of));
                let built = began.elapsed();
                let (mut made, mut progress) = (0, Instant::now());
                let docs_oid = write_folder(&repo, &tree, Some(&docs), &mut made, &mut progress);
                let mut top = repo.treebuilder(Some(&root)).unwrap();
                top.insert("docs", docs_oid, 0o040_000).unwrap();
                let top = repo.find_tree(top.write().unwrap()).unwrap();
                let who = Signature::now("Measure", "measure@example.test").unwrap();
                let commit = repo
                    .commit(None, &who, &who, "documents", &top, &[&parent])
                    .unwrap();
                repo.reference(&branch, commit, true, "documents").unwrap();
                drop((top, docs, root, parent, head, tree));
                let written = began.elapsed();
                crate::git::git()
                    .current_dir(&scratch.0)
                    .arg("repack")
                    .arg("-a")
                    .arg("-d")
                    .arg("-q")
                    .run()
                    .unwrap();
                let repo = Repository::open(&scratch.0).unwrap();
                eprintln!(
                    "{documents} Documents: layout {:.1} s, written {:.1} s, packed {:.1} s, {} packs",
                    built.as_secs_f64(),
                    written.as_secs_f64(),
                    began.elapsed().as_secs_f64(),
                    pack_count(&repo)
                );

                let opened = Instant::now();
                let vault = crate::testing::open(&scratch.0, none()).await.unwrap();
                eprintln!("open {:.2} s", opened.elapsed().as_secs_f64());
                let mut x = 0x2545_f491_4f6c_dd1d_u64;
                let report = |label: &str, times: &[f64]| {
                    let mean = times.iter().sum::<f64>() / times.len() as f64;
                    let max = times.iter().copied().fold(0.0, f64::max);
                    eprintln!(
                        "{documents} Documents, {label}: mean {:.1} ms, max {:.1} ms",
                        mean * 1e3,
                        max * 1e3
                    );
                    mean
                };
                let mut fresh = Vec::new();
                for i in 0..200 {
                    let request = crate::testing::upsert_of(&id_of(documents + i), b"new");
                    let at = Instant::now();
                    crate::testing::upsert(&vault, "docs", request)
                        .await
                        .unwrap();
                    fresh.push(at.elapsed().as_secs_f64());
                }
                let mut updated = Vec::new();
                for i in 0..200 {
                    let request =
                        crate::testing::upsert_of(&id_of(next(&mut x) % documents), &noise(i, 64));
                    let at = Instant::now();
                    crate::testing::upsert(&vault, "docs", request)
                        .await
                        .unwrap();
                    updated.push(at.elapsed().as_secs_f64());
                }
                let new_mean = report("200 new ids", &fresh);
                let old_mean = report("200 updates", &updated);
                eprintln!(
                    "{documents} Documents, get: mean {:.0} us, {} MiB anonymous",
                    get_micros(&vault, documents, 5000).await,
                    anonymous_bytes() >> 20
                );
                vault.close().await.unwrap();
                assert!(new_mean < 0.05, "a new id took {new_mean} s on average");
                assert!(old_mean < 0.05, "an update took {old_mean} s on average");
            }
        }

        /// Packs `objects` of the overlay into `real` as one pack.
        fn pack_of(overlay: &Overlay, real: &Repository, objects: &[Oid]) {
            let mut builder = overlay.repo().packbuilder().unwrap();
            for id in objects {
                builder.insert_object(*id, None).unwrap();
            }
            super::super::stream(&mut builder, real, None).unwrap();
        }

        /// The worst merge: 16 packs of 12 000 objects of 400 bytes each,
        /// about 75 MiB and 192 000 objects, merged at once. Time and the
        /// peak of anonymous memory over what the process held before.
        #[test]
        #[ignore = "measures time and memory: run by hand"]
        fn the_worst_merge_at_the_cap() {
            let per_pack = count_from_env("SCHEDA_MEASURE_PACK_OBJECTS", 12_000);
            let scratch = Scratch::new("measure-merge");
            let repo = scratch.repo();
            let overlay = Overlay::over(&repo).unwrap();
            let mut seed = 0;
            for _ in 0..DUE_PACKS {
                let ids: Vec<Oid> = (0..per_pack)
                    .map(|_| {
                        seed += 1;
                        overlay.repo().blob(&noise(seed, 400)).unwrap()
                    })
                    .collect();
                pack_of(&overlay, &repo, &ids);
            }
            drop(overlay);
            let size = fs_size(&pack_dir(&repo));
            let base = anonymous_bytes();
            let peak = Arc::new(AtomicU64::new(base));
            let sampling = Arc::new(AtomicBool::new(true));
            let sampler = {
                let (peak, sampling) = (Arc::clone(&peak), Arc::clone(&sampling));
                std::thread::spawn(move || {
                    while sampling.load(Ordering::Acquire) {
                        peak.fetch_max(anonymous_bytes(), Ordering::AcqRel);
                        std::thread::sleep(Duration::from_millis(20));
                    }
                })
            };
            let began = Instant::now();
            let merges = settle(&scratch.0);
            let took = began.elapsed();
            sampling.store(false, Ordering::Release);
            sampler.join().unwrap();
            eprintln!(
                "merged {} objects in {} MiB: {merges} merge(s), {:.1} s, peak anonymous +{} MiB, {} packs left",
                u64::from(DUE_PACKS as u32) * per_pack,
                size >> 20,
                took.as_secs_f64(),
                (peak.load(Ordering::Acquire) - base) >> 20,
                pack_count(&repo)
            );
        }

        fn fs_size(dir: &Path) -> u64 {
            std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().metadata().unwrap().len())
                .sum()
        }

        /// A Document read with 1, 100, and 500 packs: 100 000 Documents,
        /// their objects spread at random over the packs.
        #[test]
        #[ignore = "measures time: run by hand"]
        fn reading_a_document_with_one_hundred_and_five_hundred_packs() {
            let documents = count_from_env("SCHEDA_MEASURE_DOCUMENTS", 100_000);
            for packs in [1usize, 100, 500] {
                let scratch = Scratch::new(&format!("measure-read-{packs}"));
                let repo = scratch.repo();
                let overlay = Overlay::over(&repo).unwrap();
                let mem = overlay.repo();
                let mut update = TreeUpdateBuilder::new();
                let mut objects: Vec<Oid> = Vec::new();
                for n in 0..documents {
                    let blob = mem.blob(&noise(n + 1, 600)).unwrap();
                    objects.push(blob);
                    update.upsert(document_path(n), blob, git2::FileMode::Blob);
                }
                let empty = mem.treebuilder(None).unwrap().write().unwrap();
                let base = mem.find_tree(empty).unwrap();
                let root = update.create_updated(mem, &base).unwrap();
                // Every tree below the root, and the root.
                let mut trees = vec![root];
                let mut at = 0;
                while at < trees.len() {
                    let tree = mem.find_tree(trees[at]).unwrap();
                    for entry in tree.iter() {
                        if entry.kind() == Some(git2::ObjectType::Tree) {
                            trees.push(entry.id());
                        }
                    }
                    at += 1;
                }
                objects.extend(trees);
                let tree = mem.find_tree(root).unwrap();
                let when = git2::Time::new(1_700_000_000, 0);
                let who = Signature::new("Test", "test@example.test", &when).unwrap();
                let commit = mem.commit(None, &who, &who, "all", &tree, &[]).unwrap();
                objects.push(commit);
                drop((tree, base));
                // Spread at random.
                let mut parts: Vec<Vec<Oid>> = vec![Vec::new(); packs];
                let mut x = 0x2545_f491_4f6c_dd1d_u64;
                for id in objects {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    parts[(x % packs as u64) as usize].push(id);
                }
                for part in &parts {
                    if !part.is_empty() {
                        pack_of(&overlay, &repo, part);
                    }
                }
                drop(overlay);
                repo.reference("refs/heads/main", commit, true, "all")
                    .unwrap();
                repo.set_head("refs/heads/main").unwrap();
                // A handle of its own, so the cache starts empty.
                let reader = Repository::open(&scratch.0).unwrap();
                let random = read_micros(&reader, documents, 20_000);
                let again = read_micros(&reader, documents, 20_000);
                eprintln!(
                    "{} packs: {documents} Documents, random read {random:.1} us, then {again:.1} us",
                    pack_count(&repo)
                );
            }
        }
    }
}
