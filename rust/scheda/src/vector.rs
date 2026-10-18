//! The vector side of the Index: an HNSW index of the vectors of the
//! Documents, kept by the indexer, and the reader that searches open it
//! with.
//!
//! ```text
//! .scheda/index/vector/vectors   the model, the key map, and the HNSW index
//! .scheda/index/vector/state     the write of `vectors` the side stands on,
//!                                and the state of the Vault it reached
//! ```
//!
//! **What is embedded.** Every value of each `vector` column of each
//! Document of a declared collection that is not `encrypted` and not
//! `exclude_vector_index`: a text list gives one vector per element. The
//! lexical side projects each Document once per round and offers the texts
//! of its `vector` columns; this side moves them into one `Embed` stream.
//!
//! **Keys.** A vector is stored under a `u64` this side hands out. The key
//! map gives each key the collection, the id, the Vault-relative file, the
//! column, the place of the value in the column, and the digest of the
//! text, so a hit leads back to its Document, a changed or deleted Document
//! (or journal segment) drops exactly its own vectors, and a text that did
//! not change keeps its vector without a call to the embedder.
//!
//! **One file.** `vectors` holds all a search reads, and a round that
//! changes it writes it whole: a temporary file, then a rename, as every
//! file of the Index. Each write gives it a new stamp, so a reader knows
//! whether the vectors it holds are still those on disk. `state` names that
//! stamp and the state of the Vault the side reached; a round that changes
//! no vector writes `state` alone. A `state` that names another stamp, or
//! none, says that a round died between the two writes: the side is out of
//! step until a `Reindex`.
//!
//! **Model.** `vectors` records the model id and the dimensions the vectors
//! were made with. An embedder of another model never adds to them; a
//! rebuild makes the Index again with the model of the handle that asked.
//!
//! **A failure** of the embedder fails the round and leaves both files
//! alone. The vectors that came back before the failure are kept in memory,
//! keyed by the digest of their text, until a round succeeds, so the next
//! try sends only the texts that have none. A text the embedder refuses
//! (`REFUSED`) is remembered too: the next try leaves its Document out of
//! the vector side, as the lexical side leaves out a Document its
//! Projector refuses; its file stays.

// The searches of the `Vault` service read the Index through `Reader`.
#![cfg_attr(not(test), expect(dead_code, reason = "the search is not built yet"))]

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use scheda_api::v1::{EmbedderErrorCode, VaultErrorCode};
use serde::{Deserialize, Serialize};
use sha1::{Digest as _, Sha1};
use usearch::{IndexOptions, MetricKind, ScalarKind};

use crate::collection::Kind;
use crate::documents;
use crate::error::{self, Context, Error, Fault, OrFault, io};
use crate::index::{Index, Offer, Round, Side, State, rebuilding};
use crate::isolated;
use crate::lexical;
use crate::ports::{EmbedderPort, Installed};

/// The version of the layout of `vectors`.
const FORMAT: u32 = 1;

const VECTORS_FILE: &str = "vectors";
const STATE_FILE: &str = "state";

/// The bytes `vectors` starts with: the format, the stamp, and the length
/// of the model record.
const PREFIX: usize = 4 + 8 + 4;

fn broken(message: impl Into<String>) -> Fault {
    Fault::new(VaultErrorCode::IndexBroken, message)
}

fn too_short() -> Fault {
    broken("the vector index ends too soon")
}

/// The digest of a text: what a kept vector is keyed by.
type Digest = [u8; 20];

fn digest(text: &str) -> Digest {
    Sha1::digest(text.as_bytes()).into()
}

/// A digest in JSON: forty hexadecimal digits.
mod hex_digest {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    use super::Digest;

    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    pub(super) fn serialize<S: Serializer>(
        digest: &Digest,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut text = String::with_capacity(2 * digest.len());
        for byte in digest {
            text.push(char::from(DIGITS[usize::from(byte >> 4)]));
            text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        serializer.serialize_str(&text)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Digest, D::Error> {
        let text = String::deserialize(deserializer)?;
        let mut digest = Digest::default();
        if text.len() != 2 * digest.len() {
            return Err(D::Error::custom("a digest has forty hexadecimal digits"));
        }
        let (pairs, _) = text.as_bytes().as_chunks::<2>();
        for (byte, pair) in digest.iter_mut().zip(pairs) {
            let pair = std::str::from_utf8(pair).map_err(D::Error::custom)?;
            *byte = u8::from_str_radix(pair, 16).map_err(D::Error::custom)?;
        }
        Ok(digest)
    }
}

/// The model and the length of the vectors of the Index.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Recorded {
    /// The model that made the vectors.
    pub(crate) model_id: String,
    /// The length of each vector.
    pub(crate) dimensions: usize,
}

impl Recorded {
    /// Whether `embedder` is of the model of these vectors, with their
    /// length.
    pub(crate) fn is(&self, embedder: &EmbedderPort) -> bool {
        self.model_id == embedder.model_id() && self.dimensions == embedder.dimensions()
    }
}

/// The start of `vectors`: which write it is, and the model of its vectors.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Header {
    /// Changes with every write of the file.
    stamp: u64,
    recorded: Recorded,
}

fn vectors_file(index: &Index) -> PathBuf {
    index.vector_dir().join(VECTORS_FILE)
}

fn state_file(index: &Index) -> PathBuf {
    index.vector_dir().join(STATE_FILE)
}

/// A field of `N` bytes at the front of `bytes`, and the bytes after it.
fn field<const N: usize>(bytes: &[u8]) -> Result<(&[u8; N], &[u8]), Fault> {
    bytes.split_first_chunk::<N>().ok_or_else(too_short)
}

/// The first `length` bytes of `bytes`, and the bytes after them.
fn part(bytes: &[u8], length: u64) -> Result<(&[u8], &[u8]), Fault> {
    let length = usize::try_from(length).map_err(|_| too_short())?;
    bytes.split_at_checked(length).ok_or_else(too_short)
}

/// The stamp and the length of the model record, from the prefix of
/// `vectors`; and the bytes after the prefix.
fn parse_prefix(bytes: &[u8]) -> Result<(u64, u64, &[u8]), Fault> {
    let (format, rest) = field::<4>(bytes)?;
    if u32::from_le_bytes(*format) != FORMAT {
        return Err(broken("the vector index is of another format"));
    }
    let (stamp, rest) = field::<8>(rest)?;
    let (record, rest) = field::<4>(rest)?;
    Ok((
        u64::from_le_bytes(*stamp),
        u64::from(u32::from_le_bytes(*record)),
        rest,
    ))
}

fn parse_recorded(bytes: &[u8]) -> Result<Recorded, Fault> {
    serde_json::from_slice(bytes)
        .or_fault(VaultErrorCode::IndexBroken, "the model record is damaged")
}

/// The header of `vectors`, read without the vectors; `None` when the
/// Index has no vector side.
fn read_header(index: &Index) -> Result<Option<Header>, Error> {
    let mut file = match fs::File::open(vectors_file(index)) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io(err)).context("open the vector index"),
    };
    let mut prefix = [0u8; PREFIX];
    read_exact(&mut file, &mut prefix)?;
    let (stamp, length, _) = parse_prefix(&prefix)?;
    let mut record = vec![0u8; usize::try_from(length).map_err(|_| too_short())?];
    read_exact(&mut file, &mut record)?;
    Ok(Some(Header {
        stamp,
        recorded: parse_recorded(&record)?,
    }))
}

fn read_exact(file: &mut fs::File, bytes: &mut [u8]) -> Result<(), Error> {
    file.read_exact(bytes).map_err(|err| {
        if err.kind() == std::io::ErrorKind::UnexpectedEof {
            too_short().with_source(err).into()
        } else {
            Error::from(io(err)).context("read the vector index")
        }
    })
}

/// The model and the length of the vectors the Index holds; `None` when it
/// holds none yet. Reads the header of `vectors` only.
///
/// # Errors
///
/// `INDEX_BROKEN` when the record cannot be read.
pub(crate) fn recorded(index: &Index) -> Result<Option<Recorded>, Error> {
    Ok(read_header(index)?.map(|header| header.recorded))
}

/// What `state` says: the write of `vectors` the side stands on, and the
/// state of the Vault it reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Reached {
    stamp: u64,
    state: State,
}

impl Reached {
    fn line(self) -> String {
        format!("{} {}\n", self.stamp, self.state.line())
    }

    fn parse(text: &str) -> Option<Reached> {
        let (stamp, state) = text.trim().split_once(' ')?;
        Some(Reached {
            stamp: stamp.parse().ok()?,
            state: State::parse(state)?,
        })
    }
}

