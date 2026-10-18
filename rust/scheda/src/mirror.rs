//! The mirror: the one remote repository a Vault sends its writes to and
//! brings the writes of others from.
//!
//! The network goes through the git command line (`git fetch`, `git push`;
//! `git clone` in `open`), so the person's SSH configuration, agent, and
//! credential helpers apply. Everything else is the git library, in
//! process: the remote's configuration, reading the fetched commit, the
//! join, and the move of the branch.
//!
//! A pull takes the write lock like a write, since it moves `HEAD` and the
//! work tree. What it does with the fetched commit depends on the history:
//!
//! - the Vault already holds it: nothing changes;
//! - the Vault's `HEAD` is an ancestor of it: `HEAD` moves forward to it;
//! - both moved: the two are joined Document by Document, never line by line.
//!   Against the merge base, a path changed on one side takes that side, a
//!   path changed on both sides to the same thing stays, and a path changed
//!   on both sides differently is `MIRROR_CONFLICT` with nothing changed. A
//!   join is one commit with both parents, the Vault's first.
//!
//! Isolated collections never leave the machine: only the repository of the
//! Vault is fetched or pushed, and the nested repository under `.isolated/`
//! is never contacted, whatever remote it has.

use std::collections::BTreeMap;
use std::path::Path;

use buffa_types::google::protobuf::Empty;
use git2::build::TreeUpdateBuilder;
use git2::{Commit, ErrorCode, FileMode, ObjectType, Oid, Repository, Tree};
use scheda_api::v1::{MirrorInfo, MirrorSetting, VaultErrorCode};

use crate::commit::branch_of;
use crate::error::{self, Context, Error, Fault, lib_fault, redact_remote_text};
use crate::repo::{self, Repo};
use crate::vault::Vault;
use crate::{git, stored};

/// The message of the commit that joins the history of the mirror to the
/// Vault's.
const JOIN_MESSAGE: &str = "join the mirror's writes";

/// How many conflicting paths a failure names before it says "and more".
const NAMED_CONFLICTS: usize = 10;

/// What a side changed at one path: the blob and mode it holds there, or
/// nothing when it removed the path.
type State = Option<(Oid, FileMode)>;

fn no_mirror() -> Fault {
    Fault::new(VaultErrorCode::NotFound, "the Vault has no mirror")
}

impl Vault {
    /// The mirror as this Vault knows it, read locally: its URL (empty
    /// without one), `auto_sync`, and how the last sync the Vault ran by
    /// itself failed. Contacts no network.
    ///
    /// # Errors
    ///
    /// `STORAGE` or `REPLICATION` when git cannot be read.
    pub(crate) fn mirror_info(&self) -> Result<MirrorInfo, Error> {
        let snap = self.repos().snapshot(Repo::Main)?;
        let git = snap.repository().ok_or_else(repository_missing)?;
        let mut info = MirrorInfo::default()
            .with_url(repo::origin_of(git)?.unwrap_or_default())
            .with_auto_sync(self.auto_sync());
        if let Some(failure) = self.sync_failure() {
            info.last_auto_sync_error = error::report(&failure).into();
        }
        Ok(info)
    }

