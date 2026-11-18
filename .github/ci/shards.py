#!/usr/bin/python3
"""Split a gate's work across matrix jobs, proved from the shards' reports.

The planner prints one JSON array of gates. `expand` replaces every mutants
gate with one entry per shard, each carrying its own `--shard k/n`, and marks
every entry with whether its gate needs the tools baked into the image. The
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
import tomllib

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


def needs_image(config: dict, gate: str) -> bool:
    """Whether `gate` needs the tools baked into the image.

    The image pre-installs exactly the tools a gate would otherwise build
    with `cargo install`, and nothing else. A setup without it, or one that
    only curls a tarball or runs pipx, npx or go, fetches its own tools.
    """
    # A matrix gate names its check and command, as in commands/mutants.
    command = config.get("commands", {}).get(gate.rpartition("/")[2], {})
    return any("cargo install" in line for line in command.get("setup", []))


def load_config(path: str | None) -> dict:
    """The project's gate config, or an empty one when it is absent.

    The entrypoint passes /work/.atelier/config.toml, which need not exist;
    then no gate is marked as needing the image's tools.
    """
    if path is None:
        return {}
    try:
        with open(path, "rb") as stream:
            return tomllib.load(stream)
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError):
        return {}


def expand_shards(matrix: str, shards: int, config: dict | None = None) -> str:
    """The planner's gate matrix with each mutants gate split into `shards`.

    Every entry also says whether its gate needs the image's baked tools,
    read from `config`'s `[commands.<name>.setup]`.
    """
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
        tools = needs_image(config or {}, gate)
        if gate.rpartition("/")[2] != "mutants":
            expanded.append({**item, "tools": tools})
            continue
        if "shard" in item:
            raise ValueError(f"gate {gate} already carries shard {item['shard']}")
        for k in range(shards):
            expanded.append(
                {
                    "gate": gate,
                    "root": root,
                    "shard": f"{k}/{shards}",
                    "tools": tools,
                }
            )
    return json.dumps(expanded)


def main(argv: list[str] | None = None) -> int:
    """`expand N [CONFIG]`: read the planner's matrix, print N jobs per gate."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["expand"])
    parser.add_argument("shards", type=int)
    parser.add_argument(
        "config",
        nargs="?",
        help="the project's .atelier/config.toml inside the container",
    )
    args = parser.parse_args(argv)
    try:
        print(expand_shards(sys.stdin.read(), args.shards, load_config(args.config)))
    except ValueError as error:
        print(f"atelier CI: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
