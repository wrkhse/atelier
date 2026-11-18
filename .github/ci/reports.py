"""Fail-closed checks for Kani 0.68 and cargo-mutants 27.1 CI reports.

Usage: python3 .github/ci/reports.py <gate> <reports> [--atelier]
       python3 .github/ci/reports.py --partition <shards> <reports>
<reports> holds kani.json, mutants-selection.json, base.txt and
mutants.out/outcomes.json, as the gate container wrote them to /reports.
--partition collects one gate's shards, each in its own directory under
<reports> with a container/ sub-directory, and proves their reports are one
complete partition.
--atelier adds atelier's own floor: both core proofs, and the canary. run.sh
passes it when the workflow runs from the repository it checks.
Other gates rely on their process exit status and need no report.
Contracts: Kani's kani-driver/src/frontend/schema_utils.rs and cargo-mutants'
src/outcome.rs, src/output.rs and src/shard.rs at the pinned upstream release
tags.
"""

import argparse
import json
import sys
from collections import Counter
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE), str(HERE.parent)]
from shards import parse_shard, slice_size  # noqa: E402

MAX_REPORT_BYTES = 128 * 1024 * 1024

# The signal boundary of the 2026-11-13 host incident. When a change selects
# no mutant, cargo.py mutates this file so the report is never empty.
CANARY = "crates/cli/src/command_step/run.rs"
# The longest canary reason the job summary shows, on one line.
MAX_REASON = 300
ATELIER_HARNESSES = frozenset(
    {
        "machine::proofs::a_run_follows_what_rungs_answers",
        "machine::proofs::"
        "a_move_outside_rungs_keeps_the_marks_and_the_resident_steps_right",
    }
)
PROPERTY_STATUSES = {
    "Success": "passed",
    "Failure": "failed",
    "Unreachable": "unreachable",
    "Undetermined": "undetermined",
    "Unknown": "undetermined",
    "Error": "solver_error",
    "Satisfied": "satisfied",
    "Unsatisfiable": "unsatisfiable",
    "Covered": "covered",
    "Uncovered": "uncovered",
}


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def object_value(value: object, label: str) -> dict:
    require(isinstance(value, dict), f"{label} must be an object")
    return value


def array_value(value: object, label: str) -> list:
    require(isinstance(value, list), f"{label} must be an array")
    return value


def count(value: object, label: str) -> int:
    require(type(value) is int and value >= 0, f"{label} must be a nonnegative integer")
    return value


def unique_object(pairs: list) -> dict:
    result = {}
    for key, value in pairs:
        require(key not in result, f"duplicate JSON key: {key}")
        result[key] = value
    return result


def invalid_constant(value: str) -> None:
    raise ValueError(f"invalid JSON number: {value}")


def read_report(path: Path) -> dict:
    require(path.is_file() and not path.is_symlink(), f"missing regular report: {path}")
    with path.open("rb") as source:
        raw = source.read(MAX_REPORT_BYTES + 1)
    require(0 < len(raw) <= MAX_REPORT_BYTES, f"empty or oversized report: {path}")
    return object_value(
        json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=unique_object,
            parse_constant=invalid_constant,
        ),
        str(path),
    )


def harness_entries(
    value: object, key: str, label: str, expected: frozenset | None = None
) -> dict:
    entries = {}
    for item in array_value(value, label):
        entry = object_value(item, label)
        name = entry.get(key)
        require(isinstance(name, str) and bool(name), f"missing {label} identity")
        require(name not in entries, f"duplicate {label} identity: {name}")
        entries[name] = entry
    require(bool(entries), f"no {label}")
    if expected is not None:
        require(frozenset(entries) == expected, f"incomplete {label}")
    return entries


