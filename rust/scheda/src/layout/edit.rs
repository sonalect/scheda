//! An incremental edit that keeps the layout of one folder canonical.
//!
//! A write changes a few names of a folder that may hold a million. The
//! edit reads only the nodes on the paths of those names and, when a node
//! must be counted or flattened, at most about `N + 1` more. After every
//! operation the tree equals [`Tree::build`] of the set of names.
//!
//! A node is [`Node::Open`] (its entries and subnodes are in memory) or
//! [`Node::Stored`] (an opaque id that a [`Store`] loads). An open node
//! keeps the id it was loaded with until it changes, so a writer reuses
//! every subtree that still has one and writes only the nodes without.
//!
//! A failed operation (a store that cannot load, a stored node that is not
//! canonical) leaves the edit unusable.

use std::collections::BTreeMap;
use std::mem;

use scheda_api::v1::VaultErrorCode;

use super::{N, Tree};
use crate::error::{Error, Fault};

/// The content of one node: the names it holds as entries, with their
/// payload, and its subnodes keyed by the bytes they add to the prefix of
/// this node.
#[derive(Debug)]
pub(crate) struct Body<E, I> {
    pub(crate) entries: BTreeMap<Vec<u8>, E>,
    pub(crate) children: BTreeMap<Vec<u8>, Node<E, I>>,
}

impl<E, I> Body<E, I> {
    /// A body without entries and subnodes.
    pub(crate) fn empty() -> Self {
        Self {
            entries: BTreeMap::new(),
            children: BTreeMap::new(),
        }
    }

    fn flat(name: &[u8], entry: E) -> Self {
        let mut body = Self::empty();
        body.entries.insert(name.to_vec(), entry);
        body
    }

    /// The body of the canonical tree `tree`; the payload of each name is
    /// moved out of `payload`.
    pub(crate) fn from_tree(tree: &Tree, payload: &mut BTreeMap<Vec<u8>, E>) -> Self {
        let mut body = Self::empty();
        for name in tree.entries() {
            if let Some(entry) = payload.remove(name) {
                body.entries.insert(name.to_vec(), entry);
            }
        }
        for (added, child) in tree.children() {
            body.children.insert(
                added.to_vec(),
                Node::Open(Open::new(Self::from_tree(child, payload))),
            );
        }
        body
    }
}

/// A node whose content is in memory.
#[derive(Debug)]
pub(crate) struct Open<E, I> {
    /// The id the content has in the store, while it is unchanged.
    pub(crate) id: Option<I>,
    pub(crate) body: Body<E, I>,
}

impl<E, I> Open<E, I> {
    /// A node without an id: a writer must write it.
    pub(crate) fn new(body: Body<E, I>) -> Self {
        Self { id: None, body }
    }
}

/// A node of the edit.
#[derive(Debug)]
pub(crate) enum Node<E, I> {
    /// The content is in the store under this id and is not loaded.
    Stored(I),
    /// The content is in memory.
    Open(Open<E, I>),
}

/// Where stored nodes are loaded from.
pub(crate) trait Store<E, I> {
    /// The content of the node `id`; its subnodes are [`Node::Stored`].
    fn load(&self, id: &I) -> Result<Body<E, I>, Error>;
}

impl<E, I: Clone> Node<E, I> {
    #[cfg(test)]
    /// The id of the node while it is unchanged since it was stored or
    /// loaded; `None` when it was edited and must be written.
    pub(crate) fn id(&self) -> Option<&I> {
        match self {
            Self::Stored(id) => Some(id),
            Self::Open(open) => open.id.as_ref(),
        }
    }

    /// Loads the node when it is stored.
    fn open<S: Store<E, I>>(&mut self, store: &S) -> Result<&mut Open<E, I>, Error> {
        if let Self::Stored(id) = self {
            let body = store.load(id)?;
            *self = Self::Open(Open {
                id: Some(id.clone()),
                body,
            });
        }
        match self {
            Self::Open(open) => Ok(open),
            Self::Stored(_) => Err(not_canonical("a stored node did not open")),
        }
    }
}

