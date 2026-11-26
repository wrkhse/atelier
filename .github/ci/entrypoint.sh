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

# The one Callgrind Ir value named field (new or old) in a benchmark report.
extract_ir() {
  local field=$1 report=$2

  jq -nce --arg field "$field" '
    [limit(2; inputs | .profiles[]? | select(.tool == "Callgrind")
      | .data.total.metrics.Ir.values[$field])]
    | if length > 1 then
        error("metrics/performance: duplicate Callgrind Ir values.\($field)")
      elif .[0] == null then
        error("metrics/performance: missing Callgrind Ir values.\($field)")
      elif (.[0] | type) != "number" then
        error("metrics/performance: nonnumeric Callgrind Ir values.\($field)")
      else {ir: .[0]} end
  ' "$report"
}

# The instruction_limit_percent in a tree's .moon/config/metrics.toml, or the
# default 5 when the tree has no such file. A file that is not a regular one,
# or that lacks exactly one numeric value, fails.
instruction_limit() {
  local config=$1/.moon/config/metrics.toml limit

  if [[ ! -e "$config" && ! -L "$config" ]]; then
    echo 5
    return
  fi
  if [[ ! -f "$config" || -L "$config" ]]; then
    echo "metrics/performance: $config is not a regular file" >&2
    return 1
  fi
  if ! limit=$(awk '
    sub(/^[[:space:]]*instruction_limit_percent[[:space:]]*=[[:space:]]*/, "") {
      sub(/[[:space:]]*(#.*)?$/, "")
      print
    }
  ' "$config") || [[ ! "$limit" =~ ^-?[0-9]+([.][0-9]+)?$ ]]; then
    echo "metrics/performance: expected one numeric instruction_limit_percent" \
      "in $config" >&2
    return 1
  fi
  echo "$limit"
}

# The new_bench_instruction_ir in a tree's .moon/config/metrics.toml: the
# absolute Callgrind Ir budget of a benchmark with no counterpart on the base.
# Prints nothing when the tree has no such file or no such key. A file that is
# not a regular one, or a key that is not set exactly once to a positive
# integer, fails.
new_bench_budget() {
  local config=$1/.moon/config/metrics.toml budget

  if [[ ! -e "$config" && ! -L "$config" ]]; then
    return
  fi
  if [[ ! -f "$config" || -L "$config" ]]; then
    echo "metrics/performance: $config is not a regular file" >&2
    return 1
  fi
  if ! budget=$(awk '
    sub(/^[[:space:]]*new_bench_instruction_ir[[:space:]]*=[[:space:]]*/, "") {
      sub(/[[:space:]]*(#.*)?$/, "")
      print "value:" $0
    }
  ' "$config"); then
    echo "metrics/performance: could not read $config" >&2
    return 1
  fi
  if [[ -z "$budget" ]]; then
    return
  fi
  if [[ ! "$budget" =~ ^value:[1-9][0-9]{0,17}$ ]]; then
    echo "metrics/performance: expected one positive integer" \
      "new_bench_instruction_ir in $config" >&2
    return 1
  fi
  echo "${budget#value:}"
}

# metrics/performance-head measures head alone, in its own container, which
# has no /reports and no access to the base's evidence: it writes only
# /head-out, holding head.jsonl, new-benches.txt and each head-only
# benchmark's new-NAME-ir.json, which the runner copies out and the trusted
# metrics/performance container reads, read-only, at /head-in.
if [[ "$gate" == metrics/performance-head ]]; then
  if [[ -z "${BASE:-}" ]]; then
    echo "metrics/performance-head needs BASE" >&2
    exit 1
  fi

  git worktree add --detach /work-base "$BASE"

  # The base's harness governs head's measurement, so a base without one has
  # nothing to measure head by. Head's harness is never used in its place.
  if [[ ! -f /work-base/reporter/benches/library_benchmark.rs ]]; then
    echo "metrics/performance-head: the base has no benchmark harness" >&2
    exit 1
  fi

  # The gate container drops all capabilities and fixes SecurityOpt, so
  # gungraun's setarch -R cannot set ADDR_NO_RANDOMIZE. The check compares
  # total Callgrind Ir counts, which address layout does not change.
  export GUNGRAUN_HOME=/head-gungraun GUNGRAUN_ALLOW_ASLR=true
  mkdir -p /head-out

  # A benchmark that head's reporter manifest names and the base's does not
  # has no base count to compare with; the trusted container judges it
  # against an absolute budget. Head's own manifest builds it, so these run
  # before the base's files replace head's. The names, one per line, go to
  # new-benches.txt, empty when there are none.
  if ! python3 - /source/reporter/Cargo.toml /work-base/reporter/Cargo.toml \
    > /head-out/new-benches.txt <<'PY'
import re
import sys
import tomllib


def benches(path):
    with open(path, "rb") as manifest:
        targets = tomllib.load(manifest).get("bench", [])
    if not isinstance(targets, list):
        sys.exit(f"{path}: [[bench]] is not an array of tables")
    names = []
    for target in targets:
        name = target.get("name") if isinstance(target, dict) else None
        if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", name):
            sys.exit(f"{path}: a [[bench]] has no plain name")
        names.append(name)
    return names


head = benches(sys.argv[1])
base = set(benches(sys.argv[2]))
for name in dict.fromkeys(head):
    if name not in base:
        print(name)
PY
  then
    echo "metrics/performance-head: could not list head's benchmarks" >&2
    exit 1
  fi

  mapfile -t new_benches < /head-out/new-benches.txt
  for name in "${new_benches[@]}"; do
    report="/head-out/new-$name.jsonl"
    if ! cargo bench --locked --bench "$name" -- --output-format=json \
      > "$report"; then
      echo "metrics/performance-head: head-only benchmark $name failed" >&2
      exit 1
    fi
    if ! extract_ir new "$report" > "/head-out/new-$name-ir.json"; then
      echo "metrics/performance-head: head-only benchmark $name has no" \
        "single Callgrind Ir value" >&2
      exit 1
    fi
  done

  # The base's library_benchmark.rs, reporter manifest, workspace Cargo.toml
  # and .cargo/ replace head's, so a pull request cannot weaken the benchmark
  # it is measured by, point [lib] or [[bench]] at a decoy, or change how it
  # is compiled. Head keeps its own lockfile.
  cp -- /work-base/reporter/benches/library_benchmark.rs \
    /work/reporter/benches/library_benchmark.rs
  cp -- /work-base/reporter/Cargo.toml /work/reporter/Cargo.toml
  cp -- /work-base/Cargo.toml /work/Cargo.toml
  rm -rf -- /work/.cargo
  if [[ -d /work-base/.cargo ]]; then
    cp -R -- /work-base/.cargo /work/.cargo
  fi

  # --locked keeps the build on head's lockfile.
  cargo bench --locked --bench library_benchmark -- --output-format=json \
    > /head-out/head.jsonl

  report=/head-out/head.jsonl
  if [[ ! -f "$report" || -L "$report" || ! -s "$report" ]]; then
    echo "metrics/performance-head: missing or empty benchmark report at" \
      "$report" >&2
    exit 1
  fi
  if ! extract_ir new "$report" > /dev/null; then
    echo "metrics/performance-head: $report has no single Callgrind Ir value" >&2
    exit 1
  fi
  echo "metrics/performance-head: measured head"
  exit 0
fi

# metrics/performance is the trusted container: it measures the base and
# judges. Head ran in a separate container with no access to /reports, so it
# cannot rewrite the base measurement or the limit; its report arrives
# read-only at /head-in, and the two Ir values are compared directly.
if [[ "$gate" == metrics/performance ]]; then
  if [[ -z "${BASE:-}" ]]; then
    echo "metrics/performance needs BASE" >&2
    exit 1
  fi
  head_report=/head-in/head.jsonl
  if [[ ! -e "$head_report" && ! -L "$head_report" ]]; then
    echo "metrics/performance: head's container wrote no report at" \
      "$head_report" >&2
    exit 1
  fi
  if [[ -L "$head_report" || ! -f "$head_report" ]]; then
    echo "metrics/performance: $head_report is not a plain file" >&2
    exit 1
  fi
  if [[ ! -s "$head_report" ]]; then
    echo "metrics/performance: $head_report is empty" >&2
    exit 1
  fi
  # Head's container lists its head-only benchmarks here, one per line; an
  # empty file means there are none, a missing one fails.
  new_list=/head-in/new-benches.txt
  if [[ -L "$new_list" || ! -f "$new_list" ]]; then
    echo "metrics/performance: $new_list is not a plain file" >&2
    exit 1
  fi
  mapfile -t new_benches < "$new_list"
  for name in "${new_benches[@]}"; do
    if [[ ! "$name" =~ ^[A-Za-z0-9_-]+$ ]]; then
      echo "metrics/performance: $new_list names an invalid benchmark" >&2
      exit 1
    fi
  done
  if [[ ! -x /binary/atelier-metrics ]]; then
    echo "/binary/atelier-metrics is not executable" >&2
    exit 1
  fi

  git worktree add --detach /work-base "$BASE"

  # Only the base's harness is ever built or run here; head's code never is.
  if [[ ! -f /work-base/reporter/benches/library_benchmark.rs ]]; then
    echo "metrics/performance: the base has no benchmark harness" >&2
    exit 1
  fi

  # The stricter of the two limits applies: a pull request may tighten its
  # budget, but one that raises it is measured against the base's.
  base_limit=$(instruction_limit /work-base) || exit 1
  head_limit=$(instruction_limit /work) || exit 1
  if awk -v base="$base_limit" -v head="$head_limit" \
    'BEGIN { exit !(head + 0 < base + 0) }'; then
    limit=$head_limit limit_from=head
  else
    limit=$base_limit limit_from=base
  fi
  limit_line="limit: instruction_limit_percent $limit from $limit_from"
  limit_line+=" (base $base_limit, head $head_limit)"
  echo "metrics/performance: $limit_line"

  # Both sides run the base's benchmark, built with the base's profile and
  # rustflags, which the head container applied to its side.
  # The gate container drops all capabilities and fixes SecurityOpt, so
  # gungraun's setarch -R cannot set ADDR_NO_RANDOMIZE. The check compares
  # total Callgrind Ir counts, which address layout does not change.
  export GUNGRAUN_HOME=/reports/gungraun GUNGRAUN_ALLOW_ASLR=true

  # --locked keeps the build on the base tree's lockfile.
  (
    cd /work-base
    cargo bench --locked --bench library_benchmark \
      -- --save-baseline=base --output-format=json > /reports/base.jsonl
  )

  report=/reports/base.jsonl
  if [[ ! -f "$report" || -L "$report" || ! -s "$report" ]]; then
    echo "metrics/performance: missing or empty benchmark report at $report" >&2
    exit 1
  fi

  # Each side's single measured count, compared directly: base from this
  # container, head from its own.
  extract_ir new /reports/base.jsonl > /reports/base-ir.json
  extract_ir new "$head_report" > /reports/head-ir.json

  echo "$limit_line" > /reports/summary.txt
  status=0
  /binary/atelier-metrics budget --base /reports/base-ir.json \
    --head /reports/head-ir.json --limit "$limit" \
    | tee -a /reports/summary.txt || status=$?

  # Each head-only benchmark has no base count, so its head count is judged
  # against the absolute budget: a count above it fails at limit 0. The
  # smaller of the base's and head's budgets applies, and a missing one fails.
  if (( ${#new_benches[@]} > 0 )); then
    base_budget=$(new_bench_budget /work-base) || exit 1
    head_budget=$(new_bench_budget /work) || exit 1
    if [[ -z "$base_budget" && -z "$head_budget" ]]; then
      echo "metrics/performance: head adds benchmarks but neither tree sets" \
        "new_bench_instruction_ir" >&2
      exit 1
    fi
    budget=${base_budget:-$head_budget}
    if [[ -n "$head_budget" ]] && (( head_budget < budget )); then
      budget=$head_budget
    fi
    for name in "${new_benches[@]}"; do
      printf '{"ir":%s}\n' "$budget" > "/reports/new-$name-budget.json"
      echo "new benchmark $name: new_bench_instruction_ir $budget" \
        "(base ${base_budget:-none}, head ${head_budget:-none})" \
        | tee -a /reports/summary.txt
      bench_status=0
      /binary/atelier-metrics budget --base "/reports/new-$name-budget.json" \
        --head "/head-in/new-$name-ir.json" --limit 0 \
        | tee -a /reports/summary.txt || bench_status=$?
      if (( bench_status != 0 )); then
        status=$bench_status
      fi
    done
  fi
  exit "$status"
fi

# metrics/spread runs the checked-out tree's benchmark ten times with ASLR
# allowed, as metrics/performance does, and writes each run's total Callgrind
# Ir to spread.txt, one plain number per line. The workflow judges how far
# they spread; a run without exactly one Ir value fails here.
if [[ "$gate" == metrics/spread ]]; then
  export GUNGRAUN_HOME=/reports/gungraun GUNGRAUN_ALLOW_ASLR=true
  values=()
  for run in {1..10}; do
    report="/reports/spread-$run.jsonl"
    cargo bench --locked --bench library_benchmark -- --output-format=json \
      > "$report"
    if [[ ! -f "$report" || -L "$report" || ! -s "$report" ]]; then
      echo "metrics/spread: run $run wrote no benchmark report at $report" >&2
      exit 1
    fi
    if ! ir=$(extract_ir new "$report" | jq -r .ir); then
      echo "metrics/spread: run $run has no single Callgrind Ir value" >&2
      exit 1
    fi
    values+=("$ir")
  done
  printf '%s\n' "${values[@]}" > /reports/spread.txt
  cat -- /reports/spread.txt
  exit
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
