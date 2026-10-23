//! The indexer: one task per handle that keeps the Index at the state of
//! `HEAD`, on the runtime that opened the Vault.
//!
//! The task sleeps on a [`Signal`]. A write of this handle that moved
//! `HEAD` (main or nested), `Reindex`, and a port set or removed, wake it
//! at once; otherwise it
//! looks at `HEAD` every `index_poll_ms`, which finds the writes of other
//! processes, commits made by hand, and pulls. A look reads two ref files
//! and two small files of the Index and starts no process.
//!
//! A handle indexes when it has every port the Index needs: a Vault whose
//! Projector or embedder is of other ports than the Index was built with
//! finds it stale, and one that lacks the embedder a `vector` column needs
//! does not index; the status says which, and such a handle still reads
//! the Index others keep. The ports are compared with the Index at the
//! first look and whenever a port was set or removed or the live generation
//! changed since, `HEAD` moved or not, so the status names a mismatch in
//! the round a port change wakes; a look that finds none of these compares
//! nothing. A round takes the ports once, after reading how many port
//! changes were made, and keeps them to its end: a port set while it runs
//! is the next round's. Every state a round or a look ends with carries that
//! count as `IndexState.port_change`, whatever it found, so the caller of a
//! port change waits for the verdict by the number the change returned. A
//! build without the feature `index` keeps the journal id maps only, in
//! every handle.
//!
//! A round takes `flock` on `index.lock` without waiting (another indexer
//! holds it: the round is skipped and looked at again later), reads `HEAD`
//! of both repositories and the Index's `commit`, and, when they differ,
//! gives the difference to every [`Side`], then writes `commit`, then lets
//! the lock go, then updates the status and wakes its watchers. A rebuild
//! (`Reindex`, a missing `commit`, another `format`, a commit that is gone
//! from its repository) is the same round from an empty Index: `commit` goes
//! first and is written last. A `Reindex` records the live generation when
//! it is asked for; a round that finds another one live, built with this
//! handle's ports and columns (by another handle that was asked too), has
//! it served and builds nothing.
//!
//! A round that fails changes no `commit` and fails no write: the status
//! carries `last_error` and the round is tried again after a pause that
//! grows while it keeps failing. A panic inside a round is caught and
//! reported the same way. Only this task writes under `index/`.
//!
//! A round is one blocking step: every part of it reads or writes files
//! under `index.lock`. Between two rounds the task waits without a thread.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use protocontract::RuntimeCode;
#[cfg(feature = "index")]
use scheda_api::v1::ErrorReport;
#[cfg(any(test, feature = "index"))]
use scheda_api::v1::VaultErrorCode;
use scheda_api::v1::{IndexState, Rebuild, rebuild};
use scheda_blocking::{Blocking, blocking};
use tokio::runtime::Handle;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use crate::collection;
#[cfg(feature = "index")]
use crate::collection::Declarations;
#[cfg(any(test, feature = "index"))]
use crate::error::Fault;
use crate::error::{Context as _, Error, error_report};
#[cfg(feature = "index")]
use crate::fs::restrict_tree;
#[cfg(feature = "index")]
use crate::generation::{self, Commit, Generation, Layout};
use crate::idmap::JournalMaps;
use crate::index::{Counts, Index, Round, Side, State};
use crate::ports::Installed;
use crate::repo::{Repo, Repos};
#[cfg(feature = "index")]
use crate::tables::{self, Cache, Calls, Plan, Work};

/// How many past states a watcher that falls behind can still read one by
/// one.
const HISTORY: usize = 256;

/// The least time between two progress items of a rebuild.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// What wakes the indexer.
#[derive(Debug, Default)]
pub(crate) struct Signal {
    stop: AtomicBool,
    /// How many times a port of the Vault was set or removed.
    ports: AtomicU64,
    wake: Notify,
}

impl Signal {
    /// `HEAD` moved, or a rebuild was asked for: look now. A wake that
    /// comes while the indexer is at a round counts once it is done, and
    /// one that comes while it pauses after a failed round ends the pause.
    pub(crate) fn moved(&self) {
        self.wake.notify_one();
    }

    /// A port of the Vault was set or removed: look now, with it, and
    /// compare the ports with the Index again even when `HEAD` stands.
    /// Called once the port is in place. Counts as [`Signal::moved`] does.
    /// Returns the number of this change, `IndexState.port_change` of the
    /// states that reflect it.
    pub(crate) fn ports_changed(&self) -> u64 {
        let number = self.ports.fetch_add(1, Ordering::Release) + 1;
        self.wake.notify_one();
        number
    }

