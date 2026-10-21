//! The id maps of the journals: for each journal, which segment holds the
//! line of each id.
//!
//! A map is a file of the Index, `index/journal/{n}/ids`, where `n` is the
//! number the Index gave the journal (`index/journal/numbers`; never the
//! collection's name, which could be 255 bytes), written whole by the
//! indexer only. It records the state of the Vault it was made
//! at, so it is true on its own: a line it does not list is looked for in
//! the segments that differ between that state and `HEAD` (a tree
//! comparison, no process), and only a journal with no map is scanned
//! whole. A write never touches a map.
//!
//! ```text
//! scheda-ids 1
//! main {oid} [isolated {oid}]      the state the map is true at
//! {n}                              how many segments follow
//! "{segment path}"                 JSON strings, Vault-relative
//! {id}\t{index of its segment}     one line per id, sorted by id
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use scheda_api::v1::{Problem, VaultErrorCode};

use crate::check::problem;
use crate::collection::{Declaration, Declarations, Kind};
use crate::error::{Context, Error, Fault, io};
use crate::index::{Index, Numbers, Round, Side, State};
use crate::isolated;
use crate::journal::{self, Line, SegmentMemo};
use crate::repo::{Delta, Snap, Views};

const HEADER: &str = "scheda-ids 1";

fn broken(message: impl Into<String>) -> Fault {
    Fault::new(VaultErrorCode::IndexBroken, message)
}

/// The id map of one journal.
#[derive(Debug)]
pub(crate) struct IdMap {
    state: State,
    segments: Vec<String>,
    ids: BTreeMap<String, u32>,
}

impl IdMap {
    fn new(state: State) -> IdMap {
        IdMap {
            state,
            segments: Vec::new(),
            ids: BTreeMap::new(),
        }
    }

    /// The file's text. Only the segments some id points to are written.
    fn encode(&self) -> Result<Vec<u8>, Fault> {
        let mut used: Vec<u32> = self.ids.values().copied().collect();
        used.sort_unstable();
        used.dedup();
        let remap: HashMap<u32, usize> = used.iter().enumerate().map(|(to, &n)| (n, to)).collect();
        let mut out = format!("{HEADER}\n{}\n{}\n", self.state.line(), used.len());
        for n in &used {
            let path = self
                .segments
                .get(*n as usize)
                .ok_or_else(|| broken("an id points to a segment the map does not list"))?;
            let json = serde_json::to_string(path)
                .map_err(|err| broken("a segment path cannot be written").with_source(err))?;
            out.push_str(&json);
            out.push('\n');
        }
        for (id, n) in &self.ids {
            let at = remap.get(n).copied().unwrap_or(0);
            let _ = writeln!(out, "{id}\t{at}");
        }
        Ok(out.into_bytes())
    }

    /// The map a file's bytes hold; `INDEX_BROKEN` when they are not a map.
    fn parse(bytes: &[u8]) -> Result<IdMap, Fault> {
        let text = std::str::from_utf8(bytes)
            .map_err(|err| broken("the id map is not UTF-8").with_source(err))?;
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Err(broken("the id map has another header"));
        }
        let state = lines
            .next()
            .and_then(State::parse)
            .ok_or_else(|| broken("the id map has no state"))?;
        let count: usize = lines
            .next()
            .and_then(|line| line.parse().ok())
            .ok_or_else(|| broken("the id map has no segment count"))?;
        let mut segments = Vec::with_capacity(count.min(1 << 16));
        for _ in 0..count {
            let line = lines
                .next()
                .ok_or_else(|| broken("the id map ends inside its segment list"))?;
            let path: String = serde_json::from_str(line).map_err(|err| {
                broken("the id map has a segment path that is not a string").with_source(err)
            })?;
            segments.push(path);
        }
        let mut ids = BTreeMap::new();
        for line in lines {
            let (id, at) = line
                .split_once('\t')
                .ok_or_else(|| broken("the id map has a line without a tab"))?;
            let at: u32 = at
                .parse()
                .map_err(|err| broken("the id map has a bad segment number").with_source(err))?;
            if at as usize >= segments.len() {
                return Err(broken("the id map points past its segment list"));
            }
            ids.insert(id.to_owned(), at);
        }
        Ok(IdMap {
            state,
            segments,
            ids,
        })
    }

    /// The segment the line of `id` is in.
    fn segment_of(&self, id: &str) -> Option<&str> {
        self.ids
            .get(id)
            .and_then(|n| self.segments.get(*n as usize))
            .map(String::as_str)
    }

    fn intern(&mut self, path: &str) -> u32 {
        let at = self
            .segments
            .iter()
            .position(|known| known == path)
            .unwrap_or_else(|| {
                self.segments.push(path.to_owned());
                self.segments.len() - 1
            });
        at as u32
    }

    /// Forgets every id whose segment is in `touched`.
    fn forget(&mut self, touched: &HashSet<&str>) {
        let segments = &self.segments;
        self.ids.retain(|_, n| {
            segments
                .get(*n as usize)
                .is_none_or(|path| !touched.contains(path.as_str()))
        });
    }

    /// Adds the lines of the segment `path` in the commit of `snap`. The
    /// first line of an id in append order is the one kept. Returns how
    /// many lines were read and how many were left out (a line that does
    /// not parse, a tail without `\n`); `None` when the commit holds no
    /// such file.
    fn add_segment(&mut self, snap: &Snap, path: &Path) -> Result<Option<(u64, u64)>, Fault> {
        let Some(bytes) = journal::read_segment(snap, path)? else {
            return Ok(None);
        };
        let rel = journal::rel_str(path);
        let at = self.intern(&rel);
        let (lines, tail) = journal::split_lines(&bytes);
        let (mut read, mut skipped) = (0u64, u64::from(tail.is_some()));
        for line in lines {
            let Ok(line) = journal::parse_line(line) else {
                skipped += 1;
                continue;
            };
            read += 1;
            match self.ids.get(&line.id) {
                None => {
                    self.ids.insert(line.id, at);
                }
                Some(&known) => {
                    let later = self.segments.get(known as usize).is_some_and(|known| {
                        journal::segment_order(Path::new(known)) > journal::segment_order(path)
                    });
                    if later {
                        self.ids.insert(line.id, at);
                    }
                }
            }
        }
        Ok(Some((read, skipped)))
    }
}

