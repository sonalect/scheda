//! File helpers shared by every layer that writes into a Vault: atomic
//! writes, the advisory lock, symlink checks, owner-only modes, and the
//! POSIX form of a Vault-relative path.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use scheda_api::v1::{Problem, VaultErrorCode};

use crate::error::{Context, Error, Fault, io};

/// Numbers the temporary files of this process.
static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

/// The prefix and the suffix of a temporary file's name.
const TMP_PREFIX: &str = ".scheda.";
const TMP_SUFFIX: &str = ".tmp";

/// An `flock` held on an open file. Dropping it unlocks before the file
/// closes: a close alone leaves the lock held while a child process another
/// thread forked still has the descriptor, until that child runs `exec`,
/// and a handle trying the lock in that moment would find it taken.
#[derive(Debug)]
pub(crate) struct Flock(fs::File);

impl Flock {
    /// Take ownership of `file`, which holds the lock.
    pub(crate) fn held(file: fs::File) -> Self {
        Self(file)
    }

    /// The locked file, for the holder that keeps data in it.
    pub(crate) fn file(&self) -> &fs::File {
        &self.0
    }
}

impl Drop for Flock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Writes `bytes` to `path` so that a reader sees the old file or the new
/// one, never a part: a temporary file in the same directory, flushed to
/// disk, then renamed over `path`. Missing parent directories are created.
///
/// The temporary file is `.scheda.{pid}.{n}.tmp`: no two writes of one
/// process, nor of two processes alive at once, share a name, and a name a
/// dead process left behind is passed over. It is removed when the write
/// fails after making it.
///
/// The file is made with mode `0666`, or `0777` when `executable`, so the
/// process umask decides what is left, as it does for a file git writes. No
/// library code reads or sets the umask.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8], executable: bool) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(io)
            .context("atomic_write: create directory")?;
    }
    let mode = if executable { 0o777 } else { 0o666 };
    write_tmp_then_rename(path, bytes, mode)
}

/// [`atomic_write`] for a file under `{vault}/.scheda/`: the directories it
/// makes and the file are owner-only from the moment they exist, so no
/// reader (`Check` among them) ever sees them with a wider mode.
pub(crate) fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        create_dir_private(dir).context("atomic_write: create directory")?;
    }
    write_tmp_then_rename(path, bytes, 0o600)
}

/// [`atomic_write_private`] that never replaces: `bytes` go to `path` only
/// when no file is there, and a reader sees no file or the whole new one.
/// Returns whether this call made the file; `false` leaves the file that
/// another writer put there first. The temporary file is flushed, then moved
/// to `path` by a rename that fails when `path` exists, and removed when it
/// was not moved.
pub(crate) fn create_private(path: &Path, bytes: &[u8]) -> Result<bool, Error> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    create_dir_private(dir).context("atomic_write: create directory")?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let (tmp, file) = create_temporary(dir, &options, &NEXT_TMP)?;
    let placed = fill(file, bytes).and_then(|()| rename_no_replace(&tmp, path));
    if !matches!(placed, Ok(true)) {
        let _ = fs::remove_file(&tmp);
    }
    placed
}

/// Removes the temporary files of [`atomic_write`] directly in `dir`: what
/// writes left that their process did not live to rename. The caller makes
/// sure no write into `dir` runs meanwhile. A missing `dir` has none.
///
/// # Errors
///
/// `STORAGE` when `dir` cannot be read or a file there removed.
pub(crate) fn remove_temporary(dir: &Path) -> Result<(), Error> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(io(err)).context("read a folder for temporary files"),
    };
    for entry in entries {
        let entry = entry
            .map_err(io)
            .context("read a folder for temporary files")?;
        let name = entry.file_name();
        let temporary = name
            .to_str()
            .is_some_and(|name| name.starts_with(TMP_PREFIX) && name.ends_with(TMP_SUFFIX));
        if !temporary {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io(err)).context("remove a temporary file"),
        }
    }
    Ok(())
}

/// Makes `dir` and its missing parents, each owner-only from the moment it
/// exists.
pub(crate) fn create_dir_private(dir: &Path) -> Result<(), Error> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(io).map_err(Error::from)
}

