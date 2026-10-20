//! The Index on disk, and the round that moves it.
//!
//! The Index lives in `{vault}/.scheda/index/` and is never committed. It
//! has one state for all its parts: the `commit` file names the commit of
//! `HEAD` the Index reflects, in the main repository and, when there is one,
//! in the nested repository under `.isolated/`. A round makes the
//! difference between that state and `HEAD` and gives it to every
//! [`Side`], then writes `commit` last, so a file of the Index that
//! `commit` does not cover is never believed: a round that dies halfway
//! leaves the old `commit`, and the next round does the work again.
//!
//! ```text
//! .scheda/index.lock        flock, held for one round
//! .scheda/index/format      the number of the layout of these files
//! .scheda/index/commit      the state the journal id maps reflect, written last
//! .scheda/index/journal/    the id maps of the journals
//! .scheda/index/current     the live generation of the Lance tables (`index`)
//! .scheda/index/g-…/        one generation of the Lance tables (`index`)
//! ```
//!
//! Every file is written whole (a temporary file, then a rename) and is
//! owner-only. Only the indexer writes under `index/`, under the lock; a
//! file of another layout is never read, the Index is made again. What an
//! earlier layout wrote beside these files (`lexical/`, `vector/`,
//! `columns`, `stale`) is not read either, and stays until the Index is
//! made again.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use git2::Oid;

use crate::collection::{self, Declarations};
use crate::error::{Context, Error, io};
use crate::fs::{
    Flock, atomic_write_private, create_dir_private, remove_temporary, restrict_dir,
    restrict_file_below,
};
use crate::isolated;
use crate::repo::{Delta, Repo, Snap, Views};

/// The folders directly in `dir` (none when it is missing); a link is not
/// one.
fn folders_in(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(io(err)).context("read a folder of the Index"),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io).context("read a folder of the Index")?;
        if entry
            .file_type()
            .map_err(io)
            .context("read a folder of the Index")?
            .is_dir()
        {
            out.push(entry.path());
        }
    }
    Ok(out)
}

/// The number of the layout of the files of the Index. An Index with another
/// number (the Tantivy and USearch Index of `0.22` is 2) is removed whole
/// and made again, not migrated.
pub(crate) const FORMAT: u32 = 3;

const FORMAT_FILE: &str = "format";
const COMMIT_FILE: &str = "commit";
const JOURNAL_DIR: &str = "journal";

/// The state of the Vault the Index reflects: the commit `HEAD` names in the
/// main repository, and in the nested repository (`None` where there is no
/// commit or no repository).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct State {
    main: Option<Oid>,
    isolated: Option<Oid>,
}

impl State {
    /// The state in which the main repository is at `main` and the nested
    /// one at `isolated`.
    pub(crate) fn new(main: Option<Oid>, isolated: Option<Oid>) -> State {
        State { main, isolated }
    }

    /// The state the snapshots of `views` are of.
    pub(crate) fn of(views: &Views) -> State {
        State::new(
            views.of(Repo::Main).commit(),
            views.of(Repo::Isolated).commit(),
        )
    }

    /// The commit of `repo` in this state.
    pub(crate) fn oid(self, repo: Repo) -> Option<Oid> {
        match repo {
            Repo::Main => self.main,
            Repo::Isolated => self.isolated,
        }
    }

    /// The opaque token the state is reported as (`IndexState.version`).
    pub(crate) fn token(self) -> String {
        match (self.main, self.isolated) {
            (Some(main), Some(isolated)) => format!("{main}+{isolated}"),
            (Some(main), None) => main.to_string(),
            (None, Some(isolated)) => format!("-+{isolated}"),
            (None, None) => String::from("-"),
        }
    }

    /// The state a token names: what [`State::token`] made, or `None` for
    /// text that is not a token.
    #[cfg(feature = "index")]
    pub(crate) fn of_token(token: &str) -> Option<State> {
        let (main, isolated) = match token.split_once('+') {
            Some((main, isolated)) => (main, Some(isolated)),
            None => (token, None),
        };
        let main = parse_oid(main)?;
        let isolated = match isolated {
            None => None,
            Some(word) => Some(Oid::from_str(word).ok()?),
        };
        Some(State { main, isolated })
    }

