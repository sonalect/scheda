//! Background indexer: one thread per Vault handle that follows git `HEAD`.
//!
//! The thread wakes when this handle commits, and otherwise every poll
//! interval. Each round it reads `HEAD` of the Vault (and of the nested
//! repository under `{vault}/.isolated/`), compares it with the commit each
//! Index leg has reached (`<gen>/marks/`), and applies the diff in batches.
//! A leg's mark moves only after its writes are durable, so a round that
//! stops halfway is repeated, and repeating it is harmless.
//!
//! One process indexes a work tree at a time: the lexical leg belongs to
//! whoever holds `{vault}/.scheda/index.lock`, the vector leg to whoever
//! holds `{vault}/.scheda/vector.lock` — only a handle whose embedder
//! matches the generation's `model_id` and dimensions takes that one. Both
//! are `try_lock`; a handle that loses keeps polling and takes the role
//! when the holder goes away. The indexer never takes `write.lock`.
//!
//! The thread holds no reference to the handle. Dropping the last handle
//! (the `Vault` and every `Collection` made from it) stops and joins it;
//! the stop flag is checked between batches.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, TryLockError};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::diff::{self, Change, RecordChange};
use crate::error::{Code, ContextIo, io_code};
use crate::git::{self, tree};
use crate::index::{self, IndexHandle, JournalIdMaps};
use crate::isolated;
use crate::journal;
use crate::marks::{self, Leg, Repo};
use crate::schema::{self, CollectionDecl, Decls, Kind};
use crate::search::{Legs, SearchEngine};
use crate::store;
use crate::{Embedder, Error, Projector, Record};

/// Default interval between two reads of `HEAD`.
pub(crate) const DEFAULT_POLL: Duration = Duration::from_millis(1000);
/// Records per lexical batch. The vector leg uses the embedder's
/// `many_limit` instead.
const LEXICAL_BATCH: usize = 256;
const BACKOFF_FIRST: Duration = Duration::from_millis(100);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
const INDEX_LOCK: &str = "index.lock";
const VECTOR_LOCK: &str = "vector.lock";

thread_local! {
    static START_ON_OPEN: Cell<bool> = const { Cell::new(false) };
}

/// Whether `open` on this thread starts an indexer. Off while writes still
/// update the Index inline; a test turns it on for its own thread.
pub(crate) fn start_on_open() -> bool {
    START_ON_OPEN.with(Cell::get)
}

/// Restores the previous [`start_on_open`] value when dropped.
#[cfg(test)]
pub(crate) struct StartOnOpen(bool);

#[cfg(test)]
pub(crate) fn set_start_on_open(on: bool) -> StartOnOpen {
    StartOnOpen(START_ON_OPEN.with(|cell| cell.replace(on)))
}

#[cfg(test)]
impl Drop for StartOnOpen {
    fn drop(&mut self) {
        START_ON_OPEN.with(|cell| cell.set(self.0));
    }
}

/// What the thread needs from the handle. Shared state only: nothing here
/// keeps the handle itself alive.
pub(crate) struct Context {
    pub(crate) vault: PathBuf,
    pub(crate) index: Arc<IndexHandle>,
    pub(crate) search: Arc<SearchEngine>,
    pub(crate) journal_ids: Arc<Mutex<JournalIdMaps>>,
    pub(crate) embedder: Arc<Mutex<Option<Arc<dyn Embedder>>>>,
    pub(crate) projector: Option<Arc<dyn Projector>>,
    pub(crate) poll: Duration,
}

/// Where the Index stands against git `HEAD`, as one handle sees it.
///
/// Marks come from the live generation on disk, so they include what an
/// indexer in another process wrote. Fields are added as the indexer
/// grows; read them by name.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct IndexStatus {
    /// `HEAD` of the Vault work tree. `None` before the first commit.
    pub head: Option<String>,
    /// `HEAD` of the nested repository under `{vault}/.isolated/`, when
    /// there is one with a commit.
    pub isolated_head: Option<String>,
    /// Commit the lexical leg has reached for the Vault work tree.
    pub lexical: Option<String>,
    /// Commit the vector leg has reached for the Vault work tree.
    pub vector: Option<String>,
    /// Commit the lexical leg has reached for the nested repository.
    pub isolated_lexical: Option<String>,
    /// Commit the vector leg has reached for the nested repository.
    pub isolated_vector: Option<String>,
    /// This handle indexes the lexical leg (it holds
    /// `{vault}/.scheda/index.lock`).
    pub lexical_role: bool,
    /// This handle indexes the vector leg (it holds
    /// `{vault}/.scheda/vector.lock`).
    pub vector_role: bool,
    /// Failure of this handle's last indexing round; cleared by the next
    /// round that succeeds. A failed round changes no mark and fails no
    /// write.
    pub last_error: Option<Error>,
}

