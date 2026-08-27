# 04. Files and git

Document 4 of the Scheda package. Version 0.23, 27 August 2026.

Normative rules for bytes on disk, the git working tree, the write lock, `commit`, Remote, `push` / `pull` / `sync`, and how `open` creates or clones a Vault. Terms are in `02`. Collection kinds and identity are in `03`. Isolation and “no network by default” are restated here where they touch git; the rest is `09`. Codes: `STO`, `GIT`. Phase **core** unless marked otherwise.

An assertion without a check is not a requirement. This package does not use Git LFS.

---

## 1. Files are the source of truth

**STO-1. Recoverable from the tree.** Anything the library cannot rebuild from Vault text files must not exist in Scheda. The Index is not truth. *Check:* delete the Index, open the Vault with another copy of an implementation, `reindex`; the set of Records and each Record's content match the files.

**STO-2. Readable without the library.** Records are UTF-8 without BOM. `document`: Markdown with YAML 1.2 between `---`, or YAML 1.2 throughout. `journal`: JSONL, one RFC 8785 (JSON Canonicalization Scheme) object per line, `\n`, no comments. Object keys are sorted by UTF-16 code unit; numbers by ECMAScript `NumberToString`; strings escaped as the RFC states. An `integer` field is the decimal digits (`1`), not `1.0`. `singleton`: YAML 1.2 in the file `record.yaml` (SCH-6). There is no Schema key that picks a format: JSON is a subset of YAML 1.2, so an imported JSON file parses unchanged once it carries that name. *Check:* a fixture Vault opens in an editor and in `git` with no Scheda; `git diff` after changing a field shows that field's text, not a binary dump. *Check:* a journal line with an `integer` field holds `1`, not `1.0`.

**Exception (**later**, ISO-5):** `{vault}/.isolated/` with `encrypt` stores isolated Record files as age ASCII armor. Those files are still UTF-8 text and `git diff`able as armor, not as YAML/JSONL. Plaintext is recovered with the `secret` (Scheda or `age -d`), not by opening the blob in an editor. The main Vault tree is never this exception.

**STO-3. Journal lines are not rewritten.** Same as MOD-16: no update/delete API. On read/`check`, a line that is not a single JSON object, or a final line without `\n`, is an error. Scheda does not silently truncate or rewrite the segment to “fix” it. *Check:* flip bytes in the middle of a closed segment → `check` fails; `replace` on the collection → error.

**STO-4. One writer per Vault working tree.** A process that will mutate the Vault holds a write lock. Many readers are allowed. A reader is not blocked by the writer and may see a state slightly older than the writer's latest write. *Check:* a second `open` for write fails while the first writer holds the lock; a reader can `get` during `add` without tearing the file (STO-5).

The lock is an OS file lock on `{vault}/.scheda/write.lock` (not a pid-file,
not under `index/`). It is released when the process dies because the kernel
closes the descriptor. Stale pid-files are not the mechanism: they would
survive a crash. Locks do not travel with `git push` / `git pull`. Two clones
are two working trees: each may have a writer. NFS/SMB lock semantics are not
relied on; Vaults on those shares are outside **core**.

**STO-5. A failed write is not the new truth.** Validation failure, a full disk, or a killed process must not leave a half-written `document` or `singleton` as the file at the Record path, and must not leave a `journal` segment cut in the middle of a line as an accepted Record. *Check:* interrupt `replace` / `add`; after reopen, the set of accepted Records equals the last successful write. Technique for `document`/`singleton`: write a sibling temp file in the same directory, `fsync`, rename over the target. Technique for `journal`: write one complete line including `\n`; a tail without `\n` is not a Record (STO-3).

---

## 2. The Vault is a git working tree

**GIT-1. Root is a repository.** The Vault directory is a git working tree. The Index lives **inside** that tree under `.scheda/` and is gitignored; it is not committed. A Remote is optional. `init` and `clone` write `core.longpaths` in the Vault's local git config (`commit` and `discard` set it on an older tree that lacks it) so Windows can stage a 255-byte filename component when the absolute path is longer than MAX_PATH. Unix ignores the key. *Check:* `git status` at the Vault root lists collection files and `.gitignore`, and does not list Index files; a Vault with no `remote` still `commit`s. *Check:* after `open` of a new Vault, local `core.longpaths` is true. *Check:* `add` of a 252-character `id` (on-disk `{id}.md` = 255 bytes) commits.

**GIT-2. Autocommit unless a transaction is open.** Outside a transaction, each mutating call (`add`, `replace`, `create_collection`) writes the working tree and then creates one git commit. The default message is `scheda`. Isolated Record paths commit in `{vault}/.isolated/` nested git; Schema stubs and non-isolated files commit in the main repository. If that commit fails, those writes are discarded: only paths Scheda recorded are restored to HEAD (new files are deleted). This is not `git reset --hard` of the whole tree.

