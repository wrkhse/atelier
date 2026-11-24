//! Fail-closed reading of moon's `ciReport.json`.
//!
//! Ported from the report validation in `.github/ci/reports.py` at commit
//! f687307: a report must be a regular file, not empty, within its size limit
//! and valid JSON.
//!
//! serde's derived `Deserialize` refuses a repeated field, the defence against
//! duplicate keys that `unique_object` gave the old validator. It covers only
//! the fields the reporter reads; a repeated key it ignores, such as
//! `createdAt`, passes.
//!
//! Moon records actions besides `run-task`, such as `sync-workspace` with no
//! `params` or `setup-toolchain` with no `target`, so those fields are
//! optional and such actions are ignored, not refused.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use serde::Deserialize;
use serde_json::error::Category;

use crate::{GateCheck, GateState, ReportError};

/// The statuses moon gives a task that succeeded.
const PASSING: [&str; 3] = ["passed", "cached", "cached-from-remote"];

/// A recorded `moon ci` run; moon's other fields are not read.
#[derive(Debug, Deserialize)]
pub struct CiReport {
    pub actions: Vec<Action>,
}

/// One action of the run, such as running `reporter:fmt`.
#[derive(Debug, Deserialize)]
pub struct Action {
    pub node: Node,
    pub status: String,
    pub label: Option<String>,
}

/// The kind of action, such as `run-task`, and its parameters.
#[derive(Debug, Deserialize)]
pub struct Node {
    pub action: String,
    pub params: Option<Params>,
}

/// The task target an action ran, such as `reporter:fmt`.
#[derive(Debug, Deserialize)]
pub struct Params {
    pub target: Option<String>,
}

/// Reads the report at `path`, refusing anything it cannot trust.
///
/// # Errors
///
/// `Missing` when the path is not a regular file (a symlink is refused, as
/// `symlink_metadata` does not follow it), `Empty` for an empty file,
/// `TooLarge` past `max_bytes`, and `Invalid` for unreadable or malformed
/// JSON or a report of the wrong shape.
pub fn read(path: &Path, max_bytes: u64) -> Result<CiReport, ReportError> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Err(ReportError::Missing);
    };

    if !metadata.file_type().is_file() {
        return Err(ReportError::Missing);
    }

    let raw = read_bounded(path, max_bytes)?;
    parse(&raw)
}

/// Reads at most one byte past `max_bytes`, so an oversized file is never
/// held in memory whole.
fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, ReportError> {
    let file = File::open(path).map_err(|_| ReportError::Missing)?;
    let mut limited = file.take(max_bytes.saturating_add(1));
    let mut raw = Vec::new();

    if let Err(error) = limited.read_to_end(&mut raw) {
        return Err(unreadable(&error));
    }

    let size = u64::try_from(raw.len()).map_err(|_| ReportError::TooLarge)?;

    if size > max_bytes {
        return Err(ReportError::TooLarge);
    }

    Ok(raw)
}

fn unreadable(error: &io::Error) -> ReportError {
    ReportError::Invalid(format!("unreadable report: {}", error.kind()))
}

fn parse(raw: &[u8]) -> Result<CiReport, ReportError> {
    if raw.is_empty() {
        return Err(ReportError::Empty);
    }

    serde_json::from_slice(raw).map_err(|error| invalid(&error))
}

fn invalid(error: &serde_json::Error) -> ReportError {
    let kind = match error.classify() {
        Category::Io => "unreadable json",
        Category::Syntax => "malformed json",
        Category::Data => "unexpected json shape",
        Category::Eof => "truncated json",
    };

    ReportError::Invalid(format!("{kind} at line {}", error.line()))
}

/// One check per gate, in the order of `gates`.
///
/// A gate such as `rust/fmt` matches the `run-task` actions whose target
/// ends in its task id, `:fmt`. Matching is on the task id after the gate's
/// last slash, so a full target such as `reporter:fmt` is not accepted as a
/// gate name. It passes only when it has actions and every one of them
/// succeeded. The detail names the first action that did not
/// succeed, or the first action when all did.
#[must_use]
pub fn results(report: &CiReport, gates: &[String]) -> Vec<GateCheck> {
    gates.iter().map(|gate| result(report, gate)).collect()
}

