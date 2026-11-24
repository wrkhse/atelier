#!/bin/bash
# Breaks rust/test: creates an integration test that panics.
set -euo pipefail

test -d reporter/tests || { echo "run from the repository root" >&2; exit 2; }

# noclobber: refuse to overwrite a file of that name.
set -o noclobber
printf '%s\n' \
  '//! A deliberate failing test for the rust/test gate.' \
  '' \
  '#[test]' \
  'fn the_violation_fails() {' \
  '    panic!("deliberate rust/test violation");' \
  '}' > reporter/tests/violation.rs
