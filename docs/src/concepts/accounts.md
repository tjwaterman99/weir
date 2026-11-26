# Accounts and tokens

An account is one login on one provider host: a name you choose, the user it belongs to, and where
its token is kept.

```text
weir auth login github.com --name work
weir auth list
weir auth logout work
```

`weir auth login` checks the token with the provider before saving anything, so a bad token fails
at once. `weir auth list` checks every account again and exits `1` if any token has stopped working.

Each [tracked owner](tracking.md) goes through exactly one account. Every repository under that
owner uses it for listing, cloning, fetching and pushing. See
[several accounts on one host](../guides/multiple-accounts.md).

## Where tokens are kept

weir's database never contains a token. It records where to find one.

| Option | The token comes from | Good for |
|---|---|---|
| (default) | the macOS Keychain; weir asks for it once at login | a Mac you work on |
| `--env NAME` | the environment variable `NAME`, read each time | servers and CI |
| `--command "..."` | the output of a command, run each time | reusing another tool's login, such as `gh auth token --user myself` |

Tokens are never written into remote URLs and never printed.

Two things to know about the Keychain:

- It is only available on macOS. On Linux, use `--env` or `--command`.
- It ties access to the exact program that stored a token. After weir is upgraded, macOS may ask
  once whether the new binary may read it. `--command "gh auth token"` avoids the Keychain entirely.

## A token that stops working

A rejected token is reported once per account, however many owners use it, and those owners are
skipped for that run. Nothing else changes: no mirror is touched, and no repository is marked as
gone. Run `weir auth login` again with the same `--name` to replace the token.
