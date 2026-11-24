#!/bin/bash
# Breaks rust/clippy: appends formatted code with a needless return, which
# clippy denies under -D warnings.
set -euo pipefail

test -f reporter/src/lib.rs || { echo "run from the repository root" >&2; exit 2; }

printf '%s\n' \
  '' \
  '/// A deliberate `clippy::needless_return`.' \
  '#[must_use]' \
  'pub const fn clippy_violation() -> u32 {' \
  '    return 1;' \
  '}' >> reporter/src/lib.rs
