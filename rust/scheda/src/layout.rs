//! The canonical layout of one folder of names.
//!
//! A folder holds names (byte strings): the entries of a collection folder
//! that the layout counts. Dotfiles are not given to this module: they
//! neither count nor move. The layout of a folder follows from the set of
//! its names alone, so the same names give the same tree on every clone.
//!
//! A folder with prefix `P` and a set `S` of names, all starting with `P`,
//! is flat when `|S| <= N`: every name is an entry of the folder. Otherwise
//! it is split: `C` is the longest common prefix of `S`, a name equal to `C`
//! stays an entry, and the rest are grouped by their byte at position
//! `|C|`; each group is a subfolder with prefix `C + b`, laid out by the
//! same rule. The subfolder is named `_` and the lowercase hex of the bytes
//! it adds to `P`; more than [`MAX_LABEL_BYTES`] added bytes become a chain
//! of nested folders, each of at most that many.
//!
//! A Document id never starts with `_`, so a label cannot be an id.

use std::collections::{BTreeMap, BTreeSet};

mod edit;
mod write;

pub(crate) use write::{Entry, Moved, Op, apply, moved};

/// The most entries a flat folder holds.
pub(crate) const N: usize = 64;

/// The most bytes one folder name of a label chain encodes.
pub(crate) const MAX_LABEL_BYTES: usize = 120;

/// The canonical tree of one folder: its entries and its subfolders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tree {
    /// The prefix every name of this folder starts with.
    prefix: Vec<u8>,
    /// The names that are entries of this folder.
    entries: BTreeSet<Vec<u8>>,
    /// The subfolders, keyed by the bytes they add to `prefix` (the label
    /// chain of a key is [`encode`] of it).
    children: BTreeMap<Vec<u8>, Tree>,
}

impl Tree {
    /// The canonical tree of `names` under `prefix`. Every name must start
    /// with `prefix`; a name that does not is left out. Duplicates count
    /// once, and the order of `names` does not matter.
    pub(crate) fn build<I, B>(prefix: &[u8], names: I) -> Self
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        let set: BTreeSet<Vec<u8>> = names
            .into_iter()
            .map(|name| name.as_ref().to_vec())
            .filter(|name| name.starts_with(prefix))
            .collect();
        let sorted: Vec<&[u8]> = set.iter().map(Vec::as_slice).collect();
        Self::build_sorted(prefix, &sorted)
    }

    /// `names` is sorted, distinct, and every name starts with `prefix`.
    fn build_sorted(prefix: &[u8], names: &[&[u8]]) -> Self {
        let mut tree = Self {
            prefix: prefix.to_vec(),
            entries: BTreeSet::new(),
            children: BTreeMap::new(),
        };
        let (Some(first), Some(last)) = (names.first(), names.last()) else {
            return tree;
        };
        if names.len() <= N {
            tree.entries = names.iter().map(|name| name.to_vec()).collect();
            return tree;
        }
        // Sorted names: the common prefix of all is that of the first and
        // the last.
        let common = first
            .iter()
            .zip(last.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let rest = match names.first() {
            Some(name) if name.len() == common => {
                tree.entries.insert(name.to_vec());
                &names[1..]
            }
            _ => names,
        };
        let mut start = 0;
        while start < rest.len() {
            let byte = rest[start][common];
            let len = rest[start..]
                .iter()
                .take_while(|name| name[common] == byte)
                .count();
            let group = &rest[start..start + len];
            let child_prefix = &group[0][..=common];
            tree.children.insert(
                child_prefix[prefix.len()..].to_vec(),
                Self::build_sorted(child_prefix, group),
            );
            start += len;
        }
        tree
    }

    #[cfg(test)]
    /// The prefix every name of this folder starts with.
    pub(crate) fn prefix(&self) -> &[u8] {
        &self.prefix
    }

    /// The names that are entries of this folder, in byte order.
    pub(crate) fn entries(&self) -> impl Iterator<Item = &[u8]> {
        self.entries.iter().map(Vec::as_slice)
    }

    /// The subfolders as (bytes added to this folder's prefix, tree), in
    /// byte order of those bytes. The folder names are [`encode`] of the
    /// bytes.
    pub(crate) fn children(&self) -> impl Iterator<Item = (&[u8], &Tree)> {
        self.children
            .iter()
            .map(|(added, tree)| (added.as_slice(), tree))
    }

    #[cfg(test)]
    /// Whether this folder is split into subfolders.
    pub(crate) fn is_split(&self) -> bool {
        !self.children.is_empty()
    }

    #[cfg(test)]
    /// The path of `name` in this tree: the chain of folder names (labels)
    /// from this folder to the one that holds it, empty when this folder
    /// holds it. `None` when the tree does not hold `name`.
    pub(crate) fn path(&self, name: &[u8]) -> Option<Vec<String>> {
        if self.entries.contains(name) {
            return Some(Vec::new());
        }
        // The subfolders of one folder differ at their first added byte, so
        // at most one has a prefix of `name`.
        let (added, child) = self
            .children
            .iter()
            .find(|(_, child)| name.starts_with(&child.prefix))?;
        let mut chain = encode(added);
        chain.extend(child.path(name)?);
        Some(chain)
    }
}

