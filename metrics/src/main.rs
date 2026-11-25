//! Compares the recorded measurements of a base and a head version and
//! prints a before and after table: function complexity from
//! rust-code-analysis, and instruction counts against a budget. It fails on
//! a regression and on input it cannot trust. It writes no files; the job
//! appends its output to the summary.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use atelier_metrics::{Comparison, FunctionMetric, budget, compare, passed};
use clap::{Parser, Subcommand};
use serde_json::Value;

/// The largest metrics file read, in bytes.
const MAX_FILE_BYTES: u64 = 8_388_608;

/// The most metrics files read under one report directory, its folders
/// included.
const MAX_FILES: usize = 4096;

/// The most folders read under one report directory, itself included.
const MAX_FOLDERS: usize = 1024;

/// The deepest folder read below a report directory, which is depth 0.
/// rust-code-analysis mirrors the source tree, so `src/lib.rs` is reported
/// at `src/lib.rs.json`.
const MAX_DEPTH: usize = 8;

/// The largest instruction count file read, in bytes.
const MAX_COUNT_BYTES: u64 = 65_536;

/// The name given to a space rust-code-analysis could not name.
const ANONYMOUS: &str = "<anonymous>";

/// The complexity table's heading and divider.
const TABLE_HEAD: &str = concat!(
    "| function | state | cyclomatic | cognitive | halstead volume | maintainability |\n",
    "| --- | --- | --- | --- | --- | --- |",
);

/// Compare the measurements of a base and a head version.
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Compare function complexity; fails only when a function regressed.
    Complexity {
        /// Directory of rust-code-analysis JSON for the base version.
        #[arg(long, value_name = "DIR")]
        base: PathBuf,

        /// Directory of rust-code-analysis JSON for the head version.
        #[arg(long, value_name = "DIR")]
        head: PathBuf,
    },

    /// Compare instruction counts; fails past the limit.
    Budget {
        /// JSON with the base version's count, such as `{"ir": 1000}`.
        #[arg(long, value_name = "FILE")]
        base: PathBuf,

        /// JSON with the head version's count.
        #[arg(long, value_name = "FILE")]
        head: PathBuf,

        /// The largest increase allowed, in percent, such as 5.
        #[arg(long, value_name = "PERCENT", allow_negative_numbers = true)]
        limit: f64,
    },
}

/// Why input was refused.
#[derive(Debug)]
enum Reason {
    /// The path is not a directory: absent, a file or a symlink.
    MissingDirectory,
    /// The path is not a regular file: absent, a directory or a symlink.
    MissingFile,
    NoFiles,
    /// The head report holds no function, so nothing was compared.
    NoFunctions,
    TooManyFiles,
    TooManyFolders,
    /// A folder lies more than `MAX_DEPTH` folders below the report.
    TooDeep,
    Oversized,
    Unreadable(io::ErrorKind),
    Malformed,
    MissingField(&'static str),
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDirectory => f.write_str("missing directory"),
            Self::MissingFile => f.write_str("missing regular file"),
            Self::NoFiles => f.write_str("no json files"),
            Self::NoFunctions => f.write_str("no functions to compare"),
            Self::TooManyFiles => f.write_str("too many json files"),
            Self::TooManyFolders => f.write_str("too many folders"),
            Self::TooDeep => f.write_str("folder nested too deep"),
            Self::Oversized => f.write_str("oversized input"),
            Self::Unreadable(kind) => write!(f, "unreadable input: {kind}"),
            Self::Malformed => f.write_str("malformed json"),
            Self::MissingField(field) => write!(f, "missing metric field {field}"),
        }
    }
}

/// A refused path and the reason.
#[derive(Debug)]
struct Refusal {
    path: PathBuf,
    reason: Reason,
}

impl Refusal {
    fn new(path: &Path, reason: Reason) -> Self {
        Self {
            path: path.to_owned(),
            reason,
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.reason)
    }
}

/// Fails when a comparison failed or its input was refused.
fn main() -> ExitCode {
    let compared = match Args::parse().command {
        Command::Complexity { base, head } => complexity(&base, &head),
        Command::Budget { base, head, limit } => instructions(&base, &head, limit),
    };

    match compared {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(refusal) => {
            eprintln!("{refusal}");
            ExitCode::FAILURE
        }
    }
}

/// Prints a row for each head function; true when none regressed. A new
/// function is listed as new and passes.
/// An empty comparison is refused, as it would pass having checked nothing.
fn complexity(base: &Path, head: &Path) -> Result<bool, Refusal> {
    let before = functions(base)?;
    let after = functions(head)?;
    let comparisons = compare(&before, &after);

    if comparisons.is_empty() {
        return Err(Refusal::new(head, Reason::NoFunctions));
    }

    println!("{TABLE_HEAD}");

    for comparison in &comparisons {
        println!("{}", row(comparison));
    }

    let passing = passed(&comparisons);

    println!("complexity: {}", verdict(passing));
    Ok(passing)
}

