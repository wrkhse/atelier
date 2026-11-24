//! Reads a recorded moon CI report and reports each gate as a GitHub check.

pub mod checks;
pub mod report;

use std::fmt;

/// The longest detail a check shows: `MAX_REASON` in the validator that
/// `report` ports.
const MAX_DETAIL: usize = 300;

/// Whether a gate passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateState {
    Passed,
    Failed,
}

/// One GitHub check: its full name, such as `rust/fmt`, its state and a
/// short detail for the check's summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateCheck {
    pub name: String,
    pub state: GateState,
    pub detail: String,
}

/// Why a CI report was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    /// The path is not a regular file: absent, a directory or a symlink.
    Missing,
    Empty,
    TooLarge,
    Invalid(String),
}

impl fmt::Display for ReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("missing regular report"),
            Self::Empty => f.write_str("empty report"),
            Self::TooLarge => f.write_str("oversized report"),
            Self::Invalid(reason) => write!(f, "invalid report: {reason}"),
        }
    }
}

impl std::error::Error for ReportError {}

/// The first `MAX_DETAIL` characters of `text`, cut on a character boundary.
fn excerpt(text: &str) -> String {
    text.chars().take(MAX_DETAIL).collect()
}

#[cfg(test)]
mod tests {
    use super::{ReportError, excerpt};

    #[test]
    fn report_errors_read_as_short_lowercase_messages() {
        let invalid = ReportError::Invalid("bad json".to_owned());

        assert_eq!(ReportError::Missing.to_string(), "missing regular report");
        assert_eq!(ReportError::Empty.to_string(), "empty report");
        assert_eq!(ReportError::TooLarge.to_string(), "oversized report");
        assert_eq!(invalid.to_string(), "invalid report: bad json");
    }

    #[test]
    fn an_excerpt_keeps_three_hundred_characters_on_a_boundary() {
        let long = "é".repeat(301);

        assert_eq!(excerpt(&long), "é".repeat(300));
        assert_eq!(excerpt("short"), "short");
    }
}
