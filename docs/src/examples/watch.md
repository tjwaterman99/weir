## Examples

```text
$ weir watch
updated   github.com/acme/api  638bb2c -> df75e0b
[412/709] 3 cloned, 12 updated, 380 unchanged · 3m10s · monorepo (14.1 GB) 2m05s, web, +2
```

The last line redraws in place until the sync ends. With no sync running, `weir watch` says so and
exits; `weir watch --follow` waits for the next one.