def validate_properties(result: dict, entry: dict) -> None:
    checks = array_value(result.get("checks"), "checks")
    require(bool(checks), "harness has no property checks")
    statuses = Counter()
    for index, item in enumerate(checks, 1):
        check = object_value(item, "check")
        require(count(check.get("id"), "check.id") == index, "incomplete check identities")
        status = check.get("status")
        require(status in ("Success", "Unreachable"), f"unsuccessful check: {status}")
        statuses[PROPERTY_STATUSES[status]] += 1
    require(statuses["passed"] > 0, "harness has no passed checks")
    details = object_value(entry.get("property_details"), "property_details")
    require(details.get("error") is None, "property extraction error")
    require(
        count(details.get("total_properties"), "total_properties") == len(checks),
        "property total does not match checks",
    )
    for field in set(PROPERTY_STATUSES.values()):
        require(
            count(details.get(field), field) == statuses[field],
            f"property count mismatch: {field}",
        )


def validate_kani(report: dict, atelier: bool) -> str:
    """Check the Kani export; the summary line names the floor applied."""
    metadata = object_value(report.get("metadata"), "metadata")
    require(metadata.get("kani_version") == "0.68.0", "unexpected Kani version")
    harnesses = harness_entries(report.get("harness_metadata"), "pretty_name", "metadata")
    names = frozenset(harnesses)
    if atelier:
        require(ATELIER_HARNESSES <= names, "atelier's core proofs did not all run")
    verification = object_value(report.get("verification_results"), "verification_results")
    summary = object_value(verification.get("summary"), "summary")
    require(summary.get("status") == "completed", "Kani run is not completed")
    for field in ("total_harnesses", "executed", "successful"):
        require(count(summary.get(field), field) == len(names), f"incomplete {field}")
    require(count(summary.get("failed"), "failed") == 0, "failed Kani harnesses")
    results = harness_entries(verification.get("results"), "harness_id", "results", names)
    errors = harness_entries(report.get("error_details"), "harness_id", "errors", names)
    properties = harness_entries(
        report.get("property_details"), "harness_id", "properties", names
    )
    harness_entries(report.get("cbmc"), "harness_id", "cbmc", names)
    for name, result in results.items():
        if atelier and name in ATELIER_HARNESSES:
            crate = harnesses[name].get("crate_name")
            require(crate == "atelier_core", "wrong harness crate")
        attributes = object_value(harnesses[name].get("attributes"), "attributes")
        require(
            attributes.get("kind") == "Proof" and attributes.get("should_panic") is False,
            "expected a non-panicking proof harness",
        )
        require(result.get("status") == "Success", f"unsuccessful harness: {name}")
        require(errors[name].get("has_errors") is False, f"harness errors: {name}")
        require(errors[name].get("error_type") is None, f"harness error type: {name}")
        require(
            errors[name].get("exit_status") in (None, "success"),
            f"harness exit failure: {name}",
        )
        validate_properties(result, properties[name])
    floor = (
        f"atelier floor: its {len(ATELIER_HARNESSES)} core proofs required"
        if atelier
        else "no atelier floor (not atelier's own workflow)"
    )
    return f"{len(names)} harnesses verified; {floor}"


def timestamp(value: object, label: str) -> datetime:
    require(isinstance(value, str) and bool(value), f"missing {label}")
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    require(parsed.tzinfo is not None, f"{label} must include a timezone")
    return parsed


def failure_status(value: object) -> bool:
    return (
        isinstance(value, dict)
        and set(value) == {"Failure"}
        and type(value["Failure"]) is int
        and value["Failure"] != 0
    )


def validate_phases(outcome: dict, summary: str) -> None:
    phases = [
        object_value(item, "phase")
        for item in array_value(outcome.get("phase_results"), "phase_results")
    ]
    names = [phase.get("phase") for phase in phases]
    expected = ["Build"] if summary == "Unviable" else ["Build", "Test"]
    require(names == expected, f"incomplete phases for {summary}")
    for phase in phases[:-1]:
        require(phase.get("process_status") == "Success", "unsuccessful build")
    final = phases[-1].get("process_status")
    if summary == "Success":
        require(final == "Success", "baseline tests did not succeed")
    else:
        require(failure_status(final), f"missing failure evidence for {summary}")


