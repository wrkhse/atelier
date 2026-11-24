#!/bin/bash
set -euo pipefail

# Modes: selftest and gate run in the gate container; validate runs in a
# fresh one, offline, with the reports and the reporter mounted read-only.
MODE=${MODE:?mode}

if [[ "$MODE" == validate ]]; then
  read -r -a gates <<< "${GATES:-}"
  if [[ ${#gates[@]} -eq 0 ]]; then
    echo "no gates to validate" >&2
    exit 1
  fi
  args=(--report /reports/ci-report.json)
  for gate in "${gates[@]}"; do
    args+=(--gate "$gate")
  done
  exec /binary/atelier-reporter "${args[@]}"
fi

if [[ "$MODE" == selftest ]]; then
  if [[ ! -x /binary/atelier-reporter ]]; then
    echo "/binary/atelier-reporter is not executable" >&2
    exit 1
  fi
  /usr/local/bin/moon --version
  /usr/bin/jq --version
  exit
fi

if [[ "$MODE" != gate ]]; then
  echo "unknown mode $MODE" >&2
  exit 2
fi

# GATE is one gate, such as rust/fmt; moon ci runs only its task.
gate=${GATE:?gate}
task=${gate##*/}

# The host checkout is read-only and has no persisted checkout credential.
# All source execution stays here.
cp -R /source/. /work/
git config --global --add safe.directory /work
cd /work

# The mutants task reads this diff. An unreachable base fails the gate; it never passes as an empty diff.
mkdir -p target
if [[ -z "${BASE:-}" ]]; then
  : > target/mutants.diff
elif git cat-file -e "$BASE^{commit}" 2>/dev/null; then
  git diff "$BASE"...HEAD -- '*.rs' > target/mutants.diff
else
  echo "base $BASE is unavailable" >&2
  exit 1
fi

# moon's own status; its report is judged in a separate container.
status=0
moon ci ":$task" || status=$?

report=.moon/cache/ciReport.json
if [[ ! -f "$report" || -L "$report" || ! -s "$report" ]]; then
  echo "moon ci wrote no report at $report" >&2
  exit 1
fi
/usr/bin/cp "$report" /reports/ci-report.json
exit "$status"