/// Writes `bytes` to a new temporary file beside `path`, created with Unix
/// `mode` (the umask applies to it), and renames it over `path`.
fn write_tmp_then_rename(path: &Path, bytes: &[u8], mode: u32) -> Result<(), Error> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    let (tmp, file) = create_temporary(dir, &options, &NEXT_TMP)?;
    let written = fill_then_rename(file, &tmp, path, bytes);
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// A new temporary file in `dir`, opened with `options`: the first free
/// name `.scheda.{pid}.{n}.tmp`, with `n` from `next`. A name is taken only
/// by a file a dead process of the same pid left; the next number is free
/// of it.
fn create_temporary(
    dir: &Path,
    options: &OpenOptions,
    next: &AtomicU64,
) -> Result<(PathBuf, fs::File), Error> {
    let pid = std::process::id();
    loop {
        let n = next.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!("{TMP_PREFIX}{pid}.{n}{TMP_SUFFIX}"));
        match options.open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(io(err)).context("atomic_write: create temporary file"),
        }
    }
}

/// Writes `bytes` into `file`, the temporary file at `tmp`, flushes it to
/// disk, and renames it over `path`.
fn fill_then_rename(file: fs::File, tmp: &Path, path: &Path, bytes: &[u8]) -> Result<(), Error> {
    fill(file, bytes)?;
    #[cfg(test)]
    syncs::record(path);
    rename_replace(tmp, path)
}

/// Flushes the file at `path` to disk.
///
/// # Errors
///
/// `STORAGE` when it cannot be opened or flushed.
#[cfg(feature = "index")]
pub(crate) fn sync_file(path: &Path) -> Result<(), Error> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(io)
        .with_context(|| format!("flush {} to disk", path.display()))?;
    #[cfg(test)]
    syncs::record(path);
    Ok(())
}

/// Flushes the folder at `path` (the names in it) to disk. A system that
/// cannot flush a folder keeps it to the file system, as git does there.
///
/// # Errors
///
/// `STORAGE` when it cannot be opened or flushed.
#[cfg(feature = "index")]
pub(crate) fn sync_dir(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|dir| dir.sync_all())
            .map_err(io)
            .with_context(|| format!("flush {} to disk", path.display()))?;
        #[cfg(test)]
        syncs::record(path);
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// The files and folders the tests saw flushed, in order.
#[cfg(test)]
pub(crate) mod syncs {
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, PoisonError};

    static LOG: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    pub(super) fn record(path: &Path) {
        LOG.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(path.to_path_buf());
    }

    /// How many flushes there are so far.
    #[cfg(feature = "index")]
    pub(crate) fn mark() -> usize {
        LOG.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// The paths under `root` flushed after `mark`, in order.
    #[cfg(feature = "index")]
    pub(crate) fn since(mark: usize, root: &Path) -> Vec<PathBuf> {
        LOG.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .skip(mark)
            .filter(|path| path.starts_with(root))
            .cloned()
            .collect()
    }
}

/// Writes `bytes` into the temporary `file`, flushes it to disk, and closes
/// it.
fn fill(mut file: fs::File, bytes: &[u8]) -> Result<(), Error> {
    file.write_all(bytes)
        .map_err(io)
        .context("atomic_write: write temporary file")?;
    file.sync_all()
        .map_err(io)
        .context("atomic_write: flush temporary file")
}

/// `fs::rename` cannot replace an existing file on Windows.
fn rename_replace(tmp: &Path, path: &Path) -> Result<(), Error> {
    #[cfg(windows)]
    {
        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        move_file(tmp, path, MOVEFILE_REPLACE_EXISTING)
            .map_err(io)
            .context("atomic_write: replace file")
    }
    #[cfg(not(windows))]
    {
        fs::rename(tmp, path)
            .map_err(io)
            .context("atomic_write: rename temporary file")
    }
}

/// Renames `tmp` to `path` unless a file is there, which the one system call
/// checks as it renames: `false` then, and `tmp` stays.
fn rename_no_replace(tmp: &Path, path: &Path) -> Result<bool, Error> {
    match rename_exclusive(tmp, path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(err) => Err(io(err)).context("atomic_write: rename temporary file"),
    }
}

