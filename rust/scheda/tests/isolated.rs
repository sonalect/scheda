//! P4: unencrypted isolated nested git (ISO-2, ISO-4, ISO-6, IDX-4).
//! Collection names here and in `testdata/` must not appear in
//! `rust/scheda/src` (MOD-1, SCH-1).

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use git2::{IndexEntry, IndexTime, Oid, Repository};
use scheda::{Code, Filter, OpenOptions, RecordAdd, RecordReplace, Schema, Vault};
use serde_json::{Value, json};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let n = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scheda-p4-{}-{n}", process::id()));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn options(&self) -> OpenOptions {
        OpenOptions::new()
    }

    fn reopen(&self) -> Vault {
        Vault::open(self.vault(), self.options(), None).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const WEST: &str = include_str!("../testdata/schema/west.yaml");
const WEST_ISOLATED: &str = include_str!("../testdata/schema/west_isolated.yaml");
const SEALED_LOG: &str = include_str!("../testdata/isolated/log.yaml");

fn rec(fields: Value) -> RecordAdd {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn rec_rep(fields: Value) -> RecordReplace {
    serde_json::from_value(json!({ "fields": fields })).unwrap()
}

fn live_index_at(vault: &Path) -> PathBuf {
    let root = vault.join(".scheda").join("index");
    match fs::read_to_string(root.join("current")) {
        Ok(name) => root.join(name.trim()),
        Err(_) => root,
    }
}

fn write_schema(scratch: &Scratch, body: &str) {
    let name = peek_schema_name(body);
    let dir = scratch.vault().join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(".schema.yaml"), body).unwrap();
}

fn log_len(dir: &Path) -> usize {
    let repo = Repository::open(dir).unwrap();
    let mut walk = repo.revwalk().unwrap();
    if walk.push_head().is_err() {
        return 0;
    }
    walk.count()
}

fn peek_schema_name(body: &str) -> String {
    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("name:") {
            return rest.trim().trim_matches('"').to_string();
        }
    }
    panic!("schema fixture name");
}

fn term(field: &str, value: &str) -> scheda::FindQuery {
    serde_json::from_value::<Filter>(json!({"term": {"field": field, "value": value}}))
        .unwrap()
        .into()
}

fn title_schema(isolated: bool) -> Schema {
    let mut value = json!({
        "version": 1,
        "document": {},
        "fields": [{"name": "title", "string": {}}]
    });
    if isolated {
        value["isolated"] = json!(true);
    }
    serde_json::from_value(value).unwrap()
}

#[cfg(unix)]
fn assert_owner_only_tree(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::symlink_metadata(root).unwrap();
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_dir() {
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o700,
            "{}",
            root.display()
        );
        for entry in fs::read_dir(root).unwrap() {
            assert_owner_only_tree(&entry.unwrap().path());
        }
        return;
    }
    if meta.is_file() {
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "{}",
            root.display()
        );
    }
}

#[test]
fn create_isolated_journal_writes_nested_git_not_main_log() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    let vault = scratch.reopen();
    vault.begin().unwrap();
    let decl: Schema = serde_json::from_value(json!({
        "version": 1,
        "journal": {"segment_max_bytes": 45},
        "isolated": true,
        "fields": [{"name": "msg", "string": {}}]
    }))
    .unwrap();
    vault.create_collection("sealed_log", decl, None).unwrap();
    let main_before = log_len(vault.path());
    let nested = scratch.vault().join(".isolated");
    assert!(Repository::open(&nested).is_ok());
    let collection = vault.collection("sealed_log").unwrap();
    collection
        .add("a", rec(json!({"msg": "one"})), None)
        .unwrap();
    collection
        .add("b", rec(json!({"msg": "two"})), None)
        .unwrap();
    assert_eq!(log_len(vault.path()), main_before);
    vault.commit("isolated batch").unwrap();
    assert_eq!(log_len(vault.path()), main_before + 1);
    assert_eq!(log_len(&nested), 1);
    assert!(
        git2::Repository::open(&nested)
            .unwrap()
            .remotes()
            .unwrap()
            .is_empty()
    );
    let got = collection.get("a").unwrap();
    assert_eq!(got.id.as_deref(), Some("a"));
    let expected = format!(".isolated/sealed_log/{:08x}-{}.jsonl", 1, vault.writer());
    assert_eq!(got.path.as_deref(), Some(expected.as_str()));
    let ids: Vec<_> = collection
        .lines()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(ids, ["a", "b"]);
}

