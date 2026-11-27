# Contributing

## One-time local setup

Enable this repository's pre-commit hook once per checkout:

    git config core.hooksPath .githooks

On each commit the hook formats only staged files: rustfmt (edition 2024) on
staged .rs paths, and tidy v2.0.0 on the other staged files using tidy's
defaults (this repository has no tidy config). tidy skips file types it does
not recognize. The hook re-stages the formatted result. A recognized file it
cannot parse, a partially staged file, or a tidy download/checksum/platform
failure stops the commit with a named reason. It runs no clippy, compile or
tests. The tidy pins live as plain data in .github/tidy.env; the binary is
verified by SHA-256 on first use and its cached archive is verified on every
run at `~/.cache/wrkhse/tidy-<sha>/`.

CI runs the hook selftest on Ubuntu and macOS on pull requests that change
hook paths, on pushes to main, daily on main, and on demand.
