//! Creates GitHub check runs through the `gh` CLI. The child inherits the
//! environment, so `gh` reads `GH_TOKEN` as it would on its own.

use std::fmt;
use std::io;
use std::process::Command;

use crate::{GateCheck, GateState};

/// Why a check run could not be created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    GhNotFound,
    Failed(String),
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GhNotFound => f.write_str("gh not found on PATH"),
            Self::Failed(reason) => write!(f, "gh api failed: {reason}"),
        }
    }
}

impl std::error::Error for CheckError {}

/// Creates one completed check run for `check` on commit `sha` of `repo`.
///
/// # Errors
///
/// `GhNotFound` when `gh` is not on `PATH`, and `Failed` when it could
/// not start or exited unsuccessfully, with at most 300 characters of its
/// standard error.
pub fn create(check: &GateCheck, repo: &str, sha: &str) -> Result<(), CheckError> {
    let output = Command::new("gh")
        .args(arguments(check, repo, sha))
        .output()
        .map_err(|error| spawn_error(&error))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = format!("{}: {}", output.status, stderr.trim());

    Err(CheckError::Failed(crate::excerpt(&reason)))
}

fn spawn_error(error: &io::Error) -> CheckError {
    if error.kind() == io::ErrorKind::NotFound {
        CheckError::GhNotFound
    } else {
        CheckError::Failed(crate::excerpt(&error.to_string()))
    }
}

/// `gh api`'s arguments. `-f` sends each value as a raw string, so a
/// detail starting with `@` is never read as a file name.
fn arguments(check: &GateCheck, repo: &str, sha: &str) -> Vec<String> {
    vec![
        "api".to_owned(),
        "--method".to_owned(),
        "POST".to_owned(),
        format!("repos/{repo}/check-runs"),
        "-f".to_owned(),
        format!("name={}", check.name),
        "-f".to_owned(),
        format!("head_sha={sha}"),
        "-f".to_owned(),
        "status=completed".to_owned(),
        "-f".to_owned(),
        format!("conclusion={}", conclusion(check.state)),
        "-f".to_owned(),
        format!("output[title]={}", check.name),
        "-f".to_owned(),
        format!("output[summary]={}", check.detail),
    ]
}

const fn conclusion(state: GateState) -> &'static str {
    match state {
        GateState::Passed => "success",
        GateState::Failed => "failure",
    }
}

#[cfg(test)]
mod tests {
    use super::arguments;
    use crate::{GateCheck, GateState};

    #[test]
    fn a_failed_gate_becomes_a_completed_failure() {
        let check = GateCheck {
            name: "rust/test".to_owned(),
            state: GateState::Failed,
            detail: "RunTask(core:test): failed".to_owned(),
        };

        let args = arguments(&check, "wrkhse/atelier", "abc123");

        let expected = [
            "api",
            "--method",
            "POST",
            "repos/wrkhse/atelier/check-runs",
            "-f",
            "name=rust/test",
            "-f",
            "head_sha=abc123",
            "-f",
            "status=completed",
            "-f",
            "conclusion=failure",
            "-f",
            "output[title]=rust/test",
            "-f",
            "output[summary]=RunTask(core:test): failed",
        ];
        assert_eq!(args, expected);
    }
}