#[test]
fn nested_commit_rolls_back_when_main_commit_fails() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, WEST);
    let vault = scratch.reopen();
    let decl: Schema = serde_json::from_value(json!({
        "version": 1,
        "journal": {"segment_max_bytes": 1024},
        "isolated": true,
        "fields": [{"name": "msg", "string": {}}]
    }))
    .unwrap();
    vault.create_collection("sealed_log", decl, None).unwrap();
    vault
        .collection("sealed_log")
        .unwrap()
        .add("e1", rec(json!({"msg": "one"})), None)
        .unwrap();
    let nested = scratch.vault().join(".isolated");
    let before = log_len(&nested);
    vault.begin().unwrap();
    vault
        .collection("sealed_log")
        .unwrap()
        .add("e2", rec(json!({"msg": "two"})), None)
        .unwrap();
    vault
        .collection("west_docs")
        .unwrap()
        .add("n1", rec(json!({"title": "main"})), None)
        .unwrap();
    let git_dir = scratch.vault().join(".git");
    let bak = scratch.root.join("git.bak");
    fs::rename(&git_dir, &bak).unwrap();
    let err = vault.commit("mixed").unwrap_err();
    fs::rename(&bak, &git_dir).unwrap();
    assert!(err.known_code().is_some(), "{err}");
    assert_eq!(log_len(&nested), before);
    let ids: Vec<_> = vault
        .collection("sealed_log")
        .unwrap()
        .lines()
        .unwrap()
        .map(|record| record.unwrap().id.unwrap())
        .collect();
    assert_eq!(ids, ["e1"]);
}

#[test]
fn isolated_journal_rotates_and_ids_map_is_in_main_index() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    let vault = scratch.reopen();
    vault.begin().unwrap();
    let decl: Schema = serde_json::from_value(json!({
        "version": 1,
        "journal": {"segment_max_bytes": 1024, "segment_max_lines": 1},
        "isolated": true,
        "fields": [{"name": "msg", "string": {}}]
    }))
    .unwrap();
    vault.create_collection("sealed_log", decl, None).unwrap();
    let collection = vault.collection("sealed_log").unwrap();
    collection
        .add("a", rec(json!({"msg": "one"})), None)
        .unwrap();
    collection
        .add("b", rec(json!({"msg": "two"})), None)
        .unwrap();
    vault.commit("rotate").unwrap();
    let nested = scratch.vault().join(".isolated").join("sealed_log");
    assert!(
        nested
            .join(format!("00000001-{}.jsonl", vault.writer()))
            .is_file()
    );
    assert!(
        nested
            .join(format!("00000002-{}.jsonl", vault.writer()))
            .is_file()
    );
    assert!(!scratch.vault().join(".isolated").join(".scheda").exists());
    let ids = live_index_at(&scratch.vault())
        .join("journal")
        .join("sealed_log")
        .join("ids");
    assert!(ids.is_file());
    let haystack = fs::read_to_string(&ids).unwrap();
    assert!(haystack.contains("\"a\""));
    assert!(haystack.contains("\"b\""));
    assert_eq!(collection.get("a").unwrap().id.as_deref(), Some("a"));
    assert_eq!(collection.get("b").unwrap().id.as_deref(), Some("b"));
}

#[test]
fn isolated_add_without_begin_is_one_nested_commit() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, SEALED_LOG);
    let vault = scratch.reopen();
    let nested = scratch.vault().join(".isolated");
    vault
        .collection("sealed_log")
        .unwrap()
        .add("e1", rec(json!({"msg": "x"})), None)
        .unwrap();
    assert_eq!(log_len(&nested), 1);
    assert_eq!(log_len(vault.path()), 1);
}

