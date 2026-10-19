//! The reads that return many things, through the objects of the contract:
//! `Collection.GetMany` and `Collection.History` (lazy streams on one
//! snapshot) and `Vault.Check` (a report). The laziness checks count blob
//! reads, which only the library can; they are unit tests in `src/reads.rs`.

mod common;

use std::sync::Arc;

use buffa_types::google::protobuf::Empty;
use scheda::v1::SchedaSync;
use scheda::v1::document_selection::Selection;
use scheda::v1::{
    CollectionDeclaration, CollectionSync, Document, DocumentDeletion, DocumentHistoryEntry,
    DocumentId, DocumentSelection, DocumentUpsert, Ids, Listing, OpenOptions,
    VaultErrorCode as Code, VaultSync,
};
use scheda::{Error, Scheda};
use serde_json::json;

use common::{Scratch, open, opening, snapshot, vault as built_vault, write};

fn declare(vault: &Arc<dyn VaultSync>, value: serde_json::Value) {
    let declaration: CollectionDeclaration = serde_json::from_value(value).unwrap();
    vault.create_collection(declaration).unwrap();
}

fn collection(vault: &Arc<dyn VaultSync>, name: &str) -> Arc<dyn CollectionSync> {
    vault
        .get_collection(scheda::v1::CollectionKey::default().with_name(name))
        .unwrap()
        .to_sync()
        .unwrap()
}

/// A Vault with `notes` (standard), `log` (journal), `config` (a singleton
/// with the id `settings`), and `diary` (standard, isolated).
fn fresh(name: &str) -> (Scratch, std::path::PathBuf, Arc<dyn VaultSync>) {
    let scratch = Scratch::new(name);
    let path = scratch.join("vault");
    let options: OpenOptions = serde_json::from_value(json!({
        "writeTimeoutMs": 30000,
        "author": {"name": "Ada", "email": "ada@example.test"}
    }))
    .unwrap();
    let vault = open(&path, options).unwrap();
    declare(&vault, json!({"name": "notes", "schema": {"standard": {}}}));
    declare(&vault, json!({"name": "log", "schema": {"journal": {}}}));
    declare(
        &vault,
        json!({"name": "config", "schema": {"singleton": {"id": "settings"}}}),
    );
    declare(
        &vault,
        json!({"name": "diary", "schema": {"standard": {}, "isolated": true}}),
    );
    (scratch, path, vault)
}

fn put(collection: &Arc<dyn CollectionSync>, id: &str, raw: &str, note: &str) -> Document {
    collection
        .upsert(
            DocumentUpsert::default()
                .with_id(id)
                .with_raw(raw.as_bytes().to_vec())
                .with_note(note),
        )
        .unwrap()
        .document
        .into_option()
        .unwrap()
}

fn append(collection: &Arc<dyn CollectionSync>, id: &str, raw: &str) {
    collection
        .create(
            scheda::v1::DocumentCreation::default()
                .with_id(id)
                .with_raw(raw.as_bytes().to_vec())
                .with_note("line"),
        )
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
fn ids_of(collection: &Arc<dyn CollectionSync>, selection: DocumentSelection) -> Vec<String> {
    collection
        .get_many(selection)
        .unwrap()
        .map(|item| item.unwrap().id.unwrap())
        .collect()
}

fn text(document: &Document) -> &str {
    std::str::from_utf8(document.raw.as_deref().unwrap_or_default()).unwrap()
}

fn history(collection: &Arc<dyn CollectionSync>, id: &str) -> Vec<DocumentHistoryEntry> {
    collection
        .history(key(id))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn an_arc_dyn_vault_sync_from_open_drives_a_vault_end_to_end() {
    let scratch = Scratch::new("reads-end-to-end");
    let path = scratch.join("vault");
    let vault = Scheda
        .open(opening(&path, OpenOptions::default()))
        .unwrap()
        .to_sync()
        .unwrap();
    declare(&vault, json!({"name": "notes", "schema": {"standard": {}}}));
    let notes = collection(&vault, "notes");
    put(&notes, "a.md", "one", "first");
    put(&notes, "a.md", "two", "second");
    put(&notes, "b.md", "other", "third");

    let got: Vec<Document> = notes
        .get_many(listing(true, None, None))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(got.len(), 2);
    assert_eq!(text(&got[0]), "two");
    assert_eq!(got[0].collection.as_deref(), Some("notes"));
    let versions = history(&notes, "a.md");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].note.as_deref(), Some("second"));
    assert_eq!(versions[1].note.as_deref(), Some("first"));
    assert!(vault.check(Empty::default()).unwrap().problems.is_empty());
    vault.close(Empty::default()).unwrap();
}

