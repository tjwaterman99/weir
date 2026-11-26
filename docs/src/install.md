# Install

weir runs on macOS and Linux. It needs `git` on your `PATH`, which macOS and most Linux systems
already have.

## Homebrew

```text
brew install tjwaterman99/tap/weir
brew services start weir
```

The first command installs a prebuilt binary. The second starts weir in the background and keeps
it running; do it once you have added an account and tracked something, or before, as you like.
See the [quick start](quick-start.md).

`brew upgrade` picks up new releases, and the running daemon switches to the new version on its
own.

weir is not in Homebrew's core repository, so the first install names its tap,
`tjwaterman99/tap`. After that the short name works everywhere: `brew services start weir`,
`brew upgrade weir`. If you prefer to add the tap first:

```text
brew tap tjwaterman99/tap
brew install weir
```

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