#[test]
fn isolated_document_round_trip_and_main_find_skips_it() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, WEST);
    write_schema(&scratch, WEST_ISOLATED);
    let vault = scratch.reopen();
    vault
        .collection("west_docs")
        .unwrap()
        .add("alpha", rec(json!({"title": "public"})), None)
        .unwrap();
    vault
        .collection("west_isolated")
        .unwrap()
        .add("secret", rec(json!({"west_pass": "nested-secret"})), None)
        .unwrap();
    let got = vault
        .collection("west_isolated")
        .unwrap()
        .get("secret")
        .unwrap();
    assert_eq!(got.id.as_deref(), Some("secret"));
    let page = vault.find(&[], &Filter::all().into(), 0, 10).unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].id.as_deref(), Some("alpha"));
    let named = vault
        .find(&["west_isolated"], &Filter::all().into(), 0, 10)
        .unwrap();
    assert_eq!(named.records.len(), 1);
    assert_eq!(named.records[0].id.as_deref(), Some("secret"));
    let scoped = vault
        .collection("west_isolated")
        .unwrap()
        .find(&Filter::all().into(), 0, 10)
        .unwrap();
    assert_eq!(scoped.records.len(), 1);
    assert_eq!(scoped.records[0].id.as_deref(), Some("secret"));
    let repo = git2::Repository::open(scratch.vault()).unwrap();
    assert!(repo.is_path_ignored(Path::new(".scheda")).unwrap());
    assert!(repo.is_path_ignored(Path::new(".scheda/index")).unwrap());
    #[cfg(unix)]
    assert_owner_only_tree(&scratch.vault().join(".scheda"));
}

#[test]
fn isolated_replace_and_history_use_nested_git() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, WEST_ISOLATED);
    let vault = scratch.reopen();
    let collection = vault.collection("west_isolated").unwrap();
    collection
        .add("secret", rec(json!({"west_pass": "one"})), None)
        .unwrap();
    vault.begin().unwrap();
    collection
        .replace("secret", rec_rep(json!({"west_pass": "two"})), None)
        .unwrap();
    vault.commit("second").unwrap();
    let got = collection.get("secret").unwrap();
    let hist = collection.history("secret", 0, 10).unwrap();
    assert!(hist.entries.len() >= 2);
    let nested = scratch.vault().join(".isolated");
    assert!(log_len(&nested) >= 2);
    assert_eq!(got.id.as_deref(), Some("secret"));
}

#[test]
fn isolated_remove_uses_nested_git() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, WEST_ISOLATED);
    let vault = scratch.reopen();
    let collection = vault.collection("west_isolated").unwrap();
    collection
        .add("secret", rec(json!({"west_pass": "one"})), None)
        .unwrap();
    vault.begin().unwrap();
    collection
        .replace("secret", rec_rep(json!({"west_pass": "two"})), None)
        .unwrap();
    vault.commit("second").unwrap();
    let nested = scratch.vault().join(".isolated");
    let before = log_len(&nested);
    let hist_before = collection.history("secret", 0, 10).unwrap();
    assert!(hist_before.entries.len() >= 2);
    collection.remove("secret", None).unwrap();
    assert_eq!(
        collection.get("secret").unwrap_err().known_code(),
        Some(Code::NotFound)
    );
    assert!(
        !scratch
            .vault()
            .join(".isolated")
            .join("west_isolated")
            .join("secret.md")
            .is_file()
    );
    assert_eq!(log_len(&nested), before + 1);
    let hist = collection.history("secret", 0, 10).unwrap();
    assert_eq!(hist.entries.len(), hist_before.entries.len());
}

#[test]
fn check_passes_when_nested_git_has_a_remote() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, SEALED_LOG);
    let vault = scratch.reopen();
    vault
        .collection("sealed_log")
        .unwrap()
        .add("e1", rec(json!({"msg": "x"})), None)
        .unwrap();
    vault.check().unwrap();
    let nested = scratch.vault().join(".isolated");
    let repo = Repository::open(&nested).unwrap();
    repo.remote("origin", "https://example.invalid/isolated.git")
        .unwrap();
    vault.check().unwrap();
}

