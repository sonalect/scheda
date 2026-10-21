//! Reads of a tree whose Documents sit under layout folders (`_<hex>`) that
//! a person, a mirror, or a later write made: every Document is found
//! whether its file is flat or split, and a listing yields every id.

use std::collections::BTreeSet;
use std::path::Path;

use buffa_types::google::protobuf::Empty;
use git2::{IndexAddOption, Repository, Signature};
use protocontract::IterStream;
use scheda::v1::document_operation::Operation;
use scheda::v1::document_selection::Selection;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, DocumentCreation, DocumentDeletion, DocumentId,
    DocumentOperation, DocumentSelection, DocumentUpsert, DynCollectionAsync, DynVaultAsync,
    OpenOptions, VaultAsync, VaultDocumentOperation, VaultErrorCode as Code, WorkTree,
};
use serde_json::json;

use crate::common::{Scratch, all, both, collection, open, work_tree, write};

/// A Vault of generation 3 at `path` with the collection `notes` and
/// nothing else, committed.
fn empty_vault(path: &Path) {
    work_tree(
        path,
        &[
            (".scheda.yaml", "version: 3\n"),
            (
                "notes/.collection.yaml",
                "name: notes\nschema:\n  standard: {}\n",
            ),
        ],
    );
}

/// Commits what the work tree holds now with git2, as a person's tool
/// would.
fn commit_by_hand(path: &Path) {
    let repo = Repository::open(path).unwrap();
    let mut index = repo.index().unwrap();
    index.add_all(["*"], IndexAddOption::DEFAULT, None).unwrap();
    index.update_all(["*"], None).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = Signature::now("Test", "test@example.test").unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "split by hand", &tree, &[&parent])
        .unwrap();
}

fn key(id: &str) -> DocumentId {
    DocumentId::default().with_id(id)
}

/// The collection `notes` of the Vault at `path`.
async fn notes_of(path: &Path) -> DynCollectionAsync {
    let vault = open(path, OpenOptions::default()).await.unwrap();
    collection(&vault, "notes").await.unwrap()
}

/// Seventy ids, `a0.md` to `g9.md`, and the bytes of each.
fn ids() -> Vec<(String, String)> {
    ('a'..='g')
        .flat_map(|letter| {
            (0..10).map(move |digit| {
                let id = format!("{letter}{digit}.md");
                let text = format!("body of {id}");
                (id, text)
            })
        })
        .collect()
}

/// The ids of every Document `notes` lists, in order, with their bytes.
async fn listed(notes: &DynCollectionAsync) -> Vec<(String, Vec<u8>)> {
    let stream = notes
        .get_many(DocumentSelection {
            include_raw: Some(true),
            selection: Some(Selection::Listing(Box::default())),
            ..DocumentSelection::default()
        })
        .await
        .unwrap();
    all(stream)
        .await
        .unwrap()
        .into_iter()
        .map(|document| (document.id.unwrap(), document.raw.unwrap()))
        .collect()
}

#[test]
fn get_finds_every_document_of_a_collection_split_by_hand_and_a_missing_id_is_not_found() {
    both(|| async {
        let scratch = Scratch::new("layout-get");
        let path = scratch.join("vault");
        empty_vault(&path);
        // The folders the layout makes at 70 ids: one per first byte.
        for (id, text) in ids() {
            let label = format!("_{:02x}", id.as_bytes()[0]);
            write(&path, &format!("notes/{label}/{id}"), &text);
        }
        commit_by_hand(&path);
        let notes = notes_of(&path).await;
        for (id, text) in ids() {
            let got = notes.get(key(&id)).await.unwrap();
            assert_eq!(got.raw.as_deref(), Some(text.as_bytes()), "{id}");
            assert!(got.revision.is_some(), "{id}");
        }
        for missing in ["h0.md", "a10.md", "a", "z/a0.md"] {
            let error = notes.get(key(missing)).await.unwrap_err();
            assert!(error.is(Code::NotFound), "{missing}");
        }
    });
}

