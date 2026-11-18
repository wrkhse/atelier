#!/usr/bin/python3
"""Persist proof results without changing the trusted base gate definitions."""

import json
import os
import re
import subprocess
import sys
from pathlib import Path

# reports.py owns the canary's path: beside bin/ in the image, beside this
# file in the repository.
HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE), str(HERE.parent)]
from reports import CANARY, MAX_REASON  # noqa: E402
from shards import parse_shard  # noqa: E402

CARGO = "/usr/local/cargo/bin/cargo"
GIT = "/usr/bin/git"
REPORTS = Path("/reports")
LIMIT_SECS = 3600
# The runner sets this to "k/n" for one shard of a mutants gate.
SHARD = os.environ.get("ATELIER_SHARD", "")
# One diff's deleted tests may cover this many production files before the
# change is too wide for one safeguard run: split the change instead.
MAX_SAFEGUARD_FILES = 64
# A test attribute, a `proptest!` block, and the first function line after
# either, for the textual scan.
ATTRIBUTE = re.compile(
    r"^\s*#\[((?:[A-Za-z0-9_]+::)*(?:test|rstest|test_case|proptest|proof|proof_for_contract))\b"
)
PROPTEST = re.compile(r"^\s*proptest!\s*\{")
# Every modifier Rust allows before `fn`, in any combination and order.
FUNCTION = re.compile(
    r'^\s*(?:(?:pub(?:\s*\([^)]*\))?|const|async|unsafe|extern\s+"[^"]*")\s+)*fn\s+([A-Za-z0-9_]+)'
)


def options(args: list[str], *extra: str) -> list[str]:
    """`args` with cargo-mutants options added before any test arguments."""
    end = args.index("--") if "--" in args else len(args)
    return [*args[:end], *extra, *args[end:]]


def listed(args: list[str]) -> list:
    """The mutants `args` select; cargo-mutants prints nothing for none."""
    try:
        output = subprocess.run(
            [CARGO, *options(args, "--list", "--json")],
            check=True,
            capture_output=True,
            text=True,
            timeout=LIMIT_SECS,
        ).stdout
    except subprocess.CalledProcessError as error:
        raise ValueError(f"cargo mutants --list failed: {last_line(error.stderr)}") from None
    except subprocess.TimeoutExpired:
        raise ValueError(f"cargo mutants --list ran past {LIMIT_SECS}s") from None

    if not output.strip():
        return []

    mutants = json.loads(output)

    if not isinstance(mutants, list):
        raise ValueError("mutation selection is not a list")

    return mutants


def last_line(text: str | None) -> str:
    """A tool's last stderr line, so a refusal stays one line."""
    lines = [line for line in (text or "").splitlines() if line.strip()]
    return lines[-1][:200] if lines else "no message"


