//! Packing of the loose objects of a repository, in process.
//!
//! A write stores its blobs, trees, and commit as loose objects, one file
//! each, and the git library never packs them (the `git` commands Scheda
//! runs carry `gc.auto=0`). An append to a journal stores the whole open
//! segment as a new blob, so the loose objects of a long journal add up to
//! the square of its segment. Packing stores each version of a blob as a
//! delta against a neighbour, which is small for an append.
//!
//! The write lock is held, a write has just been stored, and the library's
//! pack builder is used, so a local write still starts no process.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use git2::{ObjectType, Oid, Repository};
use scheda_api::v1::VaultErrorCode;

use crate::error::{Fault, io, lib_fault};

/// When a repository is packed, and when its packs are merged.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// Pack when the loose objects are estimated to number more than this.
    pub(crate) loose: usize,
    /// Pack when the loose objects take more than this many bytes on disk
    /// (counted only after a write that stored a large blob).
    pub(crate) bytes: u64,
    /// Merge packs when there are this many or more.
    pub(crate) packs: usize,
    /// How many packs, the smallest, one merge takes.
    pub(crate) width: usize,
}

impl Limits {
    /// The limits a write uses; the measurements are in the CHANGELOG.
    ///
    /// `loose` bounds the number of files in the object directories: a
    /// directory with tens of thousands of entries slows every lookup of a
    /// loose object, and the person's own `git` lists them. 1024 loose
    /// objects are about 250 appends of a few objects each.
    ///
    /// `bytes` bounds the pause of one packing and the space the loose
    /// objects take meanwhile. An append to a segment near its cap stores
    /// 1 MiB or so, so the count alone would let 250 of them pile up. A
    /// packing costs what it reads: 8 MiB of loose objects took 0.3 s
    /// (journal of 200-byte lines) to 0.4 s (4000-byte lines) in the
    /// probe.
    ///
    /// `packs` bounds the packs a lookup that misses has to search, and the
    /// full copies of a segment: each pack starts a delta chain of its own.
    /// git's own `gc.autoPackLimit` is 50.
    ///
    /// `width` bounds the pause of a merge, which reads everything in the
    /// packs it takes: all 16 packs at once took 7.3 s in the probe. Four
    /// of the smallest cost about four packings, and merging the smallest
    /// first makes the sizes grow by a factor each time, so the work per
    /// byte stays logarithmic in the history.
    pub(crate) const DEFAULT: Limits = Limits {
        loose: 1024,
        bytes: 8 * 1024 * 1024,
        packs: 16,
        width: 4,
    };
}

/// How many of the 256 object directories are looked at to estimate the
/// number of loose objects. The names of objects are hashes, so the
/// directories fill evenly; git's `gc.auto` looks at one (`17`) and
/// multiplies by 256. 16 directories give an estimate within about a tenth
/// at 1024 objects and cost 16 small directory reads on a write, where
/// reading all 256 added a millisecond to a write of 1.6 milliseconds
/// (measured).
const SAMPLE: u32 = 16;

/// A write that stores this many bytes of blobs, `Limits::bytes` over 128,
/// counts the loose objects exactly, all 256 directories with their sizes.
/// The bytes cannot be estimated from a sample: a few large blobs fall into
/// it by chance (1 MiB appended to a segment 60 times, a sample of 16
/// directories held 4 of them where 3.75 were expected, and 29 MB of loose
/// objects passed unseen against a limit of 8 MiB). A write this large
/// takes ten milliseconds or more to hash, compress, and flush, so one
/// more millisecond is a tenth of it; a smaller write adds too little to
/// the bytes for the count of its objects not to catch up first.
const EXACT_DIVISOR: u64 = 128;

/// The packs of an object directory are not merged while a multi-pack
/// index names them: the library trusts the index and would not look for a
/// deleted pack's objects elsewhere. Scheda never writes one; a person's
/// `git multi-pack-index write` does.
const MIDX: &str = "multi-pack-index";