#[test]
fn an_id_with_slash_components_is_found_when_its_middle_folder_is_split() {
    both(|| async {
        let scratch = Scratch::new("layout-slash");
        let path = scratch.join("vault");
        empty_vault(&path);
        // `a/b/` is split by hand; `a/` and the collection are flat.
        write(&path, "notes/a/b/_63/c1.md", "deep one");
        write(&path, "notes/a/b/_63/c2.md", "deep two");
        write(&path, "notes/a/b/d.md", "flat beside the label");
        write(&path, "notes/a/top.md", "above");
        commit_by_hand(&path);
        let notes = notes_of(&path).await;
        for (id, text) in [
            ("a/b/c1.md", "deep one"),
            ("a/b/c2.md", "deep two"),
            ("a/b/d.md", "flat beside the label"),
            ("a/top.md", "above"),
        ] {
            let got = notes.get(key(id)).await.unwrap();
            assert_eq!(got.raw.as_deref(), Some(text.as_bytes()), "{id}");
        }
        for missing in ["a/b/c3.md", "a/c1.md", "b/c1.md"] {
            let error = notes.get(key(missing)).await.unwrap_err();
            assert!(error.is(Code::NotFound), "{missing}");
        }
        let all = listed(&notes).await;
        let got: Vec<&str> = all.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(got, ["a/b/c1.md", "a/b/c2.md", "a/b/d.md", "a/top.md"]);
    });
}

#[test]
fn a_tree_split_at_a_byte_the_layout_would_not_choose_still_reads() {
    both(|| async {
        let scratch = Scratch::new("layout-odd");
        let path = scratch.join("vault");
        empty_vault(&path);
        // Seventy ids split at their first two bytes, with a few files left
        // flat beside the folders.
        for (id, text) in ids() {
            if id.starts_with('a') {
                write(&path, &format!("notes/{id}"), &text);
            } else {
                let label = format!("_{:02x}{:02x}", id.as_bytes()[0], id.as_bytes()[1]);
                write(&path, &format!("notes/{label}/{id}"), &text);
            }
        }
        // A short label inside a long one: the longest label wins, and a
        // name the long one does not hold is looked for in the short one.
        write(&path, "notes/_67/_39/g9.md", "moved");
        let _ = std::fs::remove_file(path.join("notes/_6739/g9.md"));
        commit_by_hand(&path);
        let notes = notes_of(&path).await;
        for (id, text) in ids() {
            let text = if id == "g9.md" { "moved" } else { &text };
            let got = notes.get(key(&id)).await.unwrap();
            assert_eq!(got.raw.as_deref(), Some(text.as_bytes()), "{id}");
        }
        let error = notes.get(key("g99.md")).await.unwrap_err();
        assert!(error.is(Code::NotFound));
    });
}

#[test]
fn the_listing_of_a_collection_split_by_hand_yields_every_id_in_order() {
    both(|| async {
        let scratch = Scratch::new("layout-list");
        let path = scratch.join("vault");
        empty_vault(&path);
        for (id, text) in ids() {
            let label = format!("_{:02x}", id.as_bytes()[0]);
            write(&path, &format!("notes/{label}/{id}"), &text);
        }
        // A flat file equal to a label's prefix, a folder of an id beside the
        // labels, and a dotfile that is not a Document.
        write(&path, "notes/b", "flat");
        write(&path, "notes/z/inner.md", "an id folder");
        write(&path, "notes/.hidden", "not a Document");
        commit_by_hand(&path);
        let notes = notes_of(&path).await;
        let mut want: Vec<(String, Vec<u8>)> = ids()
            .into_iter()
            .map(|(id, text)| (id, text.into_bytes()))
            .collect();
        want.push(("b".to_owned(), b"flat".to_vec()));
        want.push(("z/inner.md".to_owned(), b"an id folder".to_vec()));
        want.sort();
        assert_eq!(listed(&notes).await, want);
    });
}

