//! Running a step's command in the background, so a poll never waits on
//! it, and reading how it ended on a later poll.
//!
//! Each start runs in its own folder, which holds what a later poll reads:
//! `started`, the time it started; `worktree`, where it runs; `identity`,
//! the record of its leader; `output`, what it printed; and `exit`, its
//! exit status once it ends. Its process group is not the daemon's, so a
//! daemon restarted while it runs leaves it running.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use atelier_command::{Signal, signal_group};
use atelier_core::{Outcome, PluginError, Settings};

use super::DEFAULT_TIMEOUT_SECS;
use super::identity::{Identity, Leader};
use crate::log::log;

/// Runs the command: `$1` is the command and `$2` the worktree. The outer
/// shell is the group leader and stays alive until the work ends, so the
/// group lasts as long as the command does. The status file is renamed into
/// place, so a poll never reads it half written.
const SCRIPT: &str = r#"here=$PWD
( cd "$2" && exec sh -c "$1" ) < /dev/null > output 2>&1
echo $? > "$here/exit.tmp"
mv "$here/exit.tmp" "$here/exit"
"#;

/// The most of a failed command's output its outcome tells: its end.
const TAIL_BYTES: u64 = 4096;

/// How many of those last lines the outcome keeps.
const TAIL_LINES: usize = 20;

/// The unsignalled lines the log already has, with the reason each gave,
/// so a poll that finds the same record does not repeat one.
static UNSIGNED: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());

/// Starts the step's command in `worktree`, keeping what later polls read
/// in `folder`, emptied first, so a round started again starts clean. A
/// start repeated on a folder whose record still names a command stops it
/// first, so a daemon restarted between a start and the run state it could
/// not save cannot orphan what it started.
/// `ATELIER_BASE` names `origin/<base>`, as for the gates.
///
/// # Errors
///
/// The command an earlier start left in `folder` cannot be stopped or
/// cannot be confirmed gone because its record still names a leader that
/// is stopping, the folder cannot be made, `sh` cannot start, or the
/// leader's identity cannot be recorded; all transient.
pub fn start(
    folder: &Path,
    worktree: &Path,
    base: &str,
    settings: &Settings,
) -> Result<(), PluginError> {
    let run = settings.text("", "run").ok().flatten().unwrap_or_default();
    let failed = |what: &str, error: std::io::Error| {
        PluginError::transient(format!("{what} {}: {error}", folder.display()))
    };

    if folder.exists() {
        clear(folder)?;
    }

    fs::create_dir_all(folder).map_err(|error| failed("making", error))?;
    fs::write(folder.join("started"), now().to_string())
        .map_err(|error| failed("writing the start time in", error))?;
    fs::write(folder.join("worktree"), worktree.display().to_string())
        .map_err(|error| failed("writing the worktree in", error))?;

    let mut child = spawn(folder, worktree, base, run).map_err(|error| {
        // Nothing ran, so a folder kept here would refuse every later start
        // for nothing.
        let _ = fs::remove_dir_all(folder);

        failed("starting a command in", error)
    })?;
    let leader = child.id();
    let recorded = Identity::of(leader).and_then(|identity| publish(folder, &identity.render()));

    if let Err(why) = recorded {
        abandon(folder, &mut child);

        return Err(PluginError::transient(format!(
            "the command in {} could not record its identity: {why}",
            folder.display()
        )));
    }

    reap(child);

    Ok(())
}

