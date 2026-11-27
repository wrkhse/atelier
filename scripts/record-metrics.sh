#!/usr/bin/env bash
# Records slice 2's metric gates from a released candidate: a two-pane
# terminal (commands on the left, captions on the right) driven by tmux,
# recorded by termctrl and exported as a GIF. Every gate goes through the
# contained runner of .github/ci; atelier-metrics comes only from the
# checksum-verified release archive. GitHub-hosted Linux only.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true || "${RUNNER_OS:-}" != Linux \
  || "${RUNNER_ENVIRONMENT:-}" != github-hosted ]]; then
  echo "record-metrics.sh runs only on a GitHub-hosted Linux runner" >&2
  exit 2
fi

RELEASE_TAG=${RELEASE_TAG:?the release tag to record, such as v0.2.0-rc.2}
: "${GH_TOKEN:?GH_TOKEN is needed to download the release assets}"

# derive_base_tag TAG prints TAG's previous rc tag with one leading v. TAG may
# arrive with or without that v; both forms resolve to the same base.
derive_base_tag() {
  local tag=$1 bare rc

  bare=${tag#v}
  if [[ ! "$bare" =~ ^[0-9]+\.[0-9]+\.[0-9]+-rc\.[0-9]+$ ]]; then
    echo "tag $tag is not a release candidate tag" >&2
    return 2
  fi

  rc=${bare##*-rc.}
  if (( 10#$rc == 0 )); then
    echo "tag $tag has no previous rc tag to compare" >&2
    return 2
  fi

  printf 'v%s-rc.%d\n' "${bare%-rc.*}" "$((10#$rc - 1))"
}

# normalize_release_tag TAG prints TAG with exactly one leading v, so a tag
# supplied with or without that prefix resolves to the same form.
normalize_release_tag() {
  printf 'v%s\n' "${1#v}"
}

# Both tags go through the one-leading-v rule: RELEASE_TAG is the supplied tag
# with a single leading v, and BASE is derived from that same form.
RELEASE_TAG=$(normalize_release_tag "$RELEASE_TAG")
BASE=$(derive_base_tag "$RELEASE_TAG") || exit $?
VERSION=${RELEASE_TAG#v}

ARCHIVE="atelier-$RELEASE_TAG-linux-x64.tar.gz"
RELEASE_DIR="$RUNNER_TEMP/release-metrics"
REL="$RUNNER_TEMP/rel-metrics"
METRICS_PATH="$REL/bin/atelier-metrics"
REPORTER_PATH="$REL/bin/atelier-reporter"
SOURCE="$RUNNER_TEMP/slice2-head"
SUPPORT="$GITHUB_WORKSPACE/.github/ci"
CI_SUPPORT="$SUPPORT"
METRICS_TARGET="target/x86_64-unknown-linux-musl/release/atelier-metrics"
# The released binary is staged at METRICS_TARGET on purpose; only a local
# build is forbidden in the recording.
LOCAL_BUILD_RE='cargo build.*atelier-metrics|atelier-metrics.*cargo build'
LOCAL_BUILD_RE+='|Compiling atelier-metrics'
RESULTS_FILE="reporter/src/report.rs"
WORK="$RUNNER_TEMP/record-metrics"
ORIGINAL="$WORK/report.rs.orig"
PATCHER="$WORK/patch_results.py"
RECORDING="$RUNNER_TEMP/metrics.termctrl"
VIDEO="$RUNNER_TEMP/slice-2.mp4"
CAPTIONS="$WORK/captions.log"
SCREENS="$WORK/screens"
EDIT="$WORK/edit.json"
GIF="$WORK/slice-2.gif"
GATE_LOG="$WORK/gate.log"
SUMMARY="$RUNNER_TEMP/atelier-reports/container/summary.txt"
SESSION=metrics-demo
GIF_MAX_BYTES=3000000
GIF_MAX_WIDTH=1200
# Two containers per metrics gate, each bounded to six minutes.
METRICS_CONTAINER_LIMIT=6m
ALLOC_BEFORE=
ALLOC_AFTER=
STOPPED=0

LEFT=
RIGHT=

die() {
  echo "record-metrics: $*" >&2
  exit 1
}

# The base worktree is created from a tag inside the gate container, so the
# checkout must carry every tag and the full object history.
check_history() {
  local tag

  if [[ "$(git -C "$GITHUB_WORKSPACE" rev-parse \
    --is-shallow-repository)" == true ]]; then
    die "the checkout needs full history and tags"
  fi

  for tag in "$RELEASE_TAG" "$BASE"; do
    git -C "$GITHUB_WORKSPACE" rev-parse --verify --quiet \
      "refs/tags/$tag^{commit}" > /dev/null \
      || die "tag $tag is missing from the checkout"
  done
}

fetch_release() {
  rm -rf "$RELEASE_DIR"
  mkdir -p "$RELEASE_DIR"

  gh release download "$RELEASE_TAG" --repo "$GITHUB_REPOSITORY" \
    --pattern "$ARCHIVE" --pattern SHA256SUMS --dir "$RELEASE_DIR"

  if [[ ! -f "$RELEASE_DIR/$ARCHIVE" || ! -f "$RELEASE_DIR/SHA256SUMS" ]]; then
    die "release $RELEASE_TAG lacks $ARCHIVE or SHA256SUMS"
  fi
}

# Exactly one SHA256SUMS line names the archive; it alone is checked.
verify_release() {
  local count line

  count=$(rg -c -F -- "$ARCHIVE" "$RELEASE_DIR/SHA256SUMS") || count=0

  if [[ "$count" != 1 ]]; then
    die "SHA256SUMS has $count lines for $ARCHIVE, expected 1"
  fi

  line=$(rg -F -- "$ARCHIVE" "$RELEASE_DIR/SHA256SUMS")

  if [[ "$line" != *"  $ARCHIVE" ]]; then
    die "SHA256SUMS line for $ARCHIVE is malformed: $line"
  fi

  printf '%s\n' "$line" > "$RELEASE_DIR/$ARCHIVE.sha256"
  (cd "$RELEASE_DIR" && sha256sum -c "$ARCHIVE.sha256")
}

check_binary() {
  local path=$1 name=$2 reported

  if [[ "$path" != "$REL/"* || ! -f "$path" || -L "$path" ]]; then
    die "$name: expected a plain file in $REL: $path"
  fi

  if ! reported=$("$path" --version); then
    die "$name: --version failed at $path"
  fi
  if [[ "$reported" != "$name $VERSION" ]]; then
    die "$name: expected '$name $VERSION', got '$reported'"
  fi
}

stage_release() {
  rm -rf "$REL"
  mkdir -p "$REL"
  tar -xzf "$RELEASE_DIR/$ARCHIVE" -C "$REL"

  check_binary "$REPORTER_PATH" atelier-reporter
  check_binary "$METRICS_PATH" atelier-metrics
}

make_worktree() {
  rm -rf "$SOURCE"
  git -C "$GITHUB_WORKSPACE" worktree prune
  git -C "$GITHUB_WORKSPACE" worktree add --detach "$SOURCE" "$RELEASE_TAG"

  # The gate mounts SOURCE alone. A worktree gitfile points back into the
  # runner workspace, leaving the container with no objects for its own base
  # worktree. A full .git directory in SOURCE keeps both trees reachable.
  [[ -f "$SOURCE/.git" ]] || die "no worktree gitfile at $SOURCE/.git"
  rm -f "$SOURCE/.git"
  cp -R "$GITHUB_WORKSPACE/.git" "$SOURCE/.git"
  rm -rf "$SOURCE/.git/worktrees"
  git -C "$SOURCE" reset --quiet --hard "$RELEASE_TAG"
  git -C "$SOURCE" checkout --quiet --force --detach "$RELEASE_TAG"
}

# REQUIRE_INSTALLED defaults to 1: after a gate, the container has installed
# the binary. Staging passes 0 because no container has run yet.
check_staged_metrics() {
  local require_installed=${1:-1}
  local released staged installed installed_sum

  [[ -f "$SOURCE/$METRICS_TARGET" && ! -L "$SOURCE/$METRICS_TARGET" ]] \
    || die "the staged metrics binary is not a plain file"
  released=$(sha256sum "$METRICS_PATH" | awk '{print $1}')
  staged=$(sha256sum "$SOURCE/$METRICS_TARGET" | awk '{print $1}')

  if [[ "$released" != "$staged" ]]; then
    die "the staged metrics binary differs from the released one"
  fi

  installed="$RUNNER_TEMP/atelier/atelier-metrics"
  if ((require_installed)); then
    [[ -f "$installed" && ! -L "$installed" ]] \
      || die "the container's metrics binary is missing or not a plain file"
    installed_sum=$(sha256sum "$installed" | awk '{print $1}')
    if [[ "$installed_sum" != "$released" ]]; then
      die "the container's metrics binary differs from the released one"
    fi
  fi
}

stage_metrics() {
  if [[ -e "$SOURCE/$METRICS_TARGET" \
    || -L "$SOURCE/$METRICS_TARGET" ]]; then
    die "a local build already occupies $SOURCE/$METRICS_TARGET"
  fi

  mkdir -p "$(dirname "$SOURCE/$METRICS_TARGET")"
  install -m 0555 -- "$METRICS_PATH" "$SOURCE/$METRICS_TARGET"
  check_staged_metrics 0
}

# Every patch finds the exact results() signature once, then inserts lines
# after it. Named failures keep a missing signature from being mistaken for
# a gate verdict.
write_patch_helper() {
  cat > "$PATCHER" <<'PY'
import pathlib
import sys

mode, target, original = sys.argv[1:4]
path = pathlib.Path(target)
saved = pathlib.Path(original)

if mode == "verify-clean":
    if path.read_bytes() != saved.read_bytes():
        print("::error::verify-clean report.rs is not the released source")
        sys.exit(1)
    sys.exit(0)

if mode == "restore":
    if not saved.is_file():
        print("::error::restore has no saved report.rs")
        sys.exit(1)
    path.write_bytes(saved.read_bytes())
    if path.read_bytes() != saved.read_bytes():
        print("::error::restore did not restore report.rs")
        sys.exit(1)
    sys.exit(0)

signature = (
    "pub fn results(report: &CiReport, gates: &[String])"
    " -> Vec<GateCheck> {"
)
lines = path.read_text().split("\n")
found = [i for i, line in enumerate(lines) if line.strip() == signature]
if len(found) != 1:
    print(
        f"::error::{mode} expected one results() signature in {path},"
        f" found {len(found)}"
    )
    sys.exit(1)

patches = {
    "slowdown": [
        "    for i in 0..256 {",
        "        std::hint::black_box(i);",
        "    }",
    ],
    "complexity": [
        "    if gates.is_empty()"
        " || (report.actions.is_empty() && gates.len() > 1) {",
        "        return gates.iter().map(|gate| result(report, gate))"
        ".collect();",
        "    } else if gates.len() == 1 && report.actions.len() > 1 {",
        "        let _seen = gates.len();",
        "    } else if report.actions.len() == gates.len() {",
        "        let _seen = report.actions.len();",
        "    }",
    ],
    "allocated": [
        "    let extra: Vec<usize> =",
        "        (0..report.actions.len().saturating_mul(8).max(1))"
        ".collect();",
        "    std::hint::black_box(&extra);",
        "    drop(extra);",
    ],
}

if mode not in patches:
    print(f"::error::unknown patch mode {mode}")
    sys.exit(1)

lines[found[0] + 1 : found[0] + 1] = patches[mode]
path.write_text("\n".join(lines))
PY
}

patch_results() {
  local mode=$1 needle

  python3 "$PATCHER" "$mode" "$SOURCE/$RESULTS_FILE" "$ORIGINAL"

  case "$mode" in
    slowdown) needle='black_box(i)' ;;
    complexity) needle='let _seen = gates.len();' ;;
    allocated) needle='let extra: Vec<usize> =' ;;
    *) die "unknown patch mode $mode" ;;
  esac

  rg -qF -- "$needle" "$SOURCE/$RESULTS_FILE" \
    || die "$mode: the patch is not in report.rs"
}

restore_results() {
  python3 "$PATCHER" restore "$SOURCE/$RESULTS_FILE" "$ORIGINAL"
  python3 "$PATCHER" verify-clean "$SOURCE/$RESULTS_FILE" "$ORIGINAL"
}

capture() {
  tmux capture-pane -p -J -t "$1" | sed 's/[[:space:]]*$//'
}

pane_has() {
  local pane=$1
  shift

  rg -q "$@" <<< "$(capture "$pane")"
}

pane_lacks() {
  ! pane_has "$@"
}

# wait_until SECONDS COMMAND...: polls once a second, bounded.
wait_until() {
  local limit=$1 waited
  shift

  for ((waited = 0; waited < limit; waited++)); do
    if "$@"; then
      return 0
    fi

    sleep 1
  done

  echo "timed out after ${limit}s: $*" >&2
  return 1
}

type_line() {
  tmux send-keys -t "$LEFT" -l -- "$1"
  tmux send-keys -t "$LEFT" Enter
}

caption() {
  local text=$1

  printf '%s\n' "$text" >> "$CAPTIONS"
  wait_until 10 pane_has "$RIGHT" -F -- "$text"
}

start_panes() {
  : > "$CAPTIONS"

  if [[ -z "${TERM:-}" || "$TERM" == dumb ]]; then
    export TERM=xterm-256color
  fi

  export SOURCE CI_SUPPORT BASE GATE_LOG METRICS_CONTAINER_LIMIT

  # The pane's shell inherits these; the visible export adds the gate limit.
  LEFT=$(tmux new-session -d -s "$SESSION" -x 120 -y 34 -c "$SOURCE" \
    -P -F '#{pane_id}' \
    env -u GITHUB_STEP_SUMMARY PS1='$ ' HISTFILE=/dev/null \
    bash --noprofile --norc -i)
  tmux set-option -t "$SESSION" status off
  RIGHT=$(tmux split-window -h -l 44 -t "$LEFT" -P -F '#{pane_id}' \
    tail -n +1 -F "$CAPTIONS")
  tmux select-pane -t "$LEFT"
}

start_recording() {
  local intro="atelier: metrics against the previous version"

  printf '%s from release %s\n' "$intro" "$RELEASE_TAG" >> "$CAPTIONS"
  wait_until 10 pane_has "$RIGHT" -F -- "$intro"

  termctrl start "$SESSION" --record "$RECORDING" --cols 120 --rows 34 \
    -- tmux attach-session -t "$SESSION"
  termctrl wait "$SESSION" "$intro" --timeout 20000

  type_line "echo release $RELEASE_TAG"
  wait_until 10 pane_has "$LEFT" -F -- "release $RELEASE_TAG"

  type_line "'$METRICS_PATH' --version"
  wait_until 30 pane_has "$LEFT" -x -F -- "atelier-metrics $VERSION"

  type_line "'$REPORTER_PATH' --version"
  wait_until 30 pane_has "$LEFT" -x -F -- "atelier-reporter $VERSION"

  capture "$LEFT" > "$SCREENS/release-command.txt"
  mkdir -p "$SCREENS/release"
  cp -- "$SCREENS/release-command.txt" "$SCREENS/release/screen.txt"
  termctrl mark "$SESSION" release
  sleep 2

  type_line "export METRICS_CONTAINER_LIMIT=6m"
}

save_screen() {
  local dir="$SCREENS/$1"

  mkdir -p "$dir"
  termctrl save "$SESSION" --format txt --out "$dir/screen"
}

# gate_run MARKER CAPTION GATE EXIT VISIBLE HOLD: one contained gate on a
# cleared screen, with its verdict checked in the pane and in the summary.
gate_run() {
  local marker=$1 text=$2 gate=$3 code=$4 visible=$5 hold=$6
  local screen typed

  caption "$text"
  rm -rf "$RUNNER_TEMP/atelier-reports" "$RUNNER_TEMP/atelier-head-reports"
  tmux send-keys -t "$LEFT" C-l
  wait_until 10 pane_lacks "$LEFT" '^exit [0-9]+$'

  typed="GATE=$gate bash \"\$CI_SUPPORT/run.sh\" metrics 2>>\"\$GATE_LOG\""
  type_line "$typed; echo \"exit \$?\""
  # Seven gates at 600s each is 70m, leaving room in the 120m job limit.
  wait_until 600 pane_has "$LEFT" '^exit [0-9]+$'

  screen=$(capture "$LEFT")
  rg -qx "exit $code" <<< "$screen" \
    || die "$marker: expected exit $code in the pane"
  rg -qF -- "$visible" <<< "$screen" \
    || die "$marker: expected '$visible' in the pane"

  [[ -f "$SUMMARY" && ! -L "$SUMMARY" && -s "$SUMMARY" ]] \
    || die "$marker: no non-empty summary at $SUMMARY"
  rg -qF -- "$visible" "$SUMMARY" \
    || die "$marker: expected '$visible' in the summary"
  if [[ "$gate" != metrics/resources ]]; then
    rg -qx -F -- "$visible" "$SUMMARY" \
      || die "$marker: expected the verdict line '$visible'"
  fi

  check_staged_metrics
  termctrl mark "$SESSION" "$marker"
  save_screen "$marker"
  sleep "$hold"
}

read_alloc() {
  local cells

  cells=$(awk -F'|' '
    function trim(s) { gsub(/^[ \t]+|[ \t]+$/, "", s); return s }
    trim($2) == "Allocations (blocks)" {
      print trim($3) "\t" trim($4)
      found = 1
      exit
    }
    END { exit !found }
  ' "$SUMMARY") || die "no Allocations (blocks) row in $SUMMARY"

  IFS=$'\t' read -r ALLOC_BEFORE ALLOC_AFTER <<< "$cells"
  [[ "$ALLOC_BEFORE" =~ ^[0-9]+$ && "$ALLOC_AFTER" =~ ^[0-9]+$ ]] \
    || die "the allocations row has nonnumeric cells: $cells"
}

drive() {
  local control allocated

  start_recording

  # Story 1: a real slowdown loop fails the instruction budget.
  patch_results slowdown
  gate_run budget-fail \
    "1. A real slowdown in results(): budget: failed" \
    metrics/performance 1 'budget: failed' 2
  restore_results
  gate_run budget-pass \
    "1. Restored results(): budget: passed" \
    metrics/performance 0 'budget: passed' 2

  # Story 2: extra branching raises the complexity ratchet.
  patch_results complexity
  gate_run complexity-fail \
    "2. Extra branching in results(): complexity: failed" \
    metrics/complexity 1 'complexity: failed' 2
  restore_results
  gate_run complexity-pass \
    "2. Restored results(): complexity: passed" \
    metrics/complexity 0 'complexity: passed' 2

  # Story 3: resources report, never fail. The control run holds the
  # original count; the patch must put head above the base's cell.
  gate_run resources-control \
    "3. Original results(): allocation counts" \
    metrics/resources 0 'Allocations (blocks)' 2
  read_alloc
  control=$ALLOC_AFTER

  patch_results allocated
  gate_run resources-allocated \
    "3. An extra allocation: head blocks above base" \
    metrics/resources 0 'Allocations (blocks)' 2
  read_alloc
  awk -v base="$ALLOC_BEFORE" -v head="$ALLOC_AFTER" \
    'BEGIN { exit !(head + 0 > base + 0) }' \
    || die "allocated head blocks $ALLOC_AFTER are not above base" \
      "$ALLOC_BEFORE"
  allocated=$ALLOC_AFTER

  restore_results
  gate_run resources-restored \
    "3. Restored results(): allocations back to $control" \
    metrics/resources 0 'Allocations (blocks)' 3
  read_alloc
  [[ "$ALLOC_AFTER" == "$control" ]] \
    || die "restored head blocks $ALLOC_AFTER are not the original $control"

  printf 'blocks: original %s, allocated %s, restored %s\n' \
    "$control" "$allocated" "$ALLOC_AFTER" >> "$CAPTIONS"
}

expected_text() {
  case "$1" in
    release) printf 'release %s' "$RELEASE_TAG" ;;
    budget-fail) printf 'budget: failed' ;;
    budget-pass) printf 'budget: passed' ;;
    complexity-fail) printf 'complexity: failed' ;;
    complexity-pass) printf 'complexity: passed' ;;
    resources-control|resources-allocated|resources-restored)
      printf 'Allocations (blocks)' ;;
    *) die "unknown marker $1" ;;
  esac
}

