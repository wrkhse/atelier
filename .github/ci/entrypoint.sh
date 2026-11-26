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

# GATE is all or one gate, such as rust/fmt.
gate=${GATE:?gate}

# The host checkout is read-only and has no persisted checkout credential.
# All source execution stays here.
cp -R /source/. /work/
git config --global --add safe.directory /work
cd /work

# The mutants task reads this absolute path from any project directory.
# An unreachable base fails the gate; it never passes as an empty diff.
diff="$PWD/target/mutants.diff"
mkdir -p target
if [[ -z "${BASE:-}" ]]; then
  : > "$diff"
elif git cat-file -e "$BASE^{commit}" 2>/dev/null; then
  git diff "$BASE"...HEAD -- '*.rs' > "$diff"
else
  echo "base $BASE is unavailable" >&2
  exit 1
fi
export ATELIER_MUTANTS_DIFF="$diff"

# metrics/complexity measures the base and head trees with the same pinned
# rust-code-analysis, one json file per source file with every function in
# it; atelier-metrics then compares them here, in the container. Run from
# inside each tree, the files are named by relative path, so both sides line
# up. A side with no json fails, as it would compare nothing, and so does a
# side with fewer json files than Rust files, as a source file would go
# unmeasured. The files are listed one by one, since the tool skips a hidden
# path such as "." and so walks nothing when given the tree itself. Both
# listings stop at one past the bound, so neither is held whole: head may end
# find early, which only happens past the bound, and that fails on its count.
analyze() {
  local tree=$1 side=$2 listing reports file
  local -i listed=0 counted=0
  local -a files=() found=() args=()
  local -r max_files=4096
  mkdir -p "/reports/$side"
  listing=$(cd "$tree" && find . -type f -name '*.rs' -not -path './target/*' \
    -not -path './.git/*' -print | head -n "$((max_files + 1))") || listed=$?
  if [[ -n "$listing" ]]; then
    listing=$(LC_ALL=C sort <<< "$listing")
    mapfile -t files <<< "$listing"
  fi
  if (( ${#files[@]} > max_files )); then
    echo "metrics/complexity: $side has over $max_files Rust files" >&2
    exit 1
  fi
  if (( listed != 0 )); then
    echo "metrics/complexity: could not list the Rust files of $side" >&2
    exit 1
  fi
  for file in "${files[@]}"; do
    args+=(-p "$file")
  done
  (cd "$tree" && rust-code-analysis-cli -m -O json --pr -o "/reports/$side" \
    "${args[@]}")
  # -type f leaves out symlinks, so only files the tool wrote are counted.
  reports=$(find "/reports/$side" -type f -name '*.json' -print \
    | head -n "$((max_files + 1))") || counted=$?
  if [[ -n "$reports" ]]; then
    mapfile -t found <<< "$reports"
  fi
  if (( ${#found[@]} == 0 )); then
    echo "metrics/complexity: rust-code-analysis wrote no json for $side" >&2
    exit 1
  fi
  if (( ${#found[@]} < ${#files[@]} )); then
    echo "metrics/complexity: $side has ${#files[@]} Rust files but only" \
      "${#found[@]} json reports; a source file has no report" >&2
    exit 1
  fi
  if (( ${#found[@]} > ${#files[@]} )); then
    echo "metrics/complexity: $side has ${#files[@]} Rust files but" \
      "${#found[@]} json reports; a report has no source file" >&2
    exit 1
  fi
  if (( counted != 0 )); then
    echo "metrics/complexity: could not list the json reports of $side" >&2
    exit 1
  fi
}

if [[ "$gate" == metrics/complexity ]]; then
  if [[ -z "${BASE:-}" ]]; then
    echo "metrics/complexity needs BASE" >&2
    exit 1
  fi
  git worktree add --detach /work-base "$BASE"
  analyze /work-base base
  analyze /work head
  if [[ ! -x /binary/atelier-metrics ]]; then
    echo "/binary/atelier-metrics is not executable" >&2
    exit 1
  fi
  # The comparison decides the check: with pipefail, its exit status is the
  # container's. The runner only shows summary.txt.
  status=0
  /binary/atelier-metrics complexity --base /reports/base --head /reports/head \
    | tee /reports/summary.txt || status=$?
  exit "$status"
fi

# moon's own status; its report is judged in a separate container.
status=0
if [[ "$gate" == all ]]; then
  moon ci || status=$?
else
  moon ci ":${gate##*/}" || status=$?
fi

report=.moon/cache/ciReport.json
if [[ ! -f "$report" || -L "$report" || ! -s "$report" ]]; then
  echo "moon ci wrote no report at $report" >&2
  exit 1
fi
/usr/bin/cp "$report" /reports/ci-report.json
exit "$status"
