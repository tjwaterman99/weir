# AGENTS.md

Rust CLI (`weir`). One binary, no async. Read `README.md` first for what it does.

- `src/main.rs`: clap definitions and every command except sync.
- `src/sync.rs`: the sync engine (list providers, diff against the database, run jobs on a thread pool).
- `src/git.rs`: everything done to a mirror on disk, by running `git`.
- `src/provider.rs`: the `Provider` trait, GitHub, and the `local` provider (a directory of bare repositories).
- `src/secret.rs`: where tokens live (keychain, environment variable, command).
- `src/db.rs`: the SQLite schema and row types. `src/target.rs`: parsing `host/owner/repo`.

## Build, test, lint

```
cargo build
cargo test
cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

Run all three before pushing; CI runs the same on Linux and macOS.

## Rules

- **weir never discards work.** No `reset`, `clean`, `stash` or forced checkout on a mirror. A
  mirror with modified files is skipped and reported. Keep that tested.
- **Only `weir prune` deletes**, and only mirrors with nothing to lose (`git::blockers`).
- **A failed or partial listing never marks repositories as gone.** Only a listing that succeeded does.
- **Tokens never go in the database, in a remote URL or in output.** The database holds a reference.
- Tests must not touch the network, `~/Repos`, `~/.weir` or the user's git config. `tests/cli.rs`
  runs the real binary against the `local` provider in a temp directory, with `WEIR_HOME`,
  `HOME` and `GIT_CONFIG_GLOBAL` pointed inside it. The GitHub provider is tested against a small
  HTTP server in the same file.
- Schema changes: bump `PRAGMA user_version` in `db::open` and migrate; never edit the existing
  schema in place once a release exists.
- New provider: implement `Provider`, add it to `provider::build`, and test it the way GitHub is.
