# Cleaning up

weir never deletes a mirror on its own. Mirrors pile up in two ways: repositories that are gone,
excluded, transferred or no longer tracked, and clones under the root that weir never managed.

## Mirrors weir no longer needs

`weir status` says how many there are. `weir prune` deletes them:

```text
{{#include ../examples/out/prune.txt}}
```

Without `--dry-run`, it lists what it is about to delete and asks first. `--yes` skips the
question; without a terminal and without `--yes`, it refuses.

| State | Pruned? |
|---|---|
| `gone`, `excluded`, `transferred`, `untracked` | Yes |
| `archived` | Only with `--archived` |
| `inaccessible` | Never. A loss of access may be temporary. |

### What prune refuses to delete

A mirror is kept, and prune says why, if it has any of:

- worktrees;
- commits that were never pushed;
- stashes;
- modified or untracked files.

Deal with those yourself, then prune again.

## Clones weir does not manage

A directory under the root that weir does not know about is never synced, so it goes stale without
anyone noticing. `weir status` counts them, and `weir doctor` explains each one:

```text
{{#include ../examples/out/doctor.txt}}
```

| Kind | Meaning | What to do |
|---|---|---|
| `duplicate` | A stale copy of a repository whose mirror is somewhere else, usually left by a rename. | `weir prune --unmanaged` deletes it. |
| `pending` | A repository weir will manage. | Nothing: the next sync adopts it. |
| `excluded` | It exists upstream under a tracked owner, but your filters leave it out. | Change the filters, or delete it yourself. |
| `untracked` | It exists upstream, and nothing tracks it. | `weir track` it, or leave it. |
| `gone` | The provider has no such repository, or will not show it to you. | This may be the only copy. Decide yourself. |
| `inaccessible` | It exists, but access is refused. | Check the account's access. |
| `no-remote` | It has no `origin`. | This is the only copy. weir leaves it alone. |
| `foreign` | Its origin is not a repository on this provider. | weir leaves it alone. |

`weir prune --unmanaged` deletes only the `duplicate` kind, and only those with nothing to lose.
It leaves every other kind alone, because some of them are the only copy of that code.

`weir doctor` makes one request to the provider per unmanaged clone.
