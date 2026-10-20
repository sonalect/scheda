//! The reads that return many things, through the objects of the contract:
//! `Collection.GetMany` and `Collection.History` (lazy streams on one
//! snapshot) and `Vault.Check` (a report). The laziness checks count blob
//! reads, which only the library can; they are unit tests in `src/reads.rs`.

use std::collections::HashSet;

use buffa_types::google::protobuf::Empty;
use scheda::v1::SchedaAsync;
use scheda::v1::document_operation::Operation;
use scheda::v1::document_selection::Selection;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, Document, DocumentCreation, DocumentDeletion,
    DocumentHistoryEntry, DocumentId, DocumentOperation, DocumentSelection, DocumentUpsert,
    DynCollectionAsync, DynVaultAsync, Ids, Listing, OpenOptions, VaultAsync,
    VaultDocumentOperation, VaultErrorCode as Code,
};
use scheda::{Error, Scheda};
use serde_json::json;

use crate::common::{
    Scratch, all, both, git, next, open, opening, snapshot, stream_of, vault as built_vault, write,
};

async fn declare(vault: &DynVaultAsync, value: serde_json::Value) {
    let declaration: CollectionDeclaration = serde_json::from_value(value).unwrap();
    vault.create_collection(declaration).await.unwrap();
}

async fn collection(vault: &DynVaultAsync, name: &str) -> DynCollectionAsync {
    vault
        .get_collection(scheda::v1::CollectionKey::default().with_name(name))
        .await
        .unwrap()
        .to_async_inline()
        .unwrap()
}

/// A Vault with `notes` (standard), `log` (journal), `config` (a singleton
/// with the id `settings`), and `diary` (standard, isolated).
async fn fresh(name: &str) -> (Scratch, std::path::PathBuf, DynVaultAsync) {
    let scratch = Scratch::new(name);
    let path = scratch.join("vault");
    let options: OpenOptions = serde_json::from_value(json!({
        "writeTimeoutMs": 30000,
        "author": {"name": "Ada", "email": "ada@example.test"}
    }))
    .unwrap();
    let vault = open(&path, options).await.unwrap();
    declare(&vault, json!({"name": "notes", "schema": {"standard": {}}})).await;
    declare(&vault, json!({"name": "log", "schema": {"journal": {}}})).await;
    declare(
        &vault,
        json!({"name": "config", "schema": {"singleton": {"id": "settings"}}}),
    )
    .await;
    declare(
        &vault,
        json!({"name": "diary", "schema": {"standard": {}, "isolated": true}}),
    )
    .await;
    (scratch, path, vault)
}

async fn put(collection: &DynCollectionAsync, id: &str, raw: &str, note: &str) -> Document {
    collection
        .upsert(
            DocumentUpsert::default()
                .with_id(id)
                .with_raw(raw.as_bytes().to_vec())
                .with_note(note),
        )
        .await
        .unwrap()
        .document
        .into_option()
        .unwrap()
}

async fn append(collection: &DynCollectionAsync, id: &str, raw: &str) {
    collection
        .create(
            scheda::v1::DocumentCreation::default()
                .with_id(id)
                .with_raw(raw.as_bytes().to_vec())
                .with_note("line"),
        )
        .await
        .unwrap();
}

fn key(id: &str) -> DocumentId {
    DocumentId::default().with_id(id)
}

fn ids(include_raw: bool, ids: &[&str]) -> DocumentSelection {
    DocumentSelection {
        include_raw: Some(include_raw),
        selection: Some(Selection::Ids(Box::new(Ids {
            ids: ids.iter().map(|id| (*id).to_owned()).collect(),
            ..Ids::default()
        }))),
        ..DocumentSelection::default()
    }
}

fn listing(include_raw: bool, limit: Option<u32>, start_after: Option<&str>) -> DocumentSelection {
    DocumentSelection {
        include_raw: Some(include_raw),
        selection: Some(Selection::Listing(Box::new(Listing {
            limit,
            start_after: start_after.map(str::to_owned),
            ..Listing::default()
        }))),
        ..DocumentSelection::default()
    }
}

/// The ids of the Documents a selection gives, in order.
async fn ids_of(collection: &DynCollectionAsync, selection: DocumentSelection) -> Vec<String> {
    let stream = collection.get_many(selection).await.unwrap();
    all(stream)
        .await
        .unwrap()
        .into_iter()
        .map(|document| document.id.unwrap())
        .collect()
}

