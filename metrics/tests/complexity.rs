//! Runs the built metrics binary on recorded measurements.
//!
//! These are process-boundary tests, run by the test gate. Miri checks the
//! library code and cannot spawn processes, so it skips them.

use std::env;
use std::fs;
use std::io;
use std::iter;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

/// The largest metrics file the binary reads, in bytes.
const MAX_FILE_BYTES: usize = 8_388_608;

/// The most metrics files the binary reads under one report directory.
const MAX_FILES: usize = 4096;

/// The most folders the binary reads under one report directory, itself
/// included.
const MAX_FOLDERS: usize = 1024;

/// The deepest folder the binary reads below a report directory.
const MAX_DEPTH: usize = 8;

const TABLE_HEAD: [&str; 2] = [
    "| function | state | cyclomatic | cognitive | halstead volume | maintainability |",
    "| --- | --- | --- | --- | --- | --- |",
];

const NEW_SAME: &str = "| src/lib.rs:Config::new | same | 1.00 -> 1.00 | 0.00 -> 0.00 \
    | 18.00 -> 18.00 | 140.50 -> 140.50 |";

const VALIDATE_SAME: &str = "| src/lib.rs:Config::validate | same | 4.00 -> 4.00 \
    | 5.00 -> 5.00 | 200.75 -> 200.75 | 95.50 -> 95.50 |";

const VALIDATE_IMPROVED: &str = "| src/lib.rs:Config::validate | improved | 4.00 -> 3.00 \
    | 5.00 -> 3.00 | 200.75 -> 180.25 | 95.50 -> 101.50 |";

const RESET_NEW: &str = "| src/lib.rs:Config::reset | new | - -> 1.00 | - -> 0.00 \
    | - -> 12.00 | - -> 150.75 |";

const PARSE_SAME: &str = "| src/lib.rs:parse | same | 3.00 -> 3.00 | 2.00 -> 2.00 \
    | 120.50 -> 120.50 | 110.25 -> 110.25 |";

const PARSE_REGRESSED: &str = "| src/lib.rs:parse | regressed | 3.00 -> 4.00 \
    | 2.00 -> 4.00 | 120.50 -> 150.50 | 110.25 -> 101.25 |";

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

/// The recorded fixture at `name`, such as `complexity/base`.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The base fixture's rust-code-analysis output for `src/lib.rs`.
fn base_output() -> io::Result<String> {
    fs::read_to_string(fixture("complexity/base/lib.json"))
}

fn complexity(base: &Path, head: &Path) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_atelier-metrics"))
        .arg("complexity")
        .arg("--base")
        .arg(base)
        .arg("--head")
        .arg(head)
        .output()
}

fn budget(base: &Path, head: &Path, limit: &str) -> io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_atelier-metrics"))
        .arg("budget")
        .arg("--base")
        .arg(base)
        .arg("--head")
        .arg(head)
        .args(["--limit", limit])
        .output()
}

