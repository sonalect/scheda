//! Laying out a standard collection in git trees.
//!
//! The logical folders of a collection are its folder and, for an id with
//! `/` in it, one folder per component. Each logical folder is laid out on
//! its own by the rule of the parent module: its entries (Document files
//! and id folders) are counted, and above [`N`](super::N) they are split
//! into label folders. A write changes the names of the logical folders on
//! the paths of its ids, deepest first, through an [`Edit`] of each, and
//! writes only the trees that changed.

use std::collections::BTreeMap;

use git2::{ObjectType, Oid, Repository};
use scheda_api::v1::VaultErrorCode;

use super::edit::{Body, Edit, Node, Open, Store};
use super::{MAX_LABEL_BYTES, Tree, decode, encode};
use crate::error::{Error, Fault, lib_fault};
use crate::repo::{NodeKind, Snap};

/// The file mode of a tree in git.
const TREE_MODE: i32 = 0o040_000;

/// What a name of a logical folder holds: the object and its file mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) id: Oid,
    pub(crate) mode: i32,
}

/// What a write does to one Document.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Op {
    /// The Document is this blob afterwards.
    Put(Entry),
    /// The Document is gone afterwards.
    Remove,
}

/// A dotfile at the root of a collection folder, carried through unchanged.
type Dotfile = (Vec<u8>, Entry);

/// Loads the nodes of a folder from the trees of a repository.
struct GitStore<'r> {
    repo: &'r Repository,
}

impl Store<Entry, Oid> for GitStore<'_> {
    fn load(&self, id: &Oid) -> Result<Body<Entry, Oid>, Error> {
        Ok(read_body(self.repo, *id, false)?.0)
    }
}

/// The content of the tree `id` as a node body: a subfolder whose name is a
/// label is a child, every other name an entry. A label chain is one child
/// keyed by the joined bytes of its links, and the child is the tree at its
/// end. A folder of exactly [`MAX_LABEL_BYTES`] bytes is a link that
/// continues when all it holds is one label folder; a canonical body never
/// holds only that, so a final label of that length that holds a body is
/// still read as a body.
///
/// With `root`, the names that start with `.` are returned apart instead of
/// being entries.
fn read_body(
    repo: &Repository,
    id: Oid,
    root: bool,
) -> Result<(Body<Entry, Oid>, Vec<Dotfile>), Error> {
    let tree = repo.find_tree(id).map_err(lib_fault)?;
    let mut body = Body::empty();
    let mut dots = Vec::new();
    for item in tree.iter() {
        let name = item.name_bytes();
        let entry = Entry {
            id: item.id(),
            mode: item.filemode_raw(),
        };
        if root && name.starts_with(b".") {
            dots.push((name.to_vec(), entry));
            continue;
        }
        let label = (item.kind() == Some(ObjectType::Tree))
            .then(|| item.name().ok().and_then(decode))
            .flatten();
        match label {
            Some(mut key) => {
                let mut at = entry.id;
                while key.len() % MAX_LABEL_BYTES == 0 {
                    let Some((more, next)) = link_of(repo, at)? else {
                        break;
                    };
                    key.extend(more);
                    at = next;
                }
                body.children.insert(key, Node::Stored(at));
            }
            None => {
                body.entries.insert(name.to_vec(), entry);
            }
        }
    }
    Ok((body, dots))
}

/// The label and the tree of the one folder that the tree `id` holds, when
/// that is all it holds and the folder is a label.
fn link_of(repo: &Repository, id: Oid) -> Result<Option<(Vec<u8>, Oid)>, Error> {
    let tree = repo.find_tree(id).map_err(lib_fault)?;
    if tree.len() != 1 {
        return Ok(None);
    }
    let Some(item) = tree.get(0) else {
        return Ok(None);
    };
    if item.kind() != Some(ObjectType::Tree) {
        return Ok(None);
    }
    Ok(item
        .name()
        .ok()
        .and_then(decode)
        .map(|label| (label, item.id())))
}

