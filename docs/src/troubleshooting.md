# Troubleshooting

Start with `weir status`. It names the accounts, what is tracked, the last sync, and every mirror
that needs attention.

```text
{{#include examples/out/status-problems.txt}}
```

## A mirror is `skipped`: "has modified files"

Someone edited a tracked file in the mirror. weir will not overwrite it. In the mirror:

```text
git status                 # see what changed
git stash                  # keep the changes for later, or
git checkout -- <file>     # discard them
```

Then run `weir sync`. If the change was meant to be real work, move it to a
[worktree](guides/worktrees.md).

## A mirror is `healed` every time

Something keeps checking out a branch in the mirror: a script, an editor, an agent. The branch is
kept each time, so nothing is lost, but find what does it and point it at a worktree.

## A mirror is `failed`

The message says why. The common ones:

| Message | Cause and fix |
|---|---|
| `is a clone of <url>, not of <url>` | The directory holds a different repository. Move it out of the way, or delete it, and sync again. |
| `exists and is not a git repository` | A plain directory is where the mirror should be. Move it. |
| `git fetch failed: ... Authentication failed` | The account cannot read this repository. Check the token's scopes, and for an organization with single sign-on, that the token is authorized for it. |
| `git checkout failed: ... untracked working tree files would be overwritten` | An untracked file in the mirror is in the way of a file that now exists upstream. Move or delete it. |

## "account NAME: the token was rejected"

The token expired or was revoked. Replace it under the same name:

```text
weir auth login github.com --name NAME
```

Nothing else changed while it was broken. The account's owners were skipped, and no repository was
marked gone.

## Many repositories suddenly show as `gone`

That does not happen from an outage: a failed listing changes nothing. It happens when the listing
succeeds but no longer contains them, which usually means the account lost access to an
organization, for example an expired single sign-on authorization. The mirrors are all still on
disk. Fix the access and sync again; they become ordinary mirrors once more. Do not run
`weir prune` in the meantime.

## A sync looks stuck

It is probably cloning something large. The status line shows what is in flight and for how long,
and `weir status` from another terminal shows the same. Afterwards, `weir log --slow` lists the
slowest repositories. `weir track ... --max-size MB` keeps very large repositories out.

## "another weir sync is already running"

You passed `--no-wait` while a sync was in progress. Without it, `weir sync` waits. `weir watch`
shows what the running one is doing.

## The Keychain asks for permission again

macOS ties Keychain access to the exact program that stored the token, so an upgraded weir may be
asked once. Choose "Always Allow". To avoid the Keychain altogether, use
`weir auth login <host> --command "gh auth token"`.

## A mirror seems to be behind

```text
weir sync --all <owner>/<repo>
```

fetches it regardless of what the provider's listing says.

## The daemon is not running

```text
weir service status
```

says whether the service is installed and the daemon alive. `~/.weir/daemon.log` has the daemon's
own notes. If its token comes from a command, check that the command uses a full path; a service's
`PATH` is shorter than your shell's.

## `weir status` warns that no sync has succeeded

Every recent sync ended with a problem. The "Needs attention" list and the `problem:` lines under
"Last sync" say what. Fixing those clears the warning at the next sync.

## Starting over

Deleting `~/.weir/weir.db` loses weir's memory, not your mirrors. Add the accounts and track the
owners again, and the next sync adopts what is on disk.