def validate_mutants(report: dict, empty_ok: bool = False) -> dict:
    """The tested mutants' names and files, and how many were caught.

    A shard whose slice of the gate's selection is empty tests no mutant, so
    `empty_ok` admits exactly that report; every other run must test one.
    """
    require(report.get("cargo_mutants_version") == "27.1.0", "unexpected mutants version")
    start = timestamp(report.get("start_time"), "start_time")
    end = timestamp(report.get("end_time"), "end_time")
    require(end >= start, "mutants end_time precedes start_time")
    total = count(report.get("total_mutants"), "total_mutants")
    require(total > 0 or empty_ok, "zero mutants tested")
    outcomes = array_value(report.get("outcomes"), "outcomes")
    require(len(outcomes) == total + 1, "outcome count must include mutants and baseline")
    counts = Counter()
    names = set()
    files = []
    baseline = 0
    for item in outcomes:
        outcome = object_value(item, "outcome")
        scenario = outcome.get("scenario")
        summary = outcome.get("summary")
        if scenario == "Baseline":
            baseline += 1
            require(summary == "Success", "unsuccessful baseline")
        else:
            mutant = object_value(scenario, "scenario")
            require(set(mutant) == {"Mutant"}, "unknown mutant scenario")
            details = object_value(mutant["Mutant"], "Mutant")
            identity = details.get("name")
            require(isinstance(identity, str) and bool(identity.strip()), "missing mutant name")
            require(identity not in names, "duplicate mutant outcome")
            names.add(identity)
            files.append(details.get("file"))
            require(summary in ("CaughtMutant", "Unviable"), f"bad mutant outcome: {summary}")
            counts[summary] += 1
        validate_phases(outcome, summary)
    require(baseline == 1 and len(names) == total, "incomplete baseline or mutant outcomes")
    expected = {
        "caught": counts["CaughtMutant"],
        "unviable": counts["Unviable"],
        "missed": 0,
        "timeout": 0,
        "success": 0,
    }
    counters = {}
    for field, wanted in expected.items():
        counters[field] = count(report.get(field), field)
        require(counters[field] == wanted, f"mutant count mismatch: {field}")
    return {
        "names": sorted(names),
        "files": files,
        "caught": counts["CaughtMutant"],
        "missed": counters["missed"],
        "timeout": counters["timeout"],
    }


def selection_shard(selection: dict) -> tuple[int, int, int] | None:
    """A selection's shard index and count, and the whole gate's mutant total."""
    label = selection.get("shard")
    if label is None:
        return None
    require(isinstance(label, str), "a shard is a k/n string")
    parsed = parse_shard(label)
    require(parsed is not None, "a shard cannot be empty")
    k, n = parsed
    total = count(selection.get("total"), "total")
    require(total >= 1, "a shard needs a non-empty gate selection")
    return k, n, total


def shard_slice(shard: tuple[int, int, int] | None) -> int | None:
    """The number of mutants a shard must test, or None when unsharded."""
    if shard is None:
        return None
    k, n, total = shard
    return slice_size(total, k, n)


def shard_prefix(shard: tuple[int, int, int] | None) -> str:
    """The summary's opening when one shard of a larger run was tested."""
    if shard is None:
        return ""
    k, n, total = shard
    return f"shard {k}/{n} of {total} mutants: "


def selected_names(
    selection: dict, tested: dict, label: str, expected: int | None = None
) -> int:
    """The selection's names, which must be the tested ones and its shard's slice."""
    names = array_value(selection.get("selected"), "selected")
    if expected is None:
        require(bool(names), f"the {label} selected no mutant")
    require(
        all(isinstance(name, str) and name for name in names),
        f"bad {label} mutant",
    )
    require(sorted(names) == tested["names"], f"tested mutants differ from the {label}'s")
    if expected is not None:
        require(
            len(names) == expected,
            f"the shard tests {len(names)} mutants, not {expected}",
        )
    return len(names)


