#!/bin/bash
# Breaks rust/mutants: adds a pure function no test covers, so every one of
# its mutants is missed.
set -euo pipefail

test -f reporter/src/lib.rs || { echo "run from the repository root" >&2; exit 2; }

# noclobber: refuse to overwrite a file of that name.
set -o noclobber
printf '%s\n' \
  '//! A deliberate untested function for the rust/mutants gate.' \
  '' \
  '/// Whether `n` is even. No test covers it.' \
  '#[must_use]' \
  'pub const fn is_even(n: u32) -> bool {' \
  '    n % 2 == 0' \
  '}' > reporter/src/violation.rs

printf '\npub mod violation;\n' >> reporter/src/lib.rs
