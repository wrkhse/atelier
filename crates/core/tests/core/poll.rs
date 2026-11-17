//! A poll through `observe` and `act`, against an in-memory world.

use atelier_core::rungs::{Outcome, Progress, Status};
use atelier_core::{
    Action, Failure, Filter, IssueKey, Mark, Marks, Observed, Phase, PullRequest, Reason, Record,
    Rollup, State, StepRecord, Tag, act, observe,
};

use crate::world::{World, list, step, table};

fn key() -> IssueKey {
    IssueKey {
        tracker: "mem".into(),
        id: "12".into(),
    }
}

fn phase(state: &State) -> Phase {
    state
        .runs
        .get(&key())
        .map_or(Phase::Untracked, |record| record.phase)
}

/// The status of the step called `id`.
fn status(state: &State, id: &str) -> Option<Status> {
    let record = state.runs.get(&key())?;
    let index = record.steps.iter().position(|step| step.id == id)?;

    record.progress.status.get(index).copied()
}

fn building() -> (World, State) {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    let mut state = State::default();
    world.poll(&world.project(), &mut state);
    (world, state)
}

const fn open_pull(failed_rounds: u8, checks: Rollup, draft: bool) -> PullRequest {
    PullRequest::Open {
        number: 31,
        draft,
        failed_rounds,
        checks,
    }
}

/// The builder was interrupted once, then told `why` and to act on nothing
/// later.
fn stopped(world: &World, why: &str) {
    let told = world.told.borrow();

    assert_eq!(*world.interrupts.borrow(), ["ses_0"]);
    assert!(
        matches!(
            told.last(),
            Some((session, text)) if session == "ses_0"
                && text.contains(why)
                && text.contains("Take no further action")
        ),
        "{told:?}"
    );
}

#[test]
fn a_requested_issue_starts_one_builder_across_many_polls() {
    let (world, mut state) = building();
    let project = world.project();

    for _ in 0..5 {
        world.poll(&project, &mut state);
    }

    let record = &state.runs[&key()];
    let ticket = world.ticket("12");

    assert_eq!(record.phase, Phase::Running);
    assert_eq!(record.pipeline, "build");
    assert_eq!(
        status(&state, "worktree"),
        Some(Status::Settled(Outcome(0)))
    );
    assert_eq!(status(&state, "builder"), Some(Status::Running));
    assert_eq!(record.branch, "issue-12-fix-the-dead-link");
    assert_eq!(*world.started.borrow(), ["worktree", "builder"]);
    assert_eq!(world.sessions.borrow().len(), 1);
    assert_eq!(ticket.marks, Marks::default().with(Tag::Working));
    assert_eq!(ticket.comments.len(), 1);
    assert!(ticket.comments[0].contains("`builder`"));
    assert!(ticket.comments[0].contains("ses_0"));
    assert!(ticket.comments[0].contains("issue-12-fix-the-dead-link"));

    let brief = &world.briefs.borrow()[0];
    assert_eq!(brief.title, "#12 Fix the dead link");
    assert!(brief.text().contains("Read #12"));
    assert!(brief.text().contains("into v2"));
    assert!(brief.denied.contains(&"merge *".to_owned()));
}

/// A stop after any save resumes from that save: the run ends where an
/// uninterrupted run ends, with every mark and the worktree as they were.
#[test]
fn a_stop_after_any_save_resumes_to_the_same_end() {
    let (clean, clean_state) = building();

    for crash_at in 1..=12 {
        let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
        let project = world.project();
        let plugins = world.plugins();
        let mut saved = State::default();
        let mut working = saved.clone();
        let mut count = 0;

        let observation = observe(&project, &plugins, &saved).unwrap();
        let mut save = |state: &State| {
            count += 1;
            if count == crash_at {
                return Err(());
            }
            saved = state.clone();
            Ok(())
        };
        let _ = act(&observation, &mut working, &project, &plugins, &mut save);

        let mut state = saved;
        for _ in 0..3 {
            world.poll(&project, &mut state);
        }

        assert_eq!(phase(&state), Phase::Running, "stop at save {crash_at}");
        assert_eq!(
            status(&state, "builder"),
            Some(Status::Running),
            "stop at save {crash_at}"
        );
        assert_eq!(
            world.ticket("12").marks,
            clean.ticket("12").marks,
            "stop at save {crash_at}"
        );
        assert_eq!(
            *world.worktrees.borrow(),
            *clean.worktrees.borrow(),
            "stop at save {crash_at}"
        );
        assert!(
            world.sessions.borrow().len() <= 2,
            "stop at save {crash_at}"
        );
        assert_eq!(
            state.runs[&key()].worktree,
            clean_state.runs[&key()].worktree
        );
    }
}

#[test]
fn a_settled_step_without_restart_is_not_observed_again() {
    let (world, mut state) = building();
    world.observed.borrow_mut().clear();

    world.poll(&world.project(), &mut state);

    assert_eq!(*world.observed.borrow(), ["builder"]);
    assert_eq!(
        status(&state, "worktree"),
        Some(Status::Settled(Outcome(0)))
    );
}