check_markers() {
  local markers timestamp marker frame expected name status=0

  markers=$(termctrl markers "$RECORDING") \
    || die "termctrl markers failed for $RECORDING"
  [[ -n "$markers" ]] || die "no markers in $RECORDING"

  for name in release budget-fail budget-pass complexity-fail \
    complexity-pass resources-control resources-allocated \
    resources-restored; do
    rg -qF -- $'\t'"$name" <<< "$markers" \
      || die "marker $name is missing"
  done

  # termctrl markers lists each timestamp and marker separated by a tab.
  while IFS=$'\t' read -r timestamp marker; do
    [[ "$timestamp" =~ ^[0-9]+$ && -n "$marker" ]] \
      || die "malformed termctrl markers output"
    frame=$(termctrl show --recording "$RECORDING" --at-marker "$marker") \
      || die "termctrl show failed at marker $marker"
    [[ -n "${frame//[[:space:]]/}" ]] \
      || die "blank frame at marker $marker"

    expected=$(expected_text "$marker")
    rg -qF -- "$expected" <<< "$frame" \
      || die "marker $marker lacks '$expected'"
    status=0
    rg -q -- "$LOCAL_BUILD_RE" <<< "$frame" || status=$?
    case "$status" in
      0) die "marker $marker shows a locally built metrics binary" ;;
      1) ;;
      *) die "could not scan marker $marker for a local metrics build" ;;
    esac
  done <<< "$markers"
}

