//! Async API (`tokio` feature): every call runs the sync one on Tokio's
//! blocking pool, on both runtime flavors.

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{AsyncVault, Code, OpenOptions, Schema, Unit};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-async", n),
        }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn document_schema() -> Schema {
    Schema {
        kind: Unit::default().into(),
        ..Schema::default()
    }
}

async fn round_trip() {
    let scratch = Scratch::new();
    let vault = AsyncVault::open(scratch.vault(), OpenOptions::new(), None)
        .await
        .unwrap();
    let notes = vault
        .create_collection("notes".into(), document_schema(), None)
        .await
        .unwrap();
    notes
        .add("alpha".into(), b"one".to_vec(), Some("add alpha".into()))
        .await
        .unwrap();
    notes
        .add("beta".into(), b"two".to_vec(), None)
        .await
        .unwrap();
    let got = notes.get("alpha".into()).await.unwrap();
    assert_eq!(got.raw.as_deref(), Some(b"one".as_slice()));
    assert_eq!(notes.ids().await.unwrap(), ["alpha", "beta"]);
    notes
        .replace("alpha".into(), b"uno".to_vec(), None)
        .await
        .unwrap();
    let history = notes.history("alpha".into(), 0, 10).await.unwrap();
    assert_eq!(history.entries.len(), 2);
    notes.remove("beta".into(), None).await.unwrap();
    let documents = notes.documents().await.unwrap();
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].raw.as_deref(), Some(b"uno".as_slice()));
    let err = notes.get("beta".into()).await.unwrap_err();
    assert_eq!(err.known_code(), Some(Code::NotFound));
    vault
        .transaction("two at once", |vault| {
            let notes = vault.collection("notes")?;
            notes.add("gamma", b"three", None)?;
            notes.add("delta", b"four", None)
        })
        .await
        .unwrap();
    assert_eq!(notes.ids().await.unwrap(), ["alpha", "delta", "gamma"]);
    assert_eq!(vault.info().await.unwrap().collections.len(), 1);
    vault.check().await.unwrap();
    #[cfg(feature = "find")]
    {
        let status = vault
            .wait_indexed(std::time::Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(status.lexical, status.head);
        let page = notes.find(scheda::FindQuery::all(), 0, 10).await.unwrap();
        assert_eq!(page.records.len(), 3);
        let page = vault
            .find(vec!["notes".into()], scheda::FindQuery::all(), 0, 10)
            .await
            .unwrap();
        assert_eq!(page.total, Some(3));
        assert_eq!(vault.index_status().await.unwrap().head, status.head);
    }
    vault.reindex().await.unwrap();
    #[cfg(feature = "find")]
    {
        let page = notes.find(scheda::FindQuery::all(), 0, 10).await.unwrap();
        assert_eq!(page.records.len(), 3);
    }
    vault.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn round_trip_on_a_multi_thread_runtime() {
    round_trip().await;
}

#[tokio::test(flavor = "current_thread")]
async fn round_trip_on_a_current_thread_runtime() {
    round_trip().await;
}

/// `push`, `pull`, and `sync` against a bare remote.
async fn remote_round_trip() {
    let scratch = Scratch::new();
    let bare = scratch.root.join("bare.git");
    let made = std::process::Command::new("git")
        .args(["init", "--quiet", "--bare", "--initial-branch=main", "--"])
        .arg(&bare)
        .status()
        .unwrap();
    assert!(made.success());
    let seed = AsyncVault::open(scratch.root.join("seed"), OpenOptions::new(), None)
        .await
        .unwrap();
    seed.create_collection("notes".into(), document_schema(), None)
        .await
        .unwrap();
    let pushed = std::process::Command::new("git")
        .current_dir(seed.path())
        .args(["push", "--quiet"])
        .arg(&bare)
        .arg("HEAD:main")
        .status()
        .unwrap();
    assert!(pushed.success());
    seed.close().await.unwrap();
    let url = format!("file://{}", bare.display());
    let a = AsyncVault::open(
        scratch.root.join("a"),
        OpenOptions::new().remote_repo(&url),
        None,
    )
    .await
    .unwrap();
    let b = AsyncVault::open(
        scratch.root.join("b"),
        OpenOptions::new().remote_repo(&url),
        None,
    )
    .await
    .unwrap();
    a.collection("notes")
        .unwrap()
        .add("from-a".into(), b"one".to_vec(), None)
        .await
        .unwrap();
    a.push().await.unwrap();
    b.pull().await.unwrap();
    let got = b
        .collection("notes")
        .unwrap()
        .get("from-a".into())
        .await
        .unwrap();
    assert_eq!(got.raw.as_deref(), Some(b"one".as_slice()));
    b.collection("notes")
        .unwrap()
        .add("from-b".into(), b"two".to_vec(), None)
        .await
        .unwrap();
    b.sync().await.unwrap();
    a.sync().await.unwrap();
    assert_eq!(
        a.collection("notes").unwrap().ids().await.unwrap(),
        ["from-a", "from-b"]
    );
    a.close().await.unwrap();
    b.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_round_trip_on_a_multi_thread_runtime() {
    remote_round_trip().await;
}

#[tokio::test(flavor = "current_thread")]
async fn remote_round_trip_on_a_current_thread_runtime() {
    remote_round_trip().await;
}

async fn journal_lines() {
    let scratch = Scratch::new();
    let vault = AsyncVault::open(scratch.vault(), OpenOptions::new(), None)
        .await
        .unwrap();
    let schema = scheda::schema_from_value(&serde_json::json!({"journal": {}})).unwrap();
    let log = vault
        .create_collection("log".into(), schema, None)
        .await
        .unwrap();
    for id in ["a", "b", "c"] {
        log.add(id.into(), id.as_bytes().to_vec(), None)
            .await
            .unwrap();
    }
    let ids: Vec<String> = log
        .lines()
        .await
        .unwrap()
        .into_iter()
        .filter_map(|record| record.id)
        .collect();
    assert_eq!(ids, ["a", "b", "c"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn journal_lines_on_a_multi_thread_runtime() {
    journal_lines().await;
}

#[tokio::test(flavor = "current_thread")]
async fn journal_lines_on_a_current_thread_runtime() {
    journal_lines().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn close_with_a_clone_alive_leaves_the_clone_working() {
    let scratch = Scratch::new();
    let vault = AsyncVault::open(scratch.vault(), OpenOptions::new(), None)
        .await
        .unwrap();
    let clone = vault.clone();
    vault.close().await.unwrap();
    let notes = clone
        .create_collection("notes".into(), document_schema(), None)
        .await
        .unwrap();
    notes
        .add("alpha".into(), b"one".to_vec(), None)
        .await
        .unwrap();
    clone.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn an_open_error_keeps_its_code() {
    let scratch = Scratch::new();
    let options = OpenOptions::new().with_writer("BAD WRITER".to_owned());
    let err = AsyncVault::open(scratch.vault(), options, None)
        .await
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[cfg(feature = "search")]
struct PanickingReranker;

#[cfg(feature = "search")]
impl scheda::Reranker for PanickingReranker {
    fn many_limit(&self) -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::new(8).unwrap()
    }

    fn score_many(
        &self,
        _query: &str,
        _records: &[&scheda::Record],
    ) -> Result<Vec<f64>, scheda::Error> {
        panic!("reranker bug");
    }
}

/// A caller callback that panics on the blocking thread is an error with
/// the method in its stack, not a panic of the caller's task.
#[cfg(feature = "search")]
#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_callback_is_io_other() {
    let scratch = Scratch::new();
    let vault = AsyncVault::open_with(
        scratch.vault(),
        OpenOptions::new(),
        None,
        Some(common::json_text_projector(&["body"])),
    )
    .await
    .unwrap();
    let notes = vault
        .create_collection("notes".into(), document_schema(), None)
        .await
        .unwrap();
    for id in ["alpha", "beta"] {
        notes
            .add(
                id.into(),
                serde_json::to_vec(&serde_json::json!({"body": "kryptonite"})).unwrap(),
                None,
            )
            .await
            .unwrap();
    }
    vault
        .wait_indexed(std::time::Duration::from_secs(30))
        .await
        .unwrap();
    let query = scheda::SearchQuery::default()
        .with_text("kryptonite")
        .with_k(5);
    let err = notes
        .search(query, Some(std::sync::Arc::new(PanickingReranker)))
        .await
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IoOther));
    assert!(
        err.stack
            .iter()
            .any(|frame| frame.contains("[AsyncCollection::search]")),
        "{:?}",
        err.stack
    );
}

/// Two writes, a pause between them, and the thread they ran on.
fn add_pair(vault: &scheda::Vault, i: usize) -> Result<std::thread::ThreadId, scheda::Error> {
    let notes = vault.collection("notes")?;
    let thread = std::thread::current().id();
    notes.add(&format!("t{i}"), b"first", None)?;
    std::thread::sleep(std::time::Duration::from_millis(2));
    notes.add(&format!("u{i}"), b"second", None)?;
    assert_eq!(std::thread::current().id(), thread);
    Ok(thread)
}

/// The single commit that holds `id`, as `(hash, message)`.
async fn only_commit(notes: &scheda::AsyncCollection, id: &str) -> (String, String) {
    let history = notes.history(id.into(), 0, 10).await.unwrap();
    assert_eq!(history.entries.len(), 1, "{id}: {:?}", history.entries);
    let entry = &history.entries[0];
    (
        entry.hash.clone().unwrap(),
        entry.message.clone().unwrap_or_default(),
    )
}

/// Each of 64 tasks runs its transaction until it commits. Every attempt
/// either commits both writes in one commit or is `locked` and leaves no
/// trace; none is split across commits or threads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_transactions_commit_whole_or_are_locked() {
    const TASKS: usize = 64;
    let scratch = Scratch::new();
    let vault = AsyncVault::open(scratch.vault(), OpenOptions::new(), None)
        .await
        .unwrap();
    let notes = vault
        .create_collection("notes".into(), document_schema(), None)
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..TASKS {
        let vault = vault.clone();
        let notes = notes.clone();
        tasks.spawn(async move {
            let mut locked = 0;
            loop {
                let result = vault
                    .transaction(format!("tx {i}"), move |vault| add_pair(vault, i))
                    .await;
                match result {
                    Ok(_thread) => return locked,
                    Err(err) => {
                        assert_eq!(err.known_code(), Some(Code::Locked), "{:?}", err.stack);
                        for id in [format!("t{i}"), format!("u{i}")] {
                            let err = notes.get(id).await.unwrap_err();
                            assert_eq!(err.known_code(), Some(Code::NotFound));
                        }
                        locked += 1;
                        assert!(tokio::time::Instant::now() < deadline, "task {i} starved");
                        tokio::time::sleep(std::time::Duration::from_millis(1 + (i % 7) as u64))
                            .await;
                    }
                }
            }
        });
    }
    let mut locked = 0;
    while let Some(joined) = tasks.join_next().await {
        locked += joined.unwrap();
    }
    assert!(locked >= 1, "no attempt ran into another transaction");
    for i in 0..TASKS {
        let (first, message) = only_commit(&notes, &format!("t{i}")).await;
        let (second, _) = only_commit(&notes, &format!("u{i}")).await;
        assert_eq!(first, second, "transaction {i} split across commits");
        assert_eq!(message.trim(), format!("tx {i}"));
    }
    assert_eq!(notes.ids().await.unwrap().len(), 2 * TASKS);
    vault.check().await.unwrap();
    vault.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn an_error_or_a_panic_discards_the_transaction() {
    let scratch = Scratch::new();
    let vault = AsyncVault::open(scratch.vault(), OpenOptions::new(), None)
        .await
        .unwrap();
    let notes = vault
        .create_collection("notes".into(), document_schema(), None)
        .await
        .unwrap();
    let err = vault
        .transaction("fails", |vault| -> Result<(), scheda::Error> {
            vault.collection("notes")?.add("a", b"one", None)?;
            Err(scheda::Error::invalid("caller gave up"))
        })
        .await
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let err = vault
        .transaction("panics", |vault| -> Result<(), scheda::Error> {
            vault.collection("notes")?.add("b", b"two", None)?;
            panic!("caller bug");
        })
        .await
        .unwrap_err();
    assert_eq!(err.known_code(), Some(Code::IoOther));
    assert!(
        err.stack
            .iter()
            .any(|frame| frame.contains("[AsyncVault::transaction]")),
        "{:?}",
        err.stack
    );
    for id in ["a", "b"] {
        let err = notes.get(id.into()).await.unwrap_err();
        assert_eq!(err.known_code(), Some(Code::NotFound), "{id}");
    }
    let value = vault
        .transaction("works", |vault| {
            vault.collection("notes")?.add("c", b"three", None)?;
            Ok(7)
        })
        .await
        .unwrap();
    assert_eq!(value, 7);
    let (_, message) = only_commit(&notes, "c").await;
    assert!(message.contains("works"), "{message:?}");
    vault.check().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sync_transaction_on_another_thread_makes_it_locked() {
    let scratch = Scratch::new();
    let vault = AsyncVault::open(scratch.vault(), OpenOptions::new(), None)
        .await
        .unwrap();
    let notes = vault
        .create_collection("notes".into(), document_schema(), None)
        .await
        .unwrap();
    let (begun_tx, begun_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let holder = {
        let vault = vault.clone();
        std::thread::spawn(move || {
            let sync = vault.as_vault();
            sync.begin().unwrap();
            sync.collection("notes")
                .unwrap()
                .add("held", b"one", None)
                .unwrap();
            begun_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            sync.commit("sync transaction").unwrap();
        })
    };
    begun_rx.await.unwrap();
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let err = {
        let ran = std::sync::Arc::clone(&ran);
        vault
            .transaction("async", move |_vault| {
                ran.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap_err()
    };
    assert_eq!(err.known_code(), Some(Code::Locked), "{:?}", err.stack);
    assert!(!ran.load(Ordering::SeqCst));
    release_tx.send(()).unwrap();
    tokio::task::spawn_blocking(move || holder.join().unwrap())
        .await
        .unwrap();
    only_commit(&notes, "held").await;
    vault
        .transaction("async", |vault| {
            vault.collection("notes")?.add("after", b"two", None)
        })
        .await
        .unwrap();
    only_commit(&notes, "after").await;
}

/// Embeds only while its gate is open; a call waits for the gate.
#[cfg(feature = "search")]
#[derive(Clone, Default)]
struct GateEmbedder {
    gate: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

#[cfg(feature = "search")]
impl GateEmbedder {
    fn open(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}

#[cfg(feature = "search")]
impl scheda::Embedder for GateEmbedder {
    fn model_id(&self) -> &str {
        "gate/d4"
    }

    fn dimensions(&self) -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::new(4).unwrap()
    }

    fn many_limit(&self) -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::new(16).unwrap()
    }

    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, scheda::Error> {
        let (open, cond) = &*self.gate;
        let _open = cond
            .wait_while(open.lock().unwrap(), |open| !*open)
            .unwrap();
        Ok(vec![vec![1.0, 0.0, 0.0, 0.0]; texts.len()])
    }
}

/// Opens the gate on drop, so a failed assertion does not leave the
/// indexer thread blocked in the embedder.
#[cfg(feature = "search")]
struct OpenOnDrop(GateEmbedder);

#[cfg(feature = "search")]
impl Drop for OpenOnDrop {
    fn drop(&mut self) {
        self.0.open();
    }
}

/// While the vector leg is held up, `wait_indexed` sleeps without a
/// blocking thread: on a pool of one, other calls still run. It wakes on
/// the indexer's round, not on its poll (an hour here), and a bounded
/// wait ends `io_timed_out` with no round finishing.
#[cfg(feature = "search")]
#[test]
fn wait_indexed_holds_no_blocking_thread() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let scratch = Scratch::new();
        let gate = OpenOnDrop(GateEmbedder::default());
        let vault = AsyncVault::open_with(
            scratch.vault(),
            OpenOptions::new().with_index_poll_ms(3_600_000),
            Some(std::sync::Arc::new(gate.0.clone())),
            Some(common::json_text_projector(&["body"])),
        )
        .await
        .unwrap();
        let notes = vault
            .create_collection("notes".into(), document_schema(), None)
            .await
            .unwrap();
        notes
            .add(
                "alpha".into(),
                serde_json::to_vec(&serde_json::json!({"body": "one"})).unwrap(),
                None,
            )
            .await
            .unwrap();
        let err = vault
            .wait_indexed(std::time::Duration::from_millis(200))
            .await
            .unwrap_err();
        assert_eq!(err.known_code(), Some(Code::IoTimedOut), "{:?}", err.stack);
        let waiting = {
            let vault = vault.clone();
            tokio::spawn(
                async move { vault.wait_indexed(std::time::Duration::from_secs(60)).await },
            )
        };
        tokio::task::yield_now().await;
        let info = tokio::time::timeout(std::time::Duration::from_secs(20), vault.info())
            .await
            .expect("info waited for the blocking thread")
            .unwrap();
        assert_eq!(info.collections.len(), 1);
        assert!(!waiting.is_finished());
        gate.0.open();
        let status = waiting.await.unwrap().unwrap();
        assert_eq!(status.vector, status.head);
        assert_eq!(status.lexical, status.head);
        vault.close().await.unwrap();
    });
}

#[cfg(feature = "search")]
type Threads = std::sync::Arc<std::sync::Mutex<Vec<(&'static str, std::thread::ThreadId)>>>;

/// Notes the thread of every query embedding.
#[cfg(feature = "search")]
struct ThreadEmbedder(Threads);

#[cfg(feature = "search")]
impl scheda::Embedder for ThreadEmbedder {
    fn model_id(&self) -> &str {
        "threads/d4"
    }

    fn dimensions(&self) -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::new(4).unwrap()
    }

    fn many_limit(&self) -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::new(16).unwrap()
    }

    fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, scheda::Error> {
        Ok(vec![vec![1.0, 0.0, 0.0, 0.0]; texts.len()])
    }

    fn embed_query(&self, _text: &str) -> Result<Vec<f32>, scheda::Error> {
        self.0
            .lock()
            .unwrap()
            .push(("embed", std::thread::current().id()));
        Ok(vec![1.0, 0.0, 0.0, 0.0])
    }
}

/// Notes the thread of every rerank.
#[cfg(feature = "search")]
struct ThreadReranker(Threads);

#[cfg(feature = "search")]
impl scheda::Reranker for ThreadReranker {
    fn many_limit(&self) -> std::num::NonZeroUsize {
        std::num::NonZeroUsize::new(8).unwrap()
    }

    fn score_many(
        &self,
        _query: &str,
        records: &[&scheda::Record],
    ) -> Result<Vec<f64>, scheda::Error> {
        self.0
            .lock()
            .unwrap()
            .push(("rerank", std::thread::current().id()));
        Ok(vec![1.0; records.len()])
    }
}

/// The sync `Embedder` and `Reranker` run inside the one blocking task of
/// a `search`, never on the runtime's thread.
#[cfg(feature = "search")]
#[tokio::test(flavor = "current_thread")]
async fn search_embeds_and_reranks_on_one_blocking_thread() {
    let scratch = Scratch::new();
    let threads = Threads::default();
    let vault = AsyncVault::open_with(
        scratch.vault(),
        OpenOptions::new(),
        Some(std::sync::Arc::new(ThreadEmbedder(threads.clone()))),
        Some(common::json_text_projector(&["body"])),
    )
    .await
    .unwrap();
    let notes = vault
        .create_collection("notes".into(), document_schema(), None)
        .await
        .unwrap();
    notes
        .add(
            "alpha".into(),
            serde_json::to_vec(&serde_json::json!({"body": "kryptonite"})).unwrap(),
            None,
        )
        .await
        .unwrap();
    vault
        .wait_indexed(std::time::Duration::from_secs(30))
        .await
        .unwrap();
    let query = scheda::SearchQuery::default()
        .with_text("kryptonite")
        .with_k(5);
    let hits = notes
        .search(
            query,
            Some(std::sync::Arc::new(ThreadReranker(threads.clone()))),
        )
        .await
        .unwrap();
    assert!(!hits.hits.is_empty());
    let threads = threads.lock().unwrap().clone();
    let kinds: Vec<&str> = threads.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(kinds, ["embed", "rerank"]);
    assert_eq!(threads[0].1, threads[1].1);
    assert_ne!(threads[0].1, std::thread::current().id());
    vault.close().await.unwrap();
}
