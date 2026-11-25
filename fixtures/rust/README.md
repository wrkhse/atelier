# Gate fixture

`fixtures/rust` is the gate fixture: a one-crate Rust workspace, `app`, with
one function and one test, no dependencies, that passes every gate.

The `fixture` workflow, `.github/workflows/fixture.yml`, proves each gate
fails closed. For each of `rust/fmt`, `rust/clippy`, `rust/test` and
`rust/mutants` it applies one deliberate violation to the reporter crate,
commits it in the job's checkout only, runs that one gate in the contained
gate container and requires it to fail. It then restores the reporter and
requires the same gate to pass.

```mermaid
flowchart LR
    Apply["fixtures/violations/fmt.sh<br/>commit in the checkout"] --> Fail["run.sh gate<br/>rust/fmt must fail"]
    Fail --> Restore["reset to the clean commit"]
    Restore --> Pass["run.sh gate<br/>rust/fmt must pass"]
```

Each script in `fixtures/violations` takes no arguments and runs from the
repository root.

| Script       | Violation                                                        | Fails          |
| ------------ | ---------------------------------------------------------------- | -------------- |
| `fmt.sh`     | Appends an unformatted function to `reporter/src/lib.rs`         | `rust/fmt`     |
| `clippy.sh`  | Appends a formatted function with a needless `return`            | `rust/clippy`  |
| `test.sh`    | Creates `reporter/tests/violation.rs`, one test that panics      | `rust/test`    |
| `mutants.sh` | Adds `reporter/src/violation.rs`, a pure function no test covers | `rust/mutants` |

The mutants gate tests only the lines in `git diff BASE...HEAD`, so the
workflow commits each violation in its own checkout; nothing is pushed, and
the job holds no credential.

Kani, Miri and MSRV get native violations in the next pull request of this
slice. Until then the reporter's tests cover how their failures map to a
failed check, fail-closed.
