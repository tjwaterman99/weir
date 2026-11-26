## Examples

A first sync clones everything:

```text
{{#include out/sync-first.txt}}
```

Later syncs only touch what changed:

```text
{{#include out/sync-update.txt}}
```

A mirror someone edited is skipped, and one whose HEAD was moved is put back:

```text
{{#include out/sync-problems.txt}}
```

See what would happen without changing anything:

```text
{{#include out/sync-dry-run.txt}}
```

Sync one owner or one repository, or fetch everything regardless of what changed:

```text
weir sync acme
weir sync acme/api
weir sync --all
```

See [what a sync does](../../concepts/sync.md) for what each outcome means.
