//! Runs the built reporter on a recorded moon report, with a stand-in `gh`
//! that logs its arguments one per line to `GH_LOG`.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const GATES: [&str; 4] = ["rust/fmt", "rust/clippy", "rust/test", "rust/mutants"];

/// What publishing needs; `--validate-only` runs without it.
const PUBLISH: [&str; 4] = ["--repo", "wrkhse/atelier", "--sha", "abc123"];

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

fn fixture(dir: &Path) -> io::Result<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = manifest.join("tests/fixtures/ci-report.json");
    let report = dir.join("ci-report.json");

    fs::copy(source, &report)?;
    Ok(report)
}

/// Runs the reporter on `report` with the four gates, the stand-in `gh`
/// first on `PATH` and `extra` arguments last, such as `PUBLISH`.
fn run(dir: &Path, report: &Path, extra: &[&str]) -> io::Result<Output> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let stand_in = manifest.join("tests/stand_in");
    let inherited = env::var_os("PATH").unwrap_or_default();
    let entries = std::iter::once(stand_in).chain(env::split_paths(&inherited));
    let path = env::join_paths(entries).map_err(io::Error::other)?;

    let mut command = Command::new(env!("CARGO_BIN_EXE_atelier-reporter"));
    command.arg("--report").arg(report);

    for gate in GATES {
        command.args(["--gate", gate]);
    }

    command
        .args(extra)
        .env("GH_LOG", dir.join("gh.log"))
        .env("PATH", path)
        .output()
}

/// Every value `gh` was given for the field `key`, such as `name=`.
fn logged(dir: &Path, key: &str) -> io::Result<Vec<String>> {
    let log = fs::read_to_string(dir.join("gh.log"))?;

    Ok(log
        .lines()
        .filter_map(|line| line.strip_prefix(key))
        .map(str::to_owned)
        .collect())
}

fn stdout_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn each_gate_becomes_one_check_in_order() {
    let dir = scratch("publish").expect("scratch folder");
    let report = fixture(&dir).expect("copy the fixture");

    let output = run(&dir, &report, &PUBLISH).expect("run the reporter");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let names = logged(&dir, "name=").expect("read the gh log");
    let conclusions = logged(&dir, "conclusion=").expect("read the gh log");
    assert!(output.status.success(), "{stderr}");
    assert_eq!(names, GATES);
    assert_eq!(conclusions, ["success", "success", "failure", "success"]);
    assert_eq!(stdout_lines(&output), OUTCOMES);

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
fn a_missing_report_fails_every_gate() {
    let dir = scratch("missing").expect("scratch folder");
    let report = dir.join("absent.json");

    let output = run(&dir, &report, &PUBLISH).expect("run the reporter");

    let conclusions = logged(&dir, "conclusion=").expect("read the gh log");
    let summaries = logged(&dir, "output[summary]=").expect("read the gh log");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(conclusions, ["failure"; 4]);
    assert_eq!(summaries, ["missing regular report"; 4]);

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
fn validate_only_reports_every_gate_without_gh() {
    let dir = scratch("validate").expect("scratch folder");
    let report = fixture(&dir).expect("copy the fixture");

    let output = run(&dir, &report, &["--validate-only"]).expect("run the reporter");

    assert_eq!(output.status.code(), Some(1));
    assert!(!dir.join("gh.log").exists());
    assert_eq!(stdout_lines(&output), OUTCOMES);

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}