/// Packs the loose objects of `git` when `limits` say they are too many,
/// and with them the smallest packs when there are too many of those. The
/// caller holds the write lock.
///
/// `stored` is the bytes of blobs the write that just ended stored.
///
/// Returns whether a pack was written.
///
/// A pack is complete on disk (the library flushes its pack, its index, and
/// the folder) before any file it replaces is removed, and what is removed
/// is only what the pack holds, so a reader (this process, another one, the
/// person's `git`) finds every object at every moment: one that misses a
/// file looks in the packs again.
///
/// # Errors
///
/// `STORAGE` or `REPLICATION` when a file or the library fails. Nothing
/// that was readable becomes unreadable: a failure leaves loose objects
/// that are also packed, or packs that are also merged.
pub(crate) fn pack_if_due(git: &Repository, limits: &Limits, stored: u64) -> Result<bool, Fault> {
    let objects = git.commondir().join("objects");
    if !due(&objects, limits, stored)? {
        return Ok(false);
    }
    let loose = loose_objects(&objects)?;
    let pack_dir = objects.join("pack");
    let mut old = packs(&pack_dir)?;
    let mut merging: Vec<String> = Vec::new();
    if old.len() >= limits.packs && !pack_dir.join(MIDX).exists() {
        old.sort_by_key(|(_, size)| *size);
        merging = old
            .into_iter()
            .take(limits.width)
            .map(|(stem, _)| stem)
            .collect();
    }
    let mut members: Vec<Oid> = loose.iter().map(|(id, _)| *id).collect();
    for stem in &merging {
        members.extend(index_ids(&pack_dir.join(format!("{stem}.idx")))?);
    }
    let set: HashSet<Oid> = members.iter().copied().collect();
    if set.is_empty() {
        return Ok(false);
    }
    let names = names_of(git, &set)?;
    let mut builder = git.packbuilder().map_err(lib_fault)?;
    for id in &set {
        let name = names.get(id).map(String::as_str);
        builder.insert_object(*id, name).map_err(lib_fault)?;
    }
    builder.write(&pack_dir, 0).map_err(lib_fault)?;
    let fresh = builder
        .name()
        .ok()
        .flatten()
        .map(|hash| format!("pack-{hash}"));
    // The pack is complete. Only now do the files it replaces go.
    let mut first: Option<Fault> = None;
    for (_, path) in &loose {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                first.get_or_insert(io(err));
            }
        }
    }
    for stem in &merging {
        if fresh.as_deref() == Some(stem.as_str()) {
            continue;
        }
        // A pack that cannot be removed (Windows keeps a file another
        // reader has open) stays: its objects are in the new pack too, and
        // a later merge takes it again.
        let _ = remove_pack(&pack_dir, stem);
    }
    match first {
        Some(fault) => Err(fault),
        None => Ok(true),
    }
}

