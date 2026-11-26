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
  # before the base's harness replaces head's. The names, one per line, go to
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

  if ! LC_ALL=C awk 'NR > 64 || length($0) > 64 { exit 1 }' \
    /head-out/new-benches.txt; then
    echo "metrics/performance-head: new-benches.txt must have at most" \
      "64 lines of at most 64 characters each" >&2
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

  # Only the harness file comes from the base. Head builds with its own
  # manifests, lockfile and .cargo settings.
  cp -- /work-base/reporter/benches/library_benchmark.rs \
    /work/reporter/benches/library_benchmark.rs

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

  # Bound the regular-file payload before the runner copies it out.
  find /head-out -type f -printf '%s\n' | awk '
    function fail(reason) {
      print "metrics/performance-head: /head-out " reason > "/dev/stderr"
      exit 1
    }
    NR > 32 { fail("has more than 32 regular files") }
    $1 > 16 * 1024 * 1024 { fail("has a regular file larger than 16 MiB") }
    {
      total += $1
      if (total > 32 * 1024 * 1024) {
        fail("has more than 32 MiB of regular files in total")
      }
    }
  '

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
  if ! LC_ALL=C awk 'NR > 64 || length($0) > 64 { exit 1 }' "$new_list"; then
    echo "metrics/performance: $new_list must have at most" \
      "64 lines of at most 64 characters each" >&2
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

  # Both sides use the base's harness file. Each tree builds with its own
  # manifests, lockfile and .cargo settings.
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

# metrics/spread runs head with the base's harness file ten times with ASLR
# allowed, as metrics/performance does, and writes each run's total Callgrind
# Ir to spread.txt, one plain number per line. The workflow judges how far
# they spread; a run without exactly one Ir value fails here.
if [[ "$gate" == metrics/spread ]]; then
  if [[ -z "${BASE:-}" ]]; then
    echo "metrics/spread needs BASE" >&2
    exit 1
  fi

  git worktree add --detach /work-base "$BASE"
  if [[ ! -f /work-base/reporter/benches/library_benchmark.rs ]]; then
    echo "metrics/spread: the base has no benchmark harness" >&2
    exit 1
  fi

  # Only the harness file comes from the base. Head builds with its own
  # manifests, lockfile and .cargo settings.
  cp -- /work-base/reporter/benches/library_benchmark.rs \
    /work/reporter/benches/library_benchmark.rs

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

# A /proc counter must be a nonnegative integer; otherwise fail, named. The
# callers check that the process exists before reading it.
require_proc_number() {
  local name=$1 value=$2

  if [[ ! "$value" =~ ^[0-9]+$ ]]; then
    echo "metrics/resources: missing or nonnumeric $name in /proc" >&2
    exit 1
  fi
}