/// Every name of the folder the tree `id` is the root of, whatever the
/// shape of the layout under it: the entries of the label folders at any
/// depth are the names of the folder. With `root`, dotfiles are returned
/// apart.
fn collect(
    repo: &Repository,
    id: Oid,
    root: bool,
    names: &mut BTreeMap<Vec<u8>, Entry>,
    dots: &mut Vec<Dotfile>,
) -> Result<(), Error> {
    let tree = repo.find_tree(id).map_err(lib_fault)?;
    for item in tree.iter() {
        let name = item.name_bytes();
        let entry = Entry {
            id: item.id(),
            mode: item.filemode_raw(),
        };
        if root && name.starts_with(b".") {
            dots.push((name.to_vec(), entry));
        } else if item.kind() == Some(ObjectType::Tree)
            && item.name().ok().and_then(decode).is_some()
        {
            collect(repo, entry.id, false, names, dots)?;
        } else {
            names.insert(name.to_vec(), entry);
        }
    }
    Ok(())
}

/// Writes the nodes of `node` that have no id, bottom-up, and returns the id
/// of its tree; `None` for a node that holds nothing (and no `extra`). The
/// nodes that still have their id are not written again. `extra` are
/// entries of the tree that are not names of the layout (the dotfiles of a
/// collection folder).
fn write_node(
    repo: &Repository,
    node: &mut Node<Entry, Oid>,
    extra: &[Dotfile],
) -> Result<Option<Oid>, Error> {
    let open = match node {
        Node::Stored(id) => return Ok(Some(*id)),
        Node::Open(open) => open,
    };
    if let Some(id) = open.id {
        return Ok(Some(id));
    }
    if open.body.entries.is_empty() && open.body.children.is_empty() && extra.is_empty() {
        return Ok(None);
    }
    let mut builder = repo.treebuilder(None).map_err(lib_fault)?;
    for (name, entry) in open
        .body
        .entries
        .iter()
        .chain(extra.iter().map(|d| (&d.0, &d.1)))
    {
        builder
            .insert(name.as_slice(), entry.id, entry.mode)
            .map_err(lib_fault)?;
    }
    for (key, child) in &mut open.body.children {
        let Some(mut below) = write_node(repo, child, &[])? else {
            continue;
        };
        // The links of the chain, innermost first: each one holds the next.
        let chain = encode(key);
        for at in (0..chain.len()).rev() {
            if at == 0 {
                builder
                    .insert(chain[0].as_str(), below, TREE_MODE)
                    .map_err(lib_fault)?;
            } else {
                let mut link = repo.treebuilder(None).map_err(lib_fault)?;
                link.insert(chain[at].as_str(), below, TREE_MODE)
                    .map_err(lib_fault)?;
                below = link.write().map_err(lib_fault)?;
            }
        }
    }
    let id = builder.write().map_err(lib_fault)?;
    open.id = Some(id);
    Ok(Some(id))
}

/// The number of folders above a logical folder (`""` is the collection
/// folder).
fn depth(folder: &str) -> usize {
    if folder.is_empty() {
        0
    } else {
        folder.matches('/').count() + 1
    }
}

fn collision(folder: &str, name: &str) -> Error {
    Fault::new(
        VaultErrorCode::InvalidArgument,
        format!("{folder}{name}: the write names it as a folder and as a file"),
    )
    .into()
}

