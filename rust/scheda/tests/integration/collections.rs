//! The collection calls of the handle: list, get, create, update, delete,
//! and the batch of them, against a Vault on disk.

use std::path::{Path, PathBuf};

use buffa_types::google::protobuf::Empty;
use protocontract::IterStream;
use scheda::v1::collection_result::Result as ResultArm;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, CollectionDeletion, CollectionKey, CollectionOperation,
    DynVaultAsync, OpenOptions, VaultAsync, VaultErrorCode as Code,
};
use scheda::{Error, Fault};
use serde_json::{Value, json};

use crate::common::{Scratch, both, git, open, snapshot, vault, work_tree, write};
use crate::probe::{assert_settled, commits, git_out};

fn collection(value: Value) -> CollectionDeclaration {
    serde_json::from_value(value).unwrap()
}

fn standard(name: &str) -> CollectionDeclaration {
    collection(json!({"name": name, "schema": {"standard": {}}}))
}

fn journal(name: &str) -> CollectionDeclaration {
    collection(json!({"name": name, "schema": {"journal": {}}}))
}

fn isolated(name: &str) -> CollectionDeclaration {
    collection(json!({"name": name, "schema": {"standard": {}, "isolated": true}}))
}

/// The declaration the Vault holds for the collection `name`.
async fn declared(vault: &DynVaultAsync, name: &str) -> CollectionDeclaration {
    vault
        .get_collection(key(name))
        .await
        .unwrap()
        .to_async_inline()
        .unwrap()
        .describe(Empty::default())
        .await
        .unwrap()
}

fn key(name: &str) -> CollectionKey {
    CollectionKey::default().with_name(name)
}

fn deletion(name: &str) -> CollectionDeletion {
    CollectionDeletion::default().with_name(name)
}

fn operation(value: Value) -> CollectionOperation {
    serde_json::from_value(value).unwrap()
}

/// A new Vault at `name` below a scratch directory.
async fn fresh(name: &str) -> (Scratch, PathBuf, DynVaultAsync) {
    let scratch = Scratch::new(name);
    let path = scratch.join("vault");
    let vault = open(&path, OpenOptions::default()).await.unwrap();
    (scratch, path, vault)
}

/// A Document file put into the collection by plain git, as a person does.
fn put_document(path: &Path, rel: &str) {
    write(path, rel, "a Document");
    git(path, &["add", "--all"]);
    git(path, &["commit", "--quiet", "--message", "a Document"]);
}

/// A Document file put into the nested repository of the isolated
/// collections by plain git, as a person does. `rel` is below `.isolated/`.
fn put_isolated_document(path: &Path, rel: &str) {
    let nested = path.join(".isolated");
    std::fs::create_dir_all(&nested).unwrap();
    if !nested.join(".git").exists() {
        git(&nested, &["init", "--quiet"]);
    }
    write(&nested, rel, "a Document");
    git(&nested, &["add", "--all"]);
    git(&nested, &["commit", "--quiet", "--message", "a Document"]);
}

fn subject(path: &Path) -> String {
    git_out(path, &["log", "-1", "--format=%s"])
}

fn read(path: &Path, rel: &str) -> String {
    std::fs::read_to_string(path.join(rel)).unwrap()
}

#[test]
fn create_returns_the_collection_with_defaults_and_writes_none() {
    both(|| async {
        let (_scratch, path, vault) = fresh("create").await;
        let before = commits(&path);
        let made = vault.create_collection(journal("log")).await.unwrap();
        assert_eq!(
            serde_json::to_value(&made).unwrap(),
            json!({"name": "log", "schema": {"journal": {"segmentMaxBytes": "1048576"}}})
        );
        let file = read(&path, "log/.collection.yaml");
        assert!(file.contains("journal"), "{file}");
        assert!(!file.contains("segment_max_bytes"), "{file}");
        assert!(!file.contains("segmentMaxBytes"), "{file}");
        assert_eq!(commits(&path), before + 1);
        assert_eq!(subject(&path), "create collection log (journal)");
        assert_settled(&path, &[]);
        assert_eq!(declared(&vault, "log").await, made);
        // Another handle opens the Vault and reads the same declaration.
        let other = open(&path, OpenOptions::default()).await.unwrap();
        assert_eq!(declared(&other, "log").await, made);
    });
}

