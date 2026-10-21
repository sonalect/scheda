//! The objects on the runtimes a caller may have: a current-thread one and
//! a multi-thread one, a blocking pool of one thread, a caller that drops a
//! call midway, and a caller without async code of its own, who wraps a
//! Vault in protocontract's `Blocking`.

use std::fs::File;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use scheda::v1::document_operation::Operation;
use scheda::v1::document_selection::Selection;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, CollectionDeletion, CollectionKey, DocumentCreation,
    DocumentId, DocumentOperation, DocumentSelection, DocumentUpdate, DocumentUpsert, Listing,
    MirrorAsync, OpenOptions, SchedaAsync, VaultAsync, VaultDocumentOperation,
    VaultErrorCode as Code, VaultOpening,
};
use scheda::{Error, Scheda};
use serde_json::json;
use tokio::runtime::{Builder, Runtime};

use crate::common::{
    Scratch, all, both, collection, command, file_url, git, mirror, open, opening, snapshot,
    stream_of, vault as built_vault, write_waits_for_the_lock,
};

/// How long a step may take before the test gives up on it.
const DEADLINE: Duration = Duration::from_secs(60);

/// A runtime of `workers` worker threads (a current-thread runtime for 0)
/// whose blocking pool has `pool` threads.
fn runtime(workers: usize, pool: usize) -> Runtime {
    let mut builder = if workers == 0 {
        Builder::new_current_thread()
    } else {
        let mut builder = Builder::new_multi_thread();
        builder.worker_threads(workers);
        builder
    };
    builder
        .max_blocking_threads(pool)
        .enable_all()
        .build()
        .unwrap()
}

fn options() -> OpenOptions {
    OpenOptions::default()
        .with_index_poll_ms(10)
        .with_write_timeout_ms(20_000)
}

/// A scratch directory with the Vault of the other tests built in it.
fn fresh(name: &str) -> (Scratch, PathBuf) {
    let scratch = Scratch::new(name);
    let path = scratch.join("v");
    built_vault(&path);
    (scratch, path)
}

fn creation(id: &str, raw: &str) -> DocumentCreation {
    DocumentCreation::default()
        .with_id(id)
        .with_raw(raw.as_bytes().to_vec())
        .with_note("test")
}

fn upsert(id: &str, raw: &str) -> DocumentUpsert {
    DocumentUpsert::default()
        .with_id(id)
        .with_raw(raw.as_bytes().to_vec())
        .with_note("test")
}

fn id(id: &str) -> DocumentId {
    DocumentId::default().with_id(id)
}

fn listing() -> DocumentSelection {
    DocumentSelection {
        include_raw: Some(true),
        selection: Some(Selection::Listing(Box::<Listing>::default())),
        ..DocumentSelection::default()
    }
}

fn declaration(name: &str) -> CollectionDeclaration {
    serde_json::from_value(json!({"name": name, "schema": {"standard": {}}})).unwrap()
}

fn operation(operation: Operation) -> DocumentOperation {
    DocumentOperation {
        operation: Some(operation),
        ..DocumentOperation::default()
    }
}

/// `HEAD` of the Vault at `path`: the state token of a Vault that holds no
/// isolated Document.
fn head(path: &Path) -> String {
    rev_parse(path, "HEAD").unwrap()
}

/// What `git rev-parse what` prints in `dir`, `None` when it fails.
fn rev_parse(dir: &Path, what: &str) -> Option<String> {
    let out = command(dir, &["rev-parse", "--verify", "--quiet", what])
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap().trim().to_owned())
}

/// Takes the write lock of the Vault at `path` as another process would.
fn hold_write_lock(path: &Path) -> File {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.join(".scheda/commit.lock"))
        .unwrap();
    file.lock().unwrap();
    file
}

/// Runs `future` with the time the test allows it.
async fn within<T>(what: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(DEADLINE, future)
        .await
        .unwrap_or_else(|_| panic!("timed out: {what}"))
}