def evaluated_diff(selection: dict, fallback: bool) -> tuple[int, int] | None:
    """The diff's changed files and mutants, or None when it was not evaluated."""
    selected, files = selection.get("diff_selected"), selection.get("changed_files")
    if selected is None or files is None:
        require(selected is None and files is None, "half an evaluated diff")
        require(fallback, "a diff was not evaluated though the base was available")
        return None
    require(not fallback, "the base was unavailable, so no diff can be evaluated")
    files = array_value(files, "changed_files")
    require(all(isinstance(file, str) and file for file in files), "bad changed file")
    return len(files), count(selected, "diff_selected")


def validate_canary(
    selection: dict,
    tested: dict,
    diff: tuple[int, int] | None,
    shard: tuple[int, int, int] | None,
) -> str:
    """The canary's own report; a sharded one proves its catch by partition."""
    require(diff is None or diff[1] == 0, "canary with a diff that selected mutants")
    canary = selection.get("canary")
    require(canary == CANARY, f"unexpected canary: {canary}")
    listed = selected_names(selection, tested, "canary", shard_slice(shard))
    require(all(file == CANARY for file in tested["files"]), "a canary mutated another file")
    # A sharded canary's caught mutant is required of the whole partition.
    require(tested["caught"] > 0 or shard is not None, "no canary mutant was caught")
    why = selection.get("reason")
    require(isinstance(why, str) and bool(why.strip()), "canary run without a reason")
    require(len(why) <= MAX_REASON and "\n" not in why, "canary reason is not one short line")
    head = "diff not evaluated" if diff is None else f"diff changed {diff[0]} repository files"
    return f"{head}, selected 0 mutants; canary {listed} mutants in {CANARY}: {why}"


def validate_selection(selection: dict, tested: dict, fallback: bool) -> str:
    """Which mutants were tested and why; a canary never reads as diff coverage."""
    shard = selection_shard(selection)
    mode = selection.get("mode")
    if mode == "configured":
        summary = f"{len(tested['names'])} mutants as the gate configures them (no --in-diff)"
        if shard is None:
            return summary
        selected_names(selection, tested, "configured run", shard_slice(shard))
        return shard_prefix(shard) + summary
    require(mode in ("diff", "canary"), f"unknown mutants selection: {mode}")
    diff = evaluated_diff(selection, fallback)
    if mode == "canary":
        return shard_prefix(shard) + validate_canary(selection, tested, diff, shard)
    require(diff is not None, "a diff run needs an evaluated diff")
    files, selected = diff
    require(files > 0, "a diff that selected mutants changed no file")
    names = selected_names(selection, tested, "diff", shard_slice(shard))
    if shard is None:
        require(selected == names, "diff count mismatch")
        return f"diff changed {files} repository files, selected {selected} mutants; canary 0 mutants (not run)"
    require(selected == shard[2], "diff count mismatch")
    return shard_prefix(shard) + (
        f"diff changed {files} repository files, selected {selected} mutants; "
        f"this shard tests {names}"
    )


def validate_shard(selection: dict, reports: Path) -> tuple[dict, str]:
    """One mutants report against its selection: the tested mutants and summary."""
    shard = selection_shard(selection)
    empty = shard_slice(shard) == 0
    outcomes = read_report(reports / "mutants.out" / "outcomes.json")
    tested = validate_mutants(outcomes, empty)
    fallback = (reports / "base.txt").is_file()
    return tested, validate_selection(selection, tested, fallback)


