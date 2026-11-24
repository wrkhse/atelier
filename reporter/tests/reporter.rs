//! Runs the built reporter on a recorded moon report.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const GATES: [&str; 4] = ["rust/fmt", "rust/clippy", "rust/test", "rust/mutants"];

const OUTCOMES: [&str; 4] = [
    "rust/fmt: passed",
    "rust/clippy: passed",
    "rust/test: failed",
    "rust/mutants: passed",
];

/// A fresh folder under Cargo's temporary folder for integration tests.
fn scratch(label: &str) -> io::Result<PathBuf> {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    let name = format!("{label}-{}-{}", process::id(), since.as_nanos());
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("test-tmp")
        .join(name);

    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Copies the recorded report `name`, such as `ci-report.json`, into `dir`.
fn fixture(dir: &Path, name: &str) -> io::Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = manifest.join("tests/fixtures").join(name);
    let report = dir.join(name);

    fs::copy(source, &report)?;
    Ok(report)
}

/// Runs the reporter on `report` with each of `gates`, such as `GATES`.
/// `GITHUB_STEP_SUMMARY` is `summary` when given, and unset otherwise, so
/// a run under GitHub Actions never writes to the job's own summary.
fn run(report: &Path, gates: &[&str], summary: Option<&Path>) -> io::Result<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_atelier-reporter"));
    command.arg("--report").arg(report);

    for gate in gates {
        command.args(["--gate", gate]);
    }

    match summary {
        Some(file) => command.env("GITHUB_STEP_SUMMARY", file),
        None => command.env_remove("GITHUB_STEP_SUMMARY"),
    };

    command.output()
}

fn stdout_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn validate_only_reports_every_gate() {
    let dir = scratch("validate").expect("scratch folder");
    let report = fixture(&dir, "ci-report.json").expect("copy the fixture");

    let output = run(&report, &GATES, None).expect("run the reporter");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_lines(&output), OUTCOMES);

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
fn validate_only_succeeds_when_all_gates_pass() {
    let dir = scratch("validate-pass").expect("scratch folder");
    let report = fixture(&dir, "ci-report-pass.json").expect("copy the fixture");

    let output = run(&report, &GATES, None).expect("run the reporter");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let passed = [
        "rust/fmt: passed",
        "rust/clippy: passed",
        "rust/test: passed",
        "rust/mutants: passed",
    ];
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(stdout_lines(&output), passed);

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
fn validate_only_fails_every_gate_of_a_missing_report() {
    let dir = scratch("missing").expect("scratch folder");
    let report = dir.join("absent.json");

    let output = run(&report, &GATES, None).expect("run the reporter");

    let failed = [
        "rust/fmt: failed",
        "rust/clippy: failed",
        "rust/test: failed",
        "rust/mutants: failed",
    ];
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_lines(&output), failed);

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
fn summary_is_written_for_each_gate() {
    let dir = scratch("summary").expect("scratch folder");
    let report = fixture(&dir, "ci-report-pass.json").expect("copy the fixture");
    let summary = dir.join("summary.md");

    let output = run(&report, &GATES, Some(&summary)).expect("run the reporter");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let written = fs::read_to_string(&summary).expect("read the summary");
    let expected = "## Gate results
- rust/fmt: passed
- rust/clippy: passed
- rust/test: passed
- rust/mutants: passed
";
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(written, expected);

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
fn duplicate_gate_is_refused() {
    let dir = scratch("duplicate").expect("scratch folder");
    let report = fixture(&dir, "ci-report-pass.json").expect("copy the fixture");
    let gates = ["rust/fmt", "rust/clippy", "rust/fmt"];

    let output = run(&report, &gates, None).expect("run the reporter");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout, "");
    assert!(stderr.contains("rust/fmt"), "{stderr}");

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}
