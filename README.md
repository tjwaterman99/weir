# weir

weir keeps a local, read-only copy of every repository you care about on GitHub, and keeps those
copies up to date.

You tell it which organizations, users or single repositories to follow. It clones them into one
directory tree and keeps every copy at the latest commit of its default branch. The copies are
protected against accidental edits, so they always show what is upstream.

```
$ weir auth login github.com
$ weir track github.com/acme
$ weir sync
cloned    github.com/acme/api
cloned    github.com/acme/web
weir: 2 cloned in 4s

$ brew services start weir    # from now on, mirrors stay a few minutes behind

$ rg "retry_policy" ~/Repos/github.com/acme
```

**Documentation: <https://tjwaterman99.github.io/weir/>**

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

## Install

With [Homebrew](https://brew.sh), on macOS or Linux:

```
brew install tjwaterman99/tap/weir
brew services start weir
```

The first command installs a prebuilt binary; the second keeps your mirrors current in the
background. weir needs `git` on your `PATH`. The
[install page](https://tjwaterman99.github.io/weir/install.html) covers release archives and
building from source.

## Quick start

1. **Add an account.** On macOS, weir asks for a token and stores it in the Keychain.

   ```
   weir auth login github.com
   ```

   If you already use the GitHub CLI, let weir ask it each time. This also works on Linux:

   ```
   weir auth login github.com --command "gh auth token"
   ```

2. **Say what to mirror.** An organization, a user, or one repository.

   ```
   weir track github.com/acme
   ```

3. **Sync.** The first run clones everything. Later runs only contact repositories that changed.

   ```
   weir sync
   ```

4. **Keep it current.** Run weir in the background from now on.

   ```
   brew services start weir
   ```

   Without Homebrew, `weir service install` does the same.

5. **Use the mirrors.** They live at `~/Repos/<host>/<owner>/<name>`.

   ```
   weir list
   weir path api        # prints ~/Repos/github.com/acme/api
   weir status
   ```

## What you get

- **Mirrors that are never on a branch.** Each one sits on a detached HEAD at the tip of the
  remote's default branch, with read-only files. Updating one is a fetch, never a merge.
  [Mirrors](https://tjwaterman99.github.io/weir/concepts/mirrors.html)
- **Several accounts per host.** Each owner goes through one account, and each mirror's git config
  names it, so fetches and pushes sign in as the right user.
  [Accounts](https://tjwaterman99.github.io/weir/concepts/accounts.html)
- **Changes go through worktrees.** A worktree off a mirror is a writable checkout on its own
  branch that shares the mirror's history.
  [Worktrees](https://tjwaterman99.github.io/weir/guides/worktrees.html)
- **A background daemon.** Every few minutes it asks what changed, at no cost when nothing did.
  [Background](https://tjwaterman99.github.io/weir/guides/background.html)
- **It follows what happens upstream.** Renames, transfers, archiving, deletion and lost access
  each have a defined outcome.
  [Upstream changes](https://tjwaterman99.github.io/weir/concepts/upstream-changes.html)
- **It never discards work.** No reset, no clean, no stash. A mirror someone edited is skipped and
  reported, and only `weir prune` deletes anything, after asking.
  [What a sync does](https://tjwaterman99.github.io/weir/concepts/sync.html)
- **It tells you when it is not working.** A live view of a running sync, a history of past runs,
  and a status that fails when syncs stop succeeding.
  [Monitoring](https://tjwaterman99.github.io/weir/guides/monitoring.html)

## Commands

```
weir auth login|list|logout        accounts
weir track / untrack               what to mirror
weir sync                          bring every mirror up to date
weir daemon / service              do that continuously, in the background
weir status / list / path          what is mirrored, and where
weir log / watch                   what syncs did, and what one is doing now
weir doctor / prune                find and delete what is no longer needed
weir provider / config / version   setup
```

Each one has a page in the
[command reference](https://tjwaterman99.github.io/weir/reference/commands/index.html), and
`weir <command> --help` prints the same text.

## Providers

GitHub is supported. GitHub Enterprise Server should work but has not been tried against a real
server. GitLab and Bitbucket are not implemented yet.

## Development

```
cargo test                                               # unit tests and end-to-end tests
cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

The end-to-end tests run the real binary against throwaway repositories in a temp directory. They
use no network and never touch your own mirrors, database or git config. See `AGENTS.md` for the
layout of the code and the rules a change has to keep.

The documentation site is built from `docs/` with [mdBook](https://rust-lang.github.io/mdBook/):

```
mdbook serve docs                                        # preview at http://localhost:3000
cargo run -- reference docs/src/reference/commands --examples docs/src/examples
docs/capture-examples.sh                                 # refresh the example output
```

The command reference is generated from the command-line definitions; a test fails when it is out
of date.

Releases: bump `version` in `Cargo.toml`, merge, then push a tag `vX.Y.Z`. The release workflow
tests and builds for macOS and Linux (Apple silicon, Intel and ARM), publishes a GitHub release and
updates the Homebrew tap. A tag with a suffix, such as `v1.2.0-rc.1`, publishes a pre-release and
leaves the tap alone.