/// The write path, the reads, and the streams of the Vault, the collection,
/// and the mirror.
async fn round_trip() {
    let (_scratch, path) = fresh("runtime-round-trip");
    let vault = open(&path, options()).await.unwrap();
    let info = vault.info(Empty::default()).await.unwrap();
    assert_eq!(
        info.path.as_deref(),
        Some(path.canonicalize().unwrap().to_str().unwrap())
    );
    let notes = collection(&vault, "notes").await.unwrap();
    assert_eq!(
        notes
            .describe(Empty::default())
            .await
            .unwrap()
            .name
            .as_deref(),
        Some("notes")
    );

    // A write returns the token of the commit it made, and a write that
    // stores nothing the token it found.
    let created = notes.create(creation("a.md", "one")).await.unwrap();
    assert_eq!(created.version.as_deref(), Some(head(&path).as_str()));
    let upserted = notes.upsert(upsert("b.md", "two")).await.unwrap();
    assert_eq!(upserted.version.as_deref(), Some(head(&path).as_str()));
    assert_ne!(upserted.version, created.version);
    let again = notes.upsert(upsert("b.md", "two")).await.unwrap();
    assert_eq!(again.version, upserted.version);
    let updated = notes
        .update(
            DocumentUpdate::default()
                .with_id("a.md")
                .with_raw(b"uno".to_vec())
                .with_note("test"),
        )
        .await
        .unwrap();
    assert_eq!(
        updated.document.as_option().and_then(|d| d.raw.as_deref()),
        Some(&b"uno"[..])
    );
    assert_eq!(updated.version.as_deref(), Some(head(&path).as_str()));
    assert_eq!(
        notes.get(id("a.md")).await.unwrap().raw.as_deref(),
        Some(&b"uno"[..])
    );
    assert!(
        notes
            .get(id("zzz.md"))
            .await
            .unwrap_err()
            .is(Code::NotFound)
    );
    assert!(
        notes
            .create(creation("a.md", "again"))
            .await
            .unwrap_err()
            .is(Code::AlreadyExists)
    );

    let batch = notes
        .batch(stream_of(vec![
            operation(Operation::Upsert(Box::new(upsert("c.md", "three")))),
            operation(Operation::Delete(Box::new(
                scheda::v1::DocumentDeletion::default()
                    .with_id("b.md")
                    .with_note("test"),
            ))),
        ]))
        .await
        .unwrap();
    assert_eq!(batch.results.len(), 2);
    assert_eq!(batch.version.as_deref(), Some(head(&path).as_str()));
    let deleted = notes
        .delete(
            scheda::v1::DocumentDeletion::default()
                .with_id("c.md")
                .with_note("test"),
        )
        .await
        .unwrap();
    assert!(deleted.document.is_unset());
    assert_eq!(deleted.version.as_deref(), Some(head(&path).as_str()));
    notes.create(creation("c.md", "three")).await.unwrap();

    let documents = all(notes.get_many(listing()).await.unwrap()).await.unwrap();
    let ids: Vec<_> = documents.iter().map(|d| d.id.clone().unwrap()).collect();
    assert_eq!(ids, ["a.md", "c.md"]);
    let history = all(notes.history(id("a.md")).await.unwrap()).await.unwrap();
    assert_eq!(history.len(), 2, "the creation and the update");

    // The Vault writes too: a collection, then Documents in two.
    vault.create_collection(declaration("extra")).await.unwrap();
    assert_eq!(
        vault
            .list_collections(Empty::default())
            .await
            .unwrap()
            .collections
            .len(),
        3
    );
    let across = vault
        .batch_documents(stream_of(vec![
            VaultDocumentOperation {
                collection: Some("notes".to_owned()),
                operation: operation(Operation::Upsert(Box::new(upsert("d.md", "four")))).into(),
                ..VaultDocumentOperation::default()
            },
            VaultDocumentOperation {
                collection: Some("extra".to_owned()),
                operation: operation(Operation::Upsert(Box::new(upsert("e.md", "five")))).into(),
                ..VaultDocumentOperation::default()
            },
        ]))
        .await
        .unwrap();
    assert_eq!(across.results.len(), 2);
    assert_eq!(across.version.as_deref(), Some(head(&path).as_str()));
    assert_eq!(
        vault
            .info(Empty::default())
            .await
            .unwrap()
            .version
            .as_deref(),
        Some(head(&path).as_str())
    );
    let extra = collection(&vault, "extra").await.unwrap();
    assert_eq!(
        extra.get(id("e.md")).await.unwrap().raw.as_deref(),
        Some(&b"five"[..])
    );
    assert!(
        vault
            .delete_collection(CollectionDeletion::default().with_name("extra"))
            .await
            .unwrap_err()
            .is(Code::FailedPrecondition),
        "a collection that holds Documents stays"
    );
    assert!(
        vault
            .get_collection(CollectionKey::default().with_name("missing"))
            .await
            .err()
            .unwrap()
            .is(Code::NotFound)
    );

    let mirror = mirror(&vault).await.unwrap();
    assert_eq!(
        mirror
            .info(Empty::default())
            .await
            .unwrap()
            .url
            .as_deref()
            .unwrap_or(""),
        ""
    );
    assert!(
        mirror
            .push(Empty::default())
            .await
            .unwrap_err()
            .is(Code::NotFound),
        "no mirror to push to"
    );

    assert!(
        vault
            .check(Empty::default())
            .await
            .unwrap()
            .problems
            .is_empty()
    );
    vault.close(Empty::default()).await.unwrap();
}

