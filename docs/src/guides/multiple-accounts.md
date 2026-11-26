# Several accounts on one host

A work account and a personal account on the same host is the common case. Add each one under its
own name, then say which account each owner goes through:

```text
weir auth login github.com --name work
weir auth login github.com --name personal

weir track github.com/acme   --account work
weir track github.com/myself --account personal
```

`--account` is optional while a host has one account, and required once it has two. Leaving it out
then is an error that lists the accounts to choose from.

## What the account decides

Every repository uses the account of the owner it was tracked under, for four things:

1. listing the owner's repositories;
2. cloning;
3. fetching;
4. pushing from a [worktree](worktrees.md).

The last three work because each mirror's git config names its account. Whichever account your
other tools are signed in with makes no difference inside a mirror or its worktrees.

## With the GitHub CLI

`gh` keeps several logins per host. Point each weir account at one of them, and weir needs no
tokens of its own:

```text
weir auth login github.com --name work     --command "gh auth token --user work-login"
weir auth login github.com --name personal --command "gh auth token --user personal-login"
```

If a scheduler or the background service runs weir, give the full path to `gh`, for example
`/opt/homebrew/bin/gh`. Their `PATH` is shorter than your shell's.

## Checking

```text
weir auth list
```

lists every account with its login and whether its token still works.

## Moving an owner to another account

Run `track` again with the other account:

```text
weir track github.com/acme --account personal
```

The mirrors stay where they are. Their credential helper is updated the next time each one is
synced; `weir sync --all github.com/acme` does all of them at once.