fn stdout_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The table of a comparison: its heading, `rows`, then `verdict`.
fn table(rows: &[&str], verdict: &str) -> Vec<String> {
    TABLE_HEAD
        .iter()
        .chain(rows)
        .chain(iter::once(&verdict))
        .map(|line| (*line).to_owned())
        .collect()
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_passes_when_nothing_regressed() {
    let base = fixture("complexity/base");
    let head = fixture("complexity/improved");

    let output = complexity(&base, &head).expect("run the metrics binary");

    let rows = [NEW_SAME, VALIDATE_IMPROVED, PARSE_SAME];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_lines(&output), table(&rows, "complexity: passed"));
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_fails_on_a_regressed_function() {
    let base = fixture("complexity/base");
    let head = fixture("complexity/head");

    let output = complexity(&base, &head).expect("run the metrics binary");

    let rows = [NEW_SAME, VALIDATE_IMPROVED, RESET_NEW, PARSE_REGRESSED];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_lines(&output), table(&rows, "complexity: failed"));
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_passes_with_new_functions_within_budget_and_skips_other_files() {
    let head = scratch("new").expect("scratch folder");
    let extra = base_output()
        .expect("read the base fixture")
        .replace("src/lib.rs", "src/extra.rs");
    let improved = fixture("complexity/improved/lib.json");

    fs::write(head.join("extra.json"), extra).expect("write extra.json");
    fs::copy(improved, head.join("lib.json")).expect("copy lib.json");
    fs::write(head.join("notes.txt"), "not json").expect("write notes.txt");

    let output = complexity(&fixture("complexity/base"), &head).expect("run the metrics binary");

    let rows = [
        "| src/extra.rs:Config::new | new | - -> 1.00 | - -> 0.00 | - -> 18.00 \
         | - -> 140.50 |",
        "| src/extra.rs:Config::validate | new | - -> 4.00 | - -> 5.00 | - -> 200.75 \
         | - -> 95.50 |",
        "| src/extra.rs:parse | new | - -> 3.00 | - -> 2.00 | - -> 120.50 | - -> 110.25 |",
        NEW_SAME,
        VALIDATE_IMPROVED,
        PARSE_SAME,
    ];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_lines(&output), table(&rows, "complexity: passed"));

    fs::remove_dir_all(&head).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_fails_with_a_new_function_over_budget() {
    let base = fixture("complexity/base");
    let head = scratch("over-budget").expect("scratch folder");
    let extra = serde_json::json!({
        "name": "src/extra.rs",
        "kind": "unit",
        "spaces": [{
            "name": "parse",
            "kind": "function",
            "metrics": {
                "cyclomatic": {"sum": 21},
                "cognitive": {"sum": 30},
                "halstead": {"volume": 2000},
                "mi": {"mi_original": 50}
            },
            "spaces": []
        }]
    });

    fs::write(head.join("extra.json"), extra.to_string()).expect("write extra.json");
    fs::copy(base.join("lib.json"), head.join("lib.json")).expect("copy lib.json");

    let output = complexity(&base, &head).expect("run the metrics binary");

    let rows = [
        "| src/extra.rs:parse | new | - -> 21.00 | - -> 30.00 \
         | - -> 2000.00 | - -> 50.00 |",
        NEW_SAME,
        VALIDATE_SAME,
        PARSE_SAME,
    ];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_lines(&output), table(&rows, "complexity: failed"));

    fs::remove_dir_all(&head).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_reads_an_array_of_units() {
    let head = scratch("array").expect("scratch folder");
    let improved = fs::read_to_string(fixture("complexity/improved/lib.json"))
        .expect("read the improved fixture");

    fs::write(head.join("units.json"), format!("[{improved}]")).expect("write units.json");

    let output = complexity(&fixture("complexity/base"), &head).expect("run the metrics binary");

    let rows = [NEW_SAME, VALIDATE_IMPROVED, PARSE_SAME];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_lines(&output), table(&rows, "complexity: passed"));

    fs::remove_dir_all(&head).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_reads_reports_in_nested_folders() {
    let dir = scratch("nested").expect("scratch folder");
    let report = Path::new("crates/app/src/lib.rs.json");
    let base = dir.join("base");
    let head = dir.join("head");
    let sources = [
        (&base, fixture("complexity/base/lib.json")),
        (&head, fixture("complexity/improved/lib.json")),
    ];

    for (folder, source) in sources {
        let path = folder.join(report);
        let parent = path.parent().expect("a report folder");

        fs::create_dir_all(parent).expect("make the report folders");
        fs::copy(source, &path).expect("copy the nested report");
    }

    let output = complexity(&base, &head).expect("run the metrics binary");

    let rows = [NEW_SAME, VALIDATE_IMPROVED, PARSE_SAME];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_lines(&output), table(&rows, "complexity: passed"));

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_reads_a_folder_at_the_depth_limit_and_refuses_one_past_it() {
    let dir = scratch("deep").expect("scratch folder");
    let chain: PathBuf = (1..=MAX_DEPTH).map(|depth| depth.to_string()).collect();
    let at_limit = dir.join("at");
    let past_limit = dir.join("past");
    let deepest = past_limit.join(&chain).join("deeper");
    let report = base_output().expect("read the base fixture");

    fs::create_dir_all(at_limit.join(&chain)).expect("make the at-limit folders");
    fs::write(at_limit.join(&chain).join("lib.json"), report).expect("write the at-limit report");
    fs::create_dir_all(&deepest).expect("make the past-limit folders");

    let read = complexity(&fixture("complexity/base"), &at_limit).expect("run at the limit");
    let refused = complexity(&fixture("complexity/base"), &past_limit).expect("run past it");

    let rows = [NEW_SAME, VALIDATE_SAME, PARSE_SAME];
    assert_eq!(stderr(&read), "");
    assert_eq!(read.status.code(), Some(0));
    assert_eq!(stdout_lines(&read), table(&rows, "complexity: passed"));
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(stdout_lines(&refused), Vec::<String>::new());
    assert_eq!(
        stderr(&refused),
        format!("{}: folder nested too deep\n", deepest.display())
    );

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_reads_folders_at_the_limit_and_refuses_one_past_it() {
    let dir = scratch("folders").expect("scratch folder");
    let at_limit = dir.join("at");
    let past_limit = dir.join("past");
    let report = base_output().expect("read the base fixture");

    for folder in [&at_limit, &past_limit] {
        fs::create_dir(folder).expect("make a report folder");
        fs::write(folder.join("lib.json"), &report).expect("write lib.json");

        for index in 1..MAX_FOLDERS {
            fs::create_dir(folder.join(index.to_string())).expect("make a folder");
        }
    }

    fs::create_dir(past_limit.join("extra")).expect("make the folder past the limit");

    let read = complexity(&fixture("complexity/base"), &at_limit).expect("run at the limit");
    let refused = complexity(&fixture("complexity/base"), &past_limit).expect("run past it");

    let rows = [NEW_SAME, VALIDATE_SAME, PARSE_SAME];
    assert_eq!(stderr(&read), "");
    assert_eq!(read.status.code(), Some(0));
    assert_eq!(stdout_lines(&read), table(&rows, "complexity: passed"));
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(stdout_lines(&refused), Vec::<String>::new());
    assert_eq!(
        stderr(&refused),
        format!("{}: too many folders\n", past_limit.display())
    );

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_refuses_a_missing_directory() {
    let dir = scratch("missing").expect("scratch folder");
    let absent = dir.join("absent");

    let output = complexity(&fixture("complexity/base"), &absent).expect("run the metrics binary");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_lines(&output), Vec::<String>::new());
    assert_eq!(
        stderr(&output),
        format!("{}: missing directory\n", absent.display())
    );

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_refuses_malformed_json() {
    let head = scratch("malformed").expect("scratch folder");
    let file = head.join("lib.json");
    fs::write(&file, r#"{"name":"#).expect("write lib.json");

    let output = complexity(&fixture("complexity/base"), &head).expect("run the metrics binary");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_lines(&output), Vec::<String>::new());
    assert_eq!(
        stderr(&output),
        format!("{}: malformed json\n", file.display())
    );

    fs::remove_dir_all(&head).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_refuses_a_missing_metric_field() {
    let head = scratch("field").expect("scratch folder");
    let file = head.join("lib.json");
    let without = base_output()
        .expect("read the base fixture")
        .replace("mi_original", "mi_unknown");
    fs::write(&file, without).expect("write lib.json");

    let output = complexity(&fixture("complexity/base"), &head).expect("run the metrics binary");

    let expected = format!(
        "{}: missing metric field metrics.mi.mi_original\n",
        file.display()
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_lines(&output), Vec::<String>::new());
    assert_eq!(stderr(&output), expected);

    fs::remove_dir_all(&head).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_lists_a_repeated_name_once() {
    let head = scratch("repeated").expect("scratch folder");
    let base = fixture("complexity/base/lib.json");

    fs::copy(&base, head.join("a.json")).expect("copy a.json");
    fs::copy(&base, head.join("b.json")).expect("copy b.json");

    let output = complexity(&fixture("complexity/base"), &head).expect("run the metrics binary");

    let rows = [NEW_SAME, VALIDATE_SAME, PARSE_SAME];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_lines(&output), table(&rows, "complexity: passed"));

    fs::remove_dir_all(&head).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_compares_the_worst_case_of_a_repeated_name() {
    let dir = scratch("repeated-regressed").expect("scratch folder");
    let base = dir.join("base");
    let head = dir.join("head");
    let unchanged = fixture("complexity/base/lib.json");
    let changed = fixture("complexity/head/lib.json");
    let copies = [
        (&base, "a.json", &unchanged),
        (&base, "b.json", &unchanged),
        (&head, "a.json", &unchanged),
        (&head, "b.json", &changed),
    ];

    for folder in [&base, &head] {
        fs::create_dir(folder).expect("make a report folder");
    }

    for (folder, name, source) in copies {
        fs::copy(source, folder.join(name)).expect("copy a report");
    }

    let output = complexity(&base, &head).expect("run the metrics binary");

    let rows = [NEW_SAME, VALIDATE_SAME, PARSE_REGRESSED, RESET_NEW];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_lines(&output), table(&rows, "complexity: failed"));

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_reads_a_file_at_the_limit_and_refuses_one_past_it() {
    let dir = scratch("oversized").expect("scratch folder");
    let at_limit = dir.join("at");
    let past_limit = dir.join("past");
    let mut padded = base_output().expect("read the base fixture").into_bytes();

    padded.resize(MAX_FILE_BYTES, b' ');
    fs::create_dir(&at_limit).expect("make the at-limit folder");
    fs::write(at_limit.join("lib.json"), &padded).expect("write the at-limit file");

    padded.push(b' ');
    fs::create_dir(&past_limit).expect("make the past-limit folder");
    fs::write(past_limit.join("lib.json"), &padded).expect("write the past-limit file");

    let read = complexity(&fixture("complexity/base"), &at_limit).expect("run at the limit");
    let refused = complexity(&fixture("complexity/base"), &past_limit).expect("run past it");

    let rows = [NEW_SAME, VALIDATE_SAME, PARSE_SAME];
    let oversized = format!(
        "{}: oversized input\n",
        past_limit.join("lib.json").display()
    );
    assert_eq!(stderr(&read), "");
    assert_eq!(read.status.code(), Some(0));
    assert_eq!(stdout_lines(&read), table(&rows, "complexity: passed"));
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(stderr(&refused), oversized);

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_refuses_too_many_files() {
    let head = scratch("many").expect("scratch folder");

    for index in 0..=MAX_FILES {
        fs::write(head.join(format!("{index}.json")), "").expect("write a json file");
    }

    let output = complexity(&fixture("complexity/base"), &head).expect("run the metrics binary");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_lines(&output), Vec::<String>::new());
    assert_eq!(
        stderr(&output),
        format!("{}: too many json files\n", head.display())
    );

    fs::remove_dir_all(&head).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_accepts_const_only_reports_beside_a_report_with_functions() {
    let head = scratch("const-only").expect("scratch folder");
    let improved = fixture("complexity/improved/lib.json");
    let constants = r#"{"name":"src/constants.rs","kind":"unit","spaces":[]}"#;
    let array = r#"[{
        "name":"src/limits.rs","kind":"unit","spaces":[],"error":null,"errors":[]
    }]"#;

    fs::copy(improved, head.join("lib.json")).expect("copy lib.json");
    fs::write(head.join("constants.json"), constants).expect("write the unit report");
    fs::write(head.join("array.json"), array).expect("write the array report");

    let output = complexity(&fixture("complexity/base"), &head).expect("run the metrics binary");

    let rows = [NEW_SAME, VALIDATE_IMPROVED, PARSE_SAME];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_lines(&output), table(&rows, "complexity: passed"));

    fs::remove_dir_all(&head).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_passes_when_all_head_reports_are_functionless() {
    let head = scratch("functionless").expect("scratch folder");
    let modules = r#"{
        "name":"src/lib.rs","kind":"unit",
        "spaces":[{"name":"constants","kind":"module","spaces":[]}]
    }"#;
    let constants = r#"{"name":"src/constants.rs","kind":"unit","spaces":[]}"#;

    fs::write(head.join("lib.json"), modules).expect("write lib.json");
    fs::write(head.join("constants.json"), constants).expect("write constants.json");

    let output = complexity(&fixture("complexity/base"), &head).expect("run the metrics binary");

    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_lines(&output), table(&[], "complexity: passed"));

    fs::remove_dir_all(&head).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_refuses_reports_without_a_top_level_unit() {
    let head = scratch("no-unit").expect("scratch folder");
    let file = head.join("extra.json");
    let base = fixture("complexity/base");
    let cases = [
        "{}",
        "[]",
        r#"{"name":"src/extra.rs","kind":"module","spaces":[]}"#,
    ];

    fs::copy(base.join("lib.json"), head.join("lib.json")).expect("copy lib.json");

    for report in cases {
        fs::write(&file, report).expect("write extra.json");

        let output = complexity(&base, &head).expect("run the metrics binary");

        assert_eq!(output.status.code(), Some(1), "{report}");
        assert_eq!(stdout_lines(&output), Vec::<String>::new(), "{report}");
        assert_eq!(
            stderr(&output),
            format!("{}: incomplete analysis report\n", file.display()),
            "{report}"
        );
    }

    fs::remove_dir_all(&head).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn complexity_refuses_reports_with_parse_errors_even_with_a_unit() {
    let head = scratch("parse-error").expect("scratch folder");
    let file = head.join("lib.json");
    let base = fixture("complexity/base");
    let unit = base_output().expect("read the base fixture");
    let errors = [
        format!(r#"[{unit},{{"error":"parse failed"}}]"#),
        unit.replacen(
            r#""name": "src/lib.rs""#,
            r#""errors": ["parse failed"], "name": "src/lib.rs""#,
            1,
        ),
        unit.replacen(
            r#""name": "src/lib.rs""#,
            r#""error": {"message": "parse failed"}, "name": "src/lib.rs""#,
            1,
        ),
    ];

    for report in errors {
        fs::write(&file, report).expect("write lib.json");

        let output = complexity(&base, &head).expect("run the metrics binary");

        assert_eq!(output.status.code(), Some(1));
        assert_eq!(stdout_lines(&output), Vec::<String>::new());
        assert_eq!(
            stderr(&output),
            format!("{}: incomplete analysis report\n", file.display())
        );
    }

    fs::remove_dir_all(&head).expect("remove the scratch folder");
}

#[cfg(unix)]
#[test]
#[cfg_attr(miri, ignore)]
fn complexity_skips_symlinks() {
    let head = scratch("symlink").expect("scratch folder");
    let link = head.join("lib.json");
    std::os::unix::fs::symlink(fixture("complexity/base/lib.json"), &link).expect("make symlink");

    let output = complexity(&fixture("complexity/base"), &head).expect("run the metrics binary");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_lines(&output), Vec::<String>::new());
    assert_eq!(
        stderr(&output),
        format!("{}: no json files\n", head.display())
    );

    fs::remove_dir_all(&head).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn budget_passes_within_the_limit() {
    let base = fixture("budget/base.json");
    let head = fixture("budget/head.json");

    let output = budget(&base, &head, "5").expect("run the metrics binary");

    let expected = [
        "instructions before: 1000000",
        "instructions after: 1040000",
        "limit: +5%",
        "budget: passed",
    ];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_lines(&output), expected);
}

#[test]
#[cfg_attr(miri, ignore)]
fn budget_passes_an_unchanged_count_at_a_zero_limit() {
    let base = fixture("budget/base.json");

    let output = budget(&base, &base, "0").expect("run the metrics binary");

    let expected = [
        "instructions before: 1000000",
        "instructions after: 1000000",
        "limit: +0%",
        "budget: passed",
    ];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_lines(&output), expected);
}

#[test]
#[cfg_attr(miri, ignore)]
fn budget_fails_at_the_limit() {
    let dir = scratch("at-limit").expect("scratch folder");
    let base = fixture("budget/base.json");
    let head = dir.join("head.json");
    fs::write(&head, r#"{"ir": 1050000}"#).expect("write head.json");

    let output = budget(&base, &head, "5").expect("run the metrics binary");

    let expected = [
        "instructions before: 1000000",
        "instructions after: 1050000",
        "limit: +5%",
        "budget: failed",
    ];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_lines(&output), expected);

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn budget_fails_past_the_limit() {
    let base = fixture("budget/base.json");
    let head = fixture("budget/head.json");

    let output = budget(&base, &head, "3").expect("run the metrics binary");

    let expected = [
        "instructions before: 1000000",
        "instructions after: 1040000",
        "limit: +3%",
        "budget: failed",
    ];
    assert_eq!(stderr(&output), "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_lines(&output), expected);
}

#[test]
#[cfg_attr(miri, ignore)]
fn budget_requires_a_reduction_past_a_negative_limit() {
    let dir = scratch("negative-limit").expect("scratch folder");
    let base = fixture("budget/base.json");
    let reduced = dir.join("reduced.json");
    let barely = dir.join("barely.json");
    fs::write(&reduced, r#"{"ir": 940000}"#).expect("write reduced.json");
    fs::write(&barely, r#"{"ir": 950000}"#).expect("write barely.json");

    let cases = [
        (
            &base,
            "instructions after: 1000000",
            Some(1),
            "budget: failed",
        ),
        (
            &barely,
            "instructions after: 950000",
            Some(1),
            "budget: failed",
        ),
        (
            &reduced,
            "instructions after: 940000",
            Some(0),
            "budget: passed",
        ),
    ];

    for (head, after, status, verdict) in cases {
        let output = budget(&base, head, "-5").expect("run the metrics binary");

        let expected = ["instructions before: 1000000", after, "limit: -5%", verdict];
        assert_eq!(stderr(&output), "");
        assert_eq!(output.status.code(), status);
        assert_eq!(stdout_lines(&output), expected);
    }

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}

#[test]
#[cfg_attr(miri, ignore)]
fn budget_refuses_a_count_it_cannot_read() {
    let dir = scratch("count").expect("scratch folder");
    let base = fixture("budget/base.json");
    let unnamed = dir.join("unnamed.json");
    let malformed = dir.join("malformed.json");
    let absent = dir.join("absent.json");

    fs::write(&unnamed, r#"{"instructions": 1040000}"#).expect("write unnamed.json");
    fs::write(&malformed, r#"{"ir": "#).expect("write malformed.json");

    let refusals = [
        (unnamed, "missing metric field ir"),
        (malformed, "malformed json"),
        (absent, "missing regular file"),
    ];

    for (head, reason) in &refusals {
        let output = budget(&base, head, "5").expect("run the metrics binary");

        assert_eq!(output.status.code(), Some(1));
        assert_eq!(stdout_lines(&output), Vec::<String>::new());
        assert_eq!(stderr(&output), format!("{}: {reason}\n", head.display()));
    }

    fs::remove_dir_all(&dir).expect("remove the scratch folder");
}