fn text(document: &Document) -> &str {
    std::str::from_utf8(document.raw.as_deref().unwrap_or_default()).unwrap()
}

async fn history(collection: &DynCollectionAsync, id: &str) -> Vec<DocumentHistoryEntry> {
    all(collection.history(key(id)).await.unwrap())
        .await
        .unwrap()
}

/// The `write_id` of each version, newest first.
async fn write_ids(collection: &DynCollectionAsync, id: &str) -> Vec<String> {
    history(collection, id)
        .await
        .into_iter()
        .map(|version| version.write_id.unwrap())
        .collect()
}

fn deleting(id: &str, note: &str) -> Operation {
    Operation::Delete(Box::new(
        DocumentDeletion::default().with_id(id).with_note(note),
    ))
}

fn creating(id: &str, raw: &str, note: &str) -> Operation {
    Operation::Create(Box::new(
        DocumentCreation::default()
            .with_id(id)
            .with_raw(raw.as_bytes().to_vec())
            .with_note(note),
    ))
}

fn in_collection(operation: Operation) -> DocumentOperation {
    DocumentOperation {
        operation: Some(operation),
        ..DocumentOperation::default()
    }
}

fn in_vault(collection: &str, operation: Operation) -> VaultDocumentOperation {
    VaultDocumentOperation {
        collection: Some(collection.to_owned()),
        operation: in_collection(operation).into(),
        ..VaultDocumentOperation::default()
    }
}

#[test]
fn an_arc_dyn_vault_sync_from_open_drives_a_vault_end_to_end() {
    both(|| async {
        let scratch = Scratch::new("reads-end-to-end");
        let path = scratch.join("vault");
        let vault = Scheda
            .open(opening(&path, OpenOptions::default()))
            .await
            .unwrap()
            .to_async_inline()
            .unwrap();
        declare(&vault, json!({"name": "notes", "schema": {"standard": {}}})).await;
        let notes = collection(&vault, "notes").await;
        put(&notes, "a.md", "one", "first").await;
        put(&notes, "a.md", "two", "second").await;
        put(&notes, "b.md", "other", "third").await;

        let got: Vec<Document> = all(notes.get_many(listing(true, None, None)).await.unwrap())
            .await
            .unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(text(&got[0]), "two");
        assert_eq!(got[0].collection.as_deref(), Some("notes"));
        let versions = history(&notes, "a.md").await;
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].note.as_deref(), Some("second"));
        assert_eq!(versions[1].note.as_deref(), Some("first"));
        assert!(
            vault
                .check(Empty::default())
                .await
                .unwrap()
                .problems
                .is_empty()
        );
        vault.close(Empty::default()).await.unwrap();
    });
}

#[test]
fn a_selection_with_neither_ids_nor_listing_is_invalid_argument() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-neither").await;
        let notes = collection(&vault, "notes").await;
        let error = notes
            .get_many(DocumentSelection::default().with_include_raw(true))
            .await
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument));
    });
}

#[test]
fn get_many_of_an_unknown_collection_is_not_found_after_it_is_deleted() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-unknown").await;
        let notes = collection(&vault, "notes").await;
        vault
            .delete_collection(scheda::v1::CollectionDeletion::default().with_name("notes"))
            .await
            .unwrap();
        assert!(
            notes
                .get_many(listing(true, None, None))
                .await
                .err()
                .unwrap()
                .is(Code::NotFound)
        );
        assert!(
            notes
                .history(key("a.md"))
                .await
                .err()
                .unwrap()
                .is(Code::NotFound)
        );
    });
}

#[test]
fn ids_give_exactly_those_in_the_order_given_and_skip_the_absent() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-ids").await;
        let notes = collection(&vault, "notes").await;
        for id in ["a.md", "b.md", "c.md", "d/e.md"] {
            put(&notes, id, id, "put").await;
        }
        assert_eq!(
            ids_of(&notes, ids(true, &["d/e.md", "missing.md", "a.md", "c.md"])).await,
            ["d/e.md", "a.md", "c.md"]
        );
        assert!(ids_of(&notes, ids(true, &[])).await.is_empty());
    });
}