/// `state`; `None` when there is none.
fn read_reached(index: &Index) -> Result<Option<Reached>, Error> {
    match fs::read_to_string(state_file(index)) {
        Ok(text) => Reached::parse(&text)
            .map(Some)
            .ok_or_else(|| broken("the state of the vector index says nothing").into()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io(err)).context("read the state of the vectors"),
    }
}

/// The stamp of a new write of `vectors`: never one a reader can hold for an
/// earlier write.
fn next_stamp(previous: Option<u64>) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
        });
    previous
        .map_or(now, |previous| now.max(previous.saturating_add(1)))
        .max(1)
}

/// The HNSW index: cosine distance over `f32`.
struct Ann {
    inner: usearch::Index,
}

impl std::fmt::Debug for Ann {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ann")
            .field("dimensions", &self.inner.dimensions())
            .field("len", &self.inner.size())
            .finish()
    }
}

impl Ann {
    const CONNECTIVITY: usize = 16;
    const EXPANSION_ADD: usize = 128;
    /// A search walks at least this wide and is cut to `k`, so a deeper
    /// request does not reshuffle the head of a shallower one.
    const EXPANSION_SEARCH: usize = 512;

    fn new(dimensions: usize) -> Result<Ann, Fault> {
        let options = IndexOptions {
            dimensions,
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            connectivity: Self::CONNECTIVITY,
            expansion_add: Self::EXPANSION_ADD,
            expansion_search: Self::EXPANSION_SEARCH,
            multi: false,
        };
        let inner = usearch::Index::new(&options)
            .or_fault(VaultErrorCode::Storage, "create the vector index")?;
        let ann = Ann { inner };
        ann.apply_expansion();
        Ok(ann)
    }

    /// The index `bytes` hold, copied out of them.
    fn from_bytes(bytes: &[u8]) -> Result<Ann, Fault> {
        let inner = usearch::Index::restore_from_buffer(bytes)
            .or_fault(VaultErrorCode::IndexBroken, "open the vector index")?;
        let ann = Ann { inner };
        ann.apply_expansion();
        Ok(ann)
    }

    fn apply_expansion(&self) {
        self.inner.change_expansion_add(Self::EXPANSION_ADD);
        self.inner.change_expansion_search(Self::EXPANSION_SEARCH);
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    fn len(&self) -> usize {
        self.inner.size()
    }

    /// Makes room for `more` vectors beyond those held.
    fn reserve(&self, more: usize) -> Result<(), Fault> {
        let wanted = self.inner.size().saturating_add(more);
        if wanted <= self.inner.capacity() {
            return Ok(());
        }
        self.inner
            .reserve(wanted)
            .or_fault(VaultErrorCode::Storage, "make room in the vector index")
    }

    fn add(&self, key: u64, vector: &[f32]) -> Result<(), Fault> {
        self.inner
            .add(key, vector)
            .or_fault(VaultErrorCode::Storage, "add a vector to the vector index")
    }

    fn remove(&self, key: u64) -> Result<(), Fault> {
        if !self.inner.contains(key) {
            return Ok(());
        }
        self.inner.remove(key).map(|_| ()).or_fault(
            VaultErrorCode::IndexBroken,
            "remove a vector from the vector index",
        )
    }

    fn get(&self, key: u64) -> Result<Option<Vec<f32>>, Fault> {
        if !self.inner.contains(key) {
            return Ok(None);
        }
        let mut vector = Vec::new();
        let found = self.inner.export(key, &mut vector).or_fault(
            VaultErrorCode::IndexBroken,
            "read a vector of the vector index",
        )?;
        vector.truncate(self.inner.dimensions());
        Ok((found > 0).then_some(vector))
    }

    /// The `k` nearest keys with their cosine distances, nearest first.
    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(u64, f32)>, Fault> {
        if k == 0 || self.inner.size() == 0 {
            return Ok(Vec::new());
        }
        let walk = k.max(Self::EXPANSION_SEARCH);
        let hits = self
            .inner
            .search(query, walk)
            .or_fault(VaultErrorCode::IndexBroken, "search the vector index")?;
        Ok(hits.keys.into_iter().zip(hits.distances).take(k).collect())
    }

    /// Appends the index to `bytes`, with its length before it.
    fn append_to(&self, bytes: &mut Vec<u8>) -> Result<(), Fault> {
        let length = self.inner.serialized_length();
        bytes.extend_from_slice(&(length as u64).to_le_bytes());
        let start = bytes.len();
        bytes.resize(start + length, 0);
        self.inner
            .save_to_buffer(&mut bytes[start..])
            .or_fault(VaultErrorCode::Storage, "write the vector index")
    }
}

/// What a vector is a vector of.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Slot {
    /// The collection of the Document.
    pub(crate) collection: String,
    /// The id of the Document (of the line, for a journal).
    pub(crate) id: String,
    /// The Vault-relative file the Document came from.
    pub(crate) file: String,
    /// The `vector` column the value is in.
    pub(crate) column: String,
    /// The place of the value among those of the column.
    pub(crate) at: u32,
    /// The digest of the text the vector was made of.
    #[serde(with = "hex_digest")]
    pub(crate) digest: Digest,
}

/// One entry of the key map.
#[derive(Debug, Deserialize, Serialize)]
struct Row {
    key: u64,
    #[serde(flatten)]
    slot: Slot,
}

/// The key map as `vectors` holds it.
#[derive(Debug, Deserialize, Serialize)]
struct KeysFile {
    next: u64,
    slots: Vec<Row>,
}

/// The key of the Document `id` of `collection`, as `by_doc` has it.
fn doc_key(collection: &str, id: &str) -> String {
    lexical::key_of(collection, id)
}

/// The keys of the vectors, and what each stands for.
#[derive(Debug, Default)]
struct Keys {
    next: u64,
    slots: BTreeMap<u64, Slot>,
    by_doc: HashMap<String, Vec<u64>>,
    by_file: HashMap<String, Vec<u64>>,
}

impl Keys {
    fn new() -> Keys {
        Keys {
            next: 1,
            ..Keys::default()
        }
    }

    fn parse(bytes: &[u8]) -> Result<Keys, Fault> {
        let file: KeysFile = serde_json::from_slice(bytes)
            .or_fault(VaultErrorCode::IndexBroken, "the vector keys are damaged")?;
        let mut keys = Keys::new();
        for row in file.slots {
            keys.put(row.key, row.slot);
        }
        keys.next = keys
            .next
            .max(file.next)
            .max(keys.slots.keys().next_back().map_or(1, |last| last + 1));
        Ok(keys)
    }

    fn encode(&self) -> Result<Vec<u8>, Fault> {
        let file = KeysFile {
            next: self.next,
            slots: self
                .slots
                .iter()
                .map(|(key, slot)| Row {
                    key: *key,
                    slot: slot.clone(),
                })
                .collect(),
        };
        serde_json::to_vec(&file).or_fault(VaultErrorCode::Storage, "write the vector keys")
    }

    fn put(&mut self, key: u64, slot: Slot) {
        self.by_doc
            .entry(doc_key(&slot.collection, &slot.id))
            .or_default()
            .push(key);
        self.by_file.entry(slot.file.clone()).or_default().push(key);
        self.slots.insert(key, slot);
        self.next = self.next.max(key.saturating_add(1));
    }

    /// Gives `slot` the next key.
    fn insert(&mut self, slot: Slot) -> u64 {
        let key = self.next;
        self.put(key, slot);
        key
    }

    fn remove(&mut self, key: u64) {
        let Some(slot) = self.slots.remove(&key) else {
            return;
        };
        let doc = doc_key(&slot.collection, &slot.id);
        forget(&mut self.by_doc, &doc, key);
        forget(&mut self.by_file, &slot.file, key);
    }

    fn of_doc(&self, collection: &str, id: &str) -> Vec<u64> {
        self.by_doc
            .get(&doc_key(collection, id))
            .cloned()
            .unwrap_or_default()
    }

    fn of_file(&self, file: &str) -> Vec<u64> {
        self.by_file.get(file).cloned().unwrap_or_default()
    }

    fn of_collection(&self, collection: &str) -> Vec<u64> {
        self.slots
            .iter()
            .filter(|(_, slot)| slot.collection == collection)
            .map(|(key, _)| *key)
            .collect()
    }
}

fn forget(map: &mut HashMap<String, Vec<u64>>, name: &str, key: u64) {
    if let Some(keys) = map.get_mut(name) {
        keys.retain(|have| *have != key);
        if keys.is_empty() {
            map.remove(name);
        }
    }
}