#[test]
fn collections_are_listed_in_name_order_with_isolated_ones() {
    both(|| async {
        let (_scratch, path, vault) = fresh("list").await;
        for name in ["b", "c", "a"] {
            vault.create_collection(standard(name)).await.unwrap();
        }
        vault.create_collection(isolated("iso")).await.unwrap();
        let listed = vault.list_collections(Empty::default()).await.unwrap();
        let names: Vec<_> = listed
            .collections
            .iter()
            .map(|c| c.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["a", "b", "c", "iso"]);
        assert_eq!(subject(&path), "create collection iso (standard, isolated)");
        assert!(
            path.join(".isolated/iso").is_dir(),
            "the place of an isolated collection"
        );
        assert_settled(&path, &[]);
        let error = vault.get_collection(key("missing")).await.unwrap_err();
        assert!(error.is(Code::NotFound));
        let error = vault
            .get_collection(CollectionKey::default())
            .await
            .unwrap_err();
        assert!(error.is(Code::InvalidArgument));
    });
}

#[test]
fn create_refuses_a_declared_name_and_a_request_that_breaks_a_rule() {
    both(|| async {
        let (_scratch, path, vault) = fresh("create-refused").await;
        vault.create_collection(standard("notes")).await.unwrap();
        let before = commits(&path);
        let error = vault.create_collection(journal("notes")).await.unwrap_err();
        assert!(error.is(Code::AlreadyExists));
        let encrypted_journal = collection(json!({
            "name": "secret", "schema": {"journal": {}, "encrypted": true}
        }));
        let no_kind = collection(json!({"name": "nokind", "schema": {}}));
        let no_schema = collection(json!({"name": "noschema"}));
        let singleton_slug = collection(json!({
            "name": "one", "schema": {"singleton": {"id": "a/b"}}
        }));
        let bad_journal = collection(json!({
            "name": "log", "schema": {"journal": {"segmentMaxBytes": "0"}}
        }));
        let mut cases = vec![
            encrypted_journal,
            no_kind,
            no_schema,
            singleton_slug,
            bad_journal,
            CollectionDeclaration::default(),
        ];
        for name in [".hidden", "a/b", "", " x", "x.", "CON", "../x"] {
            cases.push(standard(name));
        }
        for bad in cases {
            let error = vault.create_collection(bad.clone()).await.unwrap_err();
            assert!(error.is(Code::InvalidArgument), "{bad:?}");
        }
        assert_eq!(commits(&path), before);
        assert_settled(&path, &[]);
        assert_eq!(
            vault
                .list_collections(Empty::default())
                .await
                .unwrap()
                .collections
                .len(),
            1
        );
    });
}

#[test]
fn update_is_not_found_for_an_undeclared_name() {
    both(|| async {
        let (_scratch, path, vault) = fresh("update-missing").await;
        let before = commits(&path);
        let error = vault.update_collection(standard("nope")).await.unwrap_err();
        assert!(error.is(Code::NotFound));
        assert_eq!(commits(&path), before);
    });
}

