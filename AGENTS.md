# AGENTS.md

Rust CLI (`weir`). One binary, no async. Read `README.md` first for what it does.

- `src/main.rs`: clap definitions and every command except sync.
- `src/sync.rs`: the sync engine (list providers, diff against the database, run jobs on a thread
  pool) and everything a sync shows: the live status line, streamed JSON, the log file.
- `src/runs.rs`, `src/watch.rs`: reading run history, and `weir watch`. A running sync writes its
  progress to its `sync_runs` row; other processes read it from there.
- `src/daemon.rs`: `weir daemon`, the sync engine on a loop (offline, battery, backoff, wake-up).
  `src/service.rs`: `weir service`, which registers it with launchd or systemd.
- `src/settings.rs`: the settings (`weir config`). Add a setting there, with a default and help.
- `src/git.rs`: everything done to a mirror on disk, by running `git`.
- `src/provider.rs`: the `Provider` trait, GitHub, and the `local` provider (a directory of bare repositories).
- `src/stray.rs`: finds git repositories under the root that weir does not manage and asks the
  provider what they are (`weir doctor`, `weir prune --unmanaged`).
- `src/secret.rs`: where tokens live (keychain, environment variable, command).
- `src/db.rs`: the SQLite schema and row types. `src/target.rs`: parsing `host/owner/repo`.

## Build, test, lint

```
cargo build
cargo test
cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

Run all three before pushing; CI runs the same on Linux only, so macOS is covered by running
them locally.

## Releasing

`.github/workflows/release.yml` runs on a `vX.Y.Z` tag: it checks the tag against `Cargo.toml`,
tests, builds four targets, publishes a GitHub release, and writes `Formula/weir.rb` to
`tjwaterman99/homebrew-tap` with `packaging/formula.sh` (pushed with the `TAP_DEPLOY_KEY` secret, a
deploy key that can only write to the tap). Rehearse with a `-rc.N` tag: it publishes a pre-release
and skips the tap. PR CI runs on Linux only; macOS is exercised by the release workflow.

## Rules

- **weir never discards work.** No `reset`, `clean`, `stash` or forced checkout on a mirror. A
  mirror with modified files is skipped and reported. Keep that tested. The single exception is
  finishing weir's own interrupted checkout, recognized by the `.git/weir-checkout` marker that is
  written only after the tree was verified clean.
- **Interruption must be safe at every step.** Clones go to a `.weir-clone-*` directory and are
  renamed into place when complete. On shutdown only clones and fetches are stopped
  (`git::abort_network`); a checkout is left to finish.
- **Listings are conditional requests** (`http_cache` table, one view per account). An idle daemon
  cycle must cost no rate limit and run no git command against a GitHub provider.
- **Only `weir prune` deletes**, and only mirrors with nothing to lose (`git::blockers`).
- **A failed or partial listing never marks repositories as gone.** Only a listing that succeeded
  does, and a repository that dropped out of one is looked up by id before its state changes. If
  that lookup fails, nothing changes.
- **Repository states:** `active`, `archived` (kept and searchable, pruned only with `--archived`),
  `excluded`, `gone`, `transferred`, `inaccessible` (never pruned), `untracked`. Every upstream
  change in the README's table has an end-to-end test; keep it that way.
- **Tokens never go in the database, in a remote URL or in output.** The database holds a reference.
- Tests must not touch the network, `~/Repos`, `~/.weir` or the user's git config. `tests/cli.rs`
  runs the real binary against the `local` provider in a temp directory, with `WEIR_HOME`,
  `HOME` and `GIT_CONFIG_GLOBAL` pointed inside it. The GitHub provider is tested against a small
  HTTP server in the same file.
- Schema changes: add an entry to `MIGRATIONS` in `src/db.rs`. Never edit `SCHEMA` or an entry
  that has shipped.
- New provider: implement `Provider`, add it to `provider::build`, and test it the way GitHub is.