    /// How many times a port was set or removed. A port is in place before
    /// this moves, so ports read after it are at least as new as the count.
    fn ports(&self) -> u64 {
        self.ports.load(Ordering::Acquire)
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Waits until woken, stopped, or `timeout` passes. Returns `false`
    /// when stopped.
    async fn wait(&self, timeout: Duration) -> bool {
        if !self.stopped() {
            let _ = tokio::time::timeout(timeout, self.wake.notified()).await;
        }
        !self.stopped()
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
    /// A rebuild was asked for and no rebuild has served it yet: it stands
    /// through the tries that fail.
    requested: bool,
    /// The live generation when the rebuild was asked for; `None` when
    /// there was none.
    #[cfg(feature = "index")]
    requested_beside: Option<String>,
    /// A rebuild is running.
    running: bool,
    /// The indexer has ended: a watch ends too.
    closed: bool,
}

#[derive(Debug)]
struct Shared {
    signal: Arc<Signal>,
    inner: Mutex<Inner>,
    /// The number of the next item, sent each time it moves or the
    /// indexer ends: what a watcher waits on.
    changed: watch::Sender<u64>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        // Every edit leaves `Inner` whole before it can fail, so a poisoned
        // lock is as good as a clean one.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes the state and makes it the next item for the watchers.
    fn publish(&self, edit: impl FnOnce(&mut Inner)) {
        self.publish_if(|inner| {
            edit(inner);
            true
        });
    }

    /// As [`Shared::publish`], when `edit`, which sees the state and may
    /// change it, returns `true`; nothing is published when it returns
    /// `false`, and it then leaves the state as it was.
    fn publish_if(&self, edit: impl FnOnce(&mut Inner) -> bool) {
        let mut inner = self.lock();
        if !edit(&mut inner) {
            return;
        }
        let state = inner.state.clone();
        inner.history.push_back(state);
        if inner.history.len() > HISTORY {
            inner.history.pop_front();
        }
        inner.next += 1;
        let next = inner.next;
        drop(inner);
        self.changed.send_replace(next);
    }

    /// Changes the state without making an item: what a call to
    /// `get_index_status` sees, not what a watcher is told.
    fn adjust(&self, edit: impl FnOnce(&mut Inner)) {
        edit(&mut self.lock());
    }

    fn close(&self) {
        let next = {
            let mut inner = self.lock();
            inner.closed = true;
            inner.next
        };
        self.changed.send_replace(next);
    }
}

/// The indexer of one handle: its task and what it shares with the calls.
#[derive(Debug)]
pub(crate) struct Indexer {
    /// The Vault it indexes.
    vault: PathBuf,
    shared: Arc<Shared>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Indexer {
    /// An indexer of the Vault at `vault` that is not running. `signal` is
    /// the one the write path of the handle wakes.
    pub(crate) fn new(signal: Arc<Signal>, vault: &Path) -> Indexer {
        Indexer {
            vault: vault.to_path_buf(),
            shared: Arc::new(Shared {
                signal,
                inner: Mutex::new(Inner {
                    state: IndexState::default(),
                    history: VecDeque::new(),
                    next: 0,
                    requested: false,
                    #[cfg(feature = "index")]
                    requested_beside: None,
                    running: false,
                    closed: false,
                }),
                changed: watch::Sender::new(0),
            }),
            task: Mutex::new(None),
        }
    }

    /// Starts the task, whose repositories `repos` reads, on the runtime of
    /// the caller. It looks at `HEAD` every `poll`.
    ///
    /// # Errors
    ///
    /// `NO_RUNTIME` outside a tokio runtime.
    pub(crate) fn start(
        &self,
        repos: Arc<Repos>,
        ports: Arc<Installed>,
        poll: Duration,
    ) -> Result<(), Error> {
        let vault = self.vault.as_path();
        #[cfg(not(feature = "index"))]
        let _ = ports;
        let runtime = Handle::try_current().map_err(|_| {
            Error::new(
                RuntimeCode::NoRuntime,
                "a Vault runs its indexer on a tokio runtime; open it inside one",
            )
        })?;
        let worker = Worker {
            context: Context {
                shared: Arc::clone(&self.shared),
                repos,
                index: Index::new(vault),
                #[cfg(test)]
                vault: vault.to_path_buf(),
                #[cfg(feature = "index")]
                ports,
            },
            kept: None,
        };
        let task = runtime.spawn(worker.run(poll));
        *self.task.lock().unwrap_or_else(PoisonError::into_inner) = Some(task);
        Ok(())
    }

    /// Asks for a rebuild and returns at once. The request stands until a
    /// rebuild is done: a try that fails is followed by another. It records
    /// the live generation now: another one, built since with this
    /// handle's ports and columns, serves it.
    ///
    /// # Errors
    ///
    /// `ABORTED` when a rebuild was asked for and is not done, or runs;
    /// `STORAGE` when the live generation cannot be read.
    #[cfg(any(test, feature = "index"))]
    pub(crate) async fn reindex(&self) -> Result<(), Error> {
        #[cfg(feature = "index")]
        let live = {
            let layout = Layout::new(Index::new(&self.vault).root());
            blocking(move |cx| {
                Ok(layout
                    .current(cx)?
                    .map(|generation| generation.name().to_owned()))
            })
            .await?
        };
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
            #[cfg(feature = "index")]
            {
                inner.requested_beside = live;
            }
        }
        self.shared.signal.moved();
        Ok(())
    }

    /// The state of the Index now.
    #[cfg(any(test, feature = "index"))]
    pub(crate) fn status(&self) -> IndexState {
        self.shared.lock().state.clone()
    }

    /// Whether this handle has a rebuild asked for or running. Both flags
    /// clear together, in the same step that publishes the new generation's
    /// state, so a reader that sees neither and then reads the live
    /// generation sees the generation the rebuild made.
    #[cfg(feature = "index")]
    pub(crate) fn rebuild_pending(&self) -> bool {
        let inner = self.shared.lock();
        inner.requested || inner.running
    }

    /// A stream of the state now, then of each change. It holds no lock
    /// while it waits and ends only when the indexer does; a watcher that
    /// drops it is gone at once.
    #[cfg(any(test, feature = "index"))]
    pub(crate) fn watch(&self) -> Watch {
        Watch {
            shared: Arc::clone(&self.shared),
            changed: self.shared.changed.subscribe(),
            next: None,
        }
    }

    /// Whether the task runs.
    #[cfg(test)]
    pub(crate) fn is_running(&self) -> bool {
        self.task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|task| !task.is_finished())
    }

    /// Stops the task and waits for it: a round in progress ends at its
    /// next step. Idempotent.
    ///
    /// # Errors
    ///
    /// `PANICKED` when the task died by a panic that was not in a round;
    /// `CANCELLED` when the runtime ended it first.
    pub(crate) async fn stop(&self) -> Result<(), Error> {
        self.shared.signal.stop();
        let task = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let ended = match task {
            Some(task) => task.await.map_err(|error| {
                let code = if error.is_cancelled() {
                    RuntimeCode::Cancelled
                } else {
                    RuntimeCode::Panicked
                };
                Error::new(code, format!("the indexer task failed: {error}"))
            }),
            None => Ok(()),
        };
        self.shared.close();
        ended
    }
}

impl Drop for Indexer {
    /// A handle dropped without `Close` cannot wait: the task is told to
    /// stop and ends after its current round, on its own.
    fn drop(&mut self) {
        self.shared.signal.stop();
    }
}

/// What a watcher finds when it looks at the shared state.
#[cfg(any(test, feature = "index"))]
enum Read {
    /// The next item, and the cursor after it.
    Item(Box<IndexState>, u64),
    /// The indexer ended: the watch ends.
    End,
    /// Nothing new yet.
    Wait,
}

#[cfg(any(test, feature = "index"))]
impl Inner {
    /// What the watcher whose cursor is `cursor` (`None` before the first
    /// item) is to do next.
    fn read(&self, cursor: Option<u64>) -> Read {
        let Some(want) = cursor else {
            return Read::Item(Box::new(self.state.clone()), self.next);
        };
        if want < self.next {
            // The oldest item still kept is `self.next - len`; a watcher
            // further behind than that skips to it.
            let oldest = self.next - self.history.len() as u64;
            let at = want.max(oldest);
            return match self.history.get((at - oldest) as usize) {
                Some(state) => Read::Item(Box::new(state.clone()), at + 1),
                None => Read::End,
            };
        }
        if self.closed { Read::End } else { Read::Wait }
    }
}

/// A stream of the states of the Index.
#[cfg(any(test, feature = "index"))]
pub(crate) struct Watch {
    shared: Arc<Shared>,
    changed: watch::Receiver<u64>,
    /// The number of the next item to give; `None` before the first.
    next: Option<u64>,
}

#[cfg(any(test, feature = "index"))]
impl Watch {
    /// The next state, `None` once the indexer has ended.
    pub(crate) async fn next(&mut self) -> Option<Result<IndexState, Error>> {
        loop {
            // Marked seen before the state is read, so a change made after
            // the read wakes the wait below.
            self.changed.borrow_and_update();
            let read = self.shared.lock().read(self.next);
            match read {
                Read::Item(state, next) => {
                    self.next = Some(next);
                    return Some(Ok(*state));
                }
                Read::End => return None,
                Read::Wait => {
                    if self.changed.changed().await.is_err() {
                        return None;
                    }
                }
            }
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

/// What a round brought the Index to.
struct Applied {
    /// The state the Index reflects now.
    state: State,
    counts: Counts,
    /// Why it was built again, when it was.
    rebuilt: Option<rebuild::Cause>,
}

/// What a round keeps between two tries: the vectors the embedder gave
/// and the Projector's answers, and the last comparison of the ports with
/// an Index at `HEAD`. Nothing in a build without the Index.
#[derive(Default)]
struct Kept {
    #[cfg(feature = "index")]
    cache: Cache,
    #[cfg(feature = "index")]
    compared: Option<Compared>,
}

/// What a comparison of this Vault's ports with an Index at `HEAD` was made
/// with, and what it found. It stands while no port is set or removed and
/// the live generation keeps its `commit`, which names the state whose
/// declarations and Projector columns were compared.
#[cfg(feature = "index")]
struct Compared {
    /// [`Signal::ports`], read before the ports were.
    ports: u64,
    generation: String,
    commit: Commit,
    /// Why this Vault does not index that Index; `None` when it does.
    refused: Option<ErrorReport>,
}

/// What the look at the start of a round found.
struct Look {
    asked: bool,
    /// The live generation when the rebuild was asked for.
    #[cfg(feature = "index")]
    asked_beside: Option<String>,
    /// The state the journal id maps reflect; `None` before they are built,
    /// or when the files are of another layout.
    maps: Option<State>,
    /// The state `HEAD` names now.
    head: State,
    /// The live generation of the Lance tables and its commit.
    #[cfg(feature = "index")]
    live: Option<(Generation, Option<Commit>)>,
}

impl Look {
    /// The state the Index reflects, as `IndexState.version` reports it.
    fn version(&self) -> Option<State> {
        #[cfg(feature = "index")]
        {
            self.live
                .as_ref()
                .and_then(|(_, commit)| commit.as_ref())
                .and_then(Commit::state)
        }
        #[cfg(not(feature = "index"))]
        {
            self.maps
        }
    }

    /// The live commit of the Lance tables.
    #[cfg(feature = "index")]
    fn commit(&self) -> Option<&Commit> {
        self.live.as_ref().and_then(|(_, commit)| commit.as_ref())
    }

    /// Whether nothing is to be done: no rebuild asked, and every part at
    /// `HEAD`.
    fn current(&self) -> bool {
        !self.asked && self.maps == Some(self.head) && self.version() == Some(self.head)
    }
}

/// What a round works with: cheap to clone into the task the round runs
/// as.
#[derive(Clone)]
struct Context {
    shared: Arc<Shared>,
    repos: Arc<Repos>,
    index: Index,
    #[cfg(test)]
    vault: std::path::PathBuf,
    #[cfg(feature = "index")]
    ports: Arc<Installed>,
}

struct Worker {
    context: Context,
    /// Taken by the task of a round, given back when it ends; lost with a
    /// round that panicked.
    kept: Option<Kept>,
}

impl Worker {
    async fn run(mut self, poll: Duration) {
        let shared = Arc::clone(&self.context.shared);
        let longest = poll.saturating_mul(8);
        let mut backoff = poll;
        let mut pause = Duration::ZERO;
        loop {
            let go = if pause.is_zero() {
                !shared.signal.stopped()
            } else {
                shared.signal.wait(pause).await
            };
            if !go {
                break;
            }
            // Read before the round reads the ports: every state the round
            // ends with reflects this change, or a later one.
            let changes = shared.signal.ports();
            let Some(outcome) = self.round(changes).await else {
                // The runtime shuts down.
                break;
            };
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
                    if shared.signal.stopped() {
                        break;
                    }
                    self.context.fail(&error, changes);
                    pause = backoff;
                    backoff = backoff.saturating_mul(2).min(longest);
                }
            }
        }
        shared.close();
    }

    /// One round, as a task of its own: a panic in it is a failure of the
    /// round, and the indexer goes on. `None` when the runtime shuts down.
    /// `changes` is the port change the states it ends with reflect.
    async fn round(&mut self, changes: u64) -> Option<Result<Next, Error>> {
        let context = self.context.clone();
        let mut kept = self.kept.take().unwrap_or_default();
        let task = tokio::spawn(async move {
            let outcome = context.round(&mut kept, changes).await;
            (kept, outcome)
        });
        match task.await {
            Ok((kept, outcome)) => {
                self.kept = Some(kept);
                Some(outcome)
            }
            Err(error) if error.is_panic() => Some(Err(panicked(error.into_panic().as_ref()))),
            Err(_) => None,
        }
    }
}

impl Context {
    /// One look at `HEAD` and, if the Index is behind, one round. Every
    /// state it ends with, whatever it found, says it reflects the port
    /// change `changes`, read before the round read the ports.
    async fn round(&self, kept: &mut Kept, changes: u64) -> Result<Next, Error> {
        // The ports of the whole round, taken once: a port set while it runs
        // is the next round's, never part of this one's work.
        #[cfg(feature = "index")]
        let calls = Calls::of(&self.ports);
        let look = {
            let context = self.clone();
            blocking(move |cx| context.look(cx)).await?
        };
        if look.current() {
            // An Index at `HEAD` may be of other ports than this Vault's: a
            // port set or removed since, others given at open, or another
            // Vault that moved the Index.
            #[cfg(feature = "index")]
            if !self.agrees(&look, kept, &calls, changes).await? {
                return Ok(Next::Idle);
            }
            self.follow(look.version(), look.head, changes);
            return Ok(Next::Idle);
        }
        #[cfg(feature = "index")]
        let Some(plan) = self.plan(&look, &calls, changes).await? else {
            return Ok(Next::Idle);
        };
        let index = self.index.clone();
        let Some(lock) = blocking(move |_cx| {
            let Some(lock) = index.try_lock()? else {
                return Ok(None);
            };
            // Only a round writes under the Index: with the lock held, a
            // temporary file there is one a dead process left.
            index.sweep_temporary(&lock)?;
            Ok(Some(lock))
        })
        .await?
        else {
            // Another indexer holds the lock; it will do the round. This
            // Vault's ports fit the Index: an earlier failure is over.
            self.leave_to_other(look.head, changes);
            return Ok(Next::Idle);
        };
        #[cfg(feature = "index")]
        let applied = self.apply(&look, plan, &calls, &mut kept.cache).await;
        #[cfg(not(feature = "index"))]
        let applied = {
            let _ = kept;
            self.apply(&look).await
        };
        // Lance writes its files with the process umask. The round narrows
        // them to the owner however it ends, done, failed, or stopped by
        // `Close`, while it still holds the lock; what a killed round left
        // is narrowed by the next one, which walks the whole Index.
        #[cfg(feature = "index")]
        let applied = {
            let root = self.index.root().to_path_buf();
            let narrowed = blocking(move |_cx| restrict_tree(&root)).await;
            applied.and_then(|applied| narrowed.map(|()| applied))
        };
        // The commits are written: the lock goes before anyone is told.
        drop(lock);
        let applied = applied?;
        let served = look.asked;
        self.shared.publish(|inner| {
            if served {
                inner.requested = false;
            }
            // The round read `HEAD` again for its work, after its look: the
            // state it brought the Index to is the newest stored state it
            // knows, so the Index is never shown ahead of it.
            inner.state.version = Some(applied.state.token());
            inner.state.stored_version = Some(applied.state.token());
            inner.state.pending = Some(0);
            inner.state.role = Some(false);
            inner.state.last_error = buffa::MessageField::none();
            inner.state.port_change = Some(changes);
            if let Some(cause) = applied.rebuilt {
                inner.running = false;
                inner.state.rebuild =
                    rebuild_of(rebuild::Stage::STAGE_DONE, applied.counts, Some(cause)).into();
            }
        });
        Ok(Next::Again)
    }

