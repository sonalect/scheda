# Fresh session and effort level

Two pieces of session advice to the owner: when to start clean, and when
to change the effort level.

## Fresh session

A long session carries old logs, file dumps, and decisions from finished
tasks. They cost tokens on every turn and pull new work toward old
context. When that happens, tell the owner and recommend a clean start.

### When to suggest it

At the start of a new request, check:

- **Unrelated task.** The new request does not depend on anything done
  earlier in this session: another area of the code, another repo, or a
  question after the previous task was committed or dropped.
- **Already summarized.** The context was compacted at least once, and
  the new task is not a continuation of the summarized work.
- **Heavy leftovers.** Large test logs, file dumps, or review output from
  finished tasks fill most of the conversation.
- **Drift.** I mixed up an earlier decision with the current one, or the
  owner had to correct me for using stale context.

One signal from the list together with a new, unrelated task is enough.
Do not suggest it when the new task builds on this session's work
(follow-up fix, the next stage of the same plan, the commit of what was
just changed).

### How

One short note in the reply, before starting the work:

1. Why: which signal fired, in one line.
2. Recommendation: `/clear` (or a new session); `.claude/rules/` and
   memory load again on their own.
3. Handoff: if anything from this session matters for the next one
   (branch, uncommitted changes, an open decision), give it as a short
   brief the owner can paste into the new session. Save lasting facts to
   `.claude/notes/` first (`project-notes.md`).

Then do what the owner says. If he continues here, do the task; the
suggestion is advice, not a gate.

## Effort level

Levels, low to high: `low`, `medium`, `high`, `xhigh`, `max`. The owner
sets them (`/effort`, or the `/model` menu). Effort is separate from the
model choice in `delegation.md`; both spend the same usage limits.

| Level | Task |
| - | - |
| `low` | lookups, running a command, a one-line edit, a factual answer |
| `medium` | routine change in known code, docs pass, mechanical edits, small tests |
| `high` | multi-file feature, debugging with an unknown cause, proto/API change |
| `xhigh` | design, plan, work-order document, full review, concurrency, locks, on-disk format, Index invariants |
| `max` | stuck after `xhigh`, or a subtle bug that two attempts did not fix |

Suggest **up** when the second attempt at the same fix failed, when the
task turned out to touch concurrency, on-disk format, a public contract,
or security below `xhigh`, or when the owner corrected my reasoning.
Suggest **down** during a run of lookups or mechanical edits at `high` or
above, or when what remains is routine (tests, docs pass, commit).

One line, before the work or when the signal fires: the current level if
I can tell it, the recommended one, and why. Then carry on at the current
level unless the owner switches.

## Do not

- Repeat a suggestion for the same task after the owner declined it.
- Suggest a fresh session in the middle of a task, unless drift already
  caused a mistake.
- Suggest a level change on every turn.
- Refuse or delay work until the owner restarts or switches.
