//! Records that changed between two commits of a Vault repository.
//!
//! The background indexer turns `(from, to)` into a list of
//! `(collection, id)` upserts and deletes, read from git objects rather
//! than the work tree, so uncommitted writes never reach the Index.
//! Renames are a delete of the old id plus an upsert of the new one. A
//! changed collection declaration asks for that whole collection. A `from`
//! commit the repository no longer holds (pruned by `gc`) asks for a full
//! rebuild of the leg; any other `from`, ancestor of `to` or not, diffs
//! tree to tree.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Error;
use crate::git::tree;
use crate::journal;
use crate::marks::Repo;
use crate::schema::{self, CollectionDecl, Decls, Kind};

/// Declaration file names at the root of a collection directory. A change
/// to any of them, leftovers included, re-reads the whole collection.
const DECLARATION_FILES: [&str; 6] = [
    ".collection.yaml",
    ".collection.yml",
    ".collection.json",
    ".schema.yaml",
    ".schema.yml",
    ".schema.json",
];

/// What happened to one Record between the two commits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Change {
    /// Present in `to`. `path` is repository-relative (for a `journal`, the
    /// segment that holds the line); `blob` is that file's object id in
    /// `to`.
    Upsert { path: String, blob: String },
    /// Absent from `to`.
    Delete,
}

/// One Record and what happened to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordChange {
    pub(crate) collection: String,
    pub(crate) id: String,
    pub(crate) change: Change,
}

/// Work for one leg to move from `from` to `to`.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct Diff {
    /// Record-level changes by collection. A `document` or `singleton` is
    /// in `id` order; a `journal` lists its deletes, then its upserts in
    /// append order (segment, then line), so the Index can number lines in
    /// the order they were written. None for a collection in `whole`.
    /// Within one collection a document id appears once.
    pub(crate) changes: Vec<RecordChange>,
    /// Collections whose declaration changed: index each in full from `to`,
    /// or drop it when `to` no longer declares it.
    pub(crate) whole: BTreeSet<String>,
    /// `from` is gone from the repository: rebuild the leg from `to`.
    pub(crate) rebuild: bool,
}

/// Changes between `from` and `to` in the repository at `dir`.
///
/// `decls` are the declarations of the main repository at its `to` commit;
/// `repo` says which tree `dir` is, so only the collections stored there
/// count (isolated collections live in the nested repository, their
/// declarations in the main one). `from` `None` is the empty tree: every
/// Record of `to` is an upsert. Encrypted collections are left out.
pub(crate) fn diff(
    dir: &Path,
    repo: Repo,
    decls: &Decls,
    from: Option<&str>,
    to: &str,
) -> Result<Diff, Error> {
    let from = match from {
        Some(oid) if !tree::has_commit(dir, oid)? => {
            return Ok(Diff {
                rebuild: true,
                ..Diff::default()
            });
        }
        Some(oid) => oid.to_owned(),
        None => tree::empty_tree_oid(dir)?,
    };
    let mut whole = BTreeSet::new();
    let found = scan(
        dir,
        repo,
        |top| decls.get(top),
        tree::diff_trees(dir, &from, to, &[])?,
        Some(&mut whole),
    )?;
    Ok(Diff {
        changes: ordered(found, &whole),
        whole,
        rebuild: false,
    })
}

/// Every Record of `decl` in commit `to` of the repository that stores it
/// (`dir`), as upserts in the order [`diff`] uses. Encrypted → empty.
pub(crate) fn collection(
    dir: &Path,
    decl: &CollectionDecl,
    to: &str,
) -> Result<Vec<RecordChange>, Error> {
    if decl.encrypted {
        return Ok(Vec::new());
    }
    let repo = if decl.isolated {
        Repo::Isolated
    } else {
        Repo::Main
    };
    let empty = tree::empty_tree_oid(dir)?;
    let prefix = format!("{}/", schema::collection_dir(&decl.name));
    let changes = tree::diff_trees(dir, &empty, to, &[prefix.as_str()])?;
    let found = scan(
        dir,
        repo,
        |top| (top == decl.name).then_some(decl),
        changes,
        None,
    )?;
    Ok(ordered(found, &BTreeSet::new()))
}

