# Ready means ready

"Ready", "done", "we can start" claims that someone can build on this
without coming back. Make that claim only with a proof that ran.

## Before the claim

- Enumerate the consumer's real artifacts, not its requirements document:
  every schema, every file it writes, every call it makes. List them and
  mark each one verified or unverified. A surface not looked at is not a
  surface cleared.
- Run the smallest thing that exercises those artifacts end to end: a
  fixture, a throwaway test, a spike. Reasoning is not a proof.
- A done stage names the test or fixture that proves it. A checkbox
  ticked against my own plan text proves nothing.

## When there is no proof

Say what it is: "review found no blocker". Then name the surfaces that
were not checked, so the reader knows the risk being taken.

## A plan that crosses repositories

A plan for repo B derived from repo A's documents is a draft. Before it
is called complete, write A's real inputs against B's real API once.
Cheap now, a new stage later.
