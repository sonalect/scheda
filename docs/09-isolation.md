# 09. Isolation and perimeter

Document 9 of the Scheda package. Version 0.11, 27 August 2026.

What must not leak: network by default (**core**); isolated collections under `{vault}/.isolated/` (gitignored nested git, never the main Remote), sealed fields, and optional encryption of that nested tree (**later**). Terms are in `02`. Schema marks are in SCH-15. Remote traffic is in `04`. Codes: `ISO`. Phase **later** unless marked **core** or **agent**.

An assertion without a check is not a requirement.

---

## 1. Network

**ISO-1. No network unless opted in.** With `auto_sync` off and no `open` that clones / `pull` / `push` / `sync`, Scheda does not open outbound connections for `open`, writes, `commit`, lexical `reindex`, or `search` without an embedder. A network embedder is supplied by the caller (IDX-8). Git credentials are not stored in the Vault (`04`). *Check (**core**):* those operations succeed with the network disabled; a stub embedder that does not call out leaves the audit empty.

`open` with `remote_repo` on a missing path and `auto_sync` are explicit exceptions (GIT-8, GIT-10). They still do not put host tokens in Record files.

---

## 2. Isolated collection (**later**)

The Vault is **one directory**. Isolated Records are not a second `open` path elsewhere on disk.

This section is implemented in `scheda` without encryption: nested git, `0600`, no Remote, nested Index, `check` over that tree, and `journal` as well as `document`. The phase tag stays **later** for a reason: a binding does not need `.isolated/` to pass **core**, **search**, or **agent**. Section 3 (age encryption) is not implemented.

**ISO-2. Nested git under `{vault}/.isolated/`, not a sibling Vault.** Schema `isolated: true` (SCH-15) means Record files live at `{vault}/.isolated/{name}/`. Isolated Records use the same fixed names under that directory (`{id}.md`, journal segments, `record.yaml`). The Schema file stays in the main tree at `{vault}/{name}/.schema.yaml` (also `.yml` / `.json`) so `open` of a clone of the main Vault still lists the collection (ISO-3). First `open` that creates a Vault writes `.gitignore` entries for `.scheda/` and `.isolated/` (GIT-13).

`{vault}/.isolated/` is a git working tree Scheda creates on the first isolated write (`create_collection` with `isolated`, or `add` on that collection). Local `commit` and `history` of isolated Record paths use that nested repository. File mode: owner-only (`0600` on Unix). On other systems Scheda does not change ACLs; the nested git and gitignore remain the perimeter. It is not required to exist for the main Vault to `open`, or for `find` / `search` on non-isolated collections. Deleting `{vault}/.scheda/` does not delete `.isolated/` (Index is disposable; isolated Records are not).

`check` fails if the **parent** git index tracks any path under `.isolated/` (`git add -f` is not a perimeter). *Check (**later**):* fixture Vault with `.isolated/` gitignored; `find` / `search` on a non-isolated collection return no isolated Records; `open` succeeds if `.isolated/` is missing; `git log` in `.isolated/` grows after `add` on an isolated collection; a tracked `.isolated/` path fails `check`.

The caller uses the **same** `Vault` handle. `collection(name)` for an isolated name is the opt-in for those Records. `create_collection` with `isolated` writes the Schema stub in the main tree and creates the nested git if needed. Edges and `search` on a non-isolated collection do not read `.isolated/`. Isolated Index is `{vault}/.isolated/.scheda/` (gitignored in the nested repo), not the main `{vault}/.scheda/`. `Vault.reindex` rebuilds the main Index from main-tree Records; if `.isolated/` exists, it also rebuilds the nested Index. Isolated Record writes commit in the nested git (GIT-2); the Schema stub commit is the main repository.

**ISO-6. Nested isolated git never has a Remote.** Scheda must not clone `.isolated/` from a URL, must not `git remote add` there, and must reject `push`, `pull`, `sync`, and `auto_sync` against that nested repository (`unsupported` or `invalid`). `Vault.push` publishes only the main tree. `check` fails if the nested git has remotes configured (a human added `origin` outside Scheda). *Check (**later**):* `push` after an isolated insert does not call the network for `.isolated/` and does not upload those files; a fixture nested repo with `origin` fails `check`.

**ISO-3. Agent sees isolation.** `info` (AGT-1) lists isolated collections as isolated and omits `schema.fields`. Tools that scan the main Index or non-isolated collections cannot `get` / `search` sealed data. Reaching isolated Records is `collection(name)` on this handle (and `secret` if the nested tree is encrypted). *Check (**agent**):* agent tools that are not bound to that collection never return a fixture sealed value.

