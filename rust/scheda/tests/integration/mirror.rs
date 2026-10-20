//! The mirror through the public handle: `open` that clones, `Pull`, `Push`,
//! `Sync`, `SetMirror`, `RemoveMirror`, and the nested repository that no
//! call contacts. The mirrors are bare repositories on disk reached through
//! `file://` URLs; nothing here touches a network.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, CollectionKey, DocumentDeletion, DocumentId,
    DocumentUpsert, DynCollectionAsync, DynMirrorAsync, DynVaultAsync, MirrorAsync, MirrorSetting,
    OpenMode, OpenOptions, VaultAsync, VaultErrorCode as Code,
};

use crate::common::{
    Scratch, both, command, git, open, snapshot, vault as built_vault, work_tree, write,
};
use crate::probe::{assert_settled, commits, git_out};

fn standard(name: &str) -> CollectionDeclaration {
    serde_json::from_value(serde_json::json!({"name": name, "schema": {"standard": {}}})).unwrap()
}

fn isolated(name: &str) -> CollectionDeclaration {
    serde_json::from_value(
        serde_json::json!({"name": name, "schema": {"standard": {}, "isolated": true}}),
    )
    .unwrap()
}

/// The collection `name` of `vault`, which must be declared.
async fn collection(vault: &DynVaultAsync, name: &str) -> DynCollectionAsync {
    vault
        .get_collection(CollectionKey::default().with_name(name))
        .await
        .unwrap()
        .to_async_inline()
        .unwrap()
}

/// The mirror of `vault`.
async fn mirror_of(vault: &DynVaultAsync) -> DynMirrorAsync {
    vault
        .mirror(Empty::default())
        .await
        .unwrap()
        .to_async_inline()
        .unwrap()
}

fn id(id: &str) -> DocumentId {
    DocumentId {
        id: Some(id.to_owned()),
        ..Default::default()
    }
}

fn upsert(id: &str, raw: &str, note: &str) -> DocumentUpsert {
    DocumentUpsert {
        id: Some(id.to_owned()),
        raw: Some(raw.as_bytes().to_vec()),
        note: Some(note.to_owned()),
        ..Default::default()
    }
}

/// Writes `raw` as the Document `id` of `notes`.
async fn put(vault: &DynVaultAsync, id: &str, raw: &str) {
    put_noted(vault, id, raw, &format!("write {id}")).await;
}

async fn put_noted(vault: &DynVaultAsync, doc: &str, raw: &str, note: &str) {
    collection(vault, "notes")
        .await
        .upsert(upsert(doc, raw, note))
        .await
        .unwrap();
}

/// The text of the Document `id` of `notes`, as the handle reads it.
async fn read(vault: &DynVaultAsync, doc: &str) -> String {
    let found = collection(vault, "notes").await.get(id(doc)).await.unwrap();
    String::from_utf8(found.raw.unwrap()).unwrap()
}

async fn is_absent(vault: &DynVaultAsync, doc: &str) -> bool {
    collection(vault, "notes")
        .await
        .get(id(doc))
        .await
        .is_err_and(|error| error.is(Code::NotFound))
}