/// The folder names of the label of `added`, the bytes a subfolder adds to
/// its parent's prefix: `_` and their lowercase hex, cut into a chain of at
/// most [`MAX_LABEL_BYTES`] bytes per folder, outermost first. Empty for
/// no bytes.
pub(crate) fn encode(added: &[u8]) -> Vec<String> {
    added
        .chunks(MAX_LABEL_BYTES)
        .map(|chunk| {
            let mut label = String::with_capacity(1 + 2 * chunk.len());
            label.push('_');
            for byte in chunk {
                label.push(char::from(HEX[usize::from(byte >> 4)]));
                label.push(char::from(HEX[usize::from(byte & 15)]));
            }
            label
        })
        .collect()
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// The bytes one folder name adds to its parent's prefix, or `None` when
/// `folder` is not a label: it must be `_` and at least one pair of
/// lowercase hex digits. A name longer than a chain link is read too, so a
/// tree made elsewhere still reads.
pub(crate) fn decode(folder: &str) -> Option<Vec<u8>> {
    let hex = folder.strip_prefix('_')?.as_bytes();
    if hex.is_empty() || hex.len() % 2 != 0 {
        return None;
    }
    hex.chunks(2)
        .map(|pair| Some(nibble(pair[0])? << 4 | nibble(pair[1])?))
        .collect()
}

#[cfg(test)]
/// The bytes a chain of folder names adds, in order; `None` when any link
/// is not a label.
pub(crate) fn decode_chain<S: AsRef<str>>(chain: &[S]) -> Option<Vec<u8>> {
    let mut added = Vec::new();
    for link in chain {
        added.extend(decode(link.as_ref())?);
    }
    Some(added)
}

/// The id a path below a collection folder holds: its components without
/// the layout folders, which only say where the file sits. The last
/// component is the file and always stays.
pub(crate) fn id_of(below: &str) -> String {
    let mut parts: Vec<&str> = below.split('/').collect();
    let file = parts.pop();
    parts.retain(|part| decode(part).is_none());
    parts.extend(file);
    parts.join("/")
}

/// The file name of `below`, a path under a collection folder, when every
/// folder on the way is a layout folder; `None` when any is not.
pub(crate) fn file_in_labels(below: &str) -> Option<&str> {
    let (folders, file) = match below.rsplit_once('/') {
        Some((folders, file)) => (Some(folders), file),
        None => (None, below),
    };
    folders
        .is_none_or(|folders| folders.split('/').all(|folder| decode(folder).is_some()))
        .then_some(file)
}

fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

/// Where a read goes next in a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Descent<'a> {
    /// The name is looked for in this folder.
    Here,
    /// The name is looked for in this subfolder (its folder name).
    Into(&'a str),
}

/// The step of a read in a folder of any shape, canonical or not.
///
/// `entries` and `dirs` are the folder's listing: names of files and of
/// id subfolders, and names of subfolders. `prefix` is the folder's prefix.
/// A name listed in `entries` is [`Descent::Here`]. Otherwise the step is
/// into the subfolder whose label's bytes, appended to `prefix`, are a
/// prefix of `wanted`; the longest such match wins. A folder name that is
/// not a label is not a subfolder of the layout. With no match the step is
/// `Here`.
pub(crate) fn descend<'a, E: AsRef<[u8]>>(
    prefix: &[u8],
    entries: &[E],
    dirs: &[&'a str],
    wanted: &[u8],
) -> Descent<'a> {
    if entries.iter().any(|entry| entry.as_ref() == wanted) {
        return Descent::Here;
    }
    let Some(tail) = wanted.strip_prefix(prefix) else {
        return Descent::Here;
    };
    dirs.iter()
        .filter_map(|dir| {
            let added = decode(dir)?;
            tail.starts_with(&added).then_some((added.len(), *dir))
        })
        // The longest match; the earlier folder name on a tie.
        .fold(None::<(usize, &'a str)>, |best, found| match best {
            Some(best) if best.0 >= found.0 => Some(best),
            _ => Some(found),
        })
        .map_or(Descent::Here, |(_, dir)| Descent::Into(dir))
}