screen_file() {
  local files

  mapfile -t files < <(find "$SCREENS/$1" -type f -name '*.txt')

  if ((${#files[@]} != 1)); then
    die "expected one txt screen for $1, found ${#files[@]}"
  fi

  printf '%s\n' "${files[0]}"
}

check_screens() {
  local marker file expected status=0

  for marker in release budget-fail budget-pass complexity-fail \
    complexity-pass resources-control resources-allocated \
    resources-restored; do
    file=$(screen_file "$marker")
    expected=$(expected_text "$marker")
    rg -qF -- "$expected" "$file" \
      || die "$marker screen lacks '$expected'"
  done

  rg -q --no-ignore -- "$LOCAL_BUILD_RE" "$SCREENS" "$GATE_LOG" \
    || status=$?
  case "$status" in
    0) die "a saved screen or gate log shows a local metrics build" ;;
    1) ;;
    *) die "could not scan saved screens and gate log" ;;
  esac
}

check_provenance() {
  check_binary "$METRICS_PATH" atelier-metrics
  check_binary "$REPORTER_PATH" atelier-reporter
  check_staged_metrics

  rg -qF -- "'$METRICS_PATH' --version" "$SCREENS/release-command.txt" \
    || die "the release screen lacks the released metrics path"
  rg -qF -- "'$REPORTER_PATH' --version" "$SCREENS/release-command.txt" \
    || die "the release screen lacks the released reporter path"
}

