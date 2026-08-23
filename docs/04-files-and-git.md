# 04. Files and git

Document 4 of the Scheda package. Version 0.5, 23 August 2026.

Normative rules for bytes on disk, the git working tree, the write lock, `commit`, Remote, `push_remote` / `pull_remote` / `sync_remote`, and `clone`. Terms are in `02`. Collection kinds and identity are in `03`. Isolation and “no network by default” are restated here where they touch git; the rest is `09`. Codes: `STO`, `GIT`. Phase **core** unless marked otherwise.

An assertion without a check is not a requirement. This package does not use Git LFS.

---

## 1. Files are the source of truth

**STO-1. Recoverable from the tree.** Anything the library cannot rebuild from Vault text files must not exist in Scheda. The Index is not truth. *Check:* delete the Index, open the Vault with another copy of an implementation, `reindex`; the set of Records and each Record's content match the files.

**STO-2. Readable without the library.** Records are UTF-8 without BOM. `document`: Markdown with YAML 1.2 between `---`, or YAML 1.2 throughout. `journal`: JSONL, one object per line, `\n`, no comments. `singleton`: JSON or YAML 1.2 as Schema says. *Check:* a fixture Vault opens in an editor and in `git` with no Scheda; `git diff` after changing a field shows that field's text, not a binary dump.

**Exception (**later**, ISO-5):** an isolated root with `encrypt` stores Record files as age ASCII armor. Those files are still UTF-8 text and `git diff`able as armor, not as YAML/JSONL. Plaintext is recovered with the `secret` (Scheda or `age -d`), not by opening the blob in an editor. The main Vault is never this exception.

**STO-3. Journal lines are not rewritten.** Same as MOD-16: no update/delete API. On read/`check`, a line that is not a single JSON object, or a final line without `\n`, is an error. Scheda does not silently truncate or rewrite the segment to “fix” it. *Check:* flip bytes in the middle of a closed segment → `check` fails; `replace_one` on the collection → error.

**STO-4. One writer per Vault working tree.** A process that will mutate the Vault holds a write lock. Many readers are allowed. A reader is not blocked by the writer and may see a state slightly older than the writer's latest write. *Check:* a second `open` for write fails while the first writer holds the lock; a reader can `find_one` during `insert_one` without tearing the file (STO-5).

The lock is an OS file lock on a lockfile the implementation owns (typically inside `.scheda/`, not a pid-file). It is released when the process dies because the kernel closes the descriptor. Stale pid-files are not the mechanism: they would survive a crash. Locks do not travel with `git push` / `git pull`. Two clones are two working trees: each may have a writer. NFS/SMB lock semantics are not relied on; Vaults on those shares are outside **core**.

**STO-5. A failed write is not the new truth.** Validation failure, a full disk, or a killed process must not leave a half-written `document` or `singleton` as the file at the Record path, and must not leave a `journal` segment cut in the middle of a line as an accepted Record. *Check:* interrupt `replace_one` / `insert_one`; after reopen, the set of accepted Records equals the last successful write. Technique for `document`/`singleton`: write a sibling temp file in the same directory, `fsync`, rename over the target. Technique for `journal`: write one complete line including `\n`; a tail without `\n` is not a Record (STO-3).

---

## 2. The Vault is a git working tree

**GIT-1. Root is a repository.** The Vault directory is a git working tree. The Index lives **inside** that tree under `.scheda/` and is gitignored; it is not committed. A Remote is optional. *Check:* `git status` at the Vault root lists collection files and `.gitignore`, and does not list Index files; a Vault with no `remote` still `commit`s.

**GIT-2. `commit` is the caller's.** Scheda writes the working tree. It does not create a git commit on `insert_one` or `replace_one`. `commit(message)` stages the paths this process wrote (records and Schema it changed) and creates one commit with the caller's message. It must not `git add` the whole tree in a way that stages junk at the Vault root. *Check:* several inserts without `commit` → `git log` unchanged; one `commit` → one new commit whose paths are those writes.

Who owns untracked files the human created in the Vault is the caller's problem; Scheda does not auto-add them.

**GIT-3. History of a path.** For a `document` or `singleton` path, the library can return git commits that touched that path: hash, time, author, message, bytes at that commit. That is a VCS address of bytes, not a Schema `revision` field. *Check:* two commits editing one `document`; the API's snapshots match `git show`.

Journal history is history of segment files. Line-level “this Record's first commit” is **later** (requires blaming JSONL). **Core** must still return segment-file history like any other path.

**GIT-4. Conflicts are git's, shown to a person.** Scheda does not three-way-merge YAML. Conflict markers in a `document` make that Record unreadable until the person resolves them; `check` fails on that file. Two closed journal segments that never overlapped do not conflict. Two writers who both appended to the same **open** segment on different clones can conflict on that file; rotation does not make that safe. *Check:* a conflicted `document` is rejected on read/`check`; merging two segments with disjoint new lines and unique `id`s can be a clean git merge of those files.