    /// The one line the state is stored as: `main {oid}`, then
    /// ` isolated {oid}` when there is a nested commit; `-` for no commit.
    pub(crate) fn line(self) -> String {
        let main = self
            .main
            .map_or_else(|| String::from("-"), |oid| oid.to_string());
        match self.isolated {
            Some(isolated) => format!("main {main} isolated {isolated}"),
            None => format!("main {main}"),
        }
    }

    /// The state a stored [`State::line`] says, or `None` when the line is
    /// not one.
    pub(crate) fn parse(line: &str) -> Option<State> {
        let mut words = line.split_whitespace();
        if words.next()? != "main" {
            return None;
        }
        let main = parse_oid(words.next()?)?;
        let isolated = match words.next() {
            None => None,
            Some("isolated") => parse_oid(words.next()?)?,
            Some(_) => return None,
        };
        words.next().is_none().then_some(State { main, isolated })
    }
}

/// `-` is no commit; anything else must be an object id.
fn parse_oid(word: &str) -> Option<Option<Oid>> {
    if word == "-" {
        return Some(None);
    }
    Oid::from_str(word).ok().map(Some)
}

/// The Index of one Vault on disk.
#[derive(Clone, Debug)]
pub(crate) struct Index {
    scheda: PathBuf,
    root: PathBuf,
}

impl Index {
    /// The Index of the Vault at `vault`. Nothing is read or made.
    pub(crate) fn new(vault: &Path) -> Index {
        let scheda = vault.join(".scheda");
        Index {
            root: scheda.join("index"),
            scheda,
        }
    }

    /// `{vault}/.scheda/index`.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The folder of the id maps of the journals.
    pub(crate) fn journal_dir(&self) -> PathBuf {
        self.root.join(JOURNAL_DIR)
    }

    /// The id map file of the journal `collection`.
    pub(crate) fn ids_file(&self, collection: &str) -> PathBuf {
        self.journal_dir().join(collection).join("ids")
    }

