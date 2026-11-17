//! Running a tool, such as `git`, `gh` or `opencode`, with a deadline and
//! bounded output, so a hung tool never holds up a poll.
//!
//! ```
//! use std::process::Command;
//! use std::time::Duration;
//!
//! let mut command = Command::new("git");
//! command.arg("--version");
//!
//! let output = atelier_command::run(command, Duration::from_secs(10))?;
//!
//! assert!(output.success);
//! assert!(output.stdout.starts_with("git version"));
//! # Ok::<(), atelier_core::PluginError>(())
//! ```

use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use atelier_core::PluginError;

/// How often a running command is checked.
const TICK: Duration = Duration::from_millis(20);

/// The most output kept from one stream: its end, where a tool says why it
/// failed.
const MAX_OUTPUT: usize = 1 << 20;

/// How long `/bin/ps` may take to answer with atelier's process group.
const PS_DEADLINE: Duration = Duration::from_secs(5);

/// What a finished command printed, and whether it succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `command` with no input, in its own process group. Each output is
/// read to its end, so a tool that prints a lot runs on, and its last 1 MiB
/// is kept, with any bytes that are not UTF-8 replaced.
///
/// # Errors
///
/// The tool cannot start, or runs past `deadline` and is killed with every
/// process it started; both transient, so the next poll tries again.
pub fn run(mut command: Command, deadline: Duration) -> Result<Output, PluginError> {
    let program = command.get_program().to_string_lossy().into_owned();

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| PluginError::transient(format!("{program} could not start: {error}")))?;

    let stdout = child.stdout.take().map(reader);
    let stderr = child.stderr.take().map(reader);
    let started = Instant::now();

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() >= deadline => {
                // The group too: a child the tool started, such as ssh, would
                // otherwise hold the output open past the deadline.
                if let Some(mut kill) = signal_group(Signal::Kill, u64::from(child.id())) {
                    let _ = kill.status();
                }
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("{program} ran past {deadline:?}"));
            }
            Ok(None) => thread::sleep(TICK),
            Err(error) => break Err(format!("{program}: {error}")),
        }
    };

    let stdout = stdout.map_or_default(collect);
    let stderr = stderr.map_or_default(collect);
    let status = status.map_err(PluginError::transient)?;

    Ok(Output {
        success: status.success(),
        stdout,
        stderr,
    })
}

/// A signal atelier sends to a process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// `-0`: delivers nothing, and succeeds while any process is left.
    Probe,
    /// `-KILL`: ends every process at once.
    Kill,
}

impl Signal {
    /// The option `kill` takes for it.
    #[must_use]
    pub const fn option(self) -> &'static str {
        match self {
            Self::Probe => "-0",
            Self::Kill => "-KILL",
        }
    }
}

/// Prepares `/bin/kill <signal> -- -<group>` for one process group without
/// starting it, so it signals that group alone. The program is absolute so
/// `PATH` cannot substitute another executable.
///
/// It returns `None` for a target that is atelier itself, its parent, or
/// the group atelier runs in: any of those would signal the daemon or the
/// launcher that started it. It also returns `None` for a group outside
/// `2..=i32::MAX`: `0` and `1` address atelier's own group or every
/// permitted process, and `kill` truncates a larger number into its pid
/// range, where it can do the same. When atelier's own group cannot be
/// read, it refuses every target, since it cannot tell whether the target
/// is its own group.
///
/// Every group signal atelier sends is built here, so the guard holds at
/// both the deadline kill above and a background step's cleanup.
///
/// ```
/// use atelier_command::{Signal, signal_group};
///
/// let command = signal_group(Signal::Kill, 4242).unwrap();
///
/// assert_eq!(command.get_program(), "/bin/kill");
/// assert!(signal_group(Signal::Kill, 1).is_none());
/// ```
#[must_use]
pub fn signal_group(signal: Signal, group: u64) -> Option<Command> {
    // Targets 0 and 1 would address our group or every permitted process,
    // and `kill` truncates a larger number into its pid range, so 2^32 + 1
    // would reach every process too.
    if !i32::try_from(group).is_ok_and(|group| group > 1) {
        return None;
    }

    // Refuse before building anything: signalling atelier, its parent or its
    // own group would signal the daemon or the launcher.
    if refused(group) {
        return None;
    }

    let mut command = Command::new("/bin/kill");
    command
        .args([signal.option(), "--", &format!("-{group}")])
        .stderr(Stdio::null());

    Some(command)
}

/// Whether `group` is a target a signal must not reach: atelier's own pid,
/// its parent's pid, or the group atelier runs in. With no group readable,
/// every target is refused.
fn refused(group: u64) -> bool {
    let Some(own_group) = own_group() else {
        return true;
    };

    let own = [
        u64::from(std::process::id()),
        u64::from(std::os::unix::process::parent_id()),
        u64::from(own_group),
    ];

    own.contains(&group)
}