    /// Sets the URL of the mirror, the remote `origin` of the repository.
    /// Without a mirror, it is set; with the same URL, nothing happens, so a
    /// repeat is safe; with another URL, the call is `ALREADY_EXISTS` naming
    /// the current one, unless `replace_existing`. Contacts no network, and
    /// runs no process.
    ///
    /// The setting is checked before the write lock is taken: a URL that is
    /// not an absolute URI or an scp-style address, or that carries a
    /// password, is `INVALID_ARGUMENT` (credentials come from git, never
    /// from the Vault). The call takes the write lock, so it never meets a
    /// running pull.
    ///
    /// # Errors
    ///
    /// The codes above; `ABORTED` (retryable) when the write lock is not
    /// free within `write_timeout_ms`; `STORAGE` or `REPLICATION` when git
    /// fails.
    pub(crate) fn set_mirror(&self, mirror_setting: MirrorSetting) -> Result<Empty, Error> {
        stored::validate(&mirror_setting).context("set the mirror")?;
        let url = mirror_setting.url.as_deref().unwrap_or_default().trim();
        let replace = mirror_setting.replace_existing.unwrap_or(false);
        let lock = self.committer().lock().context("take the write lock")?;
        let snap = lock.snapshot(Repo::Main)?;
        let git = snap.repository().ok_or_else(repository_missing)?;
        match repo::origin_of(git)? {
            Some(current) if current == url => {}
            Some(current) if !replace => {
                return Err(Fault::new(
                    VaultErrorCode::AlreadyExists,
                    format!(
                        "the Vault has the mirror {} already; set replace_existing to replace it",
                        redact_remote_text(&current)
                    ),
                )
                .into());
            }
            Some(_) => git.remote_set_url("origin", url).map_err(lib_fault)?,
            None if git.find_remote("origin").is_ok() => {
                git.remote_set_url("origin", url).map_err(lib_fault)?;
            }
            None => {
                git.remote("origin", url).map_err(lib_fault)?;
            }
        }
        Ok(Empty::default())
    }

    /// Detaches the mirror: the Vault then works locally only. Without a
    /// mirror, `NOT_FOUND`. Contacts no network, and runs no process. Takes
    /// the write lock, so it never meets a running pull.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` as above; `ABORTED` (retryable) when the write lock is
    /// not free within `write_timeout_ms`; `STORAGE` or `REPLICATION` when
    /// git fails.
    pub(crate) fn remove_mirror(&self, _empty: Empty) -> Result<Empty, Error> {
        let lock = self.committer().lock().context("take the write lock")?;
        let snap = lock.snapshot(Repo::Main)?;
        let git = snap.repository().ok_or_else(repository_missing)?;
        if repo::origin_of(git)?.is_none() {
            return Err(no_mirror().into());
        }
        git.remote_delete("origin").map_err(lib_fault)?;
        Ok(Empty::default())
    }

    /// Sends the stored writes to the mirror: `git push -- origin
    /// refs/heads/b:refs/heads/b` for the branch `b` `HEAD` names. Takes no
    /// write lock. Isolated collections never leave the machine.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` without a mirror; `MIRROR_AHEAD` when the mirror holds
    /// writes this Vault has not pulled; `UNAUTHENTICATED` when the mirror
    /// refuses the credentials; `UNAVAILABLE` when it cannot be reached;
    /// `REPLICATION` for any other failure of git.
    pub(crate) fn push(&self, _empty: Empty) -> Result<Empty, Error> {
        self.push_now().context("push to the mirror")?;
        self.clear_sync_failure();
        Ok(Empty::default())
    }

    /// Brings the writes of the mirror into the Vault. Takes the write lock.
    ///
    /// Fetches the branch `HEAD` names with `git fetch -- origin <branch>`
    /// (`HEAD` of the mirror for a branch with no commit yet); a mirror
    /// without that branch, an empty mirror, is nothing to pull. Then, in
    /// process: a commit the Vault holds already changes nothing; a mirror
    /// that only adds to the Vault's history moves `HEAD` forward; histories
    /// that both moved are joined path by path in one commit with both
    /// parents (`join the mirror's writes`). The work tree follows `HEAD` as
    /// after a write; a file a person changed is left as it is.
    ///
    /// # Errors
    ///
    /// `NOT_FOUND` without a mirror; `MIRROR_CONFLICT` naming the paths when
    /// a path was changed on both sides differently, with the Vault, its
    /// work tree, and git's index as they were; `ABORTED` (retryable) when
    /// the write lock is not free within `write_timeout_ms` or `HEAD` moved
    /// under the pull; the codes of [`push`](Self::push) for the network.
    pub(crate) fn pull(&self, _empty: Empty) -> Result<Empty, Error> {
        self.pull_now().context("pull from the mirror")?;
        self.clear_sync_failure();
        Ok(Empty::default())
    }