#[test]
fn a_selection_with_neither_ids_nor_listing_is_invalid_argument() {
    let (_scratch, _path, vault) = fresh("reads-neither");
    let notes = collection(&vault, "notes");
    let error = notes
        .get_many(DocumentSelection::default().with_include_raw(true))
        .err()
        .unwrap();
    assert!(error.is(Code::InvalidArgument));
}

#[test]
fn get_many_of_an_unknown_collection_is_not_found_after_it_is_deleted() {
    let (_scratch, _path, vault) = fresh("reads-unknown");
    let notes = collection(&vault, "notes");
    vault
        .delete_collection(scheda::v1::CollectionDeletion::default().with_name("notes"))
        .unwrap();
    assert!(
        notes
            .get_many(listing(true, None, None))
            .err()
            .unwrap()
            .is(Code::NotFound)
    );
    assert!(notes.history(key("a.md")).err().unwrap().is(Code::NotFound));
}

#[test]
fn ids_give_exactly_those_in_the_order_given_and_skip_the_absent() {
    let (_scratch, _path, vault) = fresh("reads-ids");
    let notes = collection(&vault, "notes");
    for id in ["a.md", "b.md", "c.md", "d/e.md"] {
        put(&notes, id, id, "put");
    }
    assert_eq!(
        ids_of(&notes, ids(true, &["d/e.md", "missing.md", "a.md", "c.md"])),
        ["d/e.md", "a.md", "c.md"]
    );
    assert!(ids_of(&notes, ids(true, &[])).is_empty());
}

#[test]
fn an_id_that_breaks_the_rules_of_the_collection_is_invalid_argument() {
    let (_scratch, _path, vault) = fresh("reads-bad-id");
    let notes = collection(&vault, "notes");
    let error = notes
        .get_many(ids(true, &["a.md", "../escape"]))
        .err()
        .unwrap();
    assert!(error.is(Code::InvalidArgument));
    let error = notes.history(key("../escape")).err().unwrap();
    assert!(error.is(Code::InvalidArgument));
}

