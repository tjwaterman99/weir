## Examples

```text
{{#include out/prune.txt}}
```

```text
weir prune                 # lists what it will delete and asks first
weir prune --yes           # no question, for scripts
weir prune --archived      # also mirrors of archived repositories
weir prune --unmanaged     # also stale copies that weir does not manage
```

See [cleaning up](../../guides/cleaning-up.md) for what prune refuses to delete.
