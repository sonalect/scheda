# 09. Isolation and perimeter

Document 9 of the Scheda package. Version 0.16, 31 August 2026.

What must not leak: network by default (**core**); isolated collections under `{vault}/.isolated/` (gitignored nested git, sealed fields) and encryption of a collection's Record files, which is a separate property and applies to any one collection, isolated or not (**later**). Terms are in `02`. Schema marks are in SCH-15 (isolation) and SCH-16 (encryption). Remote traffic is in `04`. Codes: `ISO`. Phase **later** unless marked **core** or **agent**.

An assertion without a check is not a requirement.

---

## 1. Network

**ISO-1. No network unless opted in.** With `auto_sync` off and no `open` that clones / `pull` / `push` / `sync`, Scheda does not open outbound connections for `open`, writes, `commit`, lexical `reindex`, or `search` without an embedder. A network embedder is supplied by the caller (IDX-8). Git credentials are not stored in the Vault (`04`). *Check (**core**):* those operations succeed with the network disabled; a stub embedder that does not call out leaves the audit empty.

`open` with `remote_repo` on a missing path and `auto_sync` are explicit exceptions (GIT-8, GIT-10). They still do not put host tokens in Record files.

---

## 2. Isolated collection (**later**)

The Vault is **one directory**. Isolated Records are not a second `open` path elsewhere on disk.

This section is implemented in `scheda` without encryption: nested git, `0600`, no main-Remote traffic, `check` over that tree, and `journal` as well as `document`. Isolated Records are in the one Vault Index and are queryable by name; there is no nested Index (ISO-10 retired). The phase tag stays **later** for a reason: a binding does not need `.isolated/` to pass **core**, **search**, or **agent**. Section 3 (age encryption) is a separate property and a separate Cargo feature; a build without `encrypt` still passes this section.

**ISO-2. Nested git under `{vault}/.isolated/`, not a sibling Vault.** Schema `isolated: true` (SCH-15) means Record files live at `{vault}/.isolated/{name}/`. Isolated Records use the same fixed names under that directory (`{id}.md`, journal segments, `record.yaml`). The Schema file stays in the main tree at `{vault}/{name}/.schema.yaml` (also `.yml` / `.json`) so `open` of a clone of the main Vault still lists the collection (ISO-3). First `open` that creates a Vault writes `.gitignore` entries for `.scheda/` and `.isolated/` (GIT-13).

`{vault}/.isolated/` is a git working tree Scheda creates on the first isolated write (`create_collection` with `isolated`, or `add` on that collection). Local `commit` and `history` of isolated Record paths use that nested repository. File mode: owner-only (`0600` on Unix). On other systems Scheda does not change ACLs; the nested git and gitignore remain the perimeter. It is not required to exist for the main Vault to `open`, or for `find` / `search` on non-isolated collections. Deleting `{vault}/.scheda/` does not delete `.isolated/` (Index is disposable; isolated Records are not).

`check` fails if the **parent** git index tracks any path under `.isolated/` (`git add -f` is not a perimeter). *Check (**later**):* fixture Vault with `.isolated/` gitignored; `find` / `search` on a non-isolated collection return no isolated Records; `open` succeeds if `.isolated/` is missing; `git log` in `.isolated/` grows after `add` on an isolated collection; a tracked `.isolated/` path fails `check`.

The caller uses the **same** `Vault` handle. `collection(name)` for an isolated name is the opt-in for those Records. `create_collection` with `isolated` writes the Schema stub in the main tree and creates the nested git if needed. A query that does not name an isolated collection returns nothing of theirs. Isolated Records are in the one Index at `{vault}/.scheda/`, which is owner-only for that reason (ISO-4); there is no nested Index. `Vault.reindex` rebuilds that one Index from every collection it can read, isolated included. Isolated Record writes commit in the nested git (GIT-2); the Schema stub commit is the main repository.

**ISO-6. The nested git has its own Remote or none, and never the main one (relaxation **not implemented**).** The first sentence below holds today. The relaxation — a Remote of the nested tree's own — is written here as the decided contract, and `scheda` does not implement it yet: it still refuses `git remote add` there and `check` still fails on a nested `origin`. The work is `12` P16. Until it lands, treat the second half as specification ahead of code. `Vault.push`, `pull`, `sync`, and `auto_sync` publish and fetch the **main tree only**: they must not call the network for `.isolated/`, whatever that nested repository has configured. Reaching a nested Remote is a separate, explicit call; there is no option that makes one traverse the other. The nested tree may have a Remote of its own, because the two audiences are not the same one: a consumer can put shared material on a shared Remote and its own encrypted private state on a personal one. `check` does not fail on a nested `origin`. What a consumer must weigh, and record in its own documents, is that ciphertext in a pushed history is permanent: a promise of the form "after this row is retired the identity behind it cannot be recovered" holds physically while the file sits on one disk that can be destroyed, and after a push it holds only while a key stays secret, because git history does not forget. *Check (**later**):* `push` after an isolated insert does not call the network for `.isolated/` and does not upload those files, with and without a nested `origin`; a fixture nested repo with `origin` passes `check`.