# Sample the reporter worker's /proc entries over its idle window: peak VmRSS,
# Threads and fd count, and the last exact voluntary_ctxt_switches and wchar.
# The worker is a background child under a three-second hard timeout; the
# sampler takes at most twenty samples at 100 ms, reading /proc only while the
# worker exists. The five values go to $out/proc.json as JSON.
sample_reporter_proc() {
  local out=$1 fixture=$2
  local proc_file=$out/proc.json
  local worker='' samples=0
  local status='' io='' rss='' threads='' vctxt='' wchar='' fds=''
  local peak_rss=0 peak_threads=0 peak_fds=0

  timeout 3 bash -c "
    fixture=\$1
    while :; do
      target/release/atelier-reporter --report \"\$fixture\" --gate rust/fmt \\
        > /dev/null 2>&1 || true
    done
  " reporter-worker "$fixture" &
  worker=$!

  while (( samples < 20 )); do
    if [[ ! -d "/proc/$worker" ]]; then
      break
    fi
    status=/proc/$worker/status
    io=/proc/$worker/io
    rss=$(awk '/^VmRSS:/ { print $2; exit }' "$status") || rss=
    threads=$(awk '/^Threads:/ { print $2; exit }' "$status") || threads=
    vctxt=$(awk '/^voluntary_ctxt_switches:/ { print $2; exit }' \
      "$status") || vctxt=
    wchar=$(awk '/^wchar:/ { print $2; exit }' "$io") || wchar=
    fds=$(find "/proc/$worker/fd" -mindepth 1 -maxdepth 1 -printf . \
      2>/dev/null | wc -c) || fds=

    require_proc_number VmRSS "$rss"
    require_proc_number Threads "$threads"
    require_proc_number voluntary_ctxt_switches "$vctxt"
    require_proc_number wchar "$wchar"
    require_proc_number fd_count "$fds"

    if (( rss > peak_rss )); then
      peak_rss=$rss
    fi
    if (( threads > peak_threads )); then
      peak_threads=$threads
    fi
    if (( fds > peak_fds )); then
      peak_fds=$fds
    fi

    samples=$((samples + 1))
    sleep 0.1
  done

  kill "$worker" 2>/dev/null || true
  wait "$worker" 2>/dev/null || true

  if (( samples == 0 )); then
    echo "metrics/resources: no /proc samples of the reporter worker" >&2
    exit 1
  fi

  printf '{"idle_footprint_kib":%s,"voluntary_ctxt_switches":%s,' \
    "$peak_rss" "$vctxt" > "$proc_file"
  printf '"wchar_bytes":%s,"sampled_peak_threads":%s,' \
    "$wchar" "$peak_threads" >> "$proc_file"
  printf '"sampled_peak_fds":%s}\n' "$peak_fds" >> "$proc_file"
}

