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

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, TryLockError};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::diff::{self, Change, RecordChange};
use crate::error::{Code, Context as _, ContextIo, io_code};
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
pub(crate) const BACKOFF_FIRST: Duration = Duration::from_millis(100);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
const INDEX_LOCK: &str = "index.lock";
const VECTOR_LOCK: &str = "vector.lock";

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
    /// Commit the journal `id` maps have reached for the Vault work tree.
    /// Writes add their own lines at once; this mark covers lines that
    /// reached `HEAD` another way (`pull`, a `git` commit, another process).
    pub journal_ids: Option<String>,
    /// Commit the journal `id` maps have reached for the nested repository.
    pub isolated_journal_ids: Option<String>,
    /// Records the lexical leg has yet to apply to reach `head` and
    /// `isolated_head`, both repositories together: added, changed, and
    /// removed Records of lexically indexed collections, and every Record
    /// of a collection whose declaration changed. A leg that must be
    /// rebuilt counts every Record. `Some(0)` when the leg is at `HEAD`;
    /// `None` when the Records could not be counted, for example because
    /// the declarations at `HEAD` do not load (the indexer then reports the
    /// same in `last_error`).
    pub lexical_pending: Option<u64>,
    /// The same count for the vector leg. It stays above `0` while no
    /// handle can write vectors (no embedder, or only ones of another
    /// model): [`Vault::wait_indexed`](crate::Vault::wait_indexed) does not
    /// wait for such a leg, and this is where its lag shows.
    pub vector_pending: Option<u64>,
    /// This handle's embedder has another `model_id` or dimensions than
    /// the vectors of the live generation. It does not write the vector
    /// leg, and its `search` runs without that leg, rather than mix two
    /// vector spaces. `reindex` moves the Index to the new model.
    pub model_mismatch: bool,
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
    /// Something changed (a commit, a new embedder, a published
    /// generation): run a round now, even while a failed round waits out
    /// its backoff.
    wake: bool,
    /// Someone waits for the Index: run a round now unless a failure holds
    /// the indexer back.
    nudge: bool,
    /// The embedder changed: a vector failure of the old one holds nothing.
    new_embedder: bool,
    stop: bool,
    /// Callers that asked the current round to end early (see
    /// [`Indexer::pause`]).
    pause: u32,
}

struct Shared {
    ctx: Context,
    signal: Mutex<Signal>,
    /// Held by the thread for the length of one round.
    busy: Mutex<()>,
    wake: Condvar,
    state: Mutex<State>,
    changed: Condvar,
    #[cfg(test)]
    tid: Mutex<Option<String>>,
}

impl Shared {
    /// Whether the current round should end at the next batch boundary:
    /// the handle is dropping, or someone is switching its generation.
    fn stopping(&self) -> bool {
        let signal = lock(&self.signal);
        signal.stop || signal.pause > 0
    }
}