**ISO-3. Agent sees isolation and encryption.** `info` (AGT-1) lists isolated collections as isolated, encrypted ones as encrypted, and omits `schema.fields`. A tool that scans broadly cannot reach sealed data: a query with no collection named excludes isolated collections, even though the one Index holds them (ISO-4), so surfacing them takes naming one. Reaching isolated Records is `collection(name)` on this handle; reaching an encrypted collection's Records also needs the handle secret (ISO-5). *Check (**agent**):* agent tools that are not bound to that collection never return a fixture sealed value.

**ISO-4. No Index holds encrypted bytes; sealed values sit in an owner-only Index.** There is one Index per Vault, `{vault}/.scheda/`, and it covers every collection the build can read, isolated included. Sealed field values are therefore inside it, and that is safe for one reason only: the directory is owner-only (`0700` directories, `0600` files, on Unix) and gitignored, so it is readable by the owner and by nobody else, and it never leaves the machine (IDX-4, GIT-13). A mode wider than that fails `check`.

No Index holds anything from an **encrypted** collection. The Index is plaintext on disk — owner-only, but plaintext — and an encrypted collection exists precisely because reading the disk as the owner is not supposed to be enough. `reindex` skips those collections and no write updates an entry for them.

State the consequence plainly, because a consumer may rely on the opposite: a backup that copies the Vault and deliberately skips `.isolated/` still carries those field values, in the Index. Deleting `{vault}/.scheda/` is safe (IDX-1) and is what removes them.

*Check:* grep of `{vault}/.scheda/` does not contain a fixture string written into an encrypted collection, before or after `reindex`. *Check:* the Index directory is `0700` and its files `0600` after a write and after `reindex`; `chmod 0755` on it fails `check` with that path.

---

## 3. Encryption of a collection (**later**)

Encryption is a property of **one collection**, declared by its Schema (SCH-16), and it is independent of `isolated`: all four combinations are valid. A consumer can want ciphertext on disk for material it fully intends to publish — precisely so that a `push` is the backup and no directory has to be copied by hand — and can want a plain nested tree for something merely local. Unencrypted `.isolated/` remains valid (`0600`). Scheda does not encrypt a Vault, a tree, or a directory: it encrypts the Record files of the collections that ask for it, and nothing else in the working tree.

Encryption is offered for `document` and `singleton`. An encrypted `journal` is `invalid` when the Schema is read (SCH-16). Since no Index holds an encrypted collection (ISO-4), the ways in are `get` by a known `id` and the `documents()` / `ids()` walk (LIB-25).

The capability is an opt-in Cargo feature, `encrypt`. A build without it must refuse by code and must never hand a caller armor bytes dressed as a Record (LIB-2, LIB-15).

**ISO-5. One secret on the handle, for every encrypted collection.** `OpenOptions.encryption` (`api.v1.Encryption`) carries the secret for this handle; the message requires `passphrase` or `identity`. Unset: nothing is decrypted and nothing is encrypted. `open` of the **Vault** never requires it, and plain collections work without it. I/O on an encrypted collection — `add`, `replace`, `get`, `documents()`, `check` of that collection — **requires** it, and without it those calls fail with `crypto`; `ids()` reads file names and works without it. There is no plaintext Record file in an encrypted collection at any moment, including during a write. One handle carries one secret: two encrypted collections under different secrets need two handles. *Check (**later**):* `add` then `get` round-trips with the secret; `get` without it and `get` with a wrong one both fail with `crypto`; `ids()` without it succeeds; `git show` of the Record blob is not YAML plaintext; a plain collection on the same Vault round-trips with no secret at all.

**ISO-7. One secret, not a key pair in the API.** The caller passes either a **passphrase** or an **age identity** (private key file) as `OpenOptions.encryption` (`api.v1.Encryption`). Scheda does not take separate “encrypt key” and “decrypt key” arguments. The secret is never written into the Vault working tree, `.isolated/`, or any Index. It must not arrive as a command-line argument, because argv is readable by every process on the machine: the CLI takes it from a file path or the environment (`14`). Scheda is not a KMS.