    /// What the Index and `HEAD` are at now.
    fn look(&self, _cx: &Blocking) -> Result<Look, Error> {
        #[cfg(feature = "index")]
        let (asked, asked_beside) = {
            let inner = self.shared.lock();
            (inner.requested, inner.requested_beside.clone())
        };
        #[cfg(not(feature = "index"))]
        let asked = self.shared.lock().requested;
        let format = self.index.has_format()?;
        let maps = if format {
            self.index.read_commit()?
        } else {
            None
        };
        let head = State::of(&self.repos.views().context("read HEAD")?);
        #[cfg(feature = "index")]
        let live = if format {
            match Layout::new(self.index.root()).current(_cx)? {
                Some(generation) => {
                    let commit = generation.read_commit(_cx)?;
                    Some((generation, commit))
                }
                None => None,
            }
        } else {
            None
        };
        Ok(Look {
            asked,
            #[cfg(feature = "index")]
            asked_beside,
            maps,
            head,
            #[cfg(feature = "index")]
            live,
        })
    }

    /// The plan of this Vault for the round, or `None` when it does not
    /// index: the Index was built with other ports than this Vault's, or
    /// the Vault lacks a port the Index needs. The status says which.
    #[cfg(feature = "index")]
    async fn plan(&self, look: &Look, calls: &Calls, changes: u64) -> Result<Option<Plan>, Error> {
        match self.verdict(look, calls).await? {
            Ok(plan) => Ok(Some(plan)),
            Err(report) => {
                self.refuse(look, &report, changes);
                Ok(None)
            }
        }
    }

    /// Whether this Vault indexes the Index that `look` found at `HEAD`;
    /// when it does not, the status says why, as [`Context::plan`] has it.
    /// The ports are compared with the Index again only when a port was
    /// set or removed, or the live generation or its `commit` changed,
    /// since the last comparison: a look that finds neither reads nothing
    /// more and asks the Projector nothing.
    #[cfg(feature = "index")]
    async fn agrees(
        &self,
        look: &Look,
        kept: &mut Kept,
        calls: &Calls,
        changes: u64,
    ) -> Result<bool, Error> {
        let Some((generation, Some(commit))) = &look.live else {
            // No Index to compare with: a round builds one.
            return Ok(true);
        };
        // `changes` was read before the round took its ports: a port set
        // meanwhile moves the count past it, so the next look compares
        // again.
        let again = kept.compared.as_ref().is_none_or(|last| {
            last.ports != changes || last.generation != generation.name() || last.commit != *commit
        });
        if again {
            let refused = self.verdict(look, calls).await?.err();
            kept.compared = Some(Compared {
                ports: changes,
                generation: generation.name().to_owned(),
                commit: commit.clone(),
                refused,
            });
        }
        let agrees = match kept
            .compared
            .as_ref()
            .and_then(|compared| compared.refused.as_ref())
        {
            Some(report) => {
                self.refuse(look, report, changes);
                false
            }
            None => true,
        };
        #[cfg(test)]
        hooks::count_look(&self.vault);
        Ok(agrees)
    }

    /// The plan of this Vault, with the ports `calls`, for the round, or
    /// the report of why it does not index: `INDEX_STALE` when the Index was
    /// built with other ports than these, `FAILED_PRECONDITION` naming the
    /// port the Vault lacks.
    ///
    /// # Errors
    ///
    /// The failure of git, of the Index, or of the Projector.
    #[cfg(feature = "index")]
    async fn verdict(
        &self,
        look: &Look,
        calls: &Calls,
    ) -> Result<Result<Plan, ErrorReport>, Error> {
        #[cfg(test)]
        hooks::count_verdict(&self.vault);
        let repos = Arc::clone(&self.repos);
        let head = look.head;
        let main = head.oid(Repo::Main);
        let declarations = blocking(move |_cx| {
            let Some(views) = repos.views_at(head.oid(Repo::Main), head.oid(Repo::Isolated))?
            else {
                return Err(Error::new(
                    VaultErrorCode::Aborted,
                    "the commit HEAD named is gone",
                ));
            };
            collection::load(views.of(Repo::Main)).context("read the collections")
        })
        .await?;
        let plan = tables::plan(calls, main, Arc::new(declarations)).await?;
        let stale = match look.commit() {
            Some(commit) if !look.asked => tables::stale(commit, &plan),
            _ => None,
        };
        let error: Error = match (stale, &plan.lacking) {
            (Some(reason), _) => Fault::new(VaultErrorCode::IndexStale, reason).into(),
            (None, Some(what)) => Fault::new(
                VaultErrorCode::FailedPrecondition,
                format!("this Vault lacks {what}"),
            )
            .into(),
            (None, None) => return Ok(Ok(plan)),
        };
        Ok(Err(error_report(&error)))
    }

