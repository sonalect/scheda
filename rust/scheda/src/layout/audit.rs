//! The check of the layout: a Document file, or an id folder, that sits
//! where the canonical layout of its folder does not put it, and a folder
//! past its bound.
//!
//! Reading tolerates any tree, so these are problems of the report and not
//! failures: the Vault still reads, and the next write to a folder lays it
//! out again.

use std::collections::{BTreeMap, BTreeSet};

use scheda_api::v1::{Problem, VaultErrorCode};

use super::{MAX_LABEL_BYTES, N, Tree, decode, encode};
use crate::check::problem;
use crate::collection::{Declarations, Kind};
use crate::error::{Context, Error, Fault};
use crate::isolated;
use crate::repo::{NodeKind, Snap, Views};

/// What the walk of one physical folder found.
struct Folder {
    /// Vault-relative path of the folder.
    rel: String,
    /// Names the folder holds itself, files and id folders.
    direct: usize,
    /// Names the folder and every label folder below it hold.
    total: usize,
    /// Whether the folder has label folders.
    split: bool,
    /// Whether the folder is a link of a long label chain: all it holds is
    /// one more label folder.
    link: bool,
}

/// Every name of one logical folder with where it sits.
#[derive(Default)]
struct Gathered {
    /// (name, label chain from the logical folder, Vault-relative path of
    /// the physical folder).
    placed: Vec<(String, Vec<String>, String)>,
    folders: Vec<Folder>,
    /// Vault-relative paths of the id folders, each a logical folder.
    id_dirs: Vec<String>,
}

/// Checks the layout of every `standard` and `journal` collection, as the
/// commits of `views` hold them, and collects what is wrong, each as an
/// `INVALID_ARGUMENT` problem with a Vault-relative path:
///
/// - a Document file or an id folder that is not where the canonical
///   layout of its folder puts it (the path of the entry);
/// - a folder that holds more than the bound of entries itself, or that is
///   split though all it holds would fit one flat folder (the path of the
///   folder).
///
/// A collection with a symlink on its way is left to the other checks. The
/// call fails only when it cannot run: an I/O failure.
pub(crate) fn check_layout(
    views: &Views,
    declarations: &Declarations,
) -> Result<Vec<Problem>, Error> {
    let mut problems = Vec::new();
    for decl in declarations.iter() {
        if decl.kind() == Kind::Singleton {
            continue;
        }
        let snap = views.of(decl.repo());
        let dir_rel = isolated::vault_rel(decl, "");
        let dir_rel = dir_rel.trim_end_matches('/');
        let links = snap
            .symlinks_below(dir_rel)
            .context("check_layout: look for symlinks")?;
        if links
            .iter()
            .any(|link| dir_rel == link || dir_rel.starts_with(&format!("{link}/")))
        {
            continue;
        }
        check_logical(snap, dir_rel, true, &mut problems)
            .context("check_layout: check the folders")?;
    }
    Ok(problems)
}

/// Checks the logical folder at `rel` and, one by one, the id folders in it.
fn check_logical(
    snap: &Snap,
    rel: &str,
    top: bool,
    problems: &mut Vec<Problem>,
) -> Result<(), Fault> {
    let mut gathered = Gathered::default();
    gather(snap, rel, &mut Vec::new(), top, &mut gathered)?;
    let canonical = Tree::build(
        b"",
        gathered.placed.iter().map(|(name, ..)| name.as_bytes()),
    );
    let mut canon = BTreeMap::new();
    canonical_chains(&canonical, &mut Vec::new(), &mut canon);
    let mut reported = BTreeSet::new();
    for folder in &gathered.folders {
        if folder.direct > N {
            reported.insert(folder.rel.as_str());
            problems.push(problem(
                VaultErrorCode::InvalidArgument,
                Some(&folder.rel),
                format!(
                    "the folder holds {} entries; the bound is {N}",
                    folder.direct
                ),
            ));
        } else if folder.split && !folder.link && folder.total <= N {
            problems.push(problem(
                VaultErrorCode::InvalidArgument,
                Some(&folder.rel),
                format!(
                    "the folder is split though its {} entries fit one folder of at most {N}",
                    folder.total
                ),
            ));
        }
    }
    for (name, chain, folder) in &gathered.placed {
        if reported.contains(folder.as_str()) || canon.get(name.as_bytes()) == Some(chain) {
            continue;
        }
        let path = format!("{folder}/{name}");
        problems.push(problem(
            VaultErrorCode::InvalidArgument,
            Some(&path),
            "the entry is not where the layout of its folder puts it",
        ));
    }
    for dir in &gathered.id_dirs {
        check_logical(snap, dir, false, problems)?;
    }
    Ok(())
}

/// Walks the physical folder at `rel` and the label folders below it into
/// `out`; returns the names they hold together.
fn gather(
    snap: &Snap,
    rel: &str,
    chain: &mut Vec<String>,
    top: bool,
    out: &mut Gathered,
) -> Result<usize, Fault> {
    let mut direct = 0;
    let mut total = 0;
    let mut labels = 0;
    let mut subfolders = Vec::new();
    for child in snap.list(rel)?.unwrap_or_default() {
        if top && chain.is_empty() && child.name.starts_with('.') {
            continue;
        }
        if child.node.kind == NodeKind::Dir && decode(&child.name).is_some() {
            labels += 1;
            subfolders.push(child.name);
            continue;
        }
        direct += 1;
        if child.node.kind == NodeKind::Dir {
            out.id_dirs.push(format!("{rel}/{}", child.name));
        }
        out.placed.push((child.name, chain.clone(), rel.to_owned()));
    }
    total += direct;
    for name in &subfolders {
        chain.push(name.clone());
        total += gather(snap, &format!("{rel}/{name}"), chain, false, out)?;
        chain.pop();
    }
    let link = direct == 0
        && labels == 1
        && rel
            .rsplit('/')
            .next()
            .and_then(decode)
            .is_some_and(|added| added.len() == MAX_LABEL_BYTES);
    out.folders.push(Folder {
        rel: rel.to_owned(),
        direct,
        total,
        split: labels > 0,
        link,
    });
    Ok(total)
}

/// The label chain the canonical tree puts each name under.
fn canonical_chains(
    tree: &Tree,
    chain: &mut Vec<String>,
    out: &mut BTreeMap<Vec<u8>, Vec<String>>,
) {
    for name in tree.entries() {
        out.insert(name.to_vec(), chain.clone());
    }
    for (added, child) in tree.children() {
        let links = encode(added);
        let depth = chain.len();
        chain.extend(links);
        canonical_chains(child, chain, out);
        chain.truncate(depth);
    }
}