#[test]
fn a_stale_listing_of_a_closed_or_unmarked_item_starts_no_run() {
    use atelier_core::{Issue, Observation, Seen, Watched};
    use std::collections::BTreeMap;

    for (open, marks) in [
        (false, Marks::default().with(Tag::Working)),
        (true, Marks::default()),
    ] {
        let world = World::with_issue("12", "Fix the dead link", &[]);
        let issue = Issue {
            key: key(),
            title: "Fix the dead link".into(),
            open,
            marks: marks.clone(),
        };
        let watched = Watched {
            issue,
            seen: Seen {
                open,
                marks,
                pull_request: None,
            },
        };
        let observation = Observation {
            issues: BTreeMap::from([(key(), watched)]),
            ..Observation::default()
        };
        let mut state = State::default();

        let report = act(
            &observation,
            &mut state,
            &world.project(),
            &world.plugins(),
            &mut |_| Ok::<(), ()>(()),
        )
        .unwrap();

        assert_eq!(report.moves, []);
        assert_eq!(state.runs.len(), 0);
    }
}

#[expect(
    clippy::unwrap_used,
    reason = "test fixture: the in-memory world must answer and save"
)]
fn requested(
    world: &World,
    project: &atelier_core::Project,
    state: &mut State,
    request: atelier_core::Request,
) -> atelier_core::Report {
    let plugins = world.plugins();
    let mut observation = observe(project, &plugins, state).unwrap();
    observation.requests.insert(key(), vec![request]);

    act(&observation, state, project, &plugins, &mut |_| {
        Ok::<(), ()>(())
    })
    .unwrap()
}

#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test fixture: building holds its builder session"
)]
fn finish(state: &State, name: &str) -> atelier_core::Request {
    let record = &state.runs[&key()];
    atelier_core::Request {
        generation: record.generation,
        command: atelier_core::Command::Finish {
            step: "builder".into(),
            outcome: atelier_core::Outcome {
                name: name.into(),
                why: "nothing needs changing".into(),
            },
            session: record.steps[1].session.as_ref().unwrap().id.clone(),
            failed: record.progress.failed[1],
        },
    }
}

#[test]
fn a_finish_request_settles_its_step_and_is_harmless_when_replayed() {
    let (world, mut state) = building();
    let request = finish(&state, "nothing");

    requested(&world, &world.project(), &mut state, request.clone());
    let runs = state.runs.clone();
    requested(&world, &world.project(), &mut state, request);

    assert_eq!(phase(&state), Phase::Done);
    assert_eq!(state.runs, runs);
    assert_eq!(status(&state, "builder"), Some(Status::Settled(Outcome(1))));
    assert!(
        world
            .ticket("12")
            .comments
            .last()
            .unwrap()
            .contains("nothing needs changing")
    );
}

#[test]
fn stale_or_invalid_finish_requests_leave_the_run_alone() {
    let (world, mut state) = building();
    let good = finish(&state, "nothing");
    let mut old_pass = good.clone();
    old_pass.generation = old_pass.generation.saturating_sub(1);
    let mut old_session = good.clone();
    if let atelier_core::Command::Finish { session, .. } = &mut old_session.command {
        *session = "replaced-session".into();
    }
    let mut old_round = good;
    if let atelier_core::Command::Finish { failed, .. } = &mut old_round.command {
        *failed = 1;
    }
    let unknown = finish(&state, "unknown");

    for request in [old_pass, old_session, old_round, unknown] {
        let before = state.runs.clone();

        let report = requested(&world, &world.project(), &mut state, request);

        assert_eq!(state.runs, before);
        assert_eq!(report.moves, []);
    }
}

#[test]
fn a_stop_request_preempts_pending_starts_and_removes_the_request_mark() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    let mut state = State::default();
    let mut record = Record::new(&key(), "Fix the dead link", "build");
    record.begin(&world.project().pipelines[0]);
    record.phase = Phase::Claiming;
    record.pending = vec![Action::Mark(Mark::Working), Action::Unmark(Mark::Requested)];
    let request = atelier_core::Request {
        generation: record.generation,
        command: atelier_core::Command::Stop,
    };
    state.runs.insert(key(), record);

    requested(&world, &world.project(), &mut state, request);

    assert_eq!(phase(&state), Phase::Stopped);
    assert_eq!(world.ticket("12").marks, Marks::default());
    assert_eq!(*world.started.borrow(), Vec::<String>::new());
}

#[test]
fn a_finish_waits_for_a_pending_start_and_cannot_settle_its_replacement() {
    let (world, mut state) = building();
    let request = finish(&state, "nothing");
    state.runs.get_mut(&key()).unwrap().pending = vec![Action::Start(1)];

    requested(&world, &world.project(), &mut state, request);

    assert_eq!(phase(&state), Phase::Running);
    assert_eq!(world.sessions.borrow().len(), 2);
    assert_eq!(
        state.runs[&key()].steps[1].session.as_ref().unwrap().id,
        "ses_1"
    );
}

#[test]
fn a_stop_request_takes_priority_over_a_queued_finish() {
    let (world, mut state) = building();
    let finish = finish(&state, "nothing");
    let stop = atelier_core::Request {
        generation: state.runs[&key()].generation,
        command: atelier_core::Command::Stop,
    };
    let project = world.project();
    let plugins = world.plugins();
    let mut observation = observe(&project, &plugins, &state).unwrap();
    observation.requests.insert(key(), vec![finish, stop]);

    act(&observation, &mut state, &project, &plugins, &mut |_| {
        Ok::<(), ()>(())
    })
    .unwrap();

    assert_eq!(phase(&state), Phase::Stopped);
    assert!(world.ticket("12").open);
}