/// Owner of the indexer thread. Dropping it stops and joins the thread.
pub(crate) struct Indexer {
    shared: Arc<Shared>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Indexer {
    /// Start the thread. Its first round runs at once: catch-up on `open`.
    pub(crate) fn start(ctx: Context) -> Result<Self, Error> {
        let shared = Arc::new(Shared {
            ctx,
            signal: Mutex::new(Signal {
                wake: true,
                ..Signal::default()
            }),
            busy: Mutex::new(()),
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
            thread: Mutex::new(Some(thread)),
        })
    }

    /// Ask for a round now instead of at the next poll: something the
    /// Index follows changed.
    pub(crate) fn wake(&self) {
        lock(&self.shared.signal).wake = true;
        self.shared.wake.notify_one();
    }

    /// Ask for a round now on behalf of a caller that waits for the Index.
    /// Unlike [`wake`](Self::wake) it does not cut a backoff short, so a
    /// waiting caller does not retry a failing embedder in a tight loop.
    pub(crate) fn nudge(&self) {
        lock(&self.shared.signal).nudge = true;
        self.shared.wake.notify_one();
    }

    /// The handle's embedder changed: retry the vector leg now.
    pub(crate) fn embedder_changed(&self) {
        {
            let mut signal = lock(&self.shared.signal);
            signal.new_embedder = true;
            signal.wake = true;
        }
        self.shared.wake.notify_one();
    }

    /// End the current round at its next batch boundary and keep the next
    /// one from starting until the guard drops. The caller then swaps the
    /// handle's generation without a round still writing the old one's
    /// diff into it. The round leaves its marks where they were.
    pub(crate) fn pause(&self) -> Paused<'_> {
        lock(&self.shared.signal).pause += 1;
        Paused {
            indexer: self,
            _busy: Some(lock(&self.shared.busy)),
        }
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

impl Indexer {
    /// Stop the thread and wait for it. It finishes the Record it is
    /// indexing (at most one embedder call) and leaves every mark where
    /// its last durable write put it. Later calls return at once.
    pub(crate) fn shutdown(&self) -> Result<(), Error> {
        lock(&self.shared.signal).stop = true;
        self.shared.wake.notify_one();
        let Some(thread) = lock(&self.thread).take() else {
            return Ok(());
        };
        if thread.thread().id() == thread::current().id() {
            return Ok(());
        }
        thread.join().map_err(|_| {
            Error::new(Code::IndexLexical, "the background indexer thread panicked")
                .context("[Indexer::shutdown] join thread")
        })
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Returned by [`Indexer::pause`]; dropping it lets the indexer run again
/// and wakes it.
pub(crate) struct Paused<'a> {
    indexer: &'a Indexer,
    _busy: Option<MutexGuard<'a, ()>>,
}

impl Drop for Paused<'_> {
    fn drop(&mut self) {
        self._busy = None;
        {
            let mut signal = lock(&self.indexer.shared.signal);
            signal.pause = signal.pause.saturating_sub(1);
            signal.wake = true;
        }
        self.indexer.shared.wake.notify_one();
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
    let mut retry = Retry::default();
    let mut next_poll = Instant::now();
    loop {
        {
            let mut signal = lock(&shared.signal);
            loop {
                if signal.stop {
                    break;
                }
                if std::mem::take(&mut signal.new_embedder) {
                    retry.vector = Backoff::default();
                }
                let now = Instant::now();
                let held = retry.round.until().filter(|until| now < *until);
                // A vector failure alone holds only the vector leg; its
                // retry is due when the hold ends, even between polls.
                let due = retry
                    .vector
                    .until()
                    .map_or(next_poll, |until| until.min(next_poll));
                let run = signal.wake || (held.is_none() && (signal.nudge || now >= due));
                if run {
                    break;
                }
                let until = held.unwrap_or(due);
                signal = match shared.wake.wait_timeout(signal, until - now) {
                    Ok((guard, _)) => guard,
                    Err(poisoned) => poisoned.into_inner().0,
                };
            }
            if signal.stop {
                break;
            }
            signal.wake = false;
            signal.nudge = false;
        }
        let outcome = {
            let _busy = lock(&shared.busy);
            if lock(&shared.signal).pause > 0 {
                continue;
            }
            worker.skip_vector = retry.vector.holding(Instant::now());
            catch_unwind(AssertUnwindSafe(|| worker.round(shared)))
        };
        next_poll = later(shared.ctx.poll);
        let error = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(err)) => Some(err),
            Err(_) => Some(Error::new(
                worker.leg_code(),
                "a caller callback panicked inside the background indexer",
            )),
        };
        let last_error = retry.record(&worker, error);
        let mut state = lock(&shared.state);
        state.rounds = state.rounds.saturating_add(1);
        state.last_error = last_error;
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

/// Capped exponential backoff after failed rounds.
#[derive(Default)]
struct Backoff {
    /// Last delay and when it ends; `None` after a success.
    hold: Option<(Duration, Instant)>,
}

impl Backoff {
    fn until(&self) -> Option<Instant> {
        self.hold.map(|(_, until)| until)
    }

    fn holding(&self, now: Instant) -> bool {
        self.until().is_some_and(|until| now < until)
    }

    fn fail(&mut self) {
        let delay = match self.hold {
            None => BACKOFF_FIRST,
            Some((last, _)) => last.saturating_mul(2).min(BACKOFF_CAP),
        };
        self.hold = Some((delay, later(delay)));
    }
}

/// `delay` from now; now when the clock cannot represent that.
fn later(delay: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(delay).unwrap_or(now)
}

/// Failures the indexer is backing off from. A failure of the vector leg
/// (the embedder, mostly) holds back that leg alone: the lexical leg and
/// the journal `id` maps keep following `HEAD`.
#[derive(Default)]
struct Retry {
    round: Backoff,
    vector: Backoff,
    /// Failure of the vector leg's last attempt, reported while that leg
    /// is held back and later rounds skip it.
    vector_error: Option<Error>,
}

impl Retry {
    /// Account for a finished round; returns the error to report.
    fn record(&mut self, worker: &Worker, error: Option<Error>) -> Option<Error> {
        match error {
            Some(err) if worker.in_vector => {
                // The lexical leg finished before the vector leg started.
                self.round = Backoff::default();
                self.vector.fail();
                self.vector_error = Some(err.clone());
                Some(err)
            }
            Some(err) => {
                self.round.fail();
                Some(err)
            }
            None => {
                self.round = Backoff::default();
                if !worker.skip_vector {
                    self.vector = Backoff::default();
                    self.vector_error = None;
                }
                self.vector_error.clone()
            }
        }
    }
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
pub(crate) struct Heads {
    main: (PathBuf, String),
    nested: Option<(PathBuf, String)>,
}

impl Heads {
    /// Read both heads now. `None` before the Vault's first commit.
    pub(crate) fn read(vault: &Path) -> Result<Option<Self>, Error> {
        let Some(head) = git::peel_head_oid(vault)? else {
            return Ok(None);
        };
        let nested_dir = isolated::root(vault);
        let nested = if git::is_work_tree(&nested_dir) {
            git::peel_head_oid(&nested_dir)?.map(|head| (nested_dir, head))
        } else {
            None
        };
        Ok(Some(Self {
            main: (vault.to_path_buf(), head),
            nested,
        }))
    }

    /// The Vault's `HEAD`.
    pub(crate) fn main(&self) -> &str {
        &self.main.1
    }

    /// The nested repository's `HEAD`, if it has one.
    pub(crate) fn nested(&self) -> Option<&str> {
        self.nested.as_ref().map(|(_, head)| head.as_str())
    }

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
    /// The vector leg is backing off from a failure: leave it this round.
    skip_vector: bool,
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
        follow_current(ctx).context("[Indexer] follow current generation")?;
        ctx.search
            .follow_disk()
            .context("[Indexer] follow other handles' saves")?;
        let embedder = lock(&ctx.embedder).clone();
        let generation = ctx.index.dir()?;
        self.update_roles(ctx, &generation, embedder.as_deref())
            .context("[Indexer] take roles")?;
        {
            // Report roles before any mark moves, so a caller that sees a
            // mark this round wrote also sees who wrote it.
            let mut state = lock(&shared.state);
            state.lexical_role = self.lexical.is_some();
            state.vector_role = self.vector.is_some();
        }
        if self.lexical.is_none() && self.vector.is_none() {
            return Ok(());
        }
        ctx.search.enable_write()?;
        let result = self
            .catch_up(shared, &generation, embedder.as_deref())
            .context("[Indexer] catch up with HEAD");
        let released = ctx.search.release_writer();
        result.and(released)
    }

    fn update_roles(
        &mut self,
        ctx: &Context,
        generation: &Path,
        embedder: Option<&dyn Embedder>,
    ) -> Result<(), Error> {
        if !lexical_allowed(&ctx.search, generation)? {
            // Another projector built this generation: writing would mix
            // two column sets. `reindex` from a handle with this projector
            // is the explicit way over.
            self.lexical = None;
        } else if self.lexical.is_none() {
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
        let Some(heads) = Heads::read(&ctx.vault)? else {
            return Ok(());
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
                if job.apply_ids(repo)? == Pass::Stopped {
                    return Ok(());
                }
            }
        }
        if self.vector.is_some() && embedder.is_some() && !self.skip_vector {
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
                // The unfinished generation is an orphan the next `open`
                // prunes; until then its files are owner-only too.
                engine.release_writer()?;
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
    /// Bring the journal `id` maps of the live generation to `HEAD` of
    /// `repo`: lines a commit added are remembered, lines it removed are
    /// forgotten. Writes keep adding their own lines inline; this covers
    /// commits that reach `HEAD` another way. A collection read whole (a
    /// changed declaration, a map filled from nothing) only gains lines: a
    /// stale entry answers `not_found` from the segment anyway.
    fn apply_ids(&self, repo: Repo) -> Result<Pass, Error> {
        let Some((dir, head)) = self.heads.of(repo) else {
            return Ok(Pass::Done);
        };
        let mark = marks::read(self.generation, Leg::Ids, repo)?;
        if mark.as_deref() == Some(head) {
            return Ok(Pass::Done);
        }
        let mut found = diff::diff(dir, repo, self.decls, mark.as_deref(), head)?;
        if found.rebuild {
            found = diff::diff(dir, repo, self.decls, None, head)?;
        }
        let ctx = &self.shared.ctx;
        let journal = |name: &str| {
            self.decls
                .get(name)
                .filter(|decl| decl.kind == Kind::Journal && !decl.encrypted)
        };
        for name in &found.whole {
            let Some(decl) = journal(name) else {
                continue;
            };
            let Some((decl_dir, decl_head)) = self.heads.for_decl(decl) else {
                continue;
            };
            for (i, change) in diff::collection(decl_dir, decl, decl_head)?
                .iter()
                .enumerate()
            {
                if i % LEXICAL_BATCH == 0 && self.shared.stopping() {
                    return Ok(Pass::Stopped);
                }
                if let Change::Upsert { path, .. } = &change.change {
                    let mut maps = lock(&ctx.journal_ids);
                    index::remember_id(
                        self.generation,
                        &mut maps,
                        &decl.name,
                        &change.id,
                        &vault_rel(decl, path),
                    )?;
                }
            }
        }
        for (i, change) in found.changes.iter().enumerate() {
            if i % LEXICAL_BATCH == 0 && self.shared.stopping() {
                return Ok(Pass::Stopped);
            }
            let Some(decl) = journal(&change.collection) else {
                continue;
            };
            let mut maps = lock(&ctx.journal_ids);
            match &change.change {
                Change::Upsert { path, .. } => index::remember_id(
                    self.generation,
                    &mut maps,
                    &decl.name,
                    &change.id,
                    &vault_rel(decl, path),
                )?,
                Change::Delete => {
                    index::forget_remembered(self.generation, &mut maps, &decl.name, &change.id)?;
                }
            }
        }
        marks::write(self.generation, Leg::Ids, repo, head)?;
        Ok(Pass::Done)
    }

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
        // Commit, wait for Tantivy's merges, and close the writer before the
        // mark moves: a caller that sees the mark finds every file final
        // and owner-only, and no writer lock left behind.
        engine.release_writer()?;
        index::restrict_index_tree(self.generation)?;
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

/// Point the handle at generation `dir`. Journal lines this handle wrote
/// that `dir`'s maps lack — an open transaction's, not committed when the
/// generation was built — are carried over, so `get` keeps finding them.
fn switch_to(
    ctx: &Context,
    dir: PathBuf,
    engine: SearchEngine,
    mut maps: JournalIdMaps,
) -> Result<(), Error> {
    ctx.search.adopt(engine)?;
    let mut live = lock(&ctx.journal_ids);
    index::carry_over(&dir, &live, &mut maps)?;
    ctx.index.set_dir(dir)?;
    *live = maps;
    Ok(())
}

/// Whether a handle whose engine is `search` may take the lexical role on
/// `generation`: it was built by the same projector (or by none yet), or
/// it is in another format and is about to be rebuilt anyway.
pub(crate) fn lexical_allowed(search: &SearchEngine, generation: &Path) -> Result<bool, Error> {
    Ok(needs_new_generation(generation)? || search.lexical_writable(generation)?)
}

/// A generation with a `format` file other than the current one, or the
/// legacy layout directly under `index/`.
fn needs_new_generation(generation: &Path) -> Result<bool, Error> {
    Ok(matches!(index::format_version(generation)?, Some(v) if v != index::FORMAT_VERSION))
}

/// Records `leg` of `generation` has yet to apply to reach `heads`, both
/// repositories together; see [`IndexStatus::lexical_pending`]. `marked`
/// `false` ignores the marks on disk (a generation about to be replaced):
/// everything is pending.
pub(crate) fn pending(
    generation: &Path,
    heads: &Heads,
    decls: &Decls,
    leg: Leg,
    marked: bool,
) -> Result<u64, Error> {
    let wanted = |name: &str| decls.get(name).filter(|decl| leg_wanted(decl, leg));
    let mut count = 0usize;
    for repo in [Repo::Main, Repo::Isolated] {
        let Some((dir, head)) = heads.of(repo) else {
            continue;
        };
        let mark = if marked {
            marks::read(generation, leg, repo)?
        } else {
            None
        };
        if mark.as_deref() == Some(head) {
            continue;
        }
        let mut found = diff::diff(dir, repo, decls, mark.as_deref(), head)?;
        if found.rebuild {
            found = diff::diff(dir, repo, decls, None, head)?;
        }
        count += found
            .changes
            .iter()
            .filter(|change| wanted(&change.collection).is_some())
            .count();
        for name in &found.whole {
            let Some(decl) = wanted(name) else {
                continue;
            };
            if let Some((decl_dir, decl_head)) = heads.for_decl(decl) {
                count += diff::collection(decl_dir, decl, decl_head)?.len();
            }
        }
    }
    Ok(u64::try_from(count).unwrap_or(u64::MAX))
}

/// Whether `embedder` is set and has another model than the vectors of
/// `generation`.
pub(crate) fn model_mismatch(
    generation: &Path,
    embedder: Option<&dyn Embedder>,
) -> Result<bool, Error> {
    #[cfg(feature = "search")]
    {
        match embedder {
            Some(embedder) => Ok(!crate::search::vector_model_matches(generation, embedder)?),
            None => Ok(false),
        }
    }
    #[cfg(not(feature = "search"))]
    {
        let _ = (generation, embedder);
        Ok(false)
    }
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
            Change::Upsert { path, .. } => Some((change.id.clone(), vault_rel(decl, path))),
            Change::Delete => None,
        })
        .collect()
}

/// Vault-relative path of `path`, which is relative to the repository
/// that stores `decl` (the nested one for an isolated collection).
fn vault_rel(decl: &CollectionDecl, path: &str) -> String {
    if decl.isolated {
        format!("{}/{path}", isolated::DIR)
    } else {
        path.to_owned()
    }
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
                    if legs.vector && shared.stopping() {
                        // At most one embedder call in flight on close.
                        return Ok(Pass::Stopped);
                    }
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
