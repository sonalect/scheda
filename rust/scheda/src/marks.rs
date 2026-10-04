//! Per-leg commit marks of one Index generation.
//!
//! `<gen>/marks/` holds one file per leg and repository: `lexical`,
//! `vector`, `ids`, and the same three prefixed `isolated-` for the nested
//! repository under `{vault}/.isolated/`. Each file is one commit id and a
//! newline: the commit whose tree that leg reflects. A leg advances its mark
//! only after its own writes are durable, and every mark is written by temp
//! file plus rename, so a crash leaves the previous mark and the next run
//! reapplies the same diff. One file per leg means two processes that hold
//! different roles never rewrite the same file.
//!
//! A missing mark, or a file that does not hold a commit id, means "nothing
//! indexed yet": the leg is filled from the empty tree.

use std::path::{Path, PathBuf};

use crate::Error;
#[cfg(any(test, feature = "find"))]
use crate::error::ContextIo;
use crate::index;
use crate::store;

const MARKS_DIR: &str = "marks";

/// One artifact of a generation that follows `HEAD`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Leg {
    /// Tantivy documents.
    Lexical,
    /// USearch slots.
    Vector,
    /// Journal `id` → segment maps.
    Ids,
}

/// Repository whose `HEAD` a mark follows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Repo {
    /// The Vault work tree.
    Main,
    /// The nested work tree under `{vault}/.isolated/`.
    Isolated,
}

fn file_name(leg: Leg, repo: Repo) -> &'static str {
    match (repo, leg) {
        (Repo::Main, Leg::Lexical) => "lexical",
        (Repo::Main, Leg::Vector) => "vector",
        (Repo::Main, Leg::Ids) => "ids",
        (Repo::Isolated, Leg::Lexical) => "isolated-lexical",
        (Repo::Isolated, Leg::Vector) => "isolated-vector",
        (Repo::Isolated, Leg::Ids) => "isolated-ids",
    }
}

fn mark_path(generation: &Path, leg: Leg, repo: Repo) -> PathBuf {
    generation.join(MARKS_DIR).join(file_name(leg, repo))
}

/// Whether `text` is a full lowercase SHA-1 or SHA-256 commit id.
fn is_commit_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64)
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Commit `leg` of `repo` has reached in `generation`.
///
/// `None` when the file is absent or does not hold a commit id: that leg
/// starts from the empty tree. Other read failures are errors.
#[cfg(any(test, feature = "find"))]
pub(crate) fn read(generation: &Path, leg: Leg, repo: Repo) -> Result<Option<String>, Error> {
    match std::fs::read(mark_path(generation, leg, repo)) {
        Ok(bytes) => Ok(std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| text.strip_suffix('\n'))
            .filter(|oid| is_commit_id(oid))
            .map(str::to_owned)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).context("[Marks::read] read mark"),
    }
}