#[test]
fn an_id_that_breaks_the_rules_of_the_collection_is_invalid_argument() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-bad-id").await;
        let notes = collection(&vault, "notes").await;
        let error = notes
            .get_many(ids(true, &["a.md", "../escape"]))
            .await
            .err()
            .unwrap();
        assert!(error.is(Code::InvalidArgument));
        let error = notes.history(key("../escape")).await.err().unwrap();
        assert!(error.is(Code::InvalidArgument));
    });
}

#[test]
fn without_include_raw_a_document_has_keys_and_a_revision_and_no_bytes() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-keys").await;
        let notes = collection(&vault, "notes").await;
        let stored = put(&notes, "a.md", "bytes", "put").await;
        for selection in [listing(false, None, None), ids(false, &["a.md"])] {
            let got: Vec<Document> = all(notes.get_many(selection).await.unwrap()).await.unwrap();
            assert_eq!(got.len(), 1);
            assert_eq!(got[0].collection.as_deref(), Some("notes"));
            assert_eq!(got[0].id.as_deref(), Some("a.md"));
            assert_eq!(got[0].revision, stored.revision);
            assert!(got[0].raw.is_none());
        }
        // Unset is the same as false.
        let got = notes
            .get_many(DocumentSelection {
                selection: Some(Selection::Listing(Box::default())),
                ..DocumentSelection::default()
            })
            .await
            .unwrap();
        let got: Vec<Document> = all(got).await.unwrap();
        assert!(got[0].raw.is_none());
    });
}

#[test]
fn a_listing_is_in_id_order_whole_ids_and_reads_no_file_but_the_commit() {
    both(|| async {
        let (_scratch, path, vault) = fresh("reads-order").await;
        let notes = collection(&vault, "notes").await;
        for id in ["c.md", "b/x.md", "b.md", "a.md", "b/a/y.md"] {
            put(&notes, id, id, "put").await;
        }
        let before = snapshot(&path);
        assert_eq!(
            ids_of(&notes, listing(true, None, None)).await,
            ["a.md", "b.md", "b/a/y.md", "b/x.md", "c.md"]
        );
        assert_eq!(snapshot(&path), before, "a read writes nothing");
    });
}

#[test]
fn pages_cover_a_collection_once_also_when_documents_are_added_between() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-pages").await;
        let notes = collection(&vault, "notes").await;
        let originals = ["b.md", "d.md", "e/f.md", "g.md", "i.md", "k.md", "m/n.md"];
        for id in originals {
            put(&notes, id, id, "put").await;
        }
        let mut seen = Vec::new();
        let mut after: Option<String> = None;
        let mut round = 0;
        loop {
            let page = ids_of(&notes, listing(false, Some(3), after.as_deref())).await;
            if page.is_empty() {
                break;
            }
            seen.extend(page.iter().cloned());
            after = page.last().cloned();
            round += 1;
            // A Document added before the place the pages have reached is not
            // seen; one added after it is.
            put(&notes, &format!("a{round}.md"), "early", "put").await;
            put(&notes, &format!("z{round}.md"), "late", "put").await;
        }
        let mut once = seen.clone();
        once.sort();
        once.dedup();
        assert_eq!(once, seen, "no Document twice, in order");
        for id in originals {
            assert!(seen.iter().any(|seen| seen == id), "{id} was missed");
        }
        assert!(!seen.iter().any(|id| id == "a1.md"));
        assert!(seen.iter().any(|id| id == "z1.md"));
    });
}

#[test]
fn a_start_after_that_is_no_document_is_a_place_in_the_order() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-place").await;
        let notes = collection(&vault, "notes").await;
        for id in ["b.md", "c/d.md", "e.md"] {
            put(&notes, id, id, "put").await;
        }
        assert_eq!(
            ids_of(&notes, listing(false, None, Some("b"))).await,
            ["b.md", "c/d.md", "e.md"]
        );
        assert_eq!(
            ids_of(&notes, listing(false, None, Some("c/"))).await,
            ["c/d.md", "e.md"]
        );
        assert_eq!(
            ids_of(&notes, listing(false, None, Some("c/d.md"))).await,
            ["e.md"]
        );
        assert!(
            ids_of(&notes, listing(false, None, Some("z")))
                .await
                .is_empty()
        );
        assert_eq!(
            ids_of(&notes, listing(false, Some(1), None)).await,
            ["b.md"]
        );
        assert!(
            ids_of(&notes, listing(false, Some(0), None))
                .await
                .is_empty()
        );
    });
}

