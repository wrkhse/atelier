#!/usr/bin/env bash
set -euo pipefail

fail() {
  echo "mutants: $*" >&2
  exit 1
}

diff=${ATELIER_MUTANTS_DIFF:-}

if [[ ! -f "$diff" || ! -s "$diff" ]]; then
  base=${ATELIER_BASE:-origin/main}

  if ! command -v git >/dev/null; then
    fail "git is unavailable; set ATELIER_MUTANTS_DIFF to a non-empty file"
  fi
  if ! root=$(git rev-parse --show-toplevel 2>/dev/null); then
    fail "no Git worktree; set ATELIER_MUTANTS_DIFF to a non-empty file"
  fi
  if ! commit=$(
    git rev-parse --verify --quiet --end-of-options "$base^{commit}"
  ); then
    fail "base $base is unavailable; set ATELIER_BASE to a reachable commit"
  fi

  diff="$root/target/mutants.diff"
  mkdir -p "$root/target"

  if ! git -C "$root" diff "$commit"...HEAD -- '*.rs' > "$diff"; then
    fail "could not compute the Rust diff from $base to HEAD"
  fi
fi

exec cargo mutants --in-diff "$diff" --timeout 120 --jobs 3