    /// This Vault does not index the Index `look` found, for the reason
    /// `report` gives, with the ports of the port change `changes`: the
    /// status says so, unless it does already, or a rebuild was asked for
    /// since the look: the round that request wakes says what it finds, and
    /// a watcher that asked is not told a refusal the rebuild is there to
    /// end.
    #[cfg(feature = "index")]
    fn refuse(&self, look: &Look, report: &ErrorReport, changes: u64) {
        let version = look.version().map(State::token);
        let stored = Some(look.head.token());
        let asked = look.asked;
        self.shared.publish_if(|inner| {
            if !asked && inner.requested {
                return false;
            }
            if inner.state.last_error.as_option() == Some(report)
                && inner.state.version == version
                && inner.state.stored_version == stored
                && inner.state.port_change == Some(changes)
            {
                return false;
            }
            // Only a request this look saw is over: one asked since, by
            // `reindex`, stands for the next round.
            if asked {
                inner.requested = false;
            }
            inner.state.version = version;
            inner.state.stored_version = stored;
            inner.state.role = Some(false);
            inner.state.pending = None;
            inner.state.last_error = report.clone().into();
            inner.state.port_change = Some(changes);
            true
        });
    }

    /// The round itself, with `index.lock` held: the journal id maps, then
    /// the Lance tables, each with its commit last. Every Document is
    /// projected and embedded with `calls`, the ports `plan` was made with.
    #[cfg(feature = "index")]
    async fn apply(
        &self,
        look: &Look,
        plan: Plan,
        calls: &Calls,
        cache: &mut Cache,
    ) -> Result<Applied, Error> {
        #[cfg(test)]
        hooks::after_look(&self.vault).await;
        let layout = Layout::new(self.index.root());
        let cause = if look.asked {
            rebuild::Cause::CAUSE_REINDEX
        } else {
            rebuild::Cause::CAUSE_NEW
        };
        // A rebuild asked for is served already when another generation is
        // live than when it was asked for, built with this Vault's ports and
        // columns: one that another handle, asked too, built meanwhile.
        let served = look.asked && {
            let layout = layout.clone();
            let live = blocking(move |cx| match layout.current(cx)? {
                Some(generation) => Ok(generation
                    .read_commit(cx)?
                    .map(|commit| (generation.name().to_owned(), commit))),
                None => Ok(None),
            })
            .await?;
            live.is_some_and(|(name, commit)| {
                look.asked_beside.as_deref() != Some(name.as_str())
                    && commit.ports == plan.ports
                    && tables::stale(&commit, &plan).is_none()
            })
        };
        if served {
            self.shared.publish(|inner| {
                inner.running = true;
                inner.state.rebuild = rebuild_of(
                    rebuild::Stage::STAGE_RUNNING,
                    Counts::default(),
                    Some(cause),
                )
                .into();
            });
        }
        let asked = look.asked && !served;
        let maps = {
            let context = self.clone();
            blocking(move |cx| context.maps(cx, asked)).await?
        };
        let live = {
            let layout = layout.clone();
            blocking(move |cx| match layout.current(cx)? {
                Some(generation) => {
                    let commit = generation.read_commit(cx)?;
                    Ok(Some((generation, commit)))
                }
                None => Ok(None),
            })
            .await?
        };
        let rebuild = asked || maps.reset || !matches!(live, Some((_, Some(_))));
        // The plan is of the collections declared where the round brings
        // the Index: made again, with the same ports, when `HEAD` moved
        // since the look.
        let plan = if maps.head == look.head {
            plan
        } else {
            let live_commit = match (&live, rebuild) {
                (Some((_, Some(commit))), false) => Some(commit),
                _ => None,
            };
            usable(
                tables::plan(
                    calls,
                    maps.head.oid(Repo::Main),
                    Arc::clone(&maps.declarations),
                )
                .await?,
                live_commit,
            )?
        };
        if rebuild {
            self.shared.publish(|inner| {
                inner.running = true;
                inner.state.role = Some(true);
                inner.state.rebuild = rebuild_of(
                    rebuild::Stage::STAGE_RUNNING,
                    Counts::default(),
                    Some(cause),
                )
                .into();
            });
        }
        let (generation, commit) = match (&live, rebuild) {
            (Some((generation, Some(commit))), false) => {
                // Read after the round that wrote it ended: what it stopped
                // naming stopped being named before now.
                let mut commit = commit.clone();
                commit.stamp(generation::now_millis());
                (generation.clone(), commit)
            }
            _ => {
                let layout = layout.clone();
                let generation = blocking(move |cx| layout.create(cx)).await?;
                (generation, Commit::empty(plan.ports.clone()))
            }
        };
        // A rebuild with the embedder model and vector length of the live
        // generation takes the vectors it holds instead of embedding their
        // texts again.
        let earlier = match (&live, rebuild) {
            (Some((live, Some(commit))), true)
                if commit.ports.embedder.is_some()
                    && commit.ports.embedder == plan.ports.embedder =>
            {
                Some(tables::Earlier {
                    dir: live.dir().to_path_buf(),
                    commit: commit.clone(),
                })
            }
            _ => None,
        };
        let shared = Arc::clone(&self.shared);
        let stopped: Arc<dyn Fn() -> bool + Send + Sync> = {
            let shared = Arc::clone(&self.shared);
            Arc::new(move || shared.signal.stopped())
        };
        let mut last = Instant::now();
        let mut tick = move |counts: Counts| -> Result<(), Error> {
            if shared.signal.stopped() {
                return Err(Error::new(
                    RuntimeCode::Cancelled,
                    "the indexer is stopping",
                ));
            }
            if shared.lock().running && last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                shared.publish(|inner| {
                    inner.state.rebuild =
                        rebuild_of(rebuild::Stage::STAGE_RUNNING, counts, Some(cause)).into();
                });
            }
            Ok(())
        };
        let mut work = Work {
            repos: Arc::clone(&self.repos),
            generation: generation.clone(),
            commit,
            earlier,
            plan,
            calls,
            cache,
            tick: &mut tick,
            stopped,
            counts: Counts::default(),
        };
        let mut to = maps.head;
        work.bring_to(to).await?;
        if rebuild {
            // A rebuild catches up once, with the tree difference, to the
            // `HEAD` of the moment it is built, then switches: the writes
            // made while it was built appear at the switch, those made
            // while it caught up in the next round. Waiting for `HEAD` to
            // stand still would never switch under a steady stream of
            // writes. The journal id maps catch up first, and the tables
            // to the state they reached, so the whole Index is at one.
            let again = {
                let context = self.clone();
                blocking(move |cx| context.maps(cx, false)).await?
            };
            if again.head != to {
                work.plan = usable(
                    tables::plan(
                        calls,
                        again.head.oid(Repo::Main),
                        Arc::clone(&again.declarations),
                    )
                    .await?,
                    Some(&work.commit),
                )?;
                work.bring_to(again.head).await?;
                to = again.head;
            }
        }
        // The tables read every file the maps read: their counts are the
        // round's.
        let counts = work.counts;
        let mut commit = work.commit;
        if rebuild {
            commit.retired = live
                .as_ref()
                .map(|(generation, _)| generation::Retired::now(generation.name().to_owned()));
        }
        #[cfg(test)]
        hooks::before_state(&self.vault, || self.shared.signal.stopped()).await?;
        let state = commit.state().unwrap_or(to);
        blocking(move |cx| {
            generation.sweep_tables(cx, &mut commit);
            generation.write_commit(cx, &commit)?;
            if rebuild {
                layout.switch(cx, &generation)?;
            }
            layout.sweep(cx, Some(generation.name()), commit.retired.as_ref())
        })
        .await?;
        cache.clear();
        Ok(Applied {
            state,
            // A request another handle served read nothing here.
            counts: if rebuild { counts } else { Counts::default() },
            rebuilt: (rebuild || served).then_some(cause),
        })
    }

    /// The round itself, with `index.lock` held: the journal id maps, with
    /// their commit last.
    #[cfg(not(feature = "index"))]
    async fn apply(&self, look: &Look) -> Result<Applied, Error> {
        let maps = {
            let context = self.clone();
            let asked = look.asked;
            blocking(move |cx| context.maps(cx, asked)).await?
        };
        Ok(Applied {
            state: maps.head,
            counts: maps.counts,
            rebuilt: maps.rebuilt,
        })
    }

