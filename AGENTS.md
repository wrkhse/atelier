# Working on atelier

atelier is a CI engine for monorepos. Its gates live in inheritable moon task
files and run in CI or a container, never on a developer's machine. The
presets and their containment arrive with slice 1 (#87).

On a developer's machine, use file tools, `git` and `gh`. Do not run builds,
checks, tests, site or documentation builds, or any atelier binary locally.
This applies to child agents too. Do not run a local container as a workaround.

Gates must run on GitHub-hosted CI inside a bounded, non-root Linux container
with a private PID namespace, no capabilities, no host network, no Docker
socket and no credentials, with bounded memory, processes, CPU and time. A
missing or empty report fails. Never weaken a gate or exclude signalling code
to obtain a pass. A change to CI or the runner needs review for what it
loosens: CI cannot catch a pull request that disables its own containment.

Use a separate branch and worktree for each pull request, and preserve other
branches' work.

Issues hold what a person can do and how it is verified; code detail goes in
the pull request. Docs describe what runs today; planned work goes on the
roadmap. Keep lines at most 85 columns and write in the present tense.