#[test]
fn a_singleton_lists_its_one_document() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-singleton").await;
        let config = collection(&vault, "config").await;
        assert!(ids_of(&config, listing(true, None, None)).await.is_empty());
        put(&config, "settings", "on", "put").await;
        let got: Vec<Document> = all(config.get_many(listing(true, None, None)).await.unwrap())
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(text(&got[0]), "on");
        assert!(
            ids_of(&config, listing(true, None, Some("settings")))
                .await
                .is_empty()
        );
        assert_eq!(
            ids_of(&config, ids(false, &["other", "settings"])).await,
            ["settings"]
        );
    });
}

#[test]
fn a_journal_lists_in_append_order_with_pages_by_line() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-journal").await;
        let log = collection(&vault, "log").await;
        for id in ["z1", "a2", "m3", "b4"] {
            append(&log, id, &format!("line {id}")).await;
        }
        let got: Vec<Document> = all(log.get_many(listing(true, None, None)).await.unwrap())
            .await
            .unwrap();
        assert_eq!(
            got.iter()
                .map(|d| d.id.clone().unwrap())
                .collect::<Vec<_>>(),
            ["z1", "a2", "m3", "b4"]
        );
        assert_eq!(text(&got[1]), "line a2");
        assert!(got[1].revision.is_none(), "a line has no revision");
        assert_eq!(
            ids_of(&log, listing(false, Some(2), None)).await,
            ["z1", "a2"]
        );
        assert_eq!(
            ids_of(&log, listing(false, Some(2), Some("a2"))).await,
            ["m3", "b4"]
        );
        // Where no line has the id there is no "after".
        assert!(
            ids_of(&log, listing(false, None, Some("nope")))
                .await
                .is_empty()
        );
        let keys: Vec<Document> = all(log.get_many(listing(false, Some(1), None)).await.unwrap())
            .await
            .unwrap();
        assert!(keys[0].raw.is_none());
        assert_eq!(
            ids_of(&log, ids(true, &["m3", "absent", "z1"])).await,
            ["m3", "z1"]
        );
    });
}

#[test]
fn a_stream_started_before_a_write_does_not_see_it() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-snapshot").await;
        let notes = collection(&vault, "notes").await;
        put(&notes, "a.md", "a", "put").await;
        put(&notes, "c.md", "c", "put").await;
        let mut stream = Box::pin(notes.get_many(listing(true, None, None)).await.unwrap());
        assert_eq!(
            next(&mut stream).await.unwrap().unwrap().id.as_deref(),
            Some("a.md")
        );
        put(&notes, "b.md", "b", "put").await;
        put(&notes, "c.md", "changed", "put").await;
        notes
            .delete(
                DocumentDeletion::default()
                    .with_id("a.md")
                    .with_note("gone"),
            )
            .await
            .unwrap();
        let rest: Vec<Document> = all(stream).await.unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id.as_deref(), Some("c.md"));
        assert_eq!(text(&rest[0]), "c");
        assert_eq!(
            ids_of(&notes, listing(false, None, None)).await,
            ["b.md", "c.md"]
        );
    });
}

#[test]
fn after_close_the_next_item_of_a_held_stream_is_failed_precondition() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-close").await;
        let notes = collection(&vault, "notes").await;
        for id in ["a.md", "b.md", "c.md"] {
            put(&notes, id, id, "put").await;
        }
        let mut documents = Box::pin(notes.get_many(listing(true, None, None)).await.unwrap());
        let mut versions = Box::pin(notes.history(key("a.md")).await.unwrap());
        assert!(next(&mut documents).await.unwrap().is_ok());
        vault.close(Empty::default()).await.unwrap();
        let error = next(&mut documents).await.unwrap().unwrap_err();
        assert!(error.is(Code::FailedPrecondition));
        assert!(next(&mut documents).await.is_none());
        let error: Error = next(&mut versions).await.unwrap().unwrap_err();
        assert!(error.is(Code::FailedPrecondition));
    });
}

