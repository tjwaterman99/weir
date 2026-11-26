# weir

weir keeps a local, read-only copy of every repository you care about on GitHub, and keeps those
copies up to date.

You tell it which organizations, users or single repositories to follow. It clones them into one
directory tree and keeps every copy at the latest commit of its default branch. The copies are
protected against accidental edits, so they always show what is upstream.

```text
$ weir auth login github.com
$ weir track github.com/acme
$ weir sync
cloned    github.com/acme/api
cloned    github.com/acme/web
weir: 2 cloned in 4s

$ brew services start weir    # from now on, mirrors stay a few minutes behind

$ rg "retry_policy" ~/Repos/github.com/acme
```

## Why it exists

**Local copies are much faster to search than a provider's API.** A coding agent, or a person,
answering "where is this used?" across a few hundred repositories wants `rg` over files on disk.
It takes milliseconds, has no rate limit, and sees every file. Searching through a web API is slow,
limited and incomplete.

Local copies only help if you can trust them, and a pile of clones goes wrong in three ways:

- **They go stale.** Nobody runs `git pull` in 800 directories. New repositories never get cloned,
  and renamed ones are left behind under their old name.
- **They drift.** Sooner or later someone, or some agent, edits a file or creates a branch in the
  reference copy. From then on it no longer shows upstream, and pulling it becomes a merge problem.
- **They need the right login.** With a work account and a personal account on the same host, every
  clone, fetch and push has to pick the right one. Getting it wrong shows up as a confusing 404.

weir handles all three. It finds, clones and updates repositories on its own. It keeps the copies
read-only and puts them back if they move. And it knows which account each repository belongs to.

The name: a weir is a low dam across a river. Water only flows over it one way.

## Where to go next

- [Install](install.md) it, then follow the [quick start](quick-start.md).
- Read [what a mirror is](concepts/mirrors.md) and [what a sync does](concepts/sync.md).
- Look up a command in the [reference](reference/commands/index.md).
