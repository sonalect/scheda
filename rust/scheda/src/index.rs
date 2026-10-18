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
//! .scheda/index/commit      the state the Index reflects, written last
//! .scheda/index/journal/    the id maps of the journals
//! .scheda/index/lexical/    the Tantivy index of the lexical side
//! .scheda/index/vector/     the vectors of the vector side and the state they reached
//! .scheda/index/columns     the columns each collection was indexed with
//! .scheda/index/stale       the collections whose columns changed since
//! ```
//!
//! Every file is written whole (a temporary file, then a rename) and is
//! owner-only. Only the indexer writes under `index/`, under the lock; a
//! file of another layout is never read, the Index is made again.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
#[cfg(feature = "find")]
use std::sync::Arc;

use git2::Oid;
#[cfg(feature = "find")]
use scheda_api::v1::VaultErrorCode;

use crate::collection::{self, Declarations};
#[cfg(feature = "find")]
use crate::error::Fault;
use crate::error::{Context, Error, io};
use crate::fs::{
    Flock, atomic_write_private, create_dir_private, restrict_dir, restrict_file_below,
};
use crate::isolated;
#[cfg(feature = "find")]
use crate::ports::ProjectorPort;
use crate::repo::{Delta, Repo, Snap, Views};

/// The number of the layout of the files of the Index. An Index with another
/// number is made again, not migrated.
pub(crate) const FORMAT: u32 = 2;

const FORMAT_FILE: &str = "format";
const COMMIT_FILE: &str = "commit";
const JOURNAL_DIR: &str = "journal";
const LEXICAL_DIR: &str = "lexical";
#[cfg(feature = "search")]
const VECTOR_DIR: &str = "vector";
#[cfg(feature = "find")]
const COLUMNS_FILE: &str = "columns";
#[cfg(feature = "find")]
const STALE_FILE: &str = "stale";

/// The state of the Vault the Index reflects: the commit `HEAD` names in the
/// main repository, and in the nested repository (`None` where there is no
/// commit or no repository).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct State {
    main: Option<Oid>,
    isolated: Option<Oid>,
}