write_edit_plan() {
  cat > "$EDIT" <<'JSON'
{
  "clips": [
    {
      "from": "release",
      "to": "budget-fail",
      "caption": "1. Slowdown -> budget: failed",
      "speed": 2,
      "hold_ms": 2500
    },
    {
      "from": "budget-fail",
      "to": "budget-pass",
      "caption": "1. Restored -> budget: passed",
      "speed": 2,
      "hold_ms": 2500
    },
    {
      "from": "budget-pass",
      "to": "complexity-fail",
      "caption": "2. More branches -> complexity: failed",
      "speed": 2,
      "hold_ms": 2500
    },
    {
      "from": "complexity-fail",
      "to": "complexity-pass",
      "caption": "2. Restored -> complexity: passed",
      "speed": 2,
      "hold_ms": 2500
    },
    {
      "from": "complexity-pass",
      "to": "resources-control",
      "caption": "3. Original allocation counts",
      "speed": 2,
      "hold_ms": 2500
    },
    {
      "from": "resources-control",
      "to": "resources-allocated",
      "caption": "3. Extra allocation -> blocks above base",
      "speed": 2,
      "hold_ms": 2500
    },
    {
      "from": "resources-allocated",
      "to": "resources-restored",
      "caption": "3. Restored -> original count",
      "speed": 1,
      "hold_ms": 3500
    }
  ]
}
JSON
}

