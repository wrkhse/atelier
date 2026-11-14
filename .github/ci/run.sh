#!/bin/bash
# GitHub-hosted Linux only. Never use this runner on a developer machine.
set -euo pipefail
if [[ "${GITHUB_ACTIONS:-}" != true || "${RUNNER_OS:-}" != Linux \
  || "${RUNNER_ENVIRONMENT:-}" != github-hosted ]]; then
  echo "run.sh runs only on a GitHub-hosted Linux runner; run checks in CI" >&2
  exit 2
fi

export MODE=${1:?mode}
export GATE=${2:-}
export ROOT=${3:-.}
export BASE=${4:-}
# atelier's own floor (both core proofs, the mutants canary) applies when the
# workflow runs from the repository it checks: atelier's own CI.
export FLOOR=
if [[ -n "${WORKFLOW_REPOSITORY:-}" && "$WORKFLOW_REPOSITORY" == "${GITHUB_REPOSITORY:-}" ]]; then
  FLOOR=atelier
fi
# Host-written inspections sit beside container/, which holds only what a
# container wrote.
reports="$RUNNER_TEMP/atelier-reports"
output="$reports/container"
mkdir -p "$output" "$RUNNER_TEMP/atelier"

# Each job stays inside its 90 minutes: plan is selftest + plan, a gate is
# gate + validate.
case "$MODE" in
  selftest | plan-release) limit=10m ;;
  plan) limit=60m ;;
  gate) limit=75m ;;
  *) echo "unknown mode $MODE" >&2; exit 2 ;;
esac

support=${CI_SUPPORT:?pinned CI support directory}
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
    --env MODE --env GATE --env ROOT --env BASE --env FLOOR atelier-gates >/dev/null
  names+=("$name")
  local sources=()
  for mount in "$@"; do
    sources+=("$(sed -E 's/.*type=([^,]*),source=([^,]*),target=([^,]*).*/\1:\2:\3/' <<< "$mount")")
  done
  contained "$name" "$network" "${sources[@]}"
}

# contained NAME NETWORK TYPE:SOURCE:TARGET...: refuse to start a container whose
# recorded settings differ from these exact values; the inspection is kept as
# evidence. This guards against regressions and daemon defaults, not against
# an edit to run.sh itself.
contained() {
  local name=$1 network=$2
  shift 2
  docker inspect "$name" > "$reports/$name.json"
  jq -e --arg network "$network" --args '.[0] as $c | $c.HostConfig as $h
    | $c.Config.User == "10001:10001"
    and $h.CapDrop == ["ALL"] and ($h.CapAdd // [] | length == 0)
    and $h.SecurityOpt == ["no-new-privileges"]
    and $h.Privileged == false
    and ($h.PidMode == "" or $h.PidMode == "private")
    and $h.IpcMode == "private" and $h.UsernsMode == "" and $h.UTSMode == ""
    and $h.NetworkMode == $network
    and ($h.Devices // [] | length == 0) and ($h.VolumesFrom // [] | length == 0)
    and $h.PidsLimit == 4096 and $h.Memory > 0 and $h.MemorySwap == $h.Memory
    and $h.NanoCpus > 0
    and ($h.Binds // [] | length == 0)
    and ($h.Mounts | all(.ReadOnly == true))
    and ([$h.Mounts[] | "\(.Type):\(.Source):\(.Target)"] | sort) == ($ARGS.positional | sort)' \
    "$@" < "$reports/$name.json" >/dev/null \
    || { echo "container $name is not contained" >&2; exit 1; }
}

# summary TEXT: append known, bounded lines to the job summary; container
# output never reaches it unfiltered.
summary() {
  [[ -n "${GITHUB_STEP_SUMMARY:-}" ]] || return 0
  grep -E '^(\*\*commands/[a-z0-9_-]+:\*\* |base [0-9a-f]+ is unavailable)' <<< "$1" \
    | cut -b1-500 | head -n 5 | sed 's/$/\n/' >> "$GITHUB_STEP_SUMMARY" || true
}

# start NAME: run it within the mode's limit; its exit status.
start() {
  timeout --signal=TERM --kill-after=30s "$limit" docker start --attach "$1"
}

id="$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT-$RANDOM"
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
