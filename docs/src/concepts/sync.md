# What a sync does

A sync asks each provider for the current list of repositories, compares it with what weir
recorded last time, and then acts on each repository. `weir sync` runs one; the
[daemon](../guides/background.md) runs one every few minutes.

## Outcomes

Each repository that something happened to gets one line:

| Output | Meaning |
|---|---|
| `cloned` | A new repository was cloned. |
| `updated` | The mirror moved to a newer commit. |
| `adopted` | A clone that was already at the mirror's path was taken over. See [adopting clones](../guides/existing-clones.md). |
| `healed` | HEAD had been moved, for example by `git checkout -b`. It was put back, and the branch was kept. |
| `moved` | The repository was renamed upstream. The mirror's directory was renamed to match, without cloning again. |
| `archived` | The repository was archived upstream. The mirror is kept and stays searchable. |
| `recreated` | The repository was deleted and created again upstream. The mirror follows the new one. |
| `transferred` | The repository moved to an owner you do not track. The mirror stays on disk. |
| `inaccessible` | The repository still exists, but this account can no longer read it. The mirror stays on disk. |
| `gone` | The repository no longer exists upstream. The mirror stays on disk. |
| `excluded` | Your filters no longer include it. The mirror stays on disk. |
| `skipped` | The mirror has modified files. It was left exactly as it is. |
| `failed` | Something went wrong for this repository. The reason is printed. |

Repositories with nothing new are counted as `unchanged` and not listed.

What `skipped` and `healed` look like:

```text
{{#include ../examples/out/sync-problems.txt}}
```

## Three promises

- **weir never discards work.** It does not reset, clean or stash. A mirror with modified files is
  skipped and reported until you deal with it. The one forced checkout weir ever does is to finish
  its own, if it was killed half-way through one; it leaves itself a marker to know.
- **Only `weir prune` deletes mirrors.** It asks first, and refuses any mirror that has worktrees,
  local commits, stashes or stray files. It never deletes a mirror of a repository you merely lost
  access to. See [cleaning up](../guides/cleaning-up.md).
- **A provider outage changes nothing.** If a list of repositories cannot be fetched, weir reports
  it and leaves every mirror as it was. A repository that drops out of a listing is looked up
  directly before its state changes, and if that lookup fails, nothing changes.

## Why it is fast

- **Only repositories with new pushes are fetched.** The provider's listing says when each
  repository was last pushed to. A repository whose time has not moved is not contacted at all.
  weir only reads one small file to check that its HEAD is still where it left it.
- **Unchanged listings are free.** Each listing is a conditional request. When nothing changed,
  the provider answers "not modified", which does not count against the rate limit.
- **Several repositories at once.** `--jobs` sets how many (8 by default; 4 in the daemon).

`weir sync --all` fetches every repository regardless. Use it if you suspect a mirror is behind.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Everything is in order. |
| `1` | Some repository or account needs attention: a mirror was skipped or failed, a token was rejected, a provider could not be listed. |
| `2` | It could not run. |

## Trying it first

`weir sync --dry-run` shows what would happen and changes nothing, on disk or in the database:

```text
{{#include ../examples/out/sync-dry-run.txt}}
```

## One at a time

Two syncs never run at once. A second `weir sync` waits for the first to finish, then runs.
`--no-wait` makes it fail instead. `status`, `list`, `path`, `log` and `watch` never wait.