#[test]
fn a_transient_failure_retries_and_keeps_the_listing_unconfirmed() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    let project = world.project();
    let mut state = State::default();
    *world.fail.borrow_mut() = Some((3, Failure::Transient));

    let report = world.poll(&project, &mut state);

    assert_eq!(report.failures.len(), 1);
    assert_eq!(phase(&state), Phase::Running);
    assert_eq!(status(&state, "worktree"), Some(Status::Running));
    assert_eq!(state.etags.len(), 0);

    let report = world.poll(&project, &mut state);

    assert_eq!(report.failures, []);
    assert_eq!(status(&state, "builder"), Some(Status::Running));
    assert_eq!(state.etags.len(), 2);
}

#[test]
fn a_permanent_start_failure_blocks_the_run() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    let project = world.project();
    let mut state = State::default();
    *world.fail.borrow_mut() = Some((3, Failure::Permanent));

    world.poll(&project, &mut state);

    let ticket = world.ticket("12");
    assert_eq!(phase(&state), Phase::Blocked(Reason::Start(0)));
    assert_eq!(ticket.marks, Marks::default().with(Tag::Blocked));
    assert!(ticket.comments[0].contains("could not start `worktree`"));
    assert!(ticket.comments[0].contains("call 3 fails"));
    assert_eq!(*world.sessions.borrow(), []);
}

#[test]
fn a_claim_that_fails_for_good_blocks_the_run() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    let mut state = State::default();
    *world.fail.borrow_mut() = Some((1, Failure::Permanent));

    world.poll(&world.project(), &mut state);

    assert_eq!(phase(&state), Phase::Blocked(Reason::Claim));
    assert!(world.ticket("12").comments[0].contains("could not claim"));
}

#[test]
fn a_fresh_pass_keeps_each_steps_own_previous_session() {
    let (world, state) = building();
    let project = world.project();
    let mut record = state.runs[&key()].clone();
    let previous = record.steps[1].session.clone();

    record.begin(&project.pipelines[0]);

    assert!(previous.is_some());
    assert_eq!(record.steps[1].session, previous);
    assert_eq!(record.steps[0].session, None);
    assert_eq!(record.steps[2].session, None);
}

#[test]
fn a_blocked_run_requested_again_continues_on_its_branch() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    let project = world.project();
    let mut state = State::default();
    *world.fail.borrow_mut() = Some((3, Failure::Permanent));
    world.poll(&project, &mut state);
    *world.fail.borrow_mut() = None;

    world.edit("12", |ticket| {
        ticket.marks = std::mem::take(&mut ticket.marks).with(Tag::Trigger("build"));
    });
    world.poll(&project, &mut state);

    assert_eq!(phase(&state), Phase::Running);
    assert_eq!(status(&state, "builder"), Some(Status::Running));
    assert_eq!(
        world.ticket("12").marks,
        Marks::default().with(Tag::Working)
    );
    assert_eq!(state.runs[&key()].branch, "issue-12-fix-the-dead-link");
}

#[test]
fn removing_the_working_mark_stops_the_run_and_removes_its_worktree() {
    let (world, mut state) = building();

    world.edit("12", |ticket| {
        ticket.marks = std::mem::take(&mut ticket.marks).without(Tag::Working);
    });
    world.poll(&world.project(), &mut state);

    assert_eq!(phase(&state), Phase::Stopped);
    stopped(&world, "lost its working mark or was closed");
    assert_eq!(world.worktrees.borrow().len(), 0);
}

#[test]
fn closing_the_issue_stops_the_builder_and_drops_the_working_mark() {
    let (world, mut state) = building();

    world.edit("12", |ticket| ticket.open = false);
    world.poll(&world.project(), &mut state);

    assert_eq!(phase(&state), Phase::Stopped);
    stopped(&world, "lost its working mark or was closed");
    assert!(!world.ticket("12").marks.has(Tag::Working));
}

#[test]
fn failed_rounds_up_to_the_limit_block_the_run() {
    let (world, mut state) = building();
    let project = world.project();
    let branch = state.runs[&key()].branch.clone();

    for (failed, rounds, expected) in [
        (0, 0, Phase::Running),
        (2, 2, Phase::Running),
        (3, 3, Phase::Blocked(Reason::Failed(2))),
    ] {
        world
            .pulls
            .borrow_mut()
            .insert(branch.clone(), open_pull(failed, Rollup::Failed, true));
        world.poll(&project, &mut state);
        assert_eq!(phase(&state), expected, "{failed} failed rounds");
        assert_eq!(state.runs[&key()].progress.failed[2], rounds);
    }

    let ticket = world.ticket("12");
    assert_eq!(ticket.marks, Marks::default().with(Tag::Blocked));
    assert!(
        ticket
            .comments
            .last()
            .unwrap()
            .contains("`checks` failed: CI failed on 3 commits")
    );
    stopped(&world, "CI failed on 3 commits");
    assert_eq!(world.worktrees.borrow().len(), 0);
}

