# `atelier.command`

atelier's own kind of step: a shell command run in the run's worktree, with
a time limit. `atelier` is the plugin that offers it, under its own name.

| File     | Holds                                                    |
| -------- | -------------------------------------------------------- |
| `mod.rs` | `Atelier`, the plugin, and checking a command's settings |
| `run.rs` | Starting a command in the background, and how it ended   |

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
  ended from the step's folder: `started`, `group`, `output` and `exit`.
- **Its own process group,** not the daemon's, so a daemon restarted while it
  runs leaves it running. Its limit signals every process still in that
  group, not descendants that create a different group or session.
- **Outcomes.** `passed` on exit 0; `failed` on any other exit, past its
  limit, or when it stopped without an exit status, each with why and the
  last 20 lines it printed.
- **Cleanup.** Every run exit cancels a command still running in its step's
  folder. A completed command needs no signal. Each pass uses its own
  generation folder, so a fresh pass cannot signal an older command's group.
- **`ATELIER_BASE`** is `origin/<base>`, as for the gates.

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
