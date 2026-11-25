#!/usr/bin/env bash
# Records the released atelier build running its rust/fmt gate end to end: a
# two-pane terminal (commands on the left, captions on the right) driven by
# tmux, recorded by termctrl and exported as a GIF. It ends on the released
# reporter judging recorded moon results for all seven gates. GitHub-hosted
# Linux only; the gate itself runs in the contained runner of .github/ci.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true || "${RUNNER_OS:-}" != Linux \
  || "${RUNNER_ENVIRONMENT:-}" != github-hosted ]]; then
  echo "record.sh runs only on a GitHub-hosted Linux runner; record in CI" >&2
  exit 2
fi

RELEASE_TAG=${RELEASE_TAG:?the release tag to record, such as v0.2.0-rc.1}

if [[ ! "$RELEASE_TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+-rc\.[0-9]+$ ]]; then
  echo "RELEASE_TAG $RELEASE_TAG is not a release candidate tag" >&2
  exit 2
fi

VERSION=${RELEASE_TAG#v}
ARCHIVE="atelier-$RELEASE_TAG-linux-x64.tar.gz"
RELEASE_DIR="$RUNNER_TEMP/release"
REL="$RUNNER_TEMP/rel"
SCRATCH="$RUNNER_TEMP/scratch"
SUPPORT="$RUNNER_TEMP/ci-support"
RECORDING="$RUNNER_TEMP/demo.termctrl"
VIDEO="$RUNNER_TEMP/slice-1.mp4"
WORK="$RUNNER_TEMP/record"
CAPTIONS="$WORK/captions.log"
SCREENS="$WORK/screens"
EDIT="$WORK/edit.json"
GIF="$WORK/slice-1.gif"
GATE_LOG="$WORK/gate.log"
RESULTS="$RUNNER_TEMP/atelier-reports/container/ci-report.json"
GATE_TASKS=(fmt clippy test kani miri mutants msrv)
SESSION=demo
REPORTER_PATH=target/x86_64-unknown-linux-musl/release/atelier-reporter
GIF_MAX_BYTES=3000000
GIF_MAX_WIDTH=1200

LEFT=
RIGHT=

# This runs only after the release workflow succeeded, so a missing asset,
# checksum or version fails the recording; nothing else is ever recorded.
fetch_release() {
  rm -rf "$RELEASE_DIR"
  mkdir -p "$RELEASE_DIR"

  gh release download "$RELEASE_TAG" --repo "$GITHUB_REPOSITORY" \
    --pattern "$ARCHIVE" --pattern SHA256SUMS --dir "$RELEASE_DIR"

  if [[ ! -f "$RELEASE_DIR/$ARCHIVE" || ! -f "$RELEASE_DIR/SHA256SUMS" ]]; then
    echo "release $RELEASE_TAG lacks $ARCHIVE or SHA256SUMS" >&2
    return 1
  fi
}

# Exactly one SHA256SUMS line names the archive; it alone is checked.
verify_release() {
  local count line

  count=$(rg -c -F -- "$ARCHIVE" "$RELEASE_DIR/SHA256SUMS") || count=0

  if [[ "$count" != 1 ]]; then
    echo "SHA256SUMS has $count lines for $ARCHIVE, expected 1" >&2
    return 1
  fi

  line=$(rg -F -- "$ARCHIVE" "$RELEASE_DIR/SHA256SUMS")

  if [[ "$line" != *"  $ARCHIVE" ]]; then
    echo "SHA256SUMS line for $ARCHIVE is malformed: $line" >&2
    return 1
  fi

  printf '%s\n' "$line" > "$RELEASE_DIR/$ARCHIVE.sha256"
  (cd "$RELEASE_DIR" && sha256sum -c "$ARCHIVE.sha256")
}

stage_release() {
  local reported

  rm -rf "$REL"
  mkdir -p "$REL"
  tar -xzf "$RELEASE_DIR/$ARCHIVE" -C "$REL"

  reported=$("$REL/bin/atelier-reporter" --version)

  if [[ "$reported" != "atelier-reporter $VERSION" ]]; then
    echo "the released reporter reports '$reported', expected $VERSION" >&2
    return 1
  fi
}

write_crate() {
  printf '/target\n' > "$SCRATCH/.gitignore"

  cat > "$SCRATCH/Cargo.toml" <<'TOML'
[workspace]
members = ["app"]
resolver = "3"

[workspace.package]
edition = "2024"
rust-version = "1.100"
TOML

  cat > "$SCRATCH/app/Cargo.toml" <<'TOML'
[package]
name = "app"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
publish = false
TOML

  printf 'layer: library\nlanguage: rust\n' > "$SCRATCH/app/moon.yml"

  cat > "$SCRATCH/app/src/lib.rs" <<'RUST'
pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

#[cfg(test)]
mod tests {
    #[test]
    fn adds_two_numbers() {
        assert_eq!(super::add(2, 3), 5);
    }
}
RUST

  cargo generate-lockfile --offline --manifest-path "$SCRATCH/Cargo.toml"
}

# origin/main holds the moon workspace alone; the crate is one commit ahead
# on demo, so BASE=origin/main resolves inside the gate container.
commit_scratch() {
  local git=(git -C "$SCRATCH" -c user.name=atelier
    -c user.email=record@atelier.invalid)

  "${git[@]}" init --quiet --initial-branch=main
  "${git[@]}" add .gitignore .moon .prototools
  "${git[@]}" commit --quiet -m "chore: moon workspace"
  "${git[@]}" update-ref refs/remotes/origin/main HEAD
  "${git[@]}" switch --quiet --create demo
  "${git[@]}" add Cargo.toml Cargo.lock app
  "${git[@]}" commit --quiet -m "feat: add the app crate"
}

make_scratch() {
  rm -rf "$SCRATCH" "$SUPPORT"
  mkdir -p "$SCRATCH/app/src"
  cp -R "$REL/.moon" "$SCRATCH/.moon"
  cp "$REL/.prototools" "$SCRATCH/.prototools"
  cp -R "$GITHUB_WORKSPACE/.github/ci" "$SUPPORT"

  cat > "$SCRATCH/.moon/workspace.yml" <<'YAML'
projects:
  app: "app"
vcs:
  defaultBranch: "main"
YAML

  write_crate
  install -D -m 0755 "$REL/bin/atelier-reporter" "$SCRATCH/$REPORTER_PATH"
  commit_scratch
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
  local marker=$1 text=$2

  printf '%s\n' "$text" >> "$CAPTIONS"
  wait_until 10 pane_has "$RIGHT" -F -- "$text"
  termctrl mark "$SESSION" "$marker"
}

start_panes() {
  : > "$CAPTIONS"

  if [[ -z "${TERM:-}" || "$TERM" == dumb ]]; then
    export TERM=xterm-256color
  fi

  # The pane's shell inherits these; the visible export adds the gate.
  export SOURCE="$SCRATCH" CI_SUPPORT="$SUPPORT" GATE_LOG RESULTS

  # Without GITHUB_STEP_SUMMARY, the recorded results never reach the job
  # summary as if they were this run's gates.
  LEFT=$(tmux new-session -d -s "$SESSION" -x 120 -y 34 -c "$SCRATCH" \
    -P -F '#{pane_id}' \
    env -u GITHUB_STEP_SUMMARY PS1='$ ' HISTFILE=/dev/null \
    bash --noprofile --norc -i)
  tmux set-option -t "$SESSION" status off
  RIGHT=$(tmux split-window -h -l 44 -t "$LEFT" -P -F '#{pane_id}' \
    tail -n +1 -F "$CAPTIONS")
  tmux select-pane -t "$LEFT"
}

start_recording() {
  local intro="atelier: the rust/fmt gate"

  printf '%s\n' "$intro from release $RELEASE_TAG" >> "$CAPTIONS"
  wait_until 10 pane_has "$RIGHT" -F -- "$intro"

  termctrl start "$SESSION" --record "$RECORDING" --cols 120 --rows 34 \
    -- tmux attach-session -t "$SESSION"
  termctrl wait "$SESSION" "$intro" --timeout 20000
  termctrl mark "$SESSION" ready

  type_line "./$REPORTER_PATH --version"
  wait_until 30 pane_has "$LEFT" -x -F -- "atelier-reporter $VERSION"
  type_line "export GATE=rust/fmt GATES=rust/fmt BASE=origin/main"
}

save_screen() {
  local dir="$SCREENS/$1"

  mkdir -p "$dir"
  termctrl save "$SESSION" --format txt --out "$dir/screen"
}

# gate_step MARKER RESULT EXIT: one gate run on a cleared screen, so each
# saved screen holds only its own result.
gate_step() {
  local marker=$1 result=$2 code=$3 screen

  rm -rf "$RUNNER_TEMP/atelier-reports"
  tmux send-keys -t "$LEFT" C-l
  wait_until 10 pane_lacks "$LEFT" '^exit [0-9]+$'

  # The pane's shell expands these, so the recording shows the real command.
  # shellcheck disable=SC2016
  type_line 'bash "$CI_SUPPORT/run.sh" gate 2>>"$GATE_LOG"; echo "exit $?"'
  wait_until 900 pane_has "$LEFT" '^exit [0-9]+$'

  screen=$(capture "$LEFT")

  if ! rg -qx "rust/fmt: $result" <<< "$screen" \
    || ! rg -qx "exit $code" <<< "$screen"; then
    echo "$marker: expected rust/fmt: $result and exit $code" >&2
    return 1
  fi

  termctrl mark "$SESSION" "$marker"
  save_screen "$marker"
}

# The failure must be rustfmt's, not a broken container.
expect_diff() {
  local seen

  seen="$(tmux capture-pane -p -J -S - -t "$LEFT")$(cat "$GATE_LOG")"

  if ! rg -q 'Diff in .*app/src/lib\.rs' <<< "$seen"; then
    echo "the failed run shows no rustfmt diff for app/src/lib.rs" >&2
    return 1
  fi
}

# The runner writes RESULTS and also validates it in a separate container.
all_gates_step() {
  local typed

  rm -rf "$RUNNER_TEMP/atelier-reports"
  tmux send-keys -t "$LEFT" C-l
  wait_until 10 pane_lacks "$LEFT" '^exit [0-9]+$'

  typed="GATE=all GATES=\"$*\" bash \"\$CI_SUPPORT/run.sh\" gate 2>>\"\$GATE_LOG\""
  type_line "$typed; gate_status=\$?; echo \"gate exit \$gate_status\""
  # The gate and validation have 85 minutes combined, plus runner overhead.
  wait_until 5400 pane_has "$LEFT" '^gate exit [0-9]+$'
}

# summary_step: the released reporter judges the container's results for all
# seven gates, so the final frame lists one check per gate.
summary_step() {
  local gates=() gate screen typed

  for gate in "$@"; do
    gates+=(--gate "$gate")
  done

  tmux send-keys -t "$LEFT" C-l
  wait_until 10 pane_lacks "$LEFT" '^gate exit [0-9]+$'

  typed="./$REPORTER_PATH --report \"\$RESULTS\" ${gates[*]}"
  type_line "$typed; echo \"exit \$?\"; echo \"gate exit \$gate_status\""
  wait_until 30 pane_has "$LEFT" '^gate exit [0-9]+$'

  screen=$(capture "$LEFT")

  if ! rg -qx "exit 0" <<< "$screen"; then
    echo "final-summary: the reporter did not exit 0" >&2
    return 1
  fi
  if ! rg -qx "gate exit 0" <<< "$screen"; then
    echo "final-summary: the all-gates run did not exit 0" >&2
    return 1
  fi

  termctrl mark "$SESSION" final-summary
  save_screen final-summary
}

restored() {
  ! rg -qF 'ugly( )' "$SCRATCH/app/src/lib.rs"
}

drive() {
  local gates=("${GATE_TASKS[@]/#/rust/}")

  start_recording

  caption caption-clean "1. Clean crate: run the gate"
  gate_step clean-pass passed 0
  caption caption-clean-result "1. Result: rust/fmt: passed"

  caption caption-violation "2. Append an unformatted line, run again"
  type_line "echo 'pub fn  ugly( )->u64{ 1 }' >> app/src/lib.rs"
  wait_until 10 rg -qF 'ugly( )' "$SCRATCH/app/src/lib.rs"
  gate_step violation-fail failed 1
  expect_diff
  caption caption-violation-result "2. Result: rust/fmt: failed"

  caption caption-final "3. Restore the file, run again"
  type_line "git checkout -- app/src/lib.rs"
  wait_until 10 restored
  gate_step final-pass passed 0
  caption caption-final-result "3. Result: rust/fmt: passed"

  caption caption-summary "4. Real moon results, one check per gate"
  all_gates_step "${gates[@]}"
  summary_step "${gates[@]}"

  termctrl stop "$SESSION"
  tmux kill-session -t "$SESSION"
}

matches() {
  local count

  count=$(rg -c -- "$1" "$2") || count=0
  printf '%s\n' "$count"
}

# expect_count FILE PASSED FAILED LABEL: reporter lines start the left pane,
# so only they match at the start of a line; captions sit to their right.
expect_count() {
  local passed failed

  passed=$(matches '^rust/fmt: passed\b' "$1")
  failed=$(matches '^rust/fmt: failed\b' "$1")

  if [[ "$passed" != "$2" || "$failed" != "$3" ]]; then
    echo "$4: $passed passed and $failed failed lines," \
      "expected $2 and $3" >&2
    return 1
  fi
}

screen_file() {
  local files

  mapfile -t files < <(find "$SCREENS/$1" -type f -name '*.txt')

  if ((${#files[@]} != 1)); then
    echo "expected one txt screen for $1, found ${#files[@]}" >&2
    return 1
  fi

  printf '%s\n' "${files[0]}"
}

check_screens() {
  local all="$SCREENS/all.txt" marker file

  : > "$all"

  for marker in clean-pass violation-fail final-pass; do
    file=$(screen_file "$marker")
    cat "$file" >> "$all"
  done

  expect_count "$(screen_file clean-pass)" 1 0 clean-pass
  expect_count "$(screen_file violation-fail)" 0 1 violation-fail
  expect_count "$(screen_file final-pass)" 1 0 final-pass
  expect_count "$all" 2 1 "the three screens"
  expect_summary "$(screen_file final-summary)"
}

# expect_summary FILE: exactly one passed line per gate and no failed line.
expect_summary() {
  local task count passed failed

  for task in "${GATE_TASKS[@]}"; do
    count=$(matches "^rust/$task: passed\b" "$1")

    if [[ "$count" != 1 ]]; then
      echo "final-summary: $count rust/$task: passed lines, expected 1" >&2
      return 1
    fi
  done

  passed=$(matches '^rust/[a-z]+: passed\b' "$1")
  failed=$(matches '^rust/[a-z]+: failed\b' "$1")

  if [[ "$passed" != "${#GATE_TASKS[@]}" || "$failed" != 0 ]]; then
    echo "final-summary: $passed passed and $failed failed lines," \
      "expected ${#GATE_TASKS[@]} and 0" >&2
    return 1
  fi
}

write_edit_plan() {
  cat > "$EDIT" <<'JSON'
{
  "clips": [
    {
      "from": "ready",
      "to": "caption-clean-result",
      "caption": "1. Clean crate -> rust/fmt: passed",
      "speed": 2,
      "hold_ms": 2500
    },
    {
      "from": "caption-clean-result",
      "to": "caption-violation-result",
      "caption": "2. Unformatted line -> rust/fmt: failed",
      "speed": 2,
      "hold_ms": 2500
    },
    {
      "from": "caption-violation-result",
      "to": "caption-final-result",
      "caption": "3. File restored -> rust/fmt: passed",
      "speed": 2,
      "hold_ms": 2500
    },
    {
      "from": "caption-final-result",
      "to": "final-summary",
      "caption": "4. Real moon results: one check per gate",
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
    echo "slice-1.gif is $bytes bytes, limit $GIF_MAX_BYTES" >&2
    return 1
  fi

  if ((width > GIF_MAX_WIDTH)); then
    echo "slice-1.gif is $width pixels wide, limit $GIF_MAX_WIDTH" >&2
    return 1
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

  termctrl stop "$SESSION" >/dev/null 2>&1 || true
  tmux kill-session -t "$SESSION" >/dev/null 2>&1 || true
  exit "$status"
}

main() {
  trap finish EXIT

  rm -rf "$WORK"
  mkdir -p "$SCREENS"
  : > "$GATE_LOG"

  fetch_release
  unset GH_TOKEN GITHUB_TOKEN

  verify_release
  stage_release
  make_scratch

  # Built before recording, so run.sh's own build is cached and quick.
  docker build --tag atelier-gates --file "$SUPPORT/Dockerfile" "$SUPPORT"

  start_panes
  drive
  check_screens
  export_video
  make_gif
  cp "$GIF" "$GITHUB_WORKSPACE/slice-1.gif"
}

main "$@"