#[test]
fn update_keeps_the_unknown_keys_of_the_file_and_writes_no_default() {
    both(|| async {
        let scratch = Scratch::new("update-unknown");
        let path = scratch.join("vault");
        work_tree(
            &path,
            &[
                (".scheda.yaml", "version: 4\n"),
                (".gitignore", ".scheda/\n.isolated/\n"),
                (
                    "cache/.collection.yaml",
                    "name: cache\nfuture_top: 7\nschema:\n  journal:\n    future_inner: [1, 2]\n",
                ),
                (
                    "notes/.collection.json",
                    "{\"name\": \"notes\", \"schema\": {\"standard\": {}}, \"later\": true}\n",
                ),
            ],
        );
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        // The declaration in memory has the default; the file has not.
        let stored = declared(&vault, "cache").await;
        assert_eq!(
            serde_json::to_value(&stored).unwrap()["schema"]["journal"]["segmentMaxBytes"],
            "1048576"
        );
        let update = collection(json!({
            "name": "cache", "schema": {"journal": {"segmentMaxLines": "100"}, "indexScope": "INDEX_SCOPE_IDS"}
        }));
        let reply = vault.update_collection(update).await.unwrap();
        let reply = serde_json::to_value(&reply).unwrap();
        assert_eq!(reply["schema"]["journal"]["segmentMaxBytes"], "1048576");
        assert_eq!(reply["schema"]["journal"]["segmentMaxLines"], "100");
        let file = read(&path, "cache/.collection.yaml");
        assert!(file.contains("future_top: 7"), "{file}");
        assert!(file.contains("future_inner"), "{file}");
        assert!(file.contains("100"), "{file}");
        assert!(!file.contains("segment_max_bytes"), "{file}");
        assert!(!file.contains("segmentMaxBytes"), "{file}");
        assert_eq!(subject(&path), "update collection cache (journal)");
        // A JSON file stays a JSON file, with its unknown key.
        let reply = vault
            .update_collection(collection(
                json!({"name": "notes", "schema": {"standard": {}, "indexScope": "INDEX_SCOPE_NO_VECTORS"}}),
            ))
            .await
            .unwrap();
        assert_eq!(reply.name.as_deref(), Some("notes"));
        let file = read(&path, "notes/.collection.json");
        assert!(file.contains("\"later\": true"), "{file}");
        assert!(file.contains("indexScope"), "{file}");
        assert!(!path.join("notes/.collection.yaml").exists());
        assert_settled(&path, &[]);
        // A unknown key of a field that goes goes with it.
        vault.update_collection(standard("cache")).await.unwrap();
        let file = read(&path, "cache/.collection.yaml");
        assert!(file.contains("future_top: 7"), "{file}");
        assert!(!file.contains("future_inner"), "{file}");
    });
}

#[test]
fn an_undefined_index_scope_is_invalid_argument_and_stores_nothing() {
    both(|| async {
        let (_scratch, path, vault) = fresh("undefined-scope").await;
        vault.create_collection(standard("notes")).await.unwrap();
        let before = commits(&path);
        for scope in [json!(9), json!(-1)] {
            let declaration =
                json!({"name": "other", "schema": {"standard": {}, "indexScope": scope}});
            let error = vault
                .create_collection(collection(declaration.clone()))
                .await
                .unwrap_err();
            assert!(error.is(Code::InvalidArgument), "{declaration}");
            let declaration =
                json!({"name": "notes", "schema": {"standard": {}, "indexScope": scope}});
            let error = vault
                .update_collection(collection(declaration.clone()))
                .await
                .unwrap_err();
            assert!(error.is(Code::InvalidArgument), "{declaration}");
        }
        assert_eq!(commits(&path), before, "nothing was stored");
    });
}

#[test]
fn a_retired_index_key_in_a_collection_file_is_read_past_kept_and_not_acted_on() {
    both(|| async {
        let scratch = Scratch::new("retired-key");
        let path = scratch.join("vault");
        work_tree(
            &path,
            &[
                (".scheda.yaml", "version: 4\n"),
                (".gitignore", ".scheda/\n.isolated/\n"),
                (
                    "notes/.collection.yaml",
                    "name: notes\nschema:\n  standard: {}\n  excludeVectorIndex: true\n",
                ),
            ],
        );
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        let stored = serde_json::to_value(declared(&vault, "notes").await).unwrap();
        assert!(stored["schema"].get("indexScope").is_none(), "{stored}");
        assert!(
            stored["schema"].get("excludeVectorIndex").is_none(),
            "{stored}"
        );
        vault
            .update_collection(collection(
                json!({"name": "notes", "schema": {"standard": {}, "indexScope": "INDEX_SCOPE_IDS"}}),
            ))
            .await
            .unwrap();
        let file = read(&path, "notes/.collection.yaml");
        assert!(file.contains("excludeVectorIndex: true"), "{file}");
        assert!(file.contains("indexScope: INDEX_SCOPE_IDS"), "{file}");
    });
}

