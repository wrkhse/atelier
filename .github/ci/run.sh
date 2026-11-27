#!/bin/bash
# GitHub-hosted Linux only. Never use this runner on a developer machine.
set -euo pipefail
if [[ "${GITHUB_ACTIONS:-}" != true || "${RUNNER_OS:-}" != Linux \
  || "${RUNNER_ENVIRONMENT:-}" != github-hosted ]]; then
  echo "run.sh runs only on a GitHub-hosted Linux runner; run checks in CI" >&2
  exit 2
fi

# run.sh MODE, with GATE, GATES and BASE from the environment.
MODE=${1:?mode}
export GATE=${GATE:-}
export ROOT=${ROOT:-.}
export BASE=${BASE:-}
export GATES=${GATES:-}
support=${CI_SUPPORT:?pinned CI support directory}
source=${SOURCE:?checkout}
# Host-written inspections sit beside container/, which holds only what a
# container wrote. The reporter sits alone in atelier/, mounted as /binary.
reports="$RUNNER_TEMP/atelier-reports"
output="$reports/container"
binary="$RUNNER_TEMP/atelier"
mkdir -p "$output" "$binary"

# Each job stays inside its 90 minutes: a gate is gate + validate. The
# metrics mode runs one metrics gate in the container, with no moon report
# to validate.
container_mode=$MODE
case "$MODE" in
  selftest | validate) limit=10m ;;
  gate | record-gate) limit=75m; container_mode=gate ;;
  metrics) limit=75m; container_mode=gate ;;
  *) echo "unknown mode $MODE" >&2; exit 2 ;;
esac

if [[ "$MODE" == metrics && "$GATE" != metrics/complexity \
  && "$GATE" != metrics/performance && "$GATE" != metrics/spread \
  && "$GATE" != metrics/resources ]]; then
  echo "metrics: GATE must be metrics/complexity, metrics/performance," \
    "metrics/spread or metrics/resources, not '$GATE'" >&2
  exit 2
fi

# Only recordings select a released reporter; other modes use the checkout's.
# The metrics mode validates no moon report; /binary holds only the
# checkout's atelier-metrics, which compares the trees in the container.
if [[ "$MODE" == record-gate ]]; then
  reporter=${REPORTER:?record-gate requires REPORTER}
  if [[ "$reporter" == *[[:cntrl:]]* ]]; then
    echo "record-gate: REPORTER must not contain control characters" >&2
    exit 1
  fi
elif [[ "$MODE" != metrics ]]; then
  reporter="$source/target/x86_64-unknown-linux-musl/release/atelier-reporter"
fi

if [[ "$MODE" != metrics ]]; then
  if [[ ! -f "$reporter" || -L "$reporter" ]]; then
    echo "the reporter is not a plain file at $reporter" >&2
    exit 1
  fi
  install -m 0555 -- "$reporter" "$binary/atelier-reporter"
fi

if [[ "$MODE" == metrics ]]; then
  metrics="$source/target/x86_64-unknown-linux-musl/release/atelier-metrics"
  if [[ ! -f "$metrics" || -L "$metrics" ]]; then
    echo "metrics: atelier-metrics is not a plain file at $metrics" >&2
    exit 1
  fi
  install -m 0555 -- "$metrics" "$binary/atelier-metrics"
fi

if [[ "$MODE" == record-gate ]]; then
  printf 'reporter: %s\n' "$reporter" >&2
fi

# A transient crates.io fetch failure while installing kani-verifier can fail
# one job's image build while siblings on the same head pass. Retry a bounded
# number of times before failing with a named reason.
image_built=0
for ((attempt = 1; attempt <= 3; attempt++)); do
  if docker build --tag atelier-gates --file "$support/Dockerfile" \
    "$support" >&2; then
    image_built=1
    break
  fi

  if ((attempt < 3)); then
    sleep 5
  fi
done

if ((image_built == 0)); then
  echo "docker build: image build failed after 3 attempts" >&2
  exit 1
fi
names=()
trap 'docker rm --force "${names[@]}" >/dev/null 2>&1 || true' EXIT