    /// [`pull`](Self::pull), then [`push`](Self::push). Against a mirror that holds
    /// nothing yet the pull is nothing to pull, so the push sends everything.
    ///
    /// # Errors
    ///
    /// Those of [`pull`](Self::pull) and [`push`](Self::push); a failed pull stops the
    /// call before the push.
    pub(crate) fn sync(&self, _empty: Empty) -> Result<Empty, Error> {
        self.pull_now().context("pull from the mirror")?;
        self.push_now().context("push to the mirror")?;
        self.clear_sync_failure();
        Ok(Empty::default())
    }

    /// The pull of `open` with `auto_sync`: only for a Vault that has a
    /// mirror. A failure is kept, not raised.
    pub(crate) fn pull_at_open(&self) {
        if self.auto_sync() {
            self.auto(Self::pull_now);
        }
    }

    /// The push after a write with `auto_sync`, called once the write lock
    /// is let go: only for a Vault that has a mirror. A failure is kept, not
    /// raised, and the write stays stored.
    pub(crate) fn push_after_write(&self) {
        if self.auto_sync() {
            self.auto(Self::push_now);
        }
    }

    /// Runs `step` if the Vault has a mirror and keeps its outcome.
    fn auto(&self, step: fn(&Vault) -> Result<(), Error>) {
        let outcome = match self.has_mirror() {
            Ok(false) => return,
            Ok(true) => step(self),
            Err(error) => Err(error),
        };
        self.record_sync(outcome);
    }

    fn has_mirror(&self) -> Result<bool, Error> {
        let snap = self.repos().snapshot(Repo::Main)?;
        let git = snap.repository().ok_or_else(repository_missing)?;
        Ok(repo::origin_of(git)?.is_some())
    }

    fn push_now(&self) -> Result<(), Error> {
        let snap = self.repos().snapshot(Repo::Main)?;
        let git = snap.repository().ok_or_else(repository_missing)?;
        if repo::origin_of(git)?.is_none() {
            return Err(no_mirror().into());
        }
        let reference = branch_of(git)?;
        git::push(self.path(), short_name(&reference))
    }

    fn pull_now(&self) -> Result<(), Error> {
        let lock = self.committer().lock().context("take the write lock")?;
        let snap = lock.snapshot(Repo::Main)?;
        let git = snap.repository().ok_or_else(repository_missing)?;
        if repo::origin_of(git)?.is_none() {
            return Err(no_mirror().into());
        }
        let reference = branch_of(git)?;
        let wanted = if snap.commit().is_some() {
            short_name(&reference)
        } else {
            "HEAD"
        };
        if !git::fetch(lock.root(), wanted).context("fetch")? {
            return Ok(());
        }
        let Some(theirs) = fetched_commit(git)? else {
            return Ok(());
        };
        let theirs_commit = git.find_commit(theirs).map_err(lib_fault)?;
        let Some(ours) = snap.commit() else {
            return lock.advance_to(&snap, theirs, "pull the mirror");
        };
        if ours == theirs || git.graph_descendant_of(ours, theirs).map_err(lib_fault)? {
            return Ok(());
        }
        if git.graph_descendant_of(theirs, ours).map_err(lib_fault)? {
            return lock.advance_to(&snap, theirs, "pull the mirror");
        }
        let ours_commit = git.find_commit(ours).map_err(lib_fault)?;
        let tree = join_trees(git, &ours_commit, &theirs_commit).context("join")?;
        let tree = git.find_tree(tree).map_err(lib_fault)?;
        let who = lock.signature()?;
        let joined = git
            .commit(
                None,
                &who,
                &who,
                &format!("{JOIN_MESSAGE}\n"),
                &tree,
                &[&ours_commit, &theirs_commit],
            )
            .map_err(lib_fault)?;
        lock.advance_to(&snap, joined, JOIN_MESSAGE)
    }
}

fn repository_missing() -> Fault {
    Fault::new(
        VaultErrorCode::Replication,
        "the repository of the Vault is not there",
    )
}

/// The branch name of a full reference name.
fn short_name(reference: &str) -> &str {
    reference.strip_prefix("refs/heads/").unwrap_or(reference)
}

