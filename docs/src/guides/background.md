# Keeping mirrors current in the background

Run weir as a background service and the mirrors stay a few minutes behind their providers, all
day, with nothing to schedule.

With Homebrew:

```text
brew services start weir
```

Without Homebrew:

```text
weir service install
```

Either one registers `weir daemon` with launchd on macOS or systemd on Linux. It starts now,
starts again at every login, and is restarted if it dies.

You can start it before you have added an account or tracked anything. It idles, looks again every
30 seconds, and picks up your first `weir track` on its own.

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
- **It picks up upgrades.** When its binary is replaced, by `brew upgrade` or otherwise, it exits
  at the start of its next cycle and the service manager starts the new version.

## Managing it

With Homebrew:

```text
brew services info weir      # is it loaded and running?
brew services stop weir
brew services restart weir
```

Without Homebrew:

```text
weir service status      # installed? running? when is the next cycle?
weir service stop        # until the next login, or `weir service start`
weir service start
weir service uninstall   # stop it and remove the service
```

Whichever started it, `weir status` and `weir service status` say whether the daemon is running
and when its next cycle is, and `weir watch --follow` shows each cycle as it happens.

One service manager owns the daemon. On a Homebrew install, `weir service install` and
`weir service start` tell you to use `brew services`, so two services are never registered for
the same daemon.

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
| `$(brew --prefix)/var/log/weir.log` | The daemon's own notes when Homebrew runs it: started, stopped, offline, woke up |
| `~/.weir/daemon.log` | The same, when `weir service install` set it up |
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
- **`--env` tokens** need the variable to be set in the service's environment, which neither
  `brew services` nor `weir service install` does for you. Prefer `--command` or the Keychain here.