#[test]
fn the_objects_work_on_both_runtimes() {
    both(round_trip);
}

#[test]
fn the_objects_work_with_a_blocking_pool_of_one_thread() {
    runtime(0, 1).block_on(round_trip());
    runtime(2, 1).block_on(round_trip());
}

#[test]
fn a_refused_open_keeps_its_code() {
    both(|| async {
        let error = SchedaAsync::open(&Scheda, VaultOpening::default())
            .await
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument));
        let info = SchedaAsync::info(&Scheda, Empty::default()).await.unwrap();
        assert_eq!(info.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    });
}

#[test]
fn a_blocking_caller_wraps_the_vault_on_a_multi_thread_runtime_and_is_refused_on_a_current_thread_one()
 {
    let (_scratch, path) = fresh("runtime-blocking");
    // A caller without async code: a multi-thread runtime, and the blocking
    // form of the reference taken inside it.
    let multi = runtime(2, 4);
    let reference = multi
        .block_on(SchedaAsync::open(&Scheda, opening(&path, options())))
        .unwrap();
    let vault = {
        let _inside = multi.enter();
        reference.to_sync().unwrap()
    };
    let notes = vault
        .get_collection(CollectionKey::default().with_name("notes"))
        .unwrap();
    let notes = {
        let _inside = multi.enter();
        notes.to_sync().unwrap()
    };
    notes.upsert(upsert("a.md", "blocking")).unwrap();
    assert_eq!(
        notes.get(id("a.md")).unwrap().raw.as_deref(),
        Some(&b"blocking"[..])
    );
    vault.close(Empty::default()).unwrap();

    // On a current-thread runtime the one thread cannot block on a call the
    // same thread must drive: the bridge refuses, it does not hang.
    let current = runtime(0, 4);
    current.block_on(async {
        let reference = SchedaAsync::open(&Scheda, opening(&path, options()))
            .await
            .unwrap();
        let vault = reference.to_sync().unwrap();
        let error = vault.info(Empty::default()).unwrap_err();
        assert!(error.is(protocontract::RuntimeCode::CannotBlock), "{error}");
        reference
            .to_async_inline()
            .unwrap()
            .close(Empty::default())
            .await
            .unwrap();
    });
}

