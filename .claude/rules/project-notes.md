# Lasting knowledge lives in the repository

What the owner decided, what is deferred, and what was measured must
survive a new machine, a moved folder, or a new workspace. The agent's
local memory (`~/.claude/projects/…/memory/`) survives none of these, so it
is not where lasting knowledge goes.

## Where

- **A standing rule** (how to work, a design principle, an owner
  preference): a file in `.claude/rules/`. Loaded in every session.
- **Project state, deferred work, measurements, references**: a file in
  `.claude/notes/`, listed in `.claude/notes/README.md`. Not loaded by
  itself.
- **A decision about the product**: the document it belongs to in
  `docs/` (a requirement, a work-order stage, `10` deferrals).
- **Local memory**: only what is true of this machine alone (a path, a
  running service). Never the only copy of anything above.

## When

- Read `.claude/notes/README.md` before planning, before a design, and
  when the owner names a topic that may be deferred ("we discussed",
  "later", "the old numbers").
- Write a note when the owner asks to keep something for later, defers a
  decision, or a run produces numbers worth comparing against. Update the
  note in place; do not add a second note on the same topic.
- English. A note has no frontmatter: a title, the facts, **Why** and
  **How to apply** when it guides work.