/// Map tree changes to Record changes. `whole`, when given, collects
/// collections whose declaration file changed (main repository only).
fn scan<'d>(
    dir: &Path,
    repo: Repo,
    lookup: impl Fn(&str) -> Option<&'d CollectionDecl>,
    changes: Vec<tree::TreeChange>,
    mut whole: Option<&mut BTreeSet<String>>,
) -> Result<Found, Error> {
    let mut found = Found::default();
    let mut segments = Vec::new();
    for change in changes {
        let Some((top, rest)) = change.path.split_once('/') else {
            continue;
        };
        if repo == Repo::Main && DECLARATION_FILES.contains(&rest) {
            if let Some(whole) = whole.as_deref_mut() {
                whole.insert(top.to_owned());
            }
            continue;
        }
        let Some(decl) = lookup(top) else {
            continue;
        };
        if decl.encrypted || decl.isolated != (repo == Repo::Isolated) {
            continue;
        }
        match decl.kind {
            Kind::Journal => {
                if !rest.contains('/') && journal::is_segment_name(rest) {
                    segments.push((decl, change));
                }
            }
            Kind::Document | Kind::Singleton => {
                if record_id(decl, rest) {
                    found.put(decl, rest, &change.path, change.new.as_deref(), None);
                }
            }
        }
    }
    journal_changes(dir, &segments, &mut found)?;
    Ok(found)
}

/// Changes in output order, leaving out collections in `whole`.
fn ordered(found: Found, whole: &BTreeSet<String>) -> Vec<RecordChange> {
    let mut keyed: Vec<_> = found
        .0
        .into_iter()
        .filter(|((collection, _), _)| !whole.contains(collection))
        .map(|((collection, id), (change, order))| {
            let key = (collection.clone(), order, id.clone());
            (
                key,
                RecordChange {
                    collection,
                    id,
                    change,
                },
            )
        })
        .collect();
    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    keyed.into_iter().map(|(_, change)| change).collect()
}

/// Whether `rest` (a path below the collection directory) is a Record file
/// the work-tree listing would yield: no hidden component, slug directory
/// names, and for a `singleton` only the declared id.
fn record_id(decl: &CollectionDecl, rest: &str) -> bool {
    match decl.kind {
        Kind::Singleton => decl.singleton_id.as_deref() == Some(rest),
        Kind::Document => {
            let mut parts = rest.split('/').peekable();
            while let Some(part) = parts.next() {
                if part.is_empty() || part.starts_with('.') {
                    return false;
                }
                if parts.peek().is_some() && crate::id::require_slug("id", part).is_err() {
                    return false;
                }
            }
            true
        }
        Kind::Journal => false,
    }
}

/// Position of a change inside its collection: deletes and document
/// upserts first (then by `id`), journal upserts after them in append order.
type Order = Option<(journal::SegmentOrder, usize)>;

/// Changes keyed by `(collection, id)`. An upsert wins over a delete of
/// the same id: a journal line that moved between segments is still there.
#[derive(Default)]
struct Found(BTreeMap<(String, String), (Change, Order)>);

impl Found {
    fn put(
        &mut self,
        decl: &CollectionDecl,
        id: &str,
        path: &str,
        blob: Option<&str>,
        order: Order,
    ) {
        let key = (decl.name.clone(), id.to_owned());
        match blob {
            Some(blob) => {
                self.0.insert(
                    key,
                    (
                        Change::Upsert {
                            path: path.to_owned(),
                            blob: blob.to_owned(),
                        },
                        order,
                    ),
                );
            }
            None => {
                self.0.entry(key).or_insert((Change::Delete, None));
            }
        }
    }
}