/// What a builder tells its caller about each segment it reads: lines read,
/// lines left out, segments read.
type Note<'a> = &'a mut dyn FnMut(u64, u64, u64) -> Result<(), Error>;

/// The map of the journal `decl` from every segment of the commit of `snap`.
fn build(snap: &Snap, decl: &Declaration, state: State, note: Note<'_>) -> Result<IdMap, Error> {
    let mut map = IdMap::new(state);
    for path in journal::list_segments(snap, decl).context("list the segments")? {
        if let Some((read, skipped)) = map.add_segment(snap, &path).context("read a segment")? {
            note(read, skipped, 1)?;
        }
    }
    Ok(map)
}

/// Brings `map` from its state to the commit of `snap`: the segments that
/// differ in between are read again.
fn update(
    map: &mut IdMap,
    snap: &Snap,
    decl: &Declaration,
    changed: &[Delta],
    to: State,
    note: Note<'_>,
) -> Result<(), Error> {
    let touched: HashSet<&str> = changed.iter().map(|delta| delta.path.as_str()).collect();
    map.forget(&touched);
    for path in journal::list_segments(snap, decl).context("list the segments")? {
        if !touched.contains(journal::rel_str(&path).as_str()) {
            continue;
        }
        if let Some((read, skipped)) = map.add_segment(snap, &path).context("read a segment")? {
            note(read, skipped, 1)?;
        }
    }
    map.state = to;
    Ok(())
}

/// The map file of the journal `name`; `None` when there is none or it is
/// not a map.
fn read_map(index: &Index, name: &str) -> Result<Option<IdMap>, Error> {
    let Some(file) = index.ids_file(name) else {
        return Ok(None);
    };
    match fs::read(file) {
        Ok(bytes) => Ok(IdMap::parse(&bytes).ok()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io(err)).context("read an id map"),
    }
}

/// The part of the Index that holds the id maps of the journals.
#[derive(Debug, Default)]
pub(crate) struct JournalMaps;

impl Side for JournalMaps {
    fn apply(&mut self, round: &mut Round<'_>) -> Result<(), Error> {
        let index = round.index();
        let declarations = round.declarations();
        let numbers = number_journals(index, declarations)?;
        for decl in declarations
            .iter()
            .filter(|decl| decl.kind() == Kind::Journal)
        {
            let snap = round.views().of(decl.repo());
            let Some(n) = numbers.of(decl.name()) else {
                continue;
            };
            let file = index.map_dir(n).join("ids");
            let to = round.to();
            let existing = if round.is_whole(decl.name()) {
                None
            } else {
                read_map(index, decl.name())?
            };
            let dir = isolated::vault_rel(decl, "");
            let from = existing
                .as_ref()
                .map(|map| snap.changed_since(map.state.oid(decl.repo()), Some(&dir)))
                .transpose()
                .context("compare the commits of a journal")?
                .flatten();
            let mut note = |read, skipped, segments| round.note(read, skipped, segments);
            let map = match (existing, from) {
                (Some(mut map), Some(changed)) => {
                    if changed.is_empty() {
                        continue;
                    }
                    update(&mut map, snap, decl, &changed, to, &mut note)
                        .context("update an id map")?;
                    map
                }
                _ => build(snap, decl, to, &mut note).context("build an id map")?,
            };
            index
                .write(&file, &map.encode()?)
                .context("write an id map")?;
        }
        Ok(())
    }
}