#[test]
fn vault_push_leaves_nested_remote_untouched() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, WEST_ISOLATED);
    let vault = scratch.reopen();
    vault
        .collection("west_isolated")
        .unwrap()
        .add("secret", rec(json!({"west_pass": "one"})), None)
        .unwrap();
    let vault_dir = scratch.vault();
    let root = vault_dir.parent().unwrap();
    let main_bare = root.join("main-bare");
    let nested_bare = root.join("nested-bare");
    let nested = scratch.vault().join(".isolated");
    make_bare(vault.path(), &main_bare);
    make_bare(&nested, &nested_bare);
    let main_url = as_url(&main_bare);
    let nested_url = as_url(&nested_bare);
    set_origin(vault.path(), &main_url);
    set_origin(&nested, &nested_url);
    vault.check().unwrap();
    assert_eq!(origin_url(&nested), nested_url);
    assert_ne!(origin_url(&nested), origin_url(vault.path()));
    let nested_cfg = fs::read_to_string(nested.join(".git").join("config")).unwrap();
    assert!(
        !nested_cfg.contains(&main_url),
        "main origin must not appear in nested config: {nested_cfg}"
    );

    vault
        .collection("west_isolated")
        .unwrap()
        .add("later", rec(json!({"west_pass": "two"})), None)
        .unwrap();
    let nested_head = head_id(&nested_bare);
    vault.push().unwrap();
    assert_eq!(head_id(&nested_bare), nested_head);
    assert!(!tree_contains(&main_bare, "later.md"));
    assert!(!tree_contains(&main_bare, "secret.md"));
    assert!(!tree_contains(&nested_bare, "later.md"));

    vault.push_isolated().unwrap();
    assert_ne!(head_id(&nested_bare), nested_head);
    assert!(tree_contains(&nested_bare, "later.md"));
    assert!(tree_contains(&nested_bare, "secret.md"));
    assert!(!tree_contains(&main_bare, "later.md"));
    assert_eq!(origin_url(&nested), nested_url);
}

fn as_url(path: &Path) -> String {
    let path = path.to_str().expect("utf-8 path");
    if let Some(rest) = path.strip_prefix('/') {
        format!("file:///{rest}")
    } else {
        format!("file:///{}", path.replace('\\', "/"))
    }
}

fn make_bare(src: &Path, bare: &Path) {
    git2::build::RepoBuilder::new()
        .bare(true)
        .clone(&as_url(src), bare)
        .unwrap();
}

fn set_origin(dir: &Path, url: &str) {
    let repo = Repository::open(dir).unwrap();
    if repo.find_remote("origin").is_ok() {
        repo.remote_set_url("origin", url).unwrap();
    } else {
        repo.remote("origin", url).unwrap();
    }
}

fn origin_url(dir: &Path) -> String {
    Repository::open(dir)
        .unwrap()
        .find_remote("origin")
        .unwrap()
        .url()
        .unwrap()
        .to_owned()
}

fn head_id(dir: &Path) -> git2::Oid {
    Repository::open(dir)
        .unwrap()
        .head()
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id()
}

fn tree_contains(dir: &Path, needle: &str) -> bool {
    let repo = Repository::open(dir).unwrap();
    let tree = repo.head().unwrap().peel_to_tree().unwrap();
    let mut found = false;
    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if let Ok(name) = entry.name() {
            let path = format!("{root}{name}");
            if path.contains(needle) {
                found = true;
            }
        }
        git2::TreeWalkResult::Ok
    })
    .unwrap();
    found
}

#[test]
fn check_fails_when_parent_tracks_isolated() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, SEALED_LOG);
    let vault = scratch.reopen();
    vault
        .collection("sealed_log")
        .unwrap()
        .add("e1", rec(json!({"msg": "x"})), None)
        .unwrap();
    let rel = format!(".isolated/sealed_log/{:08x}-{}.jsonl", 1, vault.writer());
    let bytes = fs::read(vault.path().join(&rel)).unwrap();
    let repo = Repository::open(vault.path()).unwrap();
    let mut index = repo.index().unwrap();
    let entry = IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: 0o100644,
        uid: 0,
        gid: 0,
        file_size: bytes.len() as u32,
        id: Oid::ZERO_SHA1,
        flags: 0,
        flags_extended: 0,
        path: rel.as_bytes().to_vec(),
    };
    index.add_frombuffer(&entry, &bytes).unwrap();
    index.write().unwrap();
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}

