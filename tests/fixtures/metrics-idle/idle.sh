#!/usr/bin/env bash
# A long-running fixture for the declared-process idle sample. It writes more
# than a kilobyte to stdout over a bounded six-second window, sleeping a
# tenth of a second between writes.
set -euo pipefail

start=$SECONDS
while (( SECONDS - start < 6 )); do
  printf 'idle line %d: %s\n' "$SECONDS" \
    '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
  sleep 0.1
done