/// Line-level changes of each changed segment: an id whose payload is new
/// or differs is an upsert, an id the old segment had and the new one
/// lacks is a delete.
fn journal_changes(
    dir: &Path,
    segments: &[(&CollectionDecl, tree::TreeChange)],
    found: &mut Found,
) -> Result<(), Error> {
    let specs: Vec<String> = segments
        .iter()
        .flat_map(|(_, change)| [change.old.clone(), change.new.clone()])
        .flatten()
        .collect();
    let mut blobs: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    for (spec, bytes) in specs.iter().zip(tree::read_blobs(dir, &specs)?) {
        if let Some(bytes) = bytes {
            blobs.insert(spec.as_str(), bytes);
        }
    }
    let lines = |oid: Option<&str>| {
        oid.and_then(|oid| blobs.get(oid))
            .map(|bytes| journal::segment_lines(bytes))
            .unwrap_or_default()
    };
    for (decl, change) in segments {
        let old = lines(change.old.as_deref());
        let new = lines(change.new.as_deref());
        let segment = journal::segment_order(Path::new(&change.path));
        for (id, (line, payload)) in &new {
            if old.get(id).map(|(_, old)| old) != Some(payload) {
                let order = Some((segment.clone(), *line));
                found.put(decl, id, &change.path, change.new.as_deref(), order);
            }
        }
        for id in old.keys().filter(|id| !new.contains_key(*id)) {
            found.put(decl, id, &change.path, None, None);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{Change, Diff, RecordChange, diff};
    use crate::marks::Repo;
    use crate::schema;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    const NOTES: &str = "version: 1\nname: notes\nschema:\n  document: {}\n";
    const LOG: &str = "version: 1\nname: log\nschema:\n  journal: {}\n";

    /// A scratch git repository driven by the `git` CLI, outside Scheda.
    struct Git(PathBuf);

    impl Git {
        fn new() -> Self {
            crate::git::isolate_test_git_env();
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("scheda-diff-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let repo = Self(dir);
            repo.git(&["init", "-q", "-b", "main"]);
            repo
        }

        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn remove(&self, rel: &str) {
            fs::remove_file(self.0.join(rel)).unwrap();
        }

        fn commit(&self, message: &str) -> String {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "--allow-empty", "-m", message]);
            self.git(&["rev-parse", "HEAD"])
        }

        fn diff(&self, from: Option<&str>, to: &str) -> Diff {
            let decls = schema::load_at(&self.0, to).unwrap();
            diff(&self.0, Repo::Main, &decls, from, to).unwrap()
        }

        fn blob(&self, commit: &str, rel: &str) -> String {
            self.git(&["rev-parse", &format!("{commit}:{rel}")])
        }
    }

    impl Drop for Git {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn notes() -> Git {
        let repo = Git::new();
        repo.write("notes/.collection.yaml", NOTES);
        repo.write("log/.collection.yaml", LOG);
        repo
    }

    fn upsert(repo: &Git, to: &str, collection: &str, id: &str, path: &str) -> RecordChange {
        RecordChange {
            collection: collection.to_owned(),
            id: id.to_owned(),
            change: Change::Upsert {
                path: path.to_owned(),
                blob: repo.blob(to, path),
            },
        }
    }

    fn delete(collection: &str, id: &str) -> RecordChange {
        RecordChange {
            collection: collection.to_owned(),
            id: id.to_owned(),
            change: Change::Delete,
        }
    }

    #[test]
    fn plain_commit_upserts_the_new_record() {
        let repo = notes();
        let c1 = repo.commit("schema");
        repo.write("notes/a", "A");
        let c2 = repo.commit("a");
        let got = repo.diff(Some(&c1), &c2);
        assert_eq!(got.changes, [upsert(&repo, &c2, "notes", "a", "notes/a")]);
        assert!(got.whole.is_empty());
        assert!(!got.rebuild);
    }

    #[test]
    fn several_commits_at_once_give_the_net_change() {
        let repo = notes();
        repo.write("notes/a", "A");
        repo.write("notes/gone", "G");
        let c1 = repo.commit("one");
        repo.write("notes/a", "A2");
        repo.write("notes/b", "B");
        repo.commit("two");
        repo.write("notes/b", "B2");
        repo.write("notes/temp", "T");
        repo.remove("notes/gone");
        repo.commit("three");
        repo.remove("notes/temp");
        let c4 = repo.commit("four");
        let got = repo.diff(Some(&c1), &c4);
        assert_eq!(
            got.changes,
            [
                upsert(&repo, &c4, "notes", "a", "notes/a"),
                upsert(&repo, &c4, "notes", "b", "notes/b"),
                delete("notes", "gone"),
            ]
        );
    }

    #[test]
    fn delete_and_rename() {
        let repo = notes();
        repo.write("notes/a", "A");
        repo.write("notes/old", "O");
        let c1 = repo.commit("one");
        repo.remove("notes/a");
        repo.git(&["mv", "notes/old", "notes/new"]);
        let c2 = repo.commit("two");
        let got = repo.diff(Some(&c1), &c2);
        assert_eq!(
            got.changes,
            [
                delete("notes", "a"),
                upsert(&repo, &c2, "notes", "new", "notes/new"),
                delete("notes", "old"),
            ]
        );
    }

    #[test]
    fn reset_hard_back_reverses_the_change() {
        let repo = notes();
        repo.write("notes/a", "A");
        let c1 = repo.commit("one");
        repo.write("notes/a", "A2");
        repo.write("notes/b", "B");
        let c2 = repo.commit("two");
        repo.git(&["reset", "-q", "--hard", &c1]);
        let got = repo.diff(Some(&c2), &c1);
        assert_eq!(
            got.changes,
            [
                upsert(&repo, &c1, "notes", "a", "notes/a"),
                delete("notes", "b")
            ]
        );
    }

    #[test]
    fn amend_diffs_from_the_replaced_commit() {
        let repo = notes();
        repo.commit("schema");
        repo.write("notes/b", "B");
        let c2 = repo.commit("two");
        repo.write("notes/b", "B2");
        repo.write("notes/d", "D");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-q", "--amend", "-m", "two again"]);
        let amended = repo.git(&["rev-parse", "HEAD"]);
        assert_ne!(amended, c2);
        let got = repo.diff(Some(&c2), &amended);
        assert_eq!(
            got.changes,
            [
                upsert(&repo, &amended, "notes", "b", "notes/b"),
                upsert(&repo, &amended, "notes", "d", "notes/d"),
            ]
        );
    }

    #[test]
    fn branch_switch_diffs_tree_to_tree() {
        let repo = notes();
        repo.write("notes/base", "0");
        repo.commit("base");
        repo.git(&["checkout", "-q", "-b", "feature"]);
        repo.write("notes/x", "X");
        let feature = repo.commit("x");
        repo.git(&["checkout", "-q", "main"]);
        repo.write("notes/y", "Y");
        let main = repo.commit("y");
        let got = repo.diff(Some(&feature), &main);
        assert_eq!(
            got.changes,
            [
                delete("notes", "x"),
                upsert(&repo, &main, "notes", "y", "notes/y"),
            ]
        );
    }

    #[test]
    fn pruned_mark_asks_for_a_rebuild() {
        let repo = notes();
        let c1 = repo.commit("schema");
        repo.git(&["checkout", "-q", "-b", "scratch"]);
        repo.write("notes/a", "A");
        let lost = repo.commit("lost");
        repo.git(&["checkout", "-q", "main"]);
        repo.git(&["branch", "-q", "-D", "scratch"]);
        repo.git(&["reflog", "expire", "--expire=now", "--all"]);
        repo.git(&["gc", "-q", "--prune=now"]);
        let got = repo.diff(Some(&lost), &c1);
        assert!(got.rebuild);
        assert!(got.changes.is_empty());
    }

    #[test]
    fn no_mark_reads_every_declared_collection_whole() {
        let repo = notes();
        repo.write("notes/a", "A");
        repo.write("notes/dir/b", "B");
        let c1 = repo.commit("one");
        let got = repo.diff(None, &c1);
        assert!(got.changes.is_empty(), "{:?}", got.changes);
        assert_eq!(got.whole.iter().collect::<Vec<_>>(), ["log", "notes"]);
        assert!(!got.rebuild);
    }

    #[test]
    fn declaration_change_asks_for_the_whole_collection() {
        let repo = notes();
        repo.write("notes/a", "A");
        let c1 = repo.commit("one");
        repo.write(
            "notes/.collection.yaml",
            "version: 1\nname: notes\nschema:\n  document: {}\n  exclude_vector_index: true\n",
        );
        repo.write("notes/b", "B");
        repo.remove("log/.collection.yaml");
        let c2 = repo.commit("two");
        let got = repo.diff(Some(&c1), &c2);
        assert!(got.changes.is_empty(), "{:?}", got.changes);
        assert_eq!(got.whole.iter().collect::<Vec<_>>(), ["log", "notes"]);
    }

    #[test]
    fn non_record_paths_are_skipped() {
        let repo = notes();
        repo.write(
            "single/.collection.yaml",
            "version: 1\nname: single\nschema:\n  singleton:\n    id: settings\n",
        );
        let c1 = repo.commit("schema");
        repo.write("README", "top-level file");
        repo.write("notes/.hidden", "h");
        repo.write("notes/.dir/a", "h");
        repo.write("notes/not a slug!/a", "h");
        repo.write("undeclared/a", "u");
        repo.write("single/other", "o");
        repo.write("single/settings", "s");
        let c2 = repo.commit("two");
        let got = repo.diff(Some(&c1), &c2);
        assert_eq!(
            got.changes,
            [upsert(&repo, &c2, "single", "settings", "single/settings")]
        );
    }

    #[test]
    fn encrypted_collection_stays_out() {
        let repo = Git::new();
        repo.write(
            "vault/.collection.yaml",
            "version: 1\nname: vault\nschema:\n  document: {}\n  encrypted: true\n",
        );
        let c1 = repo.commit("schema");
        repo.write("vault/a", "-----BEGIN AGE ENCRYPTED FILE-----\n");
        let c2 = repo.commit("a");
        assert!(repo.diff(Some(&c1), &c2).changes.is_empty());
    }

    #[test]
    fn journal_lines_are_diffed_one_by_one() {
        let repo = notes();
        repo.write("log/00000001.jsonl", "a: one\nb: two\n");
        repo.write("log/00000002.jsonl", "m: moving\n");
        let c1 = repo.commit("one");
        // Append c, edit b, drop a, move m into the first segment, and
        // leave a torn tail that the walk would not read either.
        repo.write("log/00000001.jsonl", "b: TWO\nc: three\nm: moving\nd: torn");
        repo.remove("log/00000002.jsonl");
        let c2 = repo.commit("two");
        let got = repo.diff(Some(&c1), &c2);
        let seg = "log/00000001.jsonl";
        assert_eq!(
            got.changes,
            [
                delete("log", "a"),
                upsert(&repo, &c2, "log", "b", seg),
                upsert(&repo, &c2, "log", "c", seg),
                upsert(&repo, &c2, "log", "m", seg),
            ]
        );
    }

    #[test]
    fn journal_upserts_come_in_append_order() {
        let repo = notes();
        let c1 = repo.commit("schema");
        repo.write("log/00000002.jsonl", "a: three\n");
        repo.write("log/00000001.jsonl", "z: one\nb: two\n");
        let c2 = repo.commit("lines");
        let got = repo.diff(Some(&c1), &c2);
        let ids: Vec<&str> = got.changes.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["z", "b", "a"]);
    }

    #[test]
    fn collection_lists_every_record_of_one_collection() {
        let repo = notes();
        repo.write("notes/a", "A");
        repo.write("notes/sub/b", "B");
        repo.write("notes-other/c", "C");
        repo.write("log/00000001.jsonl", "x: 1\ny: 2\n");
        let c1 = repo.commit("one");
        let decls = schema::load_at(&repo.0, &c1).unwrap();
        let notes = super::collection(&repo.0, decls.get("notes").unwrap(), &c1).unwrap();
        assert_eq!(
            notes,
            [
                upsert(&repo, &c1, "notes", "a", "notes/a"),
                upsert(&repo, &c1, "notes", "sub/b", "notes/sub/b"),
            ]
        );
        let log = super::collection(&repo.0, decls.get("log").unwrap(), &c1).unwrap();
        let ids: Vec<&str> = log.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["x", "y"]);
    }

    #[test]
    fn isolated_records_count_only_in_the_nested_repository() {
        let main = Git::new();
        main.write(
            "west/.collection.yaml",
            "version: 1\nname: west\nschema:\n  document: {}\n  isolated: true\n",
        );
        main.write("notes/.collection.yaml", NOTES);
        let m1 = main.commit("schema");
        main.write("west/stray", "not where isolated Records live");
        let m2 = main.commit("stray");
        let decls = schema::load_at(&main.0, &m2).unwrap();
        let in_main = diff(&main.0, Repo::Main, &decls, Some(&m1), &m2).unwrap();
        assert_eq!(in_main, Diff::default());

        let nested = Git::new();
        nested.write("west/a", "A");
        nested.write("notes/b", "not a nested collection");
        let n1 = nested.commit("a");
        let got = diff(&nested.0, Repo::Isolated, &decls, None, &n1).unwrap();
        assert_eq!(got.changes, [upsert(&nested, &n1, "west", "a", "west/a")]);
        assert!(got.whole.is_empty(), "declarations live in the main repo");
    }

    #[test]
    fn git_cli_commit_after_a_scheda_commit() {
        let scratch = Git::new();
        let path = scratch.0.join("vault");
        let vault = crate::Vault::open(&path, crate::OpenOptions::new(), None).unwrap();
        let schema = crate::Schema {
            kind: crate::Unit::default().into(),
            ..crate::Schema::default()
        };
        let notes = vault.create_collection("notes", schema, None).unwrap();
        notes.add("a", b"A", None).unwrap();
        let by_scheda = crate::git::peel_head_oid(&path).unwrap().unwrap();
        let repo = Git(path);
        repo.write("notes/a", "edited by hand");
        repo.write("notes/b", "B");
        let by_hand = repo.commit("hand edit");
        let got = repo.diff(Some(&by_scheda), &by_hand);
        assert_eq!(
            got.changes,
            [
                upsert(&repo, &by_hand, "notes", "a", "notes/a"),
                upsert(&repo, &by_hand, "notes", "b", "notes/b"),
            ]
        );
        drop(vault);
    }

    #[test]
    fn uncommitted_declaration_is_not_read() {
        let repo = notes();
        let c1 = repo.commit("schema");
        repo.write(
            "draft/.collection.yaml",
            "version: 1\nname: draft\nschema:\n  document: {}\n",
        );
        let decls = schema::load_at(&repo.0, &c1).unwrap();
        assert!(decls.get("draft").is_none());
        assert!(decls.get("notes").is_some());
    }

    #[test]
    fn committed_declaration_errors_match_the_work_tree_rules() {
        let repo = notes();
        repo.write("notes/.schema.yaml", "x: 1\n");
        let c1 = repo.commit("leftover");
        let err = schema::load_at(&repo.0, &c1).unwrap_err();
        assert_eq!(err.known_code(), Some(crate::Code::Invalid));
        repo.remove("notes/.schema.yaml");
        repo.write("notes/.collection.json", "{}");
        let c2 = repo.commit("two files");
        let err = schema::load_at(&repo.0, &c2).unwrap_err();
        assert_eq!(err.known_code(), Some(crate::Code::Invalid));
        repo.remove("notes/.collection.json");
        repo.write("other/.collection.yaml", NOTES);
        let c3 = repo.commit("wrong dir");
        let err = schema::load_at(&repo.0, &c3).unwrap_err();
        assert_eq!(err.known_code(), Some(crate::Code::Invalid));
    }
}