/// What the thread last reported.
#[derive(Clone, Debug, Default)]
pub(crate) struct State {
    /// This handle holds `index.lock`.
    pub(crate) lexical_role: bool,
    /// This handle holds `vector.lock`.
    pub(crate) vector_role: bool,
    /// Failure of the last round; cleared by a round that succeeds.
    pub(crate) last_error: Option<Error>,
    /// Rounds finished since start.
    pub(crate) rounds: u64,
}

#[derive(Default)]
struct Signal {
    wake: bool,
    stop: bool,
}

struct Shared {
    ctx: Context,
    signal: Mutex<Signal>,
    wake: Condvar,
    state: Mutex<State>,
    changed: Condvar,
    #[cfg(test)]
    tid: Mutex<Option<String>>,
}

impl Shared {
    fn stopping(&self) -> bool {
        lock(&self.signal).stop
    }
}

/// Owner of the indexer thread. Dropping it stops and joins the thread.
pub(crate) struct Indexer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Indexer {
    /// Start the thread. Its first round runs at once: catch-up on `open`.
    pub(crate) fn start(ctx: Context) -> Result<Self, Error> {
        let shared = Arc::new(Shared {
            ctx,
            signal: Mutex::new(Signal {
                wake: true,
                stop: false,
            }),
            wake: Condvar::new(),
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            #[cfg(test)]
            tid: Mutex::new(None),
        });
        let worker = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name(String::from("scheda-indexer"))
            .spawn(move || run(&worker))
            .context("[Indexer::start] spawn thread")?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Ask for a round now instead of at the next poll.
    pub(crate) fn wake(&self) {
        lock(&self.shared.signal).wake = true;
        self.shared.wake.notify_one();
    }

    /// Latest report of the thread.
    pub(crate) fn state(&self) -> State {
        lock(&self.shared.state).clone()
    }

    /// Wait until the thread finishes a round after `after`, or `timeout`.
    pub(crate) fn wait_round(&self, after: u64, timeout: Duration) {
        let state = lock(&self.shared.state);
        if state.rounds > after {
            return;
        }
        let _ = self
            .shared
            .changed
            .wait_timeout_while(state, timeout, |state| state.rounds <= after);
    }

    /// `/proc` task path of the thread, once it has started.
    #[cfg(test)]
    pub(crate) fn task_path(&self) -> Option<String> {
        lock(&self.shared.tid).clone()
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        lock(&self.shared.signal).stop = true;
        self.shared.wake.notify_one();
        if let Some(thread) = self.thread.take()
            && thread.thread().id() != thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn run(shared: &Shared) {
    #[cfg(test)]
    {
        *lock(&shared.tid) = fs::read_link("/proc/thread-self")
            .ok()
            .map(|path| format!("/proc/{}", path.display()));
    }
    let mut worker = Worker::default();
    let mut backoff: Option<Duration> = None;
    loop {
        {
            let mut signal = lock(&shared.signal);
            if !signal.wake && !signal.stop {
                let wait = backoff.unwrap_or(shared.ctx.poll);
                signal = match shared.wake.wait_timeout(signal, wait) {
                    Ok((guard, _)) => guard,
                    Err(poisoned) => poisoned.into_inner().0,
                };
            }
            if signal.stop {
                break;
            }
            signal.wake = false;
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| worker.round(shared)));
        let error = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(err)) => Some(err),
            Err(_) => Some(Error::new(
                worker.leg_code(),
                "a caller callback panicked inside the background indexer",
            )),
        };
        backoff = match (&error, backoff) {
            (None, _) => None,
            (Some(_), None) => Some(BACKOFF_FIRST),
            (Some(_), Some(last)) => Some(last.saturating_mul(2).min(BACKOFF_CAP)),
        };
        let mut state = lock(&shared.state);
        state.rounds = state.rounds.saturating_add(1);
        state.last_error = error;
        state.lexical_role = worker.lexical.is_some();
        state.vector_role = worker.vector.is_some();
        drop(state);
        shared.changed.notify_all();
    }
    worker.lexical = None;
    worker.vector = None;
    let mut state = lock(&shared.state);
    state.lexical_role = false;
    state.vector_role = false;
    drop(state);
    shared.changed.notify_all();
}