/// The process group atelier runs in, read once and kept. `None` when this
/// platform has no reader or the read fails, which refuses every target.
/// Only a read that succeeded is kept, so a failed read is tried again by
/// the next call instead of refusing every kill until a restart.
fn own_group() -> Option<u32> {
    static GROUP: OnceLock<u32> = OnceLock::new();

    if let Some(group) = GROUP.get() {
        return Some(*group);
    }

    let group = read_own_group()?;

    // Another caller may have read it first; either value is the same group.
    let _ = GROUP.set(group);

    Some(group)
}

/// Reads atelier's process group with this platform's reader. Every reader
/// compiles on every target, so only the call is chosen.
fn read_own_group() -> Option<u32> {
    if cfg!(target_os = "linux") {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;

        parse_pgrp(&stat)
    } else if cfg!(target_os = "macos") {
        read_pgid_with_ps()
    } else {
        None
    }
}

/// Reads atelier's process group from `/bin/ps`, killing it at
/// `PS_DEADLINE`. It polls the child itself rather than going through
/// `run`: `run` signals the group on its own deadline, and
/// `signal_group` reads the own group, so a read inside it would wait on
/// itself.
fn read_pgid_with_ps() -> Option<u32> {
    let mut child = Command::new("/bin/ps")
        .env("LC_ALL", "C")
        .args(["-o", "pgid=", "-p", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    parse_pgid(&read_child(&mut child, PS_DEADLINE)?)
}

/// Reads what a child prints once it exits in `deadline`, killing and
/// reaping the child this call owns when the deadline comes first. Output
/// is returned only for a child that exited successfully.
fn read_child(child: &mut Child, deadline: Duration) -> Option<String> {
    let started = Instant::now();

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();

                break None;
            }
            Ok(None) => thread::sleep(TICK),
            Err(_) => break None,
        }
    };

    if !status?.success() {
        return None;
    }

    let mut stdout = child.stdout.take()?;
    let mut printed = String::new();
    stdout.read_to_string(&mut printed).ok()?;

    Some(printed)
}

/// The process group in a `/proc/<pid>/stat` line: the third field after
/// the command name, which may itself hold spaces and parentheses.
fn parse_pgrp(stat: &str) -> Option<u32> {
    let (_, rest) = stat.rsplit_once(')')?;

    rest.split_whitespace().nth(2)?.parse().ok()
}

/// The group `ps -o pgid=` printed: its output trimmed, which must be
/// digits.
fn parse_pgid(ps: &str) -> Option<u32> {
    ps.trim().parse().ok()
}

/// Reads `stream` to its end, keeping its last `MAX_OUTPUT` bytes. Reading
/// on, rather than closing the stream, keeps a tool that prints past the
/// bound from being stopped by a closed pipe.
fn reader(mut stream: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut tail = Tail(Vec::new());
        let _ = io::copy(&mut stream, &mut tail);

        String::from_utf8_lossy(&tail.0).into_owned()
    })
}

/// A sink that keeps only the last `MAX_OUTPUT` bytes written to it.
struct Tail(Vec<u8>);

