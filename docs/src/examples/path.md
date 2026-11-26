## Examples

```text
{{#include out/path.txt}}
```

It is made for command substitution:

```text
cd "$(weir path api)"
rg "retry_policy" "$(weir path acme/api)"
```

A name that exists under two owners is an error that lists both; use `<owner>/<name>`.