/// Applies `docs`, the Documents a write puts and removes in the collection
/// folder `dir` (it ends in `/`), to the tree `root` of `snap`, and returns
/// the tree of the collection folder afterwards, `None` when it holds
/// nothing. Trees are written to `mem`, a repository that reads the one of
/// `snap`.
///
/// Each Document touches the logical folders on its id: `a/b/c.md` changes
/// `c.md` in the folder `a/b`, `b` in `a`, and `a` in the collection
/// folder. The deepest folder is edited first and its new tree is put into
/// its parent as the entry for its name, or removed from it when the folder
/// became empty. A folder is found through the layout in the tree `root`,
/// and the dotfiles of the collection folder are carried through.
///
/// A folder that is not canonical (made by hand) fails the incremental
/// edit; it is then read whole once, laid out by the rule, and the edit
/// goes on. That costs one walk of the folder, once: the tree it writes is
/// canonical.
///
/// # Errors
///
/// `INVALID_ARGUMENT` when the write names an id as a file and as a folder;
/// `STORAGE` or `REPLICATION` when the library fails.
pub(crate) fn apply(
    snap: &Snap,
    mem: &Repository,
    root: Oid,
    dir: &str,
    docs: Vec<(String, Op)>,
) -> Result<Option<Oid>, Error> {
    let store = GitStore { repo: mem };
    let top = snap
        .lookup_in(root, dir.trim_end_matches('/'))?
        .filter(|node| node.kind == NodeKind::Dir)
        .map(|node| node.id);
    // The names to change in each logical folder, by the folder's id path.
    let mut pending: BTreeMap<String, BTreeMap<String, Op>> = BTreeMap::new();
    for (id, op) in docs {
        let (folder, name) = id.rsplit_once('/').unwrap_or(("", id.as_str()));
        pending
            .entry(folder.to_owned())
            .or_default()
            .insert(name.to_owned(), op);
    }
    // The deepest folder first: its new tree is an entry of its parent,
    // which is edited after every folder below it.
    while let Some(folder) = pending.keys().max_by_key(|key| depth(key)).cloned() {
        let ops = pending.remove(&folder).unwrap_or_default();
        let before = if folder.is_empty() {
            top
        } else {
            match snap.locate_in(root, dir, &folder)? {
                Some(path) => snap
                    .lookup_in(root, &path)?
                    .filter(|node| node.kind == NodeKind::Dir)
                    .map(|node| node.id),
                None => None,
            }
        };
        let made = edit_folder(mem, &store, before, folder.is_empty(), &ops)?;
        if folder.is_empty() {
            return Ok(made);
        }
        if made == before {
            continue;
        }
        let (parent, name) = folder.rsplit_once('/').unwrap_or(("", folder.as_str()));
        let op = match made {
            Some(id) => Op::Put(Entry {
                id,
                mode: TREE_MODE,
            }),
            None => Op::Remove,
        };
        let up = pending.entry(parent.to_owned()).or_default();
        if up.insert(name.to_owned(), op).is_some() {
            return Err(collision(dir, &folder));
        }
    }
    Ok(top)
}

/// Edits the logical folder whose tree is `before` with `ops` and writes
/// it. `root` is the collection folder: its dotfiles stay.
fn edit_folder(
    mem: &Repository,
    store: &GitStore<'_>,
    before: Option<Oid>,
    root: bool,
    ops: &BTreeMap<String, Op>,
) -> Result<Option<Oid>, Error> {
    let (start, mut dots) = match before {
        Some(id) if root => {
            let (body, dots) = read_body(mem, id, true)?;
            (Node::Open(Open { id: Some(id), body }), dots)
        }
        Some(id) => (Node::Stored(id), Vec::new()),
        None => (Node::Open(Open::new(Body::empty())), Vec::new()),
    };
    let mut edit = Edit::new(b"", start);
    let mut failed = run(&mut edit, store, ops);
    if let Err(error) = &failed
        && error.is(VaultErrorCode::FailedPrecondition)
        && let Some(id) = before
    {
        // The folder is not canonical. Its names are collected by one walk
        // of it, laid out by the rule, and the edit is done again on that.
        let mut names = BTreeMap::new();
        dots.clear();
        collect(mem, id, root, &mut names, &mut dots)?;
        let tree = Tree::build(b"", names.keys());
        let body = Body::from_tree(&tree, &mut names);
        edit = Edit::new(b"", Node::Open(Open::new(body)));
        failed = run(&mut edit, store, ops);
    }
    failed?;
    write_node(mem, edit.root_mut(), &dots)
}

