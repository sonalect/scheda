//! File helpers shared by every layer that writes into a Vault: atomic
//! writes, the advisory lock, symlink checks, owner-only modes, and the
//! POSIX form of a Vault-relative path.

// The writers of the next layers call what `open` does not.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the layers that write Documents and take the write lock are not built yet"
    )
)]

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

use scheda_api::v1::{Problem, VaultErrorCode};

use crate::check::problem;
use crate::error::{Context, Error, Fault, io};

static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

/// An `flock` held on an open file. Dropping it unlocks before the file
/// closes: a close alone leaves the lock held while a child process another
/// thread forked still has the descriptor, until that child runs `exec`,
/// and a handle trying the lock in that moment would find it taken.
pub(crate) struct Flock(fs::File);

impl Flock {
    /// Take ownership of `file`, which holds the lock.
    pub(crate) fn held(file: fs::File) -> Self {
        Self(file)
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
/// The temporary file is removed when the write fails.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(io)
            .context("atomic_write: create directory")?;
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
        .map_err(io)
        .context("atomic_write: create temporary file")?;
    file.write_all(bytes)
        .map_err(io)
        .context("atomic_write: write temporary file")?;
    file.sync_all()
        .map_err(io)
        .context("atomic_write: flush temporary file")?;
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
        fs::rename(tmp, path)
            .map_err(io)
            .context("atomic_write: rename temporary file")
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
        Err(io(std::io::Error::last_os_error())).context("atomic_write: replace file")
    } else {
        Ok(())
    }
}

/// `path` relative to `vault`, in POSIX form (`a/b/c`). A path outside the
/// Vault, or one that steps out with `..`, is `INVALID_ARGUMENT`.
pub(crate) fn rel_posix(vault: &Path, path: &Path) -> Result<String, Error> {
    rel_of(vault, path).map_err(Error::from)
}

/// [`rel_posix`] for the code that raises a [`Fault`] itself.
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

/// Every symlink on the way to `dir` or below it, as Vault-relative paths
/// in walk order. A link on the way to `dir` is the only one reported:
/// nothing below it is read. A `dir` that does not exist has none.
pub(crate) fn find_symlinks(vault: &Path, dir: &Path) -> Result<Vec<String>, Error> {
    let mut found = Vec::new();
    if let Some(rel) = symlink_on_path(vault, dir).context("find_symlinks: stat component")? {
        found.push(rel);
        return Ok(found);
    }
    if dir.is_dir() {
        walk_symlinks(vault, dir, &mut found)?;
    }
    Ok(found)
}

fn walk_symlinks(vault: &Path, dir: &Path, found: &mut Vec<String>) -> Result<(), Error> {
    let entries = fs::read_dir(dir)
        .map_err(io)
        .context("find_symlinks: read directory")?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(io)
            .context("find_symlinks: read directory entry")?;
        paths.push(entry.path());
    }
    paths.sort();
    for path in paths {
        let meta = fs::symlink_metadata(&path)
            .map_err(io)
            .context("find_symlinks: stat entry")?;
        if meta.file_type().is_symlink() {
            found.push(rel_posix(vault, &path)?);
        } else if meta.is_dir() {
            walk_symlinks(vault, &path, found)?;
        }
    }
    Ok(())
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

/// Walks `root` and applies owner-only modes to every directory and file.
/// For files other code created under `{vault}/.scheda/`. A missing `root`
/// is a no-op, and symlinks are not followed.
pub(crate) fn restrict_tree(root: &Path) -> Result<(), Error> {
    let meta = match fs::symlink_metadata(root) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(io(err)).context("restrict_tree: stat path");
        }
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
                return Err(io(err)).context("restrict_tree: read directory");
            }
        };
        for entry in entries {
            let entry = entry
                .map_err(io)
                .context("restrict_tree: read directory entry")?;
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
/// files). Every path with a wider mode is a `FAILED_PRECONDITION` problem
/// with the Vault-relative path. Other systems do not inspect ACLs.
pub(crate) fn check_scheda_owner_only(vault: &Path) -> Result<Vec<Problem>, Error> {
    let mut problems = Vec::new();
    #[cfg(unix)]
    {
        let scheda = vault.join(".scheda");
        if scheda.exists() {
            check_owner_only_tree(vault, &scheda, &mut problems)?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = vault;
    }
    Ok(problems)
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
    problems: &mut Vec<Problem>,
) -> Result<(), Error> {
    use std::os::unix::fs::PermissionsExt;
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(io(err)).context("check_scheda_owner_only: read mode"),
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let rel = rel_posix(vault, path)?;
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
            check_owner_only_tree(vault, &child, problems)?;
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

    use super::{
        Flock, atomic_write, check_scheda_owner_only, find_symlinks, reject_symlinks, rel_posix,
        restrict_dir, restrict_file, restrict_file_below, restrict_tree,
    };

    /// A scratch directory that is removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-fs-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A copy of the descriptor, as a child another thread forked holds
    /// until it runs `exec`, does not keep a dropped [`Flock`] locked.
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
        atomic_write(&path, b"one").unwrap();
        atomic_write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("b.txt")]);
    }

    #[test]
    fn atomic_write_into_a_file_is_a_storage_failure() {
        let scratch = Scratch::new("atomic-fail");
        let file = scratch.0.join("f");
        std::fs::write(&file, b"x").unwrap();
        let error = atomic_write(&file.join("child"), b"y").unwrap_err();
        assert!(error.is(VaultErrorCode::Storage));
    }

    #[test]
    fn rel_posix_joins_with_slashes_and_refuses_outside_paths() {
        let vault = Path::new("/v");
        assert_eq!(rel_posix(vault, Path::new("/v/a/b")).unwrap(), "a/b");
        assert_eq!(rel_posix(vault, Path::new("/v")).unwrap(), "");
        let error = rel_posix(vault, Path::new("/w/a")).unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
        let error = rel_posix(vault, Path::new("/v/../w")).unwrap_err();
        assert!(error.is(VaultErrorCode::InvalidArgument));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_refused_with_their_path() {
        let scratch = Scratch::new("symlink");
        let vault = &scratch.0;
        std::fs::create_dir_all(vault.join("c/sub")).unwrap();
        std::fs::write(vault.join("outside.txt"), b"x").unwrap();
        assert!(find_symlinks(vault, &vault.join("c")).unwrap().is_empty());
        assert!(
            find_symlinks(vault, &vault.join("missing"))
                .unwrap()
                .is_empty()
        );
        std::os::unix::fs::symlink(vault.join("outside.txt"), vault.join("c/sub/link")).unwrap();
        std::os::unix::fs::symlink(vault.join("outside.txt"), vault.join("c/other")).unwrap();
        // Every link is reported, not the first only.
        assert_eq!(
            find_symlinks(vault, &vault.join("c")).unwrap(),
            vec!["c/other".to_owned(), "c/sub/link".to_owned()]
        );
        // A link on the way to the directory is the only one reported.
        std::os::unix::fs::symlink(vault.join("c"), vault.join("via")).unwrap();
        assert_eq!(
            find_symlinks(vault, &vault.join("via")).unwrap(),
            vec!["via".to_owned()]
        );
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
        restrict_tree(&scheda).unwrap();
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
        restrict_tree(&vault.join("missing")).unwrap();
    }
}