/// Record that `leg` of `repo` in `generation` reflects commit `oid`.
///
/// Temp file, `fsync`, rename: a crash leaves the previous mark. `oid` that
/// is not a full lowercase commit id is `invalid` and writes nothing.
pub(crate) fn write(generation: &Path, leg: Leg, repo: Repo, oid: &str) -> Result<(), Error> {
    if !is_commit_id(oid) {
        return Err(Error::invalid(format!("not a commit id: {oid:?}"))
            .context("[Marks::write] check commit id"));
    }
    store::atomic_write(
        &mark_path(generation, leg, repo),
        format!("{oid}\n").as_bytes(),
    )?;
    index::restrict_index_tree(generation)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::Code;

    const A: &str = "0123456789abcdef0123456789abcdef01234567";
    const B: &str = "89abcdef0123456789abcdef0123456789abcdef";

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        /// `<root>/.scheda/index/1`, the shape `restrict_index_tree` expects.
        fn new() -> Self {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("scheda-marks-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let generation = root.join(".scheda").join("index").join("1");
            fs::create_dir_all(&generation).unwrap();
            Self(root)
        }

        fn generation(&self) -> PathBuf {
            self.0.join(".scheda").join("index").join("1")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_mark_is_none() {
        let scratch = Scratch::new();
        assert_eq!(
            read(&scratch.generation(), Leg::Lexical, Repo::Main).unwrap(),
            None
        );
    }

    #[test]
    fn mark_round_trips_per_leg_and_repo() {
        let scratch = Scratch::new();
        let generation = scratch.generation();
        write(&generation, Leg::Lexical, Repo::Main, A).unwrap();
        write(&generation, Leg::Vector, Repo::Isolated, B).unwrap();
        assert_eq!(
            read(&generation, Leg::Lexical, Repo::Main)
                .unwrap()
                .as_deref(),
            Some(A)
        );
        assert_eq!(
            read(&generation, Leg::Vector, Repo::Isolated)
                .unwrap()
                .as_deref(),
            Some(B)
        );
        assert_eq!(read(&generation, Leg::Vector, Repo::Main).unwrap(), None);
        assert_eq!(read(&generation, Leg::Ids, Repo::Main).unwrap(), None);
        assert_eq!(
            fs::read_to_string(generation.join("marks").join("isolated-vector")).unwrap(),
            format!("{B}\n")
        );
    }

    #[test]
    fn sha256_commit_id_is_a_mark() {
        let scratch = Scratch::new();
        let oid = "a".repeat(64);
        write(&scratch.generation(), Leg::Ids, Repo::Main, &oid).unwrap();
        assert_eq!(
            read(&scratch.generation(), Leg::Ids, Repo::Main).unwrap(),
            Some(oid)
        );
    }

    #[test]
    fn crash_before_rename_keeps_old_mark() {
        let scratch = Scratch::new();
        let generation = scratch.generation();
        write(&generation, Leg::Lexical, Repo::Main, A).unwrap();
        // A temp file written but never renamed: what a crash between the
        // `fsync` and the rename leaves behind.
        fs::write(
            generation.join("marks").join(".scheda.99.tmp"),
            format!("{B}\n"),
        )
        .unwrap();
        assert_eq!(
            read(&generation, Leg::Lexical, Repo::Main)
                .unwrap()
                .as_deref(),
            Some(A)
        );
        write(&generation, Leg::Lexical, Repo::Main, B).unwrap();
        assert_eq!(
            read(&generation, Leg::Lexical, Repo::Main)
                .unwrap()
                .as_deref(),
            Some(B)
        );
    }

    #[test]
    fn garbage_mark_reads_as_absent() {
        let scratch = Scratch::new();
        let generation = scratch.generation();
        let dir = generation.join("marks");
        fs::create_dir_all(&dir).unwrap();
        for body in [
            "".as_bytes(),
            b"HEAD\n",
            A.as_bytes(),
            format!("{}\n", A.to_uppercase()).as_bytes(),
            format!("{A} 7\n").as_bytes(),
            b"\xff\xfe\n",
        ] {
            fs::write(dir.join("lexical"), body).unwrap();
            assert_eq!(
                read(&generation, Leg::Lexical, Repo::Main).unwrap(),
                None,
                "{body:?}"
            );
        }
    }

    #[test]
    fn non_commit_id_is_invalid_and_writes_nothing() {
        let scratch = Scratch::new();
        let generation = scratch.generation();
        let err = write(&generation, Leg::Lexical, Repo::Main, "../../etc").unwrap_err();
        assert_eq!(err.known_code(), Some(Code::Invalid));
        assert!(!generation.join("marks").exists());
    }

    #[cfg(unix)]
    #[test]
    fn marks_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = Scratch::new();
        let generation = scratch.generation();
        write(&generation, Leg::Ids, Repo::Main, A).unwrap();
        let dir = fs::metadata(generation.join("marks")).unwrap();
        assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        let file = fs::metadata(generation.join("marks").join("ids")).unwrap();
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
    }
}
