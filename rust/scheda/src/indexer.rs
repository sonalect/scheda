//! The indexer: one OS thread per handle that keeps the Index at the state
//! of `HEAD`.
//!
//! The thread sleeps on a [`Signal`]. A write of this handle that moved
//! `HEAD` (main or nested), and `Reindex`, wake it at once; otherwise it
//! looks at `HEAD` every `index_poll_ms`, which finds the writes of other
//! processes, commits made by hand, and pulls. A look reads two ref files
//! and two small files of the Index and starts no process.
//!
//! A handle takes a round only when it has every port the Index needs (see
//! `who`); otherwise it only reads: its `role` is false and its status
//! follows the version other handles reach. In a build without `find` every
//! handle indexes.
//!
//! A round takes `flock` on `index.lock` without waiting (another indexer
//! holds it: the round is skipped and looked at again later), reads `HEAD`
//! of both repositories and the Index's `commit`, and, when they differ,
//! gives the difference to every [`Side`], then writes `commit`, then lets
//! the lock go, then updates the status and wakes its watchers. A rebuild
//! (`Reindex`, a missing `commit`, another `format`, a commit that is gone
//! from its repository) is the same round from an empty Index: `commit` goes
//! first and is written last.
//!
//! A round that fails changes no `commit` and fails no write: the status
//! carries `last_error` and the round is tried again after a pause that
//! grows while it keeps failing. A panic inside a round is caught and
//! reported the same way. Only this thread writes under `index/`.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{Builder, JoinHandle};
use std::time::{Duration, Instant};

use protocontract::RuntimeCode;
#[cfg(any(feature = "find", test))]
use scheda_api::v1::VaultErrorCode;
use scheda_api::v1::{ErrorReport, IndexState, Rebuild, rebuild};

use crate::collection;
#[cfg(any(feature = "find", test))]
use crate::error::Fault;
use crate::error::{Context, Error, io, report};
use crate::idmap::JournalMaps;
use crate::index::{Counts, Index, Round, Side, State};
#[cfg(feature = "find")]
use crate::lexical::Lexical;
use crate::ports::Installed;
#[cfg(feature = "find")]
use crate::ports::ProjectorPort;
use crate::repo::{Repo, Repos};
#[cfg(feature = "search")]
use crate::vector::Vector;
#[cfg(feature = "find")]
use crate::who::Who;

/// The name of the thread, which a test counts.
pub(crate) const THREAD_NAME: &str = "scheda-indexer";

/// How many past states a watcher that falls behind can still read one by
/// one.
const HISTORY: usize = 256;

/// The least time between two progress items of a rebuild.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// What wakes the indexer thread.
#[derive(Debug, Default)]
pub(crate) struct Signal {
    flags: Mutex<Flags>,
    wake: Condvar,
}

#[derive(Debug, Default)]
struct Flags {
    moved: bool,
    stop: bool,
}

impl Signal {
    fn flags(&self) -> MutexGuard<'_, Flags> {
        // Two booleans: a panic cannot leave them half-written.
        self.flags.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `HEAD` moved, or a rebuild was asked for: look now.
    pub(crate) fn moved(&self) {
        self.flags().moved = true;
        self.wake.notify_all();
    }

    fn stop(&self) {
        self.flags().stop = true;
        self.wake.notify_all();
    }

    fn stopped(&self) -> bool {
        self.flags().stop
    }

