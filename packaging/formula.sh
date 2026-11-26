#!/bin/sh
# Prints the Homebrew formula for a release.
#
#   packaging/formula.sh <version> <dir holding the release's .sha256 files>
#
# The release workflow runs this and pushes the result to the tap,
# https://github.com/tjwaterman99/homebrew-tap, as Formula/weir.rb.
set -eu

version="$1"
dist="$2"
base="https://github.com/tjwaterman99/weir/releases/download/v$version"

sha() {
  file="$dist/weir-$version-$1.tar.gz.sha256"
  [ -f "$file" ] || { echo "missing $file" >&2; exit 1; }
  cut -d' ' -f1 "$file"
}

cat <<FORMULA
class Weir < Formula
  desc "Keeps local, read-only mirrors of your repositories up to date"
  homepage "https://github.com/tjwaterman99/weir"
  version "$version"

  on_macos do
    on_arm do
      url "$base/weir-$version-aarch64-apple-darwin.tar.gz"
      sha256 "$(sha aarch64-apple-darwin)"
    end
    on_intel do
      url "$base/weir-$version-x86_64-apple-darwin.tar.gz"
      sha256 "$(sha x86_64-apple-darwin)"
    end
  end

  on_linux do
    on_arm do
      url "$base/weir-$version-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "$(sha aarch64-unknown-linux-gnu)"
    end
    on_intel do
      url "$base/weir-$version-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "$(sha x86_64-unknown-linux-gnu)"
    end
  end

  def install
    bin.install "weir"
  end

  def caveats
    <<~EOS
      To keep your mirrors current in the background, run:
        weir service install
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/weir version")
  end
end
FORMULA