#[test]
fn passing_checks_and_a_ready_pull_request_reach_the_merge_which_closes_it() {
    let (world, mut state) = building();
    let project = world.project();
    let branch = state.runs[&key()].branch.clone();
    let set = |pull| world.pulls.borrow_mut().insert(branch.clone(), pull);

    assert_eq!(state.runs[&key()].pull_request, None);

    set(open_pull(1, Rollup::Passed, true));
    world.poll(&project, &mut state);
    assert_eq!(status(&state, "builder"), Some(Status::Settled(Outcome(0))));
    assert_eq!(status(&state, "checks"), Some(Status::Settled(Outcome(0))));
    assert_eq!(status(&state, "ready"), Some(Status::Running));
    assert_eq!(state.runs[&key()].progress.failed[2], 1);
    assert_eq!(state.runs[&key()].pull_request, Some(31));
    let told = world.told.borrow().clone();
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0].0, "ses_0");
    assert!(told[0].1.contains("Every check passed"), "{told:?}");
    assert!(told[0].1.contains("Mark it ready"), "{told:?}");

    world.pulls.borrow_mut().remove(&branch);
    world.poll(&project, &mut state);
    assert_eq!(state.runs[&key()].pull_request, Some(31));
    assert_eq!(status(&state, "ready"), Some(Status::Running));

    set(open_pull(1, Rollup::Passed, false));
    world.poll(&project, &mut state);
    assert_eq!(status(&state, "merge"), Some(Status::Running));

    set(open_pull(1, Rollup::Pending, false));
    world.poll(&project, &mut state);
    assert_eq!(status(&state, "checks"), Some(Status::Running));
    assert_eq!(status(&state, "ready"), Some(Status::Pending));
    assert_eq!(status(&state, "merge"), Some(Status::Pending));
    assert_eq!(
        world.told.borrow().len(),
        1,
        "told again without a new green"
    );

    set(open_pull(1, Rollup::Passed, true));
    world.poll(&project, &mut state);
    assert_eq!(status(&state, "ready"), Some(Status::Running));
    assert_eq!(world.told.borrow().len(), 2, "a new green is told");

    set(PullRequest::Merged { number: 32 });
    world.poll(&project, &mut state);
    assert_eq!(state.runs[&key()].pull_request, Some(32));

    let ticket = world.ticket("12");
    let report = &ticket.comments[ticket.comments.len() - 1];
    assert_eq!(phase(&state), Phase::Done);
    assert!(!ticket.open);
    assert!(!ticket.marks.has(Tag::Working));
    assert_eq!(world.worktrees.borrow().len(), 0);
    stopped(&world, "atelier finished this run of build");
    assert!(report.starts_with("atelier finished `build`:"), "{report}");
    assert!(
        report.contains("- `checks`: passed (every check passed)")
            && report.contains("- `ready`: passed (#32 merged)"),
        "{report}"
    );
    assert!(
        report.contains("- `merge`: merged (#32 merged)"),
        "{report}"
    );
}

#[test]
fn a_pull_request_closed_unmerged_blocks_the_run() {
    let (world, mut state) = building();
    let branch = state.runs[&key()].branch.clone();

    world
        .pulls
        .borrow_mut()
        .insert(branch, PullRequest::Closed { number: 31 });
    world.poll(&world.project(), &mut state);

    assert_eq!(phase(&state), Phase::Blocked(Reason::Failed(1)));
    stopped(&world, "closed without merging");
}

/// Two commands after the worktree, and a fix when either fails.
fn audit() -> Vec<atelier_core::Settings> {
    vec![
        step("worktree", "git.worktree", &[]),
        step("deps", "atelier.command", &[]),
        step("lint", "atelier.command", &[("after", list(&["worktree"]))]),
        step(
            "fix",
            "atelier.command",
            &[(
                "after_any",
                table(&[("deps", "failed"), ("lint", "failed")]),
            )],
        ),
    ]
}

fn passed(why: &str) -> Observed {
    Observed::Settled(atelier_core::Outcome {
        name: "passed".into(),
        why: why.into(),
    })
}

#[test]
fn a_pipeline_without_forge_steps_does_not_ask_the_forge() {
    let world = World::with_issue("12", "Audit", &[Tag::Trigger("build")]);
    let mut project = world.project_with(&audit());
    project.forge = "another-plugin".into();
    let mut state = State::default();

    world.poll(&project, &mut state);
    world.poll(&project, &mut state);

    assert_eq!(phase(&state), Phase::Running);
    assert_eq!(*world.forge_reads.borrow(), 0);
}

#[test]
fn steps_whose_needs_hold_together_start_in_the_same_poll() {
    let world = World::with_issue("12", "Audit", &[Tag::Trigger("build")]);
    let project = world.project_with(&audit());
    let mut state = State::default();

    world.poll(&project, &mut state);

    assert_eq!(*world.started.borrow(), ["worktree", "deps", "lint"]);
    assert_eq!(status(&state, "deps"), Some(Status::Running));
    assert_eq!(status(&state, "lint"), Some(Status::Running));

    world.commands.borrow_mut().insert(
        "deps".into(),
        Observed::Failed {
            why: "two advisories".into(),
            last: false,
        },
    );
    world
        .commands
        .borrow_mut()
        .insert("lint".into(), passed("clean"));
    world.poll(&project, &mut state);

    assert_eq!(status(&state, "fix"), Some(Status::Running));
    assert_eq!(phase(&state), Phase::Running);

    world
        .commands
        .borrow_mut()
        .insert("fix".into(), passed("fixed"));
    world.poll(&project, &mut state);

    let ticket = world.ticket("12");
    let report = ticket.comments.last().unwrap();
    assert_eq!(phase(&state), Phase::Done);
    assert!(
        report.contains("- `deps`: failed (two advisories)"),
        "{report}"
    );
    assert!(report.contains("- `fix`: passed (fixed)"), "{report}");
    assert!(!ticket.open);
}