    /// The journal id maps brought to `HEAD`, under the lock: from nothing
    /// when a rebuild was asked for or there are none of this layout.
    fn maps(&self, _cx: &Blocking, asked: bool) -> Result<Maps, Error> {
        let format = self.index.has_format()?;
        let committed = if format {
            self.index.read_commit()?
        } else {
            None
        };
        let views = self.repos.views().context("read HEAD")?;
        let head = State::of(&views);
        let declarations =
            Arc::new(collection::load(views.of(Repo::Main)).context("read the collections")?);
        if !asked && committed == Some(head) {
            return Ok(Maps {
                head,
                #[cfg(not(feature = "index"))]
                counts: Counts::default(),
                #[cfg(feature = "index")]
                reset: false,
                #[cfg(not(feature = "index"))]
                rebuilt: None,
                #[cfg(feature = "index")]
                declarations,
            });
        }
        #[cfg(test)]
        let _in_round = hooks::enter(&self.vault)?;
        let cause = if asked {
            rebuild::Cause::CAUSE_REINDEX
        } else {
            rebuild::Cause::CAUSE_NEW
        };
        if !format {
            self.index.reset().context("empty the Index")?;
        } else if asked {
            self.index
                .reset_maps()
                .context("empty the journal id maps")?;
        }
        let from = if asked || !format { None } else { committed };
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
                    inner.state.rebuild =
                        rebuild_of(rebuild::Stage::STAGE_RUNNING, counts, Some(cause)).into();
                });
            }
            Ok(())
        };
        let mut round = match Round::new(&self.index, &views, &declarations, from, &mut tick)? {
            Some(round) => round,
            None => {
                // The commit the maps reflect is gone from its repository:
                // they are made again from nothing.
                self.index
                    .reset_maps()
                    .context("empty the journal id maps")?;
                match Round::new(&self.index, &views, &declarations, None, &mut tick)? {
                    Some(round) => round,
                    None => unreachable!("a round from nothing always knows its difference"),
                }
            }
        };
        let pending = round.pending();
        #[cfg(not(feature = "index"))]
        let rebuilt = round.is_rebuild().then_some(cause);
        self.shared.adjust(|inner| {
            inner.state.role = Some(true);
            inner.state.pending = pending;
        });
        #[cfg(not(feature = "index"))]
        if rebuilt.is_some() {
            self.shared.publish(|inner| {
                inner.running = true;
                inner.state.version = None;
                inner.state.rebuild = rebuild_of(
                    rebuild::Stage::STAGE_RUNNING,
                    Counts::default(),
                    Some(cause),
                )
                .into();
            });
        }
        #[cfg(test)]
        hooks::at_work(&self.vault)?;
        JournalMaps.apply(&mut round)?;
        #[cfg(not(feature = "index"))]
        let counts = round.counts();
        self.index
            .write_commit(head)
            .context("record the state of the journal id maps")?;
        drop(round);
        Ok(Maps {
            head,
            #[cfg(not(feature = "index"))]
            counts,
            #[cfg(feature = "index")]
            reset: !format,
            #[cfg(not(feature = "index"))]
            rebuilt,
            #[cfg(feature = "index")]
            declarations,
        })
    }

    /// Nothing is to be done by this handle: the Index is at `committed`
    /// (or not built), and either it is at `head`, the state of `HEAD` now,
    /// or another handle has to bring it there. Another indexer may have
    /// moved it, and an earlier failure is over. A look made after the port
    /// change `changes` found it so.
    fn follow(&self, committed: Option<State>, head: State, changes: u64) {
        let token = committed.map(State::token);
        let head = Some(head.token());
        let pending = committed.map(|_| 0);
        let differs = {
            let inner = self.shared.lock();
            inner.state.version != token
                || inner.state.stored_version != head
                || inner.state.last_error.is_set()
                || inner.state.role != Some(false)
                || inner.state.pending != pending
                || inner.state.port_change != Some(changes)
        };
        if differs {
            self.shared.publish(|inner| {
                inner.state.version = token;
                inner.state.stored_version = head;
                inner.state.pending = pending;
                inner.state.role = Some(false);
                inner.state.last_error = buffa::MessageField::none();
                inner.state.port_change = Some(changes);
            });
        }
    }

    /// The Index is behind `head`, this handle's ports of the port change
    /// `changes` fit it, and another indexer holds the lock: that one
    /// brings it there. The status says so, as an item when it tells a
    /// watcher something new: a failure is over, or the port change moved.
    fn leave_to_other(&self, head: State, changes: u64) {
        let edit = |inner: &mut Inner| {
            inner.state.role = Some(false);
            inner.state.stored_version = Some(head.token());
            inner.state.last_error = buffa::MessageField::none();
            inner.state.port_change = Some(changes);
        };
        let news = {
            let inner = self.shared.lock();
            inner.state.last_error.is_set() || inner.state.port_change != Some(changes)
        };
        if news {
            self.shared.publish(edit);
        } else {
            self.shared.adjust(edit);
        }
    }

    /// A round that began after the port change `changes` failed: the
    /// status says so.
    fn fail(&self, error: &Error, changes: u64) {
        let report = error_report(error);
        self.shared.publish(|inner| {
            let rebuilding = std::mem::take(&mut inner.running);
            inner.state.role = Some(false);
            inner.state.last_error = report.clone().into();
            inner.state.port_change = Some(changes);
            if rebuilding {
                let cause = inner
                    .state
                    .rebuild
                    .as_option()
                    .and_then(|rebuild| rebuild.cause)
                    .and_then(|cause| cause.as_known());
                let mut failed = rebuild_of(rebuild::Stage::STAGE_FAILED, Counts::default(), cause);
                failed.error = report.into();
                inner.state.rebuild = failed.into();
            }
        });
    }
}

/// What the journal id maps' part of a round did.
struct Maps {
    /// The state they reflect now.
    head: State,
    /// What they read: the round's counts without the tables.
    #[cfg(not(feature = "index"))]
    counts: Counts,
    /// Whether the Index was of another layout and was emptied whole.
    #[cfg(feature = "index")]
    reset: bool,
    /// Why they were made from nothing, when they were.
    #[cfg(not(feature = "index"))]
    rebuilt: Option<rebuild::Cause>,
    /// The collections declared at `head`.
    #[cfg(feature = "index")]
    declarations: Arc<Declarations>,
}

/// `plan`, when a round can bring the Index forward with it: the Vault
/// lacks no port, and `commit`, the Index it brings forward, was built
/// with the same ports and columns.
///
/// # Errors
///
/// `FAILED_PRECONDITION` naming the port the Vault lacks; `INDEX_STALE`.
#[cfg(feature = "index")]
fn usable(plan: Plan, commit: Option<&Commit>) -> Result<Plan, Error> {
    if let Some(what) = &plan.lacking {
        return Err(Fault::new(
            VaultErrorCode::FailedPrecondition,
            format!("this Vault lacks {what}"),
        )
        .into());
    }
    if let Some(reason) = commit.and_then(|commit| tables::stale(commit, &plan)) {
        return Err(Fault::new(VaultErrorCode::IndexStale, reason).into());
    }
    Ok(plan)
}

fn rebuild_of(stage: rebuild::Stage, counts: Counts, cause: Option<rebuild::Cause>) -> Rebuild {
    let rebuild = Rebuild::default()
        .with_stage(stage)
        .with_documents_read(counts.documents_read)
        .with_documents_skipped(counts.documents_skipped)
        .with_segments_read(counts.segments_read);
    match cause {
        Some(cause) => rebuild.with_cause(cause),
        None => rebuild,
    }
}

/// The error of a round that panicked.
fn panicked(payload: &(dyn std::any::Any + Send)) -> Error {
    Error::new(
        RuntimeCode::Panicked,
        format!(
            "an indexing round panicked: {}",
            scheda_blocking::panic_message(payload)
        ),
    )
}