# The resources of one tree, measured from inside it with its own manifests,
# lockfile and .cargo settings: hyperfine times the reporter on a fixed
# report, Gungraun's DHAT counts the benchmark's allocations, cargo-bloat
# sizes the reporter binary and cargo metadata lists the dependencies. The raw
# outputs stay in OUT beside resources.json, which keeps only the values the
# table shows; a missing or nonnumeric value fails, named. The body is a
# subshell, so its cd stays inside it.
measure_resources() (
  local tree=$1 out=$2
  local fixture=$out/report.json
  local build_start='' build_end='' build_seconds=''

  cd "$tree"

  build_start=$(date +%s%N)
  if ! cargo build --release --locked -p atelier-reporter; then
    echo "metrics/resources: could not build atelier-reporter in $tree" >&2
    exit 1
  fi
  build_end=$(date +%s%N)
  build_seconds=$(awk -v start="$build_start" -v end="$build_end" \
    'BEGIN { printf "%.9f", (end - start) / 1000000000 }')

  if [[ ! -e "$fixture" && ! -L "$fixture" ]]; then
    cat > "$fixture" <<'JSON'
{"actions": [{
  "node": {"action": "run-task", "params": {"target": "reporter:fmt"}},
  "status": "passed",
  "label": "RunTask(reporter:fmt)"
}]}
JSON
  fi

  if [[ ! -f "$fixture" || -L "$fixture" || ! -s "$fixture" ]]; then
    echo "metrics/resources: $fixture is not a plain non-empty file" >&2
    exit 1
  fi

  if ! hyperfine --warmup 2 --runs 10 --export-json "$out/hyperfine.json" \
    --metrics time_wall_clock,time_user,time_system,memory_peak_resident \
    "target/release/atelier-reporter --report $fixture --gate rust/fmt"; then
    echo "metrics/resources: hyperfine could not time the reporter in $tree" >&2
    exit 1
  fi

  if ! cargo bench --locked --bench library_benchmark \
    -- --tools=dhat --output-format=json > "$out/dhat.jsonl"; then
    echo "metrics/resources: the DHAT benchmark failed in $tree" >&2
    exit 1
  fi

  # The workspace is virtual, so cargo-bloat is told which binary to size.
  if ! cargo bloat --release --locked -p atelier-reporter \
    --bin atelier-reporter --crates --message-format json -n 0 \
    > "$out/bloat.json"; then
    echo "metrics/resources: cargo-bloat could not size the reporter in" \
      "$tree" >&2
    exit 1
  fi

  if ! cargo metadata --format-version 1 --locked > "$out/metadata.json"; then
    echo "metrics/resources: cargo metadata failed in $tree" >&2
    exit 1
  fi

  sample_reporter_proc "$out" "$fixture"

  if ! jq -n --slurpfile hyperfine "$out/hyperfine.json" \
    --slurpfile dhat "$out/dhat.jsonl" --slurpfile bloat "$out/bloat.json" \
    --slurpfile metadata "$out/metadata.json" \
    --slurpfile proc "$out/proc.json" \
    --argjson build_seconds "$build_seconds" '
    def need($name):
      if type == "number" then .
      else error("metrics/resources: missing or nonnumeric \($name)") end;
    def list($name):
      if type == "array" then .
      else error("metrics/resources: missing list \($name)") end;

    ($hyperfine[0].results[0].summary) as $run
    | ([$dhat[] | .profiles[]? | select(.tool == "DHAT")
        | .data.total.metrics][0]) as $alloc
    | $bloat[0] as $size
    | ($metadata[0].packages | list("cargo metadata packages")) as $packages
    | {
        cpu_seconds: (($run.time_user.mean | need("time_user.mean"))
          + ($run.time_system.mean | need("time_system.mean"))),
        peak_rss_bytes: ($run.memory_peak_resident.median
          | need("memory_peak_resident.median")),
        startup_seconds: ($run.time_wall_clock.median
          | need("time_wall_clock.median")),
        build_seconds: ($build_seconds | need("build_seconds")),
        idle_footprint_kib: ($proc[0].idle_footprint_kib
          | need("idle_footprint_kib")),
        voluntary_ctxt_switches: ($proc[0].voluntary_ctxt_switches
          | need("voluntary_ctxt_switches")),
        wchar_bytes: ($proc[0].wchar_bytes | need("wchar_bytes")),
        sampled_peak_threads: ($proc[0].sampled_peak_threads
          | need("sampled_peak_threads")),
        sampled_peak_fds: ($proc[0].sampled_peak_fds
          | need("sampled_peak_fds")),
        total_bytes: ($alloc.TotalBytes.values.new
          | need("DHAT TotalBytes")),
        total_blocks: ($alloc.TotalBlocks.values.new
          | need("DHAT TotalBlocks")),
        binary_file_size: ($size["file-size"] | need("bloat file-size")),
        text_section_size: ($size["text-section-size"]
          | need("bloat text-section-size")),
        crates: ([$size.crates | list("bloat crates") | .[]
          | {name: (.name | tostring), size: (.size | need("crate size"))}]
          | sort_by(-.size) | .[:5]),
        package_count: ($packages | length),
        workspace_members: ($metadata[0].workspace_members
          | list("cargo metadata workspace_members") | length),
        duplicate_versions: ($packages | group_by(.name)
          | map(select(map(.version) | unique | length > 1)) | length)
      }
    | if (.crates | length) == 0 then
        error("metrics/resources: cargo-bloat listed no crates")
      else . end
  ' > "$out/resources.json"; then
    echo "metrics/resources: could not combine the measurements of $tree" >&2
    exit 1
  fi
)

# True when FILE is a resources.json of the shape measure_resources writes:
# every value a number, and one to five crates with short names.
resources_valid() {
  jq -e '
    def num: type == "number";

    type == "object"
    and (keys == ["binary_file_size", "build_seconds", "cpu_seconds",
      "crates", "duplicate_versions", "idle_footprint_kib",
      "package_count", "peak_rss_bytes", "sampled_peak_fds",
      "sampled_peak_threads", "startup_seconds", "text_section_size",
      "total_blocks", "total_bytes", "voluntary_ctxt_switches",
      "wchar_bytes", "workspace_members"])
    and ([.cpu_seconds, .peak_rss_bytes, .total_bytes, .total_blocks,
      .binary_file_size, .text_section_size, .package_count,
      .workspace_members, .duplicate_versions, .startup_seconds,
      .build_seconds, .idle_footprint_kib, .voluntary_ctxt_switches,
      .wchar_bytes, .sampled_peak_threads, .sampled_peak_fds] | all(num))
    and (.crates | type == "array" and length >= 1 and length <= 5
      and all(.[]; type == "object" and (.size | num)
        and (.name | type == "string" and length <= 64)))
  ' "$1" > /dev/null
}

