"""Report contract regressions; execute only inside the CI container."""

import contextlib
import copy
import importlib.machinery
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("reports.py")
SPEC = importlib.util.spec_from_file_location("ci_reports", SCRIPT)
reports = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reports)

# In the image the wrapper is bin/cargo; in the repository, cargo.py.
WRAPPER = SCRIPT.with_name("bin") / "cargo"
WRAPPER = WRAPPER if WRAPPER.is_file() else SCRIPT.with_name("cargo.py")
LOADER = importlib.machinery.SourceFileLoader("ci_cargo", str(WRAPPER))
cargo = importlib.util.module_from_spec(importlib.util.spec_from_loader("ci_cargo", LOADER))
LOADER.exec_module(cargo)

SHARDS = SCRIPT.with_name("shards.py")
SHARD_SPEC = importlib.util.spec_from_file_location("ci_shards", SHARDS)
shards = importlib.util.module_from_spec(SHARD_SPEC)
SHARD_SPEC.loader.exec_module(shards)


def kani_report(names: list[str] | None = None, crate: str = "atelier_core") -> dict:
    """Representative export from Kani 0.68's schema_utils.rs contract."""
    names = sorted(reports.ATELIER_HARNESSES) if names is None else names
    total = len(names)
    return {
        "metadata": {"kani_version": "0.68.0"},
        "harness_metadata": [
            {
                "pretty_name": name,
                "crate_name": crate,
                "attributes": {"kind": "Proof", "should_panic": False},
            }
            for name in names
        ],
        "verification_results": {
            "summary": {
                "total_harnesses": total,
                "executed": total,
                "status": "completed",
                "successful": total,
                "failed": 0,
            },
            "results": [
                {
                    "harness_id": name,
                    "status": "Success",
                    "checks": [{"id": 1, "status": "Success"}],
                }
                for name in names
            ],
        },
        "error_details": [
            {"harness_id": name, "has_errors": False} for name in names
        ],
        "property_details": [
            {
                "harness_id": name,
                "property_details": {
                    "total_properties": 1,
                    **{
                        field: int(field == "passed")
                        for field in set(reports.PROPERTY_STATUSES.values())
                    },
                },
            }
            for name in names
        ],
        "cbmc": [{"harness_id": name} for name in names],
    }


def phases(final: object = "Success", unviable: bool = False) -> list:
    if unviable:
        return [{"phase": "Build", "process_status": final}]
    return [
        {"phase": "Build", "process_status": "Success"},
        {"phase": "Test", "process_status": final},
    ]


def mutants_report(names: list[str] | None = None, file: str = "src/lib.rs") -> dict:
    """Matches the real 27.1 report; mutants alternate caught and unviable."""
    names = (
        ["src/lib.rs:1: replace true", "src/lib.rs:2: replace return"]
        if names is None
        else names
    )
    outcomes = [
        {"scenario": "Baseline", "summary": "Success", "phase_results": phases()}
    ]
    for index, name in enumerate(names, 1):
        unviable = index % 2 == 0
        outcomes.append(
            {
                "scenario": {"Mutant": {"name": name, "file": file}},
                "summary": "Unviable" if unviable else "CaughtMutant",
                "phase_results": phases({"Failure": 99 + index}, unviable=unviable),
            }
        )
    caught = (len(names) + 1) // 2
    return {
        "cargo_mutants_version": "27.1.0",
        "start_time": "2026-11-13T15:49:29.548516Z",
        "end_time": "2026-11-13T15:50:29.548516Z",
        "total_mutants": len(names),
        "caught": caught,
        "unviable": len(names) - caught,
        "missed": 0,
        "timeout": 0,
        "success": 0,
        "outcomes": outcomes,
    }


def diff_selection(value: object) -> dict:
    """What cargo.py records when a diff selects exactly the report's mutants."""
    try:
        names = [item["scenario"]["Mutant"]["name"] for item in value["outcomes"][1:]]
    except (KeyError, IndexError, TypeError):
        names = []
    return {
        "mode": "diff",
        "total": len(names),
        "diff_selected": len(names),
        "changed_files": ["src/lib.rs"],
        "selected": names,
    }


def shard_names(total: int, k: int, n: int) -> list[str]:
    """Distinct mutant names for one shard of a `total`-mutant gate."""
    start = sum(shards.slice_size(total, index, n) for index in range(k))
    size = shards.slice_size(total, k, n)
    return [f"src/lib.rs:{start + index}: replace return" for index in range(size)]


def sharded_selection(
    names: list[str], k: int, n: int, total: int, **changes: object
) -> dict:
    """What cargo.py records for shard k of n when it tests these names."""
    selection = {
        "mode": "diff",
        "gate": "commands/mutants",
        "root": ".",
        "shard": f"{k}/{n}",
        "total": total,
        "diff_selected": total,
        "changed_files": ["src/lib.rs"],
        "selected": names,
    }
    return {**selection, **changes}


class ReportCase(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def write(self, gate: str, value: object, selection: dict | None = None) -> Path:
        kani = gate.rpartition("/")[2] == "kani"
        relative = "kani.json" if kani else "mutants.out/outcomes.json"
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value), encoding="utf-8")
        if not kani:
            # By default the report is what a diff selected.
            self.select(selection or diff_selection(value))
        return path

    def select(self, selection: dict) -> None:
        path = self.root / "mutants-selection.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(selection), encoding="utf-8")

    def rejected(self, gate: str, value: object) -> None:
        self.write(gate, value)
        with self.assertRaises(ValueError):
            reports.validate(gate, self.root, True)