fn not_canonical(message: &str) -> Error {
    Fault::new(VaultErrorCode::FailedPrecondition, message).into()
}

fn lcp(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

impl<E, I: Clone> Open<E, I> {
    /// `C`, the common prefix of all the names of this split node, from its
    /// shape alone.
    fn split_point(&self, prefix: &[u8]) -> Result<Vec<u8>, Error> {
        let children = &self.body.children;
        if let (Some((first, _)), Some((last, _))) =
            (children.first_key_value(), children.last_key_value())
            && children.len() >= 2
        {
            let mut point = prefix.to_vec();
            point.extend_from_slice(&first[..lcp(first, last)]);
            return Ok(point);
        }
        let mut entries = self.body.entries.keys();
        match (entries.next(), entries.next(), children.keys().next()) {
            (Some(entry), None, Some(key))
                if entry.starts_with(prefix)
                    && prefix
                        .iter()
                        .chain(key)
                        .copied()
                        .collect::<Vec<u8>>()
                        .starts_with(entry) =>
            {
                Ok(entry.clone())
            }
            _ => Err(not_canonical("a split node is not canonical")),
        }
    }

    /// Inserts `name` (it starts with `prefix`) under this node.
    fn insert<S: Store<E, I>>(
        &mut self,
        store: &S,
        prefix: &[u8],
        name: &[u8],
        entry: E,
    ) -> Result<(), Error> {
        self.id = None;
        if self.body.children.is_empty() {
            if self.body.entries.contains_key(name) || self.body.entries.len() < N {
                self.body.entries.insert(name.to_vec(), entry);
                return Ok(());
            }
            let mut all = mem::take(&mut self.body.entries);
            all.insert(name.to_vec(), entry);
            let tree = Tree::build(prefix, all.keys());
            self.body = Body::from_tree(&tree, &mut all);
            return Ok(());
        }
        let point = self.split_point(prefix)?;
        let shared = lcp(&point, name);
        if shared == point.len() {
            if name == point.as_slice() {
                self.body.entries.insert(name.to_vec(), entry);
                return Ok(());
            }
            let below = &name[..=point.len()];
            let key = name[prefix.len()..=point.len()].to_vec();
            match self.body.children.get_mut(&key) {
                Some(child) => child.open(store)?.insert(store, below, name, entry)?,
                None => {
                    self.body
                        .children
                        .insert(key, Node::Open(Open::new(Body::flat(name, entry))));
                }
            }
            return Ok(());
        }
        // The node splits higher: at `point[..shared]`. Its old content is
        // one subnode, at the byte `point[shared]`, under a longer prefix.
        let moved = shared + 1 - prefix.len();
        let old_children = mem::take(&mut self.body.children)
            .into_iter()
            .map(|(key, child)| (key[moved..].to_vec(), child))
            .collect();
        let old = Open::new(Body {
            entries: mem::take(&mut self.body.entries),
            children: old_children,
        });
        self.body
            .children
            .insert(point[prefix.len()..=shared].to_vec(), Node::Open(old));
        if name.len() == shared {
            self.body.entries.insert(name.to_vec(), entry);
        } else {
            self.body.children.insert(
                name[prefix.len()..=shared].to_vec(),
                Node::Open(Open::new(Body::flat(name, entry))),
            );
        }
        Ok(())
    }

    /// Removes `name` (it starts with `prefix`) from under this node; whether
    /// it was there.
    fn remove<S: Store<E, I>>(
        &mut self,
        store: &S,
        prefix: &[u8],
        name: &[u8],
    ) -> Result<bool, Error> {
        if self.body.entries.remove(name).is_none() {
            let tail = &name[prefix.len()..];
            let Some(key) = self
                .body
                .children
                .keys()
                .find(|key| tail.starts_with(key))
                .cloned()
            else {
                return Ok(false);
            };
            let Some(child) = self.body.children.get_mut(&key) else {
                return Ok(false);
            };
            let open = child.open(store)?;
            if !open.remove(store, &name[..prefix.len() + key.len()], name)? {
                return Ok(false);
            }
            if open.body.entries.is_empty() && open.body.children.is_empty() {
                self.body.children.remove(&key);
            }
        }
        self.id = None;
        self.settle(store)?;
        Ok(true)
    }

    /// Restores the canonical shape of this node after a name left it: a
    /// subtree of at most `N` names is flat, and a node with one subnode and
    /// no entry takes the content of that subnode.
    fn settle<S: Store<E, I>>(&mut self, store: &S) -> Result<(), Error> {
        if self.body.children.is_empty() {
            return Ok(());
        }
        if self.fits(store)? {
            let mut all = BTreeMap::new();
            self.drain(store, &mut all)?;
            self.body = Body {
                entries: all,
                children: BTreeMap::new(),
            };
            return Ok(());
        }
        while self.body.entries.is_empty() && self.body.children.len() == 1 {
            let Some(key) = self.body.children.keys().next().cloned() else {
                break;
            };
            let Some(child) = self.body.children.get_mut(&key) else {
                break;
            };
            child.open(store)?;
            let Some(Node::Open(open)) = self.body.children.remove(&key) else {
                break;
            };
            self.body.entries = open.body.entries;
            self.body.children = open
                .body
                .children
                .into_iter()
                .map(|(rest, grandchild)| {
                    let mut joined = key.clone();
                    joined.extend(rest);
                    (joined, grandchild)
                })
                .collect();
        }
        Ok(())
    }

    /// Whether the subtree holds at most `N` names. It stops counting at
    /// `N + 1`, and a subnode that is split holds more than `N` by itself;
    /// the subnodes in memory are asked first, then the stored ones, each
    /// loaded once.
    fn fits<S: Store<E, I>>(&mut self, store: &S) -> Result<bool, Error> {
        let mut count = self.body.entries.len();
        if count > N {
            return Ok(false);
        }
        for child in self.body.children.values() {
            if let Node::Open(open) = child {
                if !open.body.children.is_empty() {
                    return Ok(false);
                }
                count += open.body.entries.len();
                if count > N {
                    return Ok(false);
                }
            }
        }
        for child in self.body.children.values_mut() {
            if matches!(child, Node::Stored(_)) {
                let open = child.open(store)?;
                if !open.body.children.is_empty() {
                    return Ok(false);
                }
                count += open.body.entries.len();
                if count > N {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Moves every name of the subtree into `out`.
    fn drain<S: Store<E, I>>(
        &mut self,
        store: &S,
        out: &mut BTreeMap<Vec<u8>, E>,
    ) -> Result<(), Error> {
        out.append(&mut self.body.entries);
        for child in mem::take(&mut self.body.children).into_values() {
            let mut child = child;
            child.open(store)?.drain(store, out)?;
        }
        Ok(())
    }
}

/// An edit of the folder at `prefix` whose root node is `root`.
#[derive(Debug)]
pub(crate) struct Edit<E, I> {
    prefix: Vec<u8>,
    root: Node<E, I>,
}

impl<E, I: Clone> Edit<E, I> {
    /// An edit of the folder with prefix `prefix` and the node `root`,
    /// canonical for that prefix.
    pub(crate) fn new(prefix: &[u8], root: Node<E, I>) -> Self {
        Self {
            prefix: prefix.to_vec(),
            root,
        }
    }

    #[cfg(test)]
    /// The root node as the edits left it.
    pub(crate) fn root(&self) -> &Node<E, I> {
        &self.root
    }

    /// Mutable access to the root, for a writer that stores nodes.
    pub(crate) fn root_mut(&mut self) -> &mut Node<E, I> {
        &mut self.root
    }

    /// Sets the payload of `name`, adding the name when it is new. The name
    /// must start with the prefix of the folder.
    pub(crate) fn insert<S: Store<E, I>>(
        &mut self,
        store: &S,
        name: &[u8],
        entry: E,
    ) -> Result<(), Error> {
        if !name.starts_with(&self.prefix) {
            return Err(Fault::new(
                VaultErrorCode::InvalidArgument,
                "the name does not start with the prefix of the folder",
            )
            .into());
        }
        self.root
            .open(store)?
            .insert(store, &self.prefix, name, entry)
    }

    /// Removes `name`; whether it was there.
    pub(crate) fn remove<S: Store<E, I>>(&mut self, store: &S, name: &[u8]) -> Result<bool, Error> {
        if !name.starts_with(&self.prefix) {
            return Ok(false);
        }
        self.root.open(store)?.remove(store, &self.prefix, name)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::hash_map::DefaultHasher;
    use std::collections::{BTreeSet, HashMap};
    use std::hash::{Hash, Hasher};

    use super::*;

    type Payload = u32;
    type Id = u64;
    type TestNode = Node<Payload, Id>;
    type TestBody = Body<Payload, Id>;

    #[derive(Clone, Hash)]
    struct Saved {
        entries: Vec<(Vec<u8>, Payload)>,
        children: Vec<(Vec<u8>, Id)>,
    }

    /// Content-addressed nodes in memory, with a count of loads.
    #[derive(Default)]
    struct MemStore {
        nodes: RefCell<HashMap<Id, Saved>>,
        loads: Cell<usize>,
    }

    impl MemStore {
        fn body(&self, id: &Id) -> Result<TestBody, Error> {
            let nodes = self.nodes.borrow();
            let Some(saved) = nodes.get(id) else {
                return Err(not_canonical("no such node"));
            };
            Ok(Body {
                entries: saved.entries.iter().cloned().collect(),
                children: saved
                    .children
                    .iter()
                    .map(|(key, id)| (key.clone(), Node::Stored(*id)))
                    .collect(),
            })
        }

        /// Writes every node without an id, bottom-up, and returns the id
        /// of `node`.
        fn write(&self, node: &mut TestNode) -> Id {
            match node {
                Node::Stored(id) => *id,
                Node::Open(open) => {
                    if let Some(id) = open.id {
                        return id;
                    }
                    let saved = Saved {
                        entries: open
                            .body
                            .entries
                            .iter()
                            .map(|(name, entry)| (name.clone(), *entry))
                            .collect(),
                        children: open
                            .body
                            .children
                            .iter_mut()
                            .map(|(key, child)| (key.clone(), self.write(child)))
                            .collect(),
                    };
                    let mut hasher = DefaultHasher::new();
                    saved.hash(&mut hasher);
                    let id = hasher.finish();
                    self.nodes.borrow_mut().insert(id, saved);
                    open.id = Some(id);
                    id
                }
            }
        }

        /// The id of the canonical tree of `model` under `prefix`.
        fn write_built(&self, prefix: &[u8], model: &BTreeMap<Vec<u8>, Payload>) -> Id {
            let tree = Tree::build(prefix, model.keys());
            let mut payload = model.clone();
            let mut root = Node::Open(Open::new(Body::from_tree(&tree, &mut payload)));
            self.write(&mut root)
        }

        fn stored(&self) -> usize {
            self.nodes.borrow().len()
        }
    }

    impl Store<Payload, Id> for MemStore {
        fn load(&self, id: &Id) -> Result<TestBody, Error> {
            self.loads.set(self.loads.get() + 1);
            self.body(id)
        }
    }

    fn tree_of(store: &MemStore, body: &TestBody, prefix: &[u8]) -> Result<Tree, Error> {
        let mut tree = Tree {
            prefix: prefix.to_vec(),
            entries: body.entries.keys().cloned().collect(),
            children: BTreeMap::new(),
        };
        for (key, child) in &body.children {
            let mut below = prefix.to_vec();
            below.extend_from_slice(key);
            let sub = match child {
                Node::Open(open) => tree_of(store, &open.body, &below)?,
                Node::Stored(id) => tree_of(store, &store.body(id)?, &below)?,
            };
            tree.children.insert(key.clone(), sub);
        }
        Ok(tree)
    }

    fn payloads(
        store: &MemStore,
        body: &TestBody,
        out: &mut BTreeMap<Vec<u8>, Payload>,
    ) -> Result<(), Error> {
        out.extend(
            body.entries
                .iter()
                .map(|(name, entry)| (name.clone(), *entry)),
        );
        for child in body.children.values() {
            match child {
                Node::Open(open) => payloads(store, &open.body, out)?,
                Node::Stored(id) => payloads(store, &store.body(id)?, out)?,
            }
        }
        Ok(())
    }

    /// The edit as a tree and as a map, without counting loads.
    fn snapshot(
        store: &MemStore,
        edit: &Edit<Payload, Id>,
    ) -> Result<(Tree, BTreeMap<Vec<u8>, Payload>), Error> {
        let body = match edit.root() {
            Node::Open(open) => {
                let mut copy = Body::empty();
                copy.entries = open.body.entries.clone();
                for (key, child) in &open.body.children {
                    copy.children.insert(key.clone(), clone_node(child));
                }
                copy
            }
            Node::Stored(id) => store.body(id)?,
        };
        let mut all = BTreeMap::new();
        payloads(store, &body, &mut all)?;
        Ok((tree_of(store, &body, &edit.prefix)?, all))
    }

    fn clone_node(node: &TestNode) -> TestNode {
        match node {
            Node::Stored(id) => Node::Stored(*id),
            Node::Open(open) => Node::Open(Open {
                id: open.id,
                body: Body {
                    entries: open.body.entries.clone(),
                    children: open
                        .body
                        .children
                        .iter()
                        .map(|(key, child)| (key.clone(), clone_node(child)))
                        .collect(),
                },
            }),
        }
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: usize) -> usize {
            usize::try_from(self.next() % bound as u64).unwrap_or(0)
        }
    }

    fn hex_name(rng: &mut Rng) -> Vec<u8> {
        format!("l{:016x}{:016x}", rng.next(), rng.next()).into_bytes()
    }

    fn short_name(rng: &mut Rng) -> Vec<u8> {
        let len = rng.below(7);
        (0..len).map(|_| b'a' + rng.below(3) as u8).collect()
    }

    fn chain_name(rng: &mut Rng) -> Vec<u8> {
        let lens = [0, 100, 130, 250];
        let mut name = vec![b'k'; lens[rng.below(lens.len())]];
        for _ in 0..rng.below(4) {
            name.push(b'a' + rng.below(3) as u8);
        }
        name
    }

    fn boundary_name(rng: &mut Rng) -> Vec<u8> {
        match rng.below(72) {
            71 => b"n".to_vec(),
            i => format!("n{i:03}").into_bytes(),
        }
    }

    /// 3 000 operations; after each, the edit is `Tree::build` of the set.
    /// Every ten operations the tree is written and dropped to its id, so
    /// the next operations load.
    fn oracle(prefix: &[u8], seed: u64, make: fn(&mut Rng) -> Vec<u8>, steered: bool) {
        let store = MemStore::default();
        let mut edit = Edit::new(prefix, Node::Open(Open::new(Body::empty())));
        let mut model: BTreeMap<Vec<u8>, Payload> = BTreeMap::new();
        let mut rng = Rng(seed);
        for step in 0..3_000u32 {
            let grow = if steered {
                match model.len() {
                    64 => rng.below(2) == 0,
                    len => len < 64,
                }
            } else if (step / 500) % 2 == 0 {
                rng.below(4) != 0
            } else {
                rng.below(4) == 0
            };
            if grow {
                let mut name = prefix.to_vec();
                name.extend(make(&mut rng));
                model.insert(name.clone(), step);
                edit.insert(&store, &name, step).unwrap();
            } else if rng.below(20) == 0 {
                let mut name = prefix.to_vec();
                name.extend(make(&mut rng));
                let was = model.remove(&name).is_some();
                assert_eq!(edit.remove(&store, &name).unwrap(), was);
            } else if !model.is_empty() {
                let name = model.keys().nth(rng.below(model.len())).cloned().unwrap();
                model.remove(&name);
                assert!(edit.remove(&store, &name).unwrap());
            }
            let (tree, payload) = snapshot(&store, &edit).unwrap();
            assert_eq!(tree, Tree::build(prefix, model.keys()), "step {step}");
            assert_eq!(payload, model, "step {step}");
            if step % 10 == 9 {
                let id = store.write(edit.root_mut());
                assert_eq!(id, store.write_built(prefix, &model), "step {step}");
                *edit.root_mut() = Node::Stored(id);
            }
        }
        assert!(store.loads.get() > 0);
    }

    #[test]
    fn the_oracle_holds_for_random_hex_ids() {
        oracle(b"l", 1, hex_name, false);
    }

    #[test]
    fn the_oracle_holds_for_short_ids() {
        oracle(b"", 2, short_name, false);
        oracle(b"ab", 3, short_name, false);
    }

    #[test]
    fn the_oracle_holds_for_label_chains() {
        oracle(b"", 4, chain_name, false);
        oracle(b"k", 5, chain_name, false);
    }

    #[test]
    fn the_oracle_holds_at_the_boundary() {
        oracle(b"", 6, boundary_name, true);
        oracle(b"n", 7, boundary_name, true);
    }

    #[test]
    fn a_name_outside_the_prefix_is_refused_and_absent() {
        let store = MemStore::default();
        let mut edit = Edit::new(b"l", Node::Open(Open::new(Body::empty())));
        let err = edit.insert(&store, b"x1", 1).unwrap_err();
        assert!(err.is(VaultErrorCode::InvalidArgument));
        assert!(!edit.remove(&store, b"x1").unwrap());
    }

    /// The nodes from the root to the one that would hold `name`.
    fn path_len(tree: &Tree, name: &[u8]) -> usize {
        let below = tree
            .children()
            .find(|(_, child)| name.starts_with(child.prefix()));
        1 + below.map_or(0, |(_, child)| path_len(child, name))
    }

    fn big_store(count: usize) -> (MemStore, Id, BTreeMap<Vec<u8>, Payload>) {
        let mut rng = Rng(77);
        let mut model = BTreeMap::new();
        while model.len() < count {
            model.insert(hex_name(&mut rng), 0);
        }
        let store = MemStore::default();
        let id = store.write_built(b"", &model);
        (store, id, model)
    }

    #[test]
    fn one_insert_loads_only_its_path_and_rewrites_only_it() {
        let (store, id, model) = big_store(10_000);
        let before = store.stored();
        let tree = Tree::build(b"", model.keys());
        let mut rng = Rng(5);
        for round in 0..20 {
            let name = hex_name(&mut rng);
            let nodes = path_len(&tree, &name);
            let loads = store.loads.get();
            let mut edit = Edit::new(b"", Node::Stored(id));
            edit.insert(&store, &name, 9).unwrap();
            let used = store.loads.get() - loads;
            // No sibling is read, even when a full leaf splits.
            assert_eq!(used, nodes, "round {round}");
            assert!(used <= 6, "a path of {used} nodes");
            // The nodes of the path are written again; the others keep
            // their ids.
            let root_children: Vec<Id> = store
                .body(&id)
                .unwrap()
                .children
                .values()
                .filter_map(|child| child.id().copied())
                .collect();
            let fresh = store.stored();
            let new_id = store.write(edit.root_mut());
            let written = store.stored() - fresh;
            assert!(
                written <= nodes + 16,
                "{written} nodes for a path of {nodes}"
            );
            let kept = match edit.root() {
                Node::Open(open) => open
                    .body
                    .children
                    .values()
                    .filter_map(|child| child.id().copied())
                    .filter(|id| root_children.contains(id))
                    .count(),
                Node::Stored(_) => 0,
            };
            assert!(kept + 1 >= root_children.len(), "round {round}");
            let mut with = model.clone();
            with.insert(name, 9);
            assert_eq!(new_id, store.write_built(b"", &with), "round {round}");
        }
        assert!(store.stored() > before);
    }

    #[test]
    fn one_remove_loads_its_path_and_a_bounded_number_of_others() {
        let (store, id, model) = big_store(10_000);
        let tree = Tree::build(b"", model.keys());
        let mut rng = Rng(8);
        for round in 0..20 {
            let name = model.keys().nth(rng.below(model.len())).cloned().unwrap();
            let nodes = path_len(&tree, &name);
            let loads = store.loads.get();
            let mut edit = Edit::new(b"", Node::Stored(id));
            assert!(edit.remove(&store, &name).unwrap());
            let used = store.loads.get() - loads;
            assert!(used >= nodes, "round {round}");
            assert!(used <= nodes + N + 1, "{used} loads, path of {nodes}");
            assert!(used < 100, "round {round}: {used} loads of 10 000 names");
            let mut rest = model.clone();
            rest.remove(&name);
            let new_id = store.write(edit.root_mut());
            assert_eq!(new_id, store.write_built(b"", &rest), "round {round}");
        }
    }

    #[test]
    fn a_remove_that_flattens_reads_at_most_n_plus_one_nodes() {
        // 65 names with 65 different next bytes: 65 one-name leaves.
        let model: BTreeMap<Vec<u8>, Payload> =
            (0..65u8).map(|b| (vec![b'a', b + 40], 0)).collect();
        let store = MemStore::default();
        let id = store.write_built(b"", &model);
        let mut edit = Edit::new(b"", Node::Stored(id));
        assert!(edit.remove(&store, &[b'a', 40]).unwrap());
        assert!(
            store.loads.get() <= 1 + N + 1,
            "{} loads",
            store.loads.get()
        );
        let (tree, _) = snapshot(&store, &edit).unwrap();
        assert!(!tree.is_split());
        assert_eq!(tree.entries().count(), N);
        // An absent name changes nothing and keeps the ids.
        let mut edit = Edit::new(b"", Node::Stored(id));
        assert!(!edit.remove(&store, b"zz").unwrap());
        assert_eq!(edit.root().id(), Some(&id));
    }

    #[test]
    fn a_set_of_names_that_repeats_is_the_same_set() {
        let store = MemStore::default();
        let mut edit = Edit::new(b"", Node::Open(Open::new(Body::empty())));
        let names: BTreeSet<Vec<u8>> = (0..70).map(|i| format!("n{i:03}").into_bytes()).collect();
        for name in &names {
            edit.insert(&store, name, 1).unwrap();
            edit.insert(&store, name, 2).unwrap();
        }
        let (tree, payload) = snapshot(&store, &edit).unwrap();
        assert_eq!(tree, Tree::build(b"", names.iter()));
        assert!(payload.values().all(|entry| *entry == 2));
    }

    #[test]
    fn a_node_left_with_one_subnode_takes_its_content() {
        // "a" and "ab" are entries over 70 names that all start with "abc";
        // removing them one by one hoists the subnode at each level.
        let store = MemStore::default();
        let mut model: BTreeMap<Vec<u8>, Payload> = BTreeMap::new();
        let mut edit = Edit::new(b"", Node::Open(Open::new(Body::empty())));
        let mut names: Vec<Vec<u8>> = vec![b"a".to_vec(), b"ab".to_vec()];
        names.extend((0..70).map(|i| format!("abc{i:03}").into_bytes()));
        for name in &names {
            model.insert(name.clone(), 1);
            edit.insert(&store, name, 1).unwrap();
        }
        let (tree, _) = snapshot(&store, &edit).unwrap();
        assert_eq!(tree.entries().collect::<Vec<_>>(), vec![b"a".as_slice()]);
        for name in [b"a".to_vec(), b"ab".to_vec()] {
            model.remove(&name);
            assert!(edit.remove(&store, &name).unwrap());
            let (tree, payload) = snapshot(&store, &edit).unwrap();
            assert_eq!(tree, Tree::build(b"", model.keys()));
            assert_eq!(payload, model);
            // Written and reloaded, the same.
            let id = store.write(edit.root_mut());
            assert_eq!(id, store.write_built(b"", &model));
            *edit.root_mut() = Node::Stored(id);
        }
        let (tree, _) = snapshot(&store, &edit).unwrap();
        assert_eq!(tree.children().count(), 7);
        assert!(tree.entries().next().is_none());
        // Insert "a" again: the node splits higher and the old one moves
        // down under a longer prefix.
        model.insert(b"a".to_vec(), 2);
        edit.insert(&store, b"a", 2).unwrap();
        let (tree, _) = snapshot(&store, &edit).unwrap();
        assert_eq!(tree, Tree::build(b"", model.keys()));
    }
}