#[test]
fn history_follows_a_document_moved_into_a_layout_folder_and_counts_no_move_as_a_version() {
    both(|| async {
        let scratch = Scratch::new("layout-history");
        let path = scratch.join("vault");
        empty_vault(&path);
        write(&path, "notes/a0.md", "first");
        commit_by_hand(&path);
        // The same bytes under a layout folder: a move, not a version.
        std::fs::remove_file(path.join("notes/a0.md")).unwrap();
        write(&path, "notes/_61/a0.md", "first");
        commit_by_hand(&path);
        // New bytes at the new place.
        write(&path, "notes/_61/a0.md", "second");
        commit_by_hand(&path);
        let notes = notes_of(&path).await;
        let versions = all(notes.history(key("a0.md")).await.unwrap())
            .await
            .unwrap();
        let raws: Vec<&[u8]> = versions
            .iter()
            .map(|version| version.raw.as_deref().unwrap())
            .collect();
        assert_eq!(raws, [&b"second"[..], &b"first"[..]]);
    });
}

// ---------------------------------------------------------------------
// Writes lay a standard collection out canonically.

/// The physical path of each name under the rule of the layout, written
/// again here from the rule alone: a folder of more than 64 names is split
/// at the first byte where they differ, into folders named `_` and the
/// hex of the bytes they add to the prefix. Names have no `/`.
pub(crate) fn canonical(names: &BTreeSet<String>) -> BTreeSet<String> {
    fn lay(prefix: usize, names: &[&str], at: &str, out: &mut BTreeSet<String>) {
        if names.len() <= 64 {
            out.extend(names.iter().map(|name| format!("{at}{name}")));
            return;
        }
        let (first, last) = (names[0], names[names.len() - 1]);
        let common = first
            .bytes()
            .zip(last.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let mut rest = names;
        if first.len() == common {
            out.insert(format!("{at}{first}"));
            rest = &names[1..];
        }
        let mut start = 0;
        while start < rest.len() {
            let byte = rest[start].as_bytes()[common];
            let len = rest[start..]
                .iter()
                .take_while(|name| name.as_bytes()[common] == byte)
                .count();
            let group = &rest[start..start + len];
            let label: String = group[0].as_bytes()[prefix..=common]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            lay(common + 1, group, &format!("{at}_{label}/"), out);
            start += len;
        }
    }
    let sorted: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut out = BTreeSet::new();
    lay(0, &sorted, "", &mut out);
    out
}

/// The tree of the folder `dir` in the commit `HEAD` names in the
/// repository at `path`, and the paths of its files below it, without the
/// dotfiles at its root.
pub(crate) fn physical(path: &Path, dir: &str) -> (git2::Oid, BTreeSet<String>) {
    fn walk(repo: &Repository, tree: &git2::Tree<'_>, at: &str, out: &mut BTreeSet<String>) {
        for entry in tree.iter() {
            let name = entry.name().unwrap();
            if at.is_empty() && name.starts_with('.') {
                continue;
            }
            let here = format!("{at}{name}");
            if entry.kind() == Some(git2::ObjectType::Tree) {
                walk(
                    repo,
                    &repo.find_tree(entry.id()).unwrap(),
                    &format!("{here}/"),
                    out,
                );
            } else {
                out.insert(here);
            }
        }
    }
    let repo = Repository::open(path).unwrap();
    let root = repo
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .tree()
        .unwrap();
    let id = root.get_path(Path::new(dir)).unwrap().id();
    let mut out = BTreeSet::new();
    walk(&repo, &repo.find_tree(id).unwrap(), "", &mut out);
    (id, out)
}

/// The files below `dir` in the work tree at `path`, without the dotfiles
/// at its root.
fn on_disk(path: &Path, dir: &str) -> BTreeSet<String> {
    fn walk(base: &Path, at: &Path, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            if at == base && name.starts_with('.') {
                continue;
            }
            if entry.file_type().unwrap().is_dir() {
                walk(base, &entry.path(), out);
            } else {
                let rel = entry.path();
                let rel = rel.strip_prefix(base).unwrap();
                out.insert(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let base = path.join(dir);
    let mut out = BTreeSet::new();
    walk(&base, &base, &mut out);
    out
}

fn none() -> OpenOptions {
    OpenOptions::default().with_work_tree(WorkTree::WORK_TREE_NONE)
}

/// A new Vault at `path` with the standard collection `notes`.
async fn new_vault(path: &Path, options: OpenOptions) -> DynVaultAsync {
    let vault = open(path, options).await.unwrap();
    let declaration: CollectionDeclaration =
        serde_json::from_value(json!({"name": "notes", "schema": {"standard": {}}})).unwrap();
    vault.create_collection(declaration).await.unwrap();
    vault
}

fn body(id: &str) -> Vec<u8> {
    format!("body of {id}").into_bytes()
}

async fn put(notes: &DynCollectionAsync, id: &str) {
    notes
        .upsert(
            DocumentUpsert::default()
                .with_id(id)
                .with_raw(body(id))
                .with_note("put"),
        )
        .await
        .unwrap();
}

async fn remove(notes: &DynCollectionAsync, id: &str) {
    notes
        .delete(DocumentDeletion::default().with_id(id).with_note("remove"))
        .await
        .unwrap();
}

async fn get_all(notes: &DynCollectionAsync, ids: &BTreeSet<String>) {
    for id in ids {
        let got = notes.get(key(id)).await.unwrap();
        assert_eq!(got.raw.as_deref(), Some(body(id).as_slice()), "{id}");
    }
}

/// `n000.md`, `n001.md`, and so on: `count` ids that share their start.
fn numbered_ids(count: usize) -> BTreeSet<String> {
    (0..count).map(|n| format!("n{n:03}.md")).collect()
}

fn packs(path: &Path) -> usize {
    std::fs::read_dir(path.join(".git/objects/pack"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|extension| extension == "pack")
        })
        .count()
}

#[test]
fn a_folder_is_flat_at_64_documents_splits_at_65_and_is_flat_again_after_a_removal_with_the_files_where_the_tree_has_them()
 {
    both(|| async {
        let scratch = Scratch::new("layout-split-merge");
        let path = scratch.join("vault");
        let vault = new_vault(&path, OpenOptions::default()).await;
        let notes = collection(&vault, "notes").await.unwrap();
        let mut ids = numbered_ids(64);
        for id in &ids {
            put(&notes, id).await;
        }
        let (_, files) = physical(&path, "notes");
        assert_eq!(files, ids, "64 Documents are flat");
        assert_eq!(on_disk(&path, "notes"), files);
        get_all(&notes, &ids).await;

        put(&notes, "n064.md").await;
        ids.insert("n064.md".to_owned());
        let (_, files) = physical(&path, "notes");
        assert_eq!(files, canonical(&ids), "the 65th splits the folder");
        assert!(files.iter().all(|file| file.starts_with('_')), "{files:?}");
        assert_eq!(on_disk(&path, "notes"), files, "no stale file is left");
        get_all(&notes, &ids).await;
        crate::probe::assert_settled(&path, &[]);

        remove(&notes, "n010.md").await;
        ids.remove("n010.md");
        let (_, files) = physical(&path, "notes");
        assert_eq!(files, ids, "64 again, flat");
        assert_eq!(on_disk(&path, "notes"), files, "no stale file is left");
        get_all(&notes, &ids).await;
        let error = notes.get(key("n010.md")).await.unwrap_err();
        assert!(error.is(Code::NotFound));
        crate::probe::assert_settled(&path, &[]);
        vault.close(Empty::default()).await.unwrap();
    });
}

#[test]
fn the_same_100_ids_written_in_two_orders_in_two_vaults_give_equal_trees() {
    both(|| async {
        let scratch = Scratch::new("layout-orders");
        let ids: Vec<String> = numbered_ids(100).into_iter().collect();
        let mut trees = Vec::new();
        for (name, stride) in [("up", 1), ("shuffled", 37)] {
            let path = scratch.join(name);
            let vault = new_vault(&path, none()).await;
            let notes = collection(&vault, "notes").await.unwrap();
            for step in 0..ids.len() {
                put(&notes, &ids[step * stride % ids.len()]).await;
            }
            trees.push(physical(&path, "notes"));
            vault.close(Empty::default()).await.unwrap();
        }
        assert_eq!(trees[0].0, trees[1].0, "the same tree id");
        assert_eq!(trees[0].1, canonical(&numbered_ids(100)));
    });
}

#[test]
fn after_each_of_300_seeded_writes_and_deletes_the_collection_tree_is_the_canonical_one() {
    both(|| async {
        let scratch = Scratch::new("layout-seeded");
        let path = scratch.join("vault");
        let vault = new_vault(&path, none()).await;
        let notes = collection(&vault, "notes").await.unwrap();
        // Ids that share their start, in three families.
        let universe: Vec<String> = (0..60)
            .flat_map(|n| {
                ["note-", "notes-", "n0"]
                    .into_iter()
                    .map(move |family| format!("{family}{n:03}.md"))
            })
            .collect();
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut held = BTreeSet::new();
        for step in 0..300 {
            let id = universe[usize::try_from(next() % universe.len() as u64).unwrap()].clone();
            if held.contains(&id) {
                if held.len() > 66 || next() % 3 == 0 {
                    remove(&notes, &id).await;
                    held.remove(&id);
                } else {
                    continue;
                }
            } else {
                put(&notes, &id).await;
                held.insert(id);
            }
            let (_, files) = physical(&path, "notes");
            assert_eq!(files, canonical(&held), "step {step}, {} ids", held.len());
        }
        assert!(held.len() > 40, "{}", held.len());
        get_all(&notes, &held).await;
        vault.close(Empty::default()).await.unwrap();
    });
}

#[test]
fn a_batch_of_200_documents_gives_the_canonical_tree_and_one_pack() {
    both(|| async {
        let scratch = Scratch::new("layout-batch");
        let path = scratch.join("vault");
        let vault = new_vault(&path, none()).await;
        let notes = collection(&vault, "notes").await.unwrap();
        put(&notes, "warm.md").await;
        let ids = numbered_ids(200);
        let before = packs(&path);
        let operations: Vec<Result<VaultDocumentOperation, scheda::Error>> = ids
            .iter()
            .map(|id| {
                Ok(VaultDocumentOperation {
                    collection: Some("notes".to_owned()),
                    operation: DocumentOperation {
                        operation: Some(Operation::Create(Box::new(
                            DocumentCreation::default()
                                .with_id(id.as_str())
                                .with_raw(body(id))
                                .with_note("n"),
                        ))),
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                })
            })
            .collect();
        vault
            .batch_documents(IterStream::new(operations.into_iter()))
            .await
            .unwrap();
        assert_eq!(packs(&path), before + 1, "one pack");
        let mut all_ids = ids;
        all_ids.insert("warm.md".to_owned());
        let (_, files) = physical(&path, "notes");
        assert_eq!(files, canonical(&all_ids));
        get_all(&notes, &all_ids).await;
        vault.close(Empty::default()).await.unwrap();
    });
}

#[test]
fn an_id_with_components_among_70_siblings_splits_its_own_folder_and_leaves_the_entry_in_the_root()
{
    both(|| async {
        let scratch = Scratch::new("layout-components");
        let path = scratch.join("vault");
        let vault = new_vault(&path, none()).await;
        let notes = collection(&vault, "notes").await.unwrap();
        let mut ids: BTreeSet<String> = (0..70).map(|n| format!("a/b/c{n:03}.md")).collect();
        ids.insert("a/top.md".to_owned());
        ids.insert("z.md".to_owned());
        for id in &ids {
            put(&notes, id).await;
        }
        let repo = Repository::open(&path).unwrap();
        let head = repo
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap();
        let tree_at = |at: &str| {
            let id = head.get_path(Path::new(at)).unwrap().id();
            repo.find_tree(id).unwrap()
        };
        let names = |tree: &git2::Tree<'_>| -> Vec<String> {
            tree.iter()
                .map(|entry| entry.name().unwrap().to_owned())
                .filter(|name| !name.starts_with('.'))
                .collect()
        };
        assert_eq!(names(&tree_at("notes")), ["a", "z.md"]);
        assert_eq!(names(&tree_at("notes/a")), ["b", "top.md"]);
        let split = names(&tree_at("notes/a/b"));
        assert!(split.len() >= 2, "{split:?}");
        assert!(split.iter().all(|name| name.starts_with('_')), "{split:?}");
        let files: BTreeSet<String> = ids
            .iter()
            .filter_map(|id| id.strip_prefix("a/b/"))
            .map(str::to_owned)
            .collect();
        let (_, all) = physical(&path, "notes/a/b");
        assert_eq!(all, canonical(&files));
        get_all(&notes, &ids).await;
        // A removal inside the split folder keeps the entry above.
        remove(&notes, "a/b/c000.md").await;
        assert_eq!(names(&tree_at("notes")), ["a", "z.md"]);
        vault.close(Empty::default()).await.unwrap();
    });
}

#[test]
fn the_history_of_a_document_a_split_moved_has_no_new_version() {
    both(|| async {
        let scratch = Scratch::new("layout-moved-history");
        let path = scratch.join("vault");
        let vault = new_vault(&path, none()).await;
        let notes = collection(&vault, "notes").await.unwrap();
        for id in numbered_ids(64) {
            put(&notes, &id).await;
        }
        let versions = |notes: DynCollectionAsync| async move {
            all(notes.history(key("n000.md")).await.unwrap())
                .await
                .unwrap()
                .into_iter()
                .map(|version| (version.revision, version.write_id))
                .collect::<Vec<_>>()
        };
        let before = versions(notes.clone()).await;
        assert_eq!(before.len(), 1);
        put(&notes, "n064.md").await;
        let (_, files) = physical(&path, "notes");
        assert!(!files.contains("n000.md"), "the split moved it: {files:?}");
        assert_eq!(versions(notes.clone()).await, before);
        // A change of its bytes is a new version, at its new place.
        notes
            .upsert(
                DocumentUpsert::default()
                    .with_id("n000.md")
                    .with_raw(b"changed".to_vec())
                    .with_note("change"),
            )
            .await
            .unwrap();
        assert_eq!(versions(notes.clone()).await.len(), 2);
        vault.close(Empty::default()).await.unwrap();
    });
}

#[test]
fn one_write_makes_a_collection_split_by_hand_canonical() {
    both(|| async {
        let scratch = Scratch::new("layout-hand-split");
        let path = scratch.join("vault");
        let vault = new_vault(&path, OpenOptions::default()).await;
        vault.close(Empty::default()).await.unwrap();
        // 70 Documents in one folder `_61`: the layout splits them one level
        // further, so the folder as it is was not made by it.
        let mut ids: BTreeSet<String> = (0..70).map(|n| format!("a{n:03}.md")).collect();
        for id in &ids {
            write(
                &path,
                &format!("notes/_61/{id}"),
                &String::from_utf8(body(id)).unwrap(),
            );
        }
        commit_by_hand(&path);
        let notes = notes_of(&path).await;
        get_all(&notes, &ids).await;
        put(&notes, "b.md").await;
        ids.insert("b.md".to_owned());
        let (_, files) = physical(&path, "notes");
        assert_eq!(files, canonical(&ids));
        assert_eq!(on_disk(&path, "notes"), files, "the files follow the tree");
        get_all(&notes, &ids).await;
        crate::probe::assert_settled(&path, &[]);
    });
}