A transaction is the only supported batch. There is no `OpenOptions` field that turns autocommit off for the handle. The caller chooses: each mutating call is its own commit (no `begin`), or many writes then one `commit`. A consumer that appends many journal lines must hold a transaction; otherwise each line is one commit.

A transaction is a property of one library handle while that process holds the write lock (STO-4). It is not a flag on the working tree. `open` always starts with no transaction. Dropping the handle ends the transaction without a git commit. Recorded paths stay in `{vault}/.scheda/tx.log` (next to `write.lock`, not under `index/`) so a later process can restore them. `open` / `get` / `check` / `reindex` do not restore them. The next `begin`, `discard`, or mutating call outside a transaction restores those paths to HEAD. `reindex` does not delete `{vault}/.scheda/tx.log`.

The CLI does not expose `begin` / `commit` / `discard`. Each mutating CLI command autocommits. Batch writes are a library-handle concern.

`begin` takes this handle into a transaction: leftover Scheda writes are discarded first; `invalid` if a transaction is already open on this handle. Inside a transaction, mutating calls do not commit. `commit(message)` ends the transaction and creates one git commit per repository that has recorded paths (main tree and/or `{vault}/.isolated/`), with that message; a side with no recorded paths is skipped. Those two commits are not one atomic unit. It is `invalid` if no transaction is open, or if there is nothing to commit on either side. `discard` restores those paths to HEAD in the repository that owns each path, reloads Schema, and rebuilds the Index (including the nested Index when `.isolated/` exists); it is allowed without an open transaction to clear crash leftovers. Staging is only those recorded paths. It must not `git add` the whole tree in a way that stages junk at the Vault root, and must not stage isolated paths into the parent git.

*Check:* several inserts after `begin` without `commit` → `git log` unchanged; one `commit` → one new commit whose paths are those writes. *Check:* `add` without `begin` → one new commit with message `scheda`. *Check:* 10⁴ journal `add` after `begin`, one `commit` → `git log` grows by one. *Check:* drop the handle mid-transaction after complete journal lines plus a truncated tail; reopen without `begin` / `discard`; `get` returns the complete Records and not the truncated line (STO-5). *Check:* drop the handle mid-transaction; reopen; `commit` is `invalid`; the next `add` without `begin` discards the leftover and the abandoned `id` is gone.

Who owns untracked files the human created in the Vault is the caller's problem; Scheda does not auto-add them.

**GIT-3. History of a Record.** `Collection.history(id, …)` is the same identity path as `get` (IDX-10): validate the slug, then git commits that touched that file (hash, time, author, message, bytes at that commit). Newest first: `offset` 0 is the latest commit. Does not ask the Index which ids exist. The caller does not pass a path. `journal`: IDX-5 map to the segment, then git of that file. That is a VCS address of bytes, not a Schema `revision` field, and not journal `find` append order. Isolated Records use the nested git. *Check:* two commits editing one `document`; the API's snapshots match `git show` of that file; first `history` page entry is the newer commit. *Check:* delete `.scheda/`; `history` of a fixture `document` still matches `git show`.

Line-level “this journal Record's first commit” is **later** (requires blaming JSONL). **Core** `history(id)` on `journal` is git history of the segment file.

**GIT-4. Conflicts are git's, shown to a person.** Scheda does not three-way-merge YAML. Conflict markers in a `document` make that Record unreadable until the person resolves them; `check` fails on that file. Two closed journal segments that never overlapped do not conflict. Two writers who both appended to the same **open** segment on different clones can conflict on that file; rotation does not make that safe. *Check:* a conflicted `document` is rejected on read/`check`; merging two segments with disjoint new lines and unique `id`s can be a clean git merge of those files.

**GIT-5. Several Vaults per process.** `open` on distinct roots is allowed. Search, `find`, and edges do not cross roots unless the caller issues a combined request (API in `07`). *Check:* a Record in Vault A never appears in `find`/`search` on Vault B.

**GIT-6. No Git LFS.** Collection files stay ordinary git blobs. Segment size caps (MOD-10, default in `05`) exist so hosts that reject large files (~100 MiB) and slow `git diff` are not hit. *Check:* Schema/default cap is below 50 MiB; the library never configures `filter.lfs`.

---

## 3. Remote, open, pull, push

A **Remote** is the one optional git remote Scheda uses on the Vault repository, named `origin`. GitHub is an example host, not a Scheda type. Auth is git's (credentials, SSH, `GIT_ASKPASS`); Scheda does not store host tokens in the Vault. Extra remotes in git config are ignored.

