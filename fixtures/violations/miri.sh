#!/bin/bash
# Breaks rust/miri: adds a Miri-only test that leaks a box. The workspace
# forbids unsafe code, so the violation is a safe leak, which Miri reports;
# the test gate never compiles the test.
set -euo pipefail

test -f reporter/src/lib.rs || { echo "run from the repository root" >&2; exit 2; }

# noclobber: refuse to overwrite a file of that name.
set -o noclobber
printf '%s\n' \
  '//! A deliberate memory leak for the rust/miri gate.' \
  '' \
  '#[test]' \
  'fn the_violation_leaks() {' \
  '    std::mem::forget(Box::new(1_u8));' \
  '}' > reporter/src/miri_violation.rs

printf '\n#[cfg(miri)]\nmod miri_violation;\n' >> reporter/src/lib.rs