#[test]
fn history_lists_versions_newest_first_with_revision_author_note_and_bytes() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-history").await;
        let notes = collection(&vault, "notes").await;
        let one = put(&notes, "a.md", "one", "first\nmore").await;
        let two = put(&notes, "a.md", "two", "second").await;
        put(&notes, "other.md", "x", "elsewhere").await;
        notes
            .delete(
                DocumentDeletion::default()
                    .with_id("a.md")
                    .with_note("removed"),
            )
            .await
            .unwrap();
        let three = put(&notes, "a.md", "three", "again").await;
        let versions = history(&notes, "a.md").await;
        assert_eq!(versions.len(), 4);
        let revisions: Vec<_> = versions.iter().map(|v| v.revision.clone()).collect();
        assert_eq!(
            revisions,
            [three.revision, None, two.revision, one.revision]
        );
        let notes_of: Vec<_> = versions.iter().map(|v| v.note.clone().unwrap()).collect();
        assert_eq!(notes_of, ["again", "removed", "second", "first\nmore"]);
        let raws: Vec<_> = versions
            .iter()
            .map(|v| v.raw.clone().unwrap_or_default())
            .collect();
        assert_eq!(raws, [&b"three"[..], b"", b"two", b"one"]);
        for version in &versions {
            let author = version.author.as_option().unwrap();
            assert_eq!(author.name.as_deref(), Some("Ada"));
            assert_eq!(author.email.as_deref(), Some("ada@example.test"));
            assert!(version.time.as_option().unwrap().seconds > 1_700_000_000);
        }
        let times: Vec<_> = versions
            .iter()
            .map(|v| v.time.as_option().unwrap().seconds)
            .collect();
        assert!(times.windows(2).all(|pair| pair[0] >= pair[1]));
        let writes: HashSet<_> = versions.iter().map(|v| v.write_id.clone()).collect();
        assert_eq!(writes.len(), 4, "four writes, four write ids");
        assert!(!writes.contains(&None) && !writes.contains(&Some(String::new())));
    });
}

#[test]
fn one_write_gives_every_version_it_made_one_write_id() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-write-id-batch").await;
        let notes = collection(&vault, "notes").await;
        let config = collection(&vault, "config").await;
        put(&notes, "old.md", "record", "seed").await;
        // A record that moves from one id to another, and a write to another
        // collection, in one batch of the Vault.
        vault
            .batch_documents(stream_of(vec![
                in_vault("notes", deleting("old.md", "move")),
                in_vault("notes", creating("new.md", "record", "move")),
                in_vault("config", creating("settings", "S", "move")),
            ]))
            .await
            .unwrap();
        let old = history(&notes, "old.md").await;
        assert_eq!(old.len(), 2);
        assert_eq!(
            old[0].raw.as_deref().unwrap_or_default(),
            b"",
            "the deletion"
        );
        let moved = old[0].write_id.clone().unwrap();
        assert!(!moved.is_empty(), "the deleting version has one");
        assert_ne!(old[1].write_id.as_deref(), Some(moved.as_str()), "the seed");
        assert_eq!(
            write_ids(&notes, "new.md").await,
            std::slice::from_ref(&moved)
        );
        assert_eq!(
            write_ids(&config, "settings").await,
            std::slice::from_ref(&moved)
        );

        // A batch of one collection, too.
        notes
            .batch(stream_of(vec![
                in_collection(deleting("new.md", "again")),
                in_collection(creating("newer.md", "record", "again")),
            ]))
            .await
            .unwrap();
        let new = write_ids(&notes, "new.md").await;
        assert_eq!(new.len(), 2);
        assert_eq!(new[1], moved);
        assert_ne!(new[0], moved);
        assert_eq!(write_ids(&notes, "newer.md").await, [new[0].clone()]);
    });
}

#[test]
fn two_writes_in_one_second_with_one_note_have_two_write_ids() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-write-id-second").await;
        let notes = collection(&vault, "notes").await;
        let diary = collection(&vault, "diary").await;
        // The same move as two calls instead of one batch. A second may tick
        // between them; then the pair is written again under other ids.
        for attempt in 0..5 {
            let (from, to) = (format!("from{attempt}.md"), format!("to{attempt}.md"));
            put(&notes, &from, "record", "seed").await;
            notes
                .delete(DocumentDeletion::default().with_id(&from).with_note("move"))
                .await
                .unwrap();
            put(&notes, &to, "record", "move").await;
            put(&diary, &to, "record", "move").await;
            let deletion = history(&notes, &from).await.remove(0);
            let creation = history(&notes, &to).await.remove(0);
            let isolated = history(&diary, &to).await.remove(0);
            if deletion.time != creation.time || creation.time != isolated.time {
                continue;
            }
            assert_eq!(deletion.note, creation.note);
            let ids: HashSet<_> = [deletion.write_id, creation.write_id, isolated.write_id]
                .into_iter()
                .collect();
            assert_eq!(ids.len(), 3, "three writes, the isolated one included");
            return;
        }
        panic!("no attempt wrote its three versions in one second");
    });
}