/// How a leg's pass ended.
#[derive(Debug, Eq, PartialEq)]
enum Pass {
    Done,
    /// The handle is dropping: leave the mark where it is.
    Stopped,
    /// The mark's commit is gone: rebuild the leg.
    Rebuild,
}

/// `HEAD` of the Vault and, when it is a work tree, of the nested repository.
struct Heads {
    main: (PathBuf, String),
    nested: Option<(PathBuf, String)>,
}

impl Heads {
    fn of(&self, repo: Repo) -> Option<(&Path, &str)> {
        match repo {
            Repo::Main => Some((self.main.0.as_path(), self.main.1.as_str())),
            Repo::Isolated => self
                .nested
                .as_ref()
                .map(|(dir, head)| (dir.as_path(), head.as_str())),
        }
    }

    fn for_decl(&self, decl: &CollectionDecl) -> Option<(&Path, &str)> {
        self.of(if decl.isolated {
            Repo::Isolated
        } else {
            Repo::Main
        })
    }
}

#[derive(Default)]
struct Worker {
    lexical: Option<File>,
    vector: Option<File>,
    /// Leg being applied, for the code of a caller panic.
    in_vector: bool,
}

impl Worker {
    fn leg_code(&self) -> Code {
        if self.in_vector {
            Code::IndexVector
        } else {
            Code::IndexLexical
        }
    }

    fn round(&mut self, shared: &Shared) -> Result<(), Error> {
        let ctx = &shared.ctx;
        self.in_vector = false;
        follow_current(ctx)?;
        let embedder = lock(&ctx.embedder).clone();
        let generation = ctx.index.dir()?;
        self.update_roles(ctx, &generation, embedder.as_deref())?;
        if self.lexical.is_none() && self.vector.is_none() {
            return Ok(());
        }
        ctx.search.enable_write()?;
        let result = self.catch_up(shared, &generation, embedder.as_deref());
        let released = ctx.search.release_writer();
        result.and(released)
    }

    fn update_roles(
        &mut self,
        ctx: &Context,
        generation: &Path,
        embedder: Option<&dyn Embedder>,
    ) -> Result<(), Error> {
        if self.lexical.is_none() {
            self.lexical = try_role(&ctx.vault, INDEX_LOCK)?;
        }
        if vector_allowed(generation, embedder)? {
            if self.vector.is_none() {
                self.vector = try_role(&ctx.vault, VECTOR_LOCK)?;
            }
        } else {
            self.vector = None;
        }
        Ok(())
    }

    fn catch_up(
        &mut self,
        shared: &Shared,
        generation: &Path,
        embedder: Option<&dyn Embedder>,
    ) -> Result<(), Error> {
        let ctx = &shared.ctx;
        let Some(head) = git::peel_head_oid(&ctx.vault)? else {
            return Ok(());
        };
        let nested_dir = isolated::root(&ctx.vault);
        let nested = if git::is_work_tree(&nested_dir) {
            git::peel_head_oid(&nested_dir)?.map(|head| (nested_dir, head))
        } else {
            None
        };
        let heads = Heads {
            main: (ctx.vault.clone(), head),
            nested,
        };
        if self.lexical.is_some() && needs_new_generation(generation)? {
            return self.rebuild_generation(shared, &heads, embedder);
        }
        let decls = schema::load_at(&ctx.vault, &heads.main.1)?;
        let job = Job {
            shared,
            generation,
            decls: &decls,
            heads: &heads,
            embedder,
        };
        if self.lexical.is_some() {
            for repo in [Repo::Main, Repo::Isolated] {
                match job.apply_leg(repo, Leg::Lexical)? {
                    Pass::Done => {}
                    Pass::Stopped => return Ok(()),
                    Pass::Rebuild => return self.rebuild_generation(shared, &heads, embedder),
                }
            }
        }
        if self.vector.is_some() && embedder.is_some() {
            self.in_vector = true;
            for repo in [Repo::Main, Repo::Isolated] {
                let mut pass = job.apply_leg(repo, Leg::Vector)?;
                if pass == Pass::Rebuild {
                    // The vector leg may live in another process than the
                    // lexical one, so it cannot publish a generation:
                    // refill it in place from the empty tree.
                    pass = job.apply_leg_from(repo, Leg::Vector, None)?;
                }
                if pass == Pass::Stopped {
                    return Ok(());
                }
            }
            self.in_vector = false;
        }
        Ok(())
    }

