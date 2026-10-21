# Small loop, one measurement

## No slow run without the owner's permission

A slow test runs only after the owner has allowed that run explicitly
(the owner's decision, 21 October 2026). Slow: a `memory::` or
`measure::` test, any test in a release build, a run of thousands of
operations, anything expected to take more than about 10 seconds. The
gate (`bazel test //...`) and the functional suites are not slow tests;
a functional test that takes more than a few seconds is a defect
(`gate-tests-behaviour.md`).

Ask with: what runs, its size, the expected time, and the question it
answers. A subagent does not ask the owner: it stops and reports what it
would run to the session that delegated the work, which asks.

## The rule of the loop

Work is done on the smallest input that shows the behaviour. A heavy
measurement runs once, at the end, to report a number, never to debug.
The owner's decision (21 October 2026), after an agent ran a test of
100 000 writes seven times in an hour, most runs after a release rebuild
of 10 to 17 minutes, when the first run's numbers already showed the
cause.

## The loop

- A rule is tested on synthetic data first: a simulation of a million
  writes of a pure function runs in milliseconds.
- Real I/O: a few hundred to a few thousand operations, a debug build,
  `--no-default-features` when the feature is not involved.
- A release build with every feature costs 10 to 17 minutes. Do not
  rebuild it for a measurement that does not need the features.

## The measurement

- Heavy: 10 000 operations or more, a release build, a memory target, or
  over a minute. It is `#[ignore]`, runs alone, and runs only after the
  small loop is green.
- Before it runs, estimate its time from the small run: the time of one
  operation, how it grows with the data, times the count. Over 20 minutes
  is mis-sized (`stoppable-work.md`).
- When it fails or is slower than the estimate, do not run it again. Find
  the cause on a small input, in the numbers it printed, or in the code;
  run it once more after the fix. A second failure: stop and report.

```text
# BAD — rerun the 100 000-write test, with a rebuild, to see what happens
# GOOD — 2 000 writes show the time of a write growing with the Documents;
#        the cause is found there, and the big run happens once
```