/// `renameat2` with `RENAME_NOREPLACE`.
#[cfg(target_os = "linux")]
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::{CString, c_char, c_int, c_uint};
    use std::os::unix::ffi::OsStrExt;

    const AT_FDCWD: c_int = -100;
    const RENAME_NOREPLACE: c_uint = 1;

    unsafe extern "C" {
        fn renameat2(
            old_dir: c_int,
            old: *const c_char,
            new_dir: c_int,
            new: *const c_char,
            flags: c_uint,
        ) -> c_int;
    }

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both are NUL-terminated paths that live across the call.
    let made = unsafe {
        renameat2(
            AT_FDCWD,
            from.as_ptr(),
            AT_FDCWD,
            to.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if made == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// `renamex_np` with `RENAME_EXCL`.
#[cfg(target_os = "macos")]
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::{CString, c_char, c_int, c_uint};
    use std::os::unix::ffi::OsStrExt;

    const RENAME_EXCL: c_uint = 0x0000_0004;

    unsafe extern "C" {
        fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
    }

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both are NUL-terminated paths that live across the call.
    if unsafe { renamex_np(from.as_ptr(), to.as_ptr(), RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`.
#[cfg(windows)]
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    move_file(from, to, 0)
}

/// A Unix with neither call: a hard link, which no file at `to` survives
/// either, then the old name goes.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::hard_link(from, to)?;
    fs::remove_file(from)
}

/// `MoveFileExW` from `from` to `to` with `flags`.
#[cfg(windows)]
fn move_file(from: &Path, to: &Path, flags: u32) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let from = wide(from);
    let to = wide(to);
    // SAFETY: both buffers are NUL-terminated UTF-16 from `encode_wide`.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// `fs::canonicalize`, in the form a person writes on Windows: `C:\a\b` or
/// `\\server\share\a`, not the verbatim `\\?\C:\a\b` the system gives. A
/// path that has no other form stays verbatim: one of `MAX_PATH` characters
/// or more, or one with a name Windows reads otherwise without the prefix
/// (a device such as `CON`, a name that ends with a dot or a space).
/// Elsewhere it is `fs::canonicalize`.
pub(crate) fn canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    #[cfg(windows)]
    let canonical = plain_windows_path(&canonical).unwrap_or(canonical);
    Ok(canonical)
}

/// The plain form of the verbatim Windows path `path`, when it has one.
#[cfg(windows)]
fn plain_windows_path(path: &Path) -> Option<PathBuf> {
    use std::path::Prefix;

    /// The characters of a path, its closing NUL counted.
    const MAX_PATH: usize = 260;

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return None;
    };
    let mut plain = match prefix.kind() {
        Prefix::VerbatimDisk(letter) => format!("{}:", char::from(letter)),
        Prefix::VerbatimUNC(server, share) => {
            format!(r"\\{}\{}", server.to_str()?, share.to_str()?)
        }
        _ => return None,
    };
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                let name = name.to_str()?;
                if name.ends_with(['.', ' ']) || crate::id::is_windows_device(name) {
                    return None;
                }
                plain.push('\\');
                plain.push_str(name);
            }
            _ => return None,
        }
    }
    // The root of a drive is `C:\`; `C:` alone is its current folder.
    if plain.ends_with(':') {
        plain.push('\\');
    }
    (plain.encode_utf16().count() < MAX_PATH).then(|| PathBuf::from(plain))
}

/// `path` relative to `vault`, in POSIX form (`a/b/c`). A path outside the
/// Vault, or one that steps out with `..`, is `INVALID_ARGUMENT`.
pub(crate) fn rel_of(vault: &Path, path: &Path) -> Result<String, Fault> {
    let outside = || {
        Fault::new(
            VaultErrorCode::InvalidArgument,
            "the path is outside the Vault",
        )
    };
    let rel = path.strip_prefix(vault).map_err(|_| outside())?;
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
            _ => return Err(outside()),
        }
    }
    Ok(out)
}

/// Fails when `path`, or any component of it below `vault`, is a symlink
/// (`INVALID_ARGUMENT`, with the Vault-relative path). The walk checks each
/// component with `symlink_metadata`, from the first below the Vault root to
/// the last, and stops at one that does not exist: nothing below it can be a
/// link. The Vault root itself may be reached through a link, and a `path`
/// outside the Vault passes.
pub(crate) fn reject_symlinks(vault: &Path, path: &Path) -> Result<(), Fault> {
    match symlink_on_path(vault, path)? {
        Some(rel) => Err(symlink_fault(rel)),
        None => Ok(()),
    }
}

/// The Vault-relative path of the first symlink among the components of
/// `path` below `vault`, or `None`. Walks as [`reject_symlinks`] does.
fn symlink_on_path(vault: &Path, path: &Path) -> Result<Option<String>, Fault> {
    let Ok(rest) = path.strip_prefix(vault) else {
        return Ok(None);
    };
    let mut cur = vault.to_path_buf();
    for component in rest.components() {
        cur.push(component);
        match fs::symlink_metadata(&cur) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Ok(Some(rel_of(vault, &cur)?));
            }
            Ok(_) => {}
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(err) => {
                return Err(io(err).with_path(rel_of(vault, &cur)?));
            }
        }
    }
    Ok(None)
}

fn symlink_fault(rel: String) -> Fault {
    Fault::new(
        VaultErrorCode::InvalidArgument,
        format!("{rel} is a symlink"),
    )
    .with_path(rel)
}

/// Owner-only file mode (`0600` on Unix). Other systems leave ACLs unchanged.
pub(crate) fn restrict_file(path: &Path) -> Result<(), Error> {
    set_mode(path, 0o600)
}

/// Owner-only directory mode (`0700` on Unix). Other systems leave ACLs
/// unchanged.
pub(crate) fn restrict_dir(path: &Path) -> Result<(), Error> {
    set_mode(path, 0o700)
}

/// Narrows every file and folder at and below `root` to the owner, as
/// [`restrict_file`] and [`restrict_dir`] do: what a library wrote there
/// with the process umask (Lance's datasets). A symlink is left as it is,
/// and a path that is gone is nothing to do.
///
/// # Errors
///
/// `STORAGE` when a mode cannot be read or set.
#[cfg(feature = "index")]
pub(crate) fn restrict_tree(root: &Path) -> Result<(), Error> {
    let meta = match fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(io(err)).context("look at a file of the Index"),
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        restrict_dir(root)?;
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(io(err)).context("read a folder of the Index"),
        };
        for entry in entries {
            let entry = entry.map_err(io).context("read a folder of the Index")?;
            restrict_tree(&entry.path())?;
        }
        return Ok(());
    }
    if meta.is_file() {
        restrict_file(root)?;
    }
    Ok(())
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

/// `{vault}/.scheda/` must be owner-only on Unix (`0700` directories, `0600`
/// files). Every path with a wider mode is a `FAILED_PRECONDITION` problem
/// with the Vault-relative path. Other systems do not inspect ACLs.
pub(crate) fn check_scheda_owner_only(vault: &Path) -> Result<Vec<Problem>, Error> {
    #[cfg(unix)]
    {
        let mut problems = Vec::new();
        let scheda = vault.join(".scheda");
        if scheda.exists() {
            // A round of the indexer writes the generations of the Index
            // with the process umask and narrows their modes when it ends,
            // so they are looked at only while no round runs; holding the
            // lock keeps one from beginning meanwhile.
            let index = crate::index::Index::new(vault);
            let round = index.try_lock()?;
            let skip_generations = round.is_none();
            check_owner_only_tree(vault, &scheda, skip_generations, &mut problems)?;
            drop(round);
        }
        Ok(problems)
    }
    #[cfg(not(unix))]
    {
        let _ = vault;
        Ok(Vec::new())
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;
        match fs::set_permissions(path, Permissions::from_mode(mode)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(io(err)).context("set_mode: set owner-only mode"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

#[cfg(unix)]
fn check_owner_only_tree(
    vault: &Path,
    path: &Path,
    skip_generations: bool,
    problems: &mut Vec<Problem>,
) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;

    use crate::check::problem;

    if skip_generations
        && path.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("index"))
        && path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|name| crate::index::generation_number(name).is_some())
    {
        return Ok(());
    }
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(io(err)).context("check_scheda_owner_only: read mode"),
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let rel = rel_of(vault, path)?;
    let mode = meta.permissions().mode() & 0o777;
    if meta.is_dir() {
        if mode != 0o700 {
            let message = format!("{rel} mode is {mode:04o}; an owner-only directory is 0700");
            problems.push(problem(
                VaultErrorCode::FailedPrecondition,
                Some(&rel),
                message,
            ));
        }
        let entries = fs::read_dir(path)
            .map_err(io)
            .context("check_scheda_owner_only: read directory")?;
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(io)
                .context("check_scheda_owner_only: read directory entry")?;
            paths.push(entry.path());
        }
        paths.sort();
        for child in paths {
            check_owner_only_tree(vault, &child, skip_generations, problems)?;
        }
        return Ok(());
    }
    if meta.is_file() && mode != 0o600 {
        let message = format!("{rel} mode is {mode:04o}; an owner-only file is 0600");
        problems.push(problem(
            VaultErrorCode::FailedPrecondition,
            Some(&rel),
            message,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use scheda_api::v1::VaultErrorCode;

    use super::{Flock, atomic_write, create_private, create_temporary, rel_of, remove_temporary};
    #[cfg(unix)]
    use super::{
        check_scheda_owner_only, reject_symlinks, restrict_dir, restrict_file, restrict_file_below,
    };
    use crate::error::Error;

    /// A scratch directory that is removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-fs-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            crate::testing::remove_when_the_test_ends(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    /// A copy of the descriptor, as a child another thread forked holds
    /// until it runs `exec`, does not keep a dropped [`Flock`] locked.
    #[cfg(unix)]
    #[test]
    fn a_private_write_makes_its_folders_and_its_file_owner_only_from_the_start() {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let root = base.join(format!("scheda-fs-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let file = root.join("a/b/file");
        super::atomic_write_private(&file, b"x").unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&root.join("a/b")), 0o700);
        assert_eq!(mode(&root.join("a")), 0o700);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_dropped_flock_is_free_while_a_copy_of_its_descriptor_lives() {
        let scratch = Scratch::new("flock");
        let path = scratch.0.join("held.lock");
        let open = || {
            std::fs::File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .unwrap()
        };
        let file = open();
        file.lock().unwrap();
        let copy = file.try_clone().unwrap();
        drop(Flock::held(file));
        assert!(open().try_lock().is_ok());
        drop(copy);
    }

    #[test]
    fn atomic_write_creates_parents_and_replaces_without_leftovers() {
        let scratch = Scratch::new("atomic");
        let path = scratch.0.join("a").join("b.txt");
        atomic_write(&path, b"one", false).unwrap();
        atomic_write(&path, b"two", false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("b.txt")]);
    }

    #[test]
    fn a_temporary_name_a_dead_process_left_is_passed_over_and_kept() {
        let scratch = Scratch::new("tmp-taken");
        let pid = std::process::id();
        for n in 0..3 {
            std::fs::write(scratch.0.join(format!(".scheda.{pid}.{n}.tmp")), b"left").unwrap();
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let next = std::sync::atomic::AtomicU64::new(0);
        let (tmp, file) = create_temporary(&scratch.0, &options, &next).unwrap();
        drop(file);
        assert_eq!(
            tmp.file_name().unwrap().to_str().unwrap(),
            format!(".scheda.{pid}.3.tmp")
        );
        for n in 0..3 {
            let left = scratch.0.join(format!(".scheda.{pid}.{n}.tmp"));
            assert_eq!(std::fs::read(left).unwrap(), b"left");
        }
    }

    #[test]
    fn remove_temporary_removes_the_temporary_files_only() {
        let scratch = Scratch::new("tmp-sweep");
        for name in [".scheda.12.3.tmp", ".scheda.7.tmp"] {
            std::fs::write(scratch.0.join(name), b"left").unwrap();
        }
        for name in [".scheda.yaml", "notes.tmp", "commit"] {
            std::fs::write(scratch.0.join(name), b"kept").unwrap();
        }
        std::fs::create_dir(scratch.0.join(".scheda.9.tmp.d")).unwrap();
        remove_temporary(&scratch.0).unwrap();
        let mut names: Vec<_> = std::fs::read_dir(&scratch.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [".scheda.9.tmp.d", ".scheda.yaml", "commit", "notes.tmp"]
        );
        remove_temporary(&scratch.0.join("missing")).unwrap();
    }

    #[test]
    fn atomic_write_into_a_file_is_a_storage_failure() {
        let scratch = Scratch::new("atomic-fail");
        let file = scratch.0.join("f");
        std::fs::write(&file, b"x").unwrap();
        let error = atomic_write(&file.join("child"), b"y", false).unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
    }

    #[test]
    fn canonicalize_resolves_the_path_and_writes_no_verbatim_prefix() {
        let scratch = Scratch::new("canonical");
        let got = super::canonicalize(&scratch.0).unwrap();
        assert!(got.is_absolute());
        assert_eq!(
            std::fs::canonicalize(&got).unwrap(),
            std::fs::canonicalize(&scratch.0).unwrap()
        );
        assert!(
            !got.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            got.display()
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_verbatim_windows_path_is_plain_when_it_has_a_plain_form() {
        let plain = |path: &str| {
            super::plain_windows_path(Path::new(path)).map(|path| path.display().to_string())
        };
        assert_eq!(plain(r"\\?\C:\a\b").as_deref(), Some(r"C:\a\b"));
        assert_eq!(plain(r"\\?\C:\").as_deref(), Some(r"C:\"));
        assert_eq!(
            plain(r"\\?\UNC\server\share\a").as_deref(),
            Some(r"\\server\share\a")
        );
        for verbatim_only in [
            r"\\?\C:\a\CON",
            r"\\?\C:\a\nul.txt",
            r"\\?\C:\a\b.",
            r"\\?\C:\a\b ",
        ] {
            assert_eq!(plain(verbatim_only), None, "{verbatim_only}");
        }
        assert_eq!(plain(&format!(r"\\?\C:\{}", "a".repeat(300))), None);
        assert_eq!(plain(r"C:\a"), None, "not verbatim");
    }

    #[test]
    fn create_private_makes_a_file_once_and_keeps_the_first() {
        let scratch = Scratch::new("create");
        let file = scratch.0.join("dir/f");
        assert!(create_private(&file, b"one").unwrap());
        assert!(!create_private(&file, b"two").unwrap());
        assert_eq!(std::fs::read(&file).unwrap(), b"one");
        let names: Vec<_> = std::fs::read_dir(scratch.0.join("dir"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["f"], "no temporary file is left");
    }

    #[test]
    fn rel_of_joins_with_slashes_and_refuses_outside_paths() {
        let vault = Path::new("/v");
        assert_eq!(rel_of(vault, Path::new("/v/a/b")).unwrap(), "a/b");
        assert_eq!(rel_of(vault, Path::new("/v")).unwrap(), "");
        let error = Error::from(rel_of(vault, Path::new("/w/a")).unwrap_err());
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let error = Error::from(rel_of(vault, Path::new("/v/../w")).unwrap_err());
        assert!(error.is(VaultErrorCode::InvalidArgument));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_refused_with_their_path() {
        let scratch = Scratch::new("symlink");
        let vault = &scratch.0;
        std::fs::create_dir_all(vault.join("c/sub")).unwrap();
        std::fs::write(vault.join("outside.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(vault.join("outside.txt"), vault.join("c/sub/link")).unwrap();
        let fault = reject_symlinks(vault, &vault.join("c/sub/link")).unwrap_err();
        assert_eq!(fault.path(), Some("c/sub/link"));
        assert!(crate::error::Error::from(fault).is(VaultErrorCode::InvalidArgument));
        reject_symlinks(vault, &vault.join("c/sub")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn owner_only_modes_are_applied_and_checked() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("modes");
        let vault = &scratch.0;
        let scheda = vault.join(".scheda");
        std::fs::create_dir_all(scheda.join("deep")).unwrap();
        let file = scheda.join("deep/f");
        std::fs::write(&file, b"x").unwrap();
        let wide = std::fs::metadata(&scheda).unwrap().permissions().mode() & 0o777 != 0o700;
        assert_eq!(!check_scheda_owner_only(vault).unwrap().is_empty(), wide);
        restrict_dir(&scheda).unwrap();
        restrict_dir(&scheda.join("deep")).unwrap();
        restrict_file(&file).unwrap();
        assert!(check_scheda_owner_only(vault).unwrap().is_empty());
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&scheda), 0o700);
        assert_eq!(mode(&file), 0o600);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let problems = check_scheda_owner_only(vault).unwrap();
        assert_eq!(problems.len(), 1);
        assert_eq!(
            problems[0].code.and_then(|code| code.as_known()),
            Some(VaultErrorCode::FailedPrecondition)
        );
        assert_eq!(problems[0].path.as_deref(), Some(".scheda/deep/f"));
        restrict_file(&file).unwrap();
        restrict_dir(&scheda.join("deep")).unwrap();
        restrict_file_below(vault, &file).unwrap();
        assert!(check_scheda_owner_only(vault).unwrap().is_empty());
    }
}
