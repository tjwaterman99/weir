# Quick start

Five steps take you from nothing to a directory of mirrors that keeps itself current.

## 1. Add an account

On macOS, weir asks for a token and stores it in the Keychain:

```text
weir auth login github.com
```

If you already use the GitHub CLI, you can skip the token and let weir ask `gh` each time. This
also works on Linux, where there is no Keychain:

```text
weir auth login github.com --command "gh auth token"
```

The token needs read access to the repositories you want mirrored (`repo` scope for a classic
token). It needs write access too if you will push from [worktrees](guides/worktrees.md).

## 2. Say what to mirror

An organization, a user, or one repository:

```text
{{#include examples/out/track.txt}}
```

## 3. Sync

```text
{{#include examples/out/sync-first.txt}}
```

The first run clones everything. Later runs only contact repositories that received a push since
the last run, so a sync of several hundred repositories usually takes seconds:

```text
{{#include examples/out/sync-update.txt}}
```

## 4. Keep it current

Let weir run in the background from now on. With Homebrew:

```text
brew services start weir
```

Without Homebrew, `weir service install` does the same. See
[keeping mirrors current in the background](guides/background.md) for what the daemon does.

## 5. Use the mirrors

```text
{{#include examples/out/list.txt}}
```

```text
{{#include examples/out/path.txt}}
```

Search them with whatever you like:

```text
rg "retry_policy" "$(weir path api)"
```

`weir status` shows the accounts, what is tracked, the last sync and anything that needs attention.