#[cfg(unix)]
#[test]
fn isolated_record_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, SEALED_LOG);
    let vault = scratch.reopen();
    vault
        .collection("sealed_log")
        .unwrap()
        .add("e1", rec(json!({"msg": "x"})), None)
        .unwrap();
    let path = scratch
        .vault()
        .join(".isolated")
        .join("sealed_log")
        .join(format!("00000001-{}.jsonl", vault.writer()));
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn open_succeeds_when_isolated_tree_is_missing() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, WEST);
    write_schema(&scratch, WEST_ISOLATED);
    assert!(!scratch.vault().join(".isolated").exists());
    let vault = scratch.reopen();
    vault.reindex().unwrap();
    vault.find(&[], &Filter::all().into(), 0, 10).unwrap();
    vault.collection("west_isolated").unwrap();
}

#[cfg(unix)]
#[test]
fn scheda_index_is_owner_only_and_wide_mode_fails_check() {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    let vault = scratch.reopen();
    vault
        .create_collection("plain_notes", title_schema(false), None)
        .unwrap();
    vault
        .collection("plain_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "public"})), None)
        .unwrap();
    assert_owner_only_tree(&scratch.vault().join(".scheda"));
    vault.reindex().unwrap();
    assert_owner_only_tree(&scratch.vault().join(".scheda"));
    fs::set_permissions(vault.index_dir(), Permissions::from_mode(0o755)).unwrap();
    let err = vault.check().unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
    let text = format!("{err}");
    assert!(
        text.contains(".scheda/index"),
        "check must name the Index directory: {text}"
    );
}

#[test]
fn isolated_write_has_no_nested_scheda_and_reindex_counts_it() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    let vault = scratch.reopen();
    vault
        .create_collection("plain_notes", title_schema(false), None)
        .unwrap();
    vault
        .create_collection("sealed_notes", title_schema(true), None)
        .unwrap();
    vault
        .collection("plain_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "public"})), None)
        .unwrap();
    vault
        .collection("sealed_notes")
        .unwrap()
        .add("secret", rec(json!({"title": "sealed"})), None)
        .unwrap();
    assert!(!scratch.vault().join(".isolated").join(".scheda").exists());
    let counts = vault.reindex().unwrap();
    assert_eq!(counts.records_read, Some(2));
    assert!(!scratch.vault().join(".isolated").join(".scheda").exists());
}

#[test]
fn term_query_over_isolated_and_plain_returns_both() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    let vault = scratch.reopen();
    vault
        .create_collection("plain_notes", title_schema(false), None)
        .unwrap();
    vault
        .create_collection("sealed_notes", title_schema(true), None)
        .unwrap();
    vault
        .collection("plain_notes")
        .unwrap()
        .add("alpha", rec(json!({"title": "shared"})), None)
        .unwrap();
    vault
        .collection("sealed_notes")
        .unwrap()
        .add("secret", rec(json!({"title": "shared"})), None)
        .unwrap();
    let query = term("title", "shared");
    let page = vault
        .find(&["plain_notes", "sealed_notes"], &query, 0, 10)
        .unwrap();
    assert_eq!(page.total, Some(2));
    let mut names: Vec<_> = page
        .records
        .iter()
        .map(|record| record.collection.as_deref().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["plain_notes", "sealed_notes"]);
    let isolated = vault
        .collection("sealed_notes")
        .unwrap()
        .find(&query, 0, 10)
        .unwrap();
    assert_eq!(isolated.total, Some(1));
    assert_eq!(isolated.records[0].id.as_deref(), Some("secret"));
    let plain = vault
        .collection("plain_notes")
        .unwrap()
        .find(&query, 0, 10)
        .unwrap();
    assert_eq!(plain.total, Some(1));
    assert_eq!(plain.records[0].id.as_deref(), Some("alpha"));
}