def validate_partition(shards: int, reports: Path) -> str:
    """Prove the shards' own reports are one complete partition of their gate.

    Each shard directory holds a container/ with its selection and report.
    The shard indexes must be exactly 0..shards-1 once each, the whole-gate
    totals must agree, and the shards' tested mutants must be distinct and
    add up to that total. A sharded canary needs one caught mutant across
    the group, not in every report; each report must still mutate only
    CANARY.
    """
    require(shards >= 1, "a partition needs at least one shard")
    directories = sorted(path for path in reports.iterdir() if path.is_dir())
    require(bool(directories), f"no shard reports under {reports}")
    groups = {}
    for directory in directories:
        container = directory / "container"
        selection_path = container / "mutants-selection.json"
        selection = read_report(selection_path)
        shard = selection_shard(selection)
        require(shard is not None, f"an unsharded selection: {selection_path}")
        require(
            shard[1] == shards,
            f"{container} is shard {shard[0]}/{shard[1]}, not of {shards}",
        )
        gate, root, mode = (
            selection.get("gate"),
            selection.get("root"),
            selection.get("mode"),
        )
        require(isinstance(gate, str) and gate, f"{container} records no gate")
        require(isinstance(root, str) and root, f"{container} records no root")
        require(isinstance(mode, str) and mode, f"{container} records no mode")
        entry = (container, selection, shard)
        groups.setdefault((gate, root, mode), []).append(entry)
    parts = []
    for (gate, root, mode), entries in groups.items():
        label = f"{gate} at {root} ({mode})"
        indexes = sorted(shard[0] for _, _, shard in entries)
        expected = list(range(shards))
        require(indexes == expected, f"{label} has shards {indexes}, not {expected}")
        totals = {shard[2] for _, _, shard in entries}
        require(len(totals) == 1, f"{label} divides totals {sorted(totals)}")
        total = totals.pop()
        tested = []
        caught = 0
        missed = 0
        timeouts = 0
        files = []
        for container, selection, _ in entries:
            report, _ = validate_shard(selection, container)
            tested.extend(report["names"])
            caught += report["caught"]
            missed += report["missed"]
            timeouts += report["timeout"]
            files.extend(report["files"])
        require(len(tested) == len(set(tested)), f"{label} tests a mutant twice")
        require(
            len(tested) == total,
            f"{label} tests {len(tested)} mutants, not {total}",
        )
        if mode == "canary":
            require(caught > 0, f"{label} caught no canary mutant")
            require(
                all(file == CANARY for file in files),
                f"{label} mutated another file",
            )
        parts.append(
            f"{len(entries)} shards, {len(tested)} mutants, "
            f"{missed} missed, {timeouts} timeouts"
        )
    return "; ".join(parts)


def validate(gate: str, reports: Path, atelier: bool) -> str | None:
    """Check `commands/kani` and `commands/mutants`; the summary line, if any."""
    name = gate.rpartition("/")[2]
    if name == "kani":
        return validate_kani(read_report(reports / "kani.json"), atelier)
    elif name == "mutants":
        selection = read_report(reports / "mutants-selection.json")
        # The canary is atelier's own, under the same floor as the proofs.
        require(
            selection.get("mode") != "canary" or atelier,
            "the canary is atelier's own",
        )
        return validate_shard(selection, reports)[1]
    return None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("arguments", nargs="*", metavar="<gate> <reports>")
    parser.add_argument("--atelier", action="store_true", help="atelier's own floor")
    parser.add_argument("--partition", type=int, metavar="SHARDS")
    args = parser.parse_args(argv)
    # --partition N <reports> takes only the reports directory as a positional.
    if args.partition is None:
        if len(args.arguments) != 2:
            parser.error("a gate and a reports directory are required")
        gate, reports = args.arguments[0], Path(args.arguments[1])
    else:
        if len(args.arguments) != 1:
            parser.error("--partition takes one reports directory")
        gate, reports = None, Path(args.arguments[0])
    label = "partition" if gate is None else gate
    try:
        if gate is None:
            summary = validate_partition(args.partition, reports)
        else:
            summary = validate(gate, reports, args.atelier)
    except (OSError, UnicodeError, ValueError, RecursionError) as error:
        print(f"{label}: invalid CI report: {error}", file=sys.stderr)
        return 1
    if summary:
        print(f"**{label}:** {summary}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