/// The vector side in memory: what `vectors` holds.
struct Stored {
    header: Header,
    ann: Ann,
    keys: Keys,
}

impl Stored {
    /// An empty side for the vectors of `embedder`.
    fn new(embedder: &EmbedderPort) -> Result<Stored, Error> {
        Ok(Stored {
            header: Header {
                stamp: 0,
                recorded: Recorded {
                    model_id: embedder.model_id().to_owned(),
                    dimensions: embedder.dimensions(),
                },
            },
            ann: Ann::new(embedder.dimensions()).context("create the vectors")?,
            keys: Keys::new(),
        })
    }

    /// `vectors` as bytes: the prefix, the model record, the key map with its
    /// length before it, and the HNSW index with its length before it.
    fn encode(&self) -> Result<Vec<u8>, Error> {
        let record = serde_json::to_vec(&self.header.recorded)
            .or_fault(VaultErrorCode::Storage, "write the model record")?;
        let record_length = u32::try_from(record.len())
            .map_err(|_| Fault::new(VaultErrorCode::Storage, "the model record is too long"))?;
        let keys = self.keys.encode()?;
        let mut bytes = Vec::with_capacity(
            PREFIX + record.len() + 8 + keys.len() + 8 + self.ann.inner.serialized_length(),
        );
        bytes.extend_from_slice(&FORMAT.to_le_bytes());
        bytes.extend_from_slice(&self.header.stamp.to_le_bytes());
        bytes.extend_from_slice(&record_length.to_le_bytes());
        bytes.extend_from_slice(&record);
        bytes.extend_from_slice(&(keys.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&keys);
        self.ann.append_to(&mut bytes)?;
        Ok(bytes)
    }
}

/// Reads `vectors` whole; `None` when the Index has no vector side.
fn load(index: &Index) -> Result<Option<Stored>, Error> {
    let bytes = match fs::read(vectors_file(index)) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io(err)).context("read the vector index"),
    };
    let (stamp, length, rest) = parse_prefix(&bytes)?;
    let (record, rest) = part(rest, length)?;
    let recorded = parse_recorded(record)?;
    let (length, rest) = field::<8>(rest)?;
    let (keys, rest) = part(rest, u64::from_le_bytes(*length))?;
    let keys = Keys::parse(keys)?;
    let (length, rest) = field::<8>(rest)?;
    if rest.len() as u64 != u64::from_le_bytes(*length) {
        return Err(broken("the vector index is not of the length it records").into());
    }
    let ann = Ann::from_bytes(rest)?;
    if ann.dimensions() != recorded.dimensions {
        return Err(broken("the vector index and its model record disagree").into());
    }
    if keys.slots.len() != ann.len() {
        return Err(broken("the vector keys and the vector index disagree").into());
    }
    Ok(Some(Stored {
        header: Header { stamp, recorded },
        ann,
        keys,
    }))
}

/// Writes the side at `to`: `vectors` with a new stamp when `changed`, then
/// `state`.
fn write(
    index: &Index,
    stored: &mut Stored,
    previous: Option<u64>,
    changed: bool,
    to: State,
) -> Result<(), Error> {
    if changed {
        stored.header.stamp = next_stamp(previous);
        index
            .write(&vectors_file(index), &stored.encode()?)
            .context("write the vectors")?;
    }
    let reached = Reached {
        stamp: stored.header.stamp,
        state: to,
    };
    index
        .write(&state_file(index), reached.line().as_bytes())
        .context("record the state of the vectors")
}

/// The collections the vector side takes, and those it leaves alone.
struct Targets {
    /// The collections with a `vector` column that are not stale.
    names: BTreeSet<String>,
    /// The collections whose columns changed since the Index was built.
    stale: BTreeSet<String>,
}

fn targets(round: &Round<'_>) -> Result<Targets, Error> {
    let index = round.index();
    let stale = lexical::read_stale(index)?;
    let dump = lexical::read_dump(index).context("read the recorded columns")?;
    let names = round
        .declarations()
        .iter()
        .filter(|decl| decl.vector_indexed() && !stale.contains(decl.name()))
        .filter(|decl| {
            dump.as_ref()
                .and_then(|dump| dump.columns(decl.name()))
                .is_some_and(|columns| columns.iter().any(|column| column.vector))
        })
        .map(|decl| decl.name().to_owned())
        .collect();
    Ok(Targets { names, stale })
}

fn out_of_step() -> Error {
    broken("the vector index is not at the state the Index reflects; run Reindex").into()
}

fn precondition(message: &'static str) -> Error {
    Fault::new(VaultErrorCode::FailedPrecondition, message).into()
}

/// What this handle keeps between two tries of a round.
#[derive(Default)]
struct Cache {
    /// The model of `vectors`: another model makes them useless.
    model: String,
    /// The vectors at hand for the texts of the round, by the digest of the
    /// text: those the embedder gave back, and those of the texts the Index
    /// holds already.
    vectors: HashMap<Digest, Vec<f32>>,
    /// The texts the embedder refused.
    refused: HashSet<Digest>,
}

impl Cache {
    /// Empties the cache when it holds what another model made.
    fn keep_for(&mut self, model: &str) {
        if self.model != model {
            *self = Cache {
                model: model.to_owned(),
                ..Cache::default()
            };
        }
    }
}

/// The vector side of the Index.
pub(crate) struct Vector {
    ports: Arc<Installed>,
    cache: Cache,
}

impl Vector {
    /// The side of a handle that holds `ports`.
    pub(crate) fn new(ports: Arc<Installed>) -> Vector {
        Vector {
            ports,
            cache: Cache::default(),
        }
    }

    /// Takes into the cache the stored vectors of the texts in `digests`
    /// that the Index holds already, so they are not embedded again: a
    /// round that walks every Document (the lexical side ahead of the Index
    /// after a failed round, say) sends only the texts that changed.
    fn reuse(&mut self, stored: &Stored, digests: &[Vec<Digest>]) -> Result<(), Error> {
        let wanted: HashSet<&Digest> = digests
            .iter()
            .flatten()
            .filter(|d| !self.cache.vectors.contains_key(*d))
            .collect();
        if wanted.is_empty() {
            return Ok(());
        }
        for (key, slot) in &stored.keys.slots {
            if wanted.contains(&slot.digest)
                && !self.cache.vectors.contains_key(&slot.digest)
                && let Some(vector) = stored.ann.get(*key)?
            {
                self.cache.vectors.insert(slot.digest, vector);
            }
        }
        Ok(())
    }

    /// The vectors of the texts of `offers` that the cache lacks, from the
    /// embedder. The texts are moved into the call.
    fn embed(
        &mut self,
        embedder: &EmbedderPort,
        offers: &mut [Offer],
        digests: &[Vec<Digest>],
    ) -> Result<(), Error> {
        let mut seen = HashSet::new();
        let mut sending = Vec::new();
        let mut texts = Vec::new();
        for (offer, digests) in offers.iter_mut().zip(digests) {
            if digests.iter().any(|d| self.cache.refused.contains(d)) {
                continue;
            }
            for (piece, d) in offer.pieces.iter_mut().zip(digests) {
                if self.cache.vectors.contains_key(d) || !seen.insert(*d) {
                    continue;
                }
                sending.push(*d);
                texts.push(std::mem::take(&mut piece.text));
            }
        }
        if texts.is_empty() {
            return Ok(());
        }
        let embedded = embedder.embed(texts);
        let kept = embedded.vectors.len();
        for (d, vector) in sending.iter().zip(embedded.vectors) {
            self.cache.vectors.insert(*d, vector);
        }
        match embedded.failure {
            None => Ok(()),
            Some(error) => {
                // The text after the last vector that came is the one the
                // embedder stopped at.
                if error.is(EmbedderErrorCode::Refused)
                    && let Some(d) = sending.get(kept)
                {
                    self.cache.refused.insert(*d);
                }
                Err(error)
            }
        }
    }
}