export_video() {
  write_edit_plan
  termctrl markers "$RECORDING"

  termctrl video "$RECORDING" --edit "$EDIT" --footer \
    --pixel-ratio 1 --padding 0 --font-family 'DejaVu Sans Mono' \
    --out "$VIDEO"
}

make_gif() {
  local filter bytes width

  filter="fps=8,scale='min($GIF_MAX_WIDTH,iw)':-1:flags=lanczos,"
  filter+="split[s0][s1];[s0]palettegen=max_colors=32:stats_mode=diff[p];"
  filter+="[s1][p]paletteuse=dither=none"
  ffmpeg -y -loglevel error -i "$VIDEO" -vf "$filter" -loop 0 "$GIF"

  bytes=$(stat -c %s "$GIF")
  width=$(ffprobe -v error -select_streams v -show_entries stream=width \
    -of csv=p=0 "$GIF")

  if ((bytes >= GIF_MAX_BYTES)); then
    die "slice-2.gif is $bytes bytes, limit $GIF_MAX_BYTES"
  fi
  if ((width > GIF_MAX_WIDTH)); then
    die "slice-2.gif is $width pixels wide, limit $GIF_MAX_WIDTH"
  fi
}

stop_recording() {
  if ((STOPPED == 0)); then
    termctrl stop "$SESSION" >/dev/null 2>&1 || true
    tmux kill-session -t "$SESSION" >/dev/null 2>&1 || true
    STOPPED=1

    echo "release: $RELEASE_TAG"
    echo "metrics: $METRICS_PATH"
    echo "reporter: $REPORTER_PATH"
  fi
}

