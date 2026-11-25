#!/bin/bash
# Breaks rust/msrv: lowers the declared rust-version to 1.99 and calls
# bool::toggle, stable since 1.100. The msrv gate checks with the declared
# version; the other gates keep 1.100 and compile it.
set -euo pipefail

test -f Cargo.toml && test -f reporter/src/lib.rs \
  || { echo "run from the repository root" >&2; exit 2; }

# Refuse to run twice, or on a manifest that declares another version.
grep -qx 'rust-version = "1.100"' Cargo.toml \
  || { echo 'Cargo.toml does not declare rust-version = "1.100"' >&2; exit 1; }

sed -i 's/^rust-version = "1\.100"$/rust-version = "1.99"/' Cargo.toml

printf '%s\n' \
  '' \
  '/// A deliberate use of `bool::toggle`, stable since 1.100.' \
  '#[must_use]' \
  'pub const fn msrv_violation(mut flag: bool) -> bool {' \
  '    flag.toggle();' \
  '    flag' \
  '}' >> reporter/src/lib.rs
