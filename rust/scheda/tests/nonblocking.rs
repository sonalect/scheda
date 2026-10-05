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
    assert_eq!(vault.info().await.unwrap().collections.len(), 1);
    vault.check().await.unwrap();
    #[cfg(feature = "find")]
    {
        vault
            .wait_indexed(std::time::Duration::from_secs(30))
            .await
            .unwrap();
        let page = notes.find(scheda::FindQuery::all(), 0, 10).await.unwrap();
        assert_eq!(page.records.len(), 1);
        let page = vault
            .find(vec!["notes".into()], scheda::FindQuery::all(), 0, 10)
            .await
            .unwrap();
        assert_eq!(page.total, Some(1));
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

#[tokio::test(flavor = "multi_thread")]
async fn journal_lines_are_collected() {
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