/// Does `ops` to `edit`.
fn run(
    edit: &mut Edit<Entry, Oid>,
    store: &GitStore<'_>,
    ops: &BTreeMap<String, Op>,
) -> Result<(), Error> {
    for (name, op) in ops {
        match op {
            Op::Put(entry) => edit.insert(store, name.as_bytes(), *entry)?,
            Op::Remove => {
                edit.remove(store, name.as_bytes())?;
            }
        }
    }
    Ok(())
}

/// A file of a collection folder that differs between two trees.
#[derive(Debug)]
pub(crate) struct Moved {
    /// The Vault-relative path.
    pub(crate) path: String,
    /// The blob and whether it is executable, or `None` when the new tree
    /// holds no regular file there.
    pub(crate) want: Option<(Oid, bool)>,
    /// The blob the old tree held there, when it held a regular file.
    pub(crate) was: Option<Oid>,
}

/// The files that differ between the folder `before` and the folder `after`
/// (trees, `None` for no folder) at the path `at`, found by comparing the
/// ids of trees, so the cost is the trees that changed, not the folder. A
/// Document the layout moved is a file gone at one path and one there at
/// another.
///
/// # Errors
///
/// `REPLICATION` for a path that is not UTF-8; `STORAGE` or `REPLICATION`
/// when the library fails.
pub(crate) fn moved(
    repo: &Repository,
    at: &str,
    before: Option<Oid>,
    after: Option<Oid>,
) -> Result<Vec<Moved>, Error> {
    let mut out = Vec::new();
    diff(repo, at, before, after, &mut out)?;
    Ok(out)
}

fn diff(
    repo: &Repository,
    at: &str,
    before: Option<Oid>,
    after: Option<Oid>,
    out: &mut Vec<Moved>,
) -> Result<(), Error> {
    if before == after {
        return Ok(());
    }
    type Pair = (Option<Entry>, Option<Entry>);
    let mut names: BTreeMap<Vec<u8>, Pair> = BTreeMap::new();
    for (side, id) in [(0, before), (1, after)] {
        let Some(id) = id else { continue };
        let tree = repo.find_tree(id).map_err(lib_fault)?;
        for item in tree.iter() {
            let entry = Entry {
                id: item.id(),
                mode: item.filemode_raw(),
            };
            let pair = names.entry(item.name_bytes().to_vec()).or_default();
            if side == 0 {
                pair.0 = Some(entry);
            } else {
                pair.1 = Some(entry);
            }
        }
    }
    for (name, (old, new)) in names {
        if old == new {
            continue;
        }
        let Ok(name) = std::str::from_utf8(&name) else {
            return Err(Fault::new(
                VaultErrorCode::Replication,
                "a tree holds a path that is not UTF-8",
            )
            .into());
        };
        let path = format!("{at}/{name}");
        let tree_of = |entry: Option<Entry>| entry.filter(|e| e.mode == TREE_MODE).map(|e| e.id);
        let (old_tree, new_tree) = (tree_of(old), tree_of(new));
        if old_tree.is_some() || new_tree.is_some() {
            diff(repo, &path, old_tree, new_tree, out)?;
        }
        let file_of = |entry: Option<Entry>| entry.filter(|e| e.mode != TREE_MODE);
        let (old_file, new_file) = (file_of(old), file_of(new));
        if old_file != new_file {
            out.push(Moved {
                path,
                want: new_file.and_then(|e| match e.mode {
                    0o100_644 | 0o100_664 => Some((e.id, false)),
                    0o100_755 => Some((e.id, true)),
                    _ => None,
                }),
                was: old_file
                    .filter(|e| matches!(e.mode, 0o100_644 | 0o100_664 | 0o100_755))
                    .map(|e| e.id),
            });
        }
    }
    Ok(())
}
