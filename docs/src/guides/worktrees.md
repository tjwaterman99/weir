# Making changes with worktrees

A mirror is for reading. To change code, create a git worktree from it.

A worktree is a separate, writable checkout on its own branch. It shares the mirror's history, so
it takes almost no disk space and no time to create.

```text
cd "$(weir path api)"
git worktree add -b fix-timeout ~/work/api-fix-timeout origin/main
cd ~/work/api-fix-timeout

# edit, commit, then:
git push -u origin fix-timeout
```

## Why this works well

- **The mirror never leaves the default branch.** It keeps showing what is upstream while you work.
- **The push signs in as the right user.** A worktree shares the mirror's git config, including
  the credential helper that names the weir account.
- **Starting from `origin/main` is always starting from fresh code,** because the mirror is kept
  current.

The account's token needs write access for the push to work.

## Finishing

```text
git worktree remove ~/work/api-fix-timeout
```

The branch stays in the mirror's repository until you delete it with `git branch -d`.

## What weir does about worktrees

- A sync never touches a worktree, or the branch it is on.
- When a repository is renamed upstream and its mirror is moved, the worktrees are repaired.
- `weir prune` refuses to delete a mirror that still has worktrees.

## If you check out a branch in the mirror by mistake

The next sync puts the mirror back on the default branch and reports `healed`. Your branch is
kept. Create a worktree for it:

```text
git worktree add ~/work/api-my-branch my-branch
```
