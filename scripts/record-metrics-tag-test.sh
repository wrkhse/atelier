#!/usr/bin/env bash
# Checks derive_base_tag from the recorder: an rc tag resolves to its
# predecessor with one leading v, whatever form was supplied.
set -euo pipefail

recorder=${1:?pass the recorder script path}
function_file=$(mktemp)
trap 'rm -f "$function_file"' EXIT

awk '
  /^derive_base_tag/ { found = 1 }
  found { print }
  found && /^}/ { exit }
' "$recorder" > "$function_file"

if [[ ! -s "$function_file" ]]; then
  echo "::error::derive_base_tag not found in $recorder"
  exit 1
fi

# The extracted function uses only shell builtins.
# shellcheck source=/dev/null
source "$function_file"

check_base() {
  local supplied=$1 expected=$2 output status=0
  local -a lines=()

  output=$(derive_base_tag "$supplied") || status=$?
  if ((status != 0)); then
    echo "::error::$supplied failed unexpectedly: $output"
    exit 1
  fi

  mapfile -t lines <<< "$output"
  if ((${#lines[@]} != 1)); then
    echo "::error::$supplied produced ${#lines[@]} lines, expected one"
    exit 1
  fi
  if [[ "${lines[0]}" != "$expected" ]]; then
    echo "::error::$supplied produced '${lines[0]}', expected '$expected'"
    exit 1
  fi

  printf 'pass: %s -> %s\n' "$supplied" "${lines[0]}"
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

check_base v0.2.0-rc.2 v0.2.0-rc.1
check_base 0.2.0-rc.2 v0.2.0-rc.1

check_failure v0.2.0-rc.0 'tag v0.2.0-rc.0 has no previous rc tag to compare'
check_failure v0.2.0 'tag v0.2.0 is not a release candidate tag'
