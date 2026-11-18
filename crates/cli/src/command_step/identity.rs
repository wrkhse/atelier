//! Who leads a command's process group, recorded when it starts and
//! checked before a later poll observes or signals that group, so a stale
//! record never leads to a signal.
//!
//! The record and the check reduce that risk; they do not remove it. macOS
//! start times resolve to the second, and checking and then running
//! `/bin/kill` leaves a window in which the leader can exit and its number
//! be reused. This is not containment and not a proof of ownership.
//!
//! Linux is read through `/proc`, and macOS through `ps` and `sysctl`. Both
//! readers are compiled on every target, and the running platform chooses
//! between them.

use std::fs;
use std::process::Command;
use std::time::Duration;

use atelier_command::run;

/// The first line of a rendered identity: its format, and its version.
const HEADER: &str = "atelier-command-identity 1";

/// The lowest leader a signal may name: 0 addresses atelier's own group,
/// and 1 every permitted process.
const LOWEST_LEADER: i64 = 2;

/// `ESRCH`: the kernel's answer for a process that has gone.
const ESRCH: i32 = 3;

/// How long reading the boot identity or a process may take.
const READ_DEADLINE: Duration = Duration::from_secs(10);

/// What identifies a command's group leader across polls and daemon
/// restarts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The number the leader has, which is also its group's.
    pub leader: u32,
    /// The boot it started in: the kernel's boot id on Linux, and
    /// `kern.bootsessionuuid` on macOS.
    pub boot: String,
    /// When it started, in that boot's terms: ticks since boot on Linux,
    /// and its start clock time on macOS.
    pub start: String,
}

impl Identity {
    /// Records the leader of the process group `leader` leads.
    ///
    /// The caller must still hold the leader unreaped, so its number cannot
    /// be reused; a leader that has already exited is therefore still
    /// recorded, since only its entry is left and its group is still its
    /// own.
    ///
    /// # Errors
    ///
    /// The system cannot report the boot or `leader`, or `leader` is not
    /// the leader of its own group.
    pub fn of(leader: u32) -> Result<Self, String> {
        let boot = boot()?;
        let process = process(leader)?
            .filter(|process| process.group == leader)
            .ok_or_else(|| format!("process {leader} does not lead its own group"))?;

        Ok(Self {
            leader,
            boot,
            start: process.start,
        })
    }

    /// The identity as `parse` reads it back: four lines, the last ended
    /// by a newline.
    #[must_use]
    pub fn render(&self) -> String {
        let Self {
            leader,
            boot,
            start,
        } = self;

        format!("{HEADER}\nleader {leader}\nboot {boot}\nstart {start}\n")
    }

    /// Reads the identity `render` writes: its header, then the leader,
    /// boot and start lines. A leader with a leading `+` or zeros parses,
    /// as `str::parse` accepts them.
    ///
    /// # Errors
    ///
    /// `text` has no header, a line is missing, out of order or extra, a
    /// value is empty or padded with whitespace, or the leader does not
    /// parse as a number in 2..=2147483647.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lines = text.lines();

        if lines.next() != Some(HEADER) {
            return Err("no identity header".into());
        }

        let leader = parse_leader(field(lines.next(), "leader")?)?;
        let boot = field(lines.next(), "boot")?;
        let start = field(lines.next(), "start")?;

        if let Some(line) = lines.next() {
            return Err(format!("unexpected line after start: {line}"));
        }

        Ok(Self {
            leader,
            boot: boot.into(),
            start: start.into(),
        })
    }

    /// Whether this record still names the leader, checked against the
    /// system now.
    ///
    /// # Errors
    ///
    /// The system cannot report the boot or the process.
    pub fn check(&self) -> Result<Leader, String> {
        Ok(judge(self, &boot()?, process(self.leader)?.as_ref()))
    }
}

/// Whether a recorded leader still leads its group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leader {
    /// It leads the group it led when it was recorded.
    Running,
    /// The computer has restarted since the record, so none of its
    /// processes is left.
    Rebooted,
    /// It has exited, or its number now names another process or group.
    Gone,
}