    /// Takes the lock of a round without waiting: `flock` on
    /// `{vault}/.scheda/index.lock`. `None` when another indexer, of this
    /// process or another, holds it.
    pub(crate) fn try_lock(&self) -> Result<Option<Flock>, Error> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file: File = options
            .open(self.scheda.join("index.lock"))
            .map_err(io)
            .context("open the index lock file")?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Flock::held(file))),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(err)) => Err(io(err)).context("take the index lock"),
        }
    }

    /// Removes the temporary files writes under the Index left when their
    /// process died, in every folder a round writes a file of its own into:
    /// the Index folder, the folders in it (its generations among them), and
    /// each journal's id map folder; Lance names its own files. Only a round
    /// writes under the Index, and it holds `index.lock`, as `_held` shows
    /// the caller does: every such file is a leftover.
    ///
    /// # Errors
    ///
    /// `STORAGE` when a folder cannot be read or a file removed.
    pub(crate) fn sweep_temporary(&self, _held: &Flock) -> Result<(), Error> {
        remove_temporary(&self.root)?;
        for dir in folders_in(&self.root)? {
            remove_temporary(&dir)?;
        }
        for dir in folders_in(&self.journal_dir())? {
            remove_temporary(&dir)?;
        }
        Ok(())
    }

    /// Whether the Index on disk has the layout this build reads.
    pub(crate) fn has_format(&self) -> Result<bool, Error> {
        match fs::read_to_string(self.root.join(FORMAT_FILE)) {
            Ok(text) => Ok(text.trim().parse::<u32>().is_ok_and(|n| n == FORMAT)),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidData
                ) =>
            {
                Ok(false)
            }
            Err(err) => Err(io(err)).context("read the format of the Index"),
        }
    }

    /// The state the Index reflects, or `None` when it has no commit: not
    /// built, being rebuilt, or a `commit` file that says nothing valid.
    pub(crate) fn read_commit(&self) -> Result<Option<State>, Error> {
        match fs::read_to_string(self.root.join(COMMIT_FILE)) {
            Ok(text) => Ok(State::parse(text.trim())),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidData
                ) =>
            {
                Ok(None)
            }
            Err(err) => Err(io(err)).context("read the commit of the Index"),
        }
    }

    /// Records `state` as the state the Index reflects. The last write of a
    /// round.
    pub(crate) fn write_commit(&self, state: State) -> Result<(), Error> {
        self.write(
            &self.root.join(COMMIT_FILE),
            format!("{}\n", state.line()).as_bytes(),
        )
        .context("write the commit of the Index")
    }

    /// Writes the file at `path` below the Index whole, owner-only.
    pub(crate) fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        atomic_write_private(path, bytes)?;
        restrict_file_below(&self.scheda, path)
    }

    /// Empties the journal id maps for a rebuild of theirs: `commit` goes
    /// first, so maps half emptied or half built are never taken for built
    /// ones; then the maps. The generations of the Lance tables stay: a
    /// rebuild of those is made beside the live one.
    pub(crate) fn reset_maps(&self) -> Result<(), Error> {
        match fs::remove_file(self.root.join(COMMIT_FILE)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io(err)).context("remove the commit of the Index"),
        }
        match fs::remove_dir_all(self.journal_dir()) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(io(err)).context("remove the journal id maps"),
        }
    }

    /// Empties the Index of another layout whole: `commit` goes first, so an
    /// Index that is half emptied or half built is never taken for a built
    /// one; then every other file, whatever layout it is of; then `format`
    /// is written again.
    pub(crate) fn reset(&self) -> Result<(), Error> {
        match fs::remove_file(self.root.join(COMMIT_FILE)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io(err)).context("remove the commit of the Index"),
        }
        create_dir_private(&self.root).context("create the Index folder")?;
        restrict_dir(&self.root)?;
        for entry in fs::read_dir(&self.root)
            .map_err(io)
            .context("read the Index folder")?
        {
            let path = entry.map_err(io).context("read the Index folder")?.path();
            let is_dir = fs::symlink_metadata(&path)
                .map_err(io)
                .context("look at a file of the Index")?
                .is_dir();
            let removed = if is_dir {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            match removed {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(io(err)).context("remove a file of the Index"),
            }
        }
        self.write(
            &self.root.join(FORMAT_FILE),
            format!("{FORMAT}\n").as_bytes(),
        )
        .context("write the format of the Index")
    }
}

/// What a round has read so far, for the progress of a rebuild.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Counts {
    /// Documents read.
    pub(crate) documents_read: u64,
    /// Documents left out because they could not be read.
    pub(crate) documents_skipped: u64,
    /// Journal segments read.
    pub(crate) segments_read: u64,
}

/// What a round has to apply.
#[derive(Debug)]
enum Changes {
    /// Everything: the Index was emptied.
    Everything,
    /// What differs between the state the Index reflected and the new one.
    Since {
        /// The files that differ, in both repositories.
        paths: Vec<Delta>,
        /// The collections whose declaration file differs: all their
        /// Documents count as changed.
        whole: BTreeSet<String>,
    },
}

impl Changes {
    /// The difference between `from` and the state of `views`. `None` when
    /// a commit of `from` is gone from its repository: nothing says what
    /// changed, so the Index is made again.
    fn since(views: &Views, from: State) -> Result<Option<Changes>, Error> {
        let mut paths = Vec::new();
        for repo in [Repo::Main, Repo::Isolated] {
            match views
                .of(repo)
                .changed_since(from.oid(repo), None)
                .context("compare the commits")?
            {
                Some(mut deltas) => paths.append(&mut deltas),
                None => return Ok(None),
            }
        }
        let whole = paths
            .iter()
            .filter_map(|delta| collection::declaration_dir(&delta.path))
            .map(str::to_owned)
            .collect();
        Ok(Some(Changes::Since { paths, whole }))
    }
}

