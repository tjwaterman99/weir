# When a repository changes upstream

Repositories do not stay put. Here is what weir does about each kind of change.

| Upstream change | What weir does |
|---|---|
| Renamed | Renames the mirror's directory to match. No new clone. |
| Transferred to another owner | If you track the new owner, the mirror moves there. If not, it is marked `transferred` and kept. |
| Archived | Syncs it one last time, marks it `archived` and keeps it. |
| Unarchived | It becomes an ordinary mirror again. |
| Deleted | Marked `gone` and kept until you run `weir prune`. |
| Access lost | Marked `inaccessible` and kept. `weir prune` never deletes it. If access comes back, so does the mirror. |
| Deleted and created again under the same name | The mirror follows the new repository, and the sync says `recreated`. |
| Default branch changed, or force-pushed | The mirror moves to the new tip of the new default branch. |

```text
{{#include ../examples/out/sync-upstream.txt}}
```

## Renames

weir identifies a repository by the provider's own id for it, not by its name. When the name
changes, weir sees the same id under a new name and renames the directory. Worktrees attached to
the mirror are repaired so they keep working.

## Archived repositories

An archived repository never changes again, so keeping its mirror costs nothing and it is often
still worth searching. An archived mirror:

- no longer shows in `weir list`, unless you pass `--archived`;
- is still found by `weir path`;
- is left alone by `weir prune`, unless you pass `--archived`.

```text
{{#include ../examples/out/list-archived.txt}}
```

A repository that was archived before weir ever mirrored it is not cloned, unless its owner is
tracked with `--archived`. If a clone of it is already on disk, weir adopts that clone.

## Gone, or just out of sight?

When a repository drops out of a listing, weir looks it up by id before deciding anything:

- **Not found:** `gone`.
- **Found under another owner:** `transferred`, with the new name in the message.
- **Found, but access is refused, or it is no longer listed for this account:** `inaccessible`.
- **The lookup itself failed:** nothing changes, and the sync reports a problem.

One limit is worth knowing. GitHub answers "not found" for a private repository your token can no
longer see, exactly as it does for a deleted one. weir can only report those as `gone`. That is
one reason `weir prune` always asks before deleting.
