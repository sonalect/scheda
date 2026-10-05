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
