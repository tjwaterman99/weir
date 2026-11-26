# Install

weir runs on macOS and Linux. It needs `git` on your `PATH`, which macOS and most Linux systems
already have.

## Homebrew

```text
brew install tjwaterman99/tap/weir
```

That installs a prebuilt binary. `brew upgrade` picks up new releases.

## A release archive

Download the archive for your platform from the
[releases page](https://github.com/tjwaterman99/weir/releases), unpack it, and put `weir` in a
directory on your `PATH`.

| Platform | Archive |
|---|---|
| macOS, Apple silicon | `weir-<version>-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `weir-<version>-x86_64-apple-darwin.tar.gz` |
| Linux, x86_64 | `weir-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux, ARM | `weir-<version>-aarch64-unknown-linux-gnu.tar.gz` |

Each archive has a `.sha256` file next to it.

## From source

This needs a Rust toolchain, 1.89 or newer.

```text
git clone https://github.com/tjwaterman99/weir
cd weir
cargo install --path .
```

## Check it

```text
$ weir version
weir 0.2.0 (8b83a6864)
```

Keep `weir` on your `PATH` however you installed it. Each mirror's git config calls `weir` by name
when git needs a token.