/// An empty bare repository on the branch `main`.
fn bare(scratch: &Scratch, name: &str) -> PathBuf {
    let path = scratch.join(name);
    std::fs::create_dir_all(&path).unwrap();
    git(
        &path,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    path
}

fn url(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// A bare mirror that holds a Vault with the collection `notes` and the
/// Documents `docs` (id, text), made through the handle itself.
async fn seeded(scratch: &Scratch, name: &str, docs: &[(&str, &str)]) -> PathBuf {
    let mirror = bare(scratch, &format!("{name}.git"));
    let seed = open(
        &scratch.join(&format!("{name}-seed")),
        OpenOptions::default(),
    )
    .await
    .unwrap();
    seed.create_collection(standard("notes")).await.unwrap();
    for (id, text) in docs {
        put(&seed, id, text).await;
    }
    mirror_of(&seed)
        .await
        .set(MirrorSetting::default().with_url(url(&mirror)))
        .await
        .unwrap();
    mirror_of(&seed).await.push(Empty::default()).await.unwrap();
    mirror
}

async fn clone_of(scratch: &Scratch, name: &str, mirror: &Path) -> (PathBuf, DynVaultAsync) {
    let path = scratch.join(name);
    let vault = open(&path, OpenOptions::default().with_mirror(url(mirror)))
        .await
        .unwrap();
    (path, vault)
}

fn head(dir: &Path) -> String {
    git_out(dir, &["rev-parse", "HEAD"])
}

fn mirror_head(mirror: &Path) -> String {
    git_out(mirror, &["rev-parse", "refs/heads/main"])
}

/// `git status` is clean: what a clone or an `open` leaves.
fn assert_clean(dir: &Path) {
    assert_eq!(git_out(dir, &["status", "--porcelain"]), "");
}

async fn pull(vault: &DynVaultAsync) -> Result<Empty, scheda::Error> {
    mirror_of(vault).await.pull(Empty::default()).await
}

#[test]
fn a_missing_path_an_empty_folder_and_create_new_clone_the_mirror() {
    both(|| async {
        let scratch = Scratch::new("clone");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let before = commits(&scratch.join("m-seed"));
        for (name, folder, mode) in [
            ("missing", false, OpenMode::OPEN_MODE_OPEN_OR_CREATE),
            ("empty", true, OpenMode::OPEN_MODE_OPEN_OR_CREATE),
            ("create-new", false, OpenMode::OPEN_MODE_CREATE_NEW),
            ("create-new-empty", true, OpenMode::OPEN_MODE_CREATE_NEW),
        ] {
            let path = scratch.join(&format!("{name}/vault"));
            if folder {
                std::fs::create_dir_all(&path).unwrap();
            }
            let vault = open(
                &path,
                OpenOptions::default()
                    .with_mirror(url(&mirror))
                    .with_mode(mode),
            )
            .await
            .unwrap();
            assert_eq!(read(&vault, "a.md").await, "one", "{name}");
            assert_eq!(commits(&path), before, "{name}: the history of the mirror");
            assert_eq!(head(&path), mirror_head(&mirror), "{name}");
            assert_eq!(
                git_out(&path, &["config", "remote.origin.url"]),
                url(&mirror)
            );
            assert_eq!(
                git_out(&path, &["config", "--local", "core.longpaths"]),
                "true"
            );
            assert_clean(&path);
            drop(vault);
            // Open again: the Vault exists, and the URL is its mirror's.
            open(&path, OpenOptions::default().with_mirror(url(&mirror)))
                .await
                .unwrap();
            assert_eq!(commits(&path), before, "{name}: open adds nothing");
        }
    });
}

#[test]
fn a_mirror_built_by_hand_that_is_a_vault_opens_with_its_documents() {
    both(|| async {
        let scratch = Scratch::new("by-hand");
        let hand = scratch.join("hand");
        built_vault(&hand);
        write(&hand, "notes/a.md", "written by hand");
        git(&hand, &["add", "--all"]);
        git(&hand, &["commit", "--quiet", "--message", "a Document"]);
        let (path, vault) = clone_of(&scratch, "vault", &hand).await;
        assert_eq!(read(&vault, "a.md").await, "written by hand");
        assert_eq!(
            commits(&path),
            2,
            "the history of the mirror, no marker commit"
        );
        assert_eq!(
            vault
                .list_collections(Empty::default())
                .await
                .unwrap()
                .collections
                .len(),
            2
        );
    });
}

#[test]
fn an_empty_mirror_gives_a_vault_of_generation_two_in_one_commit() {
    both(|| async {
        let scratch = Scratch::new("empty-mirror");
        let mirror = bare(&scratch, "m.git");
        let (path, _vault) = clone_of(&scratch, "vault", &mirror).await;
        assert_eq!(commits(&path), 1);
        assert_eq!(
            std::fs::read_to_string(path.join(".scheda.yaml")).unwrap(),
            "version: 2\n"
        );
        let ignore = std::fs::read_to_string(path.join(".gitignore")).unwrap();
        assert!(ignore.lines().any(|line| line == ".scheda/"), "{ignore}");
        assert_eq!(
            git_out(&path, &["ls-tree", "-r", "--name-only", "HEAD"]),
            ".gitignore\n.scheda.yaml"
        );
        assert_settled(&path, &[]);
        // `open` sends nothing without `auto_sync`.
        assert_eq!(git_out(&mirror, &["for-each-ref"]), "");
    });
}

#[test]
fn a_plain_repository_as_mirror_gets_the_marker_on_top_of_its_history() {
    both(|| async {
        let scratch = Scratch::new("plain-mirror");
        let plain = scratch.join("plain");
        work_tree(&plain, &[("readme.md", "hello\n")]);
        let (path, _vault) = clone_of(&scratch, "vault", &plain).await;
        assert_eq!(commits(&path), 2);
        assert_eq!(git_out(&path, &["show", "HEAD~1:readme.md"]), "hello");
        assert_eq!(
            std::fs::read_to_string(path.join(".scheda.yaml")).unwrap(),
            "version: 2\n"
        );
        assert_settled(&path, &[]);
    });
}

#[test]
fn a_mirror_with_a_marker_of_another_generation_is_generation_and_leaves_nothing() {
    both(|| async {
        let scratch = Scratch::new("generation");
        let other = scratch.join("other");
        work_tree(&other, &[(".scheda.yaml", "version: 3\n")]);
        let target = scratch.join("here/vault");
        let error = open(&target, OpenOptions::default().with_mirror(url(&other)))
            .await
            .err()
            .unwrap();
        assert!(error.is(Code::Generation));
        assert!(!target.exists());
        let left: Vec<_> = std::fs::read_dir(scratch.join("here"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "{left:?}");
        // An empty folder stays an empty folder.
        let folder = scratch.join("folder");
        std::fs::create_dir(&folder).unwrap();
        let error = open(&folder, OpenOptions::default().with_mirror(url(&other)))
            .await
            .err()
            .unwrap();
        assert!(error.is(Code::Generation));
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);
    });
}

#[test]
fn a_clone_that_fails_leaves_no_vault_and_returns() {
    both(|| async {
        let scratch = Scratch::new("bad-clone");
        let bad = format!("file://{}", scratch.join("no-such-mirror.git").display());
        let start = Instant::now();
        let target = scratch.join("here/vault");
        let error = open(&target, OpenOptions::default().with_mirror(&bad))
            .await
            .err()
            .unwrap();
        assert!(error.is(Code::Replication));
        assert!(start.elapsed() < Duration::from_secs(30));
        assert!(!target.exists());
        let left: Vec<_> = std::fs::read_dir(scratch.join("here"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "{left:?}");
        // Nor does OPEN_EXISTING clone: the path is missing.
        let error = open(
            &target,
            OpenOptions::default()
                .with_mirror(&bad)
                .with_mode(OpenMode::OPEN_MODE_OPEN_EXISTING),
        )
        .await
        .err()
        .unwrap();
        assert!(error.is(Code::NotFound));
    });
}

#[test]
fn two_opens_of_one_new_path_with_a_mirror_make_one_vault() {
    both(|| async {
        let scratch = Scratch::new("clone-race");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let before = commits(&scratch.join("m-seed"));
        for round in 0..3 {
            let path = scratch.join(&format!("race{round}/vault"));
            let handles: Vec<_> = (0..3)
                .map(|_| {
                    let path = path.clone();
                    let options = OpenOptions::default().with_mirror(url(&mirror));
                    tokio::spawn(async move { open(&path, options).await })
                })
                .collect();
            let mut vaults: Vec<Result<DynVaultAsync, scheda::Error>> = Vec::new();
            for handle in handles {
                vaults.push(handle.await.unwrap());
            }
            for vault in &vaults {
                assert!(vault.is_ok(), "round {round}: {:?}", vault.as_ref().err());
            }
            assert_eq!(commits(&path), before, "round {round}");
            assert_clean(&path);
            let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(left.len(), 1, "round {round}: {left:?}");
        }
    });
}

#[test]
fn pull_with_nothing_new_changes_nothing() {
    both(|| async {
        let scratch = Scratch::new("pull-nothing");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path, vault) = clone_of(&scratch, "a", &mirror).await;
        let before = head(&path);
        pull(&vault).await.unwrap();
        pull(&vault).await.unwrap();
        assert_eq!(head(&path), before);
        assert_settled(&path, &[]);
    });
}

#[test]
fn a_pull_fast_forwards_and_get_document_reads_the_new_documents_at_once() {
    both(|| async {
        let scratch = Scratch::new("pull-ff");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path_a, a) = clone_of(&scratch, "a", &mirror).await;
        let (_, b) = clone_of(&scratch, "b", &mirror).await;
        put(&b, "b.md", "from b").await;
        put(&b, "a.md", "two").await;
        mirror_of(&b).await.push(Empty::default()).await.unwrap();
        let before = commits(&path_a);
        assert!(is_absent(&a, "b.md").await);
        pull(&a).await.unwrap();
        assert_eq!(read(&a, "b.md").await, "from b");
        assert_eq!(read(&a, "a.md").await, "two");
        assert_eq!(commits(&path_a), before + 2);
        assert_eq!(head(&path_a), mirror_head(&mirror));
        assert_eq!(
            std::fs::read_to_string(path_a.join("notes/b.md")).unwrap(),
            "from b"
        );
        assert_settled(&path_a, &[]);
    });
}

#[test]
fn both_sides_changing_different_documents_join_in_one_commit_with_both_parents() {
    both(|| async {
        let scratch = Scratch::new("join");
        let mirror = seeded(&scratch, "m", &[("a.md", "one"), ("gone.md", "x")]).await;
        let (path_a, a) = clone_of(&scratch, "a", &mirror).await;
        let (_, b) = clone_of(&scratch, "b", &mirror).await;
        put(&b, "from-b.md", "b").await;
        collection(&b, "notes")
            .await
            .delete(DocumentDeletion {
                id: Some("gone.md".to_owned()),
                note: Some("remove".to_owned()),
                ..Default::default()
            })
            .await
            .unwrap();
        mirror_of(&b).await.push(Empty::default()).await.unwrap();
        put(&a, "from-a.md", "a").await;
        let ours = head(&path_a);
        let theirs = mirror_head(&mirror);
        let before = commits(&path_a);
        pull(&a).await.unwrap();
        let joined = git_out(&path_a, &["rev-list", "--parents", "-n", "1", "HEAD"]);
        assert_eq!(joined, format!("{} {ours} {theirs}", head(&path_a)));
        assert_eq!(
            git_out(&path_a, &["log", "-1", "--format=%s"]),
            "join the mirror's writes"
        );
        // Their commits, ours, and the join.
        assert_eq!(commits(&path_a), before + 2 + 1);
        assert_eq!(read(&a, "from-a.md").await, "a");
        assert_eq!(read(&a, "from-b.md").await, "b");
        assert_eq!(read(&a, "a.md").await, "one");
        assert!(is_absent(&a, "gone.md").await);
        assert!(!path_a.join("notes/gone.md").exists());
        assert_settled(&path_a, &[]);
        // The join can be pushed, and then the mirror holds it.
        mirror_of(&a).await.push(Empty::default()).await.unwrap();
        assert_eq!(mirror_head(&mirror), head(&path_a));
    });
}

#[test]
fn both_sides_changing_one_document_differently_is_mirror_conflict_and_changes_nothing() {
    both(|| async {
        let scratch = Scratch::new("conflict");
        let mirror = seeded(&scratch, "m", &[("a.md", "one"), ("b.md", "one")]).await;
        let (path_a, a) = clone_of(&scratch, "a", &mirror).await;
        let (_, b) = clone_of(&scratch, "b", &mirror).await;
        put(&b, "a.md", "from b").await;
        put(&b, "b.md", "from b").await;
        put(&b, "only-b.md", "b").await;
        mirror_of(&b).await.push(Empty::default()).await.unwrap();
        put(&a, "a.md", "from a").await;
        put(&a, "b.md", "from a").await;
        let before_head = head(&path_a);
        let before_files = snapshot(&path_a.join("notes"));
        let error = pull(&a).await.unwrap_err();
        assert!(error.is(Code::MirrorConflict));
        let text = error.to_string();
        assert!(
            text.contains("notes/a.md") && text.contains("notes/b.md"),
            "{text}"
        );
        assert_eq!(head(&path_a), before_head);
        assert_eq!(snapshot(&path_a.join("notes")), before_files);
        assert_eq!(read(&a, "a.md").await, "from a");
        assert!(is_absent(&a, "only-b.md").await);
        assert_eq!(git_out(&path_a, &["ls-files", "--unmerged"]), "");
        assert!(!path_a.join(".git/MERGE_HEAD").exists());
        assert_settled(&path_a, &[]);
        // `Sync` stops at the failed pull.
        let error = mirror_of(&a)
            .await
            .sync(Empty::default())
            .await
            .unwrap_err();
        assert!(error.is(Code::MirrorConflict));
        assert_eq!(head(&path_a), before_head);
    });
}

#[test]
fn both_sides_changing_a_document_the_same_way_join() {
    both(|| async {
        let scratch = Scratch::new("same");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path_a, a) = clone_of(&scratch, "a", &mirror).await;
        let (_, b) = clone_of(&scratch, "b", &mirror).await;
        put(&b, "a.md", "same").await;
        mirror_of(&b).await.push(Empty::default()).await.unwrap();
        // The same bytes under another note: another commit, the same file.
        put_noted(&a, "a.md", "same", "a's own words").await;
        pull(&a).await.unwrap();
        let parents = git_out(&path_a, &["rev-list", "--parents", "-n", "1", "HEAD"]);
        assert_eq!(parents.split(' ').count(), 3, "{parents}");
        assert_eq!(read(&a, "a.md").await, "same");
        assert_settled(&path_a, &[]);
    });
}