/// Gives every journal a number, drops the numbers of collections that are
/// not journals now with their maps, and records the numbers before any map
/// they name is written.
///
/// Numbers that are missing or broken while maps stand beside them are not
/// trusted: the maps go, and are built again.
fn number_journals(index: &Index, declarations: &Declarations) -> Result<Numbers, Error> {
    let is_journal = |name: &str| {
        declarations
            .get(name)
            .is_some_and(|decl| decl.kind() == Kind::Journal)
    };
    let mut changed = false;
    let mut numbers = match index.read_numbers()? {
        Some(numbers) => numbers,
        None => {
            match fs::remove_dir_all(index.journal_dir()) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(io(err)).context("remove the id maps"),
            }
            Numbers::default()
        }
    };
    changed |= numbers.retain(is_journal);
    for decl in declarations
        .iter()
        .filter(|decl| decl.kind() == Kind::Journal)
    {
        changed |= numbers.assign(decl.name()).1;
    }
    if changed {
        index.write_numbers(&numbers)?;
    }
    remove_stale(index, &numbers)?;
    Ok(numbers)
}

/// Removes the maps of the numbers no journal holds.
fn remove_stale(index: &Index, numbers: &Numbers) -> Result<(), Error> {
    let entries = match fs::read_dir(index.journal_dir()) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(io(err)).context("read the id map folder"),
    };
    for entry in entries {
        let entry = entry.map_err(io).context("read the id map folder")?;
        let is_dir = entry
            .file_type()
            .map_err(io)
            .context("read the id map folder")?
            .is_dir();
        if !is_dir {
            continue;
        }
        let keep = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_some_and(|n| numbers.holds(n));
        if !keep {
            fs::remove_dir_all(entry.path())
                .map_err(io)
                .context("remove an id map")?;
        }
    }
    Ok(())
}

/// What tells that a map file changed since it was read.
type Stamp = (u64, Option<SystemTime>);

/// The id maps as readers use them: each read from its file once and kept
/// until the file changes.
#[derive(Debug)]
pub(crate) struct IdMaps {
    index: Index,
    cache: Mutex<HashMap<String, (Stamp, Arc<IdMap>)>>,
}

impl IdMaps {
    /// The maps of the Index `index`.
    pub(crate) fn new(index: Index) -> IdMaps {
        IdMaps {
            index,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn cache(&self) -> MutexGuard<'_, HashMap<String, (Stamp, Arc<IdMap>)>> {
        // A map table no panic can leave half-written.
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The map of the journal `name`; `None` when there is no map, or the
    /// file is not one (the journal is then scanned whole).
    ///
    /// The map is a shortcut: a file that cannot be read for whatever
    /// reason (missing, a folder in its way, a mode) is a journal with no
    /// map, and a lookup then scans, as it always may.
    fn load(&self, name: &str) -> Option<Arc<IdMap>> {
        let Some(file) = self.index.ids_file(name) else {
            self.cache().remove(name);
            return None;
        };
        let Ok(meta) = fs::metadata(&file) else {
            self.cache().remove(name);
            return None;
        };
        let stamp = (meta.len(), meta.modified().ok());
        if let Some((known, map)) = self.cache().get(name)
            && *known == stamp
        {
            return Some(Arc::clone(map));
        }
        let parsed = fs::read(&file)
            .ok()
            .and_then(|bytes| IdMap::parse(&bytes).ok());
        let Some(map) = parsed else {
            self.cache().remove(name);
            return None;
        };
        let map = Arc::new(map);
        self.cache()
            .insert(name.to_owned(), (stamp, Arc::clone(&map)));
        Some(map)
    }

    /// The segment the map of `decl` places the line `id` in, if the map
    /// has the line. A hint: a segment changed by hand may say otherwise.
    pub(crate) fn segment_of(&self, decl: &Declaration, id: &str) -> Option<String> {
        self.load(decl.name())?.segment_of(id).map(str::to_owned)
    }

    /// The line of `id` in the journal `decl`, as the commit of `snap`
    /// holds it. See [`Lookup`].
    pub(crate) fn find(
        &self,
        snap: &Snap,
        decl: &Declaration,
        id: &str,
    ) -> Result<Option<Line>, Fault> {
        self.find_with(snap, decl, id, &mut SegmentMemo::default())
    }

    /// [`find`](Self::find), reading the segment through `memo`: a reader
    /// that finds many lines (a page) passes the same memo each time.
    pub(crate) fn find_with(
        &self,
        snap: &Snap,
        decl: &Declaration,
        id: &str,
        memo: &mut SegmentMemo,
    ) -> Result<Option<Line>, Fault> {
        self.lookup(snap, decl).find_with(id, memo)
    }

    /// A lookup of the lines of the journal `decl` as the commit of `snap`
    /// holds them, for one call that asks about many ids.
    pub(crate) fn lookup<'a>(&self, snap: &'a Snap, decl: &'a Declaration) -> Lookup<'a> {
        Lookup {
            snap,
            decl,
            map: self.load(decl.name()),
            unmapped: None,
        }
    }
}

/// The segments no map vouches for: those still to read, those read, and
/// the segment of each id read so far.
#[derive(Debug)]
struct Unmapped {
    /// The segments not read yet, the next one last.
    left: Vec<PathBuf>,
    read: Vec<PathBuf>,
    ids: HashMap<String, usize>,
}

/// Finds lines of one journal at one commit: the segment the map names for
/// the id (read and confirmed); else the segments that differ between the
/// map's state and the commit; and only a journal with no map (or one whose
/// state is gone from its repository, or one the map is wrong about) is
/// scanned. The segments that are not the map's are read in order, only as
/// far as a question needs, each once, and their ids kept: one question
/// stops at the segment that holds its id, and many questions read the
/// segments once between them.
#[derive(Debug)]
pub(crate) struct Lookup<'a> {
    snap: &'a Snap,
    decl: &'a Declaration,
    map: Option<Arc<IdMap>>,
    unmapped: Option<Unmapped>,
}

impl Lookup<'_> {
    /// The line of `id`, if the journal has it.
    pub(crate) fn find(&mut self, id: &str) -> Result<Option<Line>, Fault> {
        self.find_with(id, &mut SegmentMemo::default())
    }

