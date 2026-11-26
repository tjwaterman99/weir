# Files and providers

## Where things are kept

| What | Where | Change it with |
|---|---|---|
| Mirrors | `~/Repos/<host>/<owner>/<name>` | `weir config set root <dir>` |
| Database: accounts, what is tracked, every repository, sync history | `~/.weir/weir.db` | the `WEIR_HOME` environment variable |
| Log of every change, one line each | `~/.weir/weir.log` | `WEIR_HOME` |
| The daemon's own notes | `$(brew --prefix)/var/log/weir.log` with Homebrew, `~/.weir/daemon.log` otherwise | |
| Tokens | the Keychain, an environment variable or a command | `weir auth login` |
| The background service, with Homebrew | `~/Library/LaunchAgents/sh.brew.weir.plist` on macOS, `~/.config/systemd/user/sh.brew.weir.service` on Linux (older Homebrew versions name them `homebrew.mxcl.weir.plist` and `homebrew.weir.service`) | `brew services start` and `stop` |
| The background service, without Homebrew | `~/Library/LaunchAgents/dev.weir.daemon.plist` on macOS, `~/.config/systemd/user/weir.service` on Linux | `weir service install` and `uninstall` |

The database is a single SQLite file. Deleting it loses weir's memory, not your mirrors: track the
same owners again and the next sync adopts what is on disk.

`weir.log` starts a new file when it passes 5 MB; the previous one is kept as `weir.log.1`.

## Inside a mirror

weir adds two things to a mirror beyond what `git clone` creates:

| What | Why |
|---|---|
| `credential.helper` entries in `.git/config` | So git signs in with the mirror's account |
| `.git/weir-checkout`, briefly | A marker while weir moves the checkout, so an interrupted one can be finished |

While a repository is being cloned, it lives in a directory named `.weir-clone-<name>-<pid>` next
to where the mirror will be. It is renamed into place when the clone is complete.

## Providers

| Provider | Status |
|---|---|
| GitHub (github.com) | Supported |
| GitHub Enterprise Server | Should work, since it has the same API, but it has not been tried against a real server: `weir provider add github git.example.com` |
| GitLab, Bitbucket | Not implemented yet. The hosts are recognized, and tracking them says so. |
| `local` | A directory of bare repositories laid out as `<dir>/<owner>/<name>.git`. The test suite and the examples in this documentation use it. |

## Limits

- Mirrors follow the default branch only. Other branches are fetched but never checked out.
- Read-only means "hard to change by accident". Anyone who runs `chmod` can still edit a mirror;
  the next sync reports it as `skipped`.
- Mirrors are full clones. There is no shallow or partial clone option yet.
- Submodules and Git LFS content are not fetched.
- GitHub does not say whether a private repository was deleted or merely hidden from you. Both
  show as `gone`.