    /// Build a new generation from the commits and publish it. Runs when
    /// the live one has another format or its lexical mark cannot be
    /// diffed. Holds `reindex.lock` so no `open` prunes the unfinished
    /// directory; a `reindex` already running wins and this round does
    /// nothing.
    fn rebuild_generation(
        &self,
        shared: &Shared,
        heads: &Heads,
        embedder: Option<&dyn Embedder>,
    ) -> Result<(), Error> {
        let ctx = &shared.ctx;
        let Some(_rebuild) = crate::reindex::try_lock(&ctx.vault)? else {
            return Ok(());
        };
        let previous = ctx.index.dir()?;
        let new_dir = index::start_generation(&ctx.index.root)?;
        #[cfg(feature = "search")]
        crate::search::copy_vector_dump_as_is(&previous, &new_dir)?;
        #[cfg(not(feature = "search"))]
        let _ = previous;
        let engine = SearchEngine::open(&new_dir, true, ctx.projector.clone())?;
        let decls = schema::load_at(&ctx.vault, &heads.main.1)?;
        engine.init_generation(&decls)?;
        let vector = self.vector.is_some() && embedder.is_some();
        let legs = Legs {
            lexical: true,
            vector,
        };
        let embedder = if vector { embedder } else { None };
        let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut maps = JournalIdMaps::default();
        for decl in decls.collections().filter(|decl| !decl.encrypted) {
            let Some((dir, head)) = heads.for_decl(decl) else {
                if decl.kind == Kind::Journal {
                    index::rewrite_collection(&new_dir, &mut maps, &decl.name, BTreeMap::new())?;
                }
                continue;
            };
            let records = diff::collection(dir, decl, head)?;
            if decl.kind == Kind::Journal {
                index::rewrite_collection(
                    &new_dir,
                    &mut maps,
                    &decl.name,
                    journal_ids(decl, &records),
                )?;
            }
            seen.insert(
                decl.name.clone(),
                records.iter().map(|change| change.id.clone()).collect(),
            );
            if apply_changes(shared, &engine, &decls, dir, &records, legs, embedder)?
                == Pass::Stopped
            {
                return Ok(());
            }
        }
        #[cfg(feature = "search")]
        if vector {
            engine.prune_vectors(&seen, None)?;
        }
        #[cfg(not(feature = "search"))]
        let _ = &seen;
        engine.flush()?;
        engine.release_writer()?;
        for repo in [Repo::Main, Repo::Isolated] {
            let Some((_, head)) = heads.of(repo) else {
                continue;
            };
            marks::write(&new_dir, Leg::Lexical, repo, head)?;
            marks::write(&new_dir, Leg::Ids, repo, head)?;
            if vector {
                marks::write(&new_dir, Leg::Vector, repo, head)?;
            }
        }
        index::publish_generation(&ctx.index.root, &new_dir)?;
        switch_to(ctx, new_dir, engine, maps)
    }
}

/// Inputs of one round, shared by every leg and repository it applies.
struct Job<'a> {
    shared: &'a Shared,
    generation: &'a Path,
    decls: &'a Decls,
    heads: &'a Heads,
    embedder: Option<&'a dyn Embedder>,
}