#[test]
fn the_kind_isolated_and_encrypted_change_only_while_the_collection_is_empty() {
    both(|| async {
        let (_scratch, path, vault) = fresh("update-empty").await;
        vault.create_collection(standard("notes")).await.unwrap();
        vault.create_collection(isolated("iso")).await.unwrap();
        put_document(&path, "notes/a.md");
        put_isolated_document(&path, "iso/a.md");
        let before = commits(&path);
        for changed in [
            json!({"name": "notes", "schema": {"journal": {}}}),
            json!({"name": "notes", "schema": {"standard": {}, "isolated": true}}),
            json!({"name": "notes", "schema": {"standard": {}, "encrypted": true}}),
            json!({"name": "iso", "schema": {"standard": {}}}),
        ] {
            let error = vault
                .update_collection(collection(changed.clone()))
                .await
                .unwrap_err();
            assert!(error.is(Code::FailedPrecondition), "{changed}");
        }
        assert_eq!(commits(&path), before, "nothing was stored");
        // Other fields change while it holds Documents.
        vault
            .update_collection(collection(
                json!({"name": "notes", "schema": {"standard": {}, "indexScope": "INDEX_SCOPE_IDS"}}),
            ))
            .await
            .unwrap();
        assert_eq!(commits(&path), before + 1);
        // An empty collection changes its kind and its place.
        vault.create_collection(standard("empty")).await.unwrap();
        vault.update_collection(journal("empty")).await.unwrap();
        vault.update_collection(isolated("empty")).await.unwrap();
        assert!(path.join(".isolated/empty").is_dir());
        vault.update_collection(standard("empty")).await.unwrap();
        assert!(!path.join(".isolated/empty").exists());
        assert_settled(&path, &[]);
    });
}

#[test]
fn delete_refuses_a_collection_with_documents_and_changes_nothing() {
    both(|| async {
        let (_scratch, path, vault) = fresh("delete-full").await;
        vault.create_collection(standard("notes")).await.unwrap();
        vault.create_collection(journal("log")).await.unwrap();
        vault.create_collection(isolated("iso")).await.unwrap();
        put_document(&path, "notes/a.md");
        put_document(&path, "log/00000001-w.jsonl");
        put_isolated_document(&path, "iso/a.md");
        let before = commits(&path);
        for name in ["notes", "log", "iso"] {
            let error = vault.delete_collection(deletion(name)).await.unwrap_err();
            assert!(error.is(Code::FailedPrecondition), "{name}");
            assert!(path.join(name).is_dir(), "{name}");
        }
        assert!(path.join("notes/.collection.yaml").is_file());
        assert!(path.join(".isolated/iso/a.md").is_file());
        assert_eq!(commits(&path), before);
        assert_eq!(
            vault
                .list_collections(Empty::default())
                .await
                .unwrap()
                .collections
                .len(),
            3
        );
    });
}

#[test]
fn delete_removes_an_empty_collection_and_the_name_can_be_declared_again() {
    both(|| async {
        let (_scratch, path, vault) = fresh("delete-empty").await;
        for declared in [standard("notes"), journal("log"), isolated("iso")] {
            let name = declared.name.clone().unwrap();
            vault.create_collection(declared.clone()).await.unwrap();
            let before = commits(&path);
            vault.delete_collection(deletion(&name)).await.unwrap();
            assert_eq!(commits(&path), before + 1);
            assert_eq!(subject(&path), format!("delete collection {name}"));
            assert!(!path.join(&name).exists(), "{name}");
            assert!(!path.join(".isolated").join(&name).exists(), "{name}");
            assert_settled(&path, &[]);
            let error = vault.get_collection(key(&name)).await.unwrap_err();
            assert!(error.is(Code::NotFound));
            vault.create_collection(declared).await.unwrap();
            assert!(path.join(&name).join(".collection.yaml").is_file());
            assert_settled(&path, &[]);
        }
    });
}

#[test]
fn delete_of_an_unknown_name_is_not_found_or_nothing_with_ignore_missing() {
    both(|| async {
        let (_scratch, path, vault) = fresh("delete-missing").await;
        let before = commits(&path);
        let error = vault.delete_collection(deletion("nope")).await.unwrap_err();
        assert!(error.is(Code::NotFound));
        vault
            .delete_collection(deletion("nope").with_ignore_missing(true))
            .await
            .unwrap();
        let error = vault
            .delete_collection(CollectionDeletion::default())
            .await
            .unwrap_err();
        assert!(error.is(Code::InvalidArgument));
        assert_eq!(commits(&path), before);
    });
}