/// Starts the step's command in `folder`, in a process group of its own, so
/// a later poll can signal that group and the daemon's group is left alone.
/// `$1` is the command and `$2` the worktree, as `SCRIPT` reads them.
fn spawn(folder: &Path, worktree: &Path, base: &str, run: &str) -> std::io::Result<Child> {
    Command::new("/bin/sh")
        .args(["-c", SCRIPT, "atelier-command", run])
        .arg(worktree)
        .current_dir(folder)
        .env("ATELIER_BASE", format!("origin/{base}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
}

/// Empties `folder` for a start, but only once a fresh check finds the
/// leader it names gone. A folder whose command is still stopping, or whose
/// record is a Legacy, Unpublished or Invalid one, is kept, since no later
/// poll could stop what it names; the error is transient, so a person can
/// look at the folder and a later poll retries.
fn clear(folder: &Path) -> Result<(), PluginError> {
    stop(folder)?;

    match record(folder) {
        Ok(identity) => match identity.check() {
            Ok(Leader::Rebooted | Leader::Gone) => {}
            Ok(Leader::Running) => return Err(still(folder)),
            Err(why) => {
                return Err(PluginError::transient(format!(
                    "checking the command's leader in {}: {why}",
                    folder.display()
                )));
            }
        },
        Err(Unrecorded::Absent) => {}
        Err(Unrecorded::Legacy) => return Err(held(folder, "it has no identity record")),
        Err(Unrecorded::Unpublished) => {
            return Err(held(
                folder,
                "it started but its identity was never recorded",
            ));
        }
        Err(Unrecorded::Invalid(why)) => {
            return Err(held(
                folder,
                &format!("its identity record is invalid ({why})"),
            ));
        }
        Err(Unrecorded::Unreadable(error)) => {
            return Err(PluginError::transient(format!(
                "reading the command's identity in {}: {error}",
                folder.display()
            )));
        }
    }

    fs::remove_dir_all(folder)
        .map_err(|error| PluginError::transient(format!("emptying {}: {error}", folder.display())))
}

/// A record the start cannot verify and stop, kept rather than deleted:
/// it is the only trace of a command that may still be running.
fn held(folder: &Path, what: &str) -> PluginError {
    PluginError::transient(format!(
        "the command an earlier start left in {} cannot be stopped: {}",
        folder.display(),
        unsignalled(folder, what)
    ))
}

/// A record whose leader atelier could not confirm gone: the folder is
/// kept until a later poll finds it, so a refused or failed signal cannot
/// orphan the command it names.
fn still(folder: &Path) -> PluginError {
    PluginError::transient(format!(
        "the command an earlier start left in {} is still stopping; atelier keeps the folder until it is gone",
        folder.display()
    ))
}

/// How the command started in `folder` ended, if it has: `passed` on
/// exit 0, `failed` on any other exit, past its time limit, which kills
/// it with every process it started, when it stopped without an exit
/// status, as on a restart of the computer, or when it started but never
/// published its identity.
///
/// # Errors
///
/// No command was started in `folder`, or its leader cannot be checked.
pub fn observe(folder: &Path, settings: &Settings) -> Result<Option<Outcome>, PluginError> {
    if let Some(outcome) = ended(folder) {
        return Ok(Some(outcome));
    }

    let identity = match record(folder) {
        Ok(identity) => identity,
        Err(Unrecorded::Absent) => {
            return Err(PluginError::permanent(format!(
                "no command was started in {}",
                folder.display()
            )));
        }
        Err(Unrecorded::Legacy) => {
            return Ok(Some(failed(unsignalled(
                folder,
                "it has no identity record",
            ))));
        }
        Err(Unrecorded::Unpublished) => {
            return Ok(Some(failed(unsignalled(
                folder,
                "it started but its identity was never recorded",
            ))));
        }
        Err(Unrecorded::Unreadable(error)) => {
            return Err(PluginError::transient(format!(
                "reading the command's identity in {}: {error}",
                folder.display()
            )));
        }
        Err(Unrecorded::Invalid(why)) => return Ok(Some(invalid(folder, &why))),
    };

    let started = fs::read_to_string(folder.join("started"))
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok());
    let Some(started) = started else {
        return Ok(Some(invalid(folder, "no start time")));
    };

    match identity.check() {
        Err(why) => Err(PluginError::transient(format!(
            "checking the command's leader: {why}"
        ))),
        Ok(Leader::Rebooted) => Ok(Some(ended(folder).unwrap_or_else(|| {
            failed("it stopped before it exited, as on a restart of the computer".into())
        }))),
        Ok(Leader::Gone) => {
            Ok(Some(ended(folder).unwrap_or_else(|| {
                failed("it stopped before it exited".into())
            })))
        }
        Ok(Leader::Running) => {
            let timeout = settings
                .number("", "timeout")
                .ok()
                .flatten()
                .unwrap_or(DEFAULT_TIMEOUT_SECS);
            let limit = u64::try_from(timeout).unwrap_or(u64::MAX);

            if now().saturating_sub(started) >= limit {
                kill(folder, identity.leader);

                return Ok(Some(failed(format!(
                    "it ran past its {timeout}-second limit{}",
                    tail(folder)
                ))));
            }

            Ok(None)
        }
    }
}

/// Cancels a command still running in this attempt's folder. A command
/// that already exited or an attempt never started needs no signal; a
/// record that cannot name a leader sends none either, and the log and
/// `observe` say so.
///
/// # Errors
///
/// A transient error when the record cannot be read or the leader cannot
/// be checked, since either may clear and the stop be tried again.
/// Nothing is signalled in any error case.
pub fn stop(folder: &Path) -> Result<(), PluginError> {
    if folder.join("exit").exists() {
        return Ok(());
    }

    let identity = match record(folder) {
        Ok(identity) => identity,
        Err(Unrecorded::Absent) => return Ok(()),
        // Fail closed and say so: no signal is sent, `observe` settles the
        // step `failed`, and a permanent error here would be retried on
        // every poll and keep the run from ending.
        Err(Unrecorded::Legacy) => {
            unsigned(folder, "it has no identity record");

            return Ok(());
        }
        Err(Unrecorded::Unpublished) => {
            unsigned(folder, "it started but its identity was never recorded");

            return Ok(());
        }
        Err(Unrecorded::Invalid(why)) => {
            unsigned(folder, &format!("its identity record is invalid ({why})"));

            return Ok(());
        }
        Err(Unrecorded::Unreadable(error)) => {
            return Err(PluginError::transient(format!(
                "reading the command's identity: {error}"
            )));
        }
    };

    match identity.check() {
        Err(why) => Err(PluginError::transient(format!(
            "checking the command's leader: {why}"
        ))),
        Ok(Leader::Running) => {
            // Checking and then signalling leaves a window in which the
            // leader can exit and its number be reused: the record narrows,
            // but does not close, that window.
            kill(folder, identity.leader);

            Ok(())
        }
        Ok(Leader::Rebooted | Leader::Gone) => Ok(()),
    }
}

/// Says in the log that the command in `folder` was not signalled, and
/// why: `what` names the record that cannot say which leader to signal.
/// A poll that finds the same record says it once.
fn unsigned(folder: &Path, what: &str) {
    if note(folder, what) {
        log(
            "atelier.command",
            &format!("{}: {}", folder.display(), unsignalled(folder, what)),
        );
    }
}

/// Whether this reason for `folder` is new to the log, remembering it.
fn note(folder: &Path, what: &str) -> bool {
    let entry = (folder.to_path_buf(), what.to_owned());
    let mut seen = match UNSIGNED.lock() {
        Ok(seen) => seen,
        // The list only grows, so a poisoned lock still holds every answer
        // it gave before.
        Err(poisoned) => poisoned.into_inner(),
    };

    if seen.contains(&entry) {
        return false;
    }

    seen.push(entry);

    true
}

/// What every text about a command atelier left unsignalled says: `what`
/// names the record that could not say which leader to signal, and where
/// the command runs follows, since it may still.
fn unsignalled(folder: &Path, what: &str) -> String {
    format!(
        "{what}; atelier sent no signal, and the command may still be running {}",
        where_it_runs(folder)
    )
}

/// Where the command in `folder` runs, from the record `start` writes, or
/// `its worktree` when that is missing, unreadable or empty.
fn where_it_runs(folder: &Path) -> String {
    fs::read_to_string(folder.join("worktree"))
        .ok()
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map_or_else(|| "in its worktree".to_owned(), |path| format!("in {path}"))
}

/// What reading a command's identity found instead of one.
enum Unrecorded {
    /// Nothing was started in the folder.
    Absent,
    /// A record from an earlier version, which named the group.
    Legacy,
    /// A command that started but never published its identity.
    Unpublished,
    /// The record could not be read.
    Unreadable(std::io::Error),
    /// The record cannot name a leader.
    Invalid(String),
}

/// The identity of the command started in `folder`, or what the folder
/// holds instead of one.
fn record(folder: &Path) -> Result<Identity, Unrecorded> {
    let text = match fs::read_to_string(folder.join("identity")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(if folder.join("group").exists() {
                Unrecorded::Legacy
            } else if folder.join("started").exists() || folder.join("identity.tmp").exists() {
                Unrecorded::Unpublished
            } else {
                Unrecorded::Absent
            });
        }
        Err(error) => return Err(Unrecorded::Unreadable(error)),
    };

    Identity::parse(&text).map_err(Unrecorded::Invalid)
}