#[test]
fn after_close_every_object_is_failed_precondition_and_a_second_close_is_nothing() {
    both(|| async {
        let (_scratch, path) = fresh("runtime-closed");
        let vault = open(&path, options()).await.unwrap();
        let notes = collection(&vault, "notes").await.unwrap();
        notes.upsert(upsert("a.md", "a")).await.unwrap();
        let mirror = mirror(&vault).await.unwrap();

        vault.close(Empty::default()).await.unwrap();

        let closed = |error: Error| assert!(error.is(Code::FailedPrecondition), "{error}");
        closed(vault.info(Empty::default()).await.unwrap_err());
        closed(
            vault
                .get_collection(CollectionKey::default().with_name("notes"))
                .await
                .err()
                .unwrap(),
        );
        closed(vault.mirror(Empty::default()).await.err().unwrap());
        closed(
            vault
                .create_collection(declaration("more"))
                .await
                .unwrap_err(),
        );
        closed(notes.get(id("a.md")).await.unwrap_err());
        closed(notes.upsert(upsert("b.md", "b")).await.unwrap_err());
        closed(mirror.info(Empty::default()).await.unwrap_err());
        vault.close(Empty::default()).await.unwrap();

        // The write before the close is stored; another Vault reads it.
        let again = open(&path, options()).await.unwrap();
        let notes = collection(&again, "notes").await.unwrap();
        assert_eq!(
            notes.get(id("a.md")).await.unwrap().raw.as_deref(),
            Some(&b"a"[..])
        );
        again.close(Empty::default()).await.unwrap();
    });
}

#[test]
fn a_stream_after_close_fails_its_next_item_and_ends() {
    both(|| async {
        let (_scratch, path) = fresh("runtime-stream-closed");
        let vault = open(&path, options()).await.unwrap();
        let notes = collection(&vault, "notes").await.unwrap();
        for n in 0..3 {
            notes
                .upsert(upsert(&format!("d{n}.md"), "x"))
                .await
                .unwrap();
        }
        let mut stream = Box::pin(notes.get_many(listing()).await.unwrap());
        let first = crate::common::next(&mut stream).await.unwrap().unwrap();
        assert_eq!(first.id.as_deref(), Some("d0.md"));
        vault.close(Empty::default()).await.unwrap();
        let error = crate::common::next(&mut stream).await.unwrap().unwrap_err();
        assert!(error.is(Code::FailedPrecondition), "{error}");
        assert!(crate::common::next(&mut stream).await.is_none());
    });
}

#[test]
fn a_write_waiting_for_the_write_lock_holds_no_pool_thread_and_aborts_at_the_deadline() {
    for workers in [0, 2] {
        let (_scratch, path) = fresh("runtime-lock");
        // One thread in the blocking pool: whatever waits for the lock on it
        // would stop every other call.
        runtime(workers, 1).block_on(async {
            let vault = open(&path, options()).await.unwrap();
            let notes = collection(&vault, "notes").await.unwrap();
            let held = hold_write_lock(&path);
            let waiting = tokio::spawn({
                let notes = notes.clone();
                async move { notes.upsert(upsert("a.md", "late")).await }
            });
            write_waits_for_the_lock(&path, 1).await;
            assert!(!waiting.is_finished(), "the write waits for the lock");
            // The single pool thread is free: a read gets through.
            let started = Instant::now();
            assert!(
                within("a read while a write waits", notes.get(id("none.md")))
                    .await
                    .unwrap_err()
                    .is(Code::NotFound)
            );
            assert!(started.elapsed() < Duration::from_secs(5));
            // A second write queues behind the first, and holds no thread
            // either.
            let other = tokio::spawn({
                let vault = vault.clone();
                async move { vault.create_collection(declaration("more")).await }
            });
            // A write queued behind another has nothing open yet that shows
            // it (it holds no file before its turn). The margin makes the
            // check below stronger on a slow runner, and a faster one never
            // fails it: the lock is held, so `other` cannot finish.
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(!other.is_finished());
            assert!(
                within("info while two writes wait", vault.info(Empty::default()))
                    .await
                    .is_ok()
            );

            drop(held);
            within("the first write", waiting).await.unwrap().unwrap();
            within("the second write", other).await.unwrap().unwrap();
            assert_eq!(
                notes.get(id("a.md")).await.unwrap().raw.as_deref(),
                Some(&b"late"[..])
            );
            vault.close(Empty::default()).await.unwrap();
        });

        // At the deadline the write is ABORTED, and nothing is written.
        let before = runtime(workers, 1).block_on(async {
            let vault = open(&path, options().with_write_timeout_ms(300))
                .await
                .unwrap();
            let notes = collection(&vault, "notes").await.unwrap();
            let held = hold_write_lock(&path);
            let before = snapshot(&path);
            let error = notes.upsert(upsert("b.md", "never")).await.unwrap_err();
            assert!(error.is(Code::Aborted), "{error}");
            drop(held);
            vault.close(Empty::default()).await.unwrap();
            before
        });
        assert_eq!(snapshot(&path), before, "an aborted write stores nothing");
    }
}