impl Job<'_> {
    fn apply_leg(&self, repo: Repo, leg: Leg) -> Result<Pass, Error> {
        let Some((_, head)) = self.heads.of(repo) else {
            return Ok(Pass::Done);
        };
        let mark = marks::read(self.generation, leg, repo)?;
        if mark.as_deref() == Some(head) {
            return Ok(Pass::Done);
        }
        if leg == Leg::Lexical && mark.is_none() && self.generation.join("lexical").is_dir() {
            // Documents of an unknown commit: rebuild rather than guess
            // which of them are stale.
            return Ok(Pass::Rebuild);
        }
        self.apply_leg_from(repo, leg, mark.as_deref())
    }

    /// Apply `from` → `HEAD` of `repo` to `leg` of the live generation, then
    /// move that leg's mark. `from` `None` fills the leg from nothing.
    fn apply_leg_from(&self, repo: Repo, leg: Leg, from: Option<&str>) -> Result<Pass, Error> {
        let Some((dir, head)) = self.heads.of(repo) else {
            return Ok(Pass::Done);
        };
        let (shared, decls, heads) = (self.shared, self.decls, self.heads);
        let embedder = if leg == Leg::Vector {
            self.embedder
        } else {
            None
        };
        let engine = &shared.ctx.search;
        let legs = legs_of(leg);
        let found = diff::diff(dir, repo, decls, from, head)?;
        if found.rebuild {
            return Ok(Pass::Rebuild);
        }
        let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for name in &found.whole {
            let decl = decls.get(name).filter(|decl| leg_wanted(decl, leg));
            let records = match (decl, decl.and_then(|decl| heads.for_decl(decl))) {
                (Some(decl), Some((decl_dir, decl_head))) => {
                    Some((decl, decl_dir, diff::collection(decl_dir, decl, decl_head)?))
                }
                _ => None,
            };
            let Some((decl, decl_dir, records)) = records else {
                engine.forget_collection(name, legs)?;
                continue;
            };
            if leg == Leg::Lexical {
                engine.forget_collection(name, legs)?;
            } else {
                seen.insert(
                    name.clone(),
                    records.iter().map(|change| change.id.clone()).collect(),
                );
            }
            if apply_changes(shared, engine, decls, decl_dir, &records, legs, embedder)?
                == Pass::Stopped
            {
                return Ok(Pass::Stopped);
            }
            #[cfg(feature = "search")]
            if leg == Leg::Vector {
                engine.prune_vectors(&seen, Some(decl.name.as_str()))?;
            }
            #[cfg(not(feature = "search"))]
            let _ = decl;
        }
        #[cfg(feature = "search")]
        if leg == Leg::Vector && from.is_none() && repo == Repo::Main {
            // Filled from nothing: a collection the commit no longer
            // declares keeps no slot.
            for decl in decls.collections() {
                seen.entry(decl.name.clone()).or_default();
            }
            engine.prune_vectors(&seen, None)?;
        }
        #[cfg(not(feature = "search"))]
        let _ = &seen;
        if apply_changes(shared, engine, decls, dir, &found.changes, legs, embedder)?
            == Pass::Stopped
        {
            return Ok(Pass::Stopped);
        }
        engine.flush()?;
        marks::write(self.generation, leg, repo, head)?;
        Ok(Pass::Done)
    }
}

/// Bind the handle to the generation `current` names when another handle
/// or process published a new one.
fn follow_current(ctx: &Context) -> Result<(), Error> {
    let Some(live) = index::current_dir(&ctx.index.root)? else {
        return Ok(());
    };
    if live == ctx.index.dir()? || !live.is_dir() {
        return Ok(());
    }
    let engine = SearchEngine::open(&live, false, ctx.projector.clone())?;
    let decls = schema::load(&ctx.vault)?;
    let maps = index::load_journal_ids(&live, &decls)?;
    switch_to(ctx, live, engine, maps)
}

fn switch_to(
    ctx: &Context,
    dir: PathBuf,
    engine: SearchEngine,
    maps: JournalIdMaps,
) -> Result<(), Error> {
    ctx.search.adopt(engine)?;
    ctx.index.set_dir(dir)?;
    *lock(&ctx.journal_ids) = maps;
    Ok(())
}

/// A generation with a `format` file other than the current one, or the
/// legacy layout directly under `index/`.
fn needs_new_generation(generation: &Path) -> Result<bool, Error> {
    Ok(matches!(index::format_version(generation)?, Some(v) if v != index::FORMAT_VERSION))
}

/// Whether `embedder` may write the vector leg of `generation`.
pub(crate) fn vector_allowed(
    generation: &Path,
    embedder: Option<&dyn Embedder>,
) -> Result<bool, Error> {
    #[cfg(feature = "search")]
    {
        match embedder {
            Some(embedder) => crate::search::vector_model_matches(generation, embedder),
            None => Ok(false),
        }
    }
    #[cfg(not(feature = "search"))]
    {
        let _ = (generation, embedder);
        Ok(false)
    }
}