    /// Waits until woken, stopped, or `timeout` passes; a wake that came
    /// meanwhile counts. Returns `false` when stopped.
    fn wait(&self, timeout: Duration) -> bool {
        let mut flags = self.flags();
        if !flags.moved && !flags.stop {
            flags = self
                .wake
                .wait_timeout_while(flags, timeout, |flags| !flags.moved && !flags.stop)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        flags.moved = false;
        !flags.stop
    }
}

/// What the indexer and its watchers share.
#[derive(Debug)]
struct Inner {
    /// The state now, with the changes that do not make an item.
    state: IndexState,
    /// The states published, newest last; the one before `next` is the
    /// last published.
    history: VecDeque<IndexState>,
    /// The number of the next item.
    next: u64,
    /// A rebuild was asked for and has not begun.
    requested: bool,
    /// A rebuild is running.
    running: bool,
    /// The indexer has ended: a watch ends too.
    closed: bool,
}

#[derive(Debug)]
struct Shared {
    signal: Arc<Signal>,
    inner: Mutex<Inner>,
    changed: Condvar,
    /// How many times this indexer took the lock of a round.
    #[cfg(test)]
    locks: AtomicU64,
    /// How many times this indexer looked at the Index.
    #[cfg(test)]
    looks: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Every edit leaves `Inner` whole before it can fail, so a poisoned
        // lock is as good as a clean one.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes the state and makes it the next item for the watchers.
    fn publish(&self, edit: impl FnOnce(&mut Inner)) {
        let mut inner = self.lock();
        edit(&mut inner);
        let state = inner.state.clone();
        inner.history.push_back(state);
        if inner.history.len() > HISTORY {
            inner.history.pop_front();
        }
        inner.next += 1;
        drop(inner);
        self.changed.notify_all();
    }

    /// Changes the state without making an item: what a call to
    /// `get_index_status` sees, not what a watcher is told.
    fn adjust(&self, edit: impl FnOnce(&mut Inner)) {
        edit(&mut self.lock());
    }

    fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }
}

