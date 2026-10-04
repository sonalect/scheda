# Work the owner can stop

The owner must be able to end everything by stopping this session. A run
that survives that is a defect, not diligence.

## Never detach

Forbidden in a command, and in any script a command writes: `setsid`,
`nohup`, `disown`, `&` on a long job, `systemd-run`, `at`, `cron`, and
any re-exec whose purpose is to leave the session's process group.

The Bash tool's own `run_in_background` is not detaching: the session
tracks that task and can stop it. The command it runs must still obey
this rule and the time limit below.

```bash
# BAD — outlives the stop, the owner has to reboot
setsid ./run-variant.sh 750 &

# GOOD — dies with the session
./run-variant.sh 750
```

"The measurement would be lost" is not a reason. A lost measurement is
cheap. A job the owner cannot kill costs a reboot.

## Twenty minutes, then report

No single command may be expected to run longer than 20 minutes. Split
it: fewer targets, a smaller sample, one variant instead of three. Emit
progress at least once a minute, so a stalled run is visible at once
instead of hours later.

An experiment that costs an hour to answer one question is mis-sized.
Use the smallest input that can still answer it.

## Cold rebuilds are a stall

If `bazel` or `cargo` starts building dependencies you did not change,
the command is sandboxed or running outside the repo root: it cannot see
`~/.cache/bazel`, and Cargo gets a private target directory under `/tmp`.
Abort it. `/tmp` is a tmpfs, and filling it wedges the machine. Re-run
per `run-commands.md`, from the repo root and unsandboxed.

## Say what is running

Never report that no command was run while a process you started is
alive. Before claiming work has stopped, list the processes and show
that none remain.

## After a stop order

Stop means stop. Run nothing, wait for nothing, relaunch nothing,
restore nothing. Reply with status only.