#[test]
fn a_write_id_is_the_same_on_every_read_after_a_reopen_and_in_a_clone() {
    both(|| async {
        let (scratch, path, vault) = fresh("reads-write-id-stable").await;
        let notes = collection(&vault, "notes").await;
        put(&notes, "a.md", "one", "first").await;
        put(&notes, "a.md", "two", "second").await;
        notes
            .delete(
                DocumentDeletion::default()
                    .with_id("a.md")
                    .with_note("gone"),
            )
            .await
            .unwrap();
        let ids = write_ids(&notes, "a.md").await;
        assert_eq!(ids.len(), 3);
        assert_eq!(write_ids(&notes, "a.md").await, ids, "every read");
        vault.close(Empty::default()).await.unwrap();

        let vault = open(&path, OpenOptions::default()).await.unwrap();
        let notes = collection(&vault, "notes").await;
        assert_eq!(write_ids(&notes, "a.md").await, ids, "after a reopen");

        let copy = scratch.join("clone");
        git(
            &path,
            &[
                "clone",
                "--quiet",
                &path.to_string_lossy(),
                &copy.to_string_lossy(),
            ],
        );
        let clone = open(&copy, OpenOptions::default()).await.unwrap();
        let notes = collection(&clone, "notes").await;
        assert_eq!(write_ids(&notes, "a.md").await, ids, "in a clone");
    });
}

#[test]
fn history_of_a_document_never_stored_is_empty_and_a_rewrite_of_equal_bytes_is_no_version() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-history-empty").await;
        let notes = collection(&vault, "notes").await;
        put(&notes, "a.md", "same", "first").await;
        put(&notes, "a.md", "same", "again").await;
        assert!(history(&notes, "never.md").await.is_empty());
        assert_eq!(history(&notes, "a.md").await.len(), 1);
        let config = collection(&vault, "config").await;
        assert!(history(&config, "settings").await.is_empty());
        assert!(history(&config, "other").await.is_empty());
    });
}

#[test]
fn history_of_a_journal_is_unimplemented() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-history-journal").await;
        let log = collection(&vault, "log").await;
        append(&log, "a1", "line").await;
        assert!(
            log.history(key("a1"))
                .await
                .err()
                .unwrap()
                .is(Code::Unimplemented)
        );
    });
}

#[test]
fn history_of_an_isolated_collection_comes_from_the_nested_repository() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-history-isolated").await;
        let diary = collection(&vault, "diary").await;
        put(&diary, "day.md", "one", "morning").await;
        put(&diary, "day.md", "two", "evening").await;
        let notes = collection(&vault, "notes").await;
        put(&notes, "day.md", "main", "main one").await;
        let versions = history(&diary, "day.md").await;
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].note.as_deref(), Some("evening"));
        assert_eq!(versions[0].raw.as_deref(), Some(&b"two"[..]));
        assert_eq!(versions[1].raw.as_deref(), Some(&b"one"[..]));
        assert_eq!(history(&notes, "day.md").await.len(), 1);
    });
}

#[test]
fn a_commit_made_by_hand_is_a_version_with_no_note() {
    both(|| async {
        let (_scratch, path, vault) = fresh("reads-history-hand").await;
        let notes = collection(&vault, "notes").await;
        put(&notes, "a.md", "one", "first").await;
        write(&path, "notes/a.md", "by hand");
        git(&path, &["add", "notes/a.md"]);
        git(&path, &["commit", "--quiet", "--message", "edit"]);
        let versions = history(&notes, "a.md").await;
        assert_eq!(versions.len(), 2);
        assert!(versions[0].note.is_none());
        assert_eq!(versions[0].raw.as_deref(), Some(&b"by hand"[..]));
        assert_eq!(
            versions[0].author.as_option().unwrap().name.as_deref(),
            Some("Test")
        );
        assert_eq!(versions[1].note.as_deref(), Some("first"));
    });
}