/// A process as the system reports it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    /// The process group it leads or belongs to.
    pub group: u32,
    /// When it started, in the same terms as `Identity::start`.
    pub start: String,
    /// Whether it has exited and only its entry is left.
    pub zombie: bool,
}

/// Whether `identity`'s leader still leads its group, as `boot` and
/// `process` report them now.
#[must_use]
pub fn judge(identity: &Identity, boot: &str, process: Option<&Process>) -> Leader {
    if boot != identity.boot {
        return Leader::Rebooted;
    }

    let Some(process) = process else {
        return Leader::Gone;
    };

    if process.zombie || process.start != identity.start || process.group != identity.leader {
        return Leader::Gone;
    }

    Leader::Running
}

/// The boot that is running now: the kernel's boot id on Linux, and
/// `kern.bootsessionuuid` on macOS. A start hands it to the leader, which
/// records it in the identity it publishes itself.
pub fn boot() -> Result<String, String> {
    if cfg!(target_os = "linux") {
        proc_boot()
    } else if cfg!(target_os = "macos") {
        sysctl_boot()
    } else {
        Err("a command's identity is read on Linux and macOS only".into())
    }
}

/// The boot that is running now, as `/proc/sys/kernel/random/boot_id` names
/// it.
fn proc_boot() -> Result<String, String> {
    let text = fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|error| format!("reading the boot id: {error}"))?;
    let boot = text.trim();

    if boot.is_empty() {
        return Err("the boot id is empty".into());
    }

    Ok(boot.into())
}

/// The boot that is running now, as `sysctl -n kern.bootsessionuuid` gives
/// its per-boot UUID.
fn sysctl_boot() -> Result<String, String> {
    let mut command = Command::new("/usr/sbin/sysctl");
    command.args(["-n", "kern.bootsessionuuid"]);

    let output = run(command, READ_DEADLINE).map_err(|error| error.message)?;

    output
        .success
        .then(|| parse_uuid(&output.stdout))
        .flatten()
        .ok_or_else(|| "kern.bootsessionuuid is unreadable".to_owned())
}

/// Whether reading a process's file failed because the process has gone.
fn has_gone(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(ESRCH)
}

/// The process `pid` names, if there is one, read from `/proc` on Linux
/// and from `ps` on macOS.
fn process(pid: u32) -> Result<Option<Process>, String> {
    if cfg!(target_os = "linux") {
        proc_process(pid)
    } else if cfg!(target_os = "macos") {
        ps_process(pid)
    } else {
        Err("a command's identity is read on Linux and macOS only".into())
    }
}

/// The process `pid` names, if there is one, as its `/proc/<pid>/stat`
/// line describes it.
fn proc_process(pid: u32) -> Result<Option<Process>, String> {
    let path = format!("/proc/{pid}/stat");

    read_stat_again(&path, || fs::read_to_string(&path))
}

/// Reads `path` with `read`, and once more when the line it gave does not
/// parse. A process that is killed and reaped while its line is read can
/// leave a line that is not whole, and the second read then finds it gone;
/// a line that does not parse twice is an error, as before.
fn read_stat_again(
    path: &str,
    mut read: impl FnMut() -> std::io::Result<String>,
) -> Result<Option<Process>, String> {
    let first = read();
    let torn = first.as_ref().is_ok_and(|text| parse_stat(text).is_none());

    if torn {
        return read_stat(path, read());
    }

    read_stat(path, first)
}

/// What `read` gave for `path`: the process its stat line describes, none
/// when the process has gone, or why the read failed.
fn read_stat(path: &str, read: std::io::Result<String>) -> Result<Option<Process>, String> {
    match read {
        Ok(text) => parse_stat(&text)
            .map(Some)
            .ok_or_else(|| format!("unreadable {path}")),
        Err(error) if has_gone(&error) => Ok(None),
        Err(error) => Err(format!("reading {path}: {error}")),
    }
}

