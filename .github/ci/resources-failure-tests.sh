#!/usr/bin/env bash
# Named-failure checks for the resources helpers in the CI entrypoint. The
# helpers are read from the entrypoint, never duplicated into this repository.
set -euo pipefail

entrypoint=${1:?usage: resources-failure-tests.sh ENTRYPOINT}
if [[ ! -f "$entrypoint" || ! -r "$entrypoint" ]]; then
  echo "::error::entrypoint is not a readable file: $entrypoint" >&2
  exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

# Take each named helper from its signature to the closing brace at the left
# margin. Every nested brace in these helpers is indented, so only the
# function's own brace ends a block.
awk '
  /^(gnu_time_field|terminate_tree|validate_samples|run_bounded)\(\) \{/ {
    inside = 1
  }
  inside { print }
  inside && /^}/ { inside = 0 }
' "$entrypoint" > "$tmp/helpers.sh"

# shellcheck source=/dev/null
source "$tmp/helpers.sh"

fail() {
  echo "::error::$*" >&2
  exit 1
}

# Requires a nonzero exit and the named text in a case's captured output.
# The helper runs in a command substitution, so a bare exit inside it cannot
# end this script before the check.
expect_named_failure() {
  local case_name=$1 expected=$2 status=$3 output=$4

  (( status != 0 )) \
    || fail "$case_name expected a nonzero exit, got $status"
  grep -Fq -- "$expected" <<< "$output" \
    || fail "$case_name expected '$expected' in output: $output"
  printf 'ok: %s\n' "$case_name"
}

# Case A: nine valid four-number records are one short of the ten the sampler
# keeps, so validate_samples must fail and name the process it sampled.
for ((i = 0; i < 9; i++)); do
  printf '1024 1 0 3\n'
done > "$tmp/samples.txt"

status=0
output=$(validate_samples "$tmp/samples.txt" idle-fixture 2>&1) || status=$?
expect_named_failure 'case A: too few samples' \
  'metrics/resources: too few valid /proc samples of idle-fixture' \
  "$status" "$output"

# Case B: GNU time output without the named field must fail, naming it.
status=0
output=$(gnu_time_field 'Command being timed: "true"' \
  'Voluntary context switches' 2>&1) || status=$?
expect_named_failure 'case B: missing GNU time field' \
  'metrics/resources: missing or nonnumeric Voluntary context switches in GNU time' \
  "$status" "$output"

# Case C: two ticks give the child no chance to finish; the parent tears it
# down and reports the timeout, naming the label and the directory.
status=0
output=$(run_bounded 2 timeout-test sleep 60 2>&1) || status=$?
expect_named_failure 'case C: bounded child timeout' \
  'timeout-test timed out in' "$status" "$output"