#[test]
fn a_write_dropped_while_it_waits_for_the_lock_writes_nothing_and_takes_no_lock() {
    both(|| async {
        let (_scratch, path) = fresh("runtime-dropped-wait");
        let vault = open(&path, options()).await.unwrap();
        let notes = collection(&vault, "notes").await.unwrap();
        let held = hold_write_lock(&path);
        let before = snapshot(&path);
        let waiting = tokio::spawn({
            let notes = notes.clone();
            async move { notes.upsert(upsert("a.md", "dropped")).await }
        });
        write_waits_for_the_lock(&path, 1).await;
        waiting.abort();
        assert!(waiting.await.unwrap_err().is_cancelled());
        drop(held);
        assert_eq!(snapshot(&path), before, "nothing was written");
        // The lock is free: the next write takes it at once.
        let written = within("a write after the drop", notes.upsert(upsert("b.md", "b")))
            .await
            .unwrap();
        assert_eq!(written.version.as_deref(), Some(head(&path).as_str()));
        assert!(notes.get(id("a.md")).await.unwrap_err().is(Code::NotFound));
        vault.close(Empty::default()).await.unwrap();
    });
}

#[test]
fn an_empty_batch_waits_for_no_lock_and_answers_with_the_state_now() {
    both(|| async {
        let (_scratch, path) = fresh("runtime-empty-batch");
        // The deadline of a write that waits is two seconds: a batch with no
        // write must not be one.
        let vault = open(&path, options().with_write_timeout_ms(2_000))
            .await
            .unwrap();
        let notes = collection(&vault, "notes").await.unwrap();
        let held = hold_write_lock(&path);
        let started = Instant::now();
        let in_collection = within(
            "an empty batch of a collection",
            notes.batch(stream_of(Vec::<DocumentOperation>::new())),
        )
        .await
        .unwrap();
        let in_vault = within(
            "an empty batch of the Vault",
            vault.batch_documents(stream_of(Vec::<VaultDocumentOperation>::new())),
        )
        .await
        .unwrap();
        assert!(started.elapsed() < Duration::from_millis(1_500));
        for reply in [in_collection, in_vault] {
            assert!(reply.results.is_empty());
            assert_eq!(reply.version.as_deref(), Some(head(&path).as_str()));
        }
        // A batch that writes waits for the lock and is refused at the
        // deadline.
        let error = notes
            .batch(stream_of(vec![operation(Operation::Upsert(Box::new(
                upsert("a.md", "late"),
            )))]))
            .await
            .unwrap_err();
        assert!(error.is(Code::Aborted), "{error}");
        drop(held);
        vault.close(Empty::default()).await.unwrap();
    });
}

#[test]
fn a_request_that_can_never_succeed_fails_at_once_while_the_write_lock_is_held() {
    both(|| async {
        let (_scratch, path) = fresh("runtime-prechecks");
        let vault = open(&path, options().with_write_timeout_ms(5_000))
            .await
            .unwrap();
        let notes = collection(&vault, "notes").await.unwrap();
        vault.create_collection(declaration("gone")).await.unwrap();
        let gone = collection(&vault, "gone").await.unwrap();
        vault
            .delete_collection(CollectionDeletion::default().with_name("gone"))
            .await
            .unwrap();

        let held = hold_write_lock(&path);
        let before = snapshot(&path);
        // Each of these would wait the five seconds out and come back
        // `ABORTED` (retryable) if the checks ran after the wait.
        let started = Instant::now();
        let no_note = DocumentCreation::default()
            .with_id("a.md")
            .with_raw(b"a".to_vec());
        let error = notes.create(no_note).await.unwrap_err();
        assert!(error.is(Code::InvalidArgument), "{error}");
        let error = gone.upsert(upsert("a.md", "a")).await.unwrap_err();
        assert!(error.is(Code::NotFound), "{error}");
        let bad = DocumentUpsert::default()
            .with_id("b.md")
            .with_raw(b"b".to_vec());
        let error = notes
            .batch(stream_of(vec![
                operation(Operation::Upsert(Box::new(upsert("a.md", "a")))),
                operation(Operation::Upsert(Box::new(bad))),
            ]))
            .await
            .unwrap_err();
        assert!(error.is(Code::InvalidArgument), "{error}");
        let error = vault
            .create_collection(declaration("Not A Slug!"))
            .await
            .unwrap_err();
        assert!(error.is(Code::InvalidArgument), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the refusals waited for the lock"
        );
        drop(held);
        assert_eq!(snapshot(&path), before, "a refused write stores nothing");
        vault.close(Empty::default()).await.unwrap();
    });
}