#[test]
fn a_pull_from_an_empty_mirror_is_nothing_to_pull() {
    both(|| async {
        let scratch = Scratch::new("pull-empty");
        let mirror = bare(&scratch, "m.git");
        let (path, vault) = clone_of(&scratch, "a", &mirror).await;
        let before = head(&path);
        pull(&vault).await.unwrap();
        assert_eq!(head(&path), before);
        assert_settled(&path, &[]);
    });
}

#[test]
fn a_person_edit_of_a_file_the_pull_changes_survives_and_git_shows_it_changed() {
    both(|| async {
        let scratch = Scratch::new("pull-edit");
        let mirror = seeded(&scratch, "m", &[("a.md", "one"), ("b.md", "one")]).await;
        let (path_a, a) = clone_of(&scratch, "a", &mirror).await;
        let (_, b) = clone_of(&scratch, "b", &mirror).await;
        put(&b, "a.md", "from b").await;
        put(&b, "b.md", "from b").await;
        mirror_of(&b).await.push(Empty::default()).await.unwrap();
        write(&path_a, "notes/a.md", "mine, not committed");
        pull(&a).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(path_a.join("notes/a.md")).unwrap(),
            "mine, not committed"
        );
        assert_eq!(
            std::fs::read_to_string(path_a.join("notes/b.md")).unwrap(),
            "from b"
        );
        // The handle reads the commit, not the person's file.
        assert_eq!(read(&a, "a.md").await, "from b");
        assert_eq!(
            git_out(&path_a, &["status", "--porcelain"]),
            " M notes/a.md"
        );
        assert_settled(&path_a, &["notes/a.md"]);
    });
}

