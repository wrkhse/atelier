#!/usr/bin/python3
"""Split a gate's work across matrix jobs, proved from the shards' reports.

The planner prints one JSON array of gates. `expand` replaces every mutants
gate with one entry per shard, each carrying its own `--shard k/n`, and the
runner starts each entry as its own job. The shards are not trusted
separately: reports.py collects their own reports and proves they partition
the gate, with no mutant tested twice and none left out.

cargo-mutants slices a selection itself, as `Sharding::Slice` documents at
mutants.rs/shards.html and implements in src/shard.rs: chunk =
ceil(total / n), start = k * chunk, end = min((k + 1) * chunk, total). The
expected size of every shard follows from the gate's whole total, so the
partition can be checked without trusting any single shard.
"""

import argparse
import json
import re
import sys

MAX_SHARDS = 8
SHARD = re.compile(r"([0-9]+)/([0-9]+)")


def parse_shard(value: str) -> tuple[int, int] | None:
    """`k/n` as a shard index and count, or None when the run is unsharded."""
    if not value:
        return None
    parsed = SHARD.fullmatch(value)
    if parsed is None:
        raise ValueError(f"a shard is k/n, not {value!r}")
    k, n = int(parsed.group(1)), int(parsed.group(2))
    if not 0 <= k < n <= MAX_SHARDS:
        raise ValueError(f"shard {value} is not k/n with 0 <= k < n <= {MAX_SHARDS}")
    return k, n


def slice_size(total: int, k: int, n: int) -> int:
    """How many mutants slice `k` of `n` tests, by cargo-mutants' own rule."""
    if total < 0:
        raise ValueError(f"a mutant total must be nonnegative, not {total}")
    if n < 1:
        raise ValueError(f"a shard count must be at least 1, not {n}")
    if k < 0:
        raise ValueError(f"a shard index must be nonnegative, not {k}")
    chunk = (total + n - 1) // n
    start = k * chunk
    end = min((k + 1) * chunk, total)
    return max(0, end - start)


def expand_shards(matrix: str, shards: int) -> str:
    """The planner's gate matrix with each mutants gate split into `shards`."""
    if not 1 <= shards <= MAX_SHARDS:
        raise ValueError(f"a shard count is 1 to {MAX_SHARDS}, not {shards}")
    try:
        gates = json.loads(matrix)
    except json.JSONDecodeError as error:
        raise ValueError(f"the gate matrix is not JSON: {error}") from None
    if not isinstance(gates, list):
        raise ValueError("the gate matrix is not an array")
    expanded = []
    for item in gates:
        if not isinstance(item, dict):
            raise ValueError(f"a gate is not an object: {item!r}")
        gate, root = item.get("gate"), item.get("root")
        if not isinstance(gate, str) or not gate:
            raise ValueError(f"a gate has no name: {item!r}")
        if not isinstance(root, str) or not root:
            raise ValueError(f"gate {gate} has no root")
        if gate.rpartition("/")[2] != "mutants":
            expanded.append(item)
            continue
        if "shard" in item:
            raise ValueError(f"gate {gate} already carries shard {item['shard']}")
        for k in range(shards):
            expanded.append({"gate": gate, "root": root, "shard": f"{k}/{shards}"})
    return json.dumps(expanded)


def main(argv: list[str] | None = None) -> int:
    """`expand N`: read the planner's matrix on stdin, print N jobs per gate."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["expand"])
    parser.add_argument("shards", type=int)
    args = parser.parse_args(argv)
    try:
        print(expand_shards(sys.stdin.read(), args.shards))
    except ValueError as error:
        print(f"atelier CI: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
