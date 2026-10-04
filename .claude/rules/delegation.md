# Delegation and model economy

The subscription has daily and weekly usage limits. Work on the cheapest
model that does it well. This rule is the owner's standing request to
delegate in the cases below; do not ask before doing so.

## Models

- **Sonnet** is the default main session. It implements, debugs, and
  writes docs.
- **Opus** is for design, plans, work-order documents, full reviews
  (`full-review.md`), concurrency and on-disk format questions, and
  debugging where Sonnet is stuck. The owner switches the session
  (`/model opus`, or `/model opusplan` for Opus in plan mode and Sonnet
  for execution).
- **Haiku** runs the project agents below for commands, lookups, and
  mechanical edits.

Never start an Opus subagent. When a task needs Opus, say so and let the
owner switch.

## Project agents (`.claude/agents/`)

| Agent | Model | Use for |
| - | - | - |
| `runner` | Haiku | `bazel test`, `cargo test` / `clippy`, generate, crate_universe repin, any long or noisy command; returns failures only |
| `scout` | Haiku | where X is defined / used, lists by criterion, summaries of large files or logs, a consumer's use of an API, dependency publish dates |
| `mechanic` | Haiku | renames across files, dependency sorting, exact replacements — fully specified, volume only |
| `implementer` | Sonnet | from an Opus session: a specified change across files, with tests and the docs pass |

Use `scout` instead of the built-in Explore and general-purpose agents:
they run on the main session's model.

## Who delegates what

- **Opus session:** small edits (one or two files, a few tool calls)
  itself; implementation larger than that to `implementer`; commands,
  searches, and mechanical edits to the Haiku agents.
- **Sonnet session or `implementer`:** implementation and docs itself;
  commands, searches, and mechanical edits to the Haiku agents.
- **Haiku agents** never delegate.

## Do not delegate when

- The task takes about three tool calls or fewer.
- The brief would be longer than the work.
- The task depends on decisions from this conversation that do not fit in
  a short brief.
- It is a full review, a design, or a work-order document.
- It writes `docs/` text or rustdoc. Haiku never writes these; Sonnet or
  Opus does. CHANGELOG entries are written by the session that made the
  change, since it already knows what changed.

## Brief

The agent starts with no conversation context. Give it absolute paths, the
exact change or question, the done criterion, and the report shape. It
loads `.claude/rules/` itself; name the rules that matter most for the
task. Launch independent Haiku tasks in parallel.

## After the agent returns

Verify it: read `git diff` for edits, and check the reported exit codes
for commands. Never report success on the agent's word alone
(`readiness.md`). If the result is wrong, send the findings back (paths,
what is wrong, the expected fix) rather than guessing what the agent
meant.

## Escalation

- A Haiku agent fails or returns a doubtful result: one retry with a
  sharper brief, then do the work in the calling session.
- Sonnet fails twice on the same problem: stop, report what was tried,
  and suggest the owner switch to Opus.