#[test]
fn pull_push_and_sync_without_a_mirror_are_not_found() {
    both(|| async {
        let scratch = Scratch::new("no-mirror");
        let vault = open(&scratch.join("v"), OpenOptions::default())
            .await
            .unwrap();
        for error in [
            mirror_of(&vault)
                .await
                .pull(Empty::default())
                .await
                .unwrap_err(),
            mirror_of(&vault)
                .await
                .push(Empty::default())
                .await
                .unwrap_err(),
            mirror_of(&vault)
                .await
                .sync(Empty::default())
                .await
                .unwrap_err(),
            mirror_of(&vault)
                .await
                .remove(Empty::default())
                .await
                .unwrap_err(),
        ] {
            assert!(error.is(Code::NotFound));
        }
    });
}

#[test]
fn push_sends_the_commits_and_a_mirror_ahead_is_mirror_ahead() {
    both(|| async {
        let scratch = Scratch::new("push");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path_a, a) = clone_of(&scratch, "a", &mirror).await;
        let (_, b) = clone_of(&scratch, "b", &mirror).await;
        put(&a, "a.md", "two").await;
        put(&a, "x.md", "x").await;
        assert_ne!(mirror_head(&mirror), head(&path_a));
        mirror_of(&a).await.push(Empty::default()).await.unwrap();
        assert_eq!(mirror_head(&mirror), head(&path_a));
        // Nothing to send is not an error.
        mirror_of(&a).await.push(Empty::default()).await.unwrap();
        // B has not pulled A's writes: its push is refused.
        put(&b, "y.md", "y").await;
        let error = mirror_of(&b)
            .await
            .push(Empty::default())
            .await
            .unwrap_err();
        assert!(error.is(Code::MirrorAhead));
        assert_eq!(mirror_head(&mirror), head(&path_a));
        // After the pull and the join, it goes through.
        pull(&b).await.unwrap();
        mirror_of(&b).await.push(Empty::default()).await.unwrap();
        assert_eq!(read(&b, "x.md").await, "x");
    });
}