def changed_files() -> list[str]:
    """The repository files the change touches, proving its diff can be made."""
    base = os.environ.get("ATELIER_BASE")

    if not base:
        raise ValueError("no ATELIER_BASE, so no diff can be evaluated")

    try:
        output = subprocess.run(
            [
                GIT,
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--name-only",
                base,
                "--",
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=LIMIT_SECS,
        ).stdout
    except subprocess.CalledProcessError as error:
        raise ValueError(f"git diff against {base} failed: {last_line(error.stderr)}") from None
    except subprocess.TimeoutExpired:
        raise ValueError(f"git diff against {base} ran past {LIMIT_SECS}s") from None

    return sorted(line for line in output.splitlines() if line)


def without_diff(args: list[str]) -> list[str] | None:
    """`args` without their diff filter, or None when they have none."""
    for index, arg in enumerate(args):
        if arg in ("--in-diff", "-D"):
            return [*args[:index], *args[index + 2 :]]

        if arg.startswith("--in-diff="):
            return [*args[:index], *args[index + 1 :]]

    return None


def shard_options(args: list[str], shard: tuple[int, int] | None) -> list[str]:
    """`args` with the shard cargo-mutants needs to agree on across jobs."""
    if shard is None:
        return args

    k, n = shard
    return options(args, "--shard", f"{k}/{n}")


def file_options(args: list[str], files: list[str]) -> list[str]:
    """`args` with one `--file` per path; cargo-mutants repeats the option."""
    for file in files:
        args = options(args, "--file", file)
    return args


def provenance(shard: tuple[int, int] | None) -> dict:
    """The run's gate, root and shard, each only when it is set."""
    entry = {}
    gate, root = os.environ.get("GATE", ""), os.environ.get("ROOT", "")

    if gate:
        entry["gate"] = gate

    if root:
        entry["root"] = root

    if shard is not None:
        entry["shard"] = f"{shard[0]}/{shard[1]}"

    return entry


def canary(
    args: list[str], evaluated: dict, why: str, shard: tuple[int, int] | None
) -> tuple[list[str], dict]:
    """Mutate the canary instead of an empty or unevaluated diff."""
    if os.environ.get("FLOOR") != "atelier":
        raise ValueError(f"{why}; the canary is atelier's own, so this gate has no evidence")

    if not Path(CANARY).is_file():
        raise ValueError(f"{why}, and the canary {CANARY} is missing")

    rest = without_diff(args)
    assert rest is not None, "only a diff run falls back to the canary"
    run = shard_options(rest, shard)
    whole = listed(options(rest, "--file", CANARY))

    # A canary that shrinks to nothing is no evidence either.
    if not whole:
        raise ValueError(f"the canary {CANARY} yields no mutant")

    mutants = whole if shard is None else listed(options(run, "--file", CANARY))
    selection = {
        "mode": "canary",
        **provenance(shard),
        "total": len(whole),
        **evaluated,
        "canary": CANARY,
        "selected": sorted(mutant.get("name") for mutant in mutants),
        "reason": " ".join(why.split())[:MAX_REASON],
    }
    return options(run, "--file", CANARY), selection


def select(args: list[str], shard: tuple[int, int] | None) -> tuple[list[str], dict]:
    """The diff's mutants, or the canary's when the diff has none to give."""
    note = REPORTS / "base.txt"

    # Without a usable base the diff is not this change's; never call it one.
    if note.is_file():
        unevaluated = {"diff_selected": None, "changed_files": None}
        return canary(args, unevaluated, note.read_text(encoding="utf-8"), shard)

    files = changed_files()
    whole = listed(args)
    evaluated = {"diff_selected": len(whole), "changed_files": files}

    if not whole:
        base = os.environ["ATELIER_BASE"]
        why = f"the diff against {base} changes {len(files)} repository files and selects no mutant"
        return canary(args, evaluated, why, shard)

    run = shard_options(args, shard)
    mutants = whole if shard is None else listed(run)
    selection = {
        "mode": "diff",
        **provenance(shard),
        "total": len(whole),
        **evaluated,
        "selected": sorted(mutant.get("name") for mutant in mutants),
    }
    return run, selection


def configured(
    args: list[str], shard: tuple[int, int] | None
) -> tuple[list[str], dict]:
    """A run with no diff filter: the whole configured selection, sliced."""
    whole = listed(args)
    run = shard_options(args, shard)
    mutants = whole if shard is None else listed(run)
    selection = {
        "mode": "configured",
        **provenance(shard),
        "total": len(whole),
        "selected": sorted(mutant.get("name") for mutant in mutants),
    }
    return run, selection


def blob(base: str, file: str) -> str | None:
    """`file`'s text at `base`, or None when that commit does not hold it."""
    try:
        output = subprocess.run(
            [GIT, "show", f"{base}:{file}"],
            check=True,
            capture_output=True,
            timeout=LIMIT_SECS,
        ).stdout
    except subprocess.CalledProcessError:
        return None
    except subprocess.TimeoutExpired:
        raise ValueError(f"git show {base}:{file} ran past {LIMIT_SECS}s") from None
    return output.decode("utf-8")


def test_names(text: str) -> set[str]:
    """The test names `text` declares, by its attributes and `proptest!` blocks.

    This is a conservative textual scan, not a Rust parser: an attribute, or
    a `proptest!` block, claims the first later `fn` line, so a deleted test
    is recognised without compiling the change, and over-reporting one only
    adds work.
    """
    names = set()
    attribute = None
    for line in text.splitlines():
        found = FUNCTION.match(line)
        if attribute is not None and found is not None:
            names.add(f"{attribute}:{found.group(1)}")
            attribute = None
        marker = ATTRIBUTE.match(line)
        if marker is not None:
            attribute = marker.group(1)
        elif PROPTEST.match(line):
            attribute = "proptest!"
    return names


def deleted_tests(base: str, file: str) -> list[str]:
    """The tests `file` held at `base` and no longer holds."""
    old = blob(base, file)
    if old is None:
        return []
    path = Path(file)
    new = path.read_text(encoding="utf-8") if path.is_file() else ""
    return sorted(test_names(old) - test_names(new))


def deleted_by_file(base: str, files: list[str]) -> dict[str, list[str]]:
    """The tests each changed Rust source file lost since `base`."""
    deleted = {}
    for file in files:
        if not file.startswith(("crates/", "plugins/")) or not file.endswith(".rs"):
            continue
        names = deleted_tests(base, file)
        if names:
            deleted[file] = names
    return deleted


def covered_files(file: str) -> list[str]:
    """The production files a changed source or test file's tests covered.

    A deleted `src` file also covered its parent module: the tests in
    `src/foo/tests.rs` exercised `src/foo.rs` through `mod foo`, so that
    module is mutated too when it exists.
    """
    parts = Path(file).parts
    if len(parts) < 4 or parts[0] not in ("crates", "plugins"):
        return []
    crate, kind, path = Path(*parts[:2]), parts[2], Path(file)
    if kind == "src":
        if path.is_file():
            return [file]
        siblings = [str(sibling) for sibling in path.parent.glob("*.rs")]
        if path.parent.name != "src":
            module = path.parent.parent / f"{path.parent.name}.rs"
            if module.is_file():
                siblings.append(str(module))
        return sorted(siblings)
    if kind == "tests":
        return sorted(str(name) for name in (crate / "src").rglob("*.rs"))
    return []


def safeguard_targets(deleted: dict[str, list[str]]) -> list[str]:
    """The existing production files the deleted tests covered, at most 64."""
    files = sorted({name for file in deleted for name in covered_files(file)})
    if not files:
        raise ValueError(
            f"the deleted tests in {next(iter(deleted))} cover no production file"
        )
    if len(files) > MAX_SAFEGUARD_FILES:
        raise ValueError(
            f"the deleted tests cover {len(files)} production files, "
            f"more than {MAX_SAFEGUARD_FILES}: split the change"
        )
    return files


def safeguard(
    args: list[str], files: list[str], shard: tuple[int, int] | None
) -> list[str] | None:
    """Mutate the production files whose deleted tests this diff removed.

    A deleted test cannot fail, so the diff's own selection no longer proves
    the code it used to; those production files are mutated instead.
    """
    deleted = deleted_by_file(os.environ["ATELIER_BASE"], files)
    if not deleted:
        return None
    targets = safeguard_targets(deleted)
    rest = without_diff(args)
    assert rest is not None, "a safeguard comes from a diff run"
    whole = listed(file_options(rest, targets))

    if not whole:
        joined = ", ".join(targets)
        raise ValueError(
            f"the deleted tests in {joined} cover no mutant there; "
            "restore them or name the equivalents in .cargo/mutants.toml"
        )

    run = shard_options(rest, shard)
    listing = file_options(run, targets)
    mutants = whole if shard is None else listed(listing)
    selection = {
        "mode": "safeguard",
        **provenance(shard),
        "total": len(whole),
        "selected": sorted(mutant.get("name") for mutant in mutants),
        "deleted": sorted(
            f"{file}::{name}" for file, names in deleted.items() for name in names
        ),
        "files": targets,
    }
    path = REPORTS / "mutants-safeguard.json"
    path.write_text(json.dumps(selection), encoding="utf-8")
    print(f"mutants safeguard: {json.dumps(selection)}", file=sys.stderr)
    return options(listing, "--output", str(REPORTS / "mutants-safeguard"))


def mutants(args: list[str]) -> list[list[str]]:
    """Record which mutants the run tests, then return the run to make."""
    shard = parse_shard(SHARD)

    if without_diff(args) is not None:
        run, selection = select(args, shard)
    else:
        run, selection = configured(args, shard)

    (REPORTS / "mutants-selection.json").write_text(json.dumps(selection), encoding="utf-8")
    print(f"mutants selection: {json.dumps(selection)}", file=sys.stderr)
    runs = [options(run, "--output", str(REPORTS))]
    files = selection.get("changed_files")

    if without_diff(args) is not None and files is not None:
        guard = safeguard(args, files, shard)

        if guard is not None:
            runs.append(guard)

    return runs


def arguments(args: list[str]) -> list[list[str]]:
    """Export Kani's results; record which mutants a run tests, and why."""
    if args and args[0] == "kani" and args[1:] != ["setup"]:
        export = ["-Z", "unstable-options", "--export-json"]
        return [[*args, *export, str(REPORTS / "kani.json")]]

    if not args or args[0] != "mutants" or any(arg.startswith("--list") for arg in args):
        return [args]

    return mutants(args)


def main() -> int:
    REPORTS.mkdir(exist_ok=True)

    try:
        runs = arguments(sys.argv[1:])
    except ValueError as error:
        # A refusal is a verdict, not a crash: one line, and the gate fails.
        print(f"atelier CI: {error}", file=sys.stderr)
        return 1

    for args in runs:
        try:
            status = subprocess.run([CARGO, *args], check=False, timeout=LIMIT_SECS)
        except subprocess.TimeoutExpired:
            # run() has already killed cargo itself; the verdict stays one line.
            message = f"atelier CI: cargo {' '.join(args[:1])} ran past {LIMIT_SECS}s"
            print(message, file=sys.stderr)
            return 1

        if status.returncode != 0:
            return status.returncode

    return 0


if __name__ == "__main__":
    sys.exit(main())