/// The commit `git fetch` left in `FETCH_HEAD`, if it left one.
fn fetched_commit(git: &Repository) -> Result<Option<Oid>, Error> {
    let mut found = None;
    let read = git.fetchhead_foreach(|_, _, oid, merge| {
        if merge && found.is_none() {
            found = Some(*oid);
        }
        true
    });
    match read {
        Ok(()) => Ok(found),
        Err(err) if err.code() == ErrorCode::NotFound => Ok(None),
        Err(err) => Err(lib_fault(err).into()),
    }
}

/// The tree of the join of `ours` and `theirs`: the tree of `ours` with what
/// only `theirs` changed since their merge base. Histories with no merge base
/// join as if the base were empty.
///
/// # Errors
///
/// `MIRROR_CONFLICT` naming the paths both sides changed differently, or
/// that would put a file where the other side has a folder; nothing is
/// stored but objects no reference names.
fn join_trees(git: &Repository, ours: &Commit<'_>, theirs: &Commit<'_>) -> Result<Oid, Error> {
    let base = match git.merge_base(ours.id(), theirs.id()) {
        Ok(id) => Some(
            git.find_commit(id)
                .and_then(|commit| commit.tree())
                .map_err(lib_fault)?,
        ),
        Err(err) if err.code() == ErrorCode::NotFound => None,
        Err(err) => return Err(lib_fault(err).into()),
    };
    let ours_tree = ours.tree().map_err(lib_fault)?;
    let theirs_tree = theirs.tree().map_err(lib_fault)?;
    let mine = changes(git, base.as_ref(), &ours_tree)?;
    let others = changes(git, base.as_ref(), &theirs_tree)?;
    let mut conflicts: Vec<&str> = Vec::new();
    let mut update = TreeUpdateBuilder::new();
    for (path, state) in &others {
        match mine.get(path) {
            Some(same) if same == state => {}
            Some(_) => conflicts.push(path),
            None if state.is_some() && collides(&ours_tree, path)? => conflicts.push(path),
            None => match state {
                Some((id, mode)) => {
                    update.upsert(path, *id, *mode);
                }
                None => {
                    update.remove(path);
                }
            },
        }
    }
    if !conflicts.is_empty() {
        return Err(conflict(&conflicts).into());
    }
    Ok(update.create_updated(git, &ours_tree).map_err(lib_fault)?)
}

fn conflict(paths: &[&str]) -> Fault {
    let mut named = paths
        .iter()
        .take(NAMED_CONFLICTS)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > NAMED_CONFLICTS {
        named.push_str(&format!(" and {} more", paths.len() - NAMED_CONFLICTS));
    }
    Fault::new(
        VaultErrorCode::MirrorConflict,
        format!("the mirror and the Vault changed these paths differently: {named}"),
    )
}

/// What `tree` changed since `base` (nothing, when it is `None`), path by
/// path, for every file the two differ in.
fn changes(
    git: &Repository,
    base: Option<&Tree<'_>>,
    tree: &Tree<'_>,
) -> Result<BTreeMap<String, State>, Error> {
    let diff = git
        .diff_tree_to_tree(base, Some(tree), None)
        .map_err(lib_fault)?;
    let mut out = BTreeMap::new();
    for delta in diff.deltas() {
        let deleted = delta.status() == git2::Delta::Deleted;
        let file = if deleted {
            delta.old_file()
        } else {
            delta.new_file()
        };
        let Some(path) = file.path().and_then(Path::to_str) else {
            return Err(Fault::new(
                VaultErrorCode::Replication,
                "a history holds a path that is not UTF-8",
            )
            .into());
        };
        let state = (!deleted).then(|| (file.id(), file.mode()));
        out.insert(path.to_owned(), state);
    }
    Ok(out)
}

