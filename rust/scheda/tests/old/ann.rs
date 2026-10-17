//! USearch ANN smoke tests. Not CollectionHandle::search.

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use scheda::{AnnIndex, Code};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        common::isolate_test_git_env();
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        Self {
            root: common::scratch_root("scheda-ann", n),
        }
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
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn wrong_vector_length_is_invalid() {
    let idx = AnnIndex::new(3).unwrap();
    let err = idx.add(1, &[1.0, 0.0]).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[test]
fn get_round_trips_added_vector_and_none_for_absent() {
    let idx = AnnIndex::new(4).unwrap();
    idx.add(7, &[0.0, 1.0, 0.0, 0.0]).unwrap();
    let got = idx.get(7).unwrap().expect("stored vector");
    assert_eq!(got, vec![0.0, 1.0, 0.0, 0.0]);
    assert!(idx.get(99).unwrap().is_none());
}

fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom == 0.0 {
        1.0
    } else {
        1.0 - (dot / denom)
    }
}

fn exact_neighbors(vectors: &[(u64, Vec<f32>)], query: &[f32], k: usize) -> Vec<u64> {
    let mut ranked: Vec<(f32, u64)> = vectors
        .iter()
        .map(|(key, vector)| (cosine_distance(query, vector), *key))
        .collect();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    ranked.into_iter().take(k).map(|(_, key)| key).collect()
}

fn fixture_vectors(count: usize, dim: usize) -> Vec<(u64, Vec<f32>)> {
    (1..=count as u64)
        .map(|key| {
            let mut vector: Vec<f32> = (0..dim)
                .map(|axis| {
                    let n = key
                        .wrapping_mul(1_103_515_245)
                        .wrapping_add(axis as u64 + 1);
                    ((n % 1000) as f32) / 1000.0 + 0.001
                })
                .collect();
            let peak = ((key as usize).saturating_sub(1)) % dim;
            vector[peak] += 2.0;
            let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            for x in &mut vector {
                *x /= norm;
            }
            (key, vector)
        })
        .collect()
}

#[test]
fn search_n_is_prefix_of_search_2n_and_matches_brute_force() {
    let dim = 8;
    let vectors = fixture_vectors(80, dim);
    let idx = AnnIndex::new(dim).unwrap();
    for (key, vector) in &vectors {
        idx.add(*key, vector).unwrap();
    }
    let query = &vectors[0].1;
    let n = 8;
    let two_n = 16;
    let exact = exact_neighbors(&vectors, query, two_n);
    let shallow: Vec<u64> = idx
        .search(query, n)
        .unwrap()
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    let deep: Vec<u64> = idx
        .search(query, two_n)
        .unwrap()
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    assert_eq!(shallow, exact[..n]);
    assert_eq!(deep, exact);
    assert_eq!(shallow, deep[..n]);

    let scratch = Scratch::new();
    let path = scratch.root.join("prefix.usearch");
    idx.save(&path).unwrap();
    let loaded = AnnIndex::restore(&path).unwrap();
    let again: Vec<u64> = loaded
        .search(query, n)
        .unwrap()
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    assert_eq!(again, shallow);
}
