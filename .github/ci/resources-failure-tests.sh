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
  /^(gnu_time_field|switch_delta|terminate_tree)\(\) \{/ { inside = 1 }
  /^(gnu_time_elapsed_ms|validate_samples|run_bounded)\(\) \{/ { inside = 1 }
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

# True while the Case C fixture command is alive. That script is the only
# "sleep 60" this test starts, so a survivor means the teardown left the
# timed-out child running.
sleep_fixture_remains() {
  local entry line

  for entry in /proc/[0-9]*/cmdline; do
    [[ -r "$entry" ]] || continue
    line=$(tr '\0' ' ' < "$entry") || continue
    if [[ "$line" == 'sleep 60 '* ]]; then
      return 0
    fi
  done
  return 1
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

# Case B2: GNU time's m:ss elapsed field is converted to whole milliseconds.
printf 'Elapsed (wall clock) time (m:ss): 1:23.45\n' > "$tmp/gnu-time.txt"
output=$(gnu_time_elapsed_ms "$tmp/gnu-time.txt") || fail "case B2: $output"
if [[ "$output" != 83450 ]]; then
  fail "case B2: expected 83450 ms, got $output"
fi
printf 'ok: case B2: GNU time elapsed m:ss to ms\n'

# Case B3: GNU time output without an elapsed field fails, naming the field.
printf 'Command being timed: "true"\n' > "$tmp/gnu-time.txt"
status=0
output=$(gnu_time_elapsed_ms "$tmp/gnu-time.txt" 2>&1) || status=$?
expect_named_failure 'case B3: missing GNU time elapsed field' \
  'metrics/resources: missing or malformed Elapsed (wall clock) time in GNU time' \
  "$status" "$output"

# Case C: two ticks give the child no chance to finish; the parent must tear
# it down quickly, name the timeout and leave no child running. A teardown
# that never signals would let the 60-second sleep finish on its own.
case_c_start=$(date +%s%N)
status=0
run_bounded 2 timeout-test sleep 60 2> "$tmp/case-c.err" || status=$?
case_c_end=$(date +%s%N)

case_c_ms=$(awk -v start="$case_c_start" -v end="$case_c_end" \
  'BEGIN { printf "%.0f", (end - start) / 1000000 }')
if (( case_c_ms >= 5000 )); then
  fail "case C: teardown took ${case_c_ms} ms for a 60-second sleep"
fi
if sleep_fixture_remains; then
  fail 'case C: the timed-out sleep 60 is still running'
fi
output=$(cat -- "$tmp/case-c.err")
expect_named_failure 'case C: bounded child timeout' \
  'timeout-test timed out in' "$status" "$output"

# Case D: switch_delta returns the real difference of two cumulative counters
# and fails named when the end counter is missing or nonnumeric.
output=$(switch_delta 20 27 window-fixture) || fail "case D: $output"
if [[ "$output" != 7 ]]; then
  fail "case D: expected a delta of 7, got $output"
fi
printf 'ok: case D: switch delta\n'

status=0
output=$(switch_delta 20 xx window-fixture 2>&1) || status=$?
expect_named_failure 'case D: malformed end counter' \
  'metrics/resources: could not read voluntary_ctxt_switches at the end' \
  "$status" "$output"
