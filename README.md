# weir

weir keeps a local, read-only copy of every repository you care about on GitHub, and keeps those
copies up to date.

You tell it which organizations, users or single repositories to follow. It clones them into one
directory tree, and each time it runs it brings every copy to the latest commit of its default
branch. The copies are protected against accidental edits, so they always show what is upstream.

```
$ weir auth login github.com
$ weir track github.com/acme
$ weir sync
cloned    github.com/acme/api
cloned    github.com/acme/web
weir: 2 cloned in 4s

$ rg "retry_policy" ~/Repos/github.com/acme
```

## Why it exists

**Local copies are much faster to search than a provider's API.** A coding agent (or a person)
answering "where is this used?" across a few hundred repositories wants `rg` over files on disk:
it takes milliseconds, has no rate limit, and sees every file. Searching through a web API is slow,
limited and incomplete.

Local copies only help if you can trust them, and a pile of clones goes wrong in three ways:

- **They go stale.** Nobody runs `git pull` in 800 directories. New repositories never get cloned,
  and renamed ones are left behind under their old name.
- **They drift.** Sooner or later someone, or some agent, edits a file or creates a branch in the
  reference copy. From then on it no longer shows upstream, and pulling it becomes a merge problem.
- **They need the right login.** With a work account and a personal account on the same host, every
  clone, fetch and push has to pick the right one. Getting it wrong shows up as a confusing 404.

weir handles all three: it finds, clones and updates repositories on its own, it keeps the copies
read-only and puts them back if they move, and it knows which account each repository belongs to.

The name: a weir is a low dam across a river. Water only flows over it one way.

## What a mirror looks like

weir calls each local copy a mirror. It lives at `<root>/<host>/<owner>/<name>`, by default
`~/Repos/github.com/acme/api`. A mirror is an ordinary git clone with three differences:

- **It is not on a branch.** HEAD is detached at the latest commit of the remote's default branch
  (`origin/main`, `origin/develop`, whatever the provider reports). There is no local `main` to
  fall behind or diverge.
- **Its files are read-only.** An editor or an agent that tries to save over a file is refused.
- **It carries its own credentials.** The mirror's git config names the weir account to use, so
  git commands run inside it sign in as the right user.

Because it is a real clone, `git log`, `git blame` and `git grep` all work in it.

## Install

weir needs `git` and a Rust toolchain (1.89 or newer). It runs on macOS and Linux.

```
git clone https://github.com/tjwaterman99/weir
cd weir
cargo install --path .
```

That puts `weir` in `~/.cargo/bin`. Keep that directory on your `PATH`: git calls `weir` by name
when a mirror needs a token.

## Quick start

