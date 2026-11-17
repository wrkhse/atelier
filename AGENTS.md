# Working on atelier

On a developer's machine, use file tools, `git`, `gh`, and `cargo fmt`
only. Do not run Cargo build, check, Clippy, test, doc or metadata; nextest,
Kani, mutants; site or documentation builds; or the atelier binary. This
applies to child agents too. Do not run a local container as a workaround.

Every check runs on GitHub-hosted CI. The planner and all gates execute as a
non-root user inside a Linux container with a private PID namespace, no
capabilities, no host network, no Docker socket and no credentials, with
bounded memory, processes, CPU and time; a separate offline container
validates the Kani and mutation reports, and a missing or empty one fails.
[The CI runner](https://github.com/wrkhse/atelier/tree/main/.github/ci)
describes it. Never weaken a gate or exclude signalling code to obtain a pass.

Developers never run gates on the Mac. By the owner's proof contract of
2026-11-15, the installed daemon orchestrates real repositories, while
mutation, fuzzing and process-signalling checks run only in contained CI,
which the daemon observes through `forge.checks`. A local command step in
the proof is benign, bounded and confined to repository data.
`atelier_command::signal_group` guards only the signals atelier itself
sends and is defence in depth, not containment: a mutant can change the
guard itself. Every group signal still goes through it, and a change that
builds one another way is refused in review; the only other kills are std
`Child::kill` calls on a single child the caller still owns.

Atelier's own pull requests run the workflow and `.github/ci` they contain,
so a change to either needs review for what it loosens; CI cannot catch a
pull request that disables its own containment.

Do not edit the user's atelier configuration, durable state, managed clones
or historical worktree paths as part of development. Use a separate branch
and worktree for each pull request, and preserve other branches' work.
