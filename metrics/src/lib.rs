//! Compares the measurements of a base and a head version: function
//! complexity against the base or an absolute budget, and instruction counts
//! against a relative budget.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;

use serde::Deserialize;

const MAX_CYCLOMATIC: f64 = 20.0;
const MAX_COGNITIVE: f64 = 30.0;
const MAX_HALSTEAD_VOLUME: f64 = 2000.0;
const MIN_MAINTAINABILITY: f64 = 50.0;

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
    /// The file and qualified name, such as `src/lib.rs:Config::new`, which
    /// matches a function across versions.
    fn key(&self) -> String {
        format!("{}:{}", self.file, self.name)
    }

    /// Folds `other`, a function of the same key, into this one, keeping the
    /// worse value of each measure.
    fn absorb(&mut self, other: &Self) {
        self.cyclomatic = highest(self.cyclomatic, other.cyclomatic);
        self.cognitive = highest(self.cognitive, other.cognitive);
        self.halstead_volume = highest(self.halstead_volume, other.halstead_volume);
        self.maintainability = lowest(self.maintainability, other.maintainability);
    }
}

/// The larger of two values, or `NaN` when either cannot be compared, so a
/// measure that is not a number survives aggregation and the ratchet fails
/// closed. `f64::max` would drop it.
fn highest(one: f64, other: f64) -> f64 {
    match other.partial_cmp(&one) {
        Some(Ordering::Greater) => other,
        Some(Ordering::Less | Ordering::Equal) => one,
        None => f64::NAN,
    }
}

/// The smaller of two values, or `NaN` when either cannot be compared.
fn lowest(one: f64, other: f64) -> f64 {
    match other.partial_cmp(&one) {
        Some(Ordering::Less) => other,
        Some(Ordering::Greater | Ordering::Equal) => one,
        None => f64::NAN,
    }
}