#[test]
fn a_step_whose_needs_cannot_hold_is_skipped_and_the_run_is_done() {
    let world = World::with_issue("12", "Audit", &[Tag::Trigger("build")]);
    let project = world.project_with(&audit());
    let mut state = State::default();
    world
        .commands
        .borrow_mut()
        .insert("deps".into(), passed("no advisories"));
    world
        .commands
        .borrow_mut()
        .insert("lint".into(), passed("clean"));

    world.poll(&project, &mut state);

    let report = world.ticket("12").comments.last().unwrap().clone();
    assert_eq!(phase(&state), Phase::Done);
    assert_eq!(status(&state, "fix"), Some(Status::Skipped));
    assert!(report.contains("- `fix`: skipped"), "{report}");
    assert_eq!(*world.started.borrow(), ["worktree", "deps", "lint"]);
}

#[test]
fn a_command_that_settles_an_outcome_it_lacks_is_reported() {
    let world = World::with_issue("12", "Audit", &[Tag::Trigger("build")]);
    let project = world.project_with(&audit());
    let mut state = State::default();
    world.commands.borrow_mut().insert(
        "deps".into(),
        Observed::Settled(atelier_core::Outcome {
            name: "found".into(),
            why: String::new(),
        }),
    );

    let report = world.poll(&project, &mut state);

    assert_eq!(phase(&state), Phase::Running);
    assert!(
        report.failures[0]
            .1
            .message
            .contains("deps settled found, which is none of its outcomes"),
        "{report:?}"
    );
}

#[test]
fn an_issue_marked_working_that_atelier_never_took_is_blocked() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Working]);
    let mut state = State::default();

    world.poll(&world.project(), &mut state);

    let ticket = world.ticket("12");
    assert_eq!(phase(&state), Phase::Blocked(Reason::LostTrack));
    assert_eq!(ticket.marks, Marks::default().with(Tag::Blocked));
    assert!(ticket.comments[0].contains("no record"));
}

#[test]
fn an_unmarked_issue_is_left_alone() {
    let world = World::with_issue("12", "Fix the dead link", &[]);
    let mut state = State::default();

    let report = world.poll(&world.project(), &mut state);

    assert_eq!(report.moves, []);
    assert_eq!(state.runs.len(), 0);
    assert_eq!(*world.changes.borrow(), 0);
}

#[test]
fn a_finished_run_is_not_read_again() {
    let (world, mut state) = building();
    let project = world.project();
    let branch = state.runs[&key()].branch.clone();
    world
        .pulls
        .borrow_mut()
        .insert(branch, PullRequest::Merged { number: 31 });
    world.poll(&project, &mut state);
    let reads = *world.reads.borrow();

    world.poll(&project, &mut state);

    assert_eq!(phase(&state), Phase::Done);
    assert_eq!(*world.reads.borrow(), reads);
}

/// A run of the build with the worktree passed and the builder running,
/// as a record would hold it.
fn running(steps: &[&str]) -> Record {
    let mut record = Record::new(&key(), "Fix the dead link", "build");
    let mut progress = Progress::default();
    progress.status[0] = Status::Settled(Outcome(0));
    progress.status[1] = Status::Running;
    record.phase = Phase::Running;
    record.progress = progress;
    record.steps = steps
        .iter()
        .map(|id| StepRecord {
            id: (*id).to_owned(),
            plugin: String::new(),
            session: None,
            why: None,
        })
        .collect();
    record
}

#[test]
fn a_builder_without_a_worktree_is_blocked_not_started() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Working]);
    let mut state = State::default();
    let mut record = running(&["worktree", "builder", "checks", "ready", "merge"]);
    record.pending = vec![Action::Start(1)];
    state.runs.insert(key(), record);

    world.poll(&world.project(), &mut state);

    assert_eq!(phase(&state), Phase::Blocked(Reason::Start(1)));
    assert!(
        world.ticket("12").comments[0]
            .contains("could not start `builder` for this issue: the session has no worktree")
    );
    assert_eq!(world.sessions.borrow().len(), 0);
}

#[test]
fn a_run_whose_pipeline_changed_under_it_is_blocked() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Working]);
    let mut state = State::default();
    state
        .runs
        .insert(key(), running(&["worktree", "builder", "review"]));

    world.poll(&world.project(), &mut state);

    assert_eq!(phase(&state), Phase::Blocked(Reason::Changed));
    assert!(world.ticket("12").comments[0].contains("its pipeline changed while it ran"));
}

#[test]
fn removing_a_pipeline_blocks_its_running_or_claiming_run() {
    for before in [Phase::Running, Phase::Claiming] {
        let world = World::with_issue("12", "Fix the dead link", &[Tag::Working]);
        let mut state = State::default();
        let mut record = running(&["worktree", "builder", "checks", "ready", "merge"]);
        record.phase = before;
        record.pipeline = "removed".into();
        state.runs.insert(key(), record);

        let report = world.poll(&world.project(), &mut state);

        assert_eq!(phase(&state), Phase::Blocked(Reason::Changed), "{before:?}");
        assert_eq!(report.failures, []);
    }
}