#[test]
fn without_include_raw_a_document_has_keys_and_a_revision_and_no_bytes() {
    let (_scratch, _path, vault) = fresh("reads-keys");
    let notes = collection(&vault, "notes");
    let stored = put(&notes, "a.md", "bytes", "put");
    for selection in [listing(false, None, None), ids(false, &["a.md"])] {
        let got: Vec<Document> = notes
            .get_many(selection)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].collection.as_deref(), Some("notes"));
        assert_eq!(got[0].id.as_deref(), Some("a.md"));
        assert_eq!(got[0].revision, stored.revision);
        assert!(got[0].raw.is_none());
    }
    // Unset is the same as false.
    let got: Vec<Document> = notes
        .get_many(DocumentSelection {
            selection: Some(Selection::Listing(Box::default())),
            ..DocumentSelection::default()
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(got[0].raw.is_none());
}

#[test]
fn a_listing_is_in_id_order_whole_ids_and_reads_no_file_but_the_commit() {
    let (_scratch, path, vault) = fresh("reads-order");
    let notes = collection(&vault, "notes");
    for id in ["c.md", "b/x.md", "b.md", "a.md", "b/a/y.md"] {
        put(&notes, id, id, "put");
    }
    let before = snapshot(&path);
    assert_eq!(
        ids_of(&notes, listing(true, None, None)),
        ["a.md", "b.md", "b/a/y.md", "b/x.md", "c.md"]
    );
    assert_eq!(snapshot(&path), before, "a read writes nothing");
}

#[test]
fn pages_cover_a_collection_once_also_when_documents_are_added_between() {
    let (_scratch, _path, vault) = fresh("reads-pages");
    let notes = collection(&vault, "notes");
    let originals = ["b.md", "d.md", "e/f.md", "g.md", "i.md", "k.md", "m/n.md"];
    for id in originals {
        put(&notes, id, id, "put");
    }
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    let mut round = 0;
    loop {
        let page = ids_of(&notes, listing(false, Some(3), after.as_deref()));
        if page.is_empty() {
            break;
        }
        seen.extend(page.iter().cloned());
        after = page.last().cloned();
        round += 1;
        // A Document added before the place the pages have reached is not
        // seen; one added after it is.
        put(&notes, &format!("a{round}.md"), "early", "put");
        put(&notes, &format!("z{round}.md"), "late", "put");
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
}

#[test]
fn a_start_after_that_is_no_document_is_a_place_in_the_order() {
    let (_scratch, _path, vault) = fresh("reads-place");
    let notes = collection(&vault, "notes");
    for id in ["b.md", "c/d.md", "e.md"] {
        put(&notes, id, id, "put");
    }
    assert_eq!(
        ids_of(&notes, listing(false, None, Some("b"))),
        ["b.md", "c/d.md", "e.md"]
    );
    assert_eq!(
        ids_of(&notes, listing(false, None, Some("c/"))),
        ["c/d.md", "e.md"]
    );
    assert_eq!(
        ids_of(&notes, listing(false, None, Some("c/d.md"))),
        ["e.md"]
    );
    assert!(ids_of(&notes, listing(false, None, Some("z"))).is_empty());
    assert_eq!(ids_of(&notes, listing(false, Some(1), None)), ["b.md"]);
    assert!(ids_of(&notes, listing(false, Some(0), None)).is_empty());
}

#[test]
fn a_singleton_lists_its_one_document() {
    let (_scratch, _path, vault) = fresh("reads-singleton");
    let config = collection(&vault, "config");
    assert!(ids_of(&config, listing(true, None, None)).is_empty());
    put(&config, "settings", "on", "put");
    let got: Vec<Document> = config
        .get_many(listing(true, None, None))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(got.len(), 1);
    assert_eq!(text(&got[0]), "on");
    assert!(ids_of(&config, listing(true, None, Some("settings"))).is_empty());
    assert_eq!(
        ids_of(&config, ids(false, &["other", "settings"])),
        ["settings"]
    );
}

#[test]
fn a_journal_lists_in_append_order_with_pages_by_line() {
    let (_scratch, _path, vault) = fresh("reads-journal");
    let log = collection(&vault, "log");
    for id in ["z1", "a2", "m3", "b4"] {
        append(&log, id, &format!("line {id}"));
    }
    let got: Vec<Document> = log
        .get_many(listing(true, None, None))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        got.iter()
            .map(|d| d.id.clone().unwrap())
            .collect::<Vec<_>>(),
        ["z1", "a2", "m3", "b4"]
    );
    assert_eq!(text(&got[1]), "line a2");
    assert!(got[1].revision.is_none(), "a line has no revision");
    assert_eq!(ids_of(&log, listing(false, Some(2), None)), ["z1", "a2"]);
    assert_eq!(
        ids_of(&log, listing(false, Some(2), Some("a2"))),
        ["m3", "b4"]
    );
    // Where no line has the id there is no "after".
    assert!(ids_of(&log, listing(false, None, Some("nope"))).is_empty());
    let keys: Vec<Document> = log
        .get_many(listing(false, Some(1), None))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(keys[0].raw.is_none());
    assert_eq!(
        ids_of(&log, ids(true, &["m3", "absent", "z1"])),
        ["m3", "z1"]
    );
}

#[test]
fn a_stream_started_before_a_write_does_not_see_it() {
    let (_scratch, _path, vault) = fresh("reads-snapshot");
    let notes = collection(&vault, "notes");
    put(&notes, "a.md", "a", "put");
    put(&notes, "c.md", "c", "put");
    let mut stream = notes.get_many(listing(true, None, None)).unwrap();
    assert_eq!(stream.next().unwrap().unwrap().id.as_deref(), Some("a.md"));
    put(&notes, "b.md", "b", "put");
    put(&notes, "c.md", "changed", "put");
    notes
        .delete(
            DocumentDeletion::default()
                .with_id("a.md")
                .with_note("gone"),
        )
        .unwrap();
    let rest: Vec<Document> = stream.map(Result::unwrap).collect();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].id.as_deref(), Some("c.md"));
    assert_eq!(text(&rest[0]), "c");
    assert_eq!(ids_of(&notes, listing(false, None, None)), ["b.md", "c.md"]);
}