1. **Add an account.** On macOS, weir asks for a token and stores it in the Keychain. On Linux,
   use `--env` or `--command` (see [Where tokens are kept](#where-tokens-are-kept)).

   ```
   weir auth login github.com
   ```

   If you already use the GitHub CLI, you can skip the token and let weir ask `gh` each time:

   ```
   weir auth login github.com --command "gh auth token"
   ```

   The token needs read access to the repositories you want mirrored (`repo` scope for a classic
   token). It needs write access too if you will push from worktrees, described below.

2. **Say what to mirror.** An organization, a user, or one repository.

   ```
   weir track github.com/acme
   weir track github.com/torvalds/linux
   ```

3. **Sync.**

   ```
   weir sync
   ```

   The first run clones everything. Later runs only contact repositories that received a push
   since the last run, so a sync of several hundred repositories usually takes seconds.

4. **Use the mirrors.**

   ```
   weir list                 # what is mirrored
   weir path api             # prints ~/Repos/github.com/acme/api
   weir status               # accounts, what is tracked, the last sync, anything needing attention
   ```

## Making changes to a repository

A mirror is for reading. To change code, create a git worktree from the mirror. The worktree is
a separate, writable checkout on its own branch, and it shares the mirror's history, so it costs
almost no disk space or time:

```
cd "$(weir path api)"
git worktree add -b fix-timeout ~/work/api-fix-timeout origin/main
cd ~/work/api-fix-timeout
# edit, commit, then:
git push -u origin fix-timeout
```

The worktree also shares the mirror's git config, so the push signs in with the same account weir
uses for that repository. The mirror itself never leaves the default branch.

## Several accounts on one host

Add each account under its own name, then say which account each owner goes through:

```
weir auth login github.com --name work
weir auth login github.com --name personal

weir track github.com/acme   --account work
weir track github.com/myself --account personal
```

Every repository uses the account of the owner it was tracked under, for listing, cloning,
fetching and pushing. `--account` is optional while a host has one account and required once it
has two. `weir auth list` shows every account and checks that its token still works.

### Where tokens are kept

weir's database never contains a token. It records where to find one:

| Option | The token comes from | Good for |
|---|---|---|
| (default) | the macOS Keychain; weir asks for it once at login | a Mac you work on |
| `--env NAME` | the environment variable `NAME`, read each time | servers and CI |
| `--command "..."` | the output of a command, run each time | reusing another tool's login, e.g. `gh auth token --user myself` |

Tokens are never written into remote URLs or printed.

## Choosing what to mirror

```
weir track github.com/acme                       # everything in the organization
weir track github.com/acme --exclude "*-archive" # skip names matching a glob (repeatable)
weir track github.com/acme --include "api-*"     # only names matching a glob (repeatable)
weir track github.com/acme --forks --archived    # forks and archived repositories are skipped by default
weir track github.com/acme --max-size 2000       # skip repositories larger than 2000 MB
weir track github.com/acme/api                   # a single repository
weir untrack github.com/acme                     # stop; the mirrors stay on disk
```

Running `track` again for the same owner replaces its settings.

## What a sync does

`weir sync` asks each provider for the current list of repositories, compares it with what it
recorded last time, and then acts on each repository:

| Output | Meaning |
|---|---|
| `cloned` | A new repository was cloned. |
| `updated` | The mirror moved to a newer commit. |
| `adopted` | A clone that was already at the mirror's path was taken over (first sync only). |
| `healed` | HEAD had been moved, for example by `git checkout -b`; it was put back. The branch is kept. |
| `moved` | The repository was renamed upstream; the mirror's directory was renamed to match, without cloning again. |
| `archived` | The repository was archived upstream. The mirror is kept and stays searchable. |
| `recreated` | The repository was deleted and created again upstream. The mirror follows the new one. |
| `transferred` | The repository moved to an owner you do not track. The mirror stays on disk. |
| `inaccessible` | The repository still exists, but this account can no longer read it. The mirror stays on disk. |
| `gone` | The repository no longer exists upstream. The mirror stays on disk. |
| `excluded` | Your filters no longer include it. The mirror stays on disk. |
| `skipped` | The mirror has modified files. It was left exactly as it is. |
| `failed` | Something went wrong for this repository; the reason is printed. |

Three promises hold whatever happens:

- **weir never discards work.** It does not reset, clean or stash. A mirror with modified files is
  skipped and reported until you deal with it.
- **Only `weir prune` deletes mirrors.** It deletes mirrors that are gone, excluded, transferred or
  untracked, asks first, and refuses any mirror that has worktrees, local commits, stashes or stray
  files. It never deletes a mirror of a repository you merely lost access to.
- **A provider outage changes nothing.** If the list of repositories cannot be fetched, weir
  reports it and leaves every mirror as it was. A rejected token is reported once per account.

`weir sync` exits with `0` when everything is in order, `1` when some repository or account needs
attention, and `2` when it could not run. `weir sync --dry-run` shows what would happen and
changes nothing.

### Watching a sync

In a terminal, `weir sync` keeps a status line at the bottom while it works: how many repositories
are done, the counts so far, the time taken, and what it is working on right now. A large clone
shows its size, so it does not look stuck.

```
updated   github.com/acme/api  638bb2c -> df75e0b
[412/709] 3 cloned, 12 updated, 380 unchanged · 3m10s · monorepo (14.1 GB) 2m05s, web, +2
```

From any other terminal:

```
weir status       # says "Sync running: [412/709] ..." while one is in progress
weir watch        # follows the running sync until it ends, with the same live line
weir watch --follow   # keeps following the next ones too
```

That works for a sync started by a scheduler as well. Afterwards:

```
weir log          # past runs and what each changed
weir log --slow   # the ten slowest repositories of each run
weir log --all    # also the repositories that were fetched and had not changed
```

Every run also appends to `~/.weir/weir.log`, one timestamped line per change.

For scripts, `weir sync --json` prints one JSON object per line as things happen: `start`, `plan`,
one `repo` per repository worked on, then `finish`.

### When a repository changes upstream

| Upstream change | What weir does |
|---|---|
| Renamed | Renames the mirror's directory to match. No new clone. |
| Transferred to another owner | If you track the new owner, the mirror moves there. If not, it is marked `transferred` and kept. |
| Archived | Syncs it one last time, marks it `archived` and keeps it. It no longer shows in `weir list` (use `--archived`), `weir path` still finds it, and `weir prune` leaves it alone unless you pass `--archived`. An archived repository that was never mirrored is not cloned unless the owner is tracked with `--archived`. |
| Unarchived | It becomes an ordinary mirror again. |
| Deleted | Marked `gone` and kept until you run `weir prune`. |
| Access lost | Marked `inaccessible` and kept. `weir prune` never deletes it. If access comes back, so does the mirror. |
| Deleted and created again under the same name | The mirror follows the new repository, and the sync says `recreated`. |
| Default branch changed, or force-pushed | The mirror moves to the new tip of the new default branch. |

One caveat about "access lost": GitHub answers "not found" for a private repository that your
token can no longer see, exactly as it does for a deleted one. weir can only report those as
`gone`. That is one reason `prune` always asks before deleting.

### Clones weir does not manage

A directory under the root that weir does not know about is never synced, so it goes stale without
anyone noticing. `weir status` counts them, and `weir doctor` explains each one:

```
$ weir doctor
3 git repositories under /Users/me/Repos are not managed by weir. They are never synced.

duplicate     /Users/me/Repos/github.com/acme/api-old
              a stale copy of github.com/acme/api, whose mirror is /Users/me/Repos/github.com/acme/api
gone          /Users/me/Repos/github.com/acme/prototype
              not found on the provider (deleted, or no longer visible); this may be the only copy
untracked     /Users/me/Repos/github.com/someone/tool
              is github.com/someone/tool, which is not tracked
```

`weir prune --unmanaged` deletes the stale copies (`duplicate`) that have nothing to lose. It
leaves every other kind alone, because some of them are the only copy of that code.

### Existing clones

If you already have clones laid out as `<root>/<host>/<owner>/<name>`, the first sync adopts them
in place instead of cloning again. A local default branch with commits that were never pushed is
kept and reported, and so are stashes. A clone that still points at an old name of a repository,
or sits in a directory with the old name, is recognized by asking the provider, then corrected or
moved into place.

## Running it on a schedule

weir has no daemon. Run `weir sync` from whatever scheduler you have; it waits up to a minute for
the network, and a second run started while one is in progress exits straight away.

```
# cron: every morning at 08:30
30 8 * * *  $HOME/.cargo/bin/weir sync --quiet
```

Nobody watches a scheduled run, so weir makes a bad one hard to miss:

- **`weir status` exits `1` when syncs have stopped succeeding.** It warns when the last successful
  sync is older than `stale_after_hours` (36 by default) and shows the recent record, for example
  `Last 7 runs: 6 ok, 1 with problems`. Call it from a shell prompt, a menu bar item or a monitor.
- **`on_problem` runs a command when a sync ends with problems.** The summary arrives on standard
  input and in `$WEIR_SUMMARY`; the details are in `$WEIR_PROBLEMS`.

  ```
  weir config set on_problem 'osascript -e "display notification \"$WEIR_SUMMARY\" with title \"weir\""'
  ```

`weir config list` shows every setting. `--json` on `status`, `list`, `log` and `doctor` gives the
same information to a script.

## Commands

```
weir auth login <host> [--name NAME] [--env VAR | --command CMD]
weir auth list
weir auth logout <name>

weir track <host>/<owner>[/<repo>] [--account NAME] [--include GLOB] [--exclude GLOB]
           [--forks] [--archived] [--max-size MB]
weir untrack <host>/<owner>[/<repo>]

weir sync [<target>] [--dry-run] [--all] [--jobs N] [--network-wait SECONDS]
weir prune [--dry-run] [--yes] [--archived] [--unmanaged]

weir status [<target>]
weir doctor
weir list [<target>] [--archived] [--all]
weir path <repo>
weir watch [--follow]
weir log [-n COUNT] [--slow] [--all]

weir provider add <kind> <host> [--api-url URL]
weir provider list
weir config list
weir config get|set|unset <setting> [<value>]
weir version
```

A `<target>` narrows a command to a host (`github.com`), an owner (`github.com/acme` or `acme`) or
one repository (`acme/api`, or just `api` when only one repository has that name).

`weir sync --all` fetches every repository instead of only those with new pushes. Use it if you
suspect a mirror is behind.

## Where things are kept

| What | Where | Change it with |
|---|---|---|
| Mirrors | `~/Repos/<host>/<owner>/<name>` | `weir config set root <dir>` |
| Database (accounts, what is tracked, every repository, sync history) | `~/.weir/weir.db` | the `WEIR_HOME` environment variable |
| Log of every run | `~/.weir/weir.log` | the `WEIR_HOME` environment variable |
| Tokens | the Keychain, an environment variable or a command | `weir auth login` |

The database is a single SQLite file. Deleting it loses weir's memory, not your mirrors: track the
same owners again and the next sync adopts what is on disk.

## Providers

| Provider | Status |
|---|---|
| GitHub (github.com) | Supported |
| GitHub Enterprise Server | Should work, since it has the same API, but it has not been tried against a real server: `weir provider add github git.example.com` |
| GitLab, Bitbucket | Not implemented yet. The hosts are recognized, and tracking them reports that. |
| `local` | A directory of bare repositories laid out as `<dir>/<owner>/<name>.git`. Used by the test suite. |

## Limits

- Mirrors follow the default branch only. Other branches are fetched (`git log origin/feature`
  works) but never checked out.
- Read-only means "hard to change by accident". Anyone who runs `chmod` can still edit a mirror;
  the next sync reports it as `skipped`.
- Mirrors are full clones. There is no shallow or partial clone option yet.
- Submodules and Git LFS content are not fetched.
- On macOS, a rebuilt `weir` binary may make the Keychain ask for permission again, because the
  Keychain ties access to the exact program that stored the token.

## Development

```
cargo test                                               # unit tests and end-to-end tests
cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

The end-to-end tests run the real binary against throwaway repositories in a temp directory. They
use no network and never touch your own mirrors, database or git config. See `AGENTS.md` for the
layout of the code and the rules a change has to keep.