# metrics/resources-head measures head alone, in its own container, which has
# no /reports and no access to the base's evidence: it writes only /head-out,
# which the runner copies out and the trusted metrics/resources container
# reads, read-only, at /head-in.
if [[ "$gate" == metrics/resources-head ]]; then
  if [[ -z "${BASE:-}" ]]; then
    echo "metrics/resources-head needs BASE" >&2
    exit 1
  fi

  git worktree add --detach /work-base "$BASE"

  # The base's harness measures head's allocations, as in performance.
  if [[ ! -f /work-base/reporter/benches/library_benchmark.rs ]]; then
    echo "metrics/resources-head: the base has no benchmark harness" >&2
    exit 1
  fi

  # Only the harness file comes from the base. Head builds with its own
  # manifests, lockfile and .cargo settings.
  cp -- /work-base/reporter/benches/library_benchmark.rs \
    /work/reporter/benches/library_benchmark.rs

  # The gate container drops all capabilities and fixes SecurityOpt, so
  # gungraun's setarch -R cannot set ADDR_NO_RANDOMIZE. The table reports
  # total DHAT bytes and blocks, which address layout does not change.
  export GUNGRAUN_HOME=/res-gungraun GUNGRAUN_ALLOW_ASLR=true
  mkdir -p /head-out
  measure_resources /work /head-out

  report=/head-out/resources.json
  if [[ ! -f "$report" || -L "$report" || ! -s "$report" ]]; then
    echo "metrics/resources-head: missing or empty report at $report" >&2
    exit 1
  fi

  # Bound the regular-file payload before the runner copies it out.
  find /head-out -type f -printf '%s\n' | awk '
    function fail(reason) {
      print "metrics/resources-head: /head-out " reason > "/dev/stderr"
      exit 1
    }
    NR > 32 { fail("has more than 32 regular files") }
    $1 > 16 * 1024 * 1024 { fail("has a regular file larger than 16 MiB") }
    {
      total += $1
      if (total > 32 * 1024 * 1024) {
        fail("has more than 32 MiB of regular files in total")
      }
    }
  '

  echo "metrics/resources-head: measured head"
  exit 0
fi