#[test]
fn after_close_the_next_item_of_a_held_stream_is_failed_precondition() {
    let (_scratch, _path, vault) = fresh("reads-close");
    let notes = collection(&vault, "notes");
    for id in ["a.md", "b.md", "c.md"] {
        put(&notes, id, id, "put");
    }
    let mut documents = notes.get_many(listing(true, None, None)).unwrap();
    let mut versions = notes.history(key("a.md")).unwrap();
    assert!(documents.next().unwrap().is_ok());
    vault.close(Empty::default()).unwrap();
    let error = documents.next().unwrap().unwrap_err();
    assert!(error.is(Code::FailedPrecondition));
    assert!(documents.next().is_none());
    let error: Error = versions.next().unwrap().unwrap_err();
    assert!(error.is(Code::FailedPrecondition));
}

#[test]
fn history_lists_versions_newest_first_with_revision_author_note_and_bytes() {
    let (_scratch, _path, vault) = fresh("reads-history");
    let notes = collection(&vault, "notes");
    let one = put(&notes, "a.md", "one", "first\nmore");
    let two = put(&notes, "a.md", "two", "second");
    put(&notes, "other.md", "x", "elsewhere");
    notes
        .delete(
            DocumentDeletion::default()
                .with_id("a.md")
                .with_note("removed"),
        )
        .unwrap();
    let three = put(&notes, "a.md", "three", "again");
    let versions = history(&notes, "a.md");
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
}

#[test]
fn history_of_a_document_never_stored_is_empty_and_a_rewrite_of_equal_bytes_is_no_version() {
    let (_scratch, _path, vault) = fresh("reads-history-empty");
    let notes = collection(&vault, "notes");
    put(&notes, "a.md", "same", "first");
    put(&notes, "a.md", "same", "again");
    assert!(history(&notes, "never.md").is_empty());
    assert_eq!(history(&notes, "a.md").len(), 1);
    let config = collection(&vault, "config");
    assert!(history(&config, "settings").is_empty());
    assert!(history(&config, "other").is_empty());
}

#[test]
fn history_of_a_journal_is_unimplemented() {
    let (_scratch, _path, vault) = fresh("reads-history-journal");
    let log = collection(&vault, "log");
    append(&log, "a1", "line");
    assert!(
        log.history(key("a1"))
            .err()
            .unwrap()
            .is(Code::Unimplemented)
    );
}

#[test]
fn history_of_an_isolated_collection_comes_from_the_nested_repository() {
    let (_scratch, _path, vault) = fresh("reads-history-isolated");
    let diary = collection(&vault, "diary");
    put(&diary, "day.md", "one", "morning");
    put(&diary, "day.md", "two", "evening");
    let notes = collection(&vault, "notes");
    put(&notes, "day.md", "main", "main one");
    let versions = history(&diary, "day.md");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].note.as_deref(), Some("evening"));
    assert_eq!(versions[0].raw.as_deref(), Some(&b"two"[..]));
    assert_eq!(versions[1].raw.as_deref(), Some(&b"one"[..]));
    assert_eq!(history(&notes, "day.md").len(), 1);
}