**The two forms do not cost the same, and the difference decides designs.** A passphrase is wrapped by age scrypt (ISO-8), and the derivation runs on **every Record encrypted or decrypted** — not once per handle. Age picks a work factor deliberately near a second, because that is what makes a passphrase expensive to guess; a caller cannot opt out of it. So a `documents()` walk of an encrypted collection under a passphrase costs about that per Record, and a hundred Records cost a hundred times it. An identity is X25519: an ephemeral key agreement, microseconds, no derivation. A one-off command may carry a passphrase; anything that reads more than a handful of Records, and any long-running process on a latency budget, carries an identity. Scheda does not cache a derived key to paper over this: caching it would mean holding the unwrapped key for the life of the handle, which is a different security decision than the caller made when they chose a passphrase. *Check:* grep of the Vault working tree, `.isolated/`, and the Index does not contain the passphrase or identity bytes used in the test.

**ISO-8. Format is age v1, armored.** On-disk files follow [age v1](https://age-encryption.org/v1) ([C2SP](https://github.com/C2SP/C2SP/blob/main/age.md)): **ASCII armor** (PEM: `BEGIN AGE ENCRYPTED FILE`), UTF-8, `\n` only, no BOM. Write always stores `\n` only: CR is dropped even when the encrypting library would emit `\r\n` (the `age` crate on Windows). A file that still contains `\r` is not armor (`crypto` on read and on `check`). Not raw Base64 and not age's binary encoding. Payload AEAD is **ChaCha20-Poly1305**. Passphrase wrapping is age's **scrypt**; identity wrapping is **X25519**. No cipher agility: implementations do not offer AES-GCM as an alternative. The contract is the **format**, not one library: a binding may use `age`/`rage` (Rust), `filippo.io/age` (Go), typage (TypeScript), pyrage (Python), Jagged (Java), or another conformant implementation. A file written by any binding must decrypt with the `age` CLI (`age -d`) using the same secret, and must round-trip with every other binding. *Check:* `age -d` on a fixture file with the test identity/passphrase yields the Record bytes Scheda would `get`; a homemade Base64 blob and a file that still contains `\r` are refused with `crypto` on read and on `check`.

**ISO-9. Whole file, write like STO-5.** Encrypt the entire `document` or `singleton` after the canonical plaintext is built, then write a sibling temp file, `fsync`, and rename (STO-5). The bytes that reach the temp file are already ciphertext: a plaintext Record file must not exist even briefly. Do not encrypt selected YAML fields inside a plaintext file — a partly encrypted file is neither readable without the library nor secret. *Check (**later**):* no header or body plaintext appears in the git blob, in the temp file, or anywhere under the Vault.

**ISO-10. Isolated Index. Retired.** There was a separate Index for `.isolated/` at `{vault}/.isolated/.scheda/`. It is gone: one Index per Vault holds every collection the build can read, and an owner-only mode on that one directory carries what the separate one carried (ISO-4). Nothing creates, reads, or writes a nested Index; `reindex` removes a leftover one. An older Vault needs no migration step — an absent journal `id` map is answered by a segment scan, which is slower and never wrong.

**ISO-11. The Schema is the marker.** Whether a collection's files are ciphertext is recorded in one place, its Schema (SCH-16), and nowhere else: there is no marker file, and `{vault}/.isolated/.encrypted` is not written or read. Flipping the key on a collection that already holds Record files is `invalid` and leaves those files untouched: honouring it would mean rewriting every one of them, and the caller may not even hold the secret needed to read them first. Encrypting a collection that already has content is an explicit migration by the consumer — read out, create the new collection, write in — not a Schema edit. *Check (**later**):* setting the key on a non-empty collection is `invalid` and the files are byte-identical afterwards; clearing it on a non-empty collection is `invalid` too; no `.encrypted` path exists anywhere in a fixture Vault.

---

## 4. What this document does not decide

Which collections a given consumer marks isolated or encrypted, and which of the four combinations each one is in — that consumer's Schema.

Where a consumer keeps the secret, and how it survives a lost machine — that consumer's problem, and Scheda is not a KMS (ISO-7). What Scheda owes is that a file is age v1 (ISO-8), so the `age` CLI and any conformant implementation can read it without Scheda.

How an OS other than Unix spells `0600` — Unix is `0600`; other systems leave ACLs unchanged (the nested git and gitignore are the perimeter).

Backup of `.isolated/` — local git history of that nested tree on this machine, plus whatever Remote of its own the consumer gives it (ISO-6). It is never published by the main Vault's `push` / `pull` / `sync`.

Argon2id instead of scrypt — out of scope while the format is age v1.

Which crate or package a binding vendors — that binding, as long as ISO-8 round-trips.
