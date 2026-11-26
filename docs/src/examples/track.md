## Examples

```text
{{#include out/track.txt}}
```

```text
weir track github.com/acme --exclude "*-archive"   # skip names matching a glob
weir track github.com/acme --include "api-*"       # only names matching a glob
weir track github.com/acme --forks --archived      # both are skipped by default
weir track github.com/acme --max-size 2000         # skip repositories over 2000 MB
weir track github.com/torvalds/linux               # a single repository
weir track github.com/acme --account work          # required once a host has two accounts
```

Running `track` again for the same target replaces its settings. See
[what is mirrored](../../concepts/tracking.md).