/// The process `pid` names, if there is one, as `ps` describes it.
fn ps_process(pid: u32) -> Result<Option<Process>, String> {
    let target = pid.to_string();
    let mut command = Command::new("/bin/ps");
    command
        .args(["-o", "lstart=", "-o", "pgid=", "-o", "stat=", "-p"])
        .arg(&target)
        .env("LC_ALL", "C")
        .env("TZ", "UTC0");

    let output = run(command, READ_DEADLINE).map_err(|error| error.message)?;

    read_ps(pid, output.success, &output.stdout, &output.stderr)
}

/// What the result of `ps` says about the process `pid` names: nothing,
/// the process, or why it could not be read. Output from a `ps` that
/// exited non-zero is never parsed.
fn read_ps(pid: u32, success: bool, stdout: &str, stderr: &str) -> Result<Option<Process>, String> {
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    let last_stderr = || stderr.lines().next_back().unwrap_or("no message");

    match (success, stdout.is_empty(), stderr.is_empty()) {
        // `ps` prints nothing and exits 1 when no process has that number.
        (false, true, true) => Ok(None),
        (true, false, _) => parse_ps(stdout)
            .map(Some)
            .ok_or_else(|| format!("unreadable ps output for {pid}")),
        (false, false, _) => Err(format!("ps failed for process {pid}: {}", last_stderr())),
        (_, true, false) => Err(format!(
            "ps could not read process {pid}: {}",
            last_stderr()
        )),
        (true, true, true) => Err(format!("ps printed nothing for process {pid}")),
    }
}

/// The process a `/proc/<pid>/stat` line describes: the group is field 5
/// and the start ticks field 22, as `proc(5)` numbers them, which are the
/// 3rd and 20th after the closing parenthesis. Its command name comes
/// first, in parentheses, and may hold parentheses and spaces too, so the
/// fields are read after the last `)`.
fn parse_stat(text: &str) -> Option<Process> {
    let mut fields = text.rsplit_once(')')?.1.split_whitespace();

    let state = fields.next()?;

    // The group comes after the parent, then sixteen fields of counters.
    let group = fields.nth(1)?.parse::<u32>().ok()?;
    let start = fields.nth(16)?;

    if !digits(start) {
        return None;
    }

    Some(Process {
        group,
        start: start.into(),
        zombie: matches!(state, "Z" | "X"),
    })
}

/// The process a `ps -o lstart= -o pgid= -o stat=` line describes: its
/// start time is the five fields before the group, and its state the last.
fn parse_ps(text: &str) -> Option<Process> {
    let mut fields = text.split_whitespace();

    let start = format!(
        "{} {} {} {} {}",
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?
    );
    let group = fields.next()?.parse::<u32>().ok()?;
    let zombie = fields.next()?.starts_with('Z');

    if fields.next().is_some() {
        return None;
    }

    Some(Process {
        group,
        start,
        zombie,
    })
}

/// The UUID `sysctl -n kern.bootsessionuuid` prints: 36 bytes of ASCII
/// hex digits, with `-` at byte indexes 8, 13, 18 and 23.
fn parse_uuid(text: &str) -> Option<String> {
    let uuid = text.trim();

    if uuid.len() != 36 {
        return None;
    }

    let uuid_shaped = uuid.bytes().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => byte == b'-',
        _ => byte.is_ascii_hexdigit(),
    });

    uuid_shaped.then(|| uuid.to_owned())
}

/// The value of a rendered identity's `key` line: the key, one space, and
/// a value with no whitespace around it.
fn field<'a>(line: Option<&'a str>, key: &str) -> Result<&'a str, String> {
    let rest = line
        .and_then(|line| line.strip_prefix(key))
        .and_then(|rest| rest.strip_prefix(' '))
        .ok_or_else(|| format!("no {key} line"))?;

    if rest.is_empty() {
        return Err(format!("the {key} is empty"));
    }

    if rest.trim() != rest {
        return Err(format!("the {key} has whitespace around it"));
    }

    Ok(rest)
}