/// A mirror whose `pre-receive` hook writes its process id, the one the
/// operating system gave it, to `started`, then holds every push. It gives
/// up after two minutes, so a test that fails leaves it behind for no
/// longer than that.
fn stalling_mirror(scratch: &Scratch) -> (PathBuf, PathBuf) {
    let mirror = scratch.join("mirror.git");
    std::fs::create_dir_all(&mirror).unwrap();
    git(&mirror, &["init", "--quiet", "--bare"]);
    let started = scratch.join("started");
    let writing = scratch.join("started.tmp");
    // Git for Windows runs a hook with an `sh` of its own, whose `$$` is
    // not the process id of Windows.
    let pid = if cfg!(windows) {
        "$(cat /proc/$$/winpid)"
    } else {
        "$$"
    };
    let hook = mirror.join("hooks/pre-receive");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\necho {pid} > '{writing}'\nmv '{writing}' '{started}'\ni=0\nwhile [ $i -lt 2400 ]; do sleep 0.05; i=$((i + 1)); done\nexit 1\n",
            writing = sh_path(&writing),
            started = sh_path(&started),
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (mirror, started)
}

/// `path` as the `sh` of a hook takes it: with forward slashes on Windows.
fn sh_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// The processes of a push, seen from outside through the hook of the
/// mirror: whether any of them is still there.
#[cfg(unix)]
mod processes {
    use std::ffi::c_int;

    unsafe extern "C" {
        fn getpgid(pid: c_int) -> c_int;
        #[cfg(not(target_os = "linux"))]
        fn kill(pid: c_int, signal: c_int) -> c_int;
    }

    /// The process group of a push: git, and what git runs for the mirror's
    /// side of a `file://` push (`sh`, `git-receive-pack`, the hook).
    pub struct Push {
        group: c_int,
    }

    impl Push {
        /// The push whose hook has the process id `hook`. Its group must be
        /// its own: the group of this process never empties.
        pub fn of(hook: u32) -> Push {
            let group = group_of(c_int::try_from(hook).unwrap());
            assert_ne!(
                group,
                group_of(0),
                "the push runs in a process group of its own"
            );
            Push { group }
        }

        /// Whether a process of the push has not ended yet.
        pub fn runs(&self) -> bool {
            group_has_members(self.group)
        }
    }

    /// The process group of `pid`; 0 is this process.
    fn group_of(pid: c_int) -> c_int {
        // SAFETY: a plain system call.
        let group = unsafe { getpgid(pid) };
        assert!(group > 0, "{}", std::io::Error::last_os_error());
        group
    }

    /// Whether a process of the group `group` has not ended yet. A zombie
    /// has ended: who reaps it, and when, is up to its parent.
    #[cfg(target_os = "linux")]
    fn group_has_members(group: c_int) -> bool {
        std::fs::read_dir("/proc").unwrap().any(|entry| {
            let Ok(entry) = entry else { return false };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                return false;
            };
            // `pid (comm) state ppid pgrp …`; `comm` may hold spaces.
            let Some((_, fields)) = stat.rsplit_once(')') else {
                return false;
            };
            let mut fields = fields.split_whitespace();
            let state = fields.next();
            let pgrp = fields.nth(1).and_then(|pgrp| pgrp.parse::<c_int>().ok());
            pgrp == Some(group) && state != Some("Z")
        })
    }

    /// Whether a process of the group `group` is there: signal 0 checks and
    /// sends nothing. `launchd` reaps an orphan at once.
    #[cfg(not(target_os = "linux"))]
    fn group_has_members(group: c_int) -> bool {
        // `ESRCH`, 3 on every Unix: no process in the group.
        const ESRCH: i32 = 3;
        // SAFETY: a plain system call; signal 0 sends nothing.
        let sent = unsafe { kill(-group, 0) };
        sent == 0 || std::io::Error::last_os_error().raw_os_error() != Some(ESRCH)
    }
}