#[test]
fn check_lists_every_problem_of_a_vault_with_several() {
    both(|| async {
        let scratch = Scratch::new("reads-check");
        let path = scratch.join("vault");
        built_vault(&path);
        write(&path, "log/a.jsonl", "a1: fine\nbroken line\n");
        git(&path, &["add", "--all"]);
        git(&path, &["commit", "--quiet", "--message", "a broken line"]);
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        // A person's edit that is not committed.
        write(&path, "notes/a.md", "edited, not committed");
        let report = vault.check(Empty::default()).await.unwrap();
        let paths: Vec<_> = report
            .problems
            .iter()
            .map(|p| {
                (
                    p.code.and_then(|code| code.as_known()),
                    p.path.clone().unwrap_or_default(),
                )
            })
            .collect();
        assert!(
            paths
                .iter()
                .any(|(code, path)| *code == Some(Code::InvalidArgument) && path == "log/a.jsonl"),
            "{paths:?}"
        );
        assert!(
            paths
                .iter()
                .any(|(code, path)| *code == Some(Code::FailedPrecondition) && path == "notes/a.md"),
            "{paths:?}"
        );
        assert!(report.problems.len() >= 2);
    });
}

#[test]
fn check_of_a_sound_vault_is_the_empty_report() {
    both(|| async {
        let (_scratch, _path, vault) = fresh("reads-check-sound").await;
        let notes = collection(&vault, "notes").await;
        put(&notes, "a.md", "one", "first").await;
        assert!(
            vault
                .check(Empty::default())
                .await
                .unwrap()
                .problems
                .is_empty()
        );
    });
}

#[test]
fn check_fails_only_when_it_cannot_run() {
    both(|| async {
        let scratch = Scratch::new("reads-check-cannot");
        let path = scratch.join("vault");
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        declare(
            &vault,
            json!({"name": "sealed", "schema": {"standard": {}, "encrypted": true}}),
        )
        .await;
        // The collection is encrypted and the handle has no secret: its files
        // cannot be opened to be checked.
        let error = vault.check(Empty::default()).await.err().unwrap();
        let expected = if cfg!(feature = "encrypt") {
            Code::Crypto
        } else {
            Code::Unimplemented
        };
        assert!(error.is(expected));
    });
}

#[cfg(feature = "encrypt")]
mod encrypted {
    use super::*;

    const IDENTITY: &str =
        "AGE-SECRET-KEY-1RN2D82PZQK97437LWJVQJK4Q6EQVHLQ9A8Z0QTAXZ424HV8XZZPS5Q9860";

    #[test]
    fn include_raw_decrypts_and_history_gives_plain_bytes() {
        both(|| async {
            let scratch = Scratch::new("reads-encrypted");
            let path = scratch.join("vault");
            let options: OpenOptions = serde_json::from_value(json!({
                "encryption": {"identity": IDENTITY},
                "writeTimeoutMs": 60000
            }))
            .unwrap();
            let vault = open(&path, options).await.unwrap();
            declare(
                &vault,
                json!({"name": "secrets", "schema": {"standard": {}, "encrypted": true}}),
            )
            .await;
            let secrets = collection(&vault, "secrets").await;
            let one = put(&secrets, "k.md", "first secret", "one").await;
            put(&secrets, "k.md", "second secret", "two").await;
            let got: Vec<Document> =
                all(secrets.get_many(listing(true, None, None)).await.unwrap())
                    .await
                    .unwrap();
            assert_eq!(text(&got[0]), "second secret");
            let keys: Vec<Document> =
                all(secrets.get_many(listing(false, None, None)).await.unwrap())
                    .await
                    .unwrap();
            assert!(keys[0].raw.is_none());
            assert_eq!(keys[0].revision, got[0].revision);
            let versions = history(&secrets, "k.md").await;
            assert_eq!(versions[0].raw.as_deref(), Some(&b"second secret"[..]));
            assert_eq!(versions[1].raw.as_deref(), Some(&b"first secret"[..]));
            assert_eq!(versions[1].revision, one.revision);
            vault.close(Empty::default()).await.unwrap();

            // Without the secret, the bytes are the one thing it cannot give.
            let bare = open(&path, OpenOptions::default()).await.unwrap();
            let secrets = collection(&bare, "secrets").await;
            assert!(
                secrets
                    .get_many(listing(true, None, None))
                    .await
                    .err()
                    .unwrap()
                    .is(Code::Crypto)
            );
            assert_eq!(ids_of(&secrets, listing(false, None, None)).await, ["k.md"]);
        });
    }
}
