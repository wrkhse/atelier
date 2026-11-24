//! Validates each gate of a recorded `moon ci` run and writes the job
//! summary. A report that cannot be read fails every gate. It does not create
//! GitHub checks: the gate job's own status is the check.

use std::collections::BTreeSet;
use std::env;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use atelier_reporter::{GateCheck, GateState, ReportError, report};
use clap::Parser;

/// Validate each gate of a moon CI run and write the job summary.
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    /// Path to moon's `ciReport.json`.
    #[arg(long, value_name = "PATH")]
    report: PathBuf,

    /// A gate to validate, such as `rust/fmt`; repeat for each gate.
    #[arg(long = "gate", value_name = "NAME", required = true)]
    gates: Vec<String>,

    /// The largest report read, in bytes.
    #[arg(long, value_name = "N", default_value_t = 8_388_608)]
    max_report_bytes: u64,
}

/// Fails when a gate is listed twice, the report could not be read, any
/// gate failed, or the job summary could not be written.
fn main() -> ExitCode {
    let args = Args::parse();

    if let Some(gate) = repeated(&args.gates) {
        eprintln!("gate {gate} is listed more than once");
        return ExitCode::FAILURE;
    }

    let read = report::read(&args.report, args.max_report_bytes);
    let gate_checks = match &read {
        Ok(ci) => report::results(ci, &args.gates),
        Err(error) => unreadable(&args.gates, error),
    };

    if let Err(error) = &read {
        eprintln!("{}: {error}", args.report.display());
    }

    let summarized = summarize(&gate_checks);
    let passed = validate(&gate_checks);

    if passed && summarized && read.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The first gate named again after an earlier mention, if any.
fn repeated(gates: &[String]) -> Option<&str> {
    let mut seen = BTreeSet::new();

    gates
        .iter()
        .map(String::as_str)
        .find(|gate| !seen.insert(*gate))
}

/// A report that could not be read fails every gate with the reason.
fn unreadable(gates: &[String], error: &ReportError) -> Vec<GateCheck> {
    let failed = |gate: &String| GateCheck {
        name: gate.clone(),
        state: GateState::Failed,
        detail: error.to_string(),
    };

    gates.iter().map(failed).collect()
}

/// Appends each gate's outcome to the job summary file that
/// `GITHUB_STEP_SUMMARY` names; true when it names none or the write
/// succeeded.
fn summarize(gate_checks: &[GateCheck]) -> bool {
    let named = env::var_os("GITHUB_STEP_SUMMARY").filter(|path| !path.is_empty());
    let Some(path) = named else {
        return true;
    };

    let path = Path::new(&path);

    if let Err(error) = append_summary(path, gate_checks) {
        eprintln!("job summary {}: {error}", path.display());
        return false;
    }

    true
}

fn append_summary(path: &Path, gate_checks: &[GateCheck]) -> io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "## Gate results")?;

    for check in gate_checks {
        writeln!(file, "- {}: {}", check.name, outcome(check.state))?;
    }

    Ok(())
}

/// Prints each gate; true only when every gate passed.
fn validate(gate_checks: &[GateCheck]) -> bool {
    for check in gate_checks {
        println!("{}: {}", check.name, outcome(check.state));
    }

    gate_checks
        .iter()
        .all(|check| check.state == GateState::Passed)
}

const fn outcome(state: GateState) -> &'static str {
    match state {
        GateState::Passed => "passed",
        GateState::Failed => "failed",
    }
}
