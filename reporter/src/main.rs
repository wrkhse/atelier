//! Reports each gate of a recorded `moon ci` run as a GitHub check.

use std::path::PathBuf;
use std::process::ExitCode;

use atelier_reporter::{GateCheck, GateState, ReportError, checks, report};
use clap::Parser;

/// Report each gate of a moon CI run as a GitHub check.
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    /// Path to moon's `ciReport.json`.
    #[arg(long, value_name = "PATH")]
    report: PathBuf,

    /// A check to report, such as `rust/fmt`; repeat for each gate.
    #[arg(long = "gate", value_name = "NAME", required = true)]
    gates: Vec<String>,

    /// The repository the checks belong to; required unless validating only.
    #[arg(
        long,
        value_name = "OWNER/REPO",
        required_unless_present = "validate_only"
    )]
    repo: Option<String>,

    /// The commit the checks belong to; required unless validating only.
    #[arg(long, required_unless_present = "validate_only")]
    sha: Option<String>,

    /// The largest report read, in bytes.
    #[arg(long, value_name = "N", default_value_t = 8_388_608)]
    max_report_bytes: u64,

    /// Check the report and print each gate without creating checks; fails
    /// when any gate failed.
    #[arg(long)]
    validate_only: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let read = report::read(&args.report, args.max_report_bytes);
    let gate_checks = match &read {
        Ok(ci) => report::results(ci, &args.gates),
        Err(error) => unreadable(&args.gates, error),
    };

    if let Err(error) = &read {
        eprintln!("{}: {error}", args.report.display());
    }

    let ok = if args.validate_only {
        validate(&gate_checks)
    } else {
        publish(&gate_checks, args.repo.as_deref(), args.sha.as_deref())
    };

    if ok && read.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
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

/// Prints each gate; true only when every gate passed.
fn validate(gate_checks: &[GateCheck]) -> bool {
    for check in gate_checks {
        println!("{}: {}", check.name, outcome(check.state));
    }

    gate_checks
        .iter()
        .all(|check| check.state == GateState::Passed)
}

/// Creates each check and prints its gate once it exists; false without a
/// repository or commit, or at the first check that could not be created.
fn publish(gate_checks: &[GateCheck], repo: Option<&str>, sha: Option<&str>) -> bool {
    let (Some(repo), Some(sha)) = (repo, sha) else {
        eprintln!("publishing checks needs --repo and --sha");
        return false;
    };

    for check in gate_checks {
        if let Err(error) = checks::create(check, repo, sha) {
            eprintln!("{}: {error}", check.name);
            return false;
        }

        println!("{}: {}", check.name, outcome(check.state));
    }

    true
}

const fn outcome(state: GateState) -> &'static str {
    match state {
        GateState::Passed => "passed",
        GateState::Failed => "failed",
    }
}
