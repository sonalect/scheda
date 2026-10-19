# No workarounds

The owner wants no workaround, hack, or shortcut behind a design or its
code. A solution that works by luck is one: a race left open, a timing
assumption nobody derived, a foreign error classified by its message text,
SQL or another language built by pasting values into text, a guess about
how slowly or how often a caller does something.

## Before work starts

When a plan, a decision already taken, or a library's behaviour leads to
one, above all one that can race, tell the owner before writing the code:
what it is, the evidence (a source line, a test, a measurement), and the
change that removes it, numbered as a proposal (`owner-review.md`). Do not
start the work and mention it afterwards.

```text
# BAD — prune versions "older than one hour" and hope no reader is slower
# GOOD — show that Lance prunes by creation time, that a reader can lose
#        files, and propose the rule that removes the race, before L2
```

## Do not

- Hide a known race behind a retry, a sleep, or a margin that nothing
  bounds.
- Branch on the text of an error from a library; branch on its type, or
  check the input first so the error cannot be the caller's.
- Ship a fallback path that is weaker than the main one without saying so.
