# 09. Isolation and perimeter

Document 9 of the Scheda package. Version 0.5, 23 August 2026.

What must not leak: network by default (**core**); isolated collections outside the main git Vault, sealed fields, and optional encryption of the isolated root (**later**). Terms are in `02`. Schema marks are in SCH-15. Remote traffic is in `04`. Codes: `ISO`. Phase **later** unless marked **core** or **agent**.

An assertion without a check is not a requirement.

---

## 1. Network

**ISO-1. No network unless opted in.** With `auto_sync` off and no `clone` / `pull_remote` / `push_remote` / `sync_remote`, Scheda does not open outbound connections for `open`, writes, `commit`, lexical `reindex`, or `search` without an embedder. A network embedder is supplied by the caller (IDX-8). Git credentials are not stored in the Vault (`04`). *Check (**core**):* those operations succeed with the network disabled; a stub embedder that does not call out leaves the audit empty.

`clone` and `auto_sync` are explicit exceptions (GIT-8, GIT-10). They still do not put host tokens in Record files.

---

## 2. Isolated collection (**later**)

**ISO-2. Another git root, not the main working tree.** Schema may declare a collection `isolated: true` with a directory outside the main Vault git tree (SCH-15). That directory **is** a git working tree: `init`, local `commit`, and `history` work so changes are tracked. File mode: owner-only (`0600` on Unix; equivalent ACL on other systems). It is not required to exist for the main Vault to `open`, `find`, or `search`. *Check (**later**):* fixture with two roots; queries on the main Vault return no isolated Records and do not fail if the isolated directory is missing; `git log` in the isolated root grows after `commit` on that handle.

The caller opens the isolated root as its own handle (second `open` / `init`) if it needs those Records. Edges and `search` do not cross from the main Vault into that handle unless the caller combines two handles (**later**, same as GIT-5).

**ISO-6. Isolated root never has a Remote.** Scheda must not `clone` an isolated root from a URL, must not `git remote add`, and must reject `push_remote`, `pull_remote`, `sync_remote`, and `auto_sync` on that handle (`unsupported` or `invalid`). `check` on an isolated root fails if git remotes are configured (a human added `origin` outside Scheda). *Check (**later**):* `push_remote` on the isolated handle fails and does not call the network; `clone(url)` into an isolated path is rejected; a fixture isolated repo with `origin` fails `check`.

**ISO-3. Agent sees isolation.** The main `catalog` (AGT-1) lists isolated collections as isolated and does not include their field values. Tools bound to the main Vault cannot `find_one` / `search` sealed data. A consumer that wants the agent to see the isolated root must open that root on purpose (and pass the secret if the root is encrypted). *Check (**later**, with **agent**):* agent tools on the main Vault never return a fixture sealed value.

**ISO-4. Main Index has no sealed bytes.** The derived Index of the main Vault does not copy sealed fields or isolated Records (IDX-4). *Check (**later**):* grep of main Index files does not contain a fixture sealed string.

---

## 3. Encryption of the isolated root (**later**)

Encryption is optional. Unencrypted isolated roots remain valid (`0600` + no Remote). The main Vault is never encrypted by Scheda.

**ISO-5. `encrypt` on `init`.** `init` of an isolated root takes `encrypt` (boolean, default false). If true, every Record file on disk is ciphertext. `open` of that root **requires** the same secret; without it the operation fails (`crypto`). There is no plaintext Record file in the working tree. *Check (**later**):* `init(..., encrypt=true)` then `open` without a secret fails; with the secret, `insert_one` / `find_one` round-trip; `git show` of the file is not YAML/JSONL plaintext.

**ISO-11. Marker so `open` knows.** After `init(..., encrypt=true)`, the isolated git tree contains a committed marker that is not a Record (path is the binding's, e.g. `.scheda/encrypted`). `open` reads it and demands `secret`. Unencrypted `init` must not write that marker. An empty encrypted root (no Records yet) still `open`-fails without `secret`. *Check (**later**):* encrypt `init`, no `insert_one`, `open` without `secret` → `crypto`; unencrypted isolated `open` without `secret` succeeds.

**ISO-7. One secret, not a key pair in the API.** The caller passes either a **passphrase** or an **age identity** (private key file). Scheda does not take separate “encrypt key” and “decrypt key” arguments. The secret is never written into the isolated git tree. Scheda is not a KMS. *Check (**later**):* grep of the isolated working tree does not contain the passphrase or identity bytes used in the test.

**ISO-8. Format is age v1, armored.** On-disk files follow [age v1](https://age-encryption.org/v1) ([C2SP](https://github.com/C2SP/C2SP/blob/main/age.md)): **ASCII armor** (PEM: `BEGIN AGE ENCRYPTED FILE`), UTF-8, `\n` only, no BOM. Not raw Base64 and not age's binary encoding. Payload AEAD is **ChaCha20-Poly1305**. Passphrase wrapping is age's **scrypt**; identity wrapping is **X25519**. No cipher agility: implementations do not offer AES-GCM as an alternative. The contract is the **format**, not one library: a binding may use `age`/`rage` (Rust), `filippo.io/age` (Go), typage (TypeScript), pyrage (Python), Jagged (Java), or another conformant implementation. A file written by any binding must decrypt with the `age` CLI (`age -d`) using the same secret, and must round-trip with every other binding. *Check (**later**):* `age -d` on a fixture file with the test identity/passphrase yields the Record bytes Scheda would `find_one`; a file from one binding decrypts in another; a homemade Base64 blob is rejected on `open`/`check`.

**ISO-9. Whole file, write like STO-5.** Encrypt the entire `document`, journal **segment**, or `singleton` after canonical plaintext is built. Write a sibling temp file, `fsync`, rename. Do not encrypt selected YAML fields inside a plaintext file. *Check (**later**):* no header/body plaintext appears in the committed blob.

**ISO-10. Isolated Index.** The Index for an encrypted isolated root stays outside that git tree, mode owner-only, and must not land in the main Vault Index (ISO-4). *Check (**later**):* isolated git status does not list Index files.

---

## 4. What this document does not decide

Which collections a given consumer marks isolated — that consumer's Schema.

How an OS other than Unix spells `0600` — the binding, with the same “owner only” intent.

Backup of the isolated root — local git history of armored blobs on that machine; it is not published with the main Vault Remote.

Argon2id instead of scrypt — out of scope while the format is age v1.

Which crate or package a binding vendors — that binding, as long as ISO-8 round-trips.