/// The indexer of one handle: its thread and what it shares with the calls.
#[derive(Debug)]
pub(crate) struct Indexer {
    shared: Arc<Shared>,
    /// What decides whether this handle may index.
    #[cfg(feature = "find")]
    who: Mutex<Option<Arc<Who>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Indexer {
    /// An indexer that is not running. `signal` is the one the write path
    /// of the handle wakes.
    pub(crate) fn new(signal: Arc<Signal>) -> Indexer {
        Indexer {
            shared: Arc::new(Shared {
                signal,
                inner: Mutex::new(Inner {
                    state: IndexState::default(),
                    history: VecDeque::new(),
                    next: 0,
                    requested: false,
                    running: false,
                    closed: false,
                }),
                changed: Condvar::new(),
                #[cfg(test)]
                locks: AtomicU64::new(0),
                #[cfg(test)]
                looks: AtomicU64::new(0),
            }),
            #[cfg(feature = "find")]
            who: Mutex::new(None),
            thread: Mutex::new(None),
        }
    }

    /// Starts the thread for the Vault at `vault`, whose repositories
    /// `repos` reads, with the ports of the handle. It looks at `HEAD` every
    /// `poll`.
    ///
    /// # Errors
    ///
    /// `STORAGE` when the system cannot start a thread.
    pub(crate) fn start(
        &self,
        vault: &Path,
        repos: Arc<Repos>,
        ports: Arc<Installed>,
        poll: Duration,
    ) -> Result<(), Error> {
        #[cfg(not(feature = "find"))]
        let _ = &ports;
        #[cfg(feature = "find")]
        let who = Arc::new(Who::new(
            Arc::clone(&ports),
            Arc::clone(&repos),
            Index::new(vault),
        ));
        #[cfg(feature = "find")]
        {
            *self.who.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&who));
        }
        let worker = Worker {
            shared: Arc::clone(&self.shared),
            repos,
            #[cfg(test)]
            vault: vault.to_path_buf(),
            index: Index::new(vault),
            sides: vec![
                Box::new(JournalMaps),
                #[cfg(feature = "find")]
                Box::new(Lexical),
                #[cfg(feature = "search")]
                Box::new(Vector::new(Arc::clone(&ports))),
            ],
            #[cfg(feature = "find")]
            ports,
            #[cfg(feature = "find")]
            who,
            rebuilding: false,
        };
        let handle = Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || worker.run(poll))
            .map_err(io)
            .context("start the indexer")?;
        *self.thread.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
        Ok(())
    }

    /// Asks for a rebuild and returns at once.
    #[cfg(any(feature = "find", test))]
    ///
    /// # Errors
    ///
    /// `FAILED_PRECONDITION` naming the port this handle lacks, when it
    /// lacks one the Index needs; `ABORTED` when a rebuild was asked for and
    /// has not begun, or runs.
    pub(crate) fn reindex(&self) -> Result<(), Error> {
        #[cfg(feature = "find")]
        {
            let who = self
                .who
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(who) = who
                && let Some(fault) = who.check(true).context("check the ports")?.precondition()
            {
                return Err(fault.into());
            }
        }
        {
            let mut inner = self.shared.lock();
            if inner.requested || inner.running {
                return Err(Fault::new(
                    VaultErrorCode::Aborted,
                    "a rebuild of the Index is already asked for or running",
                )
                .into());
            }
            inner.requested = true;
        }
        self.shared.signal.moved();
        Ok(())
    }

    /// How many times this indexer took the lock of a round.
    #[cfg(all(test, feature = "search"))]
    pub(crate) fn locks(&self) -> u64 {
        self.shared.locks.load(Ordering::SeqCst)
    }

    /// How many times this indexer looked at the Index.
    #[cfg(all(test, feature = "search"))]
    pub(crate) fn looks(&self) -> u64 {
        self.shared.looks.load(Ordering::SeqCst)
    }

    /// Wakes the thread: a port changed, so the handle may index now.
    pub(crate) fn wake(&self) {
        self.shared.signal.moved();
    }

    /// The state of the Index now.
    #[cfg(any(feature = "find", test))]
    pub(crate) fn status(&self) -> IndexState {
        self.shared.lock().state.clone()
    }

    /// A stream of the state now, then of each change. It holds no lock
    /// while it waits and ends only when the indexer does.
    #[cfg(any(feature = "find", test))]
    pub(crate) fn watch(&self) -> Watch {
        Watch {
            shared: Arc::clone(&self.shared),
            next: None,
        }
    }

    /// Whether the thread runs.
    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        self.thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }

    /// Stops the thread and waits for it: a round in progress ends at its
    /// next step. Idempotent.
    ///
    /// # Errors
    ///
    /// `PANICKED` when the thread died by a panic that was not in a round.
    pub(crate) fn stop(&self) -> Result<(), Error> {
        self.shared.signal.stop();
        self.shared.close();
        let handle = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match handle {
            Some(handle) => handle
                .join()
                .map_err(|_| Error::new(RuntimeCode::Panicked, "the indexer thread panicked")),
            None => Ok(()),
        }
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// The stream `WatchIndexStatus` gives.
#[cfg(any(feature = "find", test))]
pub(crate) struct Watch {
    shared: Arc<Shared>,
    /// The number of the next item to give; `None` before the first.
    next: Option<u64>,
}

#[cfg(any(feature = "find", test))]
impl Iterator for Watch {
    type Item = Result<IndexState, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut inner = self.shared.lock();
        let Some(want) = self.next else {
            self.next = Some(inner.next);
            return Some(Ok(inner.state.clone()));
        };
        loop {
            if want < inner.next {
                // The oldest item still kept is `inner.next - len`; a
                // watcher further behind than that skips to it.
                let oldest = inner.next - inner.history.len() as u64;
                let at = want.max(oldest);
                self.next = Some(at + 1);
                let state = inner.history.get((at - oldest) as usize).cloned();
                return state.map(Ok);
            }
            if inner.closed {
                return None;
            }
            inner = self
                .shared
                .changed
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// What a round did.
enum Next {
    /// The Index moved: look again at once, `HEAD` may have too.
    Again,
    /// Nothing to do now: sleep.
    Idle,
}

/// What a handle does about a round, given its ports.
#[cfg(feature = "find")]
enum Gate {
    /// It takes the round.
    Go,
    /// It does not: it follows the Index.
    Stay,
    /// A rebuild it asked for cannot be done by it.
    Refused(Error),
}

/// What the part of a round under the lock did.
enum Applied {
    /// The Index was at this state already.
    Current(State),
    /// The Index was brought to this state.
    Built {
        head: State,
        counts: Counts,
        rebuilt: bool,
    },
}

struct Worker {
    shared: Arc<Shared>,
    repos: Arc<Repos>,
    #[cfg(test)]
    vault: std::path::PathBuf,
    index: Index,
    sides: Vec<Box<dyn Side>>,
    /// The ports of the Vault: a round takes its Projector from them, and
    /// asks the vector side's texts of them.
    #[cfg(feature = "find")]
    ports: Arc<Installed>,
    /// Whether the handle may take a round.
    #[cfg(feature = "find")]
    who: Arc<Who>,
    /// A rebuild has begun and has not ended.
    rebuilding: bool,
}

impl Worker {
    fn run(mut self, poll: Duration) {
        let longest = poll.saturating_mul(8);
        let mut backoff = poll;
        let mut pause = Duration::ZERO;
        loop {
            let go = if pause.is_zero() {
                !self.shared.signal.stopped()
            } else {
                self.shared.signal.wait(pause)
            };
            if !go {
                break;
            }
            let outcome = catch_unwind(AssertUnwindSafe(|| self.round()))
                .unwrap_or_else(|payload| Err(panicked(payload.as_ref())));
            match outcome {
                Ok(Next::Again) => {
                    backoff = poll;
                    pause = Duration::ZERO;
                }
                Ok(Next::Idle) => {
                    backoff = poll;
                    pause = poll;
                }
                Err(error) => {
                    if self.shared.signal.stopped() {
                        break;
                    }
                    self.fail(&error);
                    pause = backoff;
                    backoff = backoff.saturating_mul(2).min(longest);
                }
            }
        }
        self.shared.close();
    }

    /// One look at `HEAD` and, if the Index is behind, one round.
    fn round(&mut self) -> Result<Next, Error> {
        #[cfg(test)]
        self.shared.looks.fetch_add(1, Ordering::SeqCst);
        let asked = self.shared.lock().requested;
        let committed = if self.index.has_format()? {
            self.index.read_commit()?
        } else {
            None
        };
        if !asked
            && let Some(committed) = committed
            && committed == State::of(&self.repos.views()?)
        {
            self.follow(Some(committed));
            return Ok(Next::Idle);
        }
        // One Projector for the check and the round: one given meanwhile
        // waits for the next round.
        #[cfg(feature = "find")]
        let projector = self.ports.projector();
        #[cfg(feature = "find")]
        match self.gate(asked, committed, projector.as_deref())? {
            Gate::Go => {}
            Gate::Stay => return Ok(Next::Idle),
            Gate::Refused(error) => return Err(error),
        }
        let Some(lock) = self.index.try_lock()? else {
            // Another indexer holds the lock; it will do the round.
            self.shared.adjust(|inner| inner.state.role = Some(false));
            return Ok(Next::Idle);
        };
        #[cfg(test)]
        self.shared.locks.fetch_add(1, Ordering::SeqCst);
        let applied = self.apply(
            #[cfg(feature = "find")]
            projector,
        );
        // The commit is written: the lock goes before anyone is told.
        drop(lock);
        match applied? {
            Applied::Current(state) => {
                self.follow(Some(state));
                Ok(Next::Idle)
            }
            Applied::Built {
                head,
                counts,
                rebuilt,
            } => {
                self.rebuilding = false;
                self.shared.publish(|inner| {
                    inner.state.version = Some(head.token());
                    inner.state.pending = Some(0);
                    inner.state.role = Some(false);
                    inner.state.model_mismatch = Some(false);
                    inner.state.last_error = buffa::MessageField::none();
                    if rebuilt {
                        inner.running = false;
                        inner.state.rebuild = rebuild_of(rebuild::Stage::STAGE_DONE, counts).into();
                    }
                });
                Ok(Next::Again)
            }
        }
    }

    /// The round itself, with the lock held.
    fn apply(
        &mut self,
        #[cfg(feature = "find")] projector: Option<Arc<ProjectorPort>>,
    ) -> Result<Applied, Error> {
        let committed = if self.index.has_format()? {
            self.index.read_commit()?
        } else {
            None
        };
        let views = self.repos.views().context("read HEAD")?;
        let asked = self.shared.lock().requested;
        if !asked && committed.is_some() && committed == Some(State::of(&views)) {
            return Ok(Applied::Current(State::of(&views)));
        }
        #[cfg(test)]
        let _in_round = hooks::enter(&self.vault)?;
        let declarations =
            collection::load(views.of(Repo::Main)).context("read the collections")?;
        let from = if asked { None } else { committed };
        let shared = Arc::clone(&self.shared);
        let mut last = Instant::now();
        let mut tick = |counts: &Counts| -> Result<(), Error> {
            if shared.signal.stopped() {
                return Err(Error::new(
                    RuntimeCode::Cancelled,
                    "the indexer is stopping",
                ));
            }
            if shared.lock().running && last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                let counts = *counts;
                shared.publish(|inner| {
                    inner.state.rebuild = rebuild_of(rebuild::Stage::STAGE_RUNNING, counts).into();
                });
            }
            Ok(())
        };
        let mut round = match Round::new(&self.index, &views, &declarations, from, &mut tick)? {
            Some(round) => round,
            None => match Round::new(&self.index, &views, &declarations, None, &mut tick)? {
                Some(round) => round,
                None => unreachable!("a round from nothing always knows its difference"),
            },
        };
        #[cfg(feature = "find")]
        round.set_projector(projector);
        #[cfg(feature = "search")]
        if self.ports.embedder().is_some() {
            round.collect_offers();
        }
        let pending = round.pending();
        let rebuilt = round.is_rebuild();
        self.shared.adjust(|inner| {
            inner.state.role = Some(true);
            inner.state.pending = pending;
        });
        if rebuilt {
            self.index.reset().context("empty the Index")?;
            self.rebuilding = true;
            self.shared.publish(|inner| {
                inner.requested = false;
                inner.running = true;
                inner.state.version = None;
                inner.state.rebuild =
                    rebuild_of(rebuild::Stage::STAGE_RUNNING, Counts::default()).into();
            });
        }
        #[cfg(test)]
        hooks::at_work(&self.vault)?;
        for side in &mut self.sides {
            side.apply(&mut round)?;
        }
        let head = round.to();
        let counts = round.counts();
        self.index
            .write_commit(head)
            .context("record the state of the Index")?;
        Ok(Applied::Built {
            head,
            counts,
            rebuilt,
        })
    }

    /// Nothing is to be done by this handle: the Index is at `committed`
    /// (or not built), and either it is at `HEAD` or another handle has to
    /// bring it there. Another indexer may have moved it, and an earlier
    /// failure is over.
    fn follow(&self, committed: Option<State>) {
        let token = committed.map(State::token);
        let pending = committed.map(|_| 0);
        let mismatch = self.mismatch();
        let stale = self.stale();
        let differs = {
            let inner = self.shared.lock();
            inner.state.version != token
                || inner.state.last_error.as_option() != stale.as_ref()
                || inner.state.role != Some(false)
                || inner.state.pending != pending
                || inner.state.model_mismatch != Some(mismatch)
        };
        if differs {
            self.shared.publish(|inner| {
                inner.state.version = token;
                inner.state.pending = pending;
                inner.state.role = Some(false);
                inner.state.model_mismatch = Some(mismatch);
                inner.state.last_error = stale.into();
            });
        }
    }

    /// Why this Vault does not index although it has its ports: the Index
    /// was built by a Projector of another projection (`INDEX_STALE`).
    fn stale(&self) -> Option<ErrorReport> {
        #[cfg(feature = "find")]
        {
            self.who
                .stale()
                .then(|| report(&crate::who::stale_fault().into()))
        }
        #[cfg(not(feature = "find"))]
        {
            None
        }
    }

    /// Whether the embedder of this handle is of another model than the
    /// vectors of the Index.
    fn mismatch(&self) -> bool {
        #[cfg(feature = "find")]
        {
            self.who.mismatch()
        }
        #[cfg(not(feature = "find"))]
        {
            false
        }
    }

    /// Whether this handle has the ports the round needs. When it has not,
    /// it follows the Index instead.
    #[cfg(feature = "find")]
    fn gate(
        &self,
        asked: bool,
        committed: Option<State>,
        projector: Option<&ProjectorPort>,
    ) -> Result<Gate, Error> {
        let verdict = self
            .who
            .check_with(projector, asked)
            .context("check the ports")?;
        let Some(fault) = verdict.precondition() else {
            return Ok(Gate::Go);
        };
        if asked {
            // The handle lost a port after it asked: the request is over.
            self.shared.lock().requested = false;
            return Ok(Gate::Refused(fault.into()));
        }
        self.follow(committed);
        Ok(Gate::Stay)
    }

    /// A round failed: the status says so.
    fn fail(&mut self, error: &Error) {
        let report = report(error);
        let rebuilding = std::mem::take(&mut self.rebuilding);
        self.shared.publish(|inner| {
            inner.state.role = Some(false);
            inner.state.last_error = report.clone().into();
            if rebuilding {
                inner.running = false;
                let mut failed = rebuild_of(rebuild::Stage::STAGE_FAILED, Counts::default());
                failed.error = report.into();
                inner.state.rebuild = failed.into();
            }
        });
    }
}

fn rebuild_of(stage: rebuild::Stage, counts: Counts) -> Rebuild {
    Rebuild::default()
        .with_stage(stage)
        .with_documents_read(counts.documents_read)
        .with_documents_skipped(counts.documents_skipped)
        .with_segments_read(counts.segments_read)
}

/// The error of a round that panicked.
fn panicked(payload: &(dyn std::any::Any + Send)) -> Error {
    let what = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| String::from("without a message"));
    Error::new(
        RuntimeCode::Panicked,
        format!("an indexing round panicked: {what}"),
    )
}

/// What the unit tests do to a round: stretch it, count the rounds at work
/// at once, make it panic.
#[cfg(test)]
pub(crate) mod hooks {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, PoisonError};
    use std::time::Duration;

    use crate::error::Error;

    #[derive(Default)]
    struct Hook {
        pause: Duration,
        active: usize,
        most: usize,
        panics: usize,
        fails: usize,
    }

    static HOOKS: Mutex<Option<HashMap<PathBuf, Hook>>> = Mutex::new(None);

    fn with<T>(vault: &Path, edit: impl FnOnce(&mut Hook) -> T) -> T {
        let mut guard = HOOKS.lock().unwrap_or_else(PoisonError::into_inner);
        let hooks = guard.get_or_insert_with(HashMap::new);
        edit(hooks.entry(vault.to_path_buf()).or_default())
    }

    /// Every round on `vault` takes at least `pause` at its work.
    pub(crate) fn stretch(vault: &Path, pause: Duration) {
        with(vault, |hook| hook.pause = pause);
    }

    /// The most rounds on `vault` that were at work at the same time.
    pub(crate) fn most_at_once(vault: &Path) -> usize {
        with(vault, |hook| hook.most)
    }

    /// The next round on `vault` to reach its work panics.
    pub(crate) fn panic_once(vault: &Path) {
        with(vault, |hook| hook.panics += 1);
    }

    /// The next round on `vault` to reach its work fails.
    pub(crate) fn fail_once(vault: &Path) {
        with(vault, |hook| hook.fails += 1);
    }

    /// A round at work; counted until dropped.
    pub(crate) struct InRound(PathBuf);

    impl Drop for InRound {
        fn drop(&mut self) {
            with(&self.0, |hook| hook.active -= 1);
        }
    }

    pub(crate) fn enter(vault: &Path) -> Result<InRound, Error> {
        with(vault, |hook| {
            hook.active += 1;
            hook.most = hook.most.max(hook.active);
        });
        Ok(InRound(vault.to_path_buf()))
    }

    pub(crate) fn at_work(vault: &Path) -> Result<(), Error> {
        let (pause, panics, fails) = with(vault, |hook| {
            let panics = hook.panics > 0;
            hook.panics = hook.panics.saturating_sub(1);
            let fails = !panics && hook.fails > 0;
            if fails {
                hook.fails -= 1;
            }
            (hook.pause, panics, fails)
        });
        std::thread::sleep(pause);
        if panics {
            std::panic::panic_any(String::from("a round was made to panic"));
        }
        if fails {
            return Err(Error::new(
                scheda_api::v1::VaultErrorCode::Storage,
                "a round was made to fail",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use scheda_api::v1::{
        CollectionDeclaration, DocumentCreation, IndexState, OpenOptions, VaultErrorCode,
    };

    use super::{HISTORY, Indexer, Signal, Watch, hooks};
    use crate::index::Index;
    use crate::ports::indexing_ports;
    use crate::vault::Vault;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-indexer-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Scratch(dir)
        }

        fn open(&self) -> Vault {
            Vault::open(
                &self.0,
                OpenOptions::default().with_index_poll_ms(10),
                indexing_ports(),
            )
            .unwrap()
        }

        fn canonical(&self) -> PathBuf {
            self.0.canonicalize().unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn journal(vault: &Vault, name: &str) {
        let collection: CollectionDeclaration = serde_json::from_value(serde_json::json!({
            "name": name, "schema": {"journal": {}}
        }))
        .unwrap();
        vault.create_collection(collection).unwrap();
    }

    fn line(vault: &Vault, collection: &str, id: &str) {
        vault
            .create(
                collection,
                DocumentCreation {
                    id: Some(id.to_owned()),
                    raw: Some(b"{}".to_vec()),
                    note: Some("test".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap();
    }

    fn head(path: &Path) -> String {
        let index = Index::new(path);
        let views = crate::repo::Repos::new(path).views().unwrap();
        let _ = index;
        crate::index::State::of(&views).token()
    }

    /// Waits until `done` says so, or fails the test after 20 seconds.
    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn at_head(vault: &Vault, path: &Path) -> bool {
        let state = vault.indexer().status();
        state.version.as_deref() == Some(head(path).as_str())
            && state.pending == Some(0)
            && state.role != Some(true)
    }

    #[test]
    fn the_watch_gives_the_state_at_once_and_then_one_item_per_round() {
        let scratch = Scratch::new("watch");
        let vault = scratch.open();
        journal(&vault, "log");
        let path = scratch.canonical();
        wait_until("the first build", || at_head(&vault, &path));
        let mut watch = vault.indexer().watch();
        let first = watch.next().unwrap().unwrap();
        assert_eq!(first.version.as_deref(), Some(head(&path).as_str()));
        line(&vault, "log", "one");
        let moved = watch.next().unwrap().unwrap();
        assert_eq!(moved.version.as_deref(), Some(head(&path).as_str()));
        assert_eq!(moved.pending, Some(0));
        assert_eq!(moved.role, Some(false));
        assert!(!moved.last_error.is_set());
        // No further write, no further item.
        assert!(at_head(&vault, &path));
        vault.close().unwrap();
        assert!(watch.next().is_none(), "the stream ends with the handle");
    }

    #[test]
    fn a_watcher_that_falls_far_behind_skips_to_the_oldest_item_kept() {
        let indexer = Indexer::new(Arc::new(Signal::default()));
        let mut watch = indexer.watch();
        assert!(watch.next().unwrap().unwrap().version.is_none());
        for n in 0..HISTORY + 10 {
            indexer
                .shared
                .publish(|inner| inner.state.pending = Some(n as u64));
        }
        let oldest = watch.next().unwrap().unwrap();
        assert_eq!(oldest.pending, Some(10));
        assert_eq!(
            watch.next().unwrap().unwrap().pending,
            Some(11),
            "then one by one"
        );
    }

    #[test]
    fn the_rounds_of_two_handles_of_one_vault_never_overlap() {
        let scratch = Scratch::new("overlap");
        let one = scratch.open();
        journal(&one, "log");
        let path = scratch.canonical();
        wait_until("the first build", || at_head(&one, &path));
        hooks::stretch(&path, Duration::from_millis(30));
        let two = scratch.open();
        for n in 0..6 {
            line(&one, "log", &format!("a{n}"));
            line(&two, "log", &format!("b{n}"));
        }
        wait_until("the first handle", || at_head(&one, &path));
        wait_until("the second handle", || at_head(&two, &path));
        assert_eq!(hooks::most_at_once(&path), 1);
        assert_eq!(
            one.indexer().status().version,
            two.indexer().status().version
        );
        assert!(one.indexer().status().last_error.as_option().is_none());
        assert!(two.indexer().status().last_error.as_option().is_none());
    }

    /// The first item of `watch` that reports a failed round. The status
    /// itself may have moved on by then: the retry starts one poll later.
    fn failure(watch: &mut Watch) -> IndexState {
        loop {
            let state = watch.next().unwrap().unwrap();
            if state.last_error.is_set() {
                return state;
            }
        }
    }

    #[test]
    fn a_round_that_fails_or_panics_is_reported_and_tried_again() {
        let scratch = Scratch::new("fail");
        let vault = scratch.open();
        journal(&vault, "log");
        let path = scratch.canonical();
        wait_until("the first build", || at_head(&vault, &path));
        let index = Index::new(&path);
        let before = index.read_commit().unwrap();
        hooks::fail_once(&path);
        let mut watch = vault.indexer().watch();
        line(&vault, "log", "one");
        let state = failure(&mut watch);
        let error = state.last_error.as_option().unwrap();
        assert_eq!(error.code.as_deref(), Some("VAULT_ERROR_CODE_STORAGE"));
        assert_eq!(state.role, Some(false));
        // The write was not failed; the thread goes on and the next round
        // succeeds.
        wait_until("the retry", || at_head(&vault, &path));
        assert!(!vault.indexer().status().last_error.is_set());
        assert_ne!(index.read_commit().unwrap(), before);

        hooks::panic_once(&path);
        let mut watch = vault.indexer().watch();
        line(&vault, "log", "two");
        let state = failure(&mut watch);
        let error = state.last_error.as_option().unwrap();
        assert_eq!(error.code.as_deref(), Some("PANICKED"));
        assert!(error.message.as_deref().unwrap_or("").contains("panicked"));
        wait_until("the retry", || at_head(&vault, &path));
        assert!(vault.indexer().is_running());
    }

    #[test]
    fn a_rebuild_waits_for_the_lock_and_a_second_request_is_aborted() {
        let scratch = Scratch::new("rebuild");
        let vault = scratch.open();
        journal(&vault, "log");
        line(&vault, "log", "one");
        let path = scratch.canonical();
        wait_until("the first build", || at_head(&vault, &path));
        let held = Index::new(&path).try_lock().unwrap().unwrap();
        let mut watch = vault.indexer().watch();
        watch.next().unwrap().unwrap();
        vault.indexer().reindex().unwrap();
        let error = vault.indexer().reindex().unwrap_err();
        assert!(error.is(VaultErrorCode::Aborted));
        std::thread::sleep(Duration::from_millis(60));
        let state = vault.indexer().status();
        assert_eq!(
            state
                .rebuild
                .as_option()
                .and_then(|rebuild| rebuild.stage)
                .and_then(|stage| stage.as_known()),
            Some(scheda_api::v1::rebuild::Stage::STAGE_DONE),
            "the lock is held: the rebuild has not begun, the first build is the last"
        );
        assert!(state.version.is_some());
        drop(held);
        let mut stages = Vec::new();
        let last: IndexState = loop {
            let state = watch.next().unwrap().unwrap();
            let stage = state
                .rebuild
                .as_option()
                .and_then(|rebuild| rebuild.stage)
                .and_then(|stage| stage.as_known());
            stages.push(stage);
            if state.rebuild.as_option().is_some() && state.version.is_some() {
                break state;
            }
        };
        assert!(stages.contains(&Some(scheda_api::v1::rebuild::Stage::STAGE_RUNNING)));
        let rebuild = last.rebuild.as_option().unwrap();
        assert_eq!(
            rebuild.stage.and_then(|stage| stage.as_known()),
            Some(scheda_api::v1::rebuild::Stage::STAGE_DONE)
        );
        assert_eq!(rebuild.documents_read, Some(1));
        assert_eq!(rebuild.segments_read, Some(1));
        // A new request is accepted after the rebuild.
        vault.indexer().reindex().unwrap();
    }

    #[test]
    fn close_and_drop_end_the_thread() {
        let scratch = Scratch::new("stop");
        let one = scratch.open();
        let two = scratch.open();
        assert!(one.indexer().is_running());
        let shared = Arc::clone(&two.indexer().shared);
        one.close().unwrap();
        assert!(!shared.lock().closed, "the other handle is not closed");
        drop(two);
        assert!(shared.lock().closed, "a dropped handle stops its thread");
    }
}