impl State {
    /// The state the snapshots of `views` are of.
    pub(crate) fn of(views: &Views) -> State {
        State {
            main: views.of(Repo::Main).commit(),
            isolated: views.of(Repo::Isolated).commit(),
        }
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

    /// The folder of the Tantivy index of the lexical side.
    pub(crate) fn lexical_dir(&self) -> PathBuf {
        self.root.join(LEXICAL_DIR)
    }

    /// The folder of the vector side: `vectors` (the model, the key map, and
    /// the HNSW index, in one file) and `state`.
    #[cfg(feature = "search")]
    pub(crate) fn vector_dir(&self) -> PathBuf {
        self.root.join(VECTOR_DIR)
    }

    /// The file that records the columns of each collection as the lexical
    /// side indexed them.
    #[cfg(feature = "find")]
    pub(crate) fn columns_file(&self) -> PathBuf {
        self.root.join(COLUMNS_FILE)
    }

    /// The file that lists the collections whose columns changed since the
    /// Index was built. Its presence makes the Index stale until a rebuild.
    #[cfg(feature = "find")]
    pub(crate) fn stale_file(&self) -> PathBuf {
        self.root.join(STALE_FILE)
    }

    /// The state the Index reflects, for a reader.
    ///
    /// # Errors
    ///
    /// `INDEX_REBUILDING` (retryable) when the Index has no commit, because
    /// no round has run yet or a rebuild runs, and when it is of another
    /// format, which the indexer makes again; `INDEX_STALE` when the
    /// columns of the Projector changed since the Index was built.
    #[cfg(feature = "search")]
    pub(crate) fn readable(&self) -> Result<State, Error> {
        let committed = self.committed()?;
        if self.stale_file().is_file() {
            return Err(Fault::new(
                VaultErrorCode::IndexStale,
                "the columns of the Projector changed since the Index was built; run Reindex",
            )
            .into());
        }
        Ok(committed)
    }

    /// The state the Index reflects, whether or not some collection is
    /// stale: a reader that looks at the stale collections itself
    /// ([`read_stale`](crate::lexical::read_stale)) starts here.
    ///
    /// # Errors
    ///
    /// `INDEX_REBUILDING` (retryable) when the Index has no commit, because
    /// no round has run yet or a rebuild runs, and when it is of another
    /// format, which the indexer makes again.
    #[cfg(feature = "find")]
    pub(crate) fn committed(&self) -> Result<State, Error> {
        if !self.has_format()? {
            return Err(rebuilding("the Index is not built in this format yet"));
        }
        let Some(committed) = self.read_commit()? else {
            return Err(rebuilding("the Index has no commit yet"));
        };
        Ok(committed)
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

    /// Empties the Index for a rebuild: `commit` goes first, so an Index
    /// that is half emptied or half built is never taken for a built one;
    /// then every other file, whatever layout it is of; then `format` is
    /// written again.
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

/// The failure of a reader that finds the Index not built, or being built.
#[cfg(feature = "find")]
pub(crate) fn rebuilding(what: &'static str) -> Error {
    Fault::new(VaultErrorCode::IndexRebuilding, what).into()
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
        /// The state the Index reflected.
        #[cfg(feature = "find")]
        from: State,
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
        Ok(Some(Changes::Since {
            #[cfg(feature = "find")]
            from,
            paths,
            whole,
        }))
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
    /// The Projector the round projects with.
    #[cfg(feature = "find")]
    projector: Option<Arc<ProjectorPort>>,
    /// The vector values the lexical side projected, for the vector side;
    /// `None` when no side wants them.
    #[cfg(feature = "search")]
    offers: Option<Vec<Offer>>,
}

/// The texts one Document gives the vector side: one per value of each
/// `vector` column, taken from the projection the lexical side made, so a
/// Document is projected once per round whichever sides are on.
#[cfg(feature = "search")]
#[derive(Debug)]
pub(crate) struct Offer {
    /// The collection of the Document.
    pub(crate) collection: String,
    /// The id of the Document (of the line, for a journal).
    pub(crate) id: String,
    /// The Vault-relative file the Document came from.
    pub(crate) file: String,
    /// The texts, in the order of the columns.
    pub(crate) pieces: Vec<Piece>,
}

/// One text to embed.
#[cfg(feature = "search")]
#[derive(Debug)]
pub(crate) struct Piece {
    /// The `vector` column it is a value of.
    pub(crate) column: String,
    /// Its place among the values of that column (a text list has several).
    pub(crate) at: u32,
    /// The text; the vector side moves it into the embedder.
    pub(crate) text: String,
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
            #[cfg(feature = "find")]
            projector: None,
            #[cfg(feature = "search")]
            offers: None,
        }))
    }

    /// Gives the round the Projector it projects with: the one the check of
    /// the round passed, so a Projector replaced meanwhile waits for the
    /// next round.
    #[cfg(feature = "find")]
    pub(crate) fn set_projector(&mut self, projector: Option<Arc<ProjectorPort>>) {
        self.projector = projector;
    }

    /// The Projector of the round, if the Vault has one.
    #[cfg(feature = "find")]
    pub(crate) fn projector(&self) -> Option<Arc<ProjectorPort>> {
        self.projector.clone()
    }

    /// Asks the lexical side to offer the vector values of the Documents it
    /// projects.
    #[cfg(feature = "search")]
    pub(crate) fn collect_offers(&mut self) {
        self.offers.get_or_insert_with(Vec::new);
    }

    /// Whether the vector side wants the offers of this round.
    #[cfg(feature = "search")]
    pub(crate) fn collecting(&self) -> bool {
        self.offers.is_some()
    }

    /// Hands over what the lexical side offered.
    #[cfg(feature = "search")]
    pub(crate) fn set_offers(&mut self, offers: Vec<Offer>) {
        if self.offers.is_some() {
            self.offers = Some(offers);
        }
    }

    /// Takes what the lexical side offered; `None` when nobody asked.
    #[cfg(feature = "search")]
    pub(crate) fn take_offers(&mut self) -> Option<Vec<Offer>> {
        self.offers.take()
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

    /// The state the Index reflected before the round; `None` for a rebuild.
    #[cfg(feature = "find")]
    pub(crate) fn from(&self) -> Option<State> {
        match &self.changes {
            Changes::Everything => None,
            Changes::Since { from, .. } => Some(*from),
        }
    }

    /// The files that differ between the state the Index reflected and the
    /// new one, in both repositories. Empty for a rebuild, where everything
    /// is new.
    #[cfg(feature = "find")]
    pub(crate) fn deltas(&self) -> &[Delta] {
        match &self.changes {
            Changes::Everything => &[],
            Changes::Since { paths, .. } => paths,
        }
    }

    /// The collections whose declaration file differs. Empty for a rebuild.
    #[cfg(feature = "find")]
    pub(crate) fn declaration_changes(&self) -> impl Iterator<Item = &str> {
        let names = match &self.changes {
            Changes::Everything => None,
            Changes::Since { whole, .. } => Some(whole),
        };
        names.into_iter().flatten().map(String::as_str)
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
            std::fs::create_dir_all(dir.join(".scheda")).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
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