#[test]
fn sync_against_an_empty_mirror_pushes() {
    both(|| async {
        let scratch = Scratch::new("sync-empty");
        let mirror = bare(&scratch, "m.git");
        let vault = open(&scratch.join("v"), OpenOptions::default())
            .await
            .unwrap();
        vault.create_collection(standard("notes")).await.unwrap();
        put(&vault, "a.md", "one").await;
        mirror_of(&vault)
            .await
            .set(MirrorSetting::default().with_url(url(&mirror)))
            .await
            .unwrap();
        mirror_of(&vault)
            .await
            .sync(Empty::default())
            .await
            .unwrap();
        assert_eq!(mirror_head(&mirror), head(&scratch.join("v")));
        assert_settled(&scratch.join("v"), &[]);
    });
}

#[test]
fn sync_of_two_clones_that_wrote_different_documents_ends_with_everything_everywhere() {
    both(|| async {
        let scratch = Scratch::new("sync-two");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path_a, a) = clone_of(&scratch, "a", &mirror).await;
        let (path_b, b) = clone_of(&scratch, "b", &mirror).await;
        put(&a, "from-a.md", "a").await;
        put(&b, "from-b.md", "b").await;
        mirror_of(&a).await.sync(Empty::default()).await.unwrap();
        mirror_of(&b).await.sync(Empty::default()).await.unwrap();
        mirror_of(&a).await.sync(Empty::default()).await.unwrap();
        assert_eq!(head(&path_a), mirror_head(&mirror));
        assert_eq!(head(&path_b), mirror_head(&mirror));
        for vault in [&a, &b] {
            assert_eq!(read(vault, "from-a.md").await, "a");
            assert_eq!(read(vault, "from-b.md").await, "b");
            assert_eq!(read(vault, "a.md").await, "one");
        }
        assert_settled(&path_a, &[]);
        assert_settled(&path_b, &[]);
        git(&mirror, &["fsck", "--no-progress", "--strict"]);
    });
}

/// A Vault at `path` with the mirror `mirror` and an isolated collection
/// `diary` whose nested repository has an `origin` that cannot be reached.
async fn vault_with_nested_origin(
    scratch: &Scratch,
    mirror_path: &Path,
    options: OpenOptions,
) -> DynVaultAsync {
    let path = scratch.join("v");
    let vault = open(&path, options).await.unwrap();
    vault.create_collection(standard("notes")).await.unwrap();
    vault.create_collection(isolated("diary")).await.unwrap();
    mirror_of(&vault)
        .await
        .set(MirrorSetting::default().with_url(url(mirror_path)))
        .await
        .unwrap();
    collection(&vault, "diary")
        .await
        .upsert(upsert("d", "secret", "diary"))
        .await
        .unwrap();
    let nested = path.join(".isolated");
    assert!(nested.join(".git").exists());
    let unreachable = format!(
        "file://{}",
        scratch.join("unreachable/nested.git").display()
    );
    git(&nested, &["remote", "add", "origin", &unreachable]);
    vault
}