**GIT-7. Detect Remote without the network.** `open` of an existing working tree reads git config locally. `Vault.info` reports `api.v1.VaultInfo.remote`: `origin` when that remote exists, otherwise empty. That is not a fetch. *Check:* `open` with network disabled still reports `origin` on `info` if it is configured.

**GIT-8. Network is opt-in.** `add`, `replace`, and `create_collection` inside a transaction, and `commit` (the local commit itself), do not call a Remote. Outside a transaction those mutating calls autocommit (GIT-2); then `auto_sync` may `push` (GIT-10). Network happens only for: `open` that clones (`remote_repo` on a missing path); `pull` / `push` / `sync`; auto-pull on `open` when `auto_sync` is on; auto-push after a successful local commit when `auto_sync` is on. *Check:* those writes with `auto_sync` off and no explicit remote call succeed with network disabled.

**GIT-9. `open` creates, clones, or attaches.** One public call (`07` LIB-13). There is no `init` or `clone` method name.

| `open(path)` | Path | Network | Git |
|---|---|---|---|
| existing Vault working tree | that path | only if `auto_sync` (GIT-10) | attach; `remote_repo` ignored |
| missing + `remote_repo` | new directory | yes | `git clone`; not `git init` plus a later remote add as the only path |
| missing, no `remote_repo` | empty or new directory | no | `git init`, no Remote |
| exists but is not a Vault | that path | no | `invalid` |

Isolated nested git (**later**) is created by Scheda under `{vault}/.isolated/`; `remote_repo` there is forbidden (ISO-6). *Check:* `open` of a missing path with `remote_repo` produces a Vault whose `origin` is that URL; `open` of a missing path without `remote_repo` has no Remote; `open`+`remote_repo` on an existing Vault succeeds and does not change `origin`.

**GIT-10. `auto_sync`.** Option at `open` (default **off**). When on **and** a Remote exists:

1. **Auto-pull on `open`:** fetch, then update `HEAD` only if the incoming commits are a **fast-forward**. If not fast-forward (diverged branches, would need merge or rebase), `open` fails with code `not_fast_forward`. Scheda does not merge on open. The working tree is not left in a conflicted state as a successful open.
2. **Auto-push after a local commit:** same as an explicit `push` after a successful `commit` or autocommit (GIT-12).

If `auto_sync` is on and there is no Remote, `open` and `commit` still succeed locally; remote steps are skipped (GIT-8). Mutating calls inside a transaction never pull or push.

*Check:* `auto_sync` off, Remote has new commits → `open` does not change `HEAD`. `auto_sync` on, Remote is a fast-forward → `HEAD` matches the Remote after `open`. `auto_sync` on, histories diverged → `open` fails, working tree has no conflict markers from Scheda. `auto_sync` on, `commit`, Remote reachable → commit is on the Remote.

**GIT-11. Explicit remote operations.** Always available when a Remote exists, independent of `auto_sync`:

- `pull` — fetch + fast-forward only; otherwise `not_fast_forward` (no merge).
- `push` — push the current branch to `origin`. There is no remote-name argument.
- `sync` — `pull` then `push`. If pull is not a fast-forward, push is not attempted.

*Check:* with `auto_sync` off, `sync` still publishes and fetches; diverged histories → `sync` fails before push.

**GIT-12. `push` failure does not roll back the local commit.** The caller retries `push` or `sync`. *Check:* Remote unreachable after `commit`; `HEAD` is the new commit; a later `push` publishes it.

Hooks on the Vault repository are the consumer's. They must not assume they may recurse into the same process's write lock.

---

## 4. Index location (git)

**GIT-13. Index is in the Vault tree and gitignored.** Creating a Vault writes `.gitignore` entries for `.scheda/` and `.isolated/`. The main Index lives under `{vault}/.scheda/`. Isolated Index (**later**) lives under `{vault}/.isolated/.scheda/`, gitignored in the nested repo. Callers do not choose an Index directory. `{vault}/.scheda/tx.log` is the crash journal of uncommitted Scheda paths (GIT-2), not Index; `reindex` does not delete it. If a consumer puts another cache inside the working tree, it must also be gitignored. *Check:* `git check-ignore` / `git status` after writes does not show Index files; deleting `.scheda/` does not delete Records (STO-1), including `{vault}/.isolated/` Records.

Format and rebuild semantics: `06`.

---

## 5. What this document does not decide

Schema language and the numeric segment size default — `05`.

How `get` finds a journal `id` without a full scan — `06`.

The journal line form is STO-2 (RFC 8785). The library writes that form on `add` (`07` LIB-18).

Optional age encryption of `{vault}/.isolated/` — `09` (**later**).

Multi-master without STO-4, CRDT, custom YAML merge — deferred (`10`).