**ISO-4. Main Index has no sealed bytes.** The derived Index under `{vault}/.scheda/` does not copy sealed fields or isolated Records (IDX-4). *Check (**later**):* grep of `{vault}/.scheda/` does not contain a fixture sealed string.

---

## 3. Encryption of `{vault}/.isolated/` (**later**)

Encryption is optional. Unencrypted `.isolated/` remains valid (`0600` + no Remote). The main Vault tree is never encrypted by Scheda.

**ISO-5. `encrypt` when `.isolated/` is first created.** `OpenOptions.encrypt` (boolean, default false) applies only when Scheda creates `{vault}/.isolated/`. If true, every isolated Record file on disk is ciphertext. `open` of the **Vault** does not require `secret`; main collections work. Isolated Record I/O (`collection` of an isolated name, `add` / `get` / …) **requires** the same secret when the nested tree is encrypted; without it those calls fail (`crypto`). There is no plaintext isolated Record file in `.isolated/`. If `.isolated/` already exists, `encrypt` on `open` is ignored. *Check (**later**):* `encrypt=true` on first isolated write, then isolated `get` without `secret` fails; with `secret`, `add` / `get` round-trip; `git show` of the isolated file is not YAML/JSONL plaintext; main `get` still works without `secret`.

**ISO-11. Marker so isolated I/O knows.** After encrypted creation of `.isolated/`, the nested git tree contains a committed marker that is not a Record (path `{vault}/.isolated/.encrypted`). Isolated I/O reads it and demands `secret`. Unencrypted creation must not write that marker. An empty encrypted `.isolated/` (no Records yet) still fails isolated I/O without `secret`. The marker is not under the gitignored Index directory. *Check (**later**):* encrypt create, no `add`, isolated `collection` / `get` without `secret` → `crypto`; unencrypted `.isolated/` without `secret` succeeds.

**ISO-7. One secret, not a key pair in the API.** The caller passes either a **passphrase** or an **age identity** (private key file) as `OpenOptions.secret`. Scheda does not take separate “encrypt key” and “decrypt key” arguments. The secret is never written into `.isolated/` or the main git tree. Scheda is not a KMS. *Check (**later**):* grep of the Vault working tree does not contain the passphrase or identity bytes used in the test.

**ISO-8. Format is age v1, armored.** On-disk files follow [age v1](https://age-encryption.org/v1) ([C2SP](https://github.com/C2SP/C2SP/blob/main/age.md)): **ASCII armor** (PEM: `BEGIN AGE ENCRYPTED FILE`), UTF-8, `\n` only, no BOM. Not raw Base64 and not age's binary encoding. Payload AEAD is **ChaCha20-Poly1305**. Passphrase wrapping is age's **scrypt**; identity wrapping is **X25519**. No cipher agility: implementations do not offer AES-GCM as an alternative. The contract is the **format**, not one library: a binding may use `age`/`rage` (Rust), `filippo.io/age` (Go), typage (TypeScript), pyrage (Python), Jagged (Java), or another conformant implementation. A file written by any binding must decrypt with the `age` CLI (`age -d`) using the same secret, and must round-trip with every other binding. *Check (**later**):* `age -d` on a fixture file with the test identity/passphrase yields the Record bytes Scheda would `get`; a file from one binding decrypts in another; a homemade Base64 blob is rejected on isolated I/O / `check`.

**ISO-9. Whole file, write like STO-5.** Encrypt the entire `document`, journal **segment**, or `singleton` after canonical plaintext is built. Write a sibling temp file, `fsync`, rename. Do not encrypt selected YAML fields inside a plaintext file. *Check (**later**):* no header/body plaintext appears in the nested git blob.

**ISO-10. Isolated Index.** The Index for `.isolated/` is `{vault}/.isolated/.scheda/`, mode owner-only, gitignored in the nested repo, and must not land in the main Vault Index (ISO-4). *Check (**later**):* `git status` in `.isolated/` does not list Index files.

---

## 4. What this document does not decide

Which collections a given consumer marks isolated — that consumer's Schema.

How an OS other than Unix spells `0600` — Unix is `0600`; other systems leave ACLs unchanged (the nested git and gitignore are the perimeter).

Backup of `.isolated/` — local git history of that nested tree on this machine; it is not published with the main Vault Remote.

Argon2id instead of scrypt — out of scope while the format is age v1.

Which crate or package a binding vendors — that binding, as long as ISO-8 round-trips.
