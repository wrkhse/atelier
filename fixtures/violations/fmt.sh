#!/bin/bash
# Breaks rust/fmt: appends a function cargo fmt --check would rewrite.
set -euo pipefail

test -f reporter/src/lib.rs || { echo "run from the repository root" >&2; exit 2; }

printf '\npub fn   fmt_violation( )->u32{ 1 }\n' >> reporter/src/lib.rs