# metrics/resources is the trusted container: it measures the base and writes
# the before-and-after table. Head ran in a separate container with no access
# to /reports; its resources.json arrives read-only at /head-in and is copied
# beside the base's as head-resources.json. The gate only reports: it fails
# on missing evidence, never on what it measures.
if [[ "$gate" == metrics/resources ]]; then
  if [[ -z "${BASE:-}" ]]; then
    echo "metrics/resources needs BASE" >&2
    exit 1
  fi
  head_report=/head-in/resources.json
  if [[ ! -e "$head_report" && ! -L "$head_report" ]]; then
    echo "metrics/resources: head's container wrote no report at" \
      "$head_report" >&2
    exit 1
  fi
  if [[ -L "$head_report" || ! -f "$head_report" ]]; then
    echo "metrics/resources: $head_report is not a plain file" >&2
    exit 1
  fi
  if [[ ! -s "$head_report" ]]; then
    echo "metrics/resources: $head_report is empty" >&2
    exit 1
  fi
  if ! resources_valid "$head_report"; then
    echo "metrics/resources: $head_report is not a valid resources report" >&2
    exit 1
  fi

  git worktree add --detach /work-base "$BASE"

  # Only the base tree is ever built or run here; head's code never is.
  if [[ ! -f /work-base/reporter/benches/library_benchmark.rs ]]; then
    echo "metrics/resources: the base has no benchmark harness" >&2
    exit 1
  fi

  # The gate container drops all capabilities and fixes SecurityOpt, so
  # gungraun's setarch -R cannot set ADDR_NO_RANDOMIZE. The table reports
  # total DHAT bytes and blocks, which address layout does not change.
  export GUNGRAUN_HOME=/reports/gungraun GUNGRAUN_ALLOW_ASLR=true
  measure_resources /work-base /reports

  if ! resources_valid /reports/resources.json; then
    echo "metrics/resources: the base's resources report is not valid" >&2
    exit 1
  fi
  cp -- "$head_report" /reports/head-resources.json

  # Before is the base, after is head. A crate in either side's top five
  # gets a row; a side without it shows "-".
  if ! jq -nr --slurpfile base /reports/resources.json \
    --slurpfile head /reports/head-resources.json '
    def fmt:
      if type != "number" then "-"
      elif . == floor then tostring
      else (. * 1000000 | round) / 1000000 | tostring end;
    def cell: gsub("[^A-Za-z0-9_.\\[\\]-]"; "?");
    def row($metric; $before; $after; $method):
      "| \($metric) | \($before | fmt) | \($after | fmt) | \($method) |";
    def size($side; $name):
      [$side.crates[] | select(.name == $name) | .size][0];

    $base[0] as $b
    | $head[0] as $h
    | ([$h.crates[].name, $b.crates[].name]
      | reduce .[] as $n ([]; if any(.[]; . == $n) then . else . + [$n] end))
      as $names
    | "| Metric | Before | After | Method |",
      "| --- | --- | --- | --- |",
      row("CPU time (s)"; $b.cpu_seconds; $h.cpu_seconds;
        "hyperfine time_user+time_system"),
      row("Peak memory (bytes)"; $b.peak_rss_bytes; $h.peak_rss_bytes;
        "hyperfine memory_peak_resident"),
      row("Allocations (bytes)"; $b.total_bytes; $h.total_bytes;
        "Gungraun DHAT TotalBytes"),
      row("Allocations (blocks)"; $b.total_blocks; $h.total_blocks;
        "Gungraun DHAT TotalBlocks"),
      row("Binary size (file bytes)"; $b.binary_file_size;
        $h.binary_file_size; "cargo-bloat"),
      row("Binary size (text bytes)"; $b.text_section_size;
        $h.text_section_size; "cargo-bloat"),
      ($names[] as $n
        | row("Crate \($n | cell) (text bytes, top five)"; size($b; $n);
          size($h; $n); "cargo-bloat")),
      row("Packages"; $b.package_count; $h.package_count; "cargo metadata"),
      row("Workspace members"; $b.workspace_members; $h.workspace_members;
        "cargo metadata"),
      row("Duplicate versions"; $b.duplicate_versions;
        $h.duplicate_versions; "cargo metadata"),
      row("Startup time (s)"; $b.startup_seconds; $h.startup_seconds;
        "hyperfine time_wall_clock median"),
      row("Build time (s)"; $b.build_seconds; $h.build_seconds;
        "cargo build wall clock"),
      row("Idle footprint (KiB)"; $b.idle_footprint_kib;
        $h.idle_footprint_kib; "/proc status VmRSS sampled"),
      row("Voluntary context switches"; $b.voluntary_ctxt_switches;
        $h.voluntary_ctxt_switches; "/proc status voluntary_ctxt_switches"),
      row("Bytes written (wchar)"; $b.wchar_bytes; $h.wchar_bytes;
        "/proc io wchar"),
      row("Sampled peak threads, every 100 ms"; $b.sampled_peak_threads;
        $h.sampled_peak_threads; "/proc status Threads sampled"),
      row("Sampled peak FDs, every 100 ms"; $b.sampled_peak_fds;
        $h.sampled_peak_fds; "/proc fd count sampled")
  ' > /reports/summary.txt; then
    echo "metrics/resources: could not write the resources table" >&2
    exit 1
  fi
  cat -- /reports/summary.txt

  for report in resources.json head-resources.json summary.txt; do
    file=/reports/$report
    if [[ ! -f "$file" || -L "$file" || ! -s "$file" ]]; then
      echo "metrics/resources: missing or empty report at $file" >&2
      exit 1
    fi
  done
  exit 0
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