fn assert_nested_untouched(vault_path: &Path) {
    let git_dir = vault_path.join(".isolated/.git");
    assert!(!git_dir.join("FETCH_HEAD").exists());
    assert!(!git_dir.join("refs/remotes").exists() || is_empty_dir(&git_dir.join("refs/remotes")));
    assert_eq!(
        git_out(
            &vault_path.join(".isolated"),
            &["for-each-ref", "refs/remotes"]
        ),
        ""
    );
}

fn is_empty_dir(path: &Path) -> bool {
    fn files(path: &Path) -> usize {
        std::fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                if path.is_dir() { files(&path) } else { 1 }
            })
            .sum()
    }
    files(path) == 0
}

#[test]
fn no_call_contacts_a_nested_repository_under_isolated() {
    both(|| async {
        let scratch = Scratch::new("nested");
        let mirror = bare(&scratch, "m.git");
        let vault = vault_with_nested_origin(&scratch, &mirror, OpenOptions::default()).await;
        let path = scratch.join("v");
        mirror_of(&vault)
            .await
            .push(Empty::default())
            .await
            .unwrap();
        pull(&vault).await.unwrap();
        mirror_of(&vault)
            .await
            .sync(Empty::default())
            .await
            .unwrap();
        assert_nested_untouched(&path);
        // Only the repository of the Vault reached the mirror.
        let tree = git_out(&mirror, &["ls-tree", "-r", "--name-only", "main"]);
        assert!(!tree.contains(".isolated"), "{tree}");
        assert!(!tree.contains("secret"), "{tree}");
        drop(vault);
        // With `auto_sync`, the writes and the open call nobody either.
        let vault = open(&path, OpenOptions::default().with_auto_sync(true))
            .await
            .unwrap();
        put(&vault, "a.md", "x").await;
        collection(&vault, "diary")
            .await
            .upsert(upsert("e", "more", "diary"))
            .await
            .unwrap();
        assert_nested_untouched(&path);
        assert_eq!(mirror_head(&mirror), head(&path));
    });
}

#[test]
fn set_mirror_sets_repeats_refuses_another_url_and_replaces_on_request() {
    both(|| async {
        let scratch = Scratch::new("set");
        let path = scratch.join("v");
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        let first = "ssh://git@first.invalid/org/repo.git";
        let second = "https://second.invalid/org/repo.git";
        let setting = |url: &str, replace: bool| {
            let setting = MirrorSetting::default().with_url(url);
            if replace {
                setting.with_replace_existing(true)
            } else {
                setting
            }
        };
        let before = commits(&path);
        mirror_of(&vault)
            .await
            .set(setting(first, false))
            .await
            .unwrap();
        assert_eq!(git_out(&path, &["config", "remote.origin.url"]), first);
        // Again with the same URL: nothing happens.
        mirror_of(&vault)
            .await
            .set(setting(first, false))
            .await
            .unwrap();
        mirror_of(&vault)
            .await
            .set(setting(first, true))
            .await
            .unwrap();
        assert_eq!(git_out(&path, &["config", "remote.origin.url"]), first);
        // Another URL names the current one.
        let error = mirror_of(&vault)
            .await
            .set(setting(second, false))
            .await
            .unwrap_err();
        assert!(error.is(Code::AlreadyExists));
        assert!(error.to_string().contains("first.invalid"), "{error}");
        assert!(!error.to_string().contains("org/repo"), "{error}");
        assert_eq!(git_out(&path, &["config", "remote.origin.url"]), first);
        mirror_of(&vault)
            .await
            .set(setting(second, true))
            .await
            .unwrap();
        assert_eq!(git_out(&path, &["config", "remote.origin.url"]), second);
        // The Vault opens with the new URL as its mirror.
        drop(
            open(&path, OpenOptions::default().with_mirror(second))
                .await
                .unwrap(),
        );
        // A URL with a password, or that is not a URL, is refused.
        for bad in [
            "https://user:secret@third.invalid/repo.git",
            "ssh://user:secret@third.invalid/repo.git",
            "not a url",
            "",
        ] {
            let error = mirror_of(&vault)
                .await
                .set(setting(bad, true))
                .await
                .unwrap_err();
            assert!(error.is(Code::InvalidArgument), "{bad}");
            assert!(!error.to_string().contains("secret"));
        }
        assert_eq!(git_out(&path, &["config", "remote.origin.url"]), second);
        // None of it touched the network or the history.
        assert!(!path.join(".git/FETCH_HEAD").exists());
        assert_eq!(commits(&path), before);
        assert_settled(&path, &[]);
    });
}

#[test]
fn remove_mirror_detaches_it_and_again_is_not_found() {
    both(|| async {
        let scratch = Scratch::new("remove");
        let path = scratch.join("v");
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        mirror_of(&vault)
            .await
            .set(MirrorSetting::default().with_url("ssh://git@first.invalid/org/repo.git"))
            .await
            .unwrap();
        assert_eq!(git_out(&path, &["remote"]), "origin");
        mirror_of(&vault)
            .await
            .remove(Empty::default())
            .await
            .unwrap();
        assert_eq!(git_out(&path, &["remote"]), "");
        let error = mirror_of(&vault)
            .await
            .remove(Empty::default())
            .await
            .unwrap_err();
        assert!(error.is(Code::NotFound));
        // Without a mirror the calls are `NOT_FOUND`, and the Vault can take a
        // new one.
        let error = mirror_of(&vault)
            .await
            .push(Empty::default())
            .await
            .unwrap_err();
        assert!(error.is(Code::NotFound));
        mirror_of(&vault)
            .await
            .set(MirrorSetting::default().with_url("ssh://git@second.invalid/org/repo.git"))
            .await
            .unwrap();
        assert!(!path.join(".git/FETCH_HEAD").exists());
        assert_settled(&path, &[]);
    });
}

