# What is mirrored

You tell weir what to follow with `weir track`. A target is an owner (an organization or a user)
or a single repository, always with its host:

```text
weir track github.com/acme               # everything in the organization
weir track github.com/torvalds/linux     # one repository
```

Running `track` again for the same target replaces its settings. Tracking a whole owner takes over
any single repositories of that owner you tracked before.

## Filters

By default weir mirrors every repository of an owner except forks and archived repositories.

| Option | Effect |
|---|---|
| `--include GLOB` | Only repositories whose name matches. Repeatable. |
| `--exclude GLOB` | Skip repositories whose name matches. Repeatable. |
| `--forks` | Also mirror forks. |
| `--archived` | Also clone repositories that are already archived. |
| `--max-size MB` | Skip repositories larger than this. |
| `--account NAME` | The account to use. Required once the host has more than one. |

```text
weir track github.com/acme --exclude "*-archive" --max-size 2000
```

Globs match the repository's name, not its owner, and ignore case.

`--max-size` is worth knowing about before a first sync of a large organization. One multi-gigabyte
repository can take longer to clone than everything else together.

## When filters change

If a repository that is already mirrored stops matching, weir marks it `excluded` and keeps the
mirror on disk. Nothing is deleted until you run [`weir prune`](../guides/cleaning-up.md).

## Stopping

```text
weir untrack github.com/acme
```

The mirrors stay on disk, marked `untracked`. `weir prune` deletes them.

## Narrowing a command

`sync`, `status` and `list` take an optional target to narrow what they act on. Shorter forms work
when they can only mean one thing:

| You type | It means |
|---|---|
| `github.com` | every tracked owner on that host |
| `github.com/acme` or `acme` | one owner |
| `github.com/acme/api`, `acme/api` or `api` | one repository |

`weir path` takes a repository in any of the last three forms. A bare name that exists under two
owners is an error that lists both.