#[test]
fn saved_progress_the_pipeline_cannot_resume_blocks_its_run() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Working]);
    let mut state = State::default();
    let mut record = running(&["worktree", "builder", "checks", "ready", "merge"]);
    record.progress.status[0] = Status::Pending;
    state.runs.insert(key(), record);

    world.poll(&world.project(), &mut state);

    assert_eq!(phase(&state), Phase::Blocked(Reason::Changed));
}

#[test]
fn the_filter_narrows_requests_but_not_lost_track() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    world.edit("12", |ticket| ticket.assignee = Some("someone-else".into()));
    let copy = world.ticket("12");
    world.tickets.borrow_mut().insert("13".into(), copy);
    world.edit("13", |ticket| {
        ticket.marks = Marks::default().with(Tag::Working);
    });
    *world.filter.borrow_mut() = Filter {
        assignee: Some("me".into()),
        ..Filter::default()
    };
    let mut state = State::default();

    world.poll(&world.project(), &mut state);

    let lost = IssueKey {
        tracker: "mem".into(),
        id: "13".into(),
    };
    assert!(!state.runs.contains_key(&key()));
    assert_eq!(state.runs[&lost].phase, Phase::Blocked(Reason::LostTrack));
}

/// A project whose pipelines are the built-in build and the audit
/// pipeline, in that order.
fn two_pipelines(world: &World) -> atelier_core::Project {
    let build = crate::world::build();
    let audit = audit();

    world.project_with_pipelines(&[("build", build.as_slice()), ("audit", audit.as_slice())])
}

#[test]
fn an_issue_carrying_a_pipelines_trigger_starts_that_pipeline() {
    let world = World::with_issue("12", "Audit", &[Tag::Trigger("audit")]);
    let project = two_pipelines(&world);
    let mut state = State::default();

    let report = world.poll(&project, &mut state);

    let record = &state.runs[&key()];
    let ticket = world.ticket("12");

    assert_eq!(record.pipeline, "audit");
    assert_eq!(record.phase, Phase::Running);
    assert_eq!(
        report.moves.first().map(|moved| moved.pipeline.as_str()),
        Some("audit")
    );
    assert_eq!(*world.started.borrow(), ["worktree", "deps", "lint"]);
    assert!(!ticket.marks.has(Tag::Trigger("audit")));
    assert!(ticket.marks.has(Tag::Working));
}

#[test]
fn several_triggers_start_nothing_and_are_reported_ambiguous() {
    let world = World::with_issue(
        "12",
        "Audit",
        &[Tag::Trigger("audit"), Tag::Trigger("build")],
    );
    let project = two_pipelines(&world);
    let mut state = State::default();

    let report = world.poll(&project, &mut state);

    assert_eq!(report.moves, []);
    assert_eq!(report.failures, []);
    assert_eq!(report.ambiguous.len(), 1);

    let ambiguous = report.ambiguous.first().unwrap();

    assert_eq!(ambiguous.0, key());
    assert_eq!(ambiguous.1, ["build", "audit"]);
    assert_eq!(state.runs.len(), 0);
    assert_eq!(*world.changes.borrow(), 0);
    assert_eq!(state.etags.len(), 3);

    // The listing did not change, so atelier reports nothing again: the
    // daemon's line repeats only when a listing changes.
    let report = world.poll(&project, &mut state);

    assert_eq!(report.ambiguous, []);
    assert_eq!(report.moves, []);
    assert_eq!(report.failures, []);
    assert_eq!(*world.changes.borrow(), 0);
    assert_eq!(state.runs.len(), 0);
    assert_eq!(state.etags.len(), 3);

    world.edit("12", |ticket| {
        ticket.marks = std::mem::take(&mut ticket.marks).without(Tag::Trigger("audit"));
    });
    world.poll(&project, &mut state);

    assert_eq!(state.runs[&key()].pipeline, "build");
    assert_eq!(
        world.ticket("12").marks,
        Marks::default().with(Tag::Working)
    );
}

#[test]
fn a_finished_build_asked_for_audit_runs_audit_in_a_new_generation() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    let project = two_pipelines(&world);
    let mut state = State::default();
    world.poll(&project, &mut state);
    let generation = state.runs[&key()].generation;
    let finish = finish(&state, "nothing");

    requested(&world, &project, &mut state, finish);

    assert_eq!(phase(&state), Phase::Done);
    assert!(!world.ticket("12").open);

    world.edit("12", |ticket| {
        ticket.open = true;
        ticket.marks = std::mem::take(&mut ticket.marks).with(Tag::Trigger("audit"));
    });
    world.poll(&project, &mut state);

    let record = &state.runs[&key()];
    let ticket = world.ticket("12");

    assert_eq!(record.pipeline, "audit");
    assert_eq!(record.generation, generation.saturating_add(1));
    assert_eq!(record.phase, Phase::Running);
    assert!(!ticket.marks.has(Tag::Trigger("audit")));
    assert!(ticket.marks.has(Tag::Working));
}

