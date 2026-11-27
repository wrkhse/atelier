#!/usr/bin/env bash
# Checks the recorder's rc tag helpers: normalizing supplies one leading v,
# and an rc tag resolves to its predecessor with one leading v.
set -euo pipefail

recorder=${1:?pass the recorder script path}
function_file=$(mktemp)
trap 'rm -f "$function_file"' EXIT

awk '
  /^normalize_release_tag\(\)/ || /^derive_base_tag\(\)/ { found = 1 }
  found { print }
  found && /^}/ { found = 0 }
' "$recorder" > "$function_file"

for helper in normalize_release_tag derive_base_tag; do
  if ! grep -q "^${helper}()" "$function_file"; then
    echo "::error::$helper not found in $recorder"
    exit 1
  fi
done

# The extracted functions use only shell builtins.
# shellcheck source=/dev/null
source "$function_file"

check_output() {
  local helper=$1 supplied=$2 expected=$3 output status=0
  local -a lines=()

  output=$("$helper" "$supplied") || status=$?
  if ((status != 0)); then
    echo "::error::$helper $supplied failed unexpectedly: $output"
    exit 1
  fi

  mapfile -t lines <<< "$output"
  if ((${#lines[@]} != 1)); then
    echo "::error::$helper $supplied produced ${#lines[@]} lines, expected one"
    exit 1
  fi
  if [[ "${lines[0]}" != "$expected" ]]; then
    echo "::error::$helper $supplied produced '${lines[0]}', expected '$expected'"
    exit 1
  fi

  printf 'pass: %s %s -> %s\n' "$helper" "$supplied" "${lines[0]}"
}

check_failure() {
  local supplied=$1 expected=$2 output status=0

  output=$(derive_base_tag "$supplied" 2>&1) || status=$?
  if ((status == 0)); then
    echo "::error::$supplied was accepted, expected failure"
    exit 1
  fi
  if [[ "$output" != "$expected" ]]; then
    echo "::error::$supplied failed with '$output', expected '$expected'"
    exit 1
  fi

  printf 'pass: %s fails with %s\n' "$supplied" "$output"
}

check_output normalize_release_tag v0.2.0-rc.2 v0.2.0-rc.2
check_output normalize_release_tag 0.2.0-rc.2 v0.2.0-rc.2
check_output derive_base_tag v0.2.0-rc.2 v0.2.0-rc.1
check_output derive_base_tag 0.2.0-rc.2 v0.2.0-rc.1

check_failure v0.2.0-rc.0 'tag v0.2.0-rc.0 has no previous rc tag to compare'
check_failure 0.2.0-rc.0 'tag 0.2.0-rc.0 has no previous rc tag to compare'
check_failure v0.2.0 'tag v0.2.0 is not a release candidate tag'
check_failure 0.2.0 'tag 0.2.0 is not a release candidate tag'

# The intro is one waited-for string; termctrl reads the rendered screen, so
# a wrapped intro never matches. Check from the source alone that it fits on
# one row of the right pane.
pane_width=$(sed -n 's/^RIGHT_PANE_WIDTH=\([0-9][0-9]*\)$/\1/p' "$recorder")
intro=$(sed -n 's/^[[:space:]]*local intro="\([^"]*\)"$/\1/p' "$recorder")

if [[ ! "$pane_width" =~ ^[0-9]+$ ]]; then
  echo "::error::RIGHT_PANE_WIDTH constant not found in $recorder"
  exit 1
fi
if [[ -z "$intro" ]]; then
  echo "::error::intro string not found in $recorder"
  exit 1
fi
if (( ${#intro} > 10#$pane_width )); then
  echo "::error::intro length ${#intro} exceeds right pane width $pane_width"
  exit 1
fi

printf 'pass: intro fits one row: %d <= %d\n' "${#intro}" "$((10#$pane_width))"
