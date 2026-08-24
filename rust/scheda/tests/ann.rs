//! USearch ANN smoke tests (IDX-7). Not Collection::search.

use std::fs;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{AnnIndex, Code};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-ann-{}-{n}", process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn cosine_neighbors_round_trip_through_dump() {
    let idx = AnnIndex::new(4).unwrap();
    idx.add(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    idx.add(2, &[0.0, 1.0, 0.0, 0.0]).unwrap();
    idx.add(3, &[0.9, 0.1, 0.0, 0.0]).unwrap();

    let hits = idx.search(&[1.0, 0.0, 0.0, 0.0], 2).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].0, 1);
    assert_eq!(hits[1].0, 3);
    assert!(hits[0].1 < hits[1].1);

    let scratch = Scratch::new();
    let path = scratch.root.join("vectors.usearch");
    idx.save(&path).unwrap();
    let loaded = AnnIndex::restore(&path).unwrap();
    assert_eq!(loaded.dimensions(), 4);
    assert_eq!(loaded.len(), 3);
    let again = loaded.search(&[1.0, 0.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(again[0].0, 1);
}

#[test]
fn zero_dimensions_is_invalid() {
    let err = AnnIndex::new(0).unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::Invalid));
}

#[test]
fn wrong_vector_length_is_invalid() {
    let idx = AnnIndex::new(3).unwrap();
    let err = idx.add(1, &[1.0, 0.0]).unwrap_err();
    assert_eq!(Code::of(&err), Some(Code::Invalid));
}