fn result(report: &CiReport, gate: &str) -> GateCheck {
    let task = gate.rsplit('/').next().unwrap_or(gate);
    let matching: Vec<&Action> = report
        .actions
        .iter()
        .filter(|action| runs(action, task))
        .collect();

    let Some(first) = matching.first().copied() else {
        return GateCheck {
            name: gate.to_owned(),
            state: GateState::Failed,
            detail: "no moon result".to_owned(),
        };
    };

    let unsuccessful = matching.iter().copied().find(|action| !succeeded(action));
    let state = if unsuccessful.is_some() {
        GateState::Failed
    } else {
        GateState::Passed
    };

    let shown = unsuccessful.unwrap_or(first);
    let label = shown.label.as_deref().unwrap_or("<unknown>");

    GateCheck {
        name: gate.to_owned(),
        state,
        detail: crate::excerpt(&format!("{label}: {}", shown.status)),
    }
}

fn succeeded(action: &Action) -> bool {
    PASSING.contains(&action.status.as_str())
}

fn runs(action: &Action, task: &str) -> bool {
    if action.node.action != "run-task" {
        return false;
    }

    let Some(params) = &action.node.params else {
        return false;
    };
    let Some(target) = &params.target else {
        return false;
    };

    target.rsplit(':').next() == Some(task)
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::process;

    use super::{parse, read, results};
    use crate::{GateCheck, GateState, ReportError};

    const FMT_PASSED: &str = r#"{"actions":[
        {"node":{"action":"run-task","params":{"target":"reporter:fmt"}},
         "status":"passed","label":"RunTask(reporter:fmt)"},
        {"node":{"action":"run-task","params":{"target":"core:fmt"}},
         "status":"cached","label":"RunTask(core:fmt)"}
    ]}"#;

    const TEST_FAILED: &str = r#"{"actions":[
        {"node":{"action":"run-task","params":{"target":"reporter:test"}},
         "status":"passed","label":"RunTask(reporter:test)"},
        {"node":{"action":"run-task","params":{"target":"core:test"}},
         "status":"failed","label":"RunTask(core:test)"},
        {"node":{"action":"run-task","params":{"target":"core:retest"}},
         "status":"passed","label":"RunTask(core:retest)"}
    ]}"#;

    const OTHER_ACTIONS: &str = r#"{"actions":[
        {"node":{"action":"sync-workspace"},"status":"passed",
         "label":"SyncWorkspace"},
        {"node":{"action":"setup-toolchain","params":{"runtime":{"platform":"rust"}}},
         "status":"passed","label":"SetupToolchain(rust)"},
        {"node":{"action":"run-task","params":{"target":"reporter:clippy"}},
         "status":"cached"}
    ]}"#;

    const DUPLICATE_STATUS: &str = r#"{"actions":[{"node":{"action":"run-task","params":{"target":"core:fmt"}},"status":"passed","status":"failed","label":"RunTask(core:fmt)"}]}"#;

    fn gates(names: &[&str]) -> Vec<String> {
        names.iter().copied().map(str::to_owned).collect()
    }

    fn written(name: &str, contents: &str) -> PathBuf {
        let file = format!("atelier-reporter-{}-{name}.json", process::id());
        let path = env::temp_dir().join(file);
        fs::write(&path, contents).expect("write report");
        path
    }

    #[test]
    fn a_gate_whose_actions_all_succeeded_passes() {
        let report = parse(FMT_PASSED.as_bytes()).expect("valid report");

        let checks = results(&report, &gates(&["rust/fmt"]));

        let expected = GateCheck {
            name: "rust/fmt".to_owned(),
            state: GateState::Passed,
            detail: "RunTask(reporter:fmt): passed".to_owned(),
        };
        assert_eq!(checks, vec![expected]);
    }

    #[test]
    fn a_gate_with_a_failed_action_fails() {
        let report = parse(TEST_FAILED.as_bytes()).expect("valid report");

        let checks = results(&report, &gates(&["rust/test"]));

        let expected = GateCheck {
            name: "rust/test".to_owned(),
            state: GateState::Failed,
            detail: "RunTask(core:test): failed".to_owned(),
        };
        assert_eq!(checks, vec![expected]);
    }

    #[test]
    fn actions_other_than_run_task_are_ignored() {
        let report = parse(OTHER_ACTIONS.as_bytes()).expect("valid report");

        let checks = results(&report, &gates(&["rust/clippy"]));

        let expected = GateCheck {
            name: "rust/clippy".to_owned(),
            state: GateState::Passed,
            detail: "<unknown>: cached".to_owned(),
        };
        assert_eq!(checks, vec![expected]);
    }

    #[test]
    fn a_gate_moon_did_not_run_fails() {
        let report = parse(FMT_PASSED.as_bytes()).expect("valid report");

        let checks = results(&report, &gates(&["rust/fmt", "rust/clippy"]));

        let names: Vec<&str> = checks.iter().map(|check| check.name.as_str()).collect();
        let missing = GateCheck {
            name: "rust/clippy".to_owned(),
            state: GateState::Failed,
            detail: "no moon result".to_owned(),
        };
        assert_eq!(names, ["rust/fmt", "rust/clippy"]);
        assert_eq!(checks.get(1), Some(&missing));
    }

    #[test]
    fn a_repeated_field_is_invalid() {
        let refused = parse(DUPLICATE_STATUS.as_bytes()).err();

        let expected = ReportError::Invalid("unexpected json shape at line 1".to_owned());
        assert_eq!(refused, Some(expected));
    }

    #[test]
    fn malformed_json_is_invalid() {
        let path = written("malformed", "{x}");

        let refused = read(&path, 1024).err();

        let expected = ReportError::Invalid("malformed json at line 1".to_owned());
        assert_eq!(refused, Some(expected));
        fs::remove_file(&path).expect("remove report");
    }

    #[test]
    fn truncated_json_is_invalid() {
        let path = written("truncated", r#"{"actions":[{"node":"#);

        let refused = read(&path, 1024).err();

        let expected = ReportError::Invalid("truncated json at line 1".to_owned());
        assert_eq!(refused, Some(expected));
        fs::remove_file(&path).expect("remove report");
    }

    #[test]
    fn an_object_without_actions_is_invalid() {
        let refused = parse(b"{}").err();

        let expected = ReportError::Invalid("unexpected json shape at line 1".to_owned());
        assert_eq!(refused, Some(expected));
    }

    #[test]
    fn actions_of_the_wrong_type_are_invalid() {
        let refused = parse(br#"{"actions":1}"#).err();

        let expected = ReportError::Invalid("unexpected json shape at line 1".to_owned());
        assert_eq!(refused, Some(expected));
    }

    #[test]
    fn an_empty_file_is_refused() {
        let path = written("empty", "");

        let refused = read(&path, 1024).err();

        assert_eq!(refused, Some(ReportError::Empty));
        fs::remove_file(&path).expect("remove report");
    }

    #[test]
    fn a_file_past_the_limit_is_refused() {
        let path = written("oversized", FMT_PASSED);
        let size = fs::metadata(&path).expect("report metadata").len();

        let below = size.checked_sub(1).expect("a non-empty report");

        let refused = read(&path, below).err();
        let at_limit = read(&path, size).map(|report| report.actions.len());

        assert_eq!(refused, Some(ReportError::TooLarge));
        assert_eq!(at_limit, Ok(2));
        fs::remove_file(&path).expect("remove report");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_refused() {
        let target = written("target", FMT_PASSED);
        let link = target.with_extension("link");
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link).expect("make symlink");

        let refused = read(&link, 1024).err();

        assert_eq!(refused, Some(ReportError::Missing));
        fs::remove_file(&link).expect("remove symlink");
        fs::remove_file(&target).expect("remove report");
    }
}