class ReportTests(ReportCase):
    def test_good_reports_through_cli(self) -> None:
        for gate, value in (("kani", kani_report()), ("mutants", mutants_report())):
            with self.subTest(gate=gate):
                self.write(gate, value)
                completed = subprocess.run(
                    [sys.executable, str(SCRIPT), gate, str(self.root), "--atelier"],
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                self.assertEqual(completed.returncode, 0, completed.stderr)

    def test_matrix_gate_names_are_validated(self) -> None:
        for gate in ("commands/kani", "commands/mutants"):
            with self.subTest(gate=gate):
                self.assertEqual(reports.main([gate, str(self.root), "--atelier"]), 1)
        self.write("commands/kani", kani_report())
        self.write("commands/mutants", mutants_report())
        for gate in ("commands/kani", "commands/mutants"):
            with self.subTest(gate=gate, report="good"):
                self.assertEqual(reports.main([gate, str(self.root), "--atelier"]), 0)

    def test_cli_missing_report_fails(self) -> None:
        for gate in ("kani", "mutants", "commands/kani", "commands/mutants"):
            with self.subTest(gate=gate):
                completed = subprocess.run(
                    [sys.executable, str(SCRIPT), gate, str(self.root), "--atelier"],
                    capture_output=True,
                    text=True,
                    timeout=10,
                    check=False,
                )
                self.assertEqual(completed.returncode, 1)
                self.assertIn("invalid CI report", completed.stderr)

    def test_missing_empty_malformed_and_nonobject_reports(self) -> None:
        for gate in ("kani", "mutants"):
            with self.subTest(gate=gate, case="missing"):
                with self.assertRaises(ValueError):
                    reports.validate(gate, self.root, True)
            for text in ("", " ", "{", "null", "[]", "true", "{}", '{"x":NaN}'):
                with self.subTest(gate=gate, text=text):
                    self.write(gate, {}).write_text(text, encoding="utf-8")
                    with self.assertRaises(ValueError):
                        reports.validate(gate, self.root, True)

    def test_duplicate_json_keys_and_invalid_encoding(self) -> None:
        for gate in ("kani", "mutants"):
            for raw in (b'{"outcomes":[],"outcomes":[]}', b"\xff"):
                with self.subTest(gate=gate, raw=raw):
                    self.write(gate, {}).write_bytes(raw)
                    with self.assertRaises((ValueError, UnicodeError)):
                        reports.validate(gate, self.root, True)

    def test_oversized_reports_fail(self) -> None:
        original = reports.MAX_REPORT_BYTES
        self.addCleanup(setattr, reports, "MAX_REPORT_BYTES", original)
        reports.MAX_REPORT_BYTES = 1
        self.rejected("kani", kani_report())

    def test_other_gates_need_no_report(self) -> None:
        for gate in ("fmt", "commands/clippy", "nextest", "doctest", "unknown"):
            with self.subTest(gate=gate):
                self.assertEqual(reports.main([gate, str(self.root), "--atelier"]), 0)

    def test_main_reports_malformed_input_without_traceback(self) -> None:
        self.write("kani", {}).write_text("{", encoding="utf-8")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            self.assertEqual(reports.main(["kani", str(self.root), "--atelier"]), 1)
        self.assertIn("invalid CI report", stderr.getvalue())


class SliceTests(unittest.TestCase):
    def test_slices_follow_cargo_mutants_algorithm(self) -> None:
        for total in range(10):
            sizes = [shards.slice_size(total, k, 4) for k in range(4)]
            self.assertEqual(sum(sizes), total)
        self.assertEqual(shards.slice_size(0, 0, 4), 0)
        self.assertEqual(shards.slice_size(2, 0, 4), 1)
        self.assertEqual([shards.slice_size(8, k, 4) for k in range(4)], [2, 2, 2, 2])
        self.assertEqual([shards.slice_size(3, k, 4) for k in range(4)], [1, 1, 1, 0])
        self.assertEqual(shards.slice_size(4, 4, 4), 0)
        self.assertEqual(shards.slice_size(4, 8, 4), 0)

    def test_slices_refuse_impossible_numbers(self) -> None:
        for total, k, n in ((-1, 0, 4), (4, -1, 4), (4, 0, 0)):
            with self.subTest(total=total, k=k, n=n), self.assertRaises(ValueError):
                shards.slice_size(total, k, n)


class ExpandTests(unittest.TestCase):
    def test_expand_splits_only_mutants_gates_in_order(self) -> None:
        matrix = [
            {"gate": "commands/fmt", "root": "."},
            {"gate": "commands/mutants", "root": "."},
            {"gate": "commands/kani", "root": "docs"},
        ]
        text = shards.expand_shards(json.dumps(matrix), 2)
        self.assertNotIn("\n", text)
        self.assertEqual(
            json.loads(text),
            [
                {"gate": "commands/fmt", "root": "."},
                {"gate": "commands/mutants", "root": ".", "shard": "0/2"},
                {"gate": "commands/mutants", "root": ".", "shard": "1/2"},
                {"gate": "commands/kani", "root": "docs"},
            ],
        )

    def test_expand_refuses_a_matrix_it_cannot_split(self) -> None:
        cases = {
            "not json": "[",
            "not an array": "{}",
            "not objects": '["commands/mutants"]',
            "no gate": '[{"root": "."}]',
            "empty gate": '[{"gate": "", "root": "."}]',
            "no root": '[{"gate": "commands/mutants"}]',
            "empty root": '[{"gate": "commands/mutants", "root": ""}]',
            "already sharded": (
                '[{"gate": "commands/mutants", "root": ".", "shard": "0/2"}]'
            ),
        }
        for case, text in cases.items():
            with self.subTest(case=case), self.assertRaises(ValueError):
                shards.expand_shards(text, 2)
        for count in (0, 9):
            with self.subTest(count=count), self.assertRaises(ValueError):
                shards.expand_shards("[]", count)

    def test_expand_cli_takes_the_matrix_on_stdin(self) -> None:
        completed = subprocess.run(
            [sys.executable, str(SHARDS), "expand", "2"],
            input=json.dumps([{"gate": "commands/mutants", "root": "."}]),
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(len(completed.stdout.splitlines()), 1)
        self.assertEqual(
            json.loads(completed.stdout),
            [
                {"gate": "commands/mutants", "root": ".", "shard": "0/2"},
                {"gate": "commands/mutants", "root": ".", "shard": "1/2"},
            ],
        )
        refused = subprocess.run(
            [sys.executable, str(SHARDS), "expand", "2"],
            input="{}",
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(refused.returncode, 1)
        self.assertEqual(len(refused.stderr.splitlines()), 1)


class KaniTests(ReportCase):
    def test_another_project_needs_only_consistent_complete_results(self) -> None:
        self.write("kani", kani_report(["proofs::holds"], crate="other"))
        summary = reports.validate("commands/kani", self.root, False)
        self.assertEqual(
            summary, "1 harnesses verified; no atelier floor (not atelier's own workflow)"
        )
        self.write("kani", kani_report([], crate="other"))
        with self.assertRaises(ValueError):
            reports.validate("kani", self.root, False)
        # In atelier's own CI the core proofs are required.
        self.write("kani", kani_report(["proofs::holds"], crate="other"))
        with self.assertRaises(ValueError):
            reports.validate("kani", self.root, True)

    def test_the_summary_names_atelier_floor(self) -> None:
        self.write("kani", kani_report())
        summary = reports.validate("commands/kani", self.root, True)
        self.assertEqual(summary, "2 harnesses verified; atelier floor: its 2 core proofs required")

    def test_atelier_requires_both_core_proofs(self) -> None:
        names = sorted(reports.ATELIER_HARNESSES)
        self.rejected("kani", kani_report(names[:1]))
        self.write("kani", kani_report([*names, "machine::proofs::another"]))
        reports.validate("kani", self.root, True)

    def test_summary_must_be_complete_and_consistent(self) -> None:
        for field, value in (
            ("status", "incomplete"),
            ("status", "failed"),
            ("executed", 1),
            ("successful", 1),
            ("total_harnesses", 0),
            ("total_harnesses", 3),
            ("failed", 1),
            ("failed", False),
            ("executed", 2.0),
        ):
            with self.subTest(field=field, value=value):
                report = kani_report()
                report["verification_results"]["summary"][field] = value
                self.rejected("kani", report)

    def test_missing_fields_fail(self) -> None:
        good = kani_report()
        for field in good:
            with self.subTest(field=field):
                report = copy.deepcopy(good)
                del report[field]
                self.rejected("kani", report)

    def test_every_array_must_cover_actual_harnesses_once(self) -> None:
        for key in ("harness_metadata", "error_details", "property_details", "cbmc"):
            for mode in ("empty", "partial", "duplicate", "unrelated"):
                with self.subTest(key=key, mode=mode):
                    report = kani_report()
                    if mode == "empty":
                        report[key] = []
                    elif mode == "partial":
                        report[key].pop()
                    elif mode == "duplicate":
                        report[key][1] = copy.deepcopy(report[key][0])
                    else:
                        identity = "harness_id"
                        if key == "harness_metadata":
                            identity = "pretty_name"
                        report[key][0][identity] = "unrelated::proof"
                    self.rejected("kani", report)

    def test_results_require_both_real_harnesses_and_success(self) -> None:
        for mode in ("empty", "partial", "duplicate", "unrelated", "failure"):
            with self.subTest(mode=mode):
                report = kani_report()
                results = report["verification_results"]["results"]
                if mode == "empty":
                    results.clear()
                elif mode == "partial":
                    results.pop()
                elif mode == "duplicate":
                    results[1] = copy.deepcopy(results[0])
                elif mode == "unrelated":
                    results[0]["harness_id"] = "a_run_follows_what_rungs_answers"
                else:
                    results[0]["status"] = "Failure"
                self.rejected("kani", report)

    def test_checks_cannot_be_empty_failed_or_unknown(self) -> None:
        for checks in (
            [],
            [{"id": 1, "status": "Failure"}],
            [{"id": 1, "status": "Undetermined"}],
            [{"id": 1, "status": "Error"}],
            [{"id": 1, "status": "Unexpected"}],
            [{"id": 2, "status": "Success"}],
        ):
            with self.subTest(checks=checks):
                report = kani_report()
                report["verification_results"]["results"][0]["checks"] = checks
                self.rejected("kani", report)

    def test_unreachable_checks_are_accounted_for(self) -> None:
        report = kani_report()
        report["verification_results"]["results"][0]["checks"].append(
            {"id": 2, "status": "Unreachable"}
        )
        properties = report["property_details"][0]["property_details"]
        properties["total_properties"] = 2
        properties["unreachable"] = 1
        self.write("kani", report)
        reports.validate("kani", self.root, True)

    def test_property_counts_and_error_details_fail_closed(self) -> None:
        for field, value in (
            ("total_properties", 0),
            ("passed", 0),
            ("failed", 1),
            ("undetermined", 1),
            ("solver_error", 1),
            ("passed", None),
            ("error", "extraction failed"),
        ):
            with self.subTest(field=field, value=value):
                report = kani_report()
                report["property_details"][0]["property_details"][field] = value
                self.rejected("kani", report)
        report = kani_report()
        report["error_details"][0]["has_errors"] = True
        self.rejected("kani", report)

    def test_wrong_crate_version_and_panicking_harness_fail(self) -> None:
        report = kani_report()
        report["metadata"]["kani_version"] = "0.67.0"
        self.rejected("kani", report)
        report = kani_report()
        report["harness_metadata"][0]["crate_name"] = "other"
        self.rejected("kani", report)
        report = kani_report()
        report["harness_metadata"][0]["attributes"]["should_panic"] = True
        self.rejected("kani", report)


class MutantsTests(ReportCase):
    def test_zero_mutants_is_not_a_successful_diff(self) -> None:
        report = mutants_report()
        report.update(total_mutants=0, caught=0, unviable=0)
        report["outcomes"] = report["outcomes"][:1]
        self.rejected("mutants", report)

    def test_empty_partial_duplicate_and_unknown_outcomes_fail(self) -> None:
        for mode in ("empty", "partial", "duplicate", "unknown", "no-baseline"):
            with self.subTest(mode=mode):
                report = mutants_report()
                outcomes = report["outcomes"]
                if mode == "empty":
                    outcomes.clear()
                elif mode == "partial":
                    outcomes.pop()
                elif mode == "duplicate":
                    outcomes[2] = copy.deepcopy(outcomes[1])
                elif mode == "unknown":
                    outcomes[1]["scenario"] = "unknown"
                else:
                    outcomes[0] = copy.deepcopy(outcomes[1])
                    outcomes[0]["scenario"]["Mutant"]["name"] = "src/lib.rs:3: other"
                    report["caught"] = 2
                self.rejected("mutants", report)

    def test_missing_fields_and_unfinished_reports_fail(self) -> None:
        for field in mutants_report():
            with self.subTest(field=field):
                report = mutants_report()
                del report[field]
                self.rejected("mutants", report)
        for end in (None, "", "invalid", "2026-11-12T00:00:00Z"):
            with self.subTest(end=end):
                report = mutants_report()
                report["end_time"] = end
                self.rejected("mutants", report)

    def test_counters_must_match_and_have_integer_types(self) -> None:
        for field in ("total_mutants", "caught", "unviable", "missed", "timeout", "success"):
            for value in (-1, False, "0", None, 99):
                with self.subTest(field=field, value=value):
                    report = mutants_report()
                    report[field] = value
                    self.rejected("mutants", report)

    def test_missed_timeouts_and_failures_are_rejected(self) -> None:
        for summary in ("MissedMutant", "Timeout", "Failure", "Success", "unknown"):
            with self.subTest(summary=summary):
                report = mutants_report()
                report["outcomes"][1]["summary"] = summary
                self.rejected("mutants", report)

    def test_failed_baseline_is_rejected(self) -> None:
        report = mutants_report()
        report["outcomes"][0]["summary"] = "Failure"
        self.rejected("mutants", report)
        report = mutants_report()
        report["outcomes"][0]["phase_results"] = phases({"Failure": 100})
        self.rejected("mutants", report)

    def test_incomplete_or_contradictory_phase_evidence_fails(self) -> None:
        for index, phase_results in (
            (0, []),
            (0, [{"phase": "Check", "process_status": "Success"}]),
            (1, phases()),
            (1, phases("Timeout")),
            (1, phases({"Failure": 0})),
            (1, phases({"Signalled": 9})),
            (2, phases("Success", unviable=True)),
            (2, []),
        ):
            with self.subTest(index=index, phases=phase_results):
                report = mutants_report()
                report["outcomes"][index]["phase_results"] = phase_results
                self.rejected("mutants", report)

    def test_unviable_mutants_are_accepted(self) -> None:
        report = mutants_report()
        report["outcomes"].pop(1)
        report.update(total_mutants=1, caught=0)
        self.write("mutants", report)
        reports.validate("mutants", self.root, True)


def canary_report(names: list[str] | None = None) -> dict:
    return mutants_report(names, file=reports.CANARY)


def canary_selection(**changes: object) -> dict:
    selection = {
        "mode": "canary",
        "diff_selected": 0,
        "changed_files": ["docs/a.md"],
        "canary": reports.CANARY,
        "selected": diff_selection(canary_report())["selected"],
        "reason": "the diff against abc changes 1 files and selects no mutant",
    }
    return {**selection, **changes}


class SelectionTests(ReportCase):
    def test_diff_selection_names_its_files_count_and_mutants(self) -> None:
        self.write("mutants", mutants_report())
        summary = reports.validate("commands/mutants", self.root, True)
        expected = "diff changed 1 repository files, selected 2 mutants; canary 0 mutants (not run)"
        self.assertEqual(summary, expected)
        good = diff_selection(mutants_report())
        cases = {
            "count": {**good, "diff_selected": 3},
            "names": {**good, "selected": ["src/lib.rs:9: other", good["selected"][1]]},
            "empty": {**good, "diff_selected": 0, "selected": []},
            "no files": {**good, "changed_files": []},
            "unevaluated": {**good, "diff_selected": None, "changed_files": None},
            "bad file": {**good, "changed_files": [""]},
        }
        for case, selection in cases.items():
            with self.subTest(case=case):
                self.write("mutants", mutants_report(), selection)
                with self.assertRaises(ValueError):
                    reports.validate("mutants", self.root, True)

    def test_missing_or_unknown_selection_fails(self) -> None:
        self.write("mutants", mutants_report())
        (self.root / "mutants-selection.json").unlink()
        with self.assertRaises(ValueError):
            reports.validate("mutants", self.root, True)
        self.write("mutants", mutants_report(), {"mode": "whole"})
        with self.assertRaises(ValueError):
            reports.validate("mutants", self.root, True)

    def test_canary_never_reads_as_diff_coverage(self) -> None:
        self.write("mutants", canary_report(), canary_selection())
        summary = reports.validate("mutants", self.root, True)
        head = "diff changed 1 repository files, selected 0 mutants; canary 2 mutants"
        self.assertTrue(summary.startswith(head), summary)
        self.assertIn(reports.CANARY, summary)
        self.assertIn("selects no mutant", summary)

    def test_canary_must_be_complete_confined_caught_and_short(self) -> None:
        unviable = canary_report()
        unviable["outcomes"][1] = copy.deepcopy(unviable["outcomes"][2])
        unviable["outcomes"][1]["scenario"]["Mutant"]["name"] = "src/lib.rs:1: replace true"
        unviable.update(caught=0, unviable=2)
        long = "x" * (reports.MAX_REASON + 1)
        cases = {
            "other file": (mutants_report(), canary_selection()),
            "none selected": (canary_report(), canary_selection(selected=[])),
            "names differ": (canary_report(), canary_selection(selected=["x", "y"])),
            "no reason": (canary_report(), canary_selection(reason=" ")),
            "long reason": (canary_report(), canary_selection(reason=long)),
            "two lines": (canary_report(), canary_selection(reason="a\nb")),
            "with a diff": (canary_report(), canary_selection(diff_selected=1)),
            "half evaluated": (canary_report(), canary_selection(changed_files=None)),
            "moved": (canary_report(), canary_selection(canary="crates/x.rs")),
            "all unviable": (unviable, canary_selection()),
        }
        for case, (report, selection) in cases.items():
            with self.subTest(case=case):
                self.write("mutants", report, selection)
                with self.assertRaises(ValueError):
                    reports.validate("mutants", self.root, True)

    def test_canary_is_atelier_only(self) -> None:
        self.write("mutants", canary_report(), canary_selection())
        with self.assertRaises(ValueError):
            reports.validate("mutants", self.root, False)

    def test_an_unavailable_base_allows_only_an_unevaluated_canary(self) -> None:
        (self.root / "base.txt").write_text("base 000 is unavailable\n", encoding="utf-8")
        unevaluated = {"diff_selected": None, "changed_files": None}
        self.write("mutants", mutants_report())
        with self.assertRaises(ValueError):
            reports.validate("mutants", self.root, True)
        self.write("mutants", canary_report(), canary_selection())
        with self.assertRaises(ValueError):
            reports.validate("mutants", self.root, True)
        selection = canary_selection(reason="base 000 is unavailable", **unevaluated)
        self.write("mutants", canary_report(), selection)
        summary = reports.validate("mutants", self.root, True)
        self.assertTrue(summary.startswith("diff not evaluated, selected 0 mutants"), summary)
        # Without the note, an unevaluated diff is refused.
        (self.root / "base.txt").unlink()
        with self.assertRaises(ValueError):
            reports.validate("mutants", self.root, True)

    def test_summary_through_cli(self) -> None:
        self.write("mutants", canary_report(), canary_selection())
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            code = reports.main(["commands/mutants", str(self.root), "--atelier"])
        self.assertEqual(code, 0)
        self.assertIn("**commands/mutants:** diff changed 1 repository files", stdout.getvalue())


class ShardReportTests(ReportCase):
    def test_a_shard_report_is_named_as_a_slice(self) -> None:
        names = shard_names(8, 1, 4)
        selection = sharded_selection(names, 1, 4, 8)
        self.write("mutants", mutants_report(names), selection)
        summary = reports.validate("commands/mutants", self.root, True)
        self.assertEqual(
            summary,
            "shard 1/4 of 8 mutants: diff changed 1 repository files, "
            "selected 8 mutants; this shard tests 2",
        )

    def test_a_shard_must_test_exactly_its_slice(self) -> None:
        names = shard_names(8, 0, 4)
        cases = {
            "short": sharded_selection(names[:1], 0, 4, 8),
            "long": sharded_selection([*names, "src/lib.rs:9: other"], 0, 4, 8),
            "diff count": sharded_selection(names, 0, 4, 8, diff_selected=7),
            "no total": sharded_selection(names, 0, 4, 0, diff_selected=0),
            "bad count": sharded_selection(names, 0, 9, 8),
        }
        for case, selection in cases.items():
            with self.subTest(case=case):
                report = mutants_report(selection["selected"])
                self.write("mutants", report, selection)
                with self.assertRaises(ValueError):
                    reports.validate("mutants", self.root, True)

    def test_an_empty_slice_is_admitted_only_when_sharded(self) -> None:
        empty: list[str] = []
        selection = sharded_selection(empty, 3, 4, 3)
        self.write("mutants", mutants_report(empty), selection)
        summary = reports.validate("commands/mutants", self.root, True)
        self.assertIn("this shard tests 0", summary)
        self.write("mutants", mutants_report(empty))
        with self.assertRaises(ValueError):
            reports.validate("commands/mutants", self.root, True)


class SafeguardReportTests(ReportCase):
    def safeguard(
        self,
        names: list[str],
        total: int | None = None,
        deleted: list[str] | None = None,
        report: dict | None = None,
        **changes: object,
    ) -> dict:
        """A safeguard selection and the report its second run wrote."""
        total = len(names) if total is None else total
        deleted = ["crates/x/src/lib.rs::test:gone"] if deleted is None else deleted
        selection = {
            "mode": "safeguard",
            "total": total,
            "selected": names,
            "deleted": deleted,
            "files": ["crates/x/src/lib.rs"],
            **changes,
        }
        (self.root / "mutants-safeguard.json").write_text(
            json.dumps(selection), encoding="utf-8"
        )
        outcomes = self.root / "mutants-safeguard" / "mutants.out" / "outcomes.json"
        outcomes.parent.mkdir(parents=True, exist_ok=True)
        if report is None:
            report = mutants_report(names, file="crates/x/src/lib.rs")
        outcomes.write_text(json.dumps(report), encoding="utf-8")
        return selection

    def test_a_good_safeguard_extends_the_summary(self) -> None:
        self.write("mutants", mutants_report())
        self.safeguard(["src/lib.rs:9: replace return", "src/lib.rs:10: other"])
        summary = reports.validate("commands/mutants", self.root, True)
        self.assertEqual(
            summary,
            "diff changed 1 repository files, selected 2 mutants; "
            "canary 0 mutants (not run); safeguard 2 mutants",
        )

    def test_a_sharded_safeguard_names_its_shard_count(self) -> None:
        names = shard_names(8, 1, 4)
        selection = sharded_selection(names, 1, 4, 8)
        self.write("mutants", mutants_report(names), selection)
        self.safeguard(shard_names(6, 1, 4), total=6, shard="1/4")
        summary = reports.validate("commands/mutants", self.root, True)
        self.assertTrue(summary.endswith("; safeguard 4 shards, 6 mutants"), summary)

    def test_a_safeguard_must_test_exactly_its_selection(self) -> None:
        self.write("mutants", mutants_report())
        self.safeguard(
            ["src/lib.rs:9: replace return", "src/lib.rs:10: other"],
            report=mutants_report(
                ["src/lib.rs:9: other"], file="crates/x/src/lib.rs"
            ),
        )
        with self.assertRaisesRegex(ValueError, "differ from the safeguard's"):
            reports.validate("commands/mutants", self.root, True)

    def test_a_safeguard_without_its_report_is_refused(self) -> None:
        self.write("mutants", mutants_report())
        self.safeguard(["src/lib.rs:9: replace return"])
        outcomes = self.root / "mutants-safeguard" / "mutants.out" / "outcomes.json"
        outcomes.unlink()
        with self.assertRaisesRegex(ValueError, "missing regular report"):
            reports.validate("commands/mutants", self.root, True)

    def test_a_safeguard_without_deleted_tests_is_refused(self) -> None:
        self.write("mutants", mutants_report())
        self.safeguard(["src/lib.rs:9: replace return"], deleted=[])
        with self.assertRaisesRegex(ValueError, "recorded no deleted test"):
            reports.validate("commands/mutants", self.root, True)


class PartitionTests(ReportCase):
    def shard(
        self,
        index: int,
        names: list[str],
        total: int,
        count: int,
        label: str | None = None,
        report: dict | None = None,
        **changes: object,
    ) -> Path:
        """One shard's artifact folder, with its own selection and report."""
        container = self.root / (label or f"reports-{index}") / "container"
        (container / "mutants.out").mkdir(parents=True, exist_ok=True)
        report = report or mutants_report(names)
        (container / "mutants.out" / "outcomes.json").write_text(
            json.dumps(report), encoding="utf-8"
        )
        selection = {
            "mode": "diff",
            "gate": "commands/mutants",
            "root": ".",
            "shard": f"{index}/{count}",
            "total": total,
            "diff_selected": total,
            "changed_files": ["src/lib.rs"],
            "selected": names,
            **changes,
        }
        (container / "mutants-selection.json").write_text(
            json.dumps(selection), encoding="utf-8"
        )
        return container

    def partition(self, total: int = 8, count: int = 4) -> None:
        for index in range(count):
            names = shard_names(total, index, count)
            self.shard(index, names, total, count)

    def test_one_complete_partition_passes_and_prints_its_line(self) -> None:
        self.partition()
        summary = reports.validate_partition(4, self.root)
        self.assertEqual(summary, "4 shards, 8 mutants, 0 missed, 0 timeouts")
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            code = reports.main(["--partition", "4", str(self.root)])
        self.assertEqual(code, 0)
        line = stdout.getvalue()
        self.assertTrue(line.startswith("**partition:** "), line)

    def test_a_missing_shard_is_named(self) -> None:
        for index in (1, 2, 3):
            self.shard(index, shard_names(8, index, 4), 8, 4)
        with self.assertRaisesRegex(ValueError, r"shards \[1, 2, 3\]"):
            reports.validate_partition(4, self.root)

    def test_a_duplicate_shard_index_is_refused(self) -> None:
        self.partition()
        self.shard(0, shard_names(8, 0, 4), 8, 4, label="reports-0-copy")
        with self.assertRaisesRegex(ValueError, r"shards \[0, 0, 1, 2, 3\]"):
            reports.validate_partition(4, self.root)

    def test_the_collected_shard_count_must_be_the_recorded_one(self) -> None:
        self.partition()
        with self.assertRaisesRegex(ValueError, r"shard \d/4, not of 3"):
            reports.validate_partition(3, self.root)

    def test_shards_must_agree_on_the_whole_total(self) -> None:
        for index in range(4):
            total = 8 if index < 3 else 9
            self.shard(index, shard_names(total, index, 4), total, 4)
        with self.assertRaisesRegex(ValueError, r"\[8, 9\]"):
            reports.validate_partition(4, self.root)

    def test_two_shards_cannot_test_the_same_mutant(self) -> None:
        for index in range(4):
            names = shard_names(8, index, 4)
            if index == 1:
                names = [shard_names(8, 0, 4)[0], names[1]]
            self.shard(index, names, 8, 4)
        with self.assertRaisesRegex(ValueError, "tests a mutant twice"):
            reports.validate_partition(4, self.root)

    def test_a_canary_partition_needs_one_caught_mutant(self) -> None:
        for index in range(4):
            names = shard_names(6, index, 4)
            report = canary_report(names)
            for outcome in report["outcomes"][1:]:
                outcome["summary"] = "Unviable"
                outcome["phase_results"] = phases({"Failure": 100}, unviable=True)
            report.update(caught=0, unviable=len(names))
            self.shard(
                index,
                names,
                6,
                4,
                report=report,
                mode="canary",
                diff_selected=0,
                changed_files=["docs/a.md"],
                canary=reports.CANARY,
                reason="the diff against abc changes 1 files and selects no mutant",
            )
        with self.assertRaisesRegex(ValueError, "caught no canary mutant"):
            reports.validate_partition(4, self.root)

    def test_a_caught_canary_partition_passes(self) -> None:
        for index in range(4):
            names = shard_names(6, index, 4)
            self.shard(
                index,
                names,
                6,
                4,
                report=canary_report(names),
                mode="canary",
                diff_selected=0,
                changed_files=["docs/a.md"],
                canary=reports.CANARY,
                reason="the diff against abc changes 1 files and selects no mutant",
            )
        summary = reports.validate_partition(4, self.root)
        self.assertEqual(summary, "4 shards, 6 mutants, 0 missed, 0 timeouts")

    def safeguard(
        self,
        container: Path,
        index: int,
        total: int,
        count: int,
        deleted: list[str] | None = None,
    ) -> None:
        """Attach a safeguard selection and its own report to a shard."""
        names = shard_names(total, index, count)
        outcomes = container / "mutants-safeguard" / "mutants.out"
        outcomes.mkdir(parents=True, exist_ok=True)
        report = mutants_report(names, file="crates/x/src/lib.rs")
        (outcomes / "outcomes.json").write_text(
            json.dumps(report), encoding="utf-8"
        )
        selection = {
            "mode": "safeguard",
            "gate": "commands/mutants",
            "root": ".",
            "shard": f"{index}/{count}",
            "total": total,
            "selected": names,
            "deleted": (
                ["crates/x/src/lib.rs::test:gone"] if deleted is None else deleted
            ),
            "files": ["crates/x/src/lib.rs"],
        }
        (container / "mutants-safeguard.json").write_text(
            json.dumps(selection), encoding="utf-8"
        )

    def test_a_safeguard_partition_is_proved(self) -> None:
        for index in range(4):
            container = self.shard(index, shard_names(8, index, 4), 8, 4)
            self.safeguard(container, index, 8, 4)
        summary = reports.validate_partition(4, self.root)
        self.assertEqual(
            summary,
            "4 shards, 8 mutants, 0 missed, 0 timeouts; "
            "safeguard 4 shards, 8 mutants",
        )

    def test_a_missing_safeguard_shard_is_refused(self) -> None:
        for index in range(4):
            container = self.shard(index, shard_names(8, index, 4), 8, 4)
            if index != 2:
                self.safeguard(container, index, 8, 4)
        with self.assertRaisesRegex(ValueError, "has 3 safeguards, not 4"):
            reports.validate_partition(4, self.root)

    def test_a_shard_directory_without_its_selection_is_refused(self) -> None:
        container = self.root / "reports-empty" / "container"
        container.mkdir(parents=True)
        with self.assertRaisesRegex(ValueError, "missing regular report"):
            reports.validate_partition(4, self.root)

    def test_the_partition_cli_is_isolated_and_names_the_summary(self) -> None:
        self.partition()
        completed = subprocess.run(
            [sys.executable, "-I", str(SCRIPT), "--partition", "4", str(self.root)],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertTrue(
            completed.stdout.startswith("**partition:** 4 shards, 8 mutants"),
            completed.stdout,
        )


class WrapperTests(unittest.TestCase):
    """cargo.py's choice of mutants, with cargo-mutants and git stubbed."""

    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / "reports").mkdir()
        names = (
            "REPORTS",
            "listed",
            "changed_files",
            "CARGO",
            "GIT",
            "LIMIT_SECS",
            "SHARD",
            "blob",
        )
        saved = {name: getattr(cargo, name) for name in names}
        environment = {
            name: os.environ.get(name)
            for name in ("FLOOR", "ATELIER_BASE", "GATE", "ROOT")
        }
        self.addCleanup(self.restore, saved, environment, Path.cwd())
        cargo.REPORTS = self.root / "reports"
        cargo.changed_files = lambda: ["docs/a.md"]
        cargo.SHARD = ""
        os.environ["FLOOR"] = "atelier"
        os.environ["ATELIER_BASE"] = "abc"
        for name in ("GATE", "ROOT"):
            os.environ.pop(name, None)
        os.chdir(self.root)

    @staticmethod
    def restore(saved: dict, environment: dict, cwd: Path) -> None:
        for name, value in saved.items():
            setattr(cargo, name, value)
        for name, value in environment.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        os.chdir(cwd)

    def canary(self) -> None:
        canary = self.root / reports.CANARY
        canary.parent.mkdir(parents=True)
        canary.write_text("", encoding="utf-8")

    def selection(self) -> dict:
        path = self.root / "reports" / "mutants-selection.json"
        return json.loads(path.read_text(encoding="utf-8"))

    def safeguard_selection(self) -> dict:
        path = self.root / "reports" / "mutants-safeguard.json"
        return json.loads(path.read_text(encoding="utf-8"))

    def deleted(self, file: str, old: str, new: str | None = None) -> None:
        """A changed file that lost the tests `old` held."""
        cargo.changed_files = lambda: [file]
        cargo.blob = lambda base, path: old if path == file else None
        if new is not None:
            path = self.root / file
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(new, encoding="utf-8")

    def script(self, name: str, body: str) -> str:
        path = self.root / name
        path.write_text(f"#!/usr/bin/python3\n{body}\n", encoding="utf-8")
        path.chmod(0o755)
        return str(path)

    def test_listing_keeps_test_arguments_last(self) -> None:
        # The real listed(), against a cargo that echoes its arguments.
        cargo.listed = WrapperTests.real_listed
        cargo.CARGO = self.script(
            "cargo", 'import json, sys; print(json.dumps([{"name": " ".join(sys.argv[1:])}]))'
        )
        names = cargo.listed(["mutants", "--in-diff", "d", "--", "-q"])
        self.assertEqual(names, [{"name": "mutants --in-diff d --list --json -- -q"}])
        cargo.CARGO = self.script("failing", "import sys; sys.exit(3)")
        with self.assertRaises(ValueError):
            cargo.listed(["mutants"])

    def test_the_diff_must_be_made_before_an_empty_selection_counts(self) -> None:
        cargo.changed_files = WrapperTests.real_changed_files
        os.environ.pop("ATELIER_BASE")
        with self.assertRaises(ValueError):
            cargo.changed_files()
        os.environ["ATELIER_BASE"] = "abc"
        cargo.GIT = self.script(
            "git", 'import sys; print("warning", file=sys.stderr); print("fatal: bad", file=sys.stderr); sys.exit(128)'
        )
        with self.assertRaisesRegex(ValueError, r"^git diff against abc failed: fatal: bad$"):
            cargo.changed_files()
        cargo.GIT = self.script(
            "git", 'import sys; assert sys.argv[-1] == "--", sys.argv; print("b.rs"); print("a.md")'
        )
        self.assertEqual(cargo.changed_files(), ["a.md", "b.rs"])

    def test_a_failed_diff_never_reaches_the_canary(self) -> None:
        self.canary()

        def fail() -> list[str]:
            raise ValueError("git diff against abc failed: fatal: bad")

        cargo.changed_files = fail
        cargo.listed = lambda args: [{"name": "c"}] if "--file" in args else []

        with self.assertRaisesRegex(
            ValueError, r"^git diff against abc failed: fatal: bad$"
        ):
            cargo.arguments(["mutants", "--in-diff", "d"])

        self.assertFalse((self.root / "reports" / "mutants-selection.json").exists())

    def test_a_diff_with_mutants_runs_as_configured(self) -> None:
        cargo.listed = lambda args: [{"name": "b"}, {"name": "a"}]
        args = ["mutants", "--in-diff", "d", "--jobs", "3"]
        output = str(self.root / "reports")
        self.assertEqual(cargo.arguments(args), [[*args, "--output", output]])
        expected = {
            "mode": "diff",
            "total": 2,
            "diff_selected": 2,
            "changed_files": ["docs/a.md"],
            "selected": ["a", "b"],
        }
        self.assertEqual(self.selection(), expected)

    def test_a_shard_records_and_runs_its_slice(self) -> None:
        cargo.SHARD = "1/4"
        os.environ["GATE"] = "commands/mutants"
        os.environ["ROOT"] = "."
        whole = [{"name": name} for name in "abcd"]
        one = [{"name": "b"}]

        def listed(args: list[str]) -> list:
            return one if "--shard" in args else whole

        cargo.listed = listed
        args = ["mutants", "--in-diff", "d", "--jobs", "3"]
        output = str(self.root / "reports")
        runs = cargo.arguments(args)
        expected = [
            "mutants",
            "--in-diff",
            "d",
            "--jobs",
            "3",
            "--shard",
            "1/4",
            "--output",
            output,
        ]
        self.assertEqual(runs, [expected])
        self.assertEqual(runs[0].count("--shard"), 1)
        selection = self.selection()
        self.assertEqual(selection["gate"], "commands/mutants")
        self.assertEqual(selection["root"], ".")
        self.assertEqual(selection["shard"], "1/4")
        self.assertEqual(selection["total"], 4)
        self.assertEqual(selection["diff_selected"], 4)
        self.assertEqual(selection["selected"], ["b"])

    def test_a_deleted_inline_test_is_still_mutated(self) -> None:
        file = "crates/x/src/lib.rs"
        old = "#[test]\nfn gone() {\n    assert!(true);\n}\n"
        calls = []

        def show(base: str, path: str) -> str | None:
            calls.append((base, path))
            return old

        cargo.changed_files = lambda: [file]
        cargo.blob = show
        path = self.root / file
        path.parent.mkdir(parents=True)
        path.write_text("pub fn kept() {}\n", encoding="utf-8")

        def listed(args: list[str]) -> list:
            return [{"name": "m"}] if "--file" in args else [{"name": "d"}]

        cargo.listed = listed
        runs = cargo.arguments(["mutants", "--in-diff", "d"])
        output = str(self.root / "reports")
        self.assertEqual(runs[0], ["mutants", "--in-diff", "d", "--output", output])
        self.assertEqual(
            runs[1],
            [
                "mutants",
                "--file",
                file,
                "--output",
                str(self.root / "reports" / "mutants-safeguard"),
            ],
        )
        self.assertEqual(calls, [("abc", file)])
        selection = self.safeguard_selection()
        self.assertEqual(selection["mode"], "safeguard")
        self.assertEqual(selection["deleted"], [f"{file}::test:gone"])
        self.assertEqual(selection["files"], [file])
        self.assertEqual(selection["total"], 1)
        self.assertEqual(selection["selected"], ["m"])

    def test_a_deleted_test_file_covers_its_crate_sources(self) -> None:
        file = "crates/x/tests/it.rs"
        sources = ["crates/x/src/lib.rs", "crates/x/src/nested/mod.rs"]
        for name in ("lib.rs", "nested/mod.rs"):
            path = self.root / "crates/x/src" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("", encoding="utf-8")
        self.deleted(file, "#[test]\nfn gone() {}\n")

        def listed(args: list[str]) -> list:
            return [{"name": "m"}] if "--file" in args else [{"name": "d"}]

        cargo.listed = listed
        runs = cargo.arguments(["mutants", "--in-diff", "d"])
        self.assertEqual(
            runs[1],
            [
                "mutants",
                "--file",
                "crates/x/src/lib.rs",
                "--file",
                "crates/x/src/nested/mod.rs",
                "--output",
                str(self.root / "reports" / "mutants-safeguard"),
            ],
        )
        self.assertEqual(self.safeguard_selection()["files"], sources)

    def test_kept_tests_need_no_safeguard(self) -> None:
        file = "crates/x/src/lib.rs"
        text = "#[test]\nfn kept() {}\n"
        self.deleted(file, text, text)
        cargo.listed = lambda args: [{"name": "d"}]
        runs = cargo.arguments(["mutants", "--in-diff", "d"])
        self.assertEqual(len(runs), 1)
        self.assertFalse((self.root / "reports" / "mutants-safeguard.json").exists())

    def test_a_deleted_test_and_shard_reach_both_runs(self) -> None:
        cargo.SHARD = "1/4"
        file = "crates/x/src/lib.rs"
        self.deleted(file, "#[test]\nfn gone() {}\n", "")
        whole = [{"name": name} for name in "abcd"]
        sliced = [{"name": "b"}]
        guard_whole = [{"name": f"g{index}"} for index in range(8)]
        guard_sliced = guard_whole[2:4]

        def listed(args: list[str]) -> list:
            if "--file" in args:
                return guard_sliced if "--shard" in args else guard_whole
            return sliced if "--shard" in args else whole

        cargo.listed = listed
        runs = cargo.arguments(["mutants", "--in-diff", "d"])
        self.assertEqual(runs[0].count("--shard"), 1)
        self.assertEqual(runs[1].count("--shard"), 1)
        self.assertIn("1/4", runs[0])
        self.assertIn("1/4", runs[1])
        selection = self.selection()
        self.assertEqual(selection["shard"], "1/4")
        self.assertEqual(selection["total"], 4)
        self.assertEqual(selection["selected"], ["b"])
        guard = self.safeguard_selection()
        self.assertEqual(guard["shard"], "1/4")
        self.assertEqual(guard["total"], 8)
        self.assertEqual(guard["selected"], ["g2", "g3"])

    def test_deleted_tests_without_a_production_file_are_refused(self) -> None:
        self.deleted("crates/x/tests/it.rs", "#[test]\nfn gone() {}\n")
        cargo.listed = lambda args: [{"name": "d"}]
        with self.assertRaisesRegex(ValueError, "cover no production file"):
            cargo.arguments(["mutants", "--in-diff", "d"])

    def test_more_than_64_covered_files_is_refused(self) -> None:
        self.deleted("crates/x/tests/it.rs", "#[test]\nfn gone() {}\n")
        cargo.listed = lambda args: [{"name": "d"}]
        for index in range(65):
            path = self.root / "crates/x/src" / f"file{index}.rs"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "more than 64"):
            cargo.arguments(["mutants", "--in-diff", "d"])

    def test_deleted_tests_covering_no_mutant_are_refused(self) -> None:
        file = "crates/x/src/lib.rs"
        self.deleted(file, "#[test]\nfn gone() {}\n", "")
        cargo.listed = lambda args: [] if "--file" in args else [{"name": "d"}]
        with self.assertRaisesRegex(ValueError, "cover no mutant there"):
            cargo.arguments(["mutants", "--in-diff", "d"])

    def test_an_empty_diff_mutates_the_canary_and_says_why(self) -> None:
        self.canary()
        cargo.listed = lambda args: [{"name": "c"}] if "--file" in args else []
        args = ["mutants", "--in-diff", "d", "--jobs", "3", "--", "-q"]
        runs = cargo.arguments(args)
        output = str(self.root / "reports")
        expected = ["mutants", "--jobs", "3", "--file", reports.CANARY, "--output", output]
        self.assertEqual(runs, [[*expected, "--", "-q"]])
        selection = self.selection()
        self.assertEqual(selection["mode"], "canary")
        self.assertEqual(selection["total"], 1)
        self.assertEqual(selection["diff_selected"], 0)
        self.assertEqual(selection["changed_files"], ["docs/a.md"])
        self.assertEqual(selection["selected"], ["c"])
        self.assertIn("changes 1 repository files and selects no mutant", selection["reason"])

    def test_an_unavailable_base_goes_straight_to_the_canary(self) -> None:
        self.canary()
        (self.root / "reports" / "base.txt").write_text("base 000 is unavailable\n")
        # A diff against a moved origin/main must not stand in for this change.
        cargo.listed = lambda args: [{"name": "c"}] if "--file" in args else [{"name": "x"}]
        for spelling in (["--in-diff", "d"], ["-D", "d"], ["--in-diff=d"]):
            with self.subTest(spelling=spelling):
                cargo.arguments(["mutants", *spelling])
                selection = self.selection()
                self.assertEqual(selection["mode"], "canary")
                self.assertIsNone(selection["diff_selected"])
                self.assertIsNone(selection["changed_files"])
                self.assertEqual(selection["reason"], "base 000 is unavailable")

    def test_a_missing_empty_or_foreign_canary_fails(self) -> None:
        cargo.listed = lambda args: []
        with self.assertRaises(ValueError):
            cargo.arguments(["mutants", "--in-diff", "d"])
        self.canary()
        with self.assertRaises(ValueError):
            cargo.arguments(["mutants", "--in-diff", "d"])
        cargo.listed = lambda args: [{"name": "c"}] if "--file" in args else []
        os.environ["FLOOR"] = ""
        with self.assertRaises(ValueError):
            cargo.arguments(["mutants", "--in-diff", "d"])
        self.assertFalse((self.root / "reports" / "mutants-selection.json").exists())

    def test_a_refusal_is_one_line_and_fails(self) -> None:
        cargo.listed = lambda args: []
        stderr = io.StringIO()
        saved = sys.argv
        self.addCleanup(setattr, sys, "argv", saved)
        sys.argv = ["cargo", "mutants", "--in-diff", "d"]
        with contextlib.redirect_stderr(stderr):
            self.assertEqual(cargo.main(), 1)
        self.assertEqual(len(stderr.getvalue().splitlines()), 1, stderr.getvalue())
        self.assertTrue(stderr.getvalue().startswith("atelier CI: "))

    def test_a_cargo_past_its_limit_is_one_line_and_fails(self) -> None:
        cargo.LIMIT_SECS = 1
        cargo.CARGO = self.script("slow", "import time; time.sleep(30)")
        stderr = io.StringIO()
        saved = sys.argv
        self.addCleanup(setattr, sys, "argv", saved)
        sys.argv = ["cargo", "fmt", "--check"]
        with contextlib.redirect_stderr(stderr):
            self.assertEqual(cargo.main(), 1)
        self.assertEqual(stderr.getvalue(), "atelier CI: cargo fmt ran past 1s\n")


HERE = SCRIPT.parent
CONTAINED = HERE / "contained.jq"
FLOOR = HERE / "floor.sh"
# A real `docker inspect` of the offline validator from run 43597277527,
# reduced to the fields contained.jq reads.
INSPECTION = HERE / "inspection.json"
MOUNT = "bind:/home/runner/work/_temp/atelier-reports:/reports"


class ContainmentTests(unittest.TestCase):
    """contained.jq refuses a container with any one setting off."""

    def contained(self, inspection: object, network: str = "none", *mounts: str) -> bool:
        completed = subprocess.run(
            [
                "jq",
                "-e",
                "--arg",
                "network",
                network,
                "--from-file",
                str(CONTAINED),
                "--args",
                *(mounts or (MOUNT,)),
            ],
            input=json.dumps(inspection),
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        return completed.returncode == 0

    def test_the_saved_inspection_is_contained(self) -> None:
        good = json.loads(INSPECTION.read_text(encoding="utf-8"))
        self.assertTrue(self.contained(good))

    def test_a_gate_can_use_bridge_and_its_two_readonly_mounts(self) -> None:
        gate = json.loads(INSPECTION.read_text(encoding="utf-8"))
        gate[0]["HostConfig"]["NetworkMode"] = "bridge"
        gate[0]["HostConfig"]["Mounts"] = [
            {"Type": "bind", "Source": "/checkout", "Target": "/source", "ReadOnly": True},
            {"Type": "bind", "Source": "/binary", "Target": "/binary", "ReadOnly": True},
        ]
        gate[0]["Mounts"] = [
            {"Type": "bind", "Source": "/checkout", "Destination": "/source", "RW": False},
            {"Type": "bind", "Source": "/binary", "Destination": "/binary", "RW": False},
        ]
        self.assertTrue(
            self.contained(gate, "bridge", "bind:/checkout:/source", "bind:/binary:/binary")
        )

    def test_one_bad_setting_at_a_time_is_refused(self) -> None:
        good = json.loads(INSPECTION.read_text(encoding="utf-8"))
        mount = good[0]["HostConfig"]["Mounts"][0]
        cases = {
            "root user": ("Config", "User", "0:0"),
            "cap added": ("HostConfig", "CapAdd", ["SYS_ADMIN"]),
            "caps kept": ("HostConfig", "CapDrop", []),
            "new privileges": ("HostConfig", "SecurityOpt", []),
            "extra security opt": (
                "HostConfig",
                "SecurityOpt",
                ["no-new-privileges", "seccomp=unconfined"],
            ),
            "privileged": ("HostConfig", "Privileged", True),
            "host pid": ("HostConfig", "PidMode", "host"),
            "host ipc": ("HostConfig", "IpcMode", "host"),
            "host userns": ("HostConfig", "UsernsMode", "host"),
            "host uts": ("HostConfig", "UTSMode", "host"),
            "host network": ("HostConfig", "NetworkMode", "host"),
            "bridge network": ("HostConfig", "NetworkMode", "bridge"),
            "device": ("HostConfig", "Devices", [{"PathOnHost": "/dev/kvm"}]),
            "volumes from": ("HostConfig", "VolumesFrom", ["other"]),
            "group added": ("HostConfig", "GroupAdd", ["docker"]),
            "tmpfs": ("HostConfig", "Tmpfs", {"/run": ""}),
            "no pids limit": ("HostConfig", "PidsLimit", 0),
            "no memory limit": ("HostConfig", "Memory", 0),
            "swap": ("HostConfig", "MemorySwap", -1),
            "no cpu limit": ("HostConfig", "NanoCpus", 0),
            "bind": ("HostConfig", "Binds", ["/var/run/docker.sock:/var/run/docker.sock"]),
            "writable mount": ("HostConfig", "Mounts", [{**mount, "ReadOnly": False}]),
            "extra mount": ("HostConfig", "Mounts", [mount, {**mount, "Target": "/other"}]),
            "other source": ("HostConfig", "Mounts", [{**mount, "Source": "/home/runner"}]),
            "other type": ("HostConfig", "Mounts", [{**mount, "Type": "volume"}]),
            "anonymous volume": (None, "Mounts", [*good[0]["Mounts"], {"Type": "volume"}]),
        }
        for case, (section, field, value) in cases.items():
            with self.subTest(case=case):
                bad = copy.deepcopy(good)
                target = bad[0] if section is None else bad[0][section]
                target[field] = value
                self.assertFalse(self.contained(bad))

    def test_the_network_and_mounts_must_be_the_expected_ones(self) -> None:
        good = json.loads(INSPECTION.read_text(encoding="utf-8"))
        self.assertFalse(self.contained(good, "bridge"))
        self.assertFalse(self.contained(good, "none", MOUNT, "bind:/x:/y"))
        self.assertFalse(self.contained(good, "none", "bind:/home/runner:/reports"))


class FloorTests(unittest.TestCase):
    """atelier's floor applies only in atelier's own workflow."""

    def floor(self, **environment: str) -> str:
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), **environment}
        completed = subprocess.run(
            ["bash", str(FLOOR)],
            env=env,
            capture_output=True,
            text=True,
            timeout=10,
            check=True,
        )
        return completed.stdout.strip()

    def test_the_floor_follows_the_workflow_repository(self) -> None:
        own = {"WORKFLOW_REPOSITORY": "wrkhse/atelier", "GITHUB_REPOSITORY": "wrkhse/atelier"}
        self.assertEqual(self.floor(**own), "atelier")
        other = {**own, "GITHUB_REPOSITORY": "wrkhse/chauffeur"}
        self.assertEqual(self.floor(**other), "")
        self.assertEqual(self.floor(GITHUB_REPOSITORY="wrkhse/atelier"), "")
        self.assertEqual(self.floor(WORKFLOW_REPOSITORY="", GITHUB_REPOSITORY=""), "")


class GateConfigTests(unittest.TestCase):
    """The checked-out gate definitions keep what the reports rely on."""

    @unittest.skipUnless(os.environ.get("FLOOR") == "atelier", "atelier's own gate config")
    def test_the_mutants_run_script_starts_with_set_e(self) -> None:
        # /source in the self-test container; the repository otherwise.
        candidates = [
            Path("/source/.atelier/config.toml"),
            HERE.parents[1] / ".atelier/config.toml",
        ]
        path = next((path for path in candidates if path.is_file()), None)
        self.assertIsNotNone(path, f"no gate config among {candidates}")
        config = tomllib.loads(path.read_text(encoding="utf-8"))
        run = config["commands"]["mutants"]["run"]
        first = next(line.strip() for line in run.splitlines() if line.strip())
        self.assertRegex(first, r"^set -[a-z]*e[a-z]*( |$)")


WrapperTests.real_listed = staticmethod(cargo.listed)
WrapperTests.real_changed_files = staticmethod(cargo.changed_files)


if __name__ == "__main__":
    unittest.main()
