# `atelier.command`

atelier's own kind of step: a shell command run in the run's worktree, with
a time limit. `atelier` is the plugin that offers it, under its own name.

| File          | Holds                                                                        |
| ------------- | ---------------------------------------------------------------------------- |
| `mod.rs`      | `Atelier`, the plugin, and checking a command's settings                     |
| `run.rs`      | Starting a command in the background, and how it ended                       |
| `identity.rs` | Who leads a command's group, recorded at start and checked before any signal |

```toml
[[pipeline.security-audit.step]]
id = "deps"
uses = "atelier.command"
run = "cargo deny check advisories"
timeout = 300
```

```rust
run::start(&folder, &worktree, "main", &step.settings)?;

run::observe(&folder, &step.settings)?;
// → None while it runs, then Some(Outcome { name: "failed", why: "it exited 3:\n…" })
```

- **In the background.** A start returns at once; a later poll reads how it
  ended from the step's folder: `started`, `worktree`, `identity`, `output`
  and `exit`.
- **Starting again.** A start on a folder that already exists first stops
  whatever command the folder still names, through the same identity check,
  so a start repeated after a daemon crash cannot orphan a running command.
  It empties the folder only once a fresh check finds the leader gone or
  its record from another boot. While the record still names a leader that
  is stopping, the start fails with a transient error saying the command an
  earlier start left in the folder is still stopping, keeps the folder
  until it is gone, and retries on the next poll. A legacy record (a
  `group` file and no identity), an unpublished identity or an invalid one
  is kept, and the start fails with a transient error saying the command an
  earlier start left in the folder cannot be stopped, followed by the
  unsignalled text. The folder stays, since it is the only trace of a
  command that may still run. A start that cannot spawn removes the folder
  it just made, since nothing ran; one abandoned because its identity could
  not be recorded removes the folder once its group kill was delivered,
  keeping it only when the signal was refused.
- **Its own process group,** not the daemon's, so a daemon restarted while it
  runs leaves it running. While its leader runs, its limit signals every
  process still in that group, not descendants that create a different group
  or session.
- **Outcomes.** `passed` on exit 0; `failed` on any other exit, past its
  limit, or when it stopped without an exit status, each with why and the
  last 20 lines it printed.
- **Cleanup.** Every run exit cancels a command still running in its step's
  folder. A completed command needs no signal. Each pass uses its own
  generation folder, so a fresh pass cannot signal an older command's group.
- **`ATELIER_BASE`** is `origin/<base>`, as for the gates.

## Identity

A background command runs under a shell that leads its own process group
and stays alive until the command's top level ends, then writes `exit`. The
daemon records `started` and `worktree` before it spawns that shell, and
publishes the leader's identity before the start returns, by writing
`identity.tmp`, syncing it, renaming it to `identity`, and syncing the
folder. The record holds the leader's pid, which is the group's, the boot
it runs on and the leader's own start identity. A reaper thread waits on the
leader, so it is reaped once it ends; if that thread cannot start, the
leader stays a zombie until the daemon exits, and the identity check reads a
zombie as gone.

A crash between the spawn and the rename leaves `started` and `worktree`,
and perhaps `identity.tmp`, without `identity`: the command may still be
running, and atelier sends it no signal. `observe` settles the step failed with
a reason that says it started but its identity was never recorded. The reason ends
`; atelier sent no signal, and the command may still be running in <worktree>`,
or `in its worktree` when no worktree is recorded; `stop` sends nothing, and
its log line ends the same way, logged once for that folder and reason
rather than on every poll.

```text
atelier-command-identity 1
leader 4242
boot 6f1c…
start 987654
```

Before `observe` or `stop` sends anything, it checks the record: the same
boot, the leader alive and not a zombie, the same start identity, and still
leading its group. A record from another boot settles `failed` with "it
stopped before it exited, as on a restart of the computer"; a leader that is
gone settles `failed` with "it stopped before it exited". Neither sends a
signal. A folder holding only the older `group` file, an unpublished
identity, a malformed record, or a leader outside `2..=i32::MAX` fails
closed: `observe` settles `failed` with the same ending, `stop` sends
nothing and logs it, and a start over it fails with a transient error and
keeps the folder. While a start keeps failing this way, the run waits at
that step; a person ends the run with `atelier stop "<pipeline> <issue>"`,
and the folder stays for them to check and remove, since a later run starts
in its own generation's folder.
A running command past its limit is signalled through
`/bin/kill -KILL -- -<leader>`, which `atelier_command::signal_group` builds.
When that refuses a target, the kill and a start's abandon log
`<folder>: atelier refused to signal group <n>; the command may still be running in <worktree>`.
`kill` reports whether a signal was delivered, so a refused signal or a
failed `/bin/kill` is never read as a stop. A start's fresh check keeps a
folder whose leader still runs.

Linux reads the boot identity from `/proc/sys/kernel/random/boot_id`, and
the leader's start ticks, group and state from `/proc/<pid>/stat`. macOS
reads `kern.bootsessionuuid` through `/usr/sbin/sysctl`, and the leader's
start time, group and state from `/bin/ps -o lstart= -o pgid= -o stat=`
with `LC_ALL=C` and `TZ=UTC0`, so its start time does not depend on the
locale or the time zone. No new dependency.

The record narrows which process a signal reaches; it is not containment.
On macOS a start time resolves to the second, and checking before running
`/bin/kill` leaves a window in which the leader can exit and its number be
reused. A boot identity the system cannot report fails closed, and so does a
macOS value that is not a UUID: a start kills the command it began, and a
check sends no signal. Once the command's top level exits normally, its exit
status is written and a stop sends nothing, so background processes it left
behind are not signalled. Neither are descendants that leave the group or
outlive a leader killed by something else. An unrecorded or legacy command
is left running, and named in the log and the run's report. This is not a
proof of ownership. Tests that start or signal processes run only in the
contained CI gates.

## Separate descendant groups

A process-group deadline is not process-tree containment.
[cargo-mutants 27.1.0](https://github.com/sourcefrog/cargo-mutants/tree/8ab1dc786a1f61a4e370416cc6c68b81a704e917)
runs each Cargo build or test phase in its own process group. Its own phase
timeout sends that group SIGTERM and waits for the Cargo child; its Unix
implementation has no SIGKILL escalation. If atelier's deadline kills
cargo-mutants, that signal does not reach those separate groups and
cargo-mutants cannot perform its cleanup. Descendants can survive as
orphans; if they retain output pipes, the foreground command runner's reader
joins can also wait for them.

In CI an independent boundary settles this: the gate container has a private
PID namespace and a 75-minute outer timeout, after which `run.sh` forcibly
removes it. That removes all its processes, including escaped groups. The
gate remains failed, and incomplete or missing reports fail validation.
This does not prove cleanup by atelier's inner process-group deadline.

Mutation, fuzzing and generated process-signalling checks run only in
contained CI or a separately approved disposable VM, never on the Mac,
including through an installed daemon. A signal guard does not contain
untrusted code.

Sources:
[cargo phase launch](https://github.com/sourcefrog/cargo-mutants/blob/8ab1dc786a1f61a4e370416cc6c68b81a704e917/src/cargo.rs#L32-L66),
[Unix process groups and termination](https://github.com/sourcefrog/cargo-mutants/blob/8ab1dc786a1f61a4e370416cc6c68b81a704e917/src/process/unix.rs#L14-L38),
and [timeout and wait](https://github.com/sourcefrog/cargo-mutants/blob/8ab1dc786a1f61a4e370416cc6c68b81a704e917/src/process.rs#L105-L139).
