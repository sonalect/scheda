# Consumers outside this repository

Scheda is a general-purpose library for many projects, other people's
included. Knowqore (`github.com/sonalect/knowqore`, usually cloned next to
this repository as `../knowqore`) is its **anchor consumer**: the first
user, the motivation and the source of requirements for current work. It
is not the only one, and Scheda is not built for it alone.

## Requirements from the anchor consumer

- Generalize a Knowqore need into a library capability any consumer could
  use. The API, names, defaults, and docs never mention Knowqore or its
  domain (Lessons, learn, the writer role).
- A Knowqore example may motivate a decision in a work-order document;
  the normative text states the general rule.
- When a Knowqore need and a clean general design disagree, say so to the
  owner instead of bending the library toward one caller.

## When a change can break a consumer

A change to a public name, a signature, an error `code`, a default, the
on-disk Vault / Schema / Index format, locking, threads, or the freshness
of `find` / `search` is consumer-visible — for every consumer, known or
not.

Before calling such a change done:

1. Write the effect into `CHANGELOG.md` under `Changed` / `Removed`, in
   words any caller can act on.
2. Bump per `.claude/skills/versioning/SKILL.md` at release: breaking →
   minor while major is 0.
3. Check the anchor consumer as a real-world sample: `git grep -n
   "scheda::" ../knowqore/rust` (or ask `scout`), and tell the owner
   which of its call sites change. Knowqore passing is a sample, not
   proof that other consumers are safe.

Pin used by Knowqore today: git tag, features `encrypt`, `find`, `search`,
`simd`; several processes on one Vault; calls from sync code and from
inside `tokio::task::spawn_blocking`; its own `Embedder` / `Reranker`.

Do not edit a consumer's repository from this session unless the owner
asks. `readiness.md` § "A plan that crosses repositories" applies.
