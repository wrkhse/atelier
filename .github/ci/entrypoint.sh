#!/bin/bash
set -euo pipefail

# Modes: selftest, plan, plan-release and gate run in the gate container;
# validate and partition run in a fresh one, offline, with the reports mounted
# read-only.
if [[ "$MODE" == validate ]]; then
  floor=()
  if [[ "${FLOOR:-}" == atelier ]]; then
    floor=(--atelier)
  fi
  exec /usr/bin/python3 -I /opt/ci/reports.py "$GATE" /reports "${floor[@]}"
fi

# A partition needs its caller's shard count. Refuse to start the validator
# with anything else rather than let it judge a smaller set than was planned.
if [[ "$MODE" == partition ]]; then
  if ! [[ "${SHARDS:-}" =~ ^[1-9][0-9]*$ ]]; then
    echo "partition needs a positive shard count in SHARDS, not '${SHARDS:-}'" >&2
    exit 2
  fi
  exec /usr/bin/python3 -I /opt/ci/reports.py --partition "$SHARDS" /reports
fi

if [[ "$MODE" == selftest ]]; then
  /usr/bin/python3 /opt/ci/test_reports.py -v
  /usr/local/bin/pipx --version
  /opt/pipx/bin/ruff --version
  exit
fi

# The host checkout is read-only and has no persisted checkout credential.
# All source execution, including the planner's process runner, stays here.
cp -R /source/. /work/
git config --global --add safe.directory /work

diff=()
# A push's `before` is all zeros on a branch's first push and unreachable
# after a force push. Then every gate is planned, mutants use the canary,
# and the job summary says so; it never passes as an empty diff.
if [[ "${BASE:-}" =~ ^0+$ ]] || { [[ -n "${BASE:-}" ]] \
  && ! git -C /work cat-file -e "$BASE^{commit}" 2>/dev/null; }; then
  echo "base $BASE is unavailable (first or forced push): every gate runs and mutants use the canary" \
    > /reports/base.txt
  /usr/bin/cat /reports/base.txt >&2
  BASE=
fi

if [[ -n "${BASE:-}" ]]; then
  diff=(--diff "$BASE")
fi

# A plan prints the matrix the host turns into jobs. A sharded gate is
# expanded here, so each shard is an entry of its own and the host needs no
# knowledge of how a gate shards.
if [[ "$MODE" == plan || "$MODE" == plan-release ]]; then
  if [[ "$MODE" == plan ]]; then
    cargo build --locked -p atelier-cli >&2
    /usr/bin/cp target/debug/atelier /reports/atelier
    matrix=$(target/debug/atelier gates --json "${diff[@]}")
  else
    matrix=$(/binary/atelier gates --json "${diff[@]}")
  fi
  if [[ -n "${ATELIER_MUTANTS_SHARDS:-}" ]]; then
    # pipefail makes a refusal from shards.py the pipeline's status, and set -e
    # stops the step: a malformed expansion never reaches the host as a plan.
    expanded=$(printf '%s' "$matrix" \
      | /usr/bin/python3 /opt/ci/shards.py expand "$ATELIER_MUTANTS_SHARDS" /work/.atelier/config.toml)
    printf '%s\n' "$expanded"
  else
    printf '%s\n' "$matrix"
  fi
  exit
fi

# The gate's own exit status; its reports are judged in a separate container.
set +e
/binary/atelier check --setup --gate "$GATE" --root "$ROOT" "${diff[@]}" 2>&1 \
  | /usr/bin/tee /reports/gate.log
codes=("${PIPESTATUS[@]}")
set -e
status=${codes[0]}
if [[ ${codes[1]} -ne 0 && $status -eq 0 ]]; then
  status=1
fi
exit "$status"