# Leave the runner one CPU and 2 GiB; any hosted runner size fits.
cpus=$(( $(nproc) > 1 ? $(nproc) - 1 : 1 ))
total=$(awk '/^MemTotal:/ { print int($2 / 1024) }' /proc/meminfo)
memory="$(( total > 4096 ? total - 2048 : total / 2 ))m"

# create NAME NETWORK MOUNT...: no runner home, credentials, command files or
# container-engine socket. Omitting --pid keeps Docker's private PID
# namespace. The process limit counts threads: three mutant jobs run rustc
# and nextest. GATE_OVERRIDE, when set, is the gate this one container runs,
# as metrics/performance and metrics/resources run head and base in two.
create() {
  local name=$1 network=$2
  shift 2
  local gate=${GATE_OVERRIDE:-$GATE}
  local mounts=()
  for mount in "$@"; do
    mounts+=(--mount "$mount")
  done

  docker create --name "$name" --init --user 10001:10001 \
    --cap-drop ALL --security-opt no-new-privileges \
    --pids-limit 4096 --memory "$memory" --memory-swap "$memory" --cpus "$cpus" \
    --network "$network" --ipc private --stop-timeout 10 \
    "${mounts[@]}" \
    --env "MODE=$container_mode" --env "GATE=$gate" --env ROOT --env BASE \
    --env GATES \
    atelier-gates >/dev/null
  names+=("$name")
  local sources=()
  for mount in "$@"; do
    sources+=("$(
      sed -E 's/.*type=([^,]*),source=([^,]*),target=([^,]*).*/\1:\2:\3/' \
        <<< "$mount"
    )")
  done
  contained "$name" "$network" "${sources[@]}"
}

# contained NAME NETWORK TYPE:SOURCE:TARGET...: refuse to start a container whose
# recorded settings differ from contained.jq's exact values; the inspection is
# kept as evidence. This guards against regressions and daemon defaults, not against
# an edit to run.sh itself.
contained() {
  local name=$1 network=$2
  shift 2
  docker inspect "$name" > "$reports/$name.json"
  jq -e --arg network "$network" --from-file "$support/contained.jq" --args \
    "$@" < "$reports/$name.json" >/dev/null \
    || { echo "container $name is not contained" >&2; exit 1; }
}

# start NAME: run it within the mode's limit; its exit status.
start() {
  timeout --signal=TERM --kill-after=30s "$limit" docker start --attach "$1"
}

# validate: a fresh, offline container judges the copied report with the
# reporter, both read-only; the gate cannot change either.
validate() {
  container_mode=validate
  limit=10m
  create "atelier-validate-$id" none \
    "type=bind,source=$output,target=/reports,readonly" \
    "type=bind,source=$binary,target=/binary,readonly"
  start "atelier-validate-$id"
}

id="$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT-$RANDOM"
status=0

if [[ "$container_mode" == validate ]]; then
  validate || status=1
  exit "$status"
fi

# The gate fetches crates over Docker's bridge, never the host's network; the
# self-test needs no network.
network=bridge
if [[ "$container_mode" == selftest ]]; then
  network=none
fi