/// One part of the Index. A round gives each side the difference to apply;
/// a side writes under `index/` only, and only in [`Side::apply`].
pub(crate) trait Side: Send {
    /// Brings the side to the state of `round`. After a rebuild's reset the
    /// side starts from nothing (`round.is_rebuild()`). It may be called
    /// again for the same round after a failure, so it must not depend on a
    /// failed earlier attempt. It reports what it reads with
    /// [`Round::note`], which also tells it to stop.
    fn apply(&mut self, round: &mut Round<'_>) -> Result<(), Error>;
}

/// One round of the indexer, as the sides see it.
pub(crate) struct Round<'a> {
    index: &'a Index,
    to: State,
    views: &'a Views,
    declarations: &'a Declarations,
    changes: Changes,
    counts: Counts,
    tick: &'a mut dyn FnMut(&Counts) -> Result<(), Error>,
}

impl<'a> Round<'a> {
    /// A round that moves the Index to the state of `views`, from `from`
    /// (`None`: from nothing). `tick` is called with the counts each time a
    /// side notes progress; an error from it ends the round. Returns `None`
    /// when the difference is not known (see [`Changes::since`]).
    pub(crate) fn new(
        index: &'a Index,
        views: &'a Views,
        declarations: &'a Declarations,
        from: Option<State>,
        tick: &'a mut dyn FnMut(&Counts) -> Result<(), Error>,
    ) -> Result<Option<Round<'a>>, Error> {
        let changes = match from {
            None => Changes::Everything,
            Some(from) => match Changes::since(views, from)? {
                Some(changes) => changes,
                None => return Ok(None),
            },
        };
        Ok(Some(Round {
            index,
            to: State::of(views),
            views,
            declarations,
            changes,
            counts: Counts::default(),
            tick,
        }))
    }

    /// The Index.
    pub(crate) fn index(&self) -> &'a Index {
        self.index
    }

    /// The state the round moves the Index to.
    pub(crate) fn to(&self) -> State {
        self.to
    }

    /// The snapshots of the state the round moves the Index to.
    pub(crate) fn views(&self) -> &'a Views {
        self.views
    }

    /// The collections declared in that state.
    pub(crate) fn declarations(&self) -> &'a Declarations {
        self.declarations
    }

    /// Whether the round builds the Index from nothing.
    #[cfg(not(feature = "index"))]
    pub(crate) fn is_rebuild(&self) -> bool {
        matches!(self.changes, Changes::Everything)
    }

    /// Whether every Document of the collection `name` counts as changed:
    /// the round is a rebuild, or the declaration of the collection differs.
    pub(crate) fn is_whole(&self, name: &str) -> bool {
        match &self.changes {
            Changes::Everything => true,
            Changes::Since { whole, .. } => whole.contains(name),
        }
    }

    /// Adds to the counts and lets the observer see them. An error ends the
    /// round: the indexer is stopping.
    pub(crate) fn note(&mut self, read: u64, skipped: u64, segments: u64) -> Result<(), Error> {
        self.counts.documents_read += read;
        self.counts.documents_skipped += skipped;
        self.counts.segments_read += segments;
        (self.tick)(&self.counts)
    }

    /// The counts so far.
    #[cfg(not(feature = "index"))]
    pub(crate) fn counts(&self) -> Counts {
        self.counts
    }

    /// How many files the round has to read, counting files and not
    /// Documents, so that a journal segment is one however many lines it
    /// holds: the files that changed inside
    /// a declared collection, and every file of a collection whose
    /// declaration changed. `None` when they could not be counted.
    pub(crate) fn pending(&self) -> Option<u64> {
        let mut total = 0u64;
        match &self.changes {
            Changes::Everything => {
                for decl in self.declarations.iter() {
                    total += self.count_in(decl.repo(), &isolated::vault_rel(decl, ""))?;
                }
            }
            Changes::Since { paths, whole, .. } => {
                for delta in paths {
                    if collection::declaration_dir(&delta.path).is_some() {
                        continue;
                    }
                    let name = delta
                        .path
                        .strip_prefix(".isolated/")
                        .unwrap_or(&delta.path)
                        .split('/')
                        .next()
                        .unwrap_or("");
                    if self.declarations.get(name).is_some() && !whole.contains(name) {
                        total += 1;
                    }
                }
                for name in whole {
                    if let Some(decl) = self.declarations.get(name) {
                        total += self.count_in(decl.repo(), &isolated::vault_rel(decl, ""))?;
                    }
                }
            }
        }
        Some(total)
    }

    fn count_in(&self, repo: Repo, dir: &str) -> Option<u64> {
        count_files(self.views.of(repo), dir.trim_end_matches('/')).ok()
    }
}