/// What the unit tests do to a round: stretch it, count the rounds at work
/// at once, make it panic, hold it after its look.
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
        #[cfg(feature = "index")]
        held: bool,
        #[cfg(feature = "index")]
        waiting: bool,
        #[cfg(feature = "index")]
        verdicts: usize,
        #[cfg(feature = "index")]
        looks: usize,
        #[cfg(feature = "index")]
        stop_before_state: bool,
        #[cfg(feature = "index")]
        stopped_before_state: bool,
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

    /// The rounds on `vault` wait after their look, with the lock taken,
    /// until [`release`].
    #[cfg(feature = "index")]
    pub(crate) fn hold(vault: &Path) {
        with(vault, |hook| hook.held = true);
    }

    /// The rounds on `vault` go on.
    #[cfg(feature = "index")]
    pub(crate) fn release(vault: &Path) {
        with(vault, |hook| hook.held = false);
    }

    /// Whether a round on `vault` waits at the hold.
    #[cfg(feature = "index")]
    pub(crate) fn waiting(vault: &Path) -> bool {
        with(vault, |hook| hook.waiting)
    }

    /// How many times the indexer on `vault` compared its ports with the
    /// Index.
    #[cfg(feature = "index")]
    pub(crate) fn verdicts(vault: &Path) -> usize {
        with(vault, |hook| hook.verdicts)
    }

    /// The indexer on `vault` compares its ports with the Index.
    #[cfg(feature = "index")]
    pub(crate) fn count_verdict(vault: &Path) {
        with(vault, |hook| hook.verdicts += 1);
    }

    /// How many looks of the indexer on `vault` at an Index at `HEAD` are
    /// done, the status set to what each found.
    #[cfg(feature = "index")]
    pub(crate) fn looks(vault: &Path) -> usize {
        with(vault, |hook| hook.looks)
    }

    /// A look of the indexer on `vault` at an Index at `HEAD` is done.
    #[cfg(feature = "index")]
    pub(crate) fn count_look(vault: &Path) {
        with(vault, |hook| hook.looks += 1);
    }

    /// Where a held round waits: after its look and its plan, before it
    /// reads `HEAD` again for its work.
    #[cfg(feature = "index")]
    pub(crate) async fn after_look(vault: &Path) {
        while with(vault, |hook| {
            hook.waiting = hook.held;
            hook.held
        }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// The next round on `vault` that has made its Lance versions stops
    /// there, before it writes the `commit` that names them, until the
    /// indexer stops: the state a crash at that moment leaves.
    #[cfg(feature = "index")]
    pub(crate) fn stop_before_state(vault: &Path) {
        with(vault, |hook| {
            hook.stop_before_state = true;
            hook.stopped_before_state = false;
        });
    }

    /// Whether a round on `vault` waits at [`stop_before_state`].
    #[cfg(feature = "index")]
    pub(crate) fn stopped_before_state(vault: &Path) -> bool {
        with(vault, |hook| hook.stopped_before_state)
    }

    /// Where [`stop_before_state`] stops a round; the round ends without
    /// its `commit` once `stopped` says the indexer stops.
    #[cfg(feature = "index")]
    pub(crate) async fn before_state(
        vault: &Path,
        stopped: impl Fn() -> bool,
    ) -> Result<(), Error> {
        let armed = with(vault, |hook| {
            let armed = std::mem::take(&mut hook.stop_before_state);
            hook.stopped_before_state |= armed;
            armed
        });
        if !armed {
            return Ok(());
        }
        while !stopped() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Err(Error::new(
            protocontract::RuntimeCode::Cancelled,
            "the indexer is stopping",
        ))
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

    use protocontract::RuntimeCode;
    use scheda_api::v1::{DocumentCreation, IndexState, OpenOptions, VaultErrorCode};

    use super::{HISTORY, Indexer, Signal, Watch, hooks};
    use crate::index::Index;
    use crate::testing::{create, create_collection, open};
    use crate::vault::Vault;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-indexer-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            crate::testing::remove_when_the_test_ends(&dir);
            Scratch(dir)
        }

        async fn open(&self) -> Arc<Vault> {
            open(&self.0, OpenOptions::default().with_index_poll_ms(10))
                .await
                .unwrap()
        }

        fn canonical(&self) -> PathBuf {
            crate::fs::canonicalize(&self.0).unwrap()
        }
    }

    async fn journal(vault: &Arc<Vault>, name: &str) {
        create_collection(
            vault,
            serde_json::json!({"name": name, "schema": {"journal": {}}}),
        )
        .await
        .unwrap();
    }

    async fn line(vault: &Arc<Vault>, collection: &str, id: &str) {
        create(
            vault,
            collection,
            DocumentCreation {
                id: Some(id.to_owned()),
                raw: Some(b"{}".to_vec()),
                note: Some("test".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }

    fn head(path: &Path) -> String {
        let index = Index::new(path);
        let views = crate::repo::Repos::new(path).views().unwrap();
        let _ = index;
        crate::index::State::of(&views).token()
    }

    /// Waits until `done` says so, or fails the test after 20 seconds.
    async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn at_head(vault: &Vault, path: &Path) -> bool {
        let state = vault.indexer().status();
        state.version.as_deref() == Some(head(path).as_str())
            && state.pending == Some(0)
            && state.role != Some(true)
    }

    #[tokio::test]
    async fn the_watch_gives_the_state_at_once_and_then_one_item_per_round() {
        let scratch = Scratch::new("watch");
        let vault = scratch.open().await;
        journal(&vault, "log").await;
        let path = scratch.canonical();
        wait_until("the first build", || at_head(&vault, &path)).await;
        let mut watch = vault.indexer().watch();
        let first = watch.next().await.unwrap().unwrap();
        assert_eq!(first.version.as_deref(), Some(head(&path).as_str()));
        assert_eq!(first.stored_version, first.version, "the Index is at HEAD");
        line(&vault, "log", "one").await;
        let moved = watch.next().await.unwrap().unwrap();
        assert_eq!(moved.version.as_deref(), Some(head(&path).as_str()));
        assert_eq!(moved.stored_version.as_deref(), Some(head(&path).as_str()));
        assert_eq!(moved.pending, Some(0));
        assert_eq!(moved.role, Some(false));
        assert!(!moved.last_error.is_set());
        // No further write, no further item.
        assert!(at_head(&vault, &path));
        vault.close().await.unwrap();
        assert!(
            watch.next().await.is_none(),
            "the stream ends with the handle"
        );
    }

    #[tokio::test]
    async fn a_watcher_that_falls_far_behind_skips_to_the_oldest_item_kept() {
        let indexer = Indexer::new(Arc::new(Signal::default()), Path::new("unused"));
        let mut watch = indexer.watch();
        assert!(watch.next().await.unwrap().unwrap().version.is_none());
        for n in 0..HISTORY + 10 {
            indexer
                .shared
                .publish(|inner| inner.state.pending = Some(n as u64));
        }
        let oldest = watch.next().await.unwrap().unwrap();
        assert_eq!(oldest.pending, Some(10));
        assert_eq!(
            watch.next().await.unwrap().unwrap().pending,
            Some(11),
            "then one by one"
        );
    }

    #[tokio::test]
    async fn the_rounds_of_two_handles_of_one_vault_never_overlap() {
        let scratch = Scratch::new("overlap");
        let one = scratch.open().await;
        journal(&one, "log").await;
        let path = scratch.canonical();
        wait_until("the first build", || at_head(&one, &path)).await;
        hooks::stretch(&path, Duration::from_millis(30));
        let two = scratch.open().await;
        for n in 0..6 {
            line(&one, "log", &format!("a{n}")).await;
            line(&two, "log", &format!("b{n}")).await;
        }
        wait_until("the first handle", || at_head(&one, &path)).await;
        wait_until("the second handle", || at_head(&two, &path)).await;
        assert_eq!(hooks::most_at_once(&path), 1);
        assert_eq!(
            one.indexer().status().version,
            two.indexer().status().version
        );
        assert!(one.indexer().status().last_error.as_option().is_none());
        assert!(two.indexer().status().last_error.as_option().is_none());
    }

    /// The first item of `watch` that reports a failed round, waited for as
    /// long as [`wait_until`] waits. The status itself may have moved on by
    /// then: the retry starts one poll later.
    async fn failure(watch: &mut Watch) -> IndexState {
        let wait = async {
            loop {
                let state = watch.next().await.unwrap().unwrap();
                if state.last_error.is_set() {
                    return state;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(20), wait)
            .await
            .expect("timed out waiting for a failed round")
    }

    #[tokio::test]
    async fn a_round_that_fails_or_panics_is_reported_and_tried_again() {
        let scratch = Scratch::new("fail");
        let vault = scratch.open().await;
        journal(&vault, "log").await;
        let path = scratch.canonical();
        wait_until("the first build", || at_head(&vault, &path)).await;
        let index = Index::new(&path);
        let before = index.read_commit().unwrap();
        hooks::fail_once(&path);
        // The first item is the state when it is read: read before the
        // write, so the item of the failed round comes after it, in order,
        // and not merged into the state of the retry.
        let mut watch = vault.indexer().watch();
        watch.next().await.unwrap().unwrap();
        line(&vault, "log", "one").await;
        let state = failure(&mut watch).await;
        let error = state.last_error.as_option().unwrap();
        assert_eq!(error.code.as_deref(), Some("VAULT_ERROR_CODE_STORAGE"));
        assert_eq!(state.role, Some(false));
        // The write was not failed; the thread goes on and the next round
        // succeeds.
        wait_until("the retry", || at_head(&vault, &path)).await;
        assert!(!vault.indexer().status().last_error.is_set());
        assert_ne!(index.read_commit().unwrap(), before);

        hooks::panic_once(&path);
        let mut watch = vault.indexer().watch();
        watch.next().await.unwrap().unwrap();
        line(&vault, "log", "two").await;
        let state = failure(&mut watch).await;
        let error = state.last_error.as_option().unwrap();
        assert_eq!(error.code.as_deref(), Some("PANICKED"));
        assert!(error.message.as_deref().unwrap_or("").contains("panicked"));
        wait_until("the retry", || at_head(&vault, &path)).await;
        assert!(vault.indexer().is_running());
    }

    #[tokio::test]
    async fn stop_reports_a_cancelled_task_as_cancelled_and_a_panicked_one_as_panicked() {
        let scratch = Scratch::new("stop-ended");
        for (name, ends) in [("cancelled", false), ("panicked", true)] {
            let indexer = Indexer::new(Arc::new(Signal::default()), &scratch.0.join(name));
            let task = if ends {
                tokio::spawn(async { std::panic::panic_any(String::from("the task panics")) })
            } else {
                tokio::spawn(std::future::pending())
            };
            if !ends {
                task.abort();
            }
            *indexer.task.lock().unwrap() = Some(task);
            let error = indexer.stop().await.unwrap_err();
            assert_eq!(error.is(RuntimeCode::Cancelled), !ends, "{name}: {error}");
            assert_eq!(error.is(RuntimeCode::Panicked), ends, "{name}: {error}");
        }
    }

    #[tokio::test]
    async fn a_rebuild_waits_for_the_lock_and_a_second_request_is_aborted() {
        let scratch = Scratch::new("rebuild");
        let vault = scratch.open().await;
        journal(&vault, "log").await;
        line(&vault, "log", "one").await;
        let path = scratch.canonical();
        wait_until("the first build", || at_head(&vault, &path)).await;
        let held = Index::new(&path).try_lock().unwrap().unwrap();
        let mut watch = vault.indexer().watch();
        watch.next().await.unwrap().unwrap();
        vault.indexer().reindex().await.unwrap();
        let error = vault.indexer().reindex().await.unwrap_err();
        assert!(error.is(VaultErrorCode::Aborted));
        // A margin: a rebuild that waits for the lock shows no sign, so
        // only time lets a wrongly begun rebuild show itself. A slow runner
        // makes this check weaker, and never fails it.
        tokio::time::sleep(Duration::from_millis(60)).await;
        let state = vault.indexer().status();
        assert_eq!(
            state
                .rebuild
                .as_option()
                .and_then(|rebuild| rebuild.stage)
                .and_then(|stage| stage.as_known()),
            Some(scheda_api::v1::rebuild::Stage::STAGE_DONE),
            "the lock is held: the rebuild has not begun, the first build is the last: {state:?}"
        );
        assert!(state.version.is_some());
        assert_eq!(
            state
                .rebuild
                .as_option()
                .and_then(|rebuild| rebuild.cause)
                .and_then(|cause| cause.as_known()),
            Some(scheda_api::v1::rebuild::Cause::CAUSE_NEW),
            "the first build began for want of an Index"
        );
        drop(held);
        let mut stages = Vec::new();
        let last: IndexState = loop {
            let state = watch.next().await.unwrap().unwrap();
            let stage = state
                .rebuild
                .as_option()
                .and_then(|rebuild| rebuild.stage)
                .and_then(|stage| stage.as_known());
            stages.push(stage);
            // The live Index answers while a rebuild runs (its version
            // stays): the rebuild is over when its stage says so.
            if matches!(
                stage,
                Some(
                    scheda_api::v1::rebuild::Stage::STAGE_DONE
                        | scheda_api::v1::rebuild::Stage::STAGE_FAILED
                )
            ) && stages.contains(&Some(scheda_api::v1::rebuild::Stage::STAGE_RUNNING))
            {
                break state;
            }
        };
        assert!(stages.contains(&Some(scheda_api::v1::rebuild::Stage::STAGE_RUNNING)));
        let rebuild = last.rebuild.as_option().unwrap();
        assert_eq!(
            rebuild.stage.and_then(|stage| stage.as_known()),
            Some(scheda_api::v1::rebuild::Stage::STAGE_DONE)
        );
        assert_eq!(
            rebuild.cause.and_then(|cause| cause.as_known()),
            Some(scheda_api::v1::rebuild::Cause::CAUSE_REINDEX)
        );
        assert_eq!(rebuild.documents_read, Some(1));
        assert_eq!(rebuild.segments_read, Some(1));
        // A new request is accepted after the rebuild.
        vault.indexer().reindex().await.unwrap();
    }

    #[tokio::test]
    async fn a_round_removes_the_temporary_files_a_dead_writer_left_under_the_index() {
        let scratch = Scratch::new("sweep");
        let vault = scratch.open().await;
        journal(&vault, "log").await;
        line(&vault, "log", "one").await;
        let path = scratch.canonical();
        wait_until("the first build", || at_head(&vault, &path)).await;
        let index = Index::new(&path);
        #[cfg(feature = "index")]
        let live = {
            let root = index.root().to_path_buf();
            scheda_blocking::blocking(move |cx| {
                Ok(crate::generation::Layout::new(&root)
                    .current(cx)?
                    .map(|generation| generation.dir().to_path_buf()))
            })
            .await
            .unwrap()
        };
        #[cfg(not(feature = "index"))]
        let live: Option<PathBuf> = None;
        let left: Vec<PathBuf> = [
            index.root().to_path_buf(),
            index
                .ids_file("log")
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf(),
        ]
        .into_iter()
        .chain(live)
        .map(|dir| dir.join(".scheda.4194304.0.tmp"))
        .collect();
        for file in &left {
            std::fs::write(file, b"left").unwrap();
        }
        let kept = index.root().join("notes.tmp");
        std::fs::write(&kept, b"kept").unwrap();
        line(&vault, "log", "two").await;
        wait_until("the round", || at_head(&vault, &path)).await;
        for file in &left {
            assert!(!file.exists(), "{} is left", file.display());
        }
        assert!(kept.exists());
        vault.close().await.unwrap();
    }

    /// A Projector of the projection it is given, with no columns.
    #[cfg(feature = "index")]
    struct Bare(&'static str);

    #[cfg(feature = "index")]
    impl scheda_api::v1::ProjectorSync for Bare {
        fn describe(
            &self,
            _empty: buffa_types::google::protobuf::Empty,
        ) -> Result<scheda_api::v1::ProjectorDescriptor, crate::error::Error> {
            Ok(scheda_api::v1::ProjectorDescriptor::default().with_projection_id(self.0))
        }

        fn columns(
            &self,
            _collection: scheda_api::v1::CollectionKey,
        ) -> Result<scheda_api::v1::IndexSchema, crate::error::Error> {
            Ok(scheda_api::v1::IndexSchema::default())
        }

        fn project(
            &self,
            _document: scheda_api::v1::RawDocument,
        ) -> Result<scheda_api::v1::IndexDoc, crate::error::Error> {
            Ok(scheda_api::v1::IndexDoc::default())
        }
    }

    /// Waits until a whole look of the indexer on `path` at its Index at
    /// `HEAD` began after this call, so it saw the ports set and the state
    /// stored before it, and the status says what it found. The look under
    /// way when this is called may have begun before.
    #[cfg(feature = "index")]
    async fn looked_since(path: &Path) {
        let from = hooks::looks(path);
        wait_until("a look begun since", || hooks::looks(path) >= from + 2).await;
    }

    /// Lets the indexer on `path` look many times with nothing changed,
    /// and checks that no look compared the ports with the Index.
    #[cfg(feature = "index")]
    async fn idle(path: &Path) {
        let verdicts = hooks::verdicts(path);
        // Twenty looks or so. A slow runner makes this check weaker, and
        // never fails it.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            hooks::verdicts(path),
            verdicts,
            "an idle look compared the ports"
        );
    }

    #[cfg(feature = "index")]
    #[tokio::test(flavor = "multi_thread")]
    async fn the_ports_are_compared_with_an_index_at_head_after_a_port_change_and_not_at_an_idle_look()
     {
        use scheda_api::v1::projector_ref::ProjectorRef;

        let scratch = Scratch::new("compared");
        let projector = |projection| Some(ProjectorRef::from_sync(Bare(projection)));
        let vault = crate::testing::open_with(
            &scratch.0,
            OpenOptions::default().with_index_poll_ms(10),
            crate::ports::Ports {
                projector: projector("bare@1"),
                ..crate::ports::Ports::default()
            },
        )
        .await
        .unwrap();
        journal(&vault, "log").await;
        line(&vault, "log", "one").await;
        let path = scratch.canonical();
        wait_until("the first build", || at_head(&vault, &path)).await;
        looked_since(&path).await;
        idle(&path).await;
        let code = |vault: &Vault| {
            vault
                .indexer()
                .status()
                .last_error
                .as_option()
                .and_then(|report| report.code.clone())
        };

        // Stale: said once, and not compared again while nothing changes.
        vault.set_projector(projector("bare@2")).await.unwrap();
        looked_since(&path).await;
        assert_eq!(
            code(&vault).as_deref(),
            Some("VAULT_ERROR_CODE_INDEX_STALE")
        );
        idle(&path).await;

        // Fresh again, the same way.
        vault.set_projector(projector("bare@1")).await.unwrap();
        looked_since(&path).await;
        assert_eq!(code(&vault), None);
        assert!(at_head(&vault, &path));
        idle(&path).await;

        // The same port set again is compared again.
        let verdicts = hooks::verdicts(&path);
        vault.set_projector(projector("bare@1")).await.unwrap();
        looked_since(&path).await;
        assert!(hooks::verdicts(&path) > verdicts);
        assert_eq!(code(&vault), None);
        idle(&path).await;
        vault.close().await.unwrap();
    }

    /// A Projector of the projection it is given, with one text column,
    /// that counts the Documents it projects.
    #[cfg(feature = "index")]
    struct Counted(&'static str, Arc<std::sync::atomic::AtomicUsize>);

    #[cfg(feature = "index")]
    impl scheda_api::v1::ProjectorSync for Counted {
        fn describe(
            &self,
            _empty: buffa_types::google::protobuf::Empty,
        ) -> Result<scheda_api::v1::ProjectorDescriptor, crate::error::Error> {
            Ok(scheda_api::v1::ProjectorDescriptor::default().with_projection_id(self.0))
        }

        fn columns(
            &self,
            _collection: scheda_api::v1::CollectionKey,
        ) -> Result<scheda_api::v1::IndexSchema, crate::error::Error> {
            let title = scheda_api::v1::IndexColumn::default()
                .with_name("title")
                .with_kind(scheda_api::v1::ColumnKind::COLUMN_KIND_TEXT);
            Ok(scheda_api::v1::IndexSchema {
                columns: vec![title],
                ..scheda_api::v1::IndexSchema::default()
            })
        }

        fn project(
            &self,
            _document: scheda_api::v1::RawDocument,
        ) -> Result<scheda_api::v1::IndexDoc, crate::error::Error> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(scheda_api::v1::IndexDoc::default())
        }
    }

    /// A Projector of `bare@1` that cannot answer for columns now.
    #[cfg(feature = "index")]
    struct Down;

    #[cfg(feature = "index")]
    impl scheda_api::v1::ProjectorSync for Down {
        fn describe(
            &self,
            _empty: buffa_types::google::protobuf::Empty,
        ) -> Result<scheda_api::v1::ProjectorDescriptor, crate::error::Error> {
            Ok(scheda_api::v1::ProjectorDescriptor::default().with_projection_id("bare@1"))
        }

        fn columns(
            &self,
            _collection: scheda_api::v1::CollectionKey,
        ) -> Result<scheda_api::v1::IndexSchema, crate::error::Error> {
            Err(protocontract::Failure::new(
                scheda_api::v1::ProjectorErrorCode::PROJECTOR_ERROR_CODE_UNAVAILABLE,
                "down",
            )
            .into())
        }

        fn project(
            &self,
            _document: scheda_api::v1::RawDocument,
        ) -> Result<scheda_api::v1::IndexDoc, crate::error::Error> {
            Ok(scheda_api::v1::IndexDoc::default())
        }
    }

    /// The code of the failure the state `state` reports.
    #[cfg(feature = "index")]
    fn error_code(state: &IndexState) -> Option<String> {
        state
            .last_error
            .as_option()
            .and_then(|report| report.code.clone())
    }

    /// The first state of the indexer of `vault` that reflects the port
    /// change `number`, on a watch begun now; within 20 seconds.
    #[cfg(feature = "index")]
    async fn reflecting(vault: &Vault, number: u64) -> IndexState {
        let mut watch = vault.indexer().watch();
        let wait = async {
            loop {
                let state = watch.next().await.unwrap().unwrap();
                if state.port_change >= Some(number) {
                    return state;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(20), wait)
            .await
            .expect("no state reflects the port change")
    }

    #[cfg(feature = "index")]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_port_set_during_a_round_is_reflected_after_it_and_never_used_by_it() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use scheda_api::v1::projector_ref::ProjectorRef;

        let scratch = Scratch::new("set-in-round");
        let (first, second) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let projector = |projection, count: &Arc<AtomicUsize>| {
            Some(ProjectorRef::from_sync(Counted(
                projection,
                Arc::clone(count),
            )))
        };
        let vault = crate::testing::open_with(
            &scratch.0,
            OpenOptions::default().with_index_poll_ms(10),
            crate::ports::Ports {
                projector: projector("counted@1", &first),
                ..crate::ports::Ports::default()
            },
        )
        .await
        .unwrap();
        create_collection(
            &vault,
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .await
        .unwrap();
        let path = scratch.canonical();
        wait_until("the first build", || at_head(&vault, &path)).await;
        assert_eq!(vault.indexer().status().port_change, Some(0));

        // A round held after its look, its plan made with `counted@1`.
        hooks::hold(&path);
        create(
            &vault,
            "notes",
            DocumentCreation {
                id: Some("one".to_owned()),
                raw: Some(b"{}".to_vec()),
                note: Some("test".to_owned()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        wait_until("a held round", || hooks::waiting(&path)).await;
        let number = vault
            .set_projector(projector("counted@2", &second))
            .await
            .unwrap();
        assert_eq!(number, 1);
        // The round under way does not reflect the change, however long it
        // runs.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let state = vault.indexer().status();
        assert!(state.port_change < Some(number), "{state:?}");
        hooks::release(&path);

        // It ends with its own ports; the look after it compares the new
        // one.
        let state = reflecting(&vault, number).await;
        assert_eq!(state.version.as_deref(), Some(head(&path).as_str()));
        assert_eq!(
            error_code(&state).as_deref(),
            Some("VAULT_ERROR_CODE_INDEX_STALE"),
            "{state:?}"
        );
        assert_eq!(
            first.load(Ordering::SeqCst),
            1,
            "projected by its plan's port"
        );
        assert_eq!(
            second.load(Ordering::SeqCst),
            0,
            "the new port projected nothing"
        );
        vault.close().await.unwrap();
    }

    #[cfg(feature = "index")]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_round_that_fails_after_a_port_change_reflects_it_with_its_failure() {
        use scheda_api::v1::projector_ref::ProjectorRef;

        let scratch = Scratch::new("change-fails");
        let vault = crate::testing::open_with(
            &scratch.0,
            OpenOptions::default().with_index_poll_ms(10),
            crate::ports::Ports {
                projector: Some(ProjectorRef::from_sync(Bare("bare@1"))),
                ..crate::ports::Ports::default()
            },
        )
        .await
        .unwrap();
        journal(&vault, "log").await;
        line(&vault, "log", "one").await;
        let path = scratch.canonical();
        wait_until("the first build", || at_head(&vault, &path)).await;

        // The comparison cannot be made: the state says why, and the wait
        // for the change ends.
        let number = vault
            .set_projector(Some(ProjectorRef::from_sync(Down)))
            .await
            .unwrap();
        let state = reflecting(&vault, number).await;
        assert_eq!(
            error_code(&state).as_deref(),
            Some("PROJECTOR_ERROR_CODE_UNAVAILABLE"),
            "{state:?}"
        );

        // The Index's Projector again: no failure.
        let number = vault
            .set_projector(Some(ProjectorRef::from_sync(Bare("bare@1"))))
            .await
            .unwrap();
        let state = reflecting(&vault, number).await;
        assert_eq!(error_code(&state), None, "{state:?}");
        assert!(at_head(&vault, &path));
        vault.close().await.unwrap();
    }

    #[tokio::test]
    async fn close_and_drop_end_the_thread() {
        let scratch = Scratch::new("stop");
        let one = scratch.open().await;
        let two = scratch.open().await;
        assert!(one.indexer().is_running());
        let shared = Arc::clone(&two.indexer().shared);
        one.close().await.unwrap();
        assert!(!shared.lock().closed, "the other handle is not closed");
        drop(two);
        // The task ends after its current round, on its own.
        wait_until("the task of the dropped handle", || shared.lock().closed).await;
        assert!(shared.lock().closed, "a dropped handle stops its thread");
    }
}
