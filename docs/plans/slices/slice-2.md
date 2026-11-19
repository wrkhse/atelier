# Slice 2: run a project's gates

**Status: Implemented, `0.1.0-rc.7`.** Every part is released, atelier
checks itself with its own gates, and the [end-to-end runs](#end-to-end) on
atelier and on a fixture repository pass. `0.1.0-rc.8` is the current
release.

You can see every gate a project runs, run them yourself, and have GitHub run
each as its own check on a pull request. Every commit a builder makes is
already formatted and lint-fixed. A project lists its gates as commands, as
atelier does; checks plugins that find a project's gates by themselves are
planned for [Rust](https://github.com/wrkhse/atelier/issues/2),
[TLA+](https://github.com/wrkhse/atelier/issues/3) and
[TypeScript](https://github.com/wrkhse/atelier/issues/4).

```toml
# .atelier/config.toml
checks = ["commands"]

[commands.clippy]
run = "cargo clippy --workspace --all-targets --locked -- -D warnings"
fix = "cargo clippy --fix --allow-dirty --allow-staged --workspace"

[commands.prettier]
root = "docs"
run = "npx --yes prettier@3.9.10 --check ."
fix = "npx --yes prettier@3.9.10 --write ."
```

```sh
atelier gates
# .     commands/clippy
# docs  commands/prettier

atelier check
# docs  commands/prettier  fail     1.6 s
#     [warn] plans/README.md
#     [warn] Code style issues found in the above file. Run Prettier with --write to fix.
```

```mermaid
flowchart LR
    Config[".atelier/config.toml<br/>from origin/base"] --> Plugins["checks plugins<br/>roots, gates, fixes"]
    Tree["the pull request's tree"] --> Plugins
    Plugins -- "atelier gates --json --diff" --> Matrix["gates.yml<br/>one job per gate and root"]
    Matrix -- "atelier check --gate --root" --> Check["a check on the pull request<br/>commands/clippy (.)"]
    Plugins -- "fixes" --> Hook["each builder commit<br/>atelier fix --staged"]
```

## Recording

The installed `0.1.0-rc.7`, in a scratch clone of atelier: `atelier gates`
lists its 14 gates. A table that is not formatted is added under `docs/`, so
`atelier check` runs only the docs gate, which fails and prints why;
`atelier fix` fixes the file and the check passes. Last, the fixture
repository's pull request, whose CI is the workflow `atelier init` wrote,
shows each gate as its own check.

The terminal-control capture GIF is 39.9 seconds, 1200 pixels wide and 775 KB.
Frames from its middle and end are inspected for private data.

![atelier gates lists the gates; check fails on an unformatted docs table and fix fixes it; the fixture's pull request shows each gate as its own check](../../media/slice-2.gif)

## What it builds

### The checks role (`crates/core`)

A checks plugin reads a project's tree and returns data: its roots, the
directories its gates run in, and each root's gates and fixes as shell
commands with their setup. Core picks the roots a diff touches, each file
belonging to the deepest root that holds it, and applies the project's
`[paths]` overrides, the deepest matching path winning. It does no I/O, so
it is tested with an in-memory tree.

```toml
# .atelier/config.toml
[paths."vendor"]
checks = []                     # nothing under vendor/ is checked

[paths."tools/codegen"]
skip = ["commands/mutants"]     # generated code
```

Files: [`crates/core/src/checks/`](../../../crates/core/src/checks/mod.rs).

Verified by tests with fixed plugins: a file touches the deepest roots that
hold it, and `docsite/` is not inside `docs`; with no diff every root runs;
an empty diff runs nothing; `checks` and `skip` narrow a subtree; an
unknown plugin name is refused. Mutation testing covers the role.

### The `commands` plugin (`plugins/commands`)

The project names its gates as commands, each with an optional `root`,
`fix` and `setup`, and turns the plugin on in `checks`. A gate is named
`commands/<name>`. `atelier check` puts the base the changes are measured
from in `ATELIER_BASE`, so a gate such as mutation testing can test only
them:

```toml
[commands.mutants]
run = '''
diff=$(mktemp) && git diff "$ATELIER_BASE" > "$diff"
cargo mutants --in-diff "$diff"
'''
```

Files: [`plugins/commands/`](../../../plugins/commands/README.md), the
reference for its keys, names and refusals.

### Running gates (`crates/cli`)

`atelier gates`, `check` and `fix` run in a checkout. The gates come from
the project's config as the daemon reads it, from `origin/<base>`, so a pull
request cannot drop the gate it fails; the roots come from the checkout's own
tree, so a new root is checked in the pull request that adds it. `check`
runs the gates the changes since `origin/<base>` touch, committed or not,
and any failure prints the last 40 lines of its output and exits 1.

Files: `crates/cli/src/gates.rs`, `crates/git/src/tree.rs`, and the
[CLI reference](../../../crates/cli/README.md).

Verified through the real binary, in a checkout whose gates are real shell
commands: a change under `docs` plans only the `docs` gate, a failing gate
exits 1 with its output, and a gate added on the branch does not run until it
is on the base.

### Fixes before each commit (`crates/git`, `crates/cli`)

When atelier makes a builder's worktree, it gives that worktree alone a
pre-commit hook, through Git's per-worktree config, so every commit the
builder makes is already fixed and the builder does nothing for it.

```sh
git -C <worktree> config extensions.worktreeConfig true
git -C <worktree> config --worktree core.hooksPath ~/.local/share/atelier/hooks
```

The daemon writes `~/.local/share/atelier/hooks/pre-commit` at each start.
It runs `atelier fix --staged`, then the project's own pre-commit hook. A
staged file that also has unstaged changes is left as staged, so no unstaged
work slips into the commit. A fix that fails does not stop the commit; the
gate fails in CI instead.

Files: `crates/git/src/worktree.rs`, `crates/cli/src/hook.rs`.

Verified through the real binary: a commit in a builder's worktree, with no
`atelier` on `PATH`, is committed fixed, the project's own hook runs, and
neither the checkout nor the clone gains a hooks folder.

### GitHub's CI (`plugins/github`, `.github/workflows/gates.yml`)

`atelier init` from a release build writes a workflow that calls atelier's
reusable `gates.yml`, pinned to the commit the release was built from:

```yaml
# .github/workflows/atelier.yml
jobs:
  gates:
    uses: wrkhse/atelier/.github/workflows/gates.yml@<commit> # v0.1.0-rc.8
    with:
      atelier: 0.1.0-rc.8
```

`gates.yml` reads `atelier gates --json` into a matrix, with `--diff` on a
pull request and every root on a push, and runs each gate in its root as its
own job, named such as `commands/clippy (.)`. A failure reaches the builder
through sourcefed. How each job is contained, and the reports a gate must
leave, is in [the CI runner](../../../.github/ci/README.md).

Files: [`plugins/github/src/ci.rs`](../../../plugins/github/src/ci.rs),
[`.github/workflows/gates.yml`](../../../.github/workflows/gates.yml).

### atelier checks itself (`.atelier/config.toml`)

atelier lists its 14 gates as commands in
[`.atelier/config.toml`](../../../.atelier/config.toml), and its CI calls
`gates.yml` from its own tree with `atelier: source`, so a pull request that
changes `gates.yml` runs it as changed. The gates themselves come from
`main`'s config, as for any project.

## End to end

Each run is real: atelier's own pull requests, an issue labelled with the
owner's approval and followed by the Homebrew service, and a public fixture
repository created for the check.

| Run                                                                                                          | What happens                                                                                                                                                                                                                                                                                                                                           |
| ------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| atelier's checkout                                                                                           | `atelier gates` lists the 14 gates of `.atelier/config.toml`; `atelier check` runs only the gates the changes touch.                                                                                                                                                                                                                                   |
| [#35](https://github.com/wrkhse/atelier/pull/35), [#36](https://github.com/wrkhse/atelier/pull/36)           | #35 touches only `.github`, `docs` and `scripts`, and runs only their three gates. #36 touches the code and runs every gate, each as its own check, all passing.                                                                                                                                                                                       |
| [atelier#37](https://github.com/wrkhse/atelier/issues/37) → [#38](https://github.com/wrkhse/atelier/pull/38) | The builder writes `pub   const   E2E_HOOK :   u8 = 1 ;` and its commit holds `pub const E2E_HOOK: u8 = 1;`: the hook formatted it. A table row it was asked to leave as written fails `commands/readmes (.)`, which reaches it through sourcefed; it fixes the row, every gate passes, and atelier tells it so, 12 seconds before it marks #38 ready. |
| A fixture repository's first pull request                                                                    | `atelier init` from the `0.1.0-rc.7` release writes `atelier.yml` pinned to the release's commit. Its pull request installs that release, its sum checked; `commands/todo (notes)` fails on a TODO beside a passing `commands/readme (.)`, then passes after the fix.                                                                                  |

The first full run on `main` found a fault in the runs themselves: the
command runner closed a tool's output after its first MiB, so kani, which
prints more, was stopped on its next write, and the failure's reason was
never kept. Each output is read to its end and its last MiB kept
([#36](https://github.com/wrkhse/atelier/pull/36)).
