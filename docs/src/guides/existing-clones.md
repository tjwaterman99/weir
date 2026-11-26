# Adopting clones you already have

If you already keep clones laid out as `<root>/<host>/<owner>/<name>`, weir takes them over in
place. It does not clone them again.

1. Point weir at the directory, if it is not `~/Repos`:

   ```text
   weir config set root ~/code
   ```

2. Add an account and track the owners, as in the [quick start](../quick-start.md).

3. Look before you leap:

   ```text
   weir sync --dry-run
   ```

   Existing clones show as `fetch`, new repositories as `clone`.

4. Run `weir sync`. Each existing clone is reported as `adopted`.

## What adopting does to a clone

- Fetches, then moves HEAD to a detached checkout of the default branch's latest commit.
- Deletes the local default branch, if everything on it is already on the remote.
- Marks tracked files read-only.
- Points the clone's credential helper at the right weir account.

## What it never does

- **A clone with modified files is skipped.** Commit, stash or discard the changes yourself, then
  sync again.
- **Commits that were never pushed are kept.** If the local default branch has commits the remote
  does not, the branch stays, and the sync says so.
- **Stashes are kept,** and the sync mentions them.
- **Other local branches and worktrees are left alone.**
- **A directory that is a clone of something else fails** with a message naming both URLs. weir
  will not point it at a different repository.

## Clones with an out-of-date name

Clones made long ago may predate a rename. weir asks the provider what the old name refers to now:

- **The directory has the new name but its remote still has the old one:** the remote is corrected
  and the clone adopted.
- **The directory still has the old name, and nothing is at the new one:** the directory is moved
  into place, not cloned again.

## What is left over

Anything under the root that weir does not end up managing is listed by `weir doctor`. See
[cleaning up](cleaning-up.md).
