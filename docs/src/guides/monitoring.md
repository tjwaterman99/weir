# Knowing that it is working

## While a sync runs

In a terminal, `weir sync` keeps a status line at the bottom while it works: how many repositories
are done, the counts so far, the time taken, and what it is working on right now. A large clone
shows its size, so it does not look stuck.

```text
updated   github.com/acme/api  638bb2c -> df75e0b
[412/709] 3 cloned, 12 updated, 380 unchanged · 3m10s · monorepo (14.1 GB) 2m05s, web, +2
```

When its output is not a terminal, it prints a `progress 412/709` line every 30 seconds instead.

## From another terminal

```text
weir status           # says "Sync running: [412/709] ..." while one is in progress
weir watch            # follows the running sync until it ends, with the same live line
weir watch --follow   # keeps following the next ones too
```

This works for a sync started by the daemon or by a scheduler.

## Afterwards

```text
{{#include ../examples/out/log.txt}}
```

| Command | Shows |
|---|---|
| `weir log` | Past runs and what each one changed |
| `weir log --slow` | The ten slowest repositories of each run |
| `weir log --all` | Also the repositories that were fetched and had not changed |

`weir log --slow` is the place to look when a sync takes longer than you expect. Every run also
appends to `~/.weir/weir.log`, one timestamped line per change.

## When nobody is watching

Nobody watches a background sync, so weir makes a bad one hard to miss.

### `weir status` exits 1 when syncs have stopped succeeding

It warns when the last successful sync is older than `stale_after_hours` (36 by default), and it
shows the recent record:

```text
Last sync 2026-11-26T08:30:00Z (41h10m ago), took 2s: 1 failed, 12 unchanged
Last 7 runs: 5 ok, 2 with problems

WARNING: no sync has succeeded since 2026-11-24T15:20:00Z (41h ago; the limit is 36h).
```

Call it from a shell prompt, a menu bar item or a monitor. Set `stale_after_hours` to `0` to turn
the check off.

### `on_problem` runs a command when a sync ends with problems

```text
weir config set on_problem 'osascript -e "display notification \"$WEIR_SUMMARY\" with title \"weir\""'
```

The command gets:

| Where | What |
|---|---|
| standard input | the summary line, then one line per problem |
| `$WEIR_SUMMARY` | the summary, for example `1 skipped, 12 unchanged` |
| `$WEIR_PROBLEMS` | the problems, one per line |
| `$WEIR_RUN_ID` | the run's number, as `weir log` shows it |

The daemon runs it when a problem starts, not on every cycle the problem lasts. If the command
itself fails, weir says so and carries on.

## For scripts

`--json` works on `status`, `list`, `log` and `doctor`. `weir sync --json` prints one JSON object
per line as things happen: `start`, `plan`, one `repo` per repository worked on, then `finish`.

```text
{"event":"start","run_id":12,"dry_run":false}
{"event":"plan","jobs":2,"unchanged":380}
{"event":"repo","action":"updated","repo":"github.com/acme/api","old_head":"638b...","new_head":"df75...","message":null,"millis":412}
{"event":"repo","action":"unchanged","repo":"github.com/acme/web","old_head":"0a7a...","new_head":"0a7a...","message":null,"millis":388}
{"event":"finish","run_id":12,"dry_run":false,"counts":{"unchanged":381,"updated":1},"problems":[],"seconds":3,"ok":true}
```