#[test]
fn a_running_build_that_gains_audit_runs_on_and_audit_waits_its_turn() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    let project = two_pipelines(&world);
    let mut state = State::default();
    world.poll(&project, &mut state);
    let generation = state.runs[&key()].generation;

    world.edit("12", |ticket| {
        ticket.marks = std::mem::take(&mut ticket.marks).with(Tag::Trigger("audit"));
    });
    let report = world.poll(&project, &mut state);

    assert_eq!(report.ambiguous, []);
    assert_eq!(report.moves, []);
    assert_eq!(phase(&state), Phase::Running);
    assert_eq!(state.runs[&key()].pipeline, "build");
    assert_eq!(state.runs[&key()].generation, generation);
    assert!(world.ticket("12").marks.has(Tag::Trigger("audit")));

    let finish = finish(&state, "nothing");
    requested(&world, &project, &mut state, finish);
    assert_eq!(phase(&state), Phase::Done);

    world.edit("12", |ticket| ticket.open = true);
    world.poll(&project, &mut state);

    assert_eq!(state.runs[&key()].pipeline, "audit");
    assert_eq!(state.runs[&key()].generation, generation.saturating_add(1));
}

/// A working run is not ambiguous, even when its item gains both
/// pipelines' triggers: it has nothing pending, so it goes on with its own
/// pipeline and changes no mark.
#[test]
fn a_running_build_that_gains_both_triggers_is_not_ambiguous() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    let project = two_pipelines(&world);
    let mut state = State::default();
    world.poll(&project, &mut state);

    let generation = state.runs[&key()].generation;
    let changes = *world.changes.borrow();

    // The first poll left the builder running with nothing pending, so only
    // its working mark keeps it from being free to start.
    assert_eq!(phase(&state), Phase::Running);
    assert_eq!(state.runs[&key()].pending, []);

    world.edit("12", |ticket| {
        ticket.marks = std::mem::take(&mut ticket.marks)
            .with(Tag::Trigger("build"))
            .with(Tag::Trigger("audit"));
    });

    let report = world.poll(&project, &mut state);

    assert_eq!(report.ambiguous, []);
    assert_eq!(report.moves, []);
    assert_eq!(report.failures, []);
    assert_eq!(*world.changes.borrow(), changes);
    assert_eq!(*world.started.borrow(), ["worktree", "builder"]);
    assert_eq!(phase(&state), Phase::Running);
    assert_eq!(state.runs[&key()].pipeline, "build");
    assert_eq!(state.runs[&key()].generation, generation);
    assert_eq!(
        world.ticket("12").marks,
        Marks::default()
            .with(Tag::Working)
            .with(Tag::Trigger("build"))
            .with(Tag::Trigger("audit"))
    );
}

/// An audit run takes the audit trigger off when it starts. A build
/// trigger that arrives while it runs is another pipeline's, so stopping
/// the run leaves it alone; the issue is closed, so nothing starts it
/// again in the same poll.
#[test]
fn a_stop_unmarks_the_runs_own_trigger_and_not_the_one_asked_for() {
    let world = World::with_issue("12", "Audit", &[Tag::Trigger("audit")]);
    let project = two_pipelines(&world);
    let mut state = State::default();
    world.poll(&project, &mut state);
    assert_eq!(state.runs[&key()].pipeline, "audit");

    world.edit("12", |ticket| {
        ticket.open = false;
        ticket.marks = std::mem::take(&mut ticket.marks).with(Tag::Trigger("build"));
    });
    let stop = atelier_core::Request {
        generation: state.runs[&key()].generation,
        command: atelier_core::Command::Stop,
    };
    requested(&world, &project, &mut state, stop);

    let ticket = world.ticket("12");

    assert_eq!(phase(&state), Phase::Stopped);
    assert!(ticket.marks.has(Tag::Trigger("build")));
    assert!(!ticket.marks.has(Tag::Working));
}

/// A claim that fails for good takes the run's own trigger mark off, and
/// leaves another pipeline's where it is.
#[test]
fn a_failed_claim_unmarks_only_the_runs_own_trigger() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    let project = two_pipelines(&world);
    let mut state = State::default();
    let mut record = Record::new(&key(), "Fix the dead link", "build");
    record.phase = Phase::Claiming;
    record.pending = vec![Action::Mark(Mark::Working), Action::Unmark(Mark::Requested)];
    state.runs.insert(key(), record);
    world.edit("12", |ticket| {
        ticket.open = false;
        ticket.marks = std::mem::take(&mut ticket.marks).with(Tag::Trigger("audit"));
    });
    *world.fail.borrow_mut() = Some((1, Failure::Permanent));

    world.poll(&project, &mut state);

    let ticket = world.ticket("12");

    assert_eq!(phase(&state), Phase::Blocked(Reason::Claim));
    assert!(!ticket.marks.has(Tag::Trigger("build")));
    assert!(ticket.marks.has(Tag::Trigger("audit")));
    assert!(ticket.marks.has(Tag::Blocked));
    assert!(!ticket.marks.has(Tag::Working));
}