impl Side for Vector {
    fn apply(&mut self, round: &mut Round<'_>) -> Result<(), Error> {
        let index = round.index();
        let to = round.to();
        let targets = targets(round)?;
        let header = read_header(index).context("read the vectors")?;
        if header.is_none() && targets.names.is_empty() {
            let _ = round.take_offers();
            return Ok(());
        }
        // Where the side stands: `state` counts only when it names the write
        // of `vectors` on disk.
        let at = match (&header, read_reached(index)?) {
            (Some(header), Some(reached)) if reached.stamp == header.stamp => Some(reached.state),
            _ => None,
        };
        if at == Some(to) {
            // Reached by an earlier try of this round.
            self.cache.vectors.clear();
            let _ = round.take_offers();
            return Ok(());
        }
        let embedder = self.ports.embedder().context("read the embedder")?;
        if !targets.names.is_empty() && embedder.is_none() {
            return Err(precondition(
                "the Index needs an embedder for the vectors, and this handle has none",
            ));
        }

        let incremental = match (&header, at) {
            (Some(_), Some(at)) if !round.is_rebuild() && round.from() == Some(at) => true,
            (Some(_), _) => return Err(out_of_step()),
            (None, _) => {
                if !(round.is_rebuild() || targets.names.iter().all(|name| round.is_whole(name))) {
                    return Err(out_of_step());
                }
                false
            }
        };
        let previous = header.as_ref().map(|header| header.stamp);
        let mut stored = if incremental {
            load(index)
                .context("read the vectors")?
                .ok_or_else(out_of_step)?
        } else {
            let Some(embedder) = embedder.as_deref() else {
                return Err(precondition("an Index of vectors is made with an embedder"));
            };
            Stored::new(embedder)?
        };
        if let Some(embedder) = embedder.as_deref() {
            if !stored.header.recorded.is(embedder) {
                return Err(precondition(
                    "the embedder is of another model than the vectors of the Index",
                ));
            }
            self.cache.keep_for(embedder.model_id());
        }

        let mut gone: BTreeSet<u64> = BTreeSet::new();
        if incremental {
            for (key, slot) in &stored.keys.slots {
                if !targets.names.contains(&slot.collection)
                    && !targets.stale.contains(&slot.collection)
                {
                    gone.insert(*key);
                }
            }
            for name in &targets.names {
                let Some(decl) = round.declarations().get(name) else {
                    continue;
                };
                if round.is_whole(name) {
                    gone.extend(stored.keys.of_collection(name));
                    continue;
                }
                let dir = isolated::vault_rel(decl, "");
                for delta in round.deltas() {
                    let Some(id) = delta.path.strip_prefix(dir.as_str()) else {
                        continue;
                    };
                    match decl.kind() {
                        Kind::Standard | Kind::Singleton => {
                            if documents::path_of(decl, id).is_ok() {
                                gone.extend(stored.keys.of_doc(name, id));
                            }
                        }
                        Kind::Journal => gone.extend(stored.keys.of_file(&delta.path)),
                    }
                }
            }
        }

        let mut offers = match round.take_offers() {
            Some(offers) => offers,
            None if targets.names.is_empty() => Vec::new(),
            None => {
                return Err(precondition(
                    "the lexical side offered no texts to the vector side",
                ));
            }
        };
        offers.retain(|offer| targets.names.contains(&offer.collection));
        if incremental {
            for offer in &offers {
                gone.extend(stored.keys.of_doc(&offer.collection, &offer.id));
            }
        }
        let digests: Vec<Vec<Digest>> = offers
            .iter()
            .map(|offer| {
                offer
                    .pieces
                    .iter()
                    .map(|piece| digest(&piece.text))
                    .collect()
            })
            .collect();
        if incremental {
            self.reuse(&stored, &digests)?;
        }
        round.note(0, 0, 0)?;
        if let Some(embedder) = embedder.as_deref() {
            self.embed(embedder, &mut offers, &digests)?;
        }
        round.note(0, 0, 0)?;

        let changed = !incremental || !gone.is_empty() || !offers.is_empty();
        for key in &gone {
            stored.ann.remove(*key)?;
            stored.keys.remove(*key);
        }
        let more: usize = offers.iter().map(|offer| offer.pieces.len()).sum();
        stored.ann.reserve(more)?;
        let mut skipped = 0;
        for (offer, digests) in offers.into_iter().zip(&digests) {
            if digests.iter().any(|d| self.cache.refused.contains(d)) {
                skipped += 1;
                continue;
            }
            for (piece, d) in offer.pieces.into_iter().zip(digests) {
                let Some(vector) = self.cache.vectors.get(d) else {
                    return Err(broken("the embedder gave no vector for a text").into());
                };
                let key = stored.keys.insert(Slot {
                    collection: offer.collection.clone(),
                    id: offer.id.clone(),
                    file: offer.file.clone(),
                    column: piece.column,
                    at: piece.at,
                    digest: *d,
                });
                stored.ann.add(key, vector)?;
            }
        }
        if skipped > 0 {
            round.note(0, skipped, 0)?;
        }

        write(index, &mut stored, previous, changed, to)?;
        self.cache.vectors.clear();
        Ok(())
    }
}

/// What a search can say about its vector leg.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Leg {
    /// The handle has no embedder.
    NoEmbedder,
    /// The Index holds no vector yet.
    NoVectors,
    /// The embedder is of another model or length than the vectors.
    ModelMismatch,
    /// The leg can run.
    Ran,
}

/// The vectors of one write of `vectors`, for a search: the HNSW index and
/// the keys, kept whatever the indexer does meanwhile.
#[derive(Debug)]
pub(crate) struct Snapshot {
    stamp: u64,
    recorded: Recorded,
    ann: Ann,
    keys: Keys,
}

impl Snapshot {
    fn of(stored: Stored) -> Snapshot {
        Snapshot {
            stamp: stored.header.stamp,
            recorded: stored.header.recorded,
            ann: stored.ann,
            keys: stored.keys,
        }
    }

    /// The model and the length of the vectors.
    pub(crate) fn recorded(&self) -> &Recorded {
        &self.recorded
    }

    /// How many vectors there are.
    pub(crate) fn len(&self) -> usize {
        self.ann.len()
    }

    /// Whether there is no vector.
    pub(crate) fn is_empty(&self) -> bool {
        self.ann.len() == 0
    }

    /// The `k` vectors nearest to `query`, nearest first, with their cosine
    /// distances (lower is closer).
    ///
    /// # Errors
    ///
    /// `INVALID_ARGUMENT` when `query` is not of the length of the vectors;
    /// `INDEX_BROKEN` when the index cannot be searched.
    pub(crate) fn search(&self, query: &[f32], k: usize) -> Result<Vec<(&Slot, f32)>, Error> {
        if query.len() != self.recorded.dimensions {
            return Err(Fault::new(
                VaultErrorCode::InvalidArgument,
                format!(
                    "the query vector has {} values, and the vectors of the Index have {}",
                    query.len(),
                    self.recorded.dimensions
                ),
            )
            .into());
        }
        Ok(self
            .ann
            .search(query, k)?
            .into_iter()
            .filter_map(|(key, distance)| self.keys.slots.get(&key).map(|slot| (slot, distance)))
            .collect())
    }

    /// The vectors stored for the value places of the Document `id` of
    /// `collection`, in the order of their keys.
    ///
    /// # Errors
    ///
    /// `INDEX_BROKEN` when a vector cannot be read.
    pub(crate) fn vectors_of(&self, collection: &str, id: &str) -> Result<Vec<Vec<f32>>, Error> {
        let mut found = Vec::new();
        for key in self.keys.of_doc(collection, id) {
            if let Some(vector) = self.ann.get(key)? {
                found.push(vector);
            }
        }
        Ok(found)
    }

    /// Whether a vector of the Document `id` of `collection` is there.
    pub(crate) fn has(&self, collection: &str, id: &str) -> bool {
        !self.keys.of_doc(collection, id).is_empty()
    }
}

/// The reader of the vector index of one handle. It reads `vectors` whole
/// and keeps that snapshot until a round writes the file again.
#[derive(Debug)]
pub(crate) struct Reader {
    index: Index,
    cache: Mutex<Option<Arc<Snapshot>>>,
}

impl Reader {
    /// The reader of `index`. Nothing is read.
    pub(crate) fn new(index: Index) -> Reader {
        Reader {
            index,
            cache: Mutex::new(None),
        }
    }

    /// The vectors the Index holds now; `None` when it holds no vector side
    /// (no collection has a `vector` column). A snapshot obtained earlier
    /// keeps its own vectors.
    ///
    /// # Errors
    ///
    /// `INDEX_REBUILDING` (retryable) when the Index has no commit, because
    /// no round has run yet or a rebuild runs, and when it is of another
    /// format; `INDEX_STALE` when the columns of the Projector changed since
    /// the Index was built; `INDEX_BROKEN` when the vector index cannot be
    /// read (`Reindex`).
    pub(crate) fn snapshot(&self) -> Result<Option<Arc<Snapshot>>, Error> {
        let committed = self.index.readable()?;
        let mut cache = error::mutex(&self.cache, "read the vector snapshot")?;
        let header = match read_header(&self.index) {
            Ok(Some(header)) => header,
            Ok(None) => {
                *cache = None;
                return Ok(None);
            }
            Err(error) => {
                *cache = None;
                return Err(self.failed(committed, error)?);
            }
        };
        if let Some(snapshot) = cache.as_ref()
            && snapshot.stamp == header.stamp
        {
            return Ok(Some(Arc::clone(snapshot)));
        }
        *cache = None;
        match load(&self.index) {
            Ok(Some(stored)) => {
                let snapshot = Arc::new(Snapshot::of(stored));
                *cache = Some(Arc::clone(&snapshot));
                Ok(Some(snapshot))
            }
            // Only a rebuild removes the file, and it removes the commit
            // first.
            Ok(None) => Err(rebuilding("the Index is being rebuilt")),
            Err(error) => Err(self.failed(committed, error)?),
        }
    }

