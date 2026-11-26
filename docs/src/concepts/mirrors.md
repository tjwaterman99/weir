# Mirrors

weir calls each local copy of a repository a mirror. A mirror lives at:

```text
<root>/<host>/<owner>/<name>
```

By default the root is `~/Repos`, so `github.com/acme/api` is mirrored at
`~/Repos/github.com/acme/api`. Change the root with `weir config set root <dir>`.

A mirror is an ordinary git clone with three differences.

## It is not on a branch

HEAD is detached at the latest commit of the remote's default branch: `origin/main`,
`origin/develop`, whatever the provider reports. There is no local `main` to fall behind or
diverge, so updating a mirror never involves a merge. A sync fetches and moves HEAD.

Other branches are fetched too. `git log origin/feature` works in a mirror; the branch is just
never checked out.

## Its files are read-only

weir removes the write permission from every tracked file. An editor or an agent that tries to
save over one is refused.

This means "hard to change by accident", not "impossible to change". Anyone who runs `chmod` can
still edit a mirror. If they do, the next sync reports the mirror as `skipped` and leaves the edit
alone. See [what a sync does](sync.md).

Directories stay writable, because git has to replace files when it updates the checkout. Git only
records a file's executable bit, so the read-only permission never shows up as a modification.

## It carries its own credentials

The mirror's git config names the weir account to use:

```text
[credential]
    helper =
    helper = !weir credential --account work
```

The empty first line clears any helper from your own git config. The second makes git ask weir
for the token of that account. So a `git fetch` typed inside a mirror, or a `git push` from one of
its [worktrees](../guides/worktrees.md), signs in as the right user, whichever account your other
tools happen to be using. See [accounts and tokens](accounts.md).

## It is still a real clone

`git log`, `git blame`, `git grep` and `git worktree` all work in a mirror. Mirrors are full
clones: there is no shallow or partial option yet, and submodules and Git LFS content are not
fetched.