**GIT-5. Several Vaults per process.** `open` on distinct roots is allowed. Search, `find`, and edges do not cross roots unless the caller issues a combined request (API in `07`). *Check:* a Record in Vault A never appears in `find`/`search` on Vault B.

**GIT-6. No Git LFS.** Collection files stay ordinary git blobs. Segment size caps (MOD-10, default in `05`) exist so hosts that reject large files (~100 MiB) and slow `git diff` are not hit. *Check:* Schema/default cap is below 50 MiB; the library never configures `filter.lfs`.

---

## 3. Remote, clone, pull, push

A **Remote** is a git remote on the Vault repository (`origin` by default). GitHub is an example host, not a Scheda type. Auth is git's (credentials, SSH, `GIT_ASKPASS`); Scheda does not store host tokens in the Vault.

**GIT-7. Detect Remote without the network.** `open` of an existing working tree reads git config locally and reports whether a Remote exists (`has_remote`, remote names). That is not a fetch. *Check:* `open` with network disabled still reports `origin` if it is configured.

**GIT-8. Network is opt-in.** `insert_one`, `replace_one`, `commit` (the local commit itself), and lexical `reindex` do not call a Remote. Network happens only for: `clone`; `pull_remote` / `push_remote` / `sync_remote`; auto-pull on `open` when `auto_sync` is on; auto-push after `commit` when `auto_sync` is on. *Check:* those writes with `auto_sync` off and no explicit remote call succeed with network disabled.

**GIT-9. `init` vs `clone` vs `open`.**

| Call | Path | Network | Git |
|---|---|---|---|
| `open(path)` | existing working tree | only if `auto_sync` (GIT-10) | read remotes locally |
| `init(path)` | empty or new directory | no | `git init`, no Remote |
| `clone(url, path)` | new directory (or `init`/`open` with `remote_repo=url` when the path must not already be a repo) | yes | `git clone`; not `git init` plus a later remote add as the only path |

If `path` already exists as a repository and the caller passed `remote_repo`, that is a failure (`invalid`), not a silent clone on top. Isolated roots (**later**) use `init` only; `clone` / `remote_repo` there is forbidden (ISO-6). *Check:* `clone` of a fixture URL produces a Vault whose `origin` is that URL; `init` has no Remote; `open`+`remote_repo` on an existing repo fails.

**GIT-10. `auto_sync`.** Option at `open` / `clone` (default **off**). When on **and** a Remote exists:

1. **Auto-pull on `open`:** fetch, then update `HEAD` only if the incoming commits are a **fast-forward**. If not fast-forward (diverged branches, would need merge or rebase), `open` fails with code `not_fast_forward`. Scheda does not merge on open. The working tree is not left in a conflicted state as a successful open.
2. **Auto-push after `commit`:** same as an explicit `push_remote` after a successful local commit (GIT-12).

If `auto_sync` is on and there is no Remote, `open` and `commit` still succeed locally; remote steps are skipped (GIT-8). `insert_one` / `replace_one` never pull or push.

*Check:* `auto_sync` off, Remote has new commits → `open` does not change `HEAD`. `auto_sync` on, Remote is a fast-forward → `HEAD` matches the Remote after `open`. `auto_sync` on, histories diverged → `open` fails, working tree has no conflict markers from Scheda. `auto_sync` on, `commit`, Remote reachable → commit is on the Remote.

**GIT-11. Explicit remote operations.** Always available when a Remote exists, independent of `auto_sync`:

- `pull_remote` — fetch + fast-forward only; otherwise `not_fast_forward` (no merge).
- `push_remote` — push the current branch to the Remote (name optional, default `origin`).
- `sync_remote` — `pull_remote` then `push_remote`. If pull is not a fast-forward, push is not attempted.

*Check:* with `auto_sync` off, `sync_remote` still publishes and fetches; diverged histories → `sync_remote` fails before push.

**GIT-12. `push_remote` failure does not roll back the local commit.** The caller retries `push_remote` or `sync_remote`. *Check:* Remote unreachable after `commit`; `HEAD` is the new commit; a later `push_remote` publishes it.

Hooks on the Vault repository are the consumer's. They must not assume they may recurse into the same process's write lock.

---

## 4. Index location (git)

**GIT-13. Index is in the Vault tree and gitignored.** `init` writes a `.gitignore` entry for `.scheda/`. The Index (and later additional indexes) live under `{vault}/.scheda/`. Callers do not choose an Index directory. If a consumer puts another cache inside the working tree, it must also be gitignored. *Check:* `git check-ignore` / `git status` after writes does not show Index files; deleting `.scheda/` does not delete Records (STO-1).

Format and rebuild semantics: `06`.

---

## 5. What this document does not decide

Schema language and the numeric segment size default — `05`.

How `find_one` finds a journal `id` without a full scan — `06`.

Canonical JSON of a line for hashing — `07`.

Optional age encryption of an isolated root — `09` (**later**).

Multi-master without STO-4, CRDT, custom YAML merge — deferred (`10`).
