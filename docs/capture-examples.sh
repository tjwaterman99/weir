#!/bin/bash
# Regenerates the example output shown in the documentation by running the
# real binary against throwaway repositories (the `local` provider the test
# suite uses). Nothing touches the network, your mirrors or your database.
#
#   cargo build && docs/capture-examples.sh
#
# Commit ids are stable because every commit is made at a fixed date. Paths,
# times and durations are rewritten so the files do not change run to run.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
weir="${WEIR:-$here/../target/debug/weir}"
out="$here/src/examples/out"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$out" "$tmp/home" "$tmp/up" "$tmp/work" "$tmp/Repos"

export HOME="$tmp/userhome" WEIR_HOME="$tmp/home"
export GIT_CONFIG_GLOBAL="$tmp/gitconfig" GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_DATE="2026-11-02T09:00:00Z" GIT_COMMITTER_DATE="2026-11-02T09:00:00Z"
mkdir -p "$HOME"
printf '[user]\n\tname = Example\n\temail = example@example.com\n[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n' > "$GIT_CONFIG_GLOBAL"

host=git.example.com
mirror() { echo "$tmp/Repos/$host/$1/$2"; }

make_repo() { # owner name
  git init -q --bare -b main "$tmp/up/$1/$2.git"
  git clone -q "$tmp/up/$1/$2.git" "$tmp/work/$1-$2" 2>/dev/null
  push "$1" "$2" README.md "# $2"
}
push() { # owner name file content
  ( cd "$tmp/work/$1-$2" && echo "$4" > "$3" && git add . && git commit -q -m "change $3" && git push -q origin HEAD:main )
}
# Runs weir, saves what it printed (both streams) to a file, and never stops the script.
capture() { # name, then the weir arguments
  local name="$1"; shift
  { echo "\$ weir $*"; "$weir" "$@" 2>&1 || true; } \
    | sed -e "s#$tmp/Repos#/Users/me/Repos#g" -e "s#$tmp/up#/srv/git#g" \
          -e 's/[0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\}T[0-9:]\{8\}Z/2026-11-26T08:30:00Z/g' \
          -e 's/ in [0-9]*s$/ in 2s/' -e 's/took [0-9]*s/took 2s/' \
          -e 's/([0-9hms]* ago)/(4m10s ago)/g' -e 's/  [0-9]*s  / 2s  /' \
          -e 's/([0-9]*s listing)/(1s listing)/' -e 's/^ *[0-9.]*s  /     0.4s  /' \
    > "$out/$name.txt"
}

"$weir" config set root "$tmp/Repos" > /dev/null
"$weir" provider add local "$host" --api-url "$tmp/up" > /dev/null
for name in api web docs; do make_repo acme "$name"; done

capture track       track "$host/acme"
capture sync-first  sync
capture list        list
capture path        path api
push acme api src.txt "retry_policy = 3"
capture sync-update sync
capture status      status

# Someone checks out a branch in one mirror and edits a file in another.
git -C "$(mirror acme web)" checkout -q -b quick-fix
chmod u+w "$(mirror acme docs)/README.md" && echo "my note" >> "$(mirror acme docs)/README.md"
push acme docs guide.md "# Guide"
capture sync-problems   sync
capture status-problems status
git -C "$(mirror acme docs)" checkout -q -- README.md
"$weir" sync > /dev/null

# Upstream changes: one repository is archived, one deleted, one appears.
touch "$tmp/up/acme/web.git/weir-archived"
rm -rf "$tmp/up/acme/docs.git"
make_repo acme cli
capture sync-dry-run  sync --dry-run
capture sync-upstream sync
capture list-archived list --archived
capture prune         prune --dry-run

# A clone weir does not manage: a leftover copy under an old name.
echo "acme/api-old" > "$tmp/up/acme/api.git/weir-old-names"
git clone -q "$tmp/up/acme/api.git" "$(mirror acme api-old)"
git -C "$(mirror acme api-old)" remote set-url origin "$tmp/up/acme/api-old.git"
mkdir -p "$(mirror acme scratch)" && git -C "$(mirror acme scratch)" init -q
capture doctor doctor

capture log    log -n 3
capture config config list
echo "wrote $(ls "$out" | wc -l | tr -d ' ') files to $out"
