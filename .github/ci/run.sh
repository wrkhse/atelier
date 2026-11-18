#!/bin/bash
# GitHub-hosted Linux only. Never use this runner on a developer machine.
set -euo pipefail
if [[ "${GITHUB_ACTIONS:-}" != true || "${RUNNER_OS:-}" != Linux || "${RUNNER_ENVIRONMENT:-}" != github-hosted ]]; then
  echo "run.sh runs only on a GitHub-hosted Linux runner; run checks in CI" >&2
  exit 2
fi

export MODE=${1:?mode}
export GATE=${2:-}
export ROOT=${3:-.}
export BASE=${4:-}
support=${CI_SUPPORT:?pinned CI support directory}
# atelier's own floor (both core proofs, the mutants canary) applies when the
# workflow runs from the repository it checks: atelier's own CI.
FLOOR=$(bash "$support/floor.sh")
export FLOOR
# Host-written inspections sit beside container/, which holds only what a
# container wrote.
reports="$RUNNER_TEMP/atelier-reports"
output="$reports/container"
mkdir -p "$output" "$RUNNER_TEMP/atelier"

# Each job stays inside its 90 minutes: plan is selftest + plan, a gate is
# gate + validate, a partition is validate alone, and the image job builds,
# pushes and inspects one image.
case "$MODE" in
  selftest | plan-release) limit=10m ;;
  plan) limit=60m ;;
  gate) limit=75m ;;
  # A partition only runs the validator, so it takes validation's budget.
  partition) limit=10m ;;
  # The image job runs no container; its build is the slowest thing a job does
  # outside one.
  image) limit=30m ;;
  *) echo "unknown mode $MODE" >&2; exit 2 ;;
esac

# login: authenticate to the registry with this run's token; fails when it
# cannot, and never fails the step by itself.
login() {
  if [[ -z "${GH_TOKEN:-}" || -z "${GITHUB_ACTOR:-}" ]]; then
    return 1
  fi
  printf '%s' "$GH_TOKEN" | docker login ghcr.io --username "$GITHUB_ACTOR" --password-stdin >&2
}

# The image job builds and publishes the image every gate pulls. It runs on
# the runner, outside a container, and is the only mode that makes the image
# instead of consuming it. A registry that refuses the login or the push is
# never the run's failure: it prints no digest, and the gates build instead.
if [[ "$MODE" == image ]]; then
  : "${GATE_IMAGE_REPOSITORY:?registry repository for the gate image}"
  : "${GATE_IMAGE_TAG:?tag of the gate image for this run}"
  # Never hand docker anything but a plain ghcr.io repository name.
  if [[ ! "$GATE_IMAGE_REPOSITORY" =~ ^ghcr\.io/[a-z0-9][a-z0-9._-]*/[a-z0-9][a-z0-9._-]*$ ]]; then
    echo "GATE_IMAGE_REPOSITORY is not a plain ghcr.io name: $GATE_IMAGE_REPOSITORY" >&2
    exit 2
  fi
  if [[ "$GATE_IMAGE_REPOSITORY" == *..* ]]; then
    echo "GATE_IMAGE_REPOSITORY must not contain ..: $GATE_IMAGE_REPOSITORY" >&2
    exit 2
  fi
  image="$GATE_IMAGE_REPOSITORY:$GATE_IMAGE_TAG"
  docker build --build-arg TOOLS=1 --tag "$image" \
    --file "$support/Dockerfile" "$support" >&2
  reason=
  if ! login; then
    reason="no token or login refused"
  elif ! docker push "$image" >&2; then
    reason="push refused"
  fi
  if [[ -n "$reason" ]]; then
    printf 'image not shared: %s\n' "$reason" >&2
    exit 0
  fi
  docker image inspect --format '{{index .RepoDigests 0}}' "$image"
  exit 0
fi

# gate_image: the one place that supplies the containers' image. The run
# publishes one image and every gate pulls that digest; when there is none, or
# this runner cannot pull exactly it, the job builds the image as it always
# has, so no gate depends on the registry being reachable.
gate_image() {
  local reason="no image published" pulled
  if [[ -n "${GATE_IMAGE:-}" ]]; then
    if ! login; then
      reason="no token or login refused"
    elif ! docker pull "$GATE_IMAGE" >&2; then
      reason="pull refused"
    else
      pulled=$(docker image inspect --format '{{index .RepoDigests 0}}' "$GATE_IMAGE" 2>/dev/null || true)
      if [[ "$pulled" == "$GATE_IMAGE" ]]; then
        docker tag "$GATE_IMAGE" atelier-gates
        summary "image pulled: $GATE_IMAGE"
        return 0
      fi
      reason="digest mismatch"
    fi
  fi
  summary "image not shared: $reason; this job builds it without the pinned tools"
  docker build --build-arg TOOLS=0 --tag atelier-gates --file "$support/Dockerfile" "$support" >&2
}