/// One entry per key, in order of first appearance.
///
/// A name listed more than once in a version, as nested anonymous items can
/// be, is folded into one entry holding its worst case: the highest
/// cyclomatic, cognitive and Halstead volume, and the lowest
/// maintainability. List order never decides which entries match.
fn worst_cases(metrics: &[FunctionMetric]) -> Vec<(String, FunctionMetric)> {
    let mut order = Vec::new();
    let mut merged: BTreeMap<String, FunctionMetric> = BTreeMap::new();

    for metric in metrics {
        match merged.entry(metric.key()) {
            Entry::Occupied(mut entry) => entry.get_mut().absorb(metric),
            Entry::Vacant(entry) => {
                order.push(entry.key().clone());
                entry.insert(metric.clone());
            }
        }
    }

    order
        .into_iter()
        .filter_map(|key| merged.remove(&key).map(|metric| (key, metric)))
        .collect()
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

/// Compares each head function with the same function in the base.
///
/// Each side is the worst case of its key, listed in head order of first
/// appearance. A function only in the base is not listed: removing one is
/// never a regression.
#[must_use]
pub fn compare(base: &[FunctionMetric], head: &[FunctionMetric]) -> Vec<Comparison> {
    let mut before: BTreeMap<String, FunctionMetric> = worst_cases(base).into_iter().collect();

    worst_cases(head)
        .into_iter()
        .map(|(key, metric)| {
            let previous = before.remove(&key);
            let state = previous
                .as_ref()
                .map_or(State::New, |base| judge(base, &metric));

            Comparison {
                key,
                base: previous,
                head: metric,
                state,
            }
        })
        .collect()
}

/// True when all measures of a function without a base meet their absolute
/// limits. A measure that is not a number fails its comparison.
#[must_use]
pub const fn within_absolute_budget(metric: &FunctionMetric) -> bool {
    metric.cyclomatic <= MAX_CYCLOMATIC
        && metric.cognitive <= MAX_COGNITIVE
        && metric.halstead_volume <= MAX_HALSTEAD_VOLUME
        && metric.maintainability >= MIN_MAINTAINABILITY
}

/// True when no function regressed and every new function meets the absolute
/// budget.
#[must_use]
pub fn passed(comparisons: &[Comparison]) -> bool {
    comparisons.iter().all(|comparison| match comparison.state {
        State::Same | State::Improved => true,
        State::Regressed => false,
        State::New => within_absolute_budget(&comparison.head),
    })
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

/// True when `head` stays within `limit_percent` of `base`.
///
/// `head` passes below `base` times (100 + `limit_percent`) / 100, or,
/// for a limit of zero or more, at most `base`. Under such a limit an
/// unchanged or smaller count always passes, even at 0 percent or a zero
/// base; a 5 percent limit passes a 4 percent increase and fails a 5 percent
/// one. A negative limit demands a reduction: -5 percent passes a 6 percent
/// reduction and fails a 5 percent one or an unchanged count.
///
/// Fails closed: false for `NaN` or infinite input, a negative count, or a
/// product too large to represent.
#[must_use]
pub const fn budget(base: f64, head: f64, limit_percent: f64) -> bool {
    if base < 0.0 || head < 0.0 {
        return false;
    }

    within_limit(base, head, limit_percent)
}

/// The rule of [`budget`] for counts it has already checked are not negative.
///
/// A NaN or infinite base or limit leaves `allowed` non-finite, and a NaN or
/// infinite head fails both comparisons, so neither needs its own guard.
const fn within_limit(base: f64, head: f64, limit_percent: f64) -> bool {
    let allowed = base * (100.0 + limit_percent);
    let no_increase = limit_percent >= 0.0 && head <= base;

    allowed.is_finite() && (no_increase || head * 100.0 < allowed)
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
    fn a_repeated_name_is_compared_by_its_worst_case() {
        let base = [metric("parse", BASE), metric("render", BASE)];
        let head = [
            metric("parse", [2.0, 4.0, 100.0, 120.0]),
            metric("render", BASE),
            metric("parse", [3.5, 1.0, 130.0, 109.0]),
        ];

        let compared = compare(&base, &head);

        let expected = vec![
            Comparison {
                key: "src/lib.rs:parse".to_owned(),
                base: Some(metric("parse", BASE)),
                head: metric("parse", [3.5, 4.0, 130.0, 109.0]),
                state: State::Regressed,
            },
            Comparison {
                key: "src/lib.rs:render".to_owned(),
                base: Some(metric("render", BASE)),
                head: metric("render", BASE),
                state: State::Same,
            },
        ];
        assert_eq!(compared, expected);
        assert!(!passed(&compared));
    }

    #[test]
    fn a_repeated_name_matches_whatever_its_order_or_count() {
        let better = metric("parse", [2.0, 1.0, 100.0, 111.0]);
        let base = [better.clone(), metric("parse", BASE)];
        let head = [metric("parse", BASE)];
        let reordered = [metric("parse", BASE), better.clone(), better];

        let once = compare(&base, &head);
        let thrice = compare(&base, &reordered);

        let expected = vec![Comparison {
            key: "src/lib.rs:parse".to_owned(),
            base: Some(metric("parse", BASE)),
            head: metric("parse", BASE),
            state: State::Same,
        }];
        assert_eq!(once, expected);
        assert_eq!(thrice, expected);
    }

    #[test]
    fn an_occurrence_that_is_not_a_number_regresses_its_name() {
        let cyclomatic = metric("parse", [f64::NAN, 2.0, 120.5, 110.25]);
        let maintainability = metric("parse", [3.0, 2.0, 120.5, f64::NAN]);
        let base = [metric("parse", BASE)];
        let heads = [
            [cyclomatic.clone(), metric("parse", BASE)],
            [metric("parse", BASE), cyclomatic],
            [maintainability.clone(), metric("parse", BASE)],
            [metric("parse", BASE), maintainability],
        ];

        let states: Vec<State> = heads
            .iter()
            .flat_map(|head| compare(&base, head))
            .map(|comparison| comparison.state)
            .collect();

        assert_eq!(states, [State::Regressed; 4]);
    }

    #[test]
    fn a_new_function_at_the_budget_limits_passes_and_a_regressed_one_fails() {
        let same = metric("parse", BASE);
        let worse = metric("parse", [4.0, 2.0, 120.5, 110.25]);
        let added = metric("render", [20.0, 30.0, 2000.0, 50.0]);

        let grown = compare(std::slice::from_ref(&same), &[same.clone(), added.clone()]);
        let regressed = compare(&[same], &[worse, added]);

        assert_eq!([passed(&grown), passed(&regressed)], [true, false]);
    }

    #[test]
    fn a_new_function_fails_when_any_measure_exceeds_its_budget() {
        let over_budget = [
            [21.0, 30.0, 2000.0, 50.0],
            [20.0, 31.0, 2000.0, 50.0],
            [20.0, 30.0, 2000.25, 50.0],
            [20.0, 30.0, 2000.0, 49.75],
        ];

        let verdicts = over_budget.map(|measures| {
            let compared = compare(&[], &[metric("parse", measures)]);
            passed(&compared)
        });

        assert_eq!(verdicts, [false; 4]);
    }

    #[test]
    fn a_new_function_fails_when_any_measure_is_not_a_number() {
        let unmeasurable = [
            [f64::NAN, 2.0, 120.5, 110.25],
            [3.0, f64::NAN, 120.5, 110.25],
            [3.0, 2.0, f64::NAN, 110.25],
            [3.0, 2.0, 120.5, f64::NAN],
        ];

        let verdicts = unmeasurable.map(|measures| {
            let compared = compare(&[], &[metric("parse", measures)]);
            passed(&compared)
        });

        assert_eq!(verdicts, [false; 4]);
    }

    #[test]
    fn an_existing_function_over_budget_still_passes_when_it_does_not_regress() {
        let base = [metric("parse", [22.0, 32.0, 2002.0, 48.0])];
        let better = [metric("parse", [21.0, 31.0, 2001.0, 49.0])];

        let same = compare(&base, &base);
        let improved = compare(&base, &better);

        assert_eq!([passed(&same), passed(&improved)], [true, true]);
    }

    #[test]
    fn states_read_as_lowercase_words() {
        let states = [State::Same, State::Improved, State::Regressed, State::New];

        let names: Vec<String> = states.iter().map(ToString::to_string).collect();

        assert_eq!(names, ["same", "improved", "regressed", "new"]);
    }

    #[test]
    fn a_five_percent_limit_accepts_four_and_refuses_five_and_six() {
        let verdicts = [
            budget(100.0, 104.0, 5.0),
            budget(100.0, 105.0, 5.0),
            budget(100.0, 106.0, 5.0),
        ];

        assert_eq!(verdicts, [true, false, false]);
    }

    #[test]
    fn an_unchanged_or_smaller_count_passes_at_a_zero_limit() {
        let verdicts = [
            budget(100.0, 100.0, 0.0),
            budget(100.0, 99.0, 0.0),
            budget(100.0, 101.0, 0.0),
        ];

        assert_eq!(verdicts, [true, true, false]);
    }

    #[test]
    fn a_negative_limit_requires_a_reduction_past_it() {
        let verdicts = [
            budget(100.0, 100.0, -5.0),
            budget(100.0, 99.0, -5.0),
            budget(100.0, 95.0, -5.0),
            budget(100.0, 94.0, -5.0),
        ];

        assert_eq!(verdicts, [false, false, false, true]);
    }

    #[test]
    fn a_zero_base_allows_zero_and_no_increase() {
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