#[test]
fn a_commit_made_by_hand_is_a_version_with_no_note() {
    let (_scratch, path, vault) = fresh("reads-history-hand");
    let notes = collection(&vault, "notes");
    put(&notes, "a.md", "one", "first");
    write(&path, "notes/a.md", "by hand");
    common::git(&path, &["add", "notes/a.md"]);
    common::git(&path, &["commit", "--quiet", "--message", "edit"]);
    let versions = history(&notes, "a.md");
    assert_eq!(versions.len(), 2);
    assert!(versions[0].note.is_none());
    assert_eq!(versions[0].raw.as_deref(), Some(&b"by hand"[..]));
    assert_eq!(
        versions[0].author.as_option().unwrap().name.as_deref(),
        Some("Test")
    );
    assert_eq!(versions[1].note.as_deref(), Some("first"));
}

#[test]
fn check_lists_every_problem_of_a_vault_with_several() {
    let scratch = Scratch::new("reads-check");
    let path = scratch.join("vault");
    built_vault(&path);
    write(&path, "log/a.jsonl", "a1: fine\nbroken line\n");
    common::git(&path, &["add", "--all"]);
    common::git(&path, &["commit", "--quiet", "--message", "a broken line"]);
    let vault = open(&path, OpenOptions::default()).unwrap();
    // A person's edit that is not committed.
    write(&path, "notes/a.md", "edited, not committed");
    let report = vault.check(Empty::default()).unwrap();
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
}

#[test]
fn check_of_a_sound_vault_is_the_empty_report() {
    let (_scratch, _path, vault) = fresh("reads-check-sound");
    let notes = collection(&vault, "notes");
    put(&notes, "a.md", "one", "first");
    assert!(vault.check(Empty::default()).unwrap().problems.is_empty());
}

#[test]
fn check_fails_only_when_it_cannot_run() {
    let scratch = Scratch::new("reads-check-cannot");
    let path = scratch.join("vault");
    let vault = open(&path, OpenOptions::default()).unwrap();
    declare(
        &vault,
        json!({"name": "sealed", "schema": {"standard": {}, "encrypted": true}}),
    );
    // The collection is encrypted and the handle has no secret: its files
    // cannot be opened to be checked.
    let error = vault.check(Empty::default()).err().unwrap();
    let expected = if cfg!(feature = "encrypt") {
        Code::Crypto
    } else {
        Code::Unimplemented
    };
    assert!(error.is(expected));
}

#[cfg(feature = "encrypt")]
mod encrypted {
    use super::*;

    const IDENTITY: &str =
        "AGE-SECRET-KEY-1RN2D82PZQK97437LWJVQJK4Q6EQVHLQ9A8Z0QTAXZ424HV8XZZPS5Q9860";

    #[test]
    fn include_raw_decrypts_and_history_gives_plain_bytes() {
        let scratch = Scratch::new("reads-encrypted");
        let path = scratch.join("vault");
        let options: OpenOptions = serde_json::from_value(json!({
            "encryption": {"identity": IDENTITY},
            "writeTimeoutMs": 60000
        }))
        .unwrap();
        let vault = open(&path, options).unwrap();
        declare(
            &vault,
            json!({"name": "secrets", "schema": {"standard": {}, "encrypted": true}}),
        );
        let secrets = collection(&vault, "secrets");
        let one = put(&secrets, "k.md", "first secret", "one");
        put(&secrets, "k.md", "second secret", "two");
        let got: Vec<Document> = secrets
            .get_many(listing(true, None, None))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(text(&got[0]), "second secret");
        let keys: Vec<Document> = secrets
            .get_many(listing(false, None, None))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(keys[0].raw.is_none());
        assert_eq!(keys[0].revision, got[0].revision);
        let versions = history(&secrets, "k.md");
        assert_eq!(versions[0].raw.as_deref(), Some(&b"second secret"[..]));
        assert_eq!(versions[1].raw.as_deref(), Some(&b"first secret"[..]));
        assert_eq!(versions[1].revision, one.revision);
        vault.close(Empty::default()).unwrap();

        // Without the secret, the bytes are the one thing it cannot give.
        let bare = open(&path, OpenOptions::default()).unwrap();
        let secrets = collection(&bare, "secrets");
        assert!(
            secrets
                .get_many(listing(true, None, None))
                .err()
                .unwrap()
                .is(Code::Crypto)
        );
        assert_eq!(ids_of(&secrets, listing(false, None, None)), ["k.md"]);
    }
}