names=()
trap 'docker rm --force "${names[@]}" >/dev/null 2>&1 || true' EXIT

# Leave the runner one CPU and 2 GiB; any hosted runner size fits.
cpus=$(( $(nproc) > 1 ? $(nproc) - 1 : 1 ))
total=$(awk '/^MemTotal:/ { print int($2 / 1024) }' /proc/meminfo)
memory="$(( total > 4096 ? total - 2048 : total / 2 ))m"

# create NAME NETWORK MOUNT...: no runner home, credentials, command files or
# container-engine socket. Omitting --pid keeps Docker's private PID
# namespace. The process limit counts threads: three mutant jobs run rustc
# and nextest. Every mode's variables go to every container; a mode reads only
# its own, and one the host does not set arrives empty.
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
    --env MODE --env GATE --env ROOT --env BASE --env FLOOR \
    --env ATELIER_SHARD --env ATELIER_MUTANTS_SHARDS --env SHARDS \
    atelier-gates >/dev/null
  names+=("$name")
  local sources=()
  for mount in "$@"; do
    sources+=("$(sed -E 's/.*type=([^,]*),source=([^,]*),target=([^,]*).*/\1:\2:\3/' <<< "$mount")")
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

# summary TEXT: append known, bounded lines to the job summary; container
# output never reaches it unfiltered.
summary() {
  [[ -n "${GITHUB_STEP_SUMMARY:-}" ]] || return 0
  grep -E '^(\*\*commands/[A-Za-z0-9_.-]+:\*\* |\*\*partition:\*\* |image (pulled|not shared): |base [0-9a-f]+ is unavailable)' <<< "$1" \
    | cut -b1-500 | head -n 5 | sed 's/$/\n/' >> "$GITHUB_STEP_SUMMARY" || true
}

# start NAME: run it within the mode's limit; its exit status.
start() {
  timeout --signal=TERM --kill-after=30s "$limit" docker start --attach "$1"
}

# Every mode that starts a container needs the image; the run's published
# digest is pulled when it is available, and each job builds otherwise. It
# runs after these definitions because it reports through summary.
gate_image

id="$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT-$RANDOM"

# A partition judges reports the host downloaded from every shard, in a fresh
# offline container: the shards cannot change the validator, and nothing is
# copied back. Its verdict, and its exit status, are the job's whole result.
if [[ "$MODE" == partition ]]; then
  # A shard job that died before uploading leaves no artifact, and Docker
  # refuses a missing bind source; create it so the validator names the gap.
  mkdir -p "${COLLECTED:?downloaded shard reports}"
  create "atelier-partition-$id" none \
    "type=bind,source=${COLLECTED:?downloaded shard reports},target=/reports,readonly"
  status=0
  verdict=$(start "atelier-partition-$id") || status=$?
  echo "$verdict"
  summary "$verdict"
  exit "$status"
fi

create "atelier-$MODE-$id" bridge \
  "type=bind,source=${SOURCE:?checkout},target=/source,readonly" \
  "type=bind,source=$RUNNER_TEMP/atelier,target=/binary,readonly"

status=0
start "atelier-$MODE-$id" || status=$?

# Evidence collection cannot turn a failed, timed-out or killed run green.
# docker cp keeps links, so only plain files and folders cross to the host.
docker cp "atelier-$MODE-$id:/reports/." "$output/" >&2 || true
find "$output" -mindepth 1 ! -type f ! -type d -delete

if [[ "$MODE" == gate ]]; then
  # A fresh, offline container judges the copied reports: the gate cannot
  # change the validator or its interpreter.
  MODE=validate
  limit=10m
  create "atelier-validate-$id" none "type=bind,source=$output,target=/reports,readonly"
  verdict=$(start "atelier-validate-$id") || status=1
  echo "$verdict"
  summary "$verdict"
fi

# Say once, in the plan, when the push's base was unavailable.
if [[ "$MODE" == plan* && -f "$output/base.txt" ]]; then
  summary "$(head -c 500 "$output/base.txt")"
fi
exit "$status"
