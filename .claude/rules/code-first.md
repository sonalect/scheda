# Code first, documents after

Behaviour ships before prose. On a change, the code and its tests come
first, the gates run, and only then the documents move — **once**.

## Order

1. Code and tests.
2. `cargo test` and `cargo clippy` green.
3. Documents: one pass, in the same commit or the next one. In Scheda the
   normative text lives in `docs/03`–`09`, `13`, and the work-order
   documents (`11`, `12`, `15`, …): tick a stage box only when its
   *Check:* passes under `bazel test //...`.

## Budget

Prose is about a tenth of the effort on a change, not most of it. One
pass over the affected files, then stop.

## Do not

- Edit a document before the behaviour it describes exists.
- Open the same document a second and a third time to polish wording.
- Treat document version bumps (`Document N … Version 0.x`),
  cross-reference sweeps, and renumbering as their own activity. Fold
  them into the one pass.
- Ask the owner to approve wording. Ship the code; the document follows.
- Answer "what is the next step?" with a document task while code waits.

## Exception

The owner asked for a plan, a design, a review, or a work-order document.
Then prose **is** the deliverable and this rule does not apply.
`full-review.md` and `readiness.md` keep priority.