/// One table row: each measure as base -> head, with `-` for a new function.
fn row(comparison: &Comparison) -> String {
    let pair = |measure: fn(&FunctionMetric) -> f64| {
        let before = comparison
            .base
            .as_ref()
            .map_or_else(|| "-".to_owned(), |base| format!("{:.2}", measure(base)));

        format!("{before} -> {:.2}", measure(&comparison.head))
    };

    format!(
        "| {} | {} | {} | {} | {} | {} |",
        comparison.key,
        comparison.state,
        pair(|metric| metric.cyclomatic),
        pair(|metric| metric.cognitive),
        pair(|metric| metric.halstead_volume),
        pair(|metric| metric.maintainability),
    )
}

/// Prints both counts and the limit; true when head is within the limit.
fn instructions(base: &Path, head: &Path, limit: f64) -> Result<bool, Refusal> {
    let before = count(base)?;
    let after = count(head)?;
    let passed = budget(before, after, limit);

    println!("instructions before: {before}");
    println!("instructions after: {after}");
    println!("limit: {limit:+}%");
    println!("budget: {}", verdict(passed));

    Ok(passed)
}

const fn verdict(passed: bool) -> &'static str {
    if passed { "passed" } else { "failed" }
}

/// The instruction count `ir` recorded in the JSON file at `path`.
fn count(path: &Path) -> Result<f64, Refusal> {
    let recorded = read_json(path, MAX_COUNT_BYTES)?;

    recorded
        .get("ir")
        .and_then(Value::as_f64)
        .ok_or_else(|| Refusal::new(path, Reason::MissingField("ir")))
}

/// Every function in the JSON files under `dir` and its folders, in path
/// order. A name listed more than once, such as a nested anonymous item,
/// keeps one entry per occurrence, which `keys` numbers for comparison.
fn functions(dir: &Path) -> Result<Vec<FunctionMetric>, Refusal> {
    let mut found = Vec::new();

    for file in json_files(dir)? {
        let output = read_json(&file, MAX_FILE_BYTES)?;
        flatten_file(&output, &mut found).map_err(|reason| Refusal::new(&file, reason))?;
    }

    Ok(found)
}

/// The regular `.json` files under `dir` and its folders, sorted by path so
/// the table's order does not depend on the file system. Symlinks are
/// neither followed nor read.
fn json_files(dir: &Path) -> Result<Vec<PathBuf>, Refusal> {
    let refused = |reason: Reason| Refusal::new(dir, reason);
    let metadata = fs::symlink_metadata(dir).map_err(|_| refused(Reason::MissingDirectory))?;

    if !metadata.is_dir() {
        return Err(refused(Reason::MissingDirectory));
    }

    let mut walk = Walk::new(dir);

    while let Some(folder) = walk.pending.pop() {
        walk.read(&folder)?;
    }

    let mut files = walk.files;

    if files.is_empty() {
        return Err(refused(Reason::NoFiles));
    }

    files.sort();
    Ok(files)
}

/// A folder waiting to be read, and how many folders below the report it is.
struct Folder {
    path: PathBuf,
    depth: usize,
}

/// A depth-first walk of a report directory. It queues at most
/// `MAX_FOLDERS` folders, none deeper than `MAX_DEPTH`, and keeps at most
/// `MAX_FILES` json files, refusing the report past any of them.
struct Walk<'a> {
    root: &'a Path,
    pending: Vec<Folder>,
    folders_left: usize,
    files: Vec<PathBuf>,
}

impl<'a> Walk<'a> {
    /// A walk that starts at `root`, which counts as the first folder.
    fn new(root: &'a Path) -> Self {
        let start = Folder {
            path: root.to_owned(),
            depth: 0,
        };

        Self {
            root,
            pending: vec![start],
            folders_left: MAX_FOLDERS.saturating_sub(1),
            files: Vec::new(),
        }
    }

    /// Queues the folders in `folder` and keeps its regular `.json` files.
    fn read(&mut self, folder: &Folder) -> Result<(), Refusal> {
        let unreadable =
            |error: io::Error| Refusal::new(&folder.path, Reason::Unreadable(error.kind()));

        for entry in fs::read_dir(&folder.path).map_err(unreadable)? {
            let entry = entry.map_err(unreadable)?;
            let kind = entry.file_type().map_err(unreadable)?;
            let path = entry.path();
            let json = path
                .extension()
                .is_some_and(|extension| extension == "json");

            if kind.is_dir() {
                self.queue(path, folder.depth)?;
            } else if kind.is_file() && json {
                self.keep(path)?;
            }
        }

        Ok(())
    }