impl Write for Tail {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(bytes);
        let over = self.0.len().saturating_sub(MAX_OUTPUT);
        self.0.drain(..over);

        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn collect(handle: thread::JoinHandle<String>) -> String {
    handle.join().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `group` is a target the guard must refuse: atelier's own pid,
    /// its parent's pid, or its own group. With no group readable, every
    /// target is refused.
    fn own_target(group: u64) -> bool {
        let Some(own_group) = own_group() else {
            return true;
        };

        let own = [
            u64::from(std::process::id()),
            u64::from(std::os::unix::process::parent_id()),
            u64::from(own_group),
        ];

        own.contains(&group)
    }

    #[test]
    fn group_signals_refuse_broad_targets_without_execution() {
        use std::ffi::OsStr;

        let highest = u64::from(i32::MAX.unsigned_abs());
        let refused = [
            0,
            1,
            highest + 1,
            u64::from(u32::MAX),
            1 << 32,
            (1 << 32) + 1,
            u64::MAX,
        ];

        // Inspect preparation only: even a missing guard must never run kill.
        for (signal, option) in [(Signal::Probe, "-0"), (Signal::Kill, "-KILL")] {
            for group in refused {
                assert!(
                    signal_group(signal, group).is_none(),
                    "prepared {signal:?} for forbidden group {group}"
                );
            }

            for group in [2, highest] {
                if own_target(group) {
                    continue;
                }

                let command = signal_group(signal, group).expect("an explicit target");
                let args: Vec<&OsStr> = command.get_args().collect();
                let target = format!("-{group}");

                assert_eq!(command.get_program(), OsStr::new("/bin/kill"));
                assert_eq!(
                    args,
                    [OsStr::new(option), OsStr::new("--"), OsStr::new(&target)]
                );
            }
        }
    }

    #[test]
    fn own_targets_are_refused_without_execution() {
        let own = u64::from(std::process::id());
        let parent = u64::from(std::os::unix::process::parent_id());

        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(own_group().is_some(), "own group unreadable");
        }

        // Inspect preparation only: even a missing guard must never run kill.
        for signal in [Signal::Probe, Signal::Kill] {
            assert!(
                signal_group(signal, own).is_none(),
                "prepared {signal:?} for atelier"
            );
            assert!(
                signal_group(signal, parent).is_none(),
                "prepared {signal:?} for atelier's parent"
            );

            if let Some(group) = own_group() {
                assert!(
                    signal_group(signal, u64::from(group)).is_none(),
                    "prepared {signal:?} for atelier's group"
                );
            }

            if !own_target(2) {
                assert!(signal_group(signal, 2).is_some(), "refused a plain target");
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn own_group_is_read_from_proc_stat() {
        let stat = std::fs::read_to_string("/proc/self/stat").expect("/proc/self/stat");
        let (_, rest) = stat.rsplit_once(')').expect("a command name");
        let group: u32 = rest
            .split_whitespace()
            .nth(2)
            .expect("a process group")
            .parse()
            .expect("a numeric process group");

        assert_eq!(own_group(), Some(group));
        assert!(signal_group(Signal::Kill, u64::from(group)).is_none());
    }

    #[test]
    fn parse_pgrp_reads_the_group_after_the_command_name() {
        assert_eq!(
            parse_pgrp("4242 (atelier) S 4241 4200 4242 0 -1"),
            Some(4200)
        );
        assert_eq!(
            parse_pgrp("4242 (a ) ( b) R 4241 4201 4242 0 -1"),
            Some(4201)
        );
        assert_eq!(parse_pgrp("4242 (atelier) S 4241 nope 4242"), None);
        assert_eq!(parse_pgrp(""), None);
        assert_eq!(parse_pgrp("4242 (atelier) S 4241"), None);
    }

    #[test]
    fn parse_pgid_reads_the_group_ps_printed() {
        assert_eq!(parse_pgid(" 4200\n"), Some(4200));
        assert_eq!(parse_pgid(""), None);
        assert_eq!(parse_pgid("   \n"), None);
        assert_eq!(parse_pgid("pgid\n"), None);
    }

    #[test]
    fn a_command_past_its_deadline_is_killed_with_its_children() {
        let started = Instant::now();
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5 & sleep 5"]);

        let error = run(command, Duration::from_millis(200)).unwrap_err();

        assert!(!error.is_permanent());
        assert!(error.message.contains("ran past"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn output_is_kept_up_to_its_bound() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "head -c 2000000 /dev/zero | tr '\\0' a; echo err >&2; exit 3",
        ]);

        let output = run(command, Duration::from_secs(10)).unwrap();

        assert!(!output.success);
        assert_eq!(output.stdout.len(), 1 << 20);
        assert_eq!(output.stderr, "err\n");
    }

    #[test]
    fn a_tool_writing_past_the_bound_runs_to_its_end_and_its_last_output_is_kept() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "head -c 3000000 /dev/zero | tr '\\0' a; printf '\\377'; echo; echo last",
        ]);

        let output = run(command, Duration::from_secs(10)).unwrap();

        assert!(output.success, "{}", output.stderr);
        assert!(output.stdout.ends_with("a\u{fffd}\nlast\n"));
        assert!(output.stdout.len() <= (1 << 20) + 2);
    }

    #[test]
    fn a_missing_tool_fails_transiently() {
        let error = run(Command::new("/no/such/tool"), Duration::from_secs(1)).unwrap_err();

        assert!(!error.is_permanent());
        assert!(
            error.message.contains("/no/such/tool could not start"),
            "{error}"
        );
    }

    /// Spawns `/bin/sh -c <script>` with its output piped and no input, for
    /// a test to read with its own deadline.
    fn short_child(script: &str) -> Child {
        Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("a shell")
    }

    #[test]
    fn a_child_that_exits_in_time_is_read() {
        let mut child = short_child("echo 42");

        assert_eq!(
            read_child(&mut child, Duration::from_secs(5)),
            Some("42\n".to_owned())
        );
    }

    #[test]
    fn a_child_past_the_deadline_is_killed() {
        let mut child = short_child("sleep 1; echo 42");

        assert_eq!(read_child(&mut child, Duration::from_millis(100)), None);
    }

    #[test]
    fn a_child_that_fails_is_not_read() {
        let mut child = short_child("echo 42; exit 1");

        assert_eq!(read_child(&mut child, Duration::from_secs(5)), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_ps_reader_agrees_with_proc() {
        let stat = std::fs::read_to_string("/proc/self/stat").expect("/proc/self/stat");

        assert_eq!(read_pgid_with_ps(), parse_pgrp(&stat));
    }
}