#[test]
fn a_batch_with_a_failing_third_operation_declares_changes_and_deletes_nothing() {
    both(|| async {
        let (_scratch, path, vault) = fresh("batch-fails").await;
        vault.create_collection(standard("keep")).await.unwrap();
        let before = commits(&path);
        let cases = [
            (
                json!({"update": {"name": "nope", "schema": {"standard": {}}}}),
                Code::NotFound,
            ),
            (
                json!({"create": {"name": "a", "schema": {"standard": {}}}}),
                Code::AlreadyExists,
            ),
            (json!({"delete": {"name": "nope"}}), Code::NotFound),
            (
                json!({"create": {"name": "bad/name", "schema": {"standard": {}}}}),
                Code::InvalidArgument,
            ),
        ];
        for (third, code) in cases {
            let items = vec![
                Ok(operation(
                    json!({"create": {"name": "a", "schema": {"standard": {}}}}),
                )),
                Ok(operation(json!({"delete": {"name": "keep"}}))),
                Ok(operation(third.clone())),
            ];
            let error = vault
                .batch_collections(IterStream::new(items.into_iter()))
                .await
                .unwrap_err();
            assert!(error.is(code), "{third}");
            assert!(
                error.stack().iter().any(|frame| frame == "operation 2"),
                "the error names the operation: {:?}",
                error.stack()
            );
            assert_eq!(commits(&path), before);
            assert!(!path.join("a").exists());
            assert!(path.join("keep/.collection.yaml").is_file());
            assert_settled(&path, &[]);
        }
        assert_eq!(
            vault
                .list_collections(Empty::default())
                .await
                .unwrap()
                .collections
                .len(),
            1
        );
    });
}

#[test]
fn an_err_item_and_an_item_without_a_write_store_nothing_and_name_their_index() {
    both(|| async {
        let (_scratch, path, vault) = fresh("batch-err").await;
        let before = commits(&path);
        let first = || {
            Ok(operation(
                json!({"create": {"name": "a", "schema": {"standard": {}}}}),
            ))
        };
        let items: Vec<Result<CollectionOperation, Error>> = vec![
            first(),
            Err(Fault::new(Code::Unavailable, "the stream broke").into()),
            first(),
        ];
        let error = vault
            .batch_collections(IterStream::new(items.into_iter()))
            .await
            .unwrap_err();
        assert!(error.is(Code::Unavailable));
        assert!(error.stack().iter().any(|frame| frame == "operation 1"));
        let items: Vec<Result<CollectionOperation, Error>> =
            vec![first(), Ok(CollectionOperation::default())];
        let error = vault
            .batch_collections(IterStream::new(items.into_iter()))
            .await
            .unwrap_err();
        assert!(error.is(Code::InvalidArgument));
        assert!(error.stack().iter().any(|frame| frame == "operation 1"));
        assert_eq!(commits(&path), before);
        assert!(!path.join("a").exists());
    });
}

#[test]
fn a_batch_that_succeeds_is_one_commit_with_one_result_per_operation() {
    both(|| async {
        let (_scratch, path, vault) = fresh("batch-ok").await;
        vault.create_collection(standard("old")).await.unwrap();
        vault.create_collection(standard("gone")).await.unwrap();
        let before = commits(&path);
        let items: Vec<Result<CollectionOperation, Error>> = vec![
            Ok(operation(
                json!({"create": {"name": "a", "schema": {"standard": {}}}}),
            )),
            Ok(operation(
                json!({"create": {"name": "b", "schema": {"journal": {}}}}),
            )),
            // Later operations see the earlier ones.
            Ok(operation(
                json!({"update": {"name": "a", "schema": {"standard": {}, "indexScope": "INDEX_SCOPE_IDS"}}}),
            )),
            Ok(operation(json!({"delete": {"name": "gone"}}))),
            Ok(operation(
                json!({"delete": {"name": "nope", "ignoreMissing": true}}),
            )),
            Ok(operation(
                json!({"update": {"name": "old", "schema": {"journal": {}}}}),
            )),
        ];
        let reply = vault
            .batch_collections(IterStream::new(items.into_iter()))
            .await
            .unwrap();
        assert_eq!(commits(&path), before + 1, "one write is one commit");
        assert_settled(&path, &[]);
        let kinds: Vec<&str> = reply
            .results
            .iter()
            .map(|result| match result.result.as_ref().unwrap() {
                ResultArm::Collection(_) => "collection",
                ResultArm::Deleted(_) => "deleted",
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "collection",
                "collection",
                "collection",
                "deleted",
                "deleted",
                "collection"
            ]
        );
        let message = git_out(&path, &["log", "-1", "--format=%B"]);
        assert!(message.starts_with("change 5 collections\n"), "{message}");
        for line in [
            "- create collection a (standard)",
            "- create collection b (journal)",
            "- update collection a (standard)",
            "- delete collection gone",
            "- update collection old (journal)",
        ] {
            assert!(message.contains(line), "{message}");
        }
        assert!(path.join("a/.collection.yaml").is_file());
        assert!(path.join("b/.collection.yaml").is_file());
        assert!(!path.join("gone").exists());
        let names: Vec<_> = vault
            .list_collections(Empty::default())
            .await
            .unwrap()
            .collections
            .iter()
            .map(|c| c.name.clone().unwrap())
            .collect();
        assert_eq!(names, ["a", "b", "old"]);
    });
}