    /// Queues a folder found in a folder at depth `parent`.
    fn queue(&mut self, path: PathBuf, parent: usize) -> Result<(), Refusal> {
        let depth = parent.saturating_add(1);

        if depth > MAX_DEPTH {
            return Err(Refusal::new(&path, Reason::TooDeep));
        }

        self.folders_left = self
            .folders_left
            .checked_sub(1)
            .ok_or_else(|| Refusal::new(self.root, Reason::TooManyFolders))?;

        self.pending.push(Folder { path, depth });
        Ok(())
    }

    fn keep(&mut self, path: PathBuf) -> Result<(), Refusal> {
        if self.files.len() >= MAX_FILES {
            return Err(Refusal::new(self.root, Reason::TooManyFiles));
        }

        self.files.push(path);
        Ok(())
    }
}

/// Reads and parses the JSON file at `path`, refusing anything past
/// `max_bytes` or that is not a regular file.
fn read_json(path: &Path, max_bytes: u64) -> Result<Value, Refusal> {
    let refused = |reason: Reason| Refusal::new(path, reason);
    let raw = read_bounded(path, max_bytes).map_err(refused)?;

    serde_json::from_slice(&raw).map_err(|_| refused(Reason::Malformed))
}

/// Reads at most one byte past `max_bytes`, so an oversized file is never
/// held in memory whole. A symlink is refused, as `symlink_metadata` does
/// not follow it.
fn read_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>, Reason> {
    let unreadable = |error: io::Error| Reason::Unreadable(error.kind());
    let metadata = fs::symlink_metadata(path).map_err(|_| Reason::MissingFile)?;

    if !metadata.is_file() {
        return Err(Reason::MissingFile);
    }

    let file = File::open(path).map_err(unreadable)?;
    let mut raw = Vec::new();

    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut raw)
        .map_err(unreadable)?;

    let size = u64::try_from(raw.len()).map_err(|_| Reason::Oversized)?;

    if size > max_bytes {
        return Err(Reason::Oversized);
    }

    Ok(raw)
}

/// Adds the functions of one rust-code-analysis output: a unit space for a
/// source file, or an array of them.
fn flatten_file(output: &Value, found: &mut Vec<FunctionMetric>) -> Result<(), Reason> {
    match output {
        Value::Array(units) => units.iter().try_for_each(|unit| flatten_unit(unit, found)),
        unit => flatten_unit(unit, found),
    }
}

/// Adds the functions in a unit space, whose name is the source file.
fn flatten_unit(unit: &Value, found: &mut Vec<FunctionMetric>) -> Result<(), Reason> {
    let file = text(unit, "name")?;
    let mut scope = Vec::new();

    spaces(unit)?
        .iter()
        .try_for_each(|space| flatten_space(space, file, &mut scope, found))
}

/// Adds `space` when it is a function, then the functions nested in it.
///
/// `scope` holds the names of the enclosing spaces, so `new` in two impls
/// becomes `Config::new` and `Parser::new`. The recursion is bounded by
/// `serde_json`'s nesting limit of 128.
fn flatten_space<'a>(
    space: &'a Value,
    file: &str,
    scope: &mut Vec<&'a str>,
    found: &mut Vec<FunctionMetric>,
) -> Result<(), Reason> {
    let name = space.get("name").and_then(Value::as_str);

    if text(space, "kind")? == "function" {
        let name = name.ok_or(Reason::MissingField("name"))?;
        found.push(function(space, file, &qualified(scope, name))?);
    }

    scope.push(name.unwrap_or(ANONYMOUS));

    let nested = spaces(space)?
        .iter()
        .try_for_each(|inner| flatten_space(inner, file, scope, found));

    scope.pop();
    nested
}

fn qualified(scope: &[&str], name: &str) -> String {
    let mut parts = scope.to_vec();
    parts.push(name);

    parts.join("::")
}

/// The measures of a function space.
fn function(space: &Value, file: &str, name: &str) -> Result<FunctionMetric, Reason> {
    Ok(FunctionMetric {
        name: name.to_owned(),
        file: file.to_owned(),
        cyclomatic: number(space, "metrics.cyclomatic.sum")?,
        cognitive: number(space, "metrics.cognitive.sum")?,
        halstead_volume: number(space, "metrics.halstead.volume")?,
        maintainability: number(space, "metrics.mi.mi_original")?,
    })
}

/// The number at a dotted `path` under `space`, such as
/// `metrics.cyclomatic.sum`.
fn number(space: &Value, path: &'static str) -> Result<f64, Reason> {
    path.split('.')
        .try_fold(space, |value, key| value.get(key))
        .and_then(Value::as_f64)
        .ok_or(Reason::MissingField(path))
}

fn text<'a>(space: &'a Value, field: &'static str) -> Result<&'a str, Reason> {
    space
        .get(field)
        .and_then(Value::as_str)
        .ok_or(Reason::MissingField(field))
}

fn spaces(space: &Value) -> Result<&[Value], Reason> {
    space
        .get("spaces")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or(Reason::MissingField("spaces"))
}
