//! The mirror through the public handle: `open` that clones, `Pull`, `Push`,
//! `Sync`, `SetMirror`, `RemoveMirror`, and the nested repository that no
//! call contacts. The mirrors are bare repositories on disk reached through
//! `file://` URLs; nothing here touches a network.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use buffa_types::google::protobuf::Empty;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, CollectionKey, DocumentDeletion, DocumentId,
    DocumentUpsert, DynCollectionAsync, DynMirrorAsync, DynVaultAsync, MirrorAsync, MirrorSetting,
    OpenMode, OpenOptions, VaultAsync, VaultErrorCode as Code, WorkTree,
};

use crate::common::{
    Scratch, both, file_url, git, open, snapshot, vault as built_vault, work_tree, write,
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

/// A bare mirror that holds a Vault with the collection `notes` and the
/// Documents `docs` (id, text), made through the handle itself.
async fn seeded(scratch: &Scratch, name: &str, docs: &[(&str, &str)]) -> PathBuf {
    seeded_as(scratch, name, docs, OpenOptions::default()).await
}

/// [`seeded`] with the Vault that makes the mirror opened with `options`.
async fn seeded_as(
    scratch: &Scratch,
    name: &str,
    docs: &[(&str, &str)],
    options: OpenOptions,
) -> PathBuf {
    let mirror = bare(scratch, &format!("{name}.git"));
    let seed = open(&scratch.join(&format!("{name}-seed")), options)
        .await
        .unwrap();
    seed.create_collection(standard("notes")).await.unwrap();
    for (id, text) in docs {
        put(&seed, id, text).await;
    }
    mirror_of(&seed)
        .await
        .set(MirrorSetting::default().with_url(file_url(&mirror)))
        .await
        .unwrap();
    mirror_of(&seed).await.push(Empty::default()).await.unwrap();
    mirror
}

async fn clone_of(scratch: &Scratch, name: &str, mirror: &Path) -> (PathBuf, DynVaultAsync) {
    let path = scratch.join(name);
    let vault = open(&path, OpenOptions::default().with_mirror(file_url(mirror)))
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
                    .with_mirror(file_url(&mirror))
                    .with_mode(mode),
            )
            .await
            .unwrap();
            assert_eq!(read(&vault, "a.md").await, "one", "{name}");
            assert_eq!(commits(&path), before, "{name}: the history of the mirror");
            assert_eq!(head(&path), mirror_head(&mirror), "{name}");
            assert_eq!(
                git_out(&path, &["config", "remote.origin.url"]),
                file_url(&mirror)
            );
            assert_eq!(
                git_out(&path, &["config", "--local", "core.longpaths"]),
                "true"
            );
            assert_clean(&path);
            drop(vault);
            // Open again: the Vault exists, and the URL is its mirror's.
            open(&path, OpenOptions::default().with_mirror(file_url(&mirror)))
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
fn an_empty_mirror_gives_a_vault_of_generation_four_in_one_commit() {
    both(|| async {
        let scratch = Scratch::new("empty-mirror");
        let mirror = bare(&scratch, "m.git");
        let (path, _vault) = clone_of(&scratch, "vault", &mirror).await;
        assert_eq!(commits(&path), 1);
        assert_eq!(
            std::fs::read_to_string(path.join(".scheda.yaml")).unwrap(),
            "version: 4\n"
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
            "version: 4\n"
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
        let error = open(
            &target,
            OpenOptions::default().with_mirror(file_url(&other)),
        )
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
        let error = open(
            &folder,
            OpenOptions::default().with_mirror(file_url(&other)),
        )
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
        let bad = file_url(&scratch.join("no-such-mirror.git"));
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
                    let options = OpenOptions::default().with_mirror(file_url(&mirror));
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
        // The join is stored as a pack: its commit and its trees are not
        // loose objects.
        for object in ["HEAD", "HEAD^{tree}", "HEAD:notes"] {
            let id = git_out(&path_a, &["rev-parse", object]);
            let loose = path_a.join(".git/objects").join(&id[..2]).join(&id[2..]);
            assert!(!loose.exists(), "{object} is a loose object");
        }
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
        // The fetch wrote no reference: there was no branch to write.
        assert_eq!(git_out(&path, &["for-each-ref", "refs/scheda/mirror"]), "");
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
            .set(MirrorSetting::default().with_url(file_url(&mirror)))
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
        .set(MirrorSetting::default().with_url(file_url(mirror_path)))
        .await
        .unwrap();
    collection(&vault, "diary")
        .await
        .upsert(upsert("d", "secret", "diary"))
        .await
        .unwrap();
    let nested = path.join(".isolated");
    assert!(nested.join(".git").exists());
    let unreachable = file_url(&scratch.join("unreachable/nested.git"));
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
                .with_mirror(file_url(&mirror))
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
                .with_mirror(file_url(&mirror))
                .with_auto_sync(true),
        )
        .await
        .unwrap();
        assert_eq!(read(&again, "b.md").await, "from b");
        assert_eq!(head(&path), mirror_head(&mirror));
    });
}

/// Makes `remote.origin.uploadpack` of the repository at `path` a script
/// that creates the file `started`, waits until the file `go` exists (it
/// polls the file itself, for a minute at most, so a test that fails leaves
/// it behind for no longer than that), then runs the real `git
/// upload-pack`: a fetch that stays on the network for as long as the test
/// wants.
#[cfg(unix)]
fn hold_fetches(scratch: &Scratch, path: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    use crate::common::command;

    let started = scratch.join("fetch-started");
    let go = scratch.join("fetch-go");
    let script = scratch.join("held-upload-pack.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n: > '{}'\ni=0\nwhile [ ! -e '{}' ] && [ $i -lt 3000 ]; do sleep 0.02; i=$((i + 1)); done\nexec git upload-pack \"$@\"\n",
            started.display(),
            go.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let configured = command(
        path,
        &[
            "config",
            "remote.origin.uploadpack",
            script.to_str().unwrap(),
        ],
    )
    .status()
    .unwrap();
    assert!(configured.success());
    (started, go)
}

/// Waits until `file` exists, for at most twenty seconds.
#[cfg(unix)]
async fn appears(file: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !file.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            file.display()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
#[test]
fn a_write_of_another_handle_is_not_held_up_by_a_fetch_waiting_on_the_network() {
    both(|| async {
        let scratch = Scratch::new("pull-no-lock");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path, a) = clone_of(&scratch, "a", &mirror).await;
        let (_, b) = clone_of(&scratch, "b", &mirror).await;
        put(&b, "from-b.md", "b").await;
        mirror_of(&b).await.push(Empty::default()).await.unwrap();
        let (started, go) = hold_fetches(&scratch, &path);
        let other = open(&path, OpenOptions::default()).await.unwrap();
        let pulling = {
            let a = a.clone();
            tokio::spawn(async move { pull(&a).await })
        };
        appears(&started).await;
        // The fetch is on the network and will not end until `go` exists:
        // a write of another handle still completes.
        tokio::time::timeout(
            Duration::from_secs(20),
            put(&other, "from-other.md", "other"),
        )
        .await
        .unwrap();
        assert!(!go.exists());
        assert!(!pulling.is_finished());
        std::fs::write(&go, b"").unwrap();
        tokio::time::timeout(Duration::from_secs(20), pulling)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        for handle in [&a, &other] {
            assert_eq!(read(handle, "from-b.md").await, "b");
            assert_eq!(read(handle, "from-other.md").await, "other");
        }
        assert_settled(&path, &[]);
    });
}

#[cfg(unix)]
#[test]
fn two_pulls_at_once_from_two_handles_both_succeed() {
    both(|| async {
        let scratch = Scratch::new("pull-twice");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path, a) = clone_of(&scratch, "a", &mirror).await;
        let (_, b) = clone_of(&scratch, "b", &mirror).await;
        put(&b, "from-b.md", "b").await;
        mirror_of(&b).await.push(Empty::default()).await.unwrap();
        let (started, go) = hold_fetches(&scratch, &path);
        let other = open(&path, OpenOptions::default()).await.unwrap();
        let first = {
            let a = a.clone();
            tokio::spawn(async move { pull(&a).await })
        };
        appears(&started).await;
        // The second pull waits for the fetch lock of the first.
        let second = {
            let other = other.clone();
            tokio::spawn(async move { pull(&other).await })
        };
        std::fs::write(&go, b"").unwrap();
        for pulling in [first, second] {
            tokio::time::timeout(Duration::from_secs(20), pulling)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        assert_eq!(read(&other, "from-b.md").await, "b");
        assert_eq!(head(&path), mirror_head(&mirror));
        assert_settled(&path, &[]);
    });
}

#[test]
fn a_pull_after_the_mirror_deleted_the_branch_is_nothing_to_pull_and_forgets_it() {
    both(|| async {
        let scratch = Scratch::new("pull-deleted");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path, a) = clone_of(&scratch, "a", &mirror).await;
        pull(&a).await.unwrap();
        assert!(!git_out(&path, &["for-each-ref", "refs/scheda/mirror"]).is_empty());
        git(&mirror, &["update-ref", "-d", "refs/heads/main"]);
        put(&a, "local.md", "local").await;
        let before = head(&path);
        pull(&a).await.unwrap();
        assert_eq!(head(&path), before);
        assert_eq!(git_out(&path, &["for-each-ref", "refs/scheda/mirror"]), "");
        assert_eq!(read(&a, "local.md").await, "local");
        assert_settled(&path, &[]);
    });
}

#[test]
fn a_lock_on_a_remote_tracking_reference_fails_neither_a_pull_nor_a_push() {
    both(|| async {
        let scratch = Scratch::new("tracking-lock");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path, a) = clone_of(&scratch, "a", &mirror).await;
        let (_, b) = clone_of(&scratch, "b", &mirror).await;
        put(&b, "from-b.md", "b").await;
        mirror_of(&b).await.push(Empty::default()).await.unwrap();
        // Another git process holds the lock of the reference `git push`
        // and `git fetch` would update after their work.
        let lock = path.join(".git/refs/remotes/origin/main.lock");
        std::fs::write(&lock, b"").unwrap();
        pull(&a).await.unwrap();
        assert_eq!(read(&a, "from-b.md").await, "b");
        put(&a, "from-a.md", "a").await;
        mirror_of(&a).await.push(Empty::default()).await.unwrap();
        assert_eq!(mirror_head(&mirror), head(&path));
        std::fs::remove_file(&lock).unwrap();
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
        &["clone", "--quiet", &file_url(mirror), name],
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
                .with_mirror(file_url(&mirror))
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

// ---------------------------------------------------------------------
// Clones, Pull, and Sync of a Vault without a work tree.

fn none() -> OpenOptions {
    OpenOptions::default().with_work_tree(WorkTree::WORK_TREE_NONE)
}

/// A clone of `mirror` made with `WORK_TREE_NONE`.
async fn bare_clone_of(scratch: &Scratch, name: &str, mirror: &Path) -> (PathBuf, DynVaultAsync) {
    let path = scratch.join(name);
    let vault = open(&path, none().with_mirror(file_url(mirror)))
        .await
        .unwrap();
    (path, vault)
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The Vault at `path` has no Document file, no git index, no checkout, and
/// no reflog, and its repository is marked bare.
fn assert_no_files(path: &Path) {
    assert_eq!(names_in(path), [".git", ".scheda"], "{path:?}");
    let git_dir = path.join(".git");
    assert!(!git_dir.join("index").exists(), "{path:?}: git index");
    assert!(!git_dir.join("logs").exists(), "{path:?}: reflog");
    assert_eq!(git_out(path, &["config", "--local", "core.bare"]), "true");
    assert_eq!(
        git_out(
            path,
            &["config", "--local", "--bool", "core.logAllRefUpdates"]
        ),
        "false"
    );
}

#[test]
fn a_clone_with_no_work_tree_is_bare_holds_no_file_and_reads_the_mirrors_documents() {
    both(|| async {
        let scratch = Scratch::new("none-clone");
        let mirror = seeded(&scratch, "m", &[("a.md", "one"), ("b.md", "two")]).await;
        let (path, vault) = bare_clone_of(&scratch, "c", &mirror).await;
        assert_no_files(&path);
        assert_eq!(head(&path), mirror_head(&mirror));
        assert_eq!(read(&vault, "a.md").await, "one");
        assert_eq!(read(&vault, "b.md").await, "two");
        assert_eq!(
            git_out(&path, &["config", "remote.origin.url"]),
            file_url(&mirror)
        );
        let info = vault.info(Default::default()).await.unwrap();
        assert_eq!(
            info.work_tree.and_then(|tree| tree.as_known()),
            Some(WorkTree::WORK_TREE_NONE)
        );
        // A write of the clone stays file-less too.
        put(&vault, "c.md", "three").await;
        assert_no_files(&path);
        drop(vault);
        open(&path, OpenOptions::default()).await.unwrap();
        assert_no_files(&path);
    });
}

#[test]
fn a_pull_fast_forwards_a_vault_without_a_work_tree_and_touches_no_index_no_file_no_reflog() {
    both(|| async {
        let scratch = Scratch::new("none-ff");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path_a, a) = bare_clone_of(&scratch, "a", &mirror).await;
        let (path_b, b) = bare_clone_of(&scratch, "b", &mirror).await;
        put(&a, "from-a.md", "a").await;
        mirror_of(&a).await.sync(Empty::default()).await.unwrap();
        pull(&b).await.unwrap();
        assert_eq!(head(&path_b), head(&path_a));
        assert_eq!(head(&path_b), mirror_head(&mirror));
        assert_eq!(read(&b, "from-a.md").await, "a");
        assert_no_files(&path_a);
        assert_no_files(&path_b);
    });
}

#[test]
fn both_vaults_without_a_work_tree_writing_different_documents_end_equal_through_a_join() {
    both(|| async {
        let scratch = Scratch::new("none-join");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path_a, a) = bare_clone_of(&scratch, "a", &mirror).await;
        let (path_b, b) = bare_clone_of(&scratch, "b", &mirror).await;
        put(&a, "from-a.md", "a").await;
        put(&b, "from-b.md", "b").await;
        mirror_of(&a).await.sync(Empty::default()).await.unwrap();
        mirror_of(&b).await.sync(Empty::default()).await.unwrap();
        mirror_of(&a).await.sync(Empty::default()).await.unwrap();
        assert_eq!(head(&path_a), mirror_head(&mirror));
        assert_eq!(head(&path_b), mirror_head(&mirror));
        // The second sync joined: two parents.
        let parents = git_out(&path_b, &["rev-list", "--parents", "-n", "1", "HEAD"]);
        assert_eq!(parents.split_whitespace().count(), 3, "{parents}");
        assert_eq!(
            git_out(&path_b, &["log", "-1", "--format=%s"]),
            "join the mirror's writes"
        );
        for vault in [&a, &b] {
            assert_eq!(read(vault, "from-a.md").await, "a");
            assert_eq!(read(vault, "from-b.md").await, "b");
            assert_eq!(read(vault, "a.md").await, "one");
        }
        assert_no_files(&path_a);
        assert_no_files(&path_b);
        git(&mirror, &["fsck", "--no-progress", "--strict"]);
    });
}

#[test]
fn both_vaults_without_a_work_tree_writing_one_document_differently_conflict_and_change_nothing() {
    both(|| async {
        let scratch = Scratch::new("none-conflict");
        let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
        let (path_a, a) = bare_clone_of(&scratch, "a", &mirror).await;
        let (path_b, b) = bare_clone_of(&scratch, "b", &mirror).await;
        put(&a, "a.md", "from a").await;
        put(&b, "a.md", "from b").await;
        mirror_of(&a).await.sync(Empty::default()).await.unwrap();
        let before = head(&path_b);
        let error = mirror_of(&b)
            .await
            .sync(Empty::default())
            .await
            .unwrap_err();
        assert!(error.is(Code::MirrorConflict), "{error}");
        assert!(error.to_string().contains("notes/a.md"), "{error}");
        assert_eq!(head(&path_b), before);
        assert_eq!(read(&b, "a.md").await, "from b");
        assert_eq!(mirror_head(&mirror), head(&path_a));
        assert!(!path_b.join(".git/MERGE_HEAD").exists());
        assert_no_files(&path_b);
    });
}

#[test]
fn a_clone_with_files_of_a_mirror_made_without_a_work_tree_works_and_the_reverse_too() {
    both(|| async {
        let scratch = Scratch::new("modes-cross");
        // Mirror made by a Vault without files; clone with files.
        let from_none = seeded_as(&scratch, "n", &[("a.md", "one")], none()).await;
        let (path, with) = clone_of(&scratch, "with", &from_none).await;
        assert_eq!(read(&with, "a.md").await, "one");
        assert_eq!(
            std::fs::read_to_string(path.join("notes/a.md")).unwrap(),
            "one"
        );
        assert_clean(&path);
        assert_eq!(git_out(&path, &["config", "--local", "core.bare"]), "false");
        put(&with, "w.md", "w").await;
        mirror_of(&with).await.sync(Empty::default()).await.unwrap();
        assert_clean(&path);
        // Mirror made by a Vault with files; clone without.
        let from_files = seeded(&scratch, "f", &[("a.md", "two")]).await;
        let (none_path, without) = bare_clone_of(&scratch, "without", &from_files).await;
        assert_eq!(read(&without, "a.md").await, "two");
        assert_no_files(&none_path);
        put(&without, "x.md", "x").await;
        mirror_of(&without)
            .await
            .sync(Empty::default())
            .await
            .unwrap();
        assert_eq!(mirror_head(&from_files), head(&none_path));
        assert_no_files(&none_path);
    });
}

// ---------------------------------------------------------------------
// `OpenOptions.initial_branch`.

/// Both modes of a Vault: with files and without.
const MODES: [WorkTree; 2] = [WorkTree::WORK_TREE_FILES, WorkTree::WORK_TREE_NONE];

/// The branch `HEAD` of the repository at `dir` names.
fn head_branch(dir: &Path) -> String {
    git_out(dir, &["symbolic-ref", "HEAD"])
}

#[test]
fn a_new_vault_starts_on_the_initial_branch_in_both_modes_and_unset_keeps_main() {
    both(|| async {
        for mode in MODES {
            let scratch = Scratch::new("initial-new");
            let set = scratch.join("set");
            open(
                &set,
                OpenOptions::default()
                    .with_work_tree(mode)
                    .with_initial_branch("trunk"),
            )
            .await
            .unwrap();
            assert_eq!(head_branch(&set), "refs/heads/trunk", "{mode:?}");
            assert_eq!(
                git_out(&set, &["rev-list", "--count", "refs/heads/trunk"]),
                "1",
                "{mode:?}: the marker is on the branch"
            );
            let unset = scratch.join("unset");
            open(&unset, OpenOptions::default().with_work_tree(mode))
                .await
                .unwrap();
            assert_eq!(head_branch(&unset), "refs/heads/main", "{mode:?}");
        }
    });
}

#[test]
fn a_clone_of_an_empty_mirror_starts_on_the_initial_branch_and_its_sync_pushes_it() {
    both(|| async {
        for mode in MODES {
            let scratch = Scratch::new("initial-empty");
            let mirror = bare(&scratch, "m.git");
            let path = scratch.join("v");
            let vault = open(
                &path,
                OpenOptions::default()
                    .with_mirror(file_url(&mirror))
                    .with_work_tree(mode)
                    .with_initial_branch("trunk"),
            )
            .await
            .unwrap();
            assert_eq!(head_branch(&path), "refs/heads/trunk", "{mode:?}");
            vault.create_collection(standard("notes")).await.unwrap();
            put(&vault, "a.md", "one").await;
            mirror_of(&vault)
                .await
                .sync(Empty::default())
                .await
                .unwrap();
            assert_eq!(
                git_out(&mirror, &["rev-parse", "refs/heads/trunk"]),
                head(&path),
                "{mode:?}"
            );
        }
    });
}

#[test]
fn a_mirror_that_has_the_branch_is_cloned_on_it_whatever_its_head_names() {
    both(|| async {
        for mode in MODES {
            let scratch = Scratch::new("initial-has");
            let mirror = seeded(&scratch, "m", &[("a.md", "main's")]).await;
            // `master` is one commit behind `main`, and the mirror's HEAD names it.
            git(&mirror, &["branch", "master", "main~1"]);
            git(&mirror, &["symbolic-ref", "HEAD", "refs/heads/master"]);
            let path = scratch.join("v");
            let vault = open(
                &path,
                OpenOptions::default()
                    .with_mirror(file_url(&mirror))
                    .with_work_tree(mode)
                    .with_initial_branch("main"),
            )
            .await
            .unwrap();
            assert_eq!(head_branch(&path), "refs/heads/main", "{mode:?}");
            assert_eq!(read(&vault, "a.md").await, "main's", "{mode:?}");
            if mode == WorkTree::WORK_TREE_FILES {
                assert_clean(&path);
            }
        }
    });
}

#[test]
fn a_mirror_with_commits_but_not_the_branch_is_invalid_argument_and_leaves_nothing() {
    both(|| async {
        for mode in MODES {
            let scratch = Scratch::new("initial-missing");
            let mirror = seeded(&scratch, "m", &[("a.md", "one")]).await;
            git(&mirror, &["branch", "-m", "main", "master"]);
            let path = scratch.join("v");
            let error = open(
                &path,
                OpenOptions::default()
                    .with_mirror(file_url(&mirror))
                    .with_work_tree(mode)
                    .with_initial_branch("main"),
            )
            .await
            .err()
            .unwrap();
            assert!(error.is(Code::InvalidArgument), "{mode:?}: {error}");
            assert!(!path.exists(), "{mode:?}");
            let left: Vec<_> = std::fs::read_dir(scratch.join("."))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.contains("scheda-clone"))
                .collect();
            assert!(left.is_empty(), "{mode:?}: {left:?}");
        }
    });
}

#[test]
fn an_attach_does_not_check_the_initial_branch() {
    both(|| async {
        for mode in MODES {
            let scratch = Scratch::new("initial-attach");
            let path = scratch.join("v");
            open(&path, OpenOptions::default().with_work_tree(mode))
                .await
                .unwrap();
            open(&path, OpenOptions::default().with_initial_branch("other"))
                .await
                .unwrap();
            assert_eq!(head_branch(&path), "refs/heads/main", "{mode:?}");
        }
    });
}

#[test]
fn two_clones_that_wrote_different_documents_into_one_folder_join_into_the_canonical_tree() {
    both(|| async {
        let scratch = Scratch::new("layout-join");
        let texts: Vec<(String, String)> = (0..63)
            .map(|n| (format!("n{n:03}.md"), format!("seed {n}")))
            .collect();
        let docs: Vec<(&str, &str)> = texts
            .iter()
            .map(|(id, text)| (id.as_str(), text.as_str()))
            .collect();
        let mirror = seeded_as(&scratch, "m", &docs, OpenOptions::default()).await;
        let (path_a, a) = bare_clone_of(&scratch, "a", &mirror).await;
        let (path_b, b) = bare_clone_of(&scratch, "b", &mirror).await;
        // `a` takes the folder to 65 Documents, so it splits; `b` takes it to
        // 64, flat, and removes one.
        put(&a, "n063.md", "a").await;
        put(&a, "n064.md", "a").await;
        put(&b, "n100.md", "b").await;
        collection(&b, "notes")
            .await
            .delete(
                scheda::v1::DocumentDeletion::default()
                    .with_id("n001.md")
                    .with_note("remove"),
            )
            .await
            .unwrap();
        mirror_of(&a).await.sync(Empty::default()).await.unwrap();
        mirror_of(&b).await.sync(Empty::default()).await.unwrap();
        mirror_of(&a).await.sync(Empty::default()).await.unwrap();
        assert_eq!(head(&path_a), mirror_head(&mirror));
        assert_eq!(head(&path_b), mirror_head(&mirror));
        let parents = git_out(&path_b, &["rev-list", "--parents", "-n", "1", "HEAD"]);
        assert_eq!(parents.split_whitespace().count(), 3, "a join: {parents}");
        let mut ids: std::collections::BTreeSet<String> =
            texts.iter().map(|(id, _)| id.clone()).collect();
        ids.remove("n001.md");
        ids.extend(["n063.md", "n064.md", "n100.md"].map(str::to_owned));
        let (tree_a, files) = crate::layout::physical(&path_a, "notes");
        assert_eq!(files, crate::layout::canonical(&ids));
        assert_eq!(crate::layout::physical(&path_b, "notes").0, tree_a);
        for vault in [&a, &b] {
            assert_eq!(read(vault, "n063.md").await, "a");
            assert_eq!(read(vault, "n100.md").await, "b");
            assert_eq!(read(vault, "n002.md").await, "seed 2");
            assert!(is_absent(vault, "n001.md").await);
        }
        git(&mirror, &["fsck", "--no-progress", "--strict"]);
    });
}
