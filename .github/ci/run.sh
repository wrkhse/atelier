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
# metrics mode runs the gate container for metrics/complexity alone, with no
# moon report to validate.
container_mode=$MODE
case "$MODE" in
  selftest | validate) limit=10m ;;
  gate | record-gate) limit=75m; container_mode=gate ;;
  metrics) limit=75m; container_mode=gate ;;
  *) echo "unknown mode $MODE" >&2; exit 2 ;;
esac

if [[ "$MODE" == metrics && "$GATE" != metrics/complexity ]]; then
  echo "metrics: GATE must be metrics/complexity, not '$GATE'" >&2
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

docker build --tag atelier-gates --file "$support/Dockerfile" "$support" >&2
names=()
trap 'docker rm --force "${names[@]}" >/dev/null 2>&1 || true' EXIT

# Leave the runner one CPU and 2 GiB; any hosted runner size fits.
cpus=$(( $(nproc) > 1 ? $(nproc) - 1 : 1 ))
total=$(awk '/^MemTotal:/ { print int($2 / 1024) }' /proc/meminfo)
memory="$(( total > 4096 ? total - 2048 : total / 2 ))m"

# create NAME NETWORK MOUNT...: no runner home, credentials, command files or
# container-engine socket. Omitting --pid keeps Docker's private PID
# namespace. The process limit counts threads: three mutant jobs run rustc
# and nextest.
create() {
  local name=$1 network=$2
  shift 2
  local mounts=()
  for mount in "$@"; do
    mounts+=(--mount "$mount")
  done
  docker create --name "$name" --init --user 10001:10001 \
    --cap-drop ALL --security-opt no-new-privileges \
    --pids-limit 4096 --memory "$memory" --memory-swap "$memory" --cpus "$cpus" \
    --network "$network" --ipc private --stop-timeout 10 \
    "${mounts[@]}" \
    --env "MODE=$container_mode" --env GATE --env ROOT --env BASE --env GATES \
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

create "atelier-$container_mode-$id" "$network" \
  "type=bind,source=$source,target=/source,readonly" \
  "type=bind,source=$binary,target=/binary,readonly"

start "atelier-$container_mode-$id" || status=$?

# Evidence collection cannot turn a failed, timed-out or killed run green.
# docker cp keeps links, so only plain files and folders cross to the host.
docker cp "atelier-$container_mode-$id:/reports/." "$output/" >&2 || true
find "$output" -mindepth 1 ! -type f ! -type d -delete

# The container's comparison decided the status. As a second fail-closed
# check, each side must have come back as a folder holding at least one
# non-empty report.
if [[ "$MODE" == metrics ]]; then
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
  exit "$status"
fi

if [[ "$container_mode" == gate ]]; then
  validate || status=1
fi
exit "$status"