/// Writes the leader's identity beside `started`: `identity.tmp`
/// flushed to the disk, then renamed to `identity`, then the folder
/// flushed, so a record a later poll reads is whole after a crash.
fn publish(folder: &Path, text: &str) -> Result<(), String> {
    let temporary = folder.join("identity.tmp");

    File::create(&temporary)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .map_err(|error| format!("writing it: {error}"))?;
    fs::rename(&temporary, folder.join("identity"))
        .map_err(|error| format!("publishing it: {error}"))?;
    File::open(folder)
        .map_err(|error| format!("opening the folder: {error}"))?
        .sync_all()
        .map_err(|error| format!("flushing the folder: {error}"))
}

/// Ends a command whose identity could not be recorded: no later poll could
/// find its group, so it must not outlive this call. The folder is removed
/// once the group kill was delivered, and kept only when the signal was
/// refused, since it is then the only trace of the command.
fn abandon(folder: &Path, child: &mut Child) {
    let killed = kill(folder, child.id());

    // The child is unreaped, so its number cannot be reused yet.
    let _ = child.kill();
    let _ = child.wait();

    if killed {
        let _ = fs::remove_dir_all(folder);
    }
}

/// Reaps the leader once it ends, so it is never a zombie.
fn reap(mut child: Child) {
    // An unreaped leader only shows as a zombie, which the identity check
    // reads as gone once it exits, so a thread that cannot start is safe
    // to ignore.
    let _ = std::thread::Builder::new()
        .name("atelier-command-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

/// The outcome of a record that cannot name a leader: no signal is sent.
fn invalid(folder: &Path, why: &str) -> Outcome {
    failed(unsignalled(
        folder,
        &format!("its identity record is invalid ({why})"),
    ))
}

/// The outcome its exit status gives, once it has one.
fn ended(folder: &Path) -> Option<Outcome> {
    let status = fs::read_to_string(folder.join("exit")).ok()?;

    match status.trim() {
        "0" => Some(Outcome {
            name: "passed".into(),
            why: "it exited 0".into(),
        }),
        code => Some(failed(format!("it exited {code}{}", tail(folder)))),
    }
}

fn failed(why: String) -> Outcome {
    Outcome {
        name: "failed".into(),
        why,
    }
}

/// The last lines it printed, after a colon, or nothing when it printed
/// nothing.
fn tail(folder: &Path) -> String {
    let Ok(mut file) = File::open(folder.join("output")) else {
        return String::new();
    };
    let length = file.metadata().map_or(0, |metadata| metadata.len());
    let mut bytes = Vec::new();

    if file
        .seek(SeekFrom::Start(length.saturating_sub(TAIL_BYTES)))
        .is_err()
        || file.read_to_end(&mut bytes).is_err()
    {
        return String::new();
    }

    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let last = lines
        .get(lines.len().saturating_sub(TAIL_LINES)..)
        .unwrap_or_default();

    if last.is_empty() {
        String::new()
    } else {
        format!(":\n{}", last.join("\n"))
    }
}

/// Kills the group's every process, and, when atelier refuses the target,
/// says in the log that it did. Whether a signal was delivered is
/// reported, so a caller cannot read a refusal as a stop.
fn kill(folder: &Path, group: u32) -> bool {
    let Some(mut signal) = signal_group(Signal::Kill, u64::from(group)) else {
        log("atelier.command", &refused(folder, group));

        return false;
    };

    signal.status().is_ok_and(|status| status.success())
}

/// The line atelier logs when it would not signal `group` for the command
/// in `folder`, so it may still be running.
fn refused(folder: &Path, group: u32) -> String {
    format!(
        "{}: atelier refused to signal group {group}; the command may still be running {}",
        folder.display(),
        where_it_runs(folder)
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}
#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use atelier_core::Setting;

    use super::*;

    /// A worktree and a step's folder, removed when the test ends.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("atelier-command-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("worktree")).unwrap();
            Self(root.canonicalize().unwrap())
        }

        fn folder(&self) -> std::path::PathBuf {
            self.0.join("step")
        }

        fn worktree(&self) -> std::path::PathBuf {
            self.0.join("worktree")
        }

        fn try_start(&self, run: &str, timeout: Option<i64>) -> Result<Settings, PluginError> {
            let mut settings = Settings::default().with("run", Setting::Text(run.into()));

            if let Some(secs) = timeout {
                settings = settings.with("timeout", Setting::Number(secs));
            }

            start(&self.folder(), &self.worktree(), "v2", &settings)?;

            Ok(settings)
        }

        /// Starts again, past a start that finds the folder's last leader still
        /// stopping: a kill returns before the leader is gone, and the daemon
        /// retries that start on its next poll.
        fn start_again(&self, run: &str, timeout: Option<i64>) -> Settings {
            let begun = Instant::now();

            loop {
                match self.try_start(run, timeout) {
                    Ok(settings) => return settings,
                    Err(error) => {
                        assert!(error.message.contains("is still stopping"), "{error}");
                        assert!(
                            begun.elapsed() < Duration::from_secs(10),
                            "no start: {error}"
                        );
                    }
                }

                thread::sleep(Duration::from_millis(50));
            }
        }

        fn start(&self, run: &str, timeout: Option<i64>) -> Settings {
            self.try_start(run, timeout).unwrap()
        }

        /// The outcome, once there is one, within ten seconds.
        fn outcome(&self, settings: &Settings) -> Outcome {
            let begun = Instant::now();

            loop {
                if let Some(outcome) = observe(&self.folder(), settings).unwrap() {
                    return outcome;
                }

                assert!(begun.elapsed() < Duration::from_secs(10), "no outcome");
                thread::sleep(Duration::from_millis(50));
            }
        }

        /// The process group of the command started here.
        fn group(&self) -> u64 {
            let text = fs::read_to_string(self.folder().join("identity")).unwrap();
            let identity = Identity::parse(&text).unwrap();

            u64::from(identity.leader)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Whether any process of the group is left.
    fn alive(group: u64) -> bool {
        signal_group(Signal::Probe, group)
            .is_some_and(|mut signal| signal.status().is_ok_and(|status| status.success()))
    }

    /// Kills the group's every process.
    fn kill_group(group: u64) {
        if let Some(mut signal) = signal_group(Signal::Kill, group) {
            let _ = signal.status();
        }
    }

    /// How every text about an unsignalled command ends, when the folder
    /// records no worktree.
    const MAY_STILL_RUN: &str =
        "; atelier sent no signal, and the command may still be running in its worktree";

    #[test]
    fn a_command_that_exits_0_passes_and_runs_in_the_worktree_with_its_base() {
        let scratch = Scratch::new("passes");

        let settings = scratch.start("pwd > here; printf %s \"$ATELIER_BASE\" > base", None);
        let started = Instant::now();
        let outcome = scratch.outcome(&settings);

        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            outcome,
            Outcome {
                name: "passed".into(),
                why: "it exited 0".into(),
            }
        );
        assert_eq!(
            fs::read_to_string(scratch.worktree().join("here"))
                .unwrap()
                .trim(),
            scratch.worktree().to_string_lossy()
        );
        assert_eq!(
            fs::read_to_string(scratch.worktree().join("base")).unwrap(),
            "origin/v2"
        );
    }

    #[test]
    fn a_failing_command_fails_with_the_end_of_what_it_printed() {
        let scratch = Scratch::new("fails");

        let settings = scratch.start("for i in $(seq 1 30); do echo line $i; done; exit 3", None);
        let outcome = scratch.outcome(&settings);
        let lines: Vec<String> = (11..=30).map(|line| format!("line {line}")).collect();

        assert_eq!(outcome.name, "failed");
        assert_eq!(outcome.why, format!("it exited 3:\n{}", lines.join("\n")));
    }

    #[test]
    fn a_command_past_its_limit_fails_and_is_killed_with_its_children() {
        let scratch = Scratch::new("limit");

        let settings = scratch.start("echo waiting; sleep 30 & sleep 30", Some(1));
        let group = scratch.group();
        let begun = Instant::now();
        let outcome = scratch.outcome(&settings);

        assert!(
            begun.elapsed() < Duration::from_secs(4),
            "{:?}",
            begun.elapsed()
        );
        assert_eq!(outcome.name, "failed");
        assert_eq!(outcome.why, "it ran past its 1-second limit:\nwaiting");
        thread::sleep(Duration::from_millis(200));
        assert!(!alive(group));
    }

    #[test]
    fn stopping_a_command_kills_its_group_and_is_safe_to_repeat() {
        let scratch = Scratch::new("cleanup");
        scratch.start("sleep 30 & wait", None);
        let group = scratch.group();

        stop(&scratch.folder()).unwrap();
        let begun = Instant::now();
        while alive(group) && begun.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(20));
        }
        stop(&scratch.folder()).unwrap();

        assert!(!alive(group));
        assert!(stop(&scratch.0.join("never-started")).is_ok());
    }

    #[test]
    fn abandoning_a_start_kills_its_whole_group() {
        let scratch = Scratch::new("abandon");
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 30 & sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let group = u64::from(child.id());

        thread::sleep(Duration::from_millis(100));
        assert!(alive(group), "the group never started");

        abandon(&scratch.folder(), &mut child);

        let begun = Instant::now();

        while alive(group) && begun.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(20));
        }

        assert!(!alive(group), "abandon left the group running");
    }

    /// An abandoned start removes its folder once the group kill was
    /// delivered: it is no record of anything.
    #[test]
    fn an_abandoned_start_removes_its_folder() {
        let scratch = Scratch::new("abandoned-folder");
        let folder = scratch.folder();
        fs::create_dir_all(&folder).unwrap();
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 5"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();

        abandon(&folder, &mut child);

        assert!(
            child.try_wait().unwrap().is_some(),
            "the child was not reaped"
        );
        assert!(!folder.exists());
    }

    /// A start that cannot spawn its command leaves no folder behind:
    /// nothing ran, so a kept folder would refuse every later start for
    /// nothing.
    #[test]
    fn a_start_that_cannot_spawn_leaves_no_folder() {
        let scratch = Scratch::new("cannot-spawn");
        let folder = scratch.folder();
        let settings = Settings::default().with("run", Setting::Text("true\0".into()));

        let error = start(&folder, &scratch.worktree(), "v2", &settings).unwrap_err();

        assert!(error.message.contains("starting a command in"), "{error}");
        assert!(!folder.exists());
    }

    /// A refused signal is the line atelier logs, which names the group
    /// and where the command may still run.
    #[test]
    fn a_refused_signal_says_which_group_and_where_it_may_still_run() {
        let scratch = Scratch::new("refused");
        let folder = scratch.folder();
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join("worktree"),
            scratch.worktree().display().to_string(),
        )
        .unwrap();

        assert_eq!(
            refused(&folder, 9),
            format!(
                "{}: atelier refused to signal group 9; the command may still be running in {}",
                folder.display(),
                scratch.worktree().display()
            )
        );
    }

    /// A refusal reports no signal, not a stop, and leaves the folder it
    /// named alone: groups 0 and 1 are refused before `/bin/kill` is built,
    /// so nothing is signalled.
    #[test]
    fn a_refused_signal_is_not_a_stop() {
        let scratch = Scratch::new("refused-kill");
        let folder = scratch.folder();
        fs::create_dir_all(&folder).unwrap();

        assert!(!kill(&folder, 1));
        assert!(!kill(&folder, 0));
        assert!(folder.exists());
    }

    #[test]
    fn the_identity_is_published_before_start_returns() {
        let scratch = Scratch::new("identity");
        scratch.start("sleep 30", None);
        let folder = scratch.folder();
        let text = fs::read_to_string(folder.join("identity")).unwrap();
        let identity = Identity::parse(&text).unwrap();

        assert!(!folder.join("identity.tmp").exists());
        assert_eq!(
            fs::read_to_string(folder.join("worktree")).unwrap(),
            scratch.worktree().display().to_string()
        );
        assert_eq!(identity.check().unwrap(), Leader::Running);

        kill_group(u64::from(identity.leader));
    }

    #[test]
    fn a_mismatched_identity_fails_closed_without_signalling() {
        let scratch = Scratch::new("mismatched");
        let settings = scratch.start("sleep 30", Some(1));
        let folder = scratch.folder();
        let text = fs::read_to_string(folder.join("identity")).unwrap();
        let real = Identity::parse(&text).unwrap();
        let group = u64::from(real.leader);
        let wrong_start = Identity {
            leader: real.leader,
            boot: real.boot,
            start: "0".into(),
        };
        let wrong_boot = Identity {
            leader: real.leader,
            boot: "another boot".into(),
            start: real.start,
        };

        fs::write(folder.join("identity"), wrong_start.render()).unwrap();
        thread::sleep(Duration::from_millis(1100));

        let stopped = observe(&folder, &settings).unwrap().unwrap();
        assert_eq!(stopped.name, "failed");
        assert_eq!(stopped.why, "it stopped before it exited");
        assert!(stop(&folder).is_ok());
        assert!(alive(group), "a mismatched identity signalled the group");

        fs::write(folder.join("identity"), wrong_boot.render()).unwrap();

        let rebooted = observe(&folder, &settings).unwrap().unwrap();
        assert_eq!(
            rebooted.why,
            "it stopped before it exited, as on a restart of the computer"
        );
        assert!(alive(group), "a rebooted identity signalled the group");

        kill_group(group);
        let begun = Instant::now();
        while alive(group) && begun.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(20));
        }

        assert!(!alive(group));
    }

    #[test]
    fn legacy_and_invalid_records_fail_closed() {
        let scratch = Scratch::new("unrecorded");
        let folder = scratch.folder();
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("started"), "1").unwrap();
        fs::write(folder.join("group"), "4242").unwrap();

        let legacy = observe(&folder, &Settings::default()).unwrap().unwrap();
        assert_eq!(
            legacy.why,
            format!("it has no identity record{MAY_STILL_RUN}")
        );
        assert!(stop(&folder).is_ok());

        fs::write(
            folder.join("identity"),
            "atelier-command-identity 1\nleader 1\nboot b\nstart s\n",
        )
        .unwrap();

        let invalid = observe(&folder, &Settings::default()).unwrap().unwrap();
        assert!(
            invalid.why.starts_with("its identity record is invalid")
                && invalid.why.ends_with(MAY_STILL_RUN),
            "{invalid:?}"
        );
        assert!(stop(&folder).is_ok());

        fs::remove_file(folder.join("identity")).unwrap();
        fs::create_dir(folder.join("identity")).unwrap();
        assert!(!stop(&folder).unwrap_err().is_permanent());
    }

    /// A repeated start empties a folder only once nothing it names may
    /// still run. A legacy record names a group no identity can verify, so
    /// the start refuses and keeps the only trace of it.
    #[test]
    fn a_repeated_start_over_a_legacy_record_refuses_and_keeps_it() {
        let scratch = Scratch::new("legacy-held");
        let folder = scratch.folder();
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("group"), "4242").unwrap();
        fs::write(
            folder.join("worktree"),
            scratch.worktree().display().to_string(),
        )
        .unwrap();
        let settings = Settings::default().with("run", Setting::Text("sleep 1".into()));

        let error = start(&folder, &scratch.worktree(), "v2", &settings).unwrap_err();

        assert!(error.message.contains("cannot be stopped"), "{error}");
        assert!(folder.exists());
        assert!(folder.join("group").exists());
    }

    /// A record whose start never published its identity is kept as well:
    /// nothing is known that could be stopped.
    #[test]
    fn a_repeated_start_over_an_unpublished_record_refuses_and_keeps_it() {
        let scratch = Scratch::new("unpublished-held");
        let folder = scratch.folder();
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("started"), "1").unwrap();
        fs::write(
            folder.join("worktree"),
            scratch.worktree().display().to_string(),
        )
        .unwrap();
        let settings = Settings::default().with("run", Setting::Text("sleep 1".into()));

        let error = start(&folder, &scratch.worktree(), "v2", &settings).unwrap_err();

        assert!(error.message.contains("cannot be stopped"), "{error}");
        assert!(error.message.contains("never recorded"), "{error}");
        assert!(folder.exists());
    }

    /// An invalid record is kept as well: no later poll could find the
    /// group it may name.
    #[test]
    fn a_repeated_start_over_an_invalid_record_refuses_and_keeps_it() {
        let scratch = Scratch::new("invalid-held");
        let folder = scratch.folder();
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join("worktree"),
            scratch.worktree().display().to_string(),
        )
        .unwrap();
        fs::write(folder.join("identity"), "nonsense\n").unwrap();
        let settings = Settings::default().with("run", Setting::Text("sleep 1".into()));

        let error = start(&folder, &scratch.worktree(), "v2", &settings).unwrap_err();

        assert!(error.message.contains("cannot be stopped"), "{error}");
        assert!(
            error.message.contains("its identity record is invalid"),
            "{error}"
        );
        assert!(folder.join("identity").exists());
    }

    #[test]
    fn unpublished_records_fail_closed() {
        let scratch = Scratch::new("unpublished");
        let folder = scratch.folder();
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("started"), "1").unwrap();

        let unpublished = observe(&folder, &Settings::default()).unwrap().unwrap();
        assert_eq!(
            unpublished.why,
            format!("it started but its identity was never recorded{MAY_STILL_RUN}")
        );
        assert!(stop(&folder).is_ok());

        fs::write(folder.join("identity.tmp"), "not an identity").unwrap();

        let half_written = observe(&folder, &Settings::default()).unwrap().unwrap();
        assert_eq!(half_written, unpublished);
        assert!(stop(&folder).is_ok());
    }

    #[test]
    fn observe_without_a_start_time_fails_closed() {
        let scratch = Scratch::new("no-start-time");
        let folder = scratch.folder();
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            folder.join("identity"),
            "atelier-command-identity 1\nleader 4242\nboot b\nstart s\n",
        )
        .unwrap();

        let outcome = observe(&folder, &Settings::default()).unwrap().unwrap();

        assert_eq!(
            outcome.why,
            format!("its identity record is invalid (no start time){MAY_STILL_RUN}")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_leader_is_reaped_once_the_command_ends() {
        let scratch = Scratch::new("reaped");
        let settings = scratch.start("true", None);
        let leader = scratch.group();

        assert_eq!(scratch.outcome(&settings).name, "passed");

        let path = std::path::PathBuf::from(format!("/proc/{leader}"));
        let begun = Instant::now();

        while path.exists() {
            assert!(
                begun.elapsed() < Duration::from_secs(5),
                "the leader was not reaped"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn a_command_still_running_has_no_outcome_yet() {
        let scratch = Scratch::new("running");

        let settings = scratch.start("sleep 30", None);

        assert_eq!(observe(&scratch.folder(), &settings).unwrap(), None);
        kill_group(scratch.group());
    }

    #[test]
    fn a_command_that_stopped_without_an_exit_status_fails() {
        let scratch = Scratch::new("stopped");

        let settings = scratch.start("sleep 30", None);
        kill_group(scratch.group());
        let outcome = scratch.outcome(&settings);

        assert_eq!(outcome.name, "failed");
        assert_eq!(outcome.why, "it stopped before it exited");
    }

    #[test]
    fn starting_again_starts_clean_and_no_start_is_refused() {
        let scratch = Scratch::new("again");
        let never = observe(&scratch.folder(), &Settings::default()).unwrap_err();

        let first = scratch.start("exit 1", None);
        assert_eq!(scratch.outcome(&first).name, "failed");
        let again = scratch.start_again("sleep 30", None);

        assert_eq!(observe(&scratch.folder(), &again).unwrap(), None);
        assert!(never.is_permanent());
        assert!(
            never.message.starts_with("no command was started in"),
            "{never}"
        );
        kill_group(scratch.group());
    }

    /// A daemon that crashed between a start and the run state it could not
    /// save repeats that start; the command the folder still names must not
    /// be orphaned.
    #[test]
    fn starting_again_stops_the_command_the_folder_still_names() {
        let scratch = Scratch::new("restarted");
        scratch.start_again("sleep 30", None);
        let first = scratch.group();
        scratch.start_again("sleep 30", None);
        let second = scratch.group();
        let begun = Instant::now();

        while alive(first) && begun.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(20));
        }

        assert!(!alive(first), "the first command is still running");

        kill_group(second);
    }

    /// A record whose leader is still running keeps the folder, so a
    /// refused or failed signal cannot orphan the command it names.
    #[test]
    fn a_record_whose_command_is_still_running_keeps_the_folder() {
        let scratch = Scratch::new("still-running");
        let folder = scratch.folder();
        fs::create_dir_all(&folder).unwrap();
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 5"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let identity = Identity::of(child.id()).unwrap();
        let group = u64::from(child.id());

        fs::write(folder.join("identity"), identity.render()).unwrap();
        fs::write(
            folder.join("worktree"),
            scratch.worktree().display().to_string(),
        )
        .unwrap();
        // An exit file sends no signal, so the check keeps the folder.
        fs::write(folder.join("exit"), "0\n").unwrap();

        let error = clear(&folder).unwrap_err();

        assert!(error.message.contains("is still stopping"), "{error}");
        assert!(folder.exists());
        assert!(folder.join("identity").exists());

        kill_group(group);
        let _ = child.wait();
    }

    /// The log says an unsignalled record once, however many polls find it;
    /// another reason, or the same one in another folder, is a new line.
    #[test]
    fn only_a_changed_record_is_said_again() {
        let scratch = Scratch::new("said-once");
        let folder = scratch.folder();
        let legacy = "it has no identity record";
        let unpublished = "it started but its identity was never recorded";

        assert!(note(&folder, legacy));
        assert!(!note(&folder, legacy));
        assert!(note(&folder, unpublished));
        assert!(!note(&folder, unpublished));
        assert!(note(std::path::Path::new("/elsewhere"), legacy));
    }

    /// A record whose leader has gone empties the folder, so the next start
    /// begins clean.
    #[test]
    fn a_record_whose_command_is_gone_empties_the_folder() {
        let scratch = Scratch::new("gone");
        let settings = scratch.start("true", None);

        assert_eq!(scratch.outcome(&settings).name, "passed");

        // The wrapper writes `exit` before the leader it is read from dies,
        // so a poll can find the record still naming a leader that is
        // stopping.
        let begun = Instant::now();

        loop {
            match clear(&scratch.folder()) {
                Ok(()) => break,
                Err(error) => {
                    assert!(error.message.contains("is still stopping"), "{error}");
                    assert!(
                        begun.elapsed() < Duration::from_secs(10),
                        "the folder was never emptied: {error}"
                    );
                }
            }

            thread::sleep(Duration::from_millis(50));
        }

        assert!(!scratch.folder().exists());
    }
}