    /// [`find`](Self::find), reading each segment through `memo`: the
    /// segment it read last is not read again for the next id. Only the
    /// line found is copied.
    pub(crate) fn find_with(
        &mut self,
        id: &str,
        memo: &mut SegmentMemo,
    ) -> Result<Option<Line>, Fault> {
        if let Some(map) = self.map.clone()
            && let Some(segment) = map.segment_of(id)
        {
            if let Some(line) = self.line_in(Path::new(segment), id, memo)? {
                return Ok(Some(line));
            }
            // The map is wrong about this id: the segment was changed by
            // hand. Nothing but a scan can say, and the map is not to be
            // trusted for any other id.
            self.map = None;
            self.unmapped = None;
        }
        let snap = self.snap;
        let unmapped = match self.unmapped.take() {
            Some(unmapped) => unmapped,
            None => self.list_unmapped()?,
        };
        let unmapped = self.unmapped.insert(unmapped);
        if let Some(path) = unmapped.ids.get(id).map(|at| unmapped.read[*at].clone()) {
            return self.line_in(&path, id, memo);
        }
        while let Some(path) = unmapped.left.pop() {
            let Some(bytes) = memo.read(snap, &path)? else {
                continue;
            };
            let at = unmapped.read.len();
            unmapped.read.push(path);
            let mut found = None;
            for (line_id, payload) in journal::lines(bytes) {
                if found.is_none() && line_id == id {
                    found = Some(Line {
                        id: line_id.to_owned(),
                        payload: payload.to_vec(),
                    });
                }
                if !unmapped.ids.contains_key(line_id) {
                    unmapped.ids.insert(line_id.to_owned(), at);
                }
            }
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    fn line_in(
        &self,
        path: &Path,
        id: &str,
        memo: &mut SegmentMemo,
    ) -> Result<Option<Line>, Fault> {
        let Some(bytes) = memo.read(self.snap, path)? else {
            return Ok(None);
        };
        Ok(journal::find_line(bytes, id))
    }

    /// The segments the map does not vouch for, none read yet.
    fn list_unmapped(&self) -> Result<Unmapped, Fault> {
        let changed = match &self.map {
            Some(map) => {
                let dir = isolated::vault_rel(self.decl, "");
                self.snap
                    .changed_since(map.state.oid(self.decl.repo()), Some(&dir))?
                    .map(|deltas| {
                        deltas
                            .into_iter()
                            .map(|delta| delta.path)
                            .collect::<HashSet<String>>()
                    })
            }
            None => None,
        };
        let mut left: Vec<PathBuf> = journal::list_segments(self.snap, self.decl)?
            .into_iter()
            .filter(|path| {
                changed
                    .as_ref()
                    .is_none_or(|changed| changed.contains(&journal::rel_str(path)))
            })
            .collect();
        left.reverse();
        Ok(Unmapped {
            left,
            read: Vec::new(),
            ids: HashMap::new(),
        })
    }
}

/// Checks the id maps against the segments of the commits `views` holds and
/// collects what is wrong, in collection order:
///
/// - a map file that is not a map (`INDEX_BROKEN`);
/// - a map that names a file outside its collection's segments
///   (`INDEX_BROKEN`);
/// - a map that is true at the state of `views` and differs from the one
///   the segments make (`INDEX_BROKEN`). A map at an earlier state is not
///   compared: the indexer has not caught up, which is not a defect.
///
/// A journal with no map has nothing to check. The call fails only when it
/// cannot run.
pub(crate) fn check_journal_id_maps(
    index: &Index,
    views: &Views,
    declarations: &Declarations,
) -> Result<Vec<Problem>, Error> {
    let mut problems = Vec::new();
    let now = State::of(views);
    for decl in declarations
        .iter()
        .filter(|decl| decl.kind() == Kind::Journal)
    {
        let Some(file) = index.ids_file(decl.name()) else {
            continue;
        };
        let rel = file
            .strip_prefix(
                index
                    .root()
                    .parent()
                    .and_then(Path::parent)
                    .unwrap_or(Path::new("")),
            )
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = match fs::read(&file) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(io(err)).context("check_journal_id_maps: read a map"),
        };
        let map = match IdMap::parse(&bytes) {
            Ok(map) => map,
            Err(fault) => {
                problems.push(problem(
                    VaultErrorCode::IndexBroken,
                    Some(&rel),
                    fault.message(),
                ));
                continue;
            }
        };
        let dir = isolated::vault_rel(decl, "");
        let outside = map
            .ids
            .values()
            .filter_map(|n| map.segments.get(*n as usize))
            .find(|path| !in_collection(path, &dir));
        if let Some(path) = outside {
            problems.push(problem(
                VaultErrorCode::IndexBroken,
                Some(&rel),
                format!(
                    "the id map names {path}, which is not a segment of the journal {}",
                    decl.name()
                ),
            ));
            continue;
        }
        if map.state == now {
            let expected = build(views.of(decl.repo()), decl, now, &mut |_, _, _| Ok(()))
                .context("check_journal_id_maps: read the segments")?;
            let same = expected.ids.len() == map.ids.len()
                && expected
                    .ids
                    .keys()
                    .all(|id| expected.segment_of(id) == map.segment_of(id));
            if !same {
                problems.push(problem(
                    VaultErrorCode::IndexBroken,
                    Some(&rel),
                    format!(
                        "the id map of the journal {} differs from its segments",
                        decl.name()
                    ),
                ));
            }
        }
    }
    Ok(problems)
}

/// Whether `path` is a segment file directly in the journal directory
/// `dir` (with its trailing `/`).
fn in_collection(path: &str, dir: &str) -> bool {
    path.strip_prefix(dir)
        .and_then(crate::layout::file_in_labels)
        .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('.') && rest.ends_with(".jsonl"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use scheda_api::v1::{DocumentCreation, DocumentWriteResult, OpenOptions, VaultErrorCode};

    use super::{IdMap, check_journal_id_maps, in_collection};
    use crate::document_calls::Writes;
    use crate::error::Error;
    use crate::index::{Index, State};
    use crate::journal::list_segments;
    use crate::repo::{Repo, Repos, blob_reads, fixture};
    use crate::testing::{create, create_collection, open, step};
    use crate::vault::Vault;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-idmap-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            crate::testing::remove_when_the_test_ends(&dir);
            Scratch(dir)
        }

        /// A Vault with the journal `log` and the line `old`, its indexer
        /// stopped after it built the map: the test makes the map itself.
        async fn vault(&self) -> (PathBuf, Arc<Vault>) {
            let vault = open(&self.0, OpenOptions::default().with_index_poll_ms(10))
                .await
                .unwrap();
            create_collection(
                &vault,
                serde_json::json!({"name": "log", "schema": {"journal": {}}}),
            )
            .await
            .unwrap();
            create(
                &vault,
                "log",
                DocumentCreation {
                    id: Some("old".to_owned()),
                    raw: Some(b"{}".to_vec()),
                    note: Some("test".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let path = crate::fs::canonicalize(&self.0).unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            while Index::new(&path).read_commit().unwrap().map(State::token)
                != Some(State::of(&Repos::new(&path).views().unwrap()).token())
            {
                assert!(Instant::now() < deadline, "the Index never caught up");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            vault.indexer().stop().await.unwrap();
            (path, vault)
        }

        /// A Vault with the journal `log`, capped at 40 bytes a segment
        /// (five lines of `l00: {}`), and `lines` lines in it, its indexer
        /// stopped after it built the id map of all of them.
        async fn segmented(&self, lines: usize) -> (PathBuf, Arc<Vault>) {
            let vault = open(&self.0, OpenOptions::default().with_index_poll_ms(10))
                .await
                .unwrap();
            create_collection(
                &vault,
                serde_json::json!({"name": "log", "schema": {"journal": {"segment_max_bytes": 40}}}),
            )
            .await
            .unwrap();
            for n in 0..lines {
                create(&vault, "log", creation(&format!("l{n:02}"), false))
                    .await
                    .unwrap();
            }
            let path = crate::fs::canonicalize(&self.0).unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            while Index::new(&path).read_commit().unwrap().map(State::token)
                != Some(State::of(&Repos::new(&path).views().unwrap()).token())
            {
                assert!(Instant::now() < deadline, "the Index never caught up");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            vault.indexer().stop().await.unwrap();
            (path, vault)
        }
    }

    fn creation(id: &str, keep_existing: bool) -> DocumentCreation {
        DocumentCreation {
            id: Some(id.to_owned()),
            raw: Some(b"{}".to_vec()),
            note: Some("test".to_owned()),
            keep_existing: Some(keep_existing),
            ..Default::default()
        }
    }

    /// One `Collection.Create` in `log`, its write one blocking step: the
    /// result, and how many blobs that step read.
    async fn append(
        vault: &Arc<Vault>,
        request: DocumentCreation,
    ) -> (Result<DocumentWriteResult, Error>, u64) {
        let writes = step(vault, move |cx, vault| {
            vault.precheck_documents(cx, Writes::one("log", request))
        })
        .await
        .unwrap();
        let held = vault.committer().acquire().await.unwrap();
        step(vault, move |cx, vault| {
            // The count of blob reads is of one thread: the whole write
            // is one step.
            let before = blob_reads();
            let result = vault
                .write_document_held(cx, held, writes)
                .map(|(result, _)| result);
            Ok((result, blob_reads() - before))
        })
        .await
        .unwrap()
    }

    fn segments(path: &Path, vault: &Vault) -> usize {
        let declarations = vault.declarations_now().unwrap();
        let snap = Repos::new(path).snapshot(Repo::Main).unwrap();
        list_segments(&snap, declarations.get("log").unwrap())
            .unwrap()
            .len()
    }

    fn head(path: &Path) -> State {
        State::of(&Repos::new(path).views().unwrap())
    }

    /// Replaces the map of `log` with one at `state` that lists `ids`
    /// (id, segment).
    fn craft(path: &Path, state: State, ids: &[(&str, &str)]) {
        let mut map = IdMap::new(state);
        for (id, segment) in ids {
            let at = map.intern(segment);
            map.ids.insert((*id).to_owned(), at);
        }
        let index = Index::new(path);
        let mut numbers = index.read_numbers().unwrap().unwrap_or_default();
        numbers.assign("log");
        index.write_numbers(&numbers).unwrap();
        index
            .write(&index.ids_file("log").unwrap(), &map.encode().unwrap())
            .unwrap();
    }

    fn find(vault: &Vault, path: &Path, id: &str) -> bool {
        let declarations = vault.declarations_now().unwrap();
        let decl = declarations.get("log").unwrap();
        let snap = Repos::new(path).snapshot(Repo::Main).unwrap();
        vault.id_maps().find(&snap, decl, id).unwrap().is_some()
    }

    #[tokio::test]
    async fn an_absent_id_is_looked_for_only_in_the_segments_changed_since_the_map() {
        let scratch = Scratch::new("changed");
        let (path, vault) = scratch.vault().await;
        // A map true at HEAD that lacks `old`, which is in an unchanged
        // segment: a whole scan would find it, the map's rule does not.
        craft(&path, head(&path), &[]);
        assert!(!find(&vault, &path, "old"), "nothing changed since the map");
        // A segment of another writer, committed by hand.
        std::fs::write(path.join("log/00000099-zz.jsonl"), "fresh: {}\n").unwrap();
        fixture::commit_all(&path);
        assert!(find(&vault, &path, "fresh"), "in a changed segment");
        assert!(
            !find(&vault, &path, "old"),
            "the unchanged segment is not read"
        );
        // A map whose state is gone from the repository is no map to trust:
        // the journal is scanned whole.
        let unknown = State::parse(&format!("main {}", "ab".repeat(20))).unwrap();
        craft(&path, unknown, &[]);
        assert!(find(&vault, &path, "old"));
        assert!(find(&vault, &path, "fresh"));
        // So is a map that names a segment the commit does not hold.
        craft(&path, head(&path), &[("old", "log/00000042-zz.jsonl")]);
        assert!(find(&vault, &path, "old"));
    }

    #[tokio::test]
    async fn an_append_to_a_journal_with_a_map_reads_the_open_segment_and_no_other() {
        let scratch = Scratch::new("append-reads");
        let (path, vault) = scratch.segmented(52).await;
        let closed = segments(&path, &vault);
        assert!(closed >= 8, "{closed} segments");
        let (result, reads) = append(&vault, creation("fresh", false)).await;
        result.unwrap();
        // The size of the open segment says it is not full (no blob read);
        // one read gets its bytes to append to: the id was checked through
        // the map, which names every segment already, with no read.
        assert_eq!(reads, 1, "of {closed} segments");
        assert!(reads < closed as u64);
    }

    #[tokio::test]
    async fn an_id_the_map_names_or_a_segment_changed_since_it_holds_is_already_there() {
        let scratch = Scratch::new("append-duplicate");
        let (path, vault) = scratch.segmented(52).await;
        // In a closed segment the map names.
        let (result, _) = append(&vault, creation("l03", false)).await;
        assert_eq!(
            result.unwrap_err().code(),
            VaultErrorCode::AlreadyExists.into()
        );
        let (result, _) = append(&vault, creation("l03", true)).await;
        let kept = result.unwrap();
        assert_eq!(kept.document.raw.as_deref(), Some(&b"{}"[..]));
        assert_eq!(segments(&path, &vault), 11, "nothing was stored");
        // Appended after the map's state, in a segment changed since.
        let mut late = creation("late", false);
        late.raw = Some(b"{\"late\":1}".to_vec());
        append(&vault, late).await.0.unwrap();
        let (result, _) = append(&vault, creation("late", false)).await;
        assert_eq!(
            result.unwrap_err().code(),
            VaultErrorCode::AlreadyExists.into()
        );
        let (result, _) = append(&vault, creation("late", true)).await;
        assert_eq!(
            result.unwrap().document.raw.as_deref(),
            Some(&b"{\"late\":1}"[..])
        );
    }

    #[tokio::test]
    async fn a_journal_with_no_map_refuses_a_duplicate_as_well() {
        let scratch = Scratch::new("append-no-map");
        let (path, vault) = scratch.segmented(12).await;
        std::fs::remove_file(Index::new(&path).ids_file("log").unwrap()).unwrap();
        let (result, _) = append(&vault, creation("l01", false)).await;
        assert_eq!(
            result.unwrap_err().code(),
            VaultErrorCode::AlreadyExists.into()
        );
        let (result, _) = append(&vault, creation("l11", true)).await;
        assert!(result.unwrap().document.raw.is_some());
        append(&vault, creation("fresh", false)).await.0.unwrap();
        // A map whose state is gone from the repository is no map either.
        let unknown = State::parse(&format!("main {}", "ab".repeat(20))).unwrap();
        craft(&path, unknown, &[]);
        let (result, _) = append(&vault, creation("fresh", false)).await;
        assert_eq!(
            result.unwrap_err().code(),
            VaultErrorCode::AlreadyExists.into()
        );
        // A map that is wrong about an id falls back to a scan.
        craft(&path, head(&path), &[("l02", "log/00000042-zz.jsonl")]);
        let (result, _) = append(&vault, creation("l02", false)).await;
        assert_eq!(
            result.unwrap_err().code(),
            VaultErrorCode::AlreadyExists.into()
        );
    }

    #[tokio::test]
    async fn a_lookup_with_no_map_reads_the_segments_only_as_far_as_its_id_and_each_once() {
        let scratch = Scratch::new("lookup-no-map");
        // Six segments of five lines.
        let (path, vault) = scratch.segmented(30).await;
        std::fs::remove_file(Index::new(&path).ids_file("log").unwrap()).unwrap();
        step(&vault, move |_cx, vault| {
            let declarations = vault.declarations_now()?;
            let decl = declarations.get("log").unwrap();
            let snap = Repos::new(&path).snapshot(Repo::Main)?;
            let mut lookup = vault.id_maps().lookup(&snap, decl);
            // The count of blob reads is of one thread: the whole check is
            // one step.
            let before = blob_reads();
            assert_eq!(lookup.find("l01")?.unwrap().id, "l01");
            assert_eq!(blob_reads() - before, 1, "the first segment holds it");
            let before = blob_reads();
            assert_eq!(lookup.find("l12")?.unwrap().id, "l12");
            assert_eq!(blob_reads() - before, 2, "the second and third segments");
            let before = blob_reads();
            assert_eq!(lookup.find("l03")?.unwrap().id, "l03");
            assert_eq!(blob_reads() - before, 1, "the segment read before, again");
            let before = blob_reads();
            assert!(lookup.find("absent")?.is_none());
            assert_eq!(blob_reads() - before, 3, "the three segments not read yet");
            let before = blob_reads();
            assert!(lookup.find("absent")?.is_none());
            assert_eq!(blob_reads() - before, 0, "every segment was read once");
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_journal_with_no_cap_rotates_at_one_mebibyte() {
        let scratch = Scratch::new("default-cap");
        let (path, vault) = scratch.vault().await;
        let big = |id: &str| {
            let mut request = creation(id, false);
            request.raw = Some(format!("{{\"p\":\"{}\"}}", "x".repeat(300_000)).into_bytes());
            request
        };
        // `old` and three lines of 300 KB fit in one MiB, the next does
        // not: it opens the second segment.
        for id in ["b1", "b2", "b3"] {
            append(&vault, big(id)).await.0.unwrap();
        }
        assert_eq!(segments(&path, &vault), 1);
        append(&vault, big("b4")).await.0.unwrap();
        assert_eq!(segments(&path, &vault), 2);
        // A segment over the cap, however it came to be, is closed by the
        // next append, which opens a new one.
        std::fs::write(
            path.join(format!("log/00000009-{}.jsonl", vault.writer())),
            format!("big: {}\n", "x".repeat(1_100_000)),
        )
        .unwrap();
        fixture::commit_all(&path);
        append(&vault, creation("after", false)).await.0.unwrap();
        assert_eq!(segments(&path, &vault), 4, "`after` opened a new segment");
    }

    #[tokio::test]
    async fn the_check_lists_a_broken_map_a_map_outside_its_journal_and_a_map_that_lies() {
        let scratch = Scratch::new("check");
        let (path, vault) = scratch.vault().await;
        let check = || {
            let views = Repos::new(&path).views().unwrap();
            let declarations = vault.declarations_now().unwrap();
            check_journal_id_maps(&Index::new(&path), &views, &declarations).unwrap()
        };
        assert!(check().is_empty(), "the map the indexer made is sound");
        let segment = std::fs::read_dir(path.join("log"))
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().ends_with(".jsonl"))
            .map(|entry| format!("log/{}", entry.file_name().to_string_lossy()))
            .unwrap();
        let file = ".scheda/index/journal/0/ids";

        std::fs::write(Index::new(&path).ids_file("log").unwrap(), "damaged").unwrap();
        let problems = check();
        assert_eq!(problems.len(), 1);
        assert_eq!(
            problems[0].code.and_then(|code| code.as_known()),
            Some(VaultErrorCode::IndexBroken)
        );
        assert_eq!(problems[0].path.as_deref(), Some(file));

        craft(&path, head(&path), &[("old", "notes/elsewhere.jsonl")]);
        let problems = check();
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0]
                .message
                .as_deref()
                .unwrap()
                .contains("elsewhere")
        );

        craft(&path, head(&path), &[("other", &segment)]);
        let problems = check();
        assert_eq!(
            problems.len(),
            1,
            "a map at HEAD that differs from its segments"
        );

        // A map at an earlier state is not compared: the indexer is behind.
        let earlier = State::parse(&format!("main {}", "ab".repeat(20))).unwrap();
        craft(&path, earlier, &[("other", &segment)]);
        assert!(check().is_empty());
    }

    #[tokio::test]
    async fn the_map_of_a_collection_that_is_not_a_journal_is_removed_by_a_round() {
        let scratch = Scratch::new("stale");
        let vault = open(&scratch.0, OpenOptions::default().with_index_poll_ms(10))
            .await
            .unwrap();
        let path = crate::fs::canonicalize(&scratch.0).unwrap();
        let ghost = path.join(".scheda/index/journal/ghost");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !Index::new(&path).root().join("commit").exists() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        std::fs::create_dir_all(&ghost).unwrap();
        std::fs::write(ghost.join("ids"), "x").unwrap();
        create_collection(
            &vault,
            serde_json::json!({"name": "log", "schema": {"journal": {}}}),
        )
        .await
        .unwrap();
        while ghost.exists() {
            assert!(Instant::now() < deadline, "the stale map stays");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn a_map_round_trips_and_writes_only_the_segments_it_uses() {
        let mut map = IdMap::new(State::default());
        let a = map.intern("log/00000001-w.jsonl");
        let _unused = map.intern("log/00000002-w.jsonl");
        let c = map.intern("log/00000003-w.jsonl");
        map.ids.insert("one".to_owned(), a);
        map.ids.insert("two".to_owned(), c);
        let bytes = map.encode().unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(!text.contains("00000002"), "{text}");
        let again = IdMap::parse(&bytes).unwrap();
        assert_eq!(again.segment_of("one"), Some("log/00000001-w.jsonl"));
        assert_eq!(again.segment_of("two"), Some("log/00000003-w.jsonl"));
        assert_eq!(again.segment_of("three"), None);
        assert_eq!(again.state, State::default());
    }

    #[tokio::test]
    async fn bytes_that_are_not_a_map_are_index_broken() {
        for bytes in [
            &b""[..],
            b"scheda-ids 2\n",
            b"scheda-ids 1\nmain -\n1\n\"a\"\nx\t5\n",
            b"scheda-ids 1\nmain -\n1\nnot json\n",
            b"scheda-ids 1\nmain -\n0\nno-tab\n",
            &[0xff, 0xfe],
        ] {
            let error = IdMap::parse(bytes).unwrap_err();
            assert_eq!(error.vault_code(), VaultErrorCode::IndexBroken);
        }
    }

    #[tokio::test]
    async fn a_segment_belongs_to_a_collection_by_its_path() {
        assert!(in_collection("log/00000001-w.jsonl", "log/"));
        assert!(!in_collection("log/sub/00000001-w.jsonl", "log/"));
        assert!(!in_collection("other/00000001-w.jsonl", "log/"));
        assert!(!in_collection("log/.hidden.jsonl", "log/"));
        assert!(!in_collection("log/notes.md", "log/"));
    }
}