#[test]
fn same_id_in_isolated_and_plain_is_distinct() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    let vault = scratch.reopen();
    vault
        .create_collection("plain_notes", title_schema(false), None)
        .unwrap();
    vault
        .create_collection("sealed_notes", title_schema(true), None)
        .unwrap();
    vault
        .collection("plain_notes")
        .unwrap()
        .add("shared", rec(json!({"title": "plain"})), None)
        .unwrap();
    vault
        .collection("sealed_notes")
        .unwrap()
        .add("shared", rec(json!({"title": "sealed"})), None)
        .unwrap();
    let page = vault
        .find(
            &["plain_notes", "sealed_notes"],
            &term("id", "shared"),
            0,
            10,
        )
        .unwrap();
    assert_eq!(page.total, Some(2));
    let mut pairs: Vec<_> = page
        .records
        .iter()
        .map(|record| {
            (
                record.collection.as_deref().unwrap(),
                record.id.as_deref().unwrap(),
            )
        })
        .collect();
    pairs.sort_unstable();
    assert_eq!(
        pairs,
        [("plain_notes", "shared"), ("sealed_notes", "shared")]
    );
    let empty = vault.find(&[], &term("id", "shared"), 0, 10).unwrap();
    assert_eq!(empty.total, Some(1));
    assert_eq!(empty.records[0].collection.as_deref(), Some("plain_notes"));
}

#[test]
fn old_nested_isolated_index_is_removed_by_reindex() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    let vault = scratch.reopen();
    vault.begin().unwrap();
    let decl: Schema = serde_json::from_value(json!({
        "version": 1,
        "journal": {"segment_max_bytes": 1024},
        "isolated": true,
        "fields": [{"name": "msg", "string": {}}]
    }))
    .unwrap();
    vault.create_collection("sealed_log", decl, None).unwrap();
    vault
        .collection("sealed_log")
        .unwrap()
        .add("e1", rec(json!({"msg": "one"})), None)
        .unwrap();
    vault.commit("nested map").unwrap();
    let nested_index = scratch
        .vault()
        .join(".isolated")
        .join(".scheda")
        .join("index");
    let nested_ids = nested_index.join("journal").join("sealed_log");
    fs::create_dir_all(&nested_ids).unwrap();
    fs::write(nested_index.join("format"), "3\n").unwrap();
    fs::write(
        nested_ids.join("ids"),
        "{\"id\":\"e1\",\"path\":\".isolated/sealed_log/00000001.jsonl\"}\n",
    )
    .unwrap();
    assert_eq!(
        vault
            .collection("sealed_log")
            .unwrap()
            .get("e1")
            .unwrap()
            .id
            .as_deref(),
        Some("e1")
    );
    vault.reindex().unwrap();
    assert!(!scratch.vault().join(".isolated").join(".scheda").exists());
    let main_ids = live_index_at(&scratch.vault())
        .join("journal")
        .join("sealed_log")
        .join("ids");
    assert!(main_ids.is_file());
    let haystack = fs::read_to_string(&main_ids).unwrap();
    assert!(haystack.contains("\"e1\""));
    assert_eq!(
        vault
            .collection("sealed_log")
            .unwrap()
            .get("e1")
            .unwrap()
            .id
            .as_deref(),
        Some("e1")
    );
}

#[test]
fn second_schema_claiming_isolated_name_is_invalid() {
    let scratch = Scratch::new();
    drop(Vault::open(scratch.vault(), scratch.options(), None).unwrap());
    write_schema(&scratch, WEST_ISOLATED);
    let other = scratch.vault().join("other_docs");
    fs::create_dir_all(&other).unwrap();
    fs::write(
        other.join(".schema.yaml"),
        "name: west_isolated\nkind: document\nversion: 1\nfields:\n  - name: title\n    string: {}\n",
    )
    .unwrap();
    let err = Vault::open(scratch.vault(), scratch.options(), None).unwrap_err();
    assert_eq!(err.known_code(), Some(Code::Invalid));
}