/// A claiming run with actions pending goes on with its own pipeline, even
/// when the item gains another pipeline's trigger.
#[test]
fn a_claiming_run_that_gains_another_trigger_keeps_its_pipeline() {
    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    let project = two_pipelines(&world);
    let mut state = State::default();
    let mut record = Record::new(&key(), "Fix the dead link", "build");
    record.begin(&project.pipelines[0]);
    record.phase = Phase::Claiming;
    record.pending = vec![Action::Mark(Mark::Working), Action::Unmark(Mark::Requested)];
    let generation = record.generation;
    state.runs.insert(key(), record);
    world.edit("12", |ticket| {
        ticket.marks = std::mem::take(&mut ticket.marks).with(Tag::Trigger("audit"));
    });

    let report = world.poll(&project, &mut state);

    let ticket = world.ticket("12");

    assert_eq!(report.ambiguous, []);
    assert_eq!(report.failures, []);
    assert_eq!(phase(&state), Phase::Running);
    assert_eq!(state.runs[&key()].pipeline, "build");
    assert_eq!(state.runs[&key()].generation, generation);
    assert!(!ticket.marks.has(Tag::Trigger("build")));
    assert!(ticket.marks.has(Tag::Trigger("audit")));
    assert!(ticket.marks.has(Tag::Working));
}

/// A blocked run whose item asks for another pipeline starts that
/// pipeline, afresh, in the next poll.
#[test]
fn a_blocked_run_asked_for_another_pipeline_starts_it_in_a_new_generation() {
    let world = World::with_issue("12", "Audit", &[Tag::Trigger("build")]);
    let project = two_pipelines(&world);
    let mut state = State::default();
    *world.fail.borrow_mut() = Some((3, Failure::Permanent));
    world.poll(&project, &mut state);
    *world.fail.borrow_mut() = None;

    let record = &state.runs[&key()];
    let generation = record.generation;

    assert_eq!(record.phase, Phase::Blocked(Reason::Start(0)));
    assert_eq!(record.pipeline, "build");

    world.edit("12", |ticket| {
        ticket.marks = std::mem::take(&mut ticket.marks).with(Tag::Trigger("audit"));
    });
    let report = world.poll(&project, &mut state);

    let record = &state.runs[&key()];

    assert_eq!(report.ambiguous, []);
    assert_eq!(report.failures, []);
    assert_eq!(record.phase, Phase::Running);
    assert_eq!(record.pipeline, "audit");
    assert_eq!(record.generation, generation.saturating_add(1));
    assert!(!world.ticket("12").marks.has(Tag::Trigger("audit")));
    assert!(world.ticket("12").marks.has(Tag::Working));
}

/// An ambiguous item changes nothing, even when a run of it is already
/// done or blocked.
#[test]
fn an_ambiguous_item_with_a_done_or_blocked_run_is_still_reported() {
    let world = World::with_issue(
        "12",
        "Fix the dead link",
        &[Tag::Trigger("build"), Tag::Trigger("audit")],
    );
    let project = two_pipelines(&world);

    for before in [Phase::Done, Phase::Blocked(Reason::Failed(2))] {
        let mut state = State::default();
        let mut record = Record::new(&key(), "Fix the dead link", "build");
        record.phase = before;
        state.runs.insert(key(), record);

        let report = world.poll(&project, &mut state);

        assert_eq!(report.moves, [], "{before:?}");
        assert_eq!(report.failures, [], "{before:?}");
        assert_eq!(
            report.ambiguous,
            [(key(), vec!["build".to_owned(), "audit".to_owned()])],
            "{before:?}"
        );
        assert_eq!(phase(&state), before, "{before:?}");
        assert_eq!(*world.changes.borrow(), 0, "{before:?}");
    }
}

#[test]
fn a_trigger_the_project_has_no_pipeline_for_is_ignored() {
    use atelier_core::{Issue, Observation, Seen, Watched};
    use std::collections::BTreeMap;

    let world = World::with_issue("12", "Audit", &[]);
    let marks = Marks::default().with(Tag::Trigger("release"));
    let watched = Watched {
        issue: Issue {
            key: key(),
            title: "Audit".into(),
            open: true,
            marks: marks.clone(),
        },
        seen: Seen {
            open: true,
            marks,
            pull_request: None,
        },
    };
    let observation = Observation {
        issues: BTreeMap::from([(key(), watched)]),
        ..Observation::default()
    };
    let mut state = State::default();

    let report = act(
        &observation,
        &mut state,
        &two_pipelines(&world),
        &world.plugins(),
        &mut |_| Ok::<(), ()>(()),
    )
    .unwrap();

    assert_eq!(report.moves, []);
    assert_eq!(report.ambiguous, []);
    assert_eq!(state.runs.len(), 0);
    assert_eq!(*world.changes.borrow(), 0);
}

#[test]
fn observe_lists_every_pipeline_and_the_working_mark_once() {
    use atelier_core::Listed;

    let world = World::with_issue("12", "Fix the dead link", &[Tag::Trigger("build")]);
    world.edit("12", |ticket| ticket.assignee = Some("someone-else".into()));
    *world.filter.borrow_mut() = Filter {
        assignee: Some("me".into()),
        ..Filter::default()
    };
    let project = two_pipelines(&world);

    let observation = observe(&project, &world.plugins(), &State::default()).unwrap();
    let keys: Vec<(String, Listed)> = observation.etags.keys().cloned().collect();
    let listed = *world.listings.borrow();

    assert_eq!(
        observation.issues.len(),
        0,
        "a trigger's listing takes the source's filter"
    );
    assert_eq!(
        keys,
        [
            ("mem".to_owned(), Listed::Trigger("audit".to_owned())),
            ("mem".to_owned(), Listed::Trigger("build".to_owned())),
            ("mem".to_owned(), Listed::Working),
        ]
    );
    assert_eq!(
        listed,
        project.pipelines.len().saturating_add(1),
        "each pipeline's trigger and the working mark, once"
    );
}