/// The files at and below the directory `dir` of the commit of `snap`,
/// those with a hidden name (the declaration file, for one) left out.
fn count_files(snap: &Snap, dir: &str) -> Result<u64, crate::error::Fault> {
    let mut total = 0;
    for child in snap.list(dir)?.unwrap_or_default() {
        if child.name.starts_with('.') {
            continue;
        }
        match child.node.kind {
            crate::repo::NodeKind::File => total += 1,
            crate::repo::NodeKind::Dir => {
                total += count_files(snap, &format!("{dir}/{}", child.name))?;
            }
            crate::repo::NodeKind::Link | crate::repo::NodeKind::Other => {}
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use git2::Oid;

    use super::{FORMAT, Index, State};

    fn oid(n: u8) -> Oid {
        Oid::from_bytes(&[n; 20]).unwrap()
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-index-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            crate::testing::remove_when_the_test_ends(&dir);
            std::fs::create_dir_all(dir.join(".scheda")).unwrap();
            Scratch(dir)
        }
    }

    #[test]
    fn a_state_round_trips_through_its_line() {
        for state in [
            State::default(),
            State {
                main: Some(oid(1)),
                isolated: None,
            },
            State {
                main: Some(oid(1)),
                isolated: Some(oid(2)),
            },
            State {
                main: None,
                isolated: Some(oid(2)),
            },
        ] {
            assert_eq!(State::parse(&state.line()), Some(state), "{}", state.line());
        }
        let a = State {
            main: Some(oid(1)),
            isolated: None,
        };
        let b = State {
            main: Some(oid(1)),
            isolated: Some(oid(2)),
        };
        assert_ne!(a.token(), b.token());
    }

    #[test]
    fn a_line_that_is_not_a_state_is_none() {
        for line in [
            "",
            "main",
            "main zz",
            "isolated -",
            "main - extra",
            "main - isolated",
        ] {
            assert_eq!(State::parse(line), None, "{line:?}");
        }
    }

    #[test]
    fn the_commit_is_written_whole_and_reset_removes_it_first_and_every_other_file() {
        let scratch = Scratch::new("reset");
        let index = Index::new(&scratch.0);
        assert!(!index.has_format().unwrap());
        assert_eq!(index.read_commit().unwrap(), None);
        index.reset().unwrap();
        assert!(index.has_format().unwrap());
        let state = State {
            main: Some(oid(7)),
            isolated: None,
        };
        index.write_commit(state).unwrap();
        index
            .write(&index.root().join("old/layout/file"), b"x")
            .unwrap();
        assert_eq!(index.read_commit().unwrap(), Some(state));
        index.reset().unwrap();
        assert_eq!(index.read_commit().unwrap(), None);
        assert!(!index.root().join("old").exists());
        assert_eq!(
            std::fs::read_to_string(index.root().join("format")).unwrap(),
            format!("{FORMAT}\n")
        );
    }

    #[test]
    fn another_format_or_a_broken_commit_reads_as_not_built() {
        let scratch = Scratch::new("format");
        let index = Index::new(&scratch.0);
        index.reset().unwrap();
        std::fs::write(index.root().join("format"), "999\n").unwrap();
        assert!(!index.has_format().unwrap());
        std::fs::write(index.root().join("commit"), "garbage\n").unwrap();
        assert_eq!(index.read_commit().unwrap(), None);
    }

    #[test]
    fn the_lock_is_held_by_one_and_never_waited_for() {
        let scratch = Scratch::new("lock");
        let index = Index::new(&scratch.0);
        let held = index.try_lock().unwrap();
        assert!(held.is_some());
        assert!(Index::new(&scratch.0).try_lock().unwrap().is_none());
        drop(held);
        assert!(index.try_lock().unwrap().is_some());
    }
}