/// The leader a rendered line names, which must be one process group's own,
/// so a signal reaches no other.
fn parse_leader(value: &str) -> Result<u32, String> {
    let number = value
        .parse::<i64>()
        .map_err(|_| format!("leader {value} is not a number"))?;

    if number < LOWEST_LEADER {
        return Err(format!("leader {value} is not a single process group"));
    }

    // `kill` truncates a larger number into its pid range, so a leader
    // has to fit one.
    if number > i64::from(i32::MAX) {
        return Err(format!("leader {value} is out of range"));
    }

    u32::try_from(number).map_err(|_| format!("leader {value} is out of range"))
}

/// Whether `text` is one or more digits.
fn digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An identity as `of` records one: group 4242, started at 987654 in
    /// the boot `boot-id`.
    fn recorded() -> Identity {
        Identity {
            leader: 4242,
            boot: "boot-id".into(),
            start: "987654".into(),
        }
    }

    /// The process that identity names, still running.
    fn running() -> Process {
        Process {
            group: 4242,
            start: "987654".into(),
            zombie: false,
        }
    }

    /// An identity text that names `leader`.
    fn identity_text(leader: &str) -> String {
        format!("atelier-command-identity 1\nleader {leader}\nboot b\nstart s\n")
    }

    /// A `/proc/<pid>/stat` line for `state`, `group` and `start`, whose
    /// command name holds parentheses and spaces.
    fn stat(state: &str, group: &str, start: &str) -> String {
        let head = format!("4242 (sh) (x y) {state} 1 {group} 4242 0 -1 4194560");
        let tail = format!("100 0 0 0 0 0 0 0 20 0 1 0 {start} 0 0");

        format!("{head} {tail}")
    }

    #[test]
    fn rendering_an_identity_parses_back_to_it() {
        let identity = recorded();
        let rendered = identity.render();

        assert_eq!(
            rendered,
            "atelier-command-identity 1\nleader 4242\nboot boot-id\nstart 987654\n"
        );
        assert_eq!(Identity::parse(&rendered), Ok(identity));
    }

    #[test]
    fn parsing_refuses_a_text_that_is_not_a_rendered_identity() {
        let refused = [
            ("", "no identity header"),
            (
                "atelier-command-identity 2\nleader 4242\nboot b\nstart s\n",
                "no identity header",
            ),
            (
                "atelier-command-identity 1\nboot b\nstart s\n",
                "no leader line",
            ),
            (
                "atelier-command-identity 1\nstart s\nboot b\nleader 4242\n",
                "no leader line",
            ),
            (
                "atelier-command-identity 1\nleader 4242\nstart s\n",
                "no boot line",
            ),
            (
                "atelier-command-identity 1\nleader 4242\nleader 4243\nboot b\n",
                "no boot line",
            ),
            (
                "atelier-command-identity 1\nleader 4242\nboot b\n",
                "no start line",
            ),
            (
                "atelier-command-identity 1\nleader \nboot b\nstart s\n",
                "the leader is empty",
            ),
            (
                "atelier-command-identity 1\nleader 4242 \nboot b\nstart s\n",
                "the leader has whitespace around it",
            ),
            (
                "atelier-command-identity 1\nleader 4242\nboot b\nstart s\nextra\n",
                "unexpected line after start: extra",
            ),
        ];

        for (broken, reason) in refused {
            assert_eq!(Identity::parse(broken), Err(reason.to_owned()), "{broken}");
        }
    }

    #[test]
    fn parsing_refuses_a_leader_that_could_reach_more_than_its_group() {
        for (leader, reason) in [
            ("0", "leader 0 is not a single process group"),
            ("1", "leader 1 is not a single process group"),
            ("-1", "leader -1 is not a single process group"),
            ("2147483648", "leader 2147483648 is out of range"),
            ("abc", "leader abc is not a number"),
        ] {
            assert_eq!(
                Identity::parse(&identity_text(leader)),
                Err(reason.to_owned()),
                "{leader}"
            );
        }
    }

    #[test]
    fn parsing_accepts_the_ends_of_a_leader_range() {
        for leader in ["2", "2147483647"] {
            let identity = Identity::parse(&identity_text(leader)).unwrap();

            assert_eq!(identity.leader.to_string(), leader);
        }
    }

    #[test]
    fn a_leader_runs_only_while_its_boot_start_and_group_still_match() {
        let identity = recorded();
        let (boot, other) = ("boot-id", "another-boot");

        assert_eq!(judge(&identity, boot, Some(&running())), Leader::Running);
        assert_eq!(judge(&identity, other, Some(&running())), Leader::Rebooted);
        assert_eq!(judge(&identity, boot, None), Leader::Gone);
        assert_eq!(
            judge(
                &identity,
                boot,
                Some(&Process {
                    zombie: true,
                    ..running()
                })
            ),
            Leader::Gone
        );
        assert_eq!(
            judge(
                &identity,
                boot,
                Some(&Process {
                    start: "1".into(),
                    ..running()
                })
            ),
            Leader::Gone
        );
        assert_eq!(
            judge(
                &identity,
                boot,
                Some(&Process {
                    group: 4243,
                    ..running()
                })
            ),
            Leader::Gone
        );
    }

    #[test]
    fn a_stat_line_gives_the_group_and_start_after_the_last_parenthesis() {
        assert_eq!(
            parse_stat(&stat("S", "4242", "987654")),
            Some(Process {
                group: 4242,
                start: "987654".into(),
                zombie: false,
            })
        );
    }

    #[test]
    fn a_stat_line_in_a_zombie_or_dead_state_is_marked_zombie() {
        for state in ["Z", "X"] {
            let process = parse_stat(&stat(state, "4242", "987654")).unwrap();

            assert!(process.zombie, "{state}");
        }
    }

    #[test]
    fn an_unreadable_stat_line_is_refused() {
        for broken in [
            String::new(),
            "4242 (sh) S 1 4242".to_owned(),
            stat("S", "4242", "98765x"),
            stat("S", "42x2", "987654"),
        ] {
            assert_eq!(parse_stat(&broken), None, "{broken:?}");
        }
    }

    #[test]
    fn a_stat_line_read_gives_the_process_it_describes() {
        assert_eq!(
            read_stat("/proc/4242/stat", Ok(stat("S", "4242", "987654"))),
            Ok(Some(Process {
                group: 4242,
                start: "987654".into(),
                zombie: false,
            }))
        );
    }

    #[test]
    fn a_proc_read_of_a_gone_process_is_no_process() {
        let path = "/proc/4242/stat";

        assert_eq!(
            read_stat(
                path,
                Err(std::io::Error::from(std::io::ErrorKind::NotFound))
            ),
            Ok(None)
        );
        assert_eq!(
            read_stat(path, Err(std::io::Error::from_raw_os_error(ESRCH))),
            Ok(None)
        );
    }

    #[test]
    fn a_proc_read_that_failed_another_way_is_an_error_not_a_gone_process() {
        let path = "/proc/4242/stat";

        for error in [
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            std::io::Error::from_raw_os_error(5),
        ] {
            let refused = read_stat(path, Err(error)).unwrap_err();

            assert!(
                refused.starts_with("reading /proc/4242/stat: "),
                "{refused}"
            );
        }

        assert_eq!(
            read_stat(path, Ok("garbage".into())),
            Err("unreadable /proc/4242/stat".into())
        );
    }

    /// Hands out the given reads in order, and counts them.
    fn reads(
        given: Vec<std::io::Result<String>>,
    ) -> (
        impl FnMut() -> std::io::Result<String>,
        std::rc::Rc<std::cell::Cell<usize>>,
    ) {
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        let counted = std::rc::Rc::clone(&count);
        let mut given = given.into_iter();

        let read = move || {
            counted.set(counted.get() + 1);
            given.next().expect("no more reads were given")
        };

        (read, count)
    }

    #[test]
    fn a_whole_stat_line_is_read_once() {
        let (read, count) = reads(vec![Ok(stat("S", "4242", "987654"))]);

        assert_eq!(
            read_stat_again("/proc/4242/stat", read),
            Ok(Some(Process {
                group: 4242,
                start: "987654".into(),
                zombie: false,
            }))
        );
        assert_eq!(count.get(), 1);
    }

    #[test]
    fn a_torn_stat_line_of_a_process_that_then_went_is_no_process() {
        let (read, count) = reads(vec![
            Ok("4242 (sh) S 1".into()),
            Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        ]);

        assert_eq!(read_stat_again("/proc/4242/stat", read), Ok(None));
        assert_eq!(count.get(), 2);
    }

    #[test]
    fn a_torn_stat_line_read_whole_the_second_time_gives_the_process() {
        let (read, _) = reads(vec![
            Ok("4242 (sh) S 1".into()),
            Ok(stat("Z", "4242", "987654")),
        ]);

        assert_eq!(
            read_stat_again("/proc/4242/stat", read),
            Ok(Some(Process {
                group: 4242,
                start: "987654".into(),
                zombie: true,
            }))
        );
    }

    #[test]
    fn a_stat_line_that_does_not_parse_twice_is_an_error() {
        let (read, count) = reads(vec![Ok("garbage".into()), Ok("garbage".into())]);

        assert_eq!(
            read_stat_again("/proc/4242/stat", read),
            Err("unreadable /proc/4242/stat".into())
        );
        assert_eq!(count.get(), 2);
    }

    #[test]
    fn a_ps_line_joins_its_first_five_fields_as_the_start_time() {
        assert_eq!(
            parse_ps(" Tue Nov 17 10:00:00 2026  4242 Ss"),
            Some(Process {
                group: 4242,
                start: "Tue Nov 17 10:00:00 2026".into(),
                zombie: false,
            })
        );
        assert!(
            parse_ps("Tue Nov 17 10:00:00 2026  4242 Z+")
                .unwrap()
                .zombie
        );
    }

    #[test]
    fn a_ps_line_with_too_few_or_too_many_fields_is_refused() {
        for broken in [
            "Tue Nov 17 10:00:00 2026  4242",
            "Tue Nov 17 10:00:00 2026  4242 Ss extra",
            "Tue Nov 17 10:00:00 2026  four two",
        ] {
            assert_eq!(parse_ps(broken), None, "{broken}");
        }
    }

    #[test]
    fn a_ps_result_with_no_process_is_none() {
        assert_eq!(read_ps(4242, false, "", ""), Ok(None));
    }

    #[test]
    fn a_ps_result_with_a_line_is_the_process() {
        assert_eq!(
            read_ps(4242, true, "Tue Nov 17 10:00:00 2026  4242 Ss\n", ""),
            Ok(Some(Process {
                group: 4242,
                start: "Tue Nov 17 10:00:00 2026".into(),
                zombie: false,
            }))
        );
    }

    #[test]
    fn a_failed_ps_is_an_error_even_when_its_output_would_parse() {
        assert_eq!(
            read_ps(
                4242,
                false,
                "Tue Nov 17 10:00:00 2026  4242 Ss",
                "ps: no such process\n"
            ),
            Err("ps failed for process 4242: ps: no such process".into())
        );
        assert_eq!(
            read_ps(4242, false, "Tue Nov 17 10:00:00 2026  4242 Ss", ""),
            Err("ps failed for process 4242: no message".into())
        );
    }

    #[test]
    fn an_empty_ps_output_is_an_error_naming_its_last_stderr_line() {
        assert_eq!(
            read_ps(4242, true, "", "ps: illegal option\nUsage: ps\n"),
            Err("ps could not read process 4242: Usage: ps".into())
        );
    }

    #[test]
    fn a_successful_ps_with_nothing_to_say_is_an_error() {
        assert_eq!(
            read_ps(4242, true, "", ""),
            Err("ps printed nothing for process 4242".into())
        );
    }

    #[test]
    fn a_boot_session_uuid_is_read_in_either_case_without_its_newline() {
        assert_eq!(
            parse_uuid("C3A6F5E2-1B2C-4D3E-8F90-0123456789AB\n"),
            Some("C3A6F5E2-1B2C-4D3E-8F90-0123456789AB".into())
        );
        assert_eq!(
            parse_uuid("c3a6f5e2-1b2c-4d3e-8f90-0123456789ab\n"),
            Some("c3a6f5e2-1b2c-4d3e-8f90-0123456789ab".into())
        );
    }

    #[test]
    fn a_text_that_is_not_a_boot_session_uuid_is_refused() {
        for broken in [
            "",
            "C3A6F5E2-1B2C-4D3E-8F90-0123456789A",
            "C3A6F5E2-1B2C-4D3E-8F90-0123456789ABC",
            "C3A6F5E2-1B2C-4D3E-8F90-0123456789AG",
            "C3A6F5E2X1B2C-4D3E-8F90-0123456789AB",
            "C3A6F5E21B2C-4D3E-8F90-0123456789AB-",
        ] {
            assert_eq!(parse_uuid(broken), None, "{broken:?}");
        }
    }

    /// Reads the real boot and this test's own process, and signals
    /// nothing.
    #[cfg(target_os = "linux")]
    #[test]
    fn checking_a_record_with_a_stale_start_or_another_boot_finds_no_leader() {
        let boot = boot().unwrap();
        let stale = Identity {
            leader: std::process::id(),
            boot: boot.clone(),
            start: "0".into(),
        };

        assert_eq!(stale.check(), Ok(Leader::Gone));

        let rebooted = Identity {
            boot: format!("{boot}-other"),
            ..stale
        };

        assert_eq!(rebooted.check(), Ok(Leader::Rebooted));
    }

    /// A child that inherits this test's group leads none, so `of`
    /// refuses it. Only the test's own child is killed.
    #[cfg(target_os = "linux")]
    #[test]
    fn of_refuses_a_process_that_does_not_lead_its_group() {
        let mut child = Command::new("/bin/sleep")
            .arg("5")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        assert!(Identity::of(child.id()).is_err());

        // This kills only the test's own child, never a group.
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Reading a process that has gone is told apart from a read that
    /// failed for another reason.
    #[cfg(target_os = "linux")]
    #[test]
    fn reading_a_gone_process_is_told_apart_from_other_errors() {
        assert!(has_gone(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));
        assert!(has_gone(&std::io::Error::from_raw_os_error(3)));
        assert!(!has_gone(&std::io::Error::from_raw_os_error(13)));
        assert!(!has_gone(&std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )));
    }

    /// Reads the real boot id and two real processes, and signals nothing.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_boot_is_the_kernels_boot_id() {
        let boot = boot().unwrap();
        let text = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();

        assert_eq!(boot.len(), 36);
        assert_eq!(boot, text.trim());
        assert_eq!(parse_uuid(&boot), Some(boot.clone()), "{boot}");

        let this = process(std::process::id()).unwrap().unwrap();

        assert!(digits(&this.start), "{this:?}");

        assert_eq!(process(u32::try_from(i32::MAX).unwrap()).unwrap(), None);
    }

    /// Reads this test's own process through `ps` and `/proc`, and signals
    /// nothing.
    #[cfg(target_os = "linux")]
    #[test]
    fn ps_and_proc_describe_the_same_process() {
        let pid = std::process::id();
        let from_ps = ps_process(pid).unwrap().unwrap();
        let from_proc = proc_process(pid).unwrap().unwrap();

        assert_eq!(from_ps.group, from_proc.group);
        assert_eq!(from_ps.start.split_whitespace().count(), 5);
        assert!(!from_ps.zombie);
        assert_eq!(ps_process(u32::try_from(i32::MAX).unwrap()).unwrap(), None);
    }

    /// Linux's `sysctl` has no `kern.bootsessionuuid` either, so the macOS
    /// reader gives an error rather than a boot.
    #[cfg(target_os = "linux")]
    #[test]
    fn sysctl_cannot_read_a_linux_boot() {
        // Linux has no `kern.bootsessionuuid`, so `sysctl -n` fails, and a
        // boot it cannot print is an error, never an empty one.
        assert!(sysctl_boot().is_err());
    }
}
