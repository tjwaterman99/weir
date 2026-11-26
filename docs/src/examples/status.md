## Examples

```text
{{#include out/status.txt}}
```

When something needs attention:

```text
{{#include out/status-problems.txt}}
```

`weir status` exits `1` when no sync has succeeded for longer than `stale_after_hours`. See
[knowing that it is working](../../guides/monitoring.md).