/// Whether the loose objects of `objects` are past `limits`: their number
/// estimated from a sample of the directories, their bytes counted exactly
/// when the write that just ended stored `stored` bytes, enough to matter.
fn due(objects: &Path, limits: &Limits, stored: u64) -> Result<bool, Fault> {
    let exact = stored >= limits.bytes / EXACT_DIVISOR;
    let dirs = if exact { 256 } else { SAMPLE };
    let (mut count, mut bytes) = (0usize, 0u64);
    for dir in 0..dirs {
        let Some(entries) = read_dir_if_there(&objects.join(format!("{dir:02x}")))? else {
            continue;
        };
        for entry in entries {
            let entry = entry.map_err(io)?;
            if !is_object_name(&entry.file_name()) {
                continue;
            }
            count += 1;
            if exact {
                bytes += entry.metadata().map_err(io)?.len();
            }
        }
    }
    let loose = count.saturating_mul((256 / dirs) as usize);
    Ok(loose > limits.loose || bytes > limits.bytes)
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

/// The packs in `pack_dir` that a merge may take, each as its name
/// (`pack-{hash}`) and the size of its pack file: not those a `.keep` file
/// protects.
fn packs(pack_dir: &Path) -> Result<Vec<(String, u64)>, Fault> {
    let Some(entries) = read_dir_if_there(pack_dir)? else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    let mut kept = HashSet::new();
    for entry in entries {
        let entry = entry.map_err(io)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(stem) = name.strip_suffix(".pack") {
            found.push((stem.to_owned(), entry.metadata().map_err(io)?.len()));
        } else if let Some(stem) = name.strip_suffix(".keep") {
            kept.insert(stem.to_owned());
        }
    }
    found.retain(|(stem, _)| !kept.contains(stem));
    Ok(found)
}

/// The ids of the objects a pack holds, read from its index (version 2: a
/// header of 8 bytes, a fan-out table of 256 counters, then the ids, 20
/// bytes each, in order). The count is the last counter.
fn index_ids(idx: &Path) -> Result<Vec<Oid>, Fault> {
    const HEADER: usize = 8;
    const FANOUT: usize = 256 * 4;
    let bytes = fs::read(idx).map_err(io)?;
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
    let table = bytes
        .get(HEADER + FANOUT..)
        .and_then(|rest| rest.get(..count.checked_mul(20)?))
        .ok_or_else(bad)?;
    table
        .as_chunks::<20>()
        .0
        .iter()
        .map(|id| Oid::from_bytes(id).map_err(lib_fault))
        .collect()
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

/// A name for each loose blob and tree that a loose commit changed: the
/// last part of its path. The library puts objects of one name next to
/// each other when it looks for deltas, which is what puts the versions of
/// a segment, or of a folder's tree, together; without a name they are
/// sorted by size among every other object.
fn names_of(git: &Repository, loose: &HashSet<Oid>) -> Result<HashMap<Oid, String>, Fault> {
    let odb = git.odb().map_err(lib_fault)?;
    let mut names = HashMap::new();
    for id in loose {
        let (_, kind) = odb.read_header(*id).map_err(lib_fault)?;
        if kind != ObjectType::Commit {
            continue;
        }
        let commit = git.find_commit(*id).map_err(lib_fault)?;
        let tree = commit.tree().map_err(lib_fault)?;
        let before = match commit.parent(0) {
            Ok(parent) => Some(parent.tree().map_err(lib_fault)?),
            Err(_) => None,
        };
        let diff = git
            .diff_tree_to_tree(before.as_ref(), Some(&tree), None)
            .map_err(lib_fault)?;
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        for delta in diff.deltas() {
            if delta.status() == git2::Delta::Deleted {
                continue;
            }
            let file = delta.new_file();
            let Some(path) = file.path() else { continue };
            if loose.contains(&file.id())
                && let Some(base) = path.file_name().and_then(|name| name.to_str())
            {
                names.entry(file.id()).or_insert_with(|| base.to_owned());
            }
            let mut dir = path.parent();
            while let Some(parent) = dir {
                if parent.as_os_str().is_empty() {
                    break;
                }
                dirs.insert(parent.to_path_buf());
                dir = parent.parent();
            }
        }
        for dir in dirs {
            let Ok(entry) = tree.get_path(&dir) else {
                continue;
            };
            if loose.contains(&entry.id())
                && let Some(base) = dir.file_name().and_then(|name| name.to_str())
            {
                names.entry(entry.id()).or_insert_with(|| base.to_owned());
            }
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use git2::{Oid, Repository, Signature};

    use super::{Limits, loose_objects, pack_if_due, packs};

    /// A repository in a scratch folder, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-pack-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            crate::testing::remove_when_the_test_ends(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    /// `len` bytes that do not compress.
    fn noise(seed: u64, len: usize) -> Vec<u8> {
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

    /// A commit on `HEAD` holding `log/segment` with `bytes`.
    fn commit(repo: &Repository, bytes: &[u8]) -> Oid {
        let blob = repo.blob(bytes).unwrap();
        let mut folder = repo.treebuilder(None).unwrap();
        folder.insert("segment", blob, 0o100_644).unwrap();
        let folder = folder.write().unwrap();
        let mut root = repo.treebuilder(None).unwrap();
        root.insert("log", folder, 0o040_000).unwrap();
        let tree = repo.find_tree(root.write().unwrap()).unwrap();
        let who = Signature::now("Test", "test@example.test").unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        repo.commit(Some("HEAD"), &who, &who, "append", &tree, &parents)
            .unwrap()
    }

    /// The bytes of the files below `dir`.
    fn size_of(dir: &std::path::Path) -> u64 {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let meta = entry.metadata().unwrap();
                if meta.is_dir() {
                    size_of(&entry.path())
                } else {
                    meta.len()
                }
            })
            .sum()
    }

    /// Appends `n` lines of `line` bytes to one segment, one commit each,
    /// packing as a write does; returns the content of each version.
    fn append(repo: &Repository, limits: &Limits, n: usize, line: usize) -> Vec<Vec<u8>> {
        let mut segment = Vec::new();
        let mut versions = Vec::new();
        for i in 0..n {
            segment.extend(noise(i as u64 + 1, line));
            commit(repo, &segment);
            pack_if_due(repo, limits, line as u64).unwrap();
            versions.push(segment.clone());
        }
        versions
    }

    /// Every commit of the history holds the version of the segment it was
    /// made with, and every object the database lists can be read.
    fn assert_readable(repo: &Repository, versions: &[Vec<u8>]) {
        let mut walk = repo.revwalk().unwrap();
        walk.push_head().unwrap();
        let commits: Vec<Oid> = walk.map(Result::unwrap).collect();
        assert_eq!(commits.len(), versions.len());
        for (id, version) in commits.iter().rev().zip(versions) {
            let tree = repo.find_commit(*id).unwrap().tree().unwrap();
            let entry = tree.get_path(std::path::Path::new("log/segment")).unwrap();
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

    #[test]
    fn many_appends_leave_few_loose_objects_and_a_small_database() {
        let scratch = Scratch::new("appends");
        let repo = Repository::init(&scratch.0).unwrap();
        let limits = Limits {
            loose: 200,
            bytes: u64::MAX,
            packs: 100,
            width: 4,
        };
        let versions = append(&repo, &limits, 400, 200);
        let objects = repo.path().join("objects");
        // Three objects an append (a blob, two trees) and a commit: the
        // 1600 objects of 400 appends are packed whenever the estimate of
        // the loose ones passes 200.
        let loose = loose_objects(&objects).unwrap().len();
        assert!(loose < 400, "{loose} loose objects");
        let packed = packs(&objects.join("pack")).unwrap();
        assert!(!packed.is_empty());
        assert_readable(&repo, &versions);
        // Unpacked, the versions add up to the square of the segment: 400
        // versions of 200 bytes a line, 16 MB. The loose objects left
        // (under 400, about 100 versions) are most of what remains; the
        // packs hold the segment a few times, the rest as deltas of a line.
        let unpacked: u64 = (1..=400u64).map(|i| i * 200).sum();
        let total = size_of(&objects);
        assert!(total < unpacked / 4, "{total} bytes of {unpacked}");
        let in_packs = size_of(&objects.join("pack"));
        let last = versions.last().unwrap().len() as u64;
        assert!(
            in_packs < 8 * last,
            "{in_packs} bytes in packs, segment {last}"
        );
    }

    #[test]
    fn a_merge_takes_the_smallest_packs_and_loses_no_object() {
        let scratch = Scratch::new("merge");
        let repo = Repository::init(&scratch.0).unwrap();
        let limits = Limits {
            loose: 30,
            bytes: u64::MAX,
            packs: 4,
            width: 3,
        };
        let versions = append(&repo, &limits, 300, 100);
        let objects = repo.path().join("objects");
        let packed = packs(&objects.join("pack")).unwrap();
        // The merge runs with the packing that finds four packs, and takes
        // three: the count never passes the limit by more than the one
        // pack that packing adds.
        assert!(packed.len() <= 4, "{} packs", packed.len());
        assert!(packed.len() > 1);
        assert_readable(&repo, &versions);
        // The folder holds only whole packs: an index for each pack file.
        for (stem, _) in &packed {
            assert!(objects.join("pack").join(format!("{stem}.idx")).is_file());
        }
    }

    #[test]
    fn a_pack_a_keep_file_protects_is_not_merged() {
        let scratch = Scratch::new("keep");
        let repo = Repository::init(&scratch.0).unwrap();
        let limits = Limits {
            loose: 30,
            bytes: u64::MAX,
            packs: 2,
            width: 2,
        };
        let mut versions = append(&repo, &limits, 40, 100);
        let objects = repo.path().join("objects");
        let first = packs(&objects.join("pack")).unwrap();
        assert!(!first.is_empty());
        let kept = &first[0].0;
        std::fs::write(objects.join("pack").join(format!("{kept}.keep")), "").unwrap();
        let mut segment = versions.last().unwrap().clone();
        for i in 0..200 {
            segment.extend(noise(1000 + i, 100));
            commit(&repo, &segment);
            versions.push(segment.clone());
            pack_if_due(&repo, &limits, 100).unwrap();
        }
        assert!(objects.join("pack").join(format!("{kept}.pack")).is_file());
        assert_readable(&repo, &versions);
    }
}
