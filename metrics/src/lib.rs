//! Compares the measurements of a base and a head version: function
//! complexity, which may not regress, and instruction counts against a budget.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;

/// The complexity of one function, as rust-code-analysis measures it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FunctionMetric {
    pub name: String,
    pub file: String,
    pub cyclomatic: f64,
    pub cognitive: f64,
    pub halstead_volume: f64,
    pub maintainability: f64,
}

impl FunctionMetric {
    /// The name that matches a function across versions, such as
    /// `src/lib.rs:Config::new`.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}:{}", self.file, self.name)
    }
}

/// How a head function compares with the same function in the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Same,
    Improved,
    Regressed,
    New,
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Same => "same",
            Self::Improved => "improved",
            Self::Regressed => "regressed",
            Self::New => "new",
        };

        f.write_str(name)
    }
}

/// One head function beside its base version, absent when it is new.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub key: String,
    pub base: Option<FunctionMetric>,
    pub head: FunctionMetric,
    pub state: State,
}

/// Compares each head function with the base function of the same key, in
/// head order. A function only in the base is not listed: removing one is
/// never a regression.
#[must_use]
pub fn compare(base: &[FunctionMetric], head: &[FunctionMetric]) -> Vec<Comparison> {
    let before: BTreeMap<String, &FunctionMetric> =
        base.iter().map(|metric| (metric.key(), metric)).collect();

    head.iter()
        .map(|metric| {
            let key = metric.key();
            let previous = before.get(&key).copied();
            let state = previous.map_or(State::New, |base| judge(base, metric));

            Comparison {
                key,
                base: previous.cloned(),
                head: metric.clone(),
                state,
            }
        })
        .collect()
}

/// True unless a function regressed. A new function passes: the ratchet
/// keeps existing functions from getting worse and does not stop code being
/// added.
#[must_use]
pub fn passed(comparisons: &[Comparison]) -> bool {
    comparisons
        .iter()
        .all(|comparison| comparison.state != State::Regressed)
}