/// Whether a file at `path` cannot go into `ours`: a folder is where it
/// would be, or a file is where a folder above it would be.
fn collides(ours: &Tree<'_>, path: &str) -> Result<bool, Error> {
    let kind_at = |at: &str| -> Result<Option<Option<ObjectType>>, Error> {
        match ours.get_path(Path::new(at)) {
            Ok(entry) => Ok(Some(entry.kind())),
            Err(err) if err.code() == ErrorCode::NotFound => Ok(None),
            Err(err) => Err(lib_fault(err).into()),
        }
    };
    if kind_at(path)? == Some(Some(ObjectType::Tree)) {
        return Ok(true);
    }
    let mut from = 0;
    while let Some(slash) = path[from..].find('/') {
        let end = from + slash;
        match kind_at(&path[..end])? {
            Some(Some(ObjectType::Tree)) => {}
            Some(_) => return Ok(true),
            None => return Ok(false),
        }
        from = end + 1;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use buffa_types::google::protobuf::Empty;
    use scheda_api::v1::{
        CollectionDeclaration, DocumentUpsert, MirrorSetting, OpenOptions, VaultErrorCode,
    };

    use crate::git::{git, isolate_test_git_env};
    use crate::ports::Ports;
    use crate::vault::Vault;

    /// A scratch folder that is removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            isolate_test_git_env();
            let base =
                std::env::var_os("TEST_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
            let dir = base.join(format!("scheda-mirror-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn join(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }

        /// An empty bare repository on `main`.
        fn bare(&self, name: &str) -> PathBuf {
            let path = self.join(name);
            std::fs::create_dir_all(&path).unwrap();
            git()
                .current_dir(&path)
                .arg("init")
                .arg("--quiet")
                .arg("--bare")
                .arg("--initial-branch=main")
                .run()
                .unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn out(dir: &Path, args: &[&str]) -> String {
        let mut cmd = git().current_dir(dir);
        for arg in args {
            cmd = cmd.arg(arg);
        }
        cmd.run()
            .unwrap()
            .stdout_str()
            .unwrap()
            .trim_end()
            .to_owned()
    }

    fn url(path: &Path) -> String {
        format!("file://{}", path.display())
    }

    fn put(vault: &Vault, id: &str, raw: &str) {
        vault
            .upsert(
                "notes",
                DocumentUpsert {
                    id: Some(id.to_owned()),
                    raw: Some(raw.as_bytes().to_vec()),
                    note: Some(format!("write {id}")),
                    ..Default::default()
                },
            )
            .unwrap();
    }

    fn with_notes(vault: &Vault) {
        let notes: CollectionDeclaration = serde_json::from_value(
            serde_json::json!({"name": "notes", "schema": {"standard": {}}}),
        )
        .unwrap();
        vault.create_collection(notes).unwrap();
    }

    fn auto(path: &Path) -> Vault {
        Vault::open(
            path,
            OpenOptions::default()
                .with_auto_sync(true)
                .with_write_timeout_ms(5000),
            Ports::default(),
        )
        .unwrap()
    }

    fn set(vault: &Vault, mirror: &Path) {
        vault
            .set_mirror(MirrorSetting::default().with_url(url(mirror)))
            .unwrap();
    }

    #[test]
    fn a_push_that_fails_leaves_the_write_stored_and_the_failure_in_the_handle() {
        let scratch = Scratch::new("push-fails");
        let mirror = scratch.bare("m.git");
        let path = scratch.join("a");
        let vault = auto(&path);
        // No mirror yet: a write pushes nothing and there is nothing to keep.
        with_notes(&vault);
        assert!(vault.sync_failure().is_none());
        set(&vault, &mirror);
        put(&vault, "one.md", "1");
        assert!(vault.sync_failure().is_none());
        assert_eq!(
            out(&mirror, &["rev-parse", "refs/heads/main"]),
            out(&path, &["rev-parse", "HEAD"])
        );
        // The mirror moves ahead: another clone writes and pushes.
        let other = scratch.join("b");
        git()
            .arg("clone")
            .arg("--quiet")
            .arg("--")
            .arg(url(&mirror))
            .arg(&other)
            .run()
            .unwrap();
        std::fs::write(other.join("elsewhere"), "x").unwrap();
        for args in [
            &["add", "--all"][..],
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.test",
                "commit",
                "--quiet",
                "--message",
                "ahead",
            ][..],
            &["push", "--quiet", "origin", "HEAD:refs/heads/main"][..],
        ] {
            let mut cmd = git().current_dir(&other);
            for arg in args {
                cmd = cmd.arg(arg);
            }
            cmd.run().unwrap();
        }
        let commits = out(&path, &["rev-list", "--count", "HEAD"]);
        put(&vault, "two.md", "2");
        let kept = vault.sync_failure().expect("the push failed");
        assert!(kept.is(VaultErrorCode::MirrorAhead));
        assert_eq!(
            out(&path, &["rev-list", "--count", "HEAD"]),
            (commits.parse::<usize>().unwrap() + 1).to_string(),
            "the write is stored"
        );
        // A successful pull clears it, and the next write pushes again.
        vault.pull(Empty::default()).unwrap();
        assert!(vault.sync_failure().is_none());
        put(&vault, "three.md", "3");
        assert!(vault.sync_failure().is_none());
        assert_eq!(
            out(&mirror, &["rev-parse", "refs/heads/main"]),
            out(&path, &["rev-parse", "HEAD"])
        );
        // The mirror is removed: the write stays, the failure is kept, and a
        // later successful push clears it.
        std::fs::remove_dir_all(&mirror).unwrap();
        put(&vault, "four.md", "4");
        assert!(vault.sync_failure().is_some());
        let _ = scratch.bare("m.git");
        vault.push(Empty::default()).unwrap();
        assert!(vault.sync_failure().is_none());
    }

    #[test]
    fn a_pull_failure_at_open_neither_fails_open_nor_is_lost() {
        let scratch = Scratch::new("pull-fails");
        let mirror = scratch.bare("m.git");
        let path = scratch.join("a");
        let vault = auto(&path);
        with_notes(&vault);
        set(&vault, &mirror);
        drop(vault);
        std::fs::remove_dir_all(&mirror).unwrap();
        let reopened = auto(&path);
        let kept = reopened.sync_failure().expect("the pull failed");
        assert!(kept.is(VaultErrorCode::Replication));
        // The Vault works.
        put(&reopened, "a.md", "x");
        // A Vault without a mirror keeps nothing.
        let local = auto(&scratch.join("local"));
        assert!(local.sync_failure().is_none());
    }

    #[test]
    fn a_write_that_stores_nothing_or_goes_to_isolated_does_not_push() {
        let scratch = Scratch::new("no-push");
        let mirror = scratch.bare("m.git");
        let path = scratch.join("a");
        let vault = auto(&path);
        with_notes(&vault);
        let isolated: CollectionDeclaration = serde_json::from_value(serde_json::json!(
            {"name": "diary", "schema": {"standard": {}, "isolated": true}}
        ))
        .unwrap();
        vault.create_collection(isolated).unwrap();
        set(&vault, &mirror);
        put(&vault, "a.md", "x");
        let pushed = out(&mirror, &["rev-parse", "refs/heads/main"]);
        // An unreachable mirror shows a push at once; none is tried for an
        // isolated write or for bytes that change nothing.
        vault
            .set_mirror(
                MirrorSetting::default()
                    .with_url(url(&scratch.join("gone.git")))
                    .with_replace_existing(true),
            )
            .unwrap();
        put(&vault, "a.md", "x");
        vault
            .upsert(
                "diary",
                DocumentUpsert {
                    id: Some("d".to_owned()),
                    raw: Some(b"secret".to_vec()),
                    note: Some("diary".to_owned()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(vault.sync_failure().is_none());
        assert_eq!(out(&mirror, &["rev-parse", "refs/heads/main"]), pushed);
        // A write of the main repository does try, and fails.
        put(&vault, "b.md", "y");
        assert!(vault.sync_failure().is_some());
    }

    /// A commit on `parent` (none for the first) that puts or removes the
    /// files, in the repository at `repo`, without touching a work tree.
    fn commit_on(
        repo: &git2::Repository,
        parent: Option<&git2::Commit<'_>>,
        files: &[(&str, Option<&str>)],
    ) -> git2::Oid {
        let mut update = git2::build::TreeUpdateBuilder::new();
        for (path, text) in files {
            match text {
                Some(text) => {
                    let blob = repo.blob(text.as_bytes()).unwrap();
                    update.upsert(path, blob, git2::FileMode::Blob);
                }
                None => {
                    update.remove(path);
                }
            }
        }
        let base = match parent {
            Some(parent) => parent.tree().unwrap(),
            None => {
                let empty = repo.treebuilder(None).unwrap().write().unwrap();
                repo.find_tree(empty).unwrap()
            }
        };
        let tree = repo
            .find_tree(update.create_updated(repo, &base).unwrap())
            .unwrap();
        let who = git2::Signature::now("T", "t@example.test").unwrap();
        let parents: Vec<&git2::Commit<'_>> = parent.iter().copied().collect();
        repo.commit(None, &who, &who, "c", &tree, &parents).unwrap()
    }

    fn names(repo: &git2::Repository, tree: git2::Oid) -> Vec<String> {
        let mut out = Vec::new();
        repo.find_tree(tree)
            .unwrap()
            .walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
                out.push(format!("{dir}{}", entry.name().unwrap()));
                0
            })
            .unwrap();
        out
    }

    #[test]
    fn the_join_takes_each_side_per_path_and_drops_a_folder_the_mirror_emptied() {
        let scratch = Scratch::new("join-shapes");
        crate::repo::init_library().unwrap();
        let repo = git2::Repository::init_bare(scratch.join("r")).unwrap();
        let base = commit_on(
            &repo,
            None,
            &[
                ("dir/only", Some("1")),
                ("keep", Some("1")),
                ("both", Some("1")),
            ],
        );
        let base = repo.find_commit(base).unwrap();
        let ours = commit_on(
            &repo,
            Some(&base),
            &[("mine", Some("m")), ("both", Some("same"))],
        );
        let theirs = commit_on(
            &repo,
            Some(&base),
            &[
                ("dir/only", None),
                ("theirs", Some("t")),
                ("both", Some("same")),
            ],
        );
        let (ours, theirs) = (
            repo.find_commit(ours).unwrap(),
            repo.find_commit(theirs).unwrap(),
        );
        let tree = super::join_trees(&repo, &ours, &theirs).unwrap();
        assert_eq!(names(&repo, tree), ["both", "keep", "mine", "theirs"]);
    }

    #[test]
    fn a_file_where_the_other_side_has_a_folder_is_a_conflict_and_unrelated_histories_join() {
        let scratch = Scratch::new("join-collide");
        crate::repo::init_library().unwrap();
        let repo = git2::Repository::init_bare(scratch.join("r")).unwrap();
        let base = repo
            .find_commit(commit_on(&repo, None, &[("x", Some("1"))]))
            .unwrap();
        let ours = repo
            .find_commit(commit_on(&repo, Some(&base), &[("a/b", Some("folder"))]))
            .unwrap();
        let theirs = repo
            .find_commit(commit_on(&repo, Some(&base), &[("a", Some("file"))]))
            .unwrap();
        let error = super::join_trees(&repo, &ours, &theirs).unwrap_err();
        assert!(error.is(VaultErrorCode::MirrorConflict));
        // Two histories with no common commit join as if the base were empty.
        let left = repo
            .find_commit(commit_on(
                &repo,
                None,
                &[("same", Some("1")), ("l", Some("l"))],
            ))
            .unwrap();
        let right = repo
            .find_commit(commit_on(
                &repo,
                None,
                &[("same", Some("1")), ("r", Some("r"))],
            ))
            .unwrap();
        let tree = super::join_trees(&repo, &left, &right).unwrap();
        assert_eq!(names(&repo, tree), ["l", "r", "same"]);
        let clash = repo
            .find_commit(commit_on(&repo, None, &[("same", Some("2"))]))
            .unwrap();
        let error = super::join_trees(&repo, &left, &clash).unwrap_err();
        assert!(error.is(VaultErrorCode::MirrorConflict));
    }
}
