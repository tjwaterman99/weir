# Keeping mirrors current in the background

Run weir as a background service and the mirrors stay a few minutes behind their providers, all
day, with nothing to schedule.

```text
weir service install
```

That registers `weir daemon` with launchd on macOS or systemd on Linux. It starts now, starts
again at every login, and is restarted if it dies.

## What the daemon does

It runs the same sync as `weir sync`, on a loop.

- **Every five minutes** it asks each provider what changed and syncs that. The interval is the
  `interval_seconds` setting.
- **Checking is cheap.** Each listing is a conditional request. When nothing changed, the provider
  answers "not modified", which does not count against the rate limit, and weir runs no git
  commands at all.
- **After sleep or a network drop it catches up at once.** While offline it waits quietly.
- **It backs off when something is wrong.** After repeated failures it waits longer, up to half an
  hour. When a rate limit is nearly used up, it waits for the reset.
- **It re-reads everything each cycle.** `weir track`, `weir auth login` and `weir config set`
  take effect without a restart.
- **It stops cleanly.** On shutdown it stops the clones and fetches in flight, lets checkouts
  finish, and exits. An interrupted clone leaves nothing behind, and the next cycle redoes it.

## Managing it

```text
weir service status      # installed? running? when is the next cycle?
weir service stop        # until the next login, or `weir service start`
weir service start
weir service uninstall   # stop it and remove the service
```

`weir status` includes a line about the daemon, and `weir watch --follow` shows each cycle as it
happens.

## Syncing by hand while it runs

`weir sync` still works. It waits for the cycle in progress, then runs. Use it:

- right after `weir track`, if you do not want to wait for the next cycle;
- with `--all`, to fetch everything.

## Settings

| Setting | Default | Meaning |
|---|---|---|
| `interval_seconds` | `300` | How often the daemon checks the providers |
| `daemon_jobs` | `4` | How many repositories it works on at once |
| `pause_on_battery` | `false` | Wait while the machine runs on its battery |

```text
weir config set interval_seconds 120
```

## Where it writes

| File | What |
|---|---|
| `~/.weir/daemon.log` | The daemon's own notes: started, stopped, offline, woke up |
| `~/.weir/weir.log` | One line per change to a mirror, from every sync |

## On Linux

A systemd user service stops when you log out, unless lingering is on for your user:

```text
loginctl enable-linger
```

## Without the service

On a machine where you would rather not run a service, run `weir sync` from a scheduler. It waits
up to a minute for the network.

```text
# cron: every morning at 08:30
30 8 * * *  /opt/homebrew/bin/weir sync --quiet
```

You can also run `weir daemon` under any process supervisor of your own. Give its environment a
`PATH` that includes `git` and `weir` itself.

## Tokens under a service

A service does not have your shell's environment.

- **Keychain tokens** work as they are.
- **`--command` tokens** need the full path to the command, for example
  `/opt/homebrew/bin/gh auth token --user myself`.
- **`--env` tokens** need the variable to be set in the service's environment, which
  `weir service install` does not do for you. Prefer `--command` or the Keychain here.