    /// The failure of a read of the Index at `committed`: `error` itself,
    /// unless a rebuild emptied the Index under the read.
    fn failed(&self, committed: State, error: Error) -> Result<Error, Error> {
        Ok(
            if self.index.read_commit()? == Some(committed) && self.index.has_format()? {
                error
            } else {
                rebuilding("the Index is being rebuilt")
            },
        )
    }

    /// Whether the vector leg of a search can run with `embedder`, and if
    /// not, why.
    ///
    /// # Errors
    ///
    /// As [`Reader::snapshot`], except that a handle without an embedder is
    /// told so without reading the Index.
    pub(crate) fn leg(&self, embedder: Option<&EmbedderPort>) -> Result<Leg, Error> {
        let Some(embedder) = embedder else {
            return Ok(Leg::NoEmbedder);
        };
        Ok(match self.snapshot()? {
            None => Leg::NoVectors,
            Some(snapshot) if snapshot.is_empty() => Leg::NoVectors,
            Some(snapshot) if !snapshot.recorded().is(embedder) => Leg::ModelMismatch,
            Some(_) => Leg::Ran,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use buffa_types::google::protobuf::Empty;
    use protocontract::{BoxIter, Failure};
    use scheda_api::v1::{
        Collection, CollectionKey, ColumnKind, Document, DocumentCreation, DocumentDeletion,
        DocumentKey, DocumentUpsert, EmbedderDescriptor, EmbedderErrorCode, EmbedderSync,
        IndexColumn, IndexDoc, IndexSchema, IndexState, IndexValue, OpenOptions, ProjectorSync,
        Query, RawDocument, Text, VaultErrorCode, Vector, index_value, rebuild,
    };

    use super::{Leg, Slot, Snapshot, digest, read_reached, recorded};
    use crate::Vault;
    use crate::error::Error;
    use crate::index::{Index, State};
    use crate::ports::{FixedEmbedder, Port, Ports};
    use crate::repo::Repos;

    const DEADLINE: Duration = Duration::from_secs(30);
    const DIMS: usize = 8;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-vector-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A vector made of the text alone: the same text gives the same vector,
    /// and two different texts give vectors that are not parallel.
    fn vector_of(text: &str) -> Vec<f32> {
        let mut values = vec![0.5f32; DIMS];
        for (at, byte) in text.bytes().enumerate() {
            values[(at * 7 + usize::from(byte)) % DIMS] += f32::from(byte) + at as f32;
        }
        values
    }

    /// What a test tells its embedder, and what the embedder tells back.
    struct Embedded {
        model: &'static str,
        /// The texts of each `Embed` call, in order.
        calls: Mutex<Vec<Vec<String>>>,
        /// The next call fails at this place in its stream; `true`: refused.
        fail: Mutex<Option<(usize, bool)>>,
        /// Every call fails at once, as a model that does not answer.
        down: Mutex<bool>,
    }

    impl Embedded {
        fn new(model: &'static str) -> Arc<Embedded> {
            Arc::new(Embedded {
                model,
                calls: Mutex::new(Vec::new()),
                fail: Mutex::new(None),
                down: Mutex::new(false),
            })
        }

        fn texts(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }

        fn fail_at(&self, at: usize, refused: bool) {
            *self.fail.lock().unwrap() = Some((at, refused));
        }

        fn set_down(&self, down: bool) {
            *self.down.lock().unwrap() = down;
        }
    }

    struct Counting(Arc<Embedded>);

    impl EmbedderSync for Counting {
        fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
            Ok(EmbedderDescriptor::default()
                .with_model_id(self.0.model)
                .with_dimensions(DIMS as u32))
        }

        fn embed(
            &self,
            texts: BoxIter<'static, Result<Text, Error>>,
        ) -> Result<BoxIter<'static, Result<Vector, Error>>, Error> {
            let texts: Vec<String> = texts
                .map(|text| text.map(|text| text.text.unwrap_or_default()))
                .collect::<Result<_, _>>()?;
            self.0.calls.lock().unwrap().push(texts.clone());
            let fail = if *self.0.down.lock().unwrap() {
                Some((0, false))
            } else {
                self.0.fail.lock().unwrap().take()
            };
            Ok(Box::new(texts.into_iter().enumerate().map(
                move |(at, text)| match fail {
                    Some((fails, refused)) if fails == at => Err(if refused {
                        Failure::new(EmbedderErrorCode::Refused, "never").into()
                    } else {
                        Failure::new(EmbedderErrorCode::Unavailable, "down")
                            .with_retryable(true)
                            .into()
                    }),
                    _ => Ok(Vector {
                        values: vector_of(&text),
                        ..Vector::default()
                    }),
                },
            )))
        }

        fn embed_query(&self, query: Query) -> Result<Vector, Error> {
            Ok(Vector {
                values: vector_of(&query.text.unwrap_or_default()),
                ..Vector::default()
            })
        }
    }

    /// A projector of JSON Documents: `title` is a lexical text, `body` and
    /// `tags` are the `vector` columns. It records what it was asked.
    struct Json(Arc<Mutex<Vec<(String, String)>>>);

    fn column(name: &str, kind: ColumnKind, vector: bool) -> IndexColumn {
        IndexColumn::default()
            .with_name(name)
            .with_kind(kind)
            .with_fts(!vector)
            .with_vector(vector)
    }

    impl ProjectorSync for Json {
        fn columns(&self, _collection: CollectionKey) -> Result<IndexSchema, Error> {
            Ok(IndexSchema {
                columns: vec![
                    column("title", ColumnKind::COLUMN_KIND_TEXT, false),
                    column("body", ColumnKind::COLUMN_KIND_TEXT, true),
                    column("tags", ColumnKind::COLUMN_KIND_TEXT_LIST, true),
                ],
                ..IndexSchema::default()
            })
        }

        fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
            self.0.lock().unwrap().push((
                document.collection.clone().unwrap_or_default(),
                document.id.clone().unwrap_or_default(),
            ));
            let json: serde_json::Value =
                serde_json::from_slice(&document.raw.unwrap_or_default()).unwrap_or_default();
            let mut doc = IndexDoc::default();
            for name in ["title", "body"] {
                if let Some(text) = json.get(name).and_then(|value| value.as_str()) {
                    doc.values.insert(
                        name.to_owned(),
                        IndexValue {
                            value: Some(index_value::Value::Text(text.to_owned())),
                            ..Default::default()
                        },
                    );
                }
            }
            if let Some(list) = json.get("tags").and_then(|value| value.as_array()) {
                doc.values.insert(
                    String::from("tags"),
                    IndexValue {
                        value: Some(index_value::Value::from(index_value::TextList {
                            texts: list
                                .iter()
                                .filter_map(|item| item.as_str().map(str::to_owned))
                                .collect(),
                            ..Default::default()
                        })),
                        ..Default::default()
                    },
                );
            }
            Ok(doc)
        }
    }

    fn options() -> OpenOptions {
        serde_json::from_value(serde_json::json!({"indexPollMs": 10, "writeTimeoutMs": 60000}))
            .unwrap()
    }

    fn ports(projected: &Arc<Mutex<Vec<(String, String)>>>, embedder: &Arc<Embedded>) -> Ports {
        Ports {
            projector: Some(Port::Sync(Arc::new(Json(Arc::clone(projected))))),
            embedder: Some(Port::Sync(Arc::new(Counting(Arc::clone(embedder))))),
            ..Ports::default()
        }
    }

    fn head(path: &Path) -> String {
        State::of(&Repos::new(path).views().unwrap()).token()
    }

    /// The states of a handle's indexer, forwarded by a thread so a wait can
    /// have a deadline.
    struct Items(Receiver<IndexState>);