# Evidence collection cannot turn a failed, timed-out or killed run green.
# docker cp keeps links, so only plain files and folders cross to the host.
if [[ "$MODE" == metrics && ( "$GATE" == metrics/performance \
  || "$GATE" == metrics/resources ) ]]; then
  # Head runs untrusted code, so it is measured in its own container, which
  # has no /reports and so cannot touch the base's evidence. Its output is
  # copied to the host, then mounted read-only into the trusted container
  # that measures the base and compares or tabulates the two. A failed head
  # run keeps status nonzero whatever the base container does.
  case "$GATE" in
    metrics/performance) short=perf ;;
    metrics/resources) short=res ;;
  esac
  headout="$RUNNER_TEMP/atelier-head-reports"
  rm -rf -- "$headout"
  mkdir -p "$headout"
  # The two containers run one after the other, so each gets 35 minutes to
  # fit both inside the job's 90-minute timeout. A job that runs the gate
  # more than once sets METRICS_CONTAINER_LIMIT, in whole minutes, so every
  # run fits. Zero is refused: timeout reads 0m as no limit at all.
  limit=${METRICS_CONTAINER_LIMIT:-35m}
  if [[ ! "$limit" =~ ^[1-9][0-9]*m$ ]]; then
    echo "metrics: METRICS_CONTAINER_LIMIT must be whole minutes above" \
      "zero, such as 12m, not '$limit'" >&2
    exit 2
  fi
  GATE_OVERRIDE="$GATE-head" create "atelier-$short-head-$id" \
    "$network" \
    "type=bind,source=$source,target=/source,readonly" \
    "type=bind,source=$binary,target=/binary,readonly"
  head_status=0
  start "atelier-$short-head-$id" || head_status=$?

  if (( head_status == 0 )); then
    docker cp "atelier-$short-head-$id:/head-out/." "$headout/" \
      >/dev/null 2>&1 || true
  else
    status=$head_status
  fi

  find "$headout" -mindepth 1 ! -type f ! -type d -delete
  find "$headout" -type f -printf '%s\n' | awk -v gate="$GATE" '
    function fail(reason) {
      print gate ": head output " reason > "/dev/stderr"
      exit 1
    }
    NR > 32 || $1 > 16 * 1024 * 1024 {
      fail("must have at most 32 regular files of at most 16 MiB each")
    }
    {
      total += $1
      if (total > 32 * 1024 * 1024) {
        fail("has more than 32 MiB of regular files in total")
      }
    }
  ' || exit 1

  GATE_OVERRIDE="$GATE" create "atelier-$short-base-$id" \
    "$network" \
    "type=bind,source=$source,target=/source,readonly" \
    "type=bind,source=$binary,target=/binary,readonly" \
    "type=bind,source=$headout,target=/head-in,readonly"
  start "atelier-$short-base-$id" || status=$?
  docker cp "atelier-$short-base-$id:/reports/." "$output/" >&2 || true
  find "$output" -mindepth 1 ! -type f ! -type d -delete
else
  create "atelier-$container_mode-$id" "$network" \
    "type=bind,source=$source,target=/source,readonly" \
    "type=bind,source=$binary,target=/binary,readonly"

  start "atelier-$container_mode-$id" || status=$?

  docker cp "atelier-$container_mode-$id:/reports/." "$output/" >&2 || true
  find "$output" -mindepth 1 ! -type f ! -type d -delete
fi

# The container's comparison decided the status. As a second fail-closed
# check, require non-empty evidence from both trees, or for metrics/spread
# the ten measured counts the workflow judges. metrics/resources only
# reports, so its evidence is the two resources files and the table.
if [[ "$MODE" == metrics ]]; then
  if [[ "$GATE" == metrics/spread ]]; then
    file="$output/spread.txt"
    if [[ ! -f "$file" || -L "$file" || ! -s "$file" ]]; then
      echo "metrics/spread: no non-empty plain report at $file" >&2
      status=1
    fi
  elif [[ "$GATE" == metrics/complexity ]]; then
    for side in base head; do
      folder="$output/$side"
      if [[ ! -d "$folder" ]]; then
        echo "metrics: no $side report folder at $folder" >&2
        status=1
      elif [[ -z "$(find "$folder" -type f -size +0c -print -quit)" ]]; then
        echo "metrics: the $side report folder $folder is empty" >&2
        status=1
      fi
    done
  else
    evidence=(base-ir.json head-ir.json summary.txt)
    if [[ "$GATE" == metrics/resources ]]; then
      evidence=(resources.json head-resources.json summary.txt)
    fi
    for report in "${evidence[@]}"; do
      file="$output/$report"
      if [[ ! -f "$file" || -L "$file" || ! -s "$file" ]]; then
        echo "$GATE: no non-empty plain report at $file" >&2
        status=1
      fi
    done
  fi
  exit "$status"
fi

if [[ "$container_mode" == gate ]]; then
  validate || status=1
fi
exit "$status"