/// Regressed when any measure got worse, improved when one got better and
/// none worse, and the same otherwise. Values are compared exactly.
fn judge(base: &FunctionMetric, head: &FunctionMetric) -> State {
    let changes = [
        cost(base.cyclomatic, head.cyclomatic),
        cost(base.cognitive, head.cognitive),
        cost(base.halstead_volume, head.halstead_volume),
        score(base.maintainability, head.maintainability),
    ];

    if changes.contains(&Change::Worse) {
        State::Regressed
    } else if changes.contains(&Change::Better) {
        State::Improved
    } else {
        State::Same
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change {
    Better,
    Same,
    Worse,
}

/// A measure where lower is better. One that cannot be compared, such as
/// `NaN`, is worse, so the ratchet fails closed.
fn cost(before: f64, after: f64) -> Change {
    match after.partial_cmp(&before) {
        Some(Ordering::Less) => Change::Better,
        Some(Ordering::Equal) => Change::Same,
        Some(Ordering::Greater) | None => Change::Worse,
    }
}

/// A measure where higher is better, such as the maintainability index.
fn score(before: f64, after: f64) -> Change {
    cost(after, before)
}

/// True when `head` is at most `base` times (100 + `limit_percent`) / 100.
///
/// Fails closed: false for `NaN` or infinite input, a negative count, or a
/// product too large to represent.
#[must_use]
pub const fn budget(base: f64, head: f64, limit_percent: f64) -> bool {
    if base < 0.0 || head < 0.0 {
        return false;
    }

    // A NaN or infinite base or limit leaves `allowed` non-finite, and a NaN
    // or infinite head fails the comparison, so neither needs its own guard.
    let allowed = base * (100.0 + limit_percent);

    allowed.is_finite() && head * 100.0 <= allowed
}

#[cfg(test)]
mod tests {
    use super::{Comparison, FunctionMetric, State, budget, compare, passed};

    const BASE: [f64; 4] = [3.0, 2.0, 120.5, 110.25];

    fn metric(name: &str, measures: [f64; 4]) -> FunctionMetric {
        let [cyclomatic, cognitive, halstead_volume, maintainability] = measures;

        FunctionMetric {
            name: name.to_owned(),
            file: "src/lib.rs".to_owned(),
            cyclomatic,
            cognitive,
            halstead_volume,
            maintainability,
        }
    }

    fn states(base: [f64; 4], head: [f64; 4]) -> Vec<State> {
        let compared = compare(&[metric("parse", base)], &[metric("parse", head)]);

        compared.iter().map(|comparison| comparison.state).collect()
    }

    #[test]
    fn an_unchanged_function_is_the_same() {
        assert_eq!(states(BASE, BASE), [State::Same]);
    }

    #[test]
    fn a_lower_cost_or_higher_maintainability_improves() {
        assert_eq!(states(BASE, [2.0, 2.0, 120.5, 110.25]), [State::Improved]);
        assert_eq!(states(BASE, [3.0, 1.0, 120.5, 110.25]), [State::Improved]);
        assert_eq!(states(BASE, [3.0, 2.0, 100.0, 110.25]), [State::Improved]);
        assert_eq!(states(BASE, [3.0, 2.0, 120.5, 111.0]), [State::Improved]);
    }

    #[test]
    fn a_higher_cost_or_lower_maintainability_regresses() {
        assert_eq!(states(BASE, [4.0, 2.0, 120.5, 110.25]), [State::Regressed]);
        assert_eq!(states(BASE, [3.0, 3.0, 120.5, 110.25]), [State::Regressed]);
        assert_eq!(states(BASE, [3.0, 2.0, 120.75, 110.25]), [State::Regressed]);
        assert_eq!(states(BASE, [3.0, 2.0, 120.5, 110.0]), [State::Regressed]);
    }

    #[test]
    fn one_regression_outweighs_improvements() {
        assert_eq!(states(BASE, [1.0, 1.0, 100.0, 109.0]), [State::Regressed]);
    }

    #[test]
    fn a_measure_that_is_not_a_number_regresses() {
        assert_eq!(
            states(BASE, [f64::NAN, 2.0, 120.5, 110.25]),
            [State::Regressed]
        );
        assert_eq!(
            states(BASE, [3.0, 2.0, 120.5, f64::NAN]),
            [State::Regressed]
        );
    }

    #[test]
    fn a_function_only_in_head_is_new() {
        let base = [metric("parse", BASE)];
        let head = [metric("parse", BASE), metric("render", BASE)];

        let compared = compare(&base, &head);

        let expected = vec![
            Comparison {
                key: "src/lib.rs:parse".to_owned(),
                base: Some(metric("parse", BASE)),
                head: metric("parse", BASE),
                state: State::Same,
            },
            Comparison {
                key: "src/lib.rs:render".to_owned(),
                base: None,
                head: metric("render", BASE),
                state: State::New,
            },
        ];
        assert_eq!(compared, expected);
    }

    #[test]
    fn functions_match_on_file_and_name() {
        let mut moved = metric("parse", BASE);
        moved.file = "src/main.rs".to_owned();

        let compared = compare(&[metric("parse", BASE)], &[moved]);

        let keys: Vec<(&str, State)> = compared
            .iter()
            .map(|comparison| (comparison.key.as_str(), comparison.state))
            .collect();
        assert_eq!(keys, [("src/main.rs:parse", State::New)]);
    }

    #[test]
    fn a_new_function_passes_and_a_regressed_one_fails() {
        let same = metric("parse", BASE);
        let worse = metric("parse", [4.0, 2.0, 120.5, 110.25]);
        let added = metric("render", BASE);

        let grown = compare(std::slice::from_ref(&same), &[same.clone(), added.clone()]);
        let regressed = compare(&[same], &[worse, added]);

        assert_eq!([passed(&grown), passed(&regressed)], [true, false]);
    }

    #[test]
    fn states_read_as_lowercase_words() {
        let states = [State::Same, State::Improved, State::Regressed, State::New];

        let names: Vec<String> = states.iter().map(ToString::to_string).collect();

        assert_eq!(names, ["same", "improved", "regressed", "new"]);
    }

    #[test]
    fn a_five_percent_limit_accepts_four_and_refuses_six() {
        let verdicts = [
            budget(100.0, 104.0, 5.0),
            budget(100.0, 105.0, 5.0),
            budget(100.0, 106.0, 5.0),
        ];

        assert_eq!(verdicts, [true, true, false]);
    }

    #[test]
    fn a_zero_count_within_a_zero_base_passes() {
        assert_eq!(
            [budget(0.0, 0.0, 5.0), budget(0.0, 1.0, 5.0)],
            [true, false]
        );
    }

    #[test]
    fn a_budget_fails_closed_on_input_it_cannot_trust() {
        let verdicts = [
            budget(f64::NAN, 100.0, 5.0),
            budget(100.0, f64::NAN, 5.0),
            budget(100.0, 100.0, f64::NAN),
            budget(f64::INFINITY, 100.0, 5.0),
            budget(100.0, f64::INFINITY, 5.0),
            budget(100.0, 100.0, f64::INFINITY),
            budget(-100.0, 50.0, -200.0),
            budget(100.0, -1.0, 5.0),
            budget(f64::MAX, f64::MAX, 5.0),
        ];

        assert_eq!(verdicts, [false; 9]);
    }
}
