# atelier

atelier turns a labelled GitHub issue into a pull request ready for your review.
You add `atelier:build` to an issue; atelier starts an OpenCode builder on its
own branch and worktree, the builder opens a draft pull request and pushes fixes
until every check passes, then marks it ready.

Your project's gates run in GitHub's CI, one check each, and their fixes, such
as a formatter, run on every commit a builder makes. Labels on the issue
show where its work is, and the daemon saves each step before taking it, so it
can stop at any moment and carry on where it was.

## Install

atelier runs on macOS on Apple silicon and on Linux, arm64 or x64. It needs
`git`, the GitHub CLI signed in (`gh auth status`), and OpenCode. Download the
release, check it against its checksums, and put it on your `PATH`:

```sh
release=0.1.0-rc.8
# or linux-arm64, linux-x64
target=darwin-arm64
archive="atelier-$release-$target.tar.gz"

gh release download "v$release" --repo wrkhse/atelier \
  --pattern "$archive" --pattern SHA256SUMS

# sha256sum --check on Linux
grep " $archive\$" SHA256SUMS | shasum -a 256 --check

tar -xzf "$archive" bin/atelier
mkdir -p ~/.local/bin
install -m 755 bin/atelier ~/.local/bin/atelier

atelier --version
# atelier 0.1.0-rc.8
```

### OpenCode

Each builder is an ordinary OpenCode session, started through `opencode api`, so
`opencode` must be on the daemon's `PATH` and signed in to a model. This answers
when it is ready:

```sh
opencode api get /api/info
```

A builder's session shows in OpenCode like any other: open it to watch or talk
to it. It may push only to its own branch, and asks no questions; it writes on
the pull request instead.

### A project

In a checkout whose `origin` is on GitHub, make the labels and the CI workflow,
commit the workflow, then watch the project and run the daemon:

```sh
atelier init
# owner/my-project: wrote .github/workflows/atelier.yml
# owner/my-project: labels atelier:build, atelier:working, atelier:blocked, improvement

git add .github/workflows/atelier.yml
git commit -m "ci: run atelier's gates"
git push

atelier add owner/my-project
# watching owner/my-project, cloned in /Users/me/.local/share/atelier/clones/…

atelier daemon
```

`atelier daemon` polls every watched project until it is stopped; keep it
running under whatever keeps your processes alive. It reads its own clone of
each project, never your checkout. The workflow `init` writes calls atelier's
reusable gates workflow, pinned to this release's commit.

## Turn an issue into a pull request

```sh
gh issue edit 12 --add-label atelier:build

# a poll later, 30 seconds by default
atelier status
# owner/my-project  /Users/me/.local/share/atelier/clones/owner/my-project.git
#   base        main                     found
#   ...
#   github#12   Building                 issue-12-fix-the-dead-link  ses_…  -
```

The issue's `atelier:build` label becomes `atelier:working`, and a comment names
the builder's session and branch. The builder reads the issue and the project's
`AGENTS.md`, and opens a draft pull request that says `Part of #12`.

```mermaid
sequenceDiagram
    participant You
    participant D as atelier daemon
    participant B as OpenCode builder
    participant PR as Pull request and CI
    You->>D: label atelier:build
    D->>B: worktree, branch and brief
    B->>PR: draft pull request
    PR-->>B: failed check
    B->>PR: fix and push
    PR-->>D: every check passes
    D->>B: every check passed
    B->>PR: mark ready
    PR-->>You: ready for review
```

## Run a project's gates

A project lists its gates as shell commands in `.atelier/config.toml`, with
`checks = ["commands"]` and a `[commands.<name>]` table for each, as the
[commands plugin](plugins/commands/README.md) shows. In CI, each gate the pull
request's changes touch is its own check. In a checkout, `atelier gates` lists
them, `atelier check` runs the ones your changes touch, and
`atelier check --gate commands/clippy --root .` runs one. A gate with a `fix`
command fixes its files in each builder commit, and `atelier fix --staged` does
the same for yours.

## Follow, stop and restart a builder

`atelier status` shows each watched project's settings, where each came from,
and each issue with its phase, branch, session and pull request. A builder that
only a person can unblock comments on its pull request, labels the issue
`atelier:blocked` and stops; after five failed CI rounds, atelier stops it the
same way. Removing `atelier:working` stops a builder. Adding `atelier:build`
again starts one on the same branch and pull request.

## Configure a project

Every setting has a default found from the project: the base is the default
branch, and the tracker and forge come from `origin`. `.atelier/config.toml`
holds only what differs, read from `origin`'s default branch, so a branch or a
pull request cannot change how atelier treats it. The
[command reference](crates/cli/README.md) lists the settings and timings.

## Roadmap

- [ ] Rust gates found by themselves
  ([#2](https://github.com/wrkhse/atelier/issues/2))
- [ ] TLA+ gates found by themselves
  ([#3](https://github.com/wrkhse/atelier/issues/3))
- [ ] TypeScript gates found by themselves
  ([#4](https://github.com/wrkhse/atelier/issues/4))
- [ ] A code map ([#5](https://github.com/wrkhse/atelier/issues/5))
- [ ] Runs started from sourcefed events
  ([#42](https://github.com/wrkhse/atelier/issues/42))
- [ ] Pipelines of steps, with `atelier finish` and `atelier stop`, in a
  release ([#43](https://github.com/wrkhse/atelier/issues/43))

## More

- [The `atelier` command](crates/cli/README.md): every command, the daemon and
  its state.
- [The crates](crates/README.md), and the
  [github](plugins/github/README.md), [opencode](plugins/opencode/README.md)
  and [commands](plugins/commands/README.md) plugins.
- [The design](docs/plans/README.md), with the recordings of each part working,
  and [the architecture](https://github.com/wrkhse/atelier/issues/1).
- [Working on atelier](AGENTS.md) and
  [the CI runner](https://github.com/wrkhse/atelier/tree/main/.github/ci).
- Every page as a site, at
  [atelier-docs.stevenjpx2.workers.dev](https://atelier-docs.stevenjpx2.workers.dev).