    impl Items {
        fn of(vault: &Vault) -> Items {
            let mut watch = vault.watch_index_status(Empty::default()).unwrap();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                while let Some(Ok(state)) = watch.next() {
                    if tx.send(state).is_err() {
                        break;
                    }
                }
            });
            Items(rx)
        }

        /// Reads items until `done` is true of one, or fails the test at the
        /// deadline.
        fn until(&self, what: &str, mut done: impl FnMut(&IndexState) -> bool) {
            let deadline = Instant::now() + DEADLINE;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.0.recv_timeout(left) {
                    Ok(state) if done(&state) => return,
                    Ok(_) => {}
                    Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for {what}"),
                    Err(RecvTimeoutError::Disconnected) => panic!("the stream ended: {what}"),
                }
            }
        }

        /// Waits until the Index of `vault` has reached `HEAD` of the Vault at
        /// `path`.
        fn built(&self, vault: &Vault, path: &Path) {
            let want = head(path);
            let reached = |state: &IndexState| {
                state.version.as_deref() == Some(want.as_str())
                    && state.pending == Some(0)
                    && state.role != Some(true)
            };
            if reached(&vault.get_index_status(Empty::default()).unwrap()) {
                return;
            }
            self.until("the Index to reach HEAD", reached);
        }
    }

    /// A Vault on disk with a handle that has every port.
    struct Fixture {
        _scratch: Scratch,
        path: PathBuf,
        vault: Vault,
        items: Items,
        projected: Arc<Mutex<Vec<(String, String)>>>,
        embedded: Arc<Embedded>,
    }

    fn collection(value: serde_json::Value) -> Collection {
        serde_json::from_value(value).unwrap()
    }

    fn standard(name: &str) -> serde_json::Value {
        serde_json::json!({"name": name, "schema": {"standard": {}}})
    }

    impl Fixture {
        fn new(name: &str, collections: &[serde_json::Value]) -> Fixture {
            let scratch = Scratch::new(name);
            let projected = Arc::new(Mutex::new(Vec::new()));
            let embedded = Embedded::new("model-a");
            let vault = Vault::open(
                scratch.0.join("vault"),
                options(),
                ports(&projected, &embedded),
            )
            .unwrap();
            for value in collections {
                vault.create_collection(collection(value.clone())).unwrap();
            }
            let path = scratch.0.join("vault").canonicalize().unwrap();
            let items = Items::of(&vault);
            let fixture = Fixture {
                _scratch: scratch,
                path,
                vault,
                items,
                projected,
                embedded,
            };
            fixture.built();
            fixture
        }

        fn built(&self) {
            self.items.built(&self.vault, &self.path);
        }

        /// Another handle on the same Vault.
        fn handle(&self, ports: Ports) -> (Vault, Items) {
            let vault = Vault::open(&self.path, options(), ports).unwrap();
            let items = Items::of(&vault);
            (vault, items)
        }

        fn upsert(&self, vault: &Vault, collection: &str, id: &str, raw: &str) {
            vault
                .upsert_document(DocumentUpsert {
                    document: Document {
                        collection: Some(collection.to_owned()),
                        id: Some(id.to_owned()),
                        raw: Some(raw.as_bytes().to_vec()),
                        ..Default::default()
                    }
                    .into(),
                    note: Some("test".to_owned()),
                    ..Default::default()
                })
                .unwrap();
        }

        fn put(&self, collection: &str, id: &str, raw: &str) {
            self.upsert(&self.vault, collection, id, raw);
        }

        fn remove(&self, collection: &str, id: &str) {
            self.vault
                .delete_document(DocumentDeletion {
                    document: DocumentKey {
                        collection: Some(collection.to_owned()),
                        id: Some(id.to_owned()),
                        ..Default::default()
                    }
                    .into(),
                    note: Some("test".to_owned()),
                    ..Default::default()
                })
                .unwrap();
        }

        fn snapshot(&self) -> Arc<Snapshot> {
            self.vault.vectors().snapshot().unwrap().unwrap()
        }

        fn index(&self) -> Index {
            Index::new(&self.path)
        }
    }

    /// The Document the vector nearest to the vector of `text` belongs to.
    fn nearest(snapshot: &Snapshot, text: &str) -> (Slot, f32) {
        let hits = snapshot.search(&vector_of(text), 1).unwrap();
        let (slot, distance) = hits.first().unwrap();
        ((*slot).clone(), *distance)
    }

    #[test]
    fn a_documents_vector_values_are_in_the_index_replaced_on_change_and_removed_on_delete() {
        let hidden = serde_json::json!({
            "name": "hidden", "schema": {"standard": {}, "excludeVectorIndex": true}
        });
        let fx = Fixture::new("lifecycle", &[standard("notes"), hidden]);
        fx.put(
            "notes",
            "a",
            r#"{"title":"t","body":"alpha text","tags":["red","green"]}"#,
        );
        fx.put("notes", "b", r#"{"body":"beta"}"#);
        fx.put("hidden", "h", r#"{"body":"hidden text"}"#);
        fx.built();
        let snapshot = fx.snapshot();
        assert_eq!(snapshot.len(), 4, "alpha text, red, green, beta");
        assert_eq!(
            read_reached(&fx.index()).unwrap().unwrap().state.token(),
            head(&fx.path)
        );
        assert!(!snapshot.has("hidden", "h"));
        let (slot, distance) = nearest(&snapshot, "alpha text");
        assert_eq!((slot.collection.as_str(), slot.id.as_str()), ("notes", "a"));
        assert_eq!((slot.column.as_str(), slot.at), ("body", 0));
        assert!(distance < 1e-3, "its own vector is at distance 0");
        let (slot, _) = nearest(&snapshot, "green");
        assert_eq!(
            (slot.id.as_str(), slot.column.as_str(), slot.at),
            ("a", "tags", 1)
        );
        assert_eq!(slot.file, "notes/a");
        assert_eq!(
            snapshot.recorded(),
            &super::Recorded {
                model_id: "model-a".to_owned(),
                dimensions: DIMS
            }
        );

        fx.put("notes", "a", r#"{"body":"gamma"}"#);
        fx.built();
        let snapshot = fx.snapshot();
        assert_eq!(snapshot.len(), 2, "gamma, beta");
        assert_eq!(snapshot.vectors_of("notes", "a").unwrap().len(), 1);
        assert_eq!(nearest(&snapshot, "gamma").0.id, "a");

        fx.remove("notes", "b");
        fx.built();
        let snapshot = fx.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert!(!snapshot.has("notes", "b"));
        assert!(snapshot.has("notes", "a"));
    }

    #[test]
    fn the_projection_is_shared_one_project_per_changed_document_per_round() {
        let fx = Fixture::new("shared", &[standard("notes")]);
        fx.projected.lock().unwrap().clear();
        fx.put("notes", "a", r#"{"title":"t","body":"alpha"}"#);
        fx.built();
        assert_eq!(
            fx.projected.lock().unwrap().clone(),
            vec![("notes".to_owned(), "a".to_owned())],
            "both sides on, one Project call"
        );
        assert_eq!(fx.snapshot().len(), 1);
    }

    #[test]
    fn a_failing_embedder_fails_the_round_keeps_the_vectors_and_the_next_round_sends_the_rest() {
        let fx = Fixture::new("seven-of-twenty", &[standard("notes")]);
        let before = fx.index().read_commit().unwrap();
        let held = fx.index().try_lock().unwrap().unwrap();
        for n in 0..20 {
            fx.put(
                "notes",
                &format!("n{n:02}"),
                &format!(r#"{{"body":"text {n:02}"}}"#),
            );
        }
        fx.embedded.fail_at(7, false);
        drop(held);
        fx.items
            .until("the failed round", |state| state.last_error.is_set());
        assert_eq!(
            fx.index().read_commit().unwrap(),
            before,
            "the commit stays"
        );
        let state = fx.vault.indexer().status();
        let code = state.last_error.as_option().unwrap().code.clone().unwrap();
        assert!(code.contains("UNAVAILABLE"), "{code}");
        fx.built();
        assert!(!fx.vault.indexer().status().last_error.is_set());

        let calls = fx.embedded.texts();
        let first = calls.iter().find(|call| call.len() == 20).unwrap();
        let second = calls.iter().find(|call| call.len() == 13).unwrap();
        assert_eq!(
            second.as_slice(),
            &first[7..],
            "only the 13 that had no vector"
        );
        let sent: usize = calls.iter().map(Vec::len).sum();
        assert_eq!(sent, 33, "20, then 13");
        let snapshot = fx.snapshot();
        assert_eq!(snapshot.len(), 20);
        for n in 0..20 {
            let text = format!("text {n:02}");
            assert_eq!(nearest(&snapshot, &text).0.id, format!("n{n:02}"));
        }
    }

    #[test]
    fn a_text_that_did_not_change_is_not_embedded_again_when_a_round_walks_every_document() {
        let fx = Fixture::new("reuse", &[standard("notes")]);
        for id in ["a", "b", "c"] {
            fx.put("notes", id, &format!(r#"{{"body":"text {id}"}}"#));
        }
        fx.built();
        // A model that does not answer: the lexical side reaches the write,
        // the vector side and the Index do not.
        fx.embedded.set_down(true);
        fx.put("notes", "d", r#"{"body":"text d"}"#);
        fx.items
            .until("the failed round", |state| state.last_error.is_set());
        // `HEAD` moves before the next try, so the lexical side, ahead of the
        // Index, walks every Document again and offers every text.
        let held = loop {
            if let Some(held) = fx.index().try_lock().unwrap() {
                break held;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        fx.put("notes", "e", r#"{"body":"text e"}"#);
        fx.embedded.set_down(false);
        let before = fx.embedded.texts().len();
        fx.projected.lock().unwrap().clear();
        drop(held);
        fx.built();
        assert!(
            fx.projected
                .lock()
                .unwrap()
                .contains(&("notes".to_owned(), "a".to_owned())),
            "the round walked the Documents that did not change too"
        );
        let mut sent: Vec<String> = fx.embedded.texts()[before..]
            .iter()
            .flatten()
            .cloned()
            .collect();
        sent.sort();
        assert_eq!(sent, ["text d", "text e"], "a, b, and c keep their vectors");
        let snapshot = fx.snapshot();
        assert_eq!(snapshot.len(), 5);
        for id in ["a", "b", "c", "d", "e"] {
            assert_eq!(nearest(&snapshot, &format!("text {id}")).0.id, id);
        }
    }

    #[test]
    fn a_text_the_embedder_refuses_leaves_its_document_out_and_the_round_goes_on() {
        let fx = Fixture::new("refused", &[standard("notes")]);
        let held = fx.index().try_lock().unwrap().unwrap();
        fx.put("notes", "a", r#"{"body":"ok one"}"#);
        fx.put("notes", "b", r#"{"body":"bad text"}"#);
        fx.put("notes", "c", r#"{"body":"ok two"}"#);
        fx.embedded.fail_at(1, true);
        drop(held);
        fx.items
            .until("the refused round", |state| state.last_error.is_set());
        let state = fx.vault.indexer().status();
        let code = state.last_error.as_option().unwrap().code.clone().unwrap();
        assert!(code.contains("REFUSED"), "{code}");
        fx.built();
        let calls = fx.embedded.texts();
        assert_eq!(
            calls.last().unwrap().as_slice(),
            ["ok two"],
            "the refused one is not sent again"
        );
        let snapshot = fx.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.has("notes", "a") && snapshot.has("notes", "c"));
        assert!(
            !snapshot.has("notes", "b"),
            "the Document is left out, its file stays"
        );
        assert!(
            fx.vault
                .get_document(DocumentKey {
                    collection: Some("notes".to_owned()),
                    id: Some("b".to_owned()),
                    ..Default::default()
                })
                .is_ok()
        );
    }

    #[test]
    fn the_portions_an_embedder_forms_do_not_change_the_vectors() {
        let scratch = Scratch::new("portions");
        let projected = Arc::new(Mutex::new(Vec::new()));
        let portions = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&portions);
        let fixed = FixedEmbedder::new(
            "fixed",
            DIMS as u32,
            std::num::NonZeroUsize::new(3).unwrap(),
            move |texts: Vec<String>| {
                seen.lock().unwrap().push(texts.len());
                Ok(texts.iter().map(|text| vector_of(text)).collect())
            },
        );
        let ports = Ports {
            projector: Some(Port::Sync(Arc::new(Json(projected)))),
            embedder: Some(Port::Sync(Arc::new(fixed))),
            ..Ports::default()
        };
        let vault = Vault::open(scratch.0.join("vault"), options(), ports).unwrap();
        vault
            .create_collection(collection(standard("notes")))
            .unwrap();
        let path = scratch.0.join("vault").canonicalize().unwrap();
        let items = Items::of(&vault);
        for n in 0..10 {
            vault
                .upsert_document(DocumentUpsert {
                    document: Document {
                        collection: Some("notes".to_owned()),
                        id: Some(format!("n{n}")),
                        raw: Some(format!(r#"{{"body":"word {n}"}}"#).into_bytes()),
                        ..Default::default()
                    }
                    .into(),
                    note: Some("test".to_owned()),
                    ..Default::default()
                })
                .unwrap();
        }
        items.built(&vault, &path);
        let snapshot = vault.vectors().snapshot().unwrap().unwrap();
        assert_eq!(snapshot.len(), 10);
        for n in 0..10 {
            assert_eq!(
                nearest(&snapshot, &format!("word {n}")).0.id,
                format!("n{n}")
            );
        }
        assert!(portions.lock().unwrap().iter().all(|size| *size <= 3));
    }

    #[test]
    fn a_handle_without_a_projector_or_a_needed_embedder_never_takes_a_round() {
        let fx = Fixture::new("who", &[standard("notes")]);
        let (bare, bare_items) = fx.handle(Ports::default());
        let projected = Arc::new(Mutex::new(Vec::new()));
        let projector_only = Ports {
            projector: Some(Port::Sync(Arc::new(Json(projected)))),
            ..Ports::default()
        };
        let (no_embedder, no_embedder_items) = fx.handle(projector_only);
        fx.upsert(&bare, "notes", "a", r#"{"body":"alpha"}"#);
        fx.built();
        bare_items.built(&bare, &fx.path);
        no_embedder_items.built(&no_embedder, &fx.path);
        for handle in [&bare, &no_embedder] {
            let state = handle.indexer().status();
            assert_eq!(state.role, Some(false));
            assert_eq!(state.version, fx.vault.indexer().status().version);
            assert_eq!(handle.indexer().locks(), 0, "it never took the lock");
        }
        assert!(fx.vault.indexer().locks() > 0);
        let error = bare.reindex(Empty::default()).unwrap_err();
        assert!(error.is(VaultErrorCode::FailedPrecondition));
        assert!(error.to_string().contains("Projector"), "{error}");
        let error = no_embedder.reindex(Empty::default()).unwrap_err();
        assert!(error.is(VaultErrorCode::FailedPrecondition));
        assert!(error.to_string().contains("embedder"), "{error}");
        assert_eq!(fx.snapshot().len(), 1);
    }

    #[test]
    fn a_handle_that_cannot_index_asks_its_projector_for_the_columns_once_per_head() {
        struct Asked(Json, Arc<AtomicUsize>);

        impl ProjectorSync for Asked {
            fn columns(&self, collection: CollectionKey) -> Result<IndexSchema, Error> {
                self.1.fetch_add(1, Ordering::SeqCst);
                self.0.columns(collection)
            }

            fn project(&self, document: RawDocument) -> Result<IndexDoc, Error> {
                self.0.project(document)
            }
        }

        let scratch = Scratch::new("asked");
        let asked = Arc::new(AtomicUsize::new(0));
        let projector = Asked(Json(Arc::new(Mutex::new(Vec::new()))), Arc::clone(&asked));
        let ports = Ports {
            projector: Some(Port::Sync(Arc::new(projector))),
            ..Ports::default()
        };
        let vault = Vault::open(scratch.0.join("vault"), options(), ports).unwrap();
        vault
            .create_collection(collection(standard("notes")))
            .unwrap();
        let looks = |more: u64| {
            let until = vault.indexer().looks() + more;
            let deadline = Instant::now() + DEADLINE;
            while vault.indexer().looks() < until {
                assert!(Instant::now() < deadline, "the indexer stopped looking");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        looks(5);
        let path = scratch.0.join("vault").canonicalize().unwrap();
        assert_ne!(
            Index::new(&path).read_commit().unwrap().map(State::token),
            Some(head(&path)),
            "no handle has the embedder the `vector` columns need"
        );
        let once = asked.load(Ordering::SeqCst);
        looks(20);
        assert_eq!(
            asked.load(Ordering::SeqCst),
            once,
            "HEAD did not move: the Projector is not asked again"
        );
    }

    #[test]
    fn a_vault_opened_only_without_ports_is_not_indexed_and_still_finds_its_lines() {
        let scratch = Scratch::new("no-ports");
        let vault = Vault::open(scratch.0.join("vault"), options(), Ports::default()).unwrap();
        vault
            .create_collection(collection(
                serde_json::json!({"name": "log", "schema": {"journal": {}}}),
            ))
            .unwrap();
        vault
            .create_document(DocumentCreation {
                document: Document {
                    collection: Some("log".to_owned()),
                    id: Some("one".to_owned()),
                    raw: Some(b"{}".to_vec()),
                    ..Default::default()
                }
                .into(),
                note: Some("test".to_owned()),
                ..Default::default()
            })
            .unwrap();
        let deadline = Instant::now() + DEADLINE;
        while vault.indexer().looks() < 5 {
            assert!(Instant::now() < deadline, "the indexer never looked");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(vault.indexer().locks(), 0);
        let path = scratch.0.join("vault").canonicalize().unwrap();
        assert_eq!(Index::new(&path).read_commit().unwrap(), None);
        let error = vault.lexical().snapshot().unwrap_err();
        assert!(error.is(VaultErrorCode::IndexRebuilding));
        let found = vault
            .get_document(DocumentKey {
                collection: Some("log".to_owned()),
                id: Some("one".to_owned()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(found.id.as_deref(), Some("one"));
    }

    #[test]
    fn an_embedder_of_another_model_indexes_nothing_and_a_reindex_from_it_remakes_the_vectors() {
        let fx = Fixture::new("model", &[standard("notes")]);
        fx.put("notes", "a", r#"{"body":"alpha"}"#);
        fx.put("notes", "b", r#"{"body":"beta"}"#);
        fx.built();
        let projected = Arc::new(Mutex::new(Vec::new()));
        let other = Embedded::new("model-b");
        let (b, b_items) = fx.handle(ports(&projected, &other));
        b_items.until("the mismatch", |state| state.model_mismatch == Some(true));
        assert_eq!(b.indexer().locks(), 0);
        assert_eq!(b.indexer().status().role, Some(false));
        assert_eq!(fx.vault.indexer().status().model_mismatch, Some(false));

        // A write: the handle of model-a indexes it, the other does not.
        fx.put("notes", "c", r#"{"body":"gamma"}"#);
        fx.built();
        b_items.built(&b, &fx.path);
        assert_eq!(b.indexer().locks(), 0);
        assert_eq!(recorded(&fx.index()).unwrap().unwrap().model_id, "model-a");

        b.reindex(Empty::default()).unwrap();
        b_items.until("the rebuild", |state| {
            state
                .rebuild
                .as_option()
                .and_then(|rebuild| rebuild.stage)
                .and_then(|stage| stage.as_known())
                == Some(rebuild::Stage::STAGE_DONE)
                && state.version.is_some()
                && state.model_mismatch == Some(false)
        });
        assert!(b.indexer().locks() > 0);
        assert_eq!(recorded(&fx.index()).unwrap().unwrap().model_id, "model-b");
        let remade: usize = other.texts().iter().map(Vec::len).sum();
        assert_eq!(remade, 3, "every vector made again with the new model");
        let snapshot = b.vectors().snapshot().unwrap().unwrap();
        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot.recorded().model_id, "model-b");
        fx.items
            .until("the first handle to see the other model", |state| {
                state.model_mismatch == Some(true)
            });
        let embedder = fx.vault.ports().embedder().unwrap().unwrap();
        assert_eq!(
            fx.vault.vectors().leg(Some(&embedder)).unwrap(),
            Leg::ModelMismatch
        );
    }

    #[test]
    fn the_vector_leg_says_why_it_cannot_run() {
        let fx = Fixture::new("leg", &[standard("notes")]);
        let embedder = fx.vault.ports().embedder().unwrap().unwrap();
        assert_eq!(fx.vault.vectors().leg(None).unwrap(), Leg::NoEmbedder);
        assert_eq!(
            fx.vault.vectors().leg(Some(&embedder)).unwrap(),
            Leg::NoVectors
        );
        fx.put("notes", "a", r#"{"body":"alpha"}"#);
        fx.built();
        assert_eq!(fx.vault.vectors().leg(Some(&embedder)).unwrap(), Leg::Ran);
    }

    #[test]
    fn a_damaged_vector_index_is_broken_and_a_reindex_repairs_it() {
        let fx = Fixture::new("broken", &[standard("notes")]);
        fx.put("notes", "a", r#"{"body":"alpha"}"#);
        fx.built();
        let index = fx.index();
        let file = super::vectors_file(&index);
        let bytes = std::fs::read(&file).unwrap();
        std::fs::write(&file, &bytes[..bytes.len() - 8]).unwrap();
        let reader = super::Reader::new(index);
        let error = reader.snapshot().unwrap_err();
        assert!(error.is(VaultErrorCode::IndexBroken));
        fx.vault.reindex(Empty::default()).unwrap();
        fx.items.until("the rebuild", |state| {
            state
                .rebuild
                .as_option()
                .and_then(|rebuild| rebuild.stage)
                .and_then(|stage| stage.as_known())
                == Some(rebuild::Stage::STAGE_DONE)
                && state.version.is_some()
        });
        let snapshot = reader.snapshot().unwrap().unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(nearest(&snapshot, "alpha").0.id, "a");
    }

    #[test]
    fn a_journal_line_gets_its_vector() {
        let fx = Fixture::new(
            "journal",
            &[serde_json::json!({"name": "log", "schema": {"journal": {}}})],
        );
        for (id, body) in [("one", "first entry"), ("two", "second entry")] {
            fx.vault
                .create_document(DocumentCreation {
                    document: Document {
                        collection: Some("log".to_owned()),
                        id: Some(id.to_owned()),
                        raw: Some(format!(r#"{{"body":"{body}"}}"#).into_bytes()),
                        ..Default::default()
                    }
                    .into(),
                    note: Some("test".to_owned()),
                    ..Default::default()
                })
                .unwrap();
        }
        fx.built();
        let snapshot = fx.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(nearest(&snapshot, "second entry").0.id, "two");
    }

    #[test]
    fn a_digest_of_a_text_is_the_same_for_the_same_text() {
        assert_eq!(digest("a"), digest("a"));
        assert_ne!(digest("a"), digest("b"));
    }

    #[cfg(feature = "tokio")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_async_embedder_feeds_the_indexer() {
        use protocontract::{IterStream, Stream};
        use scheda_api::v1::{DynEmbedderAsync, EmbedderAsync};

        struct Waiting;

        impl EmbedderAsync for Waiting {
            async fn describe(&self, _empty: Empty) -> Result<EmbedderDescriptor, Error> {
                Ok(EmbedderDescriptor::default()
                    .with_model_id("model-async")
                    .with_dimensions(DIMS as u32))
            }

            async fn embed<R>(
                &self,
                texts: R,
            ) -> Result<impl Stream<Item = Result<Vector, Error>> + Send + use<R>, Error>
            where
                R: Stream<Item = Result<Text, Error>> + Send + 'static,
            {
                let mut texts = Box::pin(texts);
                let mut reply = Vec::new();
                while let Some(text) = std::future::poll_fn(|cx| texts.as_mut().poll_next(cx)).await
                {
                    reply.push(Ok(Vector {
                        values: vector_of(&text?.text.unwrap_or_default()),
                        ..Vector::default()
                    }));
                }
                Ok(IterStream::new(reply.into_iter()))
            }

            async fn embed_query(&self, query: Query) -> Result<Vector, Error> {
                Ok(Vector {
                    values: vector_of(&query.text.unwrap_or_default()),
                    ..Vector::default()
                })
            }
        }

        let scratch = Scratch::new("async");
        let projected = Arc::new(Mutex::new(Vec::new()));
        let ports = Ports {
            projector: Some(Port::Sync(Arc::new(Json(projected)))),
            embedder: Some(Port::Async(DynEmbedderAsync::new(Waiting))),
            ..Ports::default()
        };
        let vault = Vault::open(scratch.0.join("vault"), options(), ports).unwrap();
        vault
            .create_collection(collection(standard("notes")))
            .unwrap();
        let path = scratch.0.join("vault").canonicalize().unwrap();
        let items = Items::of(&vault);
        vault
            .upsert_document(DocumentUpsert {
                document: Document {
                    collection: Some("notes".to_owned()),
                    id: Some("a".to_owned()),
                    raw: Some(br#"{"body":"alpha"}"#.to_vec()),
                    ..Default::default()
                }
                .into(),
                note: Some("test".to_owned()),
                ..Default::default()
            })
            .unwrap();
        let waiting = tokio::task::spawn_blocking(move || {
            items.built(&vault, &path);
            let snapshot = vault.vectors().snapshot().unwrap().unwrap();
            (
                snapshot.len(),
                nearest(&snapshot, "alpha").0.id,
                snapshot.recorded().model_id.clone(),
            )
        })
        .await
        .unwrap();
        assert_eq!(waiting, (1, "a".to_owned(), "model-async".to_owned()));
    }
}