#[test]
fn auto_sync_pushes_every_write_of_the_main_repository() {
    both(|| async {
        let scratch = Scratch::new("auto-push");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let path = scratch.join("a");
        let vault = open(
            &path,
            OpenOptions::default()
                .with_mirror(url(&mirror))
                .with_auto_sync(true),
        )
        .await
        .unwrap();
        put(&vault, "b.md", "two").await;
        assert_eq!(mirror_head(&mirror), head(&path));
        vault.create_collection(standard("more")).await.unwrap();
        assert_eq!(mirror_head(&mirror), head(&path));
        assert_settled(&path, &[]);
    });
}

#[test]
fn auto_sync_pulls_at_open() {
    both(|| async {
        let scratch = Scratch::new("auto-pull");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path, first) = clone_of(&scratch, "a", &mirror).await;
        let (_, b) = clone_of(&scratch, "b", &mirror).await;
        put(&b, "b.md", "from b").await;
        mirror_of(&b).await.push(Empty::default()).await.unwrap();
        drop(first);
        let again = open(
            &path,
            OpenOptions::default()
                .with_mirror(url(&mirror))
                .with_auto_sync(true),
        )
        .await
        .unwrap();
        assert_eq!(read(&again, "b.md").await, "from b");
        assert_eq!(head(&path), mirror_head(&mirror));
    });
}

#[cfg(unix)]
#[test]
fn a_pull_holds_the_write_lock_and_a_write_of_another_handle_waits_for_it() {
    both(|| async {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("pull-lock");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path, a) = clone_of(&scratch, "a", &mirror).await;
        let (_, b) = clone_of(&scratch, "b", &mirror).await;
        put(&b, "from-b.md", "b").await;
        mirror_of(&b).await.push(Empty::default()).await.unwrap();
        // The fetch takes a second: the mirror's upload-pack is slow.
        let slow = scratch.join("slow-upload-pack.sh");
        std::fs::write(&slow, "#!/bin/sh\nsleep 1\nexec git upload-pack \"$@\"\n").unwrap();
        std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
        let configured = command(
            &path,
            &["config", "remote.origin.uploadpack", slow.to_str().unwrap()],
        )
        .status()
        .unwrap();
        assert!(configured.success());
        let other = open(&path, OpenOptions::default()).await.unwrap();
        let ends: Arc<Mutex<Vec<(&str, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();
        let pulling = {
            let a = a.clone();
            let ends = Arc::clone(&ends);
            tokio::spawn(async move {
                pull(&a).await.unwrap();
                ends.lock().unwrap().push(("pull", Instant::now()));
            })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        // The fetch is running, with the lock held.
        put(&other, "from-other.md", "other").await;
        ends.lock().unwrap().push(("write", Instant::now()));
        pulling.await.unwrap();
        let ends = std::mem::take(&mut *ends.lock().unwrap());
        assert_eq!(ends.first().map(|end| end.0), Some("pull"), "{ends:?}");
        assert!(ends[0].1 - started >= Duration::from_millis(900));
        assert_eq!(read(&other, "from-b.md").await, "b");
        assert_eq!(read(&other, "from-other.md").await, "other");
        assert_eq!(read(&a, "from-other.md").await, "other");
        assert_settled(&path, &[]);
    });
}

/// A commit on top of the mirror's branch made with plain git in a second
/// clone: each `(path, Some(text))` is written, each `(path, None)` is
/// removed. Returns the commit the mirror now holds.
fn commit_by_hand(
    scratch: &Scratch,
    name: &str,
    mirror: &Path,
    edits: &[(&str, Option<&str>)],
) -> String {
    let dir = scratch.join(name);
    git(
        &scratch.join("."),
        &["clone", "--quiet", &url(mirror), name],
    );
    for (rel, text) in edits {
        match text {
            Some(text) => write(&dir, rel, text),
            None => std::fs::remove_file(dir.join(rel)).unwrap(),
        }
    }
    git(&dir, &["add", "--all"]);
    git(&dir, &["commit", "--quiet", "--message", "by hand"]);
    git(&dir, &["push", "--quiet", "origin", "HEAD:main"]);
    mirror_head(mirror)
}

/// What a refused pull must leave: `HEAD`, the work tree, git's index, and
/// the calls that read the Vault.
struct Standing {
    head: String,
    files: Vec<(String, Vec<u8>)>,
    index: String,
    status: String,
}

impl Standing {
    fn of(path: &Path) -> Standing {
        Standing {
            head: head(path),
            files: snapshot(&path.join("notes")),
            index: git_out(path, &["ls-files", "--stage"]),
            status: git_out(path, &["status", "--porcelain"]),
        }
    }
}

/// The pull of `vault` at `path` is refused with `code`, naming `file`
/// when given, and the Vault stands as `before` says.
async fn assert_refused(
    vault: &DynVaultAsync,
    path: &Path,
    code: Code,
    file: Option<&str>,
    before: &Standing,
) {
    let error = pull(vault).await.unwrap_err();
    assert!(error.is(code), "{error}");
    let report = scheda::error_report(&error);
    assert_eq!(report.path.as_deref(), file, "{error}");
    let after = Standing::of(path);
    assert_eq!(after.head, before.head);
    assert_eq!(after.files, before.files);
    assert_eq!(after.index, before.index);
    assert_eq!(after.status, before.status);
    vault.list_collections(Empty::default()).await.unwrap();
    assert!(
        vault
            .check(Empty::default())
            .await
            .unwrap()
            .problems
            .is_empty()
    );
    assert_eq!(read(vault, "a.md").await, "one");
}

#[test]
fn a_pull_of_a_mirror_of_another_generation_is_generation_and_changes_nothing() {
    both(|| async {
        let scratch = Scratch::new("pull-generation");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path, a) = clone_of(&scratch, "a", &mirror).await;
        commit_by_hand(
            &scratch,
            "hand",
            &mirror,
            &[(".scheda.yaml", Some("version: 3\n"))],
        );
        let before = Standing::of(&path);
        assert_refused(&a, &path, Code::Generation, Some(".scheda.yaml"), &before).await;
        // `Sync` stops at the refused pull.
        let error = mirror_of(&a)
            .await
            .sync(Empty::default())
            .await
            .unwrap_err();
        assert!(error.is(Code::Generation));
        assert_eq!(head(&path), before.head);
    });
}

