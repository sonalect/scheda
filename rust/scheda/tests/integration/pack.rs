//! The object database after many appends: the loose objects are packed in
//! process, every line stays readable, and the database does not grow with
//! the square of the segment.

use std::path::Path;

use scheda::v1::document_selection::Selection;
use scheda::v1::{
    CollectionAsync, CollectionDeclaration, Document, DocumentCreation, DocumentSelection,
    DynCollectionAsync, OpenOptions, VaultAsync,
};
use serde_json::json;

use crate::common::{Scratch, all, both, collection, open};
use crate::probe::git_out;

/// A line of 200 bytes that does not compress and differs from every other.
fn line(i: u64) -> String {
    let mut x = (i + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..200)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'a' + (x % 26) as u8)
        })
        .collect()
}

/// The bytes of the files below `dir`.
fn size_of(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                size_of(&entry.path())
            } else {
                meta.len()
            }
        })
        .sum()
}

/// `count: n` and `packs: n` of `git count-objects -v` in `repo`.
fn counted(repo: &Path, key: &str) -> u64 {
    git_out(repo, &["count-objects", "-v"])
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}: ")))
        .unwrap()
        .parse()
        .unwrap()
}

async fn append_all(log: &DynCollectionAsync, n: u64) {
    for i in 0..n {
        log.create(
            DocumentCreation::default()
                .with_id(format!("id{i:04}"))
                .with_raw(line(i).into_bytes())
                .with_note("line"),
        )
        .await
        .unwrap();
    }
}

async fn assert_lines(log: &DynCollectionAsync, n: u64) {
    let selection = DocumentSelection {
        include_raw: Some(true),
        selection: Some(Selection::Listing(Box::default())),
        ..DocumentSelection::default()
    };
    let got: Vec<Document> = all(log.get_many(selection).await.unwrap()).await.unwrap();
    assert_eq!(got.len() as u64, n);
    for (i, document) in got.iter().enumerate() {
        assert_eq!(document.raw.as_deref(), Some(line(i as u64).as_bytes()));
    }
}

/// The repository at `repo` holds fewer loose objects than a packing
/// leaves behind (the count of 1024 objects, with the error of its sample),
/// some packs, passes `git fsck`, and its packs take a few times the
/// segment of `n` appends of 200 bytes, not the sum of its versions (the
/// square of the segment): a pack holds the segment once and the versions
/// after it as deltas of a line.
fn assert_packed(repo: &Path, n: u64) {
    let loose = counted(repo, "count");
    assert!(loose < 1400, "{loose} loose objects");
    assert!(counted(repo, "packs") >= 1);
    git_out(repo, &["fsck", "--full", "--strict"]);
    let packed = size_of(&repo.join(".git/objects/pack"));
    let segment = n * 200;
    assert!(
        packed < 8 * segment,
        "{packed} bytes in packs, segment {segment}"
    );
}

#[test]
fn four_hundred_appends_to_one_journal_are_packed_and_stay_readable() {
    both(|| async {
        let scratch = Scratch::new("pack-journal");
        let path = scratch.join("vault");
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        let declaration: CollectionDeclaration =
            serde_json::from_value(json!({"name": "log", "schema": {"journal": {}}})).unwrap();
        vault.create_collection(declaration).await.unwrap();
        let log = collection(&vault, "log").await.unwrap();
        append_all(&log, 400).await;
        assert_packed(&path, 400);
        assert_lines(&log, 400).await;
    });
}

#[test]
fn the_nested_repository_of_an_isolated_collection_is_packed_too() {
    both(|| async {
        let scratch = Scratch::new("pack-isolated");
        let path = scratch.join("vault");
        let vault = open(&path, OpenOptions::default()).await.unwrap();
        let declaration: CollectionDeclaration = serde_json::from_value(
            json!({"name": "log", "schema": {"journal": {}, "isolated": true}}),
        )
        .unwrap();
        vault.create_collection(declaration).await.unwrap();
        let log = collection(&vault, "log").await.unwrap();
        append_all(&log, 400).await;
        assert_packed(&path.join(".isolated"), 400);
        assert_lines(&log, 400).await;
    });
}