/// One step of an in-order walk of a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step<'a> {
    /// An entry of the folder.
    Entry(&'a [u8]),
    /// A subfolder (its folder name) to walk at this point.
    Dir(&'a str),
}

/// The order of an in-order walk of one folder: entries and subfolders
/// sorted so that the walk yields names in byte order. A subfolder sorts at
/// its full prefix (`prefix` and the bytes of its label); an entry equal to
/// that prefix comes before the subfolder. A folder name that is not a
/// label is left out.
pub(crate) fn walk_order<'a>(
    prefix: &[u8],
    entries: &[&'a [u8]],
    dirs: &[&'a str],
) -> Vec<Step<'a>> {
    let mut keyed: Vec<(Vec<u8>, u8, Step<'a>)> = entries
        .iter()
        .map(|entry| (entry.to_vec(), 0, Step::Entry(entry)))
        .collect();
    for dir in dirs {
        if let Some(added) = decode(dir) {
            let mut key = prefix.to_vec();
            key.extend(added);
            keyed.push((key, 1, Step::Dir(dir)));
        }
    }
    keyed.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    keyed.into_iter().map(|(_, _, step)| step).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Deterministic xorshift, no dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn hex_ids(count: usize, seed: u64) -> Vec<Vec<u8>> {
        let mut rng = Rng(seed);
        let mut set = BTreeSet::new();
        while set.len() < count {
            let id = format!("l{:016x}{:016x}.yaml", rng.next(), rng.next());
            set.insert(id.into_bytes());
        }
        set.into_iter().collect()
    }

    fn numbered(count: usize) -> Vec<Vec<u8>> {
        (0..count)
            .map(|i| format!("n{i:04}").into_bytes())
            .collect()
    }

    /// A folder of a materialized tree: files and label folders.
    #[derive(Default)]
    struct Folder {
        files: Vec<Vec<u8>>,
        dirs: Vec<String>,
    }

    type Folders = HashMap<Vec<String>, Folder>;

    /// The folders of `tree` as they lie on disk: siblings whose labels
    /// start with the same chain links share those folders.
    fn materialize(tree: &Tree, at: &mut Vec<String>, out: &mut Folders) {
        let files: Vec<Vec<u8>> = tree.entries().map(<[u8]>::to_vec).collect();
        out.entry(at.clone()).or_default().files = files;
        let depth = at.len();
        for (added, child) in tree.children() {
            for link in encode(added) {
                let folder = out.entry(at.clone()).or_default();
                if !folder.dirs.contains(&link) {
                    folder.dirs.push(link.clone());
                }
                at.push(link);
            }
            materialize(child, at, out);
            at.truncate(depth);
        }
    }

    /// Reads `name` by descent from the root folder; the path it ends in.
    fn find(folders: &Folders, name: &[u8]) -> Option<Vec<String>> {
        let mut at: Vec<String> = Vec::new();
        let mut prefix: Vec<u8> = Vec::new();
        loop {
            let folder = folders.get(&at)?;
            let dirs: Vec<&str> = folder.dirs.iter().map(String::as_str).collect();
            match descend(&prefix, &folder.files, &dirs, name) {
                Descent::Here => {
                    return folder.files.iter().any(|f| f == name).then_some(at);
                }
                Descent::Into(dir) => {
                    prefix.extend(decode(dir)?);
                    at.push(dir.to_owned());
                }
            }
        }
    }

    fn walk(folders: &Folders, at: &mut Vec<String>, prefix: &[u8], out: &mut Vec<Vec<u8>>) {
        let Some(folder) = folders.get(at) else {
            return;
        };
        let files: Vec<&[u8]> = folder.files.iter().map(Vec::as_slice).collect();
        let dirs: Vec<&str> = folder.dirs.iter().map(String::as_str).collect();
        for step in walk_order(prefix, &files, &dirs) {
            match step {
                Step::Entry(name) => out.push(name.to_vec()),
                Step::Dir(dir) => {
                    let mut sub = prefix.to_vec();
                    sub.extend(decode(dir).unwrap());
                    at.push(dir.to_owned());
                    walk(folders, at, &sub, out);
                    at.pop();
                }
            }
        }
    }

    fn shape(tree: &Tree, max_flat: &mut usize, max_split: &mut usize) {
        let count = tree.entries().count() + tree.children().count();
        if tree.is_split() {
            *max_split = (*max_split).max(count);
        } else {
            *max_flat = (*max_flat).max(count);
        }
        for (_, child) in tree.children() {
            shape(child, max_flat, max_split);
        }
    }

    #[test]
    fn order_of_insertion_does_not_matter() {
        let names = hex_ids(500, 7);
        let mut shuffled = names.clone();
        let mut rng = Rng(99);
        for i in (1..shuffled.len()).rev() {
            let j = usize::try_from(rng.next() % (i as u64 + 1)).unwrap();
            shuffled.swap(i, j);
        }
        assert_ne!(names, shuffled);
        assert_eq!(Tree::build(b"", &names), Tree::build(b"", &shuffled));
    }

    #[test]
    fn sixty_four_are_flat_and_sixty_five_split() {
        let names = numbered(65);
        let flat = Tree::build(b"", &names[..64]);
        assert!(!flat.is_split());
        assert_eq!(flat.entries().count(), 64);
        let split = Tree::build(b"", &names);
        assert!(split.is_split());
        assert_eq!(split.prefix(), b"");
        // No margin: one name less gives back exactly the flat tree.
        let mut fewer = names.clone();
        fewer.remove(64);
        assert_eq!(Tree::build(b"", &fewer), flat);
        let mut other = names.clone();
        other.remove(17);
        let back = Tree::build(b"", &other);
        assert!(!back.is_split());
        assert_eq!(back.entries().count(), 64);
    }

    #[test]
    fn a_name_equal_to_the_common_prefix_stays() {
        let mut names = numbered(70);
        names.push(b"n".to_vec());
        let tree = Tree::build(b"", &names);
        assert!(tree.is_split());
        assert_eq!(tree.entries().collect::<Vec<_>>(), vec![b"n".as_slice()]);
        assert_eq!(tree.path(b"n"), Some(vec![]));
        // The split is at the byte after "n", all of "n000".."n006".
        assert_eq!(tree.path(b"n0001").map(|p| p.len()), Some(2));
    }

    #[test]
    fn bounds_hold_for_random_hex_ids() {
        let names = hex_ids(10_000, 1);
        let tree = Tree::build(b"", &names);
        let (mut flat, mut split) = (0, 0);
        shape(&tree, &mut flat, &mut split);
        assert!(flat <= N, "flat folder holds {flat}");
        assert!(split <= 257, "split folder holds {split}");
        assert!(split <= 16, "hex ids split in at most 16, got {split}");
    }

    #[test]
    fn labels_round_trip_and_long_ones_chain() {
        assert_eq!(encode(b"la"), vec!["_6c61".to_owned()]);
        assert_eq!(decode("_6c61"), Some(b"la".to_vec()));
        assert_eq!(decode("_"), None);
        assert_eq!(decode("_6"), None);
        assert_eq!(decode("_zz"), None);
        assert_eq!(decode("_6C61"), None);
        assert_eq!(decode("6c61"), None);
        let added: Vec<u8> = (0..=255u8).cycle().take(300).collect();
        let chain = encode(&added);
        assert_eq!(chain.len(), 3);
        assert!(chain.iter().all(|link| link.len() <= 241));
        assert_eq!(chain[0].len(), 241);
        assert_eq!(decode_chain(&chain), Some(added));
        assert_eq!(decode_chain(&["_6c", "x"]), None);
    }

    #[test]
    fn a_long_common_prefix_becomes_a_chain_that_reads() {
        let long = "k".repeat(300);
        let names: Vec<Vec<u8>> = (0..70)
            .map(|i| format!("{long}{i:03}").into_bytes())
            .chain([b"a".to_vec()])
            .collect();
        let tree = Tree::build(b"", &names);
        let mut folders = Folders::new();
        materialize(&tree, &mut Vec::new(), &mut folders);
        assert!(folders.keys().flatten().all(|link| link.len() <= 241));
        let path = tree.path(&names[3]).unwrap();
        assert!(path.len() > 1);
        assert!(path.iter().all(|link| link.len() <= 241));
        for name in &names {
            assert_eq!(find(&folders, name), tree.path(name));
        }
    }

    #[test]
    fn the_example_path_of_a_lesson() {
        let mut names = hex_ids(2_000, 3);
        let target = names.remove(0);
        names.push(target.clone());
        let tree = Tree::build(b"", &names);
        let path = tree.path(&target).unwrap();
        // "l" is the common prefix; the first folder adds "l" and a digit.
        assert_eq!(path[0].len(), 5);
        assert!(path[0].starts_with("_6c"));
        assert!(path[1..].iter().all(|link| link.len() == 3));
    }

    #[test]
    fn descent_finds_every_name_in_the_canonical_tree() {
        let names = hex_ids(3_000, 5);
        let tree = Tree::build(b"", &names);
        let mut folders = Folders::new();
        materialize(&tree, &mut Vec::new(), &mut folders);
        for name in &names {
            assert_eq!(find(&folders, name), tree.path(name), "{name:?}");
        }
        assert_eq!(find(&folders, b"l0000-missing"), None);
    }

    #[test]
    fn descent_finds_every_name_in_a_flat_folder_of_a_thousand() {
        let names = hex_ids(1_000, 11);
        let mut folders = Folders::new();
        folders.insert(
            vec![],
            Folder {
                files: names.clone(),
                dirs: vec![],
            },
        );
        for name in &names {
            assert_eq!(find(&folders, name), Some(vec![]));
        }
    }

    #[test]
    fn descent_finds_every_name_in_a_tree_split_by_hand() {
        // Split at the second byte, where the layout would split at the
        // third, and 300 names in one level of folders.
        let names = hex_ids(300, 13);
        let mut by_pair: BTreeMap<Vec<u8>, Vec<Vec<u8>>> = BTreeMap::new();
        for name in &names {
            by_pair
                .entry(name[..2].to_vec())
                .or_default()
                .push(name.clone());
        }
        let mut folders = Folders::new();
        let mut root = Folder::default();
        for (pair, group) in by_pair {
            let label = encode(&pair).remove(0);
            root.dirs.push(label.clone());
            folders.insert(
                vec![label],
                Folder {
                    files: group,
                    dirs: vec![],
                },
            );
        }
        folders.insert(vec![], root);
        for name in &names {
            let path = find(&folders, name).unwrap();
            assert_eq!(path.len(), 1);
        }
        assert_eq!(find(&folders, b"l-missing"), None);
    }

    #[test]
    fn descent_takes_the_longest_label() {
        let dirs = ["_6c", "_6c61", "_6c62", "_zz", "x"];
        assert_eq!(
            descend(b"", &[] as &[&[u8]], &dirs, b"labc"),
            Descent::Into("_6c61")
        );
        assert_eq!(
            descend(b"", &[] as &[&[u8]], &dirs, b"lcd"),
            Descent::Into("_6c")
        );
        assert_eq!(descend(b"", &[] as &[&[u8]], &dirs, b"mno"), Descent::Here);
        assert_eq!(
            descend(b"l", &[b"lx".as_slice()], &["_61"], b"lx"),
            Descent::Here
        );
        assert_eq!(
            descend(b"l", &[] as &[&[u8]], &["_61"], b"lab"),
            Descent::Into("_61")
        );
    }

    #[test]
    fn the_walk_yields_names_sorted() {
        let mut names = hex_ids(2_000, 17);
        names.push(b"l".to_vec());
        names.push(b"l0".to_vec());
        let tree = Tree::build(b"", &names);
        let mut folders = Folders::new();
        materialize(&tree, &mut Vec::new(), &mut folders);
        let mut out = Vec::new();
        walk(&folders, &mut Vec::new(), b"", &mut out);
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(out, sorted);
    }

    #[test]
    fn an_entry_equal_to_a_folder_prefix_comes_first() {
        let entries = [b"la".as_slice()];
        let order = walk_order(b"l", &entries, &["_61", "_62", "bad"]);
        assert_eq!(
            order,
            vec![Step::Entry(b"la"), Step::Dir("_61"), Step::Dir("_62")]
        );
    }
}