/// The processes of a push, seen from outside through the hook of the
/// mirror: whether the hook is still there.
#[cfg(windows)]
mod processes {
    use std::os::windows::io::{AsRawHandle, HandleOrNull, OwnedHandle, RawHandle};

    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0x0000_0000;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;

    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> RawHandle;
        fn WaitForSingleObject(handle: RawHandle, milliseconds: u32) -> u32;
    }

    /// The hook of a push: the last process git starts for the mirror's
    /// side of a `file://` push (git, `git-receive-pack`, the `sh` of the
    /// hook). Killing git alone leaves it running; it ends with the job of
    /// the push. The handle names this process even after its id is given
    /// to another.
    pub struct Push {
        hook: OwnedHandle,
    }

    impl Push {
        /// The push whose hook has the process id `hook`.
        pub fn of(hook: u32) -> Push {
            // SAFETY: opens the process by its id, to wait for it; the
            // handle is owned by nothing else.
            let handle =
                unsafe { HandleOrNull::from_raw_handle(OpenProcess(SYNCHRONIZE, 0, hook)) };
            let hook = OwnedHandle::try_from(handle)
                .unwrap_or_else(|_| panic!("open the hook: {}", std::io::Error::last_os_error()));
            Push { hook }
        }

        /// Whether the hook has not ended yet.
        pub fn runs(&self) -> bool {
            // SAFETY: the handle is open; a wait of 0 ms only looks.
            match unsafe { WaitForSingleObject(self.hook.as_raw_handle(), 0) } {
                WAIT_OBJECT_0 => false,
                WAIT_TIMEOUT => true,
                _ => panic!("wait for the hook: {}", std::io::Error::last_os_error()),
            }
        }
    }
}

#[test]
fn a_push_dropped_midway_leaves_the_commit_local_and_the_next_push_carries_it() {
    both(|| async {
        let (scratch, path) = fresh("runtime-dropped-push");
        let (bare, started) = stalling_mirror(&scratch);
        let url = file_url(&bare);
        git(&path, &["remote", "add", "origin", &url]);
        let vault = open(&path, options().with_auto_sync(true).with_mirror(url))
            .await
            .unwrap();
        let notes = collection(&vault, "notes").await.unwrap();
        let before = head(&path);

        // The write commits, then pushes: the push stalls in the mirror's
        // hook, and the caller drops the call there.
        let writing = tokio::spawn({
            let notes = notes.clone();
            async move { notes.upsert(upsert("a.md", "kept")).await }
        });
        within("the push to reach the mirror", async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let hook = std::fs::read_to_string(&started).unwrap();
        let push = processes::Push::of(hook.trim().parse().unwrap());
        writing.abort();
        assert!(writing.await.unwrap_err().is_cancelled());
        within("every process of the dropped push to end", async {
            while push.runs() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let local = head(&path);
        assert_ne!(local, before, "the write is stored");
        let branch = command(&path, &["symbolic-ref", "--short", "HEAD"])
            .output()
            .unwrap();
        let branch = String::from_utf8(branch.stdout).unwrap().trim().to_owned();
        let reference = format!("refs/heads/{branch}");
        assert_eq!(
            rev_parse(&bare, &reference),
            None,
            "the mirror did not take it"
        );

        // Without the hook, the next push carries the commit.
        std::fs::remove_file(bare.join("hooks/pre-receive")).unwrap();
        let mirror = mirror(&vault).await.unwrap();
        mirror.push(Empty::default()).await.unwrap();
        assert_eq!(
            rev_parse(&bare, &reference).as_deref(),
            Some(local.as_str())
        );
        assert_eq!(
            notes.get(id("a.md")).await.unwrap().raw.as_deref(),
            Some(&b"kept"[..])
        );
        vault.close(Empty::default()).await.unwrap();
    });
}
