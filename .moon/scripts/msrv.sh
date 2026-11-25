#!/usr/bin/env bash
# The rust/msrv gate: checks the workspace with the rust-version every member
# declares. cargo metadata reports each member's resolved rust-version, so a
# version inherited from [workspace.package] counts as declared. MSRV_ROOT is
# the workspace root, .. by default since moon runs the task from reporter/.
# MSRV_CHECK_ONLY=1 validates and prints the version without installing it.
set -euo pipefail

fail() {
  echo "msrv: $*" >&2
  exit 1
}

root=${MSRV_ROOT:-..}
cd "$root"

metadata=$(cargo metadata --no-deps --format-version 1)
member_list=$(jq -r '.workspace_members[]' <<<"$metadata")

if [[ -z "$member_list" ]]; then
  fail "no workspace members"
fi

mapfile -t members <<<"$member_list"

# 1.N or 1.N.P, as rustup names a release; stable, nightly and the like fail.
pattern='^1\.[0-9]{1,3}(\.[0-9]{1,3})?$'
msrv=""

for id in "${members[@]}"; do
  version=$(jq -r --arg id "$id" \
    '.packages[] | select(.id == $id) | .rust_version // empty' <<<"$metadata")

  if [[ -z "$version" ]]; then
    fail "workspace member $id has no rust-version"
  fi

  if [[ ! "$version" =~ $pattern ]]; then
    fail "invalid rust-version $version"
  fi

  if [[ -z "$msrv" ]]; then
    msrv=$version
  elif [[ "$version" != "$msrv" ]]; then
    fail "member $id declares $version, expected $msrv"
  fi
done

echo "msrv: $msrv"

if [[ "${MSRV_CHECK_ONLY:-}" == "1" ]]; then
  exit 0
fi

rustup toolchain install "$msrv" --profile minimal
RUSTUP_AUTO_INSTALL=0 cargo +"$msrv" check --workspace --all-targets \
  --all-features --locked