diagnose() {
  echo "--- left pane ---" >&2
  tmux capture-pane -p -J -S - -t "$LEFT" >&2 || true
  echo "--- captions ---" >&2
  cat "$CAPTIONS" >&2 || true
  echo "--- gate log ---" >&2
  tail -n 80 "$GATE_LOG" >&2 || true
  echo "--- markers ---" >&2
  termctrl markers "$RECORDING" >&2 || true
}

finish() {
  local status=$?

  if ((status != 0)); then
    diagnose
  fi

  if [[ -f "$PATCHER" ]]; then
    python3 "$PATCHER" restore "$SOURCE/$RESULTS_FILE" "$ORIGINAL" \
      >/dev/null 2>&1 || true
  fi
  stop_recording
  exit "$status"
}

main() {
  trap finish EXIT

  rm -rf "$WORK"
  mkdir -p "$SCREENS" "$RELEASE_DIR"
  : > "$GATE_LOG"

  check_history
  fetch_release
  unset GH_TOKEN GITHUB_TOKEN

  verify_release
  stage_release
  make_worktree
  cp -- "$SOURCE/$RESULTS_FILE" "$ORIGINAL"
  write_patch_helper
  stage_metrics

  docker build --tag atelier-gates --file "$SUPPORT/Dockerfile" "$SUPPORT"

  start_panes
  drive
  stop_recording

  check_markers
  check_screens
  check_provenance
  export_video
  make_gif
  cp -- "$GIF" "$GITHUB_WORKSPACE/slice-2.gif"
}

main "$@"