#[test]
fn a_batch_may_create_and_delete_a_name_or_delete_and_create_it_again() {
    both(|| async {
        let (_scratch, path, vault) = fresh("batch-same").await;
        vault.create_collection(standard("notes")).await.unwrap();
        let before = commits(&path);
        // Created and deleted in one batch: the tree does not change.
        let items: Vec<Result<CollectionOperation, Error>> = vec![
            Ok(operation(
                json!({"create": {"name": "temp", "schema": {"standard": {}}}}),
            )),
            Ok(operation(json!({"delete": {"name": "temp"}}))),
        ];
        vault
            .batch_collections(IterStream::new(items.into_iter()))
            .await
            .unwrap();
        assert_eq!(
            commits(&path),
            before,
            "a write that changes nothing stores nothing"
        );
        assert!(!path.join("temp").exists());
        assert_settled(&path, &[]);
        // Deleted and declared again with another kind.
        let items: Vec<Result<CollectionOperation, Error>> = vec![
            Ok(operation(json!({"delete": {"name": "notes"}}))),
            Ok(operation(
                json!({"create": {"name": "notes", "schema": {"journal": {}}}}),
            )),
        ];
        vault
            .batch_collections(IterStream::new(items.into_iter()))
            .await
            .unwrap();
        assert_eq!(commits(&path), before + 1);
        let notes = declared(&vault, "notes").await;
        assert!(notes.schema.as_option().unwrap().kind.is_some());
        assert!(read(&path, "notes/.collection.yaml").contains("journal"));
        assert_settled(&path, &[]);
        // An empty batch is nothing.
        let none: Vec<Result<CollectionOperation, Error>> = Vec::new();
        let reply = vault
            .batch_collections(IterStream::new(none.into_iter()))
            .await
            .unwrap();
        assert!(reply.results.is_empty());
        assert_eq!(commits(&path), before + 1);
    });
}

#[test]
fn a_collection_declared_by_another_handle_or_by_hand_is_seen() {
    both(|| async {
        let scratch = Scratch::new("fresh");
        let path = scratch.join("vault");
        vault(&path);
        let first = open(&path, OpenOptions::default()).await.unwrap();
        let second = open(&path, OpenOptions::default()).await.unwrap();
        assert_eq!(
            first
                .list_collections(Empty::default())
                .await
                .unwrap()
                .collections
                .len(),
            2
        );
        first.create_collection(standard("one")).await.unwrap();
        assert!(second.get_collection(key("one")).await.is_ok());
        // A person declares one and commits it with plain git.
        write(
            &path,
            "two/.collection.yaml",
            "name: two\nschema:\n  standard: {}\n",
        );
        git(&path, &["add", "two"]);
        git(&path, &["commit", "--quiet", "--message", "by hand"]);
        assert!(first.get_collection(key("two")).await.is_ok());
        let error = second.create_collection(standard("two")).await.unwrap_err();
        assert!(error.is(Code::AlreadyExists));
        // The Vault built by hand has no `.gitignore`: `.scheda/` is untracked.
        assert_settled(&path, &[".scheda/"]);
        assert!(
            snapshot(&path.join("one"))
                .iter()
                .any(|(rel, _)| rel.ends_with(".collection.yaml"))
        );
    });
}