const fn legs_of(leg: Leg) -> Legs {
    Legs {
        lexical: matches!(leg, Leg::Lexical),
        vector: matches!(leg, Leg::Vector),
    }
}

fn leg_wanted(decl: &CollectionDecl, leg: Leg) -> bool {
    match leg {
        Leg::Lexical => decl.lexical_indexed(),
        Leg::Vector => decl.vector_indexed(),
        Leg::Ids => decl.kind == Kind::Journal && !decl.encrypted,
    }
}

/// `id` → vault-relative segment path of every line in `records`.
fn journal_ids(decl: &CollectionDecl, records: &[RecordChange]) -> BTreeMap<String, String> {
    records
        .iter()
        .filter_map(|change| match &change.change {
            Change::Upsert { path, .. } if decl.isolated => {
                Some((change.id.clone(), format!("{}/{path}", isolated::DIR)))
            }
            Change::Upsert { path, .. } => Some((change.id.clone(), path.clone())),
            Change::Delete => None,
        })
        .collect()
}

/// Apply `changes`, read from the repository at `dir`, to `engine` in
/// batches; commit after each batch and stop between batches when the
/// handle drops.
fn apply_changes(
    shared: &Shared,
    engine: &SearchEngine,
    decls: &Decls,
    dir: &Path,
    changes: &[RecordChange],
    legs: Legs,
    embedder: Option<&dyn Embedder>,
) -> Result<Pass, Error> {
    let batch = match embedder {
        Some(embedder) if legs.vector => embedder.many_limit().get(),
        _ => LEXICAL_BATCH,
    };
    for chunk in changes.chunks(batch) {
        if shared.stopping() {
            return Ok(Pass::Stopped);
        }
        let blobs: Vec<String> = chunk
            .iter()
            .filter_map(|change| match &change.change {
                Change::Upsert { blob, .. } => Some(blob.clone()),
                Change::Delete => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let bytes: BTreeMap<String, Vec<u8>> = blobs
            .iter()
            .cloned()
            .zip(tree::read_blobs(dir, &blobs)?)
            .filter_map(|(oid, bytes)| bytes.map(|bytes| (oid, bytes)))
            .collect();
        let mut segments: BTreeMap<&str, BTreeMap<String, (usize, Vec<u8>)>> = BTreeMap::new();
        for change in chunk {
            let Some(decl) = decls.get(&change.collection) else {
                continue;
            };
            match &change.change {
                Change::Delete => engine.forget_legs(decl, &change.id, legs)?,
                Change::Upsert { blob, .. } => {
                    let Some(file) = bytes.get(blob) else {
                        continue;
                    };
                    let raw = if decl.kind == Kind::Journal {
                        let lines = segments
                            .entry(blob.as_str())
                            .or_insert_with(|| journal::segment_lines(file));
                        match lines.get(&change.id) {
                            Some((_, payload)) => payload.clone(),
                            None => continue,
                        }
                    } else {
                        file.clone()
                    };
                    let record = Record::default()
                        .with_collection(decl.name.as_str())
                        .with_id(change.id.as_str())
                        .with_raw(raw);
                    engine.upsert_legs(decl, &record, embedder, legs)?;
                }
            }
        }
        engine.flush()?;
    }
    Ok(Pass::Done)
}

/// Take `{vault}/.scheda/{name}` if no other handle holds it.
fn try_role(vault: &Path, name: &str) -> Result<Option<File>, Error> {
    let scheda = vault.join(".scheda");
    fs::create_dir_all(&scheda).context("[Indexer] create .scheda")?;
    store::restrict_dir(&scheda)?;
    let path = scheda.join(name);
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .context("[Indexer] open role lock")?;
    store::restrict_file(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => {
            Err(Error::new(io_code(&err), err.to_string()).context("[Indexer] lock role lock"))
        }
    }
}

/// Whether some handle on this work tree, in any process, holds the role.
pub(crate) fn role_taken(vault: &Path, vector: bool) -> Result<bool, Error> {
    let name = if vector { VECTOR_LOCK } else { INDEX_LOCK };
    Ok(try_role(vault, name)?.is_none())
}
