## Examples

```text
weir auth login github.com                                   # asks for a token, stores it in the Keychain
weir auth login github.com --command "gh auth token"         # ask the GitHub CLI each time
weir auth login github.com --env GITHUB_TOKEN                # read an environment variable each time
weir auth login github.com --name work                       # a second account on the same host
weir auth list                                               # every account, and whether its token works
```

See [accounts and tokens](../../concepts/accounts.md).
