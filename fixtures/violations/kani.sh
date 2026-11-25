#!/bin/bash
# Breaks rust/kani: adds a proof harness whose assertion is false. Kani
# injects its crate under cfg(kani), so no dependency is added.
set -euo pipefail

test -f reporter/src/lib.rs || { echo "run from the repository root" >&2; exit 2; }

# noclobber: refuse to overwrite a file of that name.
set -o noclobber
printf '%s\n' \
  '//! A deliberate failing proof for the rust/kani gate.' \
  '' \
  '#[kani::proof]' \
  'fn the_violation_fails() {' \
  '    kani::assert(1 + 1 == 3, "deliberate rust/kani violation");' \
  '}' > reporter/src/kani_violation.rs

printf '\n#[cfg(kani)]\nmod kani_violation;\n' >> reporter/src/lib.rs