#[test]
fn a_join_with_a_mirror_of_another_generation_is_generation_and_changes_nothing() {
    both(|| async {
        let scratch = Scratch::new("join-generation");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path, a) = clone_of(&scratch, "a", &mirror).await;
        commit_by_hand(
            &scratch,
            "hand",
            &mirror,
            &[(".scheda.yaml", Some("version: 3\n"))],
        );
        put(&a, "own.md", "own").await;
        let before = Standing::of(&path);
        let commits_before = commits(&path);
        assert_refused(&a, &path, Code::Generation, Some(".scheda.yaml"), &before).await;
        assert_eq!(read(&a, "own.md").await, "own");
        // The join commit may stay in the object store, unreferenced.
        assert_eq!(commits(&path), commits_before);
    });
}

#[test]
fn a_pull_of_a_collection_file_that_does_not_load_is_invalid_argument_naming_it() {
    both(|| async {
        let scratch = Scratch::new("pull-bad-collection");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path, a) = clone_of(&scratch, "a", &mirror).await;
        commit_by_hand(
            &scratch,
            "hand",
            &mirror,
            &[("bad/.collection.yaml", Some("name: bad\n"))],
        );
        let before = Standing::of(&path);
        assert_refused(
            &a,
            &path,
            Code::InvalidArgument,
            Some("bad/.collection.yaml"),
            &before,
        )
        .await;
        assert!(!path.join("bad").exists());
    });
}

#[test]
fn a_pull_of_a_commit_without_the_marker_is_not_a_vault_and_changes_nothing() {
    both(|| async {
        let scratch = Scratch::new("pull-no-marker");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path, a) = clone_of(&scratch, "a", &mirror).await;
        commit_by_hand(&scratch, "hand", &mirror, &[(".scheda.yaml", None)]);
        let before = Standing::of(&path);
        assert_refused(&a, &path, Code::NotAVault, Some(".scheda.yaml"), &before).await;
        assert!(path.join(".scheda.yaml").exists());
    });
}

#[test]
fn auto_sync_at_open_keeps_a_refused_pull_as_the_sync_failure() {
    both(|| async {
        let scratch = Scratch::new("auto-pull-generation");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path, first) = clone_of(&scratch, "a", &mirror).await;
        drop(first);
        commit_by_hand(
            &scratch,
            "hand",
            &mirror,
            &[(".scheda.yaml", Some("version: 3\n"))],
        );
        let before = head(&path);
        let again = open(
            &path,
            OpenOptions::default()
                .with_mirror(url(&mirror))
                .with_auto_sync(true),
        )
        .await
        .unwrap();
        assert_eq!(head(&path), before);
        assert_eq!(read(&again, "a.md").await, "one");
        let info = mirror_of(&again)
            .await
            .info(Empty::default())
            .await
            .unwrap();
        let failure = info.last_auto_sync_error.as_option().expect("a failure");
        assert!(
            failure
                .code
                .as_deref()
                .is_some_and(|code| code.contains("GENERATION")),
            "{failure:?}"
        );
    });
}
